//! Themes. Each is one file in `assets/themes/presets/<id>.json`, a complete description of
//! the app's look over a base of stock YouTube Music and Better Lyrics: a field a theme leaves
//! out looks stock, never like another theme. Better Lyrics community themes
//! (github.com/better-lyrics/themes) are ported from their stylesheets, in YouTube Music's
//! units (1 rem = 10 px). No code may branch on a theme's id: everything a theme can do is a
//! field here, available to all of them. Covers live in `assets/themes/covers/<id>.jpg`.

use std::sync::OnceLock;

use egui::Color32;
use serde::Deserialize;

/// Id of the app's own look, used when a saved theme no longer exists.
pub const DEFAULT_ID: &str = "ytm-rs";

// ------------------------------------------------------------------ background

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BgMode {
    /// Animated warped, blurred cover (`shaderType: kawarp`).
    Kawarp,
    /// Blurred cover, no motion: stock Better Lyrics' now playing, at a quarter brightness.
    #[default]
    CoverStatic,
    /// Flat dark with a soft gradient in the cover's colour.
    AccentGradient,
    /// One colour.
    Solid,
    /// Now playing: the cover itself fills the screen behind a scrim.
    CoverFullbleed,
}

/// A second, unwarped blurred cover under everything, as themes draw with `::before`.
#[derive(Clone, Copy, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CoverLayer {
    pub opacity: f32,
    /// Seconds per scale pulse (0: none) and how much it swells.
    pub pulse_secs: f32,
    pub pulse: f32,
    /// Seconds per full turn. 0: it doesn't turn.
    pub rotate_secs: f32,
    /// Its own brightness and saturation, in place of the background's.
    pub brightness: Option<f32>,
    pub saturate: Option<f32>,
}

/// A soft ellipse of colour, placed in fractions of the view.
#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Overlay {
    pub color: String,
    pub alpha: f32,
    pub x: f32,
    pub y: f32,
    pub rx: f32,
    pub ry: f32,
}

impl Default for Overlay {
    fn default() -> Self {
        Self { color: "white".into(), alpha: 0.1, x: 0.5, y: 0.5, rx: 0.5, ry: 0.5 }
    }
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Background {
    pub mode: BgMode,
    pub solid: String,
    /// `solid` in light mode; left empty, derived from `solid`.
    pub solid_light: String,
    /// The shader values are for now playing; pages use a calmer version.
    pub intensity: f32,
    pub saturation: f32,
    pub speed: f32,
    pub scale: f32,
    pub np_dim: f32,
    pub page_dim: f32,
    pub reactive: f32,
    /// `audioSpeedMultiplier`: how much louder music speeds the warp up. 0: not at all.
    pub audio_speed: f32,
    /// Darkening over now playing's background (0.3 was the app's old default).
    pub scrim: f32,
    pub layer: Option<CoverLayer>,
    /// A gradient over now playing in place of the flat scrim: `[start alpha, end alpha]`, in black.
    pub scrim_gradient: Option<[f32; 2]>,
    /// It runs left to right rather than top to bottom.
    pub scrim_across: bool,
    /// Soft coloured light over now playing's background (`radial-gradient`s).
    pub overlays: Vec<Overlay>,
    /// How long new art takes to fade in.
    pub crossfade_secs: f32,
    /// How long new art waits before that fade starts.
    pub crossfade_delay: f32,
    /// Audio below this level doesn't move the background (`audioBeatThreshold`).
    pub beat_threshold: f32,
    /// Bright art is darkened by up to this much (`autoDimBrightArtwork`).
    pub auto_dim: f32,
    /// Noise that hides banding, in fractions of full brightness (`dithering`).
    pub dither: f32,
    /// How many times the art is blurred (`blurPasses`). The app's own art is about 6.
    pub blur_passes: u32,
    /// While a video plays, the background follows the picture (`videoEnabled`).
    pub video_backdrop: bool,
    /// `lyrics_only` over a fullbleed cover: the fade across the cover's square, as
    /// fractions of its width where it starts and where it's gone.
    pub fade: [f32; 2],
    /// Now playing's contrast.
    pub np_contrast: f32,
    /// A fullbleed cover crossfades to the next by `crossfade_secs`, after `crossfade_delay`.
    pub fullbleed_fade: bool,
    /// The background's own values while a video drives it, where they differ from the art's.
    pub video: VideoBackdrop,
}

/// What `video_backdrop` draws with, each falling back to the same setting for the art.
#[derive(Clone, Copy, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VideoBackdrop {
    /// `videoSaturation`.
    pub saturation: Option<f32>,
    /// `videoDithering`.
    pub dither: Option<f32>,
    /// `videoDimStrength`: bright pictures are darkened by up to this much.
    pub auto_dim: Option<f32>,
    /// `videoBlurPasses`.
    pub blur_passes: Option<u32>,
}

impl Default for Background {
    fn default() -> Self {
        Self {
            mode: BgMode::CoverStatic,
            solid: "#030303".into(),
            solid_light: String::new(),
            intensity: 1.0,
            saturation: 1.5,
            speed: 1.0,
            scale: 1.2,
            np_dim: 0.25,
            page_dim: 0.5,
            reactive: 1.0,
            audio_speed: 0.0,
            scrim: 0.0,
            layer: None,
            scrim_gradient: None,
            scrim_across: false,
            overlays: Vec::new(),
            crossfade_secs: 1.4,
            crossfade_delay: 0.0,
            beat_threshold: 0.0,
            auto_dim: 0.0,
            dither: 0.006,
            blur_passes: 6,
            video_backdrop: false,
            fade: [0.4, 1.0],
            np_contrast: 1.0,
            fullbleed_fade: false,
            video: VideoBackdrop::default(),
        }
    }
}

// ------------------------------------------------------------------ lyrics

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// Where other singers' lines go.
#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAlign {
    /// v2 and v3 on the right, v1000 (everyone) in the middle.
    #[default]
    Stock,
    /// Everyone on `align`.
    Same,
    /// v1 on the left, everyone else on the right.
    OnlyV1Left,
}

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Weight {
    Regular,
    Semibold,
    #[default]
    Bold,
    /// 500: halfway between Regular and Semibold.
    Medium,
    /// 250. Latin in the interface's own face; other faces draw it as Regular.
    Light,
    /// Weight 800. Inter's and Figtree's bold already is; an installed font gets its own instance,
    /// and the other bundled fonts draw their bold, the heaviest they have.
    ExtraBold,
}

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fill {
    /// Soft per-character karaoke wipe on word-timed lyrics. Line-timed ones fade in whole.
    #[default]
    Sweep,
    /// Each word lights fully when it starts.
    Word,
    /// The whole line lights at once, word timing or not (richsync off).
    Line,
}

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Instrumental {
    /// Better Lyrics' music note filling with a wave.
    #[default]
    Note,
    /// Apple Music's three dots filling in turn.
    Dots,
    /// A ring that closes as the break passes.
    Ring,
    /// A note glyph, pulsing.
    Glyph,
    /// Nothing: the break is empty.
    Hidden,
}

/// A transition: `frac` of the line's length plus `secs`, after `delay`.
#[derive(Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Timing {
    pub frac: f32,
    pub secs: f32,
    pub delay: f32,
}

impl Default for Timing {
    fn default() -> Self {
        Self { frac: 0.0, secs: 0.2, delay: 0.0 }
    }
}

/// A word's transient bump while it's sung (`@keyframes` on `.blyrics--word`).
#[derive(Clone, Copy, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WordBounce {
    /// Peak lift in em.
    pub lift: f32,
    /// Peak scale.
    pub scale: f32,
    pub secs: f32,
    /// Where it rests afterwards, in em above the line.
    pub settle: f32,
    /// Overshoots like a spring rather than easing.
    pub spring: bool,
    /// Longer on longer words: this much of the word's length, on top of `secs`.
    pub dur_ratio: f32,
    /// A sideways shift at its peak, em.
    pub side_em: f32,
    /// The word's lit opacity as it starts. 0: full from the start.
    pub from_alpha: f32,
}

/// `--blyrics-letter-wave-*`.
#[derive(Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LetterWave {
    pub on: bool,
    /// Crest and settle in em.
    pub crest: f32,
    pub settle: f32,
    pub secs: f32,
    /// How much held words' letters swell.
    pub emphasis: f32,
    /// A word held this long swells by `emphasis`.
    pub long_secs: f32,
    /// Only words held at least this long, and of at most this many letters (0: any), wave.
    pub min_secs: f32,
    pub max_letters: usize,
    /// Letters' scale before they're sung.
    pub rest_scale: f32,
    /// The glow on waving words, rem, in place of `glow_rem`.
    pub glow_rem: f32,
}

impl Default for LetterWave {
    fn default() -> Self {
        Self { on: true, crest: 0.05, settle: 0.02, secs: 0.9, emphasis: 1.11, long_secs: 1.5, min_secs: 0.0, max_letters: 0, rest_scale: 1.0, glow_rem: 0.0 }
    }
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LyricStyle {
    pub align: Align,
    pub agent_align: AgentAlign,
    pub weight: Weight,
    pub font: String,
    /// Points per rem. Always YouTube Music's 10 unless a theme is the app's own design.
    pub rem: f32,
    /// `--blyrics-font-size` in rem by window width: `[[min_width_px, rem], ...]`.
    pub font_rem: Vec<[f32; 2]>,
    /// `clamp(min rem, vw %, max rem)`; replaces `font_rem` when set.
    pub font_vw: Option<[f32; 3]>,
    /// Lyrics this much bigger in full screen.
    pub fullscreen_scale: f32,
    pub line_height: f32,
    pub letter_spacing_em: f32,
    /// Space above and below each line, rem.
    pub line_padding_rem: f32,
    /// `.blyrics-container { padding-inline: start end }`, px.
    pub padding_inline: [f32; 2],
    /// Where the active line sits, a fraction down the view.
    pub anchor: f32,
    pub fill: Fill,
    /// `--blyrics-lyric-inactive-color`'s alpha: unsung words, before their line's opacity.
    pub dim_alpha: f32,
    pub sung_color: String,
    /// Colours per singer, v1 first; replaces `sung_color` for those who have one.
    pub agent_colors: Vec<String>,
    /// Unsung words in their singer's colour, not white.
    pub dim_from_agent: bool,
    /// Lines already sung, `[opacity, blur px]` by distance from the active line. The last
    /// entry goes on for the rest. Empty: `[1, 0]`.
    pub past: Vec<[f32; 2]>,
    /// … and lines still to come.
    pub upcoming: Vec<[f32; 2]>,
    /// The same two for line-timed lyrics, when a theme styles them differently.
    pub past_line_synced: Vec<[f32; 2]>,
    pub upcoming_line_synced: Vec<[f32; 2]>,
    /// Past lines' text alpha and colour in place of `dim_alpha` and white.
    pub past_dim_alpha: Option<f32>,
    pub past_color: String,
    pub opacity_transition: Timing,
    pub blur_transition: Timing,
    /// The same two for lines turning past and turning active, when they differ.
    /// Curve of line transitions, `cubic-bezier(a, b, c, d)`.
    pub line_ease: Option<[f32; 4]>,
    pub past_opacity_transition: Option<Timing>,
    pub past_blur_transition: Option<Timing>,
    pub active_opacity_transition: Option<Timing>,
    pub active_blur_transition: Option<Timing>,
    /// The next line goes sharp and full this long before it's sung (`.blyrics--pre-animating`).
    pub pre_sharp_secs: f32,
    /// The sung line's opacity, word-timed and line-timed.
    pub active_opacity: f32,
    pub active_opacity_line_synced: Option<f32>,
    /// Line opacity while the reader scrolls.
    pub scroll_opacity: f32,
    /// Line scale while the reader scrolls.
    pub scroll_scale: Option<f32>,
    /// A hovered line's opacity; its blur stays unless `hover_reveal` is on.
    pub hover_opacity: Option<f32>,
    /// A box behind the hovered line: `[white alpha, radius px]`.
    pub hover_box: Option<[f32; 2]>,
    /// Text alpha while the reader scrolls or hovers a line, for themes whose unsung text is hidden.
    pub reveal_dim_alpha: Option<f32>,
    /// How long a line-timed line takes to fade in.
    pub line_fade_secs: f32,
    /// Hovering a line brings it to full opacity with no blur.
    pub hover_reveal: bool,
    /// `--blyrics-hover-scale`.
    pub hover_scale: Option<f32>,
    /// `--blyrics-scale` and `--blyrics-active-scale`.
    pub rest_scale: f32,
    pub active_scale: f32,
    pub scale_secs: f32,
    /// Curves of the line growing and shrinking, `cubic-bezier(a, b, c, d)`.
    pub scale_ease: Option<[f32; 4]>,
    pub scale_exit_ease: Option<[f32; 4]>,
    /// A line becoming active slides in: `[rem from the side, secs]`. Other singers' from the other side.
    pub line_enter: Option<[f32; 2]>,
    pub letter_wave: LetterWave,
    pub wobble: bool,
    pub wobble_secs: f32,
    pub word_bounce: Option<WordBounce>,
    /// Unsung words rest this many px low and glide up as they're sung, over `word_rise_secs`.
    pub word_rise_px: f32,
    pub word_rise_secs: f32,
    /// Unsung words' scale, growing to full as they're sung.
    pub word_rest_scale: f32,
    /// Extra space between words, em.
    pub word_gap_em: f32,
    /// A soft shadow under words: `[px down, alpha]`.
    pub word_shadow: Option<[f32; 2]>,
    /// The sung colour's alpha.
    pub active_alpha: f32,
    /// Sweep timing: seconds it starts early, and its length against the word's.
    pub sweep_lead: f32,
    pub sweep_ratio: f32,
    /// Glow timing: its length against the word's, and its shortest, seconds.
    pub glow_ratio: f32,
    pub glow_min_secs: f32,
    /// Sung words settle to this alpha after this many seconds: `[alpha, secs]`.
    pub sung_fade: Option<[f32; 2]>,
    pub glow_color: String,
    /// `--blyrics-glow-color`'s alpha on words, and on held words.
    pub glow_alpha: f32,
    pub long_glow_alpha: f32,
    pub glow_rem: f32,
    /// A coloured band at the sweep's edge: stops from the edge back, band width in words.
    pub sweep_band: Vec<String>,
    pub sweep_band_width: f32,
    /// Outline of sung text: `[width em]` in `stroke_color`.
    pub stroke_em: f32,
    pub stroke_color: String,
    /// `.blyrics-background-lyric` font size (em).
    pub background_em: f32,
    /// Background vocals' opacity against the line, and their weight.
    pub background_alpha: f32,
    pub background_weight: Weight,
    pub romanization_rem: f32,
    pub romanization_alpha: f32,
    pub romanization_pill: bool,
    pub romanization_weight: Weight,
    pub romanization_font: String,
    pub translation_rem: f32,
    pub translation_alpha: f32,
    pub translation_weight: Weight,
    pub translation_font: String,
    /// Sizes when a line has both, rem: `[romanization, translation]`.
    pub both_rem: Option<[f32; 2]>,
    pub instrumental: Instrumental,
    /// The break's icon, rem across.
    pub instrumental_rem: Option<f32>,
    /// It breathes: `[smallest, largest, secs]`.
    pub instrumental_breathe: Option<[f32; 3]>,
    /// In a glass chip of this white alpha.
    pub instrumental_chip: Option<f32>,
    /// Credits' alpha: `[at rest, focused]`. Below zero: hidden.
    pub credits_alpha: [f32; 2],
    /// Lines fade out at the view's edges, in fractions of it: `[top, bottom]`.
    pub edge_fade: [f32; 2],
    /// Wrapped lines come out about even (`text-wrap: balance`).
    pub balance: bool,
    /// `font_vw` by window width, `[below px, min rem, vw, max rem]`, narrowest first.
    pub font_vw_steps: Vec<[f32; 4]>,
    /// Space above and below lines in full screen, rem.
    pub fullscreen_padding_rem: Option<f32>,
    /// The light behind the active line: `[alpha, px it reaches past the line]`.
    pub aura: Option<[f32; 2]>,
    /// A short bar beside the active line: `[width, height, alpha, px left of it]`.
    pub marker_px: Option<[f32; 4]>,
    /// Sung lines slide away: `[distance rem (negative: left), secs]`.
    pub line_exit: Option<[f32; 2]>,
    /// Romanization, translation and credits: letter spacing, em.
    pub decor_tracking_em: f32,
    /// Lit words fading back when their line ends: `[secs, delay]`.
    pub exit_fade: Option<[f32; 2]>,
    /// A glow colour per singer, v1 first.
    pub agent_glow: Vec<String>,
    /// Colours of the aura, side by side along the line, in place of the sung colour.
    pub aura_colors: Vec<String>,
    /// Colours down the marker bar, in place of the sung colour.
    pub marker_colors: Vec<String>,
    /// A glow around the whole sung line (`text-shadow`): `[px, alpha]`, in these colours, wider each.
    pub active_glow: Option<[f32; 2]>,
    pub active_glow_colors: Vec<String>,
    /// The same on a hovered line.
    pub hover_glow: Option<[f32; 2]>,
    /// Soft colour behind the lyrics' column.
    pub column_overlays: Vec<Overlay>,
    /// Colour in lines not being sung, against full (`filter: saturate`).
    pub rest_saturate: f32,
    /// How long lines above and below the active one take to scroll, against the usual.
    pub scroll_sides: [f32; 2],
    /// How long each line takes to scroll by how far it is from the sung one `i`, in ms:
    /// `[above, above log, below, below log, below step]`, as `above + ln(i + 1) * above log`
    /// and `below + ln(i + 1) * below log + i * below step`. Replaces `scroll_secs` and
    /// `scroll_sides`.
    pub scroll_curve: Option<[f32; 5]>,
    /// Line-timed lyrics: each word this much after the last, seconds.
    pub line_stagger: f32,
    /// Songs whose lyrics are nothing but an instrumental break show as having none.
    pub hide_instrumental_only: bool,
    /// Romanization and translation coming in: `[em down, em across, secs, delay]`.
    pub deco_enter: Option<[f32; 4]>,
    /// The sweep's soft edge, in words, and where its two ends finish.
    pub sweep_soft: Option<f32>,
    pub sweep_end: Option<[f32; 2]>,
    /// Unsung text's alpha on a hovered line.
    pub hover_dim_alpha: Option<f32>,
    /// The hover box: white alpha of its outline, and how it pops in `[scale, secs]`.
    pub hover_box_line: f32,
    pub hover_box_pop: Option<[f32; 2]>,
    /// The marker on a hovered line, this alpha.
    pub marker_hover: f32,
    /// The glow's curve, and it finishing over this many seconds after its line ends.
    pub glow_ease: Option<[f32; 4]>,
    pub glow_fade_secs: f32,
    /// Background vocals: em above them, and their lit alpha after they're sung.
    pub background_gap_em: f32,
    pub background_sung_alpha: Option<f32>,
    /// The word shadow's spread against the usual; none on words already lit.
    pub word_shadow_blur: f32,
    pub word_shadow_unsung: bool,
    /// Romanization and translation bigger in full screen; em between them and the line.
    pub decor_fullscreen_scale: f32,
    pub decor_gap_em: Option<f32>,
    /// `padding-inline: clamp(min px, vw %, max px)`, in place of `padding_inline`.
    pub padding_vw: Option<[f32; 3]>,
    /// `padding-block: clamp(min px, vh %, max px)` on lines, in place of `line_padding_rem`.
    pub line_padding_vh: Option<[f32; 3]>,
    /// Space above the first line, px.
    pub padding_top: f32,
    /// `padding_inline` in full screen.
    pub fullscreen_inline: Option<[f32; 2]>,
    /// `font_vw` in full screen, in place of `fullscreen_scale`.
    pub font_vw_full: Option<[f32; 3]>,
    /// Credits: in the middle, how heavy, and rem above them.
    pub credits_center: bool,
    pub credits_weight: Weight,
    pub credits_margin_rem: f32,
    /// A soft glow behind the break's glyph, white alpha.
    pub instrumental_glow: f32,
    /// Dots' `[size, gap]`, em.
    pub dot_em: Option<[f32; 2]>,
    /// The ring waves as it closes.
    pub ring_wave: bool,
    /// The Resume Autoscroll button: `[radius px, white fill, white outline]`.
    pub resume_look: [f32; 3],
    /// How long lines take to change while the reader scrolls.
    pub scroll_transition: Option<Timing>,
    /// Opacity's own time while the reader scrolls, when it differs from `scroll_transition`.
    pub scroll_opacity_transition: Option<Timing>,
    /// How much of a word's length unsung words take to rise, on top of `word_rise_secs`.
    pub word_rise_ratio: f32,
    /// Unsung words rest this many em low.
    pub word_rest_em: f32,
    /// The sung line's unsung words' alpha under the pointer.
    pub hover_unsung_alpha: Option<f32>,
    /// `hover_glow` only on the line being sung.
    pub hover_glow_sung_only: bool,
    /// Hovering the panel at all reveals every line, as scrolling does.
    pub panel_reveal: bool,
    /// Romanization and translation on the sung line, alpha.
    pub decor_active_alpha: Option<f32>,
    /// Their size as a share of the line's, in place of rem.
    pub decor_em: Option<f32>,
    /// Their line height.
    pub decor_line_height: Option<f32>,
    /// Background vocals while they're being sung, against the line's: alpha.
    pub background_lit_alpha: Option<f32>,
    /// A translation comes in this much after a romanization, seconds.
    pub translation_delay: f32,
    /// A sung line leaving goes toward its own side.
    pub line_exit_by_side: bool,
    /// A line turning active sharpens from this blur, in place of its resting one.
    pub active_blur_from: Option<f32>,
    /// The bar beside other singers' lines.
    pub marker_colors_alt: Vec<String>,
    /// `rest_saturate` four lines away and more.
    pub rest_saturate_far: Option<f32>,
    /// The resume button: height, text, black fill, shadow `[px down, blur, alpha]`, moved by `[x, y]` px.
    pub resume_height: Option<f32>,
    pub resume_text_px: Option<f32>,
    pub resume_black: bool,
    pub resume_shadow: Option<[f32; 3]>,
    pub resume_offset: [f32; 2],
    /// In full screen, the Resume Autoscroll button's own spot: `[px from the lyrics' left,
    /// fraction of their height down]`.
    pub resume_fullscreen: Option<[f32; 2]>,
    /// `--blyrics-scroll-*`.
    pub scroll_secs: f32,
    pub scroll_ease: [f32; 4],
    pub scroll_offset: f32,
    pub early_scroll: f32,
}

impl Default for LyricStyle {
    fn default() -> Self {
        Self {
            align: Align::Left,
            agent_align: AgentAlign::Stock,
            weight: Weight::Bold,
            font: "inter".into(),
            rem: 10.0,
            font_rem: vec![[0.0, 3.0]],
            font_vw: None,
            fullscreen_scale: 1.0,
            line_height: 1.333,
            letter_spacing_em: 0.0,
            line_padding_rem: 2.0,
            padding_inline: [0.0, 0.0],
            anchor: 0.37,
            fill: Fill::Sweep,
            dim_alpha: 0.3,
            sung_color: "white".into(),
            agent_colors: Vec::new(),
            dim_from_agent: false,
            past: Vec::new(),
            upcoming: Vec::new(),
            past_line_synced: Vec::new(),
            upcoming_line_synced: Vec::new(),
            past_dim_alpha: None,
            past_color: String::new(),
            opacity_transition: Timing::default(),
            blur_transition: Timing::default(),
            line_ease: None,
            past_opacity_transition: None,
            past_blur_transition: None,
            active_opacity_transition: None,
            active_blur_transition: None,
            pre_sharp_secs: 0.0,
            active_opacity: 1.0,
            active_opacity_line_synced: None,
            scroll_opacity: 1.0,
            scroll_scale: None,
            hover_opacity: None,
            hover_box: None,
            reveal_dim_alpha: None,
            line_fade_secs: 0.33,
            hover_reveal: false,
            hover_scale: None,
            rest_scale: 0.95,
            active_scale: 1.0,
            scale_secs: 0.166,
            scale_ease: None,
            scale_exit_ease: None,
            line_enter: None,
            letter_wave: LetterWave::default(),
            wobble: true,
            wobble_secs: 1.0,
            word_bounce: None,
            word_rise_px: 0.0,
            word_rise_secs: 0.4,
            word_rest_scale: 1.0,
            word_gap_em: 0.0,
            word_shadow: None,
            active_alpha: 1.0,
            sweep_lead: 0.1,
            sweep_ratio: 1.6,
            glow_ratio: 1.2,
            glow_min_secs: 1.2,
            sung_fade: None,
            glow_color: "white".into(),
            glow_alpha: 0.5,
            long_glow_alpha: 0.5,
            glow_rem: 0.8,
            sweep_band: Vec::new(),
            sweep_band_width: 0.6,
            stroke_em: 0.0,
            stroke_color: String::new(),
            background_em: 0.75,
            background_alpha: 0.78,
            background_weight: Weight::Semibold,
            romanization_rem: 1.6,
            romanization_alpha: 0.6,
            romanization_pill: true,
            romanization_weight: Weight::Semibold,
            romanization_font: String::new(),
            translation_rem: 2.0,
            translation_alpha: 0.6,
            translation_weight: Weight::Semibold,
            translation_font: String::new(),
            both_rem: None,
            instrumental: Instrumental::Note,
            instrumental_rem: None,
            instrumental_breathe: None,
            instrumental_chip: None,
            credits_alpha: [0.2, 0.85],
            edge_fade: [0.0, 0.0],
            balance: false,
            font_vw_steps: Vec::new(),
            fullscreen_padding_rem: None,
            aura: None,
            marker_px: None,
            line_exit: None,
            decor_tracking_em: 0.0,
            exit_fade: None,
            agent_glow: Vec::new(),
            aura_colors: Vec::new(),
            marker_colors: Vec::new(),
            active_glow: None,
            active_glow_colors: Vec::new(),
            hover_glow: None,
            column_overlays: Vec::new(),
            rest_saturate: 1.0,
            scroll_sides: [1.0, 1.0],
            scroll_curve: None,
            line_stagger: 0.05,
            hide_instrumental_only: false,
            deco_enter: None,
            sweep_soft: None,
            sweep_end: None,
            hover_dim_alpha: None,
            hover_box_line: 0.0,
            hover_box_pop: None,
            marker_hover: 0.0,
            glow_ease: None,
            glow_fade_secs: 0.0,
            background_gap_em: 0.1,
            background_sung_alpha: None,
            word_shadow_blur: 1.0,
            word_shadow_unsung: false,
            decor_fullscreen_scale: 1.0,
            decor_gap_em: None,
            padding_vw: None,
            line_padding_vh: None,
            padding_top: 0.0,
            fullscreen_inline: None,
            font_vw_full: None,
            credits_center: false,
            credits_weight: Weight::Regular,
            credits_margin_rem: 0.0,
            instrumental_glow: 0.0,
            dot_em: None,
            ring_wave: false,
            resume_look: [10.0, 0.10, 0.2],
            scroll_transition: None,
            scroll_opacity_transition: None,
            word_rise_ratio: 0.0,
            word_rest_em: 0.0,
            hover_unsung_alpha: None,
            hover_glow_sung_only: false,
            panel_reveal: false,
            decor_active_alpha: None,
            decor_em: None,
            decor_line_height: None,
            background_lit_alpha: None,
            translation_delay: 0.0,
            line_exit_by_side: false,
            active_blur_from: None,
            marker_colors_alt: Vec::new(),
            rest_saturate_far: None,
            resume_height: None,
            resume_text_px: None,
            resume_black: false,
            resume_shadow: None,
            resume_offset: [0.0, 0.0],
            resume_fullscreen: None,
            scroll_secs: 0.75,
            scroll_ease: [0.86, 0.0, 0.2, 1.0],
            scroll_offset: 0.5,
            early_scroll: 0.54,
        }
    }
}

impl LyricStyle {
    /// The size of unsynced lyrics: a little under the synced ones.
    pub fn plain_size(&self) -> f32 {
        self.font_rem.first().map_or(3.0, |s| s[1]) * self.rem * 0.66
    }
}

// ------------------------------------------------------------------ cover and now playing

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    #[default]
    Split,
    LyricsOnly,
    CoverHero,
    /// A small cover in the bottom corner with the title beside it, lyrics above.
    Corner,
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
#[serde(default, deny_unknown_fields)]
pub struct CoverStyle {
    pub layout: Layout,
    pub side: Side,
    pub shape: Shape,
    pub radius: f32,
    /// Radius as a fraction of the cover's side, in place of `radius`.
    pub radius_pct: Option<f32>,
    pub spin_secs: f32,
    pub spin_when_paused: bool,
    /// Grooves and a spindle hole on `record`.
    pub record_details: bool,
    pub brightness: f32,
    pub size: f32,
    /// Side as a fraction of the window's height, in place of `size`.
    pub size_vh: Option<f32>,
    /// Largest side in points.
    pub max_px: Option<f32>,
    /// Moved by this much of the window's height: `[x, y]`. It may run off the window.
    pub offset_vh: [f32; 2],
    pub shadow: f32,
    pub shadow_blur: f32,
    pub shadow_offset: f32,
    pub glow: f32,
    /// `[px, white alpha]`.
    pub outline: [f32; 2],
    /// The cover's own opacity.
    pub opacity: f32,
    /// Widest, as a fraction of its column.
    pub max_frac: Option<f32>,
    /// A second, wider shadow: `[px down, blur, alpha]`.
    pub shadow2: Option<[f32; 3]>,
    /// A third.
    pub shadow3: Option<[f32; 3]>,
    /// And a fourth and fifth; `y` can be negative, for one that falls upward.
    pub shadow4: Option<[f32; 3]>,
    pub shadow5: Option<[f32; 3]>,
    /// Opening, it also slides in from this share of its side, and fades in.
    pub open_slide: f32,
    pub open_fade: bool,
    /// A gradient over the cover: `[top, bottom]` black alpha.
    pub overlay_gradient: Option<[f32; 2]>,
    /// What changes in full screen.
    pub fullscreen: Option<CoverFullscreen>,
    /// Opening now playing, the cover grows from this scale.
    pub open_scale: f32,
    /// How long that takes.
    pub open_secs: f32,
    /// Largest side by window width: `[[min width, px], ...]`, in place of `max_px`.
    pub size_steps: Vec<[f32; 2]>,
    /// Only the top corners are round, joined to a card under it.
    pub top_only: bool,
    /// Moved inside its column, px: `[x, y]`.
    pub padding: [f32; 2],
    /// A disc slides out of its sleeve when playing and back on pause: seconds.
    pub disc_slide_secs: f32,
    /// Dims this much while the pointer is over it, on top of the theme's brightness.
    pub hover_dim: f32,
}

/// `@media (fullscreen)` rules on the cover.
#[derive(Clone, Copy, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CoverFullscreen {
    pub radius: Option<f32>,
    pub size_vh: Option<f32>,
    pub max_px: Option<f32>,
    pub no_outline: bool,
    /// The side as a fraction of its column.
    pub width_frac: Option<f32>,
    /// A disc or circle is a plain rounded cover here.
    pub plain_shape: bool,
    /// Moved inside its column, px.
    pub padding: Option<[f32; 2]>,
    /// Grows from this scale on going full screen, over this long: `[scale, secs]`.
    pub enter: Option<[f32; 2]>,
    /// Settles from this scale on leaving full screen: `[scale, secs]`.
    pub leave: Option<[f32; 2]>,
    /// Where `enter` starts from, down by this fraction of the window's height.
    pub enter_shift_vh: f32,
    /// And `leave`.
    pub leave_shift_vh: f32,
    /// `enter` and `leave` also fade in, over this much of their time.
    pub settle_fade: f32,
    /// Its shadow and brightness here.
    pub shadow: Option<f32>,
    pub brightness: Option<f32>,
    /// The cover small at the bottom left with the title beside it, the lyrics on the right.
    pub corner_layout: bool,
}

impl Default for CoverStyle {
    fn default() -> Self {
        Self {
            layout: Layout::Split,
            side: Side::Left,
            shape: Shape::Rounded,
            radius: 3.0,
            radius_pct: None,
            spin_secs: 60.0,
            spin_when_paused: false,
            record_details: false,
            brightness: 1.0,
            size: 1.0,
            size_vh: None,
            max_px: None,
            offset_vh: [0.0, 0.0],
            shadow: 0.0,
            shadow_blur: 60.0,
            shadow_offset: 0.0,
            glow: 0.0,
            outline: [0.0, 0.0],
            opacity: 1.0,
            max_frac: None,
            shadow2: None,
            shadow3: None,
            shadow4: None,
            shadow5: None,
            open_slide: 0.0,
            open_fade: false,
            overlay_gradient: None,
            fullscreen: None,
            open_scale: 1.0,
            open_secs: 0.6,
            size_steps: Vec::new(),
            top_only: false,
            padding: [0.0, 0.0],
            disc_slide_secs: 0.0,
            hover_dim: 0.28,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabStyle {
    /// Lyrics and Up next chips that slide out by the ⋯ button.
    #[default]
    Hover,
    /// An underlined strip above the lyrics, always there.
    Strip,
    /// A pill of icons in the bottom corner.
    Pill,
}

/// The song's title block and what surrounds the cover in now playing.
#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NowPlayingStyle {
    pub tabs: TabStyle,
    pub title_align: Align,
    pub title_px: f32,
    pub artist_px: f32,
    /// The title block in the lyrics' font rather than the interface's.
    pub title_lyric_font: bool,
    pub show_title: bool,
    /// A translucent card in the corner with the song, in place of the title under the cover.
    pub info_card: bool,
    /// The card holds only the text, as wide as that, and stays over a `cover_fullbleed`
    /// cover beside the lyrics in full screen.
    pub info_card_text_only: bool,
    /// Song and Video switch in the header.
    pub show_video_switch: bool,
    /// Corner radius of the video (the cover's when unset).
    pub video_radius: Option<f32>,
    /// What the tabs show.
    pub tab_content: TabContent,
    /// Unselected tabs' opacity, and the tabs' row height. Kept usable: at least 0.35.
    pub tab_idle_alpha: f32,
    pub tab_px: f32,
    /// A fill behind the tabs: `[white alpha, radius]`.
    pub tab_back: Option<[f32; 2]>,
    /// Song and Video switch.
    pub switch: SwitchLook,
    /// Title and artist: opacity and weight.
    pub title_alpha: f32,
    pub artist_alpha: f32,
    pub title_weight: Weight,
    pub artist_weight: Weight,
    /// A card around the title block under the cover.
    pub title_card: Option<TitleCard>,
    /// The corner card's look when `info_card` is on: `[black alpha, radius]`.
    pub info_card_look: [f32; 2],
    /// The cover column's share of the view: `[windowed, full screen]`.
    pub panel_frac: Option<[f32; 2]>,
    /// Hovering the cover shows the transport on it.
    pub cover_controls: bool,
    /// Video mode keeps the lyrics beside the video.
    pub video_lyrics: bool,
    /// The video's widest in full screen, as a fraction of the view.
    pub video_max_frac: Option<f32>,
    /// What a song without lyrics says.
    pub no_lyrics_text: String,
    /// How it looks saying it.
    pub no_lyrics: NoLyrics,
    /// Tabs' labels, px and their strip's lower edge in white alpha.
    pub tab_label_px: f32,
    pub tab_edge: f32,
    /// The selected tab: its colour, a glow around it, and a pill sliding under it.
    pub tab_selected_color: String,
    pub tab_glow: f32,
    pub tab_pill: Option<f32>,
    /// The strip's width as a fraction of the panel, in place of its own.
    pub tab_width_frac: Option<f32>,
    /// Lyrics beside a video: the video's share of the view `[windowed, full screen]`.
    pub video_panel_frac: Option<[f32; 2]>,
    /// The sung line under a video, in a small pill.
    pub video_pill: bool,
    /// The title block's widest, px, and whether it's as wide as the cover.
    pub title_max_px: Option<f32>,
    /// A soft shadow under the title: `[px down, alpha]`.
    pub title_shadow: Option<[f32; 2]>,
    /// Buttons on the cover: `[px, black alpha, hover scale]`.
    pub cover_buttons: [f32; 3],
    /// In full screen, previous and next on the cover when the pointer's over it, as
    /// `[px, black alpha, hover scale]`, whether `cover_controls` is on or not.
    pub fullscreen_cover_buttons: Option<[f32; 3]>,
    /// The size of the icons on the tabs.
    pub tab_icon_px: f32,
    /// The tabs' capsule catching the light: white alpha of its top edge, of a ring all
    /// round, and of its bottom edge.
    pub tab_bevel: Option<[f32; 3]>,
    /// The lyrics' panel in full screen, of the view, and its side padding.
    pub fullscreen_panel_frac: Option<f32>,
    /// Padding around everything in full screen, px, in place of the usual.
    pub fullscreen_padding: Option<[f32; 2]>,
    /// In full screen the header slides away until the pointer moves.
    pub header_autohide: bool,
    /// The cover's play button: its size, and a bare glyph with no disc.
    pub cover_play_px: f32,
    pub cover_play_bare: bool,
    /// Hovering the cover also shows a capsule with the position and times on it.
    pub cover_progress: bool,
    /// Full screen has a seek bar along the bottom this tall, times on it. 0: none.
    pub fullscreen_progress_px: f32,
    /// The ends of a title too long for its width fade, px.
    pub title_edge_fade: f32,
    /// The selected tab's line, px; the labels' weight; the selected label's colour when it differs from the line's.
    pub tab_line_px: f32,
    pub tab_label_weight: Weight,
    pub tab_label_selected: String,
    /// `tab_back` in black rather than white.
    pub tab_back_black: bool,
    /// White alpha of a line around the selected tab.
    pub tab_ring: f32,
    /// The tabs under the cover, in place of the header.
    pub tab_under_cover: bool,
    /// The corner card at the top, this far from the corner, with these weights: `[title, artist]`.
    pub info_card_top: bool,
    pub info_card_margin: f32,
    pub info_card_weights: Option<[Weight; 2]>,
    /// The title's weight in full screen.
    pub fullscreen_title_weight: Option<Weight>,
    /// The lyrics panel in full screen, from the view's top and bottom, px. Can be negative.
    pub fullscreen_panel_inset: Option<[f32; 2]>,
    /// Full screen's seek bar's corners.
    pub fullscreen_progress_radius: f32,
    /// A cover's play button grows this much under the pointer.
    pub cover_play_hover: f32,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NoLyrics {
    /// The message's size, rem.
    pub rem: f32,
    /// A note over it.
    pub icon: bool,
    /// The cover slides to the middle after this many seconds. Below zero: it stays.
    pub center_after: f32,
    /// It stays up instead of fading out after a few seconds.
    pub stays: bool,
}

impl Default for NoLyrics {
    fn default() -> Self {
        Self { rem: 1.4, icon: false, center_after: -1.0, stays: false }
    }
}

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabContent {
    #[default]
    Labels,
    Icons,
    Both,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SwitchLook {
    /// White alpha behind it and behind the selected side.
    pub back: Option<f32>,
    pub selected: Option<f32>,
    pub radius: Option<f32>,
    /// White alpha of its outline.
    pub stroke: f32,
    /// Black instead of white behind it.
    pub back_black: bool,
    /// `[px down, blur, alpha]`.
    pub shadow: Option<[f32; 3]>,
    /// In the bottom corner rather than in the header.
    pub at_corner: bool,
    /// Moved down, px.
    pub y_offset: f32,
    /// In the corner, the right one.
    pub at_right: bool,
    /// Song and Video as plain words, the selected one over `selected`.
    pub plain: bool,
    /// Moved down in full screen, in place of `y_offset`.
    pub y_offset_full: Option<f32>,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TitleCard {
    pub fill: f32,
    pub stroke: f32,
    pub radius: f32,
    pub dashed: bool,
    /// `[px down, blur, alpha]`.
    pub shadow: Option<[f32; 3]>,
    /// Joined to the cover above it: no gap, and square top corners.
    pub attached: bool,
}

impl Default for NowPlayingStyle {
    fn default() -> Self {
        Self { tabs: TabStyle::Hover, title_align: Align::Center, title_px: 24.0, artist_px: 16.0, title_lyric_font: false, show_title: true, info_card: false, info_card_text_only: false, show_video_switch: true, video_radius: None, tab_content: TabContent::Labels, tab_idle_alpha: 1.0, tab_px: 34.0, tab_back: None, switch: SwitchLook::default(), title_alpha: 1.0, artist_alpha: 1.0, title_weight: Weight::Bold, artist_weight: Weight::Regular, title_card: None, info_card_look: [0.45, 14.0], panel_frac: None, cover_controls: false, video_lyrics: false, video_max_frac: None, no_lyrics_text: String::new(), no_lyrics: NoLyrics::default(), tab_label_px: 12.5, tab_edge: 0.0, tab_selected_color: String::new(), tab_glow: 0.0, tab_pill: None, tab_width_frac: None, video_panel_frac: None, video_pill: false, title_max_px: None, title_shadow: None, cover_buttons: [44.0, 0.5, 1.0], fullscreen_cover_buttons: None, tab_icon_px: 18.0, tab_bevel: None, fullscreen_panel_frac: None, fullscreen_padding: None, header_autohide: false, cover_play_px: 76.0, cover_play_bare: false, cover_progress: false, fullscreen_progress_px: 0.0, title_edge_fade: 0.0, tab_line_px: 1.5, tab_label_weight: Weight::Semibold, tab_label_selected: String::new(), tab_back_black: false, tab_ring: 0.0, tab_under_cover: false, info_card_top: false, info_card_margin: 0.0, info_card_weights: None, fullscreen_title_weight: None, fullscreen_panel_inset: None, fullscreen_progress_radius: 0.0, cover_play_hover: 1.0 }
    }
}

// ------------------------------------------------------------------ player bar

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerBar {
    #[default]
    Docked,
    Floating,
}

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressAt {
    /// Along the bar's top edge.
    #[default]
    Top,
    /// Along its bottom edge.
    Bottom,
    /// Centred above the transport.
    AboveTransport,
}

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayButton {
    /// White disc, dark glyph.
    #[default]
    Disc,
    /// The glyph alone.
    Bare,
    /// White at 10% with a thin outline.
    Ghost,
    /// Filled with the accent.
    Accent,
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Chrome {
    pub glass: f32,
    pub radius: f32,
    pub player_bar: PlayerBar,
    pub bar_height: f32,
    /// A floating bar's corner radius (full pill past half its height).
    pub bar_radius: f32,
    /// Bar colour under `app.bar_alpha`; empty: black, or white in light mode.
    pub bar_color: String,
    /// `[sides, bottom]` of a floating bar.
    pub bar_margin: [f32; 2],
    /// A floating bar's widest, points.
    pub bar_max_px: Option<f32>,
    pub progress_at: ProgressAt,
    pub progress_px: f32,
    /// "accent", "white" or "#rrggbb".
    pub progress_color: String,
    pub progress_radius: f32,
    /// A knob on hover.
    pub progress_thumb: bool,
    pub play_button: PlayButton,
    pub hide_volume: bool,
    /// A disc spinning behind the bar's thumbnail, seconds per turn.
    pub disc_secs: Option<f32>,
    /// The thumbnail itself is a spinning disc, seconds per turn.
    pub thumb_disc_secs: Option<f32>,
    /// Bar thumbnail: side and radius.
    pub thumb_px: Option<f32>,
    pub thumb_radius: Option<f32>,
    /// Title and byline.
    pub bar_title_px: f32,
    pub bar_title_weight: Weight,
    pub bar_byline_px: f32,
    pub bar_byline_alpha: f32,
    /// `[px down, blur, alpha]`.
    pub bar_shadow: Option<[f32; 3]>,
    /// White alpha of the bar's outline, in place of the default.
    pub bar_border_alpha: Option<f32>,
    /// Bar buttons: scale and resting opacity. Kept usable: at least 0.75 and 0.5.
    pub icon_scale: f32,
    pub icon_alpha: f32,
    pub progress_alpha: f32,
    pub progress_track: f32,
    /// A gradient along the filled part, in place of `progress_color`.
    pub progress_gradient: Vec<String>,
    pub time_weight: Weight,
    pub time_alpha: f32,
    /// Where the transport and the song go.
    pub bar_layout: BarLayout,
    /// The bar's fill over now playing, when it differs: black alpha.
    pub bar_np_alpha: Option<f32>,
    /// The cover shows through the bar at this opacity while now playing is closed.
    pub bar_cover_alpha: f32,
    /// The volume slider folds into its icon until hovered.
    pub volume_on_hover: bool,
    /// A byline too long for the bar scrolls while hovered.
    pub byline_marquee: bool,
    /// Its edges fade rather than cut: px.
    pub marquee_fade: f32,
    /// The bar's insides: `[sides, top]` px.
    pub bar_pad: Option<[f32; 2]>,
    /// The transport's spacing and its smaller buttons' size against the usual; the play button's size.
    pub transport_gap: f32,
    pub transport_scale: f32,
    pub play_scale: f32,
    /// Previous and next at rest.
    pub side_alpha: f32,
    /// The transport moved along the bar, px.
    pub transport_shift: f32,
    /// A block behind the song: `[white alpha, radius]`.
    pub track_block: Option<[f32; 2]>,
    /// Where the time goes.
    pub time_at: TimeAt,
    /// The heart shows only while the pointer is on the song. Right click there has the song's whole menu.
    pub like_on_hover: bool,
    pub like_hover_scale: f32,
    /// The progress line kept this far from the bar's sides, and its knob: radius and a dot in it.
    pub progress_inset: f32,
    pub progress_thumb_px: f32,
    pub progress_thumb_dot: bool,
    /// The docked bar's outline over now playing too.
    pub bar_np_border: bool,
    /// A floating bar has no outline over now playing.
    pub bar_np_no_edge: bool,
    /// A tint across the bar, left to right, in `bar_tint_alpha`.
    pub bar_tint: Vec<String>,
    pub bar_tint_alpha: f32,
    /// The outline's colour in place of white, and only along the docked bar's top.
    pub bar_edge_color: String,
    pub bar_edge_top: bool,
    /// A docked bar's shadow up over the page: `[px, alpha]`.
    pub bar_docked_shadow: Option<[f32; 2]>,
    /// A glow around the bar's thumbnail: `[blur px, alpha]`, in `thumb_glow_color`.
    pub thumb_glow: Option<[f32; 2]>,
    pub thumb_glow_color: String,
    /// The seek tip's fill and text.
    pub tip_fill: String,
    pub tip_text: String,
    /// A progress line over the transport: its width as a fraction of the bar.
    pub progress_width_frac: Option<f32>,
    /// The knob's colour, its dot's colour, and the dot against the knob.
    pub progress_thumb_color: String,
    pub progress_thumb_dot_color: String,
    pub progress_thumb_dot_frac: f32,
    /// The knob as a pill, `[width, height]`.
    pub progress_thumb_pill: Option<[f32; 2]>,
    /// A scrolling byline goes round in this many seconds, in place of a fixed speed.
    pub marquee_secs: Option<f32>,
    /// The transport moved along the bar, as a fraction of its width.
    pub transport_shift_frac: f32,
}

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeAt {
    /// By the volume.
    #[default]
    Right,
    /// Under the transport.
    UnderTransport,
    /// Over the progress line, with `progress_at` `above_transport`.
    AboveProgress,
}

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BarLayout {
    /// The song on the left, the transport in the middle.
    #[default]
    SongLeft,
    /// The transport on the left, the song in the middle: stock YouTube Music.
    SongMiddle,
}

impl Default for Chrome {
    fn default() -> Self {
        Self {
            glass: 1.0,
            radius: 18.0,
            player_bar: PlayerBar::Docked,
            bar_height: 72.0,
            bar_radius: 18.0,
            bar_color: String::new(),
            bar_margin: [14.0, 14.0],
            bar_max_px: None,
            progress_at: ProgressAt::Top,
            progress_px: 3.0,
            progress_color: "accent".into(),
            progress_radius: 0.0,
            progress_thumb: true,
            play_button: PlayButton::Disc,
            hide_volume: false,
            disc_secs: None,
            thumb_disc_secs: None,
            thumb_px: None,
            thumb_radius: None,
            bar_title_px: 15.0,
            bar_title_weight: Weight::Semibold,
            bar_byline_px: 13.5,
            bar_byline_alpha: 1.0,
            bar_shadow: None,
            bar_border_alpha: None,
            icon_scale: 1.0,
            icon_alpha: 1.0,
            progress_alpha: 1.0,
            progress_track: 0.12,
            progress_gradient: Vec::new(),
            time_weight: Weight::Regular,
            time_alpha: 1.0,
            bar_layout: BarLayout::SongLeft,
            bar_np_alpha: None,
            bar_cover_alpha: 0.0,
            volume_on_hover: false,
            byline_marquee: false,
            marquee_fade: 0.0,
            bar_pad: None,
            transport_gap: 1.0,
            transport_scale: 1.0,
            play_scale: 1.0,
            side_alpha: 1.0,
            transport_shift: 0.0,
            track_block: None,
            time_at: TimeAt::Right,
            like_on_hover: false,
            like_hover_scale: 1.0,
            progress_inset: 0.0,
            progress_thumb_px: 7.0,
            progress_thumb_dot: false,
            bar_np_border: true,
            bar_np_no_edge: false,
            bar_tint: Vec::new(),
            bar_tint_alpha: 0.0,
            bar_edge_color: String::new(),
            bar_edge_top: false,
            bar_docked_shadow: None,
            thumb_glow: None,
            thumb_glow_color: String::new(),
            tip_fill: String::new(),
            tip_text: String::new(),
            progress_width_frac: None,
            progress_thumb_color: String::new(),
            progress_thumb_dot_color: String::new(),
            progress_thumb_dot_frac: 0.4,
            progress_thumb_pill: None,
            marquee_secs: None,
            transport_shift_frac: 0.0,
        }
    }
}

// ------------------------------------------------------------------ the rest of the app

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageBg {
    /// Whatever the background mode shows in now playing.
    Same,
    /// Blurred cover, no motion.
    Static,
    /// Dark with a soft gradient in the cover's colour.
    Accent,
    /// The preset's solid colour.
    #[default]
    Solid,
}

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidebarStyle {
    Glass,
    #[default]
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
    /// No fill: the item's text and icon take the accent.
    Text,
    /// A white block with dark text.
    Inverse,
}

#[derive(Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChipStyle {
    /// Translucent; selected is the text colour.
    #[default]
    Pill,
    Outline,
    /// Selected is the accent.
    Accent,
    /// Selected is translucent with accent text.
    Tonal,
    /// White at 10% with a thin outline, also when selected.
    Ghost,
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppStyle {
    pub font: String,
    pub page_bg: PageBg,
    pub page_dim: Option<f32>,
    pub sidebar: SidebarStyle,
    pub sidebar_alpha: f32,
    pub sidebar_border: f32,
    pub bar_alpha: f32,
    pub bar_border: bool,
    pub nav: NavStyle,
    pub chip: ChipStyle,
    pub chip_radius: Option<f32>,
    pub card_radius: f32,
    /// White fill and outline alpha behind cards' art and rows.
    pub card_fill: f32,
    pub card_stroke: f32,
    pub thumb_radius: f32,
    pub panel_radius: f32,
    /// Menus and popups, when they differ from `panel_radius`.
    pub menu_radius: Option<f32>,
    /// White outline alpha on menus and dialogs.
    pub menu_border: f32,
    /// Menu colour; empty: the app's own.
    pub menu_color: String,
    pub hover: f32,
    pub text_dim: String,
    pub search_radius: f32,
    pub search_alpha: f32,
    pub search_border: f32,
    pub hide_scrollbars: bool,
    /// The accent also on the play button, sliders and toggles.
    pub accent_controls: bool,
    /// Pages sit on a layered sheet over the background: its white alpha and top-left radius.
    pub sheet: Option<[f32; 2]>,
    /// Glass panels frost the background art behind them.
    pub frost: bool,
    /// Every weight of interface text in bold.
    pub all_bold: bool,
    pub controls: Controls,
    pub nav_item: NavItem,
    pub chip_look: ChipLook,
    pub card_look: CardLook,
    pub row_look: RowLook,
    pub menu_look: MenuLook,
    pub search_look: SearchLook,
    pub top_bar: TopBarLook,
    pub toast_look: ToastLook,
    pub toggle_look: ToggleLook,
    /// Scrollbars: `[width px, white alpha]`.
    pub scrollbar: Option<[f32; 2]>,
    /// Section headings: px and weight, and the strapline's opacity.
    pub heading_px: f32,
    pub heading_weight: Weight,
    pub strap_alpha: f32,
    /// A wash down from the top of the pages: `[colour alpha, px]`, in `page_wash_color`.
    pub page_wash: Option<[f32; 2]>,
    pub page_wash_color: String,
    /// The same, up from the bottom of the window: `[black alpha, px]`.
    pub page_foot: Option<[f32; 2]>,
    /// New pages rise this many px as they fade in.
    pub page_rise_px: f32,
    /// Album and playlist pages open under their art, at this opacity, fading out down the header.
    pub immersive_header: Option<f32>,
    /// Their title's size.
    pub header_title_px: f32,
    /// What really frosts what's behind it (OpenGL; elsewhere the plain tint stands in).
    pub glass: GlassLook,
    /// Things loading show a shape morphing, not a spinner.
    pub morph_loader: bool,
    /// Corners on the cover, search box, rows, menus and toasts are squircles (`corner-shape: superellipse(2)`).
    pub squircle: bool,
    /// Scrollbars' colour in place of white, and none on Home.
    pub scrollbar_color: String,
    pub scrollbar_home_hidden: bool,
    /// Section headings' letter spacing, em.
    pub heading_tracking_em: f32,
    pub header_title_weight: Weight,
    /// Artist and album headers: their picture's opacity in place of the default, and a wash down them
    /// from this black alpha to this one.
    pub header_image_alpha: Option<f32>,
    pub header_gradient: Option<[f32; 2]>,
    /// Text boxes outside the top row.
    pub input: Option<InputLook>,
    /// Dialogs have only a line along their bottom.
    pub dialog_bottom_line: bool,
    /// Up next: heading in capitals, on a card of the cover.
    pub queue_upper: bool,
    pub queue_card: bool,
    /// Now playing's flat colour when it differs from the pages'.
    pub np_solid: String,
    /// Durations in even-width digits.
    pub tabular_digits: bool,
    /// The pages' background: saturation, contrast and zoom in place of now playing's.
    pub page_saturation: Option<f32>,
    pub page_contrast: f32,
    pub page_scale: Option<f32>,
    /// A wash across the pages from the left: `[colour alpha, px]`, in `page_wash_color`.
    pub page_side: Option<[f32; 2]>,
    /// New pages slide up from this far on opening now playing... and a plain fade if 0.
    pub np_open_secs: f32,
    /// The main text colour on dark pages.
    pub text_color: String,
    /// How long the interface's small fades take: hovers, presses, selections.
    pub ui_fade_secs: Option<f32>,
    /// A playlist's art: its shadow `[px down, blur, alpha]` and its growth under the pointer.
    pub header_art_shadow: Option<[f32; 3]>,
    pub header_art_hover: f32,
    /// A playlist's Play button's shadow, in the accent: `[px down, blur, alpha]`.
    pub header_play_shadow: Option<[f32; 3]>,
    /// The queue's rows, where they differ.
    pub queue_look: QueueLook,
    /// An artist's top songs in this many columns, on a wide page.
    pub top_songs_cols: u32,
    /// The theme's own icons, by name: home, explore, library, previous, next, up_next, lyrics, song, video.
    pub icons: std::collections::HashMap<String, VectorIcon>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InputLook {
    pub fill: f32,
    pub radius: f32,
    /// A ring on the focused box, in the accent.
    pub focus_px: f32,
    /// The ring in white at this alpha, in place of the accent.
    pub focus_white: Option<f32>,
}

impl Default for InputLook {
    fn default() -> Self {
        Self { fill: 0.08, radius: 8.0, focus_px: 0.0, focus_white: None }
    }
}

/// The ring on whatever has keyboard focus.
#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FocusRing {
    pub color: String,
    pub px: f32,
    /// A dark line inside it, px.
    pub inner_px: f32,
}

impl Default for FocusRing {
    fn default() -> Self {
        Self { color: "white".into(), px: 2.0, inner_px: 0.0 }
    }
}

/// `backdrop-filter`s: `[blur px, saturation, contrast]`.
#[derive(Clone, Copy, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GlassLook {
    /// The top row. The page scrolls under it.
    pub top_bar: Option<[f32; 3]>,
    /// A floating player bar. The page scrolls under it.
    pub bar: Option<[f32; 3]>,
    pub menu: Option<[f32; 3]>,
    pub search_drop: Option<[f32; 3]>,
    pub toast: Option<[f32; 3]>,
    pub dialog: Option<[f32; 3]>,
    /// The play button on a card's art.
    pub card_play: Option<[f32; 3]>,
    /// Over the cover and the background in now playing.
    pub title_card: Option<[f32; 3]>,
    pub tabs: Option<[f32; 3]>,
    /// `brightness()` in the tabs' backdrop filter.
    pub tabs_bright: Option<f32>,
    pub switch: Option<[f32; 3]>,
    pub resume: Option<[f32; 3]>,
    pub instrumental: Option<[f32; 3]>,
    /// The search box itself.
    pub search: Option<[f32; 3]>,
}

/// Buttons in general.
#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Controls {
    /// Opacity of sidebar items, icons, the search box and tabs at rest. Kept readable: at least 0.5.
    pub idle_alpha: f32,
    /// White alpha of a 1 px outline on ghost fills.
    pub ghost_outline: f32,
    pub press_scale: f32,
    pub hover_scale: f32,
    /// A shelf's scroll arrows on hover: `[scale, degrees turned]`.
    pub arrow_hover: Option<[f32; 2]>,
    /// White alpha of a button, added on hover.
    pub button_fill: f32,
    pub button_hover: f32,
    /// The colour icons take on hover; empty: the text colour.
    pub hover_color: String,
    /// The Play pill in the accent.
    pub accent_primary: bool,
    /// White alpha of an outline on hover, and of a fill while pressed.
    pub hover_border: f32,
    pub press_fill: f32,
    /// `[px down, blur, alpha]` under buttons.
    pub shadow: Option<[f32; 3]>,
    /// Pressing a card, chip or row shrinks it to this.
    pub press_cards: f32,
    pub press_chips: f32,
    pub press_rows: f32,
    /// The bar's play button on hover.
    pub play_hover_scale: f32,
    pub play_hover_color: String,
    /// How keyboard focus shows.
    pub focus: Option<FocusRing>,
    /// Interface motion overshoots slightly.
    pub spring: bool,
}

impl Default for Controls {
    fn default() -> Self {
        Self { idle_alpha: 1.0, ghost_outline: 0.0, press_scale: 0.9, hover_scale: 1.0, arrow_hover: None, button_fill: 0.10, button_hover: 0.08, hover_color: String::new(), accent_primary: false, hover_border: 0.0, press_fill: 0.0, shadow: None, press_cards: 1.0, press_chips: 1.0, press_rows: 1.0, play_hover_scale: 1.05, play_hover_color: String::new(), focus: None, spring: false }
    }
}

/// Sidebar entries.
#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NavItem {
    pub radius: Option<f32>,
    pub height: f32,
    pub hover: f32,
    pub selected: f32,
    /// Px an entry slides on hover.
    pub slide: f32,
    /// Starts as an icon rail; the menu button still opens it.
    pub rail: bool,
    pub rail_px: f32,
    /// Labels under the rail's icons.
    pub rail_labels: bool,
    /// Playlists as one line of text at this height, without art.
    pub title_rows: Option<f32>,
    /// The sidebar's width, px, in place of the usual.
    pub width: Option<f32>,
    /// White alpha while pressed.
    pub press: f32,
    /// No line between the top entries and the playlists.
    pub hide_divider: bool,
    /// The sidebar's fill only once the page has scrolled.
    pub scroll_fill: bool,
    /// The selected entry's colour in place of white.
    pub selected_color: String,
    /// `selected` on now playing's bar... and over a page, as it differs.
    pub side_margin: f32,
    /// A heading over the playlists, in place of PLAYLISTS, and whether it has a + for a new one.
    pub playlists_label: String,
    pub playlists_plus: bool,
    /// The sidebar's top shows the blurred cover, fading out: `[alpha, px]`.
    pub cover_tint: Option<[f32; 2]>,
    /// The selected entry's fill in this colour, at `selected`'s alpha.
    pub selected_fill: String,
    /// The Bar style's bar, px tall.
    pub bar_px: f32,
    /// Entries' height on the rail, in place of `height`.
    pub rail_height: Option<f32>,
    /// A hovered entry's fill grows by this.
    pub hover_zoom: f32,
}

impl Default for NavItem {
    fn default() -> Self {
        Self { radius: None, height: 42.0, hover: 0.05, selected: 0.10, slide: 0.0, rail: false, rail_px: 76.0, rail_labels: false, title_rows: None, width: None, press: 0.0, hide_divider: false, scroll_fill: false, selected_color: String::new(), side_margin: 0.0, playlists_label: String::new(), playlists_plus: false, cover_tint: None, selected_fill: String::new(), bar_px: 20.0, rail_height: None, hover_zoom: 1.0 }
    }
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChipLook {
    pub height: f32,
    /// White alpha at rest and added on hover, in place of the chip style's.
    pub fill: Option<f32>,
    pub hover: Option<f32>,
    /// Selected fill ("white", "accent", "#rrggbb", or empty for the style's) and its alpha.
    pub selected_fill: String,
    pub selected_alpha: f32,
    pub selected_text: String,
    pub weight: Weight,
    /// White alpha of its outline, and its weight when selected.
    pub border: f32,
    pub selected_weight: Option<Weight>,
    /// Joined in a row like a segmented control: `[outer radius, inner radius, gap]`.
    pub segmented: Option<[f32; 3]>,
    /// A hovered chip rises this far, px; text colour of unselected chips.
    pub hover_lift: f32,
    pub text_color: String,
}

impl Default for ChipLook {
    fn default() -> Self {
        Self { height: 34.0, fill: None, hover: None, selected_fill: String::new(), selected_alpha: 1.0, selected_text: String::new(), weight: Weight::Semibold, border: 0.0, selected_weight: None, segmented: None, hover_lift: 0.0, text_color: String::new() }
    }
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CardLook {
    /// The whole art's scale on hover.
    pub hover_scale: f32,
    /// `[px down, blur, alpha]` on hover.
    pub shadow: Option<[f32; 3]>,
    pub title_px: f32,
    pub title_weight: Weight,
    pub subtitle_alpha: f32,
    /// The picture's zoom inside the art on hover, and how much it dims.
    pub image_zoom: f32,
    pub image_dim: f32,
    /// White alpha of a 1 px outline on every picture.
    pub image_outline: f32,
    /// Play button on the art: side px, black alpha, and whether hover turns it the accent.
    pub play_px: f32,
    pub play_alpha: f32,
    pub play_hover_accent: bool,
    /// The shadow shows at rest too, and the card lifts this many px on hover.
    pub shadow_rest: bool,
    pub lift_px: f32,
    /// The play button's inner ring in white alpha, and its scale on hover.
    pub play_ring: f32,
    pub play_hover_scale: f32,
    /// The ⋮ button lights up in the accent on hover.
    pub menu_hover_accent: bool,
    /// The play button: in the art's middle, a bare glyph with no disc, its colours, its fill under the pointer, the glyph's size.
    pub play_center: bool,
    pub play_bare: bool,
    pub play_color: String,
    pub play_glyph_color: String,
    pub play_hover_alpha: Option<f32>,
    pub play_glyph_px: f32,
}

impl Default for CardLook {
    fn default() -> Self {
        Self { hover_scale: 1.0, shadow: None, title_px: 14.5, title_weight: Weight::Semibold, subtitle_alpha: 1.0, image_zoom: 0.06, image_dim: 0.28, image_outline: 0.0, play_px: 44.0, play_alpha: 0.62, play_hover_accent: false, shadow_rest: false, lift_px: 0.0, play_ring: 0.0, play_hover_scale: 1.0, menu_hover_accent: false, play_center: false, play_bare: false, play_color: String::new(), play_glyph_color: String::new(), play_hover_alpha: None, play_glyph_px: 10.0 }
    }
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RowLook {
    pub height: f32,
    pub thumb_px: f32,
    pub radius: f32,
    pub hover: f32,
    pub current: f32,
    /// The playing row's tint in place of white, with its alpha in `current`.
    pub current_color: String,
    /// A bar at the playing row's left edge, px.
    pub current_bar: f32,
    /// A white line between rows, this alpha.
    pub separator: f32,
    /// The ⋮ button shows only on hover. Right click still opens the same menu.
    pub hover_menu: bool,
    /// Its dots run across rather than down.
    pub menu_dots_across: bool,
    /// A hovered row: its scale and an outline in white alpha.
    pub hover_scale: f32,
    pub hover_border: f32,
    /// Its picture: corner radius in place of the app's, zoom and how much it dims on hover.
    pub thumb_radius: Option<f32>,
    pub image_zoom: f32,
    pub image_dim: f32,
    /// Space inside the row's left and right, px.
    pub pad: f32,
    /// Artists, plays and durations: their alpha.
    pub byline_alpha: f32,
    /// A hovered row rises this far, px.
    pub hover_lift: f32,
}

/// An icon as SVG path data, filled.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VectorIcon {
    /// `[width, height]` of the path's box, and `[x, y]` of its corner when that isn't zero.
    pub view: [f32; 2],
    #[serde(default)]
    pub origin: [f32; 2],
    pub d: String,
    /// Mirrored left to right.
    #[serde(default)]
    pub flip: bool,
    /// Drawn as a line this wide with round ends, in the path's units, instead of filled.
    #[serde(default)]
    pub stroke: f32,
}

/// The queue's rows, where they differ from every other list's.
#[derive(Clone, Copy, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QueueLook {
    pub thumb_radius: Option<f32>,
    pub radius: Option<f32>,
    pub pad: Option<f32>,
    pub hover_border: Option<f32>,
    pub image_zoom: Option<f32>,
    pub image_dim: Option<f32>,
    /// The panel's own space at its sides, px.
    pub side_pad: f32,
    /// Space under the current song's row, px.
    pub current_gap: f32,
}

impl Default for RowLook {
    fn default() -> Self {
        Self { height: 56.0, thumb_px: 40.0, radius: 8.0, hover: 0.07, current: 0.07, current_color: String::new(), current_bar: 0.0, separator: 0.0, hover_menu: false, menu_dots_across: false, hover_scale: 1.0, hover_border: 0.0, thumb_radius: None, image_zoom: 0.0, image_dim: 0.5, pad: 10.0, byline_alpha: 1.0, hover_lift: 0.0 }
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MenuLook {
    pub row_px: f32,
    pub hover: f32,
    /// `menu_color`'s alpha.
    pub alpha: f32,
    /// The icon on the row's right.
    pub icon_right: bool,
    /// `[px down, blur, alpha]`.
    pub shadow: Option<[f32; 3]>,
    pub row_radius: f32,
    /// Opens from this scale, over this many seconds: `[scale, secs]`.
    pub pop: Option<[f32; 2]>,
    /// Rows slide this far in as the menu opens.
    pub item_slide: f32,
    /// White alpha of a line between rows.
    pub separator: f32,
    /// Narrowest, px.
    pub min_px: f32,
    /// Hovered rows' icon and text take the accent.
    pub hover_accent: bool,
    /// Hovered rows' icon and text slide this far, px.
    pub row_slide: f32,
    /// Dialogs' alpha, where it differs from a menu's.
    pub dialog_alpha: Option<f32>,
}

impl Default for MenuLook {
    fn default() -> Self {
        Self { row_px: 32.0, hover: 0.12, alpha: 1.0, icon_right: false, shadow: None, row_radius: 7.0, pop: None, item_slide: 0.0, separator: 0.0, min_px: 226.0, hover_accent: false, row_slide: 0.0, dialog_alpha: None }
    }
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchLook {
    pub height: f32,
    pub max_px: f32,
    /// In place of white at `search_alpha`, with that alpha.
    pub color: String,
    /// Focused: this colour outlines it, with a ring of `focus_ring` px.
    pub focus_color: String,
    pub focus_ring: f32,
    pub drop_radius: Option<f32>,
    /// Accent on its icon.
    pub accent_icon: bool,
    /// Its colour while its suggestions are open, and its bottom corners square then, with them attached.
    pub open_color: String,
    pub attached: bool,
    /// A second outline around it, `[radius, white alpha]`.
    pub outer: Option<[f32; 2]>,
    /// Focused: a line along its bottom in `focus_color` rather than an outline.
    pub underline: bool,
    /// The list's outline in white alpha, and its rows' radius.
    pub drop_ring: f32,
    pub item_radius: f32,
    /// The list opens downward from nothing.
    pub drop_grow: bool,
    /// The list's alpha, where it differs from a menu's.
    pub drop_alpha: Option<f32>,
}

impl Default for SearchLook {
    fn default() -> Self {
        Self { height: 42.0, max_px: 560.0, color: String::new(), focus_color: String::new(), focus_ring: 0.0, drop_radius: None, accent_icon: false, open_color: String::new(), attached: false, outer: None, underline: false, drop_ring: 0.0, item_radius: 8.0, drop_grow: false, drop_alpha: None }
    }
}

#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TopBarLook {
    /// A pill around the row, this wide at most.
    pub capsule_px: Option<f32>,
    /// Its fill: `[black alpha, radius]`.
    pub capsule: [f32; 2],
    /// A band behind the row, fading out below it: `[black alpha, px]`.
    pub band: Option<[f32; 2]>,
    /// Its colour in place of black, and whether it only comes once the page has scrolled.
    pub band_color: String,
    pub band_on_scroll: bool,
    /// The name's last word in this colour, and the name at the start of this row too.
    pub logo_color2: String,
    pub logo_in_bar: bool,
    /// A name in place of the sidebar's logo, in `logo_color`.
    pub logo_text: String,
    pub logo_color: String,
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToastLook {
    pub color: String,
    pub alpha: f32,
    /// White alpha of its outline, and how wide.
    pub border: [f32; 2],
    pub radius: f32,
    pub secs: f32,
    pub text_color: String,
}

impl Default for ToastLook {
    fn default() -> Self {
        Self { color: String::new(), alpha: 1.0, border: [0.0, 0.0], radius: 12.0, secs: 4.1, text_color: String::new() }
    }
}

/// Settings' toggles.
#[derive(Clone, Copy, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToggleLook {
    /// `[width, height]`.
    pub size: Option<[f32; 2]>,
    pub knob: Option<f32>,
    /// Off is an outline, not a fill.
    pub outline: bool,
    /// On is too, in white.
    pub on_outline: bool,
    /// Off's outline: `[px, white alpha]`, in place of a thin line in the dim colour.
    pub off_outline: Option<[f32; 2]>,
}

impl Default for AppStyle {
    fn default() -> Self {
        Self {
            font: "inter".into(),
            page_bg: PageBg::Solid,
            page_dim: None,
            sidebar: SidebarStyle::Transparent,
            sidebar_alpha: 0.28,
            sidebar_border: 0.0,
            bar_alpha: 1.0,
            bar_border: false,
            nav: NavStyle::Pill,
            chip: ChipStyle::Pill,
            chip_radius: None,
            card_radius: 3.0,
            card_fill: 0.0,
            card_stroke: 0.0,
            thumb_radius: 2.0,
            panel_radius: 8.0,
            menu_radius: None,
            menu_border: 0.0,
            menu_color: String::new(),
            hover: 1.0,
            text_dim: "#aaaaaa".into(),
            search_radius: 8.0,
            search_alpha: 0.09,
            search_border: 0.0,
            hide_scrollbars: false,
            accent_controls: false,
            sheet: None,
            frost: false,
            all_bold: false,
            controls: Controls::default(),
            nav_item: NavItem::default(),
            chip_look: ChipLook::default(),
            card_look: CardLook::default(),
            row_look: RowLook::default(),
            menu_look: MenuLook::default(),
            search_look: SearchLook::default(),
            top_bar: TopBarLook::default(),
            toast_look: ToastLook::default(),
            toggle_look: ToggleLook::default(),
            scrollbar: None,
            heading_px: 24.0,
            heading_weight: Weight::Bold,
            strap_alpha: 1.0,
            page_wash: None,
            page_wash_color: String::new(),
            page_foot: None,
            page_rise_px: 0.0,
            immersive_header: None,
            header_title_px: 40.0,
            glass: GlassLook::default(),
            morph_loader: false,
            squircle: false,
            scrollbar_color: String::new(),
            scrollbar_home_hidden: false,
            heading_tracking_em: 0.0,
            header_title_weight: Weight::Bold,
            header_image_alpha: None,
            header_gradient: None,
            input: None,
            dialog_bottom_line: false,
            queue_upper: false,
            queue_card: false,
            np_solid: String::new(),
            tabular_digits: false,
            page_saturation: None,
            page_contrast: 1.0,
            page_scale: None,
            page_side: None,
            np_open_secs: 0.42,
            text_color: String::new(),
            ui_fade_secs: None,
            header_art_shadow: None,
            header_art_hover: 1.0,
            header_play_shadow: None,
            queue_look: QueueLook::default(),
            top_songs_cols: 1,
            icons: Default::default(),
        }
    }
}

// ------------------------------------------------------------------ a theme

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub creators: Vec<String>,
    pub blurb: String,
    /// What this was ported from, and which version.
    pub source: String,
    pub background: Background,
    /// "cover" (sampled from the art), "system" (the Windows accent colour, else the
    /// cover's), or a fixed "#rrggbb".
    pub accent: String,
    pub lyrics: LyricStyle,
    pub cover: CoverStyle,
    pub now_playing: NowPlayingStyle,
    pub chrome: Chrome,
    pub app: AppStyle,
}

impl Default for Preset {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            creators: Vec::new(),
            blurb: String::new(),
            source: String::new(),
            background: Background::default(),
            accent: "#ff0033".into(),
            lyrics: LyricStyle::default(),
            cover: CoverStyle::default(),
            now_playing: NowPlayingStyle::default(),
            chrome: Chrome::default(),
            app: AppStyle::default(),
        }
    }
}

impl Preset {
    /// The accent to use given the one sampled from the current cover.
    pub fn accent(&self, from_cover: Color32) -> Color32 {
        if self.accent == "system" {
            return crate::theme::system_accent().unwrap_or(from_cover);
        }
        hex(&self.accent).unwrap_or(from_cover)
    }

    /// The page colour: the theme's, or in light mode its own or one derived from it.
    pub fn solid(&self) -> Color32 {
        let dark = hex(&self.background.solid).unwrap_or(crate::theme::bg());
        if !crate::theme::light() {
            return dark;
        }
        hex(&self.background.solid_light).unwrap_or_else(|| derived_light(dark))
    }
}

/// A theme with no light colour of its own: near-white, keeping a trace of its dark one's hue.
fn derived_light(dark: Color32) -> Color32 {
    let tint = |c: u8| 243 + (c as u32 * 9 / 255) as u8;
    Color32::from_rgb(tint(dark.r()), tint(dark.g()), tint(dark.b()))
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

const FILES: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/theme_presets.rs"));

/// Every theme: the app's own and Classic first, then the rest by name.
pub fn all() -> &'static [Preset] {
    static ALL: OnceLock<Vec<Preset>> = OnceLock::new();
    ALL.get_or_init(|| {
        let mut v: Vec<Preset> = FILES
            .iter()
            .filter_map(|(file, json)| match serde_json::from_str::<Preset>(json) {
                Ok(p) => Some(p),
                Err(e) => {
                    log::error!("theme {file}: {e}");
                    None
                }
            })
            .collect();
        let rank = |p: &Preset| match p.id.as_str() {
            DEFAULT_ID => 0,
            "classic" => 1,
            _ => 2,
        };
        v.sort_by_key(|p| (rank(p), p.name.to_lowercase()));
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

    /// Every theme file parses (unknown fields are refused), is named after its id and
    /// says what it comes from.
    #[test]
    fn themes_are_complete() {
        let mut ids = std::collections::HashSet::new();
        for (file, json) in FILES {
            let p: Preset = serde_json::from_str(json).unwrap_or_else(|e| panic!("{file}: {e}"));
            assert_eq!(p.id, *file, "{file}: id differs from the file name");
            assert!(ids.insert(p.id.clone()), "duplicate id {}", p.id);
            assert!(!p.name.is_empty() && !p.creators.is_empty() && !p.blurb.is_empty() && !p.source.is_empty(), "{file} lacks name/creators/blurb/source");
            assert!(matches!(p.accent.as_str(), "cover" | "system") || hex(&p.accent).is_some(), "{file} accent {}", p.accent);
            for c in [&p.background.solid, &p.app.text_dim] {
                assert!(hex(c).is_some(), "{file}: bad colour {c}");
            }
            let l = &p.lyrics;
            assert!(l.font_rem.windows(2).all(|w| w[0][0] < w[1][0]), "{file}: font_rem out of order");
            assert!(!l.font_rem.is_empty() || l.font_vw.is_some(), "{file}: no lyric size");
            assert!(p.cover.spin_secs > 0.0 && l.rem > 0.0, "{file}: zero rem or spin");
        }
        assert_eq!(all().len(), FILES.len());
        assert_eq!(get("nope").id, DEFAULT_ID);
        assert_eq!(all()[0].id, DEFAULT_ID);
        assert_eq!(all()[1].id, "classic");
    }

    /// Every theme has a cover for its card in Settings.
    #[test]
    fn themes_have_covers() {
        for p in all().iter().filter(|p| p.source != "ytm-rs") {
            assert!(cover(&p.id).is_some(), "{} has no cover", p.id);
        }
    }

    /// No theme gets a feature by name: nothing outside this file's tests looks at an id.
    #[test]
    fn nothing_branches_on_a_theme() {
        let src = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let mut stack = vec![std::path::PathBuf::from(src)];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let path = e.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|x| x == "rs") && !path.ends_with("preset.rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    for (file, _) in FILES {
                        assert!(!text.contains(&format!("id == \"{file}\"")), "{} branches on the theme {file}", path.display());
                    }
                    assert!(!text.contains("has_light"), "{} has a per-theme switch", path.display());
                    assert!(!text.contains("default_theme") || path.parent().is_some_and(|d| d.ends_with("ui")) && path.ends_with("mod.rs"), "{} has a per-theme switch", path.display());
                }
            }
        }
    }

    #[test]
    fn a_light_colour_is_derived_when_a_theme_has_none() {
        let l = derived_light(Color32::from_rgb(0, 0, 0));
        assert!(l.r() >= 243 && l.g() >= 243 && l.b() >= 243);
        assert_eq!(hex("#ff0000"), Some(Color32::from_rgb(255, 0, 0)));
    }
}
