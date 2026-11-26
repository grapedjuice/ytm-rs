#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// First: its tr! is used everywhere after.
#[macro_use]
mod i18n;
mod appicon;
mod fullscreen;
mod hls;
mod proxy;
mod renderer;
mod airplay;
mod art;
mod audio;
mod awake;
mod backdrop;
mod backend;
mod bidi;
mod cache;
mod canvas;
mod deeplink;
mod destination;
mod discord;
mod downloads;
mod hotkeys;
mod images;
mod jam;
mod innertube;
mod jsc;
mod lastfm;
mod scrobble_store;
#[cfg(test)]
mod scrobble_tests;
mod login;
mod link;
mod listenbrainz;
mod locale;
mod lyrics;
mod media;
mod model;
mod node;
mod notify;
mod opus;
mod preset;
mod recognize;
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
        if flag == login::FLAG || flag == login::SPOTIFY_FLAG || flag == turnstile::FLAG {
            // The parent hands its proxy setting down through the environment.
            proxy::init(std::path::Path::new(""));
            let language = args.next().and_then(|arg| arg.into_string().ok());
            i18n::init(language.as_deref());
        }
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
    trim_heap_periodically();
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
        // macOS: its frame, without its title strip; the traffic lights go on the app's
        // top row (see `ui::titlebar`).
        .with_fullsize_content_view(ui::titlebar_inset())
        .with_titlebar_shown(!ui::titlebar_inset())
        .with_title_shown(!ui::titlebar_inset())
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

    // Set once the window and GPU context are up: a GL error after that (a failed
    // `swap_buffers` an hour in) isn't one at start-up and shouldn't switch renderers.
    let started = std::cell::Cell::new(false);
    let run = eframe::run_native(
        "ytm-rs",
        options,
        Box::new(|cc| {
            started.set(true);
            // The fallback drew: keep it, as a wgpu failure keeps OpenGL, so later launches
            // don't fail on OpenGL first and Settings shows what's in use.
            if renderer::retrying() && cc.wgpu_render_state.is_some() {
                log::info!("renderer: {} works where OpenGL didn't; keeping it", renderer::api_name());
                let _ = renderer::save(&data_dir(), renderer::Renderer::Wgpu);
            }
            let language = cc.storage.and_then(|storage| storage.get_string("language")).filter(|l| !l.is_empty());
            // Set the locale before any background work can format its first message.
            i18n::init(language.as_deref());

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
            // Before the first frame, which then draws in the desktop's light or dark style.
            #[cfg(target_os = "linux")]
            theme::follow_system_scheme(wake);
            // Whether song notifications can show here (Windows: a Start menu entry).
            notify::look_up();
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

            let device = destination::Destination::parse(&cc.storage.and_then(|s| s.get_string("output_device")).unwrap_or_default());
            // Before the audio engine, which looks for picked network speakers if it may.
            destination::set_allowed(cc.storage.and_then(|s| s.get_string("local_network")).is_some_and(|v| v == "1"));

            let (audio_tx, audio_rx) = unbounded();
            let audio = audio::spawn(volume, log_volume, device, audio_tx, wake);

            let data_dir = data_dir();
            let (resp_tx, resp_rx) = unbounded();
            let backend = backend::Backend::new(data_dir, resp_tx, audio.clone(), Arc::new(wake))?;

            egui_extras::install_image_loaders(&ctx);
            ctx.add_bytes_loader(Arc::new(images::HttpImages::new(backend.clone())));

            // Which GL driver this is, for reports of drawing problems (Windows on ARM runs
            // GL through Mesa's D3D12 layer), and for the title bar's driver workaround.
            let gl_vendor = cc.gl.as_ref().map(|gl| {
                use eframe::glow::HasContext;
                // SAFETY: plain string queries on the current context.
                let (vendor, renderer, version) =
                    unsafe { (gl.get_parameter_string(eframe::glow::VENDOR), gl.get_parameter_string(eframe::glow::RENDERER), gl.get_parameter_string(eframe::glow::VERSION)) };
                log::info!("gl: {renderer} ({version}, {vendor})");
                vendor
            });

            let (media_tx, media_rx) = unbounded();
            if let Some(h) = hwnd(cc) {
                appicon::set_hwnd(h);
                fullscreen::set_hwnd(h);
                // Weak: a strong hold kept the window from closing, and eframe ends its loop
                // only once it has, so Quit from the tray left the process running (#153).
                if let Some(window) = cc.winit_window().map(Arc::downgrade) {
                    fullscreen::set_drag(move || window.upgrade().is_some_and(|w| w.has_focus() && w.drag_window().is_ok()));
                }
                ui::set_titlebar_hwnd(h, gl_vendor.as_deref());
                ui::set_mini_hwnd(h);
                backdrop::prepare(h);
                // eframe sets only the small icon; the taskbar button wants the big one.
                appicon::set_window_icon(appicon::get(appicon::load(backend.data_dir())));
            }
            #[cfg(target_os = "macos")]
            if let Ok(h) = raw_window_handle::HasWindowHandle::window_handle(cc)
                && let raw_window_handle::RawWindowHandle::AppKit(h) = h.as_raw()
            {
                ui::set_titlebar_view(h.ns_view.as_ptr());
            }
            let media = media::Media::new(hwnd(cc), media_tx, media::Direct::new(audio.clone()), wake);

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
                language,
            )))
        }),
    );
    if let Err(error) = &run {
        match renderer {
            renderer::Renderer::Wgpu if renderer::retrying() => {
                log::error!("renderer: {} fallback failed: {error}", renderer::api_name());
            }
            // A wgpu window that won't open would fail the same way at every launch: go
            // back to OpenGL for the next one.
            renderer::Renderer::Wgpu => {
                log::error!("renderer: {} failed to start; switching back to OpenGL", renderer::api_name());
                let _ = renderer::save(&data_dir(), renderer::Renderer::OpenGl);
            }
            // Start a fresh process: eframe's event loop and GPU state cannot be reset
            // after a failed OpenGL startup.
            renderer::Renderer::OpenGl if !started.get()
                && !renderer::retrying()
                && matches!(error, eframe::Error::Glutin(_) | eframe::Error::NoGlutinConfigs(..) | eframe::Error::OpenGL(_))
                && renderer::available() =>
            {
                log::warn!("renderer: OpenGL failed to start ({error}); trying {}", renderer::api_name());
                // Pass on this launch's arguments, such as a jam link waiting to be joined.
                let args = renderer::retry_args(std::env::args_os().skip(1));
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                if let Ok(exe) = std::env::current_exe()
                    && update::start_after_exit_with_args(&exe, &args)
                {
                    return Ok(());
                }
            }
            _ => {}
        }
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
    // Panics in the log too: the default hook only writes to stderr, which an app opened
    // from the menu doesn't have, and some panics end the app.
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("thread '{}' {info}", std::thread::current().name().unwrap_or("<unnamed>"));
        default(info);
    }));
    #[cfg(windows)]
    log_native_crashes();
}

/// A crash in native code (a graphics or audio driver, a C library) isn't a panic: Windows
/// ends the app, and only Event Viewer says why (#219). This puts the error and the DLL
/// it was in into the log first, then lets Windows carry on as before.
#[cfg(windows)]
fn log_native_crashes() {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::System::Diagnostics::Debug::{EXCEPTION_POINTERS, SetUnhandledExceptionFilter};
    use windows::Win32::System::LibraryLoader::{GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW, GetModuleHandleExW};
    unsafe extern "system" fn crashed(info: *const EXCEPTION_POINTERS) -> i32 {
        // SAFETY: Windows hands over valid pointers (or null); the module lookup only reads.
        unsafe {
            if let Some(rec) = info.as_ref().and_then(|i| i.ExceptionRecord.as_ref()) {
                let at = rec.ExceptionAddress as usize;
                let mut module = HMODULE::default();
                let flags = GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
                let (dll, offset) = if GetModuleHandleExW(flags, windows::core::PCWSTR(at as *const u16), &mut module).is_ok() {
                    let mut name = [0u16; 260];
                    let n = GetModuleFileNameW(Some(module), &mut name) as usize;
                    (String::from_utf16_lossy(&name[..n]), at - module.0 as usize)
                } else {
                    ("unknown module".to_owned(), at)
                };
                log::error!("crash: exception {:#010x} in {dll} at +{offset:#x}", rec.ExceptionCode.0 as u32);
                log::logger().flush();
            }
        }
        // EXCEPTION_CONTINUE_SEARCH: Windows ends the app and records it, as without this.
        0
    }
    // SAFETY: a plain function pointer, set once at startup.
    unsafe { SetUnhandledExceptionFilter(Some(crashed)) };
}

/// Settings, caches and the sign-in: `data` beside the portable build's exe, else the
/// user's app data. Dev hook: YTM_DATA_DIR runs on a separate profile.
/// Whether the desktop session and this run's window use Wayland. The window can use
/// XWayland for desktop features that need X11 window controls.
static WAYLAND: OnceLock<(bool, bool)> = OnceLock::new();

/// The desktop session is Wayland, whatever kind of window the app has: apps can't
/// watch for keys in the background there.
pub fn wayland_session() -> bool {
    WAYLAND.get().is_some_and(|w| w.0)
}

/// Milliseconds since 1970, by the system clock (0 should it be set before then).
pub fn unix_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// Seconds since 1970, by the system clock.
pub fn unix_secs() -> u64 {
    unix_ms() / 1000
}

/// This run's window is a Wayland one (known before the window exists).
pub fn wayland_window() -> bool {
    WAYLAND.get().is_some_and(|w| w.1)
}

/// Whether to open an X11 window in a Wayland session: with Close to tray or "Keep on top
/// on Wayland" on. A Wayland window can't hide itself, come back when asked or stay above
/// others (winit ignores the window level there); an X11 window (through XWayland) can.
fn use_x11(wayland: bool, x11: bool, wanted: bool, keep_wayland: bool) -> bool {
    wayland && x11 && wanted && !keep_wayland
}

/// Whether the switch `key` was last saved on: read from the settings file itself, since
/// this is decided before the window, and so the settings, exist.
fn saved_on(app_ron: &str, key: &str) -> bool {
    app_ron.split_whitespace().collect::<String>().contains(&format!("\"{key}\":\"1\""))
}

/// Linux: every few minutes, give the system back the memory glibc keeps after it's freed
/// (#107). Two heaps and the early release of big blocks (`main`) leave the small pieces:
/// glibc only shrinks a heap from its end, so a freed page between live ones stays with
/// the app until a trim. A trim is glibc walking its heaps, on its own thread.
fn trim_heap_periodically() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    std::thread::Builder::new()
        .name("heap-trim".into())
        .spawn(|| loop {
            std::thread::sleep(std::time::Duration::from_secs(180));
            // SAFETY: takes no pointers; glibc locks each heap while it trims it.
            unsafe { libc::malloc_trim(0) };
        })
        .ok();
}

/// Linux: note what the session is, and pick the window system for this run. Called
/// first thing, before any thread starts. Dev hook: `YTM_WAYLAND` keeps the Wayland window.
fn settle_window_system() {
    if !cfg!(target_os = "linux") {
        return;
    }
    // A restart starts the new copy with this run's environment, switch included: undo it
    // first, so the new run still knows the session is Wayland and decides afresh.
    if let Some(d) = std::env::var_os("YTM_WAYLAND_DISPLAY") {
        // SAFETY: as below, nothing else is running yet.
        unsafe {
            std::env::set_var("WAYLAND_DISPLAY", d);
            std::env::set_var("XDG_SESSION_TYPE", "wayland");
            std::env::remove_var("GDK_BACKEND");
            std::env::remove_var("YTM_WAYLAND_DISPLAY");
        }
    }
    let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    let wayland = set("WAYLAND_DISPLAY") || std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v == "wayland");
    let saved = std::fs::read_to_string(data_dir().join("app.ron")).unwrap_or_default();
    let wanted = saved_on(&saved, "close_to_tray") || saved_on(&saved, "mini_x11");
    let x11 = use_x11(wayland, set("DISPLAY"), wanted, set("YTM_WAYLAND"));
    if x11 {
        log::info!("window: X11 (XWayland), for Close to tray or Keep on top on Wayland");
        // SAFETY: nothing else is running yet (no thread has been started), so nothing
        // reads the environment while it changes. winit picks Wayland whenever
        // WAYLAND_DISPLAY is set; GTK (the tray, the sign-in window) goes by GDK_BACKEND.
        unsafe {
            if let Some(d) = std::env::var_os("WAYLAND_DISPLAY") {
                std::env::set_var("YTM_WAYLAND_DISPLAY", d);
            }
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
    use super::{saved_on, use_x11};

    #[test]
    fn an_x11_window_on_wayland_only_when_wanted() {
        assert!(use_x11(true, true, true, false));
        // Not without a setting asking for it, without XWayland, off Wayland, or when asked not to.
        assert!(!use_x11(true, true, false, false));
        assert!(!use_x11(true, false, true, false));
        assert!(!use_x11(false, true, true, false));
        assert!(!use_x11(true, true, true, true));
    }

    #[test]
    fn the_saved_settings_are_read_from_the_settings_file() {
        let ron = "{\n    \"play_songs\": \"1\",\n    \"close_to_tray\": \"1\",\n    \"mini_x11\": \"0\",\n}";
        assert!(saved_on(ron, "close_to_tray"));
        assert!(!saved_on(ron, "mini_x11"));
        assert!(saved_on("{\n    \"mini_x11\": \"1\",\n}", "mini_x11"));
        assert!(!saved_on("", "close_to_tray"));
    }
}
