#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod appicon;
mod fullscreen;
mod hls;
mod proxy;
mod renderer;
mod art;
mod audio;
mod awake;
mod backdrop;
mod backend;
mod bidi;
mod canvas;
mod deeplink;
mod discord;
mod downloads;
mod hotkeys;
mod images;
mod jam;
mod innertube;
mod jsc;
mod lastfm;
mod login;
mod listenbrainz;
mod lyrics;
mod media;
mod model;
mod opus;
mod preset;
mod remote;
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
mod video;
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
    // glibc gives each thread that allocates its own heap (up to 8 per core), and what a
    // song frees in one isn't reused by the next song's threads: on Linux the app grew a
    // few MB with every song. Two heaps keep the freed memory in use. And big blocks (a
    // song's buffer, its pictures) go back to the system as soon as they're freed: glibc
    // otherwise raises that limit past them after the first one and keeps them in its
    // heaps. Before any thread starts; MALLOC_ARENA_MAX and MALLOC_MMAP_THRESHOLD_ still
    // set them.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: only changes allocator settings, while the process is single-threaded.
        if std::env::var_os("MALLOC_ARENA_MAX").is_none() {
            unsafe { libc::mallopt(libc::M_ARENA_MAX, 2) };
        }
        if std::env::var_os("MALLOC_MMAP_THRESHOLD_").is_none() {
            unsafe { libc::mallopt(libc::M_MMAP_THRESHOLD, 128 * 1024) };
        }
    }
    // The updater's helper, run from its copy in the updates folder.
    if update::apply_main() {
        return Ok(());
    }
    // A relaunch: let the copy that started this one finish exiting.
    update::wait_for_predecessor();
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
    settle_window_system();
    update::clean_portable();
    stream::init_dev_hooks();
    proxy::init(&data_dir());

    // Dev hook: YTM_BACKGROUND opens the window off-screen, unfocused and off the
    // taskbar, so automated test runs don't pop up over whatever the user is doing.
    let background = std::env::var_os("YTM_BACKGROUND").is_some();
    ui::init_titlebar(&data_dir());
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("YT Music")
        .with_app_id("ytm-rs")
        .with_inner_size(DEFAULT_SIZE)
        .with_min_inner_size(MIN_SIZE)
        // Windows, and Linux where the desktop draws none: the app draws its own title
        // bar (see `ui::titlebar`).
        .with_decorations(!ui::custom_titlebar())
        .with_icon(appicon::get(appicon::load(&data_dir())).icon_data().expect("bundled icons are valid PNGs"));
    if background {
        viewport = viewport.with_position([-4000.0, 0.0]).with_inner_size([1920.0, 1057.0]).with_active(false).with_taskbar(false);
    }
    let renderer = renderer::init(&data_dir());
    // Both, so each settles this run's choice.
    if backdrop::init(&data_dir(), renderer) | ui::init_pill(&data_dir(), renderer) {
        viewport = viewport.with_transparent(true);
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: match renderer {
            renderer::Renderer::OpenGl => eframe::Renderer::Glow,
            renderer::Renderer::Wgpu => eframe::Renderer::Wgpu,
        },
        persist_window: !background,
        // Settings live with the rest of the data: beside the portable exe, or a
        // YTM_DATA_DIR profile (e.g. a clean signed-out one for screenshots).
        persistence_path: Some(data_dir().join("app.ron")),
        // No MSAA/depth/stencil: egui doesn't need them and they cost VRAM.
        multisampling: 0,
        depth_buffer: 0,
        stencil_buffer: 0,
        // Dev hooks: YTM_FPS paces frames itself (see `ui::pace`), so vsync is off;
        // YTM_NO_VSYNC turns it off as a driver forcing it off would.
        glow_options: eframe::egui_glow::GlowConfiguration {
            vsync: std::env::var_os("YTM_FPS").is_none() && std::env::var_os("YTM_NO_VSYNC").is_none(),
            ..Default::default()
        },
        wgpu_options: renderer::wgpu_options(),
        ..Default::default()
    };

    let run = eframe::run_native(
        "ytm-rs",
        options,
        Box::new(|cc| {
            let ctx = cc.egui_ctx.clone();
            theme::install_style(&ctx);
            theme::install_fonts(&ctx, "inter", &[]);
            theme::warm_fonts(&ctx);

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
                if let Some(window) = cc.winit_window().cloned() {
                    fullscreen::set_drag(move || window.has_focus() && window.drag_window().is_ok());
                }
                ui::set_titlebar_hwnd(h);
                ui::set_mini_hwnd(h);
                backdrop::prepare(h);
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
            if let Some(rs) = &cc.wgpu_render_state {
                let info = rs.adapter.get_info();
                log::info!("wgpu: {} ({:?}, driver {} {})", info.name, info.backend, info.driver, info.driver_info);
            }
            // The animated background needs the GPU context; without it the UI falls back
            // to a flat gradient.
            let shader = std::env::var_os("YTM_NO_SHADER").is_none();
            if let Some(gl) = &cc.gl {
                shader::install_glass(gl);
            }
            let bg = match (&cc.gl, &cc.wgpu_render_state) {
                (Some(gl), _) if shader => shader::Background::new(gl).map_err(|e| log::warn!("background shader unavailable: {e:#}")).ok(),
                (_, Some(rs)) if shader => Some(shader::Background::new_wgpu(rs)),
                _ => None,
            };

            Ok(Box::new(ui::App::new(
                backend,
                audio,
                ui::Channels { resp_rx, audio_rx, media_rx },
                media,
                bg,
                cc.storage,
            )))
        }),
    );
    // A wgpu window that won't open would fail the same way at every launch: go back
    // to OpenGL for the next one.
    if run.is_err() && renderer == renderer::Renderer::Wgpu {
        log::error!("renderer: {} failed to start; switching back to OpenGL", renderer::api_name());
        let _ = renderer::save(&data_dir(), renderer::Renderer::OpenGl);
    }
    run
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
/// Whether the desktop session is Wayland, and whether this run's window is a Wayland
/// one. Not the same thing: with Close to tray on, the window is an X11 one there.
static WAYLAND: OnceLock<(bool, bool)> = OnceLock::new();

/// The desktop session is Wayland, whatever kind of window the app has: apps can't
/// watch for keys in the background there.
pub fn wayland_session() -> bool {
    WAYLAND.get().is_some_and(|w| w.0)
}

/// This run's window is a Wayland one (known before the window exists).
pub fn wayland_window() -> bool {
    WAYLAND.get().is_some_and(|w| w.1)
}

/// Whether to open an X11 window in a Wayland session: with Close to tray on. A Wayland
/// window can't hide itself or come back when asked, so there the tray's "Show" could
/// only ask for attention; an X11 window (through XWayland) does both.
fn x11_for_tray(wayland: bool, x11: bool, close_to_tray: bool, keep_wayland: bool) -> bool {
    wayland && x11 && close_to_tray && !keep_wayland
}

/// "Close to tray" as last saved: read from the settings file itself, since this is
/// decided before the window, and so the settings, exist.
fn close_to_tray_saved(app_ron: &str) -> bool {
    app_ron.split_whitespace().collect::<String>().contains("\"close_to_tray\":\"1\"")
}

/// Linux: note what the session is, and pick the window system for this run. Called
/// first thing, before any thread starts. Dev hook: `YTM_WAYLAND` keeps the Wayland window.
fn settle_window_system() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    let wayland = set("WAYLAND_DISPLAY") || std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v == "wayland");
    let tray = close_to_tray_saved(&std::fs::read_to_string(data_dir().join("app.ron")).unwrap_or_default());
    let x11 = x11_for_tray(wayland, set("DISPLAY"), tray, set("YTM_WAYLAND"));
    if x11 {
        log::info!("window: X11 (XWayland), so Close to tray can hide the window and bring it back");
        // SAFETY: nothing else is running yet (no thread has been started), so nothing
        // reads the environment while it changes. winit goes by XDG_SESSION_TYPE first;
        // GTK (the tray, the sign-in window) by GDK_BACKEND.
        unsafe {
            std::env::remove_var("WAYLAND_DISPLAY");
            std::env::remove_var("WAYLAND_SOCKET");
            std::env::set_var("XDG_SESSION_TYPE", "x11");
            std::env::set_var("GDK_BACKEND", "x11");
        }
    }
    let _ = WAYLAND.set((wayland, wayland && !x11));
}

fn data_dir() -> std::path::PathBuf {
    if let Some(d) = std::env::var_os("YTM_DATA_DIR") {
        return d.into();
    }
    if let Some(dir) = update::portable_dir() {
        return dir.join("data");
    }
    directories::ProjectDirs::from("", "", "ytm-rs").expect("no home directory").data_dir().to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::{close_to_tray_saved, x11_for_tray};

    #[test]
    fn close_to_tray_opens_an_x11_window_on_wayland() {
        assert!(x11_for_tray(true, true, true, false));
        // Not without the setting, without XWayland, off Wayland, or when asked not to.
        assert!(!x11_for_tray(true, true, false, false));
        assert!(!x11_for_tray(true, false, true, false));
        assert!(!x11_for_tray(false, true, true, false));
        assert!(!x11_for_tray(true, true, true, true));
    }

    #[test]
    fn the_saved_setting_is_read_from_the_settings_file() {
        assert!(close_to_tray_saved("{\n    \"play_songs\": \"1\",\n    \"close_to_tray\": \"1\",\n}"));
        assert!(!close_to_tray_saved("{\n    \"close_to_tray\": \"0\",\n}"));
        assert!(!close_to_tray_saved(""));
    }
}
