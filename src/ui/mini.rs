//! The mini player: the same window, shrunk to a strip with the song, the transport and a
//! seek line, kept above other windows unless unpinned. Ctrl+M or the player bar's
//! button switches; the strip's expand button (or a double-click on it) switches back.
//!
//! Each mode keeps its own place: the full window goes back to exactly where it was
//! (maximized again if it was), and the strip reopens where it was last moved. Places
//! are kept in physical pixels with the monitor's scale, so they survive moving between
//! monitors with different scaling. Launching always opens the full window: closed in
//! the mini player, the app saves the full window's place for next time.
//!
//! On Windows the app draws the strip's window buttons itself (see `titlebar`). Elsewhere
//! the system's frame stays, with its own close button, so the strip leaves its out.
//! Keeping on top is up to the window manager (Wayland has no way to ask).

use std::time::Duration;

use egui::{Color32, Pos2, Rect, Sense, Ui, Vec2, ViewportCommand, WindowLevel, pos2, vec2};
use serde::{Deserialize, Serialize};

use super::widgets::{self, icon_at, named, paint_cover, text_at};
use super::{Action, App, fmt_time, titlebar};
use crate::theme::{self, icon};

// Decisions that are easy to change.

/// The strip when first opened. It's as tall as its contents; the width goes from
/// `MIN_W` to `MAX_W` by its sides.
pub const SIZE: Vec2 = vec2(400.0, 112.0);
const MIN_W: f32 = 330.0;
const MAX_W: f32 = 720.0;
/// Kept above other windows until unpinned.
const PINNED_BY_DEFAULT: bool = true;
/// What the strip's × does.
const CLOSE: CloseButton = CloseButton::LikeFullWindow;
/// From the work area's corner, the first time.
const MARGIN: f32 = 16.0;

#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq)]
enum CloseButton {
    /// What the full window's × does: quit, or hide to the tray with "Close to tray".
    LikeFullWindow,
    /// Back to the full window (the expand button's job too).
    Expand,
}

/// Where a window was: its outer corner in physical pixels, its inner size in points,
/// and the monitor's scale and the app's zoom then (points are that big).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub scale: f32,
    #[serde(default = "unzoomed")]
    pub zoom: f32,
}

fn unzoomed() -> f32 {
    1.0
}

impl Place {
    /// The window's place now, once the platform has said. Wayland never tells apps
    /// where their windows are (nor lets them move them): there only the size counts.
    fn current(ctx: &egui::Context) -> Option<Place> {
        let ppp = ctx.pixels_per_point();
        let size = ctx.content_rect().size();
        let zoom = ctx.zoom_factor();
        ctx.input(|i| {
            let vp = i.viewport();
            let pos = vp.outer_rect.map_or(Pos2::ZERO, |r| r.min * ppp);
            Some(Place { x: pos.x, y: pos.y, w: size.x, h: size.y, scale: vp.native_pixels_per_point?, zoom })
        })
    }

    fn size(&self) -> Vec2 {
        vec2(self.w, self.h)
    }

    /// Its size in points at the app's zoom now (it may have changed since).
    fn size_at(&self, zoom: f32) -> Vec2 {
        self.size() * (self.zoom / zoom)
    }

    /// The outer rectangle in physical pixels, given the frame around the inside (points).
    fn rect_px(&self, frame: Vec2) -> Rect {
        Rect::from_min_size(pos2(self.x, self.y), (self.size() + frame) * self.scale * self.zoom)
    }

    /// eframe's saved-window format, so a launch after closing in the mini player opens
    /// the full window where it was.
    fn eframe_window(&self, maximized: bool) -> String {
        format!(
            "(normal_geometry:(outer_position_pixels:Some((x:{:?},y:{:?})),scale_factor:{:?},inner_size_points:(x:{:?},y:{:?})),fullscreen:false,maximized:{maximized})",
            self.x, self.y, self.scale, self.w, self.h
        )
    }
}

/// The strip's first place: the bottom right of `area` (physical pixels), clear of its
/// edges, for a window with `frame` (points) around its inside. A point is `scale`
/// times `zoom` pixels.
fn corner_place(area: Rect, frame: Vec2, scale: f32, zoom: f32) -> Place {
    let outer = (SIZE + frame) * scale * zoom;
    let p = area.max - outer - Vec2::splat(MARGIN * scale);
    let p = p.max(area.min);
    Place { x: p.x.round(), y: p.y.round(), w: SIZE.x, h: SIZE.y, scale, zoom }
}

/// A window `size` (points) in the middle of `area` (physical pixels), kept inside it.
fn centered(area: Rect, size: Vec2, frame: Vec2, scale: f32, zoom: f32) -> Place {
    let outer = (size + frame) * scale * zoom;
    let p = (area.center() - outer / 2.0).max(area.min);
    Place { x: p.x.round(), y: p.y.round(), w: size.x, h: size.y, scale, zoom }
}

/// What the app can ask about the monitors (the system's; made up in tests).
#[derive(Clone, Copy)]
struct Screens {
    /// Whether some of a rectangle (physical pixels) is on a connected monitor.
    on_a_monitor: fn(Rect) -> bool,
    /// The window's monitor less the taskbar, in physical pixels.
    work_area: fn() -> Option<Rect>,
}

const SYSTEM: Screens = Screens { on_a_monitor: crate::fullscreen::on_a_monitor, work_area: crate::fullscreen::work_area };

/// A remembered strip size, within the limits (they may have changed since).
fn clamp_size(p: Place) -> Place {
    Place { w: p.w.clamp(MIN_W, MAX_W), h: SIZE.y, ..p }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    /// Waiting for a maximized full window to come down, so its normal place is known.
    Unmaximize,
    /// Move and resize now.
    Resize,
    /// Moved onto a monitor with another scale: size it once the window has that scale.
    Rescale(Place),
    /// Waiting for the window to reach its new size (None: to be maximized) before the
    /// new layout shows.
    Settle(Option<Vec2>),
}

struct Switch {
    to_mini: bool,
    step: Step,
    since: f64,
    /// The window's size when it began.
    from: Vec2,
}

/// What to draw this frame.
#[derive(Clone, Copy, PartialEq)]
pub enum Layout {
    Full,
    Mini,
    /// Just the background: mid-switch, the window no longer fits the old layout and
    /// doesn't have the new one's size yet.
    Blank,
}

pub struct Mini {
    /// The strip is showing.
    on: bool,
    pub pinned: bool,
    /// The strip's last place.
    place: Option<Place>,
    /// The full window's, from before it shrank, and whether it was maximized.
    full: Option<(Place, bool)>,
    was_maximized: bool,
    switch: Option<Switch>,
    /// A switch asked for, started once the window is ready.
    wanted: Option<bool>,
    /// When the current layout appeared, for its fade-in.
    shown_at: f64,
    /// The scale the size limits were last sent at: winit keeps them in pixels.
    limits_scale: Option<f32>,
    screens: Screens,
    /// The full window's normal place after coming back maximized, until it's restored down.
    restored_max: Option<Place>,
}

impl Mini {
    pub fn load(get: impl Fn(&str) -> Option<String>) -> Self {
        Mini {
            on: false,
            pinned: get("mini_pinned").map_or(PINNED_BY_DEFAULT, |v| v == "1"),
            place: get("mini_window").and_then(|v| serde_json::from_str(&v).ok()).map(clamp_size),
            full: None,
            was_maximized: false,
            switch: None,
            wanted: None,
            shown_at: f64::NEG_INFINITY,
            limits_scale: None,
            screens: SYSTEM,
            restored_max: None,
        }
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        storage.set_string("mini_pinned", if self.pinned { "1" } else { "0" }.into());
        if let Some(p) = &self.place {
            storage.set_string("mini_window", serde_json::to_string(p).unwrap_or_default());
        }
    }

    /// Ask for the strip (true) or the full window; started by [`Mini::take_request`].
    pub fn request(&mut self, on: bool) {
        self.wanted = Some(on);
    }

    /// The switch to start now, if any (not while one runs).
    pub fn take_request(&mut self) -> Option<bool> {
        if self.switch.is_some() {
            return None;
        }
        self.wanted.take()
    }

    /// Where it's headed once queued switches are done: what Ctrl+M flips.
    pub fn target(&self) -> bool {
        self.wanted.unwrap_or_else(|| self.active())
    }

    /// The strip is showing or on its way.
    pub fn active(&self) -> bool {
        self.switch.as_ref().map_or(self.on, |s| s.to_mini)
    }

    /// The strip is showing, or the window is switching either way: the full window's
    /// pages and now playing aren't up.
    pub fn busy(&self) -> bool {
        self.on || self.switch.is_some()
    }

    /// The strip is showing (not mid-switch).
    pub fn showing(&self) -> bool {
        self.on && self.switch.is_none()
    }

    /// The old layout stays until the window starts changing size.
    pub fn layout(&self, ctx: &egui::Context) -> Layout {
        match &self.switch {
            Some(sw) if (ctx.content_rect().size() - sw.from).length() > 1.0 => Layout::Blank,
            _ if self.on => Layout::Mini,
            _ => Layout::Full,
        }
    }

    /// The full window's place in eframe's format, while eframe's own would be wrong: with
    /// the strip up or on its way back (eframe's is the strip's), and while the full window
    /// is still maximized after it (eframe took the strip's as its normal size).
    pub fn full_window(&self) -> Option<String> {
        if self.active() || self.switch.is_some() {
            return self.full.map(|(p, max)| p.eframe_window(max));
        }
        self.restored_max.map(|p| p.eframe_window(true))
    }

    /// The new layout fades in from `base` (the background's own colour): drawn over
    /// everything, since pages and popups paint on layers of their own.
    pub fn fade_in(&self, ctx: &egui::Context, now: f64, base: Color32) {
        let t = ((now - self.shown_at) / 0.18).clamp(0.0, 1.0) as f32;
        if t >= 1.0 {
            return;
        }
        ctx.request_repaint();
        let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("mini-fade"));
        ctx.layer_painter(layer).rect_filled(ctx.content_rect(), 0.0, base.gamma_multiply(1.0 - theme::ease_out(t)));
    }

    /// Switch to the strip or back. The full window leaves full screen first (the caller's).
    pub fn toggle(&mut self, ctx: &egui::Context, now: f64) {
        if self.switch.is_some() {
            return;
        }
        let to_mini = !self.on;
        let mut step = Step::Resize;
        if to_mini {
            self.was_maximized = ctx.input(|i| i.viewport().maximized == Some(true));
            if self.was_maximized {
                ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
                step = Step::Unmaximize;
            }
        }
        self.switch = Some(Switch { to_mini, step, since: now, from: ctx.content_rect().size() });
        ctx.request_repaint();
    }

    pub fn set_pinned(&mut self, ctx: &egui::Context, on: bool) {
        self.pinned = on;
        if self.active() {
            ctx.send_viewport_cmd(ViewportCommand::WindowLevel(if on { WindowLevel::AlwaysOnTop } else { WindowLevel::Normal }));
        }
    }

    /// Once a frame: drive a switch, note the strip's place, keep the size limits right.
    pub fn tick(&mut self, ctx: &egui::Context, now: f64) {
        let scale = ctx.input(|i| i.viewport().native_pixels_per_point);
        // winit keeps the limits in pixels: again after a move to another scale or a zoom.
        if self.limits_scale.is_some_and(|s| s != ctx.pixels_per_point()) && self.switch.is_none() {
            self.limits(ctx, self.on);
        }
        if self.restored_max.is_some() && self.switch.is_none() && ctx.input(|i| i.viewport().maximized == Some(false)) {
            self.restored_max = None;
        }
        let (minimized, maximized) = ctx.input(|i| (i.viewport().minimized == Some(true), i.viewport().maximized == Some(true)));
        // Win+Up or a drag onto the screen's top edge: the strip has one height.
        if self.showing() && maximized && !minimized {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
        }
        // Minimized, the platform says no place (and the strip's isn't one maximized).
        if self.showing()
            && !minimized
            && !maximized
            && let Some(p) = Place::current(ctx).filter(|p| p.w >= MIN_W - 1.0)
        {
            self.place = Some(clamp_size(p));
        }
        let Some(sw) = &mut self.switch else { return };
        ctx.request_repaint_after(Duration::from_millis(16));
        let waited = now - sw.since;
        let to_mini = sw.to_mini;
        let next = match sw.step {
            Step::Unmaximize => {
                let down = ctx.input(|i| i.viewport().maximized != Some(true));
                // Its normal place, at once: closing before the next step still saves it.
                if down {
                    self.full = Place::current(ctx).map(|p| (p, true));
                }
                (down || waited > 0.5).then_some(Step::Resize)
            }
            Step::Resize => Some(self.resize(ctx, to_mini)),
            Step::Rescale(target) => (scale.is_some_and(|s| (s - target.scale).abs() < 0.01) || waited > 0.6).then(|| {
                self.limits(ctx, to_mini);
                let size = target.size_at(ctx.zoom_factor());
                ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
                Step::Settle(Some(size))
            }),
            Step::Settle(size) => {
                let done = match size {
                    Some(size) => (ctx.content_rect().size() - size).length() < 2.0,
                    None => ctx.input(|i| i.viewport().maximized == Some(true)),
                };
                if done || waited > 0.4 {
                    log::info!("{}: {:?}{}", if to_mini { "mini player" } else { "full window" }, Place::current(ctx), if done { "" } else { " (timed out)" });
                    self.on = to_mini;
                    if !to_mini {
                        self.restored_max = self.full.take().filter(|(_, max)| *max).map(|(p, _)| p);
                    }
                    self.switch = None;
                    self.shown_at = now;
                    return;
                }
                None
            }
        };
        if let (Some(step), Some(sw)) = (next, &mut self.switch) {
            sw.step = step;
            sw.since = now;
        }
    }

    /// Whether a place is (at least partly) on a connected monitor. Test runs live off-screen.
    fn visible(&self, p: Place, frame: Vec2) -> bool {
        // Its top middle, where it can be grabbed: a sliver on screen isn't enough.
        let r = p.rect_px(frame);
        let grip = Rect::from_center_size(pos2(r.center().x, r.top() + 12.0 * p.scale), vec2(r.width().min(120.0 * p.scale), 16.0 * p.scale));
        std::env::var_os("YTM_BACKGROUND").is_some() || (self.screens.on_a_monitor)(grip)
    }

    /// Move and size the window for the new mode; the step to wait in after.
    fn resize(&mut self, ctx: &egui::Context, to_mini: bool) -> Step {
        let now_place = Place::current(ctx);
        let frame = ctx.input(|i| {
            let vp = i.viewport();
            vp.outer_rect.zip(vp.inner_rect).map_or(Vec2::ZERO, |(o, i)| o.size() - i.size())
        });
        if to_mini {
            self.full = now_place.map(|p| (p, self.was_maximized));
            let target = match (self.place, now_place) {
                // Test runs stay off-screen, where they are; YTM_MINI_W tries other widths.
                (_, Some(here)) if std::env::var_os("YTM_BACKGROUND").is_some() => {
                    let w = std::env::var("YTM_MINI_W").ok().and_then(|w| w.parse().ok()).unwrap_or(SIZE.x);
                    clamp_size(Place { w, ..here })
                }
                (Some(p), _) if self.visible(p, frame) => p,
                (_, here) => {
                    let scale = here.map_or(1.0, |h| h.scale);
                    // Not Windows: the bottom right of the full window, where the player bar was.
                    let area = (self.screens.work_area)()
                        .or_else(|| here.map(|h| h.rect_px(frame)))
                        .unwrap_or(Rect::from_min_size(Pos2::ZERO, SIZE * scale));
                    corner_place(area, frame, scale, ctx.zoom_factor())
                }
            };
            ctx.send_viewport_cmd(ViewportCommand::WindowLevel(if self.pinned { WindowLevel::AlwaysOnTop } else { WindowLevel::Normal }));
            self.go(ctx, target, false, true)
        } else {
            if let Some(p) = now_place {
                self.place = Some(clamp_size(p));
            }
            ctx.send_viewport_cmd(ViewportCommand::WindowLevel(WindowLevel::Normal));
            // Kept until the window has settled: a save meanwhile still writes it.
            match self.full {
                Some((p, max)) if self.visible(p, frame) => self.go(ctx, p, max, false),
                // Its monitor is gone: its size, in the middle of the strip's monitor.
                full => {
                    let zoom = ctx.zoom_factor();
                    let size = full.map_or(Vec2::from(crate::DEFAULT_SIZE), |(p, _)| p.size_at(zoom));
                    let max = full.is_some_and(|(_, max)| max);
                    match ((self.screens.work_area)(), now_place) {
                        (Some(area), Some(here)) => {
                            // Its new place is the one to save (and restore down to).
                            let target = centered(area, size, frame, here.scale, zoom);
                            self.full = Some((target, max));
                            self.go(ctx, target, max, false)
                        }
                        _ => {
                            self.limits(ctx, false);
                            ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
                            Step::Settle(Some(size))
                        }
                    }
                }
            }
        }
    }

    /// Move first, then the limits (changing them can resize the window at once, which
    /// could carry it onto another monitor before the move), then the size.
    fn go(&mut self, ctx: &egui::Context, target: Place, maximize: bool, mini: bool) -> Step {
        let ppp = ctx.pixels_per_point();
        ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos2(target.x, target.y) / ppp));
        self.limits(ctx, mini);
        let scale = ctx.input(|i| i.viewport().native_pixels_per_point);
        if scale.is_some_and(|s| (s - target.scale).abs() >= 0.01) && !maximize {
            // Points mean another size there: wait for the move to land.
            return Step::Rescale(target);
        }
        let size = target.size_at(ctx.zoom_factor());
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
        if maximize {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(true));
            return Step::Settle(None);
        }
        Step::Settle(Some(size))
    }

    /// The window's size limits for a mode. Shrinking sets them first, so the full
    /// window's minimum doesn't hold the strip up.
    fn limits(&mut self, ctx: &egui::Context, mini: bool) {
        if mini {
            ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(vec2(MIN_W, SIZE.y)));
            ctx.send_viewport_cmd(ViewportCommand::MaxInnerSize(vec2(MAX_W, SIZE.y)));
        } else {
            ctx.send_viewport_cmd(ViewportCommand::MaxInnerSize(Vec2::INFINITY));
            ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(Vec2::from(crate::MIN_SIZE)));
        }
        self.limits_scale = Some(ctx.pixels_per_point());
    }
}

/// The strip: cover, title and artists, like / previous / play / next / volume, a seek
/// line along the bottom, and pin / full window / close at the top right.
pub fn strip(app: &mut App, ui: &mut Ui, now: f64) {
    let ctx = ui.ctx().clone();
    let r = ctx.content_rect();
    // Its background moves the window; a double-click goes back to the full window.
    if titlebar::drag_strip(ui, r, "mini").double_clicked() {
        app.actions.push(Action::Mini(false));
    }
    ui.painter().rect_filled(r, 0.0, theme::panel(0.18));

    let track = app.current().cloned();
    let pad = 12.0;
    let art = Rect::from_min_size(r.min + Vec2::splat(pad), Vec2::splat(r.height() - 2.0 * pad - 4.0));
    match &track {
        Some(t) => paint_cover(ui, art, &t.thumbs, theme::thumb_radius() + 1.0, 1.0),
        None => {
            ui.painter().rect_filled(art, theme::thumb_radius() + 1.0, theme::glass(0.08));
            icon_at(ui, art.center(), icon::MUSIC, 28.0, theme::text_faint());
        }
    }

    // Title and artists.
    let x0 = art.right() + 12.0;
    let x1 = r.right() - 10.0;
    let text_w = (x1 - x0).max(20.0);
    let (title, artists) = match &track {
        Some(t) => (t.title.clone(), t.artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")),
        None => ("Nothing playing".to_owned(), String::new()),
    };
    let tr = text_at(ui, pos2(x0, art.top() + 1.0), &title, theme::semibold(14.5), theme::text(), text_w);
    if let Some(t) = &track {
        named(ui.interact(tr, ui.scope_id().with("mini-title"), Sense::hover()), &t.title).on_hover_text(&t.title);
    }
    text_at(ui, pos2(x0, art.top() + 22.0), &artists, theme::regular(12.5), theme::text_dim(), text_w);

    // Top right: pin, full window, close. They show while the pointer is on the strip (or
    // one of them has the keyboard), over a backdrop, so the title has the whole width.
    let scope = ui.scope_id();
    // By a key, not the label: the pin's changes, and its focus has to stay.
    let btn_id = move |key: &str| scope.with(("mini-btn", key));
    let focused = ctx.memory(|m| m.focused()).is_some_and(|f| ["close", "full", "pin"].iter().any(|k| btn_id(k) == f));
    let over = ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| r.contains(p));
    let reveal = theme::anim_bool(&ctx, "mini-btns", over || focused, 0.15);
    let count = if titlebar::custom() { 3.0 } else { 2.0 };
    let backdrop = Rect::from_min_max(pos2(r.right() - 6.0 - 30.0 * count - 2.0, r.top() + 2.0), pos2(r.right() - 2.0, r.top() + 38.0));
    // Opaque, so the title under it fades out instead of showing through.
    let pill = theme::lerp_color(super::shell::base_color(app), if theme::light() { Color32::WHITE } else { Color32::BLACK }, 0.55);
    ui.painter().rect_filled(backdrop, 9.0, pill.gamma_multiply(reveal));
    let mut bx = r.right() - 6.0;
    let mut top_btn = |ui: &mut Ui, glyph: char, key: &str, name: &str, close: bool, on: bool| -> bool {
        let b = Rect::from_min_size(pos2(bx - 28.0, r.top() + 6.0), Vec2::splat(28.0));
        bx -= 30.0;
        let resp = ui.interact(b, btn_id(key), Sense::click());
        let resp = named(resp, name).on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand);
        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
        let down = resp.is_pointer_button_down_on();
        if h > 0.0 {
            let fill = if close { Color32::from_rgb(0xC4, 0x2B, 0x1C).gamma_multiply(h) } else { theme::glass(if down { 0.16 } else { 0.10 } * h) };
            ui.painter().rect_filled(b, 6.0, fill.gamma_multiply(reveal));
        }
        let idle = if on { theme::text() } else { theme::text_dim() };
        let fg = theme::lerp_color(idle, if close { Color32::WHITE } else { theme::text() }, h);
        icon_at(ui, b.center(), glyph, if down { 13.0 } else { 14.0 }, fg.gamma_multiply(reveal));
        if resp.has_focus() {
            ui.painter().rect_stroke(b.shrink(1.0), 6.0, egui::Stroke::new(1.5, theme::text()), egui::StrokeKind::Inside);
        }
        // Not while still hidden: a quick click there was meant for the strip.
        resp.clicked() && (reveal > 0.5 || resp.has_focus())
    };
    // The system's frame has its own close button.
    if titlebar::custom() && top_btn(ui, icon::X, "close", "Close", true, false) {
        match CLOSE {
            CloseButton::LikeFullWindow => ctx.send_viewport_cmd(ViewportCommand::Close),
            CloseButton::Expand => app.actions.push(Action::Mini(false)),
        }
    }
    if top_btn(ui, icon::MAXIMIZE_2, "full", "Full window (Ctrl+M)", false, false) {
        app.actions.push(Action::Mini(false));
    }
    let pinned = app.mini.pinned;
    if top_btn(ui, if pinned { icon::PIN } else { icon::PIN_OFF }, "pin", if pinned { "Stop keeping on top" } else { "Keep on top" }, false, pinned) {
        app.mini.set_pinned(&ctx, !pinned);
    }

    // One group, centred under the text: like, previous, play, next, volume.
    let cy = art.bottom() - 17.0;
    let cx = (x0 + x1) / 2.0;
    let enabled = track.is_some();
    let small = |ui: &mut Ui, c: Pos2, glyph: char, name: &str| -> bool {
        let b = Rect::from_center_size(c, Vec2::splat(34.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(b));
        widgets::icon_button(&mut child, glyph, 18.0, name, false).clicked()
    };
    if small(ui, pos2(cx - 44.0, cy), icon::SKIP_BACK, "Previous") && enabled {
        app.prev();
    }
    if small(ui, pos2(cx + 44.0, cy), icon::SKIP_FORWARD, "Next") && enabled {
        app.next();
    }
    let playing = app.playing();
    let pr = Rect::from_center_size(pos2(cx, cy), Vec2::splat(36.0));
    let presp = named(ui.interact(pr, ui.scope_id().with("mini-play"), Sense::click()), if playing { "Pause" } else { "Play" }).on_hover_cursor(egui::CursorIcon::PointingHand);
    let ph = theme::anim_bool(&ctx, presp.id.with("h"), presp.hovered(), 0.15);
    let scale = if presp.is_pointer_button_down_on() { 0.92 } else { 1.0 + 0.05 * ph };
    let c = pr.center();
    ui.painter().circle_filled(c, 18.0 * scale, if theme::light() { theme::text() } else { Color32::WHITE });
    let glyph = theme::on_text();
    let morph = theme::anim_bool(&ctx, "mini-play-morph", playing || app.buffering, 0.18);
    if app.buffering {
        let a = now as f32 * 6.0;
        let pts = (0..20).map(|i| c + vec2((a + i as f32 * 0.22).cos(), (a + i as f32 * 0.22).sin()) * 7.0).collect();
        ui.painter().add(egui::Shape::Path(egui::epaint::PathShape::line(pts, egui::epaint::PathStroke::new(2.0, glyph))));
        ctx.request_repaint_after(Duration::from_millis(33));
    } else if morph > 0.5 {
        widgets::pause_bars(ui, c, 6.5 * (2.0 * morph - 1.0).max(0.4), glyph);
    } else {
        widgets::play_triangle(ui, c + vec2(1.2, 0.0), 7.0 * (1.0 - 2.0 * morph).max(0.4), glyph);
    }
    if presp.clicked() && enabled {
        app.toggle();
    }

    if let Some(t) = &track {
        // What's heard, like Now playing's menu: a song version standing in for the music video.
        let t = &app.playing_track().cloned().unwrap_or_else(|| t.clone());
        let liked = app.liked.contains(&t.id);
        let lpos = pos2(cx - 88.0, cy);
        let lresp = ui.interact(Rect::from_center_size(lpos, Vec2::splat(32.0)), ui.scope_id().with("mini-like"), Sense::click());
        let name = if liked { "Remove like" } else { "Like" };
        let lresp = named(lresp, name).on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand);
        let lh = theme::anim_bool(&ctx, lresp.id.with("h"), lresp.hovered(), 0.15);
        let pop = theme::anim_bool(&ctx, ("liked", &t.id), liked, 0.3);
        ui.painter().circle_filled(lpos, 16.0, theme::glass(0.1 * lh));
        let size = 17.0 * (1.0 + 0.25 * (pop * std::f32::consts::PI).sin());
        let outline = theme::lerp_color(theme::lerp_color(theme::text_dim(), theme::text(), lh), theme::RED, pop);
        widgets::heart(ui, lpos, size, pop, outline, theme::RED);
        if lresp.clicked() {
            app.actions.push(Action::Like(t.clone(), !liked));
        }
    }

    // Volume: click to mute, scroll to change.
    let vol = app.audio.volume();
    let vpos = pos2(cx + 88.0, cy);
    let vresp = ui.interact(Rect::from_center_size(vpos, Vec2::splat(32.0)), ui.scope_id().with("mini-vol"), Sense::click());
    let pct = format!("Volume {}%", (vol * 100.0).round());
    let vresp = named(vresp, "Mute").on_hover_text(pct).on_hover_cursor(egui::CursorIcon::PointingHand);
    let vh = theme::anim_bool(&ctx, vresp.id.with("h"), vresp.hovered(), 0.15);
    ui.painter().circle_filled(vpos, 16.0, theme::glass(0.1 * vh));
    let vglyph = if vol == 0.0 { icon::VOLUME_X } else if vol < 0.5 { icon::VOLUME_1 } else { icon::VOLUME_2 };
    icon_at(ui, vpos, vglyph, 17.0, theme::lerp_color(theme::text_dim(), theme::text(), vh));
    if vresp.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            app.audio.set_volume((vol + scroll / 600.0).clamp(0.0, 1.0));
        }
    }
    if vresp.clicked() {
        let stash = super::shell::unmute_id();
        if vol > 0.0 {
            ui.data_mut(|d| d.insert_temp(stash, vol));
            app.audio.set_volume(0.0);
        } else {
            app.audio.set_volume(ui.data(|d| d.get_temp::<f32>(stash)).unwrap_or(0.6));
        }
    }

    seek_line(app, ui, r, track.as_ref());
}

/// A line along the bottom; thicker on hover, with the time under the pointer.
fn seek_line(app: &mut App, ui: &mut Ui, r: Rect, track: Option<&crate::model::Track>) {
    let status = app.audio.status.clone();
    let total = status.duration().map(|d| d.as_secs_f32()).or(track.and_then(|t| t.duration).map(|d| d as f32)).unwrap_or(0.0);
    let pos = app.seek_drag.unwrap_or(status.position_smooth().as_secs_f32()).min(total.max(0.0));
    let hit = Rect::from_min_max(pos2(r.left(), r.bottom() - 10.0), r.max);
    let resp = ui.interact(hit, ui.scope_id().with("mini-seek"), Sense::click_and_drag());
    let resp = if total > 0.0 { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
    resp.widget_info(|| egui::WidgetInfo::slider(total > 0.0, pos as f64, "Seek"));
    let h = theme::anim_bool(ui.ctx(), "mini-seek-h", total > 0.0 && (resp.hovered() || resp.dragged()), 0.15);
    let thick = 4.0 + 2.0 * h;
    let line = Rect::from_min_max(pos2(r.left(), r.bottom() - thick), r.max);
    ui.painter().rect_filled(line, 0.0, theme::glass(0.2));
    let frac = if total > 0.0 { pos / total } else { 0.0 };
    ui.painter().rect_filled(Rect::from_min_size(line.min, vec2(line.width() * frac, thick)), 0.0, app.accent_color());
    let at = |p: Pos2| ((p.x - r.left()) / r.width()).clamp(0.0, 1.0) * total;
    if h > 0.0
        && let Some(p) = resp.hover_pos().or(resp.interact_pointer_pos())
    {
        let txt = format!("{} / {}", fmt_time(at(p) as u32), fmt_time(total as u32));
        let g = widgets::galley(ui, &txt, theme::semibold(11.5), Color32::WHITE, 120.0);
        let half = g.size().x / 2.0 + 6.0;
        let x = p.x.clamp(r.left() + half + 2.0, r.right() - half - 2.0);
        let bg = Rect::from_center_size(pos2(x, line.top() - 13.0), g.size() + vec2(12.0, 5.0));
        ui.painter().rect_filled(bg, 5.0, theme::shade(0.8).gamma_multiply(h));
        ui.painter().galley(bg.min + vec2(6.0, 2.5), g, Color32::WHITE.gamma_multiply(h));
    }
    if total > 0.0
        && let Some(p) = resp.interact_pointer_pos()
    {
        if resp.dragged() {
            app.seek_drag = Some(at(p));
        }
        if resp.drag_stopped() || resp.clicked() {
            app.seek_drag = None;
            app.actions.push(Action::Seek(at(p)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_place_is_the_work_areas_corner() {
        let area = Rect::from_min_max(pos2(0.0, 0.0), pos2(2560.0, 1400.0));
        let p = corner_place(area, Vec2::ZERO, 1.0, 1.0);
        assert_eq!((p.x, p.y, p.w, p.h), (2560.0 - 400.0 - 16.0, 1400.0 - 112.0 - 16.0, SIZE.x, SIZE.y));
        // Scaled: pixels grow with the scale, points don't.
        let p = corner_place(area, vec2(0.0, 0.0), 1.5, 1.0);
        assert_eq!((p.x, p.y, p.w), (2560.0 - 600.0 - 24.0, 1400.0 - 168.0 - 24.0, SIZE.x));
        // A second monitor left of the main one has negative coordinates.
        let left = Rect::from_min_max(pos2(-1920.0, 100.0), pos2(0.0, 1140.0));
        let p = corner_place(left, vec2(2.0, 2.0), 1.0, 1.0);
        assert_eq!((p.x, p.y), (-402.0 - 16.0, 1140.0 - 114.0 - 16.0));
        // Never past the area's top left, however small it is.
        let tiny = Rect::from_min_max(pos2(10.0, 20.0), pos2(200.0, 50.0));
        let p = corner_place(tiny, Vec2::ZERO, 1.0, 1.0);
        assert_eq!((p.x, p.y), (10.0, 20.0));
        // Zoomed in, a point is bigger: the strip still ends at the margin.
        let p = corner_place(area, Vec2::ZERO, 1.0, 2.0);
        assert_eq!((p.x, p.y, p.zoom), (2560.0 - 800.0 - 16.0, 1400.0 - 224.0 - 16.0, 2.0));
    }

    #[test]
    fn a_lost_full_window_is_centred() {
        let area = Rect::from_min_max(pos2(1920.0, 0.0), pos2(3840.0, 1040.0));
        let p = centered(area, vec2(1200.0, 780.0), Vec2::ZERO, 1.0, 1.0);
        assert_eq!((p.x, p.y, p.w, p.h), (1920.0 + 360.0, 130.0, 1200.0, 780.0));
        // Bigger than the area: its top left stays on it.
        let p = centered(area, vec2(1000.0, 800.0), Vec2::ZERO, 2.0, 1.0);
        assert_eq!((p.x, p.y), (1920.0, 0.0));
    }

    #[test]
    fn remembered_sizes_stay_in_limits() {
        let p = Place { x: 5.0, y: 6.0, w: 2000.0, h: 300.0, scale: 1.25, zoom: 1.0 };
        assert_eq!(clamp_size(p), Place { w: MAX_W, h: SIZE.y, ..p });
        assert_eq!(clamp_size(Place { w: 10.0, ..p }).w, MIN_W);
    }

    #[test]
    fn full_window_is_saved_in_eframes_format() {
        let p = Place { x: 814.0, y: -183.0, w: 1200.0, h: 812.0, scale: 1.0, zoom: 1.0 };
        assert_eq!(
            p.eframe_window(false),
            "(normal_geometry:(outer_position_pixels:Some((x:814.0,y:-183.0)),scale_factor:1.0,inner_size_points:(x:1200.0,y:812.0)),fullscreen:false,maximized:false)"
        );
        let s = p.eframe_window(true);
        assert!(s.ends_with("maximized:true)"));
    }

    #[test]
    fn places_round_trip_through_settings() {
        let p = Place { x: -1500.0, y: 900.0, w: 420.0, h: SIZE.y, scale: 1.5, zoom: 1.25 };
        let back: Place = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
        // Saved before places had a zoom: unzoomed.
        let old: Place = serde_json::from_str(r#"{"x":1.0,"y":2.0,"w":400.0,"h":112.0,"scale":1.0}"#).unwrap();
        assert_eq!(old.zoom, 1.0);
        // A full window kept at 100% comes back the same size in pixels at 125%.
        assert_eq!(Place { w: 1250.0, h: 1000.0, zoom: 1.0, ..p }.size_at(1.25), vec2(1000.0, 800.0));
        let m = Mini::load(|k| (k == "mini_window").then(|| serde_json::to_string(&p).unwrap()));
        assert_eq!(m.place, Some(p));
        assert_eq!(m.pinned, PINNED_BY_DEFAULT);
        let m = Mini::load(|k| (k == "mini_pinned").then(|| "0".to_owned()));
        assert!(!m.pinned && m.place.is_none() && !m.active());
    }
}
