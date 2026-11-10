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

use std::sync::{Mutex, Once};

use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    Show,
    /// The menu's "Show"/"Hide" item: whichever it says.
    Toggle,
    Quit,
}

static PENDING: Mutex<Vec<Cmd>> = Mutex::new(Vec::new());

pub struct Tray {
    #[cfg(not(target_os = "linux"))]
    icon: Icon,
    /// What the menu's first item says now: true for "Hide".
    shown: bool,
}

/// The icon and its "Show"/"Hide" item, which says what clicking it would do.
struct Icon {
    _icon: TrayIcon,
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
                    push(Cmd::Show, &w);
                }
            }));
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| match e.id.as_ref() {
                "toggle" => push(Cmd::Toggle, &wake),
                "quit" => push(Cmd::Quit, &wake),
                _ => {}
            }));
        });

        let png = icon.rgba(32).ok_or_else(|| anyhow::anyhow!("icon doesn't decode"))?;
        #[cfg(not(target_os = "linux"))]
        return Ok(Self { icon: build(png)?, shown: false });
        #[cfg(target_os = "linux")]
        {
            gtk_thread::show(png)?;
            Ok(Self { shown: false })
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
}

impl Icon {
    fn set_shown(&self, shown: bool) {
        self.toggle.set_text(if shown { "Hide YT Music" } else { "Show YT Music" });
    }
}

#[cfg(target_os = "linux")]
impl Drop for Tray {
    fn drop(&mut self) {
        gtk_thread::hide();
    }
}

fn build(png: image::RgbaImage) -> anyhow::Result<Icon> {
    let toggle = MenuItem::with_id("toggle", "Show YT Music", true, None);
    let menu = Menu::with_items(&[&toggle, &MenuItem::with_id("quit", "Quit", true, None)])?;
    let (w, h) = png.dimensions();
    let icon = TrayIconBuilder::new()
        .with_icon(tray_icon::Icon::from_rgba(png.into_raw(), w, h)?)
        .with_tooltip("YT Music")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()?;
    Ok(Icon { _icon: icon, toggle })
}

#[cfg(target_os = "linux")]
mod gtk_thread {
    use std::cell::RefCell;
    use std::sync::OnceLock;
    use std::time::Duration;

    use super::Icon;

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
            .map_err(|e| anyhow::anyhow!("GTK: {e}"))
    }

    pub fn show(png: image::RgbaImage) -> anyhow::Result<()> {
        start()?;
        let (tx, rx) = std::sync::mpsc::channel();
        gtk::glib::MainContext::default().invoke(move || {
            let res = super::build(png).map(|icon| ICON.with(|i| *i.borrow_mut() = Some(icon)));
            let _ = tx.send(res.map_err(|e| e.to_string()));
        });
        rx.recv_timeout(Duration::from_secs(5))?.map_err(|e| anyhow::anyhow!(e))
    }

    pub fn set_shown(shown: bool) {
        gtk::glib::MainContext::default().invoke(move || {
            ICON.with(|i| {
                if let Some(i) = &*i.borrow() {
                    i.set_shown(shown);
                }
            })
        });
    }

    pub fn hide() {
        gtk::glib::MainContext::default().invoke(|| ICON.with(|i| drop(i.borrow_mut().take())));
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
