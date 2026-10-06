#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod backend;
mod fonts;
mod images;
mod innertube;
mod login;
mod media;
mod stream;

use std::sync::{Arc, OnceLock};

use crossbeam_channel::unbounded;

fn main() -> eframe::Result {
    let mut args = std::env::args_os().skip(1);
    if args.next().is_some_and(|a| a == login::FLAG) {
        if let Some(dir) = args.next() {
            login::child_main(dir.into());
        }
        return Ok(());
    }
    init_logging();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("YT Music")
            .with_app_id("ytm-rs")
            .with_inner_size([1200.0, 780.0])
            .with_min_inner_size([760.0, 480.0]),
        // No MSAA/depth/stencil: egui doesn't need them and they cost VRAM.
        multisampling: 0,
        depth_buffer: 0,
        stencil_buffer: 0,
        ..Default::default()
    };

    eframe::run_native(
        "ytm-rs",
        options,
        Box::new(|cc| {
            let ctx = cc.egui_ctx.clone();
            style(&ctx);
            fonts::install(&ctx);

            // Background threads wake the UI through this; set once the context exists.
            static CTX: OnceLock<egui::Context> = OnceLock::new();
            let _ = CTX.set(ctx.clone());
            let wake = || {
                if let Some(c) = CTX.get() {
                    c.request_repaint();
                }
            };

            let volume = cc
                .storage
                .and_then(|s| s.get_string("volume"))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.8f32);

            let (audio_tx, audio_rx) = unbounded();
            let audio = audio::spawn(volume, audio_tx, wake);

            let dirs = directories::ProjectDirs::from("", "", "ytm-rs").expect("no home directory");
            let (resp_tx, resp_rx) = unbounded();
            let backend = backend::Backend::new(dirs.data_dir().to_path_buf(), resp_tx, audio.clone(), Arc::new(wake))?;

            egui_extras::install_image_loaders(&ctx);
            ctx.add_bytes_loader(Arc::new(images::HttpImages::new(backend.clone())));

            let (media_tx, media_rx) = unbounded();
            let media = media::Media::new(hwnd(cc), media_tx, wake);

            Ok(Box::new(app::App::new(
                backend,
                audio,
                app::Channels { resp_rx, audio_rx, media_rx },
                media,
                cc.storage,
            )))
        }),
    )
}

#[cfg(windows)]
fn hwnd(cc: &eframe::CreationContext) -> Option<*mut std::ffi::c_void> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get() as *mut std::ffi::c_void),
        _ => None,
    }
}

#[cfg(not(windows))]
fn hwnd(_: &eframe::CreationContext) -> Option<*mut std::ffi::c_void> {
    None
}

fn style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
        s.visuals.panel_fill = egui::Color32::from_gray(5);
        s.visuals.selection.bg_fill = egui::Color32::from_rgb(0xff, 0x33, 0x4b);
        s.visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(6);
        s.visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(6);
        s.visuals.widgets.active.corner_radius = egui::CornerRadius::same(6);
        s.interaction.selectable_labels = false;
    });
}

/// Log to stderr in debug builds; release builds have no console, so log to a file
/// next to the settings (overwritten each launch).
fn init_logging() {
    let mut builder = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,rustypipe=off,tracing::span=off,ytm_rs=info"),
    );
    if !cfg!(debug_assertions) {
        if let Some(dirs) = directories::ProjectDirs::from("", "", "ytm-rs") {
            let _ = std::fs::create_dir_all(dirs.data_dir());
            if let Ok(file) = std::fs::File::create(dirs.data_dir().join("ytm-rs.log")) {
                builder.target(env_logger::Target::Pipe(Box::new(file)));
            }
        }
    }
    builder.init();
}
