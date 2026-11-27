//! Album-art processing for the animated background and accent colours.
//!
//! Follows Kawarp's pipeline (MIT, Better Lyrics, used by better-lyrics-shaders):
//! tint the dark areas, then blur heavily. Kawarp does this on the GPU every frame;
//! here it runs once per track on a 64×64 image, so the per-frame shader is cheap.
//!
//! The palette pass reads the sleeve the way it looks, not the way its pixels
//! compute. HSV saturation runs sky-high on near-black pixels, so a black cover
//! with a faint warm cast would score its noise as "vivid"; lifting that for the
//! UI then floods the whole app with amber. So vividness only counts where colour
//! is actually visible, the pick is a bucket of every scoring pixel rather than
//! one, greyscale covers keep a greyscale palette, and both the accent lift and
//! the tint pull are bounded.

use egui::Color32;

pub const SIZE: u32 = 64;

/// How far dark pixels are pulled toward the accent, at full colour (Kawarp's 45%).
const TINT: f32 = 0.45;
/// The tint aims at the accent dimmed to this luma: the UI accent is lifted to read
/// on dark surfaces, and pulling the backdrop toward that bright a colour glows
/// brighter than the sleeve it stands for.
const TINT_LUMA: f32 = 110.0;
/// A pixel scores into the vivid bucket above this.
const VIVID_SCORE: f32 = 0.05;
/// The bucket only picks the accent if it holds this share of the cover: less is
/// noise, even when the noise out-scores everything else on the sleeve.
const VIVID_SHARE: f32 = 0.015;
/// The vivid bucket is split into this many hue sectors (30° each), and the accent
/// comes from the strongest.
const HUE_BINS: usize = 12;
/// The least luma a lifted accent has: about 3:1 on the dark UI, even for pure blue.
const ACCENT_LUMA: f32 = 90.0;
/// Below this much colour a cover counts as greyscale: neutral accent, no tint.
const GREYSCALE: f32 = 0.05;

#[derive(Clone)]
pub struct Art {
    /// SIZE×SIZE RGBA, already tinted and blurred.
    pub rgba: Vec<u8>,
    /// Most vivid colour, lifted for use on dark backgrounds (progress bar,
    /// highlights). A greyscale cover gets a neutral grey instead, so the UI never
    /// wears a hue the sleeve doesn't have.
    pub accent: Color32,
    /// How bright the cover is overall, 0..1.
    pub brightness: f32,
}

/// A picture as a playlist cover: its middle square (covers are square, and the sleeve
/// would crop it anyway), at most `COVER_PX` across, as a JPEG. Turned the way the camera
/// says (EXIF), and see-through parts on black: kept as stored, they could be any colour.
pub fn playlist_cover(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    use image::ImageDecoder;
    const COVER_PX: u32 = 1200;
    let mut decoder = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?.into_decoder()?;
    // What `decode` checks before it allocates: a huge picture fails here, not in memory.
    image::Limits::default().reserve(decoder.total_bytes())?;
    let orientation = decoder.orientation()?;
    let mut img = image::DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);
    let img = match img.color().has_alpha() {
        true => {
            let rgba = img.to_rgba8();
            image::RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
                let [r, g, b, a] = rgba.get_pixel(x, y).0;
                let on_black = |c: u8| (c as u16 * a as u16 / 255) as u8;
                image::Rgb([on_black(r), on_black(g), on_black(b)])
            })
        }
        false => img.to_rgb8(),
    };
    let side = img.width().min(img.height());
    anyhow::ensure!(side > 0, "the picture is empty");
    let square = image::imageops::crop_imm(&img, (img.width() - side) / 2, (img.height() - side) / 2, side, side).to_image();
    let square = if side > COVER_PX { image::imageops::resize(&square, COVER_PX, COVER_PX, image::imageops::FilterType::Lanczos3) } else { square };
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90).encode_image(&square)?;
    Ok(jpeg)
}

pub fn process(bytes: &[u8]) -> anyhow::Result<Art> {
    let img = image::load_from_memory(bytes)?.to_rgb8();
    Ok(process_rgb(&img))
}

/// The still-cover pipeline over a decoded image: the sleeve's square crop, resized to
/// SIZE×SIZE, then [`process_small`].
fn process_rgb(img: &image::RgbImage) -> Art {
    // The sleeve shows the thumbnail's square crop (`Thumb::square_uv`); a raw one is
    // letterboxed 4:3 for many videos, and the backdrop's bars would tint toward the
    // accent as bands of colour nothing else shows. Crop first, like the sleeve does
    // (a view, not a copy: an embedded cover can be thousands of pixels across).
    let (w, h) = img.dimensions();
    let (x, y, cw, ch) = crate::model::square_crop_px(w, h).map_or((0, 0, w, h), |(x, y, side)| (x, y, side, side));
    let view = image::imageops::crop_imm(img, x, y, cw, ch);
    process_small(&image::imageops::resize(&*view, SIZE, SIZE, image::imageops::FilterType::Triangle))
}

/// The accent, tint and blur of a cover already SIZE×SIZE.
fn process_small(small: &image::RgbImage) -> Art {
    let look = analyse(small);
    let px: Vec<[f32; 3]> = small.pixels().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect();
    backdrop(px, &look)
}

/// A frame of an animated cover as the cover art, as a still cover's would be (accent,
/// tint and blur), sampled nearest-neighbour so it stays cheap. The clip is stretched
/// to the square sleeve as it plays (`Image::paint_at` with no uv), so the colours are
/// read stretched the same way, not centre-cropped.
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

/// The backdrop from SIZE×SIZE pixels (0–255): tinted toward the accent, then blurred.
fn backdrop(mut px: Vec<[f32; 3]>, look: &Look) -> Art {
    let brightness = px.iter().map(|p| (0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2]) / 255.0).sum::<f32>() / px.len().max(1) as f32;

    // Kawarp's tint pass: pull near-black regions toward the accent so dark covers
    // still produce a coloured backdrop instead of a black one. Two guards keep the
    // wash honest to the sleeve: it scales with how much colour the cover carries
    // (a greyscale one stays greyscale), and it aims at a dimmed accent.
    let mut tint = [look.accent.r() as f32, look.accent.g() as f32, look.accent.b() as f32];
    let tint_luma = 0.299 * tint[0] + 0.587 * tint[1] + 0.114 * tint[2];
    if tint_luma > TINT_LUMA {
        let k = TINT_LUMA / tint_luma;
        for c in &mut tint {
            *c *= k;
        }
    }
    for p in &mut px {
        let luma = (0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2]) / 255.0;
        let dark = 1.0 - smoothstep(0.0, 0.5, luma);
        let pull = dark * TINT * look.colour;
        for c in 0..3 {
            p[c] += (tint[c] - p[c]) * pull;
        }
    }
    // Three box-blur passes approximate a wide Gaussian (Kawarp runs 8 Kawase passes).
    for _ in 0..3 {
        px = box_blur(&px, SIZE as usize, 6);
    }
    let rgba = px.iter().flat_map(|p| [p[0] as u8, p[1] as u8, p[2] as u8, 255]).collect();
    Art { rgba, accent: readable(look.accent), brightness }
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

/// Two box-blur passes (one horizontal, one vertical), which together read as a
/// Gaussian.
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

/// What one pass over the cover says: the accent for the UI, and how much colour
/// the sleeve carries (0..1) — the tint pull scales with it, so a greyscale cover
/// keeps a greyscale backdrop.
struct Look {
    accent: Color32,
    colour: f32,
}

/// The accent and the cover's colourfulness, from one pass over the pixels.
///
/// Vividness is scored only where colour is visible (a bright `max`): saturation
/// divided by `max` explodes in the dark, and a black cover's warm compression
/// noise out-scores every real colour on it. Scoring pixels go into a bucket split
/// by hue; the strongest hue is averaged by score — one noisy pixel can't repaint
/// the accent alone — and the bucket must hold a real share of the cover before it
/// speaks for it. What's left
/// is judged by how much colour the sleeve carries overall: a greyscale one gets a
/// neutral accent (dark covers invert to near-white, like their own ink; pale ones
/// settle at mid grey), and a cover whose colour is a diffuse cast rather than a
/// vivid area gets its mean hue faded toward grey.
fn analyse(img: &image::RgbImage) -> Look {
    let (mut sum, mut n) = ([0.0f32; 3], 0.0f32);
    // The vivid bucket, split by hue: averaging every vivid pixel together mixes the
    // sleeve's colours into one it doesn't show (blue and yellow make grey, green and
    // purple a pale mint). Each bin holds score-weighted RGB, the weight and the count.
    let (mut chroma, mut bins, mut bucket_n) = (0.0f32, [[0.0f32; 5]; HUE_BINS], 0.0f32);
    for p in img.pixels() {
        let rgb = [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0];
        for c in 0..3 {
            sum[c] += rgb[c];
        }
        n += 1.0;
        let (max, min) = (rgb[0].max(rgb[1]).max(rgb[2]), rgb[0].min(rgb[1]).min(rgb[2]));
        chroma += max - min;
        // Favour saturated, mid-to-bright pixels — but only bright enough that the
        // colour could be seen at a glance; 0.45 is past the casts dark sleeves carry.
        if max <= 0.45 {
            continue;
        }
        let sat = (max - min) / max;
        let score = sat * sat * (1.0 - (max - 0.75).abs()) * smoothstep(0.45, 0.6, max);
        if score > VIVID_SCORE {
            let bin = &mut bins[hue_bin(rgb, max, min)];
            for c in 0..3 {
                bin[c] += rgb[c] * score;
            }
            bin[3] += score;
            bin[4] += 1.0;
            bucket_n += 1.0;
        }
    }
    let mean = [sum[0] / n, sum[1] / n, sum[2] / n];
    // How colourful the sleeve is: its mean chroma, helped out by the vivid bucket's
    // share — nine-tenths black around one bright red rose should still tint a little.
    let raw = (chroma / n + bucket_n / n * 0.35).clamp(0.0, 1.0);
    let colour = smoothstep(GREYSCALE, 0.26, raw);
    // A greyscale sleeve's accent follows its tone rather than inventing a hue: a
    // dark one inverts to near-white (the sleeve's own ink), a pale one settles at
    // mid grey; both read against the blurred backdrop.
    let luma = 0.299 * mean[0] + 0.587 * mean[1] + 0.114 * mean[2];
    let neutral = if luma > 0.6 { 140.0 } else { 210.0 };
    // Each hue with the bins either side, so a hue on a bin edge isn't split — but only
    // where the middle bin is a real part of the window, so a near-empty bin can't join
    // two hues either side of it into one between them. Only a hue covering a real
    // share of the sleeve can speak for it.
    let group = |i: usize| {
        let mut g = [0.0f32; 5];
        for d in [HUE_BINS - 1, 0, 1] {
            let bin = bins[(i + d) % HUE_BINS];
            for c in 0..5 {
                g[c] += bin[c];
            }
        }
        g
    };
    let centred = |&i: &usize| bins[i][3] * 4.0 >= group(i)[3];
    let vivid = (0..HUE_BINS).filter(centred).map(group).filter(|g| g[4] / n >= VIVID_SHARE).max_by(|a, b| a[3].total_cmp(&b[3]));
    let accent = if let Some(v) = vivid {
        // A bright vivid area the sleeve really shows: the score-weighted colour of
        // its strongest hue, lifted for the UI. Full strength, whatever the cover's
        // overall greyness — this is the sleeve's identity, not its cast.
        lift([v[0] / v[3] * 255.0, v[1] / v[3] * 255.0, v[2] / v[3] * 255.0], 3.5)
    } else if colour <= 0.0 {
        Color32::from_gray(neutral as u8)
    } else {
        // Colourful overall but nothing bright and vivid to point at (a deep navy
        // sleeve, say): the mean, lifted gently. Amplifying a dark mean without
        // bound is the other way a whisper of colour became a bright wash.
        let mut c = lift([mean[0] * 255.0, mean[1] * 255.0, mean[2] * 255.0], 3.0);
        // The hue here is a wash, not a choice, so fade it toward the greyscale
        // accent as the colour drains: the boundary to greyscale is a fade, not a
        // cliff (fading to its own grey left a dim, hard-to-read accent there).
        let keep = (colour * 2.0).min(1.0);
        if keep < 1.0 {
            let fade = |v: u8| (neutral + (f32::from(v) - neutral) * keep) as u8;
            c = Color32::from_rgb(fade(c.r()), fade(c.g()), fade(c.b()));
        }
        c
    };
    Look { accent, colour }
}

/// Which of [`HUE_BINS`] equal hue sectors a pixel with chroma (`max > min`) falls in.
fn hue_bin(rgb: [f32; 3], max: f32, min: f32) -> usize {
    let d = max - min;
    let h = if max == rgb[0] {
        ((rgb[1] - rgb[2]) / d).rem_euclid(6.0)
    } else if max == rgb[1] {
        (rgb[2] - rgb[0]) / d + 2.0
    } else {
        (rgb[0] - rgb[1]) / d + 4.0
    };
    ((h / 6.0 * HUE_BINS as f32) as usize).min(HUE_BINS - 1)
}

/// Raise brightness so the colour reads on a dark UI, but no further than
/// `gain_cap`: past that, brightness is being invented rather than lifted.
fn lift(c: [f32; 3], gain_cap: f32) -> Color32 {
    let max = c[0].max(c[1]).max(c[2]).max(1.0);
    let k = (220.0 / max).clamp(1.0, gain_cap);
    Color32::from_rgb((c[0] * k).min(255.0) as u8, (c[1] * k).min(255.0) as u8, (c[2] * k).min(255.0) as u8)
}

/// The accent as the UI shows it. A colour bright in one channel can still be dark to
/// the eye (pure blue is under 2:1 on the dark UI): it's washed toward white until it
/// reaches [`ACCENT_LUMA`]. Only the UI's copy: the backdrop tint aims at the colour itself.
fn readable(c: Color32) -> Color32 {
    let c = [c.r(), c.g(), c.b()].map(f32::from);
    let l = 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2];
    let t = ((ACCENT_LUMA - l) / (255.0 - l)).clamp(0.0, 1.0);
    let c = c.map(|v| (v + (255.0 - v) * t) as u8);
    Color32::from_rgb(c[0], c[1], c[2])
}

/// A neutral backdrop for when nothing is playing, with the accent and brightness the
/// app starts with (a song without a cover takes them too).
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
    Art { rgba, accent: crate::theme::RED, brightness: 0.0 }
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
    use super::*;

    /// A wide picture becomes its middle square, scaled down to the cover size, as a JPEG.
    #[test]
    fn a_playlist_cover_is_the_pictures_middle_square() {
        let mut img = image::RgbImage::from_pixel(3000, 1500, image::Rgb([0, 0, 255]));
        for x in 750..2250 {
            for y in 0..1500 {
                img.put_pixel(x, y, image::Rgb([255, 0, 0]));
            }
        }
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let jpeg = super::playlist_cover(&png).unwrap();
        assert_eq!(image::guess_format(&jpeg).unwrap(), image::ImageFormat::Jpeg);
        let cover = image::load_from_memory(&jpeg).unwrap().to_rgb8();
        assert_eq!(cover.dimensions(), (1200, 1200));
        // All of it from the red middle, none of the blue sides.
        for p in [cover.get_pixel(2, 2), cover.get_pixel(1197, 600), cover.get_pixel(600, 600)] {
            assert!(p[0] > 200 && p[2] < 60, "{p:?}");
        }
        assert!(super::playlist_cover(b"not a picture").is_err());
    }

    /// A photo the camera marked as turned (EXIF orientation 6, a quarter turn clockwise)
    /// comes out upright: its left half on top.
    #[test]
    fn a_playlist_cover_is_turned_the_way_the_photo_says() {
        use image::ImageEncoder;
        let img = image::RgbImage::from_fn(200, 100, |x, _| if x < 100 { image::Rgb([255, 0, 0]) } else { image::Rgb([0, 0, 255]) });
        // A little-endian TIFF header and one IFD entry: Orientation (0x0112), SHORT, 6.
        let exif = vec![b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0];
        let mut jpeg = Vec::new();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95);
        encoder.set_exif_metadata(exif).unwrap();
        encoder.write_image(&img, 200, 100, image::ExtendedColorType::Rgb8).unwrap();
        let cover = image::load_from_memory(&super::playlist_cover(&jpeg).unwrap()).unwrap().to_rgb8();
        assert_eq!(cover.dimensions(), (100, 100));
        let (top_right, bottom_left) = (cover.get_pixel(80, 20), cover.get_pixel(20, 80));
        assert!(top_right[0] > 200 && top_right[2] < 60, "{top_right:?}");
        assert!(bottom_left[2] > 200 && bottom_left[0] < 60, "{bottom_left:?}");
    }

    /// A see-through picture's clear parts come out black, not the colour stored under them.
    #[test]
    fn a_playlist_covers_see_through_parts_are_black() {
        let img = image::RgbaImage::from_fn(10, 10, |x, _| if x < 5 { image::Rgba([255, 0, 0, 0]) } else { image::Rgba([0, 255, 0, 255]) });
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let cover = image::load_from_memory(&super::playlist_cover(&png).unwrap()).unwrap().to_rgb8();
        let (clear, solid) = (cover.get_pixel(1, 5), cover.get_pixel(8, 5));
        assert!(clear.0.iter().all(|&c| c < 30), "{clear:?}");
        assert!(solid[1] > 200 && solid[0] < 60, "{solid:?}");
    }

    /// A picture far too big to open is turned down before its pixels are made room for:
    /// here a tiny JPEG whose header says 65535×65535 (about 13 GB of pixels).
    #[test]
    fn a_huge_playlist_cover_is_turned_down() {
        let mut jpeg = Vec::new();
        image::RgbImage::new(1, 1).write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg).unwrap();
        let sof = jpeg.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
        jpeg[sof + 5..sof + 9].copy_from_slice(&[0xFF; 4]);
        let err = super::playlist_cover(&jpeg).unwrap_err();
        assert!(format!("{err:#}").to_lowercase().contains("limit"), "{err:#}");
    }

    #[test]
    fn a_cover_frame_gives_a_still_covers_colours() {
        // Flat colour: sampling it small can't tell nearest from Triangle.
        let frame = egui::ColorImage::new([300, 300], vec![egui::Color32::from_rgb(180, 60, 120); 300 * 300]);
        let still = process_rgb(&image::RgbImage::from_pixel(300, 300, image::Rgb([180, 60, 120])));
        let art = from_cover_frame(&frame).unwrap();
        assert_eq!((art.accent, art.rgba), (still.accent, still.rgba));
        assert!(from_cover_frame(&egui::ColorImage::new([0, 0], vec![])).is_none());
    }

    /// A solid `base` square with a `block` block in the middle, the shape of a
    /// sleeve with one designed element on it.
    fn sleeve(base: [u8; 3], block: [u8; 3]) -> image::RgbImage {
        patch(base, block, 12, 20)
    }

    /// The same, with a 3×3 `spot`: under a percent of the cover, the shape of
    /// compression noise rather than of anything designed.
    fn speck(base: [u8; 3], spot: [u8; 3]) -> image::RgbImage {
        patch(base, spot, 15, 18)
    }

    fn patch(base: [u8; 3], fill: [u8; 3], from: u32, to: u32) -> image::RgbImage {
        let mut img = image::RgbImage::new(32, 32);
        for p in img.pixels_mut() {
            *p = image::Rgb(base);
        }
        for x in from..to {
            for y in from..to {
                img.put_pixel(x, y, image::Rgb(fill));
            }
        }
        img
    }

    fn luma(c: Color32) -> f32 {
        0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32
    }

    #[test]
    fn greyscale_cover_gets_a_neutral_accent_and_no_tint() {
        // A black sleeve with white ink — and the warm cast such artwork carries in
        // its dark areas. No hue may be invented for it, however vivid the cast reads.
        let img = sleeve([26, 22, 16], [235, 235, 235]);
        let look = analyse(&img);
        let (r, g, b) = (look.accent.r(), look.accent.g(), look.accent.b());
        assert!((r as i32 - g as i32).abs() < 6 && (g as i32 - b as i32).abs() < 6, "accent {r},{g},{b} is not neutral");
        assert!(luma(look.accent) > 170.0, "a dark cover's accent should invert to near-white");
        assert_eq!(look.colour, 0.0, "a greyscale cover must not tint the backdrop");
    }

    #[test]
    fn pale_greyscale_cover_settles_at_mid_grey() {
        let look = analyse(&sleeve([245, 243, 240], [30, 30, 30]));
        let (r, g, b) = (look.accent.r(), look.accent.g(), look.accent.b());
        assert_eq!(r, g, "accent {r},{g},{b} is not neutral");
        assert_eq!(g, b);
        assert!(luma(look.accent) < 180.0, "a pale cover's accent should stay off pure white");
    }

    #[test]
    fn vivid_cover_keeps_its_hue_at_full_tint() {
        let look = analyse(&sleeve([200, 40, 60], [220, 60, 40]));
        assert!(look.accent.r() > look.accent.g() + 60, "accent lost the red");
        assert!(look.accent.r() > look.accent.b() + 60);
        assert!(look.colour > 0.9, "a vivid cover should tint at full strength");
    }

    #[test]
    fn a_bright_logo_colours_the_accent_on_a_black_sleeve() {
        // Nine-tenths black around a bright cyan mark: the mark is the sleeve's
        // identity, so it picks the accent even though the cover is nearly grey.
        let look = analyse(&sleeve([15, 15, 18], [40, 220, 230]));
        let accent = look.accent;
        assert!(accent.b() > accent.r() + 60, "accent {accent:?} missed the cyan");
        // But the backdrop stays near-black: the sleeve is black, and looks it.
        assert!(look.colour < 0.25, "colour {} tints the backdrop too far", look.colour);
    }

    #[test]
    fn a_small_bright_logo_still_picks_the_accent() {
        // A red mark on 3.5% of a black sleeve: too little colour to tint the
        // backdrop, but enough to be the sleeve's identity — it was dropped as greyscale.
        let look = analyse(&patch([12, 12, 14], [230, 30, 30], 13, 19));
        let a = look.accent;
        assert!(a.r() > a.g() + 100 && a.r() > a.b() + 100, "accent {a:?} lost the red");
        assert_eq!(look.colour, 0.0, "a mostly black sleeve's backdrop stays untinted");
    }

    #[test]
    fn one_stray_pixel_cannot_take_the_accent() {
        // A teal cover with a speck of hot orange: the speck is noise, the teal is
        // the cover — the old single-pixel pick went orange.
        let img = speck([20, 60, 60], [240, 120, 20]);
        let look = analyse(&img);
        let accent = look.accent;
        assert!(accent.g() > accent.r(), "accent {accent:?} followed the stray pixel");
    }

    #[test]
    fn dark_cover_mean_is_lifted_with_a_bound() {
        // A deep red sleeve with no bright area: its accent stays deep red rather
        // than being amplified into a pastel.
        let img = sleeve([60, 10, 14], [60, 10, 14]);
        let look = analyse(&img);
        assert!(look.accent.r() > look.accent.g() + 40 && look.accent.r() > look.accent.b() + 40);
        assert!(luma(look.accent) < 150.0, "lift ran away: luma {}", luma(look.accent));
    }

    #[test]
    fn tint_aims_below_the_ui_accent() {
        // Half bright yellow, half black: the UI accent is the yellow at full brightness,
        // but the black is pulled only toward a dimmed accent, so it stays dark.
        let img = patch([0, 0, 0], [230, 220, 40], 0, 23);
        assert!(luma(analyse(&img).accent) > TINT_LUMA, "needs an accent the tint has to dim");
        let art = process_rgb(&img);
        let corner = &art.rgba[art.rgba.len() - 4..];
        let l = 0.299 * corner[0] as f32 + 0.587 * corner[1] as f32 + 0.114 * corner[2] as f32;
        // 49 with the dimming, 89 without it.
        assert!(l < 70.0, "the black corner glows: luma {l}");
    }

    #[test]
    fn a_faint_cast_keeps_a_readable_accent() {
        // Near-black with a faint blue cast: just past greyscale, so its mean hue is
        // faded out. It fades toward the greyscale accent, not toward its own dim
        // grey, which read at under 2:1 on the dark UI.
        let accent = analyse(&patch([10, 16, 26], [10, 16, 26], 0, 0)).accent;
        assert!(luma(accent) > 150.0, "{accent:?}");
    }

    #[test]
    fn a_wide_picture_is_read_by_its_middle_square() {
        // 16:9, blue in the middle square and red at the sides the sleeve cuts off.
        let img = image::RgbImage::from_fn(160, 90, |x, _| image::Rgb(if (35..125).contains(&x) { [60, 80, 200] } else { [240, 20, 20] }));
        let a = process_rgb(&img).accent;
        assert!(a.b() > a.r() + 60, "accent {a:?} read the cut-off sides");
        for (w, h) in [(1, 1), (1, 300), (300, 1), (5, 4), (2, 1)] {
            process_rgb(&image::RgbImage::new(w, h));
        }
    }

    #[test]
    fn two_colours_give_one_of_them_not_their_mix() {
        // About half blue, half yellow: averaged together they made a grey that
        // isn't on the sleeve. The accent is one of the two.
        let a = analyse(&patch([40, 60, 230], [230, 210, 30], 0, 23)).accent;
        // The yellow, which scores higher.
        assert!(a.r() > a.b() + 100 && a.g() > a.b() + 100, "accent {a:?} is a mix, or not the yellow");
    }

    #[test]
    fn a_hue_on_a_bin_edge_isnt_split() {
        // Two reds either side of 0°, together more than the green: split into
        // their own bins, each lost to it.
        let img = image::RgbImage::from_fn(32, 32, |_, y| image::Rgb(match y {
            0..10 => [230, 0, 5],
            10..20 => [230, 5, 0],
            _ => [0, 200, 0],
        }));
        let a = analyse(&img).accent;
        assert!(a.r() > a.g() + 100, "accent {a:?} lost the red");
    }

    #[test]
    fn other_hues_dont_carry_a_speck_past_the_share() {
        // A grey sleeve with 16 red pixels and 52 muted cyan ones: neither covers
        // a real share alone, so the red speck must not take the accent.
        let img = image::RgbImage::from_fn(64, 64, |x, y| image::Rgb(match (x, y) {
            (0..4, 0..4) => [230, 30, 30],
            (10..23, 10..14) => [110, 180, 180],
            _ => [120, 120, 120],
        }));
        let a = analyse(&img).accent;
        let (max, min) = (a.r().max(a.g()).max(a.b()), a.r().min(a.g()).min(a.b()));
        assert!(max - min < 30, "accent {a:?} followed the speck or the cyan");
    }

    #[test]
    fn a_blue_logo_gives_an_accent_that_reads_on_the_dark_ui() {
        let a = process_rgb(&patch([12, 12, 14], [0, 0, 230], 13, 19)).accent;
        assert!(a.b() > a.r() + 100, "accent {a:?} lost the blue");
        assert!(luma(a) >= ACCENT_LUMA - 1.0, "accent {a:?} is too dark for the dark UI");
    }

    #[test]
    fn the_backdrop_tint_isnt_washed_for_the_ui() {
        // A navy sleeve: its UI accent is washed lighter to read, but the backdrop
        // is tinted toward the navy itself, not toward that grey-blue.
        let art = process_rgb(&patch([0, 0, 90], [0, 0, 90], 0, 0));
        let [r, g, b, _] = [art.rgba[0], art.rgba[1], art.rgba[2], art.rgba[3]];
        assert!(r < 10 && g < 10 && b > 100, "backdrop {r},{g},{b} took the washed accent");
    }

    #[test]
    fn a_gradient_logo_is_one_hue() {
        // A small orange-to-yellow logo on grey, across three hue bins: together a
        // real share of the sleeve, so it takes the accent rather than grey.
        let img = image::RgbImage::from_fn(64, 64, |x, y| image::Rgb(match (x, y) {
            (0..20, 0) => [230, 110, 0],
            (0..26, 1) => [220, 155, 40],
            (0..26, 2) => [220, 225, 70],
            _ => [120, 120, 120],
        }));
        let a = analyse(&img).accent;
        assert!(a.r() > a.b() + 100, "accent {a:?} dropped the logo");
    }

    #[test]
    fn two_hues_apart_dont_make_one_between_them() {
        // Red and a yellow-green two bins apart: a window centred on the empty bin
        // between them made an orange the sleeve doesn't have.
        // A row of orange between them, as an edge in a real picture would leave.
        let img = image::RgbImage::from_fn(64, 64, |_, y| image::Rgb(match y {
            0..33 => [230, 0, 0],
            33 => [230, 170, 0],
            _ => [120, 230, 0],
        }));
        let a = analyse(&img).accent;
        assert!(a.r() > a.g() + 100, "accent {a:?} is a hue between the two");
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
