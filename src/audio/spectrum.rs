//! The visualizer's bars: how loud the playing song is in each of `BARS` bands, from a
//! band-pass filter per band (log-spaced, 40 Hz up), measured over ~21 ms windows and
//! smoothed like the level meter (fast up, slow down). Runs only while the visualizer
//! is on (see `Meter`).

use std::f32::consts::PI;
use std::sync::atomic::{AtomicU32, Ordering};

pub const BARS: usize = 24;
const LOW_HZ: f32 = 40.0;
const HIGH_HZ: f32 = 16_000.0;
/// Frames per measurement: about 21 ms at 48 kHz.
const WINDOW: u32 = 1024;
/// The quietest band shown, in dB below full scale: anything under it is an empty bar.
const FLOOR_DB: f32 = 60.0;

/// The bars, 0..1 each (f32 bits), for the UI to read.
pub type Bars = [AtomicU32; BARS];

/// A band-pass biquad (Audio EQ Cookbook, 0 dB peak gain).
#[derive(Clone, Copy, Default)]
struct BandPass {
    b0: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl BandPass {
    fn new(hz: f32, q: f32, rate: f32) -> Self {
        let w = 2.0 * PI * hz / rate;
        let alpha = w.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self { b0: alpha / a0, a1: -2.0 * w.cos() / a0, a2: (1.0 - alpha) / a0, ..Default::default() }
    }

    /// b1 is 0 and b2 is -b0 for this filter.
    fn run(&mut self, x: f32) -> f32 {
        let mut y = self.b0 * (x - self.x2) - self.a1 * self.y1 - self.a2 * self.y2;
        // Silence (pause) would otherwise ring it down into denormals, which are slow on x86.
        if y.abs() < 1e-20 {
            y = 0.0;
        }
        (self.x2, self.x1, self.y2, self.y1) = (self.x1, x, self.y1, y);
        y
    }
}

pub struct Spectrum {
    bands: [BandPass; BARS],
    power: [f32; BARS],
    shown: [f32; BARS],
    channels: usize,
    /// The frame being summed to mono, and how many of its samples are in.
    frame: f32,
    channel: usize,
    frames: u32,
}

impl Spectrum {
    pub fn new(rate: u32, channels: u16) -> Self {
        let rate = rate as f32;
        // Kept under 0.45 of the rate, where the filters stay stable (a 22.05 kHz device).
        let high = HIGH_HZ.min(rate * 0.45);
        let step = (high / LOW_HZ).powf(1.0 / (BARS - 1) as f32);
        // Each band as wide as the step between them.
        let q = step.sqrt() / (step - 1.0);
        let bands = std::array::from_fn(|i| BandPass::new(LOW_HZ * step.powi(i as i32), q, rate));
        Self { bands, power: [0.0; BARS], shown: [0.0; BARS], channels: channels.max(1) as usize, frame: 0.0, channel: 0, frames: 0 }
    }

    /// One interleaved sample in; every `WINDOW` frames, the bars out to `out`.
    pub fn push(&mut self, s: f32, out: &Bars) {
        self.frame += s;
        self.channel += 1;
        if self.channel < self.channels {
            return;
        }
        let x = self.frame / self.channels as f32;
        (self.frame, self.channel) = (0.0, 0);
        for (band, p) in self.bands.iter_mut().zip(&mut self.power) {
            let y = band.run(x);
            *p += y * y;
        }
        self.frames += 1;
        if self.frames < WINDOW {
            return;
        }
        self.frames = 0;
        for ((p, shown), out) in self.power.iter_mut().zip(&mut self.shown).zip(out) {
            // RMS in dB, over the floor, to 0..1; a sine at full scale reads about 1.
            let db = 10.0 * (*p / WINDOW as f32 * 2.0).max(1e-12).log10();
            let v = ((db + FLOOR_DB) / FLOOR_DB).clamp(0.0, 1.0);
            let k = if v > *shown { 0.6 } else { 0.15 };
            *shown += (v - *shown) * k;
            out.store(shown.to_bits(), Ordering::Relaxed);
            *p = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bars_for(hz: f32) -> [f32; BARS] {
        let out: Bars = std::array::from_fn(|_| AtomicU32::new(0));
        let mut s = Spectrum::new(48_000, 2);
        for n in 0..48_000 {
            let x = (2.0 * PI * hz * n as f32 / 48_000.0).sin() * 0.5;
            s.push(x, &out);
            s.push(x, &out);
        }
        std::array::from_fn(|i| f32::from_bits(out[i].load(Ordering::Relaxed)))
    }

    #[test]
    fn a_tone_lights_its_own_band_most() {
        for hz in [60.0, 1000.0, 8000.0] {
            let bars = bars_for(hz);
            let top = (0..BARS).max_by(|&a, &b| bars[a].total_cmp(&bars[b])).unwrap();
            let centre = LOW_HZ * (HIGH_HZ / LOW_HZ).powf(top as f32 / (BARS - 1) as f32);
            assert!((centre / hz).log2().abs() < 0.5, "{hz} Hz lit the {centre:.0} Hz band: {bars:?}");
            // Far-off bands stay well down.
            assert!(bars[0].min(bars[BARS - 1]) < bars[top] - 0.2, "{hz} Hz: {bars:?}");
        }
    }

    #[test]
    fn silence_is_empty_bars() {
        assert!(bars_for(0.0).iter().all(|&v| v == 0.0));
    }

    #[test]
    fn silence_after_a_sound_doesnt_ring_down_into_denormals() {
        let out: Bars = std::array::from_fn(|_| AtomicU32::new(0));
        let mut s = Spectrum::new(48_000, 1);
        s.push(1.0, &out);
        (0..48_000).for_each(|_| s.push(0.0, &out));
        assert!(s.bands.iter().all(|b| [b.y1, b.y2].iter().all(|v| *v == 0.0 || v.is_normal())));
    }
}
