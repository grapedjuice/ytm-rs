//! Reusable, hand-painted widgets. Everything custom-painted gets an accessible name
//! (`named`), so screen readers and UI Automation can see and invoke it.

use egui::{Align2, Color32, CornerRadius, Pos2, Rect, Response, Sense, Ui, Vec2, pos2, vec2};

use super::Action;
use crate::model::{Card, Target, Thumb, Track, pick_thumb, sized};
use crate::theme::{self, icon};

pub fn named(resp: Response, name: &str) -> Response {
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
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

pub fn icon_at(ui: &Ui, center: Pos2, glyph: char, size: f32, color: Color32) {
    ui.painter().text(center, Align2::CENTER_CENTER, glyph, theme::icon_font(size), color);
}

/// Circular icon button with an animated hover halo.
pub fn icon_button(ui: &mut Ui, glyph: char, size: f32, name: &str, active: bool) -> Response {
    let d = size + 16.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(d), Sense::click());
    let resp = named(resp, name).on_hover_text(name);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
    if h > 0.0 {
        ui.painter().circle_filled(rect.center(), d / 2.0, theme::glass(0.10 * h));
    }
    let color = if active { theme::TEXT } else { theme::lerp_color(theme::TEXT_DIM, theme::TEXT, h) };
    let press = if resp.is_pointer_button_down_on() { 0.9 } else { 1.0 };
    icon_at(ui, rect.center(), glyph, size * press, color);
    resp
}

/// Rounded pill button. `primary` = white with dark text (YouTube Music's Play button).
pub fn pill(ui: &mut Ui, label: &str, glyph: Option<char>, primary: bool) -> Response {
    let font = theme::semibold(14.0);
    let text_w = ui.fonts_mut(|f| f.layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE).size().x);
    let icon_w = if glyph.is_some() { 24.0 } else { 0.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(text_w + icon_w + 36.0, 38.0), Sense::click());
    let resp = named(resp, label);
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
    ui.painter().text(pos2(x, r.center().y), Align2::LEFT_CENTER, label, font, fg);
    resp
}

/// Selectable chip (home moods, library tabs).
pub fn chip(ui: &mut Ui, label: &str, selected: bool) -> Response {
    let font = theme::semibold(13.5);
    let w = ui.fonts_mut(|f| f.layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE).size().x) + 28.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 34.0), Sense::click());
    let resp = named(resp, label);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
    let s = theme::anim_bool(ui.ctx(), resp.id.with("s"), selected, 0.2);
    let fill = theme::lerp_color(theme::glass(0.09 + 0.06 * h), Color32::from_gray(245), s);
    ui.painter().rect_filled(rect, 10.0, fill);
    let fg = theme::lerp_color(theme::TEXT, Color32::from_gray(15), s);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, label, font, fg);
    resp
}

/// Paint a cover image (square-cropped, CDN-resized) with a placeholder underneath.
pub fn paint_cover(ui: &Ui, rect: Rect, thumbs: &[Thumb], radius: f32, zoom: f32) {
    if !ui.is_rect_visible(rect) {
        return;
    }
    ui.painter().rect_filled(rect, radius, theme::glass(0.07));
    let ppp = ui.ctx().pixels_per_point();
    let px = (rect.width() * ppp) as u32;
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
    let url = sized(&t.url, (px.max(60) as f32 * 1.0) as u32);
    egui::Image::new(url).uv(uv).corner_radius(CornerRadius::same(radius as u8)).paint_at(ui, rect);
}

/// Album/playlist/artist/song card with hover zoom, scrim and a play button.
pub fn card(ui: &mut Ui, c: &Card, width: f32, acts: &mut Vec<Action>) {
    let height = width + 58.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let resp = named(resp, &c.title);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.22);
    let img = Rect::from_min_size(rect.min, Vec2::splat(width));
    let radius = if c.round { width / 2.0 } else { 8.0 };
    paint_cover(ui, img, &c.thumbs, radius, 1.0 + 0.06 * h);
    if h > 0.0 && !c.round {
        ui.painter().rect_filled(img, radius, theme::shade(0.28 * h));
    }
    // Play button rises into the corner on hover.
    let mut play_hit = false;
    if h > 0.01 {
        let center = img.right_bottom() + vec2(-30.0, -30.0 + 8.0 * (1.0 - h));
        let prect = Rect::from_center_size(center, Vec2::splat(44.0));
        let p = ui.interact(prect, resp.id.with("play"), Sense::click());
        let p = named(p, &format!("Play {}", c.title));
        let ph = theme::anim_bool(ui.ctx(), p.id.with("h"), p.hovered(), 0.12);
        ui.painter().circle_filled(center, 22.0 + 2.0 * ph, theme::shade(0.62 * h));
        play_triangle(ui, center, 10.0, theme::with_alpha(Color32::WHITE, h));
        play_hit = p.clicked();
    }
    let tcolor = theme::TEXT;
    let title = galley_wrapped(ui, &c.title, theme::semibold(14.5), tcolor, width, 2);
    let title_h = title.size().y;
    ui.painter().galley(pos2(rect.left(), img.bottom() + 10.0), title, tcolor);
    text_at(ui, pos2(rect.left(), img.bottom() + 14.0 + title_h), &c.subtitle, theme::regular(13.0), theme::TEXT_DIM, width);

    if play_hit {
        match &c.target {
            Target::Song(t) => acts.push(Action::Radio(t.clone())),
            t => acts.push(Action::PlayCollection(t.clone(), false)),
        }
    } else if resp.clicked() {
        acts.push(Action::Open(c.target.clone()));
    }
    card_menu(&resp, c, acts);
}

fn card_menu(resp: &Response, c: &Card, acts: &mut Vec<Action>) {
    resp.context_menu(|ui| match &c.target {
        Target::Song(t) => song_menu(ui, t, acts),
        t => {
            if ui.button("Play").clicked() {
                acts.push(Action::PlayCollection(t.clone(), false));
            }
            if ui.button("Shuffle play").clicked() {
                acts.push(Action::PlayCollection(t.clone(), true));
            }
            if ui.button("Open").clicked() {
                acts.push(Action::Open(t.clone()));
            }
        }
    });
}

pub fn song_menu(ui: &mut Ui, t: &Track, acts: &mut Vec<Action>) {
    if ui.button("Play next").clicked() {
        acts.push(Action::PlayNext(t.clone()));
    }
    if ui.button("Add to queue").clicked() {
        acts.push(Action::Enqueue(t.clone()));
    }
    if ui.button("Start radio").clicked() {
        acts.push(Action::Radio(t.clone()));
    }
    if t.album.as_ref().and_then(|a| a.id.as_ref()).is_some() || t.first_artist_id().is_some() {
        ui.separator();
    }
    if let Some(id) = t.album.as_ref().and_then(|a| a.id.clone()) {
        if ui.button("Go to album").clicked() {
            acts.push(Action::Open(Target::Album(id)));
        }
    }
    if let Some(id) = t.first_artist_id() {
        if ui.button("Go to artist").clicked() {
            acts.push(Action::Open(Target::Artist(id.to_owned())));
        }
    }
}

/// Soft drop shadow under a rounded rect (e.g. artwork).
pub fn soft_shadow(ui: &Ui, rect: Rect, radius: f32, blur: f32, alpha: f32) {
    let s = egui::epaint::Shadow { offset: [0, (blur * 0.35) as i8], blur: blur as u8, spread: 0, color: theme::shade(alpha) };
    ui.painter().add(s.as_shape(rect, CornerRadius::same(radius as u8)));
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

/// Bouncing bars shown on the playing row. Driven by time plus the audio level.
pub fn equalizer(ui: &Ui, rect: Rect, time: f64, level: f32, playing: bool, color: Color32) {
    let n = 3;
    let w = rect.width() / (n as f32 * 1.8);
    for i in 0..n {
        let phase = time as f32 * (5.0 + i as f32 * 1.7) + i as f32 * 1.3;
        let amp = if playing { 0.35 + 0.65 * (0.5 + 0.5 * phase.sin()) * (0.5 + level.min(0.4) * 1.25) } else { 0.25 };
        let hgt = rect.height() * amp.clamp(0.15, 1.0);
        let x = rect.left() + i as f32 * w * 1.8 + w / 2.0;
        let bar = Rect::from_min_max(pos2(x - w / 2.0, rect.bottom() - hgt), pos2(x + w / 2.0, rect.bottom()));
        ui.painter().rect_filled(bar, 1.0, color);
    }
}

pub struct RowOpts {
    pub number: Option<usize>,
    pub show_album: bool,
    pub current: bool,
    pub playing: bool,
    pub accent: Color32,
    pub time: f64,
    pub level: f32,
}

/// Song row: cover (or index), title, artists, album, duration. Click plays.
pub fn track_row(ui: &mut Ui, t: &Track, o: &RowOpts) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let resp = named(resp, &t.title);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
    if h > 0.0 || o.current {
        let a = if o.current { 0.07 + 0.05 * h } else { 0.07 * h };
        ui.painter().rect_filled(rect, 8.0, theme::glass(a));
    }
    let mut x = rect.left() + 10.0;
    let cy = rect.center().y;
    if let Some(n) = o.number {
        let slot = Rect::from_center_size(pos2(x + 12.0, cy), vec2(24.0, 24.0));
        if o.current {
            equalizer(ui, slot.shrink(5.0), o.time, o.level, o.playing, o.accent);
        } else if h > 0.5 {
            play_triangle(ui, slot.center(), 6.0, theme::TEXT);
        } else {
            ui.painter().text(slot.center(), Align2::CENTER_CENTER, n.to_string(), theme::regular(14.0), theme::TEXT_DIM);
        }
        x += 36.0;
    } else {
        let img = Rect::from_min_size(pos2(x, cy - 20.0), Vec2::splat(40.0));
        paint_cover(ui, img, &t.thumbs, 5.0, 1.0);
        if o.current {
            ui.painter().rect_filled(img, 5.0, theme::shade(0.55));
            equalizer(ui, img.shrink(11.0), o.time, o.level, o.playing, Color32::WHITE);
        } else if h > 0.0 {
            ui.painter().rect_filled(img, 5.0, theme::shade(0.5 * h));
            play_triangle(ui, img.center(), 7.0, theme::with_alpha(Color32::WHITE, h));
        }
        x += 54.0;
    }
    let right = rect.right() - 14.0;
    let dur_w = 52.0;
    let avail = right - dur_w - x;
    let (title_w, album_x) = if o.show_album && avail > 520.0 { (avail * 0.55, Some(x + avail * 0.6)) } else { (avail, None) };
    let title_col = if o.current { o.accent } else { theme::TEXT };
    text_at(ui, pos2(x, cy - 18.0), &t.title, theme::semibold(14.5), title_col, title_w);
    text_at(ui, pos2(x, cy + 2.0), &t.artist_line(), theme::regular(13.0), theme::TEXT_DIM, title_w);
    if let (Some(ax), Some(album)) = (album_x, &t.album) {
        let w = right - dur_w - ax - 10.0;
        ui.painter().galley(
            pos2(ax, cy - 9.0),
            galley(ui, &album.name, theme::regular(13.5), theme::TEXT_DIM, w),
            theme::TEXT_DIM,
        );
    }
    if let Some(d) = t.duration {
        ui.painter().text(pos2(right, cy), Align2::RIGHT_CENTER, super::fmt_time(d), theme::regular(13.0), theme::TEXT_DIM);
    }
    resp
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
            egui::Image::new(url.to_owned()).corner_radius(20).paint_at(ui, r);
            ui.add_space(4.0);
        }
        ui.vertical(|ui| {
            if let Some(s) = strap {
                ui.label(egui::RichText::new(s.to_uppercase()).font(theme::semibold(12.0)).color(theme::TEXT_DIM));
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

/// Horizontal scroller that animates to a new offset when the arrows are used.
pub fn h_scroll<R>(ui: &mut Ui, id: &str, nudge: i32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let sid = egui::Id::new(("hscroll", id));
    let anim_id = sid.with("anim");
    let current = egui::scroll_area::State::load(ui.ctx(), ui.make_persistent_id(sid)).map_or(0.0, |s| s.offset.x);
    let target_id = sid.with("target");
    let mut target: Option<f32> = ui.ctx().data(|d| d.get_temp(target_id));
    if nudge != 0 {
        let step = ui.available_width() * 0.8 * nudge as f32;
        let t = (current + step).max(0.0);
        // Snap the animator to the real position, then glide to the target.
        ui.ctx().animate_value_with_time(anim_id, current, 0.0);
        target = Some(t);
        ui.ctx().data_mut(|d| d.insert_temp(target_id, t));
    }
    let mut area = egui::ScrollArea::horizontal().id_salt(sid).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden);
    if let Some(t) = target {
        let v = ui.ctx().animate_value_with_time(anim_id, t, 0.35);
        area = area.horizontal_scroll_offset(v);
        if (v - t).abs() < 0.5 {
            ui.ctx().data_mut(|d| d.remove::<f32>(target_id));
        }
    }
    area.show(ui, |ui| ui.horizontal_top(add).inner).inner
}

/// Shimmering placeholder blocks while a page loads.
pub fn skeleton(ui: &mut Ui, time: f64, rows: usize, card: f32) {
    let shimmer = |ui: &Ui, r: Rect, radius: f32| {
        let phase = ((time * 0.8) % 1.6) as f32 - 0.3;
        let x = r.left() + (r.width() + 400.0) * phase - 200.0;
        ui.painter().rect_filled(r, radius, theme::glass(0.06));
        let band = Rect::from_min_max(pos2((x - 120.0).max(r.left()), r.top()), pos2((x + 120.0).min(r.right()), r.bottom()));
        if band.width() > 0.0 {
            ui.painter().rect_filled(band, radius, theme::glass(0.04));
        }
    };
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
    ui.ctx().request_repaint();
}
