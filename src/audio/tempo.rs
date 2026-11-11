//! Playback speed, applied to each song on its way to the mixer.
//!
//! The song is resampled, like a record played faster: the pitch goes with it, and a
//! band-limited filter keeps it clean at any speed. At 1x, samples come out unchanged.
//!
//! `Tempo` counts the song's own position as it goes, so lyrics, the progress bar and
//! seeking stay on the song's clock whatever the speed.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

pub const MIN_SPEED: f32 = 0.5;
pub const MAX_SPEED: f32 = 2.0;

/// The listener's speed, read by every song's `Tempo`.
pub struct Speed(
    /// f32 bits.
    AtomicU32,
);

impl Default for Speed {
    fn default() -> Self {
        Self(AtomicU32::new(1f32.to_bits()))
    }
}

impl Speed {
    /// Rounded, so a slider's steps land on exactly 1x, which costs nothing.
    pub fn set(&self, speed: f32) {
        let speed = (speed.clamp(MIN_SPEED, MAX_SPEED) * 1000.0).round() / 1000.0;
        self.0.store(speed.to_bits(), Ordering::Relaxed);
    }

    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
}

/// Zero crossings of the resampling filter on each side. It rolls off over the top few
/// kHz, so what still folds back when faster lands up there, quiet.
const ZEROS: f64 = 8.0;
/// Points per frame in the filter's table.
const RES: usize = 64;

/// A windowed-sinc low-pass to resample through, cutting off under what speeding up
/// would fold back over the top of the range.
struct Kernel {
    speed: f32,
    /// Frames it reaches on each side.
    half: usize,
    table: Vec<f32>,
}

impl Kernel {
    fn new() -> Self {
        // Room for the widest, at the top speed: changing speed never allocates.
        let widest = ZEROS / (0.95 / MAX_SPEED as f64);
        let mut k = Self { speed: 0.0, half: 0, table: Vec::with_capacity((widest * RES as f64).ceil() as usize + 2) };
        k.set(1.0);
        k
    }

    fn set(&mut self, speed: f32) {
        let cutoff = (1.0 / speed as f64).min(1.0) * 0.95;
        let width = ZEROS / cutoff;
        let sinc = |x: f64| if x == 0.0 { 1.0 } else { (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x) };
        // Blackman, from its middle at 0 to its edge at `width`.
        let window = |x: f64| {
            let u = std::f64::consts::PI * x / width;
            0.42 + 0.5 * u.cos() + 0.08 * (2.0 * u).cos()
        };
        self.table.clear();
        self.table.extend(
            (0..=(width * RES as f64).ceil() as usize + 1)
                .map(|i| i as f64 / RES as f64)
                .map(|x| if x < width { cutoff * sinc(cutoff * x) * window(x) } else { 0.0 } as f32),
        );
        self.speed = speed;
        self.half = width.ceil() as usize;
    }

    fn at(&self, x: f64) -> f32 {
        let p = x.abs() * RES as f64;
        let i = p as usize;
        match (self.table.get(i), self.table.get(i + 1)) {
            (Some(a), Some(b)) => a + (b - a) * (p - i as f64) as f32,
            _ => 0.0,
        }
    }
}

/// Frames kept before the one going out, for the filter to reach back over.
const HISTORY: usize = 32;
const _: () = assert!(HISTORY as f64 > ZEROS / (0.95 / MAX_SPEED as f64) + 1.0, "HISTORY must cover the filter at MAX_SPEED");
/// Frames read from the song at a time.
const CHUNK: usize = 256;
/// Frames played before letting go of them.
const DRAIN: usize = 8192;
/// Frames worked out at a time. Each is a span to rodio, which checks the format
/// between them as it did between a decoder's packets: so at a song's end too, for
/// the next one's.
const BLOCK: usize = 1024;

/// A song at the listener's speed.
pub struct Tempo<S> {
    inner: S,
    speed: Arc<Speed>,
    /// The song's format when it started: one for its whole length.
    channels: rodio::ChannelCount,
    rate: rodio::SampleRate,
    /// The song's position (ms) at the frame last handed out.
    pos: Arc<AtomicU64>,
    /// Read from the song, for the resampler: interleaved frames.
    buf: Vec<f32>,
    /// The song's frame at `buf`'s first (before the start, for the history).
    first: i64,
    /// Where the next frame worked out is, in frames into `buf`.
    t: f64,
    /// Where in `buf` the song ends, once it has.
    end: Option<usize>,
    kernel: Kernel,
    /// The block going out, the song's position (ms) at each of its frames, and how
    /// many samples of it have gone.
    out: Vec<f32>,
    out_ms: Vec<u64>,
    out_pos: usize,
}

impl<S: rodio::Source> Tempo<S> {
    /// `inner` starts `at` into the song; `pos` follows where it's up to.
    pub fn new(inner: S, speed: Arc<Speed>, at: Duration, pos: Arc<AtomicU64>) -> Self {
        let (channels, rate) = (inner.channels(), inner.sample_rate());
        let ch = channels.get() as usize;
        let mut t = Self {
            inner,
            speed,
            channels,
            rate,
            pos,
            // All they'll hold, so nothing allocates on the device's thread.
            buf: Vec::with_capacity((DRAIN + CHUNK + 4 * HISTORY) * ch),
            first: 0,
            t: 0.0,
            end: None,
            kernel: Kernel::new(),
            out: Vec::with_capacity(BLOCK * ch),
            out_ms: Vec::with_capacity(BLOCK),
            out_pos: 0,
        };
        t.reset(at);
        t
    }

    fn ch(&self) -> usize {
        self.channels.get() as usize
    }

    fn reset(&mut self, at: Duration) {
        self.buf.clear();
        self.buf.resize(HISTORY * self.ch(), 0.0);
        self.first = (at.as_secs_f64() * self.rate.get() as f64).round() as i64 - HISTORY as i64;
        self.t = HISTORY as f64;
        self.end = None;
        self.pos.store(at.as_millis() as u64, Ordering::Relaxed);
        self.fill();
    }

    fn frames(&self) -> usize {
        self.buf.len() / self.ch()
    }

    /// Reads the next frames of the song into `buf`. False once there are no more.
    fn read(&mut self) -> bool {
        let (before, ch) = (self.buf.len(), self.ch());
        self.buf.extend(self.inner.by_ref().take(CHUNK * ch));
        // A last frame missing some of its channels.
        self.buf.resize(self.buf.len().next_multiple_of(ch), 0.0);
        self.buf.len() > before
    }

    /// Works out the next block, empty once the song's over.
    fn fill(&mut self) {
        self.out.clear();
        self.out_ms.clear();
        self.out_pos = 0;
        while self.out_ms.len() < BLOCK && self.next_frame() {}
    }

    /// The next frame at the listener's speed onto `out`. False once the song's over.
    fn next_frame(&mut self) -> bool {
        let speed = self.speed.get();
        if speed == 1.0 {
            self.t = self.t.round();
        } else if self.kernel.speed != speed {
            self.kernel.set(speed);
        }
        let half = if speed == 1.0 { 0 } else { self.kernel.half };
        let need = self.t as usize + half + 1;
        while self.frames() < need && self.end.is_none() {
            if !self.read() {
                self.end = Some(self.frames());
                // Silence past the end, for the filter to reach over.
                self.buf.resize(self.buf.len() + HISTORY * self.ch(), 0.0);
            }
        }
        if self.end.is_some_and(|end| self.t >= end as f64) {
            return false;
        }
        let (ch, start, base) = (self.ch(), self.out.len(), self.t as usize);
        if speed == 1.0 {
            self.out.extend_from_slice(&self.buf[base * ch..(base + 1) * ch]);
        } else {
            self.out.resize(start + ch, 0.0);
            let frame = &mut self.out[start..];
            let mut sum = 0.0;
            for k in base + 1 - half..=base + half {
                let w = self.kernel.at(k as f64 - self.t);
                sum += w;
                for (o, s) in frame.iter_mut().zip(&self.buf[k * ch..(k + 1) * ch]) {
                    *o += w * s;
                }
            }
            // So the filter's table can't leave a hum or a level change.
            if sum != 0.0 {
                frame.iter_mut().for_each(|s| *s /= sum);
            }
        }
        let ms = (self.first as f64 + self.t) * 1000.0 / self.rate.get() as f64;
        self.out_ms.push(ms.max(0.0) as u64);
        self.t += speed as f64;
        // Let go of what's played, keeping the history.
        let done = (self.t as usize).saturating_sub(HISTORY);
        if done >= DRAIN {
            self.buf.drain(..done * ch);
            self.t -= done as f64;
            self.first += done as i64;
            self.end = self.end.map(|e| e - done);
        }
        true
    }
}

impl<S: rodio::Source> Iterator for Tempo<S> {
    type Item = rodio::Sample;
    fn next(&mut self) -> Option<Self::Item> {
        let sample = *self.out.get(self.out_pos)?;
        let ch = self.ch();
        if self.out_pos.is_multiple_of(ch) {
            self.pos.store(self.out_ms[self.out_pos / ch], Ordering::Relaxed);
        }
        self.out_pos += 1;
        // The next block at once, so the span after this one is known.
        if self.out_pos == self.out.len() {
            self.fill();
        }
        Some(sample)
    }
}

impl<S: rodio::Source> rodio::Source for Tempo<S> {
    /// What's left of the block, 0 once the song's over.
    fn current_span_len(&self) -> Option<usize> {
        Some(self.out.len() - self.out_pos)
    }
    fn channels(&self) -> rodio::ChannelCount {
        self.channels
    }
    fn sample_rate(&self) -> rodio::SampleRate {
        self.rate
    }
    /// On the song's clock, like the position.
    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        self.inner.try_seek(pos)?;
        self.reset(pos);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZero;

    const RATE: u32 = 48_000;

    /// `secs` of a stereo sine at `hz`.
    fn sine(hz: f32, secs: f32) -> rodio::buffer::SamplesBuffer {
        let n = (secs * RATE as f32) as usize;
        let samples = (0..n).flat_map(|i| {
            let s = (std::f32::consts::TAU * hz * i as f32 / RATE as f32).sin() * 0.5;
            [s, s]
        });
        rodio::buffer::SamplesBuffer::new(NonZero::new(2).unwrap(), NonZero::new(RATE).unwrap(), samples.collect::<Vec<_>>())
    }

    fn tempo(src: rodio::buffer::SamplesBuffer, at: f32) -> (Tempo<rodio::buffer::SamplesBuffer>, Arc<Speed>, Arc<AtomicU64>) {
        let speed = Arc::new(Speed::default());
        speed.set(at);
        let pos = Arc::new(AtomicU64::new(0));
        (Tempo::new(src, speed.clone(), Duration::ZERO, pos.clone()), speed, pos)
    }

    /// The pitch of the left channel, by counting upward zero crossings.
    fn hz(left: &[f32]) -> f32 {
        let ups = left.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        ups as f32 * RATE as f32 / left.len() as f32
    }

    fn left(samples: &[f32]) -> Vec<f32> {
        samples.iter().step_by(2).copied().collect()
    }

    #[test]
    fn at_normal_speed_the_song_passes_through_untouched() {
        let (t, _, pos) = tempo(sine(440.0, 1.0), 1.0);
        let want: Vec<f32> = sine(440.0, 1.0).collect();
        assert_eq!(t.collect::<Vec<_>>(), want);
        assert!(pos.load(Ordering::Relaxed) >= 999);
    }

    #[test]
    fn faster_plays_faster_and_higher_on_the_songs_clock() {
        let (t, _, pos) = tempo(sine(440.0, 2.0), 1.5);
        let out: Vec<f32> = t.collect();
        // Two seconds of song in 1.33 s.
        assert!((out.len() as f32 / 2.0 / RATE as f32 - 2.0 / 1.5).abs() < 0.01, "{}", out.len());
        let left = left(&out);
        assert!((hz(&left[4800..60000]) - 660.0).abs() < 3.0, "{}", hz(&left[4800..60000]));
        assert!(pos.load(Ordering::Relaxed) >= 1990, "{}", pos.load(Ordering::Relaxed));
        // Clean: nothing over the level that went in.
        assert!(out.iter().all(|s| s.abs() < 0.51));
    }

    #[test]
    fn slower_plays_lower() {
        let (t, _, _) = tempo(sine(440.0, 1.0), 0.5);
        let out: Vec<f32> = t.collect();
        assert!((out.len() as f32 / 2.0 / RATE as f32 - 2.0).abs() < 0.01);
        assert!((hz(&left(&out)[4800..90000]) - 220.0).abs() < 2.0);
    }

    #[test]
    fn changing_speed_keeps_the_clock_running_on() {
        let (mut t, speed, pos) = tempo(sine(440.0, 3.0), 1.0);
        let _: Vec<f32> = t.by_ref().take(RATE as usize * 2).collect();
        assert_eq!(pos.load(Ordering::Relaxed), 999);
        speed.set(2.0);
        // Half a second at 2x: another second of song, less up to a block already worked
        // out at 1x.
        let _: Vec<f32> = t.by_ref().take(RATE as usize).collect();
        let lag = (BLOCK * 1000 / RATE as usize) as u64;
        let at = pos.load(Ordering::Relaxed);
        assert!((1999 - lag - 2..=2001).contains(&at), "{at}");
    }

    #[test]
    fn spans_end_with_the_song_so_rodio_sees_the_next_ones_format() {
        use rodio::Source;
        let (mut t, _, _) = tempo(sine(440.0, 1.0), 1.5);
        let mut n = 0;
        while let Some(len) = t.current_span_len().filter(|&l| l > 0) {
            assert_eq!(len % 2, 0, "whole frames");
            assert_eq!(t.by_ref().take(len).count(), len);
            n += len;
        }
        assert!(t.is_exhausted() && t.next().is_none());
        assert!((n as f32 / 2.0 / RATE as f32 - 1.0 / 1.5).abs() < 0.01, "{n}");
    }

    #[test]
    fn changing_speed_mid_song_doesnt_click() {
        let (mut t, speed, _) = tempo(sine(440.0, 2.0), 1.0);
        let mut out: Vec<f32> = t.by_ref().take(RATE as usize).collect();
        speed.set(1.25);
        out.extend(t.by_ref().take(RATE as usize));
        speed.set(1.0);
        out.extend(t);
        // No jump bigger than the fastest sine's own steepest step, with a little room.
        let step = std::f32::consts::TAU * 440.0 * 1.25 / RATE as f32 * 0.5;
        let worst = left(&out).windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        assert!(worst < step * 1.2, "{worst} vs {step}");
    }

    #[test]
    fn a_seek_moves_the_clock_there() {
        use rodio::Source;
        let (mut t, _, pos) = tempo(sine(440.0, 3.0), 1.25);
        let _: Vec<f32> = t.by_ref().take(1000).collect();
        t.try_seek(Duration::from_secs(2)).unwrap();
        assert_eq!(pos.load(Ordering::Relaxed), 2000);
        t.next();
        assert!(pos.load(Ordering::Relaxed) >= 2000);
        // About a second of song left, at 1.25x.
        let rest = t.count() as f32 / 2.0 / RATE as f32;
        assert!((rest - 0.8).abs() < 0.01, "{rest}");
    }
}
