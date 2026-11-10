//! HLS audio: YouTube's itag 234, the AAC of itag 140 cut into ~7 s segments, each
//! an ID3 tag (its start time) and then ADTS frames (Apple's "packed audio").
//!
//! The visionOS player's progressive URLs are refused past 1 MiB where YouTube asks
//! that client for a PO token (an experiment some networks get, Oct 2026); its HLS
//! segments aren't. The segments are the buffer's blocks (`stream::Shared::segmented`),
//! and this reads their frames in order and decodes them with Symphonia's AAC decoder.

use std::num::NonZero;
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use symphonia::core::audio::{Channels, SampleBuffer};
use symphonia::core::codecs::{CODEC_TYPE_AAC, CodecParameters, Decoder, DecoderOptions};
use symphonia::core::formats::Packet;
use symphonia::default::codecs::AacDecoder;

use crate::stream::StreamReader;

/// AAC's sample rates, by ADTS index.
const RATES: [u32; 13] = [96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000, 7_350];
/// Samples per AAC frame, per channel.
const FRAME: u64 = 1024;
/// The ID3 frame owner that carries a segment's start (a 33-bit, 90 kHz MPEG timestamp).
const TIMESTAMP_OWNER: &[u8] = b"com.apple.streaming.transportStreamTimestamp\0";

/// One ADTS frame in a segment.
#[derive(Debug, PartialEq)]
struct Adts {
    payload: Range<usize>,
    rate: u32,
    channels: u16,
    /// Where the next frame starts.
    next: usize,
}

/// The ADTS frame at `off` in `data`, if one starts there and fits.
fn adts_at(data: &[u8], off: usize) -> Option<Adts> {
    let h = data.get(off..off + 7)?;
    // Sync word, then layer 0.
    if h[0] != 0xFF || h[1] & 0xF6 != 0xF0 {
        return None;
    }
    // Without CRC protection the header is 7 bytes, with it 9.
    let header = if h[1] & 1 == 1 { 7 } else { 9 };
    let rate = *RATES.get(((h[2] >> 2) & 0xF) as usize)?;
    let channels = (((h[2] & 1) << 2) | (h[3] >> 6)) as u16;
    let len = (((h[3] & 3) as usize) << 11) | ((h[4] as usize) << 3) | ((h[5] as usize) >> 5);
    // One AAC frame per ADTS frame, as YouTube writes them.
    if len < header || off + len > data.len() || h[6] & 3 != 0 {
        return None;
    }
    Some(Adts { payload: off + header..off + len, rate, channels, next: off + len })
}

/// Where a segment's frames start (after its ID3 tag), and the start time the tag gives.
fn id3(data: &[u8]) -> (usize, Option<f64>) {
    if data.len() < 10 || &data[..3] != b"ID3" {
        return (0, None);
    }
    let size = data[6..10].iter().fold(0usize, |n, &b| (n << 7) | (b & 0x7F) as usize);
    let footer = if data[5] & 0x10 != 0 { 10 } else { 0 };
    let end = (10 + size + footer).min(data.len());
    let tag = &data[10..end];
    let ts = tag.windows(TIMESTAMP_OWNER.len()).position(|w| w == TIMESTAMP_OWNER).and_then(|i| {
        let b = tag.get(i + TIMESTAMP_OWNER.len()..i + TIMESTAMP_OWNER.len() + 8)?;
        let pts = u64::from_be_bytes(b.try_into().ok()?) & ((1 << 33) - 1);
        Some(pts as f64 / 90_000.0)
    });
    (end, ts)
}

fn channel_mask(n: u16) -> Channels {
    match n {
        1 => Channels::FRONT_LEFT,
        _ => Channels::FRONT_LEFT | Channels::FRONT_RIGHT,
    }
}

pub struct HlsSource {
    reader: StreamReader,
    /// Each segment's start (s), from the playlist.
    starts: Arc<[f64]>,
    /// The segment being read, its bytes, and where its next frame starts.
    seg: usize,
    data: Bytes,
    off: usize,
    /// The first segment's ID3 time: the others' are taken relative to it.
    base: Option<f64>,
    decoder: AacDecoder,
    rate: u32,
    channels: u16,
    samples: Option<SampleBuffer<f32>>,
    /// Decoded samples (interleaved) not handed out yet, from `pos`.
    buf: Vec<f32>,
    pos: usize,
    /// Samples (interleaved) still to drop, up to a seek's target.
    skip: usize,
    duration: Option<Duration>,
}

impl HlsSource {
    /// Reads the first segment for the stream's format, so it opens once that's here.
    pub fn new(mut reader: StreamReader, starts: Arc<[f64]>, duration_ms: u64) -> anyhow::Result<Self> {
        let data = reader.block_at(0)?;
        let (off, base) = id3(&data);
        let first = (off..data.len().min(off + 4096)).find_map(|o| adts_at(&data, o)).ok_or_else(|| anyhow::anyhow!("no AAC frames in the first HLS segment"))?;
        let (rate, channels) = (first.rate, first.channels);
        anyhow::ensure!(matches!(channels, 1 | 2), "{channels}-channel AAC isn't supported");
        let mut params = CodecParameters::new();
        params.for_codec(CODEC_TYPE_AAC).with_sample_rate(rate).with_channels(channel_mask(channels));
        let decoder = AacDecoder::try_new(&params, &DecoderOptions::default())?;
        Ok(Self {
            reader,
            starts,
            seg: 0,
            data,
            off,
            base,
            decoder,
            rate,
            channels,
            samples: None,
            buf: Vec::new(),
            pos: 0,
            skip: 0,
            duration: (duration_ms > 0).then(|| Duration::from_millis(duration_ms)),
        })
    }

    /// Makes segment `k` the one being read, from its first frame; returns its start
    /// time, or None when it won't arrive (the download failed or stopped).
    fn load(&mut self, k: usize) -> Option<f64> {
        if k != self.seg || self.data.is_empty() {
            self.data = self.reader.block_at(k).ok()?;
            self.seg = k;
        }
        let (off, ts) = id3(&self.data);
        self.off = off;
        Some(match (ts, self.base) {
            (Some(t), Some(b)) => t - b,
            _ => self.starts[k],
        })
    }

    /// The next frame's payload, moving on to the next segment at the end of this one.
    fn next_payload(&mut self) -> Option<Bytes> {
        loop {
            if let Some(f) = adts_at(&self.data, self.off) {
                self.off = f.next;
                return Some(self.data.slice(f.payload));
            }
            // A damaged frame: pick up at the next sync word, or go on to the next segment.
            let resync = (self.off + 1..self.data.len().saturating_sub(1)).find(|&i| self.data[i] == 0xFF && self.data[i + 1] & 0xF6 == 0xF0);
            match resync {
                Some(i) if self.off < self.data.len() => self.off = i,
                _ => {
                    if self.seg + 1 >= self.starts.len() {
                        return None;
                    }
                    self.load(self.seg + 1)?;
                }
            }
        }
    }

    /// Decode the next frame into `buf`; false at the end (or when the download failed).
    fn refill(&mut self) -> bool {
        loop {
            let Some(payload) = self.next_payload() else { return false };
            let packet = Packet::new_from_slice(0, 0, FRAME, &payload);
            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    let (spec, cap) = (*decoded.spec(), decoded.capacity() as u64);
                    let sb = match &mut self.samples {
                        Some(sb) if sb.capacity() >= cap as usize * spec.channels.count() => sb,
                        slot => slot.insert(SampleBuffer::new(cap, spec)),
                    };
                    sb.copy_interleaved_ref(decoded);
                    self.buf.clear();
                    self.buf.extend_from_slice(sb.samples());
                    let drop = self.skip.min(self.buf.len());
                    self.skip -= drop;
                    self.pos = drop;
                    if self.pos < self.buf.len() {
                        return true;
                    }
                }
                // A damaged frame: skip it rather than end the song.
                Err(e) => log::debug!("hls aac: {e}"),
            }
        }
    }
}

impl Iterator for HlsSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.pos >= self.buf.len() && !self.refill() {
            return None;
        }
        self.pos += 1;
        Some(self.buf[self.pos - 1])
    }
}

impl rodio::Source for HlsSource {
    /// A span per decoded frame, as `OpusSource` does (see there for why it ends).
    fn current_span_len(&self) -> Option<usize> {
        Some((self.buf.len() - self.pos).max(self.channels as usize))
    }
    fn channels(&self) -> rodio::ChannelCount {
        NonZero::new(self.channels).unwrap_or(NonZero::<u16>::MIN)
    }
    fn sample_rate(&self) -> rodio::SampleRate {
        NonZero::new(self.rate).unwrap_or(NonZero::<u32>::MIN)
    }
    fn total_duration(&self) -> Option<Duration> {
        self.duration
    }
    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        let t = pos.as_secs_f64();
        let failed = || rodio::source::SeekError::Other(Arc::new(std::io::Error::other("the download stopped")));
        let mut k = self.starts.partition_point(|&s| s <= t).saturating_sub(1);
        let mut start = self.load(k).ok_or_else(failed)?;
        // The playlist's times are rounded: the segment may really start just after `t`.
        if start > t && k > 0 {
            k -= 1;
            start = self.load(k).ok_or_else(failed)?;
        }
        // Each frame decodes on its own (only the overlap with the one before is lost,
        // 23 ms of a segment's start): decode from the segment's start, drop up to `t`.
        self.decoder.reset();
        self.buf.clear();
        self.pos = 0;
        self.skip = ((t - start).max(0.0) * self.rate as f64) as usize * self.channels as usize;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An ADTS frame of `payload` bytes: AAC-LC, 44.1 kHz stereo, no CRC.
    fn frame(payload: usize) -> Vec<u8> {
        let len = payload + 7;
        let mut f = vec![0xFF, 0xF1, (1 << 6) | (4 << 2), (2 << 6) | ((len >> 11) & 3) as u8, ((len >> 3) & 0xFF) as u8, (((len & 7) << 5) as u8) | 0x1F, 0xFC];
        f.resize(len, 0x21);
        f
    }

    fn id3_tag(pts: u64) -> Vec<u8> {
        let mut body = b"PRIV".to_vec();
        let content = [TIMESTAMP_OWNER, &pts.to_be_bytes()].concat();
        body.extend_from_slice(&(content.len() as u32).to_be_bytes());
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(&content);
        let n = body.len();
        let mut tag = vec![b'I', b'D', b'3', 4, 0, 0, (n >> 21) as u8 & 0x7F, (n >> 14) as u8 & 0x7F, (n >> 7) as u8 & 0x7F, n as u8 & 0x7F];
        tag.extend_from_slice(&body);
        tag
    }

    #[test]
    fn segment_header_and_frames_are_read() {
        let tag = id3_tag(632_032);
        let mut seg = tag.clone();
        seg.extend(frame(100));
        seg.extend(frame(50));
        let (off, ts) = id3(&seg);
        assert_eq!(off, tag.len());
        assert!((ts.unwrap() - 7.022_577).abs() < 1e-3, "{ts:?}");
        let a = adts_at(&seg, off).unwrap();
        assert_eq!((a.rate, a.channels, a.payload.len()), (44_100, 2, 100));
        let b = adts_at(&seg, a.next).unwrap();
        assert_eq!(b.payload.len(), 50);
        assert_eq!(b.next, seg.len());
        assert!(adts_at(&seg, b.next).is_none());
    }

    #[test]
    fn a_cut_off_frame_is_not_a_frame() {
        let f = frame(100);
        assert!(adts_at(&f[..f.len() - 1], 0).is_none());
        assert!(adts_at(&[0xFF, 0xF1, 0, 0], 0).is_none());
    }

    #[test]
    fn untagged_segments_start_at_their_first_byte() {
        assert_eq!(id3(&frame(10)), (0, None));
    }

    /// Signed out: plays a song's HLS audio end to end, and seeks in it.
    /// `cargo test --release -- --ignored hls_stream_live --nocapture`
    #[tokio::test]
    #[ignore]
    async fn hls_stream_live() {
        use rodio::Source;
        let api = crate::ytmapi::Api::new(&std::env::temp_dir().join("ytm-rs-hls-test"));
        let vd = api.visitor_data(false).await.unwrap();
        let http = reqwest::Client::new();
        let s = crate::innertube::hls_audio(&http, "4RzTAS6njw4", &vd).await.unwrap();
        let (starts, urls): (Vec<f64>, Vec<String>) = s.segments.into_iter().unzip();
        let shared = crate::stream::Shared::segmented(starts, s.duration_ms, s.loudness_db, false);
        let t = std::time::Instant::now();
        crate::stream::download(http, crate::stream::Media::Segments(urls), crate::innertube::UA.to_owned(), shared.clone(), None).await;
        assert!(shared.error().is_none(), "{:?}", shared.error());
        println!("downloaded in {:?}", t.elapsed());
        let mut d = crate::stream::decoder(shared.clone(), true).unwrap();
        let (ch, rate) = (d.channels().get() as usize, d.sample_rate().get() as usize);
        let (mut n, mut sq) = (0usize, 0f64);
        for x in d.by_ref() {
            n += 1;
            sq += (x as f64).powi(2);
        }
        let secs = n as f64 / (ch * rate) as f64;
        println!("{ch} ch, {rate} Hz, {secs:.2} s decoded of {} ms, rms {:.3}", s.duration_ms, (sq / n as f64).sqrt());
        assert!((secs - s.duration_ms as f64 / 1000.0).abs() < 0.2, "decoded {secs} s");
        assert!((sq / n as f64).sqrt() > 0.01, "silence");
        // A seek lands where it says: as much is left as the song has after it.
        let mut d = crate::stream::decoder(shared, true).unwrap();
        d.try_seek(Duration::from_secs(100)).unwrap();
        let left = d.count() as f64 / (ch * rate) as f64;
        println!("after a seek to 100 s: {left:.2} s left");
        assert!((left - (s.duration_ms as f64 / 1000.0 - 100.0)).abs() < 0.1, "{left} s left");
    }
}
