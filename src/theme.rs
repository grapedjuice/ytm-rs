//! Visual design tokens: fonts, icons, colours, motion.

use std::sync::Arc;

use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId};

// ------------------------------------------------------------------ colours

/// Base surface under the animated background (seen while it fades in).
const BG: Color32 = Color32::from_rgb(12, 10, 14);
pub const TEXT: Color32 = Color32::from_rgb(242, 240, 245);
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
    if let Some(c) = filed(F_TEXT) {
        return c;
    }
    if light() {
        return TEXT_LIGHT;
    }
    match TEXT_OVER.load(std::sync::atomic::Ordering::Relaxed) {
        0 => TEXT,
        v => {
            let [r, g, b, a] = v.to_le_bytes();
            Color32::from_rgba_premultiplied(r, g, b, a)
        }
    }
}

/// The theme's main text colour, when it has one (0: none), and its small fades against the usual.
static TEXT_OVER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static FADE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x3f80_0000);

fn fade() -> f32 {
    f32::from_bits(FADE.load(std::sync::atomic::Ordering::Relaxed))
}

/// Text and icons on a fill of the main text colour (a primary button, a selected chip).
pub fn on_text() -> Color32 {
    match filed(F_TEXT) {
        Some(t) => {
            if luma(t) > 140.0 {
                Color32::from_gray(15)
            } else {
                Color32::WHITE
            }
        }
        None if light() => Color32::WHITE,
        None => Color32::from_gray(15),
    }
}

/// Least important text: counts, hints, placeholders.
pub fn text_faint() -> Color32 {
    filed(F_TEXT_FAINT).unwrap_or(if light() { TEXT_FAINT_LIGHT } else { TEXT_FAINT })
}

/// The window's base colour, under the background.
pub fn bg() -> Color32 {
    filed(F_BACKGROUND).unwrap_or(if light() { BG_LIGHT } else { BG })
}

/// Menus and popups.
pub fn menu_fill() -> Color32 {
    // A theme file's menu colour is used as it is: solid, so it stays readable.
    if let Some(c) = filed(F_MENU) {
        return c;
    }
    if light() {
        return Color32::WHITE;
    }
    let look = &crate::preset::active().app;
    // See-through only when what's behind is really frosted: menus have to stay readable.
    let alpha = if look.glass.menu.is_some() && crate::shader::glass_ready() { look.menu_look.alpha } else { look.menu_look.alpha.max(0.85) };
    let color = crate::preset::hex(&look.menu_color).filter(|c| alpha >= look.menu_look.alpha.max(0.6) || c.r() as u32 + c.g() as u32 + c.b() as u32 <= 380).unwrap_or(Color32::from_rgb(38, 34, 42));
    with_alpha(color, alpha)
}

/// Menu colour for what has no frost behind it (toasts, the selection bar, the volume
/// popup): never so see-through that it needs the blur to stay readable.
pub fn menu_fill_solid() -> Color32 {
    if let Some(c) = filed(F_MENU) {
        return c;
    }
    if light() {
        return Color32::WHITE;
    }
    let look = &crate::preset::active().app;
    let color = crate::preset::hex(&look.menu_color).unwrap_or(Color32::from_rgb(38, 34, 42));
    with_alpha(color, look.menu_look.alpha.max(0.85))
}

/// What popups and menus are drawn on.
pub fn menu_frame(style: &egui::Style) -> egui::Frame {
    egui::Frame::popup(style).fill(menu_fill())
}

/// Dialogs and cards that float over the page.
pub fn surface_fill() -> Color32 {
    filed(F_SURFACE).unwrap_or(if light() { Color32::from_rgb(252, 251, 253) } else { Color32::from_rgb(28, 24, 32) })
}

fn luma(c: Color32) -> f32 {
    0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32
}

// A theme file's colours (Settings › Appearance › Accent colour › From file), for wallust,
// pywal and the like: `theme.conf` in the data folder, a `name = #rrggbb` on each line.
//
//     accent = #e0af68
//     background = #1a1b26
//     text = #c0caf5
//     text-dim = #a9b1d6
//     text-faint = #565f89
//     surface = #1f2335
//     menu = #24283b
//
// Any may be left out. `foreground` and `aux-foreground` are other names for `text` and
// `text-dim`. With a background, pages are that flat colour instead of the album art, and
// what isn't given (surface, menu, the dimmer text) is mixed from it and the text.

/// What `theme.conf` holds.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct FileTheme {
    pub accent: Option<Color32>,
    pub background: Option<Color32>,
    pub surface: Option<Color32>,
    pub menu: Option<Color32>,
    pub text: Option<Color32>,
    pub text_dim: Option<Color32>,
    pub text_faint: Option<Color32>,
}

impl FileTheme {
    fn parse(s: &str) -> Self {
        let mut t = Self::default();
        for line in s.lines().map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')) {
            // A line starting with # is a comment: names don't, only colours do.
            if line.starts_with('#') || line.starts_with("//") || line.starts_with(';') {
                continue;
            }
            let Some((name, value)) = line.split_once(['=', ':']) else { continue };
            let Some(c) = parse_accent(value.trim().trim_matches(['"', '\'', ',', ';'])) else { continue };
            let slot = match name.trim().trim_matches(['"', '\'']).to_ascii_lowercase().replace('_', "-").as_str() {
                "accent" => &mut t.accent,
                "background" | "bg" => &mut t.background,
                "surface" => &mut t.surface,
                "menu" => &mut t.menu,
                "text" | "foreground" | "fg" => &mut t.text,
                "text-dim" | "aux-foreground" | "secondary" => &mut t.text_dim,
                "text-faint" | "faint" => &mut t.text_faint,
                _ => continue,
            };
            *slot = Some(c);
        }
        t
    }

    /// With what follows from the colours given: surfaces a step off the background
    /// toward the text, and dimmer text a step off the text toward the background.
    fn filled(mut self) -> Self {
        if let Some(bg) = self.background {
            let ink = self.text.unwrap_or(if luma(bg) > 140.0 { TEXT_LIGHT } else { TEXT });
            self.text = Some(ink);
            self.surface = self.surface.or(Some(lerp_color(bg, ink, 0.06)));
            self.menu = self.menu.or(Some(lerp_color(bg, ink, 0.11)));
            self.text_dim = self.text_dim.or(Some(lerp_color(ink, bg, 0.28)));
            self.text_faint = self.text_faint.or(Some(lerp_color(ink, bg, 0.5)));
        } else if let Some(ink) = self.text {
            let bg = if luma(ink) > 140.0 { BG } else { BG_LIGHT };
            self.text_dim = self.text_dim.or(Some(lerp_color(ink, bg, 0.28)));
            self.text_faint = self.text_faint.or(Some(lerp_color(ink, bg, 0.5)));
        }
        self
    }

    /// Whether these are colours for light pages: by the background, if there is one.
    fn light(&self) -> Option<bool> {
        self.background.map(|bg| luma(bg) > 140.0)
    }

    /// The mode these colours are for: the background's, or without one the one the
    /// text is readable in (light text is for dark pages).
    fn mode(&self) -> Option<bool> {
        self.light().or(self.text.map(|t| luma(t) <= 140.0))
    }
}

/// A theme file to start from: the accent showing now, the app's own text colours, and
/// the background left for whoever wants flat pages. Nothing looks different until it's
/// edited.
pub fn theme_template(accent: Color32) -> String {
    let hex = |c: Color32| format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b());
    format!(
        "# ytm-rs colours. One \"name = #rrggbb\" on each line.\n\
         # Save the file and the app follows within a couple of seconds: no restart.\n\
         # Lines starting with # are ignored. Remove the # to use one.\n\
         \n\
         accent = {}\n\
         \n\
         text = {}\n\
         text-dim = {}\n\
         text-faint = {}\n\
         \n\
         # With a background, pages are that flat colour instead of the album art.\n\
         # Surfaces are dialogs and cards; menus are menus and popups.\n\
         # background = {}\n\
         # surface = {}\n\
         # menu = {}\n",
        hex(accent),
        hex(TEXT),
        hex(TEXT_DIM),
        hex(TEXT_FAINT),
        hex(BG),
        hex(Color32::from_rgb(28, 24, 32)),
        hex(Color32::from_rgb(38, 34, 42)),
    )
}

/// Write the starter theme file at `path` unless one is there already.
pub fn ensure_theme_file(path: &std::path::Path, accent: Color32) -> std::io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    std::fs::write(path, theme_template(accent))
}

/// Open a text file to edit: Notepad on Windows (a .conf file has no program of its own
/// there), the system's text editor elsewhere.
pub fn open_in_editor(path: &std::path::Path) {
    #[cfg(windows)]
    let cmd = std::process::Command::new("notepad").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let cmd = std::process::Command::new("open").arg("-t").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = std::process::Command::new("xdg-open").arg(path).spawn();
    if let Err(e) = cmd {
        log::warn!("theme file: couldn't open {}: {e}", path.display());
    }
}

const F_BACKGROUND: usize = 0;
const F_SURFACE: usize = 1;
const F_MENU: usize = 2;
const F_TEXT: usize = 3;
const F_TEXT_DIM: usize = 4;
const F_TEXT_FAINT: usize = 5;
/// The theme file's colours in use (0: not given), read very often.
static FILED: [std::sync::atomic::AtomicU32; 6] = [const { std::sync::atomic::AtomicU32::new(0) }; 6];
/// The mode the file's colours are for (its background's, else its text's). 0: either;
/// 1: dark; 2: light.
static FILED_MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// The theme file's colour for `slot`, where it applies: its colours go with its
/// background, so not on what's drawn in the other mode (now playing, dark over the art,
/// with a light theme file).
fn filed(slot: usize) -> Option<Color32> {
    use std::sync::atomic::Ordering::Relaxed;
    let v = FILED[slot].load(Relaxed);
    if v == 0 {
        return None;
    }
    let mode = FILED_MODE.load(Relaxed);
    if mode != 0 && (mode == 2) != light() {
        return None;
    }
    let [r, g, b, _] = v.to_le_bytes();
    Some(Color32::from_rgb(r, g, b))
}

/// Use these theme file colours from now on (`FileTheme::default()`: none).
pub fn set_file_theme(ctx: &egui::Context, t: FileTheme) {
    use std::sync::atomic::Ordering::Relaxed;
    let t = t.filled();
    let slots = [t.background, t.surface, t.menu, t.text, t.text_dim, t.text_faint];
    let mode = match t.mode() {
        None => 0,
        Some(false) => 1,
        Some(true) => 2,
    };
    let mut changed = FILED_MODE.swap(mode, Relaxed) != mode;
    for (slot, c) in FILED.iter().zip(slots) {
        // Opaque, so never 0.
        let v = c.map_or(0, |c| u32::from_le_bytes([c.r(), c.g(), c.b(), 255]));
        changed |= slot.swap(v, Relaxed) != v;
    }
    if changed {
        ctx.global_style_mut(|s| s.visuals = visuals());
    }
}

/// The theme file's background, when it gives one: pages are that flat colour.
pub fn file_background() -> Option<Color32> {
    filed(F_BACKGROUND)
}

/// Whether the theme file in use is for light pages (`None`: it gives no background).
pub fn file_light() -> Option<bool> {
    use std::sync::atomic::Ordering::Relaxed;
    if FILED[F_BACKGROUND].load(Relaxed) == 0 {
        return None;
    }
    Some(FILED_MODE.load(Relaxed) == 2)
}

/// The theme file, looked at every couple of seconds so it follows a tool rewriting it.
/// Empty while it's missing.
pub fn file_theme(path: &std::path::Path) -> FileTheme {
    use std::time::{Duration, Instant};
    static THEME: std::sync::Mutex<Option<(Instant, FileTheme)>> = std::sync::Mutex::new(None);
    let mut cached = THEME.lock().unwrap();
    match *cached {
        Some((at, t)) if at.elapsed() < Duration::from_secs(2) => t,
        _ => {
            let t = std::fs::read(path).map(|b| FileTheme::parse(&decode_text(&b))).unwrap_or_default();
            *cached = Some((Instant::now(), t));
            t
        }
    }
}

#[test]
fn theme_files_name_their_colours() {
    let rgb = |v: u32| Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8));
    let t = FileTheme::parse("# from wallust\naccent = #e0af68\nbackground: \"#1a1b26\"\nforeground = c0caf5\naux_foreground = #a9b1d6\nnonsense = #ffffff\ntext-faint = nope\n#565f89\n");
    assert_eq!((t.accent, t.background, t.text, t.text_dim), (rgb(0xe0af68), rgb(0x1a1b26), rgb(0xc0caf5), rgb(0xa9b1d6)));
    assert_eq!((t.text_faint, t.surface, t.menu), (None, None, None));
    assert_eq!(t.light(), Some(false));
    // What's missing is mixed from the background and the text.
    let f = t.filled();
    assert!(f.surface.is_some() && f.menu.is_some() && f.text_faint.is_some());
    assert_eq!(f.text_dim, rgb(0xa9b1d6));
    // A light background gets dark text when none is given.
    let l = FileTheme::parse("background = #fafafa").filled();
    assert_eq!((l.light(), l.text), (Some(true), Some(TEXT_LIGHT)));
    // Text alone is for the pages it reads on; it doesn't say which the pages are.
    let ink = FileTheme::parse("text = #f2f0f5");
    assert_eq!((ink.light(), ink.mode()), (None, Some(false)));
    // The starter file changes nothing: the app's own text colours, no background.
    let start = FileTheme::parse(&theme_template(Color32::from_rgb(0xe0, 0xaf, 0x68)));
    assert_eq!(start, FileTheme { accent: rgb(0xe0af68), text: Some(TEXT), text_dim: Some(TEXT_DIM), text_faint: Some(TEXT_FAINT), ..Default::default() });
    // Just an accent changes nothing else.
    assert_eq!(FileTheme::parse("accent=#ff00aa").filled(), FileTheme { accent: rgb(0xff00aa), ..Default::default() });
    assert_eq!(FileTheme::parse(""), FileTheme::default());
}

/// Theme preset multiplier for every glass surface (f32 bits; 1.0 by default).
static GLASS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x3f80_0000);

/// White at `a` opacity for controls: hover, selection, tracks, fields and toggles (black
/// in light mode). A theme can make these stronger but not weaker, or they'd disappear.
pub fn glass(a: f32) -> Color32 {
    glass_at(a * glass_scale().max(1.0))
}

/// White at `a` opacity for what only decorates: panels, dividers and placeholders. These
/// follow the theme's glass setting all the way down.
pub fn glass_panel(a: f32) -> Color32 {
    glass_at(a * glass_scale())
}

fn glass_scale() -> f32 {
    f32::from_bits(GLASS.load(std::sync::atomic::Ordering::Relaxed))
}

fn glass_at(a: f32) -> Color32 {
    let a = (a.min(1.0) * 255.0) as u8;
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
    if let Some(c) = filed(F_TEXT_DIM) {
        return c;
    }
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

/// The Windows accent colour, in the shade Windows itself uses on the surface being drawn:
/// `AccentPalette`'s second entry on dark, its fifth on light, or the base accent if
/// there's no palette. Read again every couple of seconds, so a new colour picked in
/// Windows' settings shows without a restart. `None` off Windows or when the setting
/// can't be read.
pub fn system_accent() -> Option<Color32> {
    use std::time::{Duration, Instant};
    type Shades = Option<[Color32; 2]>;
    static ACCENT: std::sync::Mutex<Option<(Instant, Shades)>> = std::sync::Mutex::new(None);
    let mut cached = ACCENT.lock().unwrap();
    let shades = match *cached {
        Some((at, c)) if at.elapsed() < Duration::from_secs(2) => c,
        _ => {
            let c = read_system_accent();
            *cached = Some((Instant::now(), c));
            c
        }
    };
    shades.map(|[dark, light_shade]| if light() { light_shade } else { dark })
}

/// The colour in the accent file, looked at every couple of seconds so it follows a tool
/// rewriting the file. None while it's missing or doesn't hold a colour.
pub fn file_accent(path: &std::path::Path) -> Option<Color32> {
    use std::time::{Duration, Instant};
    static ACCENT: std::sync::Mutex<Option<(Instant, Option<Color32>)>> = std::sync::Mutex::new(None);
    let mut cached = ACCENT.lock().unwrap();
    match *cached {
        Some((at, c)) if at.elapsed() < Duration::from_secs(2) => c,
        _ => {
            let c = std::fs::read(path).ok().and_then(|b| parse_accent(&decode_text(&b)));
            *cached = Some((Instant::now(), c));
            c
        }
    }
}

/// UTF-8, or the UTF-16 Windows PowerShell's `>` writes.
fn decode_text(bytes: &[u8]) -> String {
    match bytes {
        [0xff, 0xfe, rest @ ..] => String::from_utf16_lossy(&rest.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect::<Vec<_>>()),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// `#rrggbb` or `rrggbb`, with any whitespace or byte-order mark around it.
fn parse_accent(s: &str) -> Option<Color32> {
    crate::preset::hex(&format!("#{}", s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}').trim_start_matches('#')))
}

#[test]
fn accent_files_hold_one_colour() {
    let pink = Some(Color32::from_rgb(0xff, 0x00, 0xaa));
    assert_eq!(parse_accent("#ff00aa\n"), pink);
    assert_eq!(parse_accent("  ff00aa "), pink);
    assert_eq!(parse_accent(&decode_text(b"\xef\xbb\xbf#ff00aa\r\n")), pink);
    let utf16: Vec<u8> = [0xfeffu16].into_iter().chain("#ff00aa\r\n".encode_utf16()).flat_map(u16::to_le_bytes).collect();
    assert_eq!(parse_accent(&decode_text(&utf16)), pink);
    assert_eq!(parse_accent("#ff00"), None);
    assert_eq!(parse_accent("not a colour"), None);
}

/// The accent's shades for dark and light surfaces.
#[cfg(windows)]
fn read_system_accent() -> Option<[Color32; 2]> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::core::w;
    // SAFETY: out-buffers sized to what we pass; the strings are static.
    unsafe {
        // Eight RGBA swatches, lightest first; the second is "light 2".
        let mut palette = [0u8; 32];
        let mut size = palette.len() as u32;
        let key = w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent");
        if RegGetValueW(HKEY_CURRENT_USER, key, w!("AccentPalette"), RRF_RT_REG_BINARY, None, Some(palette.as_mut_ptr() as *mut _), Some(&mut size)).is_ok() && size >= 20 {
            return Some([Color32::from_rgb(palette[4], palette[5], palette[6]), Color32::from_rgb(palette[16], palette[17], palette[18])]);
        }
        // 0xAABBGGRR.
        let mut abgr = 0u32;
        let mut size = 4u32;
        RegGetValueW(HKEY_CURRENT_USER, w!(r"Software\Microsoft\Windows\DWM"), w!("AccentColor"), RRF_RT_REG_DWORD, None, Some(&mut abgr as *mut u32 as *mut _), Some(&mut size))
            .is_ok()
            .then(|| [Color32::from_rgb(abgr as u8, (abgr >> 8) as u8, (abgr >> 16) as u8); 2])
    }
}

#[cfg(not(windows))]
fn read_system_accent() -> Option<[Color32; 2]> {
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

/// How far a menu's content sits in from its edge.
pub const MENU_INSET: i8 = 6;

/// Corner radius of a row or field inside a menu: the menu's less the inset, so the two
/// curves run parallel instead of the inner one bulging into the corner.
pub fn menu_item_radius() -> f32 {
    (panel_radius() as f32 - MENU_INSET as f32).max(0.0)
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

/// `fg` drawn over an opaque `bg`.
pub fn over(fg: Color32, bg: Color32) -> Color32 {
    let k = 1.0 - fg.a() as f32 / 255.0;
    let c = |f: u8, b: u8| (f as f32 + b as f32 * k).round().min(255.0) as u8;
    Color32::from_rgb(c(fg.r(), bg.r()), c(fg.g(), bg.g()), c(fg.b(), bg.b()))
}

/// WCAG relative luminance.
fn luminance(c: Color32) -> f32 {
    let lin = |v: u8| {
        let s = v as f32 / 255.0;
        if s <= 0.03928 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// WCAG contrast between two opaque colours.
fn contrast(a: Color32, b: Color32) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// Text and icons on a fill of this colour: dark or white, whichever reads better.
pub fn on_fill(fill: Color32) -> Color32 {
    let dark = Color32::from_gray(15);
    if contrast(dark, fill) > contrast(Color32::WHITE, fill) { dark } else { Color32::WHITE }
}

/// `c`, with only as much black mixed in as it takes to reach this contrast on a light `bg`.
pub fn deepen(c: Color32, bg: Color32, ratio: f32) -> Color32 {
    if contrast(c, bg) >= ratio {
        return c;
    }
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..8 {
        let mid = (lo + hi) / 2.0;
        if contrast(lerp_color(c, Color32::BLACK, mid), bg) >= ratio {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    lerp_color(c, Color32::BLACK, hi)
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
    ctx.animate_value_with_time(egui::Id::unique(id), target, secs * SPEED * fade())
}

pub fn anim_bool(ctx: &egui::Context, id: impl std::hash::Hash + std::fmt::Debug, on: bool, secs: f32) -> f32 {
    eased(ctx.animate_bool_with_time(egui::Id::unique(id), on, secs * SPEED * fade()))
}

/// [`anim_bool`] for a transition whose length the theme sets itself, so `ui_fade_secs` leaves it alone.
pub fn anim_bool_timed(ctx: &egui::Context, id: impl std::hash::Hash + std::fmt::Debug, on: bool, secs: f32) -> f32 {
    eased(ctx.animate_bool_with_time(egui::Id::unique(id), on, secs * SPEED))
}

fn eased(t: f32) -> f32 {
    if crate::preset::active().app.controls.spring { ease_back(t) } else { ease_out(t) }
}

/// Past the end and back: `ease-out-back`.
fn ease_back(t: f32) -> f32 {
    const C: f32 = 1.70158;
    let u = t.clamp(0.0, 1.0) - 1.0;
    1.0 + (C + 1.0) * u * u * u + C * u * u
}

// ------------------------------------------------------------------ type

pub fn regular(size: f32) -> FontId {
    if crate::preset::active().app.all_bold {
        return bold(size);
    }
    FontId::proportional(size)
}
// Named families, made once: these are called hundreds of times a frame, and a
// clone is a reference count instead of a new string.
static SEMIBOLD: std::sync::LazyLock<FontFamily> = std::sync::LazyLock::new(|| FontFamily::Name("semibold".into()));
static BOLD: std::sync::LazyLock<FontFamily> = std::sync::LazyLock::new(|| FontFamily::Name("bold".into()));
static MEDIUM: std::sync::LazyLock<FontFamily> = std::sync::LazyLock::new(|| FontFamily::Name("medium".into()));
static LIGHT_FACE: std::sync::LazyLock<FontFamily> = std::sync::LazyLock::new(|| FontFamily::Name("light".into()));
static EXTRA_BOLD: std::sync::LazyLock<FontFamily> = std::sync::LazyLock::new(|| FontFamily::Name("extrabold".into()));
static ICONS: std::sync::LazyLock<FontFamily> = std::sync::LazyLock::new(|| FontFamily::Name("icons".into()));

pub fn semibold(size: f32) -> FontId {
    if crate::preset::active().app.all_bold {
        return bold(size);
    }
    FontId::new(size, SEMIBOLD.clone())
}

/// Interface text in a theme's weight.
pub fn weighted(w: crate::preset::Weight, size: f32) -> FontId {
    match w {
        crate::preset::Weight::ExtraBold => FontId::new(size, EXTRA_BOLD.clone()),
        crate::preset::Weight::Regular => regular(size),
        crate::preset::Weight::Semibold => semibold(size),
        crate::preset::Weight::Bold => bold(size),
        crate::preset::Weight::Medium if !crate::preset::active().app.all_bold => FontId::new(size, MEDIUM.clone()),
        crate::preset::Weight::Light if !crate::preset::active().app.all_bold => FontId::new(size, LIGHT_FACE.clone()),
        _ => bold(size),
    }
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
const INTER: [&[u8]; 5] = [
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../assets/fonts/Inter-ExtraBold.ttf"),
    include_bytes!("../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../assets/fonts/Inter-Light.ttf"),
];
const LUCIDE: &[u8] = include_bytes!("../assets/fonts/lucide.ttf");

/// Lyric typefaces used by theme presets (all SIL OFL 1.1), as static
/// regular / semibold / bold instances. They cover Latin only (their upstream has no
/// Vietnamese, Greek or Cyrillic); `lyric_font_for` sets such lines in Inter instead.
/// Instrument Sans is only ever used at semibold (one preset), so its bold slot reuses
/// that instead of shipping a weight nothing draws.
const LYRIC_FONTS: &[(&str, [&[u8]; 5])] = &[
    ("dmsans", [
        include_bytes!("../assets/fonts/DMSans-Regular.ttf"),
        include_bytes!("../assets/fonts/DMSans-SemiBold.ttf"),
        include_bytes!("../assets/fonts/DMSans-Bold.ttf"),
        include_bytes!("../assets/fonts/DMSans-Medium.ttf"),
        include_bytes!("../assets/fonts/DMSans-Regular.ttf"),
    ]),
    ("googlesansflex", [
        include_bytes!("../assets/fonts/GoogleSansFlex-Regular.ttf"),
        include_bytes!("../assets/fonts/GoogleSansFlex-SemiBold.ttf"),
        include_bytes!("../assets/fonts/GoogleSansFlex-Bold.ttf"),
        include_bytes!("../assets/fonts/GoogleSansFlex-Medium.ttf"),
        include_bytes!("../assets/fonts/GoogleSansFlex-Regular.ttf"),
    ]),
    ("figtree", [
        include_bytes!("../assets/fonts/Figtree-Regular.ttf"),
        include_bytes!("../assets/fonts/Figtree-SemiBold.ttf"),
        include_bytes!("../assets/fonts/Figtree-ExtraBold.ttf"),
        include_bytes!("../assets/fonts/Figtree-Medium.ttf"),
        include_bytes!("../assets/fonts/Figtree-Regular.ttf"),
    ]),
    ("instrumentsans", [
        include_bytes!("../assets/fonts/InstrumentSans-Regular.ttf"),
        include_bytes!("../assets/fonts/InstrumentSans-SemiBold.ttf"),
        include_bytes!("../assets/fonts/InstrumentSans-SemiBold.ttf"),
        include_bytes!("../assets/fonts/InstrumentSans-Medium.ttf"),
        include_bytes!("../assets/fonts/InstrumentSans-Regular.ttf"),
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

/// Other installed fonts the theme names (its lyrics, romanization, translation), through
/// the same always-there families as the one above, "extra0-<weight>" and on.
static EXTRAS: std::sync::RwLock<Vec<Chosen>> = std::sync::RwLock::new(Vec::new());
const EXTRA_KEYS: [&str; 3] = ["extra0", "extra1", "extra2"];

/// Counts what `install_fonts` really changed: the same name can draw differently after.
static FONTS_GENERATION: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn fonts_generation() -> u32 {
    FONTS_GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

struct Chosen {
    name: String,
    faces: [Face; 5],
    /// Its 800, nearest it has.
    extra_bold: Face,
}

/// One face of a primary font: the bytes, the face in the file, and the weight to set
/// on a variable font.
#[derive(Clone, Copy)]
struct Face {
    bytes: &'static [u8],
    index: u32,
    wght: Option<f32>,
    /// The optical size to set on a font that has that axis (Segoe UI Variable).
    opsz: Option<f32>,
}

impl Face {
    const fn of(bytes: &'static [u8]) -> Self {
        Self { bytes, index: 0, wght: None, opsz: None }
    }
}

pub fn set_font_choice(name: &str) {
    *FONT_CHOICE.write().unwrap() = name.to_owned();
}

pub fn font_choice() -> String {
    FONT_CHOICE.read().unwrap().clone()
}

/// Whether the listener's font is `name`, without copying it.
pub fn font_choice_is(name: &str) -> bool {
    *FONT_CHOICE.read().unwrap() == name
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
    let named = |c: &Chosen| c.name.eq_ignore_ascii_case(&name);
    if CHOICE.read().unwrap().as_ref().is_some_and(named) {
        return "choice";
    }
    EXTRAS.read().unwrap().iter().position(named).map_or("inter", |i| EXTRA_KEYS[i])
}

fn family(key: &str, weight: crate::preset::Weight, size: f32) -> FontId {
    use crate::preset::Weight;
    let w = match weight {
        Weight::Regular => 0,
        Weight::Semibold => 1,
        Weight::Bold => 2,
        Weight::Medium => 3,
        Weight::Light => 4,
        Weight::ExtraBold => 5,
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
        k if EXTRA_KEYS.contains(&k) => match EXTRA_KEYS.iter().position(|e| *e == k).and_then(|i| EXTRAS.read().unwrap().get(i).map(|c| c.faces[0])) {
            Some(f) => f,
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
    /// And the optical size axis, if it has one.
    opsz: Option<(f32, f32)>,
}

static INSTALLED_FONTS: std::sync::OnceLock<Vec<SysFamily>> = std::sync::OnceLock::new();
static SCANNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static SCANNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Start looking for installed fonts, so nothing on the UI thread has to wait for it.
pub fn warm_fonts(ctx: &egui::Context) {
    let ctx = ctx.clone();
    let _ = installed_fonts(move || ctx.request_repaint());
}

/// Whether the scan has finished since this was last asked.
pub fn fonts_became_ready() -> bool {
    SCANNED.swap(false, std::sync::atomic::Ordering::Relaxed)
}

/// Text fonts installed on this PC, sorted by name, without the bundled ones. `None`
/// while a background scan (started by the first call) runs; `done` runs after it.
pub fn installed_fonts(done: impl FnOnce() + Send + 'static) -> Option<&'static [SysFamily]> {
    if let Some(v) = INSTALLED_FONTS.get() {
        return Some(v);
    }
    if !SCANNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        std::thread::spawn(move || {
            let _ = INSTALLED_FONTS.set(scan_fonts());
            SCANNED.store(true, std::sync::atomic::Ordering::Relaxed);
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
            let opsz = font.axes().iter().find(|a| a.tag() == skrifa::Tag::new(b"opsz")).map(|a| (a.min_value(), a.max_value()));
            let face = SysFace { path: path.clone(), index: index as u32, weight: attrs.weight.value(), wght, opsz };
            families.entry(name.to_lowercase()).or_insert_with(|| SysFamily { name, faces: Vec::new() }).faces.push(face);
        }
    }
    families.into_values().collect()
}

/// The closest face to each of the three weights the app draws: regular, semibold, and
/// bold, leaning to 700 over 900 where a family has both.
fn choose_faces(family: &SysFamily) -> Option<[Face; 5]> {
    let mut faces = Vec::new();
    for target in [400.0, 600.0, 750.0, 500.0, 250.0] {
        faces.push(face_near(family, target)?);
    }
    faces.try_into().ok()
}

/// The face in a family that comes closest to this weight.
fn closest_face(family: &SysFamily, target: f32) -> Option<&SysFace> {
    family.faces.iter().min_by_key(|f| match f.wght {
        Some((lo, hi)) if (lo..=hi).contains(&target) => 0,
        Some((lo, hi)) => (target - target.clamp(lo, hi)).abs() as i32,
        None => (f.weight - target).abs() as i32,
    })
}

fn face_near(family: &SysFamily, target: f32) -> Option<Face> {
    let best = closest_face(family, target)?;
    let bytes = map(&best.path)?;
    // Interface text is about this big, where the axis is at its most legible.
    Some(Face { bytes, index: best.index, wght: best.wght.map(|(lo, hi)| target.clamp(lo, hi)), opsz: best.opsz.map(|(lo, hi)| 14.0f32.clamp(lo, hi)) })
}

/// Load the faces of the installed fonts in use, if that's what they are: the listener's
/// pick, else what the theme names for its interface (Fluent's Segoe UI Variable) and,
/// in `others`, for the rest. A name can list fallbacks, "Segoe UI Variable, Segoe UI",
/// tried in order. Until the scan has finished they're all Inter; `fonts_became_ready`
/// says when to come back.
fn load_choice(ui_font: &str, others: &[&str]) {
    let wanted = |n: &str| !n.is_empty() && lyric_key(n).is_none() && !n.eq_ignore_ascii_case("inter");
    let ui_name = effective_font(ui_font);
    let mut extras: Vec<String> = Vec::new();
    for other in others {
        let n = effective_font(other);
        if wanted(&n) && !n.eq_ignore_ascii_case(&ui_name) && !extras.iter().any(|e| e.eq_ignore_ascii_case(&n)) {
            extras.push(n);
        }
    }
    extras.truncate(EXTRA_KEYS.len());
    let name = wanted(&ui_name).then_some(ui_name);
    if CHOICE.read().unwrap().as_ref().map(|c| &c.name) == name.as_ref() && EXTRAS.read().unwrap().iter().map(|c| &c.name).eq(extras.iter()) {
        return;
    }
    let scan = if name.is_some() || !extras.is_empty() { installed_fonts(|| {}) } else { None };
    let Some(all) = scan else {
        *CHOICE.write().unwrap() = None;
        EXTRAS.write().unwrap().clear();
        return;
    };
    *CHOICE.write().unwrap() = name.and_then(|n| load_installed(all, n));
    *EXTRAS.write().unwrap() = extras.into_iter().filter_map(|n| load_installed(all, n)).collect();
}

fn load_installed(all: &[SysFamily], name: String) -> Option<Chosen> {
    let faces = name.split(',').map(str::trim).find_map(|n| all.iter().find(|f| f.name.eq_ignore_ascii_case(n)).and_then(|f| Some((choose_faces(f)?, face_near(f, 800.0)?))));
    if faces.is_none() {
        log::warn!("font {name:?} isn't installed; using Inter");
    }
    faces.map(|(faces, extra_bold)| Chosen { name, faces, extra_bold })
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
    if let Some(o) = face.opsz {
        tweak.coords.push(b"opsz", o);
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
///
/// `others`: what else the theme names, beyond what's bundled and `ui_font`.
pub fn install_fonts(ctx: &egui::Context, ui_font: &str, others: &[&str]) {
    load_choice(ui_font, others);
    let ui_key = font_key(ui_font);
    // Installing rebuilds the font atlas; most themes share a UI font, so skip repeats.
    static INSTALLED: std::sync::Mutex<Option<(&'static str, String, Vec<String>)>> = std::sync::Mutex::new(None);
    let loaded = CHOICE.read().unwrap().as_ref().map(|c| c.name.clone()).unwrap_or_default();
    let extras = EXTRAS.read().unwrap().iter().map(|c| c.name.clone()).collect();
    let state = Some((ui_key, loaded, extras));
    {
        let mut installed = INSTALLED.lock().unwrap();
        if *installed == state {
            return;
        }
        *installed = state;
    }
    FONTS_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut defs = FontDefinitions::default();
    let egui_default = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    defs.font_data.insert("lucide".into(), Arc::new(FontData::from_static(LUCIDE).tweak(egui::FontTweak { y_offset_factor: 0.0, ..Default::default() })));
    defs.families.insert(FontFamily::Name("icons".into()), vec!["lucide".to_owned()]);

    // Mapped once for the life of the process, however often fonts are installed:
    // the fallback list at each weight.
    type Fallbacks = [Vec<(String, Face)>; 3];
    // Medium and Light use the fallbacks' regular faces.
    let fallback_w = |w: usize| if w > 2 { 0 } else { w };
    static SYSTEM: std::sync::OnceLock<Fallbacks> = std::sync::OnceLock::new();
    let system = SYSTEM.get_or_init(|| {
        std::array::from_fn(|w| {
            CANDIDATES
                .iter()
                .filter_map(|&(path, index)| {
                    let heavy = (w > 0).then(|| HEAVIER.iter().find(|(base, _)| *base == path)).flatten().map(|(_, v)| v[w - 1]);
                    let mapped = |(p, i): (&str, u32)| map(p).map(|bytes| (format!("sys:{p}#{i}"), Face { bytes, index: i, wght: None, opsz: None }));
                    heavy.and_then(mapped).or_else(|| mapped((path, index)))
                })
                .collect()
        })
    });
    // Primary fonts: Inter, each lyric font, and the installed fonts in use, these with
    // their own 800.
    let mut groups: Vec<(&str, [Face; 5], Option<Face>)> = vec![("inter", INTER.map(Face::of), None)];
    groups.extend(LYRIC_FONTS.iter().map(|(k, w)| (*k, w.map(Face::of), None)));
    groups.extend(CHOICE.read().unwrap().as_ref().map(|c| ("choice", c.faces, Some(c.extra_bold))));
    groups.extend(EXTRAS.read().unwrap().iter().zip(EXTRA_KEYS).map(|(c, key)| (key, c.faces, Some(c.extra_bold))));
    for (key, faces, _) in &groups {
        let (asc_p, h_p) = em_metrics(faces[0].bytes, faces[0].index).unwrap_or((0.97, 1.21));
        for (w, face) in faces.iter().enumerate() {
            let name = format!("{key}-{w}");
            defs.font_data.insert(name.clone(), Arc::new(font_data(*face, 0.0)));
            let mut chain = vec![name.clone()];
            let inter = (*key != "inter").then(|| ("inter".to_owned(), Face::of(INTER[w])));
            for (fallback, f) in inter.iter().chain(&system[fallback_w(w)]) {
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
                    2 => FontFamily::Name("bold".into()),
                    3 => FontFamily::Name("medium".into()),
                    _ => FontFamily::Name("light".into()),
                };
                defs.families.insert(family, chain);
            }
        }
    }
    // 800: a bundled font's bold chain under another name, so nothing more is loaded; only
    // an installed font has a face of its own for it, on the same fallbacks.
    for (key, _, extra_bold) in &groups {
        let mut chain = defs.families[&FontFamily::Name(format!("{key}-2").into())].clone();
        if let Some(face) = extra_bold {
            defs.font_data.insert(format!("{key}-5"), Arc::new(font_data(*face, 0.0)));
            chain[0] = format!("{key}-5");
        }
        if *key == ui_key {
            defs.families.insert(FontFamily::Name("extrabold".into()), chain.clone());
        }
        defs.families.insert(FontFamily::Name(format!("{key}-5").into()), chain);
    }
    // With no installed font there, its families are Inter's, so they always exist
    // without a second copy of Inter and its fallbacks.
    for slot in std::iter::once("choice").chain(EXTRA_KEYS) {
        if groups.iter().any(|(key, ..)| *key == slot) {
            continue;
        }
        for w in 0..6 {
            let inter = defs.families[&FontFamily::Name(format!("inter-{w}").into())].clone();
            defs.families.insert(FontFamily::Name(format!("{slot}-{w}").into()), inter);
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
    TEXT_OVER.store(crate::preset::hex(&p.app.text_color).map_or(0, |c| u32::from_le_bytes(c.to_array())), std::sync::atomic::Ordering::Relaxed);
    FADE.store(p.app.ui_fade_secs.map_or(1.0, |s| (s / 0.18).clamp(0.3, 3.0)).to_bits(), std::sync::atomic::Ordering::Relaxed);
    install_style(ctx);
    // Romanization and translation look the same in every theme: the app's own fonts.
    let deco = &crate::preset::get(crate::preset::DEFAULT_ID).lyrics;
    install_fonts(ctx, &p.app.font, &[&p.lyrics.font, &deco.romanization_font, &deco.translation_font]);
}

pub fn install_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(14.0, 7.0);
        s.spacing.menu_margin = egui::Margin::same(MENU_INSET);
        s.spacing.scroll = egui::style::ScrollStyle::floating();
        if crate::preset::active().app.hide_scrollbars {
            let bar = &mut s.spacing.scroll;
            (bar.dormant_handle_opacity, bar.active_handle_opacity, bar.interact_handle_opacity) = (0.0, 0.0, 0.0);
            (bar.dormant_background_opacity, bar.active_background_opacity, bar.interact_background_opacity) = (0.0, 0.0, 0.0);
        }
        if let Some([w, a]) = crate::preset::active().app.scrollbar {
            let bar = &mut s.spacing.scroll;
            bar.bar_width = w;
            (bar.dormant_handle_opacity, bar.active_handle_opacity, bar.interact_handle_opacity) = (a * 0.6, a, (a * 2.0).min(1.0));
        }

        s.text_styles.insert(egui::TextStyle::Body, regular(14.0));
        s.text_styles.insert(egui::TextStyle::Button, semibold(14.0));
        s.text_styles.insert(egui::TextStyle::Heading, bold(22.0));
        s.text_styles.insert(egui::TextStyle::Small, regular(12.0));
        s.visuals = visuals();
        s.interaction.selectable_labels = false;
        s.animation_time = 0.18 * SPEED * fade();
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
    v.menu_corner_radius = p.app.menu_radius.map_or(pr, |r| egui::CornerRadius::same(r.clamp(0.0, 32.0) as u8));
    v.window_stroke = egui::Stroke::new(1.0, if p.app.menu_border > 0.0 { with_alpha(if light() { Color32::BLACK } else { Color32::WHITE }, p.app.menu_border) } else { glass(0.08) });
    v.popup_shadow = egui::epaint::Shadow { offset: [0, 8], blur: 28, spread: 0, color: shade(if light() { 0.16 } else { 0.5 }) };
    if let Some([y, blur, a]) = p.app.menu_look.shadow {
        v.popup_shadow = egui::epaint::Shadow { offset: [0, y as i8], blur: blur.min(255.0) as u8, spread: 0, color: shade(a) };
    }
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
    if let Some(c) = crate::preset::hex(&p.app.scrollbar_color).filter(|_| !light()) {
        // Scrollbar handles take these.
        v.widgets.inactive.fg_stroke.color = c;
        v.widgets.hovered.fg_stroke.color = c;
        v.widgets.active.fg_stroke.color = c;
    }
    if let Some(i) = p.app.input {
        v.extreme_bg_color = glass(i.fill);
        v.text_edit_bg_color = Some(glass(i.fill));
        v.widgets.inactive.corner_radius = egui::CornerRadius::same(i.radius.clamp(0.0, 28.0) as u8);
        if i.focus_px > 0.0 {
            v.selection.stroke = egui::Stroke::new(i.focus_px, i.focus_white.filter(|_| !light()).map_or(p.accent(RED), |a| with_alpha(Color32::WHITE, a)));
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_on_accents_take_the_better_contrast() {
        let dark = Color32::from_gray(15);
        assert_eq!(on_fill(Color32::from_rgb(0xff, 0xb0, 0x00)), dark);
        assert_eq!(on_fill(Color32::from_rgb(0x1d, 0xb9, 0x54)), dark);
        assert_eq!(on_fill(Color32::from_rgb(0x20, 0x20, 0x60)), Color32::WHITE);
    }

    #[test]
    fn deepening_reaches_the_contrast_and_leaves_what_has_it() {
        let cyan = Color32::from_rgb(0x67, 0xd9, 0xff);
        let deep = deepen(cyan, BG_LIGHT, 3.0);
        assert!(contrast(deep, BG_LIGHT) >= 3.0);
        // No darker than it takes.
        assert!(contrast(deep, BG_LIGHT) < 3.3);
        let dark = Color32::from_rgb(0x20, 0x20, 0x60);
        assert_eq!(deepen(dark, BG_LIGHT, 3.0), dark);
    }

    fn fixed(weight: f32) -> SysFace {
        SysFace { path: std::path::PathBuf::new(), index: 0, weight, wght: None, opsz: None }
    }

    #[test]
    fn extra_bold_takes_the_nearest_face_to_800() {
        let family = SysFamily { name: "Test".into(), faces: vec![fixed(400.0), fixed(700.0), fixed(800.0), fixed(900.0)] };
        assert_eq!(closest_face(&family, 800.0).unwrap().weight, 800.0);
        let lighter = SysFamily { name: "Test".into(), faces: vec![fixed(400.0), fixed(700.0)] };
        assert_eq!(closest_face(&lighter, 800.0).unwrap().weight, 700.0);
        // A variable font that reaches it is as close as it gets.
        let variable = SysFamily { name: "Test".into(), faces: vec![SysFace { wght: Some((100.0, 900.0)), ..fixed(400.0) }] };
        assert_eq!(closest_face(&variable, 800.0).unwrap().wght, Some((100.0, 900.0)));
    }
}
