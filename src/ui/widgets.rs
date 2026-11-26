//! Reusable, hand-painted widgets. Everything custom-painted gets an accessible name
//! (`named`), so screen readers and UI Automation can see and invoke it.

use egui::{Align2, Color32, CornerRadius, Pos2, Rect, Response, Sense, Ui, Vec2, pos2, vec2};

use super::Action;
use crate::model::{Card, Link, Target, Thumb, Track, pick_thumb, sized};
use crate::theme::{self, icon};
use crate::ytm::SongRating;

pub fn named(resp: Response, name: &str) -> Response {
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, name));
    resp
}

/// Single-line text truncated with an ellipsis to `width`. Right-to-left text is put in
/// display order (`bidi`), and cut at its end (the left) when it doesn't fit.
pub fn galley(ui: &Ui, text: &str, font: egui::FontId, color: Color32, width: f32) -> std::sync::Arc<egui::Galley> {
    galley_based(ui, text, font, color, width, None)
}

/// The interface's own text reads its language's way, whatever a sentence starts with.
pub fn ui_base() -> Option<bool> {
    crate::i18n::rtl().then_some(true)
}

/// [`galley`], read right to left or not as `base` says (`None`: by its first letter
/// with a direction; see [`crate::bidi::job_based`]).
pub fn galley_based(ui: &Ui, text: &str, font: egui::FontId, color: Color32, width: f32, base: Option<bool>) -> std::sync::Arc<egui::Galley> {
    let width = width.max(10.0);
    let rtl = base.unwrap_or_else(|| crate::bidi::rtl_base(text));
    let mut job = if !crate::bidi::has_rtl(text) {
        egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color)
    } else if rtl {
        crate::bidi::job_based(&rtl_fit(ui, text, &font, width), font, color, Some(true))
    } else {
        crate::bidi::job_based(text, font, color, Some(false))
    };
    job.wrap = egui::text::TextWrapping::truncate_at_width(width);
    ui.fonts_mut(|f| f.layout_job(job))
}

/// Right-to-left `text`, shortened at its logical end with "…" until it fits `width`
/// (egui would cut the right side, the start of the line).
fn rtl_fit(ui: &Ui, text: &str, font: &egui::FontId, width: f32) -> String {
    let fits = |s: &str| ui.fonts_mut(|f| f.layout_job(crate::bidi::job(s, font.clone(), Color32::WHITE)).size().x) <= width;
    if fits(text) {
        return text.to_owned();
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let cut = |n: usize| format!("{}…", text[..chars.get(n).map_or(text.len(), |c| c.0)].trim_end());
    // The most characters that fit with the ellipsis.
    let (mut lo, mut hi) = (0, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if fits(&cut(mid)) { lo = mid } else { hi = mid - 1 }
    }
    cut(lo)
}

/// A line of text at `pos` whose end, when it's cut short at `width`, fades over `fade` points.
pub fn faded_text_at(ui: &Ui, pos: Pos2, text: &str, font: egui::FontId, color: Color32, width: f32, fade: f32) -> Rect {
    let g = ui.painter().layout_no_wrap(text.to_owned(), font, color);
    let rect = Rect::from_min_size(pos, vec2(g.size().x.min(width), g.size().y));
    if g.size().x <= width || fade <= 0.0 {
        let cut = g.size().x > width;
        ui.painter().with_clip_rect(rect.expand2(vec2(0.0, 2.0))).galley(pos, g, color);
        if cut && color.a() >= 16 {
            tip_full_text(ui, rect, text);
        }
        return rect;
    }
    // The glyphs' own mesh, each vertex's alpha by how near the edge it is.
    let mut mesh = egui::Mesh::with_texture(egui::TextureId::default());
    let [w, h] = ui.ctx().fonts(|f| f.font_image_size());
    for row in &g.rows {
        for gl in &row.row.glyphs {
            let uv = gl.uv_rect;
            if uv.is_nothing() {
                continue;
            }
            let r = Rect::from_min_size(pos + row.pos.to_vec2() + gl.pos.to_vec2() + uv.offset, uv.size);
            let a = |x: f32| ((pos.x + width - x) / fade).clamp(0.0, 1.0);
            let base = mesh.vertices.len() as u32;
            let u = |x: u16| x as f32 / w as f32;
            let v = |y: u16| y as f32 / h as f32;
            let c = |x: f32| theme::with_alpha(color, a(x));
            mesh.vertices.push(egui::epaint::Vertex { pos: r.left_top(), uv: pos2(u(uv.min[0]), v(uv.min[1])), color: c(r.left()) });
            mesh.vertices.push(egui::epaint::Vertex { pos: r.right_top(), uv: pos2(u(uv.max[0]), v(uv.min[1])), color: c(r.right()) });
            mesh.vertices.push(egui::epaint::Vertex { pos: r.right_bottom(), uv: pos2(u(uv.max[0]), v(uv.max[1])), color: c(r.right()) });
            mesh.vertices.push(egui::epaint::Vertex { pos: r.left_bottom(), uv: pos2(u(uv.min[0]), v(uv.max[1])), color: c(r.left()) });
            mesh.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
    ui.painter().with_clip_rect(rect.expand2(vec2(0.0, 2.0))).add(egui::Shape::mesh(mesh));
    if color.a() >= 16 {
        tip_full_text(ui, rect, text);
    }
    rect
}

/// Wrapped text, at most `rows` lines.
pub fn galley_wrapped(
    ui: &Ui,
    text: &str,
    font: egui::FontId,
    color: Color32,
    width: f32,
    rows: usize,
) -> std::sync::Arc<egui::Galley> {
    galley_wrapped_based(ui, text, font, color, width, rows, None)
}

/// [`galley_wrapped`], read right to left or not as `base` says (see [`galley_based`]).
pub fn galley_wrapped_based(
    ui: &Ui,
    text: &str,
    font: egui::FontId,
    color: Color32,
    width: f32,
    rows: usize,
    base: Option<bool>,
) -> std::sync::Arc<egui::Galley> {
    let width = width.max(10.0);
    if !crate::bidi::has_rtl(text) {
        let mut job = egui::text::LayoutJob::simple(text.to_owned(), font, color, width);
        job.wrap.max_rows = rows;
        job.wrap.break_anywhere = false;
        job.wrap.overflow_character = Some('…');
        return ui.fonts_mut(|f| f.layout_job(job));
    }
    // Wrapped in reading order, then each row put in display order on its own (ordering
    // the whole text first would start the first row with the end of the sentence). The
    // rows are worked out here by measuring: a shaped row's glyphs don't count its
    // letters (لا is two letters in one glyph), so cutting the text by them split words.
    let measure = |s: &str| ui.fonts_mut(|f| f.layout_no_wrap(s.to_owned(), font.clone(), Color32::WHITE).size().x);
    let rows_text = wrap_rows(text, width, rows, measure);
    let rtl = base.unwrap_or_else(|| crate::bidi::rtl_base(text));
    ui.fonts_mut(|f| f.layout_job(crate::bidi::job_based(&rows_text.join("\n"), font, color, Some(rtl))))
}

/// `text` in rows no wider than `width` by `measure`, broken between words, at most
/// `rows` of them, the last ending in "…" when there was more. A word wider than a row
/// has one to itself.
fn wrap_rows(text: &str, width: f32, rows: usize, measure: impl Fn(&str) -> f32) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut more = false;
    'lines: for line in text.split('\n') {
        let mut row = String::new();
        for word in line.split(' ').filter(|w| !w.is_empty()) {
            let joined = if row.is_empty() { word.to_owned() } else { format!("{row} {word}") };
            if row.is_empty() || measure(&joined) <= width {
                row = joined;
                continue;
            }
            out.push(std::mem::replace(&mut row, word.to_owned()));
            if out.len() == rows {
                more = true;
                break 'lines;
            }
        }
        out.push(row);
        if out.len() == rows {
            more = text.split('\n').count() > rows;
            break;
        }
    }
    if more && let Some(last) = out.last_mut() {
        let mut words: Vec<&str> = last.split(' ').collect();
        while words.len() > 1 && measure(&format!("{}…", words.join(" "))) > width {
            words.pop();
        }
        *last = format!("{}…", words.join(" "));
    }
    out
}

/// A paragraph of the interface's own text across the width there is, in display
/// order when it has right-to-left letters, and from the right in a right-to-left
/// language. (egui's label would lay its words out left to right.)
pub fn note(ui: &mut Ui, text: &str, font: egui::FontId, color: Color32) -> Response {
    if !crate::bidi::has_rtl(text) {
        return ui.label(egui::RichText::new(text).font(font).color(color));
    }
    let w = ui.available_width();
    let g = galley_wrapped_based(ui, text, font, color, w, usize::MAX, ui_base());
    let (rect, resp) = ui.allocate_exact_size(vec2(w, g.size().y), Sense::hover());
    let x = if crate::i18n::rtl() { rect.right() - g.size().x } else { rect.left() };
    ui.painter().galley(pos2(x, rect.top()), g, color);
    resp
}

/// The interface's own short text as a label's or a hint's, in display order (see
/// [`note`]).
pub fn ui_text(text: &str, font: egui::FontId, color: Color32) -> egui::WidgetText {
    if !crate::bidi::has_rtl(text) {
        return egui::RichText::new(text).font(font).color(color).into();
    }
    crate::bidi::job_based(text, font, color, ui_base()).into()
}

/// Whether `g` was cut short with an ellipsis compared to `text` (covering both egui's
/// LTR wrapping and our RTL truncation helpers).
pub fn is_elided(g: &egui::Galley, text: &str) -> bool {
    if g.elided {
        return true;
    }
    let Some(last) = g.text().lines().last() else { return false };
    let orig_last = text.lines().last().unwrap_or(text);
    (last.starts_with('…') && !orig_last.starts_with('…')) || (last.ends_with('…') && !orig_last.ends_with('…'))
}

/// Show `text` in a hover tooltip when `g` was truncated to fit `rect`.
/// Uses `Sense::hover()` so parent rows, cards, and buttons keep their click and hover
/// states, and works on disabled widgets too (such as disabled settings rows).
pub fn tip_if_elided(ui: &Ui, rect: Rect, g: &egui::Galley, text: &str) {
    if is_elided(g, text) {
        tip_full_text(ui, rect, text);
    }
}

/// Paints `g` at `pos`, with `text` in a hover tooltip when `g` was cut short.
pub fn galley_tipped(ui: &Ui, pos: Pos2, g: std::sync::Arc<egui::Galley>, color: Color32, text: &str) {
    ui.painter().galley(pos, g.clone(), color);
    tip_if_elided(ui, Rect::from_min_size(pos, g.size()), &g, text);
}

fn tip_full_text(ui: &Ui, rect: Rect, text: &str) {
    if text.is_empty() || ui.is_sizing_pass() {
        return;
    }
    let id = ui.scope_id().with(("elided-tip", text, rect.left() as i32, rect.top() as i32));
    let resp = ui.interact(rect, id, Sense::hover());
    if resp.enabled() {
        resp.on_hover_text(text);
    } else {
        resp.on_disabled_hover_text(text);
    }
}

/// Paints single-line truncated text and returns its bounds and whether it was elided,
/// without registering a hover widget (for callers that place a clickable widget over it).
pub fn text_at_elided(ui: &Ui, pos: Pos2, text: &str, font: egui::FontId, color: Color32, width: f32) -> (Rect, bool) {
    let g = galley(ui, text, font, color, width);
    let rect = Rect::from_min_size(pos, g.size());
    let elided = is_elided(&g, text);
    ui.painter().galley(pos, g, color);
    (rect, elided)
}

pub fn text_at(ui: &Ui, pos: Pos2, text: &str, font: egui::FontId, color: Color32, width: f32) -> Rect {
    let (rect, elided) = text_at_elided(ui, pos, text, font, color, width);
    if elided && color.a() >= 16 {
        tip_full_text(ui, rect, text);
    }
    rect
}

/// [`title_at`], returning whether the title was elided without registering a hover widget
/// (for callers that place a clickable widget over the returned rect).
pub fn title_at_elided(ui: &Ui, pos: Pos2, title: &str, explicit: bool, font: egui::FontId, color: Color32, width: f32) -> (Rect, bool) {
    if !explicit {
        return text_at_elided(ui, pos, title, font, color, width);
    }
    let ppp = ui.ctx().pixels_per_point();
    let side = (font.size * 0.8 * ppp).round().max(9.0); // in physical pixels
    let gap = (side * 0.4).round() / ppp;
    let (r, elided) = text_at_elided(ui, pos, title, font, color, width - side / ppp - gap);
    let snap = |v: f32| (v * ppp).round() / ppp;
    let b = Rect::from_min_size(pos2(snap(r.right() + gap), snap(r.center().y - side / ppp / 2.0)), Vec2::splat(side / ppp));
    // Drawn from a mask made once per size: one texel a screen pixel, so it stays sharp.
    // Only the current scale's masks are kept; a new scale drops the old ones.
    type Masks = (f32, std::collections::HashMap<usize, egui::TextureHandle>);
    let side = side as usize;
    let id = egui::Id::unique("explicit-badge");
    let cached = ui.ctx().data_mut(|d| {
        let (scale, masks) = d.get_temp_mut_or_default::<Masks>(id);
        if *scale != ppp {
            *scale = ppp;
            masks.clear();
        }
        masks.get(&side).cloned()
    });
    let tex = cached.unwrap_or_else(|| {
        let t = ui.ctx().load_texture("explicit-badge", badge_mask(side), egui::TextureOptions::LINEAR);
        ui.ctx().data_mut(|d| d.get_temp_mut_or_default::<Masks>(id).1.insert(side, t.clone()));
        t
    });
    ui.painter().image(tex.id(), b, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), color);
    (r.union(b), elided)
}

/// A song's title, followed by the "E" YouTube Music marks explicit songs with: a square
/// in the title's own `color` with the E cut out of it, so whatever's behind shows through.
pub fn title_at(ui: &Ui, pos: Pos2, title: &str, explicit: bool, font: egui::FontId, color: Color32, width: f32) -> Rect {
    let (rect, elided) = title_at_elided(ui, pos, title, explicit, font, color, width);
    if elided && color.a() >= 16 {
        tip_full_text(ui, rect, title);
    }
    rect
}

/// A rounded square with an E cut out of it, `side` pixels across, as white coverage
/// (8×8 samples a pixel) for tinting: smooth edges, and the E shows what's behind.
fn badge_mask(side: usize) -> egui::ColorImage {
    let s = side as f32;
    let radius = s * 0.24;
    // The E, in fractions of the side.
    let (x0, x1, y0, y1, t, arm) = (0.34, 0.68, 0.22, 0.78, 0.12, 0.63);
    let in_e = |x: f32, y: f32| {
        (x0..x1).contains(&x)
            && (y0..y1).contains(&y)
            && (x < x0 + t || y < y0 + t || y > y1 - t || ((y - 0.5).abs() < t / 2.0 && x < arm))
    };
    let in_square = |x: f32, y: f32| {
        let dx = (radius - x).max(x - (s - radius)).max(0.0);
        let dy = (radius - y).max(y - (s - radius)).max(0.0);
        dx * dx + dy * dy <= radius * radius
    };
    const N: usize = 8;
    let pixels = (0..side * side)
        .map(|i| {
            let (px, py) = ((i % side) as f32, (i / side) as f32);
            let hits = (0..N * N)
                .filter(|j| {
                    let x = px + ((j % N) as f32 + 0.5) / N as f32;
                    let y = py + ((j / N) as f32 + 0.5) / N as f32;
                    in_square(x, y) && !in_e(x / s, y / s)
                })
                .count();
            Color32::from_white_alpha((hits * 255 / (N * N)) as u8)
        })
        .collect();
    egui::ColorImage { size: [side, side], source_size: vec2(s, s), pixels }
}

/// What a screen reader calls a song: its title, and whether it's explicit.
pub fn song_name(title: &str, explicit: bool) -> std::borrow::Cow<'_, str> {
    if explicit { crate::tr!("player-song-explicit", title = title).into() } else { title.into() }
}

/// Single-line text whose artist names are links: underlined on hover, and a click
/// opens the artist's page. Names without a channel id stay plain text.
#[allow(clippy::too_many_arguments)]
pub fn linked_text(
    ui: &Ui,
    pos: Pos2,
    text: &str,
    links: &[Link],
    font: egui::FontId,
    color: Color32,
    width: f32,
    acts: &mut Vec<Action>,
) -> Rect {
    let g = galley(ui, text, font, color, width);
    let rect = Rect::from_min_size(pos, g.size());
    // Names are found in the text as shown: right-to-left text is reordered, its names
    // with it, and they no longer come in reading order.
    let shown = g.job.text.clone();
    let rtl = crate::bidi::has_rtl(text);
    let mut search_from = 0;
    for (i, l) in links.iter().enumerate() {
        let Some(id) = &l.id else { continue };
        let name = crate::bidi::visual(&l.name);
        let from = if rtl { 0 } else { search_from };
        let Some(off) = shown[from..].find(name.as_ref()) else { continue };
        let start = from + off;
        let end = start + name.len();
        search_from = end;
        let c0 = shown[..start].chars().count();
        let c1 = shown[..end].chars().count();
        let x0 = g.pos_from_cursor(egui::text::CCursor::new(c0)).min.x;
        let x1 = g.pos_from_cursor(egui::text::CCursor::new(c1)).min.x;
        if x1 - x0 < 2.0 {
            continue; // truncated away
        }
        let r = Rect::from_min_max(pos2(pos.x + x0, rect.top()), pos2(pos.x + x1, rect.bottom()));
        let resp = ui.interact(r, ui.scope_id().with(("artist-link", text, i, pos.x as i32, pos.y as i32)), Sense::click());
        let resp = named(resp, &l.name).on_hover_cursor(egui::CursorIcon::PointingHand);
        if resp.hovered() {
            ui.painter().hline(r.x_range(), r.bottom() - 1.0, egui::Stroke::new(1.0, color));
        }
        if resp.clicked() {
            acts.push(Action::Open(Target::Artist(id.clone())));
        }
    }
    ui.painter().galley(pos, g.clone(), color);
    if color.a() >= 16 {
        tip_if_elided(ui, rect, &g, text);
    }
    rect
}

pub fn icon_at(ui: &Ui, center: Pos2, glyph: char, size: f32, color: Color32) {
    // A theme's own icon for it. Most have none.
    if !crate::preset::active().app.icons.is_empty() && super::vector::name_of(glyph).is_some_and(|name| super::vector::paint(ui, name, center, size * 1.1, color)) {
        return;
    }
    // Centred on the drawn shape: egui places these glyphs flush left in their box
    // (dropping the font's side bearing) and a little high, so box-centring is off.
    let g = ui.painter().layout_no_wrap(glyph.to_string(), theme::icon_font(size), color);
    let ink = g.mesh_bounds;
    let pos = if ink.is_positive() { center - ink.center().to_vec2() } else { center - g.size() / 2.0 };
    // Play is a triangle: centred on its box, its weight sits left. Centre its centroid.
    let pos = if glyph == theme::icon::PLAY { pos + vec2(0.083 * size, 0.0) } else { pos };
    ui.painter().galley(pos, g, color);
}

/// Heart drawn from its parametric curve: an outline, filled by `fill` (0..1). The
/// icon font only has the outline, and a liked song needs to read at a glance.
pub fn heart(ui: &Ui, center: Pos2, size: f32, fill: f32, outline: Color32, fill_color: Color32) {
    use std::f32::consts::TAU;
    // x = 16 sin^3 t, y = 13 cos t - 5 cos 2t - 2 cos 3t - cos 4t spans x ±16, y -17..12;
    // shift so the box is centred, flip y for screen space.
    static UNIT: std::sync::OnceLock<Vec<Vec2>> = std::sync::OnceLock::new();
    let unit = UNIT.get_or_init(|| {
        (0..64)
            .map(|i| {
                let t = i as f32 / 64.0 * TAU;
                let x = 16.0 * t.sin().powi(3);
                let y = 13.0 * t.cos() - 5.0 * (2.0 * t).cos() - 2.0 * (3.0 * t).cos() - (4.0 * t).cos();
                vec2(x, -y - 2.5)
            })
            .collect()
    });
    let s = size / 34.0;
    let pts: Vec<Pos2> = unit.iter().map(|v| center + *v * s).collect();
    if fill > 0.0 {
        // The heart is star-shaped around a point just below its middle, so a fan fills it.
        let mut mesh = egui::Mesh::default();
        let c = theme::with_alpha(fill_color, fill * fill_color.a() as f32 / 255.0);
        mesh.colored_vertex(center + vec2(0.0, 1.0 * s), c);
        for p in &pts {
            mesh.colored_vertex(*p, c);
        }
        let n = pts.len() as u32;
        for i in 0..n {
            mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
        }
        ui.painter().add(egui::Shape::mesh(mesh));
    }
    // Separate segments with round joins: a closed polyline mitres the tip and the
    // notch into long spikes.
    let w = 1.7;
    for (i, p) in pts.iter().enumerate() {
        let q = pts[(i + 1) % pts.len()];
        ui.painter().line_segment([*p, q], egui::Stroke::new(w, outline));
        ui.painter().circle_filled(*p, w / 2.0, outline);
    }
}

/// What shows something's on its way: a spinner, or in some themes a shape morphing.
pub fn loader(ui: &mut Ui) {
    if !crate::preset::active().app.morph_loader {
        ui.spinner();
        return;
    }
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(ui.spacing().interact_size.y), Sense::hover());
    let t = ui.input(|i| i.time) as f32;
    let c = rect.center();
    let r = rect.width() * 0.46;
    // Lobes coming and going as it turns: 3, 5, 8, ...
    const SHAPES: [f32; 4] = [3.0, 5.0, 8.0, 12.0];
    let phase = t * 0.8;
    let (a, b) = (SHAPES[phase as usize % 4], SHAPES[(phase as usize + 1) % 4]);
    let k = theme::ease_in_out(phase.fract());
    let color = ui.ctx().data(|d| d.get_temp::<Color32>(egui::Id::unique("accent"))).unwrap_or(theme::RED);
    let pts: Vec<Pos2> = (0..64).map(|i| {
        let ang = i as f32 / 64.0 * std::f32::consts::TAU;
        let wave = |n: f32| (ang * n + t).cos() * 0.14;
        let rr = r * (0.82 + wave(a) * (1.0 - k) + wave(b) * k);
        c + vec2((ang + t * 0.5).cos(), (ang + t * 0.5).sin()) * rr
    }).collect();
    ui.painter().add(egui::Shape::convex_polygon(pts, color, egui::Stroke::NONE));
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
}

/// The theme's ring on what has keyboard focus.
pub fn focus_ring(ui: &Ui, resp: &Response, rect: Rect, radius: f32) {
    let Some(f) = crate::preset::active().app.controls.focus.as_ref() else { return };
    if !resp.has_focus() {
        return;
    }
    let accent = ui.ctx().data(|d| d.get_temp::<Color32>(egui::Id::unique("accent"))).unwrap_or(theme::RED);
    let c = crate::preset::paint(&f.color, accent);
    let outer = rect.expand(f.px + f.inner_px);
    ui.painter().rect_stroke(outer, radius + f.px + f.inner_px, egui::Stroke::new(f.px, c), egui::StrokeKind::Inside);
    if f.inner_px > 0.0 {
        ui.painter().rect_stroke(rect.expand(f.inner_px), radius + f.inner_px, egui::Stroke::new(f.inner_px, Color32::BLACK), egui::StrokeKind::Inside);
    }
}

/// A rounded rect in the theme's corners: a squircle where it has them.
pub fn fill_rounded(ui: &Ui, rect: Rect, radius: f32, fill: Color32) {
    if crate::preset::active().app.squircle && radius >= 4.0 {
        ui.painter().add(egui::Shape::convex_polygon(squircle(rect, radius), fill, egui::Stroke::NONE));
    } else {
        ui.painter().rect_filled(rect, radius, fill);
    }
}

/// The outline of `rect` with `corner-shape: superellipse(2)` corners of `r`.
pub fn squircle(rect: Rect, r: f32) -> Vec<Pos2> {
    const STEPS: usize = 10;
    let r = r.min(rect.width() / 2.0).min(rect.height() / 2.0);
    let mut pts = Vec::with_capacity((STEPS + 1) * 4);
    let corners = [(rect.right() - r, rect.top() + r, 1.0, -1.0, false), (rect.right() - r, rect.bottom() - r, 1.0, 1.0, true), (rect.left() + r, rect.bottom() - r, -1.0, 1.0, false), (rect.left() + r, rect.top() + r, -1.0, -1.0, true)];
    for (cx, cy, sx, sy, flip) in corners {
        for i in 0..=STEPS {
            let a = i as f32 / STEPS as f32 * std::f32::consts::FRAC_PI_2;
            let a = if flip { std::f32::consts::FRAC_PI_2 - a } else { a };
            // |x|^4 + |y|^4 = 1: going around, x from 0 to 1 at the top right.
            // `cos` of a quarter turn comes out a hair below zero, and its root isn't a number.
            let (x, y) = (a.sin().max(0.0).sqrt(), a.cos().max(0.0).sqrt());
            pts.push(pos2(cx + sx * x * r, cy + sy * y * r));
        }
    }
    pts
}

/// Circular icon button with an animated hover halo.
pub fn icon_button(ui: &mut Ui, glyph: char, size: f32, name: &str, active: bool) -> Response {
    icon_button_turning(ui, glyph, size, name, active, None)
}

/// `icon_button` that grows and turns on hover: `[scale, degrees]`.
pub fn icon_button_turning(ui: &mut Ui, glyph: char, size: f32, name: &str, active: bool, hover: Option<[f32; 2]>) -> Response {
    let d = size + 16.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(d), Sense::click());
    let resp = named(resp, name).on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
    let c = &crate::preset::active().app.controls;
    let turned = hover.map_or(0.0, |_| theme::anim_bool(ui.ctx(), resp.id.with("turn"), resp.hovered(), 0.3));
    let [grow, degrees] = hover.map_or([1.0, 0.0], |[s, a]| [1.0 + (s - 1.0) * turned, a * turned]);
    if h > 0.0 {
        ui.painter().circle_filled(rect.center(), d / 2.0 * grow, theme::glass(0.10 * h));
        if c.ghost_outline.max(c.hover_border) > 0.0 {
            ui.painter().circle_stroke(rect.center(), d / 2.0 * grow, egui::Stroke::new(1.0, theme::glass(c.ghost_outline.max(c.hover_border) * h)));
        }
    }
    if c.press_fill > 0.0 && resp.is_pointer_button_down_on() {
        ui.painter().circle_filled(rect.center(), d / 2.0, theme::glass(c.press_fill));
    }
    focus_ring(ui, &resp, rect, d / 2.0);
    let color = if active { theme::text() } else { theme::lerp_color(theme::with_alpha(theme::text_dim(), c.idle_alpha.clamp(0.5, 1.0)), theme::text(), h) };
    let press = if resp.is_pointer_button_down_on() { c.press_scale } else { 1.0 + (c.hover_scale - 1.0) * h };
    if degrees != 0.0 {
        icon_turned(ui, rect.center(), glyph, size * press * grow, color, degrees);
    } else {
        icon_at(ui, rect.center(), glyph, size * press * grow, color);
    }
    resp
}

/// A font icon turned about its middle.
pub fn icon_turned(ui: &Ui, center: Pos2, glyph: char, size: f32, color: Color32, degrees: f32) {
    let g = ui.painter().layout_no_wrap(glyph.to_string(), theme::icon_font(size), color);
    let ink = if g.mesh_bounds.is_positive() { g.mesh_bounds.center().to_vec2() } else { g.size() / 2.0 };
    let angle = degrees.to_radians();
    // Text turns about the corner it's placed by.
    let pos = center - egui::emath::Rot2::from_angle(angle) * ink;
    ui.painter().add(egui::epaint::TextShape::new(pos, g, color).with_angle(angle));
}

/// Dark or light text, whichever reads on `bg`.
pub fn contrast(bg: Color32) -> Color32 {
    let l = 0.299 * bg.r() as f32 + 0.587 * bg.g() as f32 + 0.114 * bg.b() as f32;
    if l > 150.0 { Color32::from_gray(20) } else { Color32::WHITE }
}

/// Rounded pill button. `primary` = white with dark text (YouTube Music's Play button).
pub fn pill(ui: &mut Ui, label: &str, glyph: Option<char>, primary: bool) -> Response {
    pill_button(ui, label, glyph, primary, true, None).0
}

pub(super) fn unlike_pill(ui: &mut Ui, label: &str, wide: bool, target: egui::Id) -> bool {
    pill_button(ui, label, Some(icon::HEART), false, wide, Some(target)).1
}

fn pill_button(ui: &mut Ui, label: &str, glyph: Option<char>, primary: bool, labelled: bool, unlike: Option<egui::Id>) -> (Response, bool) {
    // Laid out once; the colour is given when it's painted.
    let text = galley(ui, label, theme::semibold(14.0), Color32::PLACEHOLDER, f32::INFINITY);
    let icon_w = if glyph.is_some() { 24.0 } else { 0.0 };
    let width = if labelled { text.size().x + icon_w + 36.0 } else { 38.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 38.0), Sense::click());
    let resp = named(resp, label).on_hover_cursor(egui::CursorIcon::PointingHand);
    let gesture = unlike.map(|target| super::ratings::like_gesture(ui, &resp, target, true, None));
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
    let c = &crate::preset::active().app.controls;
    let accent = ui.ctx().data(|d| d.get_temp::<Color32>(egui::Id::unique("accent"))).unwrap_or(theme::RED);
    let (fill, fg) = if primary && c.accent_primary {
        (theme::lerp_color(accent, Color32::WHITE, 0.12 * h), theme::on_fill(accent))
    } else if primary {
        if theme::light() { (theme::lerp_color(theme::text(), Color32::BLACK, h), Color32::WHITE) } else { (theme::lerp_color(Color32::from_gray(240), Color32::WHITE, h), Color32::from_gray(12)) }
    } else {
        (theme::glass(c.button_fill + c.button_hover * h), theme::text())
    };
    let r = rect.shrink(if resp.is_pointer_button_down_on() { (1.0 - c.press_scale) * 10.0 } else { 0.0 });
    if let Some([y, blur, a]) = c.shadow {
        soft_shadow(ui, r.translate(vec2(0.0, y)), 19.0, blur, a);
    }
    ui.painter().rect_filled(r, 19.0, fill);
    if c.press_fill > 0.0 && !primary && resp.is_pointer_button_down_on() {
        ui.painter().rect_filled(r, 19.0, theme::glass(c.press_fill));
    }
    let edge = c.ghost_outline + c.hover_border * h;
    if edge > 0.0 && !primary {
        ui.painter().rect_stroke(r, 19.0, egui::Stroke::new(1.0, theme::glass(edge)), egui::StrokeKind::Inside);
    }
    focus_ring(ui, &resp, r, 19.0);
    let mut x = r.left() + 18.0;
    if let Some(gesture) = &gesture {
        let center = if labelled { pos2(x + 8.0, r.center().y) } else { r.center() };
        super::ratings::unlike_heart(ui, center, 17.0, 1.0, fg, fg, gesture.progress);
        if gesture.hint {
            super::ratings::unlike_hint(ui, &resp);
        }
        x += icon_w;
    } else if let Some(g) = glyph {
        icon_at(ui, pos2(x + 8.0, r.center().y), g, 17.0, fg);
        x += icon_w;
    }
    if labelled {
        ui.painter().galley(pos2(x, r.center().y - text.size().y / 2.0), text, fg);
    }
    let activated = gesture.map_or_else(|| resp.clicked(), |gesture| gesture.activate);
    (resp, activated)
}

/// A round button the height of a [`pill`], for a menu of what doesn't need a button of its own.
pub fn more_pill(ui: &mut Ui, name: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(38.0), Sense::click());
    let resp = named(resp, name).on_hover_cursor(egui::CursorIcon::PointingHand);
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&resp));
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() || open, 0.15);
    ui.painter().circle_filled(rect.center(), 19.0, theme::glass(0.10 + 0.08 * h));
    icon_at(ui, rect.center(), icon::ELLIPSIS_VERTICAL, 18.0, theme::text());
    resp
}

/// Selectable chip (home moods, library tabs).
pub fn chip(ui: &mut Ui, label: &str, selected: bool) -> Response {
    let chips = &crate::preset::active().app.chip_look;
    let weight = if selected { chips.selected_weight.unwrap_or(chips.weight) } else { chips.weight };
    let text = ui.painter().layout_no_wrap(label.to_owned(), theme::weighted(weight, 13.5), Color32::PLACEHOLDER);
    let (rect, resp) = ui.allocate_exact_size(vec2(text.size().x + 28.0, chips.height), Sense::click());
    let resp = named(resp, label).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
    let s = theme::anim_bool(ui.ctx(), resp.id.with("s"), selected, 0.2);
    use crate::preset::ChipStyle;
    let look = crate::preset::active();
    let style = look.app.chip;
    let radius = look.app.chip_radius.unwrap_or((look.chrome.radius * 0.55).clamp(4.0, 17.0));
    let accent = ui.ctx().data(|d| d.get_temp::<Color32>(egui::Id::unique("accent"))).unwrap_or(theme::RED);
    let (on_fill, on_fg) = match style {
        ChipStyle::Accent => (accent, theme::on_fill(accent)),
        ChipStyle::Tonal => (theme::glass(0.2), accent),
        ChipStyle::Ghost => (theme::glass(0.2), theme::text()),
        _ => (theme::text(), theme::on_text()),
    };
    // A theme's own selected colours are for dark pages, and go as a pair.
    let on_fill = if chips.selected_fill.is_empty() || theme::light() { on_fill } else { theme::with_alpha(crate::preset::paint(&chips.selected_fill, accent), chips.selected_alpha) };
    let on_fg = if chips.selected_text.is_empty() || theme::light() { on_fg } else { crate::preset::paint(&chips.selected_text, accent) };
    let (rest, more) = match style {
        ChipStyle::Outline => (0.0, 0.04),
        ChipStyle::Ghost => (0.1, 0.05),
        _ => (0.09, 0.06),
    };
    let off_fill = theme::glass(chips.fill.unwrap_or(rest) + chips.hover.unwrap_or(more) * h);
    let fill = theme::lerp_color(off_fill, on_fill, s);
    let press = crate::preset::active().app.controls.press_chips;
    let rect = if press < 1.0 && resp.is_pointer_button_down_on() { Rect::from_center_size(rect.center(), rect.size() * press) } else { rect };
    let rect = rect.translate(vec2(0.0, -chips.hover_lift * h));
    // In a segmented row the inner corners are small, the row's ends round by the gap's side.
    let radius = chips.segmented.map_or(radius, |[_, inner, _]| inner);
    ui.painter().rect_filled(rect, radius, fill);
    if chips.border > 0.0 {
        ui.painter().rect_stroke(rect, radius, egui::Stroke::new(1.0, theme::glass(chips.border)), egui::StrokeKind::Inside);
    }
    focus_ring(ui, &resp, rect, radius);
    match style {
        ChipStyle::Outline => {
            ui.painter().rect_stroke(rect, radius, egui::Stroke::new(1.0, theme::glass(0.3 * (1.0 - s))), egui::StrokeKind::Inside);
        }
        ChipStyle::Ghost => {
            ui.painter().rect_stroke(rect, radius, egui::Stroke::new(1.0, theme::glass(0.2)), egui::StrokeKind::Inside);
        }
        _ => {}
    }
    let off_fg = if chips.text_color.is_empty() || theme::light() { theme::text() } else { crate::preset::paint(&chips.text_color, accent) };
    let fg = theme::lerp_color(off_fg, on_fg, s);
    ui.painter().galley(rect.center() - text.size() / 2.0, text, fg);
    resp
}

/// A selectable chip in the app's own look, whatever the theme. Now playing's tabs, always
/// drawn dark.
pub fn default_chip(ui: &mut Ui, label: &str, selected: bool) -> Response {
    let look = crate::preset::get(crate::preset::DEFAULT_ID);
    let text = ui.painter().layout_no_wrap(label.to_owned(), theme::semibold(13.5), Color32::PLACEHOLDER);
    let (rect, resp) = ui.allocate_exact_size(vec2(text.size().x + 28.0, 34.0), Sense::click());
    let resp = named(resp, label).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
    let s = theme::anim_bool(ui.ctx(), resp.id.with("s"), selected, 0.2);
    let radius = (look.chrome.radius * 0.55).clamp(4.0, 17.0);
    let off_fill = Color32::from_white_alpha(((0.09 + 0.06 * h) * 255.0) as u8);
    ui.painter().rect_filled(rect, radius, theme::lerp_color(off_fill, theme::TEXT, s));
    focus_ring(ui, &resp, rect, radius);
    let fg = theme::lerp_color(theme::TEXT, Color32::from_gray(15), s);
    ui.painter().galley(rect.center() - text.size() / 2.0, text, fg);
    resp
}

/// The size to fetch an image at for `px` pixels: rounded up to a few fixed steps, so
/// resizing the window doesn't fetch a new size every frame and small covers (rows,
/// sidebar, player bar) share one download.
fn fetch_px(px: u32) -> u32 {
    const STEPS: [u32; 9] = [60, 120, 180, 240, 360, 544, 720, 960, 1200];
    STEPS.into_iter().find(|&s| s >= px).unwrap_or(1200)
}

/// The cover URL to paint at `px`: that size once it has loaded, else the size shown
/// last for the same cover. A cover that changes size (going full screen, resizing the
/// window) keeps showing instead of blanking until the new size arrives, or for good
/// if that download fails.
fn steady_url(ctx: &egui::Context, base: &str, px: u32) -> String {
    let want = sized(base, px);
    let id = egui::Id::unique(("cover-shown", base));
    let ready = matches!(egui::Image::new(want.as_str()).load_for_size(ctx, Vec2::splat(px as f32)), Ok(egui::load::TexturePoll::Ready { .. }));
    if ready {
        ctx.data_mut(|d| d.insert_temp(id, px));
        return want;
    }
    match ctx.data(|d| d.get_temp::<u32>(id)) {
        Some(shown) if shown != px => sized(base, shown),
        _ => want,
    }
}

/// Paint a cover image (square-cropped, CDN-resized) with a placeholder underneath.
pub fn paint_cover(ui: &Ui, rect: Rect, thumbs: &[Thumb], radius: f32, zoom: f32) {
    let px = (rect.width() * ui.ctx().pixels_per_point()) as u32;
    paint_cover_px(ui, rect, thumbs, radius, zoom, px);
}

/// [`paint_cover`] fetching the image at `px` rather than the drawn size. A cover that
/// animates in size must ask for one fixed size: every new size is a new download,
/// blank until it arrives.
pub fn paint_cover_px(ui: &Ui, rect: Rect, thumbs: &[Thumb], radius: f32, zoom: f32, px: u32) {
    if !ui.is_rect_visible(rect) {
        return;
    }
    let px = fetch_px(px);
    ui.painter().rect_filled(rect, radius, theme::glass_panel(0.07));
    let Some(t) = pick_thumb(thumbs, px.min(544)) else {
        icon_at(ui, rect.center(), icon::MUSIC, rect.width() * 0.3, theme::text_faint());
        return;
    };
    let mut uv = t.square_uv();
    if zoom > 1.0 {
        // Zoom by cropping the UV rect: stays inside the rounded corners.
        let c = uv.center();
        let half = uv.size() / (2.0 * zoom);
        uv = Rect::from_center_size(c, half * 2.0);
    }
    let url = steady_url(ui.ctx(), &t.url, px);
    paint_remote(ui, &url, rect, radius, uv, Color32::WHITE);
    // A video's thumbnail is listed at most 480 tall, a square of 480 from it: a bigger
    // cover gets YouTube's 720 one once it's there.
    if px > t.height
        && let Some(big) = crate::model::ytimg_max(&t.url)
    {
        // Always 16:9, whatever shape the listed one was.
        let wide = Thumb { url: String::new(), width: 1280, height: 720 }.square_uv();
        paint_when_ready(ui, &big, rect, radius, if zoom > 1.0 { Rect::from_center_size(wide.center(), wide.size() / zoom) } else { wide });
    }
}

/// `url` over what's painted at `rect`, once it has loaded: nothing meanwhile, nor if it
/// fails, so what's under it shows.
pub fn paint_when_ready(ui: &Ui, url: &str, rect: Rect, radius: f32, uv: Rect) {
    if matches!(egui::Image::new(url).load_for_size(ui.ctx(), rect.size()), Ok(egui::load::TexturePoll::Ready { .. })) {
        paint_remote(ui, url, rect, radius, uv, Color32::WHITE);
    }
}

/// [`paint_cover_px`] with its own corners.
pub fn paint_cover_corners(ui: &Ui, rect: Rect, thumbs: &[Thumb], corners: CornerRadius, px: u32) {
    let px = fetch_px(px);
    ui.painter().rect_filled(rect, corners, theme::glass(0.07));
    let Some(t) = pick_thumb(thumbs, px.min(544)) else { return };
    let url = steady_url(ui.ctx(), &t.url, px);
    if let Ok(egui::load::TexturePoll::Ready { texture }) = egui::Image::new(url).load_for_size(ui.ctx(), rect.size()) {
        egui::Image::from_texture(texture).uv(t.square_uv()).corner_radius(corners).paint_at(ui, rect);
    }
}

/// Paint the cover cropped to fill `rect` (any aspect), tinted, for full-bleed backdrops.
pub fn paint_cover_fill(ui: &Ui, rect: Rect, thumbs: &[Thumb], tint: Color32) {
    let ppp = ui.ctx().pixels_per_point();
    let px = fetch_px((rect.width().max(rect.height()) * ppp) as u32).clamp(200, 1200);
    let Some(t) = pick_thumb(thumbs, px) else { return };
    let uv = t.square_uv();
    // Crop the square to the rect's aspect ratio, keeping the centre.
    let aspect = rect.width() / rect.height().max(1.0);
    let size = if aspect >= 1.0 { vec2(uv.width(), uv.height() / aspect) } else { vec2(uv.width() * aspect, uv.height()) };
    let url = steady_url(ui.ctx(), &t.url, px);
    paint_remote(ui, &url, rect, 0.0, Rect::from_center_size(uv.center(), size), tint);
}

/// Start loading a cover at `px` without drawing it, so it's ready when it appears.
pub fn preload_cover(ctx: &egui::Context, thumbs: &[Thumb], px: u32) {
    let px = fetch_px(px);
    if let Some(t) = pick_thumb(thumbs, px.min(544)) {
        let _ = egui::Image::new(sized(&t.url, px)).load_for_size(ctx, Vec2::splat(px as f32));
    }
}

/// The cover as a disc rotated by `angle` (radians): a textured triangle fan, since
/// egui's image rotation doesn't round corners. Fetched at `px` (see [`paint_cover_px`]).
pub fn paint_disc(ui: &Ui, center: Pos2, r: f32, thumbs: &[Thumb], angle: f32, tint: Color32, px: u32) {
    ui.painter().circle_filled(center, r, theme::glass_panel(0.07));
    let px = fetch_px(px);
    let Some(t) = pick_thumb(thumbs, px.min(544)) else { return };
    let url = steady_url(ui.ctx(), &t.url, px);
    // While it loads, the loader wakes the UI once it's ready.
    let Ok(egui::load::TexturePoll::Ready { texture }) = egui::Image::new(url).load_for_size(ui.ctx(), Vec2::splat(r * 2.0)) else { return };
    let uv = t.square_uv();
    let (uc, uh) = (uv.center(), uv.width() / 2.0);
    let mut mesh = egui::Mesh::with_texture(texture.id);
    mesh.vertices.push(egui::epaint::Vertex { pos: center, uv: uc, color: tint });
    let ppp = ui.ctx().pixels_per_point();
    // Enough sides that a big disc doesn't show them, and an edge that fades over about
    // a pixel: a mesh isn't antialiased, so a hard edge shows as steps.
    let n = ((r * ppp * 0.75) as u32).clamp(96, 480);
    let feather = 1.0 / ppp;
    let (inner, outer) = (|i: u32| 1 + 2 * i, |i: u32| 2 + 2 * i);
    for i in 0..=n {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        let (s, c) = a.sin_cos();
        let (sr, cr) = (a - angle).sin_cos();
        let rim = uc + vec2(cr * uh, sr * uv.height() / 2.0);
        mesh.vertices.push(egui::epaint::Vertex { pos: center + vec2(c, s) * (r - feather / 2.0), uv: rim, color: tint });
        mesh.vertices.push(egui::epaint::Vertex { pos: center + vec2(c, s) * (r + feather / 2.0), uv: rim, color: Color32::TRANSPARENT });
        if i > 0 {
            let p = i - 1;
            mesh.add_triangle(0, inner(p), inner(i));
            mesh.add_triangle(inner(p), outer(p), inner(i));
            mesh.add_triangle(outer(p), outer(i), inner(i));
        }
    }
    ui.painter().add(egui::Shape::mesh(mesh));
}

/// Album/playlist/artist/song card with hover zoom, scrim and a play button. `pin`: see
/// `card_menu_contents`.
pub fn card(ui: &mut Ui, c: &Card, width: f32, liked: bool, pin: Option<bool>, acts: &mut Vec<Action>) {
    let height = width + 58.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let resp = named(resp, &c.title).on_hover_cursor(egui::CursorIcon::PointingHand);
    // contains_pointer, not hovered: the play button on top takes the hover, and the
    // card must stay "hovered" while the pointer is on it or the button would vanish.
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.contains_pointer(), 0.22);
    let look = &crate::preset::active().app;
    let cl = &look.card_look;
    // The whole picture lifting on hover, inside the space it's been given.
    let press = if resp.is_pointer_button_down_on() { look.controls.press_cards } else { 1.0 };
    let img = Rect::from_center_size(rect.min + Vec2::splat(width / 2.0) - vec2(0.0, cl.lift_px * h), Vec2::splat(width * (1.0 + (cl.hover_scale - 1.0) * h) * press));
    let radius = if c.round { width / 2.0 } else { theme::card_radius().min(width / 2.0) };
    if let Some([y, blur, a]) = cl.shadow.filter(|_| cl.shadow_rest || h > 0.0) {
        soft_shadow(ui, img.translate(vec2(0.0, y)), radius, blur, if cl.shadow_rest { a } else { a * h });
    }
    if look.card_fill > 0.0 {
        ui.painter().rect_filled(rect, radius, theme::glass(look.card_fill));
    }
    if look.card_stroke > 0.0 {
        ui.painter().rect_stroke(rect, radius, egui::Stroke::new(1.0, theme::glass(look.card_stroke)), egui::StrokeKind::Inside);
    }
    paint_cover(ui, img, &c.thumbs, radius, 1.0 + cl.image_zoom * h * theme::hover_k());
    if h > 0.0 && !c.round {
        ui.painter().rect_filled(img, radius, theme::shade(cl.image_dim * h));
    }
    if let Target::Album(id) | Target::Playlist(id) = &c.target {
        download_veil(ui, img, radius, id);
    }
    if cl.image_outline > 0.0 {
        ui.painter().rect_stroke(img, radius, egui::Stroke::new(1.0, theme::glass(cl.image_outline)), egui::StrokeKind::Inside);
    }
    // Play button rises into the corner on hover; on the playing item it stays up
    // and shows pause.
    let active = active_state(ui, &c.target);
    let h = if active.is_some() { 1.0 } else { h };
    let mut play_hit = false;
    if h > 0.01 {
        let center = if cl.play_center { img.center() } else { img.right_bottom() + vec2(-30.0, -30.0 + 8.0 * (1.0 - h)) };
        let prect = Rect::from_center_size(center, Vec2::splat(cl.play_px.max(32.0)));
        let p = ui.interact(prect, resp.id.with("play"), Sense::click());
        let playing = active == Some(true);
        let label = if playing { crate::tr!("player-pause-item", title = c.title.as_str()) } else { crate::tr!("player-play-item", title = c.title.as_str()) };
        let p = named(p, &label).on_hover_cursor(egui::CursorIcon::PointingHand);
        let ph = theme::anim_bool(ui.ctx(), p.id.with("h"), p.hovered(), 0.12);
        let accent = ui.ctx().data(|d| d.get_temp::<Color32>(egui::Id::unique("accent"))).unwrap_or(theme::RED);
        let rest_a = egui::lerp(cl.play_alpha..=cl.play_hover_alpha.unwrap_or(cl.play_alpha), ph) * h;
        let back = crate::preset::hex(&cl.play_color).map_or(theme::shade(rest_a), |c| theme::with_alpha(c, rest_a));
        let back = if cl.play_hover_accent { theme::lerp_color(back, accent, ph) } else { back };
        if let Some(g) = look.glass.card_play {
            crate::shader::frost(ui.painter(), Rect::from_center_size(center, prect.size() + Vec2::splat(4.0 * ph)), crate::shader::Frost { look: g, radius: 999.0, opacity: h, fade: 0.0 });
        }
        let play_r = (prect.width() / 2.0 + 2.0 * ph) * (1.0 + (cl.play_hover_scale - 1.0) * ph);
        if !cl.play_bare {
            ui.painter().circle_filled(center, play_r, back);
        }
        if cl.play_ring > 0.0 {
            ui.painter().circle_stroke(center, play_r - 0.5, egui::Stroke::new(1.0, theme::with_alpha(Color32::WHITE, cl.play_ring * h)));
        }
        let glyph_c = crate::preset::hex(&cl.play_glyph_color).unwrap_or(Color32::WHITE);
        play_pause_glyph(ui, center, cl.play_glyph_px * (1.0 + (cl.play_hover_scale - 1.0) * ph), theme::with_alpha(glyph_c, h), playing);
        play_hit = p.clicked();
    }
    let tcolor = theme::text();
    let title = galley_wrapped(ui, &c.title, theme::weighted(cl.title_weight, cl.title_px), tcolor, width, 2);
    let title_h = title.size().y;
    let title_pos = pos2(rect.left(), img.bottom() + 10.0);
    galley_tipped(ui, title_pos, title, tcolor, &c.title);
    let sub_y = img.bottom() + 14.0 + title_h;
    // A playlist kept downloaded is marked at the start of its subtitle.
    let mark_w = if kept_downloaded(ui, &c.target) { DOWNLOAD_MARK_W } else { 0.0 };
    if mark_w > 0.0 {
        download_mark(ui, rect.left(), sub_y + 8.0, 1.0);
    }
    linked_text(ui, pos2(rect.left() + mark_w, sub_y), &c.subtitle, &c.links, theme::regular(13.0), theme::with_alpha(theme::text_dim(), cl.subtitle_alpha), width - 36.0 - mark_w, acts);
    // On the subtitle line, whose width leaves room for it, so a two-line title never sits under it.
    let more_rect = Rect::from_center_size(pos2(rect.right() - 16.0, sub_y + 8.0), Vec2::splat(30.0));
    let more = more_button(ui, more_rect, resp.id.with("more"), crate::tr!("ui-options"));
    let behavior = if matches!(c.target, Target::Song(_)) { egui::PopupCloseBehavior::CloseOnClickOutside } else { egui::PopupCloseBehavior::CloseOnClick };
    menu_popup(&more).close_behavior(behavior).show(|ui| card_menu_contents(ui, c, liked, pin, acts));
    // Right-click anywhere on the card opens the same menu, even on the play button,
    // a linked name or ⋮, which take the click themselves.
    right_click_menu(&resp).close_behavior(behavior).show(|ui| card_menu_contents(ui, c, liked, pin, acts));

    if play_hit {
        match &c.target {
            _ if active.is_some() => acts.push(Action::TogglePlay),
            Target::Song(t) => acts.push(Action::Radio(t.clone())),
            t => acts.push(Action::PlayCollection(t.clone(), false)),
        }
    } else if resp.clicked() && !more.clicked() {
        acts.push(Action::Open(c.target.clone()));
    }
}

/// Whether the playlist `target` is kept downloaded (its menu's Keep downloaded).
pub fn kept_downloaded(ui: &Ui, target: &Target) -> bool {
    let Target::Playlist(id) = target else { return false };
    ui.ctx().data(|d| d.get_temp::<std::sync::Arc<Vec<String>>>(egui::Id::unique("kept-downloaded"))).is_some_and(|k| k.contains(id))
}

/// Room the download mark takes at the start of a line, its gap after included.
pub const DOWNLOAD_MARK_W: f32 = 18.0;

/// The mark of a downloaded song or a playlist kept downloaded, at the start of a line
/// whose middle is `y`: an arrow down in a filled circle, as other music apps show it.
/// `alpha` fades it with the line it's on.
pub fn download_mark(ui: &Ui, x: f32, y: f32, alpha: f32) {
    let accent = ui.ctx().data(|d| d.get_temp::<Color32>(egui::Id::unique("accent"))).unwrap_or(theme::RED);
    let c = pos2(x + 6.5, y);
    ui.painter().circle_filled(c, 6.5, theme::with_alpha(accent, alpha));
    // Dark or white, whichever reads better on the accent.
    let ink = egui::Stroke::new(1.4, theme::with_alpha(theme::on_fill(accent), alpha));
    ui.painter().line_segment([c + vec2(0.0, -3.5), c + vec2(0.0, 3.2)], ink);
    ui.painter().line(vec![c + vec2(-2.8, 0.6), c + vec2(0.0, 3.4), c + vec2(2.8, 0.6)], ink);
}

/// While the album or playlist `id` downloads, its whole cover darkens, then the darkness
/// recedes from the bottom with the progress, so more and more of the picture shows in its
/// true colours. When the download ends what's left of it fades away.
pub fn download_veil(ui: &Ui, img: Rect, radius: f32, id: &str) {
    let ctx = ui.ctx();
    let runs = ctx.data(|d| d.get_temp::<std::sync::Arc<Vec<(String, f32)>>>(egui::Id::unique("download-progress")));
    let now = runs.as_ref().and_then(|r| r.iter().find(|(k, _)| k == id)).map(|&(_, p)| p);
    // Where it got to, while it shows; not downloading and gone, there's nothing to do.
    let held = egui::Id::unique(("download-veil-held", id));
    let Some(last) = now.or_else(|| ctx.data(|d| d.get_temp::<f32>(held))) else { return };
    let on = theme::anim_bool(ctx, ("download-veil-on", id), now.is_some(), 0.35);
    // The progress shows once the cover is fully dark; fading out, it holds where it was.
    let target = match now {
        Some(f) if on >= 0.999 => f,
        Some(_) => 0.0,
        None => last,
    };
    let p = theme::anim(ctx, ("download-veil", id), target, 0.5);
    if now.is_none() && on <= 0.001 {
        // Gone: a next download of it starts from a dark cover again.
        ctx.data_mut(|d| d.remove::<f32>(held));
        ctx.animate_value_with_time(egui::Id::unique(("download-veil", id)), 0.0, 0.0);
        return;
    }
    ctx.data_mut(|d| d.insert_temp(held, p));
    if p >= 0.999 {
        return;
    }
    let y = egui::lerp(img.bottom()..=img.top(), p);
    // The cover's own rounded shape, dimmed above the progress; the contrast is the edge.
    ui.painter().with_clip_rect(Rect::from_min_max(img.min, pos2(img.right(), y))).rect_filled(img, radius, theme::shade(0.62 * on));
}

/// A card's menu. `pin`: whether the playlist is pinned, where pinning it is offered.
pub fn card_menu_contents(ui: &mut Ui, c: &Card, liked: bool, pin: Option<bool>, acts: &mut Vec<Action>) {
    match &c.target {
        Target::Song(t) => song_menu(ui, t, liked, None, acts),
        t => {
            menu_body(ui, crate::preset::active().app.menu_look.min_px);
            if menu_action(ui, Some(icon::PLAY), crate::tr!("ui-play")) {
                acts.push(Action::PlayCollection(t.clone(), false));
            }
            if menu_action(ui, Some(icon::SHUFFLE), crate::tr!("ui-shuffle-play")) {
                acts.push(Action::PlayCollection(t.clone(), true));
            }
            if menu_action(ui, None, crate::tr!("settings-open")) {
                acts.push(Action::Open(t.clone()));
            }
            if matches!(t, Target::Album(_) | Target::Playlist(_)) && menu_action(ui, Some(icon::DOWNLOAD), crate::tr!("settings-download")) {
                acts.push(Action::DownloadMany(Some(t.clone()), c.title.clone(), None));
            }
            // Its songs aren't known here: shown when they went in its own folder.
            if let Target::Album(id) | Target::Playlist(id) = t
                && crate::downloads::has_list_folder(id)
                && menu_action(ui, Some(icon::FOLDER_OPEN), crate::tr!("widget-show-in-folder"))
            {
                crate::downloads::show_list(id, &[]);
            }
            if let (Some(pinned), Target::Playlist(id)) = (pin, t)
                && menu_action(ui, Some(if pinned { icon::PIN_OFF } else { icon::PIN }), if pinned { crate::tr!("widget-unpin-playlist") } else { crate::tr!("widget-pin-playlist") })
            {
                acts.push(Action::Pin(id.clone(), !pinned));
            }
            if super::actions::shareable(t) {
                menu_divider(ui);
                if menu_action(ui, None, crate::tr!("ui-copy-link")) {
                    acts.push(Action::Share(t.clone()));
                }
            }
        }
    }
}

pub fn more_button(ui: &mut Ui, rect: Rect, id: egui::Id, label: &str) -> Response {
    let response = named(ui.interact(rect, id, Sense::click()), label)
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.hovered() || egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response)) {
        ui.painter().rect_filled(rect, 7.0, theme::glass(0.16));
    }
    let lit = if crate::preset::active().app.card_look.menu_hover_accent { ui.ctx().data(|d| d.get_temp::<Color32>(egui::Id::unique("accent"))).unwrap_or(theme::RED) } else { theme::text() };
    let color = if response.hovered() { lit } else { theme::text_dim() };
    if crate::preset::active().app.row_look.menu_dots_across {
        for dx in [-5.5, 0.0, 5.5] {
            ui.painter().circle_filled(rect.center() + vec2(dx, 0.0), 1.7, color);
        }
    } else {
        icon_at(ui, rect.center(), icon::ELLIPSIS_VERTICAL, 18.0, color);
    }
    response
}

/// Frosts behind an open popup and lets it pop in, as the theme has it.
fn popup_extras(ctx: &egui::Context, id: egui::Id) {
    let open = egui::Popup::is_id_open(ctx, id);
    let opened = id.with("opened");
    if !open {
        ctx.data_mut(|d| d.remove::<f64>(opened));
        return;
    }
    let look = &crate::preset::active().app;
    let now = ctx.input(|i| i.time);
    let since = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(opened, || now));
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("menu-opened"), since));
    // Off: the menu is open at full size from its first frame.
    let since = if theme::anim_off() { f64::NEG_INFINITY } else { since };
    if look.menu_look.item_slide > 0.0 && now - since < theme::secs(0.2) {
        ctx.request_repaint();
    }
    let scale = match look.menu_look.pop {
        Some([from, secs]) => {
            let t = ((now - since) as f32 / theme::secs(f64::from(secs.max(0.01))) as f32).clamp(0.0, 1.0);
            if t < 1.0 {
                ctx.request_repaint();
            }
            egui::lerp(from..=1.0, theme::ease_out(t))
        }
        None => 1.0,
    };
    let Some(rect) = ctx.memory(|m| m.area_rect(id)) else { return };
    // Set every frame, so the zoom's last frame leaves the menu at full size.
    let c = rect.center().to_vec2();
    ctx.set_transform_layer(egui::LayerId::new(egui::Order::Foreground, id), egui::emath::TSTransform::from_translation(c) * egui::emath::TSTransform::from_scaling(scale) * egui::emath::TSTransform::from_translation(-c));
    if let Some(glass) = look.glass.menu {
        // On a layer of its own just under the popups.
        let layer = egui::LayerId::new(egui::Order::Middle, egui::Id::unique("menu-frost"));
        ctx.move_to_top(layer);
        let painter = ctx.layer_painter(layer);
        let r = Rect::from_center_size(rect.center(), rect.size() * scale);
        let radius = ctx.global_style().visuals.menu_corner_radius.nw as f32;
        crate::shader::frost(&painter, r, crate::shader::Frost { look: glass, radius, opacity: 1.0, fade: 0.0 });
    }
}

/// Frost behind an open dialog, and the line along its bottom if the theme has only that.
pub fn modal_extras(ctx: &egui::Context, id: egui::Id) {
    let look = &crate::preset::active().app;
    let Some(rect) = ctx.memory(|m| m.area_rect(id)) else { return };
    if let Some(glass) = look.glass.dialog {
        let layer = egui::LayerId::new(egui::Order::Middle, egui::Id::unique("dialog-frost"));
        ctx.move_to_top(layer);
        crate::shader::frost(&ctx.layer_painter(layer), rect, crate::shader::Frost { look: glass, radius: look.menu_radius.unwrap_or(16.0), opacity: 1.0, fade: 0.0 });
    }
    if look.dialog_bottom_line {
        let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("dialog-line")));
        p.hline(rect.x_range(), rect.bottom(), egui::Stroke::new(1.0, theme::glass(look.menu_border.max(0.2))));
    }
}

/// A button's menu, in the theme's look.
pub fn menu_popup<'a>(resp: &Response) -> egui::Popup<'a> {
    popup_extras(&resp.ctx, egui::Popup::default_response_id(resp));
    egui::Popup::menu(resp).frame(theme::menu_frame(&resp.ctx.global_style()))
}

/// A menu that a right-click anywhere on `resp` opens at the pointer, even over the
/// buttons and links inside it, which take the click themselves. Any other click on
/// it closes the menu.
pub fn right_click_menu<'a>(resp: &Response) -> egui::Popup<'a> {
    menu_popup(resp)
        .open_memory(if resp.container_secondary_clicked() {
            Some(egui::SetOpenCommand::Bool(true))
        } else if resp.container_clicked() {
            Some(egui::SetOpenCommand::Bool(false))
        } else {
            None
        })
        .at_pointer_fixed()
}

/// A right-click menu, in the theme's look.
pub fn context_popup<'a>(resp: &Response) -> egui::Popup<'a> {
    popup_extras(&resp.ctx, egui::Popup::default_response_id(resp));
    egui::Popup::context_menu(resp).frame(theme::menu_frame(&resp.ctx.global_style()))
}

/// Sets up a menu or dropdown: `width` wide, with a small gap between rows so two
/// highlights (picked and hovered) never run together.
pub fn menu_body(ui: &mut Ui, width: f32) {
    ui.set_width(width);
    ui.spacing_mut().item_spacing = vec2(0.0, 2.0);
}

/// A thin line between groups of menu rows.
pub fn menu_divider(ui: &mut Ui) {
    ui.add_space(5.0);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(r.shrink2(vec2(8.0, 0.0)), 0.0, theme::glass_panel(0.08));
    ui.add_space(5.0);
}

pub fn menu_action(ui: &mut Ui, glyph: Option<char>, label: &str) -> bool {
    menu_row(ui, glyph, label, true, None)
}

/// A menu row for one of several choices (a Sort menu's orders), ticked when it's the one
/// on. Looks like [`menu_action`]'s rows, the tick where their icons go.
pub fn menu_choice(ui: &mut Ui, label: &str, on: bool) -> bool {
    menu_row(ui, on.then_some(icon::CHECK), label, false, None)
}

/// A menu row; without a glyph, an arrow in its place when `arrow`, else nothing.
fn menu_row(ui: &mut Ui, glyph: Option<char>, label: &str, arrow: bool, unlike: Option<&Track>) -> bool {
    let m = crate::preset::active().app.menu_look;
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), m.row_px.max(26.0)), Sense::click());
    let response = named(response, label).on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.hovered() {
        ui.painter().rect_filled(rect, m.row_radius, theme::glass(m.hover));
    }
    if m.separator > 0.0 {
        ui.painter().hline(rect.x_range(), rect.bottom() + ui.spacing().item_spacing.y / 2.0, egui::Stroke::new(1.0, theme::glass(m.separator)));
    }
    let color = if response.hovered() { theme::text() } else { theme::text_dim() };
    // Rows slide in as the menu opens.
    let slide = if m.item_slide > 0.0 && !theme::anim_off() {
        let since = ui.ctx().input(|i| i.time) - ui.ctx().data(|d| d.get_temp::<f64>(egui::Id::unique("menu-opened"))).unwrap_or(0.0);
        m.item_slide * (1.0 - theme::ease_out(since as f32 / theme::secs(0.2) as f32))
    } else {
        0.0
    };
    let slide = slide + if m.row_slide > 0.0 { m.row_slide * theme::anim_bool(ui.ctx(), response.id.with("slide"), response.hovered(), 0.12) } else { 0.0 };
    let (icon_x, text_x) = if m.icon_right { (rect.right() - 19.0 + slide, rect.left() + 12.0 + slide) } else { (rect.left() + 19.0 + slide, rect.left() + 40.0 + slide) };
    let gesture = unlike.map(|track| super::ratings::like_gesture(ui, &response, super::ratings::rating_id(track), true, None));
    if let Some(gesture) = &gesture {
        super::ratings::unlike_heart(ui, pos2(icon_x, rect.center().y), 16.0, 1.0, color, color, gesture.progress);
        if gesture.hint {
            super::ratings::unlike_hint(ui, &response);
        }
    } else if let Some(glyph) = glyph {
        icon_at(ui, pos2(icon_x, rect.center().y), glyph, 16.0, color);
    } else if arrow {
        ui.painter().text(pos2(icon_x, rect.center().y), Align2::CENTER_CENTER, "↗", theme::semibold(17.0), color);
    }
    ui.painter().text(pos2(text_x, rect.center().y), Align2::LEFT_CENTER, label, theme::regular(14.0), theme::text());
    let activate = gesture.map_or_else(|| response.clicked(), |gesture| gesture.activate);
    if activate { ui.close(); }
    activate
}

/// `remove`: the song's row in the open playlist, when it's yours and the song can come out.
pub fn song_menu(ui: &mut Ui, t: &Track, liked: bool, remove: Option<usize>, acts: &mut Vec<Action>) {
    menu_body(ui, crate::preset::active().app.menu_look.min_px);
    song_actions(ui, t, liked, remove, acts);
}

/// The song menu's items, for menus that add their own (now playing's options).
pub fn song_actions(ui: &mut Ui, t: &Track, liked: bool, remove: Option<usize>, acts: &mut Vec<Action>) {
    if menu_row(ui, Some(icon::HEART), if liked { crate::tr!("ui-unlike") } else { crate::tr!("ui-like-song") }, true, liked.then_some(t)) {
        acts.push(Action::Like(t.clone(), !liked));
    }
    if menu_action(ui, Some(icon::THUMBS_DOWN), crate::tr!("ui-dislike-song")) {
        acts.push(Action::Dislike(t.clone()));
    }
    if menu_action(ui, Some(icon::LIST_PLUS), crate::tr!("ui-add-playlist")) {
        acts.push(Action::AddToPlaylist(vec![t.clone()]));
    }
    if let Some(i) = remove
        && menu_action(ui, Some(icon::LIST_MINUS), crate::tr!("ui-remove-playlist"))
    {
        acts.push(Action::RemoveFromPlaylist(vec![i]));
    }
    if menu_action(ui, None, crate::tr!("ui-copy-link")) {
        acts.push(Action::Share(Target::Song(t.clone())));
    }
    match crate::downloads::file_for(t) {
        Some(path) => {
            if menu_action(ui, Some(icon::FOLDER_OPEN), crate::tr!("widget-show-in-folder")) {
                crate::downloads::show_file(&path);
            }
        }
        None => {
            if menu_action(ui, Some(icon::DOWNLOAD), crate::tr!("settings-download")) {
                acts.push(Action::Download(t.clone()));
            }
        }
    }
    menu_divider(ui);
    song_actions_rest(ui, t, acts);
}

/// [`song_actions`]' first five as a row of icons, each named on hover, for a menu that
/// has to stay short: like (held to unlike, as the row is), dislike, add to playlist,
/// copy link, and download or show it in its folder.
pub fn song_action_icons(ui: &mut Ui, t: &Track, liked: bool, acts: &mut Vec<Action>) {
    let gap = 2.0;
    let w = ((ui.available_width() - 4.0 * gap) / 5.0).floor();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        let cell = |ui: &mut Ui, label: &str| {
            let (r, resp) = ui.allocate_exact_size(vec2(w, 36.0), Sense::click());
            let resp = named(resp, label).on_hover_text(label).on_hover_cursor(egui::CursorIcon::PointingHand);
            let m = crate::preset::active().app.menu_look;
            if resp.hovered() {
                ui.painter().rect_filled(r, m.row_radius, theme::glass(m.hover));
            }
            let color = if resp.hovered() { theme::text() } else { theme::text_dim() };
            (r, resp, color)
        };
        let label = if liked { crate::tr!("ui-unlike") } else { crate::tr!("ui-like-song") };
        let (r, resp, color) = cell(ui, label);
        let like = if liked {
            let gesture = super::ratings::like_gesture(ui, &resp, super::ratings::rating_id(t), true, None);
            super::ratings::unlike_heart(ui, r.center(), 16.0, 1.0, color, color, gesture.progress);
            if gesture.hint {
                super::ratings::unlike_hint(ui, &resp);
            }
            gesture.activate
        } else {
            icon_at(ui, r.center(), icon::HEART, 16.0, color);
            resp.clicked()
        };
        if like {
            acts.push(Action::Like(t.clone(), !liked));
            ui.close();
        }
        // Each closes the menu, as its row does.
        let icon = |ui: &mut Ui, glyph: char, label: &str| {
            let (r, resp, color) = cell(ui, label);
            icon_at(ui, r.center(), glyph, 16.0, color);
            if resp.clicked() {
                ui.close();
            }
            resp.clicked()
        };
        if icon(ui, icon::THUMBS_DOWN, crate::tr!("ui-dislike-song")) {
            acts.push(Action::Dislike(t.clone()));
        }
        if icon(ui, icon::LIST_PLUS, crate::tr!("ui-add-playlist")) {
            acts.push(Action::AddToPlaylist(vec![t.clone()]));
        }
        if icon(ui, icon::LINK, crate::tr!("ui-copy-link")) {
            acts.push(Action::Share(Target::Song(t.clone())));
        }
        match crate::downloads::file_for(t) {
            Some(path) => {
                if icon(ui, icon::FOLDER_OPEN, crate::tr!("widget-show-in-folder")) {
                    crate::downloads::show_file(&path);
                }
            }
            None => {
                if icon(ui, icon::DOWNLOAD, crate::tr!("settings-download")) {
                    acts.push(Action::Download(t.clone()));
                }
            }
        }
    });
}

/// The rest of [`song_actions`], past its first five: play next, queue, radio, and to
/// the album and artist.
pub fn song_actions_rest(ui: &mut Ui, t: &Track, acts: &mut Vec<Action>) {
    if menu_action(ui, Some(icon::SKIP_FORWARD), crate::tr!("player-play-next")) {
        acts.push(Action::PlayNext(t.clone()));
    }
    if menu_action(ui, Some(icon::LIST_MUSIC), crate::tr!("ui-add-queue")) {
        acts.push(Action::Enqueue(t.clone()));
    }
    if menu_action(ui, Some(icon::RADIO), crate::tr!("player-start-radio")) {
        acts.push(Action::Radio(t.clone()));
    }
    if t.album.as_ref().and_then(|a| a.id.as_ref()).is_some() || t.first_artist_id().is_some() {
        menu_divider(ui);
    }
    if let Some(id) = t.album.as_ref().and_then(|a| a.id.clone())
        && menu_action(ui, Some(icon::DISC_3), crate::tr!("player-go-to-album"))
    {
        acts.push(Action::Open(Target::Album(id)));
    }
    if let Some(id) = t.first_artist_id()
        && menu_action(ui, Some(icon::USER), crate::tr!("player-go-to-artist")) {
            acts.push(Action::Open(Target::Artist(id.to_owned())));
        }
    if crate::ytm::upload_entity(&t.id).is_some() {
        menu_divider(ui);
        if menu_action(ui, Some(icon::TRASH_2), crate::tr!("dialog-delete-upload")) {
            acts.push(Action::DeleteUpload(t.clone()));
        }
    }
}

/// Soft drop shadow under a rounded rect (e.g. artwork).
pub fn soft_shadow(ui: &Ui, rect: Rect, radius: f32, blur: f32, alpha: f32) {
    shadow_dropped(ui, rect, radius, blur, alpha, (blur * 0.35) as i8);
}

/// [`soft_shadow`] exactly where `rect` is, for a caller that already moved it by the theme's own offset.
pub fn soft_shadow_placed(ui: &Ui, rect: Rect, radius: f32, blur: f32, alpha: f32) {
    shadow_dropped(ui, rect, radius, blur, alpha, 0);
}

/// A soft round shadow for a disc of any size, fading out over `blur` past its edge.
pub fn disc_shadow(ui: &Ui, center: Pos2, r: f32, blur: f32, alpha: f32) {
    const RINGS: usize = 8;
    const STEPS: usize = 120;
    let mut mesh = egui::Mesh::default();
    for ring in 0..=RINGS {
        let t = ring as f32 / RINGS as f32;
        let fade = (1.0 - t) * (1.0 - t);
        for i in 0..STEPS {
            let (s, c) = (i as f32 / STEPS as f32 * std::f32::consts::TAU).sin_cos();
            mesh.colored_vertex(center + vec2(c, s) * (r - 4.0 + (blur + 4.0) * t), theme::shade(alpha * fade));
        }
    }
    let at = |ring: usize, i: usize| (ring * STEPS + i % STEPS) as u32;
    for ring in 0..RINGS {
        for i in 0..STEPS {
            mesh.add_triangle(at(ring, i), at(ring, i + 1), at(ring + 1, i));
            mesh.add_triangle(at(ring, i + 1), at(ring + 1, i + 1), at(ring + 1, i));
        }
    }
    ui.painter().add(egui::Shape::mesh(mesh));
}

fn shadow_dropped(ui: &Ui, rect: Rect, radius: f32, blur: f32, alpha: f32, drop: i8) {
    let s = egui::epaint::Shadow { offset: [0, drop], blur: blur as u8, spread: 0, color: theme::shade(alpha) };
    ui.painter().add(s.as_shape(rect, CornerRadius::same(radius as u8)));
}

/// What's playing, published by the app each frame so cards can show a pause button
/// on the item that's playing.
#[derive(Clone, Default)]
pub struct NowPlaying {
    pub song: Option<String>,
    pub source: Option<Target>,
    pub playing: bool,
}

impl NowPlaying {
    pub fn id() -> egui::Id {
        egui::Id::unique("now-playing")
    }
}

/// `Some(playing)` when `target` is what's loaded: the current song, or the album /
/// playlist / artist the queue came from.
pub fn active_state(ui: &Ui, target: &Target) -> Option<bool> {
    let np = ui.ctx().data(|d| d.get_temp::<NowPlaying>(NowPlaying::id()))?;
    let hit = match target {
        Target::Song(t) => np.song.as_deref() == Some(t.id.as_str()),
        t => np.source.as_ref() == Some(t),
    };
    hit.then_some(np.playing)
}

/// Round button glyph: pause bars while `active` is playing, otherwise a play triangle.
pub fn play_pause_glyph(ui: &Ui, center: Pos2, r: f32, color: Color32, playing: bool) {
    if playing {
        for dx in [-0.45, 0.45] {
            let bar = Rect::from_center_size(center + vec2(dx * r, 0.0), vec2(r * 0.42, r * 1.7));
            ui.painter().rect_filled(bar, r * 0.1, color);
        }
    } else {
        play_triangle(ui, center, r, color);
    }
}

/// A play triangle `r` tall each way from `center`, with its centroid (not its box) there,
/// so in a round button it looks centred rather than leaning left.
pub fn play_triangle(ui: &Ui, center: Pos2, r: f32, color: Color32) {
    let pts = vec![
        center + vec2(-r * 0.6 + r / 15.0, -r),
        center + vec2(r + r / 15.0, 0.0),
        center + vec2(-r * 0.6 + r / 15.0, r),
    ];
    ui.painter().add(egui::Shape::convex_polygon(pts, color, egui::Stroke::NONE));
}

pub fn pause_bars(ui: &Ui, center: Pos2, r: f32, color: Color32) {
    for dx in [-r * 0.45, r * 0.45] {
        let bar = Rect::from_center_size(center + vec2(dx, 0.0), vec2(r * 0.42, r * 1.7));
        ui.painter().rect_filled(bar, 1.5, color);
    }
}

/// The playing row's bars: three rounded bars rising and falling at their own slow
/// pace, settling low when paused. They tell the frame pacing they're on screen, so
/// they move at a steady rate instead of the page's slower one.
pub fn equalizer(ui: &Ui, rect: Rect, time: f64, playing: bool, color: Color32) {
    let ctx = ui.ctx();
    let on = theme::anim_bool(ctx, ui.scope_id().with("eq"), playing, 0.35);
    if on > 0.0 {
        let frame = ctx.cumulative_frame_nr();
        ctx.data_mut(|d| d.insert_temp(equalizer_id(), frame));
    }
    let px = ctx.pixels_per_point();
    let snap = |v: f32| (v * px).round() / px;
    let bar_w = snap((rect.width() * 0.22).max(2.0));
    let gap = snap(bar_w * 0.75);
    let total = bar_w * 3.0 + gap * 2.0;
    let left = snap(rect.center().x - total / 2.0);
    let t = time as f32;
    // Two slow waves per bar, so the motion doesn't visibly repeat.
    const WAVES: [(f32, f32, f32); 3] = [(4.1, 2.3, 0.0), (3.3, 5.2, 1.9), (4.7, 2.9, 3.7)];
    for (i, (a, b, off)) in WAVES.into_iter().enumerate() {
        let wave = 0.5 + 0.3 * (t * a + off).sin() + 0.2 * (t * b + off * 1.7).sin();
        let rest = [0.35, 0.6, 0.45][i];
        let amp = rest + (0.3 + 0.7 * wave.clamp(0.0, 1.0) - rest) * on;
        // The height stays fractional (and the rect unrounded): whole-pixel steps
        // read as stutter on bars this small.
        let h = (rect.height() * amp).max(bar_w);
        let x = left + i as f32 * (bar_w + gap);
        let bar = Rect::from_min_max(pos2(x, snap(rect.bottom()) - h), pos2(x + bar_w, snap(rect.bottom())));
        ui.painter().add(egui::epaint::RectShape::filled(bar, bar_w / 2.0, color).with_round_to_pixels(false));
    }
}

/// Last frame the playing bars were drawn on (frame pacing reads it).
pub fn equalizer_id() -> egui::Id {
    egui::Id::unique("equalizer_frame")
}

/// Whether the playing bars were on screen last frame.
pub fn equalizer_shown(ctx: &egui::Context) -> bool {
    let now = ctx.cumulative_frame_nr();
    ctx.data(|d| d.get_temp::<u64>(equalizer_id())).is_some_and(|f| f + 1 >= now)
}

pub struct RowOpts {
    pub number: Option<usize>,
    pub show_album: bool,
    /// Leave room for a plays column, whether or not this song has a count: set for a
    /// whole list, so its album and plays columns line up from row to row.
    pub plays_col: bool,
    pub current: bool,
    pub playing: bool,
    pub accent: Color32,
    pub time: f64,
    pub liked: bool,
    /// In the queue: who or what added the song, as shown ("Added by Sam").
    pub added: Option<String>,
    /// Choosing a match for the Spotify transfer: a click picks the song and its cover plays it.
    pub pick: bool,
    /// Selecting songs: a checkbox leads the row; `Some(true)` when this one is picked.
    pub selected: Option<bool>,
    /// The row's index in your own open playlist, so its menu can take it out.
    pub remove: Option<usize>,
    /// The row's index in the queue: on hover, an × takes the duration's place to take it out.
    pub queue_remove: Option<usize>,
}


/// A song row's height with the space to the next.
pub fn row_height(ui: &Ui) -> f32 {
    crate::preset::active().app.row_look.height + ui.spacing().item_spacing.y
}

/// A long list of song rows: only the ones on screen are built; the rest is empty space
/// of the same height. `row(ui, i)` draws row `i`, under an id of its own so its hover
/// and menu state don't jump to another song as the list scrolls.
pub fn visible_rows(ui: &mut Ui, n: usize, row: impl FnMut(&mut Ui, usize)) {
    visible_rows_gap(ui, n, 0, 0.0, row);
}

/// `visible_rows`, with `gap` of space under row `after`.
pub fn visible_rows_gap(ui: &mut Ui, n: usize, after: usize, gap: f32, mut row: impl FnMut(&mut Ui, usize)) {
    let h = row_height(ui);
    let top = ui.cursor().top();
    let clip = ui.clip_rect();
    let gap = if after < n { gap } else { 0.0 };
    // Rows past `after` sit `gap` lower.
    let below = (after + 1) as f32;
    let f = (clip.top() - top) / h;
    let first = if f < below { f.floor() } else { ((clip.top() - top - gap) / h).floor().max(below) };
    let first = (first.max(0.0) as usize).min(n);
    let last = ((((clip.bottom() - top) / h).ceil().max(0.0)) as usize + 1).clamp(first, n);
    // The gap is in what's skipped once its row is.
    ui.add_space(first as f32 * h + if first > after { gap } else { 0.0 });
    for i in first..last {
        ui.push_id(i, |ui| row(ui, i));
        if i == after && gap > 0.0 {
            ui.add_space(gap);
        }
    }
    ui.add_space((n - last) as f32 * h + if last <= after { gap } else { 0.0 });
}

/// Song row: cover (or index), title, artists, album, duration. Click plays.
pub fn track_row(ui: &mut Ui, t: &Track, o: &RowOpts, acts: &mut Vec<Action>) -> Response {
    let in_queue;
    let rl = match ui.data(|d| d.get_temp::<bool>(egui::Id::unique("in-queue"))).filter(|on| *on).map(|_| crate::preset::active().app.queue_look) {
        Some(q) => {
            let base = &crate::preset::active().app.row_look;
            in_queue = crate::preset::RowLook { thumb_radius: q.thumb_radius.or(base.thumb_radius), radius: q.radius.unwrap_or(base.radius), pad: q.pad.unwrap_or(base.pad), hover_border: q.hover_border.unwrap_or(base.hover_border), image_zoom: q.image_zoom.unwrap_or(base.image_zoom), image_dim: q.image_dim.unwrap_or(base.image_dim), ..base.clone() };
            &in_queue
        }
        None => &crate::preset::active().app.row_look,
    };
    let (rect, base) = ui.allocate_exact_size(vec2(ui.available_width(), rl.height), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return base;
    }
    // While dragging, only the drop place should show.
    let hovered = base.contains_pointer() && ui.ctx().dragged_id().is_none();
    let like_on_hover = !o.pick && o.selected.is_none();
    let (rating, show_rating, dimmed) = super::ratings::row(ui, t, o.liked, hovered && like_on_hover);
    let rating = rating.filter(|r| show_rating && (*r != SongRating::Indifferent || like_on_hover));
    let opacity = if dimmed { 0.58 } else { 1.0 };
    let dur_w = 52.0;
    let rating_w = if show_rating { 28.0 } else { 0.0 };
    // Picking: the cover (left 54 px) plays, the rest of the row picks.
    let hit_left = if o.pick && o.number.is_none() { rect.left() + 54.0 } else { rect.left() };
    let hit_rect = Rect::from_min_max(pos2(hit_left, rect.top()), pos2(rect.right() - 42.0, rect.bottom()));
    let resp = named(ui.interact(hit_rect, base.id.with("play"), Sense::click()), &song_name(&t.title, t.explicit)).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), base.id.with("h"), hovered, 0.15);
    // Grows or shrinks on the row's own place, the list around it staying put.
    let press = if resp.is_pointer_button_down_on() { crate::preset::active().app.controls.press_rows } else { 1.0 };
    let rect = if rl.hover_scale != 1.0 || press != 1.0 { Rect::from_center_size(rect.center(), rect.size() * (1.0 + (rl.hover_scale - 1.0) * h) * press) } else { rect };
    let rect = rect.translate(vec2(0.0, -rl.hover_lift * h));
    if h > 0.0 || o.current {
        let a = if o.current { rl.current + 0.05 * h } else { rl.hover * h };
        let fill = if o.current && !rl.current_color.is_empty() { theme::with_alpha(crate::preset::paint(&rl.current_color, o.accent), a) } else { theme::glass(a) };
        fill_rounded(ui, rect, rl.radius, fill);
        let outline = crate::preset::active().app.controls.ghost_outline.max(rl.hover_border);
        if outline > 0.0 && h > 0.0 {
            ui.painter().rect_stroke(rect, rl.radius, egui::Stroke::new(1.0, theme::glass(outline * h)), egui::StrokeKind::Inside);
        }
        if o.current && rl.current_bar > 0.0 {
            ui.painter().rect_filled(Rect::from_min_size(rect.min, vec2(rl.current_bar, rect.height())), rl.current_bar / 2.0, o.accent);
        }
    }
    if rl.separator > 0.0 {
        ui.painter().hline(rect.x_range(), rect.bottom(), egui::Stroke::new(1.0, theme::glass(rl.separator)));
    }
    // While selecting, the cover only shows the song (clicks pick) and picked rows are tinted.
    let h = if o.selected.is_some() { 0.0 } else { h };
    if o.selected == Some(true) {
        ui.painter().rect_filled(rect, 8.0, theme::with_alpha(o.accent, 0.14));
    }
    if o.pick {
        let p = theme::anim_bool(ui.ctx(), base.id.with("p"), resp.hovered(), 0.12);
        if p > 0.0 {
            ui.painter().rect_stroke(rect, 8.0, egui::Stroke::new(1.5, theme::with_alpha(o.accent, p)), egui::StrokeKind::Inside);
        }
    }
    let mut x = rect.left() + rl.pad;
    let cy = rect.center().y;
    if let Some(on) = o.selected {
        checkbox(ui, pos2(x + 9.0, cy), on);
        x += 30.0;
    }
    if let Some(n) = o.number {
        let slot = Rect::from_center_size(pos2(x + 12.0, cy), vec2(24.0, 24.0));
        if o.current {
            equalizer(ui, slot.shrink(4.0), o.time, o.playing, theme::with_alpha(o.accent, opacity));
        } else if h > 0.5 {
            play_triangle(ui, slot.center(), 6.0, theme::text());
        } else {
            ui.painter().text(slot.center(), Align2::CENTER_CENTER, n.to_string(), theme::regular(14.0), theme::with_alpha(theme::text_dim(), opacity));
        }
        x += 36.0;
    } else {
        let img = Rect::from_min_size(pos2(x, cy - rl.thumb_px / 2.0), Vec2::splat(rl.thumb_px));
        let tr = rl.thumb_radius.unwrap_or_else(theme::thumb_radius);
        paint_cover(ui, img, &t.thumbs, tr, 1.0 + rl.image_zoom * h);
        if dimmed {
            ui.painter().rect_filled(img, tr, theme::panel(1.0 - opacity));
        }
        if o.pick {
            let play = named(ui.interact(img, base.id.with("cover"), Sense::click()), crate::tr!("ui-play")).on_hover_text(crate::tr!("ui-play")).on_hover_cursor(egui::CursorIcon::PointingHand);
            if play.clicked() {
                acts.push(Action::Play(vec![t.clone()], 0));
            }
        }
        if o.current {
            ui.painter().rect_filled(img, tr, theme::shade(0.45));
            equalizer(ui, img.shrink(10.0), o.time, o.playing, Color32::WHITE);
        } else if h > 0.0 {
            ui.painter().rect_filled(img, tr, theme::shade(rl.image_dim * h));
            play_triangle(ui, img.center(), 7.0, theme::with_alpha(Color32::WHITE, h));
        }
        x += rl.thumb_px + 14.0;
    }
    let right = rect.right() - 41.0 - rl.pad;
    let dim = theme::with_alpha(theme::text_dim(), rl.byline_alpha * opacity);
    let text_right = right - dur_w - rating_w;
    let avail = text_right - x;
    let wide = avail > 520.0;
    // Album rows (numbered) are just song name and plays; other rows are
    // title + artists, then album, then plays when there's room.
    let album_row = o.number.is_some();
    let (title_w, plays_x, album_x) = match (wide, o.show_album, o.plays_col) {
        (true, true, true) => (avail * 0.45, Some(x + avail * 0.82), Some(x + avail * 0.5)),
        (true, true, false) => (avail * 0.55, None, Some(x + avail * 0.6)),
        (true, false, true) => (avail * 0.78, Some(x + avail * 0.82), None),
        _ => (avail, None, None),
    };
    let plays = t.plays.map(|n| crate::tr!("status-plays", count = n, shown = crate::i18n::decimal(&crate::backend::compact(n))));
    let title_col = if dimmed { theme::with_alpha(theme::text_dim(), opacity) } else if o.current { o.accent } else { theme::text() };
    let sub = match (album_row, &plays, plays_x) {
        (true, Some(p), None) => Some(p.clone()),
        (true, _, _) => None,
        (false, Some(p), None) => Some(format!("{} \u{2022} {p}", t.artist_line())),
        (false, _, _) => Some(t.artist_line()),
    };
    // Who added it first: in a narrow queue the end of the line is cut off.
    let sub = match (sub, &o.added) {
        (Some(s), Some(by)) if !s.is_empty() => Some(format!("{by} • {s}")),
        (_, Some(by)) => Some(by.clone()),
        (sub, _) => sub,
    };
    let title_y = if sub.is_some() { cy - 18.0 } else { cy - 9.0 };
    // A downloaded song is marked at the start of its second line (of its only one, with none).
    let marked = crate::downloads::is_listed(t);
    let mark_w = if marked { DOWNLOAD_MARK_W } else { 0.0 };
    let (title_x, title_room) = if sub.is_none() { (x + mark_w, title_w - mark_w) } else { (x, title_w) };
    if marked {
        download_mark(ui, x, if sub.is_some() { cy + 10.0 } else { cy }, opacity);
    }
    title_at(ui, pos2(title_x, title_y), &t.title, t.explicit, theme::semibold(14.5), title_col, title_room);
    if let Some(sub) = &sub {
        linked_text(ui, pos2(x + mark_w, cy + 2.0), sub, &t.artists, theme::regular(13.0), dim, title_w - mark_w, acts);
    }
    if let (Some(px), Some(p)) = (plays_x, &plays) {
        ui.painter().text(pos2(px, cy), Align2::LEFT_CENTER, p, theme::regular(13.5), dim);
    }
    if let (Some(ax), Some(album)) = (album_x, &t.album) {
        let w = plays_x.unwrap_or(text_right) - ax - 10.0;
        let album_color = theme::with_alpha(theme::text_dim(), opacity);
        let g = galley(ui, &album.name, theme::regular(13.5), album_color, w);
        let ar = Rect::from_min_size(pos2(ax, cy - 9.0), g.size());
        ui.painter().galley(ar.min, g.clone(), album_color);
        // Album name opens the album, like the artist names.
        if let Some(id) = &album.id {
            let r = named(ui.interact(ar, resp.id.with("album"), Sense::click()), &album.name);
            let r = r.on_hover_cursor(egui::CursorIcon::PointingHand);
            if r.hovered() {
                ui.painter().hline(ar.x_range(), ar.bottom() - 1.0, egui::Stroke::new(1.0, album_color));
            }
            if r.clicked() {
                acts.push(Action::Open(Target::Album(id.clone())));
            }
        }
        tip_if_elided(ui, ar, &g, &album.name);
    }
    if let Some(rating) = rating {
        let label = match rating {
            SongRating::Like => crate::tr!("ui-unlike"),
            SongRating::Dislike => crate::tr!("ui-remove-dislike"),
            SongRating::Indifferent => crate::tr!("ui-like-song"),
        };
        let slot = Rect::from_center_size(pos2(text_right + rating_w / 2.0, cy), Vec2::splat(26.0));
        let button = named(ui.interact(slot, base.id.with(("rating", label)), Sense::click()), label)
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let gesture = super::ratings::like_gesture(ui, &button, super::ratings::rating_id(t), rating == SongRating::Like, None);
        let highlighted = button.hovered() || button.has_focus();
        if rating != SongRating::Indifferent || hovered || button.has_focus() {
            if highlighted && theme::light() {
                ui.painter().circle_filled(slot.center(), 11.0, theme::shade(0.68));
            }
            let ink = if highlighted { Color32::WHITE } else { dim };
            if rating == SongRating::Dislike {
                super::vector::dislike(ui, slot.center(), 14.0, true, ink);
            } else {
                let fill = if rating == SongRating::Like { 1.0 } else { 0.0 };
                super::ratings::unlike_heart(ui, slot.center(), 14.0, fill, ink, ink, gesture.progress);
            }
            focus_ring(ui, &button, slot, 13.0);
        }
        if gesture.hint {
            super::ratings::unlike_hint(ui, &button);
        } else if rating != SongRating::Like {
            button.clone().on_hover_text(label);
        }
        if gesture.activate {
            acts.push(match rating {
                SongRating::Like => Action::RemoveLike(super::ratings::rated_track(t)),
                SongRating::Dislike => Action::RemoveDislike(super::ratings::rating_id(t).to_owned()),
                SongRating::Indifferent => Action::Like(super::ratings::rated_track(t), true),
            });
        }
    }
    // Not `hovered`: the queue's drag grip counts as dragged from the press, which would hide
    // the × before the click lands. The queue leaves it out once a drag has really started.
    match o.queue_remove.filter(|_| base.contains_pointer()) {
        Some(i) => {
            let x = Rect::from_center_size(pos2(right - 14.0, cy), Vec2::splat(28.0));
            let xr = named(ui.interact(x, base.id.with("rm"), Sense::click()), crate::tr!("ui-remove-queue")).on_hover_cursor(egui::CursorIcon::PointingHand);
            if xr.hovered() {
                ui.painter().circle_filled(x.center(), 14.0, theme::glass(0.16));
            }
            icon_at(ui, x.center(), icon::X, 15.0, if xr.hovered() { theme::text() } else { theme::text_dim() });
            if xr.clicked() {
                acts.push(Action::QueueRemove(i));
            }
        }
        None => {
            if let Some(d) = t.duration {
                digits_at(ui, pos2(right, cy), &super::fmt_time(d), theme::regular(13.0), dim);
            }
        }
    }
    let more_rect = Rect::from_center_size(pos2(rect.right() - 20.0, cy), Vec2::splat(30.0));
    // Hidden at rest in some themes, not gone: it's there on hover, and right click opens the same menu.
    let more_id = base.id.with("more");
    let open_id = more_id.with("open");
    if !rl.hover_menu || hovered || ui.data(|d| d.get_temp::<bool>(open_id).unwrap_or(false)) {
        let more = more_button(ui, more_rect, more_id, crate::tr!("ui-song-options"));
        menu_popup(&more).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| song_menu(ui, t, o.liked, o.remove, acts));
        if rl.hover_menu {
            let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&more));
            ui.data_mut(|d| d.insert_temp(open_id, open));
        }
    }
    // Right-click anywhere on the row opens the same menu.
    context_popup(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| song_menu(ui, t, o.liked, o.remove, acts));
    resp
}

/// A time ending at `right`, in even-width digits when the theme has them.
pub fn digits_at(ui: &Ui, right: Pos2, s: &str, font: egui::FontId, color: Color32) -> Rect {
    if !crate::preset::active().app.tabular_digits {
        return ui.painter().text(right, Align2::RIGHT_CENTER, s, font, color);
    }
    time_at(ui, right, Align2::RIGHT_CENTER, s, font, color)
}

/// `s` with every digit centred in a cell as wide as the widest digit, so a running time
/// keeps its width and doesn't shuffle sideways each second (egui can't ask the font for
/// its tabular figures). Everything else keeps its own width.
pub fn time_galley(ui: &Ui, s: &str, font: egui::FontId, color: Color32) -> std::sync::Arc<egui::Galley> {
    use egui::text::{LayoutJob, TextFormat};
    // The exact advance, not the galley's rounded-out size.
    let advance = |g: &egui::Galley| g.rows.first().map_or(0.0, |r| r.row.size.x);
    let width_of = |c: char| advance(&ui.painter().layout_no_wrap(c.to_string(), font.clone(), color));
    let widths: [f32; 10] = std::array::from_fn(|d| width_of(char::from(b'0' + d as u8)));
    let cell = widths.iter().copied().fold(0.0, f32::max);
    let format = TextFormat::simple(font.clone(), color);
    let mut job = LayoutJob::default();
    // The space each digit leaves after itself, given to whatever comes next.
    let mut carry = 0.0;
    // What the whole should measure: the same for every time of the same shape.
    let mut target = 0.0;
    let mut buf = [0u8; 4];
    for c in s.chars() {
        let pad = c.to_digit(10).map_or(0.0, |d| cell - widths[d as usize]);
        target += if c.is_ascii_digit() { cell } else { width_of(c) };
        job.append(c.encode_utf8(&mut buf), carry + pad / 2.0, format.clone());
        carry = pad / 2.0;
    }
    // The last digit's trailing half, so the width doesn't hang on which digit ends it.
    job.append("", carry, format);
    let g = ui.fonts_mut(|f| f.layout_job(job.clone()));
    // Glyph advances are snapped, so the sum can be a hair off: make it up at the end.
    let off = target - advance(&g);
    if off.abs() < 1e-3 {
        return g;
    }
    if let Some(last) = job.sections.last_mut() {
        last.leading_space = (last.leading_space + off).max(0.0);
    }
    ui.fonts_mut(|f| f.layout_job(job))
}

/// A running time `g` painted at `at` that's a button too (on `hit`): a click swaps the
/// song's length for the time left and back (`left`). Its own target, over the player
/// bar's click for now playing; brighter while hovered or focused.
pub fn time_toggle(ui: &Ui, hit: Rect, at: Pos2, g: std::sync::Arc<egui::Galley>, [rest, hover]: [Color32; 2], left: &mut bool) {
    let name = if *left { tr!("player-time-show-length", time = g.text()) } else { tr!("player-time-show-left", time = g.text()) };
    let resp = named(ui.interact(hit, ui.scope_id().with("time-toggle"), Sense::click()), &name).on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.clicked() {
        *left = !*left;
    }
    let ring = Rect::from_min_size(at, g.size()).expand(2.0);
    focus_ring(ui, &resp, ring, 4.0);
    // Themes without a focus ring still show where focus is, white text over video included.
    if resp.has_focus() && crate::preset::active().app.controls.focus.is_none() {
        ui.painter().rect_stroke(ring, 4.0, egui::Stroke::new(1.0, hover), egui::StrokeKind::Outside);
    }
    let lit = resp.hovered() || resp.has_focus();
    ui.painter().galley_with_override_text_color(at, g, if lit { hover } else { rest });
}

/// [`time_galley`] painted with `align` at `pos`, as `Painter::text` would.
pub fn time_at(ui: &Ui, pos: Pos2, align: Align2, s: &str, font: egui::FontId, color: Color32) -> Rect {
    let g = time_galley(ui, s, font, color);
    let rect = align.anchor_size(pos, g.size());
    ui.painter().galley(rect.min, g, color);
    rect
}

/// An 18 px tick box centred on `c`, white like a primary pill when ticked.
fn checkbox(ui: &Ui, c: Pos2, on: bool) {
    let r = Rect::from_center_size(c, Vec2::splat(18.0));
    if on {
        ui.painter().rect_filled(r, 5.0, theme::text());
        icon_at(ui, c, icon::CHECK, 13.0, theme::on_text());
    } else {
        ui.painter().rect_stroke(r, 5.0, egui::Stroke::new(1.5, theme::text_dim()), egui::StrokeKind::Inside);
    }
}

/// Section header with optional strapline/avatar and ‹ › scroll arrows, which of them have
/// somewhere to go ([`shelf_arrows`]). One that doesn't is dimmed, so the other keeps its
/// place under the pointer; with neither, both are hidden.
/// Returns (scroll left, scroll right) clicks.
pub fn shelf_header(ui: &mut Ui, title: &str, strap: Option<&str>, strap_thumb: Option<&str>, arrows: Option<(bool, bool)>) -> (bool, bool) {
    let mut res = (false, false);
    let look = &crate::preset::active().app;
    ui.add_space(26.0);
    ui.horizontal(|ui| {
        if let Some(url) = strap_thumb {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 20.0, theme::glass(0.1));
            paint_remote(ui, url, r, 20.0, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            ui.add_space(4.0);
        }
        ui.vertical(|ui| {
            if let Some(s) = strap {
                ui.label(egui::RichText::new(s.to_uppercase()).font(theme::semibold(12.0)).color(theme::with_alpha(theme::text_dim(), look.strap_alpha)));
            }
            ui.label(egui::RichText::new(title).font(theme::weighted(look.heading_weight, look.heading_px)).extra_letter_spacing(look.heading_tracking_em * look.heading_px).color(theme::text()));
        });
        if let Some((left, right)) = arrows {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !left && !right {
                    // They keep their room, or the page would shift as they come and go.
                    ui.set_invisible();
                }
                let turn = crate::preset::active().app.controls.arrow_hover;
                let mut arrow = |on, glyph, name: &str| ui.add_enabled_ui(on, |ui| icon_button_turning(ui, glyph, 18.0, name, false, turn)).inner.clicked();
                res.1 = arrow(right, icon::CHEVRON_RIGHT, &crate::tr!("ui-scroll-right", title = title));
                res.0 = arrow(left, icon::CHEVRON_LEFT, &crate::tr!("ui-scroll-left", title = title));
            });
        }
    });
    ui.add_space(12.0);
    res
}

/// Aligned starts for complete viewport pages, with a full final page.
fn shelf_page_starts(count: usize, visible: usize) -> Vec<usize> {
    let visible = visible.max(1);
    let last = count.saturating_sub(visible);
    let mut starts = vec![0];
    let mut next = visible;
    while next < last {
        starts.push(next);
        next += visible;
    }
    if last > *starts.last().unwrap() {
        starts.push(last);
    }
    starts
}

fn shelf_page_target(starts: &[usize], step: f32, from: f32, nudge: i32) -> f32 {
    let next = if nudge > 0 {
        starts.iter().copied().find(|&start| start as f32 * step > from + 0.5).unwrap_or(*starts.last().unwrap())
    } else {
        starts.iter().copied().rev().find(|&start| (start as f32 * step) < from - 0.5).unwrap_or(starts[0])
    };
    next as f32 * step
}

fn shelf_arrows_id(id: &str) -> egui::Id {
    egui::Id::unique(("hscroll", id)).with("arrows")
}

/// Which arrows (left, right) have somewhere to go on the [`h_scroll`] of this `id`, as last
/// laid out. At its end there can be `more` to bring in, which the right one asks for.
pub fn shelf_arrows(ui: &Ui, id: &str, more: bool) -> (bool, bool) {
    let (left, right) = ui.ctx().data(|d| d.get_temp(shelf_arrows_id(id))).unwrap_or_default();
    (left, right || (more && left))
}

/// Horizontal scroller that animates between aligned pages of cards or song columns.
pub fn h_scroll<R>(ui: &mut Ui, id: &str, nudge: i32, count: usize, item_width: f32, gap: f32, add: impl FnOnce(&mut Ui) -> R) -> (R, bool) {
    let sid = egui::Id::unique(("hscroll", id));
    let anim_id = sid.with("anim");
    // ScrollArea hashes its id_salt once more. Read that same ID, or left clicks
    // always appear to start from zero.
    let state_id = ui.make_persistent_id(egui::IdSalt::new(sid));
    let current = egui::scroll_area::State::load(ui.ctx(), state_id).map_or(0.0, |s| s.clamped_offset().x);
    let target_id = sid.with("target");
    let coast_id = sid.with("coast");
    // Calls off a glide: the arrows' and a flick's.
    let stop_glide = |ctx: &egui::Context| ctx.data_mut(|d| {
        d.remove::<f32>(target_id);
        d.remove::<f32>(coast_id);
    });
    let viewport_id = sid.with("viewport");
    let viewport = ui.ctx().data(|d| d.get_temp::<f32>(viewport_id)).unwrap_or_else(|| ui.available_width());
    let step = item_width + gap;
    let visible = ((viewport + gap) / step).floor().max(1.0) as usize;
    let starts = shelf_page_starts(count, visible);
    let last_start = *starts.last().unwrap();
    // Give the last page enough trailing room to land exactly on its first item.
    let content_width = (last_start as f32 * step + viewport).max(count as f32 * step - gap);
    let max_scroll = (content_width - viewport).max(0.0);
    let mut reached_end = false;
    // Where the arrows send it, and apart from that where a flick coasts to: the arrows
    // page on from where it is now, not from where a flick would have stopped.
    let mut target: Option<f32> = ui.ctx().data(|d| d.get_temp(target_id));
    let mut coast: Option<f32> = ui.ctx().data(|d| d.get_temp(coast_id));
    if nudge != 0 {
        let t = shelf_page_target(&starts, step, target.unwrap_or(current), nudge);
        reached_end = nudge > 0 && count > visible && (t - last_start as f32 * step).abs() < 0.5;
        // Snap the animator to the real position, then glide to the target.
        ui.ctx().animate_value_with_time(anim_id, current, 0.0);
        (target, coast) = (Some(t), None);
        ui.ctx().data_mut(|d| {
            d.insert_temp(target_id, t);
            d.remove::<f32>(coast_id);
        });
    }
    // The shelf's own drag would take an up-and-down swipe too and the page wouldn't move,
    // so the drag is handled here instead, over the cards as last laid out (only as tall
    // as they are, so it doesn't reach the shelves below).
    use egui::scroll_area::{DragScroll, ScrollSource};
    let mut area = egui::ScrollArea::horizontal()
        .id_salt(sid)
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .scroll_source(ScrollSource { drag: DragScroll::Never, ..ScrollSource::default() });
    let size_id = sid.with("size");
    let mut held = None;
    let cards = ui.ctx().data(|d| d.get_temp::<Vec2>(size_id)).map(|size| Rect::from_min_size(ui.cursor().min, size));
    // Sideways scrolling over the shelf (touchpad, Shift+wheel) takes over from any glide,
    // or the glide would put it back.
    if cards.is_some_and(|r| ui.rect_contains_pointer(r)) && ui.input(|i| i.smooth_scroll_delta.x != 0.0) {
        (target, coast) = (None, None);
        stop_glide(ui.ctx());
    }
    if let Some(rect) = cards {
        let resp = ui.interact(rect, sid.with("drag"), Sense::DRAG);
        match shelf_drag(ui, &resp, sid.with("axis")) {
            ShelfDrag::Move(dx) => {
                // A drag takes over from any glide.
                (target, coast) = (None, None);
                stop_glide(ui.ctx());
                held = Some((current - dx).clamp(0.0, max_scroll));
            }
            ShelfDrag::Coast(dx) => {
                let c = (current - dx).clamp(0.0, max_scroll);
                ui.ctx().animate_value_with_time(anim_id, current, 0.0);
                coast = Some(c);
                ui.ctx().data_mut(|d| d.insert_temp(coast_id, c));
            }
            ShelfDrag::Idle => {}
        }
    }
    if let Some(x) = held {
        area = area.horizontal_scroll_offset(x);
    } else if let Some(t) = target.or(coast) {
        let v = ui.ctx().animate_value_with_time(anim_id, t, 0.35 * theme::speed());
        area = area.horizontal_scroll_offset(v);
        if (v - t).abs() < 0.5 {
            stop_glide(ui.ctx());
        }
    }
    let out = area.show(ui, |ui| ui.horizontal_top(|ui| {
        ui.set_min_width(content_width);
        add(ui)
    }).inner);
    ui.ctx().data_mut(|d| {
        d.insert_temp(viewport_id, out.inner_rect.width());
        // Not `inner_rect`'s height: that's all the room left below, over the next shelves.
        d.insert_temp(size_id, vec2(out.inner_rect.width(), out.content_size.y));
    });
    // For the header's next frame: it's drawn before the shelf.
    let at = target.unwrap_or(out.state.clamped_offset().x);
    let goes = |nudge| shelf_page_target(&starts, step, at, nudge);
    let arrows = (goes(-1) < at - 0.5, goes(1) > at + 0.5);
    let was = ui.ctx().data_mut(|d| std::mem::replace(d.get_temp_mut_or_default(shelf_arrows_id(id)), arrows));
    if was != arrows || out.inner_rect.width() != viewport {
        ui.ctx().request_repaint();
    }
    (out.inner, reached_end)
}

/// What a drag on a shelf asks of it this frame: move it by, or coast it on by, points.
pub(super) enum ShelfDrag {
    Idle,
    Move(f32),
    Coast(f32),
}

/// A drag on a shelf. A finger, once it has gone a few points, scrolls only the way it
/// started for the rest of it, as phones and browsers do with a scroller inside a page:
/// the shelf sideways, the page up and down. The mouse's left button drags the shelf
/// sideways only, once it has gone too far for a click. Let go, and it coasts on.
pub(super) fn shelf_drag(ui: &Ui, resp: &Response, state_id: egui::Id) -> ShelfDrag {
    use egui::style::ScrollAnimation;
    /// How far a drag goes before it counts (and before its direction does), points.
    const LOCK: f32 = 6.0;
    /// How much of the speed at letting go it coasts on: points per point/second.
    const COAST: f32 = 0.3;
    let (mut moved, mut axis, mut finger): (Vec2, Option<usize>, bool) = ui.ctx().data(|d| d.get_temp(state_id)).unwrap_or((Vec2::ZERO, None, false));
    if resp.drag_started() {
        // Told once: the frame a finger lifts has no touches left in it.
        (moved, axis, finger) = (Vec2::ZERO, None, ui.input(|i| i.any_touches()));
    }
    let mut out = ShelfDrag::Idle;
    if resp.dragged_by(egui::PointerButton::Primary) {
        // From the press on: the pointer's own delta would count a move made just before it.
        let delta = if resp.drag_started() { resp.total_drag_delta().unwrap_or_default() } else { resp.drag_delta() };
        moved += delta;
        let step = match axis {
            Some(_) => delta,
            // Locked now, past what still counts as a click: what it went before counts too.
            None if moved.length() > LOCK => {
                axis = Some(usize::from(finger && moved.y.abs() >= moved.x.abs()));
                moved
            }
            None => Vec2::ZERO,
        };
        match axis {
            Some(0) => {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                out = ShelfDrag::Move(step.x);
            }
            Some(_) => ui.scroll_with_delta_animation(vec2(0.0, step.y), ScrollAnimation::none()),
            None => {}
        }
    }
    if resp.drag_stopped() {
        // Not when the drag was called off (Escape).
        if resp.drag_stopped_by(egui::PointerButton::Primary)
            && let Some(a) = axis
        {
            let speed = (ui.input(|i| i.pointer.velocity())[a] * COAST).clamp(-2000.0, 2000.0);
            if a == 0 {
                // With the last move, when it came in with the letting go.
                out = ShelfDrag::Coast(speed + ui.input(|i| i.pointer.delta().x));
            } else if speed.abs() > 1.0 {
                ui.scroll_with_delta_animation(vec2(0.0, speed), ScrollAnimation::duration(theme::secs(0.6) as f32));
            }
        }
        (moved, axis) = (Vec2::ZERO, None);
    }
    ui.ctx().data_mut(|d| d.insert_temp(state_id, (moved, axis, finger)));
    out
}

/// The skeleton's light band sweeping across `r`, for anything still loading.
pub fn shimmer_band(ui: &Ui, r: Rect, radius: f32, time: f64) {
    let phase = ((time * 0.8) % 1.6) as f32 - 0.3;
    let x = r.left() + (r.width() + 400.0) * phase - 200.0;
    let band = Rect::from_min_max(pos2((x - 120.0).max(r.left()), r.top()), pos2((x + 120.0).min(r.right()), r.bottom()));
    if band.width() > 0.0 {
        ui.painter().rect_filled(band, radius, theme::glass_panel(0.04));
    }
}

/// Paint a remote image into `rect`: the skeleton's shimmer while it downloads (not
/// egui's spinner), then a quick fade-in. One already loaded just shows.
pub fn paint_remote(ui: &Ui, url: &str, rect: Rect, radius: f32, uv: Rect, tint: Color32) {
    let fade = 0.22 / f64::from(theme::anim_speed());
    let ctx = ui.ctx();
    let now = ctx.input(|i| i.time);
    let id = egui::Id::unique(("img-fade", url));
    match egui::Image::new(url).load_for_size(ctx, rect.size()) {
        Ok(egui::load::TexturePoll::Ready { texture }) => {
            // Fade only what was seen loading; Some(None) = waiting, Some(Some(t)) = fading since t.
            let a = match ctx.data(|d| d.get_temp::<Option<f64>>(id)) {
                Some(Some(t0)) if now - t0 < fade => ((now - t0) / fade) as f32,
                Some(None) => {
                    ctx.data_mut(|d| d.insert_temp(id, Some(now)));
                    0.0
                }
                _ => 1.0,
            };
            if a < 1.0 {
                ctx.request_repaint();
            }
            let tint = tint.gamma_multiply(theme::ease_out(a));
            if crate::preset::active().app.squircle && radius >= 6.0 {
                paint_squircle_image(ui, texture.id, rect, radius, uv, tint);
            } else {
                egui::Image::from_texture(texture).uv(uv).corner_radius(CornerRadius::same(radius as u8)).tint(tint).paint_at(ui, rect);
            }
        }
        Ok(egui::load::TexturePoll::Pending { .. }) => {
            ctx.data_mut(|d| d.insert_temp::<Option<f64>>(id, None));
            shimmer_band(ui, rect, radius, now);
            ctx.request_repaint_after(std::time::Duration::from_millis(66));
        }
        // A failed image keeps its plain placeholder.
        Err(_) => {}
    }
}

/// A picture in a squircle: a fan with a soft edge.
fn paint_squircle_image(ui: &Ui, tex: egui::TextureId, rect: Rect, radius: f32, uv: Rect, tint: Color32) {
    let pts = squircle(rect, radius);
    let uv_at = |p: Pos2| pos2(egui::remap(p.x, rect.x_range(), uv.x_range()), egui::remap(p.y, rect.y_range(), uv.y_range()));
    let mut mesh = egui::Mesh::with_texture(tex);
    let c = rect.center();
    mesh.vertices.push(egui::epaint::Vertex { pos: c, uv: uv_at(c), color: tint });
    let n = pts.len() as u32;
    let feather = 0.7 / ui.ctx().pixels_per_point();
    for p in &pts {
        mesh.vertices.push(egui::epaint::Vertex { pos: *p, uv: uv_at(*p), color: tint });
    }
    for p in &pts {
        let out = *p + (*p - c).normalized() * feather;
        mesh.vertices.push(egui::epaint::Vertex { pos: out, uv: uv_at(*p), color: Color32::TRANSPARENT });
    }
    for i in 0..n {
        let j = (i + 1) % n;
        mesh.add_triangle(0, 1 + i, 1 + j);
        mesh.add_triangle(1 + i, 1 + n + i, 1 + j);
        mesh.add_triangle(1 + j, 1 + n + i, 1 + n + j);
    }
    ui.painter().add(egui::Shape::mesh(mesh));
}

/// Song rows still on their way, at the end of a list that's coming in.
pub fn skeleton_rows(ui: &mut Ui, time: f64, rows: usize) {
    for _ in 0..rows {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), crate::preset::active().app.row_look.height), Sense::hover());
        if !ui.is_rect_visible(r) {
            continue;
        }
        let cy = r.center().y;
        for (b, radius) in [
            (Rect::from_min_size(pos2(r.left() + 10.0, cy - 20.0), Vec2::splat(40.0)), theme::thumb_radius()),
            (Rect::from_min_size(pos2(r.left() + 64.0, cy - 13.0), vec2((r.width() * 0.3).min(260.0), 11.0)), 4.0),
            (Rect::from_min_size(pos2(r.left() + 64.0, cy + 4.0), vec2((r.width() * 0.18).min(160.0), 9.0)), 4.0),
        ] {
            ui.painter().rect_filled(b, radius, theme::glass_panel(0.06));
            shimmer_band(ui, b, radius, time);
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(66));
    }
}

/// Shimmering placeholder blocks while a page loads.
pub fn skeleton(ui: &mut Ui, time: f64, rows: usize, card: f32) {
    let shimmer = |ui: &Ui, r: Rect, radius: f32| {
        ui.painter().rect_filled(r, radius, theme::glass_panel(0.06));
        shimmer_band(ui, r, radius, time);
    };
    let top = ui.cursor().top();
    for _ in 0..rows {
        ui.add_space(26.0);
        let (t, _) = ui.allocate_exact_size(vec2(220.0, 26.0), Sense::hover());
        shimmer(ui, t, 6.0);
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            for _ in 0..8 {
                let (r, _) = ui.allocate_exact_size(vec2(card, card + 50.0), Sense::hover());
                shimmer(ui, Rect::from_min_size(r.min, Vec2::splat(card)), 8.0);
                shimmer(ui, Rect::from_min_size(r.min + vec2(0.0, card + 12.0), vec2(card * 0.8, 12.0)), 4.0);
                shimmer(ui, Rect::from_min_size(r.min + vec2(0.0, card + 32.0), vec2(card * 0.5, 10.0)), 4.0);
            }
        });
    }
    // The shimmer needs frames only while it's on screen, and 15 fps is plenty.
    if ui.is_rect_visible(Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=ui.cursor().top())) {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(66));
    }
}

#[cfg(test)]
mod shelf_scroll_tests {
    use super::{shelf_page_starts, shelf_page_target};

    #[test]
    fn a_squircle_has_only_real_points(){
        let pts = crate::ui::widgets::squircle(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::splat(100.0)), 20.0);
        assert!(pts.iter().all(|p| p.x.is_finite() && p.y.is_finite()));
    }

    #[test]
    fn arrows_visit_aligned_pages_in_both_directions() {
        let pages = shelf_page_starts(14, 5);
        assert_eq!(pages, [0, 5, 9]);
        let mut target = 0.0;
        for direction in [1, 1, -1] {
            target = shelf_page_target(&pages, 198.0, target, direction);
        }
        assert_eq!(target, 5.0 * 198.0);
        assert_eq!(shelf_page_target(&pages, 198.0, 0.0, -1), 0.0);
    }

    #[test]
    fn short_final_page_is_filled_without_overscrolling() {
        assert_eq!(shelf_page_starts(4, 3), [0, 1]);
        assert_eq!(shelf_page_starts(3, 3), [0]);
        assert_eq!(shelf_page_starts(1, 3), [0]);
        // The old last page is between new page starts after more picks arrive.
        assert_eq!(shelf_page_target(&shelf_page_starts(11, 3), 436.0, 2.0 * 436.0, 1), 3.0 * 436.0);
    }
}

#[cfg(test)]
mod shelf_touch_tests {
    use egui::{Event, Modifiers, PointerButton, Pos2, RawInput, Rect, TouchDeviceId, TouchId, TouchPhase, Vec2, pos2, vec2};

    /// A page that scrolls up and down with two shelves of cards in it, under a finger
    /// or a mouse.
    struct Page {
        ctx: egui::Context,
        time: f64,
        /// Room above the shelves; changing it moves them on the page.
        above: f32,
        /// The arrows' nudge for the top shelf, next frame.
        nudge: i32,
        /// How many cards the top shelf has, whether more can come in at its end, and where
        /// its arrows could go as last drawn.
        count: usize,
        more: bool,
        arrows: (bool, bool),
        /// How wide its cards are.
        card: f32,
        /// The top shelf's key: the page it's on, as `pages` gives it.
        key: String,
        /// Each shelf's first card, as last drawn, and where it started.
        cards: [Rect; 2],
        start: [Pos2; 2],
        clicks: usize,
    }

    /// How far each shelf has scrolled, and the page.
    #[derive(Debug, Clone, Copy)]
    struct Seen {
        shelf: [f32; 2],
        page: f32,
    }

    #[derive(Clone, Copy)]
    enum How {
        Finger,
        Mouse(PointerButton),
    }

    const LEFT: How = How::Mouse(PointerButton::Primary);

    impl Page {
        fn new(touch_screen: bool) -> Self {
            let mut p = Self { ctx: egui::Context::default(), time: 0.0, above: 100.0, nudge: 0, count: 20, more: false, arrows: (false, false), card: 150.0, key: "shelf-a".into(), cards: [Rect::NOTHING; 2], start: [Pos2::ZERO; 2], clicks: 0 };
            if touch_screen {
                // A touch elsewhere first, so the page knows there's a touch screen.
                p.frame(vec![touch(TouchPhase::Start, pos2(790.0, 590.0)), touch(TouchPhase::End, pos2(790.0, 590.0))]);
            }
            p.idle(3);
            p.start = p.cards.map(|c| c.min);
            p
        }

        fn frame(&mut self, events: Vec<Event>) {
            let raw = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))), time: Some(self.time), predicted_dt: 1.0 / 60.0, events, ..Default::default() };
            self.time += 1.0 / 60.0;
            let ctx = self.ctx.clone();
            let key = self.key.clone();
            let (above, nudge, count, more, width) = (self.above, std::mem::take(&mut self.nudge), self.count, self.more, self.card);
            let mut arrows = (false, false);
            let mut cards = [Rect::NOTHING; 2];
            let mut clicks = 0;
            let mut out = ctx.run_ui(raw, |ui| {
                egui::ScrollArea::vertical().id_salt("page").auto_shrink(false).show(ui, |ui| {
                    ui.add_space(above);
                    for (s, card) in cards.iter_mut().enumerate() {
                        let (id, nudge, count, width) = if s == 0 { (key.as_str(), nudge, count, width) } else { ("shelf-b", 0, 20, 150.0) };
                        if s == 0 {
                            arrows = super::shelf_arrows(ui, id, more);
                        }
                        super::h_scroll(ui, id, nudge, count, width, 10.0, |ui| {
                            for i in 0..count {
                                let (r, resp) = ui.allocate_exact_size(vec2(width, 150.0), egui::Sense::click());
                                if i == 0 {
                                    *card = r;
                                }
                                clicks += usize::from(resp.clicked());
                            }
                        });
                        ui.add_space(50.0);
                    }
                    ui.add_space(2000.0);
                });
            });
            out.textures_delta.clear();
            self.cards = cards;
            self.arrows = arrows;
            self.clicks += clicks;
        }

        fn idle(&mut self, frames: usize) {
            for _ in 0..frames {
                self.frame(vec![]);
            }
        }

        fn seen(&self) -> Seen {
            let moved = |s: usize| self.start[s] - self.cards[s].min;
            Seen { shelf: [moved(0).x, moved(1).x], page: moved(0).y }
        }

        /// On shelf `s`, `x` points from the window's left.
        fn on(&self, s: usize, x: f32) -> Pos2 {
            pos2(x, self.cards[s].center().y)
        }

        /// A drag from `from` by `by` in `steps`, let go at the end; returns what had moved
        /// just before letting go.
        fn drag(&mut self, how: How, from: Pos2, by: Vec2, steps: usize) -> Seen {
            let button = match how {
                How::Finger => PointerButton::Primary,
                How::Mouse(b) => b,
            };
            let press = |pos, pressed| Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE };
            let finger = |phase, pos| match how {
                How::Finger => vec![touch(phase, pos)],
                How::Mouse(_) => vec![],
            };
            self.frame([finger(TouchPhase::Start, from), vec![Event::PointerMoved(from), press(from, true)]].concat());
            let mut at = from;
            for _ in 0..steps {
                at += by / steps as f32;
                self.frame([finger(TouchPhase::Move, at), vec![Event::PointerMoved(at)]].concat());
            }
            let seen = self.seen();
            self.frame([vec![press(at, false)], finger(TouchPhase::End, at)].concat());
            seen
        }
    }

    fn touch(phase: TouchPhase, pos: Pos2) -> Event {
        Event::Touch { device_id: TouchDeviceId(1), id: TouchId(1), phase, pos, force: None }
    }

    #[test]
    fn swiping_up_on_the_cards_scrolls_the_page() {
        let mut p = Page::new(true);
        let seen = p.drag(How::Finger, p.on(0, 300.0), vec2(4.0, -150.0), 10);
        assert!(seen.page > 100.0, "the page didn't scroll: {seen:?}");
        assert_eq!(seen.shelf, [0.0, 0.0], "a shelf moved: {seen:?}");
    }

    #[test]
    fn swiping_sideways_on_the_cards_scrolls_the_shelf() {
        let mut p = Page::new(true);
        let seen = p.drag(How::Finger, p.on(0, 400.0), vec2(-150.0, 4.0), 10);
        assert!(seen.shelf[0] > 100.0, "the shelf didn't scroll: {seen:?}");
        assert_eq!(seen.page, 0.0, "the page moved: {seen:?}");
    }

    #[test]
    fn a_finger_lifted_after_swiping_up_leaves_the_shelf_and_the_page_coasts_on() {
        let mut p = Page::new(true);
        let held = p.drag(How::Finger, p.on(0, 400.0), vec2(-40.0, -200.0), 10);
        p.idle(60);
        let after = p.seen();
        assert_eq!(after.shelf, [0.0, 0.0], "a shelf moved: {after:?}");
        assert!(after.page > held.page + 50.0, "the page didn't coast on: {held:?} then {after:?}");
    }

    #[test]
    fn a_finger_on_the_lower_shelf_after_the_shelves_moved_scrolls_only_it() {
        let mut p = Page::new(true);
        // The shelves move up the page, and a finger lands on the lower one where it is now.
        p.above = 0.0;
        p.idle(1);
        let seen = p.drag(How::Finger, p.on(1, 400.0), vec2(-150.0, 0.0), 10);
        assert_eq!(seen.shelf[0], 0.0, "the upper shelf moved: {seen:?}");
        assert!(seen.shelf[1] > 100.0, "the lower shelf didn't scroll: {seen:?}");
    }

    #[test]
    fn the_mouse_drags_the_shelf_sideways_even_when_it_starts_down() {
        let mut p = Page::new(false);
        let seen = p.drag(LEFT, p.on(1, 400.0), vec2(-150.0, 20.0), 10);
        assert!(seen.shelf[1] > 100.0, "the shelf didn't scroll: {seen:?}");
        assert_eq!((seen.shelf[0], seen.page), (0.0, 0.0), "something else moved: {seen:?}");
    }

    #[test]
    fn a_wobbly_click_is_a_click_and_leaves_the_shelf() {
        let mut p = Page::new(false);
        p.drag(LEFT, p.on(0, 80.0), vec2(-4.0, 1.0), 2);
        p.idle(30);
        assert_eq!(p.clicks, 1);
        assert_eq!(p.seen().shelf, [0.0, 0.0]);
    }

    #[test]
    fn other_mouse_buttons_dont_drag_the_shelf() {
        let mut p = Page::new(false);
        for b in [PointerButton::Secondary, PointerButton::Middle] {
            p.drag(How::Mouse(b), p.on(0, 400.0), vec2(-200.0, 0.0), 10);
        }
        p.idle(30);
        assert_eq!(p.seen().shelf, [0.0, 0.0]);
    }

    #[test]
    fn swiping_back_while_it_coasts_turns_it_around() {
        let mut p = Page::new(true);
        let forward = p.drag(How::Finger, p.on(0, 400.0), vec2(-250.0, 0.0), 10);
        assert!(forward.shelf[0] > 100.0, "the shelf didn't scroll: {forward:?}");
        let gone = p.seen().shelf[0];
        let back = p.drag(How::Finger, p.on(0, 100.0), vec2(250.0, 0.0), 10);
        assert!(back.shelf[0] < gone - 100.0, "the shelf didn't come back: from {gone} to {back:?}");
    }

    #[test]
    fn a_click_that_moves_as_far_as_a_click_may_leaves_the_shelf() {
        let mut p = Page::new(false);
        let at = p.on(0, 300.0);
        // Moved just before the press, too: that's not part of it.
        p.frame(vec![Event::PointerMoved(at - vec2(50.0, 0.0))]);
        p.drag(LEFT, at, vec2(-6.0, 0.0), 1);
        p.idle(30);
        assert_eq!(p.clicks, 1);
        assert_eq!(p.seen().shelf, [0.0, 0.0]);
    }

    #[test]
    fn a_touchpad_swipe_back_during_a_coast_takes_over() {
        let flick = |swipe_back: bool| {
            let mut p = Page::new(false);
            p.drag(LEFT, p.on(0, 600.0), vec2(-200.0, 0.0), 4);
            if swipe_back {
                for _ in 0..5 {
                    p.frame(vec![Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(26.0, 0.0), phase: TouchPhase::Move, modifiers: Modifiers::NONE, source: egui::MouseWheelSource::Unknown }]);
                }
            }
            p.idle(90);
            p.seen().shelf[0]
        };
        let (alone, back) = (flick(false), flick(true));
        assert!(alone - back > 100.0, "the swipe back went {}", alone - back);
    }

    #[test]
    fn an_arrow_during_a_coast_goes_to_the_next_page() {
        let mut p = Page::new(false);
        p.drag(LEFT, p.on(0, 600.0), vec2(-200.0, 0.0), 4);
        p.idle(2);
        let from = p.seen().shelf[0];
        p.nudge = 1;
        p.idle(90);
        let to = p.seen().shelf[0];
        // Cards are 160 apart and four or five fit, so the next page is past `from`, within 800.
        assert!(to > from && to - from <= 800.0 && to % 160.0 == 0.0, "from {from} to {to}");
    }

    #[test]
    fn arrows_go_only_where_the_shelf_has_more_to_show() {
        let mut p = Page::new(false);
        assert_eq!(p.arrows, (false, true), "at the start");
        p.nudge = 1;
        p.idle(90);
        assert_eq!(p.arrows, (true, true), "in the middle");
        for _ in 0..5 {
            p.nudge = 1;
            p.idle(90);
        }
        assert_eq!(p.arrows, (true, false), "at the end");
        // More can come in there.
        p.more = true;
        p.idle(1);
        assert_eq!(p.arrows, (true, true), "at the end, with more to come");
        // Fewer cards than fit, even left scrolled, and even with more to come.
        p.count = 3;
        p.idle(3);
        assert_eq!(p.arrows, (false, false), "all on show");
    }

    #[test]
    fn arrows_know_the_end_as_soon_as_a_click_sends_the_shelf_there() {
        let mut p = Page::new(false);
        p.count = 6;
        p.idle(3);
        assert_eq!(p.arrows, (false, true));
        p.nudge = 1;
        p.idle(2);
        assert_eq!(p.arrows, (true, false), "gliding to the end");
        p.idle(90);
        assert_eq!(p.arrows, (true, false), "there");
    }

    #[test]
    fn a_card_wider_than_the_page_dragged_aside_can_be_sent_back() {
        let mut p = Page::new(false);
        (p.count, p.card) = (1, 1000.0);
        p.idle(3);
        assert_eq!(p.arrows, (false, false));
        p.drag(LEFT, p.on(0, 600.0), vec2(-150.0, 0.0), 4);
        p.idle(90);
        assert_eq!(p.arrows, (true, false), "dragged aside");
        p.nudge = -1;
        p.idle(90);
        assert_eq!(p.arrows, (false, false), "sent back");
    }

    #[test]
    fn a_shelf_on_another_page_starts_at_its_beginning() {
        let mut p = Page::new(false);
        p.nudge = 1;
        p.idle(90);
        // Let go while it coasts, then open the next artist.
        p.drag(LEFT, p.on(0, 600.0), vec2(-300.0, 0.0), 3);
        let scrolled = p.seen().shelf[0];
        assert!(scrolled > 0.0);
        p.key = "shelf-a-next".into();
        p.idle(90);
        assert_eq!((p.seen().shelf[0], p.arrows), (0.0, (false, true)), "the next page's shelf");
        // It scrolls on its own, by arrow and by drag.
        p.nudge = 1;
        p.idle(90);
        assert_eq!(p.arrows, (true, true));
        p.drag(LEFT, p.on(0, 600.0), vec2(-200.0, 0.0), 4);
        p.idle(90);
        assert!(p.seen().shelf[0] > 0.0);
    }

    #[test]
    fn arrows_follow_a_drag() {
        let mut p = Page::new(false);
        p.drag(LEFT, p.on(0, 600.0), vec2(-200.0, 0.0), 4);
        p.idle(90);
        assert_eq!(p.arrows, (true, true));
        for _ in 0..3 {
            p.drag(LEFT, p.on(0, 100.0), vec2(600.0, 0.0), 4);
            p.idle(90);
        }
        assert_eq!(p.arrows, (false, true));
    }
}

#[cfg(test)]
mod wrap_tests {
    use super::wrap_rows;

    /// A letter a point: a word's width is its letter count.
    fn width(s: &str) -> f32 {
        s.chars().count() as f32
    }

    #[test]
    fn rows_break_between_whole_words() {
        // لأغنية has a two-letter ligature; a row's text is still whole words.
        let text = "يُطبَّق بدءاً من الأغنية التالية";
        let rows = wrap_rows(text, 14.0, 9, width);
        assert_eq!(rows, ["يُطبَّق بدءاً", "من الأغنية", "التالية"]);
        assert!(rows.iter().all(|r| width(r) <= 14.0));
        assert_eq!(rows.join(" "), text);
    }

    #[test]
    fn too_many_rows_end_in_an_ellipsis() {
        assert_eq!(wrap_rows("one two three four five", 9.0, 2, width), ["one two", "three…"]);
        assert_eq!(wrap_rows("a\nb\nc", 9.0, 2, width), ["a", "b…"]);
        assert_eq!(wrap_rows("a\nb", 9.0, 2, width), ["a", "b"]);
    }

    #[test]
    fn a_long_word_has_a_row_to_itself() {
        assert_eq!(wrap_rows("hi extraordinarily so", 5.0, 9, width), ["hi", "extraordinarily", "so"]);
    }
}

#[cfg(test)]
mod time_tests {
    #[test]
    fn running_times_keep_their_width() {
        let ctx = egui::Context::default();
        // Inter, the app's own font, whose figures are proportional unless asked otherwise.
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert("inter".into(), std::sync::Arc::new(egui::FontData::from_static(include_bytes!("../../assets/fonts/Inter-Regular.ttf"))));
        fonts.families.insert(egui::FontFamily::Proportional, vec!["inter".into()]);
        ctx.set_fonts(fonts);
        // Nothing paints these frames: their texture uploads are let go on purpose.
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let font = egui::FontId::proportional(13.0);
            let width = |s: &str| super::time_galley(ui, s, font.clone(), egui::Color32::WHITE).rows[0].row.size.x;
            // Inter's 1 is narrower than its 0, so these differ as plain text.
            let plain = |s: &str| ui.painter().layout_no_wrap(s.to_owned(), font.clone(), egui::Color32::WHITE).size().x;
            assert!((plain("1:11") - plain("0:00")).abs() > 0.5);
            for s in ["1:11", "0:00", "2:47", "9:58", "4:44 / 3:21"] {
                let base = if s.len() > 4 { width("0:00 / 0:00") } else { width("0:00") };
                assert!((width(s) - base).abs() < 0.01, "{s}: {} vs {base}", width(s));
            }
        });
        out.textures_delta.clear();
    }

    #[test]
    fn elided_detects_single_and_multiline_truncation() {
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let font = egui::FontId::proportional(13.0);
            let long = "A long setting description that cannot fit in a narrow column without being cut off";
            let cut = super::galley(ui, long, font.clone(), egui::Color32::WHITE, 80.0);
            assert!(super::is_elided(&cut, long));
            let fit = super::galley(ui, "Short", font.clone(), egui::Color32::WHITE, 200.0);
            assert!(!super::is_elided(&fit, "Short"));
            let literal = super::galley(ui, "Wait…", font.clone(), egui::Color32::WHITE, 200.0);
            assert!(!super::is_elided(&literal, "Wait…"));
            let wrap_cut = super::galley_wrapped(ui, long, font.clone(), egui::Color32::WHITE, 80.0, 2);
            assert!(super::is_elided(&wrap_cut, long));
            let wrap_fit = super::galley_wrapped(ui, "Short text", font, egui::Color32::WHITE, 200.0, 2);
            assert!(!super::is_elided(&wrap_fit, "Short text"));
        });
        out.textures_delta.clear();
    }

    /// Up next in Arabic, too long for its room beside the window buttons, keeps its start.
    #[test]
    fn right_to_left_text_is_cut_at_its_end() {
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let font = egui::FontId::proportional(20.0);
            let text = "التالي في الانتظار";
            let full = ui.fonts_mut(|f| f.layout_job(crate::bidi::job(text, font.clone(), egui::Color32::WHITE)).size().x);
            let cut = super::rtl_fit(ui, text, &font, full / 2.0);
            assert!(cut.starts_with("التالي") && cut.ends_with('…'), "{cut}");
        });
        out.textures_delta.clear();
    }
}
