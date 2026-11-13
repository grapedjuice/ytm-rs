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
            self.art_key = None;
            self.request_art();
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

/// Dev hook: `YTM_SCRIPT="<secs>:<step>;…"` drives a test window that has no mouse or
/// keyboard, with seconds from the first frame. Steps: `m:x,y` moves the pointer, `c:x,y`
/// clicks, `r:w,h` resizes, `t:text` types, `k:esc` presses Esc (`k:f` F, and so on), `w:n` scrolls n lines (down), `f:n` flicks a touchpad n points (down) over 0.1 s, as Wayland sends it, `s:name` saves `<YTM_SCRIPT_DIR>/name.png`, `q` quits.
pub(super) fn dev_script(ctx: &egui::Context, raw: &mut egui::RawInput) {
    use std::sync::OnceLock;
    use std::time::Instant;
    enum Step {
        Move(egui::Pos2),
        Press(egui::Pos2, bool),
        Resize(egui::Pos2),
        Text(&'static str),
        Key(egui::Key),
        Wheel(f32),
        Swipe(egui::TouchPhase, f32),
        Shot(&'static str),
        Quit,
    }
    let Some(script) = dev_var("YTM_SCRIPT") else { return };
    static START: OnceLock<Instant> = OnceLock::new();
    static STEPS: OnceLock<Vec<(f64, Step)>> = OnceLock::new();
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let steps = STEPS.get_or_init(|| {
        let point = |v: &str| v.split_once(',').and_then(|(x, y)| Some(egui::pos2(x.parse().ok()?, y.parse().ok()?)));
        let mut steps = Vec::new();
        for step in script.split(';') {
            let mut parts = step.splitn(3, ':');
            let (Some(at), Some(kind)) = (parts.next().and_then(|t| t.trim().parse::<f64>().ok()), parts.next()) else { continue };
            let value = parts.next().unwrap_or("");
            match (kind, point(value)) {
                ("m", Some(p)) => steps.push((at, Step::Move(p))),
                ("c", Some(p)) => steps.extend([(at, Step::Move(p)), (at + 0.2, Step::Press(p, true)), (at + 0.3, Step::Press(p, false))]),
                ("r", Some(p)) => steps.push((at, Step::Resize(p))),
                ("t", _) => steps.push((at, Step::Text(value))),
                ("k", _) if egui::Key::from_name(&value.to_uppercase()).is_some() || value == "esc" => {
                    let key = if value == "esc" { egui::Key::Escape } else { egui::Key::from_name(&value.to_uppercase()).unwrap_or(egui::Key::Escape) };
                    steps.push((at, Step::Key(key)));
                }
                ("w", _) if value.parse::<f32>().is_ok() => steps.push((at, Step::Wheel(value.parse().unwrap_or(0.0)))),
                ("f", _) if value.parse::<f32>().is_ok() => {
                    // 6 moves 20 ms apart, then the fingers lift (Wayland's axis stop).
                    let n: f32 = value.parse().unwrap_or(0.0);
                    steps.push((at, Step::Swipe(egui::TouchPhase::Start, n / 6.0)));
                    steps.extend((1..6).map(|i| (at + 0.02 * f64::from(i), Step::Swipe(egui::TouchPhase::Move, n / 6.0))));
                    steps.push((at + 0.11, Step::Swipe(egui::TouchPhase::End, 0.0)));
                }
                ("s", _) => steps.push((at, Step::Shot(value))),
                ("q", _) => steps.push((at, Step::Quit)),
                _ => log::warn!("YTM_SCRIPT: can't read {step}"),
            }
        }
        steps.sort_by(|a, b| a.0.total_cmp(&b.0));
        steps
    });
    let now = START.get_or_init(Instant::now).elapsed().as_secs_f64();
    let mut next = NEXT.load(Ordering::Relaxed);
    while let Some((_, step)) = steps.get(next).filter(|(at, _)| *at <= now) {
        next += 1;
        match step {
            Step::Move(p) => raw.events.push(egui::Event::PointerMoved(*p)),
            Step::Press(p, pressed) => raw.events.push(egui::Event::PointerButton { pos: *p, button: egui::PointerButton::Primary, pressed: *pressed, modifiers: egui::Modifiers::NONE }),
            Step::Resize(p) => ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(p.to_vec2())),
            Step::Text(text) => raw.events.push(egui::Event::Text((*text).to_owned())),
            // Positive scrolls down, in lines.
            Step::Wheel(n) => raw.events.push(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta: egui::vec2(0.0, -*n), phase: egui::TouchPhase::Move, modifiers: egui::Modifiers::NONE, source: egui::MouseWheelSource::Wheel }),
            Step::Swipe(phase, n) => raw.events.push(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, -*n), phase: *phase, modifiers: egui::Modifiers::NONE, source: egui::MouseWheelSource::Unknown }),
            Step::Key(key) => {
                for pressed in [true, false] {
                    raw.events.push(egui::Event::Key { key: *key, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE });
                }
            }
            Step::Shot(name) => {
                let path = std::path::Path::new(dev_var("YTM_SCRIPT_DIR").unwrap_or(".")).join(format!("{name}.png"));
                ctx.request_screenshot(move |img| {
                    let [w, h] = img.size;
                    let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                    if let Some(Err(e)) = image::RgbaImage::from_raw(w as u32, h as u32, bytes).map(|i| i.save(&path)) {
                        log::warn!("screenshot failed: {e}");
                    }
                });
            }
            Step::Quit => std::process::exit(0),
        }
    }
    NEXT.store(next, Ordering::Relaxed);
    // A repaint asked for here wouldn't last the frame: an idle window has to be kept going.
    static TICKING: AtomicBool = AtomicBool::new(false);
    if !TICKING.swap(true, Ordering::Relaxed) {
        let ctx = ctx.clone();
        // Until the script ends: a test of what happens after it shouldn't see these frames.
        std::thread::spawn(move || {
            while NEXT.load(Ordering::Relaxed) < STEPS.get().map_or(usize::MAX, Vec::len) {
                ctx.request_repaint();
                std::thread::sleep(Duration::from_millis(30));
            }
            ctx.request_repaint();
        });
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

/// Dev hook: `YTM_PROBE=1` logs, each time the pointer stops somewhere new (`YTM_SCRIPT`'s
/// `m:x,y`), whether a press there would go to the title bar (move the window, maximize on
/// a double-click), and how many widgets are under it.
pub(super) fn dev_probe(ctx: &egui::Context) {
    use std::sync::Mutex;
    if dev_var("YTM_PROBE").is_none() {
        return;
    }
    static LAST: Mutex<Option<egui::Pos2>> = Mutex::new(None);
    let Some(p) = ctx.input(|i| i.pointer.hover_pos()) else { return };
    {
        let mut last = LAST.lock().unwrap();
        if *last == Some(p) {
            return;
        }
        *last = Some(p);
    }
    let (hovered, under) = ctx.viewport(|v| (v.interact_widgets.hovered.clone(), v.interact_widgets.contains_pointer.len()));
    let names = ["main", "main-over", "band-back", "capsule-back", "now-playing", "queue-float"];
    let title = names.iter().any(|n| hovered.contains(&egui::Id::unique(("titlebar-drag", *n)))) || hovered.contains(&egui::Id::unique("np-full-top"));
    log::info!("probe {:.0},{:.0}: title bar {}, {} hovered, {under} under", p.x, p.y, if title { "yes" } else { "NO" }, hovered.len());
}

/// Dev hook: `YTM_MEM=<secs>` logs, that often, what the app's own caches hold: GPU
/// textures (thumbnails and the rest), the font atlas, and the image loaders' bytes. The
/// process's own figures come from outside (Task Manager, `Get-Process`).
pub(super) fn dev_memory(ctx: &egui::Context, now: f64) {
    use std::sync::Mutex;
    let Some(every) = dev_var("YTM_MEM").and_then(|v| v.parse::<f64>().ok()) else { return };
    static LAST: Mutex<f64> = Mutex::new(f64::NEG_INFINITY);
    {
        let mut last = LAST.lock().unwrap();
        if now - *last < every {
            ctx.request_repaint_after(Duration::from_secs_f64(every - (now - *last)));
            return;
        }
        *last = now;
    }
    let mb = |b: usize| b as f64 / (1024.0 * 1024.0);
    let (count, textures, biggest) = {
        let tm = ctx.tex_manager();
        let tm = tm.read();
        let mut all: Vec<(usize, String)> = tm.allocated().map(|(_, m)| (m.bytes_used(), m.name.clone())).collect();
        all.sort_unstable_by_key(|a| std::cmp::Reverse(a.0));
        let total = all.iter().map(|a| a.0).sum::<usize>();
        let top: Vec<String> = all.iter().take(3).map(|(b, n)| format!("{} {:.1}", n.chars().take(48).collect::<String>(), mb(*b))).collect();
        (all.len(), total, top.join(", "))
    };
    let atlas = ctx.fonts(|f| f.font_image_size());
    let loaders = ctx.loaders();
    let bytes: usize = loaders.bytes.lock().iter().map(|l| l.byte_size()).sum();
    let images: usize = loaders.image.lock().iter().map(|l| l.byte_size()).sum();
    let tex_loaders: usize = loaders.texture.lock().iter().map(|l| l.byte_size()).sum();
    log::info!(
        "mem: {count} textures {:.1} MB (loader {:.1}); font atlas {}x{}; bytes {:.1} MB, decoded {:.1} MB; {} audio buffers [{biggest}]",
        mb(textures),
        mb(tex_loaders),
        atlas[0],
        atlas[1],
        mb(bytes),
        mb(images),
        crate::stream::LIVE.load(std::sync::atomic::Ordering::Relaxed)
    );
}
