//! Opus in WebM (YouTube's itag 251, about 130-160 kbps against AAC's 128): Symphonia
//! reads the container and `rusty-opus` (pure Rust) decodes the packets. rodio's own
//! decoder only knows Symphonia's codecs, and Symphonia has no Opus.

use std::io::{Read, Seek, SeekFrom};
use std::num::NonZero;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusty_opus::OpusDecoder;
use symphonia::core::codecs::CODEC_TYPE_OPUS;
use symphonia::core::formats::{FormatOptions, FormatReader, Packet, SeekMode, SeekTo};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::units::{Time, TimeBase};
use symphonia::default::formats::MkvReader;

use crate::stream::StreamReader;

/// Opus always decodes at 48 kHz.
const RATE: u32 = 48_000;
/// The longest Opus packet: 120 ms at 48 kHz, per channel.
const MAX_FRAME: usize = 5760;
/// After a jump the decoder needs about 80 ms of audio to settle (RFC 7845 §4.6): a
/// seek starts that much earlier and drops it.
const PREROLL: Duration = Duration::from_millis(80);
/// Tail bytes kept as the reader reaches the end of the file, to read WebM's trailing
/// `DiscardPadding` element (which Symphonia's `MkvReader` skips).
const TAIL_BYTES: usize = 8192;

/// Keeps the last `TAIL_BYTES` of the stream as they are read, so the final packet's
/// `DiscardPadding` can be read at EOF without re-reading the file.
struct Tail {
    base: u64,
    bytes: Vec<u8>,
}

impl Tail {
    fn new(len: u64) -> Self {
        let base = len.saturating_sub(TAIL_BYTES as u64);
        Self { base, bytes: vec![0; (len - base) as usize] }
    }

    fn record(&mut self, pos: u64, data: &[u8]) {
        let end = pos.saturating_add(data.len() as u64);
        if end <= self.base {
            return;
        }
        let src_start = self.base.saturating_sub(pos) as usize;
        let dst_start = pos.saturating_sub(self.base) as usize;
        let n = (data.len() - src_start).min(self.bytes.len().saturating_sub(dst_start));
        self.bytes[dst_start..dst_start + n].copy_from_slice(&data[src_start..src_start + n]);
    }
}

/// The buffer as a Symphonia source: seekable, with its full length known.
struct Media {
    reader: StreamReader,
    len: u64,
    pos: u64,
    tail: Arc<Mutex<Tail>>,
}

impl Read for Media {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.reader.read(buf)?;
        if n > 0 {
            let start = self.pos;
            self.pos += n as u64;
            if self.pos > self.len.saturating_sub(TAIL_BYTES as u64) {
                self.tail.lock().unwrap().record(start, &buf[..n]);
            }
        }
        Ok(n)
    }
}

impl Seek for Media {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let p = self.reader.seek(pos)?;
        self.pos = p;
        Ok(p)
    }
}

impl MediaSource for Media {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.len)
    }
}

pub struct OpusSource {
    format: MkvReader,
    track: u32,
    time_base: Option<TimeBase>,
    decoder: OpusDecoder,
    channels: u16,
    /// Decoded samples (interleaved) not handed out yet, from `pos`.
    buf: Vec<f32>,
    pos: usize,
    /// Samples (interleaved) still to drop: the encoder's pre-skip, or the preroll before a seek target.
    skip: usize,
    duration: Option<Duration>,
    /// One-packet lookahead so we recognize the final packet and trim its `DiscardPadding`.
    pending: Option<Packet>,
    eof: bool,
    tail: Arc<Mutex<Tail>>,
}

impl OpusSource {
    /// Reads the WebM header and its seek index (YouTube puts both at the start), so
    /// it opens after the first block whether or not it'll be seeked.
    pub fn new(reader: StreamReader, len: u64, duration_ms: u64) -> anyhow::Result<Self> {
        let tail = Arc::new(Mutex::new(Tail::new(len)));
        let mss = MediaSourceStream::new(Box::new(Media { reader, len, pos: 0, tail: tail.clone() }), Default::default());
        let format = MkvReader::try_new(mss, &FormatOptions::default())?;
        let track = format.tracks().iter().find(|t| t.codec_params.codec == CODEC_TYPE_OPUS).ok_or_else(|| anyhow::anyhow!("no Opus track"))?;
        // OpusHead (RFC 7845 §5.1): channel count at byte 9, pre-skip at 10-11.
        let head = track.codec_params.extra_data.as_deref().unwrap_or_default();
        let channels = head.get(9).map_or(2, |&c| c as u16);
        anyhow::ensure!(matches!(channels, 1 | 2), "{channels}-channel Opus isn't supported");
        let pre_skip = head.get(10..12).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]) as usize);
        let (track, time_base) = (track.id, track.codec_params.time_base);
        Ok(Self {
            format,
            track,
            time_base,
            decoder: OpusDecoder::new(RATE as i32, channels as usize).map_err(|e| anyhow::anyhow!("opus: {e:?}"))?,
            channels,
            buf: Vec::new(),
            pos: 0,
            skip: pre_skip * channels as usize,
            duration: (duration_ms > 0).then(|| Duration::from_millis(duration_ms)),
            pending: None,
            eof: false,
            tail,
        })
    }

    fn next_track_packet(&mut self) -> Option<Packet> {
        if self.eof {
            return None;
        }
        loop {
            match self.format.next_packet() {
                Ok(p) if p.track_id() == self.track => return Some(p),
                Ok(_) => continue,
                Err(_) => {
                    self.eof = true;
                    return None;
                }
            }
        }
    }

    /// Decode the next packet into `buf`; false at the end (or when the download failed).
    fn refill(&mut self) -> bool {
        loop {
            let packet = match self.pending.take() {
                Some(p) => p,
                None => match self.next_track_packet() {
                    Some(p) => p,
                    None => return false,
                },
            };
            self.pending = self.next_track_packet();
            let is_last = self.pending.is_none();
            let ch = self.channels as usize;
            self.buf.resize(MAX_FRAME * ch, 0.0);
            match self.decoder.decode(&packet.data, MAX_FRAME, &mut self.buf) {
                Ok(n) => {
                    let mut frames = n;
                    if is_last {
                        let tail = self.tail.lock().unwrap();
                        let trim = discard_padding_frames(&tail.bytes, &packet.data);
                        frames = frames.saturating_sub(trim);
                    }
                    self.buf.truncate(frames * ch);
                    let drop = self.skip.min(self.buf.len());
                    self.skip -= drop;
                    self.pos = drop;
                    if self.pos < self.buf.len() {
                        return true;
                    }
                }
                // A damaged packet: skip it rather than end the song.
                Err(e) => log::debug!("opus: {e:?}"),
            }
        }
    }
}

impl Iterator for OpusSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.pos >= self.buf.len() && !self.refill() {
            return None;
        }
        self.pos += 1;
        Some(self.buf[self.pos - 1])
    }
}

impl rodio::Source for OpusSource {
    /// A span per decoded packet, as rodio's own decoders do. Without an end (`None`),
    /// the resampler after a player's queue keeps 48 kHz for the songs queued after
    /// this one, and a 44.1 kHz song then plays 9 % fast.
    fn current_span_len(&self) -> Option<usize> {
        Some((self.buf.len() - self.pos).max(self.channels as usize))
    }
    fn channels(&self) -> rodio::ChannelCount {
        NonZero::new(self.channels).unwrap_or(NonZero::<u16>::MIN)
    }
    fn sample_rate(&self) -> rodio::SampleRate {
        NonZero::new(RATE).unwrap_or(NonZero::<u32>::MIN)
    }
    fn total_duration(&self) -> Option<Duration> {
        self.duration
    }
    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        let from = pos.saturating_sub(PREROLL);
        let to = SeekTo::Time { time: Time::from(from.as_secs_f64()), track_id: Some(self.track) };
        let seeked = self.format.seek(SeekMode::Accurate, to).map_err(|e| rodio::source::SeekError::Other(Arc::new(e)))?;
        // The packet it landed on starts at or before `from`: drop up to `pos`.
        let at = self.time_base.map_or(from.as_secs_f64(), |tb| {
            let t = tb.calc_time(seeked.actual_ts);
            t.seconds as f64 + t.frac
        });
        let ahead = (pos.as_secs_f64() - at).max(0.0);
        // A fresh decoder: nothing from before the jump carries over.
        if let Ok(d) = OpusDecoder::new(RATE as i32, self.channels as usize) {
            self.decoder = d;
        }
        self.buf.clear();
        self.pos = 0;
        self.pending = None;
        self.eof = false;
        self.skip = (ahead * RATE as f64) as usize * self.channels as usize;
        Ok(())
    }
}

/// Reads an EBML VINT: element ID (`keep_marker = true`, up to 4 bytes) or data size
/// (`keep_marker = false`, up to 8 bytes, rejecting unknown all-ones lengths).
fn ebml_vint(bytes: &[u8], keep_marker: bool) -> Option<(u64, usize)> {
    let first = *bytes.first()?;
    let len = first.leading_zeros() as usize + 1;
    let max_len = if keep_marker { 4 } else { 8 };
    if len > max_len || bytes.len() < len {
        return None;
    }
    let mask = if keep_marker { 0xFF } else if len == 8 { 0 } else { (1u8 << (8 - len)) - 1 };
    let val = bytes[1..len].iter().fold((first & mask) as u64, |v, &b| (v << 8) | b as u64);
    if !keep_marker && val == (1u64 << (7 * len)) - 1 {
        return None;
    }
    Some((val, len))
}

/// Parses one Matroska `BlockGroup` body (`0xA0`); returns trailing 48 kHz frames to trim
/// if its `Block` (`0xA1`) carries `last_packet`.
fn parse_block_group(mut body: &[u8], last_packet: &[u8]) -> Option<usize> {
    let (mut matched, mut discard_ns) = (false, None);
    while !body.is_empty() {
        let (id, id_len) = ebml_vint(body, true)?;
        let (len, sz_len) = ebml_vint(&body[id_len..], false)?;
        let len = usize::try_from(len).ok()?;
        let data = body.get(id_len + sz_len..id_len + sz_len + len)?;
        body = &body[id_len + sz_len + len..];
        match id {
            0xA1 => {
                // Block header: TrackNumber VINT (1..=4) + 2-byte timecode + 1-byte flags.
                if let Some((_, tn)) = ebml_vint(data, false)
                    && tn <= 4
                    && data.get(tn + 3..) == Some(last_packet)
                {
                    matched = true;
                }
            }
            0x75A2 if !data.is_empty() && data.len() <= 8 => {
                let sign = if data[0] & 0x80 != 0 { -1i64 } else { 0 };
                discard_ns = Some(data.iter().fold(sign, |v, &b| (v << 8) | b as i64));
            }
            _ => {}
        }
    }
    matched.then(|| match discard_ns {
        Some(ns) if ns > 0 => ((ns as u128 * RATE as u128 + 500_000_000) / 1_000_000_000) as usize,
        _ => 0,
    })
}

/// Extracts trailing 48 kHz frames to trim from the last Opus packet by locating its
/// Matroska `BlockGroup` (`0xA0`) in `tail` and reading `DiscardPadding` (`0x75A2`, ns).
fn discard_padding_frames(tail: &[u8], last_packet: &[u8]) -> usize {
    if last_packet.is_empty() {
        return 0;
    }
    for i in (0..tail.len()).rev() {
        if tail[i] != 0xA0 {
            continue;
        }
        let Some((len, sz_len)) = ebml_vint(&tail[i + 1..], false) else { continue };
        let Some(body) = usize::try_from(len).ok().and_then(|l| tail.get(i + 1 + sz_len..i + 1 + sz_len + l)) else { continue };
        if let Some(frames) = parse_block_group(body, last_packet) {
            return frames;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::discard_padding_frames;
    use std::time::{Duration, Instant};

    use rodio::Source;

    #[test]
    fn discard_padding_parses_real_webm_block_groups() {
        // Real tail layout from YouTube's WebM Opus streams (DiscardPadding before Block).
        let pkt = [0xAAu8; 505];
        let mut bg = vec![0x75, 0xA2, 0x84, 0x01, 0x22, 0xDE, 0xE4]; // 19,062,500 ns = 915 frames
        bg.extend_from_slice(&[0xA1, 0x41, 0xFD, 0x81, 0x00, 0x00, 0x00]); // Block: 4-byte hdr + 505 bytes
        bg.extend_from_slice(&pkt);
        let len = bg.len();
        let mut stream = vec![0x00, 0x11, 0x22, 0xA0, (0x40 | (len >> 8)) as u8, (len & 0xFF) as u8];
        stream.extend_from_slice(&bg);
        assert_eq!(discard_padding_frames(&stream, &pkt), 915);

        // 3-byte DiscardPadding (6,583,333 ns = 316 frames) after Block.
        let pkt2 = [0x55u8; 20];
        let mut bg2 = vec![0xA1, 0x80 | 24, 0x81, 0x00, 0x14, 0x00];
        bg2.extend_from_slice(&pkt2);
        bg2.extend_from_slice(&[0x75, 0xA2, 0x83, 0x64, 0x74, 0x25]);
        let mut stream2 = vec![0xA0, 0x80 | bg2.len() as u8];
        stream2.extend_from_slice(&bg2);
        assert_eq!(discard_padding_frames(&stream2, &pkt2), 316);

        // Negative DiscardPadding trims nothing.
        let mut bg3 = vec![0x75, 0xA2, 0x81, 0xF0, 0xA1, 0x80 | 24, 0x81, 0x00, 0x00, 0x00];
        bg3.extend_from_slice(&pkt2);
        let mut stream3 = vec![0xA0, 0x80 | bg3.len() as u8];
        stream3.extend_from_slice(&bg3);
        assert_eq!(discard_padding_frames(&stream3, &pkt2), 0);

        // Unrelated packet bytes containing 0x75 0xA2 must not false-match.
        let fake = [0xA0, 0x88, 0x75, 0xA2, 0x83, 0x64, 0x74, 0x25, 0x00, 0x00];
        assert_eq!(discard_padding_frames(&fake, &pkt2), 0);
    }

    /// The first `left` samples of a source, keeping its spans as they are.
    struct Head<S> {
        inner: S,
        left: usize,
    }

    impl<S: Source> Iterator for Head<S> {
        type Item = f32;
        fn next(&mut self) -> Option<f32> {
            self.left = self.left.checked_sub(1)?;
            self.inner.next()
        }
    }

    impl<S: Source> Source for Head<S> {
        fn current_span_len(&self) -> Option<usize> {
            self.inner.current_span_len().map(|n| n.min(self.left))
        }
        fn channels(&self) -> rodio::ChannelCount {
            self.inner.channels()
        }
        fn sample_rate(&self) -> rodio::SampleRate {
            self.inner.sample_rate()
        }
        fn total_duration(&self) -> Option<Duration> {
            None
        }
    }

    /// A 44.1 kHz song queued after an Opus one (48 kHz) plays at its own speed: the
    /// resampler has to see where the Opus song ends to pick up the new rate.
    /// `cargo test -- --ignored opus_then_aac_rate --nocapture`
    #[tokio::test]
    #[ignore]
    async fn opus_then_aac_rate() {
        let api = crate::ytmapi::Api::new(&std::env::temp_dir().join("ytm-rs-opus-test"));
        let vd = api.visitor_data(false).await.unwrap();
        let http = reqwest::Client::new();
        let s = crate::innertube::audio_stream(&http, "zuO2fClon98", &vd, true).await.unwrap();
        let shared = crate::stream::Shared::new(s.size, s.duration_ms, s.loudness_db, s.opus, false);
        crate::stream::download(http.clone(), crate::stream::Media::File(s.url), crate::innertube::UA.to_owned(), shared.clone(), None).await;
        let opus = crate::stream::decoder(shared, true).unwrap();
        assert_eq!(opus.sample_rate().get(), 48_000);
        let (tx, rx) = rodio::queue::queue(false);
        tx.append(Head { inner: opus, left: 48_000 * 2 });
        // One second of 44.1 kHz stereo, all ones so it can be told apart from the song.
        let one_sec = rodio::buffer::SamplesBuffer::new(std::num::NonZero::new(2).unwrap(), std::num::NonZero::new(44_100).unwrap(), vec![1.0f32; 44_100 * 2]);
        tx.append(one_sec);
        let out: Vec<f32> = rodio::source::UniformSourceIterator::new(rx, std::num::NonZero::new(2).unwrap(), std::num::NonZero::new(48_000).unwrap()).collect();
        let second = out.iter().skip(48_000 * 2).filter(|&&x| (x - 1.0).abs() < 1e-3).count() / 2;
        println!("44.1 kHz second came out as {second} frames at 48 kHz");
        assert!((second as i64 - 48_000).abs() < 200, "played at the wrong rate: {second} frames, not 48000");
    }

    /// Signed out: plays a song's Opus stream end to end and seeks in it, against its AAC stream.
    /// `cargo test --release -- --ignored opus_stream_live --nocapture`
    #[tokio::test]
    #[ignore]
    async fn opus_stream_live() {
        let api = crate::ytmapi::Api::new(&std::env::temp_dir().join("ytm-rs-opus-test"));
        let vd = api.visitor_data(false).await.unwrap();
        let http = reqwest::Client::new();
        for id in ["zuO2fClon98", "N48TFWsBA-k"] {
            for opus in [true, false] {
                let s = crate::innertube::audio_stream(&http, id, &vd, opus).await.unwrap();
                assert_eq!(s.opus, opus);
                let shared = crate::stream::Shared::new(s.size, s.duration_ms, s.loudness_db, s.opus, false);
                crate::stream::download(http.clone(), crate::stream::Media::File(s.url), crate::innertube::UA.to_owned(), shared.clone(), None).await;
                assert!(shared.error().is_none(), "{:?}", shared.error());
                let mut d = crate::stream::decoder(shared.clone(), true).unwrap();
                let (ch, rate) = (d.channels().get() as usize, d.sample_rate().get() as usize);
                let t = Instant::now();
                let (mut n, mut sq) = (0usize, 0f64);
                for x in d.by_ref() {
                    n += 1;
                    sq += (x as f64) * (x as f64);
                }
                let secs = n as f64 / (ch * rate) as f64;
                println!(
                    "{id} {}: {} kbps, {ch} ch {rate} Hz, {secs:.2} s decoded of {:.2} s, rms {:.4}, decoded in {:?}",
                    if opus { "opus" } else { "aac " },
                    s.size * 8 / s.duration_ms.max(1),
                    s.duration_ms as f64 / 1000.0,
                    (sq / n as f64).sqrt(),
                    t.elapsed()
                );
                assert!((secs - s.duration_ms as f64 / 1000.0).abs() < 0.5, "length off");
                // Seek to the middle: what's left should be the second half.
                let mut d = crate::stream::decoder(shared, true).unwrap();
                let half = Duration::from_millis(s.duration_ms / 2);
                d.try_seek(half).unwrap();
                let left = d.count() as f64 / (ch * rate) as f64;
                println!("    after seeking to {:.2} s: {left:.2} s left", half.as_secs_f64());
                assert!((left - (s.duration_ms as f64 / 1000.0 - half.as_secs_f64())).abs() < 0.5, "seek off");
            }
        }
    }
}
