//! A few bars of what it hears, from low to high, for the card to show it's listening.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

pub const BARS: usize = 24;
/// Samples looked at for each update: about 40 ms at 48 kHz, enough for the low bars.
const N: usize = 2048;
/// The bars' range, from the lowest bar's bottom up, in Hz.
const LOW: f32 = 60.0;
const HIGH: f32 = 12_000.0;
/// Empty to full, in dB below a full-scale sine.
const FLOOR: f32 = -70.0;
const CEILING: f32 = -12.0;

pub struct Meter {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    input: Vec<f32>,
    output: Vec<Complex<f32>>,
}

impl Default for Meter {
    fn default() -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(N);
        let window = (0..N).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / N as f32).cos()).collect();
        Meter { input: fft.make_input_vec(), output: fft.make_output_vec(), fft, window }
    }
}

impl Meter {

    /// The bars, 0 to 1, for the latest of `samples` (mono, at `rate`): none until there are enough.
    pub fn bars(&mut self, samples: &[f32], rate: u32) -> Option<[f32; BARS]> {
        if samples.len() < N || rate == 0 {
            return None;
        }
        let latest = &samples[samples.len() - N..];
        for ((x, s), w) in self.input.iter_mut().zip(latest).zip(&self.window) {
            *x = s * w;
        }
        self.fft.process(&mut self.input, &mut self.output).ok()?;
        let (hz, top) = (rate as f32 / N as f32, HIGH.min(rate as f32 / 2.0));
        let edge = |b: usize| LOW * (top / LOW).powf(b as f32 / BARS as f32);
        Some(std::array::from_fn(|b| {
            let (lo, hi) = (edge(b), edge(b + 1));
            let i0 = ((lo / hz) as usize).min(self.output.len() - 1);
            let i1 = ((hi / hz) as usize).clamp(i0 + 1, self.output.len());
            let mag = self.output[i0..i1].iter().map(|c| c.norm()).fold(0.0, f32::max);
            // A full-scale sine peaks at N/4 through the window. Music is quieter up high:
            // tilted 3 dB an octave about 1 kHz, so the bars are about as busy across.
            let db = 20.0 * (mag / (N as f32 / 4.0)).max(1e-9).log10() + 3.0 * (lo / 1000.0).log2();
            ((db - FLOOR) / (CEILING - FLOOR)).clamp(0.0, 1.0)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tone_fills_its_own_bar() {
        let rate = 48_000;
        let tone: Vec<f32> = (0..N).map(|i| (std::f32::consts::TAU * 1000.0 * i as f32 / rate as f32).sin() * 0.5).collect();
        let bars = Meter::default().bars(&tone, rate).unwrap();
        let loudest = (0..BARS).max_by(|a, b| bars[*a].total_cmp(&bars[*b])).unwrap();
        let edge = |b: usize| LOW * (HIGH / LOW).powf(b as f32 / BARS as f32);
        assert!(edge(loudest) <= 1000.0 * 1.1 && 1000.0 <= edge(loudest + 1) * 1.1, "bar {loudest}");
        assert!(bars[loudest] > 0.8 && bars[0] < 0.2, "{bars:?}");
        assert!(Meter::default().bars(&vec![0.0; N], rate).unwrap().iter().all(|b| *b == 0.0));
    }
}
