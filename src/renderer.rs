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

/// The saved choice; wgpu falls back to OpenGL when no adapter for it answers, since a
/// window that fails to open can't be switched back from inside the app.
pub fn init(data_dir: &Path) -> Renderer {
    let mut r = load(data_dir);
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
pub fn wgpu_options() -> eframe::egui_wgpu::WgpuConfiguration {
    let mut setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends = backends();
    eframe::egui_wgpu::WgpuConfiguration { wgpu_setup: eframe::egui_wgpu::WgpuSetup::CreateNew(setup), ..Default::default() }
}

fn available() -> bool {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = backends();
    let instance = wgpu::Instance::new(desc);
    let Ok(rt) = tokio::runtime::Builder::new_current_thread().build() else { return false };
    let adapter = rt.block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()));
    adapter.is_ok()
}
