#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod appicon;
mod fullscreen;
mod proxy;
mod art;
mod audio;
mod backend;
mod deeplink;
mod discord;
mod downloads;
mod images;
mod jam;
mod innertube;
mod jsc;
mod lastfm;
mod login;
mod lyrics;
mod media;
mod model;
mod opus;
mod preset;
mod shader;
mod sponsorblock;
mod stream;
mod theme;
mod transfer;
mod translate;
mod tray;
mod turnstile;
mod ui;
mod update;
mod webmusic;
mod ytdlp;
mod ytm;
mod vault;
mod ytmapi;

use std::sync::{Arc, OnceLock};

use crossbeam_channel::unbounded;

/// Window size on first launch, and the smallest it may be (points).
pub const DEFAULT_SIZE: [f32; 2] = [1200.0, 780.0];
pub const MIN_SIZE: [f32; 2] = [760.0, 480.0];

fn main() -> eframe::Result {
    // Child-process modes for the webview windows (see login.rs / turnstile.rs).
    let mut args = std::env::args_os().skip(1);
    if let (Some(flag), Some(dir)) = (args.next(), args.next()) {
        // The parent hands its proxy setting down through the environment.
        proxy::init(std::path::Path::new(""));
        if flag == login::FLAG {
            login::child_main(login::Site::Google, dir.into());
            return Ok(());
        }
        if flag == login::SPOTIFY_FLAG {
            login::child_main(login::Site::Spotify, dir.into());
            return Ok(());
        }
        if flag == turnstile::FLAG {
            turnstile::child_main(dir.into());
            return Ok(());
        }
    }
    // A jam link (`ytm-rs://jam/CODE` or the https share link): hand it to the running
    // instance if there is one, else join once this one is up. Before `init_logging`,
    // which would truncate the running instance's log file.
    if let Some(code) = deeplink::from_args() {
        if deeplink::forward_to_running(&code) {
            return Ok(());
        }
        deeplink::push(code);
    } else if deeplink::forward_show() {
        return Ok(());
    }
    init_logging();
    update::clean_portable();
    stream::init_dev_hooks();
    proxy::init(&data_dir());

    // Dev hook: YTM_BACKGROUND opens the window off-screen, unfocused and off the
    // taskbar, so automated test runs don't pop up over whatever the user is doing.
    let background = std::env::var_os("YTM_BACKGROUND").is_some();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("YT Music")
        .with_app_id("ytm-rs")
        .with_inner_size(DEFAULT_SIZE)
        .with_min_inner_size(MIN_SIZE)
        .with_icon(appicon::get(appicon::load(&data_dir())).icon_data().expect("bundled icons are valid PNGs"));
    if background {
        viewport = viewport.with_position([-4000.0, 0.0]).with_inner_size([1920.0, 1057.0]).with_active(false).with_taskbar(false);
    }
    let options = eframe::NativeOptions {
        viewport,
        persist_window: !background,
        // Settings live with the rest of the data: beside the portable exe, or a
        // YTM_DATA_DIR profile (e.g. a clean signed-out one for screenshots).
        persistence_path: Some(data_dir().join("app.ron")),
        // No MSAA/depth/stencil: egui doesn't need them and they cost VRAM.
        multisampling: 0,
        depth_buffer: 0,
        stencil_buffer: 0,
        // Dev hook: YTM_FPS paces frames itself (see `dev_pace`), so vsync is off.
        glow_options: eframe::egui_glow::GlowConfiguration { vsync: std::env::var_os("YTM_FPS").is_none(), ..Default::default() },
        ..Default::default()
    };

    eframe::run_native(
        "ytm-rs",
        options,
        Box::new(|cc| {
            let ctx = cc.egui_ctx.clone();
            theme::install_style(&ctx);
            theme::install_fonts(&ctx, "inter");

            // Background threads wake the UI through this; set once the context exists.
            static CTX: OnceLock<egui::Context> = OnceLock::new();
            let _ = CTX.set(ctx.clone());
            let wake = || {
                if let Some(c) = CTX.get() {
                    c.request_repaint();
                }
            };

            // Jam links from later launches arrive over a pipe (first instance only).
            deeplink::listen(wake);
            // Keep `ytm-rs:` links pointing at this copy; dev builds leave it alone
            // (even with YTM_UPDATE_FORCE).
            if update::installed() && !cfg!(debug_assertions) {
                deeplink::register_protocol();
            }

            let volume = cc
                .storage
                .and_then(|s| s.get_string("volume"))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.8f32);
            // The slider is logarithmic unless turned off. A volume saved before that
            // existed was linear: move it to where it sounds the same.
            let log_setting = cc.storage.and_then(|s| s.get_string("log_volume"));
            let log_volume = log_setting.as_deref().is_none_or(|v| v == "1");
            let volume = if log_setting.is_none() { volume.cbrt() } else { volume };
            // Dev hook: YTM_MUTE starts silent and never persists the volume.
            let volume = if std::env::var_os("YTM_MUTE").is_some() { 0.0 } else { volume };

            let device = cc.storage.and_then(|s| s.get_string("output_device")).filter(|d| !d.is_empty());

            let (audio_tx, audio_rx) = unbounded();
            let audio = audio::spawn(volume, log_volume, device, audio_tx, wake);

            let data_dir = data_dir();
            let (resp_tx, resp_rx) = unbounded();
            let backend = backend::Backend::new(data_dir, resp_tx, audio.clone(), Arc::new(wake))?;

            egui_extras::install_image_loaders(&ctx);
            ctx.add_bytes_loader(Arc::new(images::HttpImages::new(backend.clone())));

            let (media_tx, media_rx) = unbounded();
            if let Some(h) = hwnd(cc) {
                appicon::set_hwnd(h);
                fullscreen::set_hwnd(h);
                // eframe sets only the small icon; the taskbar button wants the big one.
                appicon::set_window_icon(appicon::get(appicon::load(backend.data_dir())));
            }
            let media = media::Media::new(hwnd(cc), media_tx, wake);

            // Which GL driver this is, for reports of drawing problems (Windows on ARM runs
            // GL through Mesa's D3D12 layer).
            if let Some(gl) = &cc.gl {
                use eframe::glow::HasContext;
                // SAFETY: plain string queries on the current context.
                let (renderer, version) = unsafe { (gl.get_parameter_string(eframe::glow::RENDERER), gl.get_parameter_string(eframe::glow::VERSION)) };
                log::info!("gl: {renderer} ({version})");
            }
            // The animated background needs the GL context; without it the UI falls back
            // to a flat gradient.
            let bg = cc.gl.as_ref().filter(|_| std::env::var_os("YTM_NO_SHADER").is_none()).and_then(|gl| {
                shader::Background::new(gl).map_err(|e| log::warn!("background shader unavailable: {e:#}")).ok()
            });

            Ok(Box::new(ui::App::new(
                backend,
                audio,
                ui::Channels { resp_rx, audio_rx, media_rx },
                media,
                bg,
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

/// Log to stderr in debug builds; release builds have no console, so log to a file
/// next to the settings (overwritten each launch).
fn init_logging() {
    let mut builder = env_logger::Builder::from_env(
        // A test build can be made more talkative: YTM_DEFAULT_LOG=warn,ytm_rs=debug cargo build.
        env_logger::Env::default().default_filter_or(option_env!("YTM_DEFAULT_LOG").unwrap_or("warn,ytm_rs=info")),
    );
    // Dev hook: YTM_LOG=<path> logs there (test runs beside a running copy).
    if let Some(path) = std::env::var_os("YTM_LOG") {
        if let Ok(file) = std::fs::File::create(path) {
            builder.target(env_logger::Target::Pipe(Box::new(file)));
        }
    } else if !cfg!(debug_assertions) {
        let dir = data_dir();
        let _ = std::fs::create_dir_all(&dir);
        // The last run's log too: reopening the app after a problem would otherwise wipe it.
        let _ = std::fs::rename(dir.join("ytm-rs.log"), dir.join("ytm-rs.prev.log"));
        if let Ok(file) = std::fs::File::create(dir.join("ytm-rs.log")) {
            builder.target(env_logger::Target::Pipe(Box::new(file)));
        }
    }
    builder.init();
}

/// Settings, caches and the sign-in: `data` beside the portable build's exe, else the
/// user's app data. Dev hook: YTM_DATA_DIR runs on a separate profile.
fn data_dir() -> std::path::PathBuf {
    if let Some(d) = std::env::var_os("YTM_DATA_DIR") {
        return d.into();
    }
    if let Some(dir) = update::portable_dir() {
        return dir.join("data");
    }
    directories::ProjectDirs::from("", "", "ytm-rs").expect("no home directory").data_dir().to_path_buf()
}
