//! App chrome: background, sidebar, top bar, player bar, queue drawer, toasts.

use egui::{Align2, Color32, Pos2, Rect, Sense, Ui, Vec2, pos2, vec2};

use super::widgets::{self, icon_at, icon_button, named, paint_cover, text_at};
use super::{Action, App, LibTab, NpTab, Page, Repeat, fmt_time};
use crate::model::Target;
use crate::shader::Params;
use crate::theme::{self, icon};

// ------------------------------------------------------------------ background

/// Vertical gradient (top colour → bottom colour) over `rect`.
pub fn vgradient(painter: &egui::Painter, rect: Rect, top: Color32, bottom: Color32) {
    gradient(painter, rect, [top, top, bottom, bottom]);
}

/// Horizontal gradient (left colour → right colour) over `rect`.
pub fn hgradient(painter: &egui::Painter, rect: Rect, left: Color32, right: Color32) {
    gradient(painter, rect, [left, right, left, right]);
}

/// Corner colours: left top, right top, left bottom, right bottom.
fn gradient(painter: &egui::Painter, rect: Rect, c: [Color32; 4]) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), c[0]);
    mesh.colored_vertex(rect.right_top(), c[1]);
    mesh.colored_vertex(rect.left_bottom(), c[2]);
    mesh.colored_vertex(rect.right_bottom(), c[3]);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 3, 2);
    painter.add(egui::Shape::mesh(mesh));
}

/// The background mode browse pages use: the theme's own choice, else whatever now
/// playing uses.
fn page_mode(look: &crate::preset::Preset) -> crate::preset::BgMode {
    use crate::preset::{BgMode, PageBg};
    match look.app.page_bg {
        PageBg::Same => look.background.mode,
        PageBg::Static => BgMode::CoverStatic,
        PageBg::Accent => BgMode::AccentGradient,
        PageBg::Solid => BgMode::Solid,
    }
}

/// Whether the pages' background moves while music plays (static themes need no frames).
pub fn pages_bg_moves(look: &crate::preset::Preset) -> bool {
    use crate::preset::BgMode;
    let b = &look.background;
    matches!(page_mode(look), BgMode::Kawarp | BgMode::CoverFullbleed) && b.speed > 0.0 && b.intensity > 0.0
}

/// `only`: paint just this part (the rest is covered by now playing).
pub fn background(app: &mut App, ui: &mut Ui, screen: Rect, now: f64, only: Option<Rect>) {
    use crate::preset::BgMode;
    let dt = (now - app.last_frame).clamp(0.0, 0.1);
    app.last_frame = now;
    let look = app.look();
    let b = &look.background;
    let mode = page_mode(look);
    let dim = look.app.page_dim.unwrap_or(b.page_dim).clamp(0.02, 1.0);
    // Now playing animates the shader itself, so keep the clock running for it too.
    let animated = matches!(b.mode, BgMode::Kawarp | BgMode::CoverFullbleed) && b.speed > 0.0;
    let moving = !app.default_theme && app.anim_bg && animated && app.playing();
    if moving {
        app.bg_time += dt * b.speed as f64;
    }
    let painter = match only {
        Some(r) => ui.painter().with_clip_rect(r),
        None => ui.painter().clone(),
    };
    painter.rect_filled(screen, 0.0, if app.default_theme { theme::BG } else { look.solid() });
    let art = matches!(mode, BgMode::Kawarp | BgMode::CoverStatic | BgMode::CoverFullbleed);
    if let Some(bg) = app.bg.as_ref().filter(|_| !app.default_theme && art) {
        let blend = theme::ease_in_out(((now - app.art_changed) / 1.4) as f32);
        if blend < 1.0 {
            // The crossfade to new art finishes even while paused.
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
        }
        let level = if app.reactive_bg && app.playing() { app.audio.status.level() * b.reactive } else { 0.0 };
        let still = mode == BgMode::CoverStatic;
        bg.paint(
            &painter,
            screen,
            Params {
                time: app.bg_time as f32,
                blend,
                intensity: if still { 0.0 } else { b.intensity * 0.9 },
                saturation: b.saturation * 0.9,
                dim,
                scale: (b.scale - 0.08).max(1.0) + if still { 0.0 } else { level.min(0.5) * 0.08 },
            },
        );
    } else if !app.default_theme && mode == BgMode::Solid {
        // Plain colour: nothing over it.
    } else {
        vgradient(&painter, screen, theme::with_alpha(app.accent_color(), 0.18 * if app.default_theme { 1.0 } else { dim / 0.5 }), if app.default_theme { theme::BG } else { look.solid() });
    }
    // Keep text readable: darken toward the bottom and under the sidebar.
    if app.default_theme || mode != BgMode::Solid {
        vgradient(&painter, screen, theme::shade(0.12), theme::shade(0.55));
    }
}

// ------------------------------------------------------------------ sidebar

/// The ☰ button at the top of the sidebar: collapses it to an icon rail and back.
fn menu_button(app: &mut App, ui: &mut Ui) {
    let (r, resp) = ui.allocate_exact_size(vec2(40.0, 32.0), Sense::click());
    let label = if app.sidebar_collapsed { "Expand sidebar" } else { "Collapse sidebar" };
    let resp = named(resp, label).on_hover_text(label).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
    ui.painter().circle_filled(r.center(), 18.0, theme::glass(0.08 * h));
    let color = theme::lerp_color(theme::text_dim(), theme::TEXT, h);
    for dy in [-5.5, 0.0, 5.5] {
        let y = r.center().y + dy;
        ui.painter().line_segment([pos2(r.center().x - 8.0, y), pos2(r.center().x + 8.0, y)], egui::Stroke::new(1.8, color));
    }
    if resp.clicked() {
        app.sidebar_collapsed = !app.sidebar_collapsed;
    }
}

/// How the theme draws nav items.
#[derive(Clone, Copy)]
struct NavLook {
    style: crate::preset::NavStyle,
    radius: f32,
    accent: Color32,
}

impl NavLook {
    fn of(app: &App) -> Self {
        Self { style: app.look().app.nav, radius: (app.look().chrome.radius * 0.6).clamp(4.0, 16.0), accent: app.accent_color() }
    }
}

/// A nav item's background in the theme's nav style; returns its icon colour.
/// `h` and `s` are the hover and selected animations (0..1).
fn nav_bg(ui: &Ui, look: NavLook, rect: Rect, h: f32, s: f32) -> Color32 {
    use crate::preset::NavStyle;
    let (nav_r, accent) = (look.radius, look.accent);
    let icon_fg = theme::lerp_color(theme::text_dim(), theme::TEXT, (h + s).min(1.0));
    match look.style {
        NavStyle::Pill => {
            ui.painter().rect_filled(rect, nav_r, theme::glass(0.05 * h + 0.10 * s));
            icon_fg
        }
        NavStyle::Accent => {
            ui.painter().rect_filled(rect, nav_r, theme::glass(0.05 * h));
            ui.painter().rect_filled(rect, nav_r, theme::with_alpha(accent, 0.22 * s));
            theme::lerp_color(icon_fg, accent, s)
        }
        NavStyle::Bar => {
            ui.painter().rect_filled(rect, nav_r, theme::glass(0.05 * h));
            let bar = Rect::from_center_size(pos2(rect.left() + 2.0, rect.center().y), vec2(3.0, 20.0 * s));
            ui.painter().rect_filled(bar, 2.0, accent);
            icon_fg
        }
    }
}

/// Sidebar width expanded and collapsed to an icon rail (points).
pub const SIDEBAR_W: f32 = 236.0;
pub const RAIL_W: f32 = 76.0;

/// How collapsed the sidebar is, 0 (full) to 1 (icon rail), eased over the toggle.
pub fn sidebar_collapse(app: &App, ctx: &egui::Context) -> f32 {
    let t = ctx.animate_bool_with_time(egui::Id::unique("sidebar-collapse"), app.sidebar_collapsed, 0.28 * theme::SPEED);
    // Ease in-out: starts and lands gently in both directions.
    t * t * (3.0 - 2.0 * t)
}

/// One layout for both states, so collapsing animates: the panel narrows while icons
/// and covers keep their column (24 pt from the left) and the labels fade and clip.
pub fn sidebar(app: &mut App, ui: &mut Ui) {
    const ICON_X: f32 = 24.0;
    let t = sidebar_collapse(app, ui.ctx());
    let collapsed = app.sidebar_collapsed;
    // Labels fade out over the first two thirds of collapsing (and back in late).
    let a = ((1.0 - t) * 1.5 - 0.5).clamp(0.0, 1.0);
    // Content width when expanded: text keeps its truncation while the panel narrows.
    let full_w = ui.available_width() + (SIDEBAR_W - RAIL_W) * t;

    // Menu button, logo
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(4.0);
        menu_button(app, ui);
        ui.add_space(8.0);
        ui.multiply_opacity(a);
        let (r, resp) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::click());
        ui.painter().circle_filled(r.center(), 16.0, theme::RED);
        ui.painter().circle_stroke(r.center(), 9.5, egui::Stroke::new(1.6, Color32::WHITE));
        widgets::play_triangle(ui, r.center() + vec2(0.8, 0.0), 4.6, Color32::WHITE);
        let resp = named(resp, "Home");
        let resp = if collapsed { resp } else { resp.on_hover_cursor(egui::CursorIcon::PointingHand) };
        if resp.clicked() && !collapsed {
            app.actions.push(Action::Go(Page::Home));
        }
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Music").font(theme::bold(22.0)).color(theme::TEXT));
    });
    ui.add_space(22.0);

    let mut go_to: Option<Page> = None;
    let look = NavLook::of(app);
    let nav = |ui: &mut Ui, glyph: char, label: &str, page: Page, selected: bool, go_to: &mut Option<Page>| {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 42.0), Sense::click());
        let mut resp = named(resp, label).on_hover_cursor(egui::CursorIcon::PointingHand);
        if collapsed {
            resp = resp.on_hover_text(label);
        }
        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
        let s = theme::anim_bool(ui.ctx(), resp.id.with("s"), selected, 0.2);
        let icon_fg = nav_bg(ui, look, rect, h, s);
        icon_at(ui, pos2(rect.left() + ICON_X, rect.center().y), glyph, 19.0, icon_fg);
        if a > 0.0 {
            let fg = theme::lerp_color(theme::text_dim(), theme::TEXT, (h + s).min(1.0));
            let font = if selected { theme::semibold(15.0) } else { theme::regular(15.0) };
            ui.painter().text(pos2(rect.left() + 48.0, rect.center().y), Align2::LEFT_CENTER, label, font, theme::with_alpha(fg, a));
        }
        if resp.clicked() {
            *go_to = Some(page);
        }
    };
    let p = app.page.clone();
    nav(ui, icon::HOUSE, "Home", Page::Home, p == Page::Home, &mut go_to);
    nav(ui, icon::COMPASS, "Explore", Page::Explore, p == Page::Explore, &mut go_to);
    nav(ui, icon::LIBRARY, "Library", Page::Library(LibTab::Playlists), matches!(p, Page::Library(_)), &mut go_to);
    let jam_label = match app.jam.member_count() {
        n if app.jam.active() && n > 0 => format!("Jam · {n} listening"),
        _ => "Listen together".to_owned(),
    };
    nav(ui, icon::USERS, &jam_label, Page::Jam, p == Page::Jam, &mut go_to);

    ui.add_space(14.0);
    let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(line, 0.0, theme::glass(0.08));
    ui.add_space(14.0 - 4.0 * t);

    let transferring = super::transfer::sidebar_visible(app, ui.input(|i| i.time));
    let bottom_h = if transferring { 114.0 } else { 50.0 };
    let list_h = (ui.available_height() - bottom_h).max(0.0);
    ui.allocate_ui(vec2(ui.available_width(), list_h), |ui| {
        if !app.logged_in {
            // Too wide to squeeze: cross-fade the message and the rail's icon.
            if t < 0.5 {
                ui.multiply_opacity(1.0 - 2.0 * t);
                ui.label(egui::RichText::new("Sign in to see your playlists").color(theme::text_dim()));
                ui.add_space(8.0);
                if widgets::pill(ui, "Sign in", Some(icon::CIRCLE_USER), false).clicked() {
                    app.actions.push(Action::Go(Page::Settings));
                }
            } else {
                ui.multiply_opacity(2.0 * t - 1.0);
                nav(ui, icon::CIRCLE_USER, "Sign in", Page::Settings, false, &mut go_to);
            }
            return;
        }
        // The heading folds away with the labels.
        let (head, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0 * (1.0 - t)), Sense::hover());
        if a > 0.0 {
            ui.painter().text(head.left_top(), Align2::LEFT_TOP, "PLAYLISTS", theme::semibold(11.5), theme::with_alpha(theme::TEXT_FAINT, a));
        }
        let bars = if t > 0.0 { egui::scroll_area::ScrollBarVisibility::AlwaysHidden } else { egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded };
        egui::ScrollArea::vertical().id_salt("sidebar-pl").auto_shrink(false).scroll_bar_visibility(bars).show(ui, |ui| {
            // Last launch's list until the library arrives.
            let Some(playlists) = app.library.as_ref().map(|l| &l.playlists).or(app.sidebar_cache.as_ref()) else {
                // The spinner animates every frame: give up on a request that never answered.
                if app.library_requested.elapsed() < std::time::Duration::from_secs(20) {
                    ui.horizontal(|ui| {
                        ui.add_space(ICON_X - 8.0);
                        ui.spinner();
                    });
                } else if t < 0.5 {
                    ui.multiply_opacity(1.0 - 2.0 * t);
                    ui.label(egui::RichText::new("Couldn't load your playlists").color(theme::text_dim()));
                    ui.add_space(8.0);
                    if widgets::pill(ui, "Try again", None, false).clicked() {
                        app.request_library();
                    }
                }
                return;
            };
            for c in playlists {
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 48.0), Sense::click());
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                let mut resp = named(resp, &c.title).on_hover_cursor(egui::CursorIcon::PointingHand);
                if collapsed {
                    resp = resp.on_hover_text(&c.title);
                }
                let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
                let selected = matches!(&c.target, Target::Playlist(id) if app.page == Page::Playlist(id.clone()));
                ui.painter().rect_filled(rect, 8.0, theme::glass(0.05 * h + if selected { 0.08 } else { 0.0 }));
                let img = Rect::from_center_size(pos2(rect.left() + ICON_X, rect.center().y), Vec2::splat(36.0));
                paint_cover(ui, img, &c.thumbs, theme::thumb_radius(), 1.0);
                if a > 0.0 {
                    let w = full_w - 58.0;
                    text_at(ui, pos2(img.right() + 10.0, rect.top() + 7.0), &c.title, theme::semibold(13.5), theme::with_alpha(theme::TEXT, a), w);
                    text_at(ui, pos2(img.right() + 10.0, rect.top() + 26.0), &c.subtitle, theme::regular(12.0), theme::with_alpha(theme::text_dim(), a), w);
                }
                if resp.clicked() {
                    app.actions.push(Action::Open(c.target.clone()));
                }
            }
        });
    });
    let ready = app.updater.as_ref().and_then(|u| match u.state() {
        crate::update::State::Ready { version, .. } => Some(version),
        _ => None,
    });
    let mut restart = false;
    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
        nav(ui, icon::SETTINGS, "Settings", Page::Settings, p == Page::Settings, &mut go_to);
        if transferring {
            super::transfer::sidebar_card(app, ui, collapsed, a, ICON_X);
        }
        if let Some(v) = &ready {
            ui.add_space(6.0);
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::click());
            let mut resp = named(resp, "Restart to update").on_hover_cursor(egui::CursorIcon::PointingHand);
            if collapsed {
                resp = resp.on_hover_text(format!("Restart to update to ytm-rs {v}"));
            }
            let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
            ui.painter().rect_filled(rect, 12.0, theme::with_alpha(look.accent, 0.22 + 0.12 * h));
            icon_at(ui, pos2(rect.left() + ICON_X, rect.center().y), icon::SPARKLES, 18.0, Color32::WHITE);
            if a > 0.0 {
                ui.painter().text(pos2(rect.left() + 46.0, rect.top() + 17.0), Align2::LEFT_CENTER, "Restart to update", theme::semibold(13.5), theme::with_alpha(Color32::WHITE, a));
                ui.painter().text(pos2(rect.left() + 46.0, rect.top() + 35.0), Align2::LEFT_CENTER, format!("ytm-rs {v}"), theme::regular(12.0), theme::with_alpha(Color32::WHITE, 0.75 * a));
            }
            restart = resp.clicked();
        }
    });
    if restart {
        app.restart_to_update(ui.ctx());
    }
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
        // Anywhere on the pill focuses the box, not just on the text area inside it.
        let (rect, field) = ui.allocate_exact_size(vec2(width, 42.0), Sense::click());
        let field = field.on_hover_cursor(egui::CursorIcon::Text);
        let focus = theme::anim_bool(ui.ctx(), "search-focus", app.search_focused, 0.2);
        ui.painter().rect_filled(rect, 21.0, theme::glass(0.09 + 0.05 * focus));
        if focus > 0.0 {
            ui.painter().rect_stroke(rect, 21.0, egui::Stroke::new(1.0, theme::glass(0.25 * focus)), egui::StrokeKind::Inside);
        }
        icon_at(ui, pos2(rect.left() + 22.0, rect.center().y), icon::SEARCH, 17.0, theme::text_dim());
        let clearable = !app.search_text.is_empty();
        let inner = Rect::from_min_max(pos2(rect.left() + 42.0, rect.top() + 4.0), pos2(rect.right() - if clearable { 46.0 } else { 16.0 }, rect.bottom() - 4.0));
        // Up and Down walk the suggestions (past either end back to what was typed). Taken
        // before the box sees them, where they'd move the caret. Only bare arrows (Shift
        // selects text), and only suggestions for what's typed now, not ones still being replaced.
        let fresh = app.suggestions.0 == app.search_text.trim();
        let n = if fresh { app.suggestions.1.len() } else { 0 };
        if app.search_focused && n > 0 && ui.input(|i| i.modifiers.is_none()) {
            ui.input_mut(|i| {
                if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    app.suggest_sel = match app.suggest_sel {
                        None => Some(0),
                        Some(s) if s + 1 < n => Some(s + 1),
                        Some(_) => None,
                    };
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    app.suggest_sel = match app.suggest_sel {
                        None => Some(n - 1),
                        Some(0) => None,
                        Some(s) => Some(s - 1),
                    };
                }
            });
        }
        let edit = ui.put(
            inner,
            egui::TextEdit::singleline(&mut app.search_text)
                .hint_text(egui::RichText::new("Search songs, albums, artists, podcasts").color(theme::TEXT_FAINT))
                .font(theme::regular(15.0))
                .frame(egui::Frame::NONE)
                .align(egui::Align2::LEFT_CENTER)
                .desired_width(inner.width()),
        );
        app.search_rect = Some(rect);
        if std::mem::take(&mut app.search_refocus) {
            edit.request_focus();
        }
        app.search_focused = edit.has_focus();
        if edit.gained_focus() && app.search_text.trim().is_empty() && app.logged_in {
            app.suggest_due = Some(ui.input(|i| i.time));
        }
        if clearable {
            let x = Rect::from_center_size(pos2(rect.right() - 25.0, rect.center().y), Vec2::splat(30.0));
            let resp = named(ui.interact(x, ui.scope_id().with("search-clear"), Sense::click()), "Clear search").on_hover_cursor(egui::CursorIcon::PointingHand);
            let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
            if h > 0.0 {
                ui.painter().circle_filled(x.center(), 15.0, theme::glass(0.10 * h));
            }
            icon_at(ui, x.center(), icon::X, 16.0, theme::lerp_color(theme::text_dim(), theme::TEXT, h));
            if resp.clicked() {
                app.search_text.clear();
                app.suggestions = Default::default();
                app.suggest_sel = None;
                app.suggest_due = Some(ui.input(|i| i.time));
                edit.request_focus();
            }
        }
        if field.clicked() || ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::F)) {
            edit.request_focus();
        }
        if edit.changed() {
            app.suggest_sel = None;
            let now = ui.input(|i| i.time);
            app.suggest_due = Some(now + 0.22);
            if app.search_text.trim().is_empty() {
                app.suggestions = Default::default();
            }
        }
        if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let picked = app.suggest_sel.filter(|_| fresh).and_then(|s| app.suggestions.1.get(s)).map(|s| s.text.clone());
            let q = picked.unwrap_or_else(|| app.search_text.trim().to_owned());
            if !q.is_empty() {
                app.suggestions = Default::default();
                app.actions.push(Action::Go(Page::Search(q)));
            }
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(20.0);
            let (r, resp) = ui.allocate_exact_size(Vec2::splat(36.0), Sense::click());
            let resp = named(resp, "Account").on_hover_cursor(egui::CursorIcon::PointingHand);
            let initial = app.account_name.as_deref().and_then(|n| n.chars().next()).unwrap_or('?');
            if app.logged_in {
                ui.painter().circle_filled(r.center(), 18.0, theme::lerp_color(app.accent_color(), Color32::BLACK, 0.35));
                if let Some(avatar) = &app.account_avatar {
                    widgets::paint_remote(ui, avatar, r, 18.0, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                } else {
                    ui.painter().text(r.center(), Align2::CENTER_CENTER, initial, theme::bold(16.0), Color32::WHITE);
                }
            } else {
                ui.painter().circle_filled(r.center(), 18.0, theme::glass(0.12));
                icon_at(ui, r.center(), icon::USER, 18.0, theme::text_dim());
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
    // Through a press on a remove button too: its release briefly takes the box's focus.
    let show = (app.search_focused || app.forget_held) && !app.suggestions.1.is_empty() && !app.np_open;
    let t = theme::anim_bool(ctx, "suggest-open", show, 0.15);
    if t <= 0.0 {
        return;
    }
    egui::Area::new(egui::Id::unique("suggestions"))
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
                    if !ui.input(|i| i.pointer.any_down() || i.pointer.any_released()) {
                        app.forget_held = false;
                    }
                    // Releasing that press is a click outside the box, which takes its focus
                    // (and this list) away: give it back until the press is over.
                    if app.forget_held {
                        app.search_refocus = true;
                    }
                    let now = ui.input(|i| i.time);
                    for (i, s) in app.suggestions.1.clone().into_iter().enumerate() {
                        let term = s.text;
                        // The removed one folds away, and the rows below slide up into its place.
                        if let Some((_, at)) = app.forgetting.as_ref().filter(|(t, _)| s.forget.as_deref() == Some(t.as_str())) {
                            let p = ((now - at) / 0.18).clamp(0.0, 1.0) as f32;
                            let e = 1.0 - (1.0 - p).powi(3);
                            let gap = ui.spacing().item_spacing.y;
                            // Its spacing folds away with it, so nothing jumps at the end.
                            ui.spacing_mut().item_spacing.y = 0.0;
                            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), (40.0 + gap) * (1.0 - e)), Sense::hover());
                            ui.spacing_mut().item_spacing.y = gap;
                            ui.scope(|ui| {
                                ui.set_clip_rect(r.intersect(ui.clip_rect()));
                                ui.set_opacity(1.0 - e);
                                let cy = r.top() + 20.0;
                                icon_at(ui, pos2(r.left() + 20.0, cy), icon::HISTORY, 15.0, theme::text_dim());
                                suggestion_text(ui, pos2(r.left() + 42.0, cy - 9.0), &term, &app.suggestions.0, r.width() - 88.0);
                            });
                            ui.ctx().request_repaint();
                            continue;
                        }
                        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
                        let resp = named(resp, &term).on_hover_cursor(egui::CursorIcon::PointingHand);
                        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.contains_pointer() || app.suggest_sel == Some(i), 0.1);
                        ui.painter().rect_filled(r, 8.0, theme::glass(0.08 * h));
                        let glyph = if s.forget.is_some() { icon::HISTORY } else { icon::SEARCH };
                        icon_at(ui, pos2(r.left() + 20.0, r.center().y), glyph, 15.0, theme::text_dim());
                        let room = if s.forget.is_some() { 88.0 } else { 52.0 };
                        suggestion_text(ui, pos2(r.left() + 42.0, r.center().y - 9.0), &term, &app.suggestions.0, r.width() - room);
                        // A past search can be removed from the account's history.
                        if let Some(token) = s.forget {
                            let x = Rect::from_center_size(pos2(r.right() - 22.0, r.center().y), Vec2::splat(30.0));
                            let xr = named(ui.interact(x, resp.id.with("forget"), Sense::click()), "Remove from recent searches")
                                .on_hover_text("Remove")
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            let xh = theme::anim_bool(ui.ctx(), xr.id.with("h"), xr.hovered(), 0.1);
                            if xh > 0.0 {
                                ui.painter().circle_filled(x.center(), 15.0, theme::glass(0.12 * xh));
                            }
                            icon_at(ui, x.center(), icon::TRASH_2, 15.0, theme::lerp_color(theme::text_dim(), theme::TEXT, xh));
                            // On the press, and one per press: the next row's button slides
                            // under the held pointer.
                            if (xr.is_pointer_button_down_on() || xr.clicked()) && !app.forget_held {
                                app.forget_held = true;
                                // One still folding away is done.
                                if let Some((t, _)) = app.forgetting.take() {
                                    app.suggestions.1.retain(|s| s.forget.as_deref() != Some(t.as_str()));
                                }
                                app.forgetting = Some((token.clone(), now));
                                app.suggest_sel = None;
                                app.suggest_round += 1;
                                app.forgets_pending += 1;
                                app.search_refocus = true;
                                app.backend.request(crate::backend::Req::ForgetSearch(token));
                            }
                        }
                        // Pointer-down, not click: the text box loses focus (hiding this) first.
                        if resp.is_pointer_button_down_on() || resp.clicked() {
                            app.suggestions = Default::default();
                            app.search_focused = false;
                            app.actions.push(Action::Go(Page::Search(term)));
                            break;
                        }
                    }
                    if let Some((t, _)) = app.forgetting.take_if(|(_, at)| now - *at >= 0.18) {
                        app.suggestions.1.retain(|s| s.forget.as_deref() != Some(t.as_str()));
                    }
                });
        });
}

/// A suggestion with what's typed in bold and the rest dimmed, as on music.youtube.com.
fn suggestion_text(ui: &Ui, pos: Pos2, text: &str, typed: &str, width: f32) {
    let n = text.chars().zip(typed.chars()).take_while(|(a, b)| a.to_lowercase().eq(b.to_lowercase())).count();
    let split = if n == typed.chars().count() { text.char_indices().nth(n).map_or(text.len(), |(i, _)| i) } else { 0 };
    let (bold, rest) = text.split_at(split);
    let mut job = egui::text::LayoutJob::default();
    job.append(bold, 0.0, egui::TextFormat { font_id: theme::semibold(15.0), color: theme::TEXT, ..Default::default() });
    job.append(rest, 0.0, egui::TextFormat { font_id: theme::regular(15.0), color: theme::text_dim(), ..Default::default() });
    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(10.0));
    let g = ui.fonts_mut(|f| f.layout_job(job));
    ui.painter().galley(pos, g, theme::TEXT);
}

// ------------------------------------------------------------------ player bar

pub fn player_bar(app: &mut App, ui: &mut Ui, now: f64) {
    let full = ui.max_rect();
    // A floating bar is rounded: keep the progress line clear of its corners.
    let inset = if app.look().chrome.player_bar == crate::preset::PlayerBar::Floating { app.look().chrome.radius.clamp(10.0, 26.0) } else { 0.0 };
    let track_rect = full.shrink2(vec2(inset, 0.0));
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
    let bar_hit = Rect::from_min_size(track_rect.min - vec2(0.0, 6.0), vec2(track_rect.width(), 14.0));
    let resp = ui.interact(bar_hit, ui.scope_id().with("progress"), Sense::click_and_drag());
    let resp = if total > 0.0 { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
    resp.widget_info(|| egui::WidgetInfo::slider(total > 0.0, pos as f64, "Seek"));
    let h = theme::anim_bool(ui.ctx(), "progress-h", resp.hovered() || resp.dragged(), 0.15);
    let thick = 3.0 + 2.0 * h;
    let line = Rect::from_min_size(track_rect.min, vec2(track_rect.width(), thick));
    ui.painter().rect_filled(line, 0.0, theme::glass(0.12));
    let frac = if total > 0.0 { pos / total } else { 0.0 };
    let filled = Rect::from_min_size(line.min, vec2(line.width() * frac, thick));
    ui.painter().rect_filled(filled, 0.0, app.accent_color());
    if h > 0.0 && total > 0.0 {
        // The knob and the time tip stick out above the bar, over now playing (a higher
        // layer than the bar), so they go on a layer above both and outside the bar's clip.
        let over = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, ui.scope_id().with("seek-over")));
        over.circle_filled(pos2(filled.right(), line.center().y), 7.0 * h, app.accent_color());
        if let Some(p) = resp.hover_pos() {
            let t = ((p.x - full.left()) / full.width()).clamp(0.0, 1.0) * total;
            let tip = pos2(p.x, line.top() - 16.0);
            let g = widgets::galley(ui, &fmt_time(t as u32), theme::semibold(12.0), Color32::WHITE, 80.0);
            let bg = Rect::from_center_size(tip, g.size() + vec2(12.0, 6.0));
            over.rect_filled(bg, 6.0, theme::shade(0.75));
            over.galley(bg.min + vec2(6.0, 3.0), g, Color32::WHITE);
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

    // Narrow windows give way in steps so nothing overlaps: the transport tightens, the
    // volume slider folds into its icon (hover to show it), then the time goes (the
    // progress line still shows it on hover). The title shortens to keep clear on the left.
    let time_g = (total > 0.0).then(|| {
        let txt = format!("{} / {}", fmt_time(pos as u32), fmt_time(total as u32));
        widgets::galley(ui, &txt, theme::regular(13.0), theme::text_dim(), 200.0)
    });
    let time_w = time_g.as_ref().map_or(0.0, |g| g.size().x);
    // Room between the repeat button's edge (plus a gap) and the lyrics/queue/expand buttons.
    // The volume with its slider takes 140 px plus the time; the icon alone, 36.
    let free = |half: f32| row.right() - 120.0 - (row.center().x + half + 12.0);
    let tight = free(141.0) < 140.0 + time_w;
    let half = if tight { 125.0 } else { 141.0 };
    let compact_vol = free(half) < 140.0 + time_w;
    let show_time = free(half) >= if compact_vol { 36.0 } else { 140.0 } + time_w;

    // Left: now playing
    if let Some(t) = &track {
        let art = Rect::from_center_size(pos2(row.left() + 30.0, cy), Vec2::splat(56.0));
        let ar = ui.interact(art, ui.scope_id().with("np-art"), Sense::click());
        let ar = named(ar, "Open now playing").on_hover_cursor(egui::CursorIcon::PointingHand);
        let ah = theme::anim_bool(ui.ctx(), ar.id.with("h"), ar.hovered(), 0.15);
        paint_cover(ui, art, &t.thumbs, theme::thumb_radius() + 1.0, 1.0);
        if ah > 0.0 {
            ui.painter().rect_filled(art, theme::thumb_radius() + 1.0, theme::shade(0.45 * ah));
            icon_at(ui, art.center(), icon::CHEVRON_UP, 22.0, theme::with_alpha(Color32::WHITE, ah));
        }
        if ar.clicked() {
            app.actions.push(Action::OpenNowPlaying(true));
        }
        let x = art.right() + 14.0;
        // Title, artists and the heart (about 48 px) end before the shuffle button.
        let room = row.center().x - half - 18.0 - 14.0 - 48.0 - x;
        let w = (row.width() * 0.3 - 110.0).max(80.0).min(room).max(40.0);
        let tr = text_at(ui, pos2(x, cy - 19.0), &t.title, theme::semibold(15.0), theme::TEXT, w);
        let tresp = ui.interact(tr, ui.scope_id().with("np-title"), Sense::click());
        if named(tresp, &t.title).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            app.actions.push(Action::OpenNowPlaying(true));
        }
        let mut ax = x;
        for (i, a) in t.artists.iter().enumerate() {
            let s = if i + 1 < t.artists.len() { format!("{}, ", a.name) } else { a.name.clone() };
            let r = text_at(ui, pos2(ax, cy + 2.0), &s, theme::regular(13.5), theme::text_dim(), (x + w - ax).max(10.0));
            if let Some(id) = &a.id {
                let resp = ui.interact(r, ui.scope_id().with(("artist", i)), Sense::click());
                let resp = named(resp, &a.name).on_hover_cursor(egui::CursorIcon::PointingHand);
                if resp.hovered() {
                    ui.painter().hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0, theme::text_dim()));
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
        let lresp = ui.interact(lr, ui.scope_id().with("like"), Sense::click());
        let lresp = named(lresp, if liked { "Remove like" } else { "Like" }).on_hover_text(if liked { "Remove like" } else { "Like" }).on_hover_cursor(egui::CursorIcon::PointingHand);
        let lh = theme::anim_bool(ui.ctx(), lresp.id.with("h"), lresp.hovered(), 0.15);
        let pop = theme::anim_bool(ui.ctx(), ("liked", &t.id), liked, 0.3);
        ui.painter().circle_filled(lpos, 18.0, theme::glass(0.1 * lh));
        let size = 19.0 * (1.0 + 0.25 * (pop * std::f32::consts::PI).sin());
        let idle = theme::lerp_color(theme::text_dim(), theme::TEXT, lh);
        let outline = theme::lerp_color(idle, theme::RED, pop);
        widgets::heart(ui, lpos, size, pop, outline, theme::RED);
        if lresp.clicked() {
            app.actions.push(Action::Like(t.clone(), !liked));
        }
    }

    // Centre: transport
    let accent = app.accent_color();
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
    let (outer, inner) = if tight { (108.0, 56.0) } else { (124.0, 64.0) };
    if ctl(ui, -outer, icon::SHUFFLE, "Shuffle up next", false, 18.0) {
        app.shuffle_upcoming();
    }
    if ctl(ui, -inner, icon::SKIP_BACK, "Previous", false, 22.0) {
        app.prev();
    }
    let (rep_icon, rep_on) = match app.repeat {
        Repeat::Off => (icon::REPEAT, false),
        Repeat::All => (icon::REPEAT, true),
        Repeat::One => (icon::REPEAT_1, true),
    };
    if ctl(ui, inner, icon::SKIP_FORWARD, "Next", false, 22.0) {
        app.next();
    }
    if ctl(ui, outer, rep_icon, "Repeat", rep_on, 18.0) {
        app.repeat = match app.repeat {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        };
    }
    let pr = Rect::from_center_size(center, Vec2::splat(46.0));
    let presp = ui.interact(pr, ui.scope_id().with("play"), Sense::click());
    let presp = named(presp, if playing { "Pause" } else { "Play" }).on_hover_cursor(egui::CursorIcon::PointingHand);
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
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
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
    // In now playing the queue is its Up next panel; elsewhere, the side panel.
    let queue_open = if app.np_open { app.np_tab == NpTab::UpNext } else { app.queue_open };
    if right_btn(ui, icon::LIST_MUSIC, "Queue", queue_open) {
        if app.np_open {
            app.np_tab = if queue_open { NpTab::Art } else { NpTab::UpNext };
        } else {
            app.queue_open = !app.queue_open;
        }
    }
    // Not lit for a song without lyrics: now playing shows its artwork instead.
    let lyrics_on = app.np_open && app.np_tab == NpTab::Lyrics && !app.no_lyrics();
    if right_btn(ui, icon::MIC_VOCAL, "Lyrics", lyrics_on) {
        if lyrics_on {
            app.np_open = false;
        } else {
            app.np_tab = NpTab::Lyrics;
            app.actions.push(Action::OpenNowPlaying(true));
        }
    }
    // Volume: a slider beside its icon, or in a narrow window the icon alone with the
    // slider in a small popup above it while the pointer is over either.
    let vol = app.audio.volume();
    let vi = if compact_vol {
        Rect::from_center_size(pos2(rx - 16.0, cy), Vec2::splat(32.0))
    } else {
        let slider = Rect::from_min_size(pos2(rx - 96.0, cy - 10.0), vec2(90.0, 20.0));
        volume_slider(app, ui, slider, "vol");
        Rect::from_center_size(pos2(slider.left() - 18.0, cy), Vec2::splat(32.0))
    };
    let vglyph = if vol == 0.0 { icon::VOLUME_X } else if vol < 0.5 { icon::VOLUME_1 } else { icon::VOLUME_2 };
    let mresp = named(ui.interact(vi, ui.scope_id().with("mute"), Sense::click()), "Mute").on_hover_cursor(egui::CursorIcon::PointingHand);
    icon_at(ui, vi.center(), vglyph, 18.0, if mresp.hovered() { theme::TEXT } else { theme::text_dim() });
    if compact_vol {
        let pop_id = ui.scope_id().with("vol-pop");
        let pop = Rect::from_center_size(pos2(vi.center().x, full.top() - 30.0), vec2(140.0, 40.0));
        // Stays open across the gap between icon and popup, and while dragging.
        let keep = ui.ctx().pointer_hover_pos().is_some_and(|p| pop.union(vi).expand(4.0).contains(p))
            || ui.input(|i| i.pointer.any_down());
        let open = mresp.hovered() || (ui.data(|d| d.get_temp::<bool>(pop_id).unwrap_or(false)) && keep);
        ui.data_mut(|d| d.insert_temp(pop_id, open));
        let a = theme::anim_bool(ui.ctx(), pop_id.with("a"), open, 0.12);
        if a > 0.0 {
            egui::Area::new(pop_id).order(egui::Order::Foreground).fixed_pos(pop.min).interactable(open).show(ui.ctx(), |ui| {
                ui.set_opacity(a);
                let (r, _) = ui.allocate_exact_size(pop.size(), Sense::hover());
                ui.painter().rect_filled(r, 20.0, Color32::from_rgb(38, 34, 42));
                ui.painter().rect_stroke(r, 20.0, egui::Stroke::new(1.0, theme::glass(0.08)), egui::StrokeKind::Inside);
                volume_slider(app, ui, Rect::from_center_size(r.center(), vec2(r.width() - 32.0, 20.0)), "s");
            });
        }
        // Scrolling over the icon still sets the volume.
        let scroll = if mresp.hovered() { ui.input(|i| i.smooth_scroll_delta.y) } else { 0.0 };
        if scroll != 0.0 {
            app.audio.set_volume((vol + scroll / 600.0).clamp(0.0, 1.0));
        }
    }
    if mresp.clicked() {
        let stash = ui.scope_id().with("unmute");
        if vol > 0.0 {
            ui.data_mut(|d| d.insert_temp(stash, vol));
            app.audio.set_volume(0.0);
        } else {
            let v = ui.data(|d| d.get_temp::<f32>(stash)).unwrap_or(0.6);
            app.audio.set_volume(v);
        }
    }
    if let Some(g) = time_g.filter(|_| show_time) {
        let gap = if compact_vol { 4.0 } else { 10.0 };
        ui.painter().galley(pos2(vi.left() - gap - g.size().x, cy - g.size().y / 2.0), g, theme::text_dim());
    }
}

/// The volume track: click or drag to set, scroll over it to nudge.
fn volume_slider(app: &mut App, ui: &mut Ui, slider: Rect, salt: &str) {
    let vol = app.audio.volume();
    let vresp = ui.interact(slider, ui.scope_id().with(salt), Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::PointingHand);
    vresp.widget_info(|| egui::WidgetInfo::slider(true, vol as f64, "Volume"));
    let vh = theme::anim_bool(ui.ctx(), ui.scope_id().with((salt, "h")), vresp.hovered() || vresp.dragged(), 0.15);
    let track_r = Rect::from_center_size(slider.center(), vec2(slider.width(), 4.0));
    ui.painter().rect_filled(track_r, 2.0, theme::glass(0.15));
    let fill = Rect::from_min_size(track_r.min, vec2(track_r.width() * vol, 4.0));
    ui.painter().rect_filled(fill, 2.0, theme::lerp_color(theme::text_dim(), Color32::WHITE, vh));
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
    let rows = super::pages::Rows::new(app, ui.input(|i| i.time));
    egui::ScrollArea::vertical().id_salt(("queue", salt)).auto_shrink(false).show(ui, |ui| {
        if app.queue.is_empty() {
            ui.label(egui::RichText::new("Nothing queued yet").color(theme::text_dim()));
            return;
        }
        let start = app.index.unwrap_or(0);
        ui.checkbox(&mut app.autoplay, egui::RichText::new("Autoplay similar songs").color(theme::text_dim())).on_hover_cursor(egui::CursorIcon::PointingHand);
        ui.add_space(6.0);
        let (queue, liked, acts, has_current, by) = (&app.queue, &app.liked, &mut app.actions, app.index.is_some(), &app.jam.by);
        widgets::visible_rows(ui, queue.len() - start, |ui, k| {
            let i = start + k;
            let t = &queue[i];
            // The first row is the current song.
            let current = k == 0 && has_current;
            let o = widgets::RowOpts { current, added_by: by.get(&t.id).cloned(), ..rows.opts(liked, t, None, false) };
            let resp = widgets::track_row(ui, t, &o, acts);
            if resp.clicked() {
                // The playing row pauses/resumes instead of doing nothing.
                acts.push(if current { Action::TogglePlay } else { Action::QueueJump(i) });
            }
            if !current && ui.rect_contains_pointer(Rect::from_min_max(resp.rect.min, pos2(resp.rect.right() + 42.0, resp.rect.bottom()))) {
                // Remove button over the duration column.
                let x = Rect::from_center_size(resp.rect.right_center() - vec2(26.0, 0.0), Vec2::splat(28.0));
                let xr = named(ui.interact(x, resp.id.with("rm"), Sense::click()), "Remove from queue").on_hover_cursor(egui::CursorIcon::PointingHand);
                ui.painter().circle_filled(x.center(), 14.0, theme::shade(0.6));
                icon_at(ui, x.center(), icon::X, 14.0, theme::TEXT);
                if xr.clicked() {
                    acts.push(Action::QueueRemove(i));
                }
            }
        });
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
    egui::Area::new(egui::Id::unique("toast"))
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
    // Frames only while it fades in or out; in between it just waits.
    if (0.25..4.1).contains(&age) {
        ctx.request_repaint_after(std::time::Duration::from_secs_f32(4.1 - age));
    } else {
        ctx.request_repaint();
    }
}
