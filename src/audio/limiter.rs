//! A peak limiter on everything on its way to the device, for songs normalization turns
//! up: their peaks would otherwise go past full scale and clip. Like Spotify's, it holds
//! peaks at -1 dB and lets go over about 100 ms.
//!
//! It looks a couple of milliseconds ahead (the output runs that much behind), so it's
//! already turned down by the time a peak gets out. Below the ceiling it changes nothing,
//! so at a lower volume, where a raised song still has room, it never acts.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Peaks are held here: -1 dBFS, leaving room for the device's own resampling.
const CEILING: f32 = 0.891;
/// How far ahead it looks, in ms (and how far the output runs behind).
pub const LOOKAHEAD_MS: u32 = 2;
/// How quickly it lets go once the peaks have passed.
const RELEASE_MS: f32 = 100.0;

pub struct Limiter {
    /// Off, it passes everything through, the same lookahead behind.
    on: Arc<AtomicBool>,
    channels: usize,
    /// The last `lookahead` frames, interleaved; the oldest is the next out.
    delay: Vec<f32>,
    at: usize,
    /// The peaks of the frames in `delay`, in falling order from the oldest: the front
    /// is the loudest still to come out.
    peaks: VecDeque<(u64, f32)>,
    frame: u64,
    lookahead: u64,
    /// The loudest sample of the frame coming in.
    peak: f32,
    gain: f32,
    attack: f32,
    release: f32,
}

impl Limiter {
    pub fn new(on: Arc<AtomicBool>, sample_rate: u32, channels: u16) -> Self {
        let channels = channels.max(1) as usize;
        let lookahead = (sample_rate as u64 * LOOKAHEAD_MS as u64 / 1000).max(2);
        // Down within the lookahead: after it, less than 0.3% of the way is left.
        let attack = 1.0 - (-6.0 / (lookahead - 1) as f32).exp();
        let release = 1.0 - (-1000.0 / (RELEASE_MS * sample_rate.max(1) as f32)).exp();
        Self {
            on,
            channels,
            delay: vec![0.0; lookahead as usize * channels],
            at: 0,
            peaks: VecDeque::new(),
            frame: 0,
            lookahead,
            peak: 0.0,
            gain: 1.0,
            attack,
            release,
        }
    }

    /// One sample in (channel `channel` of the frame), the one from `LOOKAHEAD_MS`
    /// earlier out.
    pub fn process(&mut self, s: f32, channel: usize) -> f32 {
        let out = std::mem::replace(&mut self.delay[self.at], s);
        self.at = (self.at + 1) % self.delay.len();
        self.peak = self.peak.max(s.abs());
        if channel + 1 >= self.channels {
            self.end_frame();
        }
        (out * self.gain).clamp(-1.0, 1.0)
    }

    fn end_frame(&mut self) {
        let peak = std::mem::take(&mut self.peak);
        while self.peaks.back().is_some_and(|&(_, p)| p <= peak) {
            self.peaks.pop_back();
        }
        self.peaks.push_back((self.frame, peak));
        while self.peaks.front().is_some_and(|&(f, _)| f + self.lookahead <= self.frame) {
            self.peaks.pop_front();
        }
        self.frame += 1;
        let loudest = self.peaks.front().map_or(0.0, |&(_, p)| p);
        let want = if self.on.load(Ordering::Relaxed) && loudest > CEILING { CEILING / loudest } else { 1.0 };
        let k = if want < self.gain { self.attack } else { self.release };
        self.gain += (want - self.gain) * k;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter(on: bool) -> Limiter {
        Limiter::new(Arc::new(AtomicBool::new(on)), 48_000, 2)
    }

    /// Runs stereo frames of `f(i)` through, returning what comes out.
    fn run(l: &mut Limiter, frames: usize, f: impl Fn(usize) -> f32) -> Vec<f32> {
        (0..frames).flat_map(|i| [f(i), f(i)]).enumerate().map(|(n, s)| l.process(s, n % 2)).collect()
    }

    fn sine(i: usize, amp: f32) -> f32 {
        amp * (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 48_000.0).sin()
    }

    #[test]
    fn below_the_ceiling_it_only_delays() {
        let mut l = limiter(true);
        let out = run(&mut l, 4_800, |i| sine(i, 0.8));
        let lag = 96; // 2 ms at 48 kHz
        for i in 0..4_800 - lag {
            assert_eq!(out[(i + lag) * 2], sine(i, 0.8));
        }
    }

    #[test]
    fn a_song_turned_up_twice_keeps_its_peaks_under_the_ceiling() {
        let mut l = limiter(true);
        // A full-scale song turned up 6 dB, and a click out of silence (no warning but the lookahead).
        let out = run(&mut l, 48_000, |i| if i < 24_000 { sine(i, 2.0) } else if i == 30_000 { 2.0 } else { 0.0 });
        let max = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(max <= CEILING * 1.01, "peak {max}");
        // Turned down, not cut off: the loud part still comes out near the ceiling.
        assert!(out[20_000 * 2..21_000 * 2].iter().any(|s| s.abs() > CEILING * 0.95));
    }

    #[test]
    fn it_lets_go_after_the_peaks() {
        let mut l = limiter(true);
        run(&mut l, 4_800, |i| sine(i, 2.0));
        // A second of quiet music after: back to as it was.
        let out = run(&mut l, 48_000, |i| sine(i, 0.5));
        assert!((out[47_000 * 2..].iter().fold(0.0f32, |m, s| m.max(s.abs())) - 0.5).abs() < 0.01);
    }

    #[test]
    fn off_it_changes_nothing_but_clipping() {
        let mut l = limiter(false);
        let out = run(&mut l, 4_800, |i| sine(i, 0.95));
        assert_eq!(out[1_000 * 2], sine(1_000 - 96, 0.95));
        assert!(out.iter().all(|s| s.abs() <= 0.95));
    }
}
