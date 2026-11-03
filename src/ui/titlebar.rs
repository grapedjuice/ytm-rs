//! The app's own title bar on Windows: no system frame, the window buttons drawn at the
//! top right, the empty top of the window to move it by (double-click maximizes), and
//! its edges to resize it by. winit keeps the window styles snapping needs, so dragging
//! to a screen edge and Win+arrows still work. Elsewhere the system's frame stays.

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

/// Whether the app draws the title bar itself: on Windows, unless `YTM_NATIVE_TITLEBAR`
/// asks for the system's.
pub fn custom() -> bool {
    cfg!(windows) && std::env::var_os("YTM_NATIVE_TITLEBAR").is_none()
}

/// Full screen has no title bar. The app's own full screen (`crate::fullscreen`) goes
/// around winit, so the app says when it's on, once a frame before drawing.
pub fn set_fullscreen(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(Id::unique("titlebar-fullscreen"), on));
}

/// Drawn now: not in full screen.
fn shown(ctx: &egui::Context) -> bool {
    custom() && !ctx.data(|d| d.get_temp::<bool>(Id::unique("titlebar-fullscreen")).unwrap_or(false)) && !ctx.input(|i| i.viewport().fullscreen.unwrap_or(false))
}

fn maximized(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().maximized.unwrap_or(false))
}

/// The window buttons' corner, which what's drawn at the top right stays clear of.
pub fn buttons_rect(ctx: &egui::Context) -> Option<Rect> {
    shown(ctx).then(|| {
        let screen = ctx.content_rect();
        Rect::from_min_size(pos2(screen.right() - 3.0 * BUTTON_W, screen.top()), vec2(3.0 * BUTTON_W, HEIGHT))
    })
}

/// The top margin above a row `row` tall at the top of the window: `normal`, or with the
/// app's title bar what centres it on the window buttons' line.
pub fn top_margin(normal: i8, row: f32) -> i8 {
    if custom() { ((HEIGHT - row) / 2.0).round() as i8 } else { normal }
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
    let resp = ui.interact(rect, Id::unique(("titlebar-drag", id)), Sense::CLICK);
    let pressed_here = resp.id.with("pressed");
    let (pressed, down, dragging) = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.is_decidedly_dragging()));
    if pressed {
        let here = resp.is_pointer_button_down_on();
        ui.ctx().data_mut(|d| d.insert_temp(pressed_here, here));
    } else if !down {
        ui.ctx().data_mut(|d| d.remove::<bool>(pressed_here));
    }
    if resp.double_clicked() {
        let max = maximized(ui.ctx());
        ui.ctx().send_viewport_cmd(ViewportCommand::Maximized(!max));
    } else if down && dragging && ui.ctx().data_mut(|d| d.remove_temp::<bool>(pressed_here)).unwrap_or(false) {
        // Once: the system moves the window until the button is let go.
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
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
                ui.painter().rect_stroke(r.shrink(3.0), 3.0, Stroke::new(1.5, theme::TEXT), egui::StrokeKind::Inside);
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
        theme::lerp_color(theme::text_dim(), theme::TEXT, h)
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
    if !shown(ctx) || maximized(ctx) {
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
            // Not focusable: twelve unnamed stops for Tab otherwise.
            let resp = ui.interact(rect, Id::unique(("resize-edge", i)), Sense::DRAG).on_hover_cursor(cursor);
            // On the press itself, so the edge doesn't lag behind the pointer.
            if resp.is_pointer_button_down_on() && ui.input(|i| i.pointer.primary_pressed()) {
                ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
            }
        }
    });
}
