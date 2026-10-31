//! Opus in WebM (YouTube's itag 251, about 130-160 kbps against AAC's 128): Symphonia
//! reads the container and `rusty-opus` (pure Rust) decodes the packets. rodio's own
//! decoder only knows Symphonia's codecs, and Symphonia has no Opus.

use std::io::{Read, Seek, SeekFrom};
use std::num::NonZero;
use std::sync::Arc;
use std::time::Duration;

use rusty_opus::OpusDecoder;
use symphonia::core::codecs::CODEC_TYPE_OPUS;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
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

/// The buffer as a Symphonia source: seekable, with its full length known.
struct Media {
    reader: StreamReader,
    len: u64,
}

impl Read for Media {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf)
    }
}

impl Seek for Media {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.reader.seek(pos)
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
}

impl OpusSource {
    /// Reads the WebM header and its seek index (YouTube puts both at the start), so
    /// it opens after the first block whether or not it'll be seeked.
    pub fn new(reader: StreamReader, len: u64, duration_ms: u64) -> anyhow::Result<Self> {
        let mss = MediaSourceStream::new(Box::new(Media { reader, len }), Default::default());
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
        })
    }

    /// Decode the next packet into `buf`; false at the end (or when the download failed).
    fn refill(&mut self) -> bool {
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(_) => return false,
            };
            if packet.track_id() != self.track {
                continue;
            }
            let ch = self.channels as usize;
            self.buf.resize(MAX_FRAME * ch, 0.0);
            match self.decoder.decode(&packet.data, MAX_FRAME, &mut self.buf) {
                Ok(n) => {
                    self.buf.truncate(n * ch);
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
    fn current_span_len(&self) -> Option<usize> {
        None
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
        self.skip = (ahead * RATE as f64) as usize * self.channels as usize;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use rodio::Source;

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
                crate::stream::download(http.clone(), s.url, crate::innertube::UA.to_owned(), shared.clone(), None).await;
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
