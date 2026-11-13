//! App chrome: background, sidebar, top bar, player bar, queue drawer, toasts.

use egui::{Align2, Color32, Pos2, Rect, Sense, Ui, Vec2, pos2, vec2};

use super::widgets::{self, icon_at, icon_button, named, paint_cover, text_at};
use super::{Action, App, LibTab, NpTab, Page, Repeat, fmt_time, row_drag};
use crate::model::Target;
use crate::shader::Params;
use crate::theme::{self, icon};

// ------------------------------------------------------------------ background

/// Vertical gradient (top colour → bottom colour) over `rect`.
use egui::lerp;

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
    // A theme file's background is the page.
    if theme::file_background().is_some() {
        return BgMode::Solid;
    }
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
/// Moves the animated background's clock on by a frame. Now playing's backdrop runs on
/// it too, also where the pages' background isn't drawn (full screen).
pub fn background_clock(app: &mut App, now: f64) {
    use crate::preset::BgMode;
    let dt = (now - app.last_frame).clamp(0.0, 0.1);
    app.last_frame = now;
    let b = &app.look().background;
    let animated = matches!(b.mode, BgMode::Kawarp | BgMode::CoverFullbleed) && b.speed > 0.0;
    if app.anim_bg && animated && app.playing() {
        // `audioSpeedMultiplier`: louder music moves it faster.
        let boost = if app.reactive_bg { 1.0 + app.audio.status.level().min(1.0) * b.audio_speed } else { 1.0 };
        app.bg_time += dt * b.speed as f64 * boost as f64;
    }
}

/// The background's flat colour, under its art and gradients.
pub fn base_color(app: &App) -> Color32 {
    // A theme file's background is the page's flat colour.
    if theme::file_background().is_some() { theme::bg() } else { app.look().solid() }
}

/// The docked or floating player bar's fill at `alpha`.
pub fn bar_fill(look: &crate::preset::Preset, alpha: f32) -> Color32 {
    match crate::preset::hex(&look.chrome.bar_color).filter(|_| !theme::light()) {
        Some(c) => theme::with_alpha(c, alpha),
        None => theme::panel(alpha),
    }
}

pub fn background(app: &mut App, ui: &mut Ui, screen: Rect, now: f64, only: Option<Rect>) {
    use crate::preset::BgMode;
    background_clock(app, now);
    let look = app.look();
    let b = &look.background;
    let mode = page_mode(look);
    // Light pages are the art at full strength under a white wash, a pale tint of the cover.
    let light = theme::light();
    let dim = if light { 1.0 } else { look.app.page_dim.unwrap_or(b.page_dim).clamp(0.02, 1.0) };
    let painter = match only {
        Some(r) => ui.painter().with_clip_rect(r),
        None => ui.painter().clone(),
    };
    // Windows' material shows through instead of the theme's colour, lightly tinted.
    if crate::backdrop::shows() {
        painter.rect_filled(screen, 0.0, if light { Color32::from_white_alpha(40) } else { Color32::from_black_alpha(40) });
        return;
    }
    painter.rect_filled(screen, 0.0, base_color(app));
    let art = matches!(mode, BgMode::Kawarp | BgMode::CoverStatic | BgMode::CoverFullbleed);
    if let Some(bg) = app.bg.as_ref().filter(|_| art) {
        let blend = theme::ease_in_out(((now - app.art_changed - b.crossfade_delay as f64) / b.crossfade_secs.max(0.05) as f64) as f32);
        if blend < 1.0 {
            // The crossfade to new art finishes even while paused.
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
        }
        let level = if app.reactive_bg && app.playing() { (app.audio.status.level() - b.beat_threshold).max(0.0) * b.reactive } else { 0.0 };
        let still = mode == BgMode::CoverStatic;
        bg.paint(
            &painter,
            screen,
            Params {
                time: app.bg_time as f32,
                blend,
                intensity: if still { 0.0 } else { b.intensity * 0.9 },
                saturation: look.app.page_saturation.unwrap_or(b.saturation * 0.9),
                dim,
                scale: look.app.page_scale.unwrap_or((b.scale - 0.08).max(1.0)) + if still { 0.0 } else { level.min(0.5) * 0.08 },
                contrast: look.app.page_contrast,
                dither: b.dither,
                rotate: 0.0,
                alpha: 1.0,
            },
        );
    } else if mode == BgMode::Solid {
        // Plain colour: nothing over it.
    } else {
        vgradient(&painter, screen, theme::with_alpha(app.accent_color(), 0.18 * dim / 0.5), look.solid());
    }
    if light {
        // A solid theme's light colour needs no wash.
        if mode != BgMode::Solid {
            painter.rect_filled(screen, 0.0, Color32::from_white_alpha(212));
        }
    } else if mode != BgMode::Solid {
        // Keep text readable: darken toward the bottom and under the sidebar.
        vgradient(&painter, screen, theme::shade(0.12), theme::shade(0.55));
    }
    // A theme's own gradients over the pages.
    // A wash meant for dark pages would only dirty light ones.
    if let Some([a, px]) = look.app.page_wash.filter(|_| !light) {
        let c = crate::preset::paint(&look.app.page_wash_color, app.accent_color());
        vgradient(&painter, Rect::from_min_size(screen.min, vec2(screen.width(), px)), theme::with_alpha(c, a), Color32::TRANSPARENT);
    }
    if let Some([a, px]) = look.app.page_side.filter(|_| !light) {
        let c = crate::preset::paint(&look.app.page_wash_color, app.accent_color());
        hgradient(&painter, Rect::from_min_size(screen.min, vec2(px, screen.height())), theme::with_alpha(c, a), Color32::TRANSPARENT);
    }
    if let Some([a, px]) = look.app.page_foot.filter(|_| !light) {
        vgradient(&painter, Rect::from_min_max(pos2(screen.left(), screen.bottom() - px), screen.max), Color32::TRANSPARENT, theme::shade(a));
    }
}

/// A theme's soft light: an ellipse of colour fading out. Six triangles' worth of rings,
/// so it costs next to nothing to draw.
pub fn overlay_light(painter: &egui::Painter, area: Rect, o: &crate::preset::Overlay, accent: Color32) {
    const STEPS: usize = 40;
    const RINGS: [(f32, f32); 4] = [(0.0, 1.0), (0.4, 0.55), (0.72, 0.18), (1.0, 0.0)];
    let color = theme::with_alpha(crate::preset::paint(&o.color, accent), o.alpha);
    let c = area.min + vec2(area.width() * o.x, area.height() * o.y);
    let (rx, ry) = (area.width() * o.rx, area.height() * o.ry);
    let mut m = egui::Mesh::default();
    for &(t, a) in &RINGS {
        for i in 0..STEPS {
            let (sin, cos) = (i as f32 / STEPS as f32 * std::f32::consts::TAU).sin_cos();
            m.colored_vertex(c + vec2(cos * rx, sin * ry) * t, color.gamma_multiply(a));
        }
    }
    let at = |ring: usize, i: usize| (ring * STEPS + i % STEPS) as u32;
    for ring in 0..RINGS.len() - 1 {
        for i in 0..STEPS {
            m.add_triangle(at(ring, i), at(ring, i + 1), at(ring + 1, i));
            m.add_triangle(at(ring, i + 1), at(ring + 1, i + 1), at(ring + 1, i));
        }
    }
    painter.add(m);
}

// ------------------------------------------------------------------ sidebar

/// The ☰ button at the top of the sidebar: collapses it to an icon rail and back.
fn menu_button(app: &mut App, ui: &mut Ui) {
    let (r, resp) = ui.allocate_exact_size(vec2(40.0, 32.0), Sense::click());
    let label = if is_rail(app) { "Expand sidebar" } else { "Collapse sidebar" };
    let resp = named(resp, label).on_hover_text(label).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
    ui.painter().circle_filled(r.center(), 18.0, theme::glass(0.08 * h));
    let color = theme::lerp_color(theme::text_dim(), theme::text(), h);
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
    hover: f32,
    selected: f32,
    outline: f32,
    selected_fill: Option<Color32>,
    bar_px: f32,
}

impl NavLook {
    fn of(app: &App) -> Self {
        let i = &app.look().app.nav_item;
        Self { style: app.look().app.nav, radius: i.radius.unwrap_or((app.look().chrome.radius * 0.6).clamp(4.0, 16.0)), accent: app.accent_color(), hover: i.hover, selected: if theme::light() && !i.selected_fill.is_empty() { 0.12 } else { i.selected }, outline: app.look().app.controls.ghost_outline, selected_fill: crate::preset::hex(&i.selected_fill).filter(|_| !theme::light()), bar_px: i.bar_px }
    }
}

/// A nav item's background in the theme's nav style; returns its icon colour.
/// `h` and `s` are the hover and selected animations (0..1).
fn nav_bg(ui: &Ui, look: NavLook, rect: Rect, h: f32, s: f32) -> Color32 {
    use crate::preset::NavStyle;
    let (nav_r, accent) = (look.radius, look.accent);
    let icon_fg = theme::lerp_color(theme::text_dim(), theme::text(), (h + s).min(1.0));
    if look.outline > 0.0 && h + s > 0.0 {
        ui.painter().rect_stroke(rect, nav_r, egui::Stroke::new(1.0, theme::glass(look.outline * (h + s).min(1.0))), egui::StrokeKind::Inside);
    }
    match look.style {
        NavStyle::Text => {
            ui.painter().rect_filled(rect, nav_r, theme::glass(look.hover * h));
            theme::lerp_color(icon_fg, accent, s)
        }
        NavStyle::Inverse => {
            ui.painter().rect_filled(rect, nav_r, theme::glass(look.hover * h));
            ui.painter().rect_filled(rect, nav_r, theme::with_alpha(theme::text(), s));
            theme::lerp_color(icon_fg, theme::on_text(), s)
        }
        NavStyle::Pill => {
            match look.selected_fill {
                Some(c) => {
                    ui.painter().rect_filled(rect, nav_r, theme::glass(look.hover * h));
                    ui.painter().rect_filled(rect, nav_r, theme::with_alpha(c, look.selected * s));
                }
                None => {
                    ui.painter().rect_filled(rect, nav_r, theme::glass(look.hover * h + look.selected * s));
                }
            }
            icon_fg
        }
        NavStyle::Accent => {
            ui.painter().rect_filled(rect, nav_r, theme::glass(look.hover * h));
            ui.painter().rect_filled(rect, nav_r, theme::with_alpha(accent, 0.22 * s));
            theme::lerp_color(icon_fg, accent, s)
        }
        NavStyle::Bar => {
            ui.painter().rect_filled(rect, nav_r, theme::glass(look.hover * h));
            if let Some(c) = look.selected_fill {
                ui.painter().rect_filled(rect, nav_r, theme::with_alpha(c, look.selected * s));
            }
            let bar = Rect::from_center_size(pos2(rect.left() + 2.0, rect.center().y), vec2(3.0, look.bar_px * s));
            ui.painter().rect_filled(bar, 2.0, accent);
            icon_fg
        }
    }
}

/// Sidebar width expanded and collapsed to an icon rail (points).
pub const SIDEBAR_W: f32 = 236.0;

/// The expanded sidebar's width in this theme.
pub fn sidebar_w(app: &App) -> f32 {
    app.look().app.nav_item.width.unwrap_or(SIDEBAR_W).clamp(200.0, 300.0)
}

/// The rail's width in this theme.
pub fn rail_w(app: &App) -> f32 {
    app.look().app.nav_item.rail_px.max(60.0)
}

/// Whether the sidebar shows as a rail: the listener's choice, turned around in a theme
/// that starts as one, so the same button opens and closes it either way.
pub fn is_rail(app: &App) -> bool {
    app.sidebar_collapsed != app.look().app.nav_item.rail
}

/// How collapsed the sidebar is, 0 (full) to 1 (icon rail), eased over the toggle.
pub fn sidebar_collapse(app: &App, ctx: &egui::Context) -> f32 {
    let t = ctx.animate_bool_with_time(egui::Id::unique("sidebar-collapse"), is_rail(app), 0.28 * theme::SPEED);
    // Ease in-out: starts and lands gently in both directions.
    t * t * (3.0 - 2.0 * t)
}

/// The theme's name for the app, its last word perhaps in a second colour.
fn logo_name(ui: &mut Ui, logo: &crate::preset::TopBarLook, accent: Color32) {
    let c = if logo.logo_color.is_empty() { theme::text() } else { crate::preset::paint(&logo.logo_color, accent) };
    ui.spacing_mut().item_spacing.x = 0.0;
    match logo.logo_text.rsplit_once(' ').filter(|_| !logo.logo_color2.is_empty()) {
        Some((first, last)) => {
            ui.label(egui::RichText::new(format!("{first} ")).font(theme::bold(17.0)).color(c));
            // The second colour is for dark pages.
            let last_color = if theme::light() { theme::text() } else { crate::preset::paint(&logo.logo_color2, accent) };
            ui.label(egui::RichText::new(last).font(theme::bold(17.0)).color(last_color));
        }
        None => {
            ui.label(egui::RichText::new(&logo.logo_text).font(theme::bold(17.0)).color(c));
        }
    }
}

/// One layout for both states, so collapsing animates: the panel narrows while icons
/// and covers keep their column (24 pt from the left) and the labels fade and clip.
pub fn sidebar(app: &mut App, ui: &mut Ui) {
    const ICON_X: f32 = 24.0;
    let t = sidebar_collapse(app, ui.ctx());
    let collapsed = is_rail(app);
    let rail_w = rail_w(app);
    let item = app.look().app.nav_item.clone();
    let idle = app.look().app.controls.idle_alpha.clamp(0.5, 1.0);
    let accent = app.accent_color();
    // Labels fade out over the first two thirds of collapsing (and back in late).
    let a = ((1.0 - t) * 1.5 - 0.5).clamp(0.0, 1.0);
    // Content width when expanded: text keeps its truncation while the panel narrows.
    let full_w = ui.available_width() + (sidebar_w(app) - rail_w) * t;

    // Menu button, logo. Laid out on its own, with only its height taken here: on the rail
    // it's wider than the rail, and would widen every row under it past what's seen, their
    // highlights cut off at the rail's edge.
    let row_w = ui.available_width();
    let head_rect = Rect::from_min_size(ui.cursor().min, vec2(row_w, ui.spacing().interact_size.y));
    let mut head = ui.new_child(egui::UiBuilder::new().max_rect(head_rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
    {
        let ui = &mut head;
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
        let logo = app.look().app.top_bar.clone();
        if logo.logo_text.is_empty() {
            ui.label(egui::RichText::new("Music").font(theme::bold(22.0)).color(theme::text()));
        } else {
            logo_name(ui, &logo, app.accent_color());
        }
    }
    ui.allocate_space(vec2(row_w, head.min_rect().height()));
    ui.add_space(22.0);

    let mut go_to: Option<Page> = None;
    let look = NavLook::of(app);
    let nav_h = if item.rail_labels { lerp(item.height..=item.rail_height.unwrap_or(item.height.max(62.0)), t) } else { lerp(item.height..=item.rail_height.unwrap_or(item.height), t) };
    let p = app.page.clone();
    let logged_in = app.logged_in;
    let nav = |ui: &mut Ui, glyph: char, label: &str, page: Page, selected: bool, go_to: &mut Option<Page>| {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), nav_h), Sense::click());
        let mut resp = named(resp, label).on_hover_cursor(egui::CursorIcon::PointingHand);
        // Clicking the page you're on reloads it; say so where that loads anything.
        if page == p && (matches!(page, Page::Home | Page::Explore) || (logged_in && matches!(page, Page::Library(_)))) {
            resp = resp.on_hover_text("Refresh (F5)");
        } else if collapsed && !item.rail_labels {
            resp = resp.on_hover_text(label);
        }
        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
        let s = theme::anim_bool(ui.ctx(), resp.id.with("s"), selected, 0.2);
        let rect = rect.shrink2(vec2(item.side_margin, 0.0));
        let fill_rect = if item.hover_zoom != 1.0 { Rect::from_center_size(rect.center(), rect.size() * (1.0 + (item.hover_zoom - 1.0) * h)) } else { rect };
        let icon_fg = nav_bg(ui, look, fill_rect, h, s);
        if item.press > 0.0 && resp.is_pointer_button_down_on() {
            ui.painter().rect_filled(rect, look.radius, theme::glass(item.press));
        }
        widgets::focus_ring(ui, &resp, rect, look.radius);
        let icon_fg = if s > 0.0 && !item.selected_color.is_empty() { theme::lerp_color(icon_fg, crate::preset::paint(&item.selected_color, look.accent), s) } else { icon_fg };
        let icon_fg = theme::with_alpha(icon_fg, lerp(idle..=1.0, (h + s).min(1.0)));
        let slid = item.slide * h;
        // On the rail with labels: the icon over a small one-line label.
        let under = item.rail_labels && t > 0.5;
        let icon_y = if under { rect.center().y - 9.0 } else { rect.center().y };
        icon_at(ui, pos2(if under { rect.center().x } else { rect.left() + ICON_X + slid }, icon_y), glyph, 19.0, icon_fg);
        if under {
            let short = label.split_whitespace().next().unwrap_or(label);
            ui.painter().text(pos2(rect.center().x, rect.center().y + 14.0), Align2::CENTER_CENTER, short, theme::regular(10.5), icon_fg);
        }
        if a > 0.0 {
            let fg = theme::lerp_color(theme::text_dim(), theme::text(), (h + s).min(1.0));
            let fg = match look.style {
                crate::preset::NavStyle::Text => theme::lerp_color(fg, look.accent, s),
                crate::preset::NavStyle::Inverse => theme::lerp_color(fg, theme::on_text(), s),
                _ => fg,
            };
            let font = if selected { theme::semibold(15.0) } else { theme::regular(15.0) };
            ui.painter().text(pos2(rect.left() + 48.0 + slid, rect.center().y), Align2::LEFT_CENTER, label, font, theme::with_alpha(fg, a * lerp(idle..=1.0, (h + s).min(1.0))));
        }
        if resp.clicked() {
            *go_to = Some(page);
        }
    };
    nav(ui, icon::HOUSE, "Home", Page::Home, p == Page::Home, &mut go_to);
    nav(ui, icon::COMPASS, "Explore", Page::Explore, matches!(p, Page::Explore | Page::Mood(_)), &mut go_to);
    nav(ui, icon::LIBRARY, "Library", Page::Library(LibTab::Playlists), matches!(p, Page::Library(_)), &mut go_to);
    let jam_label = match app.jam.member_count() {
        n if app.jam.active() && n > 0 => format!("Jam · {n} listening"),
        _ => "Listen together".to_owned(),
    };
    nav(ui, icon::USERS, &jam_label, Page::Jam, p == Page::Jam, &mut go_to);

    ui.add_space(14.0);
    let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    if !item.hide_divider {
        ui.painter().rect_filled(line, 0.0, theme::glass(0.08));
    }
    ui.add_space(14.0 - 4.0 * t);

    let transferring = super::transfer::sidebar_visible(app, ui.input(|i| i.time));
    // Settings' row, however tall the theme makes it, and the transfer card.
    let bottom_h = nav_h + 8.0 + if transferring { 64.0 } else { 0.0 };
    let list_h = (ui.available_height() - bottom_h).max(0.0);
    ui.allocate_ui(vec2(ui.available_width(), list_h), |ui| {
        if !app.logged_in {
            // Too wide to squeeze: cross-fade the message and the rail's icon.
            if t < 0.5 {
                ui.multiply_opacity(1.0 - 2.0 * t);
                ui.label(egui::RichText::new("Sign in to see your playlists").color(theme::text_dim()));
                ui.add_space(8.0);
                if widgets::pill(ui, "Sign in", Some(icon::CIRCLE_USER), false).clicked() {
                    app.start_sign_in(ui.ctx());
                }
            } else {
                ui.multiply_opacity(2.0 * t - 1.0);
                nav(ui, icon::CIRCLE_USER, "Sign in", Page::Settings, false, &mut go_to);
                if go_to.is_some() {
                    app.settings_tab = super::pages::SettingsTab::Account;
                }
            }
            return;
        }
        // The heading folds away with the labels.
        let (head, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0 * (1.0 - t)), Sense::hover());
        if a > 0.0 {
            let label = if item.playlists_label.is_empty() { "PLAYLISTS" } else { item.playlists_label.as_str() };
            ui.painter().text(head.left_top(), Align2::LEFT_TOP, label, theme::semibold(11.5), theme::with_alpha(theme::text_faint(), a));
            if item.playlists_plus {
                let plus = Rect::from_center_size(pos2(head.right() - 10.0, head.top() + 7.0), Vec2::splat(22.0));
                let resp = named(ui.interact(plus, ui.scope_id().with("new-playlist"), Sense::click()), "New playlist").on_hover_text("New playlist").on_hover_cursor(egui::CursorIcon::PointingHand);
                let color = if resp.hovered() { theme::text() } else { theme::text_faint() };
                let stroke = egui::Stroke::new(1.6, theme::with_alpha(color, a));
                ui.painter().line_segment([plus.center() - vec2(6.0, 0.0), plus.center() + vec2(6.0, 0.0)], stroke);
                ui.painter().line_segment([plus.center() - vec2(0.0, 6.0), plus.center() + vec2(0.0, 6.0)], stroke);
                if resp.clicked() {
                    app.actions.push(Action::NewPlaylist(Vec::new()));
                }
            }
        }
        let bars = if t > 0.0 { egui::scroll_area::ScrollBarVisibility::AlwaysHidden } else { egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded };
        egui::ScrollArea::vertical().id_salt("sidebar-pl").auto_shrink(false).scroll_bar_visibility(bars).show(ui, |ui| {
            // Last launch's list until the library arrives.
            let Some(playlists) = app.library.as_ref().map(|l| &l.playlists).or(app.sidebar_cache.as_ref()) else {
                // The spinner animates every frame: give up on a request that never answered.
                if app.library_requested.elapsed() < std::time::Duration::from_secs(20) {
                    ui.horizontal(|ui| {
                        ui.add_space(ICON_X - 8.0);
                        widgets::loader(ui);
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
            // A song dragged out of a list goes into the playlist it's let go on: your own
            // playlists (the ones "Add to playlist" offers) and Liked Music.
            row_drag::scroll_targets(ui, &app.row_drag);
            let mut dropped = None;
            for c in playlists {
                let title_row = item.title_rows.filter(|_| t < 0.5);
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), title_row.unwrap_or(48.0)), Sense::click());
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                let takes = matches!(&c.target, Target::Playlist(id) if id.starts_with("PL") || id == "LM");
                let over = if takes { row_drag::target(ui, &mut app.row_drag, rect) } else { None };
                if let (Some(row_drag::Over::Dropped(t)), Target::Playlist(id)) = (&over, &c.target) {
                    dropped = Some((id.clone(), t.clone()));
                }
                let mut resp = named(resp, &c.title).on_hover_cursor(egui::CursorIcon::PointingHand);
                if collapsed {
                    resp = resp.on_hover_text(&c.title);
                }
                let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
                let selected = matches!(&c.target, Target::Playlist(id) if app.page == Page::Playlist(id.clone()));
                ui.painter().rect_filled(rect, 8.0, theme::glass(0.05 * h + if selected { 0.08 } else { 0.0 }));
                if matches!(over, Some(row_drag::Over::Held)) {
                    ui.painter().rect_filled(rect, 8.0, theme::with_alpha(accent, 0.18));
                    ui.painter().rect_stroke(rect, 8.0, egui::Stroke::new(1.5, accent), egui::StrokeKind::Inside);
                }
                if title_row.is_some() {
                    text_at(ui, pos2(rect.left() + 12.0, rect.center().y - 8.0), &c.title, theme::regular(13.0), theme::with_alpha(theme::text(), a), full_w - 24.0);
                    if resp.clicked() {
                        app.actions.push(Action::Open(c.target.clone()));
                    }
                    continue;
                }
                // Under the nav's icons, which centre on their row on a labelled rail.
                let img_x = if item.rail_labels && t > 0.5 { rect.shrink2(vec2(item.side_margin, 0.0)).center().x } else { rect.left() + ICON_X };
                let img = Rect::from_center_size(pos2(img_x, rect.center().y), Vec2::splat(36.0));
                paint_cover(ui, img, &c.thumbs, theme::thumb_radius(), 1.0);
                if a > 0.0 {
                    let w = full_w - 58.0;
                    text_at(ui, pos2(img.right() + 10.0, rect.top() + 7.0), &c.title, theme::semibold(13.5), theme::with_alpha(theme::text(), a), w);
                    text_at(ui, pos2(img.right() + 10.0, rect.top() + 26.0), &c.subtitle, theme::regular(12.0), theme::with_alpha(theme::text_dim(), a), w);
                }
                if resp.clicked() {
                    app.actions.push(Action::Open(c.target.clone()));
                }
                // Right-click: the menu a playlist's card has.
                resp.context_menu(|ui| widgets::card_menu_contents(ui, c, false, &mut app.actions));
            }
            match dropped {
                Some((id, t)) if id == "LM" => {
                    app.actions.push(Action::Like(t, true));
                    app.toast("Added to Liked Music");
                }
                Some((playlist_id, t)) => app.add_to_playlist(playlist_id, &[t]),
                None => {}
            }
        });
    });
    // A downloaded update (Restart), or for the .deb a new one that's out (its notes).
    let (ready, available) = match app.updater.as_ref().map(|u| u.state()) {
        Some(crate::update::State::Ready { version, .. }) => (Some(version), false),
        Some(crate::update::State::Available { version, .. }) => (Some(version), true),
        _ => (None, false),
    };
    let mut restart = false;
    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
        nav(ui, icon::SETTINGS, "Settings", Page::Settings, p == Page::Settings, &mut go_to);
        if transferring {
            super::transfer::sidebar_card(app, ui, collapsed, a, ICON_X);
        }
        if let Some(v) = &ready {
            ui.add_space(6.0);
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::click());
            let title = if available { "Update available" } else { "Restart to update" };
            let mut resp = named(resp, title).on_hover_cursor(egui::CursorIcon::PointingHand);
            if collapsed {
                resp = resp.on_hover_text(if available { format!("ytm-rs {v} is out") } else { format!("Restart to update to ytm-rs {v}") });
            }
            let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
            ui.painter().rect_filled(rect, 12.0, theme::with_alpha(look.accent, 0.22 + 0.12 * h));
            icon_at(ui, pos2(rect.left() + ICON_X, rect.center().y), icon::SPARKLES, 18.0, theme::text());
            if a > 0.0 {
                ui.painter().text(pos2(rect.left() + 46.0, rect.top() + 17.0), Align2::LEFT_CENTER, title, theme::semibold(13.5), theme::with_alpha(theme::text(), a));
                ui.painter().text(pos2(rect.left() + 46.0, rect.top() + 35.0), Align2::LEFT_CENTER, format!("ytm-rs {v}"), theme::regular(12.0), theme::with_alpha(theme::text(), 0.75 * a));
            }
            if resp.clicked() && available {
                app.notes_dialog = Some(super::notes::NotesDialog { version: v.clone(), ready: true });
            } else {
                restart = resp.clicked();
            }
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

/// Round button with a clockwise arrow, drawn like the icon font's (Lucide's rotate-cw),
/// which the bundled subset doesn't have.
fn refresh_button(ui: &mut Ui) -> egui::Response {
    const SIZE: f32 = 18.0;
    let d = SIZE + 18.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(d), Sense::click());
    let resp = named(resp, "Refresh").on_hover_text("Refresh (F5)").on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
    if h > 0.0 {
        ui.painter().circle_filled(rect.center(), d / 2.0, theme::glass(0.10 * h));
    }
    let color = theme::lerp_color(theme::text_dim(), theme::text(), h);
    let press = if resp.is_pointer_button_down_on() { 0.9 } else { 1.0 };
    // On Lucide's 24-unit grid, centred on (12, 12).
    let unit = SIZE * press / 24.0;
    let at = |x: f32, y: f32| rect.center() + vec2(x - 12.0, y - 12.0) * unit;
    let stroke = egui::Stroke::new(2.0 * unit, color);
    // A circle of radius 9 from three o'clock round to the top right, then on to the arrow.
    let mut arc: Vec<Pos2> = (0..=40)
        .map(|i| {
            let a = (i as f32 / 40.0) * 315.0_f32.to_radians();
            at(12.0 + 9.0 * a.cos(), 12.0 + 9.0 * a.sin())
        })
        .collect();
    arc.push(at(21.0, 8.0));
    let painter = ui.painter();
    painter.add(egui::Shape::line(arc, stroke));
    painter.add(egui::Shape::line(vec![at(21.0, 3.0), at(21.0, 8.0), at(16.0, 8.0)], stroke));
    resp
}

pub fn top_bar(app: &mut App, ui: &mut Ui) {
    let tb = &app.look().app.top_bar;
    let search_h = app.look().app.search_look.height;
    // What the theme puts behind the row, under its widgets.
    let glass = app.look().app.glass.top_bar.filter(|_| !theme::light());
    if tb.capsule_px.is_none() && (tb.band.is_some() || glass.is_some()) && !theme::light() {
        let r = ui.max_rect();
        let screen = ui.ctx().content_rect();
        let [a, px] = tb.band.unwrap_or([0.0, search_h + 34.0]);
        let band = Rect::from_min_max(pos2(r.left() - 28.0, screen.top()), pos2(r.right(), r.top() + px));
        let on = if tb.band_on_scroll { (app.page_scrolled / 40.0).clamp(0.0, 1.0) } else { 1.0 };
        // The band takes the pointer from what's scrolled under it.
        if let Some(g) = glass {
            let back = Rect::from_min_size(band.min, vec2(band.width(), r.top() + search_h + 6.0 - screen.top()));
            ui.interact(back, ui.scope_id().with("band-back"), Sense::click());
            // It runs up over the window's top, so it moves the window as the title bar would.
            super::titlebar::drag_area(ui, back, "band-back");
            // Down to where the page starts, so nothing at rest sits in the blur.
            let frosted = band.with_max_y(r.top() + search_h.max(42.0) + 6.0);
            crate::shader::frost(ui.painter(), frosted, crate::shader::Frost { look: g, radius: 0.0, opacity: on, fade: 12.0 });
        }
        if a > 0.0 {
            let top = crate::preset::hex(&tb.band_color).map_or(theme::shade(a * on), |c| theme::with_alpha(c, a * on));
            vgradient(ui.painter(), band, top, Color32::TRANSPARENT);
        }
    }
    if let Some(max) = tb.capsule_px {
        let r = ui.max_rect();
        // Ends just past the search box (where it was last frame), not across the empty row.
        let box_right = ui.ctx().data(|d| d.get_temp::<f32>(egui::Id::unique("search-box-right")));
        let w = box_right.map_or(r.width().min(max), |x| (x + 16.0 - r.left()).min(r.width()).min(max));
        let pill = Rect::from_min_size(r.min - vec2(10.0, 6.0), vec2(w + 4.0, search_h + 12.0));
        // Dark glass on dark pages, light on light ones, so the box in it stays readable.
        let fill = if theme::light() { theme::with_alpha(Color32::WHITE, tb.capsule[0]) } else { theme::shade(tb.capsule[0]) };
        if let Some(g) = glass {
            ui.interact(pill, ui.scope_id().with("capsule-back"), Sense::click());
            super::titlebar::drag_area(ui, pill, "capsule-back");
            crate::shader::frost(ui.painter(), pill, crate::shader::Frost { look: g, radius: tb.capsule[1].min(127.0), opacity: 1.0, fade: 0.0 });
        }
        ui.painter().rect_filled(pill, tb.capsule[1].min(127.0), fill);
        ui.painter().rect_stroke(pill, tb.capsule[1].min(127.0), egui::Stroke::new(1.0, theme::glass(0.08)), egui::StrokeKind::Inside);
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        // A narrow row needs the logo's room for the search box and the account button.
        if tb.logo_in_bar && !tb.logo_text.is_empty() && ui.available_width() >= 620.0 {
            ui.add_space(6.0);
            ui.allocate_ui_with_layout(vec2(136.0, app.look().app.search_look.height.max(30.0)), egui::Layout::left_to_right(egui::Align::Center), |ui| logo_name(ui, tb, app.accent_color()));
            ui.add_space(10.0);
        }
        if icon_button(ui, icon::CHEVRON_LEFT, 20.0, "Back", false).clicked() {
            app.nav_back();
        }
        if icon_button(ui, icon::CHEVRON_RIGHT, 20.0, "Forward", false).clicked() {
            app.nav_forward();
        }
        if refresh_button(ui).clicked() {
            app.refresh();
        }
        ui.add_space(12.0);

        // Room for the account button, and the window buttons when they're over this row;
        // in a narrow window the box gives way rather than run under them.
        let reserve = 50.0 + super::titlebar::right_gap(ui.ctx(), ui.max_rect(), 20.0);
        let narrowest = if super::titlebar::custom() { 120.0 } else { 240.0 };
        let sl = &app.look().app.search_look;
        let width = (ui.available_width() - reserve).clamp(narrowest, sl.max_px.max(narrowest));
        // Anywhere on the pill focuses the box, not just on the text area inside it.
        let (rect, field) = ui.allocate_exact_size(vec2(width, sl.height.max(30.0)), Sense::click());
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::unique("search-box-right"), rect.right()));
        let box_look = &app.look().app;
        // At most a pill, whatever the box's height.
        let box_r = box_look.search_radius.min(rect.height() / 2.0);
        if let Some(g) = box_look.glass.search.filter(|_| !theme::light()) {
            crate::shader::frost(ui.painter(), rect, crate::shader::Frost { look: g, radius: box_r, opacity: 1.0, fade: 0.0 });
        }
        let field = field.on_hover_cursor(egui::CursorIcon::Text);
        let focus = theme::anim_bool(ui.ctx(), "search-focus", app.search_focused, 0.2);
        let rest = egui::lerp(box_look.controls.idle_alpha.clamp(0.5, 1.0)..=1.0, focus);
        let suggesting = app.search_focused && !app.suggestions.1.is_empty() && !app.np_open;
        let open = theme::anim_bool(ui.ctx(), "search-open", suggesting, 0.15);
        let fill = match crate::preset::hex(&sl.color).filter(|_| !theme::light()) {
            Some(c) => theme::with_alpha(c, box_look.search_alpha * rest + 0.05 * focus),
            // A theme's own box colour is for dark pages: on light ones, the usual faint fill.
            None if !sl.color.is_empty() => theme::glass(0.09 * rest + 0.05 * focus),
            None => theme::glass(box_look.search_alpha * rest + 0.05 * focus),
        };
        let fill = match crate::preset::hex(&sl.open_color).filter(|_| !theme::light() && open > 0.0) {
            Some(c) => theme::lerp_color(fill, c, open),
            None => fill,
        };
        // Attached to its open list: square at the bottom.
        let corners = if sl.attached && suggesting { egui::CornerRadius { nw: box_r as u8, ne: box_r as u8, sw: 0, se: 0 } } else { egui::CornerRadius::same(box_r as u8) };
        if sl.attached && suggesting {
            ui.painter().rect_filled(rect, corners, fill);
        } else {
            widgets::fill_rounded(ui, rect, box_r, fill);
        }
        if let Some([r, a]) = sl.outer {
            ui.painter().rect_stroke(rect.expand(2.0), r, egui::Stroke::new(1.0, theme::glass(a)), egui::StrokeKind::Inside);
        }
        let edge = box_look.search_border.max(0.25 * focus);
        if edge > 0.0 {
            ui.painter().rect_stroke(rect, corners, egui::Stroke::new(1.0, theme::glass(edge)), egui::StrokeKind::Inside);
        }
        let focus_color = if sl.focus_color == "accent" { Some(app.accent_color()) } else { crate::preset::hex(&sl.focus_color) };
        if let Some(c) = focus_color.filter(|_| focus > 0.0) {
            if sl.underline {
                ui.painter().hline(rect.shrink2(vec2(box_r, 0.0)).x_range(), rect.bottom() - 1.0, egui::Stroke::new(2.0, theme::with_alpha(c, focus)));
            } else {
                ui.painter().rect_stroke(rect, corners, egui::Stroke::new(1.0, theme::with_alpha(c, focus)), egui::StrokeKind::Inside);
            }
            if sl.focus_ring > 0.0 {
                ui.painter().rect_stroke(rect.expand(sl.focus_ring), box_r + sl.focus_ring, egui::Stroke::new(sl.focus_ring, theme::with_alpha(c, 0.3 * focus)), egui::StrokeKind::Inside);
            }
        }
        let search_icon = if sl.accent_icon { app.accent_color() } else { theme::text_dim() };
        icon_at(ui, pos2(rect.left() + 22.0, rect.center().y), icon::SEARCH, 17.0, search_icon);
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
                .hint_text(egui::RichText::new(if app.show_podcasts { "Search songs, albums, artists, podcasts" } else { "Search songs, albums, artists" }).color(theme::text_faint()))
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
            icon_at(ui, x.center(), icon::X, 16.0, theme::lerp_color(theme::text_dim(), theme::text(), h));
            if resp.clicked() {
                app.search_text.clear();
                app.suggestions = Default::default();
                app.suggest_sel = None;
                app.suggest_due = Some(ui.input(|i| i.time));
                edit.request_focus();
            }
        }
        if field.clicked() || ui.input(|i| i.modifiers.command && !i.modifiers.shift && i.key_pressed(egui::Key::F)) {
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
            ui.add_space(super::titlebar::right_gap(ui.ctx(), ui.max_rect(), 20.0));
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
                app.settings_tab = super::pages::SettingsTab::Account;
                app.actions.push(Action::Go(Page::Settings));
            }
            if app.loading {
                ui.add_space(10.0);
                widgets::loader(ui);
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
    let sl = &app.look().app.search_look;
    // Attached, or sliding in from a little above.
    let gap = if sl.attached { 0.0 } else { 6.0 - 6.0 * (1.0 - t) };
    if sl.drop_grow && t < 1.0 {
        // Grows down from the box, about its top edge.
        let top = rect.left_bottom().to_vec2() + vec2(0.0, gap);
        ctx.set_transform_layer(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("suggestions")), egui::emath::TSTransform::from_translation(top) * egui::emath::TSTransform::from_scaling(0.85 + 0.15 * t) * egui::emath::TSTransform::from_translation(-top));
    }
    egui::Area::new(egui::Id::unique("suggestions"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.left_bottom() + vec2(0.0, gap))
        .show(ctx, |ui| {
            ui.set_opacity(t);
            let drop_r = app.look().app.search_look.drop_radius.unwrap_or(14.0).clamp(0.0, 40.0);
            if let (Some(g), Some(last)) = (app.look().app.glass.search_drop, ctx.memory(|m| m.area_rect(egui::Id::unique("suggestions"))))
                && !theme::light()
            {
                crate::shader::frost(ui.painter(), last, crate::shader::Frost { look: g, radius: drop_r, opacity: t, fade: 0.0 });
            }
            egui::Frame::new()
                .fill(if app.look().app.glass.search_drop.is_some() && crate::shader::glass_ready() && !theme::light() { theme::with_alpha(crate::preset::hex(&app.look().app.menu_color).unwrap_or(Color32::from_rgb(38, 34, 42)), sl.drop_alpha.unwrap_or(app.look().app.menu_look.alpha)) } else { theme::menu_fill() })
                .corner_radius(if sl.attached { egui::CornerRadius { nw: 0, ne: 0, sw: drop_r as u8, se: drop_r as u8 } } else { egui::CornerRadius::same(drop_r as u8) })
                .stroke(egui::Stroke::new(if sl.drop_ring > 0.0 { 1.0 } else { 0.0 }, theme::glass(sl.drop_ring)))
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
                        ui.painter().rect_filled(r, sl.item_radius, theme::glass(0.08 * h));
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
                            icon_at(ui, x.center(), icon::TRASH_2, 15.0, theme::lerp_color(theme::text_dim(), theme::text(), xh));
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
    job.append(bold, 0.0, egui::TextFormat { font_id: theme::semibold(15.0), color: theme::text(), ..Default::default() });
    job.append(rest, 0.0, egui::TextFormat { font_id: theme::regular(15.0), color: theme::text_dim(), ..Default::default() });
    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(10.0));
    let g = ui.fonts_mut(|f| f.layout_job(job));
    ui.painter().galley(pos, g, theme::text());
}

// ------------------------------------------------------------------ player bar

/// The floating bar, over the page. `slot`: its panel.
pub fn floating_bar(app: &mut App, ui: &mut Ui, slot: Rect, now: f64) {
    let look = app.look();
    let r = match look.chrome.bar_max_px {
        Some(w) if slot.width() > w => Rect::from_center_size(slot.center(), vec2(w, slot.height())),
        _ => slot,
    };
    let radius = look.chrome.bar_radius.min(r.height() / 2.0);
    // Takes the pointer from the page under it, also between the buttons.
    ui.interact(r, ui.scope_id().with("bar-back"), Sense::click());
    let [sy, sb, sa] = look.chrome.bar_shadow.unwrap_or([0.0, 24.0, 0.45]);
    widgets::soft_shadow(ui, r.translate(vec2(0.0, sy)), radius, sb, sa);
    let bar_alpha = look.app.bar_alpha.clamp(0.0, 1.0);
    let glass = look.app.glass.bar.filter(|_| crate::shader::glass_ready() && !theme::light());
    if let Some(g) = glass {
        crate::shader::frost(ui.painter(), r, crate::shader::Frost { look: g, radius, opacity: 1.0, fade: 0.0 });
    }
    // With no real blur: the art's colour through it, what a blur of the page would show.
    let fill = if glass.is_some() { bar_fill(look, bar_alpha) } else { bar_fill(look, (bar_alpha + 0.07).min(1.0)) };
    let fill = if look.app.frost && glass.is_none() && app.current().is_some() { theme::lerp_color(fill, app.accent_color(), 0.1) } else { fill };
    let fill = match look.chrome.bar_np_alpha.filter(|_| app.np_open) {
        Some(a) => theme::shade(a),
        None => fill,
    };
    ui.painter().rect_filled(r, radius, fill);
    let edge = look.chrome.bar_border_alpha.unwrap_or(if look.app.bar_border { 0.14 } else { 0.08 });
    if !(look.chrome.bar_np_no_edge && app.np_open) {
        let c = crate::preset::hex(&look.chrome.bar_edge_color).map_or(theme::glass(edge), |c| theme::with_alpha(c, edge));
        ui.painter().rect_stroke(r, radius, egui::Stroke::new(1.0, c), egui::StrokeKind::Inside);
    }
    let mut bar = ui.new_child(egui::UiBuilder::new().max_rect(r));
    bar.set_clip_rect(r);
    player_bar(app, &mut bar, now);
}

/// The player bar's speed button: the speed it's at, lit (with the dot under it) when
/// it isn't 1×. Opens a slider for it.
fn tempo_button(app: &mut App, ui: &mut Ui, r: Rect, rest: f32, accent: Color32, dot_floor: f32) {
    let jam = app.jam.active();
    let changed = !jam && (app.speed - 1.0).abs() > 0.001;
    let resp = named(ui.interact(r, ui.scope_id().with("tempo"), Sense::click()), "Playback speed")
        .on_hover_text("Playback speed")
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
    if h > 0.0 {
        ui.painter().rect_filled(r, r.height() / 2.0, theme::glass(0.10 * h));
    }
    widgets::focus_ring(ui, &resp, r, r.height() / 2.0);
    let color = theme::with_alpha(if changed { theme::text() } else { theme::lerp_color(theme::text_dim(), theme::text(), h) }, rest);
    let speed = if jam { 1.0 } else { app.speed };
    let g = widgets::galley(ui, &super::pages::times(speed), theme::semibold(13.0), color, 60.0);
    ui.painter().galley(r.center() - g.size() / 2.0, g, color);
    if changed {
        ui.painter().circle_filled(pos2(r.center().x, (r.bottom() + 3.0).min(dot_floor)), 2.0, accent);
    }
    // A small card over the button: the speed, its slider under it.
    let popup = widgets::menu_popup(&resp).align(egui::RectAlign::TOP_END).align_alternatives(&[egui::RectAlign::BOTTOM_END]);
    popup.close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        use super::pages as p;
        widgets::menu_body(ui, 248.0);
        let pad = 10.0;
        ui.add_space(6.0);
        if jam {
            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.add(egui::Label::new(egui::RichText::new(p::TEMPO_IN_JAM).font(theme::regular(12.5)).color(theme::text_dim())).wrap());
            });
            ui.add_space(6.0);
        }
        ui.add_enabled_ui(!jam, |ui| {
            use crate::audio::tempo::{MAX_SPEED, MIN_SPEED};
            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.label(egui::RichText::new("Speed").font(theme::semibold(13.5)).color(theme::text()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(pad);
                    ui.label(egui::RichText::new(p::times(speed)).font(theme::regular(13.0)).color(theme::text_dim()));
                });
            });
            ui.horizontal(|ui| {
                ui.add_space(pad - 8.0);
                p::slider_track(ui, &mut app.speed, (MIN_SPEED, MAX_SPEED, 0.05), Some(1.0), ui.available_width() - pad + 8.0, "Speed");
            });
            // Always there, so the card keeps its size while the slider moves.
            widgets::menu_divider(ui);
            let normal = (app.speed - 1.0).abs() < 0.001;
            ui.add_enabled_ui(!normal, |ui| {
                if normal {
                    ui.multiply_opacity(0.4);
                }
                if widgets::menu_action(ui, Some(icon::HISTORY), "Back to normal") {
                    app.speed = 1.0;
                }
            });
        });
        ui.add_space(4.0);
    });
}

pub fn player_bar(app: &mut App, ui: &mut Ui, now: f64) {
    let full = ui.max_rect();
    // A floating bar is rounded: keep the progress line clear of its corners.
    let look = app.look();
    let inset = if look.chrome.player_bar == crate::preset::PlayerBar::Floating { look.chrome.bar_radius.min(full.height() / 2.0) } else { 0.0 };
    let inset = inset.max(look.chrome.progress_inset);
    let track_rect = full.shrink2(vec2(inset, 0.0));
    let (progress_px, progress_r, progress_thumb) = (look.chrome.progress_px, look.chrome.progress_radius, look.chrome.progress_thumb);
    let at_bottom = look.chrome.progress_at == crate::preset::ProgressAt::Bottom;
    let progress_color = crate::preset::paint(&look.chrome.progress_color, app.accent_color());
    let progress_color = if look.chrome.progress_color == "white" { theme::text() } else { progress_color };
    // A fill meant for a dark bar has to show on a light one: at least 3:1 on its track.
    let progress_track = theme::over(theme::glass(look.chrome.progress_track), theme::bg());
    let readable = |c: Color32| if theme::light() { theme::deepen(c, progress_track, 3.0) } else { c };
    let progress_color = theme::with_alpha(readable(progress_color), look.chrome.progress_alpha);
    let chrome = &look.chrome;
    if chrome.bar_tint.len() >= 2 && chrome.bar_tint_alpha > 0.0 && !theme::light() {
        let stops = chrome.bar_tint.len();
        for i in 0..stops - 1 {
            let at = |k: usize| theme::with_alpha(crate::preset::paint(&chrome.bar_tint[k], app.accent_color()), chrome.bar_tint_alpha);
            let seg = Rect::from_min_max(pos2(egui::lerp(full.x_range(), i as f32 / (stops - 1) as f32), full.top()), pos2(egui::lerp(full.x_range(), (i + 1) as f32 / (stops - 1) as f32), full.bottom()));
            hgradient(ui.painter(), seg, at(i), at(i + 1));
        }
    }
    if chrome.player_bar != crate::preset::PlayerBar::Floating {
        if chrome.bar_edge_top && chrome.bar_border_alpha.is_some() && (chrome.bar_np_border || !app.np_open) {
            let a = chrome.bar_border_alpha.unwrap_or(0.1);
            let c = crate::preset::hex(&chrome.bar_edge_color).map_or(theme::glass(a), |c| theme::with_alpha(c, a));
            ui.painter().hline(full.x_range(), full.top() + 0.5, egui::Stroke::new(1.0, c));
        }
        if let Some([px, a]) = chrome.bar_docked_shadow.filter(|_| !app.np_open) {
            let p = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::unique("bar-shadow")));
            vgradient(&p, Rect::from_min_max(pos2(full.left(), full.top() - px), pos2(full.right(), full.top())), Color32::TRANSPARENT, theme::shade(a));
        }
    }
    let status = app.audio.status.clone();
    let track = app.current().cloned();
    let playing = app.playing();
    let total = status
        .duration()
        .map(|d| d.as_secs_f32())
        .or(track.as_ref().and_then(|t| t.duration).map(|d| d as f32))
        .unwrap_or(0.0);
    let pos = app.seek_drag.unwrap_or(status.position_smooth().as_secs_f32()).min(total.max(0.0));

    // The cover showing through the bar while now playing is closed, under the progress line.
    // Its blurred colours: the picture itself, stretched across a thin bar, is an unreadable crop.
    if chrome.bar_cover_alpha > 0.0
        && !app.np_open
        && track.is_some()
        && let Some(bg) = app.bg.as_ref()
    {
        let b = &look.background;
        let screen = ui.ctx().content_rect();
        bg.paint(&ui.painter().with_clip_rect(full), screen, crate::shader::Params { time: 0.0, blend: 1.0, intensity: 0.0, saturation: b.saturation, dim: chrome.bar_cover_alpha, scale: b.scale, contrast: 1.0, dither: b.dither, rotate: 0.0, alpha: 1.0 });
    }

    // Progress line along the top edge; thickens with a knob on hover.
    let above = look.chrome.progress_at == crate::preset::ProgressAt::AboveTransport;
    // Over the transport, as wide as a third of the bar.
    let track_rect = if above { Rect::from_center_size(pos2(full.center().x + look.chrome.transport_shift + look.chrome.transport_shift_frac * full.width(), track_rect.center().y), vec2(look.chrome.progress_width_frac.map_or((full.width() * 0.34).clamp(220.0, 520.0), |f| (full.width() * f).max(160.0)), track_rect.height())) } else { track_rect };
    // The time sits over the line, the line coming down to make room.
    let time_over = above && look.chrome.time_at == crate::preset::TimeAt::AboveProgress;
    let edge_y = if time_over { track_rect.top() + 24.0 } else if above { track_rect.top() + 7.0 } else if at_bottom { track_rect.bottom() - progress_px } else { track_rect.top() };
    // The bar's clip would cut a target centred on a line along its edge: those grow inward.
    let bar_hit = if above {
        Rect::from_min_size(pos2(track_rect.left(), edge_y - 6.0), vec2(track_rect.width(), 14.0))
    } else if at_bottom {
        Rect::from_min_max(pos2(track_rect.left(), full.bottom() - 14.0), pos2(track_rect.right(), full.bottom()))
    } else {
        Rect::from_min_size(pos2(track_rect.left(), edge_y), vec2(track_rect.width(), 14.0))
    };
    let resp = ui.interact(bar_hit, ui.scope_id().with("progress"), Sense::click_and_drag());
    let resp = if total > 0.0 { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
    resp.widget_info(|| egui::WidgetInfo::slider(total > 0.0, pos as f64, "Seek"));
    let h = theme::anim_bool(ui.ctx(), "progress-h", resp.hovered() || resp.dragged(), 0.15);
    let thick = progress_px + if progress_thumb { 2.0 * h } else { 0.0 };
    // Along the bottom edge it thickens upward, staying inside the bar.
    let line_y = if at_bottom { edge_y - (thick - progress_px) } else { edge_y };
    let line = Rect::from_min_size(pos2(track_rect.left(), line_y), vec2(track_rect.width(), thick));
    ui.painter().rect_filled(line, progress_r, theme::glass(chrome.progress_track));
    let frac = if total > 0.0 { pos / total } else { 0.0 };
    let filled = Rect::from_min_size(line.min, vec2(line.width() * frac, thick));
    if chrome.progress_gradient.len() >= 2 {
        // Stops along the whole bar, cut off at the position: four vertices a stop.
        let stops = chrome.progress_gradient.len();
        let at = |i: usize| readable(crate::preset::paint(&chrome.progress_gradient[i], app.accent_color()));
        for i in 0..stops - 1 {
            let (a, b) = (i as f32 / (stops - 1) as f32, (i + 1) as f32 / (stops - 1) as f32);
            if a >= frac {
                break;
            }
            let end = b.min(frac);
            let seg = Rect::from_min_max(pos2(line.left() + line.width() * a, line.top()), pos2(line.left() + line.width() * end, line.bottom()));
            hgradient(ui.painter(), seg, at(i), theme::lerp_color(at(i), at(i + 1), (end - a) / (b - a)));
        }
    } else {
        ui.painter().rect_filled(filled, progress_r, progress_color);
    }
    if h > 0.0 && total > 0.0 {
        // The knob and the time tip stick out above the bar, over now playing (a higher
        // layer than the bar), so they go on a layer above both and outside the bar's clip.
        let over = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, ui.scope_id().with("seek-over")));
        if progress_thumb {
            let knob = pos2(filled.right(), line.center().y);
            let knob_c = if chrome.progress_thumb_color.is_empty() { progress_color } else { crate::preset::paint(&chrome.progress_thumb_color, app.accent_color()) };
            match chrome.progress_thumb_pill {
                Some([kw, kh]) => {
                    over.rect_filled(Rect::from_center_size(knob, vec2(kw, kh) * h), kw.min(kh) / 2.0, knob_c);
                }
                None => {
                    over.circle_filled(knob, chrome.progress_thumb_px * h, knob_c);
                }
            }
            if chrome.progress_thumb_dot {
                let dot_c = if chrome.progress_thumb_dot_color.is_empty() { theme::on_text() } else { crate::preset::paint(&chrome.progress_thumb_dot_color, app.accent_color()) };
                over.circle_filled(knob, chrome.progress_thumb_px * chrome.progress_thumb_dot_frac * h, dot_c);
            }
        }
        if let Some(p) = resp.hover_pos() {
            let t = ((p.x - line.left()) / line.width()).clamp(0.0, 1.0) * total;
            let tip = pos2(p.x, line.top() - 16.0);
            let tip_text = crate::preset::hex(&chrome.tip_text).unwrap_or(Color32::WHITE);
            let g = widgets::galley(ui, &fmt_time(t as u32), theme::semibold(12.0), tip_text, 80.0);
            let bg = Rect::from_center_size(tip, g.size() + vec2(12.0, 6.0));
            over.rect_filled(bg, 6.0, crate::preset::hex(&chrome.tip_fill).unwrap_or(theme::shade(0.75)));
            over.galley(bg.min + vec2(6.0, 3.0), g, tip_text);
        }
    }
    if total > 0.0
        && let Some(p) = resp.interact_pointer_pos()
    {
        let t = ((p.x - line.left()) / line.width()).clamp(0.0, 1.0) * total;
        if resp.dragged() {
            app.seek_drag = Some(t);
        }
        if resp.drag_stopped() || resp.clicked() {
            app.seek_drag = None;
            app.actions.push(Action::Seek(t));
        }
    }

    let [pad_x, pad_top] = chrome.bar_pad.unwrap_or([16.0, 6.0]);
    let row = Rect::from_min_max(pos2(full.left() + pad_x, full.top() + pad_top), pos2(full.right() - pad_x, full.bottom()));
    let cy = row.center().y;
    let time_under = chrome.time_at == crate::preset::TimeAt::UnderTransport;
    // Stock YouTube Music's order: the transport on the left, the song in the middle.
    let song_middle = chrome.bar_layout == crate::preset::BarLayout::SongMiddle;
    let bar_scale = chrome.icon_scale.clamp(0.75, 1.5);
    let bar_rest = chrome.icon_alpha.clamp(0.5, 1.0);

    // Narrow windows give way in steps so nothing overlaps: the transport tightens, the
    // volume slider folds into its icon (hover to show it), then the time goes (the
    // progress line still shows it on hover). The title shortens to keep clear on the left.
    let time_g = (total > 0.0).then(|| {
        let txt = format!("{} / {}", fmt_time(pos as u32), fmt_time(total as u32));
        widgets::galley(ui, &txt, theme::weighted(chrome.time_weight, 13.0), theme::with_alpha(theme::text_dim(), chrome.time_alpha), 200.0)
    });
    let time_w = time_g.as_ref().map_or(0.0, |g| g.size().x);
    // Where the song starts: at the left, or just right of the left-hand transport, the
    // middle of what's left then being about the bar's.
    let art_side = chrome.thumb_px.unwrap_or(full.height() - 26.0).clamp(28.0, 64.0);
    // How wide the longer of the title and byline really is, heart excluded.
    let song_text = track.as_ref().map_or(0.0, |t| {
        let width = |s: String, font: egui::FontId| ui.painter().layout_no_wrap(s, font, Color32::PLACEHOLDER).size().x;
        width(t.title.clone(), theme::weighted(chrome.bar_title_weight, chrome.bar_title_px)).max(width(t.artist_line(), theme::regular(chrome.bar_byline_px)))
    });
    let art_x = if song_middle {
        // The song centred in the bar, never under the transport to its left.
        let block = art_side + 14.0 + song_text.min((row.width() * 0.3 - 110.0).max(80.0)) + 48.0;
        (row.center().x - block / 2.0 + art_side / 2.0).max(row.left() + 300.0)
    } else {
        row.left() + 30.0
    };
    // A record peeking out of the thumbnail takes room before the title.
    let song_x = art_x + art_side / 2.0 + 14.0 + chrome.disc_secs.map_or(0.0, |_| art_side * 0.46);
    let title_w = |room: f32| (row.width() * 0.3 - 110.0).max(80.0).min(room).max(40.0);
    // With the song in the middle, what's left is from where its title and heart (about 48 px) end.
    // The speed button, when shown, takes 44 px.
    let speed_w = if app.tempo_on { 44.0 } else { 0.0 };
    let song_end = song_x + if track.is_some() { song_text.min(title_w(row.right() - 244.0 - speed_w - song_x)) + 48.0 } else { 0.0 };
    // Room between the repeat button's edge (plus a gap) and the speed/lyrics/queue/expand/
    // mini player buttons. The volume with its slider takes 140 px plus the time; the icon alone, 36.
    let free = |half: f32| row.right() - 160.0 - speed_w - if song_middle { song_end } else { row.center().x + half + 12.0 };
    let tight = free(141.0) < 140.0 + time_w;
    let half = if tight { 125.0 } else { 141.0 };
    let compact_vol = free(half) < 140.0 + time_w;
    let compact_vol = compact_vol || chrome.volume_on_hover || chrome.hide_volume;
    let show_time = free(half) >= if compact_vol { 36.0 } else { 140.0 } + time_w && !time_under && chrome.time_at != crate::preset::TimeAt::AboveProgress;

    // Left: now playing
    if let Some(t) = &track {
        let art = Rect::from_center_size(pos2(art_x, cy), Vec2::splat(art_side));
        let thumb_r = chrome.thumb_radius.unwrap_or(theme::thumb_radius() + 1.0);
        // Under the song, sized once the title and heart have been placed.
        let block_slot = chrome.track_block.map(|_| ui.painter().add(egui::Shape::Noop));
        let x = song_x;
        // Title, artists and the heart (about 48 px) end before the shuffle button.
        let room = if song_middle { row.right() - 244.0 - speed_w - x } else { row.center().x - half - 18.0 - 14.0 - 48.0 - x };
        let w = title_w(room);
        // Right click on the song has everything about it, like on any song's row. Registered
        // first, so the art, title and artist links above it keep their clicks; they get the
        // same menu below.
        let song_area = Rect::from_min_max(art.left_top(), pos2(x + w + 48.0, art.bottom()));
        let song_menu = ui.interact(song_area, ui.scope_id().with("song-menu"), Sense::click());
        let mut menu_on = vec![song_menu];
        if let Some(secs) = look.chrome.disc_secs {
            // A record peeking out from behind the thumbnail, spinning while it plays.
            let turn = if playing { (now / secs.max(1.0) as f64 % 1.0) as f32 * std::f32::consts::TAU } else { 0.0 };
            // Out of its sleeve while playing, back in on pause, if the theme has it move.
            let slide = look.cover.disc_slide_secs;
            let out = if slide > 0.0 { theme::ease_in_out(ui.ctx().animate_bool_with_time(egui::Id::unique("bar-disc-out"), playing, slide)) } else { 1.0 };
            let c = art.center().lerp(art.right_center(), out);
            ui.painter().circle_filled(c, art.height() * 0.46, Color32::from_gray(18));
            // Its edge, or on a dark bar only the label in the middle shows.
            ui.painter().circle_stroke(c, art.height() * 0.46, egui::Stroke::new(1.0, theme::glass(0.25)));
            let dot = c + vec2(turn.cos(), turn.sin()) * art.height() * 0.28;
            ui.painter().circle_filled(c, art.height() * 0.14, progress_color);
            ui.painter().circle_filled(dot, 1.6, theme::glass(0.5));
        }
        let ar = ui.interact(art, ui.scope_id().with("np-art"), Sense::click());
        let ar = named(ar, "Open now playing").on_hover_cursor(egui::CursorIcon::PointingHand);
        menu_on.push(ar.clone());
        let ah = theme::anim_bool(ui.ctx(), ar.id.with("h"), ar.hovered(), 0.15);
        if let Some(secs) = chrome.thumb_disc_secs {
            if let Some([blur, a]) = chrome.thumb_glow {
                let c = crate::preset::paint(&chrome.thumb_glow_color, app.accent_color());
                ui.painter().add(egui::epaint::Shadow { offset: [0, 0], blur: blur as u8, spread: 0, color: theme::with_alpha(c, a) }.as_shape(art, egui::CornerRadius::same((art.width() / 2.0) as u8)));
            }
            // The thumbnail as a little record. Frames for it come from the player's own
            // pace; paused, it holds still.
            let turn = if playing { (now / secs.max(1.0) as f64 % 1.0) as f32 * std::f32::consts::TAU } else { 0.0 };
            widgets::paint_disc(ui, art.center(), art.width() / 2.0, &t.thumbs, turn, Color32::WHITE, (art.width() * ui.ctx().pixels_per_point()) as u32);
            ui.painter().circle_filled(art.center(), 2.0, theme::shade(0.8));
            ui.painter().circle_stroke(art.center(), art.width() / 2.0, egui::Stroke::new(1.0, theme::glass(0.2)));
        } else {
            if let Some([blur, a]) = chrome.thumb_glow {
                let c = crate::preset::paint(&chrome.thumb_glow_color, app.accent_color());
                ui.painter().add(egui::epaint::Shadow { offset: [0, 0], blur: blur as u8, spread: 0, color: theme::with_alpha(c, a) }.as_shape(art, egui::CornerRadius::same(thumb_r as u8)));
            }
            paint_cover(ui, art, &t.thumbs, thumb_r, 1.0);
        }
        if ah > 0.0 {
            ui.painter().rect_filled(art, thumb_r, theme::shade(0.45 * ah));
            icon_at(ui, art.center(), icon::CHEVRON_UP, 22.0, theme::with_alpha(Color32::WHITE, ah));
        }
        if ar.clicked() {
            app.actions.push(Action::OpenNowPlaying(true));
        }
        let tr = widgets::title_at(ui, pos2(x, cy - 19.0), &t.title, t.explicit, theme::weighted(chrome.bar_title_weight, chrome.bar_title_px), theme::text(), w);
        let tresp = ui.interact(tr, ui.scope_id().with("np-title"), Sense::click());
        menu_on.push(tresp.clone());
        if named(tresp, &widgets::song_name(&t.title, t.explicit)).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            app.actions.push(Action::OpenNowPlaying(true));
        }
        let mut ax = x;
        // A byline too long for its room scrolls while hovered, and still opens the artist.
        let byline_font = theme::regular(chrome.bar_byline_px);
        let byline_color = theme::with_alpha(theme::text_dim(), chrome.bar_byline_alpha);
        let byline_area = Rect::from_min_size(pos2(x, cy + 2.0), vec2(w, chrome.bar_byline_px + 4.0));
        let on_song = ui.rect_contains_pointer(song_area);
        let long = if chrome.byline_marquee && ui.rect_contains_pointer(byline_area) {
            Some(ui.painter().layout_no_wrap(t.artist_line(), byline_font.clone(), byline_color)).filter(|g| g.size().x > w)
        } else {
            None
        };
        if let Some(g) = long {
            const GAP: f32 = 40.0;
            let cycle = g.size().x + GAP;
            let speed = chrome.marquee_secs.map_or(30.0, |s| cycle / s.max(1.0));
            let shift = (now as f32 * speed) % cycle;
            let clip = ui.painter().with_clip_rect(byline_area);
            clip.galley(byline_area.min - vec2(shift, 0.0), g.clone(), byline_color);
            clip.galley(byline_area.min + vec2(cycle - shift, 0.0), g, byline_color);
            if chrome.marquee_fade > 0.0 {
                // The bar's own colour over each end, so it fades rather than cuts.
                let edge = bar_fill(look, 1.0);
                let f = chrome.marquee_fade.min(w / 3.0);
                hgradient(&clip, Rect::from_min_size(byline_area.min, vec2(f, byline_area.height())), edge, Color32::TRANSPARENT);
                hgradient(&clip, Rect::from_min_max(pos2(byline_area.right() - f, byline_area.top()), byline_area.max), Color32::TRANSPARENT, edge);
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
            let resp = named(ui.interact(byline_area, ui.scope_id().with("byline"), Sense::click()), &t.artist_line()).on_hover_cursor(egui::CursorIcon::PointingHand);
            menu_on.push(resp.clone());
            if resp.clicked()
                && let Some(id) = t.artists.first().and_then(|a| a.id.clone())
            {
                app.actions.push(Action::Open(Target::Artist(id)));
            }
            ax = x + w;
        } else {
            for (i, a) in t.artists.iter().enumerate() {
                let s = if i + 1 < t.artists.len() { format!("{}, ", a.name) } else { a.name.clone() };
                let r = text_at(ui, pos2(ax, cy + 2.0), &s, theme::regular(chrome.bar_byline_px), theme::with_alpha(theme::text_dim(), chrome.bar_byline_alpha), (x + w - ax).max(10.0));
                if let Some(id) = &a.id {
                    let resp = ui.interact(r, ui.scope_id().with(("artist", i)), Sense::click());
                    let resp = named(resp, &a.name).on_hover_cursor(egui::CursorIcon::PointingHand);
                    menu_on.push(resp.clone());
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
        }
        // Like: a heart right after the title/artist, filled red when liked.
        // What's heard, like Now playing's menu: a song version standing in for the music video.
        let t = &app.playing_track().cloned().unwrap_or_else(|| t.clone());
        let liked = app.heard_liked();
        let text_end = tr.right().max(ax).min(x + w);
        let lpos = pos2(text_end + 24.0, cy);
        let lr = Rect::from_center_size(lpos, Vec2::splat(36.0));
        if let (Some(slot), Some([a, r])) = (block_slot, chrome.track_block) {
            let block = Rect::from_min_max(art.min - Vec2::splat(4.0), pos2(lr.right() + 4.0, art.bottom() + 4.0));
            ui.painter().set(slot, egui::epaint::RectShape::filled(block, r, theme::glass(a)));
        }
        for r in &menu_on {
            widgets::context_popup(r).show(|ui| widgets::song_menu(ui, t, liked, None, &mut app.actions));
        }
        let lresp = ui.interact(lr, ui.scope_id().with("like"), Sense::click());
        let lresp = named(lresp, if liked { "Remove like" } else { "Like" }).on_hover_text(if liked { "Remove like" } else { "Like" }).on_hover_cursor(egui::CursorIcon::PointingHand);
        let lh = theme::anim_bool(ui.ctx(), lresp.id.with("h"), lresp.hovered(), 0.15);
        let pop = theme::anim_bool(ui.ctx(), ("liked", &t.id), liked, 0.3);
        ui.painter().circle_filled(lpos, 18.0, theme::glass(0.1 * lh));
        let size = 19.0 * (1.0 + (chrome.like_hover_scale - 1.0) * lh) * (1.0 + 0.25 * (pop * std::f32::consts::PI).sin());
        // Hidden at rest in some themes, there as soon as the pointer's on the song, and a liked song always shows it.
        let show_like = !chrome.like_on_hover || liked || on_song || lresp.has_focus();
        let like_a = theme::anim_bool(ui.ctx(), lresp.id.with("show"), show_like, 0.15);
        let idle = theme::lerp_color(theme::text_dim(), theme::text(), lh);
        let outline = theme::with_alpha(theme::lerp_color(idle, theme::RED, pop), like_a);
        if like_a > 0.0 {
            widgets::heart(ui, lpos, size, pop, outline, theme::with_alpha(theme::RED, like_a));
        }
        if lresp.clicked() {
            app.actions.push(Action::LikeHeard(!liked));
        }
    }

    // Centre: transport
    let accent = app.accent_color();
    // Active dots stay inside a short bar, clear of a progress line along its bottom.
    let dot_floor = full.bottom() - 5.0 - if at_bottom { progress_px } else { 0.0 };
    let center = if song_middle { pos2(row.left() + 130.0, cy) } else { pos2(row.center().x, cy) } + vec2(chrome.transport_shift + chrome.transport_shift_frac * full.width(), if time_under { -9.0 } else if time_over { 12.0 } else { 0.0 });
    let ctl = |ui: &mut Ui, dx: f32, glyph: char, name: &str, active: bool, size: f32| -> bool {
        let size = size * bar_scale * chrome.transport_scale.clamp(0.75, 1.5);
        let dx = dx * chrome.transport_gap.clamp(0.7, 1.5);
        let r = Rect::from_center_size(center + vec2(dx, 0.0), Vec2::splat(size + 16.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r));
        if matches!(name, "Previous" | "Next") {
            child.multiply_opacity(chrome.side_alpha.clamp(0.5, 1.0));
        }
        let resp = icon_button(&mut child, glyph, size, name, active);
        if active {
            ui.painter().circle_filled(pos2(r.center().x, (r.bottom() + 2.0).min(dot_floor)), 2.0, accent);
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
    let (rep_icon, rep_on) = match app.queue.repeat {
        Repeat::Off => (icon::REPEAT, false),
        Repeat::All => (icon::REPEAT, true),
        Repeat::One => (icon::REPEAT_1, true),
    };
    if ctl(ui, inner, icon::SKIP_FORWARD, "Next", false, 22.0) {
        app.next();
    }
    if ctl(ui, outer, rep_icon, "Repeat", rep_on, 18.0) {
        app.queue.repeat = match app.queue.repeat {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        };
    }
    let play_size = chrome.play_scale.clamp(0.8, 1.5);
    let pr = Rect::from_center_size(center, Vec2::splat(46.0 * play_size));
    let presp = ui.interact(pr, ui.scope_id().with("play"), Sense::click());
    let presp = named(presp, if playing { "Pause" } else { "Play" }).on_hover_cursor(egui::CursorIcon::PointingHand);
    let ph = theme::anim_bool(ui.ctx(), presp.id.with("h"), presp.hovered(), 0.15);
    let controls = &app.look().app.controls;
    let scale = play_size * if presp.is_pointer_button_down_on() { 0.92 } else { 1.0 + (controls.play_hover_scale - 1.0) * ph };
    let play_lit = crate::preset::hex(&controls.play_hover_color);
    use crate::preset::PlayButton;
    let kind = if app.look().app.accent_controls || app.accent_controls { PlayButton::Accent } else { app.look().chrome.play_button };
    let glyph = match kind {
        PlayButton::Disc => {
            let rest = if theme::light() { theme::text() } else { Color32::WHITE };
            ui.painter().circle_filled(center, 23.0 * scale, play_lit.map_or(rest, |c| theme::lerp_color(rest, c, ph)));
            theme::on_text()
        }
        PlayButton::Bare => {
            ui.painter().circle_filled(center, 23.0 * scale, theme::glass(0.1 * ph));
            play_lit.map_or(theme::text(), |c| theme::lerp_color(theme::text(), c, ph))
        }
        PlayButton::Ghost => {
            ui.painter().circle_filled(center, 23.0 * scale, theme::glass(0.1 + 0.05 * ph));
            ui.painter().circle_stroke(center, 23.0 * scale, egui::Stroke::new(1.0, theme::glass(0.2)));
            theme::text()
        }
        PlayButton::Accent => {
            let a = app.accent_color();
            ui.painter().circle_filled(center, 23.0 * scale, a);
            theme::on_fill(a)
        }
    };
    let morph = theme::anim_bool(ui.ctx(), "play-morph", playing || app.buffering, 0.18);
    if app.buffering {
        let a = now as f32 * 6.0;
        ui.painter().add(egui::Shape::Path(egui::epaint::PathShape::line(
            (0..20).map(|i| {
                let t = a + i as f32 * 0.22;
                center + vec2(t.cos(), t.sin()) * 9.0
            })
            .collect(),
            egui::epaint::PathStroke::new(2.5, glyph),
        )));
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
    } else if morph > 0.5 {
        widgets::pause_bars(ui, center, 8.0 * (2.0 * morph - 1.0).max(0.4), glyph);
    } else {
        widgets::play_triangle(ui, center + vec2(1.5, 0.0), 8.5 * (1.0 - 2.0 * morph).max(0.4), glyph);
    }
    if presp.clicked() {
        app.toggle();
    }
    if time_under
        && let Some(g) = time_g.clone()
    {
        ui.painter().galley(pos2(center.x - g.size().x / 2.0, center.y + 21.0), g, theme::text_dim());
    }
    if time_over
        && let Some(g) = time_g.clone()
    {
        ui.painter().galley(pos2(line.center().x - g.size().x / 2.0, edge_y - g.size().y - 3.0), g, theme::text_dim());
    }

    // Right: time, volume, lyrics, queue, expand
    let mut rx = row.right();
    let mut right_btn = |ui: &mut Ui, glyph: char, name: &str, active: bool| -> bool {
        // The button is the icon's size plus 16: centred, whatever the theme scales it to.
        let size = 18.0 * bar_scale;
        let r = Rect::from_center_size(pos2(rx - 18.0, cy), Vec2::splat(size + 16.0));
        rx -= 40.0;
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r));
        child.multiply_opacity(bar_rest);
        let resp = icon_button(&mut child, glyph, size, name, active);
        if active {
            ui.painter().circle_filled(pos2(r.center().x, (cy + size / 2.0 + 11.0).min(dot_floor)), 2.0, accent);
        }
        resp.clicked()
    };
    // Right to left: now playing keeps the corner (as in YouTube Music), the mini player next to it.
    if right_btn(ui, if app.np_open { icon::CHEVRON_DOWN } else { icon::CHEVRON_UP }, "Now playing", app.np_open) {
        app.actions.push(Action::OpenNowPlaying(!app.np_open));
    }
    if right_btn(ui, icon::PICTURE_IN_PICTURE, "Mini player (Ctrl+M)", false) {
        app.actions.push(Action::Mini(true));
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
    if app.tempo_on {
        tempo_button(app, ui, Rect::from_center_size(pos2(rx - 20.0, cy), vec2(40.0, 18.0 * bar_scale + 16.0)), bar_rest, accent, dot_floor);
        rx -= 44.0;
    }
    // Volume: a slider beside its icon, or in a narrow window the icon alone with the
    // slider in a small popup above it while the pointer is over either. A theme that
    // hides the volume hides only the slider in the bar: the icon and its popup stay.
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
    icon_at(ui, vi.center(), vglyph, 18.0, if mresp.hovered() { theme::text() } else { theme::text_dim() });
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
                ui.painter().rect_filled(r, 20.0, theme::menu_fill_solid());
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
        let stash = unmute_id();
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

/// Where Mute keeps the volume to go back to (the player bar's and the mini player's).
pub fn unmute_id() -> egui::Id {
    egui::Id::unique("unmute")
}

/// The volume track: click or drag to set, scroll over it to nudge.
pub(super) fn volume_slider(app: &mut App, ui: &mut Ui, slider: Rect, salt: &str) {
    let vol = app.audio.volume();
    let vresp = ui.interact(slider, ui.scope_id().with(salt), Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::PointingHand);
    vresp.widget_info(|| egui::WidgetInfo::slider(true, vol as f64, "Volume"));
    let vh = theme::anim_bool(ui.ctx(), ui.scope_id().with((salt, "h")), vresp.hovered() || vresp.dragged(), 0.15);
    let track_r = Rect::from_center_size(slider.center(), vec2(slider.width(), 4.0));
    ui.painter().rect_filled(track_r, 2.0, theme::glass(0.15));
    let fill = Rect::from_min_size(track_r.min, vec2(track_r.width() * vol, 4.0));
    let accent = app.accent_color();
    let (rest, full) = if app.look().app.accent_controls || app.accent_controls { (accent, accent) } else { (theme::text_dim(), theme::text()) };
    ui.painter().rect_filled(fill, 2.0, theme::lerp_color(rest, full, vh));
    ui.painter().circle_filled(pos2(fill.right(), track_r.center().y), 2.0 + 4.5 * vh, full);
    if let Some(p) = vresp.interact_pointer_pos()
        && (vresp.dragged() || vresp.clicked())
    {
        app.audio.set_volume(((p.x - track_r.left()) / track_r.width()).clamp(0.0, 1.0));
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
    let q = &app.look().app;
    // The card is dark in light mode too: what's on it stays white.
    let mut white_on_card = false;
    if q.queue_card
        && let Some(t) = app.current().cloned()
    {
        // The heading on a card of what's playing.
        let r = Rect::from_min_size(ui.cursor().min - vec2(4.0, 6.0), vec2(ui.available_width() + 8.0, 44.0));
        widgets::paint_cover_fill(ui, r, &t.thumbs, Color32::from_gray(110));
        ui.painter().rect_filled(r, 0.0, theme::shade(0.35));
        white_on_card = theme::light();
    }
    ui.horizontal(|ui| {
        let (label, size) = if q.queue_upper { ("UP NEXT", 13.0) } else { ("Up next", 20.0) };
        let heading = egui::RichText::new(label).font(theme::bold(size));
        ui.label(if white_on_card { heading.color(Color32::WHITE) } else { heading });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Left of the window buttons, which sit over the drawer's top right.
            ui.add_space(super::titlebar::right_gap(ui.ctx(), ui.max_rect(), 0.0));
            let close = if white_on_card {
                let (r, resp) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::click());
                let resp = named(resp, "Close queue").on_hover_text("Close queue").on_hover_cursor(egui::CursorIcon::PointingHand);
                let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
                ui.painter().circle_filled(r.center(), 16.0, theme::with_alpha(Color32::WHITE, 0.15 * h));
                widgets::focus_ring(ui, &resp, r, 16.0);
                icon_at(ui, r.center(), icon::X, 16.0, Color32::WHITE);
                resp
            } else {
                icon_button(ui, icon::X, 16.0, "Close queue", false)
            };
            if close.clicked() {
                app.queue_open = false;
            }
        });
    });
    ui.add_space(8.0);
    queue_list(app, ui, "drawer");
}

/// "Playing from playlist", and its name, which opens it.
fn playing_from(app: &mut App, ui: &mut Ui) {
    let Some((kind, name, target)) = app.playing_from() else { return };
    ui.label(egui::RichText::new(format!("Playing from {kind}")).font(theme::regular(12.5)).color(theme::text_dim()));
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, 22.0), Sense::hover());
    let r = text_at(ui, pos2(rect.left(), rect.top() + 1.0), &name, theme::semibold(15.0), theme::text(), w);
    let resp = named(ui.interact(r, ui.scope_id().with("playing-from"), Sense::click()), &name).on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.hovered() {
        ui.painter().hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0, theme::text()));
    }
    if resp.clicked() {
        // Liked Music is Library › Songs, where it was played from.
        let liked = matches!(&target, Target::Playlist(id) if id == super::lists::LIKED_MUSIC);
        app.actions.push(if liked { Action::Go(Page::Library(LibTab::Songs)) } else { Action::Open(target) });
    }
    ui.add_space(8.0);
}

/// The whole queue, scrolled to the current song when it changes: the songs already
/// played are above it. Shared by the drawer and now playing.
pub fn queue_list(app: &mut App, ui: &mut Ui, salt: &str) {
    let rows = super::pages::Rows::new(app, ui.input(|i| i.time));
    let q = app.look().app.queue_look;
    let differs = q.thumb_radius.is_some() || q.radius.is_some() || q.pad.is_some() || q.hover_border.is_some() || q.image_zoom.is_some() || q.image_dim.is_some();
    if app.queue.tracks.is_empty() {
        ui.label(egui::RichText::new("Nothing queued yet").color(theme::text_dim()));
        return;
    }
    let start = app.queue.index.unwrap_or(0);
    // Above the list, so they stay in sight however far up it's scrolled.
    playing_from(app, ui);
    ui.horizontal(|ui| {
        ui.add_space(q.side_pad);
        ui.checkbox(&mut app.autoplay, egui::RichText::new("Autoplay similar songs").color(theme::text_dim())).on_hover_cursor(egui::CursorIcon::PointingHand);
    });
    ui.add_space(6.0);
    // A new current song (or a new queue) brings it back to the top of the list.
    let mut area = egui::ScrollArea::vertical().id_salt(("queue", salt)).auto_shrink(false);
    let shown = egui::Id::unique(("queue-at", salt));
    let at = (app.queue.index, app.queue.current().map(|t| t.id.clone()));
    if ui.data(|d| d.get_temp::<(Option<usize>, Option<String>)>(shown)).as_ref() != Some(&at) {
        ui.data_mut(|d| d.insert_temp(shown, at));
        area = area.vertical_scroll_offset(start as f32 * widgets::row_height(ui));
    }
    ui.data_mut(|d| d.insert_temp(egui::Id::unique("in-queue"), differs));
    area.show(ui, |ui| {
        let accent = app.accent_color();
        let (queue, liked, acts, has_current, by, drag) = (&app.queue.tracks, &app.liked, &mut app.actions, app.queue.index.is_some(), &app.jam.by, &mut app.row_drag);
        // Songs to come can be dragged into another order; in a jam, by the host.
        let (list, movable) = (egui::Id::unique(("queue", salt)), !app.jam.guest());
        let gap = if has_current { q.current_gap } else { 0.0 };
        // Only the songs below the current one can be dragged to; they're `gap` lower.
        let top = ui.cursor().top() + gap;
        widgets::visible_rows_gap(ui, queue.len(), start, gap, |ui, i| {
            let t = &queue[i];
            let current = i == start && has_current;
            if movable && (i > start || !has_current) {
                row_drag::grip(ui, drag, list, i, t);
            }
            let o = widgets::RowOpts { current, added_by: by.get(&t.id).cloned(), ..rows.opts(liked, t, None, false) };
            let resp = widgets::track_row(ui, t, &o, acts);
            if resp.clicked() {
                // The playing row pauses/resumes instead of doing nothing.
                acts.push(if current { Action::TogglePlay } else { Action::QueueJump(i) });
            }
            if !current && drag.is_none() && ui.rect_contains_pointer(Rect::from_min_max(resp.rect.min, pos2(resp.rect.right() + 42.0, resp.rect.bottom()))) {
                // Remove button over the duration column.
                let x = Rect::from_center_size(resp.rect.right_center() - vec2(26.0, 0.0), Vec2::splat(28.0));
                let xr = named(ui.interact(x, resp.id.with("rm"), Sense::click()), "Remove from queue").on_hover_cursor(egui::CursorIcon::PointingHand);
                ui.painter().circle_filled(x.center(), 14.0, theme::panel(0.6));
                icon_at(ui, x.center(), icon::X, 14.0, theme::text());
                if xr.clicked() {
                    acts.push(Action::QueueRemove(i));
                }
            }
        });
        let lowest = if has_current { start + 1 } else { start };
        if let Some(d) = row_drag::feedback(ui, drag, list, row_drag::Rows { top, first: 0, lowest, end: queue.len(), accent }) {
            acts.push(Action::QueueMove { from: d.from, gap: d.gap, id: d.id });
        }
    });
    ui.data_mut(|d| d.insert_temp(egui::Id::unique("in-queue"), false));
}

// ------------------------------------------------------------------ toast

pub fn toast(app: &mut App, ctx: &egui::Context, now: f64) {
    let Some((msg, shown)) = &app.toast else { return };
    if *shown < 0.0 {
        return;
    }
    let age = (now - shown) as f32;
    let tl = app.look().app.toast_look.clone();
    let hold = tl.secs.max(1.5);
    let t = theme::ease_out(age / 0.25) * (1.0 - ((age - hold) / 0.4).clamp(0.0, 1.0));
    let screen = ctx.content_rect();
    egui::Area::new(egui::Id::unique("toast"))
        .order(egui::Order::Tooltip)
        .pivot(Align2::CENTER_BOTTOM)
        // Above the player bar; in the mini player, over the strip's middle.
        .fixed_pos(Pos2::new(screen.center().x, if screen.height() < 200.0 { screen.center().y + 22.0 } else { screen.bottom() - 104.0 } + 14.0 * (1.0 - t)))
        .show(ctx, |ui| {
            ui.set_opacity(t);
            let glass = app.look().app.glass.toast.filter(|_| crate::shader::glass_ready());
            if let (Some(g), Some(last)) = (glass, ctx.memory(|m| m.area_rect(egui::Id::unique("toast")))) {
                crate::shader::frost(ui.painter(), last, crate::shader::Frost { look: g, radius: tl.radius, opacity: t, fade: 0.0 });
            }
            egui::Frame::new()
                .fill(match crate::preset::hex(&tl.color).filter(|_| !theme::light()) {
                    // A pale see-through toast under light text only works frosted.
                    Some(c) if glass.is_none() && tl.alpha < 0.6 && tl.text_color.is_empty() && c.r() as u32 + c.g() as u32 + c.b() as u32 > 380 => theme::menu_fill_solid(),
                    Some(c) => theme::with_alpha(c, if glass.is_some() { tl.alpha } else { tl.alpha.max(0.85) }),
                    None if glass.is_some() => theme::menu_fill(),
                    None => theme::menu_fill_solid(),
                })
                .stroke(egui::Stroke::new(tl.border[1], theme::glass(tl.border[0])))
                .corner_radius(tl.radius.clamp(0.0, 40.0) as u8)
                .inner_margin(egui::Margin::symmetric(18, 12))
                .shadow(egui::epaint::Shadow { offset: [0, 8], blur: 24, spread: 0, color: theme::shade(0.45) })
                .show(ui, |ui| {
                    ui.set_max_width(520.0);
                    let color = crate::preset::hex(&tl.text_color).filter(|_| !theme::light()).unwrap_or_else(theme::text);
                    ui.label(egui::RichText::new(msg.as_str()).font(theme::regular(14.0)).color(color));
                });
        });
    // Frames only while it fades in or out; in between it just waits.
    if (0.25..hold).contains(&age) {
        ctx.request_repaint_after(std::time::Duration::from_secs_f32(hold - age));
    } else if age < hold + 0.4 {
        ctx.request_repaint();
    } else {
        // Faded out: gone, whatever else would have kept it.
        app.toast = None;
    }
}
