//! Which GPU API draws the window. OpenGL unless the user picks wgpu (Vulkan; Metal on
//! macOS) in Settings › Window. The UI is a few thousand 2D triangles that only redraw
//! when something changes, so wgpu brings no visible speedup here, and it was measured
//! at about three times the memory and five times the video memory (issue #42). It's
//! there for drivers whose OpenGL misdraws, or for whoever prefers it.

use std::path::Path;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Renderer {
    OpenGl,
    Wgpu,
}

static ACTIVE: OnceLock<Renderer> = OnceLock::new();

/// A one-time relaunch after OpenGL could not create a window.
pub const RETRY_FLAG: &str = "--renderer-retry";

pub fn retrying() -> bool {
    std::env::args_os().skip(1).any(|a| a == RETRY_FLAG)
}

/// The relaunch's arguments: this launch's own (a jam link to join, say), without the
/// predecessor's `--after=`, plus the retry flag.
pub fn retry_args(args: impl Iterator<Item = std::ffi::OsString>) -> Vec<String> {
    let mut out: Vec<String> = args.filter_map(|a| a.into_string().ok()).filter(|a| !a.starts_with("--after=")).collect();
    out.push(RETRY_FLAG.into());
    out
}

/// The saved choice; wgpu falls back to OpenGL when no adapter for it answers, since a
/// window that fails to open can't be switched back from inside the app.
pub fn init(data_dir: &Path) -> Renderer {
    let mut r = if retrying() { Renderer::Wgpu } else { load(data_dir) };
    if r == Renderer::Wgpu && !available() {
        log::warn!("renderer: no {} adapter; drawing with OpenGL", api_name());
        r = Renderer::OpenGl;
    }
    let _ = ACTIVE.set(r);
    r
}

/// The renderer this run draws with.
pub fn active() -> Renderer {
    *ACTIVE.get_or_init(|| Renderer::OpenGl)
}

pub fn load(data_dir: &Path) -> Renderer {
    match std::fs::read_to_string(data_dir.join("renderer")).unwrap_or_default().trim() {
        "wgpu" => Renderer::Wgpu,
        _ => Renderer::OpenGl,
    }
}

pub fn save(data_dir: &Path, r: Renderer) -> std::io::Result<()> {
    std::fs::write(data_dir.join("renderer"), if r == Renderer::Wgpu { "wgpu" } else { "opengl" })
}

/// The API wgpu draws through here.
pub fn api_name() -> &'static str {
    if cfg!(target_os = "macos") { "Metal" } else { "Vulkan" }
}

pub fn backends() -> wgpu::Backends {
    if cfg!(target_os = "macos") { wgpu::Backends::METAL } else { wgpu::Backends::VULKAN }
}

/// eframe's wgpu settings: only [`backends`], so "Vulkan" never quietly means DX12 or GL.
/// And memory over speed (#154): wgpu's default sets aside big blocks of video memory up
/// front, and eframe's keeps three window-sized images in flight. At a Retina window's
/// size that was over 1.2 GB of video memory, which a Mac counts as the app's own; the
/// UI needs neither.
pub fn wgpu_options() -> eframe::egui_wgpu::WgpuConfiguration {
    let mut setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends = backends();
    let device = setup.device_descriptor.clone();
    setup.device_descriptor = std::sync::Arc::new(move |adapter| wgpu::DeviceDescriptor { memory_hints: wgpu::MemoryHints::MemoryUsage, ..device(adapter) });
    eframe::egui_wgpu::WgpuConfiguration {
        surface: eframe::egui_wgpu::SurfaceConfig::LOW_LATENCY,
        wgpu_setup: eframe::egui_wgpu::WgpuSetup::CreateNew(setup),
        ..Default::default()
    }
}

pub fn available() -> bool {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = backends();
    let instance = wgpu::Instance::new(desc);
    let Ok(rt) = tokio::runtime::Builder::new_current_thread().build() else { return false };
    let adapter = rt.block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()));
    adapter.is_ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn retry_args_keep_the_launch_and_add_the_flag() {
        let args = ["--after=42", "ytm-rs://jam/ABC123"].map(std::ffi::OsString::from);
        assert_eq!(super::retry_args(args.into_iter()), ["ytm-rs://jam/ABC123", super::RETRY_FLAG]);
        assert_eq!(super::retry_args(std::iter::empty()), [super::RETRY_FLAG]);
    }
}
