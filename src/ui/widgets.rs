//! Reusable, hand-painted widgets. Everything custom-painted gets an accessible name
//! (`named`), so screen readers and UI Automation can see and invoke it.

use egui::{Align2, Color32, CornerRadius, Pos2, Rect, Response, Sense, Ui, Vec2, pos2, vec2};

use super::Action;
use crate::model::{Card, Link, Target, Thumb, Track, pick_thumb, sized};
use crate::theme::{self, icon};

pub fn named(resp: Response, name: &str) -> Response {
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, name));
    resp
}

/// Single-line text truncated with an ellipsis to `width`.
pub fn galley(ui: &Ui, text: &str, font: egui::FontId, color: Color32, width: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(10.0));
    ui.fonts_mut(|f| f.layout_job(job))
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
    let mut job = egui::text::LayoutJob::simple(text.to_owned(), font, color, width.max(10.0));
    job.wrap.max_rows = rows;
    job.wrap.break_anywhere = false;
    job.wrap.overflow_character = Some('…');
    ui.fonts_mut(|f| f.layout_job(job))
}

pub fn text_at(ui: &Ui, pos: Pos2, text: &str, font: egui::FontId, color: Color32, width: f32) -> Rect {
    let g = galley(ui, text, font, color, width);
    let rect = Rect::from_min_size(pos, g.size());
    ui.painter().galley(pos, g, color);
    rect
}

/// Single-line text whose artist names are links: underlined on hover, and a click
/// opens the artist's page. Names without a channel id stay plain text.
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
    let mut search_from = 0;
    for (i, l) in links.iter().enumerate() {
        let Some(id) = &l.id else { continue };
        let Some(off) = text[search_from..].find(l.name.as_str()) else { continue };
        let start = search_from + off;
        let end = start + l.name.len();
        search_from = end;
        let c0 = text[..start].chars().count();
        let c1 = text[..end].chars().count();
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
    ui.painter().galley(pos, g, color);
    rect
}

pub fn icon_at(ui: &Ui, center: Pos2, glyph: char, size: f32, color: Color32) {
    // Centred on the drawn shape: egui places these glyphs flush left in their box
    // (dropping the font's side bearing) and a little high, so box-centring is off.
    let g = ui.painter().layout_no_wrap(glyph.to_string(), theme::icon_font(size), color);
    let ink = g.mesh_bounds;
    let pos = if ink.is_positive() { center - ink.center().to_vec2() } else { center - g.size() / 2.0 };
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

/// Circular icon button with an animated hover halo.
pub fn icon_button(ui: &mut Ui, glyph: char, size: f32, name: &str, active: bool) -> Response {
    let d = size + 16.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(d), Sense::click());
    let resp = named(resp, name).on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
    if h > 0.0 {
        ui.painter().circle_filled(rect.center(), d / 2.0, theme::glass(0.10 * h));
    }
    let color = if active { theme::TEXT } else { theme::lerp_color(theme::text_dim(), theme::TEXT, h) };
    let press = if resp.is_pointer_button_down_on() { 0.9 } else { 1.0 };
    icon_at(ui, rect.center(), glyph, size * press, color);
    resp
}

/// Rounded pill button. `primary` = white with dark text (YouTube Music's Play button).
pub fn pill(ui: &mut Ui, label: &str, glyph: Option<char>, primary: bool) -> Response {
    // Laid out once; the colour is given when it's painted.
    let text = ui.painter().layout_no_wrap(label.to_owned(), theme::semibold(14.0), Color32::PLACEHOLDER);
    let icon_w = if glyph.is_some() { 24.0 } else { 0.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(text.size().x + icon_w + 36.0, 38.0), Sense::click());
    let resp = named(resp, label).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
    let (fill, fg) = if primary {
        (theme::lerp_color(Color32::from_gray(240), Color32::WHITE, h), Color32::from_gray(12))
    } else {
        (theme::glass(0.10 + 0.08 * h), theme::TEXT)
    };
    let r = rect.shrink(if resp.is_pointer_button_down_on() { 1.0 } else { 0.0 });
    ui.painter().rect_filled(r, 19.0, fill);
    let mut x = r.left() + 18.0;
    if let Some(g) = glyph {
        icon_at(ui, pos2(x + 8.0, r.center().y), g, 17.0, fg);
        x += icon_w;
    }
    ui.painter().galley(pos2(x, r.center().y - text.size().y / 2.0), text, fg);
    resp
}

/// Selectable chip (home moods, library tabs).
pub fn chip(ui: &mut Ui, label: &str, selected: bool) -> Response {
    let text = ui.painter().layout_no_wrap(label.to_owned(), theme::semibold(13.5), Color32::PLACEHOLDER);
    let (rect, resp) = ui.allocate_exact_size(vec2(text.size().x + 28.0, 34.0), Sense::click());
    let resp = named(resp, label).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
    let s = theme::anim_bool(ui.ctx(), resp.id.with("s"), selected, 0.2);
    use crate::preset::ChipStyle;
    let style = crate::preset::active().app.chip;
    let radius = (crate::preset::active().chrome.radius * 0.55).clamp(4.0, 17.0);
    let (on_fill, on_fg) = match style {
        ChipStyle::Accent => {
            let a = ui.ctx().data(|d| d.get_temp::<Color32>(egui::Id::unique("accent"))).unwrap_or(theme::RED);
            (a, if a.r() as u32 + a.g() as u32 + a.b() as u32 > 480 { Color32::from_gray(15) } else { Color32::WHITE })
        }
        _ => (Color32::from_gray(245), Color32::from_gray(15)),
    };
    let off_fill = if style == ChipStyle::Outline { theme::glass(0.04 * h) } else { theme::glass(0.09 + 0.06 * h) };
    let fill = theme::lerp_color(off_fill, on_fill, s);
    ui.painter().rect_filled(rect, radius, fill);
    if style == ChipStyle::Outline {
        ui.painter().rect_stroke(rect, radius, egui::Stroke::new(1.0, theme::glass(0.3 * (1.0 - s))), egui::StrokeKind::Inside);
    }
    let fg = theme::lerp_color(theme::TEXT, on_fg, s);
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
    ui.painter().rect_filled(rect, radius, theme::glass(0.07));
    let Some(t) = pick_thumb(thumbs, px.min(544)) else {
        icon_at(ui, rect.center(), icon::MUSIC, rect.width() * 0.3, theme::TEXT_FAINT);
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
    ui.painter().circle_filled(center, r, theme::glass(0.07));
    let px = fetch_px(px);
    let Some(t) = pick_thumb(thumbs, px.min(544)) else { return };
    let url = steady_url(ui.ctx(), &t.url, px);
    // While it loads, the loader wakes the UI once it's ready.
    let Ok(egui::load::TexturePoll::Ready { texture }) = egui::Image::new(url).load_for_size(ui.ctx(), Vec2::splat(r * 2.0)) else { return };
    let uv = t.square_uv();
    let (uc, uh) = (uv.center(), uv.width() / 2.0);
    let mut mesh = egui::Mesh::with_texture(texture.id);
    mesh.colored_vertex(center, tint);
    mesh.vertices[0].uv = uc;
    let n = 96;
    for i in 0..=n {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        let (s, c) = a.sin_cos();
        let (sr, cr) = (a - angle).sin_cos();
        mesh.vertices.push(egui::epaint::Vertex { pos: center + vec2(c, s) * r, uv: uc + vec2(cr * uh, sr * uv.height() / 2.0), color: tint });
        if i > 0 {
            mesh.add_triangle(0, i, i + 1);
        }
    }
    ui.painter().add(egui::Shape::mesh(mesh));
}

/// Album/playlist/artist/song card with hover zoom, scrim and a play button.
pub fn card(ui: &mut Ui, c: &Card, width: f32, liked: bool, acts: &mut Vec<Action>) {
    let height = width + 58.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let resp = named(resp, &c.title).on_hover_cursor(egui::CursorIcon::PointingHand);
    // contains_pointer, not hovered: the play button on top takes the hover, and the
    // card must stay "hovered" while the pointer is on it or the button would vanish.
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.contains_pointer(), 0.22);
    let img = Rect::from_min_size(rect.min, Vec2::splat(width));
    let radius = if c.round { width / 2.0 } else { theme::card_radius().min(width / 2.0) };
    paint_cover(ui, img, &c.thumbs, radius, 1.0 + 0.06 * h * theme::hover_k());
    if h > 0.0 && !c.round {
        ui.painter().rect_filled(img, radius, theme::shade(0.28 * h));
    }
    // Play button rises into the corner on hover; on the playing item it stays up
    // and shows pause.
    let active = active_state(ui, &c.target);
    let h = if active.is_some() { 1.0 } else { h };
    let mut play_hit = false;
    if h > 0.01 {
        let center = img.right_bottom() + vec2(-30.0, -30.0 + 8.0 * (1.0 - h));
        let prect = Rect::from_center_size(center, Vec2::splat(44.0));
        let p = ui.interact(prect, resp.id.with("play"), Sense::click());
        let playing = active == Some(true);
        let p = named(p, &format!("{} {}", if playing { "Pause" } else { "Play" }, c.title)).on_hover_cursor(egui::CursorIcon::PointingHand);
        let ph = theme::anim_bool(ui.ctx(), p.id.with("h"), p.hovered(), 0.12);
        ui.painter().circle_filled(center, 22.0 + 2.0 * ph, theme::shade(0.62 * h));
        play_pause_glyph(ui, center, 10.0, theme::with_alpha(Color32::WHITE, h), playing);
        play_hit = p.clicked();
    }
    let tcolor = theme::TEXT;
    let title = galley_wrapped(ui, &c.title, theme::semibold(14.5), tcolor, width, 2);
    let title_h = title.size().y;
    ui.painter().galley(pos2(rect.left(), img.bottom() + 10.0), title, tcolor);
    let sub_y = img.bottom() + 14.0 + title_h;
    linked_text(ui, pos2(rect.left(), sub_y), &c.subtitle, &c.links, theme::regular(13.0), theme::text_dim(), width - 36.0, acts);
    // On the subtitle line, whose width leaves room for it, so a two-line title never sits under it.
    let more_rect = Rect::from_center_size(pos2(rect.right() - 16.0, sub_y + 8.0), Vec2::splat(30.0));
    let more = more_button(ui, more_rect, resp.id.with("more"), "Options");
    egui::Popup::menu(&more).show(|ui| card_menu_contents(ui, c, liked, acts));

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

fn card_menu_contents(ui: &mut Ui, c: &Card, liked: bool, acts: &mut Vec<Action>) {
    match &c.target {
        Target::Song(t) => song_menu(ui, t, liked, None, acts),
        t => {
            ui.set_width(226.0);
            ui.add_space(3.0);
            if menu_action(ui, Some(icon::PLAY), "Play") {
                acts.push(Action::PlayCollection(t.clone(), false));
            }
            if menu_action(ui, Some(icon::SHUFFLE), "Shuffle play") {
                acts.push(Action::PlayCollection(t.clone(), true));
            }
            if menu_action(ui, None, "Open") {
                acts.push(Action::Open(t.clone()));
            }
            ui.separator();
            if menu_action(ui, None, "Copy link") {
                acts.push(Action::Share(t.clone()));
            }
            ui.add_space(3.0);
        }
    }
}

fn more_button(ui: &mut Ui, rect: Rect, id: egui::Id, label: &str) -> Response {
    let response = named(ui.interact(rect, id, Sense::click()), label)
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.hovered() || egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response)) {
        ui.painter().rect_filled(rect, 7.0, theme::glass(0.16));
    }
    let color = if response.hovered() { theme::TEXT } else { theme::text_dim() };
    icon_at(ui, rect.center(), icon::ELLIPSIS_VERTICAL, 18.0, color);
    response
}

fn menu_action(ui: &mut Ui, glyph: Option<char>, label: &str) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width() - 8.0, 32.0), Sense::click());
    let response = named(response, label).on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.hovered() {
        ui.painter().rect_filled(rect, 7.0, theme::glass(0.12));
    }
    let color = if response.hovered() { theme::TEXT } else { theme::text_dim() };
    if let Some(glyph) = glyph {
        icon_at(ui, pos2(rect.left() + 19.0, rect.center().y), glyph, 16.0, color);
    } else {
        ui.painter().text(pos2(rect.left() + 19.0, rect.center().y), Align2::CENTER_CENTER, "↗", theme::semibold(17.0), color);
    }
    ui.painter().text(pos2(rect.left() + 40.0, rect.center().y), Align2::LEFT_CENTER, label, theme::regular(14.0), theme::TEXT);
    if response.clicked() { ui.close(); }
    response.clicked()
}

/// `remove`: the song's row in the open playlist, when it's yours and the song can come out.
pub fn song_menu(ui: &mut Ui, t: &Track, liked: bool, remove: Option<usize>, acts: &mut Vec<Action>) {
    ui.set_width(226.0);
    ui.add_space(3.0);
    song_actions(ui, t, liked, remove, acts);
    ui.add_space(3.0);
}

/// The song menu's items, for menus that add their own (now playing's options).
pub fn song_actions(ui: &mut Ui, t: &Track, liked: bool, remove: Option<usize>, acts: &mut Vec<Action>) {
    if menu_action(ui, Some(icon::HEART), if liked { "Remove like" } else { "Like song" }) {
        acts.push(Action::Like(t.clone(), !liked));
    }
    if menu_action(ui, Some(icon::THUMBS_DOWN), "Dislike song") {
        acts.push(Action::Dislike(t.clone()));
    }
    if menu_action(ui, Some(icon::LIST_PLUS), "Add to playlist") {
        acts.push(Action::AddToPlaylist(vec![t.clone()]));
    }
    if let Some(i) = remove {
        if menu_action(ui, Some(icon::LIST_MINUS), "Remove from playlist") {
            acts.push(Action::RemoveFromPlaylist(vec![i]));
        }
    }
    if menu_action(ui, None, "Copy link") {
        acts.push(Action::Share(Target::Song(t.clone())));
    }
    match crate::downloads::path(&t.id) {
        Some(path) => {
            if menu_action(ui, Some(icon::FOLDER_OPEN), "Show in folder") {
                crate::downloads::show_file(&path);
            }
        }
        None => {
            if menu_action(ui, Some(icon::DOWNLOAD), "Download") {
                acts.push(Action::Download(t.clone()));
            }
        }
    }
    ui.separator();
    if menu_action(ui, Some(icon::SKIP_FORWARD), "Play next") {
        acts.push(Action::PlayNext(t.clone()));
    }
    if menu_action(ui, Some(icon::LIST_MUSIC), "Add to queue") {
        acts.push(Action::Enqueue(t.clone()));
    }
    if menu_action(ui, Some(icon::RADIO), "Start radio") {
        acts.push(Action::Radio(t.clone()));
    }
    if t.album.as_ref().and_then(|a| a.id.as_ref()).is_some() || t.first_artist_id().is_some() {
        ui.separator();
    }
    if let Some(id) = t.album.as_ref().and_then(|a| a.id.clone()) {
        if menu_action(ui, Some(icon::DISC_3), "Go to album") {
            acts.push(Action::Open(Target::Album(id)));
        }
    }
    if let Some(id) = t.first_artist_id() {
        if menu_action(ui, Some(icon::USER), "Go to artist") {
            acts.push(Action::Open(Target::Artist(id.to_owned())));
        }
    }
}

/// Soft drop shadow under a rounded rect (e.g. artwork).
pub fn soft_shadow(ui: &Ui, rect: Rect, radius: f32, blur: f32, alpha: f32) {
    let s = egui::epaint::Shadow { offset: [0, (blur * 0.35) as i8], blur: blur as u8, spread: 0, color: theme::shade(alpha) };
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

pub fn play_triangle(ui: &Ui, center: Pos2, r: f32, color: Color32) {
    let pts = vec![
        center + vec2(-r * 0.6, -r),
        center + vec2(r * 1.0, 0.0),
        center + vec2(-r * 0.6, r),
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
    pub current: bool,
    pub playing: bool,
    pub accent: Color32,
    pub time: f64,
    pub liked: bool,
    /// In a jam: who added the song.
    pub added_by: Option<String>,
    /// Choosing a match for the Spotify transfer: a click picks the song and its cover plays it.
    pub pick: bool,
    /// Selecting songs: a checkbox leads the row; `Some(true)` when this one is picked.
    pub selected: Option<bool>,
    /// The row's index in your own open playlist, so its menu can take it out.
    pub remove: Option<usize>,
}

const ROW_H: f32 = 56.0;

/// A long list of song rows: only the ones on screen are built; the rest is empty space
/// of the same height. `row(ui, i)` draws row `i`, under an id of its own so its hover
/// and menu state don't jump to another song as the list scrolls.
pub fn visible_rows(ui: &mut Ui, n: usize, mut row: impl FnMut(&mut Ui, usize)) {
    let h = ROW_H + ui.spacing().item_spacing.y;
    let top = ui.cursor().top();
    let clip = ui.clip_rect();
    let first = (((clip.top() - top) / h).floor().max(0.0) as usize).min(n);
    let last = ((((clip.bottom() - top) / h).ceil().max(0.0)) as usize + 1).clamp(first, n);
    ui.add_space(first as f32 * h);
    for i in first..last {
        ui.push_id(i, |ui| row(ui, i));
    }
    ui.add_space((n - last) as f32 * h);
}

/// Song row: cover (or index), title, artists, album, duration. Click plays.
pub fn track_row(ui: &mut Ui, t: &Track, o: &RowOpts, acts: &mut Vec<Action>) -> Response {
    let (rect, base) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return base;
    }
    // Picking: the cover (left 54 px) plays, the rest of the row picks.
    let hit_left = if o.pick && o.number.is_none() { rect.left() + 54.0 } else { rect.left() };
    let hit_rect = Rect::from_min_max(pos2(hit_left, rect.top()), pos2(rect.right() - 42.0, rect.bottom()));
    let resp = named(ui.interact(hit_rect, base.id.with("play"), Sense::click()), &t.title).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), base.id.with("h"), base.contains_pointer(), 0.15);
    if h > 0.0 || o.current {
        let a = if o.current { 0.07 + 0.05 * h } else { 0.07 * h };
        ui.painter().rect_filled(rect, 8.0, theme::glass(a));
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
    let mut x = rect.left() + 10.0;
    let cy = rect.center().y;
    if let Some(on) = o.selected {
        checkbox(ui, pos2(x + 9.0, cy), on);
        x += 30.0;
    }
    if let Some(n) = o.number {
        let slot = Rect::from_center_size(pos2(x + 12.0, cy), vec2(24.0, 24.0));
        if o.current {
            equalizer(ui, slot.shrink(4.0), o.time, o.playing, o.accent);
        } else if h > 0.5 {
            play_triangle(ui, slot.center(), 6.0, theme::TEXT);
        } else {
            ui.painter().text(slot.center(), Align2::CENTER_CENTER, n.to_string(), theme::regular(14.0), theme::text_dim());
        }
        x += 36.0;
    } else {
        let img = Rect::from_min_size(pos2(x, cy - 20.0), Vec2::splat(40.0));
        let tr = theme::thumb_radius();
        paint_cover(ui, img, &t.thumbs, tr, 1.0);
        if o.pick {
            let play = named(ui.interact(img, base.id.with("cover"), Sense::click()), "Play").on_hover_text("Play").on_hover_cursor(egui::CursorIcon::PointingHand);
            if play.clicked() {
                acts.push(Action::Play(vec![t.clone()], 0));
            }
        }
        if o.current {
            ui.painter().rect_filled(img, tr, theme::shade(0.45));
            equalizer(ui, img.shrink(10.0), o.time, o.playing, Color32::WHITE);
        } else if h > 0.0 {
            ui.painter().rect_filled(img, tr, theme::shade(0.5 * h));
            play_triangle(ui, img.center(), 7.0, theme::with_alpha(Color32::WHITE, h));
        }
        x += 54.0;
    }
    let right = rect.right() - 51.0;
    let dur_w = 52.0;
    let avail = right - dur_w - x;
    let wide = avail > 520.0;
    // Album rows (numbered) are just song name and plays; other rows are
    // title + artists, then album, then plays when there's room.
    let album_row = o.number.is_some();
    let (title_w, plays_x, album_x) = match (wide, o.show_album, t.plays) {
        (true, true, Some(_)) => (avail * 0.45, Some(x + avail * 0.82), Some(x + avail * 0.5)),
        (true, true, None) => (avail * 0.55, None, Some(x + avail * 0.6)),
        (true, false, Some(_)) => (avail * 0.78, Some(x + avail * 0.82), None),
        _ => (avail, None, None),
    };
    let plays = t.plays.map(|n| format!("{} plays", crate::backend::compact(n)));
    let title_col = if o.current { o.accent } else { theme::TEXT };
    let sub = match (album_row, &plays, plays_x) {
        (true, Some(p), None) => Some(p.clone()),
        (true, _, _) => None,
        (false, Some(p), None) => Some(format!("{} \u{2022} {p}", t.artist_line())),
        (false, _, _) => Some(t.artist_line()),
    };
    let sub = match (sub, &o.added_by) {
        (Some(s), Some(by)) if !s.is_empty() => Some(format!("{s} \u{2022} Added by {by}")),
        (_, Some(by)) => Some(format!("Added by {by}")),
        (sub, _) => sub,
    };
    let title_y = if sub.is_some() { cy - 18.0 } else { cy - 9.0 };
    text_at(ui, pos2(x, title_y), &t.title, theme::semibold(14.5), title_col, title_w);
    if let Some(sub) = &sub {
        linked_text(ui, pos2(x, cy + 2.0), sub, &t.artists, theme::regular(13.0), theme::text_dim(), title_w, acts);
    }
    if let (Some(px), Some(p)) = (plays_x, &plays) {
        ui.painter().text(pos2(px, cy), Align2::LEFT_CENTER, p, theme::regular(13.5), theme::text_dim());
    }
    if let (Some(ax), Some(album)) = (album_x, &t.album) {
        let w = plays_x.unwrap_or(right - dur_w) - ax - 10.0;
        let g = galley(ui, &album.name, theme::regular(13.5), theme::text_dim(), w);
        let ar = Rect::from_min_size(pos2(ax, cy - 9.0), g.size());
        ui.painter().galley(ar.min, g, theme::text_dim());
        // Album name opens the album, like the artist names.
        if let Some(id) = &album.id {
            let r = named(ui.interact(ar, resp.id.with("album"), Sense::click()), &album.name);
            let r = r.on_hover_cursor(egui::CursorIcon::PointingHand);
            if r.hovered() {
                ui.painter().hline(ar.x_range(), ar.bottom() - 1.0, egui::Stroke::new(1.0, theme::text_dim()));
            }
            if r.clicked() {
                acts.push(Action::Open(Target::Album(id.clone())));
            }
        }
    }
    if let Some(d) = t.duration {
        ui.painter().text(pos2(right, cy), Align2::RIGHT_CENTER, super::fmt_time(d), theme::regular(13.0), theme::text_dim());
    }
    let more_rect = Rect::from_center_size(pos2(rect.right() - 20.0, cy), Vec2::splat(30.0));
    let more = more_button(ui, more_rect, base.id.with("more"), "Song options");
    egui::Popup::menu(&more).show(|ui| song_menu(ui, t, o.liked, o.remove, acts));
    // Right-click anywhere on the row opens the same menu.
    egui::Popup::context_menu(&resp).show(|ui| song_menu(ui, t, o.liked, o.remove, acts));
    resp
}

/// An 18 px tick box centred on `c`, white like a primary pill when ticked.
fn checkbox(ui: &Ui, c: Pos2, on: bool) {
    let r = Rect::from_center_size(c, Vec2::splat(18.0));
    if on {
        ui.painter().rect_filled(r, 5.0, theme::TEXT);
        icon_at(ui, c, icon::CHECK, 13.0, Color32::from_gray(12));
    } else {
        ui.painter().rect_stroke(r, 5.0, egui::Stroke::new(1.5, theme::text_dim()), egui::StrokeKind::Inside);
    }
}

/// Section header with optional strapline/avatar and ‹ › scroll arrows.
/// Returns (scroll left, scroll right) clicks.
pub fn shelf_header(ui: &mut Ui, title: &str, strap: Option<&str>, strap_thumb: Option<&str>, arrows: bool) -> (bool, bool) {
    let mut res = (false, false);
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
                ui.label(egui::RichText::new(s.to_uppercase()).font(theme::semibold(12.0)).color(theme::text_dim()));
            }
            ui.label(egui::RichText::new(title).font(theme::bold(24.0)).color(theme::TEXT));
        });
        if arrows {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                res.1 = icon_button(ui, icon::CHEVRON_RIGHT, 18.0, &format!("Scroll {title} right"), false).clicked();
                res.0 = icon_button(ui, icon::CHEVRON_LEFT, 18.0, &format!("Scroll {title} left"), false).clicked();
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

/// Horizontal scroller that animates between aligned pages of cards or song columns.
pub fn h_scroll<R>(ui: &mut Ui, id: &str, nudge: i32, count: usize, item_width: f32, gap: f32, add: impl FnOnce(&mut Ui) -> R) -> (R, bool) {
    let sid = egui::Id::unique(("hscroll", id));
    let anim_id = sid.with("anim");
    // ScrollArea hashes its id_salt once more. Read that same ID, or left clicks
    // always appear to start from zero.
    let state_id = ui.make_persistent_id(egui::IdSalt::new(sid));
    let current = egui::scroll_area::State::load(ui.ctx(), state_id).map_or(0.0, |s| s.clamped_offset().x);
    let target_id = sid.with("target");
    let viewport_id = sid.with("viewport");
    let viewport = ui.ctx().data(|d| d.get_temp::<f32>(viewport_id)).unwrap_or_else(|| ui.available_width());
    let step = item_width + gap;
    let visible = ((viewport + gap) / step).floor().max(1.0) as usize;
    let starts = shelf_page_starts(count, visible);
    let last_start = *starts.last().unwrap();
    let mut reached_end = false;
    let mut target: Option<f32> = ui.ctx().data(|d| d.get_temp(target_id));
    if nudge != 0 {
        let t = shelf_page_target(&starts, step, target.unwrap_or(current), nudge);
        reached_end = nudge > 0 && count > visible && (t - last_start as f32 * step).abs() < 0.5;
        // Snap the animator to the real position, then glide to the target.
        ui.ctx().animate_value_with_time(anim_id, current, 0.0);
        target = Some(t);
        ui.ctx().data_mut(|d| d.insert_temp(target_id, t));
    }
    let mut area = egui::ScrollArea::horizontal().id_salt(sid).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden);
    if let Some(t) = target {
        let v = ui.ctx().animate_value_with_time(anim_id, t, 0.35 * theme::SPEED);
        area = area.horizontal_scroll_offset(v);
        if (v - t).abs() < 0.5 {
            ui.ctx().data_mut(|d| d.remove::<f32>(target_id));
        }
    }
    // Give the last page enough trailing room to land exactly on its first item.
    let content_width = (last_start as f32 * step + viewport).max(count as f32 * step - gap);
    let out = area.show(ui, |ui| ui.horizontal_top(|ui| {
        ui.set_min_width(content_width);
        add(ui)
    }).inner);
    ui.ctx().data_mut(|d| d.insert_temp(viewport_id, out.inner_rect.width()));
    (out.inner, reached_end)
}

#[cfg(test)]
mod shelf_scroll_tests {
    use super::{shelf_page_starts, shelf_page_target};

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

/// The skeleton's light band sweeping across `r`, for anything still loading.
pub fn shimmer_band(ui: &Ui, r: Rect, radius: f32, time: f64) {
    let phase = ((time * 0.8) % 1.6) as f32 - 0.3;
    let x = r.left() + (r.width() + 400.0) * phase - 200.0;
    let band = Rect::from_min_max(pos2((x - 120.0).max(r.left()), r.top()), pos2((x + 120.0).min(r.right()), r.bottom()));
    if band.width() > 0.0 {
        ui.painter().rect_filled(band, radius, theme::glass(0.04));
    }
}

/// Paint a remote image into `rect`: the skeleton's shimmer while it downloads (not
/// egui's spinner), then a quick fade-in. One already loaded just shows.
pub fn paint_remote(ui: &Ui, url: &str, rect: Rect, radius: f32, uv: Rect, tint: Color32) {
    const FADE: f64 = 0.22;
    let ctx = ui.ctx();
    let now = ctx.input(|i| i.time);
    let id = egui::Id::unique(("img-fade", url));
    match egui::Image::new(url).load_for_size(ctx, rect.size()) {
        Ok(egui::load::TexturePoll::Ready { texture }) => {
            // Fade only what was seen loading; Some(None) = waiting, Some(Some(t)) = fading since t.
            let a = match ctx.data(|d| d.get_temp::<Option<f64>>(id)) {
                Some(Some(t0)) if now - t0 < FADE => ((now - t0) / FADE) as f32,
                Some(None) => {
                    ctx.data_mut(|d| d.insert_temp(id, Some(now)));
                    0.0
                }
                _ => 1.0,
            };
            if a < 1.0 {
                ctx.request_repaint();
            }
            egui::Image::from_texture(texture).uv(uv).corner_radius(CornerRadius::same(radius as u8)).tint(tint.gamma_multiply(theme::ease_out(a))).paint_at(ui, rect);
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

/// Song rows still on their way, at the end of a list that's coming in.
pub fn skeleton_rows(ui: &mut Ui, time: f64, rows: usize) {
    for _ in 0..rows {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
        if !ui.is_rect_visible(r) {
            continue;
        }
        let cy = r.center().y;
        for (b, radius) in [
            (Rect::from_min_size(pos2(r.left() + 10.0, cy - 20.0), Vec2::splat(40.0)), theme::thumb_radius()),
            (Rect::from_min_size(pos2(r.left() + 64.0, cy - 13.0), vec2((r.width() * 0.3).min(260.0), 11.0)), 4.0),
            (Rect::from_min_size(pos2(r.left() + 64.0, cy + 4.0), vec2((r.width() * 0.18).min(160.0), 9.0)), 4.0),
        ] {
            ui.painter().rect_filled(b, radius, theme::glass(0.06));
            shimmer_band(ui, b, radius, time);
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(66));
    }
}

/// Shimmering placeholder blocks while a page loads.
pub fn skeleton(ui: &mut Ui, time: f64, rows: usize, card: f32) {
    let shimmer = |ui: &Ui, r: Rect, radius: f32| {
        ui.painter().rect_filled(r, radius, theme::glass(0.06));
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
