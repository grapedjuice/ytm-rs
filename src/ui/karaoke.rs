//! Synced lyrics, ported from Better Lyrics' renderer (`@braccato/core` 1.16: engine.js,
//! inject.js, instrumental.js, lyrics.css, variables.css) to egui.
//!
//! What is the same as the extension, by construction:
//! - Timing: the clock is shown 150 ms ahead for word-synced lyrics and 115 ms for
//!   line-synced ones, and the view scrolls 0.5 s ahead of that.
//! - Words: every provider part is split on whitespace with its time pro-rated by
//!   characters; words between spaces form groups that never wrap unless longer than 20
//!   characters.
//! - Karaoke: each word has a lit copy over the dim one, revealed by a linear gradient
//!   (soft edge one tenth of the word wide) that runs from -0.2..-0.1 to 1.4..1.5 over 1.6×
//!   the word's duration, 10 % ahead of it. With letters split (the default "letter
//!   wave"), the gradient advances letter by letter in index space, each letter floats up
//!   as it is lit, and the word group gives a small sideways pop.
//! - Glow: `drop-shadow(0 0 0.8rem glow-color)` on the lit copy, decaying to nothing over
//!   max(1.2×, 1.2 s) with `ease`: a Gaussian blur of the lit glyph pixels only (never a box).
//! - Lines: scale 0.95 → 1 over 166 ms when they start, back when they end; lit words
//!   fade back to dim over 0.5 s once the next line starts.
//! - Scrolling: the view jumps to the new position and every line eases from where it was,
//!   lines further down taking a little longer (cubic-bezier(0.86, 0, 0.2, 1), 750 ms +).

use std::collections::HashMap;
use std::sync::Arc;

use egui::epaint::Vertex;
use egui::epaint::text::FontsView;
use egui::layers::ShapeIdx;
use egui::{Color32, ColorImage, FontId, Galley, Mesh, Pos2, Rect, Shape, TextureHandle, TextureId, TextureOptions, Vec2, pos2, vec2};

use crate::lyrics::{Line, Lyrics, Part, Sync};
use crate::preset::{AgentAlign, Fill, Instrumental, LetterWave, LyricStyle, Timing, Weight, WordBounce};
use crate::theme;

// ------------------------------------------------------------------ easing

/// A CSS `cubic-bezier()` timing function.
#[derive(Clone, Copy, Debug)]
pub struct Bezier(pub f32, pub f32, pub f32, pub f32);

impl Bezier {
    pub fn at(self, x: f32) -> f32 {
        if x <= 0.0 {
            return 0.0;
        }
        if x >= 1.0 {
            return 1.0;
        }
        let Bezier(x1, y1, x2, y2) = self;
        let curve = |t: f32, a: f32, b: f32| {
            let u = 1.0 - t;
            3.0 * u * u * t * a + 3.0 * u * t * t * b + t * t * t
        };
        let slope = |t: f32, a: f32, b: f32| {
            let u = 1.0 - t;
            3.0 * u * u * a + 6.0 * u * t * (b - a) + 3.0 * t * t * (1.0 - b)
        };
        let mut t = x;
        for _ in 0..8 {
            let e = curve(t, x1, x2) - x;
            if e.abs() < 1e-6 {
                return curve(t, y1, y2);
            }
            let d = slope(t, x1, x2);
            if d.abs() < 1e-6 {
                break;
            }
            t = (t - e / d).clamp(0.0, 1.0);
        }
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        t = x;
        for _ in 0..40 {
            let v = curve(t, x1, x2);
            if (v - x).abs() < 1e-6 {
                break;
            }
            if v < x {
                lo = t;
            } else {
                hi = t;
            }
            t = (lo + hi) * 0.5;
        }
        curve(t, y1, y2)
    }
}

pub const EASE: Bezier = Bezier(0.25, 0.1, 0.25, 1.0);
pub const EASE_IN: Bezier = Bezier(0.42, 0.0, 1.0, 1.0);
pub const EASE_OUT: Bezier = Bezier(0.0, 0.0, 0.58, 1.0);
pub const EASE_IN_OUT: Bezier = Bezier(0.42, 0.0, 0.58, 1.0);
/// `blyrics-decoration-in`.
const DECO_EASE: Bezier = Bezier(0.25, 1.0, 0.5, 1.0);

// ------------------------------------------------------------------ theme values

/// `--blyrics-richsync-timing-offset` / `--blyrics-timing-offset`.
pub const RICHSYNC_OFFSET: f64 = 0.150;
pub const LINE_OFFSET: f64 = 0.115;
/// `blyrics-lyric-ending-threshold-s`.
const ENDING_THRESHOLD: f64 = 0.5;
const TIME_JUMP: f64 = 0.5;
/// `blyrics-swipe-lead-ratio`, `blyrics-swipe-duration-ratio` and the swipe ramp.
const SWIPE_LEAD: f64 = 0.1;
const SWIPE_DURATION: f64 = 1.6;
const SWIPE_START: (f32, f32) = (-0.2, 1.4);
const SWIPE_END: (f32, f32) = (-0.1, 1.5);
const GLOW_RATIO: f64 = 1.2;
const GLOW_MIN: f64 = 1.2;
const FADE_OUT: f64 = 0.5;
const LETTER_WAVE_CREST: f32 = 0.4;
/// `blyrics-long-word-threshold` (ms → s): held words swell their letters.
const LONG_WORD: f64 = 1.5;
/// `blyrics-line-synced-animation-delay`.
#[cfg(test)]
const LINE_SYNC_DELAY: f64 = 0.05;
/// Word groups longer than twice `blyrics-long-word-wrap-threshold` may break.
const LONG_GROUP_CHARS: usize = 20;
const USER_SCROLL_RESUME: f64 = 25.0;
const DECO_ENTRY: f64 = 0.35;

/// Resolved look for one frame: the theme's values, at this window's size.
#[derive(Clone)]
struct Cfg {
    font: String,
    weight: Weight,
    size: f32,
    /// One CSS rem in points.
    rem: f32,
    active: Color32,
    /// Alpha of unsung words, before their line's opacity.
    dim_alpha: f32,
    glow: Color32,
    glow_rem: f32,
    scale: f32,
    active_scale: f32,
    scale_secs: f64,
    hover_scale: Option<f32>,
    hover_reveal: bool,
    padding: f32,
    line_height: f32,
    /// `letter-spacing` in points.
    spacing: f32,
    target_ratio: f32,
    align: crate::preset::Align,
    agent_align: AgentAlign,
    centered: bool,
    fill: Fill,
    wave: LetterWave,
    wobble: bool,
    wobble_secs: f64,
    bounce: Option<WordBounce>,
    rise_px: f32,
    rise_secs: f64,
    reveal_dim: Option<f32>,
    line_fade: f64,
    sung_fade: Option<[f32; 2]>,
    /// By distance from the active line: (opacity, blur).
    past: Vec<[f32; 2]>,
    upcoming: Vec<[f32; 2]>,
    past_lines: Vec<[f32; 2]>,
    upcoming_lines: Vec<[f32; 2]>,
    past_dim: Option<f32>,
    past_color: Option<Color32>,
    opacity_t: Timing,
    blur_t: Timing,
    scroll_opacity: f32,
    agent_colors: Vec<Color32>,
    dim_from_agent: bool,
    band: Vec<Color32>,
    band_width: f32,
    stroke: Option<(f32, Color32)>,
    bg_em: f32,
    roman_rem: f32,
    roman_alpha: f32,
    roman_pill: bool,
    trans_rem: f32,
    trans_alpha: f32,
    instrumental: Instrumental,
    aura: Option<[f32; 2]>,
    marker: Option<[f32; 4]>,
    line_exit: Option<[f32; 2]>,
    scroll_secs: f64,
    scroll_ease: Bezier,
    scroll_offset: f64,
    early_scroll: f64,
    /// `.blyrics-container` inline padding (start, end).
    pad: (f32, f32),
    /// `--blyrics-glow-color` alpha on the container and on held words.
    glow_alpha: f32,
    long_glow_alpha: f32,
    line_ease: Option<Bezier>,
    past_op_t: Option<Timing>,
    past_blur_t: Option<Timing>,
    active_op_t: Option<Timing>,
    active_blur_t: Option<Timing>,
    pre_sharp: f64,
    active_op: f32,
    active_op_lines: f32,
    scroll_scale: Option<f32>,
    hover_op: Option<f32>,
    hover_box: Option<[f32; 2]>,
    scale_ease: Bezier,
    scale_exit_ease: Bezier,
    line_enter: Option<[f32; 2]>,
    word_rest_scale: f32,
    word_gap: f32,
    word_shadow: Option<[f32; 2]>,
    sweep_lead: f64,
    sweep_ratio: f64,
    glow_ratio: f64,
    glow_min: f64,
    bg_alpha: f32,
    bg_weight: Weight,
    roman_weight: Weight,
    roman_family: Option<String>,
    trans_weight: Weight,
    trans_family: Option<String>,
    both_rem: Option<[f32; 2]>,
    inst_rem: Option<f32>,
    breathe: Option<[f32; 3]>,
    inst_chip: Option<f32>,
    credits_alpha: [f32; 2],
    edge_fade: [f32; 2],
    balance: bool,
    deco_tracking: f32,
    exit_fade: (f64, f64),
    agent_glow: Vec<Color32>,
    aura_colors: Vec<Color32>,
    marker_colors: Vec<Color32>,
    active_glow: Option<[f32; 2]>,
    active_glow_colors: Vec<Color32>,
    hover_glow: Option<[f32; 2]>,
    rest_saturate: f32,
    scroll_sides: [f32; 2],
    scroll_curve: Option<[f32; 5]>,
    line_stagger: f64,
    deco_enter: [f32; 4],
    sweep_soft: Option<f32>,
    sweep_end: (f32, f32),
    hover_dim: Option<f32>,
    hover_box_line: f32,
    hover_box_pop: Option<[f32; 2]>,
    marker_hover: f32,
    glow_ease: Bezier,
    glow_fade: f64,
    bg_gap: f32,
    bg_sung: Option<f32>,
    hover_unsung: Option<f32>,
    hover_glow_sung: bool,
    panel_reveal: bool,
    deco_active: Option<f32>,
    deco_em: Option<f32>,
    deco_lh: Option<f32>,
    bg_lit: Option<f32>,
    trans_delay: f32,
    exit_by_side: bool,
    blur_from: Option<f32>,
    marker_alt: Vec<Color32>,
    saturate_far: Option<f32>,
    shadow_blur: f32,
    shadow_unsung: bool,
    deco_gap: Option<f32>,
    padding_top: f32,
    credits_center: bool,
    credits_weight: Weight,
    credits_margin: f32,
    inst_glow: f32,
    dot_em: Option<[f32; 2]>,
    ring_wave: bool,
    scroll_t: Option<Timing>,
    scroll_op_t: Option<Timing>,
    rise_ratio: f64,
    rest_em: f32,
}

/// `GLOW_INVISIBLE_ALPHA`: a glow colour this faint renders nothing and is skipped.
const GLOW_INVISIBLE: f32 = 0.5 / 255.0;

/// `[opacity, blur]` at `dist` lines away (1: the next one), the last entry going on.
fn ladder(table: &[[f32; 2]], dist: usize) -> (f32, f32) {
    table.get(dist.saturating_sub(1)).or(table.last()).map_or((1.0, 0.0), |e| (e[0], e[1]))
}

impl Cfg {
    /// The most a line is scaled up: while sung, or under a theme's hover scale.
    fn max_scale(&self) -> f32 {
        self.active_scale.max(self.hover_scale.unwrap_or(1.0)).max(1.0)
    }

    /// `width`: the lyrics panel; `viewport`: the window (the theme's media queries).
    #[allow(clippy::too_many_arguments)]
    fn new(s: &LyricStyle, accent: Color32, width: f32, viewport: f32, viewport_h: f32, centered: bool, scale: f32, fullscreen: bool) -> Self {
        // Romanization and translation look the same in every theme: the app's own values.
        let deco = &crate::preset::get(crate::preset::DEFAULT_ID).lyrics;
        let mut rem = s.rem;
        let steps = s.font_rem.iter().filter(|st| viewport >= st[0]).map(|st| st[1]).next_back();
        let vw_step = s.font_vw_steps.iter().find(|st| viewport < st[0]).map(|st| [st[1], st[2], st[3]]).or(s.font_vw);
        let full_vw = s.font_vw_full.filter(|_| fullscreen);
        let step = match full_vw.or(vw_step) {
            Some([lo, vw, hi]) => (viewport * vw / 100.0 / rem).clamp(lo, hi),
            None => steps.or(s.font_rem.first().map(|st| st[1])).unwrap_or(3.0),
        };
        let mut size = step * rem * if fullscreen && full_vw.is_none() { s.fullscreen_scale } else { 1.0 };
        // Kept to about a dozen CJK characters per row on narrow windows.
        let cap = (width / 12.0).max(22.0);
        if size > cap {
            rem *= cap / size;
            size = cap;
        }
        // The listener's text size goes on top, everything in rem along with it
        // (romanization, translation, spacing), so the theme keeps its proportions.
        // Whole points: every new size fills the font atlas again (it never shrinks),
        // and resizing the window would otherwise make one per pixel of width.
        let rounded = (size * scale).round().max(1.0);
        rem *= rounded / size;
        size = rounded;
        let colour = |c: &str| crate::preset::hex(c);
        Self {
            font: theme::effective_font(&s.font),
            weight: s.weight,
            size,
            rem,
            active: theme::with_alpha(crate::preset::paint(&s.sung_color, accent), s.active_alpha),
            dim_alpha: s.dim_alpha,
            glow: crate::preset::paint(&s.glow_color, accent),
            glow_rem: s.glow_rem,
            scale: s.rest_scale,
            active_scale: s.active_scale.max(0.1),
            scale_secs: s.scale_secs.max(0.001) as f64,
            hover_scale: s.hover_scale,
            hover_reveal: s.hover_reveal,
            padding: match s.line_padding_vh {
                Some([lo, vh, hi]) => (viewport_h * vh / 100.0).clamp(lo, hi),
                None => rem * if fullscreen { s.fullscreen_padding_rem.unwrap_or(s.line_padding_rem) } else { s.line_padding_rem },
            },
            line_height: s.line_height,
            spacing: s.letter_spacing_em * size,
            target_ratio: s.anchor.clamp(0.1, 0.7),
            align: s.align,
            agent_align: s.agent_align,
            centered,
            fill: s.fill,
            wave: s.letter_wave,
            wobble: s.wobble,
            wobble_secs: s.wobble_secs.max(0.001) as f64,
            bounce: s.word_bounce,
            rise_px: s.word_rise_px * rem / s.rem.max(0.1),
            rise_secs: s.word_rise_secs.max(0.01) as f64,
            reveal_dim: s.reveal_dim_alpha,
            line_fade: s.line_fade_secs.max(0.01) as f64,
            sung_fade: s.sung_fade,
            past: s.past.clone(),
            upcoming: s.upcoming.clone(),
            past_lines: if s.past_line_synced.is_empty() { s.past.clone() } else { s.past_line_synced.clone() },
            upcoming_lines: if s.upcoming_line_synced.is_empty() { s.upcoming.clone() } else { s.upcoming_line_synced.clone() },
            past_dim: s.past_dim_alpha,
            past_color: colour(&s.past_color),
            opacity_t: s.opacity_transition,
            blur_t: s.blur_transition,
            scroll_opacity: s.scroll_opacity,
            agent_colors: s.agent_colors.iter().map(|c| crate::preset::paint(c, accent)).collect(),
            dim_from_agent: s.dim_from_agent,
            band: s.sweep_band.iter().map(|c| crate::preset::paint(c, accent)).collect(),
            band_width: s.sweep_band_width,
            stroke: (s.stroke_em > 0.0).then(|| (s.stroke_em * size, crate::preset::paint(&s.stroke_color, accent))),
            bg_em: s.background_em,
            roman_rem: deco.romanization_rem * if fullscreen { deco.decor_fullscreen_scale } else { 1.0 },
            roman_alpha: deco.romanization_alpha,
            roman_pill: deco.romanization_pill,
            trans_rem: deco.translation_rem * if fullscreen { deco.decor_fullscreen_scale } else { 1.0 },
            trans_alpha: deco.translation_alpha,
            instrumental: s.instrumental,
            aura: s.aura,
            marker: s.marker_px,
            line_exit: s.line_exit,
            scroll_secs: s.scroll_secs as f64,
            scroll_ease: Bezier(s.scroll_ease[0], s.scroll_ease[1], s.scroll_ease[2], s.scroll_ease[3]),
            scroll_offset: s.scroll_offset as f64,
            early_scroll: s.early_scroll as f64,
            pad: match (s.fullscreen_inline.filter(|_| fullscreen), s.padding_vw) {
                (Some([a, b]), _) => (a, b),
                (None, Some([lo, vw, hi])) => {
                    let p = (viewport * vw / 100.0).clamp(lo, hi);
                    (p, p)
                }
                _ => (s.padding_inline[0] * rem / 10.0, s.padding_inline[1] * rem / 10.0),
            },
            glow_alpha: s.glow_alpha,
            long_glow_alpha: s.long_glow_alpha,
            line_ease: s.line_ease.map(|e| Bezier(e[0], e[1], e[2], e[3])),
            past_op_t: s.past_opacity_transition,
            past_blur_t: s.past_blur_transition,
            active_op_t: s.active_opacity_transition,
            active_blur_t: s.active_blur_transition,
            pre_sharp: s.pre_sharp_secs as f64,
            active_op: s.active_opacity,
            active_op_lines: s.active_opacity_line_synced.unwrap_or(s.active_opacity),
            scroll_scale: s.scroll_scale,
            hover_op: s.hover_opacity,
            hover_box: s.hover_box,
            scale_ease: s.scale_ease.map_or(EASE, |e| Bezier(e[0], e[1], e[2], e[3])),
            scale_exit_ease: s.scale_exit_ease.or(s.scale_ease).map_or(EASE, |e| Bezier(e[0], e[1], e[2], e[3])),
            line_enter: s.line_enter,
            word_rest_scale: s.word_rest_scale,
            word_gap: s.word_gap_em * size,
            word_shadow: s.word_shadow,
            sweep_lead: s.sweep_lead as f64,
            sweep_ratio: s.sweep_ratio.max(0.01) as f64,
            glow_ratio: s.glow_ratio as f64,
            glow_min: s.glow_min_secs as f64,
            bg_alpha: s.background_alpha,
            bg_weight: s.background_weight,
            roman_weight: deco.romanization_weight,
            roman_family: (!deco.romanization_font.is_empty()).then(|| theme::effective_font(&deco.romanization_font)),
            trans_weight: deco.translation_weight,
            trans_family: (!deco.translation_font.is_empty()).then(|| theme::effective_font(&deco.translation_font)),
            both_rem: deco.both_rem,
            inst_rem: s.instrumental_rem,
            breathe: s.instrumental_breathe,
            inst_chip: s.instrumental_chip,
            credits_alpha: s.credits_alpha,
            edge_fade: s.edge_fade,
            balance: s.balance,
            deco_tracking: deco.decor_tracking_em,
            exit_fade: s.exit_fade.map_or((FADE_OUT, 0.0), |[secs, delay]| (secs.max(0.01) as f64, delay as f64)),
            agent_glow: s.agent_glow.iter().map(|c| crate::preset::paint(c, accent)).collect(),
            aura_colors: s.aura_colors.iter().map(|c| crate::preset::paint(c, accent)).collect(),
            marker_colors: s.marker_colors.iter().map(|c| crate::preset::paint(c, accent)).collect(),
            active_glow: s.active_glow,
            active_glow_colors: s.active_glow_colors.iter().map(|c| crate::preset::paint(c, accent)).collect(),
            hover_glow: s.hover_glow,
            rest_saturate: s.rest_saturate,
            scroll_sides: s.scroll_sides,
            scroll_curve: s.scroll_curve,
            line_stagger: s.line_stagger as f64,
            deco_enter: deco.deco_enter.unwrap_or([-0.4, 0.0, DECO_ENTRY as f32, 0.0]),
            sweep_soft: s.sweep_soft,
            sweep_end: s.sweep_end.map_or((SWIPE_START.1, SWIPE_END.1), |e| (e[0], e[1])),
            hover_dim: s.hover_dim_alpha,
            hover_box_line: s.hover_box_line,
            hover_box_pop: s.hover_box_pop,
            marker_hover: s.marker_hover,
            glow_ease: s.glow_ease.map_or(EASE, |e| Bezier(e[0], e[1], e[2], e[3])),
            glow_fade: s.glow_fade_secs as f64,
            bg_gap: s.background_gap_em,
            bg_sung: s.background_sung_alpha,
            hover_unsung: s.hover_unsung_alpha,
            hover_glow_sung: s.hover_glow_sung_only,
            panel_reveal: s.panel_reveal,
            deco_active: deco.decor_active_alpha,
            deco_em: deco.decor_em,
            deco_lh: deco.decor_line_height,
            bg_lit: s.background_lit_alpha,
            trans_delay: deco.translation_delay,
            exit_by_side: s.line_exit_by_side,
            blur_from: s.active_blur_from,
            marker_alt: s.marker_colors_alt.iter().map(|c| crate::preset::paint(c, accent)).collect(),
            saturate_far: s.rest_saturate_far,
            shadow_blur: s.word_shadow_blur,
            shadow_unsung: s.word_shadow_unsung,
            deco_gap: deco.decor_gap_em,
            padding_top: s.padding_top,
            credits_center: s.credits_center,
            credits_weight: s.credits_weight,
            credits_margin: s.credits_margin_rem * rem,
            inst_glow: s.instrumental_glow,
            dot_em: s.dot_em,
            ring_wave: s.ring_wave,
            scroll_t: s.scroll_transition,
            scroll_op_t: s.scroll_opacity_transition,
            rise_ratio: s.word_rise_ratio as f64,
            rest_em: s.word_rest_em,
        }
    }
    /// The unit romanization and translation are sized in: a third of the line, as in the
    /// default look, so they keep the same proportion to it whatever the theme's own rem.
    fn deco_rem(&self) -> f32 {
        self.size / 3.0
    }
    fn rem(&self) -> f32 {
        self.rem
    }
    /// The break's icon, across.
    fn inst_em(&self) -> f32 {
        self.inst_rem.map_or(self.size, |r| r * self.rem)
    }
    fn line_h(&self) -> f32 {
        self.size * self.line_height
    }
    /// The colours a singer's words use: (lit, unsung tint).
    fn agent_color(&self, agent: Option<&str>) -> (Color32, Color32) {
        let c = agent.and_then(|a| a.strip_prefix('v')?.parse::<usize>().ok()).and_then(|n| self.agent_colors.get(n.checked_sub(1)?).copied());
        match c {
            Some(c) => (c, if self.dim_from_agent { c } else { Color32::WHITE }),
            None => (self.active, Color32::WHITE),
        }
    }
    // Each takes the text it sets, so a line in a script the font lacks is all Inter.
    fn main_font(&self, text: &str) -> FontId {
        theme::lyric_font_for(&self.font, self.weight, self.size, text)
    }
    fn bg_font(&self, text: &str) -> FontId {
        theme::lyric_font_for(&self.font, self.bg_weight, self.size * self.bg_em, text)
    }
    /// `both`: the line has a romanization and a translation.
    fn roman_font(&self, text: &str, both: bool) -> FontId {
        let rem = self.both_rem.filter(|_| both).map_or(self.roman_rem, |b| b[0]);
        let px = self.deco_em.map_or(self.deco_rem() * rem, |em| self.size * em);
        theme::lyric_font_for(self.roman_family.as_ref().unwrap_or(&self.font), self.roman_weight, px, text)
    }
    fn trans_font(&self, text: &str, both: bool) -> FontId {
        let rem = self.both_rem.filter(|_| both).map_or(self.trans_rem, |b| b[1]);
        let px = self.deco_em.map_or(self.deco_rem() * rem, |em| self.size * em);
        theme::lyric_font_for(self.trans_family.as_ref().unwrap_or(&self.font), self.trans_weight, px, text)
    }
}

// ------------------------------------------------------------------ layout

/// One animated word (`.blyrics--word`), relative to its line's content box.
#[derive(Clone)]
struct Word {
    text: String,
    font: FontId,
    start: f64,
    dur: f64,
    /// Zero-duration (line-synced) word: fades in instead of sweeping.
    line_synced: bool,
    bg: bool,
    /// Held longer than `LONG_WORD`: letters swell as they crest.
    long: bool,
    pos: Pos2,
    w: f32,
    h: f32,
    group: usize,
    letters: bool,
    /// Right-to-left letters: lit from the right.
    rtl: bool,
    /// `letter-spacing`, points.
    spacing: f32,
}

/// A word group (`.blyrics-word-group`): what wobbles, driven by its first word.
#[derive(Clone)]
struct Group {
    first: usize,
    words: std::ops::Range<usize>,
    /// Inline (long) groups can't be transformed.
    wobbles: bool,
}

#[derive(Clone)]
enum DecoBody {
    Timed { words: Vec<Word>, groups: Vec<Group> },
    Text(String, FontId),
}

#[derive(Clone)]
struct Deco {
    body: DecoBody,
    /// Box relative to the line's content origin (the pill, or the translation's text box).
    rect: Rect,
    pill: bool,
    /// Romanization, not a translation.
    roman: bool,
    /// Identifies what was shown, so a new decoration can animate in.
    id: u64,
    /// The width its text was measured at: drawn at any other it could wrap differently.
    wrap: f32,
}

#[derive(Clone)]
struct Block {
    /// Index into the lyrics' lines (`None` for the credits).
    line: Option<usize>,
    start: f64,
    dur: f64,
    instrumental: bool,
    /// `v2`/`v3` sing on the right, `v1000` (everyone) in the middle.
    align: Align,
    /// This singer's lit colour and unsung tint.
    colors: (Color32, Color32),
    top: f32,
    height: f32,
    words: Vec<Word>,
    groups: Vec<Group>,
    decos: Vec<Deco>,
    /// Songwriter credits text.
    credits: Option<(String, FontId)>,
}

#[derive(Clone, Copy, PartialEq)]
enum Align {
    Left,
    Right,
    Center,
}

struct Layout {
    key: u64,
    width: f32,
    blocks: Vec<Block>,
    pad_top: f32,
    content_h: f32,
    richsync: bool,
}

/// A whitespace run or a piece of a part (`splitPartIntoTokens`).
#[derive(Clone)]
struct Tok {
    text: String,
    start: f64,
    dur: f64,
    bg: bool,
    space: bool,
}

fn split_part(p: &Part) -> Vec<Tok> {
    let mut chunks: Vec<(String, bool)> = Vec::new();
    for c in p.text.chars() {
        let ws = c.is_whitespace();
        match chunks.last_mut() {
            Some((s, w)) if *w == ws => s.push(c),
            _ => chunks.push((c.to_string(), ws)),
        }
    }
    let total: usize = chunks.iter().filter(|(_, w)| !*w).map(|(s, _)| s.chars().count()).sum();
    let mut cursor = 0usize;
    chunks
        .into_iter()
        .map(|(text, space)| {
            if space {
                return Tok { text, start: 0.0, dur: 0.0, bg: p.bg, space: true };
            }
            let n = text.chars().count();
            let (start_ms, end_ms) = if total == 0 {
                (p.start as f64, p.start as f64)
            } else {
                (
                    p.start as f64 + (p.dur as f64 * cursor as f64 / total as f64).round(),
                    p.start as f64 + (p.dur as f64 * (cursor + n) as f64 / total as f64).round(),
                )
            };
            cursor += n;
            Tok { text, start: start_ms / 1000.0, dur: (end_ms - start_ms) / 1000.0, bg: p.bg, space: false }
        })
        .collect()
}

/// `buildLineSyncedParts`: a line without timed parts becomes zero-duration words, each
/// 50 ms after the one before.
#[cfg(test)]
fn line_synced_parts(l: &Line) -> Vec<Part> {
    line_synced_parts_with(l, LINE_SYNC_DELAY)
}

/// Words of a line with no word timing, `delay` seconds apart.
fn line_synced_parts_with(l: &Line, delay: f64) -> Vec<Part> {
    let mut out = Vec::new();
    let mut word = 0;
    let mut chunks: Vec<(String, bool)> = Vec::new();
    for c in l.text.chars() {
        let ws = c.is_whitespace();
        match chunks.last_mut() {
            Some((s, w)) if *w == ws => s.push(c),
            _ => chunks.push((c.to_string(), ws)),
        }
    }
    for (text, ws) in chunks {
        let start = l.start + (word as f64 * delay * 1000.0) as i32;
        out.push(Part { start, dur: 0, text, bg: false });
        if !ws {
            word += 1;
        }
    }
    out
}

/// Joining scripts (Arabic and friends) keep whole-word sweeps, as in the extension.
fn joining_script(s: &str) -> bool {
    s.chars().any(|c| matches!(c as u32, 0x0600..=0x06FF | 0x0700..=0x074F | 0x0750..=0x077F | 0x0780..=0x07BF | 0x0840..=0x085F | 0x08A0..=0x08FF | 0x1800..=0x18AF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF))
}

struct Flow {
    words: Vec<Word>,
    groups: Vec<Group>,
    height: f32,
    width: f32,
}

/// Lay out tokens the way inline formatting does it in the extension: groups between
/// spaces don't break (unless long), spaces do.
fn text_size(fonts: &mut FontsView<'_>, s: &str, font: &FontId) -> Vec2 {
    fonts.layout_no_wrap(s.to_owned(), font.clone(), Color32::WHITE).size()
}

/// `rtl`: the line reads right to left, so each row is laid out from the right.
#[allow(clippy::too_many_arguments)]
fn flow(fonts: &mut FontsView<'_>, toks: Vec<Tok>, font: &FontId, max_w: f32, line_h: f32, align: Align, rtl: bool, spacing: f32, gap: f32) -> Flow {
    // `groupTokensByWord`
    enum Item {
        Space,
        Group(Vec<Tok>),
    }
    let mut items: Vec<Item> = Vec::new();
    // A single piece wider than the line (a line-synced CJK phrase arrives as one, with no
    // spaces to break at) is split into its characters, sharing its time, so it can wrap.
    let toks = toks.into_iter().flat_map(|t| {
        // Not right-to-left words either: their letters, laid out one by one, would read backwards.
        if t.space || joining_script(&t.text) || crate::bidi::has_rtl(&t.text) || t.text.chars().count() < 2 || text_size(fonts, &t.text, font).x <= max_w {
            return vec![t];
        }
        let n = t.text.chars().count();
        t.text
            .chars()
            .enumerate()
            .map(|(i, c)| Tok { text: c.to_string(), start: t.start + t.dur * i as f64 / n as f64, dur: t.dur / n as f64, bg: t.bg, space: false })
            .collect()
    });
    for t in toks {
        if t.space {
            items.push(Item::Space);
            continue;
        }
        match items.last_mut() {
            Some(Item::Group(g)) if g[0].bg == t.bg => g.push(t),
            _ => items.push(Item::Group(vec![t])),
        }
    }
    let space_w = text_size(fonts, " ", font).x.max(font.size * 0.25) + gap;
    let mut words: Vec<Word> = Vec::new();
    let mut groups: Vec<Group> = Vec::new();
    let mut rows: Vec<(usize, f32)> = vec![(0, 0.0)]; // (first word index, width)
    let (mut x, mut row) = (0.0f32, 0usize);
    let mut pending_space = false;
    for item in items {
        match item {
            Item::Space => pending_space = !words.is_empty(),
            Item::Group(toks) => {
                let text: String = toks.iter().map(|t| t.text.as_str()).collect();
                let long = text.chars().count() > LONG_GROUP_CHARS;
                let rtl_group = crate::bidi::has_rtl(&text);
                let letters = !joining_script(&text) && !rtl_group;
                let sizes: Vec<Vec2> = toks.iter().map(|t| text_size(fonts, &t.text, font) + vec2(spacing * t.text.chars().count() as f32, 0.0)).collect();
                let w: f32 = sizes.iter().map(|s| s.x).sum();
                let sp = if pending_space { space_w } else { 0.0 };
                pending_space = false;
                // A group that can't fit even on a row of its own breaks between its parts
                // rather than running off the side (the extension lets it overflow).
                let breakable = long || w > max_w;
                if x > 0.0 && x + sp + w > max_w && !breakable {
                    row += 1;
                    x = 0.0;
                    rows.push((words.len(), 0.0));
                } else {
                    x += sp;
                }
                let gi = groups.len();
                let first = words.len();
                for (t, size) in toks.iter().zip(sizes) {
                    if breakable && x > 0.0 && x + size.x > max_w {
                        row += 1;
                        x = 0.0;
                        rows.push((words.len(), 0.0));
                    }
                    words.push(Word {
                        text: t.text.clone(),
                        font: font.clone(),
                        start: t.start,
                        dur: t.dur,
                        line_synced: t.dur <= 0.0,
                        bg: t.bg,
                        long: t.dur > LONG_WORD,
                        pos: pos2(x, row as f32 * line_h + (line_h - size.y) * 0.5),
                        w: size.x,
                        h: size.y,
                        group: gi,
                        letters,
                        rtl: crate::bidi::has_rtl(&t.text),
                        spacing,
                    });
                    x += size.x;
                    rows[row].1 = x;
                }
                groups.push(Group { first, words: first..words.len(), wobbles: !long });
            }
        }
    }
    // Right to left: words go from the right of each row, except runs of left-to-right
    // ones (Latin, numbers), which keep their order; in a left-to-right line, the other
    // way round.
    for (r, &(first, rw)) in rows.iter().enumerate() {
        let end = rows.get(r + 1).map_or(words.len(), |n| n.0);
        let row = &mut words[first..end];
        if rtl {
            for w in row.iter_mut() {
                w.pos.x = rw - w.pos.x - w.w;
            }
        }
        let mut i = 0;
        while i < row.len() {
            let mut j = i;
            while j < row.len() && row[j].rtl != rtl {
                j += 1;
            }
            if j > i + 1 {
                let lo = row[i..j].iter().map(|w| w.pos.x).fold(f32::INFINITY, f32::min);
                let hi = row[i..j].iter().map(|w| w.pos.x + w.w).fold(f32::NEG_INFINITY, f32::max);
                for w in &mut row[i..j] {
                    w.pos.x = lo + hi - w.pos.x - w.w;
                }
            }
            i = j.max(i + 1);
        }
    }
    // Align rows to the right or centre.
    if align != Align::Left {
        for (r, &(first, rw)) in rows.iter().enumerate() {
            let end = rows.get(r + 1).map_or(words.len(), |n| n.0);
            let shift = if align == Align::Right { max_w - rw } else { (max_w - rw) * 0.5 };
            for w in &mut words[first..end] {
                w.pos.x += shift.max(0.0);
            }
        }
    }
    let width = rows.iter().map(|r| r.1).fold(0.0, f32::max);
    let height = if words.is_empty() { 0.0 } else { (row + 1) as f32 * line_h };
    Flow { words, groups, height, width }
}

/// Romanization for a line: timed (from the lyrics) or plain text.
pub enum Roman<'a> {
    Timed(&'a [Part], &'a str),
    Text(&'a str),
}

fn hash<T: std::hash::Hash>(v: T) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

/// `text-wrap: balance`: as narrow as the line goes in the same number of rows. Only when
/// a line is laid out, so it costs nothing per frame.
#[allow(clippy::too_many_arguments)]
fn balanced(fonts: &mut FontsView<'_>, toks: Vec<Tok>, font: &FontId, max_w: f32, line_h: f32, align: Align, rtl: bool, cfg: &Cfg) -> Flow {
    let full = flow(fonts, toks.clone(), font, max_w, line_h, align, rtl, cfg.spacing, cfg.word_gap);
    let rows = (full.height / line_h.max(1.0)).round() as i32;
    if rows < 2 {
        return full;
    }
    let (mut lo, mut hi, mut best) = (max_w / rows as f32, max_w, None);
    for _ in 0..6 {
        let mid = (lo + hi) * 0.5;
        let f = flow(fonts, toks.clone(), font, mid, line_h, align, rtl, cfg.spacing, cfg.word_gap);
        if (f.height / line_h.max(1.0)).round() as i32 == rows {
            hi = mid;
            best = Some((f, mid));
        } else {
            lo = mid;
        }
    }
    let Some((mut f, w)) = best else { return full };
    // Laid out in the narrower measure: back to the column's edge or centre.
    let dx = match align {
        Align::Left => 0.0,
        Align::Right => max_w - w,
        Align::Center => (max_w - w) * 0.5,
    };
    for word in &mut f.words {
        word.pos.x += dx;
    }
    f
}

/// Romanization, translation or credits in the theme's letter spacing (em), its rows set
/// to `align` within the galley's width.
fn deco_galley(fonts: &mut FontsView<'_>, text: String, font: FontId, wrap: f32, (tracking, line_height): (f32, Option<f32>), align: Align) -> Arc<Galley> {
    let rtl = crate::bidi::has_rtl(&text);
    if tracking == 0.0 && line_height.is_none() && !rtl {
        return align_rows(fonts.layout(text, font, Color32::WHITE, wrap), align);
    }
    let spacing = tracking * font.size;
    let height = line_height.map(|h| h * font.size);
    let styled = |job: &mut egui::text::LayoutJob| {
        for s in &mut job.sections {
            s.format.extra_letter_spacing = spacing;
            s.format.line_height = height;
        }
    };
    let mut job = egui::text::LayoutJob::simple(text.clone(), font.clone(), Color32::WHITE, wrap);
    styled(&mut job);
    let mut g = fonts.layout_job(job);
    if rtl {
        // As `widgets::galley_wrapped`: wrapped in reading order, then each row put in
        // display order on its own.
        let mut chars = text.chars();
        let rows: Vec<String> = g.rows.iter().map(|row| chars.by_ref().take(row.char_count_including_newline().0).collect::<String>().trim_end().to_owned()).collect();
        let mut job = crate::bidi::job(&rows.join("\n"), font, Color32::WHITE);
        styled(&mut job);
        g = fonts.layout_job(job);
    }
    align_rows(g, align)
}

/// A wrapped text's shorter rows moved to the right or centre of its widest.
fn align_rows(mut g: Arc<Galley>, align: Align) -> Arc<Galley> {
    if align == Align::Left || g.rows.len() < 2 {
        return g;
    }
    let width = g.size().x;
    for row in &mut Arc::make_mut(&mut g).rows {
        let used = row.row.glyphs.iter().rev().find(|gl| !gl.chr.is_whitespace()).map_or(0.0, |gl| gl.pos.x + gl.advance_width);
        let gap = (width - used).max(0.0);
        row.pos.x += if align == Align::Right { gap } else { gap * 0.5 };
    }
    g
}

#[allow(clippy::too_many_arguments)]
fn build_block(fonts: &mut FontsView<'_>, cfg: &Cfg, l: &Line, index: usize, width: f32, roman: Option<Roman>, trans: Option<&str>, agent: Option<&str>) -> Block {
    let em = cfg.size;
    let rtl = crate::bidi::rtl_base(&l.text);
    let own = match cfg.align {
        _ if cfg.centered => Align::Center,
        crate::preset::Align::Left => Align::Left,
        crate::preset::Align::Center => Align::Center,
        crate::preset::Align::Right => Align::Right,
    };
    let align = match (cfg.agent_align, agent) {
        (AgentAlign::Stock, Some("v2" | "v3")) => Align::Right,
        (AgentAlign::Stock, Some("v1000")) => Align::Center,
        (AgentAlign::OnlyV1Left, Some("v1") | None) => Align::Left,
        (AgentAlign::OnlyV1Left, Some(_)) => Align::Right,
        _ => own,
    };
    // Right-to-left lines start on the right (and the second singer's on the left).
    let align = match align {
        Align::Left if rtl => Align::Right,
        Align::Right if rtl => Align::Left,
        a => a,
    };
    // Wrapped to fit at the line's largest scale (sung, or a theme's hover), which grows
    // it about its aligned edge: wrapped at full width, a sung line ran off the side.
    let full_w = (width - em * 0.5).max(40.0);
    let content_w = (full_w / cfg.max_scale()).max(40.0);
    let line_h = cfg.line_h();
    let mut y = cfg.padding;
    let mut block = Block {
        line: Some(index),
        start: l.start as f64 / 1000.0,
        dur: l.dur.max(0) as f64 / 1000.0,
        instrumental: l.instrumental,
        align,
        colors: cfg.agent_color(agent),
        top: 0.0,
        height: 0.0,
        words: vec![],
        groups: vec![],
        decos: vec![],
        credits: None,
    };
    if l.instrumental {
        block.height = cfg.padding * 2.0 + line_h;
        return block;
    }
    let parts = if l.parts.is_empty() { line_synced_parts_with(l, cfg.line_stagger) } else { l.parts.clone() };
    let toks: Vec<Tok> = parts.iter().flat_map(split_part).collect();
    let (main, bg): (Vec<Tok>, Vec<Tok>) = toks.into_iter().partition(|t| !t.bg);
    let joined = |toks: &[Tok]| toks.iter().map(|t| t.text.as_str()).collect::<String>();
    let font = cfg.main_font(&joined(&main));
    let f = if cfg.balance { balanced(fonts, main, &font, content_w, line_h, align, rtl, cfg) } else { flow(fonts, main, &font, content_w, line_h, align, rtl, cfg.spacing, cfg.word_gap) };
    for w in f.words {
        block.words.push(Word { pos: w.pos + vec2(0.0, y), ..w });
    }
    block.groups = f.groups;
    y += f.height;
    if bg.iter().any(|t| !t.space) {
        y += em * cfg.bg_gap;
        let base = block.words.len();
        let g0 = block.groups.len();
        let font = cfg.bg_font(&joined(&bg));
        let f = flow(fonts, bg, &font, content_w, line_h, align, rtl, cfg.spacing * cfg.bg_em, cfg.word_gap * cfg.bg_em);
        for w in f.words {
            block.words.push(Word { pos: w.pos + vec2(0.0, y), group: w.group + g0, ..w });
        }
        block.groups.extend(f.groups.into_iter().map(|g| Group { first: g.first + base, words: g.words.start + base..g.words.end + base, ..g }));
        y += f.height;
    }
    let gap = cfg.deco_gap.map_or(em / 6.0, |g| g * em); // margin-top: 8px at 48 px
    let both = roman.is_some() && trans.is_some();
    if let Some(r) = roman {
        y += gap;
        let pad = vec2(cfg.deco_rem() * 0.75, cfg.deco_rem() * 0.25);
        let inner_w = content_w - pad.x * 2.0 - 2.0;
        let (body, inner, id) = match r {
            Roman::Timed(parts, text) => {
                let toks: Vec<Tok> = parts.iter().flat_map(split_part).collect();
                let font = cfg.roman_font(text, both);
                let lh = fonts.row_height(&font);
                let f = flow(fonts, toks, &font, inner_w, lh, Align::Left, false, 0.0, 0.0);
                (DecoBody::Timed { words: f.words, groups: f.groups }, vec2(f.width, f.height), hash(("rt", text)))
            }
            Roman::Text(t) => {
                let font = cfg.roman_font(t, both);
                let g = deco_galley(fonts, t.to_owned(), font.clone(), inner_w, (cfg.deco_tracking, cfg.deco_lh), align);
                (DecoBody::Text(t.to_owned(), font), g.size(), hash(("r", t)))
            }
        };
        let size = inner + pad * 2.0 + vec2(2.0, 2.0);
        let x = match align {
            Align::Left => 0.0,
            Align::Right => content_w - size.x,
            Align::Center => (content_w - size.x) * 0.5,
        };
        let rect = Rect::from_min_size(pos2(x, y), size);
        // Timed words sit inside the pill's padding.
        let body = match body {
            DecoBody::Timed { words, groups } => DecoBody::Timed { words: words.into_iter().map(|w| Word { pos: w.pos + rect.min.to_vec2() + pad + vec2(1.0, 1.0), ..w }).collect(), groups },
            b => b,
        };
        block.decos.push(Deco { body, rect, pill: cfg.roman_pill, roman: true, id, wrap: inner_w });
        y += size.y;
    }
    if let Some(t) = trans {
        y += gap;
        let font = cfg.trans_font(t, both);
        let g = deco_galley(fonts, t.to_owned(), font.clone(), content_w, (cfg.deco_tracking, cfg.deco_lh), align);
        let x = match align {
            Align::Left => 0.0,
            Align::Right => content_w - g.size().x,
            Align::Center => (content_w - g.size().x) * 0.5,
        };
        block.decos.push(Deco { body: DecoBody::Text(t.to_owned(), font), rect: Rect::from_min_size(pos2(x, y), g.size()), pill: false, roman: false, id: hash(("t", t)), wrap: content_w });
        y += g.size().y;
    }
    block.height = y + cfg.padding;
    if !cfg.wave.on {
        for w in &mut block.words {
            w.letters = false;
        }
    }
    for w in &mut block.words {
        w.long = w.dur > cfg.wave.long_secs as f64;
    }
    if cfg.wave.min_secs > 0.0 || cfg.wave.max_letters > 0 {
        for w in &mut block.words {
            w.letters &= w.dur >= cfg.wave.min_secs as f64 && (cfg.wave.max_letters == 0 || w.text.chars().count() <= cfg.wave.max_letters);
        }
    }
    // Back to the column's edge or centre, which the narrower wrap moved.
    let inset = match align {
        Align::Left => 0.0,
        Align::Right => full_w - content_w,
        Align::Center => (full_w - content_w) * 0.5,
    };
    if inset > 0.0 {
        let dx = vec2(inset, 0.0);
        for w in &mut block.words {
            w.pos += dx;
        }
        for d in &mut block.decos {
            d.rect = d.rect.translate(dx);
            if let DecoBody::Timed { words, .. } = &mut d.body {
                for w in words {
                    w.pos += dx;
                }
            }
        }
    }
    block
}

// ------------------------------------------------------------------ state

/// One scroll: every line eases from `delta` above/below its new place.
#[derive(Clone, Copy)]
struct ScrollAnim {
    start: f64,
    delta: f32,
    active: usize,
}

/// A line shifted by a relayout (a translation or romanization arriving).
#[derive(Clone, Copy)]
struct Shift {
    start: f64,
    delta: f32,
}

/// A CSS transition on one value: eases from where it was to the new target, after a delay.
#[derive(Clone, Copy, Default)]
struct Trans {
    from: f32,
    to: f32,
    start: f64,
    dur: f64,
    delay: f64,
    init: bool,
    /// The theme's curve; `None`: CSS `ease`.
    ease: Option<Bezier>,
}

impl Trans {
    fn value(&self, now: f64) -> f32 {
        if !self.init {
            return self.to;
        }
        let p = ((now - self.start - self.delay) / self.dur.max(1e-3)).clamp(0.0, 1.0) as f32;
        lerp(self.from, self.to, self.ease.unwrap_or(EASE).at(p))
    }

    fn set(&mut self, to: f32, now: f64, dur: f64, delay: f64, ease: Option<Bezier>) {
        if !self.init {
            *self = Trans { from: to, to, start: now, dur, delay, init: true, ease };
        } else if (to - self.to).abs() > 1e-4 {
            let from = self.value(now);
            *self = Trans { from, to, start: now, dur, delay, init: true, ease };
        }
    }

    fn moving(&self, now: f64) -> bool {
        self.init && (self.from - self.to).abs() > 1e-4 && now < self.start + self.delay + self.dur
    }
}

#[derive(Clone, Copy, Default)]
struct LineState {
    /// Whole-line opacity and blur (the theme's line-state rules).
    opacity: Trans,
    blur: Trans,
    selected: bool,
    /// Wall time the exit animations started (line scale back, lit words fading).
    exit: Option<f64>,
    /// Song time at exit: which words had started.
    exit_t: f64,
}

/// The lyrics view's state between frames.
#[derive(Default)]
pub struct View {
    layout: Option<Layout>,
    states: Vec<LineState>,
    scroll_top: f32,
    scroll_set: bool,
    scrolls: Vec<ScrollAnim>,
    shifts: Vec<Vec<Shift>>,
    last_scroll: Vec<usize>,
    /// The line taller than the view being sung, and its row: the view pages down with it.
    page: Option<(usize, i32)>,
    last_t: f64,
    last_wall: f64,
    /// Whether the song was playing last frame: its time and the clock only agree if so.
    last_playing: bool,
    /// The font atlas's fill last frame: egui empties the atlas when it fills up or the
    /// fonts change, and nothing laid out or copied from it before that is valid.
    atlas_fill: f32,
    user_until: f64,
    /// Reader scrolling was on last frame: when it ends, scroll back to the current line.
    was_user: bool,
    deco_born: HashMap<u64, f64>,
    glow: Glow,
    /// The look as last resolved, and what it was resolved from: it's only built again
    /// when one of those changes, not on every frame.
    cfg: Option<(CfgKey, String, Arc<Cfg>)>,
    credits_focused: bool,
    /// The frame (`cumulative_pass_nr`) the view was last drawn in; 0 once its textures
    /// are freed.
    shown_pass: u64,
}

/// Everything `Cfg::new` reads besides the listener's font, which is kept beside it.
#[derive(Clone, Copy, PartialEq)]
struct CfgKey {
    /// The theme's style, by address: it lives as long as the app does.
    style: usize,
    accent: Color32,
    /// Panel width, window width and height, text scale.
    sizes: [u32; 4],
    centered: bool,
    fullscreen: bool,
    fonts: u32,
}

pub struct Frame<'a> {
    pub lyrics: &'a Lyrics,
    /// Identifies the song, source, decorations and settings the layout depends on.
    pub key: u64,
    pub roman: Vec<Option<Roman<'a>>>,
    pub trans: Vec<Option<&'a str>>,
    /// Audible song position (s), already corrected for output latency and the
    /// listener's offsets.
    pub t: f64,
    pub playing: bool,
    pub now: f64,
    pub rect: Rect,
    pub style: &'static LyricStyle,
    pub accent: Color32,
    pub centered: bool,
    /// The listener's lyrics text size, a multiple of the theme's.
    pub scale: f32,
    pub fullscreen: bool,
    /// The break's chip frosts what's behind it.
    pub glass_chip: Option<[f32; 3]>,
}

pub struct Out {
    pub seek: Option<f64>,
    /// Something is moving: keep drawing frames.
    pub animating: bool,
    /// The reader scrolled away; a "resume" control can bring the view back.
    pub user_scrolling: bool,
}

impl View {
    pub fn reset(&mut self) {
        *self = Self { glow: std::mem::take(&mut self.glow), ..Default::default() };
    }

    /// Called every frame: once the view isn't drawn (another tab or page), its glows,
    /// blurred lines and atlas copy go; they're rebuilt as it comes back.
    pub fn release_hidden(&mut self, pass: u64) {
        if self.shown_pass != 0 && self.shown_pass + 1 < pass {
            self.glow = Glow::default();
            self.shown_pass = 0;
        }
    }

    pub fn resume(&mut self) {
        self.user_until = 0.0;
    }

    /// CSS `text-shadow: 0 0 <blur>` for a laid-out text at `at`: the same Gaussian the
    /// lyric glow uses (σ = blur / 2), as a white mask to tint when painting.
    pub fn text_glow(&mut self, ctx: &egui::Context, g: Arc<Galley>, at: Pos2, blur: f32) -> Option<(TextureId, Rect, Rect)> {
        let atlas = ctx.fonts(|fv| fv.font_image_size());
        let ppp = ctx.pixels_per_point();
        // Cached per position, so a moved or resized view re-renders it.
        let key = (hash(("text-glow", at.x.round() as i32, at.y.round() as i32, g.job.text.as_str())), usize::MAX);
        self.glow.blurred_line(ctx, key, blur / 2.0, &[], &[(g, at, 1.0)], &[], 1.0, atlas, ppp)
    }

    pub fn show(&mut self, ui: &mut egui::Ui, f: Frame) -> Out {
        let rect = f.rect;
        let ctx = ui.ctx().clone();
        self.shown_pass = ctx.cumulative_pass_nr();
        let fill = ctx.fonts(|fv| fv.font_atlas_fill_ratio());
        if fill < self.atlas_fill {
            self.layout = None;
            self.glow.atlas = None;
            self.glow.lines.clear();
        }
        self.atlas_fill = fill;
        let (viewport, viewport_h) = (ctx.content_rect().width(), ctx.content_rect().height());
        let cfg_key = CfgKey {
            style: std::ptr::from_ref(f.style) as usize,
            accent: f.accent,
            sizes: [rect.width().to_bits(), viewport.to_bits(), viewport_h.to_bits(), f.scale.to_bits()],
            centered: f.centered,
            fullscreen: f.fullscreen,
            fonts: theme::fonts_generation(),
        };
        let cfg = match &self.cfg {
            Some((key, font, cfg)) if *key == cfg_key && theme::font_choice_is(font) => cfg.clone(),
            _ => {
                let cfg = Arc::new(Cfg::new(f.style, f.accent, rect.width(), viewport, viewport_h, f.centered, f.scale, f.fullscreen));
                self.cfg = Some((cfg_key, theme::font_choice(), cfg.clone()));
                cfg
            }
        };
        // What a line's own picture depends on. The view's height isn't in it: that only moves
        // the lines, and a new blurred image per line for every step of a resize piled up.
        let image_key = hash((f.key, rect.width().round() as i32, cfg.size.to_bits(), cfg.padding.to_bits(), cfg.max_scale().to_bits(), cfg.centered, &cfg.font, cfg.weight as u8, cfg.spacing.to_bits(), cfg.line_height.to_bits(), (cfg.align as u8, cfg.agent_align as u8, cfg.wave.on), theme::fonts_generation()));
        let layout_key = hash((image_key, rect.height().round() as i32));
        if self.layout.as_ref().is_none_or(|l| l.key != layout_key) {
            self.relayout(ui, &cfg, &f, layout_key, rect);
        }
        let layout = self.layout.as_ref().unwrap();
        let richsync = layout.richsync;
        // Lines that aren't credits.
        let lines_n = layout.blocks.iter().filter(|b| b.line.is_some()).count();

        // Clock: the extension shows lyrics this far ahead of the media time.
        let t = f.t + if richsync { RICHSYNC_OFFSET } else { LINE_OFFSET };
        // Paused (or just resumed) the wall clock runs on alone: only the song's time says it jumped.
        let drift = if f.playing && self.last_playing { t - self.last_t - (f.now - self.last_wall) } else { t - self.last_t };
        let jumped = drift.abs() > TIME_JUMP || self.last_wall == 0.0;
        self.last_playing = f.playing;
        self.last_t = t;
        self.last_wall = f.now;

        // ---- selection and exits (tickView's per-line loop)
        let early = if f.playing { 2.0 } else { 0.0 };
        let mut animating = false;
        for i in 0..lines_n {
            let b = &layout.blocks[i];
            let next = layout.blocks.get(i + 1).filter(|n| n.line.is_some()).map_or(f64::INFINITY, |n| n.start);
            let end = next.max(b.start + b.dur + 0.05);
            let st = &mut self.states[i];
            let selected = t + early >= b.start && t < end;
            if selected && !st.selected {
                st.selected = true;
                st.exit = None;
            } else if !selected && st.selected {
                st.selected = false;
                let stale = end + FADE_OUT + 0.05;
                st.exit = if (f.playing || jumped) && t <= stale { Some(f.now) } else { None };
                st.exit_t = t;
            } else if !selected && jumped {
                st.exit = None;
            }
            if st.exit.is_some_and(|e| f.now - e > (cfg.exit_fade.0 + cfg.exit_fade.1).max(cfg.scale_secs).max(cfg.line_exit.map_or(0.0, |x| x[1] as f64))) {
                st.exit = None;
            }
            animating |= st.exit.is_some();
        }

        // ---- scrolling
        let view_h = rect.height();
        let scroll_time = t + cfg.scroll_offset;
        let user = f.now < self.user_until;
        let mut active: Vec<usize> = Vec::new();
        // Leaving reader scrolling (the button, or the timeout) returns to the current line now.
        let mut new_lyric = jumped || (self.was_user && !user);
        self.was_user = user;
        for i in 0..lines_n {
            let b = &layout.blocks[i];
            let next = layout.blocks.get(i + 1).filter(|n| n.line.is_some()).map_or(f64::INFINITY, |n| n.start);
            if scroll_time >= b.start - cfg.early_scroll && (scroll_time < next || scroll_time < b.start + b.dur) {
                active.push(i);
                if !self.last_scroll.contains(&i) && scroll_time >= b.start {
                    new_lyric = true;
                }
            }
        }
        let active_real = !active.is_empty();
        let last_sung = (0..lines_n).rev().find(|&i| !layout.blocks[i].instrumental);
        let credits = layout.blocks.iter().position(|b| b.credits.is_some());
        let credits_focused = credits.is_some() && last_sung.is_some_and(|i| scroll_time >= layout.blocks[i].start + layout.blocks[i].dur);
        if credits_focused != self.credits_focused {
            self.credits_focused = credits_focused;
            new_lyric = true;
        }
        // A line taller than the view would run off the bottom while it's sung: how far
        // down it the singing is (the latest word's row, or for a line-synced line, as far
        // as its time has gone), so the view pages down with it.
        let line_h = cfg.line_h();
        let sung_y = |i: usize| -> Option<f32> {
            let b = &layout.blocks[i];
            if b.height <= view_h || scroll_time < b.start {
                return None;
            }
            let mut timed = b.words.iter().filter(|w| !w.line_synced && !w.bg).peekable();
            Some(if timed.peek().is_none() {
                b.height * ((scroll_time - b.start) / b.dur.max(0.001)).clamp(0.0, 1.0) as f32
            } else {
                timed.filter(|w| w.start <= scroll_time).map(|w| w.pos.y).fold(0.0, f32::max)
            })
        };
        let page = active.last().and_then(|&i| sung_y(i).map(|y| (i, (y / line_h) as i32)));
        if page != self.page {
            self.page = page;
            new_lyric |= page.is_some();
        }
        let pos = |i: usize| layout.pad_top + layout.blocks[i].top;
        let max_scroll = (layout.content_h - view_h).max(0.0);
        if !user && (new_lyric || !self.scroll_set) && lines_n > 0 {
            if active.is_empty() {
                active.push(0);
            }
            let offset = view_h * cfg.target_ratio;
            let last = *active.last().unwrap();
            let sung: Vec<usize> = active.iter().copied().enumerate().filter(|&(k, i)| scroll_time < layout.blocks[i].start + layout.blocks[i].dur - ENDING_THRESHOLD || k == active.len() - 1).map(|(_, i)| i).collect();
            let anchor = sung
                .iter()
                .rev()
                .find(|&&i| layout.blocks[i].start <= scroll_time && scroll_time < layout.blocks[i].start + layout.blocks[i].dur)
                .or_else(|| sung.iter().rev().find(|&&i| layout.blocks[i].start <= scroll_time))
                .copied()
                .unwrap_or(sung[0]);
            // `computeActiveLinesScrollTop`
            let (first, lastl) = (sung[0], *sung.last().unwrap());
            let centre = sung.iter().map(|&i| pos(i) + layout.blocks[i].height / 2.0).sum::<f32>() / sung.len() as f32;
            let h = |i: usize| layout.blocks[i].height;
            let mut top = centre - offset;
            top = top.max(pos(lastl) + h(lastl) - view_h);
            top = top.min(pos(lastl)).min(pos(first));
            top = top.max(pos(anchor) + h(anchor) - view_h);
            top = top.min(pos(anchor));
            if let Some(y) = sung_y(anchor) {
                // Keep the row being sung, and the one after it, in view.
                top = top.max(pos(anchor) + y + line_h * 2.0 - view_h);
            }
            if credits_focused && let (Some(c), Some(ls)) = (credits, last_sung) {
                top = (pos(c) + h(c) / 2.0 - offset).min(pos(ls));
            }
            let top = top.clamp(0.0, max_scroll);
            if !self.scroll_set {
                self.scroll_top = top;
                self.scroll_set = true;
            } else if (top - self.scroll_top).abs() > 2.0 {
                self.scrolls.push(ScrollAnim { start: f.now, delta: top - self.scroll_top, active: last });
                self.scroll_top = top;
            }
            self.last_scroll = active.clone();
        }

        // Reader scrolling.
        let hovered = ui.rect_contains_pointer(rect);
        let wheel = if hovered { ui.input(|i| i.smooth_scroll_delta.y) } else { 0.0 };
        if wheel != 0.0 {
            self.scroll_top = (self.scroll_top - wheel).clamp(0.0, max_scroll);
            self.user_until = f.now + USER_SCROLL_RESUME;
            self.scrolls.clear();
        }
        let user = f.now < self.user_until;

        // Scroll animations: per line, eased from where it was.
        self.scrolls.retain(|a| f.now - a.start < 2.0);
        animating |= !self.scrolls.is_empty();
        for s in &mut self.shifts {
            s.retain(|a| f.now - a.start < DECO_ENTRY);
            animating |= !s.is_empty();
        }

        // ---- paint
        let painter = ui.painter_at(rect);
        let origin_y = rect.top() - self.scroll_top + layout.pad_top;
        let x0 = rect.left() + cfg.pad.0;
        let line_w = rect.width() - cfg.pad.0 - cfg.pad.1;
        let atlas = ctx.fonts(|fv| fv.font_image_size());
        let ppp = ctx.pixels_per_point();
        let mut seek = None;
        let first_active = active.first().copied().filter(|_| active_real);
        // The latest line being sung: `active` runs ahead of the song for scrolling.
        let last_sung_now = (0..lines_n).rev().find(|&k| self.states[k].selected && t >= layout.blocks[k].start);
        let mut glow_work: Vec<GlowJob> = Vec::new();
        let pointer = ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p));

        for o in &f.style.column_overlays {
            super::shell::overlay_light(&painter, rect, o, f.accent);
        }
        // From here on, what `fade_edges` dims: the lines, not the overlays under them.
        let first_shape = ctx.graphics(|g| g.get(painter.layer_id()).map_or(ShapeIdx(0), |l| l.next_idx()));
        for (i, b) in layout.blocks.iter().enumerate() {
            let mut dy = 0.0;
            for a in &self.scrolls {
                let rel = i as isize - a.active as isize;
                let d = match cfg.scroll_curve {
                    Some(curve) => curve_duration(rel, a.delta, curve),
                    None => (scroll_duration(rel, a.delta) + cfg.scroll_secs - 0.75) * cfg.scroll_sides[usize::from(rel > 0)] as f64,
                };
                let p = ((f.now - a.start) / d.max(0.01)).clamp(0.0, 1.0) as f32;
                dy += a.delta * (1.0 - cfg.scroll_ease.at(p));
            }
            if let Some(shifts) = self.shifts.get(i) {
                for s in shifts {
                    let p = ((f.now - s.start) / DECO_ENTRY).clamp(0.0, 1.0) as f32;
                    dy += s.delta * (1.0 - DECO_EASE.at(p));
                }
            }
            let top = origin_y + b.top + dy;
            if top > rect.bottom() + 40.0 || top + b.height < rect.top() - 40.0 {
                continue;
            }
            let block_rect = Rect::from_min_size(pos2(x0, top), vec2(line_w, b.height));
            let content = pos2(x0 + cfg.size * 0.25, top);

            if let Some((text, font)) = &b.credits {
                let a = cfg.credits_alpha[usize::from(credits_focused)];
                if a < 0.0 {
                    continue;
                }
                let a = theme::anim(&ctx, ("lyr-credits", f.key), a, 0.35);
                let o = transform_origin(block_rect, b.align, content.x);
                let g = ctx.fonts_mut(|fv| deco_galley(fv, text.clone(), font.clone(), line_w - cfg.size * 0.5, (cfg.deco_tracking, cfg.deco_lh), b.align));
                let xf = Xf { origin: o, scale: cfg.scale, content };
                let at_x = if b.align == Align::Center { ((line_w - cfg.size * 0.5) - g.size().x).max(0.0) * 0.5 } else { 0.0 };
                paint_galley(&painter, &g, pos2(at_x, cfg.padding + cfg.credits_margin), &xf, theme::with_alpha(Color32::WHITE, a), atlas);
                continue;
            }
            let li = b.line.unwrap();
            let hovered = pointer.is_some_and(|p| block_rect.contains(p));
            let st = self.states[i];
            let sung_now = st.selected && t >= b.start;

            // Whole-line opacity and blur by state, as the themes' `.blyrics--active` rules.
            let on = sung_now;
            let (past_t, up_t) = if richsync { (&cfg.past, &cfg.upcoming) } else { (&cfg.past_lines, &cfg.upcoming_lines) };
            let is_past = match first_active {
                Some(a) => i < a,
                None => lines_n > 0 && t >= layout.blocks[0].start,
            };
            let line_timed = !richsync;
            let pre = cfg.pre_sharp > 0.0 && !on && t < b.start && t >= b.start - cfg.pre_sharp;
            let target = if on {
                (if line_timed { cfg.active_op_lines } else { cfg.active_op }, 0.0)
            } else if (hovered && cfg.hover_reveal) || pre {
                (1.0, 0.0)
            } else if user || (cfg.panel_reveal && pointer.is_some_and(|p| rect.contains(p))) {
                (cfg.scroll_opacity, 0.0)
            } else {
                match (first_active, last_sung_now) {
                    (Some(a), _) if i < a => ladder(past_t, a - i),
                    (_, Some(l)) if i > l => ladder(up_t, i - l),
                    (None, _) if is_past => ladder(past_t, lines_n.saturating_sub(i)),
                    // Nothing sung yet, though the view may already be scrolling to it.
                    (_, None) => ladder(up_t, i + 1),
                    _ => (1.0, 0.0),
                }
            };
            // A hovered line's opacity alone: its blur stays.
            let target = match cfg.hover_op {
                Some(o) if hovered && !on => (o, target.1),
                _ => target,
            };
            let line_s = b.dur as f32;
            let (op_t, blur_t) = if hovered && cfg.hover_reveal {
                // `:hover { transition: all 0.3s ease }`
                ((0.3, 0.0), (0.3, 0.0))
            } else {
                let timing = |x: Timing| ((line_s * x.frac + x.secs) as f64, x.delay as f64);
                // A theme may time lines turning active and turning past on their own.
                let (o, b) = if on {
                    (cfg.active_op_t, cfg.active_blur_t)
                } else if is_past {
                    (cfg.past_op_t, cfg.past_blur_t)
                } else {
                    (None, None)
                };
                match cfg.scroll_t.filter(|_| user) {
                    Some(x) => (timing(cfg.scroll_op_t.unwrap_or(x)), timing(x)),
                    None => (timing(o.unwrap_or(cfg.opacity_t)), timing(b.unwrap_or(cfg.blur_t))),
                }
            };
            let s = &mut self.states[i];
            s.opacity.set(target.0, f.now, op_t.0, op_t.1, cfg.line_ease);
            // Lit words and their fade are only drawn sharp: a line blurs once they're done.
            s.blur.set(if st.exit.is_some() { 0.0 } else { target.1 }, f.now, blur_t.0, blur_t.1, cfg.line_ease);
            animating |= s.opacity.moving(f.now) || s.blur.moving(f.now);
            let (opacity, blur) = (s.opacity.value(f.now), s.blur.value(f.now));
            let blur = match cfg.blur_from {
                Some(from) if sung_now && t < b.start + blur_t.0.max(0.01) => {
                    animating |= f.playing;
                    lerp(from, 0.0, EASE.at(((t - b.start) / blur_t.0.max(0.01)) as f32))
                }
                _ => blur,
            };
            // Fading out toward the view's edges.
            let edge_y = top + b.height * 0.5;
            let opacity = opacity
                * if cfg.edge_fade[0] > 0.0 { ((edge_y - rect.top()) / (rect.height() * cfg.edge_fade[0])).clamp(0.0, 1.0) } else { 1.0 }
                * if cfg.edge_fade[1] > 0.0 { ((rect.bottom() - edge_y) / (rect.height() * cfg.edge_fade[1])).clamp(0.0, 1.0) } else { 1.0 };
            let dim = match (cfg.reveal_dim, cfg.past_dim) {
                _ if hovered && !on && cfg.hover_dim.is_some() => cfg.hover_dim.unwrap_or(cfg.dim_alpha),
                _ if hovered && on && cfg.hover_unsung.is_some() => cfg.hover_unsung.unwrap_or(cfg.dim_alpha),
                (Some(d), _) if user || hovered || (cfg.panel_reveal && pointer.is_some_and(|p| rect.contains(p))) => d,
                (_, Some(d)) if is_past && !on => d,
                _ => cfg.dim_alpha,
            };
            let dim_tint = match cfg.past_color {
                Some(c) if is_past && !on => c,
                _ => b.colors.1,
            };
            let rest_sat = match (cfg.saturate_far, first_active) {
                (Some(far), Some(a)) => lerp(cfg.rest_saturate, far, ((a.abs_diff(i).max(1) - 1) as f32 / 3.0).min(1.0)),
                _ => cfg.rest_saturate,
            };
            let dim_tint = if rest_sat < 1.0 && !on { desaturate(dim_tint, rest_sat) } else { dim_tint };

            // Line scale: 0.95 at rest, 1 while sung (enter at its start, exit when it ends);
            // a theme's hover scale on lines not being sung.
            let mut scale = cfg.scale;
            if st.selected && t >= b.start {
                scale = lerp(cfg.scale, cfg.active_scale, cfg.scale_ease.at(((t - b.start) / cfg.scale_secs) as f32));
            } else if let Some(e) = st.exit {
                scale = lerp(cfg.active_scale, cfg.scale, cfg.scale_exit_ease.at(((f.now - e) / cfg.scale_secs) as f32));
            } else if let Some(h) = cfg.hover_scale {
                let k = theme::anim_bool(&ctx, ("lyr-hover", i), hovered, 0.3);
                scale = lerp(cfg.scale, h, EASE.at(k));
            } else if let Some(ss) = cfg.scroll_scale.filter(|_| user) {
                scale = ss;
            }
            if st.selected && t >= b.start && t < b.start + cfg.scale_secs {
                animating |= f.playing;
            }
            let origin = transform_origin(block_rect, b.align, content.x);
            // A sung line sliding away.
            let content = match (cfg.line_exit, st.exit) {
                (Some([rem, secs]), Some(e)) => content + vec2(if cfg.exit_by_side && b.align == Align::Right { -rem } else { rem } * cfg.rem() * EASE.at(((f.now - e) / secs.max(0.01) as f64) as f32), 0.0),
                _ => content,
            };
            // A line becoming active sliding in, other singers' from the other side.
            let content = match cfg.line_enter {
                Some([rem, secs]) if st.selected && t >= b.start && t < b.start + secs as f64 => {
                    animating |= f.playing;
                    let k = 1.0 - EASE.at(((t - b.start) / secs.max(0.01) as f64) as f32);
                    let side = if b.align == Align::Right { 1.0 } else { -1.0 };
                    content + vec2(side * rem * cfg.rem() * k, 0.0)
                }
                _ => content,
            };
            let xf = Xf { origin, scale, content };
            if let Some([a, r]) = cfg.hover_box {
                let k = theme::anim_bool(&ctx, ("lyr-hover-box", i), hovered && !on, 0.2);
                if k > 0.0 {
                    let radius = r * cfg.rem() / 10.0;
                    let mut boxed = block_rect.shrink2(vec2(0.0, cfg.padding * 0.4));
                    if let Some([from, secs]) = cfg.hover_box_pop {
                        let pop = theme::anim_bool_timed(&ctx, ("lyr-hover-pop", i), hovered && !on, secs);
                        boxed = Rect::from_center_size(boxed.center(), boxed.size() * lerp(from, 1.0, Bezier(0.34, 1.56, 0.64, 1.0).at(pop)));
                    }
                    painter.rect_filled(boxed, radius, theme::glass(a * k));
                    if cfg.hover_box_line > 0.0 {
                        painter.rect_stroke(boxed, radius, egui::Stroke::new(1.0, theme::glass(cfg.hover_box_line * k)), egui::StrokeKind::Inside);
                    }
                }
            }
            if on && !b.instrumental {
                let px = cfg.rem() / 10.0;
                if let Some([a, bleed]) = cfg.aura {
                    let whole = block_rect.expand2(vec2(bleed * px, bleed * 0.4 * px));
                    if cfg.aura_colors.is_empty() {
                        painter.add(aura_mesh(whole, theme::with_alpha(cfg.active, a * opacity)));
                    } else {
                        // Soft pools of each colour, overlapping along the line.
                        let n = cfg.aura_colors.len() as f32;
                        for (k, c) in cfg.aura_colors.iter().enumerate() {
                            let w = whole.width() * 1.5 / n;
                            let x = whole.left() + whole.width() * k as f32 / n - whole.width() * 0.1;
                            painter.add(aura_mesh(Rect::from_min_size(pos2(x, whole.top()), vec2(w, whole.height())), theme::with_alpha(*c, a * opacity)));
                        }
                    }
                }
                if let Some([w, h, a, left]) = cfg.marker {
                    paint_marker(&painter, &cfg, pos2(block_rect.left() - left * px, block_rect.center().y), vec2(w, h) * px, a * opacity, b.align == Align::Right);
                }
            } else if hovered && cfg.marker_hover > 0.0 && !b.instrumental
                && let Some([w, h, _, left]) = cfg.marker
            {
                let px = cfg.rem() / 10.0;
                paint_marker(&painter, &cfg, pos2(block_rect.left() - left * px, block_rect.center().y), vec2(w, h) * px, cfg.marker_hover * opacity, b.align == Align::Right);
            }
            // The whole line's own glow, under its words; a soft copy of the line, made once.
            let line_glow = if hovered && cfg.hover_glow.is_some() && (on || !cfg.hover_glow_sung) { cfg.hover_glow } else if on { cfg.active_glow } else { None };
            // Fading out once the line stops glowing, so it's asked for on those frames too.
            let k = if cfg.active_glow.is_some() || cfg.hover_glow.is_some() { theme::anim_bool(&ctx, ("lyr-line-glow", i), line_glow.is_some(), 0.3) } else { 0.0 };
            if let Some([radius, a]) = line_glow.or(cfg.active_glow).or(cfg.hover_glow).filter(|_| k > 0.0 && !b.instrumental && blur <= 0.05) {
                let colors: &[Color32] = if (on || line_glow.is_none()) && !cfg.active_glow_colors.is_empty() { &cfg.active_glow_colors } else { std::slice::from_ref(&cfg.glow) };
                for (ci, c) in colors.iter().enumerate() {
                    let r = ((radius * cfg.rem() / 10.0 * (1.0 + ci as f32) * 4.0).round() / 4.0).max(1.0);
                    let key = (hash((image_key, ppp.to_bits())) ^ 0x6c69_6e65, i * 8 + ci);
                    // The line's words are only gathered when its image has to be made.
                    let made = self.glow.cached_line(&ctx, key, r, scale).or_else(|| {
                        let words: Vec<(&Word, f32)> = b.words.iter().map(|w| (w, 1.0)).collect();
                        self.glow.blurred_line(&ctx, key, r, &words, &[], &[], scale, atlas, ppp)
                    });
                    if let Some((tex, rr, uv)) = made {
                        let to = xf.rect(rr);
                        let snapped = (to.min.to_vec2() * ppp).round() / ppp;
                        painter.image(tex, to.translate(snapped - to.min.to_vec2()), uv, theme::with_alpha(*c, a * k * opacity / colors.len() as f32 * 2.0));
                    }
                }
            }

            if b.instrumental {
                let em = cfg.inst_em();
                // The note's middle is 1/3 em from the edge (see `paint_instrumental`).
                let inset = if cfg.instrumental == Instrumental::Note { em / 3.0 } else { em * 0.5 };
                let ix = match b.align {
                    Align::Left => inset,
                    Align::Right => line_w - cfg.size * 0.5 - inset,
                    Align::Center => (line_w - cfg.size * 0.5) * 0.5,
                };
                let at = pos2(ix, cfg.padding + cfg.line_h() * 0.5);
                if let Some(a) = cfg.inst_chip {
                    let c = xf.point(at);
                    if let Some(g) = f.glass_chip {
                        let side = em * 1.9 * scale;
                        crate::shader::frost(&painter, Rect::from_center_size(c, Vec2::splat(side)), crate::shader::Frost { look: g, radius: side / 2.0, opacity, fade: 0.0 });
                    }
                    painter.circle_filled(c, em * 0.95 * scale, theme::glass(a * opacity));
                    painter.circle_stroke(c, em * 0.95 * scale, egui::Stroke::new(1.0, theme::glass(a * opacity)));
                }
                // Breathing, about its own middle.
                let bxf = match cfg.breathe.filter(|_| on) {
                    Some([lo, hi, secs]) => {
                        animating |= f.playing;
                        let k = lerp(lo, hi, 0.5 - 0.5 * (f.now as f32 * std::f32::consts::TAU / secs.max(0.1)).cos());
                        let c = xf.point(at);
                        Xf { origin: pos2(c.x + (xf.origin.x - c.x) * k, c.y + (xf.origin.y - c.y) * k), scale: scale * k, content }
                    }
                    None => xf,
                };
                animating |= paint_instrumental(&painter, &mut self.glow, &ctx, &cfg, b, &bxf, t, f.now, st, dim * opacity, line_w - cfg.size * 0.5) && (f.playing || st.exit.is_some());
            } else {
                // A line being sung comes in sharp over its blurred copy as that clears: the copy
                // can't show words lit, and they'd all turn up at once when the blur ran out.
                let sharp = if blur <= 0.05 {
                    1.0
                } else if on {
                    // Against the most it can have been blurred by: as the next line, the last one, or the theme's own start.
                    let from = [cfg.blur_from, up_t.first().map(|u| u[1]), past_t.first().map(|u| u[1])].into_iter().flatten().fold(0.05, f32::max);
                    1.0 - (blur / from).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                if blur > 0.05 {
                    // `filter: blur()` on the whole line: its text, romanization and translation.
                    let r = (blur * 4.0).round() / 4.0;
                    // Shown from here without an entrance when the line goes sharp.
                    for d in &b.decos {
                        self.deco_born.entry(d.id).or_insert(f.now - 10.0);
                    }
                    // What the image shows besides the radius and scale `blurred_line` checks: the
                    // line's wrap and text size, and the alpha baked into it, in steps.
                    let line_key = hash((image_key, ((dim * 64.0).round() / 64.0).to_bits(), ppp.to_bits()));
                    // What goes in the image is only gathered when it has to be made.
                    let made = self.glow.cached_line(&ctx, (line_key, i), r, scale).or_else(|| {
                        let deco_words: Vec<(&Word, f32)> = b
                            .decos
                            .iter()
                            .flat_map(|d| match &d.body {
                                DecoBody::Timed { words, .. } => words.iter().map(|w| (w, dim)).collect::<Vec<_>>(),
                                DecoBody::Text(..) => vec![],
                            })
                            .collect();
                        let texts: Vec<(Arc<Galley>, Pos2, f32)> = b
                            .decos
                            .iter()
                            .filter_map(|d| match &d.body {
                                DecoBody::Text(text, font) => {
                                    let g = ctx.fonts_mut(|fv| deco_galley(fv, text.clone(), font.clone(), d.wrap, (cfg.deco_tracking, cfg.deco_lh), b.align));
                                    let pad = if d.roman { vec2(cfg.deco_rem() * 0.75 + 1.0, cfg.deco_rem() * 0.25 + 1.0) } else { Vec2::ZERO };
                                    Some((g, d.rect.min + pad, if d.roman { cfg.roman_alpha } else { cfg.trans_alpha }))
                                }
                                DecoBody::Timed { .. } => None,
                            })
                            .collect();
                        let words: Vec<(&Word, f32)> = b.words.iter().map(|w| (w, if w.bg { dim * cfg.bg_alpha } else { dim })).chain(deco_words).collect();
                        // The romanization pill is part of the line, so it's blurred with it.
                        let pills: Vec<(Rect, f32)> = b.decos.iter().filter(|d| d.pill).map(|d| (d.rect, cfg.deco_rem())).collect();
                        self.glow.blurred_line(&ctx, (line_key, i), r, &words, &texts, &pills, scale, atlas, ppp)
                    });
                    if let Some((tex, rr, uv)) = made {
                        // Rendered at the line's scale; snapped so it lands 1:1 on screen pixels.
                        let to = xf.rect(rr);
                        let snapped = (to.min.to_vec2() * ppp).round() / ppp;
                        painter.image(tex, to.translate(snapped - to.min.to_vec2()), uv, theme::with_alpha(dim_tint, opacity * (1.0 - sharp)));
                    }
                    animating |= st.exit.is_some();
                }
                if sharp > 0.0 {
                    let opacity = opacity * sharp;
                    let mut lit = LitState::new(&cfg, st, t, f.now, sung_now || st.selected, b.start);
                    if let Some(c) = f.lyrics.lines[li].agent.as_deref().and_then(|a| a.strip_prefix('v')?.parse::<usize>().ok()).and_then(|n| cfg.agent_glow.get(n.checked_sub(1)?)) {
                        lit.glow = *c;
                    }
                    let tints = (b.colors.0, dim_tint);
                    paint_words(&painter, &ctx, &cfg, &b.words, &b.groups, &xf, &lit, dim, opacity, tints, atlas, &mut glow_work, (f.key, i, 0));
                    for (k, d) in b.decos.iter().enumerate() {
                        let born = *self.deco_born.entry(d.id).or_insert(f.now);
                        let [enter_y, enter_x, enter_secs, enter_delay] = cfg.deco_enter;
                        let enter_delay = enter_delay + if d.roman { 0.0 } else { cfg.trans_delay };
                        let enter = ((f.now - born - enter_delay as f64) / enter_secs.max(0.01) as f64).clamp(0.0, 1.0) as f32;
                        let e = DECO_EASE.at(enter);
                        if enter < 1.0 {
                            animating = true;
                        }
                        // `blyrics-decoration-in`: fades in from 0.4em above.
                        let font_size = match &d.body {
                            DecoBody::Text(_, f) => f.size,
                            DecoBody::Timed { words, .. } => words.first().map_or(cfg.deco_rem() * 1.6, |w| w.font.size),
                        };
                        let dxf = Xf { content: xf.content + vec2(enter_x, enter_y) * font_size * (1.0 - e), ..xf };
                        // Unsung words' alpha hides a decoration only on lines not being sung.
                        let a = e * opacity * if on { 1.0 } else { (dim / 0.05).min(1.0) };
                        if d.pill {
                            let r = dxf.rect(d.rect);
                            let radius = (cfg.deco_rem() * scale).min(255.0) as u8;
                            painter.rect(r, radius, theme::with_alpha(Color32::WHITE, 0.05 * a), egui::Stroke::new(1.0, theme::with_alpha(Color32::WHITE, 0.1 * a)), egui::StrokeKind::Inside);
                        }
                        match &d.body {
                            DecoBody::Timed { words, groups } => {
                                paint_words(&painter, &ctx, &cfg, words, groups, &dxf, &lit, dim, a, tints, atlas, &mut glow_work, (f.key, i, k as u32 + 1));
                            }
                            DecoBody::Text(text, font) => {
                                let g = ctx.fonts_mut(|fv| deco_galley(fv, text.clone(), font.clone(), d.wrap, (cfg.deco_tracking, cfg.deco_lh), b.align));
                                let pad = if d.roman { vec2(cfg.deco_rem() * 0.75 + 1.0, cfg.deco_rem() * 0.25 + 1.0) } else { Vec2::ZERO };
                                // `--blyrics-translated-color`. Drawn through the line's transform
                                // at the one laid-out size.
                                let alpha = if d.roman { cfg.roman_alpha } else { cfg.trans_alpha };
                                let alpha = cfg.deco_active.filter(|_| on).unwrap_or(alpha);
                                paint_galley(&painter, &g, d.rect.min + pad, &dxf, theme::with_alpha(Color32::WHITE, alpha * a), atlas);
                            }
                        }
                    }
                    animating |= lit.animating(b, t) && (f.playing || lit.exit.is_some());
                }
            }

            // Click a line to seek to it (the extension seeks to the line's time).
            let hit = block_rect.intersect(rect);
            if hit.is_positive() {
                let resp = ui.interact(hit, ui.scope_id().with(("lyr-line", f.key, i)), egui::Sense::click());
                if resp.clicked() && f.lyrics.sync != Sync::None {
                    seek = Some(f.lyrics.lines[li].start as f64 / 1000.0);
                }
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
            }
        }
        // Glows (drop-shadows of the lit text) were queued while painting; draw them under
        // the lit text they belong to by inserting at the recorded positions.
        self.glow.finish(&ctx, &painter, glow_work, atlas, ppp);
        fade_edges(&ctx, painter.layer_id(), first_shape, rect, cfg.size);
        Out { seek, animating, user_scrolling: user }
    }

    fn relayout(&mut self, ui: &mut egui::Ui, cfg: &Cfg, f: &Frame, key: u64, rect: Rect) {
        let width = rect.width() - cfg.pad.0 - cfg.pad.1;
        let lyrics = f.lyrics;
        let mut blocks: Vec<Block> = ui.ctx().fonts_mut(|fonts| {
            lyrics
                .lines
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    let agent = if l.instrumental { nearest_agent(lyrics, i) } else { l.agent.clone() };
                    let roman = f.roman.get(i).and_then(|r| r.as_ref()).map(|r| match r {
                        Roman::Timed(p, t) => Roman::Timed(p, t),
                        Roman::Text(t) => Roman::Text(t),
                    });
                    let trans = f.trans.get(i).copied().flatten();
                    build_block(fonts, cfg, l, i, width, roman, trans, agent.as_deref())
                })
                .collect()
        });
        if !lyrics.songwriters.is_empty() {
            let names = &lyrics.songwriters;
            let text = if names.len() > 1 { format!("Written by {} & {}", names[..names.len() - 1].join(", "), names[names.len() - 1]) } else { format!("Written by {}", names[0]) };
            let font = theme::weighted(cfg.credits_weight, (cfg.size * 0.4).max(12.0));
            let h = ui.ctx().fonts_mut(|fv| deco_galley(fv, text.clone(), font.clone(), width - cfg.size * 0.5, (cfg.deco_tracking, cfg.deco_lh), Align::Left).size().y) + cfg.credits_margin;
            blocks.push(Block {
                line: None,
                start: 0.0,
                dur: 0.0,
                instrumental: false,
                align: if cfg.centered || cfg.credits_center { Align::Center } else { Align::Left },
                colors: (cfg.active, Color32::WHITE),
                top: 0.0,
                height: h + cfg.padding * 2.0,
                words: vec![],
                groups: vec![],
                decos: vec![],
                credits: Some((text, font)),
            });
        }
        let mut y = 0.0;
        for b in &mut blocks {
            b.top = y;
            y += b.height;
        }
        let view_h = rect.height();
        // `computeScrollPadding`
        let first_h = blocks.first().map_or(0.0, |b| b.height);
        let pad_top = (view_h * cfg.target_ratio - first_h / 2.0).max(cfg.padding_top);
        let last_centre = blocks.iter().rev().find(|b| b.line.is_some()).map(|b| b.top + b.height / 2.0);
        let last_h = blocks.iter().rev().find(|b| b.line.is_some()).map_or(0.0, |b| b.height);
        let content = y;
        let trailing = blocks.last().filter(|b| b.credits.is_some()).map_or(0.0, |b| b.height) + last_h / 2.0;
        let want = last_centre.map_or(view_h, |c| c + view_h * (1.0 - cfg.target_ratio));
        let pad_bottom = (want - content).max(view_h - content).max(view_h * (1.0 - cfg.target_ratio) - trailing).max(0.0);
        let richsync = lyrics.sync == Sync::Word;
        let same_song = self.layout.as_ref().is_some_and(|l| l.blocks.len() == blocks.len() && self.states.len() == blocks.len() && l.richsync == richsync && same_lines(l, &blocks));
        if same_song {
            // Decorations arrived (or the window resized): lines slide to their new places,
            // and the view scrolls again to the current line, wherever that now is.
            self.last_scroll.clear();
            let old = self.layout.as_ref().unwrap();
            let now = ui.input(|i| i.time);
            self.shifts.resize_with(blocks.len(), Vec::new);
            for (i, b) in blocks.iter().enumerate() {
                let delta = (old.pad_top + old.blocks[i].top) - (pad_top + b.top);
                if delta.abs() > 0.5 && (old.width - width).abs() < 1.0 {
                    self.shifts[i].push(Shift { start: now, delta });
                }
            }
        } else {
            let glow = std::mem::take(&mut self.glow);
            *self = Self { glow, ..Default::default() };
            self.states = vec![LineState::default(); blocks.len()];
            self.shifts = vec![Vec::new(); blocks.len()];
        }
        self.layout = Some(Layout { key, width, blocks, pad_top, content_h: pad_top + content + pad_bottom, richsync });
    }
}

fn same_lines(l: &Layout, blocks: &[Block]) -> bool {
    l.blocks.iter().zip(blocks).all(|(a, b)| a.line == b.line && a.start == b.start && a.words.len() == b.words.len())
}

/// `findNearestAgent`: an instrumental break takes the side of the singer nearest it.
fn nearest_agent(l: &Lyrics, i: usize) -> Option<String> {
    l.lines[i + 1..].iter().chain(l.lines[..i].iter().rev()).find(|x| !x.instrumental && x.agent.is_some()).and_then(|x| x.agent.clone())
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Duration of one line's part in a scroll, by its distance from the active line
/// (`LINE_SCROLL_AFTER_FUNCTION` and the `above` variant).
/// Whether a line sits on the side the scroll is coming from.
fn is_above(rel: isize, delta: f32) -> bool {
    let up = delta < 0.0;
    (rel < 0 && !up) || (rel > 0 && up)
}

/// A theme's `scroll_curve`: seconds for a line `rel` lines from the sung one.
fn curve_duration(rel: isize, delta: f32, [above, above_log, below, below_log, below_step]: [f32; 5]) -> f64 {
    let i = rel.unsigned_abs() as f64;
    let log = (i + 1.0).ln();
    (if is_above(rel, delta) { above as f64 + log * above_log as f64 } else { below as f64 + log * below_log as f64 + i * below_step as f64 }) / 1000.0
}

fn scroll_duration(rel: isize, delta: f32) -> f64 {
    let above = is_above(rel, delta);
    let i = rel.unsigned_abs() as f64;
    if above { (750.0 + i.min(6.0) * 20.0) / 1000.0 } else { (750.0 + (i + 2.0).ln() * 80.0 + (i + 1.0) * 20.0) / 1000.0 }
}

fn transform_origin(block: Rect, align: Align, content_left: f32) -> Pos2 {
    match align {
        Align::Left => pos2(block.left(), block.center().y),
        Align::Right => pos2(block.right(), block.center().y),
        Align::Center => pos2(block.center().x.max(content_left), block.center().y),
    }
}

/// The line's transform: content-relative positions → screen, scaled about `origin`.
#[derive(Clone, Copy)]
struct Xf {
    origin: Pos2,
    scale: f32,
    content: Pos2,
}

impl Xf {
    fn point(&self, p: Pos2) -> Pos2 {
        let s = self.content + p.to_vec2();
        self.origin + (s - self.origin) * self.scale
    }
    fn rect(&self, r: Rect) -> Rect {
        Rect::from_min_max(self.point(r.min), self.point(r.max))
    }
}

/// Lines fade out over the top and bottom edges of the view instead of being cut off
/// there. Geometry crossing an edge zone is sliced into thin horizontal bands, each
/// dimmed by its own height, so the fade is smooth and reaches zero exactly at the edge
/// (per-vertex alpha alone interpolates across a glyph and leaves a visible cut).
fn fade_edges(ctx: &egui::Context, layer: egui::LayerId, from: ShapeIdx, rect: Rect, size: f32) {
    let zone = (size * 1.6).min(rect.height() * 0.15).max(1.0);
    let edge = EdgeFade { rect, zone };
    ctx.graphics_mut(|g| {
        let list = g.entry(layer);
        for i in from.0..list.next_idx().0 {
            list.mutate_shape(ShapeIdx(i), |c| edge.shape(&mut c.shape));
        }
    });
}

struct EdgeFade {
    rect: Rect,
    zone: f32,
}

impl EdgeFade {
    fn k(&self, y: f32) -> f32 {
        let s = |d: f32| {
            let x = (d / self.zone).clamp(0.0, 1.0);
            x * x * (3.0 - 2.0 * x)
        };
        s(y - self.rect.top()) * s(self.rect.bottom() - y)
    }

    fn shape(&self, shape: &mut Shape) {
        match shape {
            Shape::Mesh(m) => {
                let (top, bottom) = (self.rect.top() + self.zone, self.rect.bottom() - self.zone);
                if m.vertices.iter().any(|v| v.pos.y < top || v.pos.y > bottom) {
                    *m = Arc::new(self.mesh(m));
                }
            }
            Shape::Rect(r) => {
                let f = self.k(r.rect.center().y);
                r.fill = r.fill.gamma_multiply(f);
                r.stroke.color = r.stroke.color.gamma_multiply(f);
            }
            Shape::Text(t) => t.opacity_factor *= self.k(t.pos.y + t.galley.size().y / 2.0),
            Shape::Vec(v) => v.iter_mut().for_each(|s| self.shape(s)),
            _ => {}
        }
    }

    fn mesh(&self, m: &Mesh) -> Mesh {
        use egui::epaint::Vertex;
        const BAND: f32 = 2.0;
        let (r, z) = (self.rect, self.zone);
        let mut cuts = Vec::new();
        for (a, b) in [(r.top(), r.top() + z), (r.bottom() - z, r.bottom())] {
            let n = ((b - a) / BAND).ceil().max(1.0) as usize;
            cuts.extend((0..=n).map(|i| a + (b - a) * i as f32 / n as f32));
        }
        let lerp = |a: &Vertex, b: &Vertex, t: f32| Vertex {
            pos: a.pos + (b.pos - a.pos) * t,
            uv: a.uv + (b.uv - a.uv) * t,
            color: a.color.lerp_to_gamma(b.color, t),
        };
        // The part of a polygon on one side of a horizontal line.
        let clip = |poly: &[Vertex], y: f32, keep_below: bool, kept: &mut Vec<Vertex>| {
            let inside = |v: &Vertex| if keep_below { v.pos.y >= y } else { v.pos.y <= y };
            kept.clear();
            for i in 0..poly.len() {
                let (a, b) = (&poly[i], &poly[(i + 1) % poly.len()]);
                if inside(a) {
                    kept.push(*a);
                }
                if inside(a) != inside(b) {
                    kept.push(lerp(a, b, (y - a.pos.y) / (b.pos.y - a.pos.y)));
                }
            }
        };
        let mut out = Mesh::with_texture(m.texture_id);
        let (mut ys, mut below, mut piece): (Vec<f32>, Vec<Vertex>, Vec<Vertex>) = (Vec::new(), Vec::new(), Vec::new());
        for tri in m.indices.as_chunks::<3>().0 {
            let poly = [m.vertices[tri[0] as usize], m.vertices[tri[1] as usize], m.vertices[tri[2] as usize]];
            let lo = poly.iter().map(|v| v.pos.y).fold(f32::INFINITY, f32::min);
            let hi = poly.iter().map(|v| v.pos.y).fold(f32::NEG_INFINITY, f32::max);
            if hi <= r.top() || lo >= r.bottom() {
                continue;
            }
            // Clear of both edge zones: nothing to cut or dim.
            if lo >= r.top() + z && hi <= r.bottom() - z {
                let base = out.vertices.len() as u32;
                out.vertices.extend_from_slice(&poly);
                out.indices.extend([base, base + 1, base + 2]);
                continue;
            }
            ys.clear();
            ys.push(lo.max(r.top()));
            ys.extend(cuts.iter().copied().filter(|&y| y > lo && y < hi));
            ys.push(hi.min(r.bottom()));
            for w in ys.windows(2) {
                clip(&poly, w[0], true, &mut below);
                clip(&below, w[1], false, &mut piece);
                if piece.len() < 3 {
                    continue;
                }
                let base = out.vertices.len() as u32;
                out.vertices.extend(piece.iter().map(|v| Vertex { color: v.color.gamma_multiply(self.k(v.pos.y)), ..*v }));
                for i in 1..piece.len() as u32 - 1 {
                    out.indices.extend([base, base + i, base + i + 1]);
                }
            }
        }
        out
    }
}

/// A laid-out galley drawn through a line transform, as glyph quads on the font texture.
fn paint_galley(painter: &egui::Painter, g: &Galley, at: Pos2, xf: &Xf, color: Color32, atlas: [usize; 2]) {
    let mut mesh = Mesh::with_texture(TextureId::default());
    let none: [f32; 0] = [];
    for row in &g.rows {
        for gl in &row.row.glyphs {
            let uv = gl.uv_rect;
            if uv.is_nothing() {
                continue;
            }
            let r = Rect::from_min_size(at + row.pos.to_vec2() + gl.pos.to_vec2() + uv.offset, uv.size);
            let uvr = Rect::from_min_max(
                pos2(uv.min[0] as f32 / atlas[0] as f32, uv.min[1] as f32 / atlas[1] as f32),
                pos2(uv.max[0] as f32 / atlas[0] as f32, uv.max[1] as f32 / atlas[1] as f32),
            );
            push_quad(&mut mesh, xf.rect(r), uvr, color, &|_| 1.0, &none);
        }
    }
    painter.add(Shape::mesh(mesh));
}

// ------------------------------------------------------------------ words

/// Where a line's highlight animations are this frame.
struct LitState {
    /// Line still running its animations (song time drives them).
    live: bool,
    /// Exit: wall time since it started, and the song time it happened at.
    exit: Option<(f32, f64)>,
    t: f64,
    /// When the line starts, song time.
    line_start: f64,
    fill: Fill,
    /// This line's glow colour.
    glow: Color32,
}

impl LitState {
    fn new(cfg: &Cfg, st: LineState, t: f64, now: f64, live: bool, line_start: f64) -> Self {
        Self { live: live && st.exit.is_none(), exit: st.exit.map(|e| ((now - e) as f32, st.exit_t)), t, line_start, fill: cfg.fill, glow: cfg.glow }
    }

    fn animating(&self, b: &Block, t: f64) -> bool {
        if self.exit.is_some() {
            return true;
        }
        self.live && b.words.iter().any(|w| t >= w.start - 0.5 && t < w.start + w.dur.max(0.0) * SWIPE_DURATION + GLOW_MIN.max(w.dur * GLOW_RATIO) + 1.0)
    }
}

/// What a word looks like this frame.
struct WordLook {
    /// Lit layer opacity.
    opacity: f32,
    /// Gradient start/end, as fractions of the word (or letter-index space).
    start: f32,
    end: f32,
    /// Glow radius in points (0 = none).
    glow: f32,
    /// Alpha of this word's `--blyrics-glow-color`.
    glow_alpha: f32,
}

fn word_look(w: &Word, lit: &LitState, cfg: &Cfg) -> WordLook {
    let glow_alpha = if w.long { cfg.long_glow_alpha } else { cfg.glow_alpha };
    let none = WordLook { opacity: 0.0, start: SWIPE_START.0, end: SWIPE_END.0, glow: 0.0, glow_alpha };
    let full = |opacity: f32| WordLook { opacity, start: SWIPE_START.1, end: SWIPE_END.1, glow: 0.0, glow_alpha };
    if let Some((since, exit_t)) = lit.exit {
        // Words already sung fade back over 0.5 s; the rest were reset.
        if exit_t < w.start {
            return none;
        }
        let end = cfg.sung_fade.map_or(0.0, |f| f[0]);
        let p = ((since as f64 - cfg.exit_fade.1).max(0.0) / cfg.exit_fade.0) as f32;
        let mut look = full(lerp(1.0, end, EASE.at(p)).min(1.0));
        // The glow running out instead of stopping with the line.
        if cfg.glow_fade > 0.0 && glow_alpha >= GLOW_INVISIBLE {
            let gone = (exit_t - w.start) + since as f64;
            look.glow = glow_radius(gone, (w.dur.max(0.0) * cfg.glow_ratio).max(cfg.glow_min).min(cfg.glow_fade + (exit_t - w.start)), cfg.glow_rem * cfg.rem(), cfg.glow_ease);
        }
        return look;
    }
    if !lit.live {
        return none;
    }
    // The whole line lights at once, whatever its words' timing.
    let e = lit.t - if lit.fill == Fill::Line { lit.line_start } else { w.start };
    if e < 0.0 {
        return none;
    }
    // `resolveLineGlowSuppression`: a glow colour too faint to see isn't animated at all.
    let glow_rem = if glow_alpha < GLOW_INVISIBLE { 0.0 } else if w.letters && cfg.wave.glow_rem > 0.0 { cfg.wave.glow_rem * cfg.rem() } else { cfg.glow_rem * cfg.rem() };
    // Sung words settling to their resting alpha.
    let rest = |look: WordLook| match cfg.sung_fade {
        Some([alpha, secs]) if e > w.dur.max(0.0) => WordLook { opacity: lerp(look.opacity, alpha, EASE.at(((e - w.dur.max(0.0)) / secs.max(0.01) as f64) as f32)), ..look },
        _ => look,
    };
    // Background vocals dimming once they've been sung.
    let rest = |look: WordLook| match cfg.bg_sung.filter(|_| w.bg && e > w.dur.max(0.0)) {
        Some(alpha) => WordLook { opacity: lerp(look.opacity, alpha, EASE.at(((e - w.dur.max(0.0)) / 0.3) as f32)), ..rest(look) },
        None => rest(look),
    };
    // Background vocals held back while they're being sung.
    let rest = |look: WordLook| match cfg.bg_lit.filter(|_| w.bg && e <= w.dur.max(0.0)) {
        Some(alpha) => {
            let r = rest(look);
            WordLook { opacity: r.opacity * alpha, ..r }
        }
        None => rest(look),
    };
    // Coming up from faint as the word's bump starts.
    let rest = |look: WordLook| match cfg.bounce.filter(|b| b.from_alpha > 0.0) {
        Some(b) => {
            let k = EASE.at((e / bounce_secs(&b, w) as f64) as f32);
            let r = rest(look);
            WordLook { opacity: r.opacity * lerp(b.from_alpha, 1.0, k), ..r }
        }
        None => rest(look),
    };
    rest(match lit.fill {
        Fill::Line | Fill::Word => {
            // Word: a colour flip with a short transition (`data-word-state`);
            // Line: the line-synced fade.
            let d = if lit.fill == Fill::Word { 0.18 } else { cfg.line_fade };
            let p = EASE.at((e / d) as f32);
            WordLook { glow: glow_radius(e, cfg.glow_min, glow_rem, cfg.glow_ease), ..full(p) }
        }
        Fill::Sweep if w.line_synced => {
            let p = EASE.at((e / cfg.line_fade) as f32);
            WordLook { glow: glow_radius(e, cfg.glow_min, glow_rem, cfg.glow_ease), ..full(p) }
        }
        Fill::Sweep => {
            let d = w.dur.max(0.0);
            let sd = d * cfg.sweep_ratio;
            let p = if sd <= 0.0 { 1.0 } else { ((e + d * cfg.sweep_lead) / sd).clamp(0.0, 1.0) as f32 };
            WordLook {
                opacity: 1.0,
                start: lerp(SWIPE_START.0, cfg.sweep_end.0, p),
                end: match cfg.sweep_soft {
                    Some(soft) => lerp(SWIPE_START.0, cfg.sweep_end.0, p) + soft,
                    None => lerp(SWIPE_END.0, cfg.sweep_end.1, p),
                },
                glow: glow_radius(e, (d * cfg.glow_ratio).max(cfg.glow_min), glow_rem, cfg.glow_ease),
                glow_alpha,
            }
        }
    })
}

/// `drop-shadow` radius: 0.8rem → 0 with `ease`, then gone.
fn glow_radius(e: f64, dur: f64, from: f32, ease: Bezier) -> f32 {
    if e < 0.0 || e >= dur {
        return 0.0;
    }
    from * (1.0 - ease.at((e / dur) as f32))
}

/// A word's bump lasts this long.
fn bounce_secs(b: &WordBounce, w: &Word) -> f32 {
    b.secs + w.dur.max(0.0) as f32 * b.dur_ratio
}

/// Colour in `c` against grey.
fn desaturate(c: Color32, k: f32) -> Color32 {
    let gray = (0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32) as u8;
    theme::lerp_color(Color32::from_rgba_premultiplied(gray, gray, gray, c.a()), c, k)
}

/// The bar beside the sung line, in the theme's colours.
fn paint_marker(painter: &egui::Painter, cfg: &Cfg, center: Pos2, size: Vec2, alpha: f32, other_singer: bool) {
    let bar = Rect::from_center_size(center, size);
    let colors = if other_singer && !cfg.marker_alt.is_empty() { &cfg.marker_alt } else { &cfg.marker_colors };
    if colors.len() < 2 {
        let c = colors.first().copied().unwrap_or(cfg.active);
        painter.rect_filled(bar, size.x / 2.0, theme::with_alpha(c, alpha));
        return;
    }
    let n = colors.len() - 1;
    for i in 0..n {
        let top = bar.top() + bar.height() * i as f32 / n as f32;
        let bottom = bar.top() + bar.height() * (i + 1) as f32 / n as f32;
        super::shell::vgradient(painter, Rect::from_x_y_ranges(bar.x_range(), top..=bottom), theme::with_alpha(colors[i], alpha), theme::with_alpha(colors[i + 1], alpha));
    }
}

/// `planLetterWave` on the default swipe ramp: (delay, duration) per letter, seconds.
fn letter_waves(n: usize, d: f64, float: f64) -> Vec<(f64, f64)> {
    let sd = d * SWIPE_DURATION;
    let lead = d * SWIPE_LEAD;
    let rise = float * LETTER_WAVE_CREST as f64;
    (0..n)
        .map(|i| {
            if sd <= 0.0 {
                return (0.0, float);
            }
            let (sf, st) = (SWIPE_START.0 as f64, SWIPE_START.1 as f64);
            let (ef, et) = (SWIPE_END.0 as f64, SWIPE_END.1 as f64);
            let begin = (sd * (i as f64 / n as f64 - ef) / (et - ef)).clamp(0.0, sd);
            let finish = (sd * ((i + 1) as f64 / n as f64 - sf) / (st - sf)).clamp(0.0, sd);
            let dur = (finish - begin).max(0.001);
            let touched = begin - lead;
            let lit = touched + dur;
            if lit - rise > touched { (touched, (lit - touched) / LETTER_WAVE_CREST as f64) } else { (lit - rise, float) }
        })
        .collect()
}

/// A letter's float this frame: (lift in em, scale).
fn letter_float(e: f64, wave: (f64, f64), long: bool, cfg: &Cfg) -> (f32, f32) {
    let p = (e - wave.0) / wave.1;
    if p < 0.0 {
        return (0.0, cfg.wave.rest_scale);
    }
    let p = p.min(1.0) as f32;
    let peak = if long { cfg.wave.emphasis } else { 1.0 };
    if p < LETTER_WAVE_CREST {
        let k = EASE_IN_OUT.at(p / LETTER_WAVE_CREST);
        (cfg.wave.crest * k, lerp(cfg.wave.rest_scale, peak, k))
    } else {
        let k = EASE_OUT.at((p - LETTER_WAVE_CREST) / (1.0 - LETTER_WAVE_CREST));
        (lerp(cfg.wave.crest, cfg.wave.settle, k), lerp(peak, 1.0, k))
    }
}

/// The group's sideways pop: (translateX in em, scaleX).
fn wobble(e: f64, secs: f64) -> (f32, f32) {
    if !(0.0..secs).contains(&e) {
        return (0.0, 1.0);
    }
    let p = EASE.at((e / secs) as f32);
    let peak = (0.05, 1.025);
    if p < 0.125 {
        let k = p / 0.125;
        (peak.0 * k, lerp(1.0, peak.1, k))
    } else if p < 0.75 {
        let k = EASE_IN_OUT.at((p - 0.125) / 0.625);
        (lerp(peak.0, 0.0, k), lerp(peak.1, 1.0, k))
    } else {
        (0.0, 1.0)
    }
}

/// A word's bump this frame: (lift in em, scale).
fn word_bounce(e: f64, b: &WordBounce, secs: f32) -> (f32, f32) {
    if e < 0.0 {
        return (0.0, 1.0);
    }
    let p = (e / secs.max(0.01) as f64).min(1.0) as f32;
    if p >= 1.0 {
        return (b.settle, 1.0);
    }
    // Up and back; a spring passes its resting point once on the way down.
    let shape = if b.spring { (p * std::f32::consts::PI * 1.5).sin() * (1.0 - p) * 1.4 } else { (p * std::f32::consts::PI).sin() };
    (b.settle * p + b.lift * shape, lerp(1.0, b.scale.max(0.1), shape.max(0.0)))
}

/// One glyph of a word, positioned for this frame.
struct GlyphQuad {
    rect: Rect,
    uv: Rect,
    /// Letter index and count, for the letter-space gradient.
    letter: usize,
    letters: usize,
}

/// A word's glyphs this frame, in content coordinates (before the line transform):
/// letter floats applied, then the group's wobble. Laid out once, at the font's size,
/// and scaled with the line: a second size would put a second copy of every glyph in
/// the shared font atlas, which only ever grows.
fn word_glyphs(ctx: &egui::Context, w: &Word, lit: &LitState, cfg: &Cfg, wob: Option<(f32, f32, f32)>, atlas: [usize; 2]) -> Vec<GlyphQuad> {
    let g = ctx.fonts_mut(|fv| fv.layout_no_wrap(w.text.clone(), w.font.clone(), Color32::WHITE));
    let n: usize = g.rows.iter().map(|r| r.row.glyphs.len()).sum();
    // From the line's start: until then its letters aren't at their resting scale yet.
    let waves = if w.letters && lit.live && lit.exit.is_none() && lit.t >= lit.line_start { Some(letter_waves(n, w.dur.max(0.0), cfg.wave.secs as f64)) } else { None };
    let e = lit.t - w.start;
    let em = w.font.size;
    // Resting low until sung, then gliding up.
    let rise_secs = cfg.rise_secs + w.dur.max(0.0) * cfg.rise_ratio;
    let rise = if (cfg.rise_px > 0.0 || cfg.rest_em != 0.0) && !w.line_synced { (cfg.rise_px + cfg.rest_em * em) * (1.0 - EASE.at((e.max(0.0) / rise_secs) as f32)) } else { 0.0 };
    // Resting a little small until sung.
    let rest = if cfg.word_rest_scale < 1.0 && !w.line_synced { lerp(cfg.word_rest_scale, 1.0, EASE.at((e.max(0.0) / cfg.rise_secs) as f32)) } else { 1.0 };
    let bounce = cfg.bounce.filter(|_| lit.live && lit.exit.is_none() && !w.line_synced).map(|b| (word_bounce(e, &b, bounce_secs(&b, w)), b.side_em * ((e as f32 / bounce_secs(&b, w).max(0.01)).clamp(0.0, 1.0) * std::f32::consts::PI).sin()));
    let mut out = Vec::with_capacity(n);
    let mut k = 0;
    for row in &g.rows {
        for gl in &row.row.glyphs {
            let idx = k;
            k += 1;
            let uv = gl.uv_rect;
            if uv.is_nothing() {
                continue;
            }
            let mut r = Rect::from_min_size(w.pos + row.pos.to_vec2() + gl.pos.to_vec2() + uv.offset + vec2(w.spacing * idx as f32, rise), uv.size);
            if rest < 1.0 {
                let c = pos2(w.pos.x + w.w * 0.5, w.pos.y + w.h * 0.5);
                r = Rect::from_min_max(c + (r.min - c) * rest, c + (r.max - c) * rest);
            }
            if let Some((b, side)) = bounce {
                // The word moves as one, about its centre.
                let c = pos2(w.pos.x + w.w * 0.5, w.pos.y + w.h * 0.5);
                r = Rect::from_min_max(c + (r.min - c) * b.1, c + (r.max - c) * b.1).translate(vec2(side * em, -b.0 * em));
            }
            if let Some(waves) = &waves {
                let (lift, s) = letter_float(e, waves[idx], w.long, cfg);
                // Letters scale about their own centre (`.blyrics--letter` box).
                let c = pos2(w.pos.x + row.pos.x + gl.pos.x + gl.advance_width * 0.5, r.center().y);
                r = Rect::from_min_max(c + (r.min - c) * s, c + (r.max - c) * s).translate(vec2(0.0, -lift * em));
            }
            if let Some((cx, tx, sx)) = wob {
                let f = |x: f32| cx + (x - cx) * sx + tx;
                r = Rect::from_x_y_ranges(f(r.left())..=f(r.right()), r.y_range());
            }
            let uvr = Rect::from_min_max(
                pos2(uv.min[0] as f32 / atlas[0] as f32, uv.min[1] as f32 / atlas[1] as f32),
                pos2(uv.max[0] as f32 / atlas[0] as f32, uv.max[1] as f32 / atlas[1] as f32),
            );
            out.push(GlyphQuad { rect: r, uv: uvr, letter: idx, letters: n });
        }
    }
    out
}

/// Draw a glyph quad, alpha following the lit gradient: 1 left of `a`, 0 right of `b`
/// (screen x), linear between, so the soft edge is exact and never a box.
fn push_quad(mesh: &mut Mesh, r: Rect, uv: Rect, color: Color32, alpha: &dyn Fn(f32) -> f32, cuts: &[f32]) {
    let mut xs = vec![r.left()];
    xs.extend(cuts.iter().copied().filter(|&c| c > r.left() && c < r.right()));
    xs.push(r.right());
    for s in xs.windows(2) {
        let (x0, x1) = (s[0], s[1]);
        if x1 - x0 <= 0.0 {
            continue;
        }
        let (a0, a1) = (alpha(x0), alpha(x1));
        if a0 <= 0.0 && a1 <= 0.0 {
            continue;
        }
        let u = |x: f32| uv.left() + (x - r.left()) / r.width().max(1e-6) * uv.width();
        let base = mesh.vertices.len() as u32;
        let c = |a: f32| theme::with_alpha(color, a);
        mesh.vertices.push(Vertex { pos: pos2(x0, r.top()), uv: pos2(u(x0), uv.top()), color: c(a0) });
        mesh.vertices.push(Vertex { pos: pos2(x1, r.top()), uv: pos2(u(x1), uv.top()), color: c(a1) });
        mesh.vertices.push(Vertex { pos: pos2(x1, r.bottom()), uv: pos2(u(x1), uv.bottom()), color: c(a1) });
        mesh.vertices.push(Vertex { pos: pos2(x0, r.bottom()), uv: pos2(u(x0), uv.bottom()), color: c(a0) });
        mesh.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

/// The lit gradient over one glyph, in screen x: (alpha function, cut points).
/// Letter mode follows the per-letter masks (index space); otherwise the word box.
/// `rtl`: the word reads right to left, so it's lit from the right.
fn lit_gradient(q: &Rect, letter: usize, letters: usize, word: Rect, look: &WordLook, by_letter: bool, rtl: bool) -> (Box<dyn Fn(f32) -> f32>, Vec<f32>) {
    let (s, e) = (look.start, look.end);
    if rtl && !by_letter {
        let xs = word.right() - word.width() * s;
        let xe = word.right() - word.width() * e - 1.0;
        let f = move |x: f32| if x >= xs { 1.0 } else if x <= xe { 0.0 } else { 1.0 - (xs - x) / (xs - xe) };
        return (Box::new(f), vec![xe, xs]);
    }
    if by_letter {
        let n = letters.max(1) as f32;
        // In letter units across this glyph: X = letter + (x - left) / width.
        let (l, w) = (q.left(), q.width().max(1e-6));
        let i = letter as f32;
        let x_at = |v: f32| l + (v * n - i) * w; // where X/n == v
        let (xs, xe) = (x_at(s), x_at(e) + 1.0);
        let f = move |x: f32| if x <= xs { 1.0 } else if x >= xe { 0.0 } else { 1.0 - (x - xs) / (xe - xs) };
        (Box::new(f), vec![xs, xe])
    } else {
        let xs = word.left() + word.width() * s;
        let xe = word.left() + word.width() * e + 1.0;
        let f = move |x: f32| if x <= xs { 1.0 } else if x >= xe { 0.0 } else { 1.0 - (x - xs) / (xe - xs) };
        (Box::new(f), vec![xs, xe])
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_words(
    painter: &egui::Painter,
    ctx: &egui::Context,
    cfg: &Cfg,
    words: &[Word],
    groups: &[Group],
    xf: &Xf,
    lit: &LitState,
    dim: f32,
    opacity: f32,
    tints: (Color32, Color32),
    atlas: [usize; 2],
    glow: &mut Vec<GlowJob>,
    key: (u64, usize, u32),
) {
    let no_cut: [f32; 0] = [];
    let mut base = Mesh::with_texture(TextureId::default());
    let mut per_word = Vec::with_capacity(words.len());
    for w in words {
        let gr = &groups[w.group];
        // Wobble: the group pops with its first word's timing (transform-origin: centre).
        let wob = if cfg.wobble && gr.wobbles && lit.live && lit.exit.is_none() {
            let first = &words[gr.first];
            let (tx, sx) = wobble(lit.t - first.start, cfg.wobble_secs);
            let ws = &words[gr.words.clone()];
            let l = ws.iter().map(|w| w.pos.x).fold(f32::INFINITY, f32::min);
            let r = ws.iter().map(|w| w.pos.x + w.w).fold(f32::NEG_INFINITY, f32::max);
            ((sx - 1.0).abs() > 1e-4 || tx.abs() > 1e-4).then_some(((l + r) * 0.5, tx * first.font.size, sx))
        } else {
            None
        };
        let quads = word_glyphs(ctx, w, lit, cfg, wob, atlas);
        let dim_c = if w.bg { dim * cfg.bg_alpha } else { dim } * opacity;
        // A soft shadow: two faint copies, so it's no more than a little extra geometry.
        // On words not yet lit, in a theme that keeps it off lit ones.
        let shadowed = !cfg.shadow_unsung || lit.t < w.start || !lit.live;
        if let Some([down, a]) = cfg.word_shadow.filter(|_| shadowed) {
            let k = cfg.rem() / 10.0;
            for (d, part) in [(down * (1.0 - 0.4 * cfg.shadow_blur), 0.6), (down * (1.0 + 0.2 * cfg.shadow_blur), 0.4)] {
                for q in &quads {
                    push_quad(&mut base, xf.rect(q.rect.translate(vec2(0.0, d * k))), q.uv, theme::shade(a * part * opacity), &|_| 1.0, &no_cut);
                }
            }
        }
        for q in &quads {
            push_quad(&mut base, xf.rect(q.rect), q.uv, theme::with_alpha(tints.1, dim_c), &|_| 1.0, &no_cut);
        }
        per_word.push((quads, wob));
    }
    // The dim run, then the highlight run over it (`.blyrics-highlight-run`, z-index 1):
    // each word's drop-shadow, then its lit text.
    painter.add(Shape::mesh(base));
    for (wi, (w, (quads, wob))) in words.iter().zip(per_word).enumerate() {
        let look = word_look(w, lit, cfg);
        if look.opacity <= 0.0 || look.end <= 0.0 {
            continue;
        }
        let word_box = {
            let (cx, tx, sx) = wob.unwrap_or((0.0, 0.0, 1.0));
            let f = |x: f32| cx + (x - cx) * sx + tx;
            xf.rect(Rect::from_min_size(pos2(f(w.pos.x), w.pos.y), vec2(w.w * sx, w.h)))
        };
        let mut m = Mesh::with_texture(TextureId::default());
        let color = theme::with_alpha(tints.0, look.opacity * opacity);
        let mut screen_quads = Vec::with_capacity(quads.len());
        for q in &quads {
            let r = xf.rect(q.rect);
            let (alpha, cuts) = lit_gradient(&r, q.letter, q.letters, word_box, &look, w.letters, w.rtl);
            if let Some((width, stroke)) = cfg.stroke {
                // A text stroke: the lit glyph in the stroke's colour around itself.
                let c = theme::with_alpha(stroke, look.opacity * opacity);
                for d in [vec2(-1.0, 0.0), vec2(1.0, 0.0), vec2(0.0, -1.0), vec2(0.0, 1.0)] {
                    push_quad(&mut m, r.translate(d * width), q.uv, c, alpha.as_ref(), &cuts);
                }
            }
            push_quad(&mut m, r, q.uv, color, alpha.as_ref(), &cuts);
            // A coloured band behind the sweep's edge, one stop after another.
            let stops = cfg.band.len();
            if stops > 0 && look.start < cfg.sweep_end.0 - 1e-3 && lit.exit.is_none() && !w.line_synced {
                let step = cfg.band_width / stops as f32 * word_box.width();
                let lead = word_box.left() + word_box.width() * look.start;
                for (si, stop) in cfg.band.iter().enumerate() {
                    let hi = lead - step * si as f32;
                    let lo = hi - step;
                    let band = move |x: f32| if x > hi || x < lo { 0.0 } else { 1.0 - (x - (lo + hi) / 2.0).abs() / (step / 2.0).max(1e-3) };
                    push_quad(&mut m, r, q.uv, theme::with_alpha(*stop, look.opacity * opacity), &band, &[lo, (lo + hi) / 2.0, hi]);
                }
            }
            screen_quads.push((r, q.uv, q.letter, q.letters, 1.0));
        }
        if look.glow > 0.05 && look.glow_alpha >= GLOW_INVISIBLE && !dev_off("glow") {
            glow.push(GlowJob {
                key: (key.0, key.1, key.2, wi as u32),
                quads: screen_quads,
                word: word_box,
                by_letter: w.letters,
                rtl: w.rtl,
                // In steps, so a fading glow's image can be kept between them.
                radius: ((look.glow * xf.scale * 4.0).round() / 4.0).max(0.25),
                pills: Vec::new(),
                color: theme::with_alpha(lit.glow, look.glow_alpha * look.opacity * opacity),
                look,
                layer: painter.add(Shape::Noop),
            });
        }
        if !m.is_empty() {
            painter.add(Shape::mesh(m));
        }
    }
}

/// An instrumental break in a theme's own style: three dots filling in turn, or a ring closing.
#[allow(clippy::too_many_arguments)]
fn paint_break(painter: &egui::Painter, cfg: &Cfg, b: &Block, xf: &Xf, t: f64, now: f64, st: LineState, dim: f32, content_w: f32) -> bool {
    let e = t - b.start;
    let d = b.dur.max(0.001);
    let p = (e / d).clamp(0.0, 1.0) as f32;
    let mut lit = if st.selected && e >= 0.0 { EASE.at((e / 0.15) as f32) } else { 0.0 };
    if let Some(x) = st.exit {
        lit = 1.0 - EASE.at(((now - x) / FADE_OUT) as f32);
    }
    let em = cfg.inst_em();
    let (r, spacing) = cfg.dot_em.map_or((em * 0.14, 2.5), |[size, gap]| (em * size * 0.5, (size + gap) / (size * 0.5).max(0.01)));
    let total = r * (2.0 + 2.0 * spacing);
    let x0 = match b.align {
        Align::Left => r,
        Align::Right => content_w - total,
        Align::Center => (content_w - total) * 0.5,
    };
    let cy = cfg.padding + cfg.line_h() * 0.5;
    match cfg.instrumental {
        Instrumental::Hidden | Instrumental::Note => {}
        Instrumental::Glyph => {
            let c = xf.point(pos2(x0 + em * 0.3, cy));
            if cfg.inst_glow > 0.0 {
                let s = egui::epaint::Shadow { offset: [0, 0], blur: (em * 0.8).min(255.0) as u8, spread: 0, color: theme::with_alpha(cfg.active, cfg.inst_glow * (dim + lit).min(1.0)) };
                painter.add(s.as_shape(Rect::from_center_size(c, Vec2::splat(em * 0.5)), egui::CornerRadius::same((em * 0.25) as u8)));
            }
            // One laid-out size, through the transform: a breathing size would add new
            // glyphs to the font atlas every frame until egui has to empty it.
            let color = theme::with_alpha(cfg.active, dim + (1.0 - dim) * lit);
            let g = painter.layout_no_wrap(crate::theme::icon::MUSIC.to_string(),crate::theme::icon_font(em * 0.8), color);
            let atlas = painter.ctx().fonts(|f| f.font_image_size());
            paint_galley(painter, &g, pos2(x0 + em * 0.3, cy) - g.size() * 0.5, xf, color, atlas);
        }
        Instrumental::Dots => {
            for i in 0..3 {
                let c = xf.point(pos2(x0 + r * (1.0 + spacing * i as f32), cy));
                let fill = ((p * 3.0 - i as f32).clamp(0.0, 1.0) * lit).max(0.0);
                painter.circle_filled(c, r * xf.scale, theme::with_alpha(cfg.active, dim + (1.0 - dim) * fill));
            }
        }
        Instrumental::Ring => {
            let c = xf.point(pos2(x0 + em * 0.4, cy));
            let radius = em * 0.36 * xf.scale;
            // The ring's edge at an angle: a theme's ring ripples as it goes.
            let wavy = |a: f32| radius * if cfg.ring_wave { 1.0 + 0.07 * (a * 8.0 + now as f32 * 3.0).sin() } else { 1.0 };
            if cfg.ring_wave {
                let round: Vec<Pos2> = (0..=96).map(|i| {
                    let a = i as f32 / 96.0 * std::f32::consts::TAU;
                    c + vec2(a.cos(), a.sin()) * wavy(a)
                }).collect();
                painter.add(Shape::line(round, egui::Stroke::new(em * 0.08, theme::with_alpha(cfg.active, dim))));
            } else {
                painter.circle_stroke(c, radius, egui::Stroke::new(em * 0.08, theme::with_alpha(cfg.active, dim)));
            }
            let n = 48;
            let arc: Vec<Pos2> = (0..=n).map(|i| {
                let a = -std::f32::consts::FRAC_PI_2 + i as f32 / n as f32 * p * std::f32::consts::TAU;
                c + vec2(a.cos(), a.sin()) * wavy(a)
            }).collect();
            painter.add(Shape::line(arc, egui::Stroke::new(em * 0.08, theme::with_alpha(cfg.active, lit))));
        }
    }
    (st.selected && e >= 0.0 && e <= d) || st.exit.is_some()
}

// ------------------------------------------------------------------ glow

/// A word's drop-shadow to rasterize once this frame's glyphs are known.
pub struct GlowJob {
    key: (u64, usize, u32, u32),
    quads: Vec<(Rect, Rect, usize, usize, f32)>,
    word: Rect,
    look: WordLook,
    by_letter: bool,
    rtl: bool,
    /// Blur radius in points (CSS: std deviation = radius / 2).
    radius: f32,
    /// Romanization pills under the text (box, corner radius): `rgba(255,255,255,.05)` with
    /// a 1px `rgba(255,255,255,.1)` border.
    pills: Vec<(Rect, f32)>,
    color: Color32,
    /// Placeholder in the paint list, under the word's lit text.
    layer: ShapeIdx,
}

/// A GL texture sized in 64 px steps. A new image that fits is written into it in place
/// (`glTexSubImage2D`); a new texture or a new size makes the driver allocate, and at 144
/// frames a second it kept over 100 MB of those allocations (a new texture per blur step of
/// every line was the bulk of it).
struct Slot {
    tex: TextureHandle,
    size: [usize; 2],
    /// The part the current image covers.
    used: [usize; 2],
    /// Of the current image: an unchanged one (a glow that has stopped moving) isn't sent again.
    hash: u64,
    /// What a word's glow was drawn from (`glow_source`): the same again isn't drawn again.
    source: u64,
}

const SLOT_STEP: usize = 64;
/// Unused slots kept for the next glow or line to write into.
const SPARE_SLOTS: usize = 8;

impl Slot {
    fn uv(&self) -> Rect {
        Rect::from_min_max(pos2(0.0, 0.0), pos2(self.used[0] as f32 / self.size[0] as f32, self.used[1] as f32 / self.size[1] as f32))
    }
}

/// A blurred line's radius in 1/4 px and scale in 1/1000.
type LineStep = (i32, i32);

/// What a blurred line's image is kept for. Its scale only matters as far as the blur can
/// show it: a soft glow is drawn again for each 5% and a slight blur for each 1%, so a line
/// easing to its new size isn't blurred afresh on every frame on the way.
fn line_step(radius: f32, scale: f32) -> LineStep {
    let per = if radius >= 8.0 { 20.0 } else if radius >= 1.0 { 100.0 } else { 1000.0 };
    ((radius * 4.0) as i32, ((scale * per).round() / per * 1000.0).round() as i32)
}

#[derive(Default)]
struct Glow {
    /// A copy of the font atlas, refreshed when glyphs we need aren't in it yet.
    atlas: Option<Arc<Coverage>>,
    textures: HashMap<(u64, usize, u32, u32), (Slot, f64)>,
    /// Per line: its blurred image, the radius (in 1/4 px) and scale (in 1/1000) it was
    /// drawn at, where it goes.
    lines: HashMap<(u64, usize), (Slot, LineStep, Rect, f64)>,
    spare: Vec<Slot>,
    scratch: Vec<f32>,
    note: Option<(usize, Arc<Vec<f32>>)>,
    note_tex: Option<(usize, TextureHandle)>,
    /// What the note's fill and glow textures show: size, progress and wave, in steps.
    note_fill: Option<(usize, u32, u32)>,
}

impl Glow {
    fn atlas(&mut self, ctx: &egui::Context, size: [usize; 2], need: &[Rect]) -> Arc<Coverage> {
        let stale = match &self.atlas {
            None => true,
            Some(a) => a.size != size || need.iter().any(|uv| glyph_blank(a, *uv)),
        };
        if stale {
            // Only the coverage (alpha) is kept: a quarter of the RGBA image egui hands out.
            let img = ctx.fonts(|f| f.image());
            self.atlas = Some(Arc::new(Coverage { size: img.size, alpha: img.pixels.iter().map(|p| p.a()).collect() }));
        }
        self.atlas.clone().unwrap()
    }

    /// Puts `img` in a texture: `old` if it fits, else a spare one that does, else a new one.
    fn upload(&mut self, ctx: &egui::Context, old: Option<Slot>, img: ColorImage, name: &str) -> Slot {
        let used = img.size;
        let image_hash = hash((used, &img.pixels));
        if let Some(s) = &old
            && s.used == used
            && s.hash == image_hash
        {
            return old.unwrap();
        }
        // The image plus a transparent column and row, so filtering at its edge reads what it
        // did on a texture of its own; whatever an earlier, bigger image left past them is
        // never sampled.
        let (w, h) = (used[0] + 1, used[1] + 1);
        let bucket = [w.next_multiple_of(SLOT_STEP), h.next_multiple_of(SLOT_STEP)];
        let fits = |s: &Slot| s.size[0] >= w && s.size[1] >= h && s.size[0] * s.size[1] <= 2 * bucket[0] * bucket[1];
        let slot = match old {
            Some(s) if fits(&s) => Some(s),
            Some(s) => {
                self.recycle(s);
                None
            }
            None => None,
        };
        let slot = slot.or_else(|| self.spare.iter().position(fits).map(|k| self.spare.swap_remove(k)));
        let size = slot.as_ref().map_or(bucket, |s| s.size);
        // A new texture gets its full size; an old one only the part being drawn.
        let (pw, ph) = if slot.is_some() { (w, h) } else { (size[0], size[1]) };
        let mut pixels = vec![Color32::TRANSPARENT; pw * ph];
        for (y, row) in img.pixels.chunks_exact(used[0]).enumerate() {
            pixels[y * pw..y * pw + used[0]].copy_from_slice(row);
        }
        let img = ColorImage { size: [pw, ph], source_size: vec2(pw as f32, ph as f32), pixels };
        match slot {
            Some(mut s) => {
                s.tex.set_partial([0, 0], img, TextureOptions::LINEAR);
                s.used = used;
                s.hash = image_hash;
                s
            }
            None => Slot { tex: ctx.load_texture(name, img, TextureOptions::LINEAR), size, used, hash: image_hash, source: 0 },
        }
    }

    fn recycle(&mut self, s: Slot) {
        if self.spare.len() >= SPARE_SLOTS {
            self.spare.remove(0);
        }
        self.spare.push(s);
    }

    fn finish(&mut self, ctx: &egui::Context, painter: &egui::Painter, jobs: Vec<GlowJob>, atlas_size: [usize; 2], ppp: f32) {
        let now = ctx.input(|i| i.time);
        if !jobs.is_empty() {
            let uvs: Vec<Rect> = jobs.iter().flat_map(|j| j.quads.iter().map(|q| q.1)).collect();
            let atlas = self.atlas(ctx, atlas_size, &uvs);
            for j in jobs {
                let bounds = j.quads.iter().fold(Rect::NOTHING, |a, q| a.union(q.0));
                let source = glow_source(&j, bounds, ppp);
                let (slot, rect) = match self.textures.remove(&j.key) {
                    // The same picture, wherever the word has moved to.
                    Some((s, _)) if s.source == source => {
                        let pad = ((j.radius * 0.5 * ppp).max(0.3) * 3.0).ceil() + 2.0;
                        let rect = Rect::from_min_size(bounds.min - Vec2::splat(pad / ppp), vec2(s.used[0] as f32, s.used[1] as f32) / ppp);
                        (s, rect)
                    }
                    old => {
                        let Some((img, rect)) = rasterize_glow(&atlas, &j, ppp, &mut self.scratch) else {
                            if let Some(o) = old {
                                self.textures.insert(j.key, o);
                            }
                            continue;
                        };
                        let mut s = self.upload(ctx, old.map(|(s, _)| s), img, "lyric-glow");
                        s.source = source;
                        (s, rect)
                    }
                };
                let mut mesh = Mesh::with_texture(slot.tex.id());
                mesh.add_rect_with_uv(rect, slot.uv(), j.color);
                self.textures.insert(j.key, (slot, now));
                painter.set(j.layer, Shape::mesh(mesh));
            }
        }
        let gone: Vec<Slot> = (self.textures.extract_if(|_, (_, used)| now - *used >= 1.0).map(|(_, (s, _))| s))
            .chain(self.lines.extract_if(|_, (.., used)| now - *used >= 4.0).map(|(_, (s, ..))| s))
            .collect();
        for s in gone {
            self.recycle(s);
        }
    }

    /// The image `blurred_line` made for `key`, if it's still at this radius and scale.
    fn cached_line(&mut self, ctx: &egui::Context, key: (u64, usize), radius: f32, scale: f32) -> Option<(TextureId, Rect, Rect)> {
        let (s, r, rect, used) = self.lines.get_mut(&key)?;
        if *r != line_step(radius, scale) {
            return None;
        }
        *used = ctx.input(|i| i.time);
        Some((s.tex.id(), *rect, s.uv()))
    }

    /// A whole line blurred (`filter: blur()`): its words, each at its own alpha, its text
    /// decorations and pills, in content coordinates, rendered at the line's `scale` so the
    /// image isn't resampled on screen. Cached per line at its latest radius and scale.
    #[allow(clippy::too_many_arguments)]
    fn blurred_line(
        &mut self,
        ctx: &egui::Context,
        key: (u64, usize),
        radius: f32,
        words: &[(&Word, f32)],
        texts: &[(Arc<Galley>, Pos2, f32)],
        pills: &[(Rect, f32)],
        scale: f32,
        atlas_size: [usize; 2],
        ppp: f32,
    ) -> Option<(TextureId, Rect, Rect)> {
        if dev_off("blur") {
            return None;
        }
        if let Some(hit) = self.cached_line(ctx, key, radius, scale) {
            return Some(hit);
        }
        let now = ctx.input(|i| i.time);
        let step = line_step(radius, scale);
        let uv_of = |min: [u16; 2], max: [u16; 2]| {
            Rect::from_min_max(
                pos2(min[0] as f32 / atlas_size[0] as f32, min[1] as f32 / atlas_size[1] as f32),
                pos2(max[0] as f32 / atlas_size[0] as f32, max[1] as f32 / atlas_size[1] as f32),
            )
        };
        let mut quads = Vec::new();
        let push_galley = |g: &Galley, origin: Pos2, spacing: f32, weight: f32, quads: &mut Vec<(Rect, Rect, usize, usize, f32)>| {
            let mut k = 0;
            for row in &g.rows {
                for gl in &row.row.glyphs {
                    let idx = k;
                    k += 1;
                    let uv = gl.uv_rect;
                    if uv.is_nothing() {
                        continue;
                    }
                    let r = Rect::from_min_size(origin + row.pos.to_vec2() + gl.pos.to_vec2() + uv.offset + vec2(spacing * idx as f32, 0.0), uv.size);
                    quads.push((r, uv_of(uv.min, uv.max), 0, 1, weight));
                }
            }
        };
        for (w, weight) in words {
            let g = ctx.fonts_mut(|fv| fv.layout_no_wrap(w.text.clone(), w.font.clone(), Color32::WHITE));
            push_galley(&g, w.pos, w.spacing, *weight, &mut quads);
        }
        for (g, at, weight) in texts {
            push_galley(g, *at, 0.0, *weight, &mut quads);
        }
        if quads.is_empty() {
            return None;
        }
        let uvs: Vec<Rect> = quads.iter().map(|q| q.1).collect();
        let atlas = self.atlas(ctx, atlas_size, &uvs);
        let bounds = quads.iter().fold(Rect::NOTHING, |a, q| a.union(q.0));
        let job = GlowJob {
            key: (0, 0, 0, 0),
            quads,
            word: bounds,
            look: WordLook { opacity: 1.0, start: 2.0, end: 3.0, glow: 0.0, glow_alpha: 0.0 },
            by_letter: false,
            rtl: false,
            // CSS `blur(r)` is a Gaussian with σ = r.
            radius: radius * 2.0,
            pills: pills.to_vec(),
            color: Color32::WHITE,
            layer: ShapeIdx(0),
        };
        let (img, content_rect) = rasterize_glow(&atlas, &job, ppp * scale, &mut self.scratch)?;
        let old = self.lines.remove(&key).map(|(s, ..)| s);
        let slot = self.upload(ctx, old, img, "lyric-blur");
        let out = (slot.tex.id(), content_rect, slot.uv());
        self.lines.insert(key, (slot, step, content_rect, now));
        Some(out)
    }
}

/// The diffuse light behind the active line: an ellipse brightest towards the line's start,
/// fading out before the edges of `r`.
fn aura_mesh(r: Rect, color: Color32) -> egui::Mesh {
    const STEPS: usize = 48;
    const RINGS: [(f32, f32); 5] = [(0.0, 1.0), (0.32, 0.57), (0.59, 0.27), (0.76, 0.1), (1.0, 0.0)];
    let c = pos2(r.left() + r.width() * 0.16, r.center().y);
    let (left, right, ry) = (r.width() * 0.16, r.width() * 0.84, r.height() * 0.5);
    let mut m = egui::Mesh::default();
    for &(t, a) in &RINGS {
        for i in 0..STEPS {
            let (sin, cos) = (i as f32 / STEPS as f32 * std::f32::consts::TAU).sin_cos();
            let x = cos * if cos < 0.0 { left } else { right };
            m.colored_vertex(c + vec2(x, sin * ry) * t, color.gamma_multiply(a));
        }
    }
    let at = |ring: usize, i: usize| (ring * STEPS + i % STEPS) as u32;
    for ring in 0..RINGS.len() - 1 {
        for i in 0..STEPS {
            m.add_triangle(at(ring, i), at(ring, i + 1), at(ring + 1, i));
            m.add_triangle(at(ring, i + 1), at(ring + 1, i + 1), at(ring + 1, i));
        }
    }
    m
}

/// Whether a glyph's texels are all empty in our atlas copy (it was added since).
/// The font atlas's coverage, one byte per texel.
struct Coverage {
    size: [usize; 2],
    alpha: Vec<u8>,
}

fn glyph_blank(a: &Coverage, uv: Rect) -> bool {
    let (w, h) = (a.size[0] as f32, a.size[1] as f32);
    let (x0, y0, x1, y1) = ((uv.left() * w) as usize, (uv.top() * h) as usize, (uv.right() * w) as usize, (uv.bottom() * h) as usize);
    if x1 <= x0 || y1 <= y0 || x1 > a.size[0] || y1 > a.size[1] {
        return true;
    }
    for y in y0..y1 {
        for x in x0..x1 {
            if a.alpha[y * a.size[0] + x] > 0 {
                return false;
            }
        }
    }
    true
}

/// The lit glyph pixels (coverage × gradient) blurred: CSS `drop-shadow(0 0 r color)`
/// is a Gaussian with σ = r / 2 of the element's alpha, drawn under it. `ppp`: bitmap
/// pixels per unit of the quads' coordinates.
fn rasterize_glow(atlas: &Coverage, j: &GlowJob, ppp: f32, scratch: &mut Vec<f32>) -> Option<(ColorImage, Rect)> {
    let sigma = (j.radius * 0.5 * ppp).max(0.3);
    let pad = (sigma * 3.0).ceil() + 2.0;
    let bounds = j.quads.iter().map(|q| q.0).chain(j.pills.iter().map(|p| p.0)).fold(Rect::NOTHING, |a, r| a.union(r));
    if !bounds.is_positive() {
        return None;
    }
    let origin = bounds.min - Vec2::splat(pad / ppp);
    let w = (bounds.width() * ppp + pad * 2.0).ceil() as usize;
    let h = (bounds.height() * ppp + pad * 2.0).ceil() as usize;
    if w == 0 || h == 0 || w * h > 4_000_000 {
        return None;
    }
    scratch.clear();
    scratch.resize(w * h, 0.0);
    for &(r, radius) in &j.pills {
        paint_pill(scratch, w, h, (r.min - origin) * ppp, r.size() * ppp, radius * ppp, ppp);
    }
    let (aw, ah) = (atlas.size[0] as f32, atlas.size[1] as f32);
    for &(r, uv, letter, letters, weight) in &j.quads {
        let (alpha, _) = lit_gradient(&r, letter, letters, j.word, &j.look, j.by_letter, j.rtl);
        let tx0 = (uv.left() * aw).round() as isize;
        let ty0 = (uv.top() * ah).round() as isize;
        let tw = ((uv.right() - uv.left()) * aw).round() as isize;
        let th = ((uv.bottom() - uv.top()) * ah).round() as isize;
        if tw <= 0 || th <= 0 {
            continue;
        }
        // Destination box in bitmap pixels (glyphs may be scaled a little by the line).
        let dx0 = (r.left() - origin.x) * ppp;
        let dy0 = (r.top() - origin.y) * ppp;
        let dw = r.width() * ppp;
        let dh = r.height() * ppp;
        let (ix0, iy0) = (dx0.floor().max(0.0) as usize, dy0.floor().max(0.0) as usize);
        let (ix1, iy1) = (((dx0 + dw).ceil() as usize).min(w), ((dy0 + dh).ceil() as usize).min(h));
        // Bilinear, with nothing outside the glyph's own texels: at another scale than the
        // atlas's, nearest-texel picks drop or double rows and the edges go jagged.
        let texel = |x: isize, y: isize| -> f32 {
            if x < 0 || y < 0 || x >= tw || y >= th {
                return 0.0;
            }
            atlas.alpha.get((ty0 + y) as usize * atlas.size[0] + (tx0 + x) as usize).map_or(0.0, |&p| p as f32 / 255.0)
        };
        for y in iy0..iy1 {
            let fy = (y as f32 + 0.5 - dy0) / dh * th as f32 - 0.5;
            let (sy, ky) = (fy.floor() as isize, fy - fy.floor());
            for x in ix0..ix1 {
                let fx = (x as f32 + 0.5 - dx0) / dw * tw as f32 - 0.5;
                let (sx, kx) = (fx.floor() as isize, fx - fx.floor());
                let top = texel(sx, sy) * (1.0 - kx) + texel(sx + 1, sy) * kx;
                let bottom = texel(sx, sy + 1) * (1.0 - kx) + texel(sx + 1, sy + 1) * kx;
                let cov = top * (1.0 - ky) + bottom * ky;
                if cov <= 0.0 {
                    continue;
                }
                let screen_x = origin.x + (x as f32 + 0.5) / ppp;
                let a = cov * alpha(screen_x) * weight;
                let d = &mut scratch[y * w + x];
                *d = (*d + a - *d * a).min(1.0);
            }
        }
    }
    gaussian(scratch, w, h, sigma);
    let pixels = scratch
        .iter()
        .map(|v| {
            let a = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            Color32::from_rgba_premultiplied(a, a, a, a)
        })
        .collect();
    let img = ColorImage { size: [w, h], source_size: vec2(w as f32, h as f32), pixels };
    Some((img, Rect::from_min_size(origin, vec2(w as f32, h as f32) / ppp)))
}

/// What a word's glow image depends on, in steps too small to see through the blur: its
/// glyphs' places within the word, the gradient and the radius. Not where the word is.
fn glow_source(j: &GlowJob, bounds: Rect, ppp: f32) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let step = |v: f32| (v * 4.0).round() as i32;
    for r in j.quads.iter().map(|q| q.0).chain(std::iter::once(j.word)) {
        [step(r.left() - bounds.left()), step(r.top() - bounds.top()), step(r.right() - bounds.left()), step(r.bottom() - bounds.top())].hash(&mut h);
    }
    let gradient = |v: f32| (v * 64.0).round() as i32;
    (gradient(j.look.start), gradient(j.look.end), j.radius.to_bits(), ppp.to_bits(), j.by_letter, j.rtl).hash(&mut h);
    h.finish().max(1)
}

/// Gaussian blur: a true kernel for small σ (Skia's cut-off is 2), else three box passes
/// per axis (what browsers use for large blurs). Boxes for a σ under 1 come out as one
/// 3-tap box, which looks blocky instead of soft.
fn gaussian(buf: &mut [f32], w: usize, h: usize, sigma: f32) {
    if sigma < 2.0 {
        let r = (sigma * 3.0).ceil() as usize;
        let mut k: Vec<f32> = (0..=2 * r).map(|i| (-(i as f32 - r as f32).powi(2) / (2.0 * sigma * sigma)).exp()).collect();
        let sum: f32 = k.iter().sum();
        k.iter_mut().for_each(|v| *v /= sum);
        let mut tmp = vec![0.0f32; buf.len()];
        convolve(buf, &mut tmp, w, h, &k, true);
        convolve(&tmp, buf, w, h, &k, false);
        return;
    }
    // Box sizes for n = 3 (W3C filter effects / "boxes for gauss").
    let n = 3.0f32;
    let ideal = (12.0 * sigma * sigma / n + 1.0).sqrt();
    let mut wl = ideal.floor() as i32;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wu = wl + 2;
    let m_ideal = (12.0 * sigma * sigma - n * (wl * wl) as f32 - 4.0 * n * wl as f32 - 3.0 * n) / (-4.0 * wl as f32 - 4.0);
    let m = m_ideal.round() as i32;
    let mut tmp = vec![0.0f32; buf.len()];
    for i in 0..3 {
        let size = if i < m { wl } else { wu };
        let r = ((size - 1) / 2).max(0) as usize;
        if r == 0 {
            continue;
        }
        box_pass(buf, &mut tmp, w, h, r, true);
        box_pass(&tmp, buf, w, h, r, false);
    }
}

/// One axis of a separable kernel (odd length, centred), zero past the edges.
fn convolve(src: &[f32], dst: &mut [f32], w: usize, h: usize, k: &[f32], horizontal: bool) {
    let r = (k.len() / 2) as isize;
    let (n, lines) = if horizontal { (w, h) } else { (h, w) };
    let at = |line: usize, i: usize| if horizontal { line * w + i } else { i * w + line };
    for line in 0..lines {
        for i in 0..n {
            let mut acc = 0.0;
            for (j, kv) in k.iter().enumerate() {
                let s = i as isize + j as isize - r;
                if s >= 0 && (s as usize) < n {
                    acc += src[at(line, s as usize)] * kv;
                }
            }
            dst[at(line, i)] = acc;
        }
    }
}

/// A romanization pill into an alpha bitmap, anti-aliased like egui's rect: the 5% fill
/// with its 1-point 10% border inside (`min`/`size`/`radius` in bitmap pixels).
fn paint_pill(buf: &mut [f32], w: usize, h: usize, min: Vec2, size: Vec2, radius: f32, ppp: f32) {
    let radius = radius.min(size.x * 0.5).min(size.y * 0.5);
    let half = size * 0.5;
    let centre = min + half;
    let (x0, y0) = (min.x.floor().max(0.0) as usize, min.y.floor().max(0.0) as usize);
    let (x1, y1) = (((min.x + size.x).ceil() as usize).min(w), ((min.y + size.y).ceil() as usize).min(h));
    let over = |d: &mut f32, a: f32| *d = (*d + a - *d * a).min(1.0);
    for y in y0..y1 {
        for x in x0..x1 {
            // Signed distance to the rounded box, negative inside.
            let p = vec2(x as f32 + 0.5, y as f32 + 0.5) - centre;
            let q = vec2(p.x.abs(), p.y.abs()) - half + Vec2::splat(radius);
            let d = vec2(q.x.max(0.0), q.y.max(0.0)).length() + q.x.max(q.y).min(0.0) - radius;
            let inside = (0.5 - d).clamp(0.0, 1.0);
            let border = inside - (0.5 - (d + ppp)).clamp(0.0, 1.0);
            let px = &mut buf[y * w + x];
            over(px, 0.05 * inside);
            over(px, 0.1 * border);
        }
    }
}

fn box_pass(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize, horizontal: bool) {
    let (n, lines) = if horizontal { (w, h) } else { (h, w) };
    let idx = |line: usize, i: usize| if horizontal { line * w + i } else { i * w + line };
    let norm = 1.0 / (2 * r + 1) as f32;
    for line in 0..lines {
        let mut acc = 0.0;
        for i in 0..=r.min(n - 1) {
            acc += src[idx(line, i)];
        }
        for i in 0..n {
            dst[idx(line, i)] = acc * norm;
            if i + r + 1 < n {
                acc += src[idx(line, i + r + 1)];
            }
            if i >= r {
                acc -= src[idx(line, i - r)];
            }
        }
    }
}

// ------------------------------------------------------------------ instrumental

/// The music note (Material Symbols `music_note`, 24-unit viewBox) as a polygon.
fn note_polygon() -> &'static [Pos2] {
    static NOTE: std::sync::OnceLock<Vec<Pos2>> = std::sync::OnceLock::new();
    NOTE.get_or_init(|| svg_path("M10 21q-1.65 0-2.825-1.175T6 17t1.175-2.825T10 13q.575 0 1.063.138t.937.412V4q0-.425.288-.712T13 3h4q.425 0 .713.288T18 4v2q0 .425-.288.713T17 7h-3v10q0 1.65-1.175 2.825T10 21"))
}

/// A minimal SVG path reader (M L H V Q T Z, absolute and relative) into a polyline.
fn svg_path(d: &str) -> Vec<Pos2> {
    let mut toks: Vec<String> = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, toks: &mut Vec<String>| {
        if !cur.is_empty() {
            toks.push(std::mem::take(cur));
        }
    };
    for c in d.chars() {
        if c.is_ascii_alphabetic() {
            flush(&mut cur, &mut toks);
            toks.push(c.to_string());
        } else if (c == '-' && !cur.is_empty() && !cur.ends_with('e')) || (c == '.' && cur.contains('.')) {
            // A sign, or a second decimal point, starts the next number.
            flush(&mut cur, &mut toks);
            cur.push(c);
        } else if c == ' ' || c == ',' {
            flush(&mut cur, &mut toks);
        } else {
            cur.push(c);
        }
    }
    flush(&mut cur, &mut toks);
    let mut out = Vec::new();
    let (mut p, mut start) = (pos2(0.0, 0.0), pos2(0.0, 0.0));
    let mut ctrl: Option<Pos2> = None;
    let mut i = 0;
    let mut cmd = 'M';
    let num = |toks: &[String], i: &mut usize| -> f32 {
        let v = toks.get(*i).and_then(|t| t.parse().ok()).unwrap_or(0.0);
        *i += 1;
        v
    };
    let quad = |out: &mut Vec<Pos2>, a: Pos2, c: Pos2, b: Pos2| {
        for k in 1..=8 {
            let t = k as f32 / 8.0;
            let u = 1.0 - t;
            out.push(pos2(u * u * a.x + 2.0 * u * t * c.x + t * t * b.x, u * u * a.y + 2.0 * u * t * c.y + t * t * b.y));
        }
    };
    while i < toks.len() {
        if toks[i].chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
            cmd = toks[i].chars().next().unwrap();
            i += 1;
        }
        let rel = cmd.is_ascii_lowercase();
        let base = if rel { p.to_vec2() } else { Vec2::ZERO };
        match cmd.to_ascii_uppercase() {
            'M' => {
                p = pos2(num(&toks, &mut i), num(&toks, &mut i)) + base;
                start = p;
                out.push(p);
                cmd = if rel { 'l' } else { 'L' };
                ctrl = None;
            }
            'L' => {
                p = pos2(num(&toks, &mut i), num(&toks, &mut i)) + base;
                out.push(p);
                ctrl = None;
            }
            'H' => {
                let x = num(&toks, &mut i);
                p = pos2(if rel { p.x + x } else { x }, p.y);
                out.push(p);
                ctrl = None;
            }
            'V' => {
                let y = num(&toks, &mut i);
                p = pos2(p.x, if rel { p.y + y } else { y });
                out.push(p);
                ctrl = None;
            }
            'Q' => {
                let c = pos2(num(&toks, &mut i), num(&toks, &mut i)) + base;
                let b = pos2(num(&toks, &mut i), num(&toks, &mut i)) + base;
                quad(&mut out, p, c, b);
                ctrl = Some(c);
                p = b;
            }
            'T' => {
                let c = ctrl.map_or(p, |c| p + (p - c));
                let b = pos2(num(&toks, &mut i), num(&toks, &mut i)) + base;
                quad(&mut out, p, c, b);
                ctrl = Some(c);
                p = b;
            }
            'Z' => {
                p = start;
                out.push(p);
                ctrl = None;
            }
            _ => i += 1,
        }
    }
    out
}

fn point_in_polygon(poly: &[Pos2], p: Pos2) -> bool {
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[j]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// The wave's top edge at viewBox x, morphing between the two paths (`m`: 0 high, 1 low)
/// and flattened toward its base (y = 4) by `flatten`.
fn wave_y(x: f32, m: f32, flatten: f32) -> f32 {
    // `M -4 3 Q 1 2 5 3 Q 10 4 14 3 Q 18 2 22 3 Q 26 4 30 3`; the low path mirrors the
    // control points around y = 3. Matching command lists interpolate point by point.
    let segs: [(f32, f32, f32, f32); 4] = [(-4.0, 1.0, 5.0, 2.0), (5.0, 10.0, 14.0, 4.0), (14.0, 18.0, 22.0, 2.0), (22.0, 26.0, 30.0, 4.0)];
    let (a, c, b, high) = *segs.iter().find(|s| x <= s.2).unwrap_or(&segs[3]);
    // x(t) = (1-t)^2 a + 2(1-t)t c + t^2 b; solve for t in [0, 1].
    let qa = a - 2.0 * c + b;
    let qb = 2.0 * (c - a);
    let qc = a - x;
    let t = if qa.abs() < 1e-6 {
        -qc / qb
    } else {
        let disc = (qb * qb - 4.0 * qa * qc).max(0.0).sqrt();
        let t1 = (-qb + disc) / (2.0 * qa);
        let t2 = (-qb - disc) / (2.0 * qa);
        if (0.0..=1.0).contains(&t1) { t1 } else { t2 }
    }
    .clamp(0.0, 1.0);
    let cy = lerp(high, 6.0 - high, m);
    let y = (1.0 - t) * (1.0 - t) * 3.0 + 2.0 * (1.0 - t) * t * cy + t * t * 3.0;
    4.0 - (4.0 - y) * flatten
}

#[allow(clippy::too_many_arguments)]
fn paint_instrumental(painter: &egui::Painter, glow: &mut Glow, ctx: &egui::Context, cfg: &Cfg, b: &Block, xf: &Xf, t: f64, now: f64, st: LineState, dim: f32, content_w: f32) -> bool {
    let em = cfg.inst_em();
    if cfg.instrumental != Instrumental::Note {
        return paint_break(painter, cfg, b, xf, t, now, st, dim, content_w);
    }
    // The icon: one em tall, 4/3 em wide, pulled 1/3 em toward the edge; viewBox 24 centred in it.
    let box_x = match b.align {
        Align::Left => -em / 3.0,
        Align::Right => content_w - em,
        Align::Center => (content_w - em * 4.0 / 3.0) * 0.5,
    };
    let row_h = cfg.line_h();
    let top = cfg.padding + (row_h - em) * 0.5;
    let unit = em / 24.0;
    let vb_origin = pos2(box_x + (em * 4.0 / 3.0 - em) * 0.5, top);
    let to_screen = |p: Pos2| xf.point(vb_origin + p.to_vec2() * unit);
    let e = t - b.start;
    let d = b.dur.max(0.001);
    // Fill opacity: in over 150 ms from the start, out over 0.5 s after the line.
    let mut fill_a = if st.selected && e >= 0.0 { EASE.at((e / 0.15) as f32) } else { 0.0 };
    if let Some(x) = st.exit {
        fill_a = 1.0 - EASE.at(((now - x) / FADE_OUT) as f32);
    }
    // In steps of a fraction of a pixel: between them the textures below are kept.
    let p = ((e / d).clamp(0.0, 1.0) as f32 * 128.0).round() / 128.0;
    // Clip: translateY(78%) -> translateY(-4%) of the 24-unit box, linear; the wave
    // flattens (scaleY 1.2 -> 0, ease-in) and morphs high <-> low every 1.25 s.
    let travel = lerp(0.78, -0.04, p) * 24.0;
    let flatten = lerp(1.2, 0.0, EASE_IN.at(p));
    let phase = ((e.max(0.0) % 1.25) / 1.25) as f32;
    let m = (EASE_IN_OUT.at(if phase < 0.5 { phase * 2.0 } else { (1.0 - phase) * 2.0 }) * 8.0).round() / 8.0;
    let sz =(em * ctx.pixels_per_point()).ceil() as usize + 2;
    let rect = Rect::from_min_max(to_screen(pos2(0.0, 0.0)), to_screen(pos2(24.0, 24.0)));
    let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    let note_tex = glow.note_texture(ctx, sz);
    painter.image(note_tex, rect, full, theme::with_alpha(Color32::WHITE, dim));
    if fill_a > 0.0 {
        // `feGaussianBlur stdDeviation=5` (viewBox units) at 0.6 under the fill.
        let sigma = 5.0 * sz as f32 / 24.0;
        let pad = (sigma * 3.0).ceil() as usize;
        let gpad = pad as f32 / sz as f32 * 24.0;
        let grect = Rect::from_min_max(to_screen(pos2(-gpad, -gpad)), to_screen(pos2(24.0 + gpad, 24.0 + gpad)));
        let key = (sz, (p * 128.0).round() as u32, (m * 8.0).round() as u32);
        let ((gt, guv), (ft, fuv)) = match glow.note_images(ctx, key) {
            Some(images) => images,
            None => {
                let mask = glow.note_mask(sz);
                let mut fill = vec![0.0f32; sz * sz];
                for y in 0..sz {
                    for x in 0..sz {
                        let v = mask[y * sz + x];
                        if v <= 0.0 {
                            continue;
                        }
                        let q = pos2((x as f32 + 0.5) / sz as f32 * 24.0, (y as f32 + 0.5) / sz as f32 * 24.0);
                        // The clip is the wave shape plus the rect under it (y >= 3.9), moved down.
                        if q.y >= wave_y(q.x, m, flatten).min(3.9) + travel {
                            fill[y * sz + x] = v;
                        }
                    }
                }
                let gw = sz + pad * 2;
                let mut g = vec![0.0f32; gw * gw];
                for y in 0..sz {
                    for x in 0..sz {
                        g[(y + pad) * gw + x + pad] = fill[y * sz + x];
                    }
                }
                gaussian(&mut g, gw, gw, sigma);
                let under = glow.set_tex(ctx, 1, mask_image(&g, gw, 0.6));
                let over = glow.set_tex(ctx, 2, mask_image(&fill, sz, 1.0));
                glow.note_fill = Some(key);
                (under, over)
            }
        };
        painter.image(gt, grect, guv, theme::with_alpha(cfg.active, fill_a));
        painter.image(ft, rect, fuv, theme::with_alpha(cfg.active, fill_a));
    }
    (st.selected && e >= 0.0 && e <= d) || st.exit.is_some()
}

fn mask_image(v: &[f32], w: usize, a: f32) -> ColorImage {
    ColorImage {
        size: [w, v.len() / w],
        source_size: vec2(w as f32, (v.len() / w) as f32),
        pixels: v
            .iter()
            .map(|v| {
                let q = ((v * a).clamp(0.0, 1.0) * 255.0).round() as u8;
                Color32::from_rgba_premultiplied(q, q, q, q)
            })
            .collect(),
    }
}

impl Glow {
    /// The note's coverage at `sz` pixels square (2x2 supersampled), cached.
    fn note_mask(&mut self, sz: usize) -> Arc<Vec<f32>> {
        if let Some((s, m)) = &self.note
            && *s == sz
        {
            return m.clone();
        }
        let poly = note_polygon();
        let mut out = vec![0.0f32; sz * sz];
        for y in 0..sz {
            for x in 0..sz {
                let mut c = 0;
                for (ox, oy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                    let q = pos2((x as f32 + ox) / sz as f32 * 24.0, (y as f32 + oy) / sz as f32 * 24.0);
                    if point_in_polygon(poly, q) {
                        c += 1;
                    }
                }
                out[y * sz + x] = c as f32 / 4.0;
            }
        }
        let m = Arc::new(out);
        self.note = Some((sz, m.clone()));
        m
    }

    fn note_texture(&mut self, ctx: &egui::Context, sz: usize) -> TextureId {
        if let Some((s, t)) = &self.note_tex
            && *s == sz
        {
            return t.id();
        }
        let mask = self.note_mask(sz);
        let t = ctx.load_texture("lyric-note", mask_image(&mask, sz, 1.0), TextureOptions::LINEAR);
        let id = t.id();
        self.note_tex = Some((sz, t));
        id
    }

    /// The note's glow and fill as last drawn, if that's what `key` asks for.
    fn note_images(&mut self, ctx: &egui::Context, key: (usize, u32, u32)) -> Option<((TextureId, Rect), (TextureId, Rect))> {
        if self.note_fill != Some(key) {
            return None;
        }
        let now = ctx.input(|i| i.time);
        let mut kept = |slot: u32| {
            self.textures.get_mut(&(u64::MAX, usize::MAX, slot, 0)).map(|(s, used)| {
                *used = now;
                (s.tex.id(), s.uv())
            })
        };
        Some((kept(1)?, kept(2)?))
    }

    /// The instrumental note's fill or glow this frame, and the part of the texture it covers.
    fn set_tex(&mut self, ctx: &egui::Context, slot: u32, img: ColorImage) -> (TextureId, Rect) {
        let key = (u64::MAX, usize::MAX, slot, 0);
        let now = ctx.input(|i| i.time);
        let old = self.textures.remove(&key).map(|(s, _)| s);
        let s = self.upload(ctx, old, img, "lyric-instrumental");
        let out = (s.tex.id(), s.uv());
        self.textures.insert(key, (s, now));
        out
    }
}

/// Dev hook: YTM_LYR_OFF=glow,blur,view leaves those parts out (memory breakdowns).
pub fn dev_off(what: &str) -> bool {
    static OFF: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    OFF.get_or_init(|| std::env::var("YTM_LYR_OFF").unwrap_or_default()).split(',').any(|x| x == what)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_rewritten_in_place() {
        let ctx = egui::Context::default();
        let img = |w: usize, h: usize| ColorImage::filled([w, h], Color32::WHITE);
        let mut g = Glow::default();
        let a = g.upload(&ctx, None, img(100, 40), "t");
        assert_eq!(a.size, [128, 64]);
        let id = a.tex.id();
        // Smaller, and a little bigger within the slot: the same texture, drawn in part.
        let b = g.upload(&ctx, Some(a), img(60, 30), "t");
        assert_eq!((b.tex.id(), b.used), (id, [60, 30]));
        assert_eq!(b.uv().max, pos2(60.0 / 128.0, 30.0 / 64.0));
        let c = g.upload(&ctx, Some(b), img(120, 60), "t");
        assert_eq!(c.tex.id(), id);
        // Outgrown: a new texture; the old one is kept for the next image that fits it.
        let d = g.upload(&ctx, Some(c), img(300, 40), "t");
        assert_ne!(d.tex.id(), id);
        assert_eq!(g.spare.len(), 1);
        let e = g.upload(&ctx, None, img(90, 50), "t");
        assert_eq!(e.tex.id(), id);
        // Far too big for a small image: not reused.
        let f = g.upload(&ctx, Some(d), img(10, 10), "t");
        assert_eq!(f.size, [64, 64]);
    }

    #[test]
    fn bezier_matches_css_ease() {
        // Reference values of CSS `ease` (cubic-bezier(0.25, 0.1, 0.25, 1)).
        assert!((EASE.at(0.5) - 0.8024).abs() < 0.002);
        assert!((EASE.at(0.25) - 0.4094).abs() < 0.002);
        assert!((EASE_IN.at(0.5) - 0.3153).abs() < 0.002);
        assert_eq!(EASE.at(0.0), 0.0);
        assert_eq!(EASE.at(1.0), 1.0);
    }

    #[test]
    fn parts_split_on_spaces_with_prorated_time() {
        let p = Part { start: 1000, dur: 1000, text: "ab cd".into(), bg: false };
        let t = split_part(&p);
        assert_eq!(t.len(), 3);
        assert!((t[0].start - 1.0).abs() < 1e-9 && (t[0].dur - 0.5).abs() < 1e-9);
        assert!(t[1].space);
        assert!((t[2].start - 1.5).abs() < 1e-9);
    }

    #[test]
    fn rtl_lines_run_from_the_right() {
        let mut fonts = egui::epaint::text::Fonts::new(Default::default(), egui::FontDefinitions::default());
        let mut fv = fonts.with_pixels_per_point(1.0);
        let font = FontId::proportional(20.0);
        let line = |text: &str| line_synced_parts(&Line { start: 0, dur: 1000, text: text.into(), ..Default::default() }).iter().flat_map(split_part).collect::<Vec<_>>();
        let x = |f: &Flow, word: &str| f.words.iter().find(|w| w.text == word).unwrap().pos.x;
        // Hebrew reads from the right; the English inside it keeps its order.
        let f = flow(&mut fv, line("שלום עולם hello world"), &font, 1000.0, 27.0, Align::Right, true, 0.0, 0.0);
        assert!(x(&f, "שלום") > x(&f, "עולם"));
        assert!(x(&f, "עולם") > x(&f, "world"));
        assert!(x(&f, "world") > x(&f, "hello"));
        // An English line with a Hebrew phrase: only the phrase turns around.
        let f = flow(&mut fv, line("my song שלום עולם remix"), &font, 1000.0, 27.0, Align::Left, false, 0.0, 0.0);
        assert!(x(&f, "song") < x(&f, "עולם") && x(&f, "עולם") < x(&f, "שלום") && x(&f, "שלום") < x(&f, "remix"));
        // A long line wraps instead of running past the edge.
        let f = flow(&mut fv, line(&"word ".repeat(40)), &font, 300.0, 27.0, Align::Left, false, 0.0, 0.0);
        assert!(f.height > 27.0 && f.words.iter().all(|w| w.pos.x + w.w <= 300.0 + 0.5));
    }

    /// Where each word of a two-word RTL phrase lands, laid out as before and as now, with
    /// the app's fonts: the first word belongs on the right (the old layout put it left).
    /// Needs the system fonts the app falls back to for Hebrew and Arabic.
    /// `cargo test --release -- --ignored rtl_word_positions --nocapture`
    #[test]
    #[ignore]
    fn rtl_word_positions() {
        let ctx = shot_ctx();
        let font = crate::theme::semibold(26.0);
        for (a, b) in [("אבג", "דהו"), ("عمر", "دياب")] {
            let text = format!("{a} {b}");
            let mean = |g: &Galley, w: &str| {
                let xs: Vec<f32> = g.rows.iter().flat_map(|r| r.row.glyphs.iter()).filter(|gl| w.contains(gl.chr)).map(|gl| gl.pos.x).collect();
                xs.iter().sum::<f32>() / xs.len().max(1) as f32
            };
            let (before, after) = ctx.fonts_mut(|f| (f.layout_no_wrap(text.clone(), font.clone(), Color32::WHITE), f.layout_job(crate::bidi::job(&text, font.clone(), Color32::WHITE))));
            println!("{text}: before first word x {:.0}, second {:.0} | after first {:.0}, second {:.0}", mean(&before, a), mean(&before, b), mean(&after, a), mean(&after, b));
            assert!(mean(&after, a) > mean(&after, b), "{text}: the first word should be on the right");
        }
    }

    /// Paused, the view settles and then asks for no frames, however long the gaps
    /// between the frames something else wakes it for.
    #[test]
    fn paused_lyrics_stop_asking_for_frames() {
        let ctx = shot_ctx();
        let words = |at: i32| (0..4).map(|k| Part { start: at + k * 1000, dur: 900, text: format!("word{k} "), bg: false }).collect::<Vec<_>>();
        let lines: Vec<Line> = (0..6).map(|i| Line { start: i * 4000, dur: 3900, text: "word0 word1 word2 word3".into(), parts: words(i * 4000), ..Default::default() }).collect();
        let lyrics = Lyrics { lines, sync: Sync::Word, provider: crate::lyrics::Provider::BLyricsRich, language: None, songwriters: vec![] };
        let style: &'static LyricStyle = Box::leak(Box::new(LyricStyle::default()));
        let mut view = View::default();
        // (frame time, song time, playing) → whether it asked for another frame soon.
        let mut frame = |now: f64, t: f64, playing: bool| {
            let input = egui::RawInput { time: Some(now), screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))), ..Default::default() };
            let mut animating = false;
            let mut out = ctx.run_ui(input, |ui| {
                let n = lyrics.lines.len();
                let f = Frame { lyrics: &lyrics, key: 1, roman: (0..n).map(|_| None).collect(), trans: vec![None; n], t, playing, now, rect: Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 500.0)), style, accent: Color32::RED, centered: false, scale: 1.0, fullscreen: false, glass_chip: None };
                animating = view.show(ui, f).animating;
            });
            out.textures_delta.clear();
            let delay = out.viewport_output.get(&egui::ViewportId::ROOT).map_or(std::time::Duration::MAX, |v| v.repaint_delay);
            animating || delay < std::time::Duration::from_millis(500)
        };
        // Playing into the middle of a word, then paused there.
        let (mut now, t) = (100.0, 5.45);
        for k in 0..90 {
            frame(now, 4.0 + k as f64 / 60.0, true);
            now += 1.0 / 60.0;
        }
        let mut busy = true;
        for _ in 0..240 {
            busy = frame(now, t, false);
            now += 1.0 / 60.0;
        }
        assert!(!busy, "still asking for frames 4 s after pausing");
        // Woken now and then (a timer elsewhere, the pointer): each time, nothing to animate.
        for gap in [0.7, 2.0, 2.0, 5.0, 0.3, 2.0] {
            now += gap;
            assert!(!frame(now, t, false), "a frame {gap} s after the last one started animating again");
        }
    }

    /// The app's fonts in a headless egui, for the pictures below.
    fn shot_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx, "Inter", &[]);
        for _ in 0..2 {
            ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
        }
        ctx
    }

    /// Draw a galley's glyphs from the font atlas into `img`, only between `clip` (y).
    fn blit(img: &mut image::RgbaImage, atlas: &ColorImage, at: Pos2, g: &Galley, rgb: [u8; 3], clip: (f32, f32)) {
        let (w, h) = (img.width() as i64, img.height() as i64);
        for row in &g.rows {
            for gl in &row.row.glyphs {
                let uv = gl.uv_rect;
                if uv.is_nothing() {
                    continue;
                }
                let o = at + row.pos.to_vec2() + gl.pos.to_vec2() + uv.offset;
                for (dy, ty) in (uv.min[1]..uv.max[1]).enumerate() {
                    for (dx, tx) in (uv.min[0]..uv.max[0]).enumerate() {
                        let a = atlas.pixels[ty as usize * atlas.size[0] + tx as usize].a() as f32 / 255.0;
                        let (x, y) = ((o.x + dx as f32).round() as i64, (o.y + dy as f32).round() as i64);
                        if a <= 0.0 || x < 0 || y < 0 || x >= w || y >= h || (y as f32) < clip.0 || (y as f32) >= clip.1 {
                            continue;
                        }
                        let p = img.get_pixel_mut(x as u32, y as u32);
                        for c in 0..3 {
                            p[c] = (p[c] as f32 * (1.0 - a) + rgb[c] as f32 * a) as u8;
                        }
                    }
                }
            }
        }
    }

    /// A PC whose fallback list has no font for the script: the system's fallback finds one.
    /// Needs a Devanagari font installed.
    /// `cargo test -- --ignored system_fonts_cover_scripts_the_list_lacks --nocapture`
    #[test]
    #[ignore]
    fn system_fonts_cover_scripts_the_list_lacks() {
        let font = FontId::proportional(30.0);
        let text = "हिजोमै";
        let without = egui::Context::default();
        without.run_ui(Default::default(), |_| {}).textures_delta.clear();
        assert!(!without.fonts_mut(|f| f.has_glyphs(&font, text)), "egui's own fonts have no Devanagari");
        let with = egui::Context::default();
        with.add_font_provider(Arc::new(egui_system_font_provider::SystemFontProvider::new()));
        with.run_ui(Default::default(), |_| {}).textures_delta.clear();
        let g = with.fonts_mut(|f| f.layout_no_wrap(text.into(), font.clone(), Color32::WHITE));
        assert!(with.fonts_mut(|f| f.has_glyphs(&font, text)), "the system's fallback found no font");
        println!("laid out {} wide", g.size().x);
    }

    /// A Devanagari lyric in each bundled lyric font and weight, as the lines are laid out.
    /// `YTM_SHOT=<file.png> cargo test --release -- --ignored devanagari_png --nocapture`
    #[test]
    #[ignore]
    fn devanagari_png() {
        use crate::preset::Weight;
        let ctx = shot_ctx();
        let lyric = "एक त हिजोमै खेर गइगो, आज नि अबेर भइसक्यो";
        let mut rows = Vec::new();
        for name in ["Inter", "DM Sans", "Google Sans Flex", "Figtree", "Instrument Sans", "Segoe UI Variable"] {
            for weight in [Weight::Regular, Weight::Semibold, Weight::Bold] {
                let font = crate::theme::lyric_font_for(name, weight, 30.0, lyric);
                let g = ctx.fonts_mut(|f| f.layout_no_wrap(lyric.into(), font.clone(), Color32::WHITE));
                let glyphs = g.rows.iter().map(|r| r.row.glyphs.len()).sum::<usize>();
                let has = ctx.fonts_mut(|f| f.has_glyphs(&font, "एकतहिजोमैखेरगइ"));
                println!("{name}: family {:?}, {} wide, {glyphs} glyphs, has_glyphs {has}", font.family, g.size().x);
                rows.push(g);
            }
        }
        let atlas = ctx.fonts(|f| f.image());
        let mut img = image::RgbaImage::from_pixel(1000, 50 * rows.len() as u32 + 20, image::Rgba([24, 22, 30, 255]));
        let h = img.height() as f32;
        for (i, g) in rows.iter().enumerate() {
            blit(&mut img, &atlas, pos2(20.0, 10.0 + 50.0 * i as f32), g, [242, 240, 245], (0.0, h));
        }
        img.save(std::env::var("YTM_SHOT").unwrap_or_else(|_| "devanagari.png".into())).unwrap();
    }

    /// Before/after picture of a lyric line taller than the view, while its 6th row is
    /// sung: the old view stayed at the line's top; the new one pages down with it. The
    /// view positions are the scroll code's (the line's top, then the paging rule).
    /// `YTM_SHOT=<file.png> cargo test --release -- --ignored paging_before_after_png`
    #[test]
    #[ignore]
    fn paging_before_after_png() {
        let ctx = shot_ctx();
        let lyric = "And I keep on running down the same old road that takes me back to you every single night, \
            through the rain and the lights of a city that never learned your name, and I still hear you calling out to me";
        let (width, view_h, font) = (470.0, 230.0, crate::theme::lyric_font_for("Inter", crate::preset::Weight::Bold, 30.0, lyric));
        let lh = 30.0 * 1.333;
        let toks = line_synced_parts(&Line { start: 0, dur: 1000, text: lyric.into(), ..Default::default() }).iter().flat_map(split_part).collect();
        let (words, galleys) = ctx.fonts_mut(|f| {
            let fl = flow(f, toks, &font, width, lh, Align::Left, false, 0.0, 0.0);
            let gs: Vec<Arc<Galley>> = fl.words.iter().map(|w| f.layout_no_wrap(w.text.clone(), font.clone(), Color32::WHITE)).collect();
            (fl.words, gs)
        });
        let labels = ctx.fonts_mut(|f| {
            ["Before (0.2.28): the 6th row is being sung, off the bottom", "After: the view pages down with it"].map(|t| f.layout_no_wrap(t.into(), crate::theme::semibold(16.0), Color32::WHITE))
        });
        let atlas = ctx.fonts(|f| f.image());
        let sung_row = 5.0;
        let y_row = sung_row * lh;
        // Old: the line's top. New: the sung row and the one after it in view.
        let tops = [0.0f32, (y_row + lh * 2.0 - view_h).max(0.0)];
        let (col, pad) = (width + 70.0, 30.0);
        let (iw, ih) = ((col * 2.0) as u32, (view_h + 110.0) as u32);
        let mut img = image::RgbaImage::from_pixel(iw, ih, image::Rgba([24, 22, 30, 255]));
        for (k, top) in tops.iter().enumerate() {
            let x0 = pad + col * k as f32;
            let (v0, v1) = (60.0, 60.0 + view_h);
            blit(&mut img, &atlas, pos2(x0, 20.0), &labels[k], [200, 196, 210], (0.0, ih as f32));
            // The view's frame.
            for x in (x0 - 10.0) as u32..(x0 + width + 10.0) as u32 {
                for y in [v0 as u32 - 1, v1 as u32] {
                    img.put_pixel(x, y, image::Rgba([90, 84, 104, 255]));
                }
            }
            for (w, g) in words.iter().zip(&galleys) {
                let row = (w.pos.y / lh).floor();
                let rgb = if row < sung_row { [242, 240, 245] } else if row == sung_row { [196, 160, 255] } else { [110, 106, 120] };
                blit(&mut img, &atlas, pos2(x0 + w.pos.x, v0 + w.pos.y - top), g, rgb, (v0, v1));
            }
        }
        let out = std::env::var("YTM_SHOT").unwrap_or_else(|_| "paging-before-after.png".into());
        img.save(&out).unwrap();
        println!("wrote {out}");
    }

    /// Before/after picture of right-to-left text with the app's fonts (#29), to a PNG.
    /// `YTM_SHOT=<file.png> cargo test --release -- --ignored rtl_before_after_png`
    #[test]
    #[ignore]
    fn rtl_before_after_png() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx, "Inter", &[]);
        for _ in 0..2 {
            ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
        }
        let titles = ["عمرو دياب - تملي معاك", "עומר אדם - תגידו לה", "Fairuz - نسم علينا الهوى (Live)"];
        let lyric = "يا ليل الصب متى غده أقيام الساعة موعده رقد السمار فأرقه أسف للبين يردده";
        let (col, width) = (560.0, 470.0);
        let title_font = crate::theme::semibold(26.0);
        let lyric_font = crate::theme::lyric_font_for("Inter", crate::preset::Weight::Bold, 30.0, lyric);
        // (position, galley) for everything drawn.
        let mut draw: Vec<(Pos2, Arc<Galley>)> = Vec::new();
        ctx.fonts_mut(|f| {
            for (i, label) in ["Before (0.2.28)", "After"].into_iter().enumerate() {
                draw.push((pos2(30.0 + col * i as f32, 20.0), f.layout_no_wrap(label.into(), crate::theme::bold(20.0), Color32::WHITE)));
            }
            for (k, t) in titles.iter().enumerate() {
                let y = 70.0 + k as f32 * 50.0;
                draw.push((pos2(30.0, y), f.layout_no_wrap(t.to_string(), title_font.clone(), Color32::WHITE)));
                draw.push((pos2(30.0 + col, y), f.layout_job(crate::bidi::job(t, title_font.clone(), Color32::WHITE))));
            }
            // Lyrics: the old view put the words left to right, wrapping at the width; the
            // new one from the right.
            let y = 240.0;
            let (lh, space) = (30.0 * 1.333, text_size(f, " ", &lyric_font).x);
            let mut at = vec2(0.0, 0.0);
            for word in lyric.split(' ') {
                let g = f.layout_no_wrap(word.into(), lyric_font.clone(), Color32::WHITE);
                if at.x > 0.0 && at.x + g.size().x > width {
                    at = vec2(0.0, at.y + lh);
                }
                let w = g.size().x;
                draw.push((pos2(30.0, y) + at, g));
                at.x += w + space;
            }
            let toks = line_synced_parts(&Line { start: 0, dur: 1000, text: lyric.into(), ..Default::default() }).iter().flat_map(split_part).collect();
            let fl = flow(f, toks, &lyric_font, width, 30.0 * 1.333, Align::Right, true, 0.0, 0.0);
            for w in fl.words {
                draw.push((pos2(30.0 + col, y) + w.pos.to_vec2(), f.layout_no_wrap(w.text.clone(), lyric_font.clone(), Color32::WHITE)));
            }
        });
        let atlas = ctx.fonts(|f| f.image());
        let (w, h) = ((col * 2.0) as u32, 470u32);
        let mut img = image::RgbaImage::from_pixel(w, h, image::Rgba([24, 22, 30, 255]));
        // The column divider.
        for y in 0..h {
            img.put_pixel(col as u32 - 15, y, image::Rgba([70, 66, 80, 255]));
        }
        for (at, g) in &draw {
            blit(&mut img, &atlas, *at, g, [242, 240, 245], (0.0, h as f32));
        }
        let out = std::env::var("YTM_SHOT").unwrap_or_else(|_| "rtl-before-after.png".into());
        img.save(&out).unwrap();
        println!("wrote {out}");
    }

    #[test]
    fn line_synced_words_are_staggered() {
        let l = Line { start: 2000, dur: 3000, text: "one two  three".into(), ..Default::default() };
        let p = line_synced_parts(&l);
        let words: Vec<_> = p.iter().filter(|p| !p.text.trim().is_empty()).collect();
        assert_eq!(words[1].start, 2050);
        assert_eq!(words[2].start, 2100);
    }

    #[test]
    fn letter_waves_crest_when_lit() {
        // A 1 s word with 4 letters: the last letter starts later than the first.
        let w = letter_waves(4, 1.0, 0.9);
        assert!(w[0].0 < w[3].0);
        // Zero-duration words float all letters together.
        assert!(letter_waves(3, 0.0, 0.9).iter().all(|w| w.0 == 0.0 && (w.1 - 0.9).abs() < 1e-9));
    }

    #[test]
    fn a_soft_blur_isnt_redrawn_for_a_small_change_of_size() {
        assert_eq!(line_step(26.0, 1.004), line_step(26.0, 1.012));
        assert_ne!(line_step(26.0, 0.94), line_step(26.0, 1.012));
        assert_eq!(line_step(2.0, 1.001), line_step(2.0, 1.004));
        assert_ne!(line_step(2.0, 1.0), line_step(2.0, 1.012));
        // Barely blurred, it stays exact.
        assert_ne!(line_step(0.5, 1.001), line_step(0.5, 1.002));
    }

    #[test]
    fn a_scroll_curve_grows_with_distance() {
        let curve = [620.0, 16.0, 650.0, 58.0, 12.0];
        // The sung line itself: below, with nothing added.
        assert!((curve_duration(0, 10.0, curve) - 0.650).abs() < 1e-6);
        assert!((curve_duration(-3, 10.0, curve) - (0.620 + 4f64.ln() * 0.016)).abs() < 1e-6);
        assert!((curve_duration(3, 10.0, curve) - (0.650 + 4f64.ln() * 0.058 + 0.036)).abs() < 1e-6);
    }

    #[test]
    fn scroll_durations_follow_the_extension() {
        assert!((scroll_duration(0, 10.0) - (0.750 + 2f64.ln() * 0.08 + 0.02)).abs() < 1e-9);
        assert!((scroll_duration(-3, 10.0) - 0.810).abs() < 1e-9);
        assert!((scroll_duration(2, 10.0) - (0.750 + 4f64.ln() * 0.08 + 0.06)).abs() < 1e-9);
    }

    #[test]
    fn gradient_is_soft_and_never_a_box() {
        let look = WordLook { opacity: 1.0, start: 0.5, end: 0.6, glow: 0.0, glow_alpha: 0.5 };
        let word = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 40.0));
        let (f, cuts) = lit_gradient(&word, 0, 1, word, &look, false, false);
        assert_eq!(f(10.0), 1.0);
        assert!(f(55.0) > 0.0 && f(55.0) < 1.0);
        assert_eq!(f(70.0), 0.0);
        assert_eq!(cuts.len(), 2);
    }

    #[test]
    fn note_path_is_closed() {
        let p = note_polygon();
        assert!(p.len() > 20);
        assert!(point_in_polygon(p, pos2(10.0, 17.0)));
        assert!(!point_in_polygon(p, pos2(2.0, 2.0)));
    }
}
