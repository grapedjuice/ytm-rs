//! Visual design tokens: fonts, icons, colours, motion.

use std::sync::Arc;

use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId};

// ------------------------------------------------------------------ colours

/// Base surface under the animated background (seen while it fades in).
pub const BG: Color32 = Color32::from_rgb(12, 10, 14);
pub const TEXT: Color32 = Color32::from_rgb(242, 240, 245);
pub const TEXT_DIM: Color32 = Color32::from_rgb(178, 172, 186);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(130, 124, 140);
/// Default accent until a cover provides one (YouTube Music red, slightly warm).
pub const RED: Color32 = Color32::from_rgb(255, 48, 72);

/// White at `a` opacity, for glassy surfaces over the background.
pub fn glass(a: f32) -> Color32 {
    Color32::from_white_alpha((a * 255.0) as u8)
}

pub fn shade(a: f32) -> Color32 {
    Color32::from_black_alpha((a * 255.0) as u8)
}

/// Scale a colour's opacity by `a` (works for opaque and already-translucent colours).
pub fn with_alpha(c: Color32, a: f32) -> Color32 {
    c.gamma_multiply(a.clamp(0.0, 1.0))
}

/// Interpolate in premultiplied space (Color32's native form), so translucent glass
/// colours blend correctly with opaque ones.
pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

// ------------------------------------------------------------------ motion

pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

pub fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 }
}

/// Animate toward `target` (eased), keyed by `id`. Requests repaints while moving.
pub fn anim(ctx: &egui::Context, id: impl std::hash::Hash + std::fmt::Debug, target: f32, secs: f32) -> f32 {
    ctx.animate_value_with_time(egui::Id::new(id), target, secs)
}

pub fn anim_bool(ctx: &egui::Context, id: impl std::hash::Hash + std::fmt::Debug, on: bool, secs: f32) -> f32 {
    ease_out(ctx.animate_bool_with_time(egui::Id::new(id), on, secs))
}

// ------------------------------------------------------------------ type

pub fn regular(size: f32) -> FontId {
    FontId::proportional(size)
}
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}
pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}
pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("icons".into()))
}

/// Lucide icons (ISC), subset to the glyphs used here.
#[allow(dead_code)]
pub mod icon {
    pub const HOUSE: char = '\u{e0f5}';
    pub const COMPASS: char = '\u{e09b}';
    pub const LIBRARY: char = '\u{e100}';
    pub const SEARCH: char = '\u{e151}';
    pub const CHEVRON_LEFT: char = '\u{e06e}';
    pub const CHEVRON_RIGHT: char = '\u{e06f}';
    pub const CHEVRON_UP: char = '\u{e070}';
    pub const CHEVRON_DOWN: char = '\u{e06d}';
    pub const PLAY: char = '\u{e13c}';
    pub const PAUSE: char = '\u{e12e}';
    pub const SKIP_BACK: char = '\u{e15f}';
    pub const SKIP_FORWARD: char = '\u{e160}';
    pub const SHUFFLE: char = '\u{e15e}';
    pub const REPEAT: char = '\u{e146}';
    pub const REPEAT_1: char = '\u{e1fd}';
    pub const VOLUME_2: char = '\u{e1ab}';
    pub const VOLUME_1: char = '\u{e1aa}';
    pub const VOLUME_X: char = '\u{e1ac}';
    pub const HEART: char = '\u{e0f2}';
    pub const THUMBS_UP: char = '\u{e18a}';
    pub const THUMBS_DOWN: char = '\u{e189}';
    pub const LIST_MUSIC: char = '\u{e2e0}';
    pub const MIC_VOCAL: char = '\u{e349}';
    pub const MAXIMIZE_2: char = '\u{e113}';
    pub const MINIMIZE_2: char = '\u{e11b}';
    pub const X: char = '\u{e1b2}';
    pub const ELLIPSIS_VERTICAL: char = '\u{e0b7}';
    pub const PLUS: char = '\u{e13d}';
    pub const LIST_PLUS: char = '\u{e23f}';
    pub const RADIO: char = '\u{e142}';
    pub const DISC_3: char = '\u{e494}';
    pub const USER: char = '\u{e19f}';
    pub const SETTINGS: char = '\u{e154}';
    pub const LOG_OUT: char = '\u{e10e}';
    pub const MUSIC: char = '\u{e122}';
    pub const LOADER_CIRCLE: char = '\u{e10a}';
    pub const TRASH_2: char = '\u{e18e}';
    pub const HISTORY: char = '\u{e1f5}';
    pub const SPARKLES: char = '\u{e412}';
    pub const CHECK: char = '\u{e06c}';
    pub const LIST_VIDEO: char = '\u{e2e2}';
    pub const CIRCLE_USER: char = '\u{e461}';
    pub const EXPAND: char = '\u{e21a}';
    pub const SHRINK: char = '\u{e220}';
}

const INTER_REGULAR: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Inter-SemiBold.ttf");
const INTER_BOLD: &[u8] = include_bytes!("../assets/fonts/Inter-ExtraBold.ttf");
const LUCIDE: &[u8] = include_bytes!("../assets/fonts/lucide.ttf");

// System fallbacks: egui's fonts cover Latin only, so CJK, Arabic, Indic, Thai,
// symbols and emoji in titles would render as boxes. Memory-mapped, not read, so a
// 20 MB CJK collection costs only the glyph pages actually drawn.
#[cfg(windows)]
const CANDIDATES: &[(&str, u32)] = &[
    ("C:\\Windows\\Fonts\\segoeui.ttf", 0),   // Latin ext., Greek, Cyrillic, Arabic, Hebrew
    ("C:\\Windows\\Fonts\\seguisym.ttf", 0),  // symbols, arrows, dingbats
    ("C:\\Windows\\Fonts\\YuGothM.ttc", 0),   // Japanese
    ("C:\\Windows\\Fonts\\msyh.ttc", 0),      // Simplified Chinese
    ("C:\\Windows\\Fonts\\msjh.ttc", 0),      // Traditional Chinese
    ("C:\\Windows\\Fonts\\malgun.ttf", 0),    // Korean
    ("C:\\Windows\\Fonts\\Nirmala.ttc", 0),   // Indic scripts
    ("C:\\Windows\\Fonts\\Nirmala.ttf", 0),
    ("C:\\Windows\\Fonts\\LeelawUI.ttf", 0),  // Thai, Lao, Khmer
    ("C:\\Windows\\Fonts\\ebrima.ttf", 0),    // Ethiopic, N'Ko, etc.
    ("C:\\Windows\\Fonts\\seguiemj.ttf", 0),  // emoji (drawn monochrome)
];

#[cfg(target_os = "macos")]
const CANDIDATES: &[(&str, u32)] = &[
    ("/System/Library/Fonts/Supplemental/Arial Unicode.ttf", 0),
    ("/System/Library/Fonts/Hiragino Sans GB.ttc", 0),
    ("/System/Library/Fonts/AppleSDGothicNeo.ttc", 0),
    ("/System/Library/Fonts/Apple Symbols.ttf", 0),
];

#[cfg(all(unix, not(target_os = "macos")))]
const CANDIDATES: &[(&str, u32)] = &[
    ("/usr/share/fonts/noto/NotoSans-Regular.ttf", 0),
    ("/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf", 0),
    ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 0),
    ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 0),
    ("/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc", 0),
    ("/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf", 0),
    ("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 0),
];

pub fn install_fonts(ctx: &egui::Context) {
    let mut defs = FontDefinitions::default();
    let mut add = |name: &str, data: FontData| {
        defs.font_data.insert(name.to_owned(), Arc::new(data));
    };
    add("inter", FontData::from_static(INTER_REGULAR));
    add("inter-semibold", FontData::from_static(INTER_SEMIBOLD));
    add("inter-bold", FontData::from_static(INTER_BOLD));
    add("lucide", FontData::from_static(LUCIDE).tweak(egui::FontTweak { y_offset_factor: 0.0, ..Default::default() }));

    let mut fallbacks = Vec::new();
    for &(path, index) in CANDIDATES {
        if let Some(bytes) = map(path) {
            let name = format!("sys:{path}");
            add(&name, FontData { index, ..FontData::from_static(bytes) });
            fallbacks.push(name);
        }
    }
    // egui's bundled emoji/icon font stays as a last resort.
    let egui_default = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let chain = |primary: &str| {
        let mut v = vec![primary.to_owned()];
        v.extend(fallbacks.iter().cloned());
        v.extend(egui_default.iter().filter(|n| n.as_str() != "Ubuntu-Light").cloned());
        v
    };
    defs.families.insert(FontFamily::Proportional, chain("inter"));
    defs.families.insert(FontFamily::Name("semibold".into()), chain("inter-semibold"));
    defs.families.insert(FontFamily::Name("bold".into()), chain("inter-bold"));
    defs.families.insert(FontFamily::Name("icons".into()), vec!["lucide".to_owned()]);
    if let Some(mono) = defs.families.get_mut(&FontFamily::Monospace) {
        mono.extend(fallbacks.iter().cloned());
    }
    ctx.set_fonts(defs);
}

fn map(path: &str) -> Option<&'static [u8]> {
    let file = std::fs::File::open(path).ok()?;
    // SAFETY: system font files aren't modified while in use; the mapping is
    // read-only and intentionally lives for the rest of the process.
    let mmap = unsafe { memmap2::Mmap::map(&file) }.ok()?;
    Some(Box::leak(Box::new(mmap)))
}

pub fn install_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(14.0, 7.0);
        s.spacing.scroll = egui::style::ScrollStyle::floating();
        s.text_styles.insert(egui::TextStyle::Body, regular(14.0));
        s.text_styles.insert(egui::TextStyle::Button, semibold(14.0));
        s.text_styles.insert(egui::TextStyle::Heading, bold(22.0));
        s.text_styles.insert(egui::TextStyle::Small, regular(12.0));
        let v = &mut s.visuals;
        v.panel_fill = BG;
        v.window_fill = Color32::from_rgb(28, 24, 32);
        v.extreme_bg_color = glass(0.08);
        v.override_text_color = Some(TEXT);
        v.selection.bg_fill = with_alpha(RED, 0.5);
        v.hyperlink_color = TEXT;
        v.window_corner_radius = egui::CornerRadius::same(14);
        v.menu_corner_radius = egui::CornerRadius::same(12);
        v.window_stroke = egui::Stroke::new(1.0, glass(0.08));
        v.popup_shadow = egui::epaint::Shadow { offset: [0, 8], blur: 28, spread: 0, color: shade(0.5) };
        for (w, fill) in [
            (&mut v.widgets.inactive, glass(0.08)),
            (&mut v.widgets.hovered, glass(0.14)),
            (&mut v.widgets.active, glass(0.2)),
            (&mut v.widgets.open, glass(0.14)),
        ] {
            w.bg_fill = fill;
            w.weak_bg_fill = fill;
            w.bg_stroke = egui::Stroke::NONE;
            w.corner_radius = egui::CornerRadius::same(18);
            w.fg_stroke.color = TEXT;
        }
        v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, glass(0.08));
        s.interaction.selectable_labels = false;
        s.animation_time = 0.18;
    });
}
