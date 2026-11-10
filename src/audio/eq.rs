//! A 10-band graphic equalizer on everything the app plays, after the mixer.
//!
//! The UI sets `EqParams` (atomics, so the device's thread never waits on a lock);
//! `Eq` runs on that thread over the interleaved samples going to the device. Gains
//! glide to new settings rather than jump, so moving a slider doesn't click, and with
//! every band at 0 dB it hands the samples through untouched.

use std::f64::consts::PI;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Band centres in Hz, an octave apart.
pub const BANDS: [f32; 10] = [31.25, 62.5, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];
pub const MAX_DB: f32 = 12.0;

pub type Gains = [f32; 10];

/// Ready-made curves, in band order.
pub const PRESETS: [(&str, Gains); 10] = [
    ("Flat", [0.0; 10]),
    ("Bass boost", [6.0, 5.0, 4.0, 2.0, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0]),
    ("Treble boost", [0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 2.0, 4.0, 5.0, 6.0]),
    ("Vocal", [-2.0, -2.0, -1.0, 0.0, 2.0, 3.0, 3.0, 2.0, 0.0, -1.0]),
    ("Rock", [4.0, 3.0, 2.0, 0.0, -1.0, -1.0, 1.0, 2.0, 3.0, 4.0]),
    ("Pop", [-1.0, 1.0, 2.0, 3.0, 2.0, 0.0, -1.0, -1.0, 1.0, 1.0]),
    ("Electronic", [5.0, 4.0, 1.0, 0.0, -2.0, 0.0, 1.0, 2.0, 4.0, 5.0]),
    ("Acoustic", [3.0, 3.0, 2.0, 1.0, 1.0, 1.0, 2.0, 3.0, 2.0, 2.0]),
    ("Classical", [3.0, 2.0, 1.0, 0.0, 0.0, 0.0, -1.0, 1.0, 2.0, 3.0]),
    // Ears lose bass and treble first as it gets quieter: this puts them back.
    ("Late night", [5.0, 4.0, 2.0, 0.0, 0.0, 0.0, 0.0, 1.0, 3.0, 4.0]),
];

/// The preset these gains are, if any.
pub fn preset_name(gains: &Gains) -> Option<&'static str> {
    PRESETS.iter().find(|(_, g)| g == gains).map(|(n, _)| *n)
}

/// About 1.4 octaves wide. Neighbours overlap, which `fit` makes up for: wider and the
/// curve can't zigzag between neighbours, narrower and it ripples between bands. This
/// keeps the curve within ~0.1 dB of every slider for any setting.
const Q: f64 = 1.0;

/// Normalized biquad coefficients (a0 = 1).
#[derive(Clone, Copy, PartialEq, Debug)]
struct Coefs {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

const IDENTITY: Coefs = Coefs { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 };

/// How many bands this rate holds: one at or above 0.45 × the rate (16 kHz on a
/// 22.05 kHz device) would go unstable.
fn bands_at(rate: f64) -> usize {
    BANDS.iter().filter(|f| (**f as f64) < rate * 0.45).count()
}

/// The peaking filter for band `i` at `db`, from the Audio EQ Cookbook (Robert
/// Bristow-Johnson). At 0 dB it stays (top and bottom cancel) so its delays ring out
/// smoothly.
fn design(i: usize, db: f32, rate: f64) -> Coefs {
    if i >= bands_at(rate) {
        return IDENTITY;
    }
    let a = 10f64.powf(db as f64 / 40.0);
    let w0 = 2.0 * PI * BANDS[i] as f64 / rate;
    let (sin, cos) = w0.sin_cos();
    // The cookbook's width-in-octaves form: it widens the bands near the top, which come
    // out squeezed otherwise and leave a dip between 8 and 16 kHz.
    let alpha = sin * ((2.0 * Q).recip().asinh() * w0 / sin).sinh();
    let a0 = 1.0 + alpha / a;
    Coefs { b0: (1.0 + alpha * a) / a0, b1: -2.0 * cos / a0, b2: (1.0 - alpha * a) / a0, a1: -2.0 * cos / a0, a2: (1.0 - alpha / a) / a0 }
}

fn design_all(gains: &Gains, rate: f64) -> [Coefs; 10] {
    std::array::from_fn(|i| design(i, gains[i], rate))
}

/// 4 sin²(w/2) at `f` Hz, which is all `Coefs::power` needs of the frequency.
fn phi(f: f64, rate: f64) -> f64 {
    let s = (PI * f / rate).sin();
    4.0 * s * s
}

impl Coefs {
    /// |H|² at the frequency `phi` is of, in the form that stays accurate down low
    /// (from RBJ's cookbook).
    fn power(&self, phi: f64) -> f64 {
        let half = |b0: f64, b1: f64, b2: f64| (b0 + b1 + b2).powi(2) - phi * (b0 * b1 + b1 * b2 + 4.0 * b0 * b2) + phi * phi * b0 * b2;
        half(self.b0, self.b1, self.b2) / half(1.0, self.a1, self.a2)
    }
}

/// The curve these filters make together at `f` Hz, in dB.
fn curve_db(coefs: &[Coefs; 10], f: f64, rate: f64) -> f64 {
    let phi = phi(f, rate);
    10.0 * coefs.iter().map(|c| c.power(phi)).product::<f64>().log10()
}

/// The gain each band gets of its own. Neighbours spill into each other, so setting each
/// to its slider would land the curve off the sliders; instead these are picked so the
/// curve meets every slider, and halfway between two lies about halfway between them
/// (least squares, after Välimäki and Liski, "Accurate cascade graphic equalizer", 2017).
fn fit(sliders: &Gains, rate: f64) -> Gains {
    const PROTOTYPE_DB: f64 = 17.0;
    // Meeting the sliders counts for more than the points between them.
    const SLIDER_WEIGHT: f64 = 30.0;
    // A sharp swing, like -12 between two +12s, takes a band about 27 dB of its own.
    const CAP: f64 = 3.0 * MAX_DB as f64;
    let n = bands_at(rate);
    if sliders.iter().all(|g| *g == 0.0) || n == 0 {
        return [0.0; 10];
    }
    // Each band's centre, and the point halfway (on a log scale) to the next.
    let point = |k: usize| {
        let (i, j) = (k / 2, k.div_ceil(2));
        ((BANDS[i] as f64 * BANDS[j] as f64).sqrt(), (sliders[i] + sliders[j]) as f64 / 2.0)
    };
    // How much a band moves the curve at each point per dB of its own gain is measured
    // at a typical gain first, then again at about the gain it came out with, twice: the
    // shape changes a little with the gain.
    let mut at = [PROTOTYPE_DB; 10];
    let mut out = [0.0; 10];
    for _ in 0..3 {
        // The normal equations (BᵀB) x = Bᵀt, B being the per-dB effects.
        let (mut a, mut b) = ([[0.0; 10]; 10], [0.0; 10]);
        let coefs: [Coefs; 10] = std::array::from_fn(|m| design(m, at[m] as f32, rate));
        for k in 0..2 * n - 1 {
            let (f, t) = point(k);
            let (w, phi) = (if k % 2 == 0 { SLIDER_WEIGHT } else { 1.0 }, phi(f, rate));
            let row: [f64; 10] = std::array::from_fn(|m| if m < n { 10.0 * coefs[m].power(phi).log10() / at[m] } else { 0.0 });
            for i in 0..n {
                b[i] += w * row[i] * t;
                for j in 0..n {
                    a[i][j] += w * row[i] * row[j];
                }
            }
        }
        let x = solve(a, b, n);
        for m in 0..n {
            out[m] = x[m].clamp(-CAP, CAP) as f32;
            // Cuts are boosts mirrored, so the shape only depends on how big the gain is.
            at[m] = x[m].abs().clamp(1.0, CAP);
        }
    }
    out
}

/// `a x = b` for the first `n` rows, by elimination with partial pivoting.
fn solve(mut a: [[f64; 10]; 10], mut b: [f64; 10], n: usize) -> [f64; 10] {
    for c in 0..n {
        let p = (c..n).max_by(|&i, &j| a[i][c].abs().total_cmp(&a[j][c].abs())).unwrap_or(c);
        a.swap(c, p);
        b.swap(c, p);
        for r in c + 1..n {
            let k = a[r][c] / a[c][c];
            let pivot = a[c];
            for (x, p) in a[r][c..n].iter_mut().zip(&pivot[c..n]) {
                *x -= k * p;
            }
            b[r] -= k * b[c];
        }
    }
    let mut x = [0.0; 10];
    for c in (0..n).rev() {
        x[c] = (b[c] - (c + 1..n).map(|j| a[c][j] * x[j]).sum::<f64>()) / a[c][c];
    }
    x
}

/// The curve the sliders make, in dB by frequency (at 48 kHz, which is what nearly
/// every device runs at; the curve barely differs at 44.1).
pub fn response(sliders: &Gains) -> impl Fn(f32) -> f32 + use<> {
    let coefs = design_all(&fit(sliders, 48_000.0), 48_000.0);
    move |f| curve_db(&coefs, f as f64, 48_000.0) as f32
}

/// How far to turn everything down at `rate` so the loudest point of the curve the
/// filters at `gains` make stays under 0 dB, for a steady tone. (Sudden sounds can
/// still ring a little past it.)
fn headroom_db(gains: &Gains, rate: f64) -> f32 {
    let coefs = design_all(gains, rate);
    let db = |f: f64| curve_db(&coefs, f, rate);
    // ~1/20 octave apart from 10 Hz to Nyquist, then each rise found to its top between
    // its neighbours: near Nyquist a band can peak sharper than the spacing.
    let grid: [f64; 241] = std::array::from_fn(|k| 10.0 * (rate / 20.0).powf(k as f64 / 240.0));
    let around = |k: usize| {
        let (mut lo, mut hi) = (grid[k.saturating_sub(1)], grid[(k + 1).min(240)]);
        for _ in 0..40 {
            let (a, b) = (lo + (hi - lo) / 3.0, hi - (hi - lo) / 3.0);
            if db(a) < db(b) { lo = a } else { hi = b }
        }
        db(lo).max(db(grid[k]))
    };
    let dbs = grid.map(db);
    // Rising into it on the left, so a flat stretch is searched once, not at every point.
    let tops = (0..=240).filter(|k| (*k == 0 || dbs[k - 1] < dbs[*k]) && (*k == 240 || dbs[k + 1] <= dbs[*k]));
    let peak = tops.map(around).chain(dbs.iter().copied()).fold(db(0.0).max(0.0), f64::max);
    // A little over, to be safe.
    if peak > 0.0 { -(peak + 0.1) as f32 } else { 0.0 }
}

/// `a` to `b` by `t`, landing on `b` exactly.
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    if t >= 1.0 { b } else { a + (b - a) * t }
}

/// What the UI wants, read by the device's thread.
#[derive(Default)]
pub struct EqParams {
    on: AtomicBool,
    /// f32 bits, dB.
    gains: [AtomicU32; 10],
    /// Turn everything down by the curve's highest boost.
    no_clip: AtomicBool,
    /// Bumped after the rest is written, for the device's thread to notice.
    version: AtomicU32,
}

impl EqParams {
    pub fn set(&self, on: bool, gains: &Gains, no_clip: bool) {
        self.on.store(on, Ordering::Relaxed);
        for (a, g) in self.gains.iter().zip(gains) {
            a.store(g.clamp(-MAX_DB, MAX_DB).to_bits(), Ordering::Relaxed);
        }
        self.no_clip.store(no_clip, Ordering::Relaxed);
        self.version.fetch_add(1, Ordering::Release);
    }
}

/// Settings are looked at, and gains moved, this many times a second.
const BLOCKS_PER_SEC: f64 = 1500.0;
/// How far the level moves per block at most: 12 dB takes ~30 ms, quick but without a click.
const STEP_DB: f32 = 0.25;
/// How far a band moves per block at most: 12 dB takes ~160 ms. Filters moving quicker
/// under a loud low tone throw it past the curve's loudest point.
const MOVE_DB: f32 = 0.05;
/// Blocks (~50 ms) the level waits after the filters move, for them to ring out.
const SETTLE: u32 = 75;

/// The equalizer itself, owned by the device's stream.
pub struct Eq {
    params: std::sync::Arc<EqParams>,
    rate: f64,
    /// Frames per block.
    block: u32,
    seen: u32,
    /// A change glides from `from` to `target`, `step` of `steps` blocks along: the level
    /// goes down to `floor` over the first `down`, the filters move over the next `moving`,
    /// then after `SETTLE` the level goes up to `target_pre`.
    from: Gains,
    from_pre: f32,
    target: Gains,
    target_pre: f32,
    floor: f32,
    down: u32,
    moving: u32,
    step: u32,
    steps: u32,
    gains: Gains,
    pre: f32,
    /// `pre` as a factor, which it ramps to by `pre_ramp` a frame over a block: a step
    /// per block would buzz.
    pre_gain: f64,
    pre_ramp: f64,
    coefs: [Coefs; 10],
    /// Per channel, per band: the filter's two delays.
    state: Vec<[[f64; 2]; 10]>,
    frame: u32,
    /// False while every gain is 0 dB and the filters have rung out: samples pass as they are.
    active: bool,
}

impl Eq {
    pub fn new(params: std::sync::Arc<EqParams>, rate: u32, channels: u16) -> Self {
        let mut eq = Self {
            params,
            rate: rate.max(1) as f64,
            block: (rate as f64 / BLOCKS_PER_SEC).round().max(1.0) as u32,
            seen: 0,
            from: [0.0; 10],
            from_pre: 0.0,
            target: [0.0; 10],
            target_pre: 0.0,
            floor: 0.0,
            down: 0,
            moving: 0,
            step: 0,
            steps: 0,
            gains: [0.0; 10],
            pre: 0.0,
            pre_gain: 1.0,
            pre_ramp: 0.0,
            coefs: [IDENTITY; 10],
            state: vec![[[0.0; 2]; 10]; channels.max(1) as usize],
            frame: 0,
            active: false,
        };
        // A device opened mid-song starts at the settings, not gliding up to them.
        eq.read();
        eq.step = eq.steps;
        eq.gains = eq.target;
        eq.pre = eq.target_pre;
        eq.pre_gain = 10f64.powf(eq.pre as f64 / 20.0);
        eq.coefs = design_all(&eq.gains, eq.rate);
        eq.active = eq.gains.iter().any(|g| *g != 0.0) || eq.pre != 0.0;
        eq
    }

    /// Takes the settings, and starts gliding to them from wherever things are now.
    fn read(&mut self) {
        let p = &self.params;
        self.seen = p.version.load(Ordering::Acquire);
        let on = p.on.load(Ordering::Relaxed);
        let sliders = if on { std::array::from_fn(|i| f32::from_bits(p.gains[i].load(Ordering::Relaxed))) } else { [0.0; 10] };
        // Turning it off still glides with room made, back up to flat's 0 dB.
        let no_clip = p.no_clip.load(Ordering::Relaxed);
        self.target = fit(&sliders, self.rate);
        self.target_pre = if no_clip { headroom_db(&self.target, self.rate) } else { 0.0 };
        self.from = self.gains;
        self.from_pre = self.pre;
        let far = (0..BANDS.len()).map(|i| (self.target[i] - self.from[i]).abs()).fold(0.0, f32::max);
        self.floor = self.from_pre.min(self.target_pre);
        if no_clip {
            // Partway, the curve can peak higher than at either end, so the level makes
            // room for the loudest of a few points along the way (finer found under 0.05 dB
            // more, inside the margin `headroom_db` leaves), and for how far filters on the
            // move throw a low tone past it: ~0.55 dB lifting a big cut off sub-bass. Under
            // 20 Hz, where music hardly goes, a full-scale tone can still go ~0.5 dB over
            // for a moment; making room for that too would dip the level on every drag.
            let partway = (1..8).map(|k| headroom_db(&std::array::from_fn(|i| lerp(self.from[i], self.target[i], k as f32 / 8.0)), self.rate));
            let room = partway.fold(self.target_pre, f32::min) - (far * 0.06).min(1.0);
            // Taken from what's needed, not from where the level is, so a drag restarting
            // this every few blocks doesn't sink it further each time.
            self.floor = self.floor.min(room);
        }
        let steps = |db: f32| (db / STEP_DB).ceil() as u32;
        (self.down, self.moving) = (steps(self.from_pre - self.floor), (far / MOVE_DB).ceil() as u32);
        self.step = 0;
        self.steps = self.down + self.moving + SETTLE + steps(self.target_pre - self.floor);
    }

    /// Once a block: take new settings, and move everything a step towards them.
    fn update(&mut self) {
        // The last block's ramp has arrived; land on it exactly.
        if self.pre_ramp != 0.0 {
            self.pre_gain = 10f64.powf(self.pre as f64 / 20.0);
            self.pre_ramp = 0.0;
        }
        if self.params.version.load(Ordering::Acquire) != self.seen {
            self.read();
        }
        if self.step < self.steps {
            self.step += 1;
            // How far along the stretch of `len` blocks starting after `start` this is.
            let along = |start: u32, len: u32| if self.step >= start + len { 1.0 } else { self.step.saturating_sub(start) as f32 / len.max(1) as f32 };
            let t = along(self.down, self.moving);
            for i in 0..BANDS.len() {
                let g = lerp(self.from[i], self.target[i], t);
                if g != self.gains[i] {
                    self.gains[i] = g;
                    self.coefs[i] = design(i, g, self.rate);
                }
            }
            // The level is down to the floor before the filters move and stays there until
            // they're done and rung out, so there's room for the curve all the way.
            let pre = if self.step <= self.down {
                lerp(self.from_pre, self.floor, along(0, self.down))
            } else {
                lerp(self.floor, self.target_pre, along(self.down + self.moving + SETTLE, self.steps - self.down - self.moving - SETTLE))
            };
            if pre != self.pre {
                self.pre = pre;
                self.pre_ramp = (10f64.powf(pre as f64 / 20.0) - self.pre_gain) / self.block as f64;
            }
            self.active = true;
        }
        if self.active {
            // Silence (pause) would otherwise ring the delays down into denormals, which
            // are slow on x86.
            let mut ringing = false;
            for d in self.state.iter_mut().flatten().flatten() {
                if d.abs() < 1e-20 {
                    *d = 0.0;
                }
                ringing |= d.abs() > 1e-9;
            }
            // Flat filters let the delays die away on their own; once they have, skip it all.
            if !ringing && self.step == self.steps && self.pre == 0.0 && self.pre_ramp == 0.0 && self.gains.iter().all(|g| *g == 0.0) {
                self.active = false;
                self.state.iter_mut().for_each(|c| *c = [[0.0; 2]; 10]);
            }
        }
    }

    /// One sample for `channel` (samples come interleaved, channel 0 first).
    pub fn process(&mut self, s: f32, channel: usize) -> f32 {
        if channel == 0 {
            if self.frame.is_multiple_of(self.block) {
                self.update();
            }
            self.frame = self.frame.wrapping_add(1);
            self.pre_gain += self.pre_ramp;
        }
        if !self.active {
            return s;
        }
        let Some(state) = self.state.get_mut(channel) else { return s };
        let mut x = s as f64 * self.pre_gain;
        for ((c, d), g) in self.coefs.iter().zip(state.iter_mut()).zip(&self.gains) {
            // A flat band that has rung out would hand `x` back as it is.
            if *g == 0.0 && d[0] == 0.0 && d[1] == 0.0 {
                continue;
            }
            // Transposed direct form II.
            let y = c.b0 * x + d[0];
            d[0] = c.b1 * x - c.a1 * y + d[1];
            d[1] = c.b2 * x - c.a2 * y;
            x = y;
        }
        // A broken sample would stay in the delays for good: start them over.
        let y = x as f32;
        if !y.is_finite() {
            *state = [[0.0; 2]; 10];
            return 0.0;
        }
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn eq(on: bool, gains: Gains, no_clip: bool, rate: u32) -> (Eq, Arc<EqParams>) {
        let p = Arc::new(EqParams::default());
        p.set(on, &gains, no_clip);
        (Eq::new(p.clone(), rate, 1), p)
    }

    /// Peak level of a sine at `f` through `eq`, after it has settled.
    fn gain_at(eq: &mut Eq, f: f64, rate: f64) -> f64 {
        let n = rate as usize;
        let mut peak = 0f64;
        for k in 0..n {
            let y = eq.process((2.0 * PI * f * k as f64 / rate).sin() as f32 * 0.25, 0) as f64;
            if k > n / 2 {
                peak = peak.max(y.abs());
            }
        }
        20.0 * (peak / 0.25).log10()
    }

    #[test]
    fn a_band_boosts_its_own_frequency() {
        let mut g = [0.0; 10];
        g[5] = 6.0;
        let (mut e, _) = eq(true, g, false, 48_000);
        assert!((gain_at(&mut e, 1000.0, 48_000.0) - 6.0).abs() < 0.3);
        assert!(gain_at(&mut e, 100.0, 48_000.0).abs() < 0.3);
    }

    #[test]
    fn the_curve_meets_the_sliders() {
        // Every preset, every mix of sliders all the way up or down (the hardest shapes),
        // and a spread of random ones.
        let mut cases: Vec<Gains> = PRESETS.iter().map(|(_, g)| *g).collect();
        cases.extend((0..1024u32).map(|m| std::array::from_fn(|i| if m >> i & 1 == 1 { MAX_DB } else { -MAX_DB })));
        cases.push([0.0, 0.0, 0.0, 0.0, 0.0, MAX_DB, -MAX_DB, MAX_DB, 5.0, 6.0]);
        let mut seed = 1u32;
        cases.extend((0..200).map(|_| {
            std::array::from_fn(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((seed >> 8) % 49) as f32 / 2.0 - MAX_DB
            })
        }));
        for rate in [8_000.0, 22_050.0, 44_100.0, 48_000.0, 96_000.0] {
            let mut worst = 0f64;
            for s in &cases {
                let coefs = design_all(&fit(s, rate), rate);
                for i in 0..bands_at(rate) {
                    worst = worst.max((curve_db(&coefs, BANDS[i] as f64, rate) - s[i] as f64).abs());
                }
            }
            assert!(worst < 0.1, "{rate}: off by {worst} dB");
        }
    }

    #[test]
    fn the_top_bands_dont_leave_a_dip_between_them() {
        for rate in [44_100.0, 48_000.0] {
            let coefs = design_all(&fit(&[MAX_DB; 10], rate), rate);
            let mid = curve_db(&coefs, (8_000f64 * 16_000.0).sqrt(), rate);
            assert!((mid - MAX_DB as f64).abs() < 0.5, "{rate}: {mid}");
        }
    }

    #[test]
    fn headroom_matches_the_loudest_point() {
        assert_eq!(headroom_db(&[0.0; 10], 48_000.0), 0.0);
        // Cuts only need the safety margin, if that.
        assert!(headroom_db(&fit(&[-6.0; 10], 48_000.0), 48_000.0) >= -0.2);
        let h = headroom_db(&fit(&PRESETS[1].1, 48_000.0), 48_000.0);
        assert!(h < -6.0 && h > -7.5, "{h}");
        // The curve past 20 kHz counts too, which a 48 kHz device still plays.
        let mut g = [0.0; 10];
        g[9] = 12.0;
        let curve = response(&g);
        let peak = (0..=2400).map(|k| curve(k as f32 * 10.0)).fold(0.0, f32::max);
        assert!(-headroom_db(&fit(&g, 48_000.0), 48_000.0) >= peak, "{peak}");
    }

    #[test]
    fn headroom_finds_peaks_sharper_than_its_points() {
        // Near where a band gets left out, it peaks sharp just under Nyquist.
        let mut seed = 7u32;
        let mut rnd = || -> Gains { std::array::from_fn(|_| { seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223); if (seed >> 9) & 1 == 1 { 12.0 } else { -12.0 } }) };
        let mut cases = vec![(35_556.0, [-12.0, 12.0, -12.0, 12.0, 12.0, -12.0, -12.0, 12.0, -12.0, 12.0])];
        for b in &BANDS[6..] {
            for r in [0.999, 1.001, 1.01] {
                (0..8).for_each(|_| cases.push((*b as f64 / 0.45 * r, rnd())));
            }
        }
        for (rate, sliders) in cases {
            let g = fit(&sliders, rate);
            let coefs = design_all(&g, rate);
            let peak = (0..=20_000).map(|k| curve_db(&coefs, rate / 2.0 * k as f64 / 20_000.0, rate)).fold(0.0, f64::max);
            assert!(-headroom_db(&g, rate) as f64 >= peak, "{rate} {sliders:?}: {peak}");
        }
    }

    #[test]
    fn a_loud_low_tone_stays_under_full_scale_through_a_change() {
        let flip = [12.0, -12.0, 12.0, 12.0, 12.0, -12.0, 12.0, 12.0, -12.0, 12.0];
        let other = [12.0, 12.0, 12.0, -12.0, -12.0, -12.0, -12.0, -12.0, -12.0, -12.0];
        let classical = PRESETS.iter().find(|p| p.0 == "Classical").unwrap().1;
        let cases = [(flip, other, 31.25, 48_000), (other, flip, 31.25, 48_000), (classical, PRESETS[1].1, 62.5, 48_000), (PRESETS[1].1, [0.0; 10], 62.5, 48_000), ([-12.0; 10], [0.0; 10], 10.0, 192_000), ([-12.0; 10], [0.0; 10], 12.0, 44_100), ([-2.0; 10], [0.0; 10], 20.0, 192_000)];
        // Picking the curve, and turning it off from where it was.
        for (on, (from, to, f, rate)) in cases.into_iter().flat_map(|c| [(true, c), (false, c)]) {
            let (mut e, p) = eq(true, from, true, rate);
            let w = 2.0 * PI * f / rate as f64;
            let mut tone = (0..).map(|k| (w * k as f64).sin() as f32);
            (0..rate).for_each(|_| _ = e.process(tone.next().unwrap(), 0));
            p.set(on, &to, true);
            let peak = (0..rate * 2).map(|_| e.process(tone.next().unwrap(), 0).abs()).fold(0.0, f32::max);
            // Reaching flat, the tone itself is full scale: only rounding over.
            assert!(peak <= 1.0001, "{from:?} to {to:?} ({on}) at {f} Hz, {rate}: {peak}");
        }
    }

    #[test]
    fn dragging_a_slider_doesnt_sink_the_level() {
        let (mut e, p) = eq(true, PRESETS[1].1, true, 48_000);
        let mut g = PRESETS[1].1;
        let target = headroom_db(&fit(&g, 48_000.0), 48_000.0);
        // Ten seconds of a new setting every five blocks.
        for k in 0..3_000 {
            g[0] = if k % 2 == 0 { 5.5 } else { 6.0 };
            p.set(true, &g, true);
            (0..e.block * 5).for_each(|_| _ = e.process(0.0, 0));
            assert!(e.pre > target - 1.5, "{k}: {}", e.pre);
        }
    }

    #[test]
    fn a_broken_sample_doesnt_stick() {
        let (mut e, _) = eq(true, PRESETS[1].1, false, 48_000);
        assert_eq!(e.process(f32::NAN, 0), 0.0);
        assert!((0..1000).all(|k| e.process((k as f32 * 0.1).sin(), 0).is_finite()));
    }

    #[test]
    fn the_level_makes_room_before_boosts_and_takes_it_back_after() {
        let (mut e, p) = eq(true, [0.0; 10], true, 48_000);
        // Presets, and one slider flipped on a sharp shape, where the curve partway peaks
        // higher than at either end.
        let sharp = [-12.0, -12.0, 12.0, 12.0, -12.0, 12.0, -12.0, -12.0, -12.0, 12.0];
        let mut flipped = sharp;
        flipped[4] = 12.0;
        for (from, to) in [([0.0; 10], PRESETS[1].1), (PRESETS[1].1, [0.0; 10]), (PRESETS[1].1, PRESETS[2].1), (sharp, flipped), (flipped, sharp)] {
            p.set(true, &from, true);
            (0..48_000).for_each(|_| _ = e.process(0.0, 0));
            p.set(true, &to, true);
            for k in 0..24_000 {
                e.process(0.0, 0);
                if k % e.block as usize == 1 {
                    // Whatever the curve is partway, the level leaves room for all of it.
                    assert!(e.pre <= headroom_db(&e.gains, 48_000.0) + 0.05, "{k}: {} {:?}", e.pre, e.gains);
                }
            }
            let to = fit(&to, 48_000.0);
            assert_eq!((e.gains, e.pre), (to, headroom_db(&to, 48_000.0)));
        }
    }

    #[test]
    fn reaching_flat_or_off_glides_to_passing_samples_through_without_a_click() {
        let mut g = [0.0; 10];
        (g[0], g[5]) = (6.0, 6.0);
        // Sliders to flat at full level, and turning it off with the level made room.
        for (on, no_clip) in [(true, false), (false, true)] {
            let (mut e, p) = eq(true, g, no_clip, 48_000);
            p.set(on, &if on { [0.0; 10] } else { g }, no_clip);
            let w = 2.0 * PI * 1000.0 / 48_000.0;
            let mut y = [0f64; 3];
            for k in 0..48_000 {
                y = [y[1], y[2], e.process((w * k as f64).sin() as f32 * 0.5, 0) as f64];
                // A 1 kHz tone's own curvature, at up to 6 dB louder: an impulse would stand out.
                if k > 2 {
                    assert!((y[2] - 2.0 * y[1] + y[0]).abs() < 0.5 * 2.0 * w * w * 1.05, "{on}: click at {k}");
                }
            }
            assert!(!e.active);
            assert_eq!(e.process(0.3, 0), 0.3);
        }
    }

    #[test]
    fn bands_past_what_the_rate_holds_are_left_out() {
        let mut g = [0.0; 10];
        g[9] = 12.0;
        let (mut e, _) = eq(true, g, true, 22_050);
        // 16 kHz is past what 22.05 kHz holds, so that slider does nothing.
        assert_eq!(bands_at(22_050.0), 9);
        assert_eq!(e.gains, [0.0; 10]);
        assert_eq!(e.process(0.3, 0), 0.3);
    }

    #[test]
    fn a_device_opened_mid_song_starts_at_the_settings() {
        let (e, _) = eq(true, PRESETS[1].1, true, 48_000);
        assert_eq!((e.gains, e.pre, e.step), (fit(&PRESETS[1].1, 48_000.0), headroom_db(&e.gains, 48_000.0), e.steps));
    }

    #[test]
    fn channels_keep_to_themselves() {
        let p = Arc::new(EqParams::default());
        p.set(true, &PRESETS[1].1, true);
        let mut e = Eq::new(p, 48_000, 2);
        for k in 0..4_800 {
            e.process((k as f32 * 0.05).sin(), 0);
            assert_eq!(e.process(0.0, 1), 0.0, "{k}");
        }
    }
}
