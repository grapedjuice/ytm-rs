//! RivaTuner Statistics Server (RTSS) overlay opt-out on Windows.
//!
//! RTSS injects `RTSSHooks64.dll` into OpenGL and Vulkan windows to draw its FPS/hardware
//! overlay. Exporting `RTSSHooksCompatibility` set to `0` tells RTSS to skip hooking this
//! process (users can still force it via `ForceHooking=1` in RTSS's own profile).

use std::path::Path;
use std::sync::OnceLock;
#[cfg(target_os = "windows")]
use std::sync::atomic::{AtomicU32, Ordering};

/// Exported PE symbol read by RTSS (`RTSSHooks64.dll`) when deciding whether to hook.
/// `0` opts out of hooking; non-zero (`1`) allows standard hooking.
#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
#[used]
pub static RTSSHooksCompatibility: AtomicU32 = AtomicU32::new(0);

static ACTIVE_DISABLED: OnceLock<bool> = OnceLock::new();

/// Whether RTSS overlay is disabled in the saved setting (defaults to `true`).
pub fn load(data_dir: &Path) -> bool {
    !matches!(std::fs::read_to_string(data_dir.join("disable_rtss")).unwrap_or_default().trim(), "0" | "false")
}

pub fn save(data_dir: &Path, disabled: bool) -> std::io::Result<()> {
    std::fs::write(data_dir.join("disable_rtss"), if disabled { "1" } else { "0" })
}

/// Apply the saved setting before any window or graphics context is created.
pub fn init(data_dir: &Path) {
    let disabled = load(data_dir);
    let _ = ACTIVE_DISABLED.set(disabled);
    #[cfg(target_os = "windows")]
    {
        RTSSHooksCompatibility.store(if disabled { 0 } else { 1 }, Ordering::Release);
    }
}

/// Whether RTSS overlay was disabled for this run.
#[cfg(target_os = "windows")]
pub fn active() -> bool {
    *ACTIVE_DISABLED.get_or_init(|| true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_disabled_and_roundtrips() {
        let dir = std::env::temp_dir().join(format!("ytm-rs-rtss-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        assert!(load(&dir));
        save(&dir, false).unwrap();
        assert!(!load(&dir));
        save(&dir, true).unwrap();
        assert!(load(&dir));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
