//! Playback speed, applied to each song on its way to the mixer.
//!
//! With the pitch kept (the default), the song is time-stretched by overlap-add (WSOLA,
//! as browsers do for a video's speed): short overlapping windows of it are laid down
//! closer together or further apart, each one taken from near where it should be so it
//! lines up with the last, and the tempo changes but the pitch doesn't. Without, it's
//! resampled, like a record played faster: the pitch goes with it, and a band-limited
//! filter keeps it clean at any speed. At 1x, samples come out unchanged either way.
//!
//! Transpose moves the pitch without the tempo: the song is
//! stretched by the speed over the pitch's shift, then resampled by the shift, which
//! brings the tempo back to the speed and takes the pitch up or down by it. Two
//! semitones up at 1x: stretched to 0.89x, then resampled 1.12x faster.
//!
//! `Tempo` counts the song's own position as it goes, so lyrics, the progress bar and
//! seeking stay on the song's clock whatever the speed.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

pub const MIN_SPEED: f32 = 0.5;
pub const MAX_SPEED: f32 = 2.0;
/// Transpose, in semitones either way: an octave.
pub const MAX_SEMITONES: i32 = 12;
/// The most the resampler ever shifts by: the top speed without keeping the pitch,
/// transposed an octave up.
const MAX_RATIO: f32 = MAX_SPEED * 2.0;

/// The listener's speed and pitch, read by every song's `Tempo`.
pub struct Speed {
    /// f32 bits.
    bits: AtomicU32,
    keep_pitch: AtomicBool,
    /// The transpose, as a ratio (f32 bits): 1 leaves it.
    pitch: AtomicU32,
}

impl Default for Speed {
    fn default() -> Self {
        Self { bits: AtomicU32::new(1f32.to_bits()), keep_pitch: AtomicBool::new(true), pitch: AtomicU32::new(1f32.to_bits()) }
    }
}

/// How much higher (above 1) or lower `semitones` make the song: exactly 1 at 0, which
/// costs nothing.
pub fn pitch_ratio(semitones: i32) -> f32 {
    2f64.powf(semitones.clamp(-MAX_SEMITONES, MAX_SEMITONES) as f64 / 12.0) as f32
}

/// The lowest transpose that's heard as asked at `speed`: fast with the pitch kept, the
/// stretch can't go further (see `Speed::stages`), so none at 2x and five down at 1.5x.
/// Within a couple of cents, so 1.5x still offers the five it nearly reaches.
pub fn lowest_semitones(speed: f32, keep_pitch: bool) -> i32 {
    if !keep_pitch {
        return -MAX_SEMITONES;
    }
    let lowest = (12.0 * (speed / MAX_SPEED).log2() - 0.05).ceil() as i32;
    lowest.clamp(-MAX_SEMITONES, 0)
}

impl Speed {
    /// Rounded, so a slider's steps land on exactly 1x, which costs nothing.
    pub fn set(&self, speed: f32) {
        let speed = (speed.clamp(MIN_SPEED, MAX_SPEED) * 1000.0).round() / 1000.0;
        self.bits.store(speed.to_bits(), Ordering::Relaxed);
    }

    pub fn get(&self) -> f32 {
        f32::from_bits(self.bits.load(Ordering::Relaxed))
    }

    /// Faster or slower without sounding higher or lower; off, like a record.
    pub fn set_keep_pitch(&self, on: bool) {
        self.keep_pitch.store(on, Ordering::Relaxed);
    }

    pub fn keeps_pitch(&self) -> bool {
        self.keep_pitch.load(Ordering::Relaxed)
    }

    /// Higher or lower by `semitones`, at any speed.
    pub fn set_pitch(&self, semitones: i32) {
        self.pitch.store(pitch_ratio(semitones).to_bits(), Ordering::Relaxed);
    }

    pub fn pitch(&self) -> f32 {
        f32::from_bits(self.pitch.load(Ordering::Relaxed))
    }

    /// What the song goes through: a stretch by the first (its tempo alone), then a
    /// resample by the second (tempo and pitch, like a record). Together they make the
    /// speed's tempo at the pitch asked for. Each is exactly 1 when it has nothing to do.
    fn stages(&self) -> (f32, f32) {
        let speed = self.get();
        let resample = if self.keeps_pitch() { 1.0 } else { speed } * self.pitch();
        // The stretch skips over the song past the top speed: each window would come from
        // further on than the last one's end, dropping what's between (a drum hit, even).
        // Fast and transposed down, the tempo wins and the pitch drops only as far as that
        // allows: none at 2x, five semitones at 1.5x.
        let resample = resample.max(speed / MAX_SPEED);
        (speed / resample, resample)
    }
}

/// Each window the stretch lays down, in seconds; they overlap by half. Long enough to
/// hold a low note's cycle, short enough not to smear a beat.
const STRETCH_WINDOW: f64 = 0.02;
/// How far either side of where a window should come from it may be taken from, to
/// line up with the one before.
const STRETCH_SEARCH: f64 = 0.015;
/// Every this many frames and offsets in the rough first search for the best line-up.
const COARSE: usize = 4;

/// The time-stretch's sizes and state, for one song.
struct Stretch {
    /// Frames in a window, and the hop between them (half).
    n: usize,
    h: usize,
    /// Frames it may look either way.
    s: usize,
    /// A periodic Hann window: its two halves add up to 1, so overlapping ones do too.
    hann: Vec<f32>,
    /// The second half of the last window laid down, already weighted: the first half
    /// of the next one goes over it.
    tail: Vec<f32>,
    /// Where in `buf` the song carries on from the last window: what the next one
    /// should sound like, to fit.
    natural: usize,
    /// Running, rather than resampling: `tail` and `natural` hold.
    on: bool,
}

impl Stretch {
    fn new(rate: u32, ch: usize) -> Self {
        let n = ((rate as f64 * STRETCH_WINDOW) as usize).max(64) & !1;
        let s = (rate as f64 * STRETCH_SEARCH) as usize;
        let hann = (0..n).map(|i| (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos()) as f32).collect();
        Self { n, h: n / 2, s, hann, tail: vec![0.0; n / 2 * ch], natural: 0, on: false }
    }

    /// Silence kept past the song's end, for windows reaching over it.
    fn pad(&self) -> usize {
        2 * (self.n + self.s)
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
    ratio: f32,
    /// Frames it reaches on each side.
    half: usize,
    table: Vec<f32>,
}

impl Kernel {
    fn new() -> Self {
        // Room for the widest, at the top ratio: changing speed or pitch never allocates.
        let widest = ZEROS / (0.95 / MAX_RATIO as f64);
        let mut k = Self { ratio: 0.0, half: 0, table: Vec::with_capacity((widest * RES as f64).ceil() as usize + 2) };
        k.set(1.0);
        k
    }

    fn set(&mut self, ratio: f32) {
        let cutoff = (1.0 / ratio as f64).min(1.0) * 0.95;
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
        self.ratio = ratio;
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
const HISTORY: usize = 48;
const _: () = assert!(HISTORY as f64 > ZEROS / (0.95 / MAX_RATIO as f64) + 1.0, "HISTORY must cover the filter at MAX_RATIO");
/// Frames read from the song at a time.
const CHUNK: usize = 256;
/// Frames played before letting go of them.
const DRAIN: usize = 8192;
/// Frames worked out at a time. Each is a span to rodio, which checks the format
/// between them as it did between a decoder's packets: so at a song's end too, for
/// the next one's.
const BLOCK: usize = 1024;

/// A song at the listener's speed and pitch.
pub struct Tempo<S> {
    inner: S,
    speed: Arc<Speed>,
    /// The song's format when it started: one for its whole length.
    channels: rodio::ChannelCount,
    rate: rodio::SampleRate,
    /// The song's position (ms) at the frame last handed out.
    pos: Arc<AtomicU64>,
    /// Read from the song, for the stretch: interleaved frames.
    buf: Vec<f32>,
    /// The song's frame at `buf`'s first (before the start, for the history).
    first: i64,
    /// Where the song carries on from, in frames into `buf`.
    t: f64,
    /// Where in `buf` the song ends, once it has.
    end: Option<usize>,
    stretch: Stretch,
    /// The song as stretched, for the resampler: interleaved frames, and the song's
    /// position (ms) at each.
    mid: Vec<f32>,
    mid_ms: Vec<f64>,
    /// Where the next frame resampled is, in frames into `mid`.
    mid_t: f64,
    /// Where in `mid` the song ends, once it has.
    mid_end: Option<usize>,
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
        let stretch = Stretch::new(rate.get(), ch);
        // Room for what the stretch keeps either side of where it's up to, too.
        let reach = 4 * (stretch.n + stretch.s) + stretch.pad();
        let mut t = Self {
            inner,
            speed,
            channels,
            rate,
            pos,
            // All they'll hold, so nothing allocates on the device's thread.
            buf: Vec::with_capacity((DRAIN + CHUNK + 4 * HISTORY + reach) * ch),
            first: 0,
            t: 0.0,
            end: None,
            // What the resampler keeps, and what a stretch's hop or a run of the song
            // as it is adds past what it asked for.
            mid: Vec::with_capacity((DRAIN + CHUNK + 4 * HISTORY + stretch.n) * ch),
            mid_ms: Vec::with_capacity(DRAIN + CHUNK + 4 * HISTORY + stretch.n),
            mid_t: 0.0,
            mid_end: None,
            kernel: Kernel::new(),
            out: Vec::with_capacity(BLOCK * ch),
            stretch,
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
        let ch = self.ch();
        self.buf.clear();
        self.buf.resize(HISTORY * ch, 0.0);
        self.first = (at.as_secs_f64() * self.rate.get() as f64).round() as i64 - HISTORY as i64;
        self.t = HISTORY as f64;
        self.end = None;
        self.stretch.on = false;
        // The resampler's history: silence, as before the song's start.
        self.mid.clear();
        self.mid.resize(HISTORY * ch, 0.0);
        self.mid_ms.clear();
        self.mid_ms.resize(HISTORY, at.as_secs_f64() * 1000.0);
        self.mid_t = HISTORY as f64;
        self.mid_end = None;
        self.pos.store(at.as_millis() as u64, Ordering::Relaxed);
        self.fill();
    }

    fn frames(&self) -> usize {
        self.buf.len() / self.ch()
    }

    fn mid_frames(&self) -> usize {
        self.mid.len() / self.ch()
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

    /// The song's next frames, stretched as far as the resampler needs, onto `mid`.
    /// False once the song's over.
    fn stretched(&mut self) -> bool {
        let (stretch, _) = self.speed.stages();
        if stretch != 1.0 {
            return self.stretch_hop(stretch);
        }
        // Back from the stretch: on from where its last window left off, which is near
        // where the song's clock says, not exactly there.
        if std::mem::take(&mut self.stretch.on) {
            self.t = self.stretch.natural as f64;
        }
        self.pass_frames()
    }

    /// The song's next frames as they are onto `mid`. False once it's over.
    fn pass_frames(&mut self) -> bool {
        // Whole frames: a stretch or a seek can leave it between two.
        self.t = self.t.round();
        let from = self.t as usize;
        self.read_to(from + 1);
        let have = self.end.unwrap_or_else(|| self.frames());
        if from >= have {
            return false;
        }
        let (ch, to) = (self.ch(), have.min(from + CHUNK));
        self.mid.extend_from_slice(&self.buf[from * ch..to * ch]);
        let (first, rate) = (self.first as f64, self.rate.get() as f64);
        self.mid_ms.extend((from..to).map(|f| (first + f as f64) * 1000.0 / rate));
        self.t = to as f64;
        // Let go of what's gone on, keeping the history.
        let done = to.saturating_sub(HISTORY);
        if done >= DRAIN {
            self.buf.drain(..done * ch);
            self.t -= done as f64;
            self.first += done as i64;
            self.end = self.end.map(|e| e - done);
        }
        true
    }

    /// Stretches on until `mid` holds `need` frames, or the song's over (and padded with
    /// silence past its end).
    fn mid_to(&mut self, need: usize) {
        while self.mid_frames() < need && self.mid_end.is_none() {
            if !self.stretched() {
                let n = self.mid_frames();
                self.mid_end = Some(n);
                let last = self.mid_ms.last().copied().unwrap_or(0.0);
                self.mid.resize(self.mid.len() + HISTORY * self.ch(), 0.0);
                self.mid_ms.resize(n + HISTORY, last);
            }
        }
    }

    /// Reads on until `buf` holds `need` frames, or the song's over (and padded with
    /// silence past its end).
    fn read_to(&mut self, need: usize) {
        while self.frames() < need && self.end.is_none() {
            if !self.read() {
                self.end = Some(self.frames());
                let pad = HISTORY.max(self.stretch.pad());
                self.buf.resize(self.buf.len() + pad * self.ch(), 0.0);
            }
        }
    }

    /// The next hop of the song, time-stretched by `speed`, onto `mid`. False once the
    /// song's over.
    fn stretch_hop(&mut self, speed: f32) -> bool {
        let (ch, n, h, s) = (self.ch(), self.stretch.n, self.stretch.h, self.stretch.s);
        let target = self.t.round() as usize;
        if !self.stretch.on {
            // Starting (or back from resampling): as if the last window had come from
            // just before here, so the first one carries straight on.
            self.read_to(target + n + 1);
            self.stretch.natural = target;
            for j in 0..h {
                for c in 0..ch {
                    self.stretch.tail[j * ch + c] = self.buf.get((target + j) * ch + c).copied().unwrap_or(0.0) * self.stretch.hann[h + j];
                }
            }
            self.stretch.on = true;
        }
        let natural = self.stretch.natural;
        self.read_to(target.max(natural) + s + n + 1);
        if self.end.is_some_and(|end| target >= end) {
            return false;
        }
        let best = self.line_up(target, natural);
        // The first half of this window over the last one's second half.
        let st = &mut self.stretch;
        for j in 0..h {
            for c in 0..ch {
                let k = j * ch + c;
                self.mid.push(st.tail[k] + self.buf[(best + j) * ch + c] * st.hann[j]);
                st.tail[k] = self.buf[(best + h + j) * ch + c] * st.hann[h + j];
            }
        }
        // On the song's clock: the hop covers `h × speed` of it.
        let rate = self.rate.get() as f64;
        let (first, t) = (self.first as f64, self.t);
        self.mid_ms.extend((0..h).map(|j| (first + t + j as f64 * speed as f64) * 1000.0 / rate));
        self.t += h as f64 * speed as f64;
        self.stretch.natural = best + h;
        // Let go of what's played, keeping what the search and the next window reach back to.
        let done = (self.t as usize).min(self.stretch.natural).saturating_sub(s + HISTORY);
        if done >= DRAIN {
            self.buf.drain(..done * ch);
            self.t -= done as f64;
            self.first += done as i64;
            self.end = self.end.map(|e| e - done);
            self.stretch.natural -= done;
        }
        true
    }

    /// Where near `target` a window sounds most like the song carrying on from
    /// `natural`: normalised cross-correlation, roughly over every few frames first, then
    /// exactly around the best of those.
    fn line_up(&self, target: usize, natural: usize) -> usize {
        let (ch, n, s) = (self.ch(), self.stretch.n, self.stretch.s);
        let mono = |f: usize| -> f32 { self.buf[f * ch..(f + 1) * ch].iter().sum() };
        let score = |c: usize, step: usize| -> f32 {
            let (mut xc, mut e) = (0.0f32, 0.0f32);
            for i in (0..n).step_by(step) {
                let a = mono(c + i);
                xc += a * mono(natural + i);
                e += a * a;
            }
            if e > 1e-9 { xc / e.sqrt() } else { f32::MIN }
        };
        let (lo, hi) = (target.saturating_sub(s), target + s);
        let best_of = |from: usize, to: usize, by: usize, step: usize| {
            (from..=to).step_by(by).map(|c| (c, score(c, step))).fold((target, f32::MIN), |b, x| if x.1 > b.1 { x } else { b })
        };
        let (rough, sc) = best_of(lo, hi, COARSE, COARSE);
        if sc == f32::MIN {
            // Silence: anywhere fits.
            return target;
        }
        best_of(rough.saturating_sub(COARSE - 1).max(lo), (rough + COARSE - 1).min(hi), 1, 1).0
    }

    /// The next frame, resampled to the pitch (and, not keeping it, the speed), onto
    /// `out`. False once the song's over.
    fn next_frame(&mut self) -> bool {
        let (_, ratio) = self.speed.stages();
        if ratio == 1.0 {
            self.mid_t = self.mid_t.round();
        } else if self.kernel.ratio != ratio {
            self.kernel.set(ratio);
        }
        let half = if ratio == 1.0 { 0 } else { self.kernel.half };
        // Silence past the end, for the filter to reach over.
        self.mid_to(self.mid_t as usize + half + 1);
        if self.mid_end.is_some_and(|end| self.mid_t >= end as f64) {
            return false;
        }
        let (ch, start, base) = (self.ch(), self.out.len(), self.mid_t as usize);
        if ratio == 1.0 {
            self.out.extend_from_slice(&self.mid[base * ch..(base + 1) * ch]);
        } else {
            self.out.resize(start + ch, 0.0);
            let frame = &mut self.out[start..];
            let mut sum = 0.0;
            for k in base + 1 - half..=base + half {
                let w = self.kernel.at(k as f64 - self.mid_t);
                sum += w;
                for (o, s) in frame.iter_mut().zip(&self.mid[k * ch..(k + 1) * ch]) {
                    *o += w * s;
                }
            }
            // So the filter's table can't leave a hum or a level change.
            if sum != 0.0 {
                frame.iter_mut().for_each(|s| *s /= sum);
            }
        }
        // On the song's clock: between the two frames it falls between.
        let a = self.mid_ms[base];
        let b = self.mid_ms.get(base + 1).copied().unwrap_or(a);
        let ms = a + (b - a) * (self.mid_t - base as f64);
        self.out_ms.push(ms.max(0.0) as u64);
        self.mid_t += ratio as f64;
        // Let go of what's played, keeping the history.
        let done = (self.mid_t as usize).saturating_sub(HISTORY);
        if done >= DRAIN {
            self.mid.drain(..done * ch);
            self.mid_ms.drain(..done);
            self.mid_t -= done as f64;
            self.mid_end = self.mid_end.map(|e| e - done);
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

    /// Resampling, like a record.
    fn tempo(src: rodio::buffer::SamplesBuffer, at: f32) -> (Tempo<rodio::buffer::SamplesBuffer>, Arc<Speed>, Arc<AtomicU64>) {
        tempo_with(src, at, false)
    }

    fn tempo_with(src: rodio::buffer::SamplesBuffer, at: f32, keep_pitch: bool) -> (Tempo<rodio::buffer::SamplesBuffer>, Arc<Speed>, Arc<AtomicU64>) {
        let speed = Arc::new(Speed::default());
        speed.set(at);
        speed.set_keep_pitch(keep_pitch);
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

    #[test]
    fn keeping_the_pitch_plays_faster_or_slower_at_the_same_pitch() {
        for (at, secs) in [(1.5, 3.0), (0.75, 2.0), (2.0, 4.0), (0.5, 1.5)] {
            let (t, _, pos) = tempo_with(sine(440.0, secs), at, true);
            let out: Vec<f32> = t.collect();
            let played = out.len() as f32 / 2.0 / RATE as f32;
            assert!((played - secs / at).abs() < 0.03, "{at}x: {played} s");
            let left = left(&out);
            let mid = &left[left.len() / 4..left.len() * 3 / 4];
            assert!((hz(mid) - 440.0).abs() < 3.0, "{at}x: {} Hz", hz(mid));
            assert!(pos.load(Ordering::Relaxed) as f32 >= secs * 1000.0 - 30.0, "{at}x: {}", pos.load(Ordering::Relaxed));
            // Windows that line up add back to the level that went in, without clicks.
            assert!(mid.iter().all(|s| s.abs() < 0.52), "{at}x: louder");
            let step = std::f32::consts::TAU * 440.0 / RATE as f32 * 0.5;
            let worst = mid.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
            assert!(worst < step * 1.3, "{at}x: {worst} vs {step}");
        }
    }

    #[test]
    fn keeping_the_pitch_at_normal_speed_passes_through_untouched() {
        let (t, _, _) = tempo_with(sine(440.0, 1.0), 1.0, true);
        let want: Vec<f32> = sine(440.0, 1.0).collect();
        assert_eq!(t.collect::<Vec<_>>(), want);
    }

    #[test]
    fn switching_pitch_keeping_mid_song_doesnt_click() {
        let (mut t, speed, _) = tempo_with(sine(440.0, 3.0), 1.0, true);
        let mut out: Vec<f32> = t.by_ref().take(RATE as usize).collect();
        speed.set(1.25);
        out.extend(t.by_ref().take(RATE as usize));
        speed.set_keep_pitch(false);
        out.extend(t.by_ref().take(RATE as usize / 2));
        speed.set_keep_pitch(true);
        out.extend(t.by_ref().take(RATE as usize / 2));
        speed.set(1.0);
        out.extend(t);
        let step = std::f32::consts::TAU * 440.0 * 1.25 / RATE as f32 * 0.5;
        let worst = left(&out).windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        assert!(worst < step * 1.5, "{worst} vs {step}");
    }

    /// A song at `at` speed, `semitones` up (or down).
    fn shifted(src: rodio::buffer::SamplesBuffer, at: f32, keep_pitch: bool, semitones: i32) -> (Tempo<rodio::buffer::SamplesBuffer>, Arc<Speed>, Arc<AtomicU64>) {
        let speed = Arc::new(Speed::default());
        speed.set(at);
        speed.set_keep_pitch(keep_pitch);
        speed.set_pitch(semitones);
        let pos = Arc::new(AtomicU64::new(0));
        (Tempo::new(src, speed.clone(), Duration::ZERO, pos.clone()), speed, pos)
    }

    #[test]
    fn pitch_ratio_is_exactly_one_untouched_and_an_octave_doubles() {
        assert_eq!(pitch_ratio(0), 1.0);
        assert_eq!(pitch_ratio(12), 2.0);
        assert_eq!(pitch_ratio(-12), 0.5);
        assert!((pitch_ratio(7) - 1.498_307).abs() < 1e-5);
        // Clamped to an octave either way.
        assert_eq!(pitch_ratio(40), 2.0);
    }

    #[test]
    fn the_stretch_never_goes_past_the_top_speed_and_the_tempo_holds() {
        for keep in [true, false] {
            for at in [0.5, 1.0, 1.5, 2.0] {
                for semitones in -12..=12 {
                    let speed = Speed::default();
                    speed.set(at);
                    speed.set_keep_pitch(keep);
                    speed.set_pitch(semitones);
                    let (stretch, resample) = speed.stages();
                    assert!(stretch <= MAX_SPEED + 1e-6, "{at}x {keep} {semitones:+}: {stretch}");
                    assert!((stretch * resample - at).abs() < 1e-5, "{at}x {keep} {semitones:+}");
                }
            }
        }
        // As far down as the stretch allows: none at 2x.
        let speed = Speed::default();
        speed.set(2.0);
        speed.set_pitch(-12);
        assert_eq!(speed.stages(), (2.0, 1.0));
    }

    #[test]
    fn the_lowest_transpose_offered_is_one_that_is_heard() {
        assert_eq!(lowest_semitones(2.0, true), 0);
        assert_eq!(lowest_semitones(1.5, true), -5);
        assert_eq!(lowest_semitones(1.0, true), -12);
        assert_eq!(lowest_semitones(2.0, false), -12);
        for step in 0..=30 {
            let at = MIN_SPEED + step as f32 * 0.05;
            let lowest = lowest_semitones(at, true);
            let speed = Speed::default();
            speed.set(at);
            speed.set_pitch(lowest);
            // Within a few cents of what's asked; one lower wouldn't be.
            let cents = |s: &Speed, semitones| 1200.0 * (s.stages().1 / pitch_ratio(semitones)).log2();
            assert!(cents(&speed, lowest).abs() < 5.0, "{at}x {lowest}: {} cents", cents(&speed, lowest));
            if lowest > -MAX_SEMITONES {
                speed.set_pitch(lowest - 1);
                assert!(cents(&speed, lowest - 1) > 5.0, "{at}x {}", lowest - 1);
            }
        }
    }

    #[test]
    fn transposing_moves_the_pitch_and_keeps_the_tempo() {
        for semitones in [12, -12, 7, -5, 1, -1] {
            let secs = 2.0;
            let (t, _, pos) = shifted(sine(440.0, secs), 1.0, true, semitones);
            let out: Vec<f32> = t.collect();
            let played = out.len() as f32 / 2.0 / RATE as f32;
            assert!((played - secs).abs() < 0.03, "{semitones:+}: {played} s");
            let want = 440.0 * 2f32.powf(semitones as f32 / 12.0);
            let left = left(&out);
            let mid = &left[left.len() / 4..left.len() * 3 / 4];
            assert!((hz(mid) - want).abs() < want * 0.01, "{semitones:+}: {} Hz, want {want}", hz(mid));
            assert!(pos.load(Ordering::Relaxed) as f32 >= secs * 1000.0 - 30.0, "{semitones:+}: {}", pos.load(Ordering::Relaxed));
            assert!(mid.iter().all(|s| s.abs() < 0.55), "{semitones:+}: louder");
        }
    }

    #[test]
    fn speed_and_transpose_together() {
        // (speed, keep the pitch, semitones, the pitch that comes out)
        let cases = [
            (1.5, true, 3, 440.0 * 2f32.powf(3.0 / 12.0)),
            (0.75, true, -2, 440.0 * 2f32.powf(-2.0 / 12.0)),
            (2.0, true, 12, 880.0),
            (0.5, true, -12, 220.0),
            // Like a record and transposed on top.
            (1.25, false, 2, 440.0 * 1.25 * 2f32.powf(2.0 / 12.0)),
            (0.5, false, 12, 440.0),
        ];
        for (at, keep, semitones, want) in cases {
            let secs = 3.0;
            let (t, _, pos) = shifted(sine(440.0, secs), at, keep, semitones);
            let out: Vec<f32> = t.collect();
            let played = out.len() as f32 / 2.0 / RATE as f32;
            assert!((played - secs / at).abs() < 0.04, "{at}x {semitones:+}: {played} s");
            let left = left(&out);
            let mid = &left[left.len() / 4..left.len() * 3 / 4];
            assert!((hz(mid) - want).abs() < want * 0.01, "{at}x {semitones:+}: {} Hz, want {want}", hz(mid));
            assert!(pos.load(Ordering::Relaxed) as f32 >= secs * 1000.0 - 40.0, "{at}x {semitones:+}: {}", pos.load(Ordering::Relaxed));
        }
    }

    #[test]
    fn transposing_mid_song_keeps_the_clock_and_doesnt_click() {
        let (mut t, speed, pos) = shifted(sine(440.0, 4.0), 1.0, true, 0);
        let mut out: Vec<f32> = t.by_ref().take(RATE as usize).collect();
        speed.set_pitch(4);
        out.extend(t.by_ref().take(RATE as usize));
        speed.set_pitch(-3);
        out.extend(t.by_ref().take(RATE as usize));
        // A second and a half in (stereo: `RATE` samples is half a second), a second and a
        // half of song: the tempo stayed 1x throughout.
        let at = pos.load(Ordering::Relaxed);
        assert!((1480..=1510).contains(&at), "{at}");
        speed.set_pitch(0);
        out.extend(t);
        let played = out.len() as f32 / 2.0 / RATE as f32;
        assert!((played - 4.0).abs() < 0.04, "{played} s");
        let step = std::f32::consts::TAU * 440.0 * 2f32.powf(4.0 / 12.0) / RATE as f32 * 0.5;
        let worst = left(&out).windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        assert!(worst < step * 1.5, "{worst} vs {step}");
    }

    #[test]
    fn a_seek_while_transposed_moves_the_clock_there() {
        use rodio::Source;
        let (mut t, _, pos) = shifted(sine(440.0, 3.0), 1.0, true, 5);
        let _: Vec<f32> = t.by_ref().take(20_000).collect();
        t.try_seek(Duration::from_secs(2)).unwrap();
        assert_eq!(pos.load(Ordering::Relaxed), 2000);
        let rest = t.count() as f32 / 2.0 / RATE as f32;
        assert!((rest - 1.0).abs() < 0.03, "{rest}");
    }

    #[test]
    fn a_seek_while_keeping_the_pitch_moves_the_clock_there() {
        use rodio::Source;
        let (mut t, _, pos) = tempo_with(sine(440.0, 3.0), 1.25, true);
        let _: Vec<f32> = t.by_ref().take(20_000).collect();
        t.try_seek(Duration::from_secs(2)).unwrap();
        assert_eq!(pos.load(Ordering::Relaxed), 2000);
        let rest = t.count() as f32 / 2.0 / RATE as f32;
        assert!((rest - 0.8).abs() < 0.03, "{rest}");
    }
}
