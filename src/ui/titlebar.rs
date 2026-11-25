//! The app's own title bar: no system frame, the window buttons drawn at the top right,
//! the empty top of the window to move it by (double-click maximizes), and its edges to
//! resize it by. On Windows always (winit keeps the window styles snapping needs, so
//! dragging to a screen edge and Win+arrows still work). On Linux where the desktop
//! draws no frame for a Wayland window (GNOME), or when Settings › Window asks for it.
//!
//! On macOS the system's frame stays (its corners, shadow, resizing and full screen),
//! but not its title strip: the app runs up to the window's top, and the system's window
//! buttons (the traffic lights) sit at the left of the app's own top row, on its line.

use egui::{Color32, CursorIcon, Id, Pos2, Rect, ResizeDirection, Sense, Stroke, Ui, ViewportCommand, pos2, vec2};

use crate::theme;

/// The top of the window, the search box's row included: its middle is the line the
/// search box and the logo share.
pub const HEIGHT: f32 = 56.0;
/// The notch at the window's top right that holds the window buttons, hanging from its
/// top edge, and what a page puts beside them: this tall.
const NOTCH_H: f32 = 32.0;
/// A button in the notch, and how far its highlight stays from the window's top: clear
/// of the notch's bottom edge too.
pub const NOTCH_BUTTON: egui::Vec2 = vec2(30.0, 24.0);
const NOTCH_TOP: f32 = 3.0;
const NOTCH_GAP: f32 = 2.0;
/// From the window's edge to the outermost button, and from the notch's curved side to
/// its innermost one, which clears the curve.
const NOTCH_PAD_EDGE: f32 = 3.0;
const NOTCH_PAD_IN: f32 = 8.0;
/// The curve where the notch meets the window's top edge, and its rounded bottom corner.
const NOTCH_FLARE: f32 = 8.0;
const NOTCH_CURVE: f32 = 14.0;
/// Between the window buttons and what a page puts in the notch: a thin line.
const NOTCH_SEP: f32 = 7.0;
const BUTTONS_W: f32 = 3.0 * NOTCH_BUTTON.x + 2.0 * NOTCH_GAP;
/// A classic window button's width, as Windows draws them: the title bar's full height.
const CLASSIC_BUTTON_W: f32 = 46.0;
/// How far in from the window's edge resizing starts, and along a corner.
const EDGE: f32 = 5.0;
const CORNER: f32 = 14.0;

/// Linux: who draws the title bar (Settings › Window), saved in `titlebar` in the data
/// folder since the window is made with or without a frame before settings load.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum Choice {
    /// The app's where the desktop draws none, the system's elsewhere.
    #[default]
    Auto,
    Own,
    System,
}

impl Choice {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Own, Self::System];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => crate::tr!("ui-automatic"),
            Self::Own => crate::tr!("window-app-titlebar"),
            Self::System => crate::tr!("window-system-titlebar"),
        }
    }

    pub fn load(data_dir: &std::path::Path) -> Self {
        match std::fs::read_to_string(data_dir.join("titlebar")).unwrap_or_default().trim() {
            "own" => Self::Own,
            "system" => Self::System,
            _ => Self::Auto,
        }
    }

    pub fn save(self, data_dir: &std::path::Path) -> std::io::Result<()> {
        let key = match self {
            Self::Auto => "auto",
            Self::Own => "own",
            Self::System => "system",
        };
        std::fs::write(data_dir.join("titlebar"), key)
    }

    /// Whether the app draws the title bar on Linux with this choice. `wayland`: the
    /// window is a Wayland one; `desktop`: `XDG_CURRENT_DESKTOP`.
    pub fn own(self, wayland: bool, desktop: &str) -> bool {
        match self {
            Self::Own => true,
            Self::System => false,
            // GNOME draws no frame around Wayland windows (it has no xdg-decoration),
            // and what winit draws in its place is a bare strip. X11 windows get one.
            Self::Auto => wayland && desktop.split(':').any(|d| d.trim().eq_ignore_ascii_case("gnome")),
        }
    }

    /// As [`Self::own`], for this session.
    pub fn own_here(self) -> bool {
        self.own(crate::wayland_window(), &std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default())
    }
}

static CUSTOM: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Settle who draws this run's title bar, before the window is made.
pub fn init(data_dir: &std::path::Path) {
    let _ = CUSTOM.set(decide(data_dir));
}

/// Where the traffic lights' left edge goes, and the room they take at the window's
/// top left (the lights, and as much again after them as before).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const LIGHTS_X: f32 = 20.0;
const LIGHTS_W: f32 = 84.0;

/// The three window buttons, from the left edge of the first to the right edge of the
/// last (macOS 26's; earlier ones are a little narrower). `LIGHTS_W` is the room kept
/// for them at the top left; this is the buttons alone.
const LIGHTS_SPAN: f32 = 60.0;

/// The narrowest the sidebar's icon rail can be: on macOS the window buttons sit over its
/// top, with as much room after them as before, so its edge doesn't run up against them.
/// It stays this wide in fullscreen, where they're hidden, so the rail doesn't jump going
/// in and out.
pub fn rail_min() -> f32 {
    if inset() { LIGHTS_X + LIGHTS_SPAN + LIGHTS_X } else { 0.0 }
}

/// macOS: the system's window buttons over the app's top row, the system's title strip
/// gone. `YTM_NATIVE_TITLEBAR` keeps the strip.
pub fn inset() -> bool {
    cfg!(target_os = "macos") && std::env::var_os("YTM_NATIVE_TITLEBAR").is_none()
}

/// Whether the window's top is the app's 56-point row, the window buttons on its line:
/// the app's own title bar, or macOS's buttons over the app.
pub fn own_row() -> bool {
    custom() || inset()
}

fn decide(data_dir: &std::path::Path) -> bool {
    if std::env::var_os("YTM_NATIVE_TITLEBAR").is_some() {
        return false;
    }
    if cfg!(target_os = "linux") {
        return Choice::load(data_dir).own_here();
    }
    cfg!(windows)
}

/// Whether the app draws the title bar itself: on Windows, and on Linux as [`init`]
/// settled it, unless `YTM_NATIVE_TITLEBAR` asks for the system's.
pub fn custom() -> bool {
    *CUSTOM.get_or_init(|| cfg!(windows) && std::env::var_os("YTM_NATIVE_TITLEBAR").is_none())
}

/// Full screen has no title bar. The app's own full screen (`crate::fullscreen`) goes
/// around winit, so the app says when it's on, once a frame before drawing.
pub fn set_fullscreen(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(Id::unique("titlebar-fullscreen"), on));
}

/// The app's top row is the title bar now: not in full screen.
fn shown(ctx: &egui::Context) -> bool {
    own_row() && !ctx.data(|d| d.get_temp::<bool>(Id::unique("titlebar-fullscreen")).unwrap_or(false)) && !ctx.input(|i| i.viewport().fullscreen.unwrap_or(false))
}

/// Settings › Appearance › Classic title bar: the window buttons square along the top
/// right, as before the notch, and what went in the notch back in its page. Once a
/// frame before drawing.
pub fn set_classic(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(Id::unique("titlebar-classic"), on));
}

fn classic(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(Id::unique("titlebar-classic")).unwrap_or(false))
}

fn maximized(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().maximized.unwrap_or(false))
}

/// Which top corner a page's buttons go in: the window buttons' notch at the right, or
/// one of their own at the left.
#[derive(Clone, Copy, PartialEq)]
pub enum Side {
    Start,
    End,
}

/// How much room pages have taken in each notch, this pass and the last: a row drawn
/// before the page that fills the notch keeps clear of it all the same.
#[derive(Clone, Copy, Default)]
struct Taken {
    pass: u64,
    end: f32,
    start: f32,
    last_end: f32,
}

fn taken(ctx: &egui::Context) -> Taken {
    let pass = ctx.cumulative_pass_nr();
    let t = ctx.data(|d| d.get_temp::<Taken>(Id::unique("notch-taken"))).unwrap_or_default();
    if t.pass == pass { t } else { Taken { pass, end: 0.0, start: 0.0, last_end: t.end } }
}

/// The width of what pages put in a notch, the gaps between them included.
fn items_w(taken: f32) -> f32 {
    (taken - NOTCH_GAP).max(0.0)
}

/// The notch's own part, not its curve into the window's top edge: the window buttons
/// and what's beside them at the right, or a page's at the left. None while there's no
/// title bar (full screen), or nothing in the left one.
fn notch_body(ctx: &egui::Context, side: Side, end: f32, start: f32) -> Option<Rect> {
    if !notch_shown(ctx) {
        return None;
    }
    let s = ctx.content_rect();
    let px = 1.0 / ctx.pixels_per_point();
    // On whole pixels: its straight edges stay sharp, and meet the curve without a seam.
    let snap = |x: f32| (x / px).round() * px;
    match side {
        Side::End => {
            let items = items_w(end);
            let w = NOTCH_PAD_EDGE + BUTTONS_W + if items > 0.0 { NOTCH_SEP + items } else { 0.0 } + NOTCH_PAD_IN;
            Some(Rect::from_min_max(pos2(snap(s.right() - w), s.top()), pos2(s.right(), snap(s.top() + NOTCH_H))))
        }
        Side::Start => {
            let items = items_w(start);
            (items > 0.0).then(|| Rect::from_min_max(s.min, pos2(snap(s.left() + NOTCH_PAD_EDGE + items + NOTCH_PAD_IN), snap(s.top() + NOTCH_H))))
        }
    }
}

/// The window buttons' notch, its curve included, or the classic buttons' corner: what's
/// drawn at the top right stays clear of it.
pub fn buttons_rect(ctx: &egui::Context) -> Option<Rect> {
    if classic(ctx) {
        return classic_rect(ctx);
    }
    let t = taken(ctx);
    notch_body(ctx, Side::End, t.end.max(t.last_end), 0.0).map(|b| b.with_min_x(b.left() - NOTCH_FLARE))
}

/// The layer the notches are painted on, and the one just above it for what's in them.
fn notch_layer() -> egui::LayerId {
    egui::LayerId::new(egui::Order::Foreground, Id::unique("window-buttons"))
}

fn notch_items_layer() -> egui::LayerId {
    egui::LayerId::new(egui::Order::Foreground, Id::unique("notch-items"))
}

/// Room for `w` points of a page's buttons in a notch, laid out from the window's edge
/// inwards (right to left in the right one), on the notch's layer: none while there's no
/// title bar. Each call this pass goes further in; the notch grows to hold them.
pub fn notch_ui(ui: &mut Ui, side: Side, w: f32) -> Option<Ui> {
    let ctx = ui.ctx();
    if !notch_shown(ctx) {
        return None;
    }
    let s = ctx.content_rect();
    let mut t = taken(ctx);
    let (x0, x1, layout) = match side {
        Side::End => {
            let right = s.right() - NOTCH_PAD_EDGE - BUTTONS_W - NOTCH_SEP - t.end;
            t.end += w + NOTCH_GAP;
            (right - w, right, egui::Layout::right_to_left(egui::Align::Center))
        }
        Side::Start => {
            let left = s.left() + NOTCH_PAD_EDGE + t.start;
            t.start += w + NOTCH_GAP;
            (left, left + w, egui::Layout::left_to_right(egui::Align::Center))
        }
    };
    ctx.data_mut(|d| d.insert_temp(Id::unique("notch-taken"), t));
    // Over the notch, which is painted at the end of the pass.
    ctx.set_sublayer(notch_layer(), notch_items_layer());
    let rect = Rect::from_x_y_ranges(x0..=x1, s.top() + NOTCH_TOP..=s.top() + NOTCH_TOP + NOTCH_BUTTON.y);
    let mut child = ui.new_child(egui::UiBuilder::new().layer_id(notch_items_layer()).max_rect(rect).layout(layout));
    child.set_clip_rect(s);
    // The notch doesn't fade or slide with what put them there (now playing coming up).
    child.set_opacity(1.0);
    // Exactly `w` wide: the caller spaces its buttons itself.
    child.spacing_mut().item_spacing.x = 0.0;
    Some(child)
}

/// Whether the notches are up: the app's own title bar, not classic, not in full screen.
/// While they are, what goes in them never goes back to its place in the page.
pub fn notch_shown(ctx: &egui::Context) -> bool {
    custom() && shown(ctx) && !classic(ctx)
}

/// The classic window buttons' corner, the title bar's full height.
fn classic_rect(ctx: &egui::Context) -> Option<Rect> {
    (custom() && shown(ctx)).then(|| {
        let screen = ctx.content_rect();
        Rect::from_min_size(pos2(screen.right() - 3.0 * CLASSIC_BUTTON_W, screen.top()), vec2(3.0 * CLASSIC_BUTTON_W, HEIGHT))
    })
}

/// The fill behind a highlighted button in a notch.
pub fn notch_highlight(ui: &Ui, r: Rect, h: f32, down: bool) {
    if h > 0.0 {
        ui.painter().rect_filled(r, 7.0, theme::glass(if down { 0.20 } else { 0.14 } * h));
    }
}

/// An icon button the size of the window buttons, for a notch.
pub fn notch_button(ui: &mut Ui, glyph: char, size: f32, name: &str, on: bool) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(NOTCH_BUTTON, Sense::click());
    let resp = super::widgets::named(resp, name).on_hover_text(name).on_hover_cursor(CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() || on, 0.12);
    notch_highlight(ui, r, h, resp.is_pointer_button_down_on());
    super::widgets::focus_ring(ui, &resp, r, 7.0);
    super::widgets::icon_at(ui, r.center(), glyph, size, theme::lerp_color(theme::text_dim(), theme::text(), h));
    resp
}

/// A chip that fits a notch: white while `selected`.
pub fn notch_chip(ui: &mut Ui, label: &str, selected: bool) -> egui::Response {
    let text = ui.painter().layout_no_wrap(label.to_owned(), theme::semibold(12.5), Color32::PLACEHOLDER);
    let (r, resp) = ui.allocate_exact_size(vec2(text.size().x + 22.0, NOTCH_BUTTON.y), Sense::click());
    let resp = super::widgets::named(resp, label).on_hover_cursor(CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
    let s = theme::anim_bool(ui.ctx(), resp.id.with("s"), selected, 0.2);
    ui.painter().rect_filled(r, 7.0, theme::lerp_color(theme::glass(0.12 + 0.06 * h), theme::text(), s));
    super::widgets::focus_ring(ui, &resp, r, 7.0);
    let fg = theme::lerp_color(theme::text(), Color32::from_gray(15), s);
    ui.painter().galley(r.center() - text.size() / 2.0, text, fg);
    resp
}

/// The notches: darker than what's behind them, in its colour; lighter on light pages.
fn notch_fill() -> Color32 {
    if theme::light() { theme::with_alpha(Color32::WHITE, 0.55) } else { theme::with_alpha(Color32::BLACK, 0.42) }
}

/// A notch: `body` and the curve where it meets the window's top edge, beside it.
fn paint_notch(painter: &egui::Painter, body: Rect, side: Side, fill: Color32) {
    let corner = match side {
        Side::End => egui::CornerRadius { sw: NOTCH_CURVE as u8, ..Default::default() },
        Side::Start => egui::CornerRadius { se: NOTCH_CURVE as u8, ..Default::default() },
    };
    painter.rect_filled(body, corner, fill);
    // The curve: the corner between the notch's side and the window's top, less a
    // quarter circle, its arc softened over a pixel.
    let px = 1.0 / painter.ctx().pixels_per_point();
    let (k, c, from, to) = match side {
        Side::End => (body.left_top(), pos2(body.left() - NOTCH_FLARE, body.top() + NOTCH_FLARE), -std::f32::consts::FRAC_PI_2, 0.0),
        Side::Start => (body.right_top(), pos2(body.right() + NOTCH_FLARE, body.top() + NOTCH_FLARE), -std::f32::consts::FRAC_PI_2, -std::f32::consts::PI),
    };
    let clear = theme::with_alpha(fill, 0.0);
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(k, fill);
    const N: u32 = 12;
    for i in 0..=N {
        let a = from + (to - from) * i as f32 / N as f32;
        let dir = vec2(a.cos(), a.sin());
        mesh.colored_vertex(c + dir * (NOTCH_FLARE + px / 2.0), fill);
        mesh.colored_vertex(c + dir * (NOTCH_FLARE - px / 2.0), clear);
    }
    for i in 0..N {
        let (s0, t0, s1, t1) = (1 + 2 * i, 2 + 2 * i, 3 + 2 * i, 4 + 2 * i);
        mesh.add_triangle(0, s0, s1);
        mesh.add_triangle(s0, t0, s1);
        mesh.add_triangle(t0, t1, s1);
    }
    painter.add(mesh);
}

/// The top margin above a row `row` tall at the top of the window: `normal`, or with the
/// app's title bar what centres it on the window buttons' line.
pub fn top_margin(normal: i8, row: f32) -> i8 {
    if own_row() { ((HEIGHT - row) / 2.0).round() as i8 } else { normal }
}

/// How much of `row` (a strip along the top) the window buttons cover from its right.
fn overlap(ctx: &egui::Context, row: Rect) -> f32 {
    buttons_rect(ctx).map_or(0.0, |b| (row.right() - b.left()).max(0.0))
}

/// The space to leave at the right of a row along the top: `normal`, or beside the
/// window buttons when they cover its end.
pub fn right_gap(ctx: &egui::Context, row: Rect, normal: f32) -> f32 {
    let o = overlap(ctx, row);
    if o > 0.0 { o + 10.0 } else { normal }
}

/// macOS: the traffic lights' corner, at the window's top left, which what's drawn
/// there stays clear of. None in full screen, which has none.
pub fn lights_rect(ctx: &egui::Context) -> Option<Rect> {
    (inset() && shown(ctx)).then(|| Rect::from_min_size(ctx.content_rect().min, vec2(LIGHTS_W, HEIGHT)))
}

/// The space to leave at the left of a row along the top: `normal`, or past the traffic
/// lights when they're over its start.
pub fn left_gap(ctx: &egui::Context, row: Rect, normal: f32) -> f32 {
    match lights_rect(ctx) {
        Some(l) if row.left() < l.right() && row.top() < l.bottom() => (l.right() - row.left()).max(normal),
        _ => normal,
    }
}

/// How far down the sidebar's content starts so the traffic lights have the top row to
/// themselves: they and the sidebar's menu button and logo don't fit side by side.
pub fn below_lights(ctx: &egui::Context) -> f32 {
    if lights_rect(ctx).is_some() { HEIGHT } else { 0.0 }
}

/// Once a frame, before drawing: macOS keeps the traffic lights on the app's top row, and
/// Windows forgets where the maximize button was until it's drawn again (not in the mini
/// player or full screen). `full`: the full window is up (the mini player keeps the
/// system's title strip), None while switching between them.
///
/// macOS puts them back where its own title strip would have them when it lays the
/// window out again (resized, full screen left, the frame back after the mini player),
/// and that can come after the frame drawn for the change. So for a moment after one,
/// it's looked at again a few times a second, until they stay put.
pub fn sync(ctx: &egui::Context, full: Option<bool>) {
    #[cfg(windows)]
    frame::snap_forget();
    #[cfg(target_os = "macos")]
    if inset() {
        let id = Id::unique("titlebar-sync");
        let (now, state) = ctx.input(|i| (i.time, (i.viewport().inner_rect.map(|r| r.size()), i.viewport().fullscreen, i.focused)));
        let (last, until) = ctx.data(|d| d.get_temp::<((Option<egui::Vec2>, Option<bool>, bool), f64)>(id)).map_or((None, 0.0), |(l, u)| (Some(l), u));
        let until = if last != Some(state) { now + 1.0 } else { until };
        let moved = full.is_some_and(|full| mac::sync(full, f64::from(HEIGHT) / 2.0, f64::from(LIGHTS_X)));
        let until = if moved { until.max(now + 0.5) } else { until };
        ctx.data_mut(|d| d.insert_temp(id, (state, until)));
        if now < until {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    let _ = (ctx, full);
}

/// Move the window by `rect` where nothing else on it takes the pointer; double-click
/// maximizes or restores it. Call it before what's drawn on that strip.
///
/// It only senses clicks: egui gives a drag to whatever senses drags under a button
/// that doesn't, so a press on Back that then moved would move the window. What moves
/// it is a press that landed here (nothing clickable on top), noted on the press
/// itself (egui forgets a press on something click-only once it moves too far for a
/// click), and then dragged. Not focusable: Tab skips it.
pub fn drag_area(ui: &Ui, rect: Rect, id: &str) {
    if !shown(ui.ctx()) {
        return;
    }
    if toggles(ui, &drag_strip(ui, rect, id)) {
        let max = maximized(ui.ctx());
        // macOS: what System Settings › Desktop & Dock says a double-click on a title bar does.
        #[cfg(target_os = "macos")]
        match mac::double_click() {
            mac::DoubleClick::Minimize => return ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true)),
            mac::DoubleClick::Nothing => return,
            mac::DoubleClick::Zoom => {}
        }
        ui.ctx().send_viewport_cmd(ViewportCommand::Maximized(!max));
    }
}

/// Whether a click on the strip `resp` maximizes or restores the window: a finger's by
/// [`double_tapped`] alone. egui counts taps as clicks too, so letting its double-click in
/// as well would toggle the window back on a quick third tap.
fn toggles(ui: &Ui, resp: &egui::Response) -> bool {
    let finger = ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Touch { .. })));
    if finger { double_tapped(ui, resp) } else { resp.double_clicked() }
}

/// A finger's double tap on `resp`. A finger lands less exactly than a mouse clicks, and
/// egui's double-click wants both clicks within 6 points and 0.3 s (#190): a tap counts
/// within a touch target's width of the one before, and GNOME's double-click time.
fn double_tapped(ui: &Ui, resp: &egui::Response) -> bool {
    const DELAY: f64 = 0.4;
    const NEAR: f32 = 48.0;
    let (Some(at), true) = (resp.interact_pointer_pos(), resp.clicked()) else { return false };
    let now = ui.input(|i| i.time);
    let id = resp.id.with("tap");
    let double = ui.data(|d| d.get_temp::<(f64, Pos2)>(id)).is_some_and(|(t, p)| now - t < DELAY && p.distance(at) < NEAR);
    ui.data_mut(|d| {
        // A third tap starts over, rather than toggling back.
        if double {
            d.remove::<(f64, Pos2)>(id);
        } else {
            d.insert_temp(id, (now, at));
        }
    });
    double
}

/// [`drag_area`] without the double-click, with or without the system's frame (the mini
/// player moves by its background everywhere): the response, for the caller's double-click.
pub fn drag_strip(ui: &Ui, rect: Rect, id: &str) -> egui::Response {
    let resp = ui.interact(rect, Id::unique(("titlebar-drag", id)), Sense::CLICK);
    let pressed_here = resp.id.with("pressed");
    let (pressed, down, dragging) = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.is_decidedly_dragging()));
    if pressed {
        let here = resp.is_pointer_button_down_on();
        ui.ctx().data_mut(|d| d.insert_temp(pressed_here, here));
    } else if !down {
        ui.ctx().data_mut(|d| d.remove::<bool>(pressed_here));
    }
    if !resp.double_clicked() && down && dragging && ui.ctx().data_mut(|d| d.remove_temp::<bool>(pressed_here)).unwrap_or(false) {
        // Once: the system moves the window until the button is let go.
        let from = ui.input(|i| i.pointer.press_origin().map(|p| p * i.pixels_per_point())).map(|p| (p.x.round() as i32, p.y.round() as i32));
        if !from.is_some_and(crate::fullscreen::start_drag) {
            ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
        }
    }
    resp
}

/// How far in from the window's right edge a scroll bar there sits, clear of the edge
/// that resizes the window.
pub fn edge_inset() -> f32 {
    if custom() { EDGE } else { 0.0 }
}

/// Interactive hit area for a window button inside the top-right notch: reaches the
/// window's top edge, the notch's bottom, and for Close the window's right edge so throwing
/// the pointer into the corner while maximized still highlights and clicks Close.
fn window_button_hit(r: Rect, screen: Rect, notch_bottom: f32, close: bool) -> Rect {
    Rect::from_min_max(pos2(r.left(), screen.top()), pos2(if close { screen.right() } else { r.right() }, notch_bottom))
}

/// The notches, and in the right one minimize, maximize or restore, and close. Last in
/// the pass, once pages have put what they will in them.
pub fn window_buttons(ctx: &egui::Context) {
    if classic(ctx) {
        return classic_buttons(ctx);
    }
    let t = taken(ctx);
    let Some(body) = notch_body(ctx, Side::End, t.end, 0.0) else { return };
    let start = notch_body(ctx, Side::Start, 0.0, t.start);
    let max = maximized(ctx);
    let screen = ctx.content_rect();
    // The layer is this area's, so what pages put in the notch (just above it) moves with it.
    egui::Area::new(Id::unique("window-buttons")).order(egui::Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
        ui.set_clip_rect(screen);
        let fill = notch_fill();
        // The notches move the window too, between their buttons.
        for (b, side) in [(Some(body), Side::End), (start, Side::Start)] {
            let Some(b) = b else { continue };
            paint_notch(ui.painter(), b, side, fill);
            drag_area(ui, b, if side == Side::End { "notch-end" } else { "notch-start" });
        }
        let items = items_w(t.end);
        if items > 0.0 {
            let x = screen.right() - NOTCH_PAD_EDGE - BUTTONS_W - NOTCH_SEP / 2.0;
            let mid = screen.top() + NOTCH_TOP + NOTCH_BUTTON.y / 2.0;
            ui.painter().vline(x, mid - 7.0..=mid + 7.0, Stroke::new(1.0, theme::glass(0.18)));
        }
        let left = screen.right() - NOTCH_PAD_EDGE - BUTTONS_W;
        for (i, (kind, name)) in kinds(max).into_iter().enumerate() {
            let r = Rect::from_min_size(pos2(left + i as f32 * (NOTCH_BUTTON.x + NOTCH_GAP), screen.top() + NOTCH_TOP), NOTCH_BUTTON);
            let hit = window_button_hit(r, screen, body.bottom(), kind == Kind::Close);
            let resp = ui.interact(hit, Id::unique(("window-button", i)), Sense::click());
            let resp = super::widgets::named(resp, name);
            let snap = if kind.maximizes() { snap(ui, if max { hit } else { hit.with_min_y(screen.top() + EDGE) }) } else { Snap::default() };
            paint_button(ui, r, kind, &resp, snap, false);
            if resp.has_focus() {
                ui.painter().rect_stroke(r, 7.0, Stroke::new(1.5, theme::text()), egui::StrokeKind::Inside);
            }
            if resp.clicked() || snap.clicked {
                ctx.send_viewport_cmd(kind.command());
            }
        }
    });
}

/// The classic window buttons: square, side by side at the top right, the title bar's
/// full height, as Windows draws them.
fn classic_buttons(ctx: &egui::Context) {
    let Some(rect) = classic_rect(ctx) else { return };
    let max = maximized(ctx);
    egui::Area::new(Id::unique("window-buttons")).order(egui::Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
        for (i, (kind, name)) in kinds(max).into_iter().enumerate() {
            let r = Rect::from_min_size(rect.min + vec2(i as f32 * CLASSIC_BUTTON_W, 0.0), vec2(CLASSIC_BUTTON_W, HEIGHT));
            // Close reaches the window's right edge, so the pointer thrown into the corner
            // while maximized still clicks it.
            let hit = if kind == Kind::Close { r.with_max_x(ctx.content_rect().right()) } else { r };
            // Not the notch's ids: a click held through the switch mustn't land on what's under it now.
            let resp = super::widgets::named(ui.interact(hit, Id::unique(("classic-window-button", i)), Sense::click()), name);
            let top = ctx.content_rect().top() + if max { 0.0 } else { EDGE };
            let snap = if kind.maximizes() { snap(ui, r.with_min_y(top)) } else { Snap::default() };
            paint_button(ui, r, kind, &resp, snap, true);
            if resp.has_focus() {
                ui.painter().rect_stroke(r.shrink(3.0), 3.0, Stroke::new(1.5, theme::text()), egui::StrokeKind::Inside);
            }
            if resp.clicked() || snap.clicked {
                ctx.send_viewport_cmd(kind.command());
            }
        }
    });
}

/// Minimize, maximize or restore (`max`: the window is maximized), and close, with their names.
fn kinds(max: bool) -> [(Kind, &'static str); 3] {
    [
        (Kind::Minimize, crate::tr!("window-minimize")),
        if max { (Kind::Restore, crate::tr!("window-restore")) } else { (Kind::Maximize, crate::tr!("window-maximize")) },
        (Kind::Close, crate::tr!("ui-close")),
    ]
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Minimize,
    Maximize,
    Restore,
    Close,
}

impl Kind {
    fn maximizes(self) -> bool {
        matches!(self, Self::Maximize | Self::Restore)
    }

    fn command(self) -> ViewportCommand {
        match self {
            Self::Minimize => ViewportCommand::Minimized(true),
            Self::Maximize => ViewportCommand::Maximized(true),
            Self::Restore => ViewportCommand::Maximized(false),
            Self::Close => ViewportCommand::Close,
        }
    }
}

/// What Windows 11 tells of the pointer over the maximize button, which it takes for its
/// own there (for its snap layouts) so egui sees the pointer leave. Nothing elsewhere.
#[derive(Clone, Copy, Default)]
struct Snap {
    hovered: bool,
    down: bool,
    clicked: bool,
}

/// Have Windows 11 take `hit` (in points) for the window's maximize button this frame,
/// so hovering it shows the snap layouts.
fn snap(ui: &Ui, hit: Rect) -> Snap {
    #[cfg(windows)]
    return frame::snap(ui, hit);
    #[cfg(not(windows))]
    {
        let _ = (ui, hit);
        Snap::default()
    }
}

/// A window button in `r`: rounded in the notch, or square and `classic`.
fn paint_button(ui: &Ui, r: Rect, kind: Kind, resp: &egui::Response, snap: Snap, classic: bool) {
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() || snap.hovered, 0.1);
    let down = resp.is_pointer_button_down_on() || snap.down;
    let p = ui.painter();
    let radius = if classic { 0.0 } else { 7.0 };
    let fg = if kind == Kind::Close {
        // Windows' own close red, and a darker one while held.
        let red = match (classic, down) {
            (true, true) => Color32::from_rgb(0x94, 0x1F, 0x15),
            (true, false) => Color32::from_rgb(0xC4, 0x2B, 0x1C),
            (false, true) => Color32::from_rgb(0xB8, 0x0F, 0x1D),
            (false, false) => Color32::from_rgb(0xE8, 0x11, 0x23),
        };
        if h > 0.0 {
            p.rect_filled(r, radius, red.gamma_multiply(h));
        }
        theme::lerp_color(theme::text_dim(), Color32::WHITE, h)
    } else {
        if !classic {
            notch_highlight(ui, r, h, down);
        } else if h > 0.0 {
            p.rect_filled(r, 0.0, theme::glass(if down { 0.16 } else { 0.10 } * h));
        }
        theme::lerp_color(theme::text_dim(), theme::text(), h)
    };
    // Thin glyphs, ten points across, on whole pixels so they stay sharp.
    let px = 1.0 / ui.ctx().pixels_per_point();
    let snap = |p: Pos2| pos2((p.x / px).round() * px + px / 2.0, (p.y / px).round() * px + px / 2.0);
    let c = snap(r.center());
    let s = Stroke::new(1.0, fg);
    match kind {
        Kind::Minimize => {
            p.line_segment([c + vec2(-5.0, 0.0), c + vec2(5.0, 0.0)], s);
        }
        Kind::Maximize => {
            p.rect_stroke(Rect::from_center_size(c, vec2(10.0, 10.0)), if classic { 1.0 } else { 2.0 }, s, egui::StrokeKind::Middle);
        }
        Kind::Restore => {
            p.rect_stroke(Rect::from_center_size(c + vec2(-1.0, 1.0), vec2(8.0, 8.0)), if classic { 1.0 } else { 1.5 }, s, egui::StrokeKind::Middle);
            // The window behind: its top and right edges, showing past the front one.
            let b = Rect::from_center_size(c + vec2(1.0, -1.0), vec2(8.0, 8.0));
            p.line_segment([pos2(b.left() + 2.0, b.top()), b.right_top()], s);
            p.line_segment([b.right_top(), pos2(b.right(), b.bottom() - 2.0)], s);
        }
        Kind::Close => {
            p.line_segment([c + vec2(-5.0, -5.0), c + vec2(5.0, 5.0)], s);
            p.line_segment([c + vec2(-5.0, 5.0), c + vec2(5.0, -5.0)], s);
        }
    }
}

/// Resize by the window's edges and corners (not while maximized). Drawn last, above
/// everything, but only a few points deep.
pub fn resize_edges(ctx: &egui::Context) {
    edges(ctx, false);
}

/// Resize by the left and right edges only: the mini player is as tall as its contents.
pub fn resize_sides(ctx: &egui::Context) {
    edges(ctx, true);
}

fn edges(ctx: &egui::Context, sides_only: bool) {
    // macOS resizes its own frame.
    if !custom() || !shown(ctx) || maximized(ctx) {
        return;
    }
    let s = ctx.content_rect();
    let (l, r, t, b) = (s.left(), s.right(), s.top(), s.bottom());
    let edges = [
        (Rect::from_min_max(pos2(l + CORNER, t), pos2(r - CORNER, t + EDGE)), ResizeDirection::North, CursorIcon::ResizeNorth),
        (Rect::from_min_max(pos2(l + CORNER, b - EDGE), pos2(r - CORNER, b)), ResizeDirection::South, CursorIcon::ResizeSouth),
        (Rect::from_min_max(pos2(l, t + CORNER), pos2(l + EDGE, b - CORNER)), ResizeDirection::West, CursorIcon::ResizeWest),
        (Rect::from_min_max(pos2(r - EDGE, t + CORNER), pos2(r, b - CORNER)), ResizeDirection::East, CursorIcon::ResizeEast),
        (Rect::from_min_max(pos2(l, t), pos2(l + CORNER, t + EDGE)), ResizeDirection::NorthWest, CursorIcon::ResizeNorthWest),
        (Rect::from_min_max(pos2(l, t), pos2(l + EDGE, t + CORNER)), ResizeDirection::NorthWest, CursorIcon::ResizeNorthWest),
        (Rect::from_min_max(pos2(r - CORNER, t), pos2(r, t + EDGE)), ResizeDirection::NorthEast, CursorIcon::ResizeNorthEast),
        (Rect::from_min_max(pos2(r - EDGE, t), pos2(r, t + CORNER)), ResizeDirection::NorthEast, CursorIcon::ResizeNorthEast),
        (Rect::from_min_max(pos2(l, b - EDGE), pos2(l + CORNER, b)), ResizeDirection::SouthWest, CursorIcon::ResizeSouthWest),
        (Rect::from_min_max(pos2(l, b - CORNER), pos2(l + EDGE, b)), ResizeDirection::SouthWest, CursorIcon::ResizeSouthWest),
        (Rect::from_min_max(pos2(r - CORNER, b - EDGE), pos2(r, b)), ResizeDirection::SouthEast, CursorIcon::ResizeSouthEast),
        (Rect::from_min_max(pos2(r - EDGE, b - CORNER), pos2(r, b)), ResizeDirection::SouthEast, CursorIcon::ResizeSouthEast),
    ];
    // Above `window-buttons` and `notch-items` (both `Order::Foreground`, which move to top
    // on click). Only the thin strips below take input. Each senses both clicks and drags
    // (without focus) so buttons and notch drag strips underneath don't also hover or
    // trigger on the outer resize edge.
    egui::Area::new(Id::unique("resize-edges"))
        .order(egui::Order::Tooltip)
        .sense(Sense::hover())
        .fixed_pos(s.min)
        .show(ctx, |ui| {
            ui.set_clip_rect(s);
            for (i, (rect, dir, cursor)) in edges.into_iter().enumerate() {
                let side = matches!(dir, ResizeDirection::East | ResizeDirection::West);
                if sides_only && !side {
                    continue;
                }
                // The sides run the full height when they're all there is.
                let rect = if sides_only { Rect::from_x_y_ranges(rect.x_range(), s.y_range()) } else { rect };
                // Not focusable: twelve unnamed stops for Tab otherwise.
                let resp = ui.interact(rect, Id::unique(("resize-edge", i)), Sense::CLICK | Sense::DRAG).on_hover_cursor(cursor);
                // On the press itself, so the edge doesn't lag behind the pointer.
                if resp.is_pointer_button_down_on() && ui.input(|i| i.pointer.primary_pressed()) {
                    ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
                }
            }
        });
}

/// The main window, from `main`: its whole area is the app's to draw on.
///
/// winit's frameless window keeps a one-pixel system frame along the top (it moves the
/// window's drawable area down a pixel so Windows still draws the drop shadow), and that
/// shows as a darker line above the app. Here that shift is undone, so the drawable area
/// is the whole window, and the shadow comes from a one-pixel frame along the bottom
/// instead. That frame sits under the app's bottom row, which the app always paints
/// opaque, so it never shows. Anything else winit decides (the work area when maximized)
/// stays as it is.
///
/// `gl_vendor` is the OpenGL driver's vendor, when OpenGL draws: see [`drop_caption`].
#[cfg_attr(not(windows), allow(unused_variables))]
pub fn set_hwnd(hwnd: *mut std::ffi::c_void, gl_vendor: Option<&str>) {
    #[cfg(windows)]
    if custom() {
        frame::init(hwnd, drop_caption(gl_vendor, std::env::var("YTM_DROP_CAPTION").ok().as_deref()));
    }
}

/// Whether the frameless window goes without the caption style too (#196). winit keeps
/// it (for snapping and the minimize and maximize animations) though the window shows
/// no caption. Intel's OpenGL drivers for older chips (HD 5500) then put the picture
/// where a captioned window's drawable area would start, a caption's height lower than
/// the window's, while the pointer lands where it should. Full screen, which drops the
/// style, and the system title bar, whose drawable area does start below a caption,
/// show it in place. Dropping the style costs only those animations, so it's for
/// Intel's OpenGL alone. `YTM_DROP_CAPTION=1` drops it anywhere, `=0` keeps it.
#[cfg_attr(not(windows), allow(dead_code))]
fn drop_caption(gl_vendor: Option<&str>, forced: Option<&str>) -> bool {
    match forced {
        Some("1") => true,
        Some("0") => false,
        _ => gl_vendor.is_some_and(|v| v.to_ascii_lowercase().contains("intel")),
    }
}

/// macOS: the main window's view, from `main`.
#[cfg(target_os = "macos")]
pub fn set_view(ns_view: *mut std::ffi::c_void) {
    mac::init(ns_view);
}

#[cfg(target_os = "macos")]
mod mac {
    use std::sync::atomic::{AtomicPtr, Ordering};

    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSView, NSWindow, NSWindowButton, NSWindowStyleMask, NSWindowTitleVisibility};
    use objc2_foundation::{NSPoint, NSString, NSUserDefaults};

    static VIEW: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

    pub fn init(ns_view: *mut std::ffi::c_void) {
        VIEW.store(ns_view, Ordering::Relaxed);
    }

    /// Put the traffic lights' left edge at `left` and their middle `center_y` down from
    /// the window's top; or, not `full` (the mini player), give the window its title strip
    /// back. Whether anything had to change.
    pub fn sync(full: bool, center_y: f64, left: f64) -> bool {
        let ptr = VIEW.load(Ordering::Relaxed);
        if ptr.is_null() || MainThreadMarker::new().is_none() {
            return false;
        }
        // SAFETY: winit's view of the main window, alive as long as the app's loop that
        // calls this, and used on the main thread.
        let view = unsafe { &*(ptr as *const NSView) };
        let Some(window) = view.window() else { return false };
        let mask = window.styleMask();
        // Borderless is the mini player; full screen has macOS's own top, sliding down.
        if !mask.contains(NSWindowStyleMask::Titled) || mask.contains(NSWindowStyleMask::FullScreen) {
            return false;
        }
        if !full {
            if !mask.contains(NSWindowStyleMask::FullSizeContentView) {
                return false;
            }
            // macOS lays the strip out afresh, the buttons back in their usual place.
            set_mask(&window, mask & !NSWindowStyleMask::FullSizeContentView);
            window.setTitlebarAppearsTransparent(false);
            window.setTitleVisibility(NSWindowTitleVisibility::Visible);
            return true;
        }
        if !mask.contains(NSWindowStyleMask::FullSizeContentView) {
            // Back from the mini player; or winit set the mask afresh, without this, when
            // the frame came back after the pill.
            set_mask(&window, mask | NSWindowStyleMask::FullSizeContentView);
        }
        if !window.titlebarAppearsTransparent() {
            window.setTitlebarAppearsTransparent(true);
            window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
        }
        let Some(close) = window.standardWindowButton(NSWindowButton::CloseButton) else { return false };
        // The buttons' bar and what holds it, moved as one: the buttons' highlight while
        // the pointer is over them moves with them. Moving the buttons inside it left
        // that where it was (winit#4466).
        // SAFETY: AppKit's own views, kept alive by the window for as long as this runs.
        let Some(holder) = (unsafe { close.superview() }).and_then(|bar| unsafe { bar.superview() }) else { return false };
        let Some(frame_view) = (unsafe { holder.superview() }) else { return false };
        // In the window's own coordinates: from its bottom left, up.
        let at = close.convertRect_toView(close.bounds(), None);
        let height = window.frame().size.height;
        let dx = left - at.origin.x;
        let down = center_y - (height - (at.origin.y + at.size.height / 2.0));
        if dx.abs() < 0.5 && down.abs() < 0.5 {
            return false;
        }
        let o = holder.frame().origin;
        let dy = if frame_view.isFlipped() { down } else { -down };
        holder.setFrameOrigin(NSPoint::new(o.x + dx, o.y + dy));
        true
    }

    /// Change the window's style, the app's area kept the size it was: the strip coming
    /// or going would otherwise take its height from it, or add it, with the window's
    /// outside the same (the mini player sized to fit its strip would lose a row). The
    /// top edge stays put.
    fn set_mask(window: &NSWindow, mask: NSWindowStyleMask) {
        let content = || window.contentView().map(|v| v.frame().size.height);
        let before = content();
        window.setStyleMask(mask);
        if let (Some(before), Some(after)) = (before, content()) {
            let lost = before - after;
            if lost.abs() > 0.5 {
                let mut f = window.frame();
                f.origin.y -= lost;
                f.size.height += lost;
                window.setFrame_display(f, true);
            }
        }
    }

    pub enum DoubleClick {
        Zoom,
        Minimize,
        Nothing,
    }

    /// What System Settings › Desktop & Dock › "Double-click a window's title bar to" says.
    pub fn double_click() -> DoubleClick {
        let defaults = NSUserDefaults::standardUserDefaults();
        match defaults.stringForKey(&NSString::from_str("AppleActionOnDoubleClick")).map(|s| s.to_string()).as_deref() {
            Some("Minimize") => DoubleClick::Minimize,
            Some("None") => DoubleClick::Nothing,
            Some(_) => DoubleClick::Zoom,
            // Older systems: a switch for minimizing, zooming otherwise.
            None if defaults.boolForKey(&NSString::from_str("AppleMiniaturizeOnDoubleClick")) => DoubleClick::Minimize,
            None => DoubleClick::Zoom,
        }
    }
}

#[cfg(windows)]
mod frame {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows::Win32::Graphics::Dwm::DwmExtendFrameIntoClientArea;
    use windows::Win32::Graphics::Gdi::{
        BLACK_BRUSH, ClientToScreen, FillRect, GetMonitorInfoW, GetStockObject, GetWindowDC, HBRUSH, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect, MonitorFromWindow, ReleaseDC,
        ScreenToClient,
    };
    use windows::Win32::UI::Controls::MARGINS;
    use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, TrackMouseEvent};
    use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_STYLE, GetClientRect, GetWindowLongW, GetWindowRect, HTMAXBUTTON, IsZoomed, MINMAXINFO, NCCALCSIZE_PARAMS, STYLESTRUCT, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        SWP_NOZORDER, SetWindowLongW, SetWindowPos, WM_GETMINMAXINFO, WM_MOUSEMOVE, WM_NCACTIVATE, WM_NCCALCSIZE, WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP,
        WM_CANCELMODE, WM_CAPTURECHANGED, WM_DPICHANGED, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE, WM_NCPAINT, WM_SIZE, WM_STYLECHANGING, WS_CAPTION,
    };

    /// See `super::drop_caption`.
    static NO_CAPTION: AtomicBool = AtomicBool::new(false);

    /// Windows 11, whose snap layouts show over the area a window says is its maximize
    /// button. Windows 10 has none, and its window is left as it was.
    static SNAP_LAYOUTS: AtomicBool = AtomicBool::new(false);

    /// The maximize button as Windows sees it: where it is (in the window's pixels, None
    /// while it isn't drawn), and the pointer over it, which Windows sends as messages for
    /// the frame rather than the app's area, so egui doesn't see it.
    struct SnapButton {
        rect: Option<RECT>,
        /// Drawn last frame, so the pointer's state on it still holds.
        drawn: bool,
        hovered: bool,
        down: bool,
        clicked: bool,
        ctx: Option<egui::Context>,
    }

    static SNAP: Mutex<SnapButton> = Mutex::new(SnapButton { rect: None, drawn: false, hovered: false, down: false, clicked: false, ctx: None });

    pub fn init(hwnd: *mut std::ffi::c_void, no_caption: bool) {
        let hwnd = HWND(hwnd);
        NO_CAPTION.store(no_caption, Ordering::Relaxed);
        SNAP_LAYOUTS.store(crate::backdrop::windows_build().is_some_and(|build| build >= 22000), Ordering::Relaxed);
        // SAFETY: the app's own window, on the thread that made it (still hidden: eframe
        // shows it after the first frame); the subclass lives as long as the window.
        unsafe {
            if !SetWindowSubclass(hwnd, Some(proc), 1, 0).as_bool() {
                log::warn!("title bar: couldn't subclass the window");
                return;
            }
            let _ = DwmExtendFrameIntoClientArea(hwnd, &MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: 0, cyBottomHeight: 1 });
            if no_caption {
                // The subclass keeps it off when winit sets the style again.
                log::info!("title bar: window without the caption style");
                SetWindowLongW(hwnd, GWL_STYLE, GetWindowLongW(hwnd, GWL_STYLE) & !(WS_CAPTION.0 as i32));
            }
            // Have Windows ask for the window's areas again.
            let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }

    /// Until the maximize button is drawn again this frame, the window has none. A button
    /// that wasn't drawn last frame (the mini player, full screen) lets go of the pointer.
    pub fn snap_forget() {
        let mut s = SNAP.lock().unwrap();
        s.rect = None;
        if !std::mem::take(&mut s.drawn) {
            (s.hovered, s.down, s.clicked) = (false, false, false);
        }
    }

    /// See `super::snap`.
    pub fn snap(ui: &egui::Ui, hit: egui::Rect) -> super::Snap {
        if !SNAP_LAYOUTS.load(Ordering::Relaxed) {
            return super::Snap::default();
        }
        let ctx = ui.ctx();
        let ppp = ctx.pixels_per_point();
        // Up, not to the nearest: a pixel row half in the resize strip above stays the strip's.
        let px = |v: f32| (v * ppp).ceil() as i32;
        let mut s = SNAP.lock().unwrap();
        s.ctx.get_or_insert_with(|| ctx.clone());
        // Not under a dialog, where egui keeps the pointer from the button too.
        if !ctx.memory(|m| m.allows_interaction(ui.layer_id())) {
            s.clicked = false;
            return super::Snap::default();
        }
        s.rect = Some(RECT { left: px(hit.left()), top: px(hit.top()), right: px(hit.right()), bottom: px(hit.bottom()) });
        s.drawn = true;
        super::Snap { hovered: s.hovered, down: s.down, clicked: std::mem::take(&mut s.clicked) }
    }

    /// The maximize button's messages, answered here: Windows shows the snap layouts while
    /// the pointer rests where the window answers `HTMAXBUTTON`, and from then on sends the
    /// pointer's moves and presses there as the frame's. None for what goes on as usual.
    fn snap_message(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        let on_button = wparam.0 == HTMAXBUTTON as usize;
        let mut s = SNAP.lock().unwrap();
        let before = (s.hovered, s.down);
        let result = match msg {
            WM_NCHITTEST => {
                let r = s.rect?;
                // Screen coordinates, signed: a monitor left of or above the main one.
                let mut p = POINT { x: lparam.0 as i16 as i32, y: (lparam.0 >> 16) as i16 as i32 };
                // SAFETY: the app's own window; the point is ours.
                let inside = unsafe { ScreenToClient(hwnd, &mut p) }.as_bool() && p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom;
                return inside.then_some(LRESULT(HTMAXBUTTON as isize));
            }
            WM_NCMOUSEMOVE if on_button => {
                if !s.hovered {
                    let mut track = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE | TME_NONCLIENT, hwndTrack: hwnd, dwHoverTime: 0 };
                    // SAFETY: the app's own window; the struct is ours and sized.
                    let _ = unsafe { TrackMouseEvent(&mut track) };
                }
                s.hovered = true;
                // On to Windows, which times the snap layouts by it.
                None
            }
            // Not on to Windows, which would hold the pointer in a loop of its own; the
            // app's button maximizes or restores on the release, as a click does.
            WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if on_button => {
                (s.hovered, s.down) = (true, true);
                Some(LRESULT(0))
            }
            WM_NCLBUTTONUP if on_button => {
                s.clicked |= std::mem::take(&mut s.down);
                Some(LRESULT(0))
            }
            // Resized or on another monitor's scale: the button moved, and is where the next
            // frame draws it.
            WM_SIZE | WM_DPICHANGED => {
                s.rect = None;
                return None;
            }
            // Off the button, or a press Windows called off: it doesn't click.
            WM_NCMOUSEMOVE | WM_NCMOUSELEAVE | WM_MOUSEMOVE | WM_CANCELMODE | WM_CAPTURECHANGED => {
                (s.hovered, s.down) = (false, false);
                None
            }
            _ => return None,
        };
        if ((s.hovered, s.down) != before || s.clicked)
            && let Some(ctx) = &s.ctx
        {
            ctx.request_repaint();
        }
        result
    }

    /// The reserved row is outside egui's clear. Black lets DWM show the material there.
    fn clear_edge(hwnd: HWND) {
        if !crate::backdrop::shows() {
            return;
        }
        // SAFETY: the app's own window; the rectangles are ours, and the DC is released.
        unsafe {
            if !IsZoomed(hwnd).as_bool() {
                return;
            }
            let (mut outer, mut client, mut origin) = (RECT::default(), RECT::default(), POINT::default());
            if GetWindowRect(hwnd, &mut outer).is_err() || GetClientRect(hwnd, &mut client).is_err() || !ClientToScreen(hwnd, &mut origin).as_bool() {
                return;
            }
            let dc = GetWindowDC(Some(hwnd));
            if dc.is_invalid() {
                return;
            }
            let x = origin.x - outer.left;
            let y = origin.y - outer.top + client.bottom;
            let line = RECT { left: x, top: y, right: x + client.right, bottom: y + 1 };
            FillRect(dc, &line, HBRUSH(GetStockObject(BLACK_BRUSH).0));
            ReleaseDC(Some(hwnd), dc);
        }
    }

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _: usize, _: usize) -> LRESULT {
        // SAFETY: called by Windows with the window's own message; for WM_NCCALCSIZE with
        // wparam set, lparam points at an NCCALCSIZE_PARAMS; for WM_STYLECHANGING, at a
        // STYLESTRUCT.
        unsafe {
            if SNAP_LAYOUTS.load(Ordering::Relaxed)
                && let Some(result) = snap_message(hwnd, msg, wparam, lparam)
            {
                return result;
            }
            if msg == WM_STYLECHANGING && wparam.0 as i32 == GWL_STYLE.0 && NO_CAPTION.load(Ordering::Relaxed) {
                (*(lparam.0 as *mut STYLESTRUCT)).styleNew &= !WS_CAPTION.0;
            }
            // Without the caption style, as GLFW's frameless windows (#196): maximized to
            // the work area, and no old-style title bar drawn on activation or a new title.
            if NO_CAPTION.load(Ordering::Relaxed) {
                if msg == WM_GETMINMAXINFO {
                    let result = DefSubclassProc(hwnd, msg, wparam, lparam);
                    to_work_area(hwnd, &mut *(lparam.0 as *mut MINMAXINFO));
                    return result;
                }
                // Activation still goes to winit, which tracks focus by it; lparam -1 asks
                // Windows not to repaint the frame for it, so no old-style title bar shows.
                if msg == WM_NCACTIVATE {
                    DefSubclassProc(hwnd, msg, wparam, LPARAM(-1));
                    return LRESULT(1);
                }
                if msg == WM_NCPAINT {
                    clear_edge(hwnd);
                    return LRESULT(1);
                }
            }
            if msg != WM_NCCALCSIZE || wparam.0 == 0 {
                let result = DefSubclassProc(hwnd, msg, wparam, lparam);
                if msg == WM_NCPAINT {
                    clear_edge(hwnd);
                }
                return result;
            }
            let params = &mut *(lparam.0 as *mut NCCALCSIZE_PARAMS);
            let proposed = params.rgrc[0];
            let result = DefSubclassProc(hwnd, msg, wparam, lparam);
            // winit's shift: the same rectangle a pixel lower. Anything else is its own
            // sizing (maximized), kept.
            let r = params.rgrc[0];
            if (r.left, r.right, r.top, r.bottom) == (proposed.left, proposed.right, proposed.top + 1, proposed.bottom + 1) {
                params.rgrc[0] = proposed;
            }
            // Maximized, a GPU window can fill its whole monitor, and Windows then presents
            // it straight to the screen: no blending, so a window material turns black. A
            // pixel short of that keeps it composited (Chromium does the same).
            if IsZoomed(hwnd).as_bool() {
                if crate::backdrop::active() != crate::backdrop::Backdrop::None {
                    params.rgrc[0].bottom -= 1;
                } else if fills_monitor(&params.rgrc[0]) {
                    // Exactly its monitor's size (maximized with the taskbar auto-hiding),
                    // it's taken for exclusive full screen by some drivers: AMD's OpenGL
                    // keeps some 80 MB more for it from then on. A pixel past the screen's
                    // edge it isn't, and, unlike a pixel short, nothing shows.
                    params.rgrc[0].bottom += 1;
                }
            }
            result
        }
    }

    /// Without the caption style Windows maximizes a window over its whole monitor, the
    /// taskbar included, as it does a full-screen game's. winit still keeps the app's area
    /// to the work area, so the strip past it went unseen but took the taskbar's clicks
    /// (#196). Maximized, it fills the work area instead, as GLFW's frameless windows do.
    fn to_work_area(hwnd: HWND, mm: &mut MINMAXINFO) {
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        // SAFETY: the app's own window; `info` is ours and sized.
        if !unsafe { GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut info) }.as_bool() {
            return;
        }
        let (m, w) = (info.rcMonitor, info.rcWork);
        mm.ptMaxPosition = POINT { x: w.left - m.left, y: w.top - m.top };
        mm.ptMaxSize = POINT { x: w.right - w.left, y: w.bottom - w.top };
    }

    /// Whether `r` covers all of the monitor it's on: the one it's going to, while the
    /// window moves to another.
    fn fills_monitor(r: &RECT) -> bool {
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        // SAFETY: `r` and `info` are ours, and `info` is sized.
        if !unsafe { GetMonitorInfoW(MonitorFromRect(r, MONITOR_DEFAULTTONEAREST), &mut info) }.as_bool() {
            return false;
        }
        let m = info.rcMonitor;
        r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
    }
}

#[cfg(test)]
mod tests {
    use egui::{Rect, pos2, vec2};

    use super::{Choice, NOTCH_BUTTON, NOTCH_H, NOTCH_PAD_EDGE, NOTCH_TOP, window_button_hit};

    #[test]
    fn close_button_hit_reaches_top_right_corner() {
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 720.0));
        let close_r = Rect::from_min_size(pos2(screen.right() - NOTCH_PAD_EDGE - NOTCH_BUTTON.x, NOTCH_TOP), NOTCH_BUTTON);
        let close_hit = window_button_hit(close_r, screen, NOTCH_H, true);
        assert!(close_hit.contains(pos2(screen.right(), screen.top())));
        assert!(close_hit.contains(pos2(close_r.center().x, NOTCH_H)));
        let max_r = close_r.translate(vec2(-(NOTCH_BUTTON.x + 2.0), 0.0));
        let max_hit = window_button_hit(max_r, screen, NOTCH_H, false);
        assert!(max_hit.contains(pos2(max_r.center().x, screen.top())));
        assert!(!max_hit.contains(pos2(screen.right(), screen.top())));
    }

    #[test]
    fn only_intel_opengl_drops_the_caption_style_unless_forced() {
        use super::drop_caption;
        assert!(drop_caption(Some("Intel"), None));
        assert!(!drop_caption(Some("NVIDIA Corporation"), None));
        assert!(!drop_caption(None, None));
        assert!(drop_caption(None, Some("1")));
        assert!(!drop_caption(Some("Intel"), Some("0")));
        assert!(!drop_caption(Some("ATI Technologies Inc."), Some("")));
    }

    #[test]
    fn linux_draws_its_own_title_bar_only_where_the_desktop_draws_none() {
        assert!(Choice::Auto.own(true, "GNOME"));
        assert!(Choice::Auto.own(true, "ubuntu:GNOME"));
        // XWayland windows get the desktop's frame.
        assert!(!Choice::Auto.own(false, "GNOME"));
        assert!(!Choice::Auto.own(true, "KDE"));
        assert!(!Choice::Auto.own(true, ""));
        assert!(Choice::Own.own(false, "KDE"));
        assert!(!Choice::System.own(true, "GNOME"));
    }
}
#[cfg(test)]
mod tap_tests {
    use egui::{Event, Modifiers, PointerButton, Pos2, RawInput, Rect, pos2, vec2};

    /// Two clicks on a title bar strip, `gap` seconds and `dx` points apart, by a finger
    /// (as egui-winit sends a tap: the touch with the pointer's events) or the mouse:
    /// whether the second one maximizes, as `drag_area` decides.
    fn second_is_double(finger: bool, gap: f64, dx: f32) -> bool {
        let doubles = toggles(finger, &[(0.5, 0.0), (0.5 + gap, dx)]);
        assert!(doubles <= 1);
        doubles == 1
    }

    /// Clicks on a title bar strip at (seconds, points right): how many times the window
    /// would maximize or restore.
    fn toggles(finger: bool, clicks: &[(f64, f32)]) -> usize {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx, "", &[]);
        let click = |t: f64, p: Pos2, id: u64| {
            let touch = |phase| Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(id), phase, pos: p, force: None };
            let button = |pressed| Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
            match finger {
                true => vec![(t, vec![touch(egui::TouchPhase::Start), Event::PointerMoved(p), button(true)]), (t + 0.08, vec![touch(egui::TouchPhase::End), button(false), Event::PointerGone])],
                false => vec![(t, vec![Event::PointerMoved(p), button(true)]), (t + 0.08, vec![button(false)])],
            }
        };
        let mut frames = vec![(0.0, vec![])];
        for (n, &(t, dx)) in clicks.iter().enumerate() {
            frames.extend(click(t, pos2(300.0 + dx, 20.0), n as u64 + 1));
        }
        let mut doubles = 0;
        for (t, events) in frames {
            let raw = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))), time: Some(t), events, ..Default::default() };
            let mut out = ctx.run_ui(raw, |ui| {
                let strip = super::drag_strip(ui, Rect::from_min_size(Pos2::ZERO, vec2(800.0, 50.0)), "test");
                doubles += usize::from(super::toggles(ui, &strip));
            });
            out.textures_delta.clear();
        }
        doubles
    }

    /// A quick triple tap maximizes once, whichever pair of taps egui would take as a
    /// double-click.
    #[test]
    fn a_quick_triple_tap_maximizes_once() {
        assert_eq!(toggles(true, &[(0.5, 0.0), (0.65, 0.0), (0.8, 0.0)]), 1);
        assert_eq!(toggles(true, &[(0.5, 0.0), (0.85, 0.0), (1.0, 0.0)]), 1);
    }

    #[test]
    fn a_finger_double_taps_the_title_bar_as_a_finger_lands() {
        // What egui's double-click already takes.
        assert!(second_is_double(true, 0.15, 0.0));
        // A finger's second tap a little beside the first, or a little later.
        assert!(second_is_double(true, 0.15, 8.0));
        assert!(second_is_double(true, 0.35, 0.0));
        // Too late, or somewhere else: two taps.
        assert!(!second_is_double(true, 0.5, 0.0));
        assert!(!second_is_double(true, 0.15, 60.0));
    }

    #[test]
    fn the_mouse_double_clicks_as_before() {
        assert!(second_is_double(false, 0.15, 0.0));
        assert!(!second_is_double(false, 0.15, 8.0));
        assert!(!second_is_double(false, 0.35, 0.0));
    }
}
