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
    process_small(&image::imageops::resize(img, SIZE, SIZE, image::imageops::FilterType::Triangle))
}

/// The accent, tint and blur of a cover already SIZE×SIZE.
fn process_small(small: &image::RgbImage) -> Art {
    let accent = palette(small);
    backdrop(small.pixels().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect(), accent)
}

/// A frame of an animated cover as the cover art, as a still cover's would be (accent,
/// tint and blur), sampled nearest-neighbour like [`from_frame`].
pub fn from_cover_frame(frame: &egui::ColorImage) -> Option<Art> {
    let ([w, h], n) = (frame.size, SIZE as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let small = image::RgbImage::from_fn(SIZE, SIZE, |x, y| {
        let c = frame.pixels[(y as usize * h / n) * w + x as usize * w / n];
        image::Rgb([c.r(), c.g(), c.b()])
    });
    Some(process_small(&small))
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
    use super::{READABLE_CAP, READABLE_KNEE, from_cover_frame, process_rgb, readable_dim};

    #[test]
    fn a_cover_frame_gives_a_still_covers_colours() {
        // Flat colour: sampling it small can't tell nearest from Triangle.
        let frame = egui::ColorImage::new([300, 300], vec![egui::Color32::from_rgb(180, 60, 120); 300 * 300]);
        let still = process_rgb(&image::RgbImage::from_pixel(300, 300, image::Rgb([180, 60, 120])));
        let art = from_cover_frame(&frame).unwrap();
        assert_eq!((art.accent, art.rgba), (still.accent, still.rgba));
        assert!(from_cover_frame(&egui::ColorImage::new([0, 0], vec![])).is_none());
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
