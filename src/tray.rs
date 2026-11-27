//! System tray icon, shown while "Close to tray" is on.
//!
//! The icon must be created on the UI thread (it owns a Win32 message window), so
//! the app creates and drops it from `logic`. Clicks arrive on that thread's message
//! loop; they are queued here and the UI is woken to handle them. On macOS it is a
//! menu bar item, made the same way (AppKit wants it on the main thread, which is the
//! UI thread there).
//!
//! On Linux the icon is a GTK one, and GTK runs its own loop on one thread for the
//! life of the process (it can't be started twice); the icon is made and dropped there.
//!
//! The menu shows the song and has play/pause, next and previous, which do what the
//! media keys do, and Recognize song. A badge at the icon's corner says whether the song
//! plays or is paused.

use std::sync::{Mutex, Once};

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    /// A left click on the icon: brings the window back, or plays/pauses if the setting says
    /// so. A double-click is two of these (on Windows, its own event between them is ignored).
    Click,
    /// The menu's "Show"/"Hide" item: whichever it says.
    Toggle,
    PlayPause,
    Next,
    Prev,
    Recognize,
    Quit,
}

static PENDING: Mutex<Vec<Cmd>> = Mutex::new(Vec::new());

/// How soon a second click makes a double-click: the system's on Windows, macOS's default there.
pub fn double_click_time() -> std::time::Duration {
    #[cfg(windows)]
    // SAFETY: no arguments; it only reads a setting.
    return std::time::Duration::from_millis(unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() }.into());
    #[cfg(not(windows))]
    std::time::Duration::from_millis(500)
}

/// The tray can't work on this system (no AppIndicator library, or GTK won't start):
/// the setting turns itself off and says why under it.
#[derive(Debug)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct Unsupported(pub String);

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unsupported {}

pub struct Tray {
    #[cfg(not(target_os = "linux"))]
    icon: Icon,
    /// What the "Show"/"Hide" item says now: true for "Hide".
    shown: bool,
    /// The song the menu and tooltip show now, and whether it says "Pause".
    now: Now,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Now {
    /// "Title — Artist", or None when nothing's playing.
    song: Option<String>,
    playing: bool,
}

/// The icon and the menu items that change.
struct Icon {
    icon: TrayIcon,
    /// The app icon as it is, for each badge to be drawn on.
    base: image::RgbaImage,
    /// The badge on it now, so the picture is only replaced when that changes.
    badge: std::cell::Cell<Option<Badge>>,
    song: MenuItem,
    play: MenuItem,
    next: MenuItem,
    prev: MenuItem,
    toggle: MenuItem,
}

impl Tray {
    /// `wake` is called (from the UI thread's message loop) after each click.
    pub fn new(icon: &crate::appicon::Variant, wake: impl Fn() + Send + Sync + Clone + 'static) -> anyhow::Result<Self> {
        // The handlers are process-wide and can only be set once.
        static HANDLERS: Once = Once::new();
        HANDLERS.call_once(|| {
            let w = wake.clone();
            TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
                if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                    push(Cmd::Click, &w);
                }
            }));
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| match e.id.as_ref() {
                "play" => push(Cmd::PlayPause, &wake),
                "next" => push(Cmd::Next, &wake),
                "prev" => push(Cmd::Prev, &wake),
                "recognize" => push(Cmd::Recognize, &wake),
                "toggle" => push(Cmd::Toggle, &wake),
                "quit" => push(Cmd::Quit, &wake),
                _ => {}
            }));
        });

        let png = icon.rgba(32).ok_or_else(|| anyhow::anyhow!("icon doesn't decode"))?;
        #[cfg(not(target_os = "linux"))]
        return Ok(Self { icon: build(png)?, shown: false, now: Now::default() });
        #[cfg(target_os = "linux")]
        {
            gtk_thread::show(png)?;
            Ok(Self { shown: false, now: Now::default() })
        }
    }

    /// Say "Hide YT Music" while the window is on screen, else "Show YT Music".
    pub fn set_shown(&mut self, shown: bool) {
        if shown == self.shown {
            return;
        }
        self.shown = shown;
        #[cfg(not(target_os = "linux"))]
        self.icon.set_shown(shown);
        #[cfg(target_os = "linux")]
        gtk_thread::set_shown(shown);
    }

    /// The song (title and artists) in the menu and the tooltip, and "Pause" while it plays.
    pub fn set_now(&mut self, song: Option<(&str, &str)>, playing: bool) {
        let song = song.map(|(title, artist)| if artist.is_empty() { title.to_owned() } else { format!("{title} — {artist}") });
        let now = Now { song, playing };
        if now == self.now {
            return;
        }
        self.now = now.clone();
        #[cfg(not(target_os = "linux"))]
        self.icon.set_now(&now);
        #[cfg(target_os = "linux")]
        gtk_thread::set_now(now);
    }
}

impl Icon {
    fn set_shown(&self, shown: bool) {
        self.toggle.set_text(if shown { crate::tr!("window-tray-hide") } else { crate::tr!("window-tray-show") });
    }

    fn set_now(&self, now: &Now) {
        self.song.set_text(now.song.as_deref().map_or_else(|| crate::tr!("player-nothing-playing").to_owned(), menu_song));
        self.play.set_text(if now.playing { crate::tr!("ui-pause") } else { crate::tr!("ui-play") });
        // Nothing to play, skip or go back to.
        for item in [&self.play, &self.next, &self.prev] {
            item.set_enabled(now.song.is_some());
        }
        let _ = self.icon.set_tooltip(Some(tooltip(now.song.as_deref())));
        let badge = now.song.is_some().then_some(if now.playing { Badge::Playing } else { Badge::Paused });
        if self.badge.replace(badge) != badge {
            let img = badge.map_or_else(|| self.base.clone(), |b| badged(&self.base, b));
            let (w, h) = img.dimensions();
            if let Ok(icon) = tray_icon::Icon::from_rgba(img.into_raw(), w, h) {
                let _ = self.icon.set_icon(Some(icon));
            }
        }
    }
}

/// What the badge at the icon's corner shows: what the music is doing, as a play or a
/// pause sign. None while nothing's playing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Badge {
    Playing,
    Paused,
}

/// `base` with a badge in its bottom right corner: a dark disc a little under half its
/// size, with a white rim to stand out on a dark or a light panel, and the sign in white.
/// Each pixel is sampled 4×4 times, for smooth edges at a tray's few pixels.
fn badged(base: &image::RgbaImage, badge: Badge) -> image::RgbaImage {
    const N: u32 = 4;
    const DARK: f32 = 30.0;
    let mut img = base.clone();
    let size = img.width().min(img.height()) as f32;
    let r = size * 0.24;
    let rim = size / 32.0;
    let (cx, cy) = (img.width() as f32 - r - 0.5, img.height() as f32 - r - 0.5);
    // In units of the disc's radius, from its centre.
    let sign = |x: f32, y: f32| match badge {
        // Pointing right, from x -0.35 to its tip at 0.5.
        Badge::Playing => x >= -0.35 && y.abs() <= 0.45 * (0.5 - x) / 0.85,
        Badge::Paused => y.abs() <= 0.42 && ((-0.42..=-0.12).contains(&x) || (0.12..=0.42).contains(&x)),
    };
    let (x0, y0) = ((cx - r).floor().max(0.0) as u32, (cy - r).floor().max(0.0) as u32);
    for py in y0..img.height() {
        for px in x0..img.width() {
            // How much of the pixel the disc covers, and of that how much is white.
            let (mut disc, mut white) = (0.0, 0.0);
            for sy in 0..N {
                for sx in 0..N {
                    let x = px as f32 + (sx as f32 + 0.5) / N as f32 - cx;
                    let y = py as f32 + (sy as f32 + 0.5) / N as f32 - cy;
                    let d = (x * x + y * y).sqrt();
                    if d <= r {
                        disc += 1.0;
                        if d > r - rim || sign(x / r, y / r) {
                            white += 1.0;
                        }
                    }
                }
            }
            if disc == 0.0 {
                continue;
            }
            let n = (N * N) as f32;
            let (disc, white) = (disc / n, white / n);
            let p = img.get_pixel_mut(px, py);
            for c in p.0.iter_mut().take(3) {
                *c = (f32::from(*c) * (1.0 - disc) + DARK * (disc - white) + 255.0 * white).round() as u8;
            }
            p.0[3] = (f32::from(p.0[3]) * (1.0 - disc) + 255.0 * disc).round() as u8;
        }
    }
    img
}

/// "YT Music", with the song under it. Windows cuts a tooltip off at 127 characters
/// (UTF-16 ones), so a long one ends in "…" there instead.
fn tooltip(song: Option<&str>) -> String {
    let Some(song) = song else { return "YT Music".to_owned() };
    cut(&format!("YT Music\n{song}"), 127, |g| g.encode_utf16().count())
}

/// The song line at the top of the menu. The menu is as wide as its widest item, so a
/// long title would stretch it across the screen; past 60 letters it ends in "…".
fn menu_song(song: &str) -> String {
    cut(song, 60, |_| 1)
}

/// `s`, or as much of it as fits in `max` with a "…" after, where `len` measures each
/// letter. It's cut between letters as a reader sees them (an accent or an emoji made
/// of several characters stays whole).
fn cut(s: &str, max: usize, len: impl Fn(&str) -> usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    if s.graphemes(true).map(&len).sum::<usize>() <= max {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut used = len("…");
    for g in s.graphemes(true) {
        used += len(g);
        if used > max {
            break;
        }
        out.push_str(g);
    }
    // No space before the "…".
    out.truncate(out.trim_end().len());
    out.push('…');
    out
}

#[cfg(target_os = "linux")]
impl Drop for Tray {
    fn drop(&mut self) {
        gtk_thread::hide();
    }
}

fn build(png: image::RgbaImage) -> anyhow::Result<Icon> {
    let song = MenuItem::with_id("song", crate::tr!("player-nothing-playing"), false, None);
    let play = MenuItem::with_id("play", crate::tr!("ui-play"), false, None);
    let next = MenuItem::with_id("next", crate::tr!("ui-next"), false, None);
    let prev = MenuItem::with_id("prev", crate::tr!("ui-previous"), false, None);
    let toggle = MenuItem::with_id("toggle", crate::tr!("window-tray-show"), true, None);
    let menu = Menu::with_items(&[
        &song,
        &play,
        &next,
        &prev,
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id("recognize", crate::tr!("recognize-song"), true, None),
        &toggle,
        &MenuItem::with_id("quit", crate::tr!("window-quit"), true, None),
    ])?;
    let (w, h) = png.dimensions();
    #[allow(unused_mut)]
    let mut builder = TrayIconBuilder::new()
        .with_icon(tray_icon::Icon::from_rgba(png.clone().into_raw(), w, h)?)
        .with_tooltip("YT Music")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false);
    #[cfg(target_os = "linux")]
    if crate::update::flatpak()
        && let Some(dir) = flatpak_icon_dir(std::env::var_os("XDG_RUNTIME_DIR"), std::env::var_os("FLATPAK_ID"))
    {
        builder = builder.with_temp_dir_path(dir);
    }
    let icon = builder.build()?;
    Ok(Icon { icon, base: png, badge: std::cell::Cell::new(None), song, play, next, prev, toggle })
}

/// Where a Flatpak keeps the icon's picture. The tray, outside the sandbox, reads it from
/// a file, and by default that's in `$XDG_RUNTIME_DIR/tray-icon`, which the host can't
/// see unless the Flatpak is given it. The app's own `$XDG_RUNTIME_DIR/app/<its id>` is
/// shared with the host at the same path, so the icon shows with no extra permission and
/// doesn't share a folder with other apps' icons.
#[cfg(target_os = "linux")]
fn flatpak_icon_dir(runtime: Option<std::ffi::OsString>, id: Option<std::ffi::OsString>) -> Option<std::path::PathBuf> {
    let (runtime, id) = (runtime.filter(|r| !r.is_empty())?, id.filter(|i| !i.is_empty())?);
    Some(std::path::Path::new(&runtime).join("app").join(id).join("tray-icon"))
}

#[cfg(target_os = "linux")]
mod gtk_thread {
    use std::cell::RefCell;
    use std::panic::AssertUnwindSafe;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Duration;

    use super::{Icon, Unsupported};

    thread_local! {
        static ICON: RefCell<Option<Icon>> = const { RefCell::new(None) };
    }

    /// Start GTK's thread once; an error if GTK can't start (no display).
    fn start() -> anyhow::Result<()> {
        static STARTED: OnceLock<Result<(), String>> = OnceLock::new();
        STARTED
            .get_or_init(|| {
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::Builder::new()
                    .name("gtk".into())
                    .spawn(move || match gtk::init() {
                        Ok(()) => {
                            let _ = tx.send(Ok(()));
                            gtk::main();
                        }
                        Err(e) => {
                            let _ = tx.send(Err(e.to_string()));
                        }
                    })
                    .map_err(|e| e.to_string())?;
                rx.recv().map_err(|e| e.to_string())?
            })
            .clone()
            .map_err(|e| Unsupported(crate::tr!("settings-tray-no-gtk", error = e)).into())
    }

    /// The names libappindicator-sys 0.9.0 loads the AppIndicator library by, in its
    /// order (its `LIB`, with the `backcompat` names it has by default).
    const APPINDICATOR: [&std::ffi::CStr; 4] =
        [c"libayatana-appindicator3.so.1", c"libappindicator3.so.1", c"libayatana-appindicator3.so", c"libappindicator3.so"];

    /// Load the AppIndicator library before tray-icon does: libappindicator-sys panics if
    /// it can't, and that panic, on GTK's thread, would end the app. Loaded the same way
    /// (same names, order and flags, from the same executable), so its own load then finds
    /// this one, which stays loaded. A failure isn't remembered: installing the library
    /// and turning the setting on again tries again.
    fn load_appindicator() -> Result<(), String> {
        static LOADED: AtomicBool = AtomicBool::new(false);
        if LOADED.load(Ordering::Relaxed) {
            return Ok(());
        }
        let mut errors = Vec::new();
        for name in APPINDICATOR {
            // SAFETY: `name` is a C string. This is the library tray-icon loads anyway
            // (running its initializers); the handle is never closed.
            if !unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) }.is_null() {
                LOADED.store(true, Ordering::Relaxed);
                return Ok(());
            }
            // SAFETY: dlerror's message is a C string, valid until the next dl* call on this thread.
            let e = unsafe { libc::dlerror() };
            errors.push(if e.is_null() { name.to_string_lossy().into_owned() } else { unsafe { std::ffi::CStr::from_ptr(e) }.to_string_lossy().into_owned() });
        }
        Err(errors.join("; "))
    }

    /// Run `f` on GTK's thread. A panic can't unwind out of GLib's callback (it would end
    /// the app), so it stops here; the panic hook has logged it.
    fn invoke(f: impl FnOnce() + Send + 'static) {
        gtk::glib::MainContext::default().invoke(move || {
            let _ = std::panic::catch_unwind(AssertUnwindSafe(f));
        });
    }

    pub fn show(png: image::RgbaImage) -> anyhow::Result<()> {
        if let Err(e) = load_appindicator() {
            log::warn!("tray: no AppIndicator library: {e}");
            return Err(Unsupported(crate::tr!("settings-tray-no-appindicator").to_owned()).into());
        }
        start()?;
        let (tx, rx) = std::sync::mpsc::channel();
        invoke(move || {
            let res = super::build(png).map(|icon| ICON.with(|i| *i.borrow_mut() = Some(icon)));
            let _ = tx.send(res.map_err(|e| e.to_string()));
        });
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(res) => res.map_err(|e| anyhow::anyhow!(e)),
            Err(RecvTimeoutError::Timeout) => Err(anyhow::anyhow!("GTK didn't make the icon within 5 s")),
            Err(RecvTimeoutError::Disconnected) => Err(anyhow::anyhow!("making the icon panicked")),
        }
    }

    pub fn set_shown(shown: bool) {
        invoke(move || {
            ICON.with(|i| {
                if let Some(i) = &*i.borrow() {
                    i.set_shown(shown);
                }
            })
        });
    }

    pub fn set_now(now: super::Now) {
        invoke(move || {
            ICON.with(|i| {
                if let Some(i) = &*i.borrow() {
                    i.set_now(&now);
                }
            })
        });
    }

    pub fn hide() {
        invoke(|| ICON.with(|i| drop(i.borrow_mut().take())));
    }
}

fn push(cmd: Cmd, wake: &impl Fn()) {
    PENDING.lock().unwrap().push(cmd);
    wake();
}

/// Clicks since the last call.
pub fn take() -> Vec<Cmd> {
    std::mem::take(&mut *PENDING.lock().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn a_flatpak_keeps_its_icon_in_its_own_runtime_folder() {
        let dir = flatpak_icon_dir(Some("/run/user/1000".into()), Some("io.github.grapedjuice.ytm-rs".into()));
        assert_eq!(dir.as_deref(), Some(std::path::Path::new("/run/user/1000/app/io.github.grapedjuice.ytm-rs/tray-icon")));
        // Without either, the library's own default.
        assert!(flatpak_icon_dir(None, Some("x".into())).is_none());
        assert!(flatpak_icon_dir(Some("/run/user/1000".into()), Some("".into())).is_none());
    }

    #[test]
    fn tooltip_fits_windows() {
        assert_eq!(tooltip(None), "YT Music");
        assert_eq!(tooltip(Some("Song — Artist")), "YT Music\nSong — Artist");
        // Just fits: left as it is.
        let fits = "a".repeat(127 - 9);
        assert_eq!(tooltip(Some(&fits)), format!("YT Music\n{fits}"));
        assert_eq!(tooltip(Some(&"a".repeat(200))), format!("YT Music\n{}…", "a".repeat(117)));
        // Cut between characters, never inside one (an emoji is two UTF-16 ones).
        let emoji = tooltip(Some(&"😀".repeat(100)));
        assert_eq!(emoji, format!("YT Music\n{}…", "😀".repeat(58)));
        assert!(emoji.encode_utf16().count() <= 127);
    }

    #[test]
    fn menu_song_stays_narrow() {
        assert_eq!(menu_song("Song — Artist"), "Song — Artist");
        let fits = "a".repeat(60);
        assert_eq!(menu_song(&fits), fits);
        assert_eq!(menu_song(&"a".repeat(200)), format!("{}…", "a".repeat(59)));
        // An emoji made of several characters, and a letter with its accent apart, stay whole.
        let family = "👨‍👩‍👧";
        assert_eq!(menu_song(&family.repeat(100)), format!("{}…", family.repeat(59)));
        let e = "e\u{301}";
        assert_eq!(menu_song(&e.repeat(100)), format!("{}…", e.repeat(59)));
        // No space left hanging before the "…".
        assert_eq!(menu_song(&format!("{} b{}", "a".repeat(58), "c".repeat(50))), format!("{}…", "a".repeat(58)));
    }

    /// The badge sits in the bottom right corner, opaque there, and leaves the rest of the
    /// icon as it was; playing and paused look different.
    #[test]
    fn a_badge_marks_only_the_corner() {
        let base = image::RgbaImage::from_pixel(32, 32, image::Rgba([200, 0, 0, 128]));
        let (play, pause) = (badged(&base, Badge::Playing), badged(&base, Badge::Paused));
        for img in [&play, &pause] {
            assert_eq!(img.get_pixel(0, 0), base.get_pixel(0, 0), "top left untouched");
            assert_eq!(img.get_pixel(8, 24), base.get_pixel(8, 24), "left of the badge untouched");
            // Opaque at the disc's centre; its rim lighter than the dark disc inside it.
            assert_eq!(img.get_pixel(24, 24).0[3], 255);
            let (rim, inside) = (img.get_pixel(31, 24).0[1], img.get_pixel(24, 18).0[1]);
            assert!(inside < 60 && rim > inside + 40, "rim {rim}, inside {inside}");
        }
        assert_ne!(play, pause);
        // Where the pause sign has its gap, the play sign is white.
        assert!(pause.get_pixel(24, 24).0[0] < 100 && play.get_pixel(24, 24).0[0] > 150);
    }

    /// `gtk_thread::APPINDICATOR` is libappindicator-sys 0.9.0's list of names: another
    /// version must be read again before this is changed.
    #[test]
    fn appindicator_names_are_from_the_locked_version() {
        let lock: Vec<&str> = include_str!("../Cargo.lock").lines().map(str::trim).collect();
        let versions: Vec<&str> = lock.windows(2).filter(|w| w[0] == "name = \"libappindicator-sys\"").map(|w| w[1]).collect();
        assert_eq!(versions, ["version = \"0.9.0\""]);
    }
}
