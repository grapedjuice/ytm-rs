//! Theme presets: native translations of the Better Lyrics community themes
//! (github.com/better-lyrics/themes). Those are CSS for the YouTube Music website,
//! which this app can't run, so each one is reduced to the knobs this renderer has:
//! the background shader, lyric styling, cover treatment and chrome. The data lives
//! in `assets/themes/presets.json`; covers in `assets/themes/covers/<id>.jpg`.

use std::sync::OnceLock;

use egui::Color32;
use serde::Deserialize;

/// Id of the built-in look (the app's own design, not a Better Lyrics theme).
pub const DEFAULT_ID: &str = "ytm-rs";

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BgMode {
    /// Animated warped, blurred cover.
    #[default]
    Kawarp,
    /// Blurred cover, no motion.
    CoverStatic,
    /// Flat dark with a soft gradient in the cover's colour.
    AccentGradient,
    /// One colour.
    Solid,
    /// Now playing: the cover itself fills the screen behind a scrim.
    CoverFullbleed,
}

#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct Background {
    pub mode: BgMode,
    pub solid: String,
    /// The shader values are for now playing; pages use a calmer version.
    pub intensity: f32,
    pub saturation: f32,
    pub speed: f32,
    pub scale: f32,
    pub np_dim: f32,
    pub page_dim: f32,
    pub reactive: f32,
    pub scrim: f32,
}

impl Default for Background {
    fn default() -> Self {
        Self {
            mode: BgMode::Kawarp,
            solid: "#0c0a0e".into(),
            intensity: 1.0,
            saturation: 1.5,
            speed: 1.0,
            scale: 1.2,
            np_dim: 0.85,
            page_dim: 0.5,
            reactive: 1.0,
            scrim: 0.3,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    #[default]
    Left,
    Center,
}

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Weight {
    Regular,
    Semibold,
    #[default]
    Bold,
}

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fill {
    /// Soft per-character karaoke wipe.
    #[default]
    Sweep,
    /// Each word lights fully when it starts.
    Word,
    /// The whole active line is lit at once.
    Line,
}

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlurScope {
    #[default]
    All,
    /// Only lines already sung.
    Past,
    /// Only lines still to come.
    Upcoming,
}

#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct LyricStyle {
    pub align: Align,
    pub weight: Weight,
    pub font: String,
    pub size: f32,
    pub line_gap: f32,
    pub anchor: f32,
    pub fill: Fill,
    pub active_alpha: f32,
    pub inactive_alpha: f32,
    pub falloff: f32,
    pub min_alpha: f32,
    /// Alpha for lines already sung (0 = they vanish); unset = same as upcoming.
    pub past_alpha: Option<f32>,
    pub sung_color: String,
    pub blur: f32,
    pub blur_scope: BlurScope,
    pub glow: f32,
    pub glow_color: String,
    pub active_scale: f32,
    pub lift: f32,
    /// Values ported straight from the theme's stylesheet; when present they replace
    /// the approximations above.
    pub css: Option<ThemeCss>,
}

/// A Better Lyrics theme's own CSS, for the parts of it the lyrics renderer reads.
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct ThemeCss {
    /// `--blyrics-font-size` in rem by window width: `[[min_width_px, rem], ...]`.
    pub font_rem: Vec<[f32; 2]>,
    /// `--blyrics-font-weight` (400 / 600 / 700).
    pub font_weight: Option<u16>,
    /// Line opacity and blur (px) before the active line (`div:not(.blyrics--active):not(.blyrics--active ~ div)`).
    pub past: Option<[f32; 2]>,
    /// … and after it (`.blyrics--active ~ div`).
    pub upcoming: Option<[f32; 2]>,
    /// `transition: opacity calc(var(--blyrics-duration) / n) delay`: `[1/n, delay s]`.
    pub opacity_transition: Option<[f32; 2]>,
    /// `transition: filter …`: `[1/n, delay s]`.
    pub filter_transition: Option<[f32; 2]>,
    /// Alpha of `--blyrics-glow-color` on the container and on held words (`[data-long-word]`).
    pub glow_alpha: Option<f32>,
    pub long_word_glow_alpha: Option<f32>,
    /// Wobble transforms set to identity.
    pub no_wobble: bool,
    /// `--blyrics-letter-wave-*`: crest (em), settle (em), duration (s), emphasis scale.
    pub letter_wave: Option<[f32; 4]>,
    /// `.blyrics-background-lyric { font-size }` in em.
    pub background_em: Option<f32>,
    /// `--blyrics-hover-scale`.
    pub hover_scale: Option<f32>,
    /// `.blyrics-container { padding-inline: start end }` in px.
    pub padding_inline: Option<[f32; 2]>,
}

impl Default for LyricStyle {
    fn default() -> Self {
        Self {
            align: Align::Left,
            weight: Weight::Bold,
            font: "inter".into(),
            size: 1.0,
            line_gap: 0.6,
            anchor: 0.37,
            fill: Fill::Sweep,
            active_alpha: 0.3,
            inactive_alpha: 0.3,
            falloff: 0.0,
            min_alpha: 0.3,
            past_alpha: None,
            sung_color: "white".into(),
            blur: 0.0,
            blur_scope: BlurScope::All,
            glow: 0.0,
            glow_color: "white".into(),
            active_scale: 1.0,
            lift: 0.0,
            css: None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    #[default]
    Split,
    LyricsOnly,
    CoverHero,
}

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    #[default]
    Left,
    Right,
}

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    #[default]
    Rounded,
    Circle,
    /// A disc that spins slowly while playing.
    Record,
}

#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct CoverStyle {
    pub layout: Layout,
    pub side: Side,
    pub shape: Shape,
    pub radius: f32,
    pub spin_secs: f32,
    pub brightness: f32,
    pub size: f32,
    pub shadow: f32,
    pub glow: f32,
}

impl Default for CoverStyle {
    fn default() -> Self {
        Self { layout: Layout::Split, side: Side::Left, shape: Shape::Rounded, radius: 12.0, spin_secs: 60.0, brightness: 1.0, size: 1.0, shadow: 0.55, glow: 0.0 }
    }
}

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerBar {
    #[default]
    Docked,
    Floating,
}

#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct Chrome {
    pub glass: f32,
    pub radius: f32,
    pub player_bar: PlayerBar,
}

impl Default for Chrome {
    fn default() -> Self {
        Self { glass: 1.0, radius: 18.0, player_bar: PlayerBar::Docked }
    }
}

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageBg {
    /// Whatever the background mode shows in now playing.
    #[default]
    Same,
    /// Blurred cover, no motion.
    Static,
    /// Dark with a soft gradient in the cover's colour.
    Accent,
    /// The preset's solid colour.
    Solid,
}

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidebarStyle {
    #[default]
    Glass,
    Transparent,
    Solid,
    /// A rounded glass card inset from the window edges.
    Floating,
}

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NavStyle {
    /// Light translucent pill.
    #[default]
    Pill,
    /// Pill tinted with the accent, accent icon.
    Accent,
    /// Accent bar at the left edge, no fill.
    Bar,
}

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChipStyle {
    #[default]
    Pill,
    Outline,
    Accent,
}

/// How the rest of the app (outside now playing) looks.
#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct AppStyle {
    pub font: String,
    pub page_bg: PageBg,
    pub page_dim: Option<f32>,
    pub sidebar: SidebarStyle,
    pub sidebar_alpha: f32,
    pub bar_alpha: f32,
    pub bar_border: bool,
    pub nav: NavStyle,
    pub chip: ChipStyle,
    pub card_radius: f32,
    pub thumb_radius: f32,
    pub panel_radius: f32,
    pub hover: f32,
    pub text_dim: String,
}

impl Default for AppStyle {
    fn default() -> Self {
        Self {
            font: "inter".into(),
            page_bg: PageBg::Same,
            page_dim: None,
            sidebar: SidebarStyle::Glass,
            sidebar_alpha: 0.28,
            bar_alpha: 0.55,
            bar_border: false,
            nav: NavStyle::Pill,
            chip: ChipStyle::Pill,
            card_radius: 8.0,
            thumb_radius: 5.0,
            panel_radius: 14.0,
            hover: 1.0,
            text_dim: "#b2acba".into(),
        }
    }
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub creators: Vec<String>,
    pub blurb: String,
    pub background: Background,
    /// "cover" (sampled from the art), "system" (the Windows accent colour, else the
    /// cover's), or a fixed "#rrggbb".
    pub accent: String,
    pub lyrics: LyricStyle,
    pub cover: CoverStyle,
    pub chrome: Chrome,
    pub app: AppStyle,
}

impl Preset {
    fn builtin() -> Self {
        Self {
            id: DEFAULT_ID.into(),
            name: "ytm-rs".into(),
            blurb: "The app's own look: animated album-art background, left-aligned karaoke lyrics.".into(),
            accent: "cover".into(),
            ..Default::default()
        }
    }

    /// The accent to use given the one sampled from the current cover.
    pub fn accent(&self, from_cover: Color32) -> Color32 {
        if self.accent == "system" {
            return crate::theme::system_accent().unwrap_or(from_cover);
        }
        hex(&self.accent).unwrap_or(from_cover)
    }

    pub fn solid(&self) -> Color32 {
        hex(&self.background.solid).unwrap_or(crate::theme::bg())
    }
}

/// Resolve a colour name from the presets: "white", "accent"/"cover", or "#rrggbb".
pub fn paint(name: &str, accent: Color32) -> Color32 {
    match name {
        "accent" | "cover" => accent,
        "white" | "" => Color32::WHITE,
        s => hex(s).unwrap_or(Color32::WHITE),
    }
}

/// "#rrggbb" as a colour.
pub fn hex(s: &str) -> Option<Color32> {
    let s = s.strip_prefix('#')?;
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// Every preset, the built-in look first.
pub fn all() -> &'static [Preset] {
    static ALL: OnceLock<Vec<Preset>> = OnceLock::new();
    ALL.get_or_init(|| {
        let mut v = vec![Preset::builtin()];
        match serde_json::from_str::<Vec<Preset>>(include_str!("../assets/themes/presets.json")) {
            Ok(list) => v.extend(list),
            Err(e) => log::error!("theme presets: {e}"),
        }
        v
    })
}

pub fn get(id: &str) -> &'static Preset {
    all().iter().find(|p| p.id == id).unwrap_or(&all()[0])
}

static ACTIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Make `id` the preset that widgets without access to the app read from.
pub fn set_active(id: &str) {
    let i = all().iter().position(|p| p.id == id).unwrap_or(0);
    ACTIVE.store(i, std::sync::atomic::Ordering::Relaxed);
}

pub fn active() -> &'static Preset {
    &all()[ACTIVE.load(std::sync::atomic::Ordering::Relaxed).min(all().len() - 1)]
}

/// The preset's cover thumbnail (JPEG), if it has one.
pub fn cover(id: &str) -> Option<&'static [u8]> {
    const COVERS: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/theme_covers.rs"));
    COVERS.iter().find(|(k, _)| *k == id).map(|(_, b)| *b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every bundled preset parses, has a cover thumbnail and a unique id.
    #[test]
    fn presets_are_complete() {
        let list: Vec<Preset> = serde_json::from_str(include_str!("../assets/themes/presets.json")).expect("presets.json parses");
        assert_eq!(list.len(), 27);
        let mut ids = std::collections::HashSet::new();
        for p in &list {
            assert!(ids.insert(p.id.clone()), "duplicate id {}", p.id);
            assert!(!p.name.is_empty() && !p.creators.is_empty() && !p.blurb.is_empty(), "{} lacks name/creators/blurb", p.id);
            assert!(cover(&p.id).is_some(), "{} has no cover", p.id);
            assert!(matches!(p.accent.as_str(), "cover" | "system" | "") || hex(&p.accent).is_some(), "{} accent {}", p.id, p.accent);
        }
        assert_eq!(all().len(), list.len() + 1);
        assert_eq!(get("nope").id, DEFAULT_ID);
    }
}
