//! Album-art processing for the animated background and accent colours.
//!
//! Follows Kawarp's pipeline (MIT, Better Lyrics, used by better-lyrics-shaders):
//! tint the dark areas, then blur heavily. Kawarp does this on the GPU every frame;
//! here it runs once per track on a 64×64 image, so the per-frame shader is cheap.

use egui::Color32;

pub const SIZE: u32 = 64;

#[derive(Clone)]
pub struct Art {
    /// SIZE×SIZE RGBA, already tinted and blurred.
    pub rgba: Vec<u8>,
    /// Most vivid colour, lifted for use on dark backgrounds (progress bar, highlights).
    pub accent: Color32,
    /// How bright the cover is overall, 0..1.
    pub brightness: f32,
}

pub fn process(bytes: &[u8]) -> anyhow::Result<Art> {
    let img = image::load_from_memory(bytes)?.to_rgb8();
    Ok(process_rgb(&img))
}

/// The still-cover pipeline over a decoded image: resize to SIZE×SIZE with Triangle,
/// pick the accent, tint the dark areas toward it, then blur.
fn process_rgb(img: &image::RgbImage) -> Art {
    let small = image::imageops::resize(img, SIZE, SIZE, image::imageops::FilterType::Triangle);
    let accent = palette(&small);
    backdrop(small.pixels().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect(), accent)
}

/// SIZE×SIZE pixels (0–255) made into the backdrop, tinted toward `accent`.
fn backdrop(mut px: Vec<[f32; 3]>, accent: Color32) -> Art {
    let brightness = px.iter().map(|p| (0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2]) / 255.0).sum::<f32>() / px.len().max(1) as f32;

    // Kawarp's tint pass: pull near-black regions toward the accent so dark covers
    // still produce a coloured backdrop instead of a black one.
    let tint = [accent.r() as f32, accent.g() as f32, accent.b() as f32];
    for p in &mut px {
        let luma = (0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2]) / 255.0;
        let dark = 1.0 - smoothstep(0.0, 0.5, luma);
        for c in 0..3 {
            p[c] += (tint[c] - p[c]) * dark * 0.45;
        }
    }
    // Three box-blur passes approximate a wide Gaussian (Kawarp runs 8 Kawase passes).
    for _ in 0..3 {
        px = box_blur(&px, SIZE as usize, 6);
    }
    let rgba = px.iter().flat_map(|p| [p[0] as u8, p[1] as u8, p[2] as u8, 255]).collect();
    Art { rgba, accent, brightness }
}

/// An animated cover's colours, from frames spread over its loop. A clip often opens on
/// a close-up or a fade-in, so no one frame stands for it, and a small vivid detail (an
/// eye, a logo) that a close-up fills the frame with shouldn't set the accent either:
/// it's the most vivid colour of the hue that's vivid over the most of the clip. The
/// backdrop is the frames' average.
#[derive(Default)]
pub struct ClipColours {
    /// Each pixel's sum over the frames, SIZE×SIZE.
    sum: Vec<[f32; 3]>,
    frames: u32,
    /// Per hue: how vivid it is over the clip (its pixels' scores summed), and its most
    /// vivid pixel with that pixel's score.
    hues: [(f32, f32, [f32; 3]); HUES],
}

/// Hue bins a clip's colours are counted in.
const HUES: usize = 24;

impl ClipColours {
    pub fn frames(&self) -> u32 {
        self.frames
    }

    pub fn add(&mut self, frame: &egui::ColorImage) {
        let [w, h] = frame.size;
        let rgb: Vec<u8> = frame.pixels.iter().flat_map(|p| [p.r(), p.g(), p.b()]).collect();
        let Some(img) = image::RgbImage::from_raw(w as u32, h as u32, rgb).filter(|_| w > 0 && h > 0) else { return };
        let small = image::imageops::resize(&img, SIZE, SIZE, image::imageops::FilterType::Triangle);
        self.sum.resize((SIZE * SIZE) as usize, [0.0; 3]);
        for (s, p) in self.sum.iter_mut().zip(small.pixels()) {
            let rgb = [p[0] as f32, p[1] as f32, p[2] as f32];
            for c in 0..3 {
                s[c] += rgb[c];
            }
            let score = vivid(rgb.map(|v| v / 255.0));
            let bin = &mut self.hues[(hue(rgb) * HUES as f32) as usize % HUES];
            bin.0 += score;
            if score > bin.1 {
                (bin.1, bin.2) = (score, rgb);
            }
        }
        self.frames += 1;
    }

    /// The clip's colours, once it has a frame.
    pub fn art(&self) -> Option<Art> {
        if self.frames == 0 {
            return None;
        }
        let n = self.frames as f32;
        let px: Vec<[f32; 3]> = self.sum.iter().map(|s| s.map(|v| v / n)).collect();
        let avg = px.iter().fold([0.0; 3], |a, p| [a[0] + p[0], a[1] + p[1], a[2] + p[2]]).map(|v| v / px.len() as f32);
        let best = self.hues.iter().max_by(|a, b| a.0.total_cmp(&b.0)).filter(|b| b.1 > 0.05);
        let accent = lift(best.map_or(avg, |b| b.2));
        Some(backdrop(px, accent))
    }
}

/// `passes` more of the art's blur (a theme's `blurPasses` over the usual).
pub fn soften(art: &mut Art, passes: u32) {
    if passes == 0 {
        return;
    }
    let n = SIZE as usize;
    let mut px: Vec<[f32; 3]> = art.rgba.as_chunks::<4>().0.iter().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect();
    for _ in 0..passes.min(6) {
        px = box_blur(&px, n, 3);
    }
    art.rgba = px.iter().flat_map(|p| [p[0] as u8, p[1] as u8, p[2] as u8, 255]).collect();
}

/// A video frame as background art: sampled small and blurred, cheap enough a few times a second.
pub fn from_frame(frame: &egui::ColorImage) -> Option<Art> {
    let ([w, h], n) = (frame.size, SIZE as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let mut px = Vec::with_capacity(n * n);
    for y in 0..n {
        for x in 0..n {
            let c = frame.pixels[(y * h / n) * w + x * w / n];
            px.push([c.r() as f32, c.g() as f32, c.b() as f32]);
        }
    }
    let brightness = px.iter().map(|p| (0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2]) / 255.0).sum::<f32>() / px.len() as f32;
    for _ in 0..2 {
        px = box_blur(&px, n, 4);
    }
    let rgba = px.iter().flat_map(|p| [p[0] as u8, p[1] as u8, p[2] as u8, 255]).collect();
    Some(Art { rgba, accent: Color32::WHITE, brightness })
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn box_blur(src: &[[f32; 3]], n: usize, r: usize) -> Vec<[f32; 3]> {
    let pass = |src: &[[f32; 3]], horizontal: bool| -> Vec<[f32; 3]> {
        let mut out = vec![[0.0; 3]; src.len()];
        for y in 0..n {
            for x in 0..n {
                let mut acc = [0.0; 3];
                for k in 0..=2 * r {
                    let o = k as isize - r as isize;
                    let (sx, sy) = if horizontal {
                        ((x as isize + o).clamp(0, n as isize - 1) as usize, y)
                    } else {
                        (x, (y as isize + o).clamp(0, n as isize - 1) as usize)
                    };
                    let p = src[sy * n + sx];
                    for c in 0..3 {
                        acc[c] += p[c];
                    }
                }
                let d = (2 * r + 1) as f32;
                out[y * n + x] = [acc[0] / d, acc[1] / d, acc[2] / d];
            }
        }
        out
    };
    pass(&pass(src, true), false)
}

/// The most vivid colour (falling back to the average for greyscale covers).
fn palette(img: &image::RgbImage) -> Color32 {
    let (mut sum, mut n) = ([0.0f32; 3], 0.0f32);
    let mut best = (0.0f32, [200.0, 60.0, 80.0]);
    for p in img.pixels() {
        let rgb = [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0];
        for c in 0..3 {
            sum[c] += rgb[c];
        }
        n += 1.0;
        let score = vivid(rgb);
        if score > best.0 {
            best = (score, [rgb[0] * 255.0, rgb[1] * 255.0, rgb[2] * 255.0]);
        }
    }
    let avg = [sum[0] / n * 255.0, sum[1] / n * 255.0, sum[2] / n * 255.0];
    let accent = if best.0 > 0.05 { best.1 } else { avg };
    lift(accent)
}

/// How vivid a colour (0–1) is as an accent: saturated, mid-to-bright pixels score highest.
fn vivid(rgb: [f32; 3]) -> f32 {
    let (max, min) = (rgb[0].max(rgb[1]).max(rgb[2]), rgb[0].min(rgb[1]).min(rgb[2]));
    let sat = if max > 0.0 { (max - min) / max } else { 0.0 };
    sat * sat * (1.0 - (max - 0.75).abs())
}

/// A colour's hue, 0..1.
fn hue([r, g, b]: [f32; 3]) -> f32 {
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let d = max - min;
    if d <= 0.0 {
        return 0.0;
    }
    let h = if max == r { ((g - b) / d).rem_euclid(6.0) } else if max == g { (b - r) / d + 2.0 } else { (r - g) / d + 4.0 };
    h / 6.0
}

/// Raise brightness so the colour reads on a dark UI.
fn lift(c: [f32; 3]) -> Color32 {
    let max = c[0].max(c[1]).max(c[2]).max(1.0);
    let k = (220.0 / max).max(1.0);
    Color32::from_rgb((c[0] * k).min(255.0) as u8, (c[1] * k).min(255.0) as u8, (c[2] * k).min(255.0) as u8)
}

/// A neutral backdrop for when nothing is playing.
pub fn idle() -> Art {
    let n = SIZE as usize;
    let mut rgba = Vec::with_capacity(n * n * 4);
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f32 / n as f32, y as f32 / n as f32);
            let r = 40.0 + 70.0 * (1.0 - fy) * fx;
            let g = 18.0 + 10.0 * fy;
            let b = 46.0 + 60.0 * fy * (1.0 - fx);
            rgba.extend_from_slice(&[r as u8, g as u8, b as u8, 255]);
        }
    }
    Art { rgba, accent: Color32::from_rgb(0xff, 0x4e, 0x6a), brightness: 0.3 }
}

/// Below this, a background behind white text is left as the theme has it.
const READABLE_KNEE: f32 = 0.2;
/// What brighter backgrounds are eased toward and never quite reach.
const READABLE_CAP: f32 = 0.3;

/// How much to darken a background whose overall brightness, as it would show (0..1),
/// is `shown`, for white text over it to stay readable (Settings › Appearance › Dim bright
/// covers). 1 leaves it be: anything up to [`READABLE_KNEE`] is, so dark covers aren't
/// dulled. Above that the brightness eases toward [`READABLE_CAP`], darkening a bright
/// cover in proportion to how far over it is.
pub fn readable_dim(shown: f32) -> f32 {
    if !shown.is_finite() || shown <= READABLE_KNEE {
        return 1.0;
    }
    let room = READABLE_CAP - READABLE_KNEE;
    let eased = READABLE_KNEE + room * (1.0 - (-(shown - READABLE_KNEE) / room).exp());
    eased / shown
}

#[cfg(test)]
mod tests {
    use super::{ClipColours, READABLE_CAP, READABLE_KNEE, process_rgb, readable_dim};

    fn frame(w: usize, px: impl Fn(usize, usize) -> [u8; 3]) -> egui::ColorImage {
        let pixels = (0..w * w).map(|i| {
            let [r, g, b] = px(i % w, i / w);
            egui::Color32::from_rgb(r, g, b)
        });
        egui::ColorImage { size: [w, w], source_size: egui::vec2(w as f32, w as f32), pixels: pixels.collect() }
    }

    #[test]
    fn one_frame_of_one_colour_gives_a_still_covers_colours() {
        let f = frame(200, |_, _| [180, 60, 120]);
        let rgb: Vec<u8> = f.pixels.iter().flat_map(|p| [p.r(), p.g(), p.b()]).collect();
        let still = process_rgb(&image::RgbImage::from_raw(200, 200, rgb).unwrap());
        let mut clip = ClipColours::default();
        assert!(clip.art().is_none());
        clip.add(&f);
        let art = clip.art().unwrap();
        assert_eq!((art.accent, art.rgba.clone()), (still.accent, still.rgba));
        assert!((art.brightness - still.brightness).abs() < 1e-5);
    }

    #[test]
    fn a_clips_accent_is_the_colour_most_of_it_has_not_a_small_detail() {
        // A pink sky with a small, more vivid blue eye: a still cover's accent would be the
        // eye; a clip's is the sky, however many close-ups of the eye it has.
        let sky = |x: usize, y: usize| if x < 12 && y < 12 { [40, 170, 240] } else { [200, 90, 170] };
        let rgb: Vec<u8> = (0..120 * 120).flat_map(|i| sky(i % 120, i / 120)).collect();
        let still = process_rgb(&image::RgbImage::from_raw(120, 120, rgb).unwrap()).accent;
        assert!(still.b() > still.r(), "{still:?}");
        let mut clip = ClipColours::default();
        clip.add(&frame(120, sky));
        clip.add(&frame(120, |x, y| if x < 40 && y < 40 { [40, 170, 240] } else { [200, 90, 170] }));
        let accent = clip.art().unwrap().accent;
        assert!(accent.r() > accent.b() && accent.r() > accent.g(), "{accent:?}");
        // The backdrop is both frames, not the last one.
        let mut last = ClipColours::default();
        last.add(&frame(120, |x, y| if x < 40 && y < 40 { [40, 170, 240] } else { [200, 90, 170] }));
        assert_ne!(clip.art().unwrap().rgba, last.art().unwrap().rgba);
    }

    #[test]
    fn dark_backgrounds_are_left_alone() {
        for shown in [0.0, 0.1, 0.2, READABLE_KNEE] {
            assert_eq!(readable_dim(shown), 1.0);
        }
        assert_eq!(readable_dim(f32::NAN), 1.0);
    }

    #[test]
    fn bright_backgrounds_are_eased_under_the_cap() {
        let mut last = READABLE_KNEE;
        for i in 1..=72 {
            let shown = READABLE_KNEE + i as f32 * 0.01;
            let after = shown * readable_dim(shown);
            // Brighter art still shows brighter, just never past the cap…
            assert!(after > last && after < READABLE_CAP, "{shown} -> {after}");
            last = after;
        }
        // …and the darkening grows with it: barely over the knee, hardly any.
        assert!(readable_dim(READABLE_KNEE + 0.01) > 0.98);
        assert!(readable_dim(1.0) < 0.41);
    }
}
