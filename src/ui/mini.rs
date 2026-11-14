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
/// The open pill's button that grows it into a card with more of the song: only where the
/// window can be clipped to a shape (Windows). Elsewhere a window big enough for the card
/// would take the clicks meant for what's around the pill.
const CARD: bool = cfg!(windows);
const PILL_REST: f32 = 216.0;
const PILL_OPEN: f32 = 290.0 + if CARD { 26.0 } else { 0.0 };
const PILL_HEIGHT: f32 = 56.0;
/// As wide as the open pill, so it only grows up or down.
const CARD_SIZE: Vec2 = vec2(PILL_OPEN, 252.0);
const CARD_RADIUS: f32 = 22.0;
/// The pill's window: always big enough for the open pill, which is only painted as far
/// as it has grown, so hovering never has to resize a window. Just the pill's, so what
/// Windows snaps (and shows it snapping) is the pill.
const PILL_WINDOW: Vec2 = vec2(PILL_OPEN + 2.0, PILL_HEIGHT + 2.0);
/// The window while the card shows, or is on its way.
const CARD_WINDOW: Vec2 = vec2(PILL_WINDOW.x, CARD_SIZE.y + 2.0);
/// How far the window moves up for a card opening upwards, in points.
const CARD_TRAVEL: f32 = CARD_SIZE.y - PILL_HEIGHT;
/// Picture in picture: the mini player for a video, when first opened, and as small and
/// as big as it goes. In between it takes the video's shape (#104).
const VIDEO_SIZE: Vec2 = vec2(480.0, 270.0);
const VIDEO_MIN: Vec2 = vec2(240.0, 135.0);
const VIDEO_MAX: Vec2 = vec2(1280.0, 720.0);

/// How long the window keeps one size before it's put in the video's shape: a resize
/// the app can't follow (Wayland's, done by the desktop) is over by then. Windows keeps
/// the shape as it's dragged instead, as mpv does.
const SNAP_AFTER: f64 = 0.3;

/// The size in the video's shape (`ratio`, width over height) and within `min` and `max`,
/// keeping the width, or with `by_height` the height.
fn in_shape(ratio: f32, size: Vec2, by_height: bool, min: Vec2, max: Vec2) -> Vec2 {
    let lo = min.x.max(min.y * ratio);
    let hi = max.x.min(max.y * ratio).max(lo);
    let w = if by_height { size.y * ratio } else { size.x }.clamp(lo, hi);
    vec2(w, w / ratio)
}

/// Whether a resize from `from` to `to` was mostly of the height, so that's the side to keep.
fn height_led(from: Vec2, to: Vec2) -> bool {
    (to.y - from.y).abs() / from.y.max(1.0) > (to.x - from.x).abs() / from.x.max(1.0)
}

/// The window is being dragged or resized (Windows' own loop for it): not the moment
/// to change its size. Only Windows says.
static MOVING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Picture in picture's shape for Windows, which keeps it while the edges are dragged:
/// the video's width over height, and the size limits, in physical pixels.
#[cfg(windows)]
static SHAPE: std::sync::Mutex<Option<(f32, Vec2, Vec2)>> = std::sync::Mutex::new(None);

/// Keeps picture in picture in the video's shape while it's resized (Windows; elsewhere it
/// takes the shape once the resize is over).
#[cfg_attr(not(windows), allow(unused_variables))]
pub fn set_hwnd(hwnd: *mut std::ffi::c_void) {
    #[cfg(windows)]
    shape::init(hwnd);
}

#[cfg(windows)]
mod shape {
    use std::sync::atomic::Ordering;

    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
    use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, GetWindowRect, WM_ENTERSIZEMOVE, WM_EXITSIZEMOVE, WM_SIZING, WMSZ_BOTTOMLEFT, WMSZ_BOTTOM, WMSZ_LEFT, WMSZ_TOP, WMSZ_TOPLEFT, WMSZ_TOPRIGHT};

    pub fn init(hwnd: *mut std::ffi::c_void) {
        // SAFETY: the app's own window, on the thread that made it; the subclass lives as
        // long as the window.
        if !unsafe { SetWindowSubclass(HWND(hwnd), Some(proc), 3, 0) }.as_bool() {
            log::warn!("picture in picture: couldn't subclass the window");
        }
    }

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _: usize, _: usize) -> LRESULT {
        match msg {
            WM_ENTERSIZEMOVE => super::MOVING.store(true, Ordering::Relaxed),
            WM_EXITSIZEMOVE => super::MOVING.store(false, Ordering::Relaxed),
            _ => {}
        }
        // SAFETY: the window's own message, passed on as it came.
        let result = unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
        let shape = *super::SHAPE.lock().unwrap();
        let Some((ratio, min, max)) = shape.filter(|_| msg == WM_SIZING) else { return result };
        let (mut outer, mut inner) = (RECT::default(), RECT::default());
        // SAFETY: the app's own window; the rectangles are ours.
        if unsafe { GetWindowRect(hwnd, &mut outer).is_err() || GetClientRect(hwnd, &mut inner).is_err() } {
            return result;
        }
        // The frame around the inside, which the dragged rectangle includes.
        let fx = (outer.right - outer.left) - (inner.right - inner.left);
        let fy = (outer.bottom - outer.top) - (inner.bottom - inner.top);
        // SAFETY: for WM_SIZING, lparam points at the window's proposed rectangle.
        let r = unsafe { &mut *(lparam.0 as *mut RECT) };
        let edge = wparam.0 as u32;
        let size = egui::vec2((r.right - r.left - fx) as f32, (r.bottom - r.top - fy) as f32);
        // Like mpv: the top or bottom edge sets the height, any other the width.
        let fit = super::in_shape(ratio, size, edge == WMSZ_TOP || edge == WMSZ_BOTTOM, min, max);
        let (w, h) = (fit.x.round() as i32 + fx, fit.y.round() as i32 + fy);
        // The edges across from the one dragged stay put.
        if matches!(edge, WMSZ_LEFT | WMSZ_TOPLEFT | WMSZ_BOTTOMLEFT) {
            r.left = r.right - w;
        } else {
            r.right = r.left + w;
        }
        if matches!(edge, WMSZ_TOP | WMSZ_TOPLEFT | WMSZ_TOPRIGHT) {
            r.top = r.bottom - h;
        } else {
            r.bottom = r.top + h;
        }
        LRESULT(1)
    }
}

/// The mini player is the video (picture in picture): a video was on show when it opened.
static VIDEO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The mini player up, or last up, is the video.
pub fn video_active() -> bool {
    VIDEO.load(std::sync::atomic::Ordering::Relaxed)
}

/// The pill is this mini player: not when it's the video.
fn pill_now() -> bool {
    pill_active() && !video_active()
}

static PILL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// The experimental pill was asked for, instead of the strip. Saved beside the settings:
/// it's needed before the window opens.
pub fn pill_saved(data_dir: &std::path::Path) -> bool {
    std::fs::read_to_string(data_dir.join("mini_style")).is_ok_and(|s| s.trim() == "pill")
}

pub fn save_pill(data_dir: &std::path::Path, on: bool) -> std::io::Result<()> {
    std::fs::write(data_dir.join("mini_style"), if on { "pill" } else { "strip" })
}

/// Settle this run's style, before the window opens. Returns whether the window has to be
/// see-through, which only OpenGL's can be.
pub fn init_pill(data_dir: &std::path::Path, renderer: crate::renderer::Renderer) -> bool {
    let on = renderer == crate::renderer::Renderer::OpenGl && pill_saved(data_dir);
    let _ = PILL.set(on);
    on
}

/// This run's mini player is the pill.
pub fn pill_active() -> bool {
    *PILL.get_or_init(|| false)
}

/// The pill `width` wide in the window `r`, set against its right edge so it grows to the
/// left and stays in the corner it's placed in. `low`: at the window's bottom, which a card
/// opening upwards has lifted.
fn pill_rect(r: Rect, width: f32, low: bool) -> Rect {
    let top = if low { r.bottom() - 1.0 - PILL_HEIGHT } else { r.top() + 1.0 };
    Rect::from_min_size(pos2(r.right() - 1.0 - width, top), vec2(width, PILL_HEIGHT))
}

/// The card in the window `r` (the card's window).
fn card_rect(r: Rect) -> Rect {
    Rect::from_min_size(pos2(r.right() - 1.0 - CARD_SIZE.x, r.top() + 1.0), CARD_SIZE)
}

/// The window's shape around `rect` (points from the window's corner) with corners of
/// `radius`: left, top, right, bottom and corner diameter in pixels. A point past the
/// edge keeps it smooth.
fn region(rect: Rect, radius: f32, ppp: f32) -> (i32, i32, i32, i32, i32) {
    let r = rect.expand(1.0);
    let d = ((radius + 1.0) * 2.0 * ppp).ceil() as i32;
    ((r.left() * ppp).floor() as i32, (r.top() * ppp).floor() as i32, (r.right() * ppp).ceil() as i32, (r.bottom() * ppp).ceil() as i32, d)
}

/// How far (whole pixels, so up and back down end where they began) the window moves up
/// for a card opening upwards.
fn travel(ppp: f32) -> f32 {
    (CARD_TRAVEL * ppp).round()
}

/// Where the mini player's place is saved: each style its own, as their sizes differ.
fn place_key() -> &'static str {
    if video_active() {
        VIDEO_KEY
    } else if pill_active() {
        "mini_pill_window"
    } else {
        "mini_window"
    }
}

const VIDEO_KEY: &str = "mini_video_window";

/// The mini player's size when first opened.
fn mini_size() -> Vec2 {
    if video_active() {
        VIDEO_SIZE
    } else if pill_active() {
        PILL_WINDOW
    } else {
        SIZE
    }
}

fn width_limits() -> (f32, f32) {
    if video_active() {
        (VIDEO_MIN.x, VIDEO_MAX.x)
    } else if pill_active() {
        (PILL_WINDOW.x, PILL_WINDOW.x)
    } else {
        (MIN_W, MAX_W)
    }
}

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
    let size = mini_size();
    let outer = (size + frame) * scale * zoom;
    let p = area.max - outer - Vec2::splat(MARGIN * scale);
    let p = p.max(area.min);
    Place { x: p.x.round(), y: p.y.round(), w: size.x, h: size.y, scale, zoom }
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
    let (min, max) = width_limits();
    // The video's is any height in its limits; the others have one.
    let h = if video_active() { p.h.clamp(VIDEO_MIN.y, VIDEO_MAX.y) } else { mini_size().y };
    Place { w: p.w.clamp(min, max), h, ..p }
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
    /// The strip's (or the pill's) last place.
    place: Option<Place>,
    /// Picture in picture's.
    video_place: Option<Place>,
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
    /// The shape the window was last clipped to, for the pill.
    shape: Option<(i32, i32, i32, i32, i32)>,
    /// The pill has grown into its card.
    card: bool,
    /// The card opens upwards from the pill; else downwards. Chosen as it opens.
    up: bool,
    /// The window is the card's size (asked for, at least), from opening until it's shut.
    tall: bool,
    /// The window has moved up for a card opening upwards: the pill is at its bottom.
    lifted: bool,
    /// The window has reached the card's size since it was asked for (it may lose it).
    grown: bool,
    /// When the card was last asked for, so the second click of a double-click on More
    /// can't land on what has grown under the pointer.
    card_at: f64,
    /// The shown video's width over height, while picture in picture has one.
    pub video_shape: Option<f32>,
    /// Picture in picture's last size in the video's shape, to tell which side a resize changed.
    shaped: Option<Vec2>,
    /// The window's size, and since when it's had it.
    sized: Option<(Vec2, f64)>,
    /// A size the video's shape was asked for from, so a desktop that says no isn't asked again.
    asked_from: Option<Vec2>,
}

impl Mini {
    pub fn load(get: impl Fn(&str) -> Option<String>) -> Self {
        Mini {
            on: false,
            pinned: get("mini_pinned").map_or(PINNED_BY_DEFAULT, |v| v == "1"),
            place: get(place_key()).and_then(|v| serde_json::from_str::<Place>(&v).ok()).map(|p| {
                // A pill saved narrower (before it had the card's button) keeps its right
                // edge, so one in the corner doesn't reopen partly off the screen.
                let x = if pill_active() { p.x + (p.w - PILL_WINDOW.x) * p.scale * p.zoom } else { p.x };
                clamp_size(Place { x, ..p })
            }),
            video_place: get(VIDEO_KEY).and_then(|v| serde_json::from_str::<Place>(&v).ok()),
            full: None,
            was_maximized: false,
            switch: None,
            wanted: None,
            shown_at: f64::NEG_INFINITY,
            limits_scale: None,
            screens: SYSTEM,
            restored_max: None,
            shape: None,
            card: false,
            up: true,
            tall: false,
            lifted: false,
            grown: false,
            card_at: f64::NEG_INFINITY,
            video_shape: None,
            shaped: None,
            sized: None,
            asked_from: None,
        }
    }

    /// Picture in picture takes the video's shape: Windows keeps it while an edge is
    /// dragged, and any other resize gets it once it's over (keeping the side changed),
    /// as does a video of another shape coming on.
    fn keep_shape(&mut self, ctx: &egui::Context, now: f64, still: bool) {
        let ratio = self.video_shape.filter(|_| still && self.showing() && video_active() && self.switch.is_none());
        #[cfg(windows)]
        {
            let px = ctx.pixels_per_point();
            *SHAPE.lock().unwrap() = ratio.map(|r| (r, VIDEO_MIN * px, VIDEO_MAX * px));
        }
        let Some(ratio) = ratio else {
            if !self.showing() {
                self.video_shape = None;
            }
            (self.shaped, self.sized, self.asked_from) = (None, None, None);
            return;
        };
        let size = ctx.content_rect().size();
        if (size.x / ratio - size.y).abs() < 1.5 {
            (self.shaped, self.asked_from) = (Some(size), None);
            return;
        }
        let since = match self.sized {
            Some((s, since)) if (s - size).length() < 0.5 => since,
            _ => {
                self.sized = Some((size, now));
                now
            }
        };
        if self.asked_from.is_some_and(|a| (a - size).length() < 0.5) {
            return;
        }
        // Still being dragged: not yet. (The app may not see the button let go after a
        // resize the system did, so the pointer can't say.)
        if MOVING.load(std::sync::atomic::Ordering::Relaxed) {
            self.sized = Some((size, now));
            ctx.request_repaint_after(Duration::from_millis(50));
            return;
        }
        let wait = SNAP_AFTER - (now - since);
        if wait > 0.0 {
            ctx.request_repaint_after(Duration::from_secs_f64(wait.max(0.05)));
            return;
        }
        let by_height = self.shaped.is_some_and(|from| height_led(from, size));
        let target = in_shape(ratio, size, by_height, VIDEO_MIN, VIDEO_MAX);
        log::debug!("picture in picture: {size:?} to the video's shape, {target:?}");
        self.asked_from = Some(size);
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(target));
    }

    /// Clip the window to the pill (or its card) at `rect`.
    fn shape_to(&mut self, ctx: &egui::Context, rect: Rect, radius: f32) {
        // On the way back, the pill still draws until the window starts changing size.
        if self.switch.is_some() {
            return;
        }
        let shape = Some(region(rect.translate(-ctx.content_rect().min.to_vec2()), radius, ctx.pixels_per_point()));
        if self.shape != shape {
            self.shape = shape;
            crate::fullscreen::set_window_shape(shape);
        }
    }

    /// Grow the pill into its card, or back. Opening, the window grows down to the card's
    /// size (the pill stays at its top); the card opens upwards unless only below has room,
    /// for which [`Mini::lift`] moves the window up once it's grown.
    fn set_card(&mut self, ctx: &egui::Context, on: bool) {
        self.card = on;
        if on {
            self.card_at = ctx.input(|i| i.time);
        }
        if !on || self.tall {
            return;
        }
        self.up = self.opens_up(ctx);
        self.tall = true;
        self.limits(ctx, true);
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(CARD_WINDOW));
    }

    /// Whether the card would open upwards from the pill's window where it is now: unless
    /// only below has room.
    fn opens_up(&self, ctx: &egui::Context) -> bool {
        let outer = ctx.input(|i| i.viewport().outer_rect);
        let (Some(outer), Some(area)) = (outer, (self.screens.work_area)()) else { return self.up };
        let ppp = ctx.pixels_per_point();
        let pill_top = (outer.min.y + 1.0) * ppp;
        let fits_up = pill_top - CARD_TRAVEL * ppp >= area.top();
        let fits_down = pill_top + CARD_SIZE.y * ppp <= area.bottom();
        fits_up || !fits_down
    }

    /// Move the window up for the card (the pill now at its bottom), or back down, keeping
    /// the pill where it is on screen.
    fn lift(&mut self, ctx: &egui::Context, up: bool) {
        let Some(outer) = ctx.input(|i| i.viewport().outer_rect) else { return };
        self.lifted = up;
        let ppp = ctx.pixels_per_point();
        let travel = travel(ppp) * if up { -1.0 } else { 1.0 };
        crate::fullscreen::move_window((outer.min.x * ppp).round() as i32, (outer.min.y * ppp + travel).round() as i32);
    }

    /// Back to the pill's size, once the card has shrunk away.
    fn shrink(&mut self, ctx: &egui::Context) {
        if self.lifted {
            self.lift(ctx, false);
        }
        (self.tall, self.grown) = (false, false);
        self.limits(ctx, true);
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(PILL_WINDOW));
    }

    /// Where the pill is, for its window at `p`: lower down while the card has lifted it.
    fn pill_place(&self, p: Place, ppp: f32) -> Place {
        let y = if self.lifted { p.y + travel(ppp) } else { p.y };
        clamp_size(Place { y, ..p })
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        storage.set_string("mini_pinned", if self.pinned { "1" } else { "0" }.into());
        let strip_key = if pill_active() { "mini_pill_window" } else { "mini_window" };
        for (key, place) in [(strip_key, &self.place), (VIDEO_KEY, &self.video_place)] {
            if let Some(p) = place {
                storage.set_string(key, serde_json::to_string(p).unwrap_or_default());
            }
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

    /// The pill is up, on the way back included (it's drawn until the window starts
    /// changing size): the window around it is see-through.
    pub fn pill_showing(&self) -> bool {
        pill_now() && self.on
    }

    /// This mini player's last place.
    fn place(&self) -> Option<Place> {
        if video_active() { self.video_place } else { self.place }
    }

    fn set_place(&mut self, p: Place) {
        if video_active() {
            self.video_place = Some(clamp_size(p));
        } else {
            self.place = Some(p);
        }
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
    /// `video`: going to the mini player with a video on show, which it then plays.
    pub fn toggle(&mut self, ctx: &egui::Context, now: f64, video: bool) {
        if self.switch.is_some() {
            return;
        }
        let to_mini = !self.on;
        if to_mini {
            VIDEO.store(video, std::sync::atomic::Ordering::Relaxed);
        }
        // Named like a browser's picture in picture while it's one. Wayland lets no app put
        // itself above others: a KDE window rule for this title (and the app, ytm-rs) can.
        if video_active() {
            ctx.send_viewport_cmd(ViewportCommand::Title(if to_mini { "Picture in picture" } else { "YT Music" }.into()));
        }
        let mut step = Step::Resize;
        self.card = false;
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
            && let Some(p) = Place::current(ctx).filter(|p| p.w >= width_limits().0 - 1.0)
        {
            let p = self.pill_place(p, ctx.pixels_per_point());
            self.set_place(p);
        }
        self.keep_shape(ctx, now, !minimized && !maximized);
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
        // The pill has no frame of its own, on any system.
        let frame = if to_mini && pill_now() { Vec2::ZERO } else { frame };
        if pill_now() {
            ctx.send_viewport_cmd(ViewportCommand::Decorations(!to_mini && !titlebar::custom()));
            self.shape = None;
            crate::fullscreen::set_window_shape(None);
        }
        if to_mini {
            self.full = now_place.map(|p| (p, self.was_maximized));
            let target = match (self.place(), now_place) {
                // Test runs stay off-screen, where they are; YTM_MINI_W tries other widths.
                (_, Some(here)) if std::env::var_os("YTM_BACKGROUND").is_some() => {
                    let w = std::env::var("YTM_MINI_W").ok().and_then(|w| w.parse().ok()).unwrap_or(mini_size().x);
                    clamp_size(Place { w, h: mini_size().y, ..here })
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
                let p = self.pill_place(p, ctx.pixels_per_point());
                self.set_place(p);
            }
            (self.tall, self.lifted, self.grown) = (false, false, false);
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
        if mini && video_active() {
            ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(VIDEO_MIN));
            ctx.send_viewport_cmd(ViewportCommand::MaxInnerSize(VIDEO_MAX));
        } else if mini {
            let ((min, max), h) = (width_limits(), if self.tall { CARD_WINDOW.y } else { mini_size().y });
            ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(vec2(min, h)));
            ctx.send_viewport_cmd(ViewportCommand::MaxInnerSize(vec2(max, h)));
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
            ui.painter().rect_filled(art, theme::thumb_radius() + 1.0, theme::glass_panel(0.08));
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
    let explicit = track.as_ref().is_some_and(|t| t.explicit);
    let tr = super::widgets::title_at(ui, pos2(x0, art.top() + 1.0), &title, explicit, theme::semibold(14.5), theme::text(), text_w);
    if let Some(t) = &track {
        named(ui.interact(tr, ui.scope_id().with("mini-title"), Sense::hover()), &super::widgets::song_name(&t.title, t.explicit)).on_hover_text(&t.title);
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
        close_window(app, &ctx);
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
    play_glyph(app, ui, c, 1.0, theme::on_text(), now);
    if presp.clicked() && enabled {
        app.toggle();
    }

    if let Some(t) = &track {
        // What's heard, like Now playing's menu: a song version standing in for the music video.
        let t = &app.playing_track().cloned().unwrap_or_else(|| t.clone());
        let liked = app.heard_liked();
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
            app.actions.push(Action::LikeHeard(!liked));
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
        toggle_mute(app, ui);
    }

    seek_line(app, ui, r, track.as_ref());
}

/// What's on the play button, `k` times the strip's size: play, pause or a spinner.
fn play_glyph(app: &App, ui: &Ui, c: Pos2, k: f32, glyph: Color32, now: f64) {
    let morph = theme::anim_bool(ui.ctx(), "mini-play-morph", app.playing() || app.buffering, 0.18);
    if app.buffering {
        let a = now as f32 * 6.0;
        let pts = (0..20).map(|i| c + vec2((a + i as f32 * 0.22).cos(), (a + i as f32 * 0.22).sin()) * 7.0 * k).collect();
        ui.painter().add(egui::Shape::Path(egui::epaint::PathShape::line(pts, egui::epaint::PathStroke::new(2.0, glyph))));
        ui.ctx().request_repaint_after(Duration::from_millis(33));
    } else if morph > 0.5 {
        widgets::pause_bars(ui, c, 6.5 * k * (2.0 * morph - 1.0).max(0.4), glyph);
    } else {
        widgets::play_triangle(ui, c + vec2(1.2 * k, 0.0), 7.0 * k * (1.0 - 2.0 * morph).max(0.4), glyph);
    }
}

/// The pill: cover in a progress ring, title and artists; pointing at it slides out
/// previous, play, next, the card's button and close. Drag to move, double-click for the
/// full window. The card grows out of it with more of the song and its controls.
pub fn pill(app: &mut App, ui: &mut Ui, now: f64) {
    let ctx = ui.ctx().clone();
    let r = ctx.content_rect();
    if app.mini.card && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.mini.set_card(&ctx, false);
    }
    let card_on = app.mini.card;
    // A seek dragged in the card ends with it.
    if !card_on {
        app.seek_drag = None;
    }
    // The card waits for its window: grown, and moved up when it opens upwards.
    let tall = r.height() >= CARD_WINDOW.y - 1.0;
    // The card's size until the window has it, also after something else (Windows leaving
    // a snap) sizes it back down: only closing the card shrinks it.
    if app.mini.tall && app.mini.switch.is_none() {
        if tall {
            app.mini.grown = true;
        } else {
            app.mini.limits(&ctx, true);
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(CARD_WINDOW));
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
    if card_on && tall && app.mini.up && !app.mini.lifted {
        app.mini.lift(&ctx, true);
    }
    let ready = app.mini.grown && (app.mini.lifted || !app.mini.up);
    // Back from the full window: no card half-shrunk from when it was left.
    if app.mini.shape.is_none() {
        ctx.animate_bool_with_time(egui::Id::unique("pill-card"), card_on && ready, 0.0);
    }
    let grow = theme::anim_bool(&ctx, "pill-card", card_on && ready, 0.3).clamp(0.0, 1.0);
    // Shut: the window back to the pill's size.
    if !card_on && app.mini.tall && grow <= 0.0 {
        app.mini.shrink(&ctx);
    }
    let low = app.mini.lifted;
    let width_id = egui::Id::unique("pill-width");
    let was = ctx.data(|d| d.get_temp::<f32>(width_id)).unwrap_or(PILL_REST);
    let over = ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| pill_rect(r, was, low).contains(p));
    // The keyboard opens it too, or Tab would land on buttons nobody can see.
    let focused = ctx.memory(|m| m.focused()).is_some_and(|f| ["prev", "play", "next", "card", "close"].iter().any(|k| pill_btn(k) == f));
    let active = over || focused || card_on;
    // A toast takes the text's place, with the pill opened for the room, until it's pointed at.
    let toast = app.toast.as_ref().filter(|_| !active).map(|(msg, _)| msg.clone());
    if app.toast.is_some() {
        ctx.request_repaint_after(Duration::from_millis(500));
    }
    let open = theme::anim_bool(&ctx, "pill-open", active || toast.is_some(), 0.18);
    let reveal = theme::anim_bool(&ctx, "pill-buttons", active, 0.18);
    let width = egui::lerp(PILL_REST..=PILL_OPEN, open);
    ctx.data_mut(|d| d.insert_temp(width_id, width));
    let (small, card) = (pill_rect(r, width, low), card_rect(r));
    let rect = Rect::from_min_max(small.min.lerp(card.min, grow), small.max.lerp(card.max, grow));
    let radius = egui::lerp(PILL_HEIGHT / 2.0..=CARD_RADIUS, grow);
    app.mini.shape_to(&ctx, rect, radius);

    // Only once it has settled: a quick second click on More or Less mustn't land on what's
    // fading in under the pointer, nor count as a double-click on the background. Opening
    // upwards, the volume slider grows under where More was: it waits out a slow
    // double-click (Windows allows half a second) too.
    let wait = app.mini.card_at + 0.5 - ctx.input(|i| i.time);
    if app.mini.card && wait > 0.0 {
        ctx.request_repaint_after(Duration::from_secs_f64(wait));
    }
    let settled = if grow < 0.5 { grow <= 0.0 } else { app.mini.card && grow >= 0.99 && wait <= 0.0 };
    let mut back = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    if !settled {
        back.disable();
    }
    let drag = titlebar::drag_strip(&back, rect, "mini");
    let name = app.current().map_or_else(|| "Nothing playing".to_owned(), |t| widgets::song_name(&t.title, t.explicit).into_owned());
    if named(drag, &name).double_clicked() {
        app.actions.push(Action::Mini(false));
    }
    ui.painter().rect_filled(rect, radius, super::shell::base_color(app));
    ui.painter().rect_stroke(rect, radius, egui::Stroke::new(1.0, theme::glass(0.18)), egui::StrokeKind::Inside);

    // One fades out before the other fades in, inside the shape as it grows.
    let mut inside = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    inside.set_clip_rect(rect.intersect(ui.clip_rect()));
    // Disabled without dimming: the fade is enough.
    if !settled {
        inside.visuals_mut().disabled_alpha = 1.0;
        inside.disable();
    }
    if grow < 0.5 {
        inside.multiply_opacity(1.0 - grow * 2.0);
        pill_row(app, &mut inside, small, reveal, toast, now);
    } else {
        inside.multiply_opacity(grow * 2.0 - 1.0);
        card_body(app, &mut inside, card, now);
    }
}

/// A pill button's id, the same wherever it's asked for (the pill's focus check, its row).
fn pill_btn(key: &str) -> egui::Id {
    egui::Id::unique(("pill-btn", key))
}

/// What's in the pill: the cover in a progress ring, title and artists or a toast, and the
/// buttons that slide out.
fn pill_row(app: &mut App, ui: &mut Ui, rect: Rect, reveal: f32, toast: Option<String>, now: f64) {
    let ctx = ui.ctx().clone();
    // The cover, with how far the song has got around it.
    let track = app.current().cloned();
    let c = pos2(rect.left() + 27.0, rect.center().y);
    let status = app.audio.status.clone();
    let total = song_length(app, track.as_ref());
    let frac = if total > 0.0 { (status.position_smooth().as_secs_f32() / total).clamp(0.0, 1.0) } else { 0.0 };
    ui.painter().circle_stroke(c, 18.5, egui::Stroke::new(2.0, theme::glass(0.2)));
    if frac > 0.0 {
        let steps = ((frac * 64.0).ceil() as usize).max(2);
        let pts = (0..=steps).map(|i| {
            let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * frac * i as f32 / steps as f32;
            c + vec2(a.cos(), a.sin()) * 18.5
        });
        ui.painter().add(egui::Shape::Path(egui::epaint::PathShape::line(pts.collect(), egui::epaint::PathStroke::new(2.0, app.accent_color()))));
    }
    let art = Rect::from_center_size(c, Vec2::splat(32.0));
    match &track {
        Some(t) => paint_cover(ui, art, &t.thumbs, 16.0, 1.0),
        None => {
            ui.painter().rect_filled(art, 16.0, theme::glass_panel(0.08));
            icon_at(ui, c, icon::MUSIC, 15.0, theme::text_faint());
        }
    }

    // Title and artists, making room for the buttons as they slide out.
    let shift = if CARD { 26.0 } else { 0.0 };
    let x0 = c.x + 30.0;
    let text_w = (rect.right() - 14.0 - (126.0 + shift) * reveal - x0).max(10.0);
    let cy = rect.center().y;
    let (title, artists) = match &track {
        Some(t) => (t.title.clone(), t.artist_line()),
        None => ("Nothing playing".to_owned(), String::new()),
    };
    let explicit = track.as_ref().is_some_and(|t| t.explicit);
    if let Some(msg) = toast {
        let g = ui.painter().layout(msg, theme::semibold(13.0), theme::text(), text_w);
        ui.painter().galley(pos2(x0, cy - g.size().y / 2.0), g, theme::text());
    } else {
        widgets::title_at(ui, pos2(x0, cy - 17.0), &title, explicit, theme::semibold(13.0), theme::text(), text_w);
        text_at(ui, pos2(x0, cy + 2.0), &artists, theme::regular(11.5), theme::text_dim(), text_w);
    }

    // Clickable once most of the way out: a quick pass over the pill shouldn't press
    // anything. Until then Tab can still get to them, which opens the pill.
    let live = reveal > 0.5 && !app.mini.card;
    let button = |ui: &mut Ui, center: Pos2, key: &str, name: &str| -> egui::Response {
        let sense = if live { Sense::click() } else { Sense::focusable_noninteractive() };
        let resp = named(ui.interact(Rect::from_center_size(center, Vec2::splat(28.0)), pill_btn(key), sense), name);
        if resp.has_focus() {
            ui.painter().circle_stroke(center, 14.0, egui::Stroke::new(1.5, theme::text()));
        }
        if live { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp }
    };
    let enabled = track.is_some();
    let right = rect.right();
    // Which way it'll open from here, while the window is the pill's.
    let up = if app.mini.tall { app.mini.up } else { app.mini.opens_up(&ctx) };
    let mut buttons = vec![(113.0 + shift, icon::SKIP_BACK, "prev", "Previous"), (47.0 + shift, icon::SKIP_FORWARD, "next", "Next"), (21.0, icon::X, "close", "Close")];
    if CARD {
        buttons.push((47.0, if up { icon::CHEVRON_UP } else { icon::CHEVRON_DOWN }, "card", "More"));
    }
    for (dx, glyph, key, name) in buttons {
        let center = pos2(right - dx, cy);
        let resp = button(ui, center, key, name);
        let hot = live && resp.hovered();
        if hot {
            ui.painter().circle_filled(center, 13.0, theme::glass(0.12));
        }
        icon_at(ui, center, glyph, if key == "close" { 13.0 } else { 16.0 }, if hot { theme::text() } else { theme::text_dim() }.gamma_multiply(reveal));
        if resp.clicked() {
            match key {
                "prev" if enabled => app.prev(),
                "next" if enabled => app.next(),
                "card" => app.mini.set_card(&ctx, true),
                "close" => close_window(app, &ctx),
                _ => {}
            }
        }
    }
    let center = pos2(right - 80.0 - shift, cy);
    let resp = button(ui, center, "play", if app.playing() { "Pause" } else { "Play" });
    ui.painter().circle_filled(center, 15.0, if theme::light() { theme::text() } else { Color32::WHITE }.gamma_multiply(reveal));
    play_glyph(app, ui, center, 0.82, theme::on_text().gamma_multiply(reveal), now);
    if resp.clicked() && enabled {
        app.toggle();
    }
}

/// The pill's card: where it's playing from, the cover with title, artists and album and a
/// like, a seek bar with the times, shuffle / previous / play / next / repeat, and volume.
fn card_body(app: &mut App, ui: &mut Ui, cr: Rect, now: f64) {
    let ctx = ui.ctx().clone();
    let (l, rt, top) = (cr.left() + 18.0, cr.right() - 18.0, cr.top());
    let track = app.current().cloned();
    let enabled = track.is_some();
    let button = |ui: &mut Ui, c: Pos2, glyph: char, size: f32, name: &str, active: bool| -> bool {
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_center_size(c, Vec2::splat(size + 16.0))));
        widgets::icon_button(&mut child, glyph, size, name, active).clicked()
    };

    // Where it's playing from (its name opens it in the full window); the full window and
    // back to the pill beside it.
    let head = top + 30.0;
    if button(ui, pos2(rt - 8.0, head), if app.mini.up { icon::CHEVRON_DOWN } else { icon::CHEVRON_UP }, 16.0, "Less", false) {
        app.mini.set_card(&ctx, false);
    }
    if button(ui, pos2(rt - 42.0, head), icon::MAXIMIZE_2, 14.0, "Full window (Ctrl+M)", false) {
        app.actions.push(Action::Mini(false));
    }
    let head_w = rt - 66.0 - l;
    // A toast ("Shuffled up next", "Sign in to like songs") takes the header's place while
    // it lasts, kept through its fade-out.
    let toast_id = egui::Id::unique("card-toast");
    if let Some((msg, _)) = &app.toast {
        ctx.data_mut(|d| d.insert_temp(toast_id, msg.clone()));
    }
    // Still opening: whatever the toast is now, without fading out an old one.
    if !ui.is_enabled() {
        ctx.animate_bool_with_time(egui::Id::unique(toast_id), app.toast.is_some(), 0.0);
    }
    let tt = theme::anim_bool(&ctx, toast_id, app.toast.is_some(), 0.2).clamp(0.0, 1.0);
    if let Some(msg) = ctx.data(|d| d.get_temp::<String>(toast_id)).filter(|_| tt > 0.0) {
        let g = ui.painter().layout(msg, theme::semibold(13.0), theme::text().gamma_multiply(tt), head_w);
        ui.painter().galley(pos2(l, head - g.size().y / 2.0 + 4.0 * (1.0 - tt)), g, theme::text());
    }
    // Faded under the toast, and not clickable once it has mostly taken over.
    let live = tt < 0.5;
    {
        let mut from_ui = ui.new_child(egui::UiBuilder::new().max_rect(cr));
        from_ui.multiply_opacity(1.0 - tt);
        let ui = &mut from_ui;
        match app.playing_from() {
            Some((kind, name, target)) => {
                text_at(ui, pos2(l, head - 16.0), &format!("Playing from {kind}"), theme::regular(11.0), theme::text_faint(), head_w);
                let nr = text_at(ui, pos2(l, head - 1.0), &name, theme::semibold(13.0), theme::text(), head_w);
                let sense = if live { Sense::click() } else { Sense::hover() };
                let resp = named(ui.interact(nr, ui.scope_id().with("card-from"), sense), &name);
                let resp = if live { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
                if resp.hovered() && live {
                    ui.painter().hline(nr.x_range(), nr.bottom(), egui::Stroke::new(1.0, theme::text()));
                }
                if resp.clicked() {
                    // Liked Music is Library › Songs, where it was played from.
                    let liked = matches!(&target, crate::model::Target::Playlist(id) if id == super::lists::LIKED_MUSIC);
                    app.actions.push(Action::Mini(false));
                    app.actions.push(if liked { Action::Go(super::Page::Library(super::LibTab::Songs)) } else { Action::Open(target) });
                }
            }
            None => {
                text_at(ui, pos2(l, head - 8.0), "Now playing", theme::semibold(13.0), theme::text_dim(), head_w);
            }
        }
    }

    // The cover, title, artists and album, and a like.
    let art = Rect::from_min_size(pos2(l, top + 60.0), Vec2::splat(68.0));
    match &track {
        Some(t) => paint_cover(ui, art, &t.thumbs, theme::thumb_radius() + 4.0, 1.0),
        None => {
            ui.painter().rect_filled(art, theme::thumb_radius() + 4.0, theme::glass_panel(0.08));
            icon_at(ui, art.center(), icon::MUSIC, 26.0, theme::text_faint());
        }
    }
    let x0 = art.right() + 14.0;
    let text_w = (rt - 34.0 - x0).max(20.0);
    match &track {
        Some(t) => {
            let tr = widgets::title_at(ui, pos2(x0, art.top() + 6.0), &t.title, t.explicit, theme::semibold(15.0), theme::text(), text_w);
            named(ui.interact(tr, ui.scope_id().with("card-title"), Sense::hover()), &widgets::song_name(&t.title, t.explicit)).on_hover_text(&t.title);
            text_at(ui, pos2(x0, art.top() + 27.0), &t.artist_line(), theme::regular(12.5), theme::text_dim(), text_w);
            if let Some(album) = &t.album {
                text_at(ui, pos2(x0, art.top() + 45.0), &album.name, theme::regular(12.0), theme::text_faint(), text_w);
            }
        }
        None => {
            text_at(ui, pos2(x0, art.center().y - 9.0), "Nothing playing", theme::semibold(15.0), theme::text_dim(), text_w);
        }
    }
    if let Some(t) = &track {
        // What's heard, like Now playing's menu: a song version standing in for the music video.
        let t = &app.playing_track().cloned().unwrap_or_else(|| t.clone());
        let liked = app.heard_liked();
        let lpos = pos2(rt - 8.0, art.center().y);
        let lresp = ui.interact(Rect::from_center_size(lpos, Vec2::splat(32.0)), ui.scope_id().with("card-like"), Sense::click());
        let name = if liked { "Remove like" } else { "Like" };
        let lresp = named(lresp, name).on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand);
        let lh = theme::anim_bool(&ctx, lresp.id.with("h"), lresp.hovered(), 0.15);
        let pop = theme::anim_bool(&ctx, ("liked", &t.id), liked, 0.3);
        ui.painter().circle_filled(lpos, 16.0, theme::glass(0.1 * lh));
        let size = 17.0 * (1.0 + 0.25 * (pop * std::f32::consts::PI).sin());
        let outline = theme::lerp_color(theme::lerp_color(theme::text_dim(), theme::text(), lh), theme::RED, pop);
        widgets::heart(ui, lpos, size, pop, outline, theme::RED);
        if lresp.clicked() {
            app.actions.push(Action::LikeHeard(!liked));
        }
    }

    card_seek(app, ui, Rect::from_min_max(pos2(l, top + 146.0), pos2(rt, top + 146.0)), track.as_ref());

    // Shuffle, previous, play, next, repeat.
    let cy = top + 192.0;
    let cx = cr.center().x;
    if button(ui, pos2(cx - 104.0, cy), icon::SHUFFLE, 16.0, "Shuffle up next", false) {
        app.shuffle_upcoming();
    }
    if button(ui, pos2(cx - 54.0, cy), icon::SKIP_BACK, 20.0, "Previous", false) && enabled {
        app.prev();
    }
    if button(ui, pos2(cx + 54.0, cy), icon::SKIP_FORWARD, 20.0, "Next", false) && enabled {
        app.next();
    }
    use super::Repeat;
    let (rep_icon, rep_on) = match app.queue.repeat {
        Repeat::Off => (icon::REPEAT, false),
        Repeat::All => (icon::REPEAT, true),
        Repeat::One => (icon::REPEAT_1, true),
    };
    let rpos = pos2(cx + 104.0, cy);
    if button(ui, rpos, rep_icon, 16.0, "Repeat", rep_on) {
        app.queue.repeat = match app.queue.repeat {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        };
    }
    if rep_on {
        ui.painter().circle_filled(rpos + vec2(0.0, 15.0), 2.0, app.accent_color());
    }
    let playing = app.playing();
    let pr = Rect::from_center_size(pos2(cx, cy), Vec2::splat(44.0));
    let presp = named(ui.interact(pr, ui.scope_id().with("card-play"), Sense::click()), if playing { "Pause" } else { "Play" }).on_hover_cursor(egui::CursorIcon::PointingHand);
    let ph = theme::anim_bool(&ctx, presp.id.with("h"), presp.hovered(), 0.15);
    let scale = if presp.is_pointer_button_down_on() { 0.92 } else { 1.0 + 0.05 * ph };
    ui.painter().circle_filled(pr.center(), 22.0 * scale, if theme::light() { theme::text() } else { Color32::WHITE });
    play_glyph(app, ui, pr.center(), 1.15, theme::on_text(), now);
    widgets::focus_ring(ui, &presp, pr, 22.0);
    if presp.clicked() && enabled {
        app.toggle();
    }

    // Volume: the speaker mutes, the slider sets it.
    let vy = top + 232.0;
    let vol = app.audio.volume();
    let vglyph = if vol == 0.0 { icon::VOLUME_X } else if vol < 0.5 { icon::VOLUME_1 } else { icon::VOLUME_2 };
    if button(ui, pos2(l + 6.0, vy), vglyph, 16.0, if vol == 0.0 { "Unmute" } else { "Mute" }, false) {
        toggle_mute(app, ui);
    }
    super::shell::volume_slider(app, ui, Rect::from_min_max(pos2(l + 30.0, vy - 10.0), pos2(rt, vy + 10.0)), "card-vol");
}

/// Mute, keeping the volume to go back to, or go back to it.
fn toggle_mute(app: &mut App, ui: &Ui) {
    let vol = app.audio.volume();
    let stash = super::shell::unmute_id();
    if vol > 0.0 {
        ui.data_mut(|d| d.insert_temp(stash, vol));
        app.audio.set_volume(0.0);
    } else {
        app.audio.set_volume(ui.data(|d| d.get_temp::<f32>(stash)).unwrap_or(0.6));
    }
}

/// The card's seek bar along `line`, with the time so far and the song's length under it.
fn card_seek(app: &mut App, ui: &mut Ui, line: Rect, track: Option<&crate::model::Track>) {
    let total = song_length(app, track);
    let pos = app.seek_drag.unwrap_or(app.audio.status.position_smooth().as_secs_f32()).min(total.max(0.0));
    let resp = ui.interact(line.expand2(vec2(4.0, 9.0)), ui.scope_id().with("card-seek"), Sense::click_and_drag());
    let resp = if total > 0.0 { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
    resp.widget_info(|| egui::WidgetInfo::slider(total > 0.0, pos as f64, "Seek"));
    let h = theme::anim_bool(ui.ctx(), "card-seek-h", total > 0.0 && (resp.hovered() || resp.dragged()), 0.15);
    let thick = 4.0 + 2.0 * h;
    let bar = Rect::from_center_size(line.center(), vec2(line.width(), thick));
    ui.painter().rect_filled(bar, thick / 2.0, theme::glass(0.18));
    let frac = if total > 0.0 { pos / total } else { 0.0 };
    let fill = Rect::from_min_size(bar.min, vec2(bar.width() * frac, thick));
    ui.painter().rect_filled(fill, thick / 2.0, app.accent_color());
    if h > 0.0 {
        ui.painter().circle_filled(pos2(fill.right(), bar.center().y), 6.0 * h, theme::text());
    }
    let font = theme::regular(11.0);
    text_at(ui, pos2(bar.left(), bar.bottom() + 7.0), &fmt_time(pos as u32), font.clone(), theme::text_faint(), 60.0);
    let g = widgets::galley(ui, &fmt_time(total as u32), font, theme::text_faint(), 60.0);
    ui.painter().galley(pos2(bar.right() - g.size().x, bar.bottom() + 7.0), g, theme::text_faint());
    let at = |p: Pos2| ((p.x - bar.left()) / bar.width()).clamp(0.0, 1.0) * total;
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

/// Picture in picture: the video filling the window (its cover for a song that plays
/// after it). Pointed at, previous / play / next over it, pin, the full window and close
/// at the top right, and a seek line. Drag to move, double-click for the full window.
pub fn video_pip(app: &mut App, ui: &mut Ui, now: f64) {
    let ctx = ui.ctx().clone();
    let r = ctx.content_rect();
    if titlebar::drag_strip(ui, r, "pip").double_clicked() {
        app.actions.push(Action::Mini(false));
    }
    ui.painter().rect_filled(r, 0.0, Color32::BLACK);
    let track = app.current().cloned();
    let video = app.shown_video().cloned();
    let frame = video.as_ref().and_then(|t| super::nowplaying::video_frame(app, &ctx, t));
    // The window takes its shape (see `keep_shape`); a song without a video leaves it be.
    app.mini.video_shape = frame.as_ref().map(|tex| tex.size_vec2()).filter(|s| s.x > 0.0 && s.y > 0.0).map(|s| s.x / s.y);
    match (&frame, &video, &track) {
        (Some(tex), _, _) => {
            let size = tex.size_vec2();
            let fit = size * (r.width() / size.x).min(r.height() / size.y);
            egui::Image::from_texture((tex.id(), fit)).paint_at(ui, Rect::from_center_size(r.center(), fit));
        }
        (None, Some(v), _) => {
            let fit = vec2(16.0, 9.0) * (r.width() / 16.0).min(r.height() / 9.0);
            super::nowplaying::video_placeholder(ui, Rect::from_center_size(r.center(), fit), v, 0.0);
        }
        (None, None, Some(t)) => paint_cover(ui, Rect::from_center_size(r.center(), Vec2::splat(r.height() - 24.0)), &t.thumbs, theme::thumb_radius() + 2.0, 1.0),
        (None, None, None) => icon_at(ui, r.center(), icon::MUSIC, 32.0, theme::text_faint()),
    }

    // The controls, while pointed at (or one has the keyboard).
    let keys = ["prev", "play", "next", "pin", "full", "close"];
    let btn_id = |key: &str| egui::Id::unique(("pip-btn", key));
    let focused = ctx.memory(|m| m.focused()).is_some_and(|f| keys.iter().any(|k| btn_id(k) == f));
    let over = ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| r.contains(p));
    let reveal = theme::anim_bool(&ctx, "pip-controls", over || focused, 0.15);
    if reveal <= 0.0 {
        return;
    }
    ui.painter().rect_filled(r, 0.0, Color32::from_black_alpha((110.0 * reveal) as u8));
    let enabled = track.is_some();
    let btn = |ui: &mut Ui, c: Pos2, side: f32, key: &str, name: &str| -> egui::Response {
        let b = Rect::from_center_size(c, Vec2::splat(side));
        let resp = named(ui.interact(b, btn_id(key), Sense::click()), name).on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand);
        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
        if h > 0.0 {
            ui.painter().circle_filled(c, side / 2.0, Color32::WHITE.gamma_multiply(0.16 * h * reveal));
        }
        if resp.has_focus() {
            ui.painter().circle_stroke(c, side / 2.0 - 1.0, egui::Stroke::new(1.5, Color32::WHITE));
        }
        resp
    };
    // Not while still fading in: a quick click there was meant for the window.
    let live = |resp: &egui::Response| resp.clicked() && (reveal > 0.5 || resp.has_focus());
    let white = Color32::WHITE.gamma_multiply(reveal);

    // Previous, play, next, in the middle.
    let c = r.center();
    let gap = (r.width() * 0.16).clamp(48.0, 84.0);
    let prev = btn(ui, c - vec2(gap, 0.0), 44.0, "prev", "Previous");
    icon_at(ui, c - vec2(gap, 0.0), icon::SKIP_BACK, 22.0, white);
    if live(&prev) && enabled {
        app.prev();
    }
    let next = btn(ui, c + vec2(gap, 0.0), 44.0, "next", "Next");
    icon_at(ui, c + vec2(gap, 0.0), icon::SKIP_FORWARD, 22.0, white);
    if live(&next) && enabled {
        app.next();
    }
    let play = btn(ui, c, 56.0, "play", if app.playing() { "Pause" } else { "Play" });
    ui.painter().circle_filled(c, 26.0, white);
    play_glyph(app, ui, c, 1.3, Color32::BLACK.gamma_multiply(reveal), now);
    if live(&play) && enabled {
        app.toggle();
    }

    // Top right: close (with no system frame to have one), the full window, pin.
    let mut x = r.right() - 22.0;
    let mut corner = |ui: &mut Ui, glyph: char, key: &str, name: &str| -> bool {
        let resp = btn(ui, pos2(x, r.top() + 22.0), 32.0, key, name);
        icon_at(ui, pos2(x, r.top() + 22.0), glyph, 16.0, white);
        x -= 36.0;
        live(&resp)
    };
    if titlebar::custom() && corner(ui, icon::X, "close", "Close") {
        close_window(app, &ctx);
    }
    if corner(ui, icon::MAXIMIZE_2, "full", "Full window (Ctrl+M)") {
        app.actions.push(Action::Mini(false));
    }
    let pinned = app.mini.pinned;
    if corner(ui, if pinned { icon::PIN } else { icon::PIN_OFF }, "pin", if pinned { "Stop keeping on top" } else { "Keep on top" }) {
        app.mini.set_pinned(&ctx, !pinned);
    }

    let mut faded = ui.new_child(egui::UiBuilder::new().max_rect(r));
    faded.multiply_opacity(reveal);
    seek_line(app, &mut faded, r, track.as_ref());
}

/// What the mini player's × does.
fn close_window(app: &mut App, ctx: &egui::Context) {
    match CLOSE {
        CloseButton::LikeFullWindow => ctx.send_viewport_cmd(ViewportCommand::Close),
        CloseButton::Expand => app.actions.push(Action::Mini(false)),
    }
}

/// The song's length in seconds: its stream's, else what it's listed with, else 0.
fn song_length(app: &App, track: Option<&crate::model::Track>) -> f32 {
    app.audio.status.duration().map(|d| d.as_secs_f32()).or(track.and_then(|t| t.duration).map(|d| d as f32)).unwrap_or(0.0)
}

/// A line along the bottom; thicker on hover, with the time under the pointer.
fn seek_line(app: &mut App, ui: &mut Ui, r: Rect, track: Option<&crate::model::Track>) {
    let status = app.audio.status.clone();
    let total = song_length(app, track);
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
    fn picture_in_picture_takes_the_videos_shape_within_its_limits() {
        let wide = 16.0 / 9.0;
        // A 4:3 video in the first 16:9 window: the width stays.
        assert_eq!(in_shape(4.0 / 3.0, vec2(480.0, 270.0), false, VIDEO_MIN, VIDEO_MAX), vec2(480.0, 360.0));
        // Made taller: the height stays.
        assert_eq!(in_shape(wide, vec2(480.0, 360.0), true, VIDEO_MIN, VIDEO_MAX), vec2(640.0, 360.0));
        // Never past the limits, in either direction, and still the video's shape.
        assert_eq!(in_shape(wide, vec2(2000.0, 100.0), false, VIDEO_MIN, VIDEO_MAX), vec2(1280.0, 720.0));
        assert_eq!(in_shape(wide, vec2(100.0, 100.0), false, VIDEO_MIN, VIDEO_MAX), vec2(240.0, 135.0));
        // A tall video can't be wider than the tallest window allows.
        let tall = in_shape(9.0 / 16.0, vec2(480.0, 270.0), false, VIDEO_MIN, VIDEO_MAX);
        assert!(tall.y <= VIDEO_MAX.y + 0.01 && (tall.x / tall.y - 9.0 / 16.0).abs() < 0.001);
    }

    #[test]
    fn the_side_a_resize_changed_more_is_the_one_kept() {
        assert!(height_led(vec2(480.0, 270.0), vec2(490.0, 400.0)));
        assert!(!height_led(vec2(480.0, 270.0), vec2(700.0, 280.0)));
        // Both by the same share: the width.
        assert!(!height_led(vec2(400.0, 200.0), vec2(800.0, 400.0)));
    }

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
    fn the_pill_grows_from_the_windows_right_edge() {
        let (win, ppp) = (Rect::from_min_size(Pos2::ZERO, PILL_WINDOW), 1.5);
        let shape = |w: f32| region(pill_rect(win, w, false), PILL_HEIGHT / 2.0, ppp);
        let (rest, open) = (shape(PILL_REST), shape(PILL_OPEN));
        assert_eq!(rest.2, open.2);
        assert!(open.0 < rest.0);
        // The open pill fills its window.
        assert_eq!(open, (0, 0, (PILL_WINDOW.x * ppp).ceil() as i32, (PILL_WINDOW.y * ppp).ceil() as i32, 87));
        // The card fills its window; the pill sits at either end of it.
        let tall = Rect::from_min_size(Pos2::ZERO, CARD_WINDOW);
        let c = region(card_rect(tall), CARD_RADIUS, ppp);
        assert_eq!(c, (0, 0, (CARD_WINDOW.x * ppp).ceil() as i32, (CARD_WINDOW.y * ppp).ceil() as i32, c.4));
        let (top, low) = (pill_rect(tall, PILL_OPEN, false), pill_rect(tall, PILL_OPEN, true));
        assert_eq!(low.top() - top.top(), CARD_TRAVEL);
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
