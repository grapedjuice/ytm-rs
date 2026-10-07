//! App chrome: background, sidebar, top bar, player bar, queue drawer, toasts.

use egui::{Align2, Color32, Pos2, Rect, Sense, Ui, Vec2, pos2, vec2};

use super::widgets::{self, icon_at, icon_button, named, paint_cover, text_at};
use super::{Action, App, LibTab, NpTab, Page, Repeat, fmt_time};
use crate::model::Target;
use crate::shader::Params;
use crate::theme::{self, icon};

// ------------------------------------------------------------------ background

/// Vertical gradient (top colour → bottom colour) over `rect`.
pub fn vgradient(ui: &Ui, rect: Rect, top: Color32, bottom: Color32) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 3, 2);
    ui.painter().add(egui::Shape::mesh(mesh));
}

pub fn background(app: &mut App, ui: &mut Ui, screen: Rect, now: f64) {
    let dt = (now - app.last_frame).clamp(0.0, 0.1);
    app.last_frame = now;
    let moving = app.anim_bg && app.playing();
    if moving {
        app.bg_time += dt;
    }
    ui.painter().rect_filled(screen, 0.0, theme::BG);
    if let Some(bg) = &app.bg {
        let blend = theme::ease_in_out(((now - app.art_changed) / 1.4) as f32);
        let level = if app.reactive_bg && app.playing() { app.audio.status.level() } else { 0.0 };
        bg.paint(
            ui.painter(),
            screen,
            Params {
                time: app.bg_time as f32,
                blend,
                intensity: 0.9,
                saturation: 1.35,
                dim: 0.5,
                scale: 1.12 + level.min(0.5) * 0.08,
            },
        );
    } else {
        vgradient(ui, screen, theme::with_alpha(app.accent, 0.18), theme::BG);
    }
    // Keep text readable: darken toward the bottom and under the sidebar.
    vgradient(ui, screen, theme::shade(0.12), theme::shade(0.55));
}

// ------------------------------------------------------------------ sidebar

pub fn sidebar(app: &mut App, ui: &mut Ui) {
    // Logo
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        let (r, resp) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::click());
        ui.painter().circle_filled(r.center(), 16.0, theme::RED);
        ui.painter().circle_stroke(r.center(), 9.5, egui::Stroke::new(1.6, Color32::WHITE));
        widgets::play_triangle(ui, r.center() + vec2(0.8, 0.0), 4.6, Color32::WHITE);
        if named(resp, "Home").clicked() {
            app.actions.push(Action::Go(Page::Home));
        }
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Music").font(theme::bold(22.0)).color(theme::TEXT));
    });
    ui.add_space(22.0);

    let mut go_to: Option<Page> = None;
    let mut nav = |ui: &mut Ui, glyph: char, label: &str, page: Page, selected: bool| {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 42.0), Sense::click());
        let resp = named(resp, label);
        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
        let s = theme::anim_bool(ui.ctx(), resp.id.with("s"), selected, 0.2);
        ui.painter().rect_filled(rect, 10.0, theme::glass(0.05 * h + 0.10 * s));
        let fg = theme::lerp_color(theme::TEXT_DIM, theme::TEXT, (h + s).min(1.0));
        icon_at(ui, pos2(rect.left() + 22.0, rect.center().y), glyph, 19.0, fg);
        let font = if selected { theme::semibold(15.0) } else { theme::regular(15.0) };
        ui.painter().text(pos2(rect.left() + 46.0, rect.center().y), Align2::LEFT_CENTER, label, font, fg);
        if resp.clicked() {
            go_to = Some(page);
        }
    };
    let p = app.page.clone();
    nav(ui, icon::HOUSE, "Home", Page::Home, p == Page::Home);
    nav(ui, icon::COMPASS, "Explore", Page::Explore, p == Page::Explore);
    nav(ui, icon::LIBRARY, "Library", Page::Library(LibTab::Playlists), matches!(p, Page::Library(_)));

    ui.add_space(14.0);
    let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(line, 0.0, theme::glass(0.08));
    ui.add_space(14.0);

    let bottom_h = 50.0;
    let list_h = (ui.available_height() - bottom_h).max(0.0);
    ui.allocate_ui(vec2(ui.available_width(), list_h), |ui| {
        if !app.logged_in {
            ui.label(egui::RichText::new("Sign in to see your playlists").color(theme::TEXT_DIM));
            ui.add_space(8.0);
            if widgets::pill(ui, "Sign in", Some(icon::CIRCLE_USER), false).clicked() {
                app.actions.push(Action::Go(Page::Settings));
            }
            return;
        }
        ui.label(egui::RichText::new("PLAYLISTS").font(theme::semibold(11.5)).color(theme::TEXT_FAINT));
        ui.add_space(6.0);
        egui::ScrollArea::vertical().id_salt("sidebar-pl").auto_shrink(false).show(ui, |ui| {
            let Some(lib) = &app.library else {
                ui.spinner();
                return;
            };
            for c in &lib.playlists {
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 48.0), Sense::click());
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                let resp = named(resp, &c.title);
                let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
                let selected = matches!(&c.target, Target::Playlist(id) if app.page == Page::Playlist(id.clone()));
                ui.painter().rect_filled(rect, 8.0, theme::glass(0.05 * h + if selected { 0.08 } else { 0.0 }));
                let img = Rect::from_min_size(rect.min + vec2(6.0, 6.0), Vec2::splat(36.0));
                paint_cover(ui, img, &c.thumbs, 5.0, 1.0);
                let w = rect.width() - 58.0;
                text_at(ui, pos2(img.right() + 10.0, rect.top() + 7.0), &c.title, theme::semibold(13.5), theme::TEXT, w);
                text_at(ui, pos2(img.right() + 10.0, rect.top() + 26.0), &c.subtitle, theme::regular(12.0), theme::TEXT_DIM, w);
                if resp.clicked() {
                    app.actions.push(Action::Open(c.target.clone()));
                }
            }
        });
    });
    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
        nav(ui, icon::SETTINGS, "Settings", Page::Settings, p == Page::Settings);
    });
    if let Some(page) = go_to {
        app.actions.push(Action::Go(page));
    }
}

// ------------------------------------------------------------------ top bar

pub fn top_bar(app: &mut App, ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if icon_button(ui, icon::CHEVRON_LEFT, 20.0, "Back", false).clicked() {
            app.nav_back();
        }
        if icon_button(ui, icon::CHEVRON_RIGHT, 20.0, "Forward", false).clicked() {
            app.nav_forward();
        }
        ui.add_space(12.0);

        let width = (ui.available_width() - 70.0).clamp(240.0, 560.0);
        let (rect, _) = ui.allocate_exact_size(vec2(width, 42.0), Sense::hover());
        let focus = theme::anim_bool(ui.ctx(), "search-focus", app.search_focused, 0.2);
        ui.painter().rect_filled(rect, 21.0, theme::glass(0.09 + 0.05 * focus));
        if focus > 0.0 {
            ui.painter().rect_stroke(rect, 21.0, egui::Stroke::new(1.0, theme::glass(0.25 * focus)), egui::StrokeKind::Inside);
        }
        icon_at(ui, pos2(rect.left() + 22.0, rect.center().y), icon::SEARCH, 17.0, theme::TEXT_DIM);
        let inner = Rect::from_min_max(pos2(rect.left() + 42.0, rect.top() + 4.0), pos2(rect.right() - 16.0, rect.bottom() - 4.0));
        let edit = ui.put(
            inner,
            egui::TextEdit::singleline(&mut app.search_text)
                .hint_text(egui::RichText::new("Search songs, albums, artists, podcasts").color(theme::TEXT_FAINT))
                .font(theme::regular(15.0))
                .frame(egui::Frame::NONE)
                .vertical_align(egui::Align::Center)
                .desired_width(inner.width()),
        );
        app.search_rect = Some(rect);
        app.search_focused = edit.has_focus();
        if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::F)) {
            edit.request_focus();
        }
        if edit.changed() {
            let now = ui.input(|i| i.time);
            app.suggest_due = Some(now + 0.22);
            if app.search_text.trim().is_empty() {
                app.suggestions = Default::default();
            }
        }
        if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let q = app.search_text.trim().to_owned();
            if !q.is_empty() {
                app.suggestions = Default::default();
                app.actions.push(Action::Go(Page::Search(q)));
            }
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (r, resp) = ui.allocate_exact_size(Vec2::splat(36.0), Sense::click());
            let resp = named(resp, "Account");
            let initial = app.account_name.as_deref().and_then(|n| n.chars().next()).unwrap_or('?');
            if app.logged_in {
                ui.painter().circle_filled(r.center(), 18.0, theme::lerp_color(app.accent, Color32::BLACK, 0.35));
                ui.painter().text(r.center(), Align2::CENTER_CENTER, initial, theme::bold(16.0), Color32::WHITE);
            } else {
                ui.painter().circle_filled(r.center(), 18.0, theme::glass(0.12));
                icon_at(ui, r.center(), icon::USER, 18.0, theme::TEXT_DIM);
            }
            if resp.clicked() {
                app.actions.push(Action::Go(Page::Settings));
            }
            if app.loading {
                ui.add_space(10.0);
                ui.spinner();
            }
        });
    });
}

/// Search suggestions dropdown under the search box.
pub fn suggestions(app: &mut App, ctx: &egui::Context) {
    let Some(rect) = app.search_rect else { return };
    let show = app.search_focused && !app.suggestions.1.is_empty() && !app.np_open;
    let t = theme::anim_bool(ctx, "suggest-open", show, 0.15);
    if t <= 0.0 {
        return;
    }
    egui::Area::new("suggestions".into())
        .order(egui::Order::Foreground)
        .fixed_pos(rect.left_bottom() + vec2(0.0, 6.0 - 6.0 * (1.0 - t)))
        .show(ctx, |ui| {
            ui.set_opacity(t);
            egui::Frame::new()
                .fill(Color32::from_rgb(30, 26, 34))
                .corner_radius(14)
                .inner_margin(6)
                .shadow(egui::epaint::Shadow { offset: [0, 10], blur: 30, spread: 0, color: theme::shade(0.5) })
                .show(ui, |ui| {
                    ui.set_width(rect.width() - 12.0);
                    for term in app.suggestions.1.clone() {
                        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
                        let resp = named(resp, &term);
                        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.1);
                        ui.painter().rect_filled(r, 8.0, theme::glass(0.08 * h));
                        icon_at(ui, pos2(r.left() + 20.0, r.center().y), icon::SEARCH, 15.0, theme::TEXT_DIM);
                        text_at(ui, pos2(r.left() + 42.0, r.center().y - 9.0), &term, theme::regular(15.0), theme::TEXT, r.width() - 52.0);
                        // Pointer-down, not click: the text box loses focus (hiding this) first.
                        if resp.is_pointer_button_down_on() || resp.clicked() {
                            app.search_text = term.clone();
                            app.suggestions = Default::default();
                            app.search_focused = false;
                            app.actions.push(Action::Go(Page::Search(term)));
                        }
                    }
                });
        });
}

// ------------------------------------------------------------------ player bar

pub fn player_bar(app: &mut App, ui: &mut Ui, now: f64) {
    let full = ui.max_rect();
    let status = app.audio.status.clone();
    let track = app.current().cloned();
    let playing = app.playing();
    let total = status
        .duration()
        .map(|d| d.as_secs_f32())
        .or(track.as_ref().and_then(|t| t.duration).map(|d| d as f32))
        .unwrap_or(0.0);
    let pos = app.seek_drag.unwrap_or(status.position_smooth().as_secs_f32()).min(total.max(0.0));

    // Progress line along the top edge; thickens with a knob on hover.
    let bar_hit = Rect::from_min_size(full.min - vec2(0.0, 6.0), vec2(full.width(), 14.0));
    let resp = ui.interact(bar_hit, ui.id().with("progress"), Sense::click_and_drag());
    resp.widget_info(|| egui::WidgetInfo::slider(total > 0.0, pos as f64, "Seek"));
    let h = theme::anim_bool(ui.ctx(), "progress-h", resp.hovered() || resp.dragged(), 0.15);
    let thick = 3.0 + 2.0 * h;
    let line = Rect::from_min_size(full.min, vec2(full.width(), thick));
    ui.painter().rect_filled(line, 0.0, theme::glass(0.12));
    let frac = if total > 0.0 { pos / total } else { 0.0 };
    let filled = Rect::from_min_size(line.min, vec2(line.width() * frac, thick));
    ui.painter().rect_filled(filled, 0.0, app.accent);
    if h > 0.0 && total > 0.0 {
        ui.painter().circle_filled(pos2(filled.right(), line.center().y), 7.0 * h, app.accent);
        if let Some(p) = resp.hover_pos() {
            let t = ((p.x - full.left()) / full.width()).clamp(0.0, 1.0) * total;
            let tip = pos2(p.x, line.top() - 16.0);
            let g = widgets::galley(ui, &fmt_time(t as u32), theme::semibold(12.0), Color32::WHITE, 80.0);
            let bg = Rect::from_center_size(tip, g.size() + vec2(12.0, 6.0));
            ui.painter().rect_filled(bg, 6.0, theme::shade(0.75));
            ui.painter().galley(bg.min + vec2(6.0, 3.0), g, Color32::WHITE);
        }
    }
    if total > 0.0 {
        if let Some(p) = resp.interact_pointer_pos() {
            let t = ((p.x - full.left()) / full.width()).clamp(0.0, 1.0) * total;
            if resp.dragged() {
                app.seek_drag = Some(t);
            }
            if resp.drag_stopped() || resp.clicked() {
                app.seek_drag = None;
                app.actions.push(Action::Seek(t));
            }
        }
    }

    let row = Rect::from_min_max(pos2(full.left() + 16.0, full.top() + 6.0), pos2(full.right() - 16.0, full.bottom()));
    let cy = row.center().y;

    // Left: now playing
    if let Some(t) = &track {
        let art = Rect::from_center_size(pos2(row.left() + 30.0, cy), Vec2::splat(56.0));
        let ar = ui.interact(art, ui.id().with("np-art"), Sense::click());
        let ar = named(ar, "Open now playing");
        let ah = theme::anim_bool(ui.ctx(), ar.id.with("h"), ar.hovered(), 0.15);
        paint_cover(ui, art, &t.thumbs, 6.0, 1.0);
        if ah > 0.0 {
            ui.painter().rect_filled(art, 6.0, theme::shade(0.45 * ah));
            icon_at(ui, art.center(), icon::CHEVRON_UP, 22.0, theme::with_alpha(Color32::WHITE, ah));
        }
        if ar.clicked() {
            app.actions.push(Action::OpenNowPlaying(true));
        }
        let x = art.right() + 14.0;
        let w = (row.width() * 0.3 - 110.0).max(80.0);
        let tr = text_at(ui, pos2(x, cy - 19.0), &t.title, theme::semibold(15.0), theme::TEXT, w);
        let tresp = ui.interact(tr, ui.id().with("np-title"), Sense::click());
        if named(tresp, &t.title).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            app.actions.push(Action::OpenNowPlaying(true));
        }
        let mut ax = x;
        for (i, a) in t.artists.iter().enumerate() {
            let s = if i + 1 < t.artists.len() { format!("{}, ", a.name) } else { a.name.clone() };
            let r = text_at(ui, pos2(ax, cy + 2.0), &s, theme::regular(13.5), theme::TEXT_DIM, (x + w - ax).max(10.0));
            if let Some(id) = &a.id {
                let resp = ui.interact(r, ui.id().with(("artist", i)), Sense::click());
                let resp = named(resp, &a.name).on_hover_cursor(egui::CursorIcon::PointingHand);
                if resp.hovered() {
                    ui.painter().hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0, theme::TEXT_DIM));
                }
                if resp.clicked() {
                    app.actions.push(Action::Open(Target::Artist(id.clone())));
                }
            }
            ax = r.right();
            if ax > x + w {
                break;
            }
        }
        // Like: a heart right after the title/artist, filled red when liked.
        let liked = app.liked.contains(&t.id);
        let text_end = tr.right().max(ax).min(x + w);
        let lpos = pos2(text_end + 24.0, cy);
        let lr = Rect::from_center_size(lpos, Vec2::splat(36.0));
        let lresp = ui.interact(lr, ui.id().with("like"), Sense::click());
        let lresp = named(lresp, if liked { "Remove like" } else { "Like" }).on_hover_text(if liked { "Remove like" } else { "Like" });
        let lh = theme::anim_bool(ui.ctx(), lresp.id.with("h"), lresp.hovered(), 0.15);
        let pop = theme::anim_bool(ui.ctx(), ("liked", &t.id), liked, 0.3);
        ui.painter().circle_filled(lpos, 18.0, theme::glass(0.1 * lh));
        let size = 19.0 * (1.0 + 0.25 * (pop * std::f32::consts::PI).sin());
        let idle = theme::lerp_color(theme::TEXT_DIM, theme::TEXT, lh);
        let outline = theme::lerp_color(idle, theme::RED, pop);
        widgets::heart(ui, lpos, size, pop, outline, theme::RED);
        if lresp.clicked() {
            app.actions.push(Action::Like(t.id.clone(), !liked));
        }
    }

    // Centre: transport
    let accent = app.accent;
    let center = pos2(row.center().x, cy);
    let ctl = |ui: &mut Ui, dx: f32, glyph: char, name: &str, active: bool, size: f32| -> bool {
        let r = Rect::from_center_size(center + vec2(dx, 0.0), Vec2::splat(size + 16.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r));
        let resp = icon_button(&mut child, glyph, size, name, active);
        if active {
            ui.painter().circle_filled(r.center_bottom() + vec2(0.0, 2.0), 2.0, accent);
        }
        resp.clicked()
    };
    if ctl(ui, -124.0, icon::SHUFFLE, "Shuffle up next", false, 18.0) {
        app.shuffle_upcoming();
    }
    if ctl(ui, -64.0, icon::SKIP_BACK, "Previous", false, 22.0) {
        app.prev();
    }
    let (rep_icon, rep_on) = match app.repeat {
        Repeat::Off => (icon::REPEAT, false),
        Repeat::All => (icon::REPEAT, true),
        Repeat::One => (icon::REPEAT_1, true),
    };
    if ctl(ui, 64.0, icon::SKIP_FORWARD, "Next", false, 22.0) {
        app.next();
    }
    if ctl(ui, 124.0, rep_icon, "Repeat", rep_on, 18.0) {
        app.repeat = match app.repeat {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        };
    }
    let pr = Rect::from_center_size(center, Vec2::splat(46.0));
    let presp = ui.interact(pr, ui.id().with("play"), Sense::click());
    let presp = named(presp, if playing { "Pause" } else { "Play" });
    let ph = theme::anim_bool(ui.ctx(), presp.id.with("h"), presp.hovered(), 0.15);
    let scale = if presp.is_pointer_button_down_on() { 0.92 } else { 1.0 + 0.05 * ph };
    ui.painter().circle_filled(center, 23.0 * scale, Color32::WHITE);
    let morph = theme::anim_bool(ui.ctx(), "play-morph", playing || app.buffering, 0.18);
    if app.buffering {
        let a = now as f32 * 6.0;
        ui.painter().add(egui::Shape::Path(egui::epaint::PathShape::line(
            (0..20).map(|i| {
                let t = a + i as f32 * 0.22;
                center + vec2(t.cos(), t.sin()) * 9.0
            })
            .collect(),
            egui::epaint::PathStroke::new(2.5, Color32::from_gray(20)),
        )));
        ui.ctx().request_repaint();
    } else if morph > 0.5 {
        widgets::pause_bars(ui, center, 8.0 * (2.0 * morph - 1.0).max(0.4), Color32::from_gray(15));
    } else {
        widgets::play_triangle(ui, center + vec2(1.5, 0.0), 8.5 * (1.0 - 2.0 * morph).max(0.4), Color32::from_gray(15));
    }
    if presp.clicked() {
        app.toggle();
    }

    // Right: time, volume, lyrics, queue, expand
    let mut rx = row.right();
    let mut right_btn = |ui: &mut Ui, glyph: char, name: &str, active: bool| -> bool {
        let r = Rect::from_center_size(pos2(rx - 18.0, cy), Vec2::splat(36.0));
        rx -= 40.0;
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r));
        let resp = icon_button(&mut child, glyph, 18.0, name, active);
        if active {
            ui.painter().circle_filled(r.center_bottom() + vec2(0.0, 2.0), 2.0, accent);
        }
        resp.clicked()
    };
    if right_btn(ui, if app.np_open { icon::CHEVRON_DOWN } else { icon::CHEVRON_UP }, "Now playing", app.np_open) {
        app.actions.push(Action::OpenNowPlaying(!app.np_open));
    }
    let queue_open = app.queue_open;
    if right_btn(ui, icon::LIST_MUSIC, "Queue", queue_open) {
        app.queue_open = !app.queue_open;
    }
    let lyrics_on = app.np_open && app.np_tab == NpTab::Lyrics;
    if right_btn(ui, icon::MIC_VOCAL, "Lyrics", lyrics_on) {
        if lyrics_on {
            app.np_open = false;
        } else {
            app.np_tab = NpTab::Lyrics;
            app.actions.push(Action::OpenNowPlaying(true));
        }
    }
    // Volume
    let vol = app.audio.volume();
    let slider = Rect::from_min_size(pos2(rx - 96.0, cy - 10.0), vec2(90.0, 20.0));
    let vresp = ui.interact(slider, ui.id().with("vol"), Sense::click_and_drag());
    vresp.widget_info(|| egui::WidgetInfo::slider(true, vol as f64, "Volume"));
    let vh = theme::anim_bool(ui.ctx(), "vol-h", vresp.hovered() || vresp.dragged(), 0.15);
    let track_r = Rect::from_center_size(slider.center(), vec2(slider.width(), 4.0));
    ui.painter().rect_filled(track_r, 2.0, theme::glass(0.15));
    let fill = Rect::from_min_size(track_r.min, vec2(track_r.width() * vol, 4.0));
    ui.painter().rect_filled(fill, 2.0, theme::lerp_color(theme::TEXT_DIM, Color32::WHITE, vh));
    ui.painter().circle_filled(pos2(fill.right(), track_r.center().y), 2.0 + 4.5 * vh, Color32::WHITE);
    if let Some(p) = vresp.interact_pointer_pos() {
        if vresp.dragged() || vresp.clicked() {
            app.audio.set_volume(((p.x - track_r.left()) / track_r.width()).clamp(0.0, 1.0));
        }
    }
    if vresp.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            app.audio.set_volume((vol + scroll / 600.0).clamp(0.0, 1.0));
        }
    }
    let vglyph = if vol == 0.0 { icon::VOLUME_X } else if vol < 0.5 { icon::VOLUME_1 } else { icon::VOLUME_2 };
    let vi = Rect::from_center_size(pos2(slider.left() - 18.0, cy), Vec2::splat(32.0));
    let mresp = named(ui.interact(vi, ui.id().with("mute"), Sense::click()), "Mute");
    icon_at(ui, vi.center(), vglyph, 18.0, if mresp.hovered() { theme::TEXT } else { theme::TEXT_DIM });
    if mresp.clicked() {
        let stash = ui.id().with("unmute");
        if vol > 0.0 {
            ui.data_mut(|d| d.insert_temp(stash, vol));
            app.audio.set_volume(0.0);
        } else {
            let v = ui.data(|d| d.get_temp::<f32>(stash)).unwrap_or(0.6);
            app.audio.set_volume(v);
        }
    }
    if total > 0.0 {
        let txt = format!("{} / {}", fmt_time(pos as u32), fmt_time(total as u32));
        ui.painter().text(pos2(vi.left() - 10.0, cy), Align2::RIGHT_CENTER, txt, theme::regular(13.0), theme::TEXT_DIM);
    }
}

// ------------------------------------------------------------------ queue drawer

pub fn queue_drawer(app: &mut App, ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Up next").font(theme::bold(20.0)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if icon_button(ui, icon::X, 16.0, "Close queue", false).clicked() {
                app.queue_open = false;
            }
        });
    });
    ui.add_space(8.0);
    queue_list(app, ui, "drawer");
}

/// Current track plus everything after it. Shared by the drawer and now playing.
pub fn queue_list(app: &mut App, ui: &mut Ui, salt: &str) {
    let now = ui.input(|i| i.time);
    let level = app.audio.status.level();
    let playing = app.playing();
    let accent = app.accent;
    egui::ScrollArea::vertical().id_salt(("queue", salt)).auto_shrink(false).show(ui, |ui| {
        if app.queue.is_empty() {
            ui.label(egui::RichText::new("Nothing queued yet").color(theme::TEXT_DIM));
            return;
        }
        let start = app.index.unwrap_or(0);
        ui.checkbox(&mut app.autoplay, egui::RichText::new("Autoplay similar songs").color(theme::TEXT_DIM));
        ui.add_space(6.0);
        for i in start..app.queue.len() {
            let t = app.queue[i].clone();
            let current = Some(i) == app.index;
            let opts = widgets::RowOpts { number: None, show_album: false, current, playing, accent, time: now, level };
            let resp = widgets::track_row(ui, &t, &opts, &mut app.actions);
            if resp.clicked() {
                // The playing row pauses/resumes instead of doing nothing.
                app.actions.push(if current { Action::TogglePlay } else { Action::QueueJump(i) });
            }
            if !current && resp.hovered() {
                // Remove button over the duration column.
                let x = Rect::from_center_size(resp.rect.right_center() - vec2(26.0, 0.0), Vec2::splat(28.0));
                let xr = named(ui.interact(x, resp.id.with("rm"), Sense::click()), "Remove from queue");
                ui.painter().circle_filled(x.center(), 14.0, theme::shade(0.6));
                icon_at(ui, x.center(), icon::X, 14.0, theme::TEXT);
                if xr.clicked() {
                    app.actions.push(Action::QueueRemove(i));
                }
            }
            let acts = &mut app.actions;
            resp.context_menu(|ui| widgets::song_menu(ui, &t, acts));
        }
    });
}

// ------------------------------------------------------------------ toast

pub fn toast(app: &mut App, ctx: &egui::Context, now: f64) {
    let Some((msg, shown)) = &app.toast else { return };
    if *shown < 0.0 {
        return;
    }
    let age = (now - shown) as f32;
    let t = theme::ease_out(age / 0.25) * (1.0 - ((age - 4.1) / 0.4).clamp(0.0, 1.0));
    let screen = ctx.content_rect();
    egui::Area::new("toast".into())
        .order(egui::Order::Tooltip)
        .pivot(Align2::CENTER_BOTTOM)
        .fixed_pos(Pos2::new(screen.center().x, screen.bottom() - 104.0 + 14.0 * (1.0 - t)))
        .show(ctx, |ui| {
            ui.set_opacity(t);
            egui::Frame::new()
                .fill(Color32::from_rgb(38, 34, 42))
                .corner_radius(12)
                .inner_margin(egui::Margin::symmetric(18, 12))
                .shadow(egui::epaint::Shadow { offset: [0, 8], blur: 24, spread: 0, color: theme::shade(0.45) })
                .show(ui, |ui| {
                    ui.set_max_width(520.0);
                    ui.label(egui::RichText::new(msg.as_str()).font(theme::regular(14.0)).color(theme::TEXT));
                });
        });
    ctx.request_repaint();
}
