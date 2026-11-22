//! Shazam's audio signature: peaks picked from a 16 kHz mono spectrogram, in Shazam's
//! binary format.
//!
//! Ported from SongRec 0.8.2 (src/core/fingerprinting/algorithm.rs and
//! signature_format.rs), copyright marin-m and the SongRec contributors,
//! https://github.com/marin-m/SongRec, licensed under the GNU GPL v3. Changed for ytm-rs
//! in November 2026: the Hanning window is computed rather than tabled (SongRec's table is
//! the same formula to 5 significant digits), encoding only (no decoding), no byteorder
//! crate, and a peak too flat to place is skipped where SongRec's assertion would panic.

use std::sync::{Arc, LazyLock};

use base64::Engine;
use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

pub const SAMPLE_RATE: u32 = 16_000;

struct Peak {
    fft_pass: u32,
    magnitude: u16,
    bin: u16,
}

/// Peaks in Shazam's four frequency bands (250–520 Hz, 520–1450, 1450–3500, 3500–5500).
pub struct Signature {
    samples: u32,
    bands: [Vec<Peak>; 4],
}

impl Signature {
    /// From 16 kHz mono samples in -1..1.
    pub fn from_samples(samples: &[f32]) -> Self {
        let mut g = Generator::new();
        // To 16-bit as rodio converts: × 32768, saturating.
        let s16: Vec<i16> = samples.iter().map(|s| (s * 32768.0) as i16).collect();
        for chunk in s16.as_chunks::<128>().0 {
            g.do_fft(chunk);
            g.do_peak_spreading();
            g.spread_done += 1;
            if g.spread_done >= 46 {
                g.do_peak_recognition();
            }
        }
        Self { samples: samples.len() as u32, bands: g.bands }
    }

    pub fn duration_ms(&self) -> u32 {
        (self.samples as u64 * 1000 / SAMPLE_RATE as u64) as u32
    }

    #[cfg(test)]
    pub fn peaks(&self) -> usize {
        self.bands.iter().map(Vec::len).sum()
    }

    pub fn to_binary(&self) -> Vec<u8> {
        let mut b: Vec<u8> = Vec::new();
        let u32le = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
        u32le(&mut b, 0xcafe2580); // magic1
        u32le(&mut b, 0); // crc32, below
        u32le(&mut b, 0); // size minus header, below
        u32le(&mut b, 0x94119c00); // magic2
        for _ in 0..3 {
            u32le(&mut b, 0);
        }
        u32le(&mut b, 3 << 27); // 16 kHz
        for _ in 0..2 {
            u32le(&mut b, 0);
        }
        u32le(&mut b, self.samples + (SAMPLE_RATE as f32 * 0.24) as u32);
        u32le(&mut b, (15 << 19) + 0x40000);
        u32le(&mut b, 0x40000000);
        u32le(&mut b, 0); // size minus header, below
        for (band, peaks) in self.bands.iter().enumerate().filter(|(_, p)| !p.is_empty()) {
            let mut pb = Vec::new();
            let mut pass = 0;
            for p in peaks {
                if p.fft_pass - pass >= 255 {
                    pb.push(0xff);
                    pb.extend_from_slice(&p.fft_pass.to_le_bytes());
                    pass = p.fft_pass;
                }
                pb.push((p.fft_pass - pass) as u8);
                pb.extend_from_slice(&p.magnitude.to_le_bytes());
                pb.extend_from_slice(&p.bin.to_le_bytes());
                pass = p.fft_pass;
            }
            u32le(&mut b, 0x60030040 + band as u32);
            u32le(&mut b, pb.len() as u32);
            b.extend_from_slice(&pb);
            b.resize(b.len() + (4 - pb.len() % 4) % 4, 0);
        }
        let size = (b.len() - 48) as u32;
        b[8..12].copy_from_slice(&size.to_le_bytes());
        b[52..56].copy_from_slice(&size.to_le_bytes());
        let crc = crc32fast::hash(&b[8..]);
        b[4..8].copy_from_slice(&crc.to_le_bytes());
        b
    }

    pub fn to_uri(&self) -> String {
        format!("data:audio/vnd.shazam.sig;base64,{}", base64::prelude::BASE64_STANDARD.encode(self.to_binary()))
    }
}

/// A 2050-point Hanning window without its zero ends.
static WINDOW: LazyLock<[f32; 2048]> = LazyLock::new(|| std::array::from_fn(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * (i + 1) as f64 / 2049.0).cos()) as f32));

struct Generator {
    ring: [i16; 2048],
    ring_at: usize,
    fft: Arc<dyn realfft::RealToComplex<f32>>,
    fft_in: Vec<f32>,
    fft_out: Vec<Complex<f32>>,
    fft_scratch: Vec<Complex<f32>>,
    /// Ring buffers of 256 spectra of 1025 bins.
    ffts: Vec<[f32; 1025]>,
    ffts_at: u8,
    spread: Vec<[f32; 1025]>,
    spread_at: u8,
    spread_done: u32,
    bands: [Vec<Peak>; 4],
}

impl Generator {
    fn new() -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(2048);
        Self {
            ring: [0; 2048],
            ring_at: 0,
            fft_in: fft.make_input_vec(),
            fft_out: fft.make_output_vec(),
            fft_scratch: fft.make_scratch_vec(),
            fft,
            ffts: vec![[0.0; 1025]; 256],
            ffts_at: 0,
            spread: vec![[0.0; 1025]; 256],
            spread_at: 0,
            spread_done: 0,
            bands: Default::default(),
        }
    }

    fn do_fft(&mut self, chunk: &[i16; 128]) {
        self.ring[self.ring_at..self.ring_at + 128].copy_from_slice(chunk);
        self.ring_at = (self.ring_at + 128) & 2047;
        for (i, w) in WINDOW.iter().enumerate() {
            self.fft_in[i] = self.ring[(i + self.ring_at) & 2047] as f32 * w;
        }
        self.fft.process_with_scratch(&mut self.fft_in, &mut self.fft_out, &mut self.fft_scratch).expect("the buffers are the plan's own");
        let out = &mut self.ffts[self.ffts_at as usize];
        for (r, c) in out.iter_mut().zip(&self.fft_out) {
            *r = ((c.re * c.re + c.im * c.im) / (1 << 17) as f32).max(0.000_000_000_1);
        }
        self.ffts_at = self.ffts_at.wrapping_add(1);
    }

    fn do_peak_spreading(&mut self) {
        let mut s = self.ffts[self.ffts_at.wrapping_sub(1) as usize];
        // Frequency-domain spreading.
        for i in 0..=1022 {
            s[i] = s[i].max(s[i + 1]).max(s[i + 2]);
        }
        self.spread[self.spread_at as usize] = s;
        // Time-domain spreading.
        for former in [1u8, 3, 6] {
            let f = &mut self.spread[self.spread_at.wrapping_sub(former) as usize];
            for (a, b) in f.iter_mut().zip(&s) {
                *a = a.max(*b);
            }
        }
        self.spread_at = self.spread_at.wrapping_add(1);
    }

    fn do_peak_recognition(&mut self) {
        let fft_46 = &self.ffts[self.ffts_at.wrapping_sub(46) as usize];
        let spread_49 = &self.spread[self.spread_at.wrapping_sub(49) as usize];
        let mag = |v: f32| v.ln().max(1.0 / 64.0) * 1477.3 + 6144.0;
        for bin in 10..=1014usize {
            let v = fft_46[bin];
            if v < 1.0 / 64.0 || v < spread_49[bin - 1] {
                continue;
            }
            // A frequency-domain local maximum…
            let mut around = [-10i32, -7, -4, -3, 1, 2, 5, 8].iter().map(|o| spread_49[(bin as i32 + o) as usize]).fold(0.0f32, f32::max);
            if v <= around {
                continue;
            }
            // …and a time-domain one.
            for o in [-53i32, -45, 165, 172, 179, 186, 193, 200, 214, 221, 228, 235, 242, 249] {
                around = around.max(self.spread[((self.spread_at as i32 + o) & 255) as usize][bin - 1]);
            }
            if v <= around {
                continue;
            }
            let (m, before, after) = (mag(v), mag(fft_46[bin - 1]), mag(fft_46[bin + 1]));
            let variation1 = m * 2.0 - before - after;
            // It should be at least 0, but isn't always, as the peak was tested
            // against the spread spectrum. 0 would divide by zero.
            if variation1 <= 0.0 || !variation1.is_finite() {
                continue;
            }
            let variation2 = (after - before) * 32.0 / variation1;
            let corrected = ((bin as i32 * 64) + variation2 as i32) as u16;
            let hz = corrected as f32 * (16000.0 / 2.0 / 1024.0 / 64.0);
            let band = match hz as i32 {
                250..=519 => 0,
                520..=1449 => 1,
                1450..=3499 => 2,
                3500..=5500 => 3,
                _ => continue,
            };
            self.bands[band].push(Peak { fft_pass: self.spread_done - 46, magnitude: m as u16, bin: corrected });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_matches_the_original_table() {
        // The ported code's Hanning table: its first, middle and last entries.
        for (i, table) in [(0, 0.000_002_350_8), (1, 0.000_009_403_2), (1024, 1.0), (2047, 0.000_002_350_8)] {
            assert!((WINDOW[i] - table).abs() <= table * 5e-5, "{i}: {} against {table}", WINDOW[i]);
        }
    }

    #[test]
    fn a_tone_gives_peaks_and_a_well_formed_signature() {
        // 3 s of a 1 kHz tone that comes and goes, so it has onsets to pick.
        let samples: Vec<f32> = (0..3 * SAMPLE_RATE)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                let on = if ((t * 4.0) as u32).is_multiple_of(2) { 0.5 } else { 0.0 };
                on * (2.0 * std::f32::consts::PI * 1000.0 * t).sin()
            })
            .collect();
        let sig = Signature::from_samples(&samples);
        assert!(sig.peaks() > 0);
        assert!(!sig.bands[1].is_empty(), "1 kHz is in the 520–1450 Hz band");
        let b = sig.to_binary();
        assert_eq!(&b[0..4], &0xcafe2580u32.to_le_bytes());
        assert_eq!(u32::from_le_bytes(b[8..12].try_into().unwrap()) as usize, b.len() - 48);
        assert_eq!(u32::from_le_bytes(b[4..8].try_into().unwrap()), crc32fast::hash(&b[8..]));
        assert_eq!(sig.duration_ms(), 3000);
    }
}
