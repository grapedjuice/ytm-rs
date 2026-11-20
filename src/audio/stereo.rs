//! Headphone crossfeed, mono and left/right balance on everything the app plays, after
//! the equalizer.
//!
//! The UI sets `StereoParams` (one atomic, so a frame never sees half a change); `Stereo`
//! runs on the device's thread over each frame. Changes glide over `RAMP_MS` rather than
//! jump, and with everything neutral it hands frames through untouched. Only the front
//! pair of a surround device is touched, and a mono device not at all.

use std::f32::consts::{FRAC_PI_4, SQRT_2};
use std::f64::consts::PI;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// How much of the other ear's sound each ear gets, after Bauer's stereophonic-to-binaural
/// filter (libbs2b's presets): what a pair of speakers in front of you does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Crossfeed {
    #[default]
    Off,
    /// Jan Meier's: 650 Hz, the other side 9.5 dB down.
    Light,
    /// Chu Moy's: 700 Hz, 6 dB down.
    Medium,
    /// libbs2b's default: 700 Hz, 4.5 dB down.
    Strong,
}

impl Crossfeed {
    pub const ALL: [Self; 4] = [Self::Off, Self::Light, Self::Medium, Self::Strong];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => crate::tr!("ui-off"),
            Self::Light => crate::tr!("ui-crossfeed-light"),
            Self::Medium => crate::tr!("ui-crossfeed-medium"),
            Self::Strong => crate::tr!("ui-crossfeed-strong"),
        }
    }

    /// As saved.
    pub fn key(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Light => "light",
            Self::Medium => "medium",
            Self::Strong => "strong",
        }
    }

    pub fn from_key(key: &str) -> Self {
        Self::ALL.into_iter().find(|c| c.key() == key).unwrap_or_default()
    }

    /// The low-pass's cut-off in Hz, and how far below its own side the other side's
    /// bass comes in, in dB.
    fn filter(self) -> Option<(f64, f64)> {
        match self {
            Self::Off => None,
            Self::Light => Some((650.0, 9.5)),
            Self::Medium => Some((700.0, 6.0)),
            Self::Strong => Some((700.0, 4.5)),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub crossfeed: Crossfeed,
    /// Both ears hear both channels: for one earbud, or hearing in one ear.
    pub mono: bool,
    /// -100 (all left) to 100 (all right).
    pub balance: i8,
}

impl Settings {
    fn to_bits(self) -> u32 {
        self.crossfeed as u32 | (self.mono as u32) << 8 | (self.balance.clamp(-100, 100) as u8 as u32) << 16
    }

    fn from_bits(bits: u32) -> Self {
        let crossfeed = Crossfeed::ALL.get((bits & 0xff) as usize).copied().unwrap_or_default();
        Self { crossfeed, mono: bits >> 8 & 1 == 1, balance: (bits >> 16) as u8 as i8 }
    }
}

/// What the UI wants, read by the device's thread. The default is neutral.
#[derive(Default)]
pub struct StereoParams(AtomicU32);

impl StereoParams {
    pub fn set(&self, s: Settings) {
        self.0.store(s.to_bits(), Ordering::Relaxed);
    }
}

/// Each side's gain at `pan`, -1 (all left) to 1. Constant power, so the music stays as
/// loud as it was (L² + R² holds), scaled so the centre is unity: the side it's turned
/// towards goes up to +3 dB as the other fades out, which suits an ear that hears less
/// (the limiter keeps that from clipping). Turning the far side down only would get
/// quieter, by up to 3 dB.
fn balance_gains(pan: f32) -> [f32; 2] {
    if pan == 0.0 {
        return [1.0, 1.0];
    }
    let t = (pan.clamp(-1.0, 1.0) + 1.0) * FRAC_PI_4;
    [(SQRT_2 * t.cos()).max(0.0), SQRT_2 * t.sin()]
}

/// libbs2b's filters (Boris Mikhaylov's): each side a high shelf of itself plus a
/// first-order low-pass of the other side, which also runs it a little behind (~0.2 ms),
/// as from a speaker further away. Scaled so bass in the middle stays at unity.
#[derive(Clone, Copy)]
struct Coefs {
    a0_lo: f64,
    b1_lo: f64,
    a0_hi: f64,
    a1_hi: f64,
    b1_hi: f64,
    gain: f64,
}

impl Coefs {
    fn new((cut, feed_db): (f64, f64), rate: f64) -> Self {
        let lo_db = feed_db * -5.0 / 6.0 - 3.0;
        let hi_db = feed_db / 6.0 - 3.0;
        let g_lo = 10f64.powf(lo_db / 20.0);
        let g_hi = 1.0 - 10f64.powf(hi_db / 20.0);
        let cut_hi = cut * 2f64.powf((lo_db - 20.0 * g_hi.log10()) / 12.0);
        let (x_lo, x_hi) = ((-2.0 * PI * cut / rate).exp(), (-2.0 * PI * cut_hi / rate).exp());
        Self { a0_lo: g_lo * (1.0 - x_lo), b1_lo: x_lo, a0_hi: 1.0 - g_hi * (1.0 - x_hi), a1_hi: -x_hi, b1_hi: x_hi, gain: 1.0 / (1.0 - g_hi + g_lo) }
    }
}

/// Per side: the low-pass's last output, the shelf's, and the shelf's last input.
#[derive(Clone, Copy, Default)]
struct Delays {
    lo: [f64; 2],
    hi: [f64; 2],
    last: [f64; 2],
}

/// How long a change takes: long enough not to click, short enough to feel at once.
const RAMP_MS: f32 = 20.0;

pub struct Stereo {
    params: Arc<StereoParams>,
    rate: f64,
    /// How far a glide moves per frame, of the whole way.
    step: f32,
    bits: u32,
    want: Settings,
    /// The crossfeed the filters are set to, which changes once its mix is down to 0.
    crossfeed: Crossfeed,
    coefs: Option<Coefs>,
    delays: Delays,
    /// How much of each the frame gets now, gliding to what `want` says. The balance
    /// glides as `pan`, so it holds the level partway too; `gains` are its.
    mix: f32,
    mono: f32,
    pan: f32,
    gains: [f32; 2],
    /// Neutral and at rest: frames pass as they are.
    idle: bool,
}

impl Stereo {
    pub fn new(params: Arc<StereoParams>, rate: u32) -> Self {
        let rate = rate.max(1) as f64;
        let mut s = Self {
            params,
            rate,
            step: 1.0 / (rate as f32 * RAMP_MS / 1000.0).max(1.0),
            bits: 0,
            want: Settings::default(),
            crossfeed: Crossfeed::Off,
            coefs: None,
            delays: Delays::default(),
            mix: 0.0,
            mono: 0.0,
            pan: 0.0,
            gains: [1.0; 2],
            idle: false,
        };
        // A device opened mid-song starts at the settings, not gliding to them.
        s.read();
        s.set_crossfeed();
        (s.mix, s.mono, s.pan) = s.targets();
        s.gains = balance_gains(s.pan);
        s.glide();
        s
    }

    fn read(&mut self) {
        self.bits = self.params.0.load(Ordering::Relaxed);
        self.want = Settings::from_bits(self.bits);
        self.idle = false;
    }

    fn set_crossfeed(&mut self) {
        self.crossfeed = self.want.crossfeed;
        self.coefs = self.crossfeed.filter().map(|f| Coefs::new(f, self.rate));
        self.delays = Delays::default();
    }

    /// Where `mix`, `mono` and `pan` are headed. A new crossfeed waits for the old one's
    /// mix to go down; on mono it has nothing to do.
    fn targets(&self) -> (f32, f32, f32) {
        let mix = self.coefs.is_some() && self.crossfeed == self.want.crossfeed && !self.want.mono;
        (if mix { 1.0 } else { 0.0 }, if self.want.mono { 1.0 } else { 0.0 }, self.want.balance as f32 / 100.0)
    }

    /// Whether this can take a peak past where it was: crossfeed's shelf can, on a sudden
    /// swing, and the balance's near side does. The limiter's on meanwhile.
    pub fn boosts(&self) -> bool {
        !self.idle && (self.mix > 0.0 || self.pan != 0.0)
    }

    fn glide(&mut self) {
        let step = |v: &mut f32, to: f32, by: f32| *v = if (to - *v).abs() <= by { to } else { *v + by.copysign(to - *v) };
        if self.mix == 0.0 && self.crossfeed != self.want.crossfeed {
            self.set_crossfeed();
        }
        let (mix, mono, pan) = self.targets();
        step(&mut self.mix, mix, self.step);
        step(&mut self.mono, mono, self.step);
        if self.pan != pan {
            // All the way across in one ramp's time.
            step(&mut self.pan, pan, self.step * 2.0);
            self.gains = balance_gains(self.pan);
        }
        if self.mix == 0.0 {
            // Fresh for when it comes back up.
            self.delays = Delays::default();
        }
        self.idle = self.want == Settings::default() && self.coefs.is_none() && self.mono == 0.0 && self.pan == 0.0;
    }

    /// What crossfeed makes of the frame.
    fn crossfeed(&mut self, c: Coefs, x: [f32; 2]) -> [f32; 2] {
        let d = &mut self.delays;
        for i in 0..2 {
            let x = x[i] as f64;
            d.lo[i] = c.a0_lo * x + c.b1_lo * d.lo[i];
            d.hi[i] = c.a0_hi * x + c.a1_hi * d.last[i] + c.b1_hi * d.hi[i];
            d.last[i] = x;
            // Silence (pause) would otherwise ring them down into denormals, which are
            // slow on x86.
            for v in [&mut d.lo[i], &mut d.hi[i]] {
                if v.abs() < 1e-20 {
                    *v = 0.0;
                }
            }
        }
        let out = [((d.hi[0] + d.lo[1]) * c.gain) as f32, ((d.hi[1] + d.lo[0]) * c.gain) as f32];
        // A broken sample would stay in the delays for good: start them over, and let
        // the frame through as it is (as everything else does with it).
        if !out.iter().all(|s| s.is_finite()) {
            self.delays = Delays::default();
            return x;
        }
        out
    }

    /// One frame, its channels in the device's order (front left and right first).
    pub fn process(&mut self, frame: &mut [f32]) {
        if self.params.0.load(Ordering::Relaxed) != self.bits {
            self.read();
        }
        if self.idle {
            return;
        }
        let [l, r, ..] = frame else { return };
        self.glide();
        let mut x = [*l, *r];
        if self.mix > 0.0
            && let Some(c) = self.coefs
        {
            let wet = self.crossfeed(c, x);
            x = std::array::from_fn(|i| x[i] + self.mix * (wet[i] - x[i]));
        }
        if self.mono > 0.0 {
            let mid = (x[0] + x[1]) / 2.0;
            // Settled, exactly the same in each: the glide's sum rounds each its own way.
            x = if self.mono == 1.0 { [mid; 2] } else { x.map(|s| s + self.mono * (mid - s)) };
        }
        (*l, *r) = (x[0] * self.gains[0], x[1] * self.gains[1]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo(s: Settings) -> (Stereo, Arc<StereoParams>) {
        let p = Arc::new(StereoParams::default());
        p.set(s);
        (Stereo::new(p.clone(), 48_000), p)
    }

    fn run(st: &mut Stereo, frame: [f32; 2]) -> [f32; 2] {
        let mut f = frame;
        st.process(&mut f);
        f
    }

    /// Steady-state peak of each side for a sine at `f` Hz, `amp` on each input side.
    fn peaks(st: &mut Stereo, f: f64, amp: [f32; 2]) -> [f32; 2] {
        let mut peak = [0f32; 2];
        for k in 0..48_000 {
            let s = (2.0 * PI * f * k as f64 / 48_000.0).sin() as f32;
            let y = run(st, [s * amp[0], s * amp[1]]);
            if k > 24_000 {
                peak = std::array::from_fn(|i| peak[i].max(y[i].abs()));
            }
        }
        peak
    }

    fn noise() -> impl Iterator<Item = [f32; 2]> {
        let mut seed = 1u32;
        std::iter::repeat_with(move || {
            std::array::from_fn(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 8) as f32 / (1 << 23) as f32 - 1.0
            })
        })
    }

    #[test]
    fn settings_survive_the_atomic() {
        for crossfeed in Crossfeed::ALL {
            for balance in [-100, -37, 0, 1, 100] {
                let s = Settings { crossfeed, mono: balance == 1, balance };
                assert_eq!(Settings::from_bits(s.to_bits()), s);
                assert_eq!(Crossfeed::from_key(crossfeed.key()), crossfeed);
            }
        }
        assert_eq!(Settings::from_bits(0), Settings::default());
    }

    #[test]
    fn neutral_passes_samples_through_unchanged() {
        let (mut st, _) = stereo(Settings::default());
        for x in noise().take(10_000).chain([[f32::MAX, f32::MIN_POSITIVE], [-0.0, f32::NAN]]) {
            let y = run(&mut st, x);
            assert_eq!(y.map(f32::to_bits), x.map(f32::to_bits));
        }
        assert!(st.idle);
        // And a surround device's other channels are never touched.
        let (mut st, _) = stereo(Settings { crossfeed: Crossfeed::Strong, mono: false, balance: 40 });
        let mut f = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6];
        st.process(&mut f);
        assert_eq!(f[2..], [0.3, 0.4, 0.5, 0.6]);
    }

    #[test]
    fn mono_sums_both_sides_into_each() {
        let (mut st, _) = stereo(Settings { mono: true, ..Default::default() });
        for x in noise().take(1_000).chain([[0.9, -0.8]]) {
            let mid = (x[0] + x[1]) / 2.0;
            assert_eq!(run(&mut st, x), [mid, mid]);
        }
        // A voice in the middle stays as loud; one side alone comes out of both, half as loud.
        assert_eq!(run(&mut st, [0.8, 0.8]), [0.8, 0.8]);
        assert_eq!(run(&mut st, [0.8, 0.0]), [0.4, 0.4]);
    }

    #[test]
    fn balance_holds_the_level_and_centre_is_unity() {
        assert_eq!(balance_gains(0.0), [1.0, 1.0]);
        assert_eq!(balance_gains(-1.0)[1], 0.0);
        assert_eq!(balance_gains(1.0)[0], 0.0);
        for b in -100..=100 {
            let [l, r] = balance_gains(b as f32 / 100.0);
            assert!((l * l + r * r - 2.0).abs() < 1e-5, "{b}");
            assert!((b <= 0 || r > l) && (b >= 0 || l > r), "{b}");
        }
        let (mut st, _) = stereo(Settings { balance: 100, ..Default::default() });
        let y = run(&mut st, [0.5, 0.5]);
        assert_eq!(y[0], 0.0);
        assert!((y[1] - 0.5 * SQRT_2).abs() < 1e-6);
    }

    #[test]
    fn crossfeed_off_passes_samples_through_unchanged() {
        // Off, with balance and mono neutral too, whatever was on before.
        let (mut st, p) = stereo(Settings { crossfeed: Crossfeed::Strong, mono: true, balance: -30 });
        p.set(Settings::default());
        noise().take(4_800).for_each(|x| _ = run(&mut st, x));
        for x in noise().take(1_000) {
            assert_eq!(run(&mut st, x), x);
        }
        assert!(st.idle);
    }

    #[test]
    fn crossfeed_feeds_bass_across_and_keeps_it_level_in_the_middle() {
        for c in [Crossfeed::Light, Crossfeed::Medium, Crossfeed::Strong] {
            let (_, feed_db) = c.filter().unwrap();
            let (mut st, _) = stereo(Settings { crossfeed: c, ..Default::default() });
            // Bass in the middle: as it was.
            let mid = peaks(&mut st, 50.0, [0.5, 0.5]);
            assert!(mid.iter().all(|p| (p - 0.5).abs() < 0.01), "{c:?}: {mid:?}");
            // Bass on the left only: the right gets it too, `feed_db` down.
            let [l, r] = peaks(&mut st, 50.0, [0.5, 0.0]);
            let db = 20.0 * (l / r).log10();
            assert!((db - feed_db as f32).abs() < 0.3, "{c:?}: {db}");
            // Treble on the left hardly crosses.
            let [l, r] = peaks(&mut st, 10_000.0, [0.5, 0.0]);
            assert!(r < l * 0.1, "{c:?}: {l} {r}");
        }
    }

    #[test]
    fn changes_glide_and_land_exactly() {
        let all = [
            Settings::default(),
            Settings { crossfeed: Crossfeed::Strong, ..Default::default() },
            Settings { crossfeed: Crossfeed::Light, ..Default::default() },
            Settings { mono: true, crossfeed: Crossfeed::Light, balance: 0 },
            Settings { balance: -100, ..Default::default() },
            Settings { balance: 100, ..Default::default() },
        ];
        for from in all {
            for to in all {
                let (mut st, p) = stereo(from);
                // A steady tone in the middle: nothing should move suddenly.
                let w = 2.0 * PI * 200.0 / 48_000.0;
                let tone = |k: usize| (w * k as f64).sin() as f32 * 0.5;
                (0..4_800).for_each(|k| _ = run(&mut st, [tone(k), tone(k)]));
                p.set(to);
                let mut last = run(&mut st, [tone(4_800), tone(4_800)]);
                for k in 4_801..9_600 {
                    let y = run(&mut st, [tone(k), tone(k)]);
                    // A 200 Hz tone moves at most ~0.013 a sample, ~0.019 turned up 3 dB.
                    assert!(y.iter().zip(last).all(|(a, b)| (a - b).abs() < 0.03), "{from:?} to {to:?}: {last:?} {y:?} at {k}");
                    // The balance holds the level partway across too.
                    let [l, r] = st.gains;
                    assert!((l * l + r * r - 2.0).abs() < 1e-5, "{from:?} to {to:?}: {:?} at {k}", st.gains);
                    last = y;
                }
                let (mix, mono, pan) = st.targets();
                assert_eq!((st.mix, st.mono, st.pan, st.gains, st.crossfeed), (mix, mono, pan, balance_gains(pan), to.crossfeed), "{from:?} to {to:?}");
                assert_eq!(st.boosts(), mix > 0.0 || pan != 0.0, "{from:?} to {to:?}");
            }
        }
    }

    #[test]
    fn a_broken_sample_doesnt_stick() {
        let (mut st, _) = stereo(Settings { crossfeed: Crossfeed::Medium, ..Default::default() });
        assert_eq!(run(&mut st, [f32::NAN, 0.25])[1], 0.25);
        assert!(noise().take(1_000).all(|x| run(&mut st, x).iter().all(|s| s.is_finite())));
    }

    #[test]
    fn silence_after_a_sound_doesnt_ring_down_into_denormals() {
        let (mut st, _) = stereo(Settings { crossfeed: Crossfeed::Strong, ..Default::default() });
        run(&mut st, [1.0, -1.0]);
        (0..48_000).for_each(|_| _ = run(&mut st, [0.0, 0.0]));
        let d = st.delays;
        assert!(d.lo.iter().chain(&d.hi).all(|v| *v == 0.0 || v.is_normal()), "{:?} {:?}", d.lo, d.hi);
    }

    #[test]
    fn a_mono_device_is_left_alone() {
        let (mut st, _) = stereo(Settings { mono: true, balance: 50, crossfeed: Crossfeed::Strong });
        let mut f = [0.7];
        st.process(&mut f);
        assert_eq!(f, [0.7]);
    }
}
