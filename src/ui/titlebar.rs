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

/// The top of the window, the search box's row included: the window buttons are this
/// tall, and its middle is the line they, the search box and the logo share.
pub const HEIGHT: f32 = 56.0;
/// The window buttons' width, as Windows draws them.
const BUTTON_W: f32 = 46.0;
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
            Self::Auto => "Automatic",
            Self::Own => "ytm-rs's",
            Self::System => "System's",
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

fn maximized(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().maximized.unwrap_or(false))
}

/// The window buttons' corner, which what's drawn at the top right stays clear of.
pub fn buttons_rect(ctx: &egui::Context) -> Option<Rect> {
    (custom() && shown(ctx)).then(|| {
        let screen = ctx.content_rect();
        Rect::from_min_size(pos2(screen.right() - 3.0 * BUTTON_W, screen.top()), vec2(3.0 * BUTTON_W, HEIGHT))
    })
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

/// Once a frame, before drawing: macOS keeps the traffic lights on the app's top row.
/// `full`: the full window is up (the mini player keeps the system's title strip),
/// None while switching between them.
///
/// macOS puts them back where its own title strip would have them when it lays the
/// window out again (resized, full screen left, the frame back after the mini player),
/// and that can come after the frame drawn for the change. So for a moment after one,
/// it's looked at again a few times a second, until they stay put.
pub fn sync(ctx: &egui::Context, full: Option<bool>) {
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
    if drag_strip(ui, rect, id).double_clicked() {
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

/// Minimize, maximize or restore, and close, at the window's top right.
pub fn window_buttons(ctx: &egui::Context) {
    let Some(rect) = buttons_rect(ctx) else { return };
    let max = maximized(ctx);
    egui::Area::new(Id::unique("window-buttons")).order(egui::Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
        let kinds = [(Kind::Minimize, "Minimize"), (if max { Kind::Restore } else { Kind::Maximize }, if max { "Restore" } else { "Maximize" }), (Kind::Close, "Close")];
        for (i, (kind, name)) in kinds.into_iter().enumerate() {
            let r = Rect::from_min_size(rect.min + vec2(i as f32 * BUTTON_W, 0.0), vec2(BUTTON_W, HEIGHT));
            let resp = ui.interact(r, Id::unique(("window-button", i)), Sense::click());
            let resp = super::widgets::named(resp, name);
            paint_button(ui, r, kind, &resp);
            if resp.has_focus() {
                ui.painter().rect_stroke(r.shrink(3.0), 3.0, Stroke::new(1.5, theme::text()), egui::StrokeKind::Inside);
            }
            if resp.clicked() {
                ctx.send_viewport_cmd(match kind {
                    Kind::Minimize => ViewportCommand::Minimized(true),
                    Kind::Maximize => ViewportCommand::Maximized(true),
                    Kind::Restore => ViewportCommand::Maximized(false),
                    Kind::Close => ViewportCommand::Close,
                });
            }
        }
    });
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Minimize,
    Maximize,
    Restore,
    Close,
}

fn paint_button(ui: &Ui, r: Rect, kind: Kind, resp: &egui::Response) {
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.1);
    let down = resp.is_pointer_button_down_on();
    let p = ui.painter();
    let fg = if kind == Kind::Close {
        // Windows' own close red, and a darker one while held.
        let red = if down { Color32::from_rgb(0x94, 0x1F, 0x15) } else { Color32::from_rgb(0xC4, 0x2B, 0x1C) };
        if h > 0.0 {
            p.rect_filled(r, 0.0, red.gamma_multiply(h));
        }
        theme::lerp_color(theme::text_dim(), Color32::WHITE, h)
    } else {
        if h > 0.0 {
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
            p.rect_stroke(Rect::from_center_size(c, vec2(10.0, 10.0)), 1.0, s, egui::StrokeKind::Middle);
        }
        Kind::Restore => {
            p.rect_stroke(Rect::from_center_size(c + vec2(-1.0, 1.0), vec2(8.0, 8.0)), 1.0, s, egui::StrokeKind::Middle);
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
    egui::Area::new(Id::unique("resize-edges")).order(egui::Order::Foreground).fixed_pos(s.min).show(ctx, |ui| {
        ui.set_clip_rect(s);
        for (i, (rect, dir, cursor)) in edges.into_iter().enumerate() {
            let side = matches!(dir, ResizeDirection::East | ResizeDirection::West);
            if sides_only && !side {
                continue;
            }
            // The sides run the full height when they're all there is.
            let rect = if sides_only { Rect::from_x_y_ranges(rect.x_range(), s.y_range()) } else { rect };
            // Not focusable: twelve unnamed stops for Tab otherwise.
            let resp = ui.interact(rect, Id::unique(("resize-edge", i)), Sense::DRAG).on_hover_cursor(cursor);
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
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows::Win32::Graphics::Dwm::DwmExtendFrameIntoClientArea;
    use windows::Win32::Graphics::Gdi::{
        BLACK_BRUSH, ClientToScreen, FillRect, GetMonitorInfoW, GetStockObject, GetWindowDC, HBRUSH, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect, MonitorFromWindow, ReleaseDC,
    };
    use windows::Win32::UI::Controls::MARGINS;
    use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_STYLE, GetClientRect, GetWindowLongW, GetWindowRect, IsZoomed, MINMAXINFO, NCCALCSIZE_PARAMS, STYLESTRUCT, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
        SetWindowLongW, SetWindowPos, WM_GETMINMAXINFO, WM_NCACTIVATE, WM_NCCALCSIZE, WM_NCPAINT, WM_STYLECHANGING, WS_CAPTION,
    };

    /// See `super::drop_caption`.
    static NO_CAPTION: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    pub fn init(hwnd: *mut std::ffi::c_void, no_caption: bool) {
        let hwnd = HWND(hwnd);
        NO_CAPTION.store(no_caption, std::sync::atomic::Ordering::Relaxed);
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
            if msg == WM_STYLECHANGING && wparam.0 as i32 == GWL_STYLE.0 && NO_CAPTION.load(std::sync::atomic::Ordering::Relaxed) {
                (*(lparam.0 as *mut STYLESTRUCT)).styleNew &= !WS_CAPTION.0;
            }
            // Without the caption style, as GLFW's frameless windows (#196): maximized to
            // the work area, and no old-style title bar drawn on activation or a new title.
            if NO_CAPTION.load(std::sync::atomic::Ordering::Relaxed) {
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
    use super::Choice;

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
