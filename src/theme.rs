//! Visual design tokens: fonts, icons, colours, motion.

use std::sync::Arc;

use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId};

// ------------------------------------------------------------------ colours

/// Base surface under the animated background (seen while it fades in).
const BG: Color32 = Color32::from_rgb(12, 10, 14);
const TEXT: Color32 = Color32::from_rgb(242, 240, 245);
const TEXT_DIM: Color32 = Color32::from_rgb(178, 172, 186);
const TEXT_FAINT: Color32 = Color32::from_rgb(130, 124, 140);
/// Light mode's counterparts.
const BG_LIGHT: Color32 = Color32::from_rgb(246, 244, 248);
const TEXT_LIGHT: Color32 = Color32::from_rgb(22, 20, 26);
const TEXT_DIM_LIGHT: Color32 = Color32::from_rgb(86, 82, 94);
const TEXT_FAINT_LIGHT: Color32 = Color32::from_rgb(124, 118, 132);
/// Default accent until a cover provides one (YouTube Music red, slightly warm).
pub const RED: Color32 = Color32::from_rgb(255, 48, 72);

/// Light or dark, as the app picked it for what's being drawn (see `set_mode`): the
/// colours below follow it. Now playing always draws dark, over the album art.
static LIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn light() -> bool {
    LIGHT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Draw what follows light or dark: the colours here and egui's style (menus, fields,
/// text) change together. Returns the mode before, to go back to.
pub fn set_mode(ctx: &egui::Context, light: bool) -> bool {
    let was = LIGHT.swap(light, std::sync::atomic::Ordering::Relaxed);
    if was != light {
        ctx.global_style_mut(|s| s.visuals = visuals());
    }
    was
}

/// Main text.
pub fn text() -> Color32 {
    if light() { TEXT_LIGHT } else { TEXT }
}

/// Text and icons on a fill of the main text colour (a primary button, a selected chip).
pub fn on_text() -> Color32 {
    if light() { Color32::WHITE } else { Color32::from_gray(15) }
}

/// Least important text: counts, hints, placeholders.
pub fn text_faint() -> Color32 {
    if light() { TEXT_FAINT_LIGHT } else { TEXT_FAINT }
}

/// The window's base colour, under the background.
pub fn bg() -> Color32 {
    if light() { BG_LIGHT } else { BG }
}

/// Menus and popups.
pub fn menu_fill() -> Color32 {
    if light() { Color32::WHITE } else { Color32::from_rgb(38, 34, 42) }
}

/// Dialogs and cards that float over the page.
pub fn surface_fill() -> Color32 {
    if light() { Color32::from_rgb(252, 251, 253) } else { Color32::from_rgb(28, 24, 32) }
}

/// Theme preset multiplier for every glass surface (f32 bits; 1.0 by default).
static GLASS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x3f80_0000);

/// White at `a` opacity, for glassy surfaces over the background (black in light mode).
pub fn glass(a: f32) -> Color32 {
    let k = f32::from_bits(GLASS.load(std::sync::atomic::Ordering::Relaxed));
    let a = ((a * k).min(1.0) * 255.0) as u8;
    if light() { Color32::from_black_alpha(a) } else { Color32::from_white_alpha(a) }
}

/// A panel over the background (sidebar, player bar, queue): darker than the page at
/// `a`, or in light mode lighter.
pub fn panel(a: f32) -> Color32 {
    if light() { Color32::from_white_alpha((a.clamp(0.0, 1.0) * 255.0) as u8) } else { shade(a) }
}

/// The active theme's secondary text colour, set with its chrome (read very often).
static DIM: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(u32::from_le_bytes(TEXT_DIM.to_array()));

/// Secondary text colour (themes may tint it).
pub fn text_dim() -> Color32 {
    if light() {
        return TEXT_DIM_LIGHT;
    }
    let [r, g, b, a] = DIM.load(std::sync::atomic::Ordering::Relaxed).to_le_bytes();
    Color32::from_rgba_premultiplied(r, g, b, a)
}

/// Light, dark, or whichever the system uses (Settings › Appearance › Mode).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn from_key(s: &str) -> Self {
        Self::ALL.into_iter().find(|a| a.key() == s).unwrap_or_default()
    }

    /// Whether this means light, given the system's mode as eframe reports it.
    pub fn is_light(self, ctx: &egui::Context) -> bool {
        match self {
            Self::Light => true,
            Self::Dark => false,
            Self::System => system_light().unwrap_or_else(|| ctx.system_theme() == Some(egui::Theme::Light)),
        }
    }
}

/// Windows' "app mode" (Settings › Personalization › Colors), read again every couple
/// of seconds so switching it there shows without a restart. `None` off Windows.
fn system_light() -> Option<bool> {
    use std::time::{Duration, Instant};
    static MODE: std::sync::Mutex<Option<(Instant, Option<bool>)>> = std::sync::Mutex::new(None);
    let mut cached = MODE.lock().unwrap();
    match *cached {
        Some((at, m)) if at.elapsed() < Duration::from_secs(2) => m,
        _ => {
            let m = read_system_light();
            *cached = Some((Instant::now(), m));
            m
        }
    }
}

#[cfg(windows)]
fn read_system_light() -> Option<bool> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::core::w;
    let mut v = 0u32;
    let mut size = 4u32;
    // SAFETY: a DWORD out-buffer of the size passed; the strings are static.
    unsafe {
        RegGetValueW(HKEY_CURRENT_USER, w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"), w!("AppsUseLightTheme"), RRF_RT_REG_DWORD, None, Some(&mut v as *mut u32 as *mut _), Some(&mut size))
            .is_ok()
            .then_some(v != 0)
    }
}

#[cfg(not(windows))]
fn read_system_light() -> Option<bool> {
    None
}

/// The Windows accent colour, in the lighter shade Windows itself uses on dark
/// surfaces (`AccentPalette`'s second entry), or the base accent if there's no palette.
/// Read again every couple of seconds, so a new colour picked in Windows' settings
/// shows without a restart. `None` off Windows or when the setting can't be read.
pub fn system_accent() -> Option<Color32> {
    use std::time::{Duration, Instant};
    static ACCENT: std::sync::Mutex<Option<(Instant, Option<Color32>)>> = std::sync::Mutex::new(None);
    let mut cached = ACCENT.lock().unwrap();
    match *cached {
        Some((at, c)) if at.elapsed() < Duration::from_secs(2) => c,
        _ => {
            let c = read_system_accent();
            *cached = Some((Instant::now(), c));
            c
        }
    }
}

#[cfg(windows)]
fn read_system_accent() -> Option<Color32> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::core::w;
    // SAFETY: out-buffers sized to what we pass; the strings are static.
    unsafe {
        // Eight RGBA swatches, lightest first; the second is "light 2".
        let mut palette = [0u8; 32];
        let mut size = palette.len() as u32;
        let key = w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent");
        if RegGetValueW(HKEY_CURRENT_USER, key, w!("AccentPalette"), RRF_RT_REG_BINARY, None, Some(palette.as_mut_ptr() as *mut _), Some(&mut size)).is_ok() && size >= 8 {
            return Some(Color32::from_rgb(palette[4], palette[5], palette[6]));
        }
        // 0xAABBGGRR.
        let mut abgr = 0u32;
        let mut size = 4u32;
        RegGetValueW(HKEY_CURRENT_USER, w!(r"Software\Microsoft\Windows\DWM"), w!("AccentColor"), RRF_RT_REG_DWORD, None, Some(&mut abgr as *mut u32 as *mut _), Some(&mut size))
            .is_ok()
            .then(|| Color32::from_rgb(abgr as u8, (abgr >> 8) as u8, (abgr >> 16) as u8))
    }
}

#[cfg(not(windows))]
fn read_system_accent() -> Option<Color32> {
    None
}

/// Corner radius of album/playlist art in cards.
pub fn card_radius() -> f32 {
    crate::preset::active().app.card_radius.clamp(0.0, 40.0)
}

/// Corner radius of small song-row thumbnails.
pub fn thumb_radius() -> f32 {
    crate::preset::active().app.thumb_radius.clamp(0.0, 20.0)
}

/// Corner radius of panels (settings sections, menus, dialogs).
pub fn panel_radius() -> u8 {
    crate::preset::active().app.panel_radius.clamp(0.0, 32.0) as u8
}

/// Strength of card hover zoom/lift.
pub fn hover_k() -> f32 {
    crate::preset::active().app.hover.clamp(0.0, 2.5)
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

/// Every UI transition runs at this fraction of the time it's written with: one knob
/// for how snappy the app feels. (Lyrics keep their own timing.)
pub const SPEED: f32 = 0.65;

/// Animate toward `target` (eased), keyed by `id`. Requests repaints while moving.
pub fn anim(ctx: &egui::Context, id: impl std::hash::Hash + std::fmt::Debug, target: f32, secs: f32) -> f32 {
    ctx.animate_value_with_time(egui::Id::unique(id), target, secs * SPEED)
}

pub fn anim_bool(ctx: &egui::Context, id: impl std::hash::Hash + std::fmt::Debug, on: bool, secs: f32) -> f32 {
    ease_out(ctx.animate_bool_with_time(egui::Id::unique(id), on, secs * SPEED))
}

// ------------------------------------------------------------------ type

pub fn regular(size: f32) -> FontId {
    FontId::proportional(size)
}
// Named families, made once: these are called hundreds of times a frame, and a
// clone is a reference count instead of a new string.
static SEMIBOLD: std::sync::LazyLock<FontFamily> = std::sync::LazyLock::new(|| FontFamily::Name("semibold".into()));
static BOLD: std::sync::LazyLock<FontFamily> = std::sync::LazyLock::new(|| FontFamily::Name("bold".into()));
static ICONS: std::sync::LazyLock<FontFamily> = std::sync::LazyLock::new(|| FontFamily::Name("icons".into()));

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, SEMIBOLD.clone())
}
pub fn bold(size: f32) -> FontId {
    FontId::new(size, BOLD.clone())
}
pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, ICONS.clone())
}

/// Lucide icons (ISC), subset to the glyphs used here.
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
    pub const THUMBS_DOWN: char = '\u{e189}';
    pub const LIST_MUSIC: char = '\u{e2e0}';
    pub const MIC_VOCAL: char = '\u{e349}';
    pub const X: char = '\u{e1b2}';
    pub const HISTORY: char = '\u{e1f5}';
    pub const TRASH_2: char = '\u{e18e}';
    pub const ELLIPSIS_VERTICAL: char = '\u{e0b7}';
    pub const LIST_PLUS: char = '\u{e23f}';
    pub const LIST_MINUS: char = '\u{e23e}';
    pub const LIST_CHECKS: char = '\u{e1d0}';
    pub const RADIO: char = '\u{e142}';
    pub const DISC_3: char = '\u{e494}';
    pub const USER: char = '\u{e19f}';
    pub const SETTINGS: char = '\u{e154}';
    pub const LOG_OUT: char = '\u{e10e}';
    pub const MUSIC: char = '\u{e122}';
    pub const SPARKLES: char = '\u{e412}';
    pub const CHECK: char = '\u{e06c}';
    pub const CIRCLE_USER: char = '\u{e461}';
    pub const EXPAND: char = '\u{e21a}';
    pub const SHRINK: char = '\u{e220}';
    pub const UPLOAD: char = '\u{e19e}';
    pub const USERS: char = '\u{e1a4}';
    pub const COPY: char = '\u{e09e}';
    pub const LINK: char = '\u{e102}';
    pub const CROWN: char = '\u{e1d6}';
    pub const HEADPHONES: char = '\u{e0f1}';
    pub const DOWNLOAD: char = '\u{e0b2}';
    pub const FOLDER_OPEN: char = '\u{e247}';
    pub const PIN: char = '\u{e259}';
    pub const PIN_OFF: char = '\u{e2b6}';
    pub const PICTURE_IN_PICTURE: char = '\u{e3af}';
    pub const MAXIMIZE_2: char = '\u{e113}';
}

/// Inter (SIL OFL 1.1), the default face: regular / semibold / extra bold, subset to
/// Latin (with Vietnamese), Greek and Cyrillic, so those scripts never fall back to
/// another font letter by letter.
const INTER: [&[u8]; 3] = [
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../assets/fonts/Inter-ExtraBold.ttf"),
];
const LUCIDE: &[u8] = include_bytes!("../assets/fonts/lucide.ttf");

/// Lyric typefaces used by theme presets (all SIL OFL 1.1), as static
/// regular / semibold / bold instances. They cover Latin only (their upstream has no
/// Vietnamese, Greek or Cyrillic); `lyric_font_for` sets such lines in Inter instead.
/// Instrument Sans is only ever used at semibold (one preset), so its bold slot reuses
/// that instead of shipping a weight nothing draws.
const LYRIC_FONTS: &[(&str, [&[u8]; 3])] = &[
    ("dmsans", [
        include_bytes!("../assets/fonts/DMSans-Regular.ttf"),
        include_bytes!("../assets/fonts/DMSans-SemiBold.ttf"),
        include_bytes!("../assets/fonts/DMSans-Bold.ttf"),
    ]),
    ("googlesansflex", [
        include_bytes!("../assets/fonts/GoogleSansFlex-Regular.ttf"),
        include_bytes!("../assets/fonts/GoogleSansFlex-SemiBold.ttf"),
        include_bytes!("../assets/fonts/GoogleSansFlex-Bold.ttf"),
    ]),
    ("figtree", [
        include_bytes!("../assets/fonts/Figtree-Regular.ttf"),
        include_bytes!("../assets/fonts/Figtree-SemiBold.ttf"),
        include_bytes!("../assets/fonts/Figtree-ExtraBold.ttf"),
    ]),
    ("instrumentsans", [
        include_bytes!("../assets/fonts/InstrumentSans-Regular.ttf"),
        include_bytes!("../assets/fonts/InstrumentSans-SemiBold.ttf"),
        include_bytes!("../assets/fonts/InstrumentSans-SemiBold.ttf"),
    ]),
];

/// The bundled faces, by the names the font setting shows and stores.
pub const BUNDLED_FONTS: &[&str] = &["Inter", "DM Sans", "Figtree", "Google Sans Flex", "Instrument Sans"];

/// The bundled font a preset font name refers to. Fonts that can't be bundled (SF Pro,
/// Spotify's) map to the closest open one; unknown names to Inter.
fn lyric_key(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_lowercase().replace(' ', "").as_str() {
        "dmsans" => "dmsans",
        "googlesansflex" | "googlesans" => "googlesansflex",
        "figtree" | "spotifymixuititle" | "spotifymix" | "circular" => "figtree",
        "switzer" | "instrumentsans" => "instrumentsans",
        _ => return None,
    })
}

// ------------------------------------------------------------------ font setting

/// The listener's font (Settings › Appearance), used instead of the theme's for the
/// interface and the lyrics. Empty: the theme's own.
static FONT_CHOICE: std::sync::RwLock<String> = std::sync::RwLock::new(String::new());

/// An installed font the listener (or the theme) picked, once its faces are loaded. Drawn through the
/// "choice-<weight>" families, which always exist (Inter until something is loaded), so
/// a frame laid out before egui swaps fonts in never names a missing family.
static CHOICE: std::sync::RwLock<Option<Chosen>> = std::sync::RwLock::new(None);

struct Chosen {
    name: String,
    faces: [Face; 3],
}

/// One face of a primary font: the bytes, the face in the file, and the weight to set
/// on a variable font.
#[derive(Clone, Copy)]
struct Face {
    bytes: &'static [u8],
    index: u32,
    wght: Option<f32>,
}

impl Face {
    const fn of(bytes: &'static [u8]) -> Self {
        Self { bytes, index: 0, wght: None }
    }
}

pub fn set_font_choice(name: &str) {
    *FONT_CHOICE.write().unwrap() = name.to_owned();
}

pub fn font_choice() -> String {
    FONT_CHOICE.read().unwrap().clone()
}

/// The font name to draw with where a theme asks for `theme_font`.
pub fn effective_font(theme_font: &str) -> String {
    let choice = FONT_CHOICE.read().unwrap();
    if choice.is_empty() { theme_font.to_owned() } else { choice.clone() }
}

/// The family key for a font name: a bundled face, "choice" for the installed font the
/// listener picked (once loaded), else Inter.
fn font_key(name: &str) -> &'static str {
    let name = effective_font(name);
    if let Some(k) = lyric_key(&name) {
        return k;
    }
    if CHOICE.read().unwrap().as_ref().is_some_and(|c| c.name.eq_ignore_ascii_case(&name)) { "choice" } else { "inter" }
}

fn family(key: &str, weight: crate::preset::Weight, size: f32) -> FontId {
    use crate::preset::Weight;
    let w = match weight {
        Weight::Regular => 0,
        Weight::Semibold => 1,
        Weight::Bold => 2,
    };
    FontId::new(size, FontFamily::Name(format!("{key}-{w}").into()))
}

/// Lyric font for a preset's font name and weight.
pub fn lyric_font(name: &str, weight: crate::preset::Weight, size: f32) -> FontId {
    family(font_key(name), weight, size)
}

/// Lyric font for one piece of text. When the font lacks letters the text uses and
/// Inter has them (Vietnamese, Greek or Cyrillic in DM Sans, Figtree, …), the whole
/// text is set in Inter rather than mixing two typefaces letter by letter.
pub fn lyric_font_for(name: &str, weight: crate::preset::Weight, size: f32, text: &str) -> FontId {
    let key = font_key(name);
    let key = if key != "inter" && lacks_letters(key, text) { "inter" } else { key };
    family(key, weight, size)
}

fn lacks_letters(key: &str, text: &str) -> bool {
    use skrifa::MetadataProvider;
    let face = match key {
        "choice" => match CHOICE.read().unwrap().as_ref() {
            Some(c) => c.faces[0],
            None => return false,
        },
        k => match LYRIC_FONTS.iter().find(|(n, _)| *n == k) {
            Some((_, w)) => Face::of(w[0]),
            None => return false,
        },
    };
    let (Ok(font), Ok(inter)) = (skrifa::FontRef::from_index(face.bytes, face.index), skrifa::FontRef::new(INTER[0])) else { return false };
    let (has, inter) = (font.charmap(), inter.charmap());
    text.chars().any(|c| c.is_alphabetic() && has.map(c).is_none() && inter.map(c).is_some())
}

// ------------------------------------------------------------------ installed fonts

/// A font family installed on this PC, for the font setting.
pub struct SysFamily {
    pub name: String,
    faces: Vec<SysFace>,
}

struct SysFace {
    path: std::path::PathBuf,
    index: u32,
    weight: f32,
    /// The weight axis range, for a variable font.
    wght: Option<(f32, f32)>,
}

static INSTALLED_FONTS: std::sync::OnceLock<Vec<SysFamily>> = std::sync::OnceLock::new();
static SCANNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Text fonts installed on this PC, sorted by name, without the bundled ones. `None`
/// while a background scan (started by the first call) runs; `done` runs after it.
pub fn installed_fonts(done: impl FnOnce() + Send + 'static) -> Option<&'static [SysFamily]> {
    if let Some(v) = INSTALLED_FONTS.get() {
        return Some(v);
    }
    if !SCANNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        std::thread::spawn(move || {
            let _ = INSTALLED_FONTS.set(scan_fonts());
            done();
        });
    }
    None
}

fn font_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    #[cfg(windows)]
    {
        let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
        dirs.push(std::path::PathBuf::from(windir).join("Fonts"));
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(std::path::PathBuf::from(local).join("Microsoft\\Windows\\Fonts"));
        }
    }
    #[cfg(target_os = "macos")]
    dirs.extend(["/System/Library/Fonts", "/Library/Fonts"].map(Into::into));
    #[cfg(all(unix, not(target_os = "macos")))]
    dirs.extend(["/usr/share/fonts", "/usr/local/share/fonts"].map(Into::into));
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(std::path::PathBuf::from(home).join(".local/share/fonts"));
    }
    dirs
}

/// Every upright, normal-width face of a text font (one that maps Latin letters), by
/// family. Only names and attributes are read; nothing stays mapped.
fn scan_fonts() -> Vec<SysFamily> {
    use skrifa::{MetadataProvider, attribute::Style, string::StringId};
    let mut files = Vec::new();
    let mut dirs = font_dirs();
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().and_then(|x| x.to_str()).is_some_and(|x| matches!(x.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc" | "otc")) {
                files.push(path);
            }
        }
    }
    let mut families: std::collections::BTreeMap<String, SysFamily> = std::collections::BTreeMap::new();
    for path in files {
        let Ok(file) = std::fs::File::open(&path) else { continue };
        // SAFETY: read-only and dropped before the next file; font files don't change
        // under a running app.
        let Ok(data) = (unsafe { memmap2::Mmap::map(&file) }) else { continue };
        let Ok(fonts) = skrifa::raw::FileRef::new(&data) else { continue };
        for (index, font) in fonts.fonts().enumerate() {
            let Ok(font) = font else { continue };
            let cmap = font.charmap();
            if cmap.is_symbol() || cmap.map('a').is_none() || cmap.map('Z').is_none() {
                continue;
            }
            let attrs = font.attributes();
            if attrs.style != Style::Normal || (attrs.stretch.ratio() - 1.0).abs() > 0.01 {
                continue;
            }
            let name = [StringId::TYPOGRAPHIC_FAMILY_NAME, StringId::FAMILY_NAME]
                .into_iter()
                .find_map(|id| font.localized_strings(id).english_or_first())
                .map(|s| s.to_string().trim().to_owned());
            // "@" names are vertical CJK variants.
            let Some(name) = name.filter(|n| !n.is_empty() && !n.starts_with('@') && lyric_key(n).is_none() && !n.eq_ignore_ascii_case("inter")) else { continue };
            let wght = font.axes().iter().find(|a| a.tag() == skrifa::Tag::new(b"wght")).map(|a| (a.min_value(), a.max_value()));
            let face = SysFace { path: path.clone(), index: index as u32, weight: attrs.weight.value(), wght };
            families.entry(name.to_lowercase()).or_insert_with(|| SysFamily { name, faces: Vec::new() }).faces.push(face);
        }
    }
    families.into_values().collect()
}

/// The closest face to each of the three weights the app draws: regular, semibold, and
/// bold, leaning to 700 over 900 where a family has both.
fn choose_faces(family: &SysFamily) -> Option<[Face; 3]> {
    let mut faces = Vec::new();
    for target in [400.0, 600.0, 750.0] {
        let best = family.faces.iter().min_by_key(|f| match f.wght {
            Some((lo, hi)) if (lo..=hi).contains(&target) => 0,
            Some((lo, hi)) => (target - target.clamp(lo, hi)).abs() as i32,
            None => (f.weight - target).abs() as i32,
        })?;
        let bytes = map(&best.path)?;
        faces.push(Face { bytes, index: best.index, wght: best.wght.map(|(lo, hi)| target.clamp(lo, hi)) });
    }
    faces.try_into().ok()
}

/// Load the faces of the installed font in use, if that's what it is: the listener's
/// pick, else one the theme names for its interface (Fluent's Segoe UI Variable). A
/// name can list fallbacks, "Segoe UI Variable, Segoe UI", tried in order.
fn load_choice(ui_font: &str) {
    let name = effective_font(ui_font);
    if name.is_empty() || lyric_key(&name).is_some() || name.eq_ignore_ascii_case("inter") {
        *CHOICE.write().unwrap() = None;
        return;
    }
    if CHOICE.read().unwrap().as_ref().is_some_and(|c| c.name == name) {
        return;
    }
    // Only when an installed font is set: a one-off scan, the first time.
    let all = INSTALLED_FONTS.get_or_init(scan_fonts);
    let faces = name.split(',').map(str::trim).find_map(|n| all.iter().find(|f| f.name.eq_ignore_ascii_case(n)).and_then(choose_faces));
    if faces.is_none() {
        log::warn!("font {name:?} isn't installed; using Inter");
    }
    *CHOICE.write().unwrap() = faces.map(|faces| Chosen { name, faces });
}

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

/// Semibold and bold faces of the fallbacks, so text in a script the primary font
/// lacks comes out at the weight the line asks for, not regular inside a bold title
/// (Better Lyrics uses Noto Sans CJK at the lyric weight).
#[cfg(windows)]
const HEAVIER: &[(&str, [(&str, u32); 2])] = &[
    ("C:\\Windows\\Fonts\\segoeui.ttf", [("C:\\Windows\\Fonts\\seguisb.ttf", 0), ("C:\\Windows\\Fonts\\segoeuib.ttf", 0)]),
    ("C:\\Windows\\Fonts\\YuGothM.ttc", [("C:\\Windows\\Fonts\\YuGothB.ttc", 0), ("C:\\Windows\\Fonts\\YuGothB.ttc", 0)]),
    ("C:\\Windows\\Fonts\\msyh.ttc", [("C:\\Windows\\Fonts\\msyhbd.ttc", 0), ("C:\\Windows\\Fonts\\msyhbd.ttc", 0)]),
    ("C:\\Windows\\Fonts\\msjh.ttc", [("C:\\Windows\\Fonts\\msjhbd.ttc", 0), ("C:\\Windows\\Fonts\\msjhbd.ttc", 0)]),
    ("C:\\Windows\\Fonts\\malgun.ttf", [("C:\\Windows\\Fonts\\malgunbd.ttf", 0), ("C:\\Windows\\Fonts\\malgunbd.ttf", 0)]),
    ("C:\\Windows\\Fonts\\Nirmala.ttc", [("C:\\Windows\\Fonts\\Nirmala.ttc", 1), ("C:\\Windows\\Fonts\\Nirmala.ttc", 1)]),
    ("C:\\Windows\\Fonts\\LeelawUI.ttf", [("C:\\Windows\\Fonts\\LeelaUIb.ttf", 0), ("C:\\Windows\\Fonts\\LeelaUIb.ttf", 0)]),
    ("C:\\Windows\\Fonts\\ebrima.ttf", [("C:\\Windows\\Fonts\\ebrimabd.ttf", 0), ("C:\\Windows\\Fonts\\ebrimabd.ttf", 0)]),
];
#[cfg(not(windows))]
const HEAVIER: &[(&str, [(&str, u32); 2])] = &[];

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

/// Ascent and line height in em, as egui lays text out (skrifa's metrics: OS/2 typo
/// values when the font asks for them, else hhea).
fn em_metrics(bytes: &[u8], index: u32) -> Option<(f32, f32)> {
    use skrifa::MetadataProvider;
    let font = skrifa::FontRef::from_index(bytes, index).ok()?;
    let m = font.metrics(skrifa::instance::Size::unscaled(), skrifa::instance::LocationRef::default());
    let upem = m.units_per_em.max(1) as f32;
    Some((m.ascent / upem, (m.ascent - m.descent + m.leading) / upem))
}

fn font_data(face: Face, y_offset_factor: f32) -> FontData {
    let mut tweak = egui::FontTweak { y_offset_factor, ..Default::default() };
    if let Some(w) = face.wght {
        tweak.coords.push(b"wght", w);
    }
    FontData { index: face.index, ..FontData::from_static(face.bytes) }.tweak(tweak)
}

/// Install fonts. `ui_font` is the theme's font name for all interface text ("inter"
/// or one of the bundled lyric fonts); the listener's font setting wins over it.
///
/// Each primary font is followed by Inter at the same weight (for the scripts the
/// theme fonts lack), then the system fallbacks at the closest weight.
///
/// egui places a fallback font's glyphs by centring its line height on the primary
/// font's, not by sharing a baseline, so CJK glyphs (Yu Gothic, Microsoft YaHei, …)
/// sit visibly higher than Latin ones in the same line. Each fallback therefore gets a
/// y offset computed from both fonts' metrics, once per primary font it backs.
pub fn install_fonts(ctx: &egui::Context, ui_font: &str) {
    load_choice(ui_font);
    let ui_key = font_key(ui_font);
    // Installing rebuilds the font atlas; most themes share a UI font, so skip repeats.
    static INSTALLED: std::sync::Mutex<Option<(&'static str, String)>> = std::sync::Mutex::new(None);
    let loaded = CHOICE.read().unwrap().as_ref().map(|c| c.name.clone()).unwrap_or_default();
    let state = Some((ui_key, loaded));
    {
        let mut installed = INSTALLED.lock().unwrap();
        if *installed == state {
            return;
        }
        *installed = state;
    }
    let mut defs = FontDefinitions::default();
    let egui_default = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    defs.font_data.insert("lucide".into(), Arc::new(FontData::from_static(LUCIDE).tweak(egui::FontTweak { y_offset_factor: 0.0, ..Default::default() })));
    defs.families.insert(FontFamily::Name("icons".into()), vec!["lucide".to_owned()]);

    // Mapped once for the life of the process, however often fonts are installed:
    // the fallback list at each weight.
    type Fallbacks = [Vec<(String, Face)>; 3];
    static SYSTEM: std::sync::OnceLock<Fallbacks> = std::sync::OnceLock::new();
    let system = SYSTEM.get_or_init(|| {
        std::array::from_fn(|w| {
            CANDIDATES
                .iter()
                .filter_map(|&(path, index)| {
                    let heavy = (w > 0).then(|| HEAVIER.iter().find(|(base, _)| *base == path)).flatten().map(|(_, v)| v[w - 1]);
                    let mapped = |(p, i): (&str, u32)| map(p).map(|bytes| (format!("sys:{p}#{i}"), Face { bytes, index: i, wght: None }));
                    heavy.and_then(mapped).or_else(|| mapped((path, index)))
                })
                .collect()
        })
    });
    // Primary fonts: Inter, each lyric font, and the installed font picked in Settings.
    let choice = CHOICE.read().unwrap().as_ref().map(|c| c.faces);
    let mut groups: Vec<(&str, [Face; 3])> = vec![("inter", INTER.map(Face::of))];
    groups.extend(LYRIC_FONTS.iter().map(|(k, w)| (*k, w.map(Face::of))));
    groups.extend(choice.map(|faces| ("choice", faces)));
    for (key, faces) in &groups {
        let (asc_p, h_p) = em_metrics(faces[0].bytes, faces[0].index).unwrap_or((0.97, 1.21));
        for (w, face) in faces.iter().enumerate() {
            let name = format!("{key}-{w}");
            defs.font_data.insert(name.clone(), Arc::new(font_data(*face, 0.0)));
            let mut chain = vec![name.clone()];
            let inter = (*key != "inter").then(|| ("inter".to_owned(), Face::of(INTER[w])));
            for (fallback, f) in inter.iter().chain(&system[w]) {
                let Some((asc_f, h_f)) = em_metrics(f.bytes, f.index) else { continue };
                let shift = asc_p - (asc_f + 0.5 * (h_p - h_f));
                let id = format!("{fallback}-{w}@{key}");
                if !defs.font_data.contains_key(&id) {
                    defs.font_data.insert(id.clone(), Arc::new(font_data(*f, shift)));
                }
                chain.push(id);
            }
            if *key == "inter" && w == 0
                && let Some(mono) = defs.families.get_mut(&FontFamily::Monospace)
            {
                mono.extend(chain[1..].iter().cloned());
            }
            // egui's bundled emoji/icon font stays as a last resort.
            chain.extend(egui_default.iter().filter(|n| n.as_str() != "Ubuntu-Light").cloned());
            defs.families.insert(FontFamily::Name(name.clone().into()), chain.clone());
            if *key == ui_key {
                let family = match w {
                    0 => FontFamily::Proportional,
                    1 => FontFamily::Name("semibold".into()),
                    _ => FontFamily::Name("bold".into()),
                };
                defs.families.insert(family, chain);
            }
        }
    }
    // With no installed font picked, the "choice" families are Inter's, so they always
    // exist without a second copy of Inter and its fallbacks.
    if choice.is_none() {
        for w in 0..3 {
            let inter = defs.families[&FontFamily::Name(format!("inter-{w}").into())].clone();
            defs.families.insert(FontFamily::Name(format!("choice-{w}").into()), inter);
        }
    }
    // The same bytes get the same `Arc` on every install, so comparing these definitions
    // with the installed ones is a pointer check per font instead of reading every file.
    for data in defs.font_data.values_mut() {
        let font = shared_bytes(&data.font);
        if !Arc::ptr_eq(&font, &data.font) {
            *data = Arc::new(FontData { font, ..(**data).clone() });
        }
    }
    ctx.set_fonts(defs);
}

/// The first `Blob` seen for these bytes. Font bytes here are all `'static` (bundled, or
/// mapped for the rest of the process), so the address identifies them for good.
fn shared_bytes(blob: &egui::epaint::text::Blob) -> egui::epaint::text::Blob {
    static SEEN: std::sync::Mutex<Vec<egui::epaint::text::Blob>> = std::sync::Mutex::new(Vec::new());
    let bytes = (**blob).as_ref();
    let mut seen = SEEN.lock().unwrap();
    if let Some(b) = seen.iter().find(|b| std::ptr::eq((***b).as_ref(), bytes)) {
        return b.clone();
    }
    seen.push(blob.clone());
    blob.clone()
}

/// Map a font file for the rest of the process. Kept by canonical path, so a file is
/// mapped once however many weights, faces or installed-font picks use it, and whichever
/// spelling names it (the installed-font scan's C:\WINDOWS, the fallback list's C:\Windows).
fn map(path: impl AsRef<std::path::Path>) -> Option<&'static [u8]> {
    static MAPPED: std::sync::Mutex<Vec<(std::path::PathBuf, &'static [u8])>> = std::sync::Mutex::new(Vec::new());
    let path = std::fs::canonicalize(path).ok()?;
    let mut mapped = MAPPED.lock().unwrap();
    if let Some((_, bytes)) = mapped.iter().find(|(p, _)| *p == path) {
        return Some(bytes);
    }
    let file = std::fs::File::open(&path).ok()?;
    // SAFETY: system font files aren't modified while in use; the mapping is
    // read-only and intentionally lives for the rest of the process.
    let mmap = unsafe { memmap2::Mmap::map(&file) }.ok()?;
    let bytes: &'static [u8] = Box::leak(Box::new(mmap));
    mapped.push((path, bytes));
    Some(bytes)
}

/// Apply a theme preset's chrome: how strong glass surfaces are and how round
/// buttons, pills and fields are.
pub fn apply_chrome(ctx: &egui::Context, p: &crate::preset::Preset) {
    GLASS.store(p.chrome.glass.clamp(0.0, 2.5).to_bits(), std::sync::atomic::Ordering::Relaxed);
    let dim = crate::preset::hex(&p.app.text_dim).unwrap_or(TEXT_DIM);
    DIM.store(u32::from_le_bytes(dim.to_array()), std::sync::atomic::Ordering::Relaxed);
    install_style(ctx);
    install_fonts(ctx, &p.app.font);
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
        s.visuals = visuals();
        s.interaction.selectable_labels = false;
        s.animation_time = 0.18 * SPEED;
    });
}

/// egui's colours and shapes for the current mode and the active preset's chrome.
fn visuals() -> egui::Visuals {
    let p = crate::preset::active();
    let mut v = if light() { egui::Visuals::light() } else { egui::Visuals::dark() };
    v.panel_fill = bg();
    v.window_fill = surface_fill();
    v.extreme_bg_color = glass(0.08);
    v.override_text_color = Some(text());
    v.selection.bg_fill = with_alpha(RED, if light() { 0.3 } else { 0.5 });
    v.hyperlink_color = text();
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);
    let pr = egui::CornerRadius::same(p.app.panel_radius.clamp(0.0, 32.0) as u8);
    v.window_corner_radius = pr;
    v.menu_corner_radius = pr;
    v.window_stroke = egui::Stroke::new(1.0, glass(0.08));
    v.popup_shadow = egui::epaint::Shadow { offset: [0, 8], blur: 28, spread: 0, color: shade(if light() { 0.16 } else { 0.5 }) };
    let r = egui::CornerRadius::same(p.chrome.radius.clamp(2.0, 28.0) as u8);
    for (w, fill) in [
        (&mut v.widgets.inactive, glass(0.08)),
        (&mut v.widgets.hovered, glass(0.14)),
        (&mut v.widgets.active, glass(0.2)),
        (&mut v.widgets.open, glass(0.14)),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.bg_stroke = egui::Stroke::NONE;
        w.corner_radius = r;
        w.fg_stroke.color = text();
    }
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, glass(0.08));
    v
}

