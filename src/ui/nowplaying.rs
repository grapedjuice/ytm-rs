//! Now-playing overlay: big art + synced lyrics / up next, over the full-strength
//! animated background. Lyrics follow better-lyrics' presentation: the active line
//! fills word by word, others dim with distance, the view glides to keep the active
//! line in place, and instrumental breaks show breathing dots.

use egui::{Align2, Color32, Pos2, Rect, Sense, Ui, Vec2, pos2, vec2};

use super::widgets::{self, icon_button, named, paint_cover, text_at};
use super::{Action, App, NpTab};
use crate::lyrics::{Line, Lyrics, Sync};
use crate::shader::Params;
use crate::theme::{self, icon};

pub fn overlay(app: &mut App, ctx: &egui::Context, now: f64) {
    let t = theme::anim_bool(ctx, "np-open", app.np_open && app.current().is_some(), 0.42);
    if t <= 0.001 {
        return;
    }
    let screen = ctx.content_rect();
    let full = if app.fullscreen { screen } else { Rect::from_min_max(screen.min, pos2(screen.right(), screen.bottom() - 86.0)) };
    let rect = full.translate(vec2(0.0, full.height() * (1.0 - t)));
    egui::Area::new("now-playing".into())
        .order(egui::Order::Middle)
        .fixed_pos(rect.min)
        .show(ctx, |ui| {
            // Swallow input so the page underneath doesn't react.
            ui.allocate_rect(rect, Sense::click_and_drag());
            let painter = ui.painter_at(rect);
            if let Some(bg) = app.bg.as_ref().filter(|_| !app.default_theme) {
                let blend = theme::ease_in_out(((now - app.art_changed) / 1.4) as f32);
                let level = if app.reactive_bg && app.playing() { app.audio.status.level() } else { 0.0 };
                bg.paint(
                    &painter,
                    rect,
                    Params { time: app.bg_time as f32, blend, intensity: 1.0, saturation: 1.5, dim: 0.85, scale: 1.2 + level.min(0.5) * 0.12 },
                );
            } else {
                painter.rect_filled(rect, 0.0, theme::lerp_color(theme::BG, app.accent_color(), 0.25));
            }
            super::shell::vgradient(ui, rect, theme::shade(0.18), theme::shade(0.42));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(32.0, 20.0))));
            body(app, &mut child, now);
        });
}

fn body(app: &mut App, ui: &mut Ui, now: f64) {
    let Some(track) = app.current().cloned() else { return };
    let area = ui.max_rect();
    // Header
    let header = Rect::from_min_size(area.min, vec2(area.width(), 44.0));
    let mut hui = ui.new_child(egui::UiBuilder::new().max_rect(header).layout(egui::Layout::left_to_right(egui::Align::Center)));
    if icon_button(&mut hui, icon::CHEVRON_DOWN, 22.0, "Close now playing", false).clicked() {
        app.np_open = false;
        if app.fullscreen {
            app.set_fullscreen(ui.ctx(), false);
        }
    }
    hui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let (g, name) = if app.fullscreen { (icon::SHRINK, "Exit full screen") } else { (icon::EXPAND, "Full screen") };
        if icon_button(ui, g, 18.0, name, false).clicked() {
            let fs = !app.fullscreen;
            app.set_fullscreen(ui.ctx(), fs);
        }
        ui.add_space(8.0);
        let more = icon_button(ui, icon::ELLIPSIS_VERTICAL, 20.0, "Song options", false);
        let liked = app.liked.contains(&track.id);
        egui::Popup::menu(&more).show(|ui| widgets::song_menu(ui, &track, liked, &mut app.actions));
        ui.add_space(8.0);
        if widgets::chip(ui, "Up next", app.np_tab == NpTab::UpNext).clicked() {
            app.np_tab = NpTab::UpNext;
        }
        if widgets::chip(ui, "Lyrics", app.np_tab == NpTab::Lyrics).clicked() {
            app.np_tab = NpTab::Lyrics;
        }
    });

    let content = Rect::from_min_max(pos2(area.left(), header.bottom() + 12.0), area.max);
    let lyrics_only = content.width() < 860.0;
    let right = if lyrics_only {
        content
    } else {
        // Left column: artwork and track info.
        let col = Rect::from_min_size(content.min, vec2(content.width() * 0.42, content.height()));
        let side = (col.width() * 0.82).min(col.height() - 130.0).max(160.0);
        let art = Rect::from_center_size(pos2(col.center().x, col.top() + side / 2.0 + (col.height() - side - 110.0).max(0.0) * 0.35), Vec2::splat(side));
        widgets::soft_shadow(ui, art, 12.0, 60.0, 0.55);
        paint_cover(ui, art, &track.thumbs, 12.0, 1.0);
        let cover = named(ui.interact(art, ui.id().with(("cover-playback", &track.id)), Sense::click()), if app.playing() { "Pause" } else { "Play" })
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let hover = theme::anim_bool(ui.ctx(), ("cover-playback-hover", &track.id), cover.hovered(), 0.16);
        if hover > 0.0 {
            ui.painter().rect_filled(art, 12.0, theme::shade(0.28 * hover));
            ui.painter().circle_filled(art.center(), 38.0, theme::shade(0.58 * hover));
            widgets::icon_at(ui, art.center(), if app.playing() || app.buffering { icon::PAUSE } else { icon::PLAY }, 37.0, theme::with_alpha(Color32::WHITE, hover));
        }
        if cover.clicked() { app.actions.push(Action::TogglePlay); }
        let w = side;
        let title_y = art.bottom() + 22.0;
        text_at(ui, pos2(art.left(), title_y), &track.title, theme::bold(28.0), Color32::WHITE, w);
        // Each artist opens their own page (features included).
        let line = track.artist_line();
        widgets::linked_text(ui, pos2(art.left(), title_y + 40.0), &line, &track.artists, theme::regular(17.0), theme::TEXT_DIM, w, &mut app.actions);
        Rect::from_min_max(pos2(col.right() + 28.0, content.top()), content.max)
    };
    let mut rui = ui.new_child(egui::UiBuilder::new().max_rect(right));
    match app.np_tab {
        NpTab::Lyrics => lyrics_panel(app, &mut rui, right, now, lyrics_only),
        NpTab::UpNext => super::shell::queue_list(app, &mut rui, "np"),
    }
}

// ------------------------------------------------------------------ lyrics

enum Item {
    Line(usize),
    /// Instrumental break between `start` and `end` ms.
    Gap { start: u32, end: u32 },
}

fn lyrics_panel(app: &mut App, ui: &mut Ui, rect: Rect, now: f64, centered: bool) {
    let Some((_, lyrics)) = &app.lyrics else {
        ui.add_space(rect.height() * 0.35);
        ui.vertical_centered(|ui| {
            ui.spinner();
            ui.label(egui::RichText::new("Loading lyrics…").color(theme::TEXT_DIM));
        });
        return;
    };
    let Some(lyrics) = lyrics.clone() else {
        ui.add_space(rect.height() * 0.4);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("No lyrics found for this song").font(theme::semibold(20.0)).color(theme::TEXT_DIM));
        });
        return;
    };
    if lyrics.sync == Sync::None {
        egui::ScrollArea::vertical().id_salt("plain-lyrics").auto_shrink(false).show(ui, |ui| {
            ui.label(egui::RichText::new("These lyrics aren't synced").font(theme::regular(13.0)).color(theme::TEXT_FAINT));
            ui.add_space(14.0);
            for l in &lyrics.lines {
                ui.label(egui::RichText::new(&l.text).font(theme::semibold(22.0)).color(theme::TEXT));
            }
            ui.add_space(20.0);
            ui.label(egui::RichText::new(format!("Lyrics from {}", lyrics.source)).font(theme::regular(12.0)).color(theme::TEXT_FAINT));
        });
        return;
    }
    synced(app, ui, rect, &lyrics, now, centered);
}

fn synced(app: &mut App, ui: &mut Ui, rect: Rect, lyrics: &Lyrics, now: f64, centered: bool) {
    let pos_ms = app.audio.status.position_smooth().as_millis() as u32 + 60;
    let width = (rect.width() - 40.0).max(200.0);
    let size = (width / 20.0).clamp(26.0, if centered { 46.0 } else { 36.0 });

    // Items: lines plus breathing gaps for long instrumental breaks.
    let mut items = Vec::new();
    let first = lyrics.lines.iter().find(|l| !l.background).map_or(0, |l| l.start);
    if first > 4500 {
        items.push(Item::Gap { start: 0, end: first });
    }
    let mut last_end = 0;
    for (i, l) in lyrics.lines.iter().enumerate() {
        if !l.background && i > 0 && l.start > last_end + 5000 {
            items.push(Item::Gap { start: last_end, end: l.start });
        }
        items.push(Item::Line(i));
        if !l.background {
            last_end = last_end.max(l.end);
        }
    }

    // Active item: latest one that has started.
    let started = |it: &Item| match it {
        Item::Line(i) => lyrics.lines[*i].start,
        Item::Gap { start, .. } => *start,
    };
    let active = items.iter().rposition(|it| started(it) <= pos_ms && !matches!(it, Item::Line(i) if lyrics.lines[*i].background));

    // Layout pass.
    let gap = size * 0.6;
    let mut ys = Vec::with_capacity(items.len());
    let mut galleys = Vec::with_capacity(items.len());
    let mut y = 0.0;
    for it in &items {
        ys.push(y);
        match it {
            Item::Line(i) => {
                let l = &lyrics.lines[*i];
                let font = if l.background { theme::semibold(size * 0.62) } else { theme::bold(size) };
                let text = display_text(l);
                let g = ui.fonts_mut(|f| f.layout(text, font, Color32::WHITE, width));
                y += g.size().y + if l.background { gap * 0.5 } else { gap };
                galleys.push(Some(g));
            }
            Item::Gap { .. } => {
                y += size * 1.3;
                galleys.push(None);
            }
        }
    }

    // Smooth follow: spring the scroll toward the active item (unless the user scrolled).
    let anchor = rect.top() + rect.height() * if centered { 0.42 } else { 0.32 };
    let target = active.map_or(0.0, |a| ys[a]) + size * 0.5;
    let sid = egui::Id::new("lyrics-scroll");
    let dt = ui.input(|i| i.stable_dt).min(0.1);
    let mut scroll: f32 = ui.data(|d| d.get_temp(sid)).unwrap_or(target);
    let hovered = ui.rect_contains_pointer(rect);
    let wheel = if hovered { ui.input(|i| i.smooth_scroll_delta.y) } else { 0.0 };
    if wheel != 0.0 {
        scroll -= wheel;
        app.lyrics_user_scroll = now;
    } else if now - app.lyrics_user_scroll > 3.0 {
        scroll += (target - scroll) * (1.0 - (-dt * 7.0).exp());
    }
    ui.data_mut(|d| d.insert_temp(sid, scroll));
    let origin_y = anchor - scroll;

    let painter = ui.painter_at(rect);
    let fade = |yy: f32| {
        // Fade lines out toward the top/bottom edges.
        let t = ((yy - rect.top()) / 90.0).min((rect.bottom() - yy) / 120.0);
        t.clamp(0.0, 1.0)
    };
    for (k, it) in items.iter().enumerate() {
        let top = origin_y + ys[k];
        let is_active = Some(k) == active;
        match it {
            Item::Gap { start, end } => {
                let a = theme::anim_bool(ui.ctx(), ("gap", k), is_active && pos_ms < *end, 0.35);
                if a <= 0.01 {
                    continue;
                }
                let p = ((pos_ms.saturating_sub(*start)) as f32 / (end - start).max(1) as f32).clamp(0.0, 1.0);
                let breath = 1.0 + 0.12 * (now as f32 * 3.0).sin();
                let x0 = if centered { rect.center().x - 30.0 } else { rect.left() + 26.0 };
                for d in 0..3 {
                    let fill = ((p * 3.0) - d as f32).clamp(0.0, 1.0);
                    let c = pos2(x0 + d as f32 * 22.0, top + size * 0.5);
                    painter.circle_filled(c, size * 0.17 * breath * a, theme::with_alpha(Color32::WHITE, a * (0.3 + 0.7 * fill)));
                }
            }
            Item::Line(i) => {
                let l = &lyrics.lines[*i];
                let Some(g) = galleys[k].clone() else { continue };
                let x = if centered {
                    rect.center().x - g.size().x / 2.0
                } else if l.alt_singer {
                    rect.right() - 20.0 - g.size().x
                } else {
                    rect.left() + 20.0
                };
                let pos = Pos2::new(x, top);
                let line_rect = Rect::from_min_size(pos, g.size());
                // Where the glyphs actually are: CJK fallback glyphs sit higher and taller
                // than the Latin row box, so hover/click use the inked bounds.
                let ink = g.mesh_bounds.translate(pos.to_vec2()).union(Rect::from_x_y_ranges(line_rect.x_range(), line_rect.center().y..=line_rect.center().y));
                if !rect.intersects(line_rect) {
                    continue;
                }
                let sung = pos_ms >= l.start && pos_ms < l.end + 200;
                let lit = is_active || (l.background && sung);
                let dist = active.map_or(3, |a| (k as isize - a as isize).unsigned_abs()) as f32;
                let base_alpha = if lit { 0.38 } else { (0.36 - dist.min(6.0) * 0.035).max(0.14) };
                let a = theme::anim(ui.ctx(), ("lyr-a", k), base_alpha, 0.3) * fade(line_rect.center().y);
                painter.galley_with_override_text_color(pos, g.clone(), theme::with_alpha(Color32::WHITE, a));
                // A line that just finished stays fully lit while it fades, so its last
                // characters are seen highlighted even when the next line follows at once.
                let lit_amt = theme::anim_bool(ui.ctx(), ("lyr-lit", k), lit, 0.35);
                if lit_amt > 0.0 {
                    let progress = if lit {
                        // Finish the fill before the next line takes over.
                        let next = lyrics.lines[*i + 1..].iter().find(|x| !x.background).map_or(u32::MAX, |x| x.start);
                        char_progress(l, pos_ms, next.saturating_sub(120))
                    } else {
                        f32::INFINITY
                    };
                    sweep(&painter, &g, pos, progress, theme::with_alpha(Color32::WHITE, lit_amt * fade(line_rect.center().y)));
                }
                // Click a line to jump there.
                let resp = ui.interact(ink, ui.id().with(("lyr", k)), Sense::click());
                let resp = named(resp, &l.text);
                if resp.hovered() {
                    painter.rect_filled(ink.expand2(vec2(10.0, 6.0)), 8.0, theme::glass(0.05));
                }
                if resp.clicked() {
                    app.actions.push(Action::Seek(l.start as f32 / 1000.0));
                }
            }
        }
    }
    painter.text(
        rect.right_bottom() - vec2(6.0, 4.0),
        Align2::RIGHT_BOTTOM,
        format!("Lyrics from {}", lyrics.source),
        theme::regular(11.5),
        theme::TEXT_FAINT,
    );
}

fn display_text(l: &Line) -> String {
    if l.words.is_empty() { l.text.clone() } else { l.words.iter().map(|w| w.text.as_str()).collect::<String>().trim_end().to_owned() }
}

/// How many characters of the line have been sung (fractional), from word timing or,
/// for line-synced lyrics, spread evenly across the line like better-lyrics does.
/// Word fills are squeezed to end by `done_by` (just before the next line starts).
fn char_progress(l: &Line, pos: u32, done_by: u32) -> f32 {
    if l.words.is_empty() {
        let n = l.text.chars().count() as f32;
        let span = ((l.end.saturating_sub(l.start)) as f32 * 0.9).min(n * 90.0).max(1.0);
        return n * ((pos.saturating_sub(l.start)) as f32 / span).clamp(0.0, 1.0);
    }
    let mut chars = 0.0;
    for w in &l.words {
        let n = w.text.chars().count() as f32;
        let (start, end) = (w.start.min(done_by), w.end.min(done_by));
        if pos >= end {
            chars += n;
        } else if pos >= start {
            let f = (pos - start) as f32 / (end - start).max(1) as f32;
            // Count trailing space only once the word is done.
            let letters = w.text.trim_end().chars().count() as f32;
            chars += letters * f;
            break;
        } else {
            break;
        }
    }
    chars
}

/// Paint the sung part of a line in full colour, clipped per row with a soft edge.
fn sweep(painter: &egui::Painter, g: &std::sync::Arc<egui::Galley>, pos: Pos2, progress: f32, color: Color32) {
    let mut remaining = progress;
    let last = g.rows.len().saturating_sub(1);
    for (r, row) in g.rows.iter().enumerate() {
        let n = row.row.glyphs.len() as f32;
        if n == 0.0 {
            continue;
        }
        // Row size comes from the Latin font; CJK fallback glyphs stand taller and would
        // poke out of a tight clip, leaving their tops unlit. Leave the outer edges open
        // and give inner rows a quarter-row margin.
        let pad = row.row.size.y * 0.25;
        let row_rect = Rect::from_min_size(pos + row.pos.to_vec2(), row.row.size);
        let top = if r == 0 { row_rect.top() - row.row.size.y } else { row_rect.top() - pad };
        let bottom = if r == last { row_rect.bottom() + row.row.size.y } else { row_rect.bottom() + pad };
        if remaining >= n {
            let clip = Rect::from_min_max(pos2(row_rect.left() - 4.0, top), pos2(row_rect.right() + 4.0, bottom));
            painter.with_clip_rect(clip).galley_with_override_text_color(pos, g.clone(), color);
            // Row breaks consume the implicit newline/space between rows.
            remaining -= n;
            continue;
        }
        if remaining <= 0.0 {
            break;
        }
        let idx = remaining.floor() as usize;
        let frac = remaining - idx as f32;
        let x = row.row.glyphs.get(idx).map_or(row.row.size.x, |gl| gl.pos.x + gl.advance_width * frac);
        let edge = row_rect.left() + x;
        let solid = Rect::from_min_max(pos2(row_rect.left() - 4.0, top), pos2(edge - 6.0, bottom));
        painter.with_clip_rect(solid).galley_with_override_text_color(pos, g.clone(), color);
        // Feather: a few thin bands with decreasing alpha.
        for b in 0..4 {
            let x0 = edge - 6.0 + b as f32 * 3.0;
            let band = Rect::from_min_max(pos2(x0, top), pos2(x0 + 3.0, bottom));
            let a = 1.0 - (b as f32 + 0.5) / 4.0;
            painter.with_clip_rect(band).galley_with_override_text_color(pos, g.clone(), theme::with_alpha(color, a * color.a() as f32 / 255.0));
        }
        break;
    }
}
