//! Frame pacing: no more frames than the screen can show.
//!
//! Vsync holds frames to the refresh rate, unless the graphics driver is set to force it
//! off (NVIDIA's "Vertical sync: Off"): then every input event is a frame, ~1000 fps
//! with a 1000 Hz mouse (issue #71).

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static STATE: Mutex<Option<Pace>> = Mutex::new(None);

struct Pace {
    last: Instant,
    gap: Option<Duration>,
    gap_checked: Instant,
    window: Instant,
    frames: u32,
    worst: Duration,
}

/// Waits out the rest of the gap since the last frame. The gap is a little under one
/// refresh, so a working vsync sets the pace and this never waits. Only where the app
/// can ask the refresh rate (Windows); elsewhere a guess could hold back a faster screen.
///
/// eframe has no hook after a frame is shown, so the wait comes after input is read:
/// input that arrives meanwhile shows a frame later, about what vsync costs anyway.
///
/// Dev hooks: YTM_FPS=<hz> holds each frame to exactly 1/<hz> s instead, as vsync does
/// on a visible window of that refresh rate (test windows sit off-screen, where Windows
/// gives them ~21 fps); YTM_FPS_LOG logs the rate reached every 5 s.
pub(super) fn frame(ctx: &egui::Context) {
    // A second pass in the same frame is no new frame.
    if ctx.current_pass_index() > 0 {
        return;
    }
    static DEV_HZ: OnceLock<Option<f64>> = OnceLock::new();
    let dev_hz = *DEV_HZ.get_or_init(|| std::env::var("YTM_FPS").ok().and_then(|v| v.parse().ok()));
    let gap = || match dev_hz {
        Some(hz) => Some(Duration::from_secs_f64(1.0 / hz)),
        None => crate::fullscreen::refresh_rate().map(|hz| Duration::from_secs_f64(0.9 / hz)),
    };
    let mut st = STATE.lock().unwrap();
    let now = Instant::now();
    let p = st.get_or_insert_with(|| Pace { last: now, gap: gap(), gap_checked: now, window: now, frames: 0, worst: Duration::ZERO });
    // The window may have moved to a screen with another refresh rate.
    if now - p.gap_checked >= Duration::from_secs(1) {
        p.gap = gap();
        p.gap_checked = now;
    }
    if let Some(next) = p.gap.map(|g| p.last + g)
        && next > now
    {
        sleep(next - now);
    }
    let start = Instant::now();
    p.worst = p.worst.max(start - p.last);
    p.last = start;
    p.frames += 1;
    let span = p.last.duration_since(p.window).as_secs_f64();
    if span >= 5.0 {
        if std::env::var_os("YTM_FPS_LOG").is_some() {
            log::info!("fps {:.1}, longest frame {} ms", p.frames as f64 / span, p.worst.as_millis());
        }
        p.window = p.last;
        p.frames = 0;
        p.worst = Duration::ZERO;
    }
}

/// Asks for the next frame `gap` after this one began. A delay given to egui counts from
/// the moment it's asked, partway through the frame, so a whole frame's work and the
/// drawing would add to it and miss the screen's refresh.
pub(super) fn repaint_after(ctx: &egui::Context, gap: Duration) {
    let began = STATE.lock().unwrap().as_ref().map_or_else(Instant::now, |p| p.last);
    ctx.request_repaint_after(gap.saturating_sub(began.elapsed()));
}

/// Sleeps for `dur` without Windows' default 15.6 ms timer quantization, which would
/// otherwise turn a 1–3 ms frame-pacing wait on a high-refresh monitor into a ~60 fps cap.
#[cfg(windows)]
fn sleep(dur: Duration) {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Threading::{
        CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, INFINITE, SetWaitableTimer,
        TIMER_ALL_ACCESS, WaitForSingleObject,
    };
    use windows::core::PCWSTR;

    struct Timer(HANDLE);
    impl Drop for Timer {
        fn drop(&mut self) {
            // SAFETY: handle owned by this thread-local timer.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    thread_local! {
        static TIMER: Option<Timer> = unsafe {
            CreateWaitableTimerExW(None, PCWSTR::null(), CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS.0)
                .ok()
                .map(Timer)
        };
    }

    let waited = TIMER.with(|t| {
        let Some(timer) = t else { return false };
        let intervals = (dur.as_nanos() / 100).clamp(1, i64::MAX as u128) as i64;
        let due = -intervals;
        // SAFETY: `timer.0` is a valid waitable timer handle owned by this thread; `due` is valid for the call.
        unsafe {
            if SetWaitableTimer(timer.0, &due, 0, None, None, false).is_err() {
                return false;
            }
            WaitForSingleObject(timer.0, INFINITE);
        }
        true
    });
    if !waited {
        std::thread::sleep(dur);
    }
}

#[cfg(not(windows))]
fn sleep(dur: Duration) {
    std::thread::sleep(dur);
}
