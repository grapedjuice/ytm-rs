//! Dev hooks: test screenshots, the theme tour, frame pacing and `YTM_*` variables.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use super::App;

impl App {
    /// Dev hook: `YTM_THEME_TOUR=<secs>` steps through every theme preset from
    /// `YTM_SCREENSHOT_AT`, saving `<YTM_SCREENSHOT stem>_<id>.png` for each, then exits.
    pub(super) fn theme_tour(&mut self, ctx: &egui::Context, now: f64) {
        let Some(path) = dev_var("YTM_SCREENSHOT").map(std::path::PathBuf::from) else { return };
        let at: f64 = dev_var("YTM_SCREENSHOT_AT").and_then(|v| v.parse().ok()).unwrap_or(8.0);
        let step: f64 = dev_var("YTM_THEME_TOUR").and_then(|v| v.parse().ok()).unwrap_or(3.0);
        // YTM_TOUR_ONLY=id,id limits the tour to those presets.
        let only = dev_var("YTM_TOUR_ONLY").unwrap_or_default();
        let all: Vec<&crate::preset::Preset> = crate::preset::all().iter().filter(|p| only.is_empty() || only.split(',').any(|o| o == p.id)).collect();
        ctx.request_repaint_after(Duration::from_millis(100));
        if now < at {
            return;
        }
        let n = ((now - at) / step) as usize;
        if n >= all.len() {
            // Let the last screenshot land, then quit.
            if now - at > step * all.len() as f64 + 1.5 {
                self.quit(ctx);
            }
        } else if self.theme_id != all[n].id {
            self.theme_id = all[n].id.clone();
            self.default_theme = false;
        }
        let shot_id = egui::Id::unique("tour-shot");
        let requested: usize = ctx.data(|d| d.get_temp(shot_id)).unwrap_or(usize::MAX);
        let in_slot = (now - at) - n as f64 * step;
        if n < all.len() && requested != n && in_slot > step * 0.8 {
            ctx.data_mut(|d| d.insert_temp(shot_id, n));
            let id = all[n].id.clone();
            let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let out = path.with_file_name(format!("{stem}_{n:02}_{id}.png"));
            ctx.request_screenshot(move |img| {
                let [w, h] = img.size;
                let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                match image::RgbaImage::from_raw(w as u32, h as u32, bytes).map(|im| im.save(&out)) {
                    Some(Ok(())) => log::info!("tour screenshot saved: {id}"),
                    other => log::warn!("tour screenshot failed: {other:?}"),
                }
            });
        }
    }
}

/// Dev hook: `YTM_SCREENSHOT=out.png` saves the app's own framebuffer after
/// `YTM_SCREENSHOT_AT` seconds (default 8), independent of OS window capture.
/// `YTM_SCREENSHOT_BURST=<n>` instead keeps the next n frames from `YTM_SCREENSHOT_AT`
/// (in memory, so saving doesn't slow them) and then writes `<stem>_<i>_<ms>.png`,
/// `ms` being each frame's time: a filmstrip of an animation and its frame pacing.
pub(super) fn dev_screenshot(ctx: &egui::Context, now: f64) {
    let Some(path) = dev_var("YTM_SCREENSHOT") else { return };
    let at: f64 = dev_var("YTM_SCREENSHOT_AT").and_then(|v| v.parse().ok()).unwrap_or(8.0);
    if let Some(n) = dev_var("YTM_SCREENSHOT_BURST").and_then(|v| v.parse::<usize>().ok()) {
        static FRAMES: std::sync::Mutex<Vec<(f64, Arc<egui::ColorImage>)>> = std::sync::Mutex::new(Vec::new());
        static REQUESTED: AtomicUsize = AtomicUsize::new(0);
        static DONE: AtomicBool = AtomicBool::new(false);
        if DONE.load(Ordering::Relaxed) {
            return;
        }
        if now >= at && REQUESTED.load(Ordering::Relaxed) < n {
            REQUESTED.fetch_add(1, Ordering::Relaxed);
            let c = ctx.clone();
            ctx.request_screenshot(move |img| {
                FRAMES.lock().unwrap().push((now, img));
                c.request_repaint();
            });
            ctx.request_repaint();
        } else if now < at {
            ctx.request_repaint_after(Duration::from_secs_f64(at - now));
        } else if FRAMES.lock().unwrap().len() == n {
            let path = std::path::PathBuf::from(path);
            let shots = std::mem::take(&mut *FRAMES.lock().unwrap());
            DONE.store(true, Ordering::Relaxed);
            std::thread::spawn(move || {
                let t0 = shots[0].0;
                for (i, (t, img)) in shots.iter().enumerate() {
                    let [w, h] = img.size;
                    let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                    let out = path.with_file_name(format!("{}_{i:03}_{:04}.png", path.file_stem().unwrap_or_default().to_string_lossy(), ((t - t0) * 1000.0) as u32));
                    if let Some(img) = image::RgbaImage::from_raw(w as u32, h as u32, bytes) {
                        let _ = img.save(out);
                    }
                }
                log::info!("screenshot saved ({} frames)", shots.len());
            });
        }
        return;
    }
    let requested = egui::Id::unique("dev-shot-requested");
    if now >= at && !ctx.data(|d| d.get_temp::<bool>(requested).unwrap_or(false)) {
        ctx.data_mut(|d| d.insert_temp(requested, true));
        ctx.request_screenshot(move |img| {
            let [w, h] = img.size;
            let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            match image::RgbaImage::from_raw(w as u32, h as u32, bytes).map(|i| i.save(&path)) {
                Some(Ok(())) => log::info!("screenshot saved"),
                other => log::warn!("screenshot failed: {other:?}"),
            }
        });
    } else if now < at {
        ctx.request_repaint_after(Duration::from_secs_f64(at - now));
    }
}

/// A `YTM_*` dev hook, read once: several are checked every frame.
pub(super) fn dev_var(name: &str) -> Option<&'static str> {
    static VARS: std::sync::OnceLock<std::collections::HashMap<String, String>> = std::sync::OnceLock::new();
    VARS.get_or_init(|| {
        std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))).filter(|(k, _)| k.starts_with("YTM_")).collect()
    })
    .get(name)
    .map(String::as_str)
}

/// Dev hook: YTM_FPS=<hz> holds each frame to 1/<hz> s, as vsync does on a visible window
/// of that refresh rate (test windows sit off-screen, where Windows gives them ~21 fps);
/// YTM_FPS_LOG logs the rate reached every 5 s.
pub(super) fn dev_pace() {
    use std::sync::Mutex;
    use std::time::Instant;
    static HZ: std::sync::OnceLock<Option<f64>> = std::sync::OnceLock::new();
    static STATE: Mutex<Option<(Instant, Instant, u32)>> = Mutex::new(None);
    let Some(hz) = *HZ.get_or_init(|| std::env::var("YTM_FPS").ok().and_then(|v| v.parse().ok())) else { return };
    let period = std::time::Duration::from_secs_f64(1.0 / hz);
    let mut st = STATE.lock().unwrap();
    let now = Instant::now();
    let (last, window, frames) = st.get_or_insert((now, now, 0));
    let next = *last + period;
    if next > now {
        std::thread::sleep(next - now);
    }
    *last = Instant::now();
    *frames += 1;
    let span = last.duration_since(*window).as_secs_f64();
    if span >= 5.0 {
        if std::env::var_os("YTM_FPS_LOG").is_some() {
            log::info!("fps {:.1}", *frames as f64 / span);
        }
        *window = *last;
        *frames = 0;
    }
}
