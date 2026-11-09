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
use crate::preset::{BlurScope, Fill, LyricStyle, ThemeCss, Weight};
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
/// `--blyrics-lyric-scroll-timing-function`.
pub const SCROLL_EASE: Bezier = Bezier(0.86, 0.0, 0.2, 1.0);
/// `blyrics-decoration-in`.
const DECO_EASE: Bezier = Bezier(0.25, 1.0, 0.5, 1.0);

// ------------------------------------------------------------------ theme values

/// `--blyrics-richsync-timing-offset` / `--blyrics-timing-offset`.
pub const RICHSYNC_OFFSET: f64 = 0.150;
pub const LINE_OFFSET: f64 = 0.115;
/// `--blyrics-scroll-timing-offset`.
const SCROLL_OFFSET: f64 = 0.5;
/// `blyrics-early-scroll-consider-s`.
const EARLY_SCROLL: f64 = 0.54;
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
const FADE_IN: f64 = 0.33;
const FADE_OUT: f64 = 0.5;
const SCALE_DURATION: f64 = 0.166;
const WOBBLE_DURATION: f64 = 1.0;
const LETTER_WAVE_CREST: f32 = 0.4;
/// `blyrics-long-word-threshold` (ms → s): held words swell their letters.
const LONG_WORD: f64 = 1.5;
/// `blyrics-line-synced-animation-delay`.
const LINE_SYNC_DELAY: f64 = 0.05;
/// Word groups longer than twice `blyrics-long-word-wrap-threshold` may break.
const LONG_GROUP_CHARS: usize = 20;
const USER_SCROLL_RESUME: f64 = 25.0;
const DECO_ENTRY: f64 = 0.35;

/// Resolved look for one frame: the extension's defaults, adjusted by the preset.
#[derive(Clone)]
struct Cfg {
    font: String,
    weight: Weight,
    size: f32,
    active: Color32,
    /// Alpha of dim text on the line being sung.
    inactive_alpha: f32,
    glow: Color32,
    glow_rem: f32,
    scale: f32,
    active_scale: f32,
    padding: f32,
    target_ratio: f32,
    centered: bool,
    fill: Fill,
    wave_crest: f32,
    wave_settle: f32,
    emphasis: f32,
    upcoming_alpha: f32,
    falloff: f32,
    min_alpha: f32,
    past_alpha: Option<f32>,
    blur: f32,
    blur_scope: BlurScope,
    /// One CSS rem in points (YouTube Music's 10 px when the theme sizes in rem).
    rem: f32,
    /// `.blyrics-background-lyric` font size (em).
    bg_em: f32,
    /// `.blyrics-container` inline padding (start, end).
    pad: (f32, f32),
    /// Word wobble on (themes can set its transforms to identity).
    wobble: bool,
    wave_duration: f64,
    /// `--blyrics-glow-color` alpha on the container and on held words.
    glow_alpha: f32,
    long_glow_alpha: f32,
    css: Option<ThemeCss>,
}

/// `GLOW_INVISIBLE_ALPHA`: a glow colour this faint renders nothing and is skipped.
const GLOW_INVISIBLE: f32 = 0.5 / 255.0;

impl Cfg {
    /// `width`: the lyrics panel; `viewport`: the window (the theme's media queries).
    /// The most a line is scaled up: while sung, or under a theme's hover scale.
    fn max_scale(&self) -> f32 {
        self.active_scale.max(self.css.as_ref().and_then(|c| c.hover_scale).unwrap_or(1.0)).max(1.0)
    }

    fn new(s: &LyricStyle, accent: Color32, width: f32, viewport: f32, centered: bool, scale: f32) -> Self {
        let css = s.css.clone();
        // Font size and rem: a theme's own `--blyrics-font-size` steps in YouTube Music's
        // 10 px rem; otherwise 48 pt scaled by the preset with the extension's proportions.
        let (mut size, mut rem) = match css.as_ref().filter(|c| !c.font_rem.is_empty()) {
            Some(c) => {
                let r = c.font_rem.iter().filter(|st| viewport >= st[0]).map(|st| st[1]).next_back().unwrap_or(c.font_rem[0][1]);
                (r * 10.0, 10.0)
            }
            None => {
                let size = 48.0 * s.size.clamp(0.6, 1.6);
                (size, size / 3.0)
            }
        };
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
        let glow = s.glow.clamp(0.0, 1.0);
        let weight = match css.as_ref().and_then(|c| c.font_weight) {
            Some(w) if w >= 700 => Weight::Bold,
            Some(w) if w >= 550 => Weight::Semibold,
            Some(_) => Weight::Regular,
            None => s.weight,
        };
        let wave = css.as_ref().and_then(|c| c.letter_wave).unwrap_or([0.05, 0.02, 0.9, 1.11]);
        let (glow_alpha, long_glow_alpha) = match css.as_ref().and_then(|c| c.glow_alpha) {
            Some(a) => (a, css.as_ref().and_then(|c| c.long_word_glow_alpha).unwrap_or(a)),
            None => (0.5 + 0.4 * glow, 0.5 + 0.4 * glow),
        };
        let pad = css.as_ref().and_then(|c| c.padding_inline).map_or((0.0, 0.0), |p| (p[0] * rem / 10.0, p[1] * rem / 10.0));
        Self {
            font: theme::effective_font(&s.font),
            weight,
            size,
            active: crate::preset::paint(&s.sung_color, accent),
            inactive_alpha: s.active_alpha,
            glow: crate::preset::paint(&s.glow_color, accent),
            glow_rem: if css.is_some() { 0.8 } else { 0.8 * (1.0 + glow) },
            scale: 0.95,
            active_scale: s.active_scale.max(1.0),
            padding: rem * 2.0 * (s.line_gap / 0.6).clamp(0.2, 3.0),
            target_ratio: s.anchor.clamp(0.1, 0.7),
            centered,
            fill: s.fill,
            wave_crest: wave[0],
            wave_settle: wave[1] + if css.is_some() { 0.0 } else { s.lift.clamp(0.0, 8.0) / size },
            emphasis: wave[3],
            upcoming_alpha: s.inactive_alpha,
            falloff: s.falloff,
            min_alpha: s.min_alpha,
            past_alpha: s.past_alpha,
            blur: s.blur,
            blur_scope: s.blur_scope,
            rem,
            bg_em: css.as_ref().and_then(|c| c.background_em).unwrap_or(0.75),
            pad,
            wobble: !css.as_ref().is_some_and(|c| c.no_wobble),
            wave_duration: wave[2] as f64,
            glow_alpha,
            long_glow_alpha,
            css,
        }
    }
    fn rem(&self) -> f32 {
        self.rem
    }
    // Each takes the text it sets, so a line in a script the font lacks is all Inter.
    fn main_font(&self, text: &str) -> FontId {
        theme::lyric_font_for(&self.font, self.weight, self.size, text)
    }
    fn bg_font(&self, text: &str) -> FontId {
        theme::lyric_font_for(&self.font, Weight::Semibold, self.size * self.bg_em, text)
    }
    fn roman_font(&self, text: &str) -> FontId {
        theme::lyric_font_for(&self.font, Weight::Semibold, self.rem() * 2.0 / 1.25, text)
    }
    fn trans_font(&self, text: &str) -> FontId {
        theme::lyric_font_for(&self.font, Weight::Semibold, self.rem() * 2.0, text)
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
    /// Identifies what was shown, so a new decoration can animate in.
    id: u64,
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
fn line_synced_parts(l: &Line) -> Vec<Part> {
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
        let start = l.start + (word as f64 * LINE_SYNC_DELAY * 1000.0) as i32;
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
fn flow(fonts: &mut FontsView<'_>, toks: Vec<Tok>, font: &FontId, max_w: f32, line_h: f32, align: Align, rtl: bool) -> Flow {
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
    let space_w = text_size(fonts, " ", font).x.max(font.size * 0.25);
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
                let sizes: Vec<Vec2> = toks.iter().map(|t| text_size(fonts, &t.text, font)).collect();
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

#[allow(clippy::too_many_arguments)]
fn build_block(fonts: &mut FontsView<'_>, cfg: &Cfg, l: &Line, index: usize, width: f32, roman: Option<Roman>, trans: Option<&str>, agent: Option<&str>) -> Block {
    let em = cfg.size;
    let rtl = crate::bidi::rtl_base(&l.text);
    let align = match agent {
        Some("v2" | "v3") => Align::Right,
        Some("v1000") => Align::Center,
        _ if cfg.centered => Align::Center,
        _ => Align::Left,
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
    let line_h = em * 1.333;
    let mut y = cfg.padding;
    let mut block = Block {
        line: Some(index),
        start: l.start as f64 / 1000.0,
        dur: l.dur.max(0) as f64 / 1000.0,
        instrumental: l.instrumental,
        align,
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
    let parts = if l.parts.is_empty() { line_synced_parts(l) } else { l.parts.clone() };
    let toks: Vec<Tok> = parts.iter().flat_map(split_part).collect();
    let (main, bg): (Vec<Tok>, Vec<Tok>) = toks.into_iter().partition(|t| !t.bg);
    let joined = |toks: &[Tok]| toks.iter().map(|t| t.text.as_str()).collect::<String>();
    let font = cfg.main_font(&joined(&main));
    let f = flow(fonts, main, &font, content_w, line_h, align, rtl);
    for w in f.words {
        block.words.push(Word { pos: w.pos + vec2(0.0, y), ..w });
    }
    block.groups = f.groups;
    y += f.height;
    if bg.iter().any(|t| !t.space) {
        y += em * 0.1;
        let base = block.words.len();
        let g0 = block.groups.len();
        let font = cfg.bg_font(&joined(&bg));
        let f = flow(fonts, bg, &font, content_w, line_h, align, rtl);
        for w in f.words {
            block.words.push(Word { pos: w.pos + vec2(0.0, y), group: w.group + g0, ..w });
        }
        block.groups.extend(f.groups.into_iter().map(|g| Group { first: g.first + base, words: g.words.start + base..g.words.end + base, ..g }));
        y += f.height;
    }
    let gap = em / 6.0; // margin-top: 8px at 48 px
    if let Some(r) = roman {
        y += gap;
        let pad = vec2(cfg.rem() * 0.75, cfg.rem() * 0.25);
        let inner_w = content_w - pad.x * 2.0 - 2.0;
        let (body, inner, id) = match r {
            Roman::Timed(parts, text) => {
                let toks: Vec<Tok> = parts.iter().flat_map(split_part).collect();
                let font = cfg.roman_font(text);
                let lh = fonts.row_height(&font);
                let f = flow(fonts, toks, &font, inner_w, lh, Align::Left, false);
                (DecoBody::Timed { words: f.words, groups: f.groups }, vec2(f.width, f.height), hash(("rt", text)))
            }
            Roman::Text(t) => {
                let font = cfg.roman_font(t);
                let g = fonts.layout(t.to_owned(), font.clone(), Color32::WHITE, inner_w);
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
        block.decos.push(Deco { body, rect, pill: true, id });
        y += size.y;
    }
    if let Some(t) = trans {
        y += gap;
        let font = cfg.trans_font(t);
        let g = fonts.layout(t.to_owned(), font.clone(), Color32::WHITE, content_w);
        let x = match align {
            Align::Left => 0.0,
            Align::Right => content_w - g.size().x,
            Align::Center => (content_w - g.size().x) * 0.5,
        };
        block.decos.push(Deco { body: DecoBody::Text(t.to_owned(), font), rect: Rect::from_min_size(pos2(x, y), g.size()), pill: false, id: hash(("t", t)) });
        y += g.size().y;
    }
    block.height = y + cfg.padding;
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
}

impl Trans {
    fn value(&self, now: f64) -> f32 {
        if !self.init {
            return self.to;
        }
        let p = ((now - self.start - self.delay) / self.dur.max(1e-3)).clamp(0.0, 1.0) as f32;
        lerp(self.from, self.to, EASE.at(p))
    }

    fn set(&mut self, to: f32, now: f64, dur: f64, delay: f64) {
        if !self.init {
            *self = Trans { from: to, to, start: now, dur, delay, init: true };
        } else if (to - self.to).abs() > 1e-4 {
            let from = self.value(now);
            *self = Trans { from, to, start: now, dur, delay, init: true };
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
    /// The font atlas's fill last frame: egui empties the atlas when it fills up or the
    /// fonts change, and nothing laid out or copied from it before that is valid.
    atlas_fill: f32,
    user_until: f64,
    /// Reader scrolling was on last frame: when it ends, scroll back to the current line.
    was_user: bool,
    deco_born: HashMap<u64, f64>,
    glow: Glow,
    credits_focused: bool,
    /// The frame (`cumulative_pass_nr`) the view was last drawn in; 0 once its textures
    /// are freed.
    shown_pass: u64,
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
    pub style: &'a LyricStyle,
    pub accent: Color32,
    pub centered: bool,
    /// The listener's lyrics text size, a multiple of the theme's.
    pub scale: f32,
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
        let cfg = Cfg::new(f.style, f.accent, rect.width(), ctx.content_rect().width(), f.centered, f.scale);
        let layout_key = hash((f.key, rect.width().round() as i32, cfg.size.to_bits(), cfg.padding.to_bits(), cfg.max_scale().to_bits(), cfg.centered, &cfg.font, cfg.weight as u8));
        if self.layout.as_ref().is_none_or(|l| l.key != layout_key) {
            self.relayout(ui, &cfg, &f, layout_key, rect);
        }
        let layout = self.layout.as_ref().unwrap();
        let richsync = layout.richsync;
        // Lines that aren't credits.
        let lines_n = layout.blocks.iter().filter(|b| b.line.is_some()).count();

        // Clock: the extension shows lyrics this far ahead of the media time.
        let t = f.t + if richsync { RICHSYNC_OFFSET } else { LINE_OFFSET };
        let jumped = (t - self.last_t - (f.now - self.last_wall)).abs() > TIME_JUMP || self.last_wall == 0.0;
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
            if st.exit.is_some_and(|e| f.now - e > FADE_OUT.max(SCALE_DURATION)) {
                st.exit = None;
            }
            animating |= st.exit.is_some();
        }

        // ---- scrolling
        let view_h = rect.height();
        let scroll_time = t + SCROLL_OFFSET;
        let user = f.now < self.user_until;
        let mut active: Vec<usize> = Vec::new();
        // Leaving reader scrolling (the button, or the timeout) returns to the current line now.
        let mut new_lyric = jumped || (self.was_user && !user);
        self.was_user = user;
        for i in 0..lines_n {
            let b = &layout.blocks[i];
            let next = layout.blocks.get(i + 1).filter(|n| n.line.is_some()).map_or(f64::INFINITY, |n| n.start);
            if scroll_time >= b.start - EARLY_SCROLL && (scroll_time < next || scroll_time < b.start + b.dur) {
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
        let line_h = cfg.size * 1.333;
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
        let first_shape = ctx.graphics(|g| g.get(painter.layer_id()).map_or(ShapeIdx(0), |l| l.next_idx()));
        let origin_y = rect.top() - self.scroll_top + layout.pad_top;
        let x0 = rect.left() + cfg.pad.0;
        let line_w = rect.width() - cfg.pad.0 - cfg.pad.1;
        let atlas = ctx.fonts(|fv| fv.font_image_size());
        let ppp = ctx.pixels_per_point();
        let mut seek = None;
        let active_line = active.last().copied();
        let first_active = active.first().copied().filter(|_| active_real);
        let mut glow_work: Vec<GlowJob> = Vec::new();
        let pointer = ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p));

        for (i, b) in layout.blocks.iter().enumerate() {
            let mut dy = 0.0;
            for a in &self.scrolls {
                let rel = i as isize - a.active as isize;
                let d = scroll_duration(rel, a.delta);
                let p = ((f.now - a.start) / d).clamp(0.0, 1.0) as f32;
                dy += a.delta * (1.0 - SCROLL_EASE.at(p));
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
                let a = if credits_focused { 0.85 } else { 0.2 };
                let a = theme::anim(&ctx, ("lyr-credits", f.key), a, 0.35);
                let o = transform_origin(block_rect, b.align, content.x);
                let g = ctx.fonts_mut(|fv| fv.layout(text.clone(), font.clone(), Color32::WHITE, line_w - cfg.size * 0.5));
                let xf = Xf { origin: o, scale: cfg.scale, content };
                paint_galley(&painter, &g, pos2(0.0, cfg.padding), &xf, theme::with_alpha(Color32::WHITE, a), atlas);
                continue;
            }
            let li = b.line.unwrap();
            let hovered = pointer.is_some_and(|p| block_rect.contains(p));
            let st = self.states[i];
            let sung_now = st.selected && t >= b.start;

            // Whole-line opacity and blur. A theme that styles lines by state (its CSS
            // `.blyrics--active` rules) gets those values and their transitions; the
            // others get the preset's approximations.
            let (dim, opacity, blur) = if let Some(css) = &cfg.css {
                let on = active.contains(&i) && active_real || st.selected;
                let target = if hovered || on {
                    (1.0, 0.0)
                } else {
                    match first_active {
                        Some(a) if i < a && !user => css.past.map_or((1.0, 0.0), |p| (p[0], p[1])),
                        Some(a) if i > a => css.upcoming.map_or((1.0, 0.0), |p| (p[0], p[1])),
                        _ => (1.0, 0.0),
                    }
                };
                let line_ms = b.dur;
                let (op_t, blur_t) = if hovered {
                    // `:hover { transition: all 0.3s ease }`
                    ((0.3, 0.0), (0.3, 0.0))
                } else {
                    let o = css.opacity_transition.unwrap_or([0.0, 0.0]);
                    let fl = css.filter_transition.unwrap_or([0.0, 0.0]);
                    ((line_ms * o[0] as f64, o[1] as f64), (line_ms * fl[0] as f64, fl[1] as f64))
                };
                let s = &mut self.states[i];
                s.opacity.set(target.0, f.now, op_t.0, op_t.1);
                s.blur.set(target.1, f.now, blur_t.0, blur_t.1);
                animating |= s.opacity.moving(f.now) || s.blur.moving(f.now);
                (cfg.inactive_alpha, s.opacity.value(f.now), s.blur.value(f.now))
            } else {
                let rel = active_line.map_or(1, |a| i as isize - a as isize);
                let past = rel < 0;
                let dist = rel.unsigned_abs() as f32;
                let upcoming = (cfg.upcoming_alpha - dist.min(6.0) * cfg.falloff).max(cfg.min_alpha.min(cfg.upcoming_alpha));
                let reveal = theme::anim_bool(&ctx, "lyr-reveal", user, 0.3);
                let dim = if sung_now || rel == 0 {
                    cfg.inactive_alpha
                } else if past {
                    lerp(cfg.past_alpha.unwrap_or(upcoming), upcoming, reveal)
                } else {
                    upcoming
                };
                let dim = theme::anim(&ctx, ("lyr-dim", f.key, i), dim, 0.3);
                let blur_on = match cfg.blur_scope {
                    BlurScope::All => true,
                    BlurScope::Past => past,
                    BlurScope::Upcoming => !past || reveal > 0.0,
                };
                let target = if cfg.blur > 0.0 && blur_on && !sung_now && rel != 0 { (cfg.blur * (1.0 + 0.35 * (dist - 1.0).clamp(0.0, 3.0))).min(9.0) } else { 0.0 };
                let s = &mut self.states[i];
                s.blur.set(target, f.now, 0.35, 0.0);
                animating |= s.blur.moving(f.now);
                (dim, 1.0, s.blur.value(f.now))
            };

            // Line scale: 0.95 at rest, 1 while sung (enter at its start, exit when it ends);
            // a theme's hover scale on lines not being sung.
            let mut scale = cfg.scale;
            if st.selected && t >= b.start {
                scale = lerp(cfg.scale, cfg.active_scale, EASE.at(((t - b.start) / SCALE_DURATION) as f32));
            } else if let Some(e) = st.exit {
                scale = lerp(cfg.active_scale, cfg.scale, EASE.at(((f.now - e) / SCALE_DURATION) as f32));
            } else if let Some(h) = cfg.css.as_ref().and_then(|c| c.hover_scale) {
                let k = theme::anim_bool(&ctx, ("lyr-hover", f.key, i), hovered, 0.3);
                scale = lerp(cfg.scale, h, EASE.at(k));
            }
            let origin = transform_origin(block_rect, b.align, content.x);
            let xf = Xf { origin, scale, content };

            if b.instrumental {
                paint_instrumental(&painter, &mut self.glow, &ctx, &cfg, b, &xf, t, f.now, st, dim * opacity, line_w - cfg.size * 0.5);
            } else if blur > 0.05 {
                // `filter: blur()` on the whole line: its text, romanization and translation.
                let r = (blur * 4.0).round() / 4.0;
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
                            let g = ctx.fonts_mut(|fv| fv.layout(text.clone(), font.clone(), Color32::WHITE, d.rect.width() + 1.0));
                            let pad = if d.pill { vec2(cfg.rem() * 0.75 + 1.0, cfg.rem() * 0.25 + 1.0) } else { Vec2::ZERO };
                            Some((g, d.rect.min + pad, 0.6))
                        }
                        DecoBody::Timed { .. } => None,
                    })
                    .collect();
                let words: Vec<(&Word, f32)> = b.words.iter().map(|w| (w, if w.bg { dim * 0.78 } else { dim })).chain(deco_words).collect();
                // The romanization pill is part of the line, so it's blurred with it.
                let pills: Vec<(Rect, f32)> = b.decos.iter().filter(|d| d.pill).map(|d| (d.rect, cfg.rem())).collect();
                if let Some((tex, rr, uv)) = self.glow.blurred_line(&ctx, (f.key, i), r, &words, &texts, &pills, scale, atlas, ppp) {
                    // Rendered at the line's scale; snapped so it lands 1:1 on screen pixels.
                    let to = xf.rect(rr);
                    let snapped = (to.min.to_vec2() * ppp).round() / ppp;
                    painter.image(tex, to.translate(snapped - to.min.to_vec2()), uv, theme::with_alpha(Color32::WHITE, opacity));
                }
                animating |= st.exit.is_some();
            } else {
                let lit = LitState::new(&cfg, st, t, f.now, sung_now || st.selected);
                paint_words(&painter, &ctx, &cfg, &b.words, &b.groups, &xf, &lit, dim, opacity, atlas, &mut glow_work, (f.key, i, 0));
                for (k, d) in b.decos.iter().enumerate() {
                    let born = *self.deco_born.entry(d.id).or_insert(f.now);
                    let enter = ((f.now - born) / DECO_ENTRY).clamp(0.0, 1.0) as f32;
                    let e = DECO_EASE.at(enter);
                    if enter < 1.0 {
                        animating = true;
                    }
                    // `blyrics-decoration-in`: fades in from 0.4em above.
                    let font_size = match &d.body {
                        DecoBody::Text(_, f) => f.size,
                        DecoBody::Timed { words, .. } => words.first().map_or(cfg.rem() * 1.6, |w| w.font.size),
                    };
                    let dxf = Xf { content: xf.content + vec2(0.0, -0.4 * font_size * (1.0 - e)), ..xf };
                    // A theme's sung lines vanish (`past_alpha` 0): so does what goes with them.
                    let a = e * opacity * (dim / 0.05).min(1.0);
                    if d.pill {
                        let r = dxf.rect(d.rect);
                        let radius = (cfg.rem() * scale).min(255.0) as u8;
                        painter.rect(r, radius, theme::with_alpha(Color32::WHITE, 0.05 * a), egui::Stroke::new(1.0, theme::with_alpha(Color32::WHITE, 0.1 * a)), egui::StrokeKind::Inside);
                    }
                    match &d.body {
                        DecoBody::Timed { words, groups } => {
                            paint_words(&painter, &ctx, &cfg, words, groups, &dxf, &lit, dim, a, atlas, &mut glow_work, (f.key, i, k as u32 + 1));
                        }
                        DecoBody::Text(text, font) => {
                            let wrap = d.rect.width() + 1.0;
                            let g = ctx.fonts_mut(|fv| fv.layout(text.clone(), font.clone(), Color32::WHITE, wrap));
                            let pad = if d.pill { vec2(cfg.rem() * 0.75 + 1.0, cfg.rem() * 0.25 + 1.0) } else { Vec2::ZERO };
                            // `--blyrics-translated-color`: white at 0.6. Drawn through the
                            // line's transform at the one laid-out size.
                            paint_galley(&painter, &g, d.rect.min + pad, &dxf, theme::with_alpha(Color32::WHITE, 0.6 * a), atlas);
                        }
                    }
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
        // Everything that follows the song's time (the sweep, glows, letter waves, a line's
        // scale-in, the instrumental note) only moves while it plays; paused, the frame is
        // the same until a wall-clock transition (exits, scrolls, fades) or input.
        if f.playing {
            animating = true;
        }
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
            let font = theme::regular((cfg.size * 0.4).max(12.0));
            let h = ui.ctx().fonts_mut(|fv| fv.layout(text.clone(), font.clone(), Color32::WHITE, width - cfg.size * 0.5).size().y);
            blocks.push(Block {
                line: None,
                start: 0.0,
                dur: 0.0,
                instrumental: false,
                align: if cfg.centered { Align::Center } else { Align::Left },
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
        let pad_top = (view_h * cfg.target_ratio - first_h / 2.0).max(0.0);
        let last_centre = blocks.iter().rev().find(|b| b.line.is_some()).map(|b| b.top + b.height / 2.0);
        let last_h = blocks.iter().rev().find(|b| b.line.is_some()).map_or(0.0, |b| b.height);
        let content = y;
        let trailing = blocks.last().filter(|b| b.credits.is_some()).map_or(0.0, |b| b.height) + last_h / 2.0;
        let want = last_centre.map_or(view_h, |c| c + view_h * (1.0 - cfg.target_ratio));
        let pad_bottom = (want - content).max(view_h - content).max(view_h * (1.0 - cfg.target_ratio) - trailing).max(0.0);
        let richsync = lyrics.sync == Sync::Word;
        let same_song = self.layout.as_ref().is_some_and(|l| l.blocks.len() == blocks.len() && self.states.len() == blocks.len() && l.richsync == richsync && same_lines(l, &blocks));
        if same_song {
            // Decorations arrived (or the window resized): lines slide to their new places.
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
fn scroll_duration(rel: isize, delta: f32) -> f64 {
    let up = delta < 0.0;
    let above = (rel < 0 && !up) || (rel > 0 && up);
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
        let clip = |poly: &[Vertex], y: f32, keep_below: bool| -> Vec<Vertex> {
            let inside = |v: &Vertex| if keep_below { v.pos.y >= y } else { v.pos.y <= y };
            let mut out = Vec::with_capacity(poly.len() + 2);
            for i in 0..poly.len() {
                let (a, b) = (&poly[i], &poly[(i + 1) % poly.len()]);
                if inside(a) {
                    out.push(*a);
                }
                if inside(a) != inside(b) {
                    out.push(lerp(a, b, (y - a.pos.y) / (b.pos.y - a.pos.y)));
                }
            }
            out
        };
        let mut out = Mesh::with_texture(m.texture_id);
        for tri in m.indices.as_chunks::<3>().0 {
            let poly = [m.vertices[tri[0] as usize], m.vertices[tri[1] as usize], m.vertices[tri[2] as usize]];
            let lo = poly.iter().map(|v| v.pos.y).fold(f32::INFINITY, f32::min);
            let hi = poly.iter().map(|v| v.pos.y).fold(f32::NEG_INFINITY, f32::max);
            if hi <= r.top() || lo >= r.bottom() {
                continue;
            }
            let mut ys: Vec<f32> = cuts.iter().copied().filter(|&y| y > lo && y < hi).collect();
            ys.insert(0, lo.max(r.top()));
            ys.push(hi.min(r.bottom()));
            for w in ys.windows(2) {
                let piece = clip(&clip(&poly, w[0], true), w[1], false);
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
    fill: Fill,
}

impl LitState {
    fn new(cfg: &Cfg, st: LineState, t: f64, now: f64, live: bool) -> Self {
        Self { live: live && st.exit.is_none(), exit: st.exit.map(|e| ((now - e) as f32, st.exit_t)), t, fill: cfg.fill }
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
        return full(1.0 - EASE.at(since / FADE_OUT as f32));
    }
    if !lit.live {
        return none;
    }
    let e = lit.t - w.start;
    if e < 0.0 {
        return none;
    }
    // `resolveLineGlowSuppression`: a glow colour too faint to see isn't animated at all.
    let glow_rem = if glow_alpha < GLOW_INVISIBLE { 0.0 } else { cfg.glow_rem * cfg.rem() };
    match lit.fill {
        Fill::Line | Fill::Word => {
            // Word: a colour flip with a short transition (`data-word-state`);
            // Line: the line-synced fade.
            let d = if lit.fill == Fill::Word { 0.18 } else { FADE_IN };
            let p = EASE.at((e / d) as f32);
            WordLook { glow: glow_radius(e, GLOW_MIN, glow_rem), ..full(p) }
        }
        Fill::Sweep if w.line_synced => {
            let p = EASE.at((e / FADE_IN) as f32);
            WordLook { glow: glow_radius(e, GLOW_MIN, glow_rem), ..full(p) }
        }
        Fill::Sweep => {
            let d = w.dur.max(0.0);
            let sd = d * SWIPE_DURATION;
            let p = if sd <= 0.0 { 1.0 } else { ((e + d * SWIPE_LEAD) / sd).clamp(0.0, 1.0) as f32 };
            WordLook {
                opacity: 1.0,
                start: lerp(SWIPE_START.0, SWIPE_START.1, p),
                end: lerp(SWIPE_END.0, SWIPE_END.1, p),
                glow: glow_radius(e, (d * GLOW_RATIO).max(GLOW_MIN), glow_rem),
                glow_alpha,
            }
        }
    }
}

/// `drop-shadow` radius: 0.8rem → 0 with `ease`, then gone.
fn glow_radius(e: f64, dur: f64, from: f32) -> f32 {
    if e < 0.0 || e >= dur {
        return 0.0;
    }
    from * (1.0 - EASE.at((e / dur) as f32))
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
        return (0.0, 1.0);
    }
    let p = p.min(1.0) as f32;
    let peak = if long { cfg.emphasis } else { 1.0 };
    if p < LETTER_WAVE_CREST {
        let k = EASE_IN_OUT.at(p / LETTER_WAVE_CREST);
        (cfg.wave_crest * k, lerp(1.0, peak, k))
    } else {
        let k = EASE_OUT.at((p - LETTER_WAVE_CREST) / (1.0 - LETTER_WAVE_CREST));
        (lerp(cfg.wave_crest, cfg.wave_settle, k), lerp(peak, 1.0, k))
    }
}

/// The group's sideways pop: (translateX in em, scaleX).
fn wobble(e: f64) -> (f32, f32) {
    if !(0.0..WOBBLE_DURATION).contains(&e) {
        return (0.0, 1.0);
    }
    let p = EASE.at((e / WOBBLE_DURATION) as f32);
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
    let waves = if w.letters && lit.live && lit.exit.is_none() { Some(letter_waves(n, w.dur.max(0.0), cfg.wave_duration)) } else { None };
    let e = lit.t - w.start;
    let em = w.font.size;
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
            let mut r = Rect::from_min_size(w.pos + row.pos.to_vec2() + gl.pos.to_vec2() + uv.offset, uv.size);
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
            let (tx, sx) = wobble(lit.t - first.start);
            let ws = &words[gr.words.clone()];
            let l = ws.iter().map(|w| w.pos.x).fold(f32::INFINITY, f32::min);
            let r = ws.iter().map(|w| w.pos.x + w.w).fold(f32::NEG_INFINITY, f32::max);
            ((sx - 1.0).abs() > 1e-4 || tx.abs() > 1e-4).then_some(((l + r) * 0.5, tx * first.font.size, sx))
        } else {
            None
        };
        let quads = word_glyphs(ctx, w, lit, cfg, wob, atlas);
        let dim_c = if w.bg { dim * 0.78 } else { dim } * opacity;
        for q in &quads {
            push_quad(&mut base, xf.rect(q.rect), q.uv, theme::with_alpha(Color32::WHITE, dim_c), &|_| 1.0, &no_cut);
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
        let color = theme::with_alpha(cfg.active, look.opacity * opacity);
        let mut screen_quads = Vec::with_capacity(quads.len());
        for q in &quads {
            let r = xf.rect(q.rect);
            let (alpha, cuts) = lit_gradient(&r, q.letter, q.letters, word_box, &look, w.letters, w.rtl);
            push_quad(&mut m, r, q.uv, color, alpha.as_ref(), &cuts);
            screen_quads.push((r, q.uv, q.letter, q.letters, 1.0));
        }
        if look.glow > 0.05 && look.glow_alpha >= GLOW_INVISIBLE && !dev_off("glow") {
            glow.push(GlowJob {
                key: (key.0, key.1, key.2, wi as u32),
                quads: screen_quads,
                word: word_box,
                by_letter: w.letters,
                rtl: w.rtl,
                radius: look.glow * xf.scale,
                pills: Vec::new(),
                color: theme::with_alpha(cfg.glow, look.glow_alpha * look.opacity * opacity),
                look,
                layer: painter.add(Shape::Noop),
            });
        }
        if !m.is_empty() {
            painter.add(Shape::mesh(m));
        }
    }
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
            None => Slot { tex: ctx.load_texture(name, img, TextureOptions::LINEAR), size, used, hash: image_hash },
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
                let Some((img, rect)) = rasterize_glow(&atlas, &j, ppp, &mut self.scratch) else { continue };
                let old = self.textures.remove(&j.key).map(|(s, _)| s);
                let slot = self.upload(ctx, old, img, "lyric-glow");
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
        let now = ctx.input(|i| i.time);
        let step = ((radius * 4.0) as i32, (scale * 1000.0).round() as i32);
        if let Some((s, r, rect, used)) = self.lines.get_mut(&key)
            && *r == step
        {
            *used = now;
            return Some((s.tex.id(), *rect, s.uv()));
        }
        let uv_of = |min: [u16; 2], max: [u16; 2]| {
            Rect::from_min_max(
                pos2(min[0] as f32 / atlas_size[0] as f32, min[1] as f32 / atlas_size[1] as f32),
                pos2(max[0] as f32 / atlas_size[0] as f32, max[1] as f32 / atlas_size[1] as f32),
            )
        };
        let mut quads = Vec::new();
        let push_galley = |g: &Galley, origin: Pos2, weight: f32, quads: &mut Vec<(Rect, Rect, usize, usize, f32)>| {
            for row in &g.rows {
                for gl in &row.row.glyphs {
                    let uv = gl.uv_rect;
                    if uv.is_nothing() {
                        continue;
                    }
                    let r = Rect::from_min_size(origin + row.pos.to_vec2() + gl.pos.to_vec2() + uv.offset, uv.size);
                    quads.push((r, uv_of(uv.min, uv.max), 0, 1, weight));
                }
            }
        };
        for (w, weight) in words {
            let g = ctx.fonts_mut(|fv| fv.layout_no_wrap(w.text.clone(), w.font.clone(), Color32::WHITE));
            push_galley(&g, w.pos, *weight, &mut quads);
        }
        for (g, at, weight) in texts {
            push_galley(g, *at, *weight, &mut quads);
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
fn paint_instrumental(painter: &egui::Painter, glow: &mut Glow, ctx: &egui::Context, cfg: &Cfg, b: &Block, xf: &Xf, t: f64, now: f64, st: LineState, dim: f32, content_w: f32) {
    let em = cfg.size;
    // The icon: one em tall, 4/3 em wide, pulled 1/3 em toward the edge; viewBox 24 centred in it.
    let box_x = match b.align {
        Align::Left => -em / 3.0,
        Align::Right => content_w - em,
        Align::Center => (content_w - em * 4.0 / 3.0) * 0.5,
    };
    let row_h = em * 1.333;
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
    let p = (e / d).clamp(0.0, 1.0) as f32;
    // Clip: translateY(78%) -> translateY(-4%) of the 24-unit box, linear; the wave
    // flattens (scaleY 1.2 -> 0, ease-in) and morphs high <-> low every 1.25 s.
    let travel = lerp(0.78, -0.04, p) * 24.0;
    let flatten = lerp(1.2, 0.0, EASE_IN.at(p));
    let phase = ((e.max(0.0) % 1.25) / 1.25) as f32;
    let m = EASE_IN_OUT.at(if phase < 0.5 { phase * 2.0 } else { (1.0 - phase) * 2.0 });
    let sz = (em * ctx.pixels_per_point()).ceil() as usize + 2;
    let rect = Rect::from_min_max(to_screen(pos2(0.0, 0.0)), to_screen(pos2(24.0, 24.0)));
    let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    let note_tex = glow.note_texture(ctx, sz);
    painter.image(note_tex, rect, full, theme::with_alpha(Color32::WHITE, dim));
    if fill_a > 0.0 {
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
        // `feGaussianBlur stdDeviation=5` (viewBox units) at 0.6 under the fill.
        let sigma = 5.0 * sz as f32 / 24.0;
        let pad = (sigma * 3.0).ceil() as usize;
        let gw = sz + pad * 2;
        let mut g = vec![0.0f32; gw * gw];
        for y in 0..sz {
            for x in 0..sz {
                g[(y + pad) * gw + x + pad] = fill[y * sz + x];
            }
        }
        gaussian(&mut g, gw, gw, sigma);
        let gpad = pad as f32 / sz as f32 * 24.0;
        let grect = Rect::from_min_max(to_screen(pos2(-gpad, -gpad)), to_screen(pos2(24.0 + gpad, 24.0 + gpad)));
        let (gt, guv) = glow.set_tex(ctx, 1, mask_image(&g, gw, 0.6));
        painter.image(gt, grect, guv, theme::with_alpha(cfg.active, fill_a));
        let (ft, fuv) = glow.set_tex(ctx, 2, mask_image(&fill, sz, 1.0));
        painter.image(ft, rect, fuv, theme::with_alpha(cfg.active, fill_a));
    }
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
        let f = flow(&mut fv, line("שלום עולם hello world"), &font, 1000.0, 27.0, Align::Right, true);
        assert!(x(&f, "שלום") > x(&f, "עולם"));
        assert!(x(&f, "עולם") > x(&f, "world"));
        assert!(x(&f, "world") > x(&f, "hello"));
        // An English line with a Hebrew phrase: only the phrase turns around.
        let f = flow(&mut fv, line("my song שלום עולם remix"), &font, 1000.0, 27.0, Align::Left, false);
        assert!(x(&f, "song") < x(&f, "עולם") && x(&f, "עולם") < x(&f, "שלום") && x(&f, "שלום") < x(&f, "remix"));
        // A long line wraps instead of running past the edge.
        let f = flow(&mut fv, line(&"word ".repeat(40)), &font, 300.0, 27.0, Align::Left, false);
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
        let style = LyricStyle::default();
        let mut view = View::default();
        // (frame time, song time, playing) → whether it asked for another frame soon.
        let mut frame = |now: f64, t: f64, playing: bool| {
            let input = egui::RawInput { time: Some(now), screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))), ..Default::default() };
            let mut animating = false;
            let mut out = ctx.run_ui(input, |ui| {
                let n = lyrics.lines.len();
                let f = Frame { lyrics: &lyrics, key: 1, roman: (0..n).map(|_| None).collect(), trans: vec![None; n], t, playing, now, rect: Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 500.0)), style: &style, accent: Color32::RED, centered: false, scale: 1.0 };
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
        crate::theme::install_fonts(&ctx, "Inter");
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
            let fl = flow(f, toks, &font, width, lh, Align::Left, false);
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
        crate::theme::install_fonts(&ctx, "Inter");
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
            let fl = flow(f, toks, &lyric_font, width, 30.0 * 1.333, Align::Right, true);
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
