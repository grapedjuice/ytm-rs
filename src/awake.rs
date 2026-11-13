//! Keeps the computer awake while something plays: no sleep while music plays, and no
//! screen blanking or lock either while a music video is on show (#111).
//!
//! A worker thread holds the request: on Linux it's a D-Bus call that can take a while,
//! and on Windows the request belongs to the thread that made it.

use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

/// How awake to keep the computer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Keep {
    Nothing,
    /// No sleep: music.
    System,
    /// No sleep, and the screen stays on and unlocked: a video.
    Display,
}

impl Keep {
    pub fn of(playing: bool, video: bool) -> Self {
        match (playing, video) {
            (false, _) => Self::Nothing,
            (true, false) => Self::System,
            (true, true) => Self::Display,
        }
    }
}

/// How long a step down waits before it's taken: between songs, or for a skip, the
/// player stops only for a moment.
const GRACE: Duration = Duration::from_secs(3);

/// What playing reads as in the OS's list of what keeps it awake.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const REASON: &str = "Playing music";

pub struct Awake {
    tx: Sender<Keep>,
    sent: Keep,
}

impl Awake {
    pub fn spawn() -> Option<Self> {
        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::Builder::new()
            .name("awake".into())
            .spawn(move || {
                // A unit struct only on some systems.
                #[allow(clippy::default_constructed_unit_structs)]
                let mut hold = os::Hold::default();
                run(&rx, GRACE, |keep| hold.set(keep));
            })
            .ok()?;
        Some(Self { tx, sent: Keep::Nothing })
    }

    /// Called every frame; only changes go to the worker.
    pub fn set(&mut self, keep: Keep) {
        if keep != self.sent {
            self.sent = keep;
            let _ = self.tx.send(keep);
        }
    }
}

/// Takes each wanted level as it comes, a step down only once it has lasted `grace`.
/// Ends when the app does, which lets go of whatever is held.
fn run(rx: &Receiver<Keep>, grace: Duration, mut apply: impl FnMut(Keep)) {
    let mut held = Keep::Nothing;
    while let Ok(mut want) = rx.recv() {
        while want < held {
            match rx.recv_timeout(grace) {
                Ok(k) => want = k,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        if want != held {
            log::debug!("keep awake: {want:?}");
            apply(want);
            held = want;
        }
    }
}

#[cfg(windows)]
mod os {
    use super::Keep;
    use windows::Win32::System::Power::{ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState};

    /// The thread's execution state, which Windows drops when the thread ends.
    #[derive(Default)]
    pub struct Hold;

    impl Hold {
        pub fn set(&mut self, keep: Keep) {
            let flags = match keep {
                Keep::Nothing => ES_CONTINUOUS,
                Keep::System => ES_CONTINUOUS | ES_SYSTEM_REQUIRED,
                Keep::Display => ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED,
            };
            if unsafe { SetThreadExecutionState(flags) }.0 == 0 {
                log::warn!("keep awake: SetThreadExecutionState failed");
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod os {
    use std::ffi::{CStr, c_char, c_void};

    use super::{Keep, REASON};

    type CFStringRef = *const c_void;
    const UTF8: u32 = 0x0800_0100;
    const ASSERTION_ON: u32 = 255;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(alloc: *const c_void, s: *const c_char, encoding: u32) -> CFStringRef;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(kind: CFStringRef, level: u32, name: CFStringRef, id: *mut u32) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
    }

    /// The power assertion held, if any.
    #[derive(Default)]
    pub struct Hold(Option<u32>);

    impl Hold {
        pub fn set(&mut self, keep: Keep) {
            let new = match keep {
                Keep::Nothing => None,
                Keep::System => assert(c"PreventUserIdleSystemSleep"),
                Keep::Display => assert(c"PreventUserIdleDisplaySleep"),
            };
            if let Some(id) = std::mem::replace(&mut self.0, new) {
                unsafe { IOPMAssertionRelease(id) };
            }
        }
    }

    impl Drop for Hold {
        fn drop(&mut self) {
            self.set(Keep::Nothing);
        }
    }

    fn assert(kind: &CStr) -> Option<u32> {
        let reason = std::ffi::CString::new(REASON).ok()?;
        unsafe {
            let kind = CFStringCreateWithCString(std::ptr::null(), kind.as_ptr(), UTF8);
            let name = CFStringCreateWithCString(std::ptr::null(), reason.as_ptr(), UTF8);
            let mut id = 0;
            let ok = !kind.is_null() && !name.is_null() && IOPMAssertionCreateWithName(kind, ASSERTION_ON, name, &mut id) == 0;
            for s in [kind, name].into_iter().filter(|s| !s.is_null()) {
                CFRelease(s);
            }
            if !ok {
                log::warn!("keep awake: IOPMAssertionCreateWithName failed");
            }
            ok.then_some(id)
        }
    }
}

/// The desktop portal's Inhibit, which works on Wayland and in a Flatpak. Without one:
/// logind's idle lock against sleep, and the screensaver's Inhibit for a video.
#[cfg(target_os = "linux")]
mod os {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    use dbus::Path;
    use dbus::arg::{OwnedFd, PropMap, Variant};
    use dbus::blocking::Connection;
    use dbus::message::MatchRule;

    use super::{Keep, REASON};

    const TIMEOUT: Duration = Duration::from_secs(5);
    const NO_ANSWER: u32 = u32::MAX;
    const PORTAL: &str = "org.freedesktop.portal.Desktop";
    const SCREENSAVER: &str = "org.freedesktop.ScreenSaver";

    /// The session bus stays open while anything is held: the portal and the screensaver
    /// let go of an app's requests when its connection closes.
    #[derive(Default)]
    pub struct Hold {
        bus: Option<Connection>,
        portal: Option<Path<'static>>,
        screensaver: Option<u32>,
        logind: Option<OwnedFd>,
    }

    impl Hold {
        /// The new request goes in before the old one goes, so nothing lapses between.
        pub fn set(&mut self, keep: Keep) {
            if self.bus.is_none() {
                self.bus = Connection::new_session().map_err(|e| log::warn!("keep awake: no session bus: {e}")).ok();
            }
            let (portal, screensaver) = (self.portal.take(), self.screensaver.take());
            let logind = self.logind.take();
            if keep != Keep::Nothing {
                self.take(keep);
            }
            let Some(bus) = &self.bus else { return };
            if let Some(handle) = portal {
                let _: Result<(), _> = bus.with_proxy(PORTAL, handle, TIMEOUT).method_call("org.freedesktop.portal.Request", "Close", ());
            }
            if let Some(cookie) = screensaver {
                let _: Result<(), _> = bus.with_proxy(SCREENSAVER, "/org/freedesktop/ScreenSaver", TIMEOUT).method_call(SCREENSAVER, "UnInhibit", (cookie,));
            }
            drop(logind);
        }

        fn take(&mut self, keep: Keep) {
            if let Some(bus) = &self.bus {
                match portal(bus, keep) {
                    Ok(handle) => {
                        self.portal = Some(handle);
                        return;
                    }
                    Err(e) => log::debug!("keep awake: no portal: {e}"),
                }
                if keep == Keep::Display {
                    let screensaver = bus.with_proxy(SCREENSAVER, "/org/freedesktop/ScreenSaver", TIMEOUT);
                    match screensaver.method_call(SCREENSAVER, "Inhibit", ("YT Music", REASON)) {
                        Ok((cookie,)) => self.screensaver = Some(cookie),
                        Err(e) => log::debug!("keep awake: no screensaver: {e}"),
                    }
                }
            }
            let logind = Connection::new_system().and_then(|c| {
                c.with_proxy("org.freedesktop.login1", "/org/freedesktop/login1", TIMEOUT)
                    .method_call("org.freedesktop.login1.Manager", "Inhibit", ("idle", "YT Music", REASON, "block"))
                    .map(|(fd,)| fd)
            });
            match logind {
                Ok(fd) => self.logind = Some(fd),
                Err(e) => log::warn!("keep awake: nothing to ask: {e}"),
            }
        }
    }

    /// Suspend, and idle (the screen blanking and locking) for a video. A desktop that
    /// can't do that says so only after the call, in a Response, which a success doesn't
    /// get: no answer in the time a call has is a yes. The request holds meanwhile, and
    /// only this thread waits.
    fn portal(bus: &Connection, keep: Keep) -> Result<Path<'static>, dbus::Error> {
        let flags: u32 = if keep == Keep::Display { 4 | 8 } else { 4 };
        let mut options = PropMap::new();
        options.insert("reason".into(), Variant(Box::new(REASON.to_owned())));
        let (handle,): (Path<'static>,) =
            bus.with_proxy(PORTAL, "/org/freedesktop/portal/desktop", TIMEOUT).method_call("org.freedesktop.portal.Inhibit", "Inhibit", ("", flags, options))?;
        let answer = Arc::new(AtomicU32::new(NO_ANSWER));
        let seen = answer.clone();
        let rule = MatchRule::new_signal("org.freedesktop.portal.Request", "Response").with_path(handle.clone());
        let token = bus.add_match(rule, move |(code,): (u32,), _, _| {
            seen.store(code, Ordering::Relaxed);
            true
        })?;
        let until = Instant::now() + TIMEOUT;
        while answer.load(Ordering::Relaxed) == NO_ANSWER
            && let Some(left) = until.checked_duration_since(Instant::now())
        {
            bus.process(left)?;
        }
        let _ = bus.remove_match(token);
        match answer.load(Ordering::Relaxed) {
            0 | NO_ANSWER => Ok(handle),
            code => {
                // A refused request stays on the portal until it's closed.
                let _: Result<(), _> = bus.with_proxy(PORTAL, handle, TIMEOUT).method_call("org.freedesktop.portal.Request", "Close", ());
                Err(dbus::Error::new_failed(&format!("refused ({code})")))
            }
        }
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod os {
    use super::Keep;

    #[derive(Default)]
    pub struct Hold;

    impl Hold {
        pub fn set(&mut self, _: Keep) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Waits well inside or well past the grace, so a slow or busy runner (macOS's
    // sleeps can overshoot by tens of milliseconds) doesn't change the outcome.
    const GRACE_MS: u64 = 200;
    const SHORT: u64 = 10;
    const LONG: u64 = 600;

    fn applied(sends: &[(Keep, u64)]) -> Vec<Keep> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let sends = sends.to_vec();
        let sender = std::thread::spawn(move || {
            for (k, wait) in sends {
                std::thread::sleep(Duration::from_millis(wait));
                tx.send(k).unwrap();
            }
            // Long enough for the last step down to be taken, then the app quits.
            std::thread::sleep(Duration::from_millis(LONG));
        });
        let mut got = vec![];
        run(&rx, Duration::from_millis(GRACE_MS), |k| got.push(k));
        sender.join().unwrap();
        got
    }

    #[test]
    fn playing_and_videos_keep_it_awake() {
        assert_eq!(Keep::of(false, true), Keep::Nothing);
        assert_eq!(Keep::of(true, false), Keep::System);
        assert_eq!(Keep::of(true, true), Keep::Display);
    }

    #[test]
    fn a_step_up_is_taken_at_once_and_a_step_down_once_it_lasts() {
        use Keep::*;
        assert_eq!(applied(&[(System, 0), (Display, 0), (System, LONG), (Nothing, LONG)]), [System, Display, System, Nothing]);
        // Between songs: the pause is over before the grace is.
        assert_eq!(applied(&[(System, 0), (Nothing, SHORT), (System, SHORT)]), [System]);
        // A video closed and opened again just as quickly.
        assert_eq!(applied(&[(Display, 0), (System, SHORT), (Display, SHORT), (Nothing, SHORT)]), [Display, Nothing]);
    }
}
