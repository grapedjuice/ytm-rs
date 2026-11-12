//! Windows 11's material behind the window (Settings › Appearance › Window material),
//! in any theme: Mica, Mica Alt or Acrylic. The window must be created see-through,
//! so a change applies from the next start, and only OpenGL's window can be (Vulkan's
//! swapchain isn't composited that way).

use std::path::Path;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Backdrop {
    #[default]
    None,
    Mica,
    MicaAlt,
    Acrylic,
}

impl Backdrop {
    pub const ALL: [Self; 4] = [Self::None, Self::Mica, Self::MicaAlt, Self::Acrylic];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Mica => "Mica",
            Self::MicaAlt => "Mica Alt",
            Self::Acrylic => "Acrylic",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Mica => "mica",
            Self::MicaAlt => "mica_alt",
            Self::Acrylic => "acrylic",
        }
    }
}

static ACTIVE: OnceLock<Backdrop> = OnceLock::new();

pub fn load(data_dir: &Path) -> Backdrop {
    let saved = std::fs::read_to_string(data_dir.join("backdrop")).unwrap_or_default();
    Backdrop::ALL.into_iter().find(|b| b.key() == saved.trim()).unwrap_or_default()
}

pub fn save(data_dir: &Path, b: Backdrop) -> std::io::Result<()> {
    std::fs::write(data_dir.join("backdrop"), b.key())
}

/// Settle this run's material, before the window opens. Returns whether the window has
/// to be see-through.
pub fn init(data_dir: &Path, renderer: crate::renderer::Renderer) -> bool {
    let b = if supported() && renderer == crate::renderer::Renderer::OpenGl { load(data_dir) } else { Backdrop::None };
    log::info!("backdrop: {b:?}");
    let _ = ACTIVE.set(b);
    b != Backdrop::None
}

/// The material this run's window was made with.
pub fn active() -> Backdrop {
    *ACTIVE.get_or_init(Backdrop::default)
}

/// Whether the material shows: when it's on, it takes the place of the pages' background.
pub fn shows() -> bool {
    active() != Backdrop::None
}

/// Windows 11 22H2 or later, where `DWMWA_SYSTEMBACKDROP_TYPE` exists.
#[cfg(windows)]
pub fn supported() -> bool {
    use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::w;
    let mut buf = [0u16; 16];
    let mut size = std::mem::size_of_val(&buf) as u32;
    // SAFETY: an out-buffer of the size passed; the strings are static.
    let read = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"), w!("CurrentBuildNumber"), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut size)) };
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    read.is_ok() && String::from_utf16_lossy(&buf[..len]).parse::<u32>().is_ok_and(|build| build >= 22621)
}

#[cfg(not(windows))]
pub fn supported() -> bool {
    false
}

/// Note the window for `follow`, when this run has a material.
#[cfg(windows)]
pub fn prepare(hwnd: *mut std::ffi::c_void) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::SetWindowSubclass;
    if active() != Backdrop::None {
        WINDOW.store(hwnd, std::sync::atomic::Ordering::Relaxed);
        // SAFETY: the app's own window, on the thread that made it; the subclass lives as
        // long as the window.
        if !unsafe { SetWindowSubclass(HWND(hwnd), Some(settings_changed), 0, 0) }.as_bool() {
            log::warn!("backdrop: couldn't subclass the window");
        }
    }
}

#[cfg(not(windows))]
pub fn prepare(_hwnd: *mut std::ffi::c_void) {}

#[cfg(windows)]
static WINDOW: std::sync::atomic::AtomicPtr<std::ffi::c_void> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// What was last sent to Windows: `show` and `light` as bits, `u8::MAX` for nothing yet.
#[cfg(windows)]
static LAST: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(u8::MAX);

/// The material while `show`, in light or dark. Called every frame; only a change reaches
/// Windows. Only these two attributes, as Tauri's `window-vibrancy` sets them: the window
/// is already see-through and frameless, and its styles and frame are what snapping and
/// maximizing depend on.
#[cfg(windows)]
pub fn follow(show: bool, light: bool) {
    use std::sync::atomic::Ordering;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmGetWindowAttribute};
    let hwnd = HWND(WINDOW.load(Ordering::Relaxed));
    if hwnd.0.is_null() {
        return;
    }
    // egui sets its dark theme on the window after the first frame, over ours: in light,
    // the material stayed dark (#109). So a window that lost ours counts as a change.
    let mut now = windows::core::BOOL::default();
    // SAFETY: the app's own window; the attribute is a BOOL, the size passed.
    let read = unsafe { DwmGetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &mut now as *mut _ as *mut _, std::mem::size_of_val(&now) as u32) };
    let kept = read.is_err() || now.as_bool() != light;
    let state = show as u8 | (light as u8) << 1;
    if LAST.swap(state, Ordering::Relaxed) != state || !kept {
        apply(hwnd, state);
    }
}

/// winit sets the system's light or dark on the window at every settings change, and
/// Windows sends one for many settings: put ours back straight away, drawing or not.
#[cfg(windows)]
unsafe extern "system" fn settings_changed(hwnd: windows::Win32::Foundation::HWND, msg: u32, wparam: windows::Win32::Foundation::WPARAM, lparam: windows::Win32::Foundation::LPARAM, _: usize, _: usize) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::WM_SETTINGCHANGE;
    // SAFETY: the window's own message, passed on as it came.
    let result = unsafe { windows::Win32::UI::Shell::DefSubclassProc(hwnd, msg, wparam, lparam) };
    let last = LAST.load(std::sync::atomic::Ordering::Relaxed);
    if msg == WM_SETTINGCHANGE && last != u8::MAX {
        apply(hwnd, last);
    }
    result
}

/// Send a `follow` state to Windows.
#[cfg(windows)]
fn apply(hwnd: windows::Win32::Foundation::HWND, state: u8) {
    use windows::Win32::Graphics::Dwm::{DWM_SYSTEMBACKDROP_TYPE, DWMSBT_MAINWINDOW, DWMSBT_NONE, DWMSBT_TABBEDWINDOW, DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
    let (show, light) = (state & 1 != 0, state & 2 != 0);
    let kind: DWM_SYSTEMBACKDROP_TYPE = match active() {
        _ if !show => DWMSBT_NONE,
        Backdrop::None => return,
        Backdrop::Mica => DWMSBT_MAINWINDOW,
        Backdrop::MicaAlt => DWMSBT_TABBEDWINDOW,
        Backdrop::Acrylic => DWMSBT_TRANSIENTWINDOW,
    };
    let dark = windows::core::BOOL::from(!light);
    // SAFETY: the app's own window; each attribute is a value of the size passed.
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, std::mem::size_of_val(&dark) as u32);
        if let Err(e) = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &kind as *const _ as *const _, std::mem::size_of_val(&kind) as u32) {
            log::warn!("backdrop: {e}");
        }
    }
}

#[cfg(not(windows))]
pub fn follow(_show: bool, _light: bool) {}

#[cfg(test)]
mod tests {
    use super::Backdrop;

    #[test]
    fn a_saved_material_comes_back_and_nonsense_means_none() {
        let dir = std::env::temp_dir().join(format!("ytm-backdrop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for b in Backdrop::ALL {
            super::save(&dir, b).unwrap();
            assert_eq!(super::load(&dir), b);
        }
        std::fs::write(dir.join("backdrop"), "sparkles").unwrap();
        assert_eq!(super::load(&dir), Backdrop::None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
