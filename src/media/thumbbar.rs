//! Previous / play-pause / next buttons on the Windows taskbar thumbnail, like
//! other music players. Clicks arrive as WM_COMMAND on the app window, which a
//! subclass turns into the same media events the system media keys send.

use std::sync::Arc;

use crossbeam_channel::Sender;
use souvlaki::MediaControlEvent;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, DeleteObject};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::Shell::{
    DefSubclassProc, ITaskbarList3, SetWindowSubclass, THB_FLAGS, THB_ICON, THB_TOOLTIP, THBF_ENABLED, THBN_CLICKED, THUMBBUTTON, TaskbarList,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, CreateIconIndirect, HICON, ICONINFO, IsWindowVisible, MSGFLT_ALLOW, KillTimer, RegisterWindowMessageW, SM_CXSMICON, SetTimer, WM_COMMAND, WM_SHOWWINDOW, WM_TIMER,
};
use windows::core::w;

const PREV: u32 = 1;
const TOGGLE: u32 = 2;
const NEXT: u32 = 3;
/// Timer that adds the buttons just after the window first shows.
const SHOWN_TIMER: usize = 0x7974;

pub struct ThumbBar {
    hwnd: HWND,
    taskbar: ITaskbarList3,
    play: HICON,
    pause: HICON,
    prev: HICON,
    next: HICON,
    playing: bool,
}

/// What the subclass needs to forward a click.
struct Hook {
    tx: Sender<MediaControlEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
    created: u32,
    bar: *const std::cell::RefCell<Option<ThumbBar>>,
}

impl ThumbBar {
    /// Installs the buttons on `hwnd`. Returned boxed so the window subclass can
    /// re-add them if Explorer restarts and the taskbar button is recreated.
    pub fn install(
        hwnd: *mut std::ffi::c_void,
        tx: Sender<MediaControlEvent>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Option<Box<std::cell::RefCell<Option<ThumbBar>>>> {
        let hwnd = HWND(hwnd);
        unsafe {
            let taskbar: ITaskbarList3 = CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| log::warn!("taskbar buttons unavailable: {e}"))
                .ok()?;
            taskbar.HrInit().ok()?;
            let px = GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForWindow(hwnd)).max(16);
            let bar = ThumbBar {
                hwnd,
                taskbar,
                play: icon(px, Glyph::Play)?,
                pause: icon(px, Glyph::Pause)?,
                prev: icon(px, Glyph::Prev)?,
                next: icon(px, Glyph::Next)?,
                playing: false,
            };
            // The taskbar button exists only once the window shows (eframe shows it
            // after the first frame), and buttons added before then never appear.
            if IsWindowVisible(hwnd).as_bool() {
                bar.add();
            }
            let cell = Box::new(std::cell::RefCell::new(Some(bar)));
            let created = RegisterWindowMessageW(w!("TaskbarButtonCreated"));
            // When the app runs elevated, Windows drops these messages from Explorer unless allowed.
            for m in [created, WM_COMMAND] {
                let _ = ChangeWindowMessageFilterEx(hwnd, m, MSGFLT_ALLOW, None);
            }
            let hook = Box::new(Hook { tx, wake, created, bar: &*cell });
            if !SetWindowSubclass(hwnd, Some(subclass), 0x7974_6d62, Box::into_raw(hook) as usize).as_bool() {
                log::warn!("taskbar buttons: couldn't subclass the window");
                return None;
            }
            Some(cell)
        }
    }

    fn buttons(&self) -> [THUMBBUTTON; 3] {
        let (toggle, tip) = if self.playing { (self.pause, "Pause") } else { (self.play, "Play") };
        [button(PREV, self.prev, "Previous"), button(TOGGLE, toggle, tip), button(NEXT, self.next, "Next")]
    }

    fn add(&self) {
        let buttons = self.buttons();
        // Adding twice fails; then the buttons are already there, so just refresh them.
        if unsafe { self.taskbar.ThumbBarAddButtons(self.hwnd, &buttons) }.is_err()
            && let Err(e) = unsafe { self.taskbar.ThumbBarUpdateButtons(self.hwnd, &buttons) }
        {
            log::warn!("taskbar buttons: {e}");
        }
    }

    pub fn set_playing(&mut self, playing: bool) {
        if self.playing != playing {
            self.playing = playing;
            let _ = unsafe { self.taskbar.ThumbBarUpdateButtons(self.hwnd, &self.buttons()) };
        }
    }
}

fn button(id: u32, icon: HICON, tip: &str) -> THUMBBUTTON {
    let mut sz = [0u16; 260];
    for (d, s) in sz.iter_mut().zip(tip.encode_utf16()) {
        *d = s;
    }
    THUMBBUTTON { dwMask: THB_ICON | THB_TOOLTIP | THB_FLAGS, iId: id, hIcon: icon, szTip: sz, dwFlags: THBF_ENABLED, ..Default::default() }
}

unsafe extern "system" fn subclass(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, data: usize) -> LRESULT {
    let hook = unsafe { &*(data as *const Hook) };
    if msg == WM_COMMAND && (wp.0 >> 16) as u32 == THBN_CLICKED {
        let ev = match (wp.0 & 0xffff) as u32 {
            PREV => Some(MediaControlEvent::Previous),
            TOGGLE => Some(MediaControlEvent::Toggle),
            NEXT => Some(MediaControlEvent::Next),
            _ => None,
        };
        if let Some(ev) = ev {
            let _ = hook.tx.send(ev);
            (hook.wake)();
            return LRESULT(0);
        }
    }
    if msg == WM_SHOWWINDOW && wp.0 != 0 {
        // Give Explorer a moment to create the taskbar button first.
        unsafe { SetTimer(Some(hwnd), SHOWN_TIMER, 250, None) };
    }
    let shown = msg == WM_TIMER && wp.0 == SHOWN_TIMER;
    if shown {
        let _ = unsafe { KillTimer(Some(hwnd), SHOWN_TIMER) };
    }
    if shown || msg == hook.created {
        // The window showed, or Explorer (re)created the taskbar button.
        if let Ok(bar) = unsafe { &*hook.bar }.try_borrow()
            && let Some(bar) = bar.as_ref()
        {
            bar.add();
        }
    }
    unsafe { DefSubclassProc(hwnd, msg, wp, lp) }
}

#[derive(Clone, Copy)]
enum Glyph {
    Play,
    Pause,
    Prev,
    Next,
}

/// Convex shapes in a unit square, white on transparent.
fn shapes(g: Glyph) -> Vec<Vec<(f32, f32)>> {
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)];
    match g {
        Glyph::Play => vec![vec![(0.27, 0.16), (0.85, 0.5), (0.27, 0.84)]],
        Glyph::Pause => vec![rect(0.24, 0.18, 0.42, 0.82), rect(0.58, 0.18, 0.76, 0.82)],
        Glyph::Prev => vec![rect(0.18, 0.2, 0.3, 0.8), vec![(0.82, 0.2), (0.82, 0.8), (0.32, 0.5)]],
        Glyph::Next => vec![rect(0.7, 0.2, 0.82, 0.8), vec![(0.18, 0.2), (0.68, 0.5), (0.18, 0.8)]],
    }
}

fn inside(poly: &[(f32, f32)], x: f32, y: f32) -> bool {
    // Convex, either winding: the point is on the same side of every edge.
    let mut sign = 0.0f32;
    for i in 0..poly.len() {
        let (ax, ay) = poly[i];
        let (bx, by) = poly[(i + 1) % poly.len()];
        let c = (bx - ax) * (y - ay) - (by - ay) * (x - ax);
        if c != 0.0 {
            if sign != 0.0 && c.signum() != sign {
                return false;
            }
            sign = c.signum();
        }
    }
    true
}

/// Coverage of each pixel, 4×4 supersampled.
fn raster(px: i32, g: Glyph) -> Vec<u8> {
    let polys = shapes(g);
    let n = px as usize;
    let mut out = vec![0u8; n * n];
    for y in 0..n {
        for x in 0..n {
            let mut hit = 0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let u = (x as f32 + (sx as f32 + 0.5) / 4.0) / n as f32;
                    let v = (y as f32 + (sy as f32 + 0.5) / 4.0) / n as f32;
                    if polys.iter().any(|p| inside(p, u, v)) {
                        hit += 1;
                    }
                }
            }
            out[y * n + x] = (hit * 255 / 16) as u8;
        }
    }
    out
}

fn icon(px: i32, g: Glyph) -> Option<HICON> {
    let alpha = raster(px, g);
    unsafe {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: px,
                biHeight: -px,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let color = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        let dst = std::slice::from_raw_parts_mut(bits as *mut u32, alpha.len());
        for (d, a) in dst.iter_mut().zip(&alpha) {
            *d = (*a as u32) << 24 | 0x00ff_ffff;
        }
        let mask = CreateBitmap(px, px, 1, 1, None);
        let icon = CreateIconIndirect(&ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color }).ok();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}
