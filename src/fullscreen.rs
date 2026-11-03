//! Full screen on Windows, in one step. winit takes the title bar off first and
//! resizes the window afterwards; in between Windows paints the frame unthemed (the
//! old Windows 7 look) and the new area black until the app draws a frame at the new
//! size. Here the style and size change together with painting held off, and the
//! window erases to a dark background instead of black.
//!
//! The window is also made one pixel taller than the monitor: graphics drivers treat
//! an OpenGL window that exactly covers it as exclusive full screen, and blank the
//! whole screen for a frame each time they switch in or out of that mode.

/// The main window, from `main`.
#[cfg_attr(not(windows), allow(unused_variables))]
pub fn set_hwnd(hwnd: *mut std::ffi::c_void) {
    #[cfg(windows)]
    imp::init(hwnd as usize);
}

/// Enter or leave full screen. False when this platform (or a missing window handle)
/// leaves it to winit.
pub fn set(on: bool) -> bool {
    #[cfg(windows)]
    return imp::set(on);
    #[cfg(not(windows))]
    {
        let _ = on;
        false
    }
}

/// The work area (the monitor less the taskbar) of the monitor the window is on, in
/// physical pixels. None where the app can't ask (not Windows, no window handle yet).
pub fn work_area() -> Option<egui::Rect> {
    #[cfg(windows)]
    return imp::work_area();
    #[cfg(not(windows))]
    None
}

/// Whether some of `rect` (physical pixels) is on a connected monitor. True where the
/// app can't ask.
pub fn on_a_monitor(rect: egui::Rect) -> bool {
    #[cfg(windows)]
    return imp::on_a_monitor(rect);
    #[cfg(not(windows))]
    {
        let _ = rect;
        true
    }
}

#[cfg(windows)]
mod imp {
    use std::sync::{Mutex, OnceLock};

    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        CreateSolidBrush, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromRect, MonitorFromWindow, RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE,
        RedrawWindow,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GCLP_HBRBACKGROUND, GWL_EXSTYLE, GWL_STYLE, GetWindowLongW, GetWindowPlacement, IsZoomed, SW_RESTORE, ShowWindow, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOCOPYBITS, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
        SendMessageW, SetClassLongPtrW, SetWindowLongW, SetWindowPlacement, SetWindowPos, WINDOWPLACEMENT, WM_SETREDRAW, WS_CAPTION, WS_EX_CLIENTEDGE,
        WS_EX_DLGMODALFRAME, WS_EX_STATICEDGE, WS_EX_WINDOWEDGE, WS_MAXIMIZE, WS_THICKFRAME,
    };

    static WINDOW: OnceLock<usize> = OnceLock::new();

    /// The window's style and placement from before full screen.
    struct Saved {
        style: i32,
        ex_style: i32,
        placement: WINDOWPLACEMENT,
    }

    // SAFETY: plain data; only touched on the UI thread, under the mutex.
    unsafe impl Send for Saved {}

    static SAVED: Mutex<Option<Saved>> = Mutex::new(None);

    pub fn init(hwnd: usize) {
        if WINDOW.set(hwnd).is_err() {
            return;
        }
        // Area the app hasn't drawn yet (just after a resize) shows this, not black.
        // SAFETY: a brush for the window's own class, kept for the process's lifetime.
        unsafe {
            let brush = CreateSolidBrush(COLORREF(0x000F_0B0D));
            SetClassLongPtrW(HWND(hwnd as *mut _), GCLP_HBRBACKGROUND, brush.0 as isize);
        }
    }

    pub fn work_area() -> Option<egui::Rect> {
        let &h = WINDOW.get()?;
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        // SAFETY: the app's own window; `info` is ours and sized.
        unsafe { GetMonitorInfoW(MonitorFromWindow(HWND(h as *mut _), MONITOR_DEFAULTTONEAREST), &mut info) }.as_bool().then(|| {
            let r = info.rcWork;
            egui::Rect::from_min_max(egui::pos2(r.left as f32, r.top as f32), egui::pos2(r.right as f32, r.bottom as f32))
        })
    }

    pub fn on_a_monitor(rect: egui::Rect) -> bool {
        let r = RECT { left: rect.left() as i32, top: rect.top() as i32, right: rect.right() as i32, bottom: rect.bottom() as i32 };
        // SAFETY: plain query.
        !unsafe { MonitorFromRect(&r, MONITOR_DEFAULTTONULL) }.is_invalid()
    }

    pub fn set(on: bool) -> bool {
        let Some(&h) = WINDOW.get() else { return false };
        let hwnd = HWND(h as *mut _);
        let mut saved = SAVED.lock().unwrap();
        if on == saved.is_some() {
            return true;
        }
        // SAFETY: the app's own window, on its UI thread; every struct passed is ours.
        unsafe {
            SendMessageW(hwnd, WM_SETREDRAW, Some(WPARAM(0)), None);
            if on {
                let style = GetWindowLongW(hwnd, GWL_STYLE);
                let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE);
                let mut placement = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
                let _ = GetWindowPlacement(hwnd, &mut placement);
                let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
                let _ = GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut info);
                let r = info.rcMonitor;
                // winit keeps a maximized window without the system frame (the app's own
                // title bar) to the work area, which would leave the taskbar's strip out:
                // restore it first. The saved placement maximizes it again on the way out.
                if IsZoomed(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
                SetWindowLongW(hwnd, GWL_STYLE, style & !((WS_CAPTION | WS_THICKFRAME | WS_MAXIMIZE).0 as i32));
                let edges = WS_EX_DLGMODALFRAME | WS_EX_WINDOWEDGE | WS_EX_CLIENTEDGE | WS_EX_STATICEDGE;
                SetWindowLongW(hwnd, GWL_EXSTYLE, ex_style & !(edges.0 as i32));
                // No copying the old picture to its old spot: until the app draws a frame
                // at the new size, the window shows its plain dark background.
                let flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED | SWP_NOCOPYBITS;
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top + 1, flags);
                *saved = Some(Saved { style, ex_style, placement });
            } else if let Some(s) = saved.take() {
                SetWindowLongW(hwnd, GWL_STYLE, s.style);
                SetWindowLongW(hwnd, GWL_EXSTYLE, s.ex_style);
                let _ = SetWindowPlacement(hwnd, &s.placement);
                let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
            }
            SendMessageW(hwnd, WM_SETREDRAW, Some(WPARAM(1)), Some(LPARAM(0)));
            let _ = RedrawWindow(Some(hwnd), None, None, RDW_ERASE | RDW_FRAME | RDW_INVALIDATE | RDW_ALLCHILDREN);
        }
        true
    }
}
