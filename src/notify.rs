//! Song notifications: the song's title, artist and cover in a desktop notification when
//! a new one starts while the window is in the background (Settings › Window).
//!
//! A worker thread owns the system's notifier, so a slow notification service never
//! holds the UI up. Each notification replaces the last, so they don't pile up.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};

/// Windows' toasts and the freedesktop notifications Linux desktops show. Not macOS yet.
pub const SUPPORTED: bool = cfg!(any(windows, target_os = "linux"));

/// Notifications can show: on Windows, only for a copy with a Start menu entry (looked
/// up once, a moment's work: see `look_up`).
pub fn available() -> bool {
    #[cfg(windows)]
    return os::start_menu_id().is_some();
    #[cfg(not(windows))]
    SUPPORTED
}

/// Find the Start menu entry now, on a thread of its own, so `available` doesn't hold
/// the UI up later (it takes a few hundred ms).
pub fn look_up() {
    #[cfg(windows)]
    let _ = std::thread::Builder::new().name("notify-lookup".into()).spawn(|| {
        os::start_menu_id();
    });
}

#[derive(Clone, Debug, Default)]
pub struct Song {
    pub title: String,
    pub artist: String,
    /// The cover, downloaded to a file.
    pub cover: Option<PathBuf>,
}

#[derive(Clone)]
pub struct Notifier {
    tx: Sender<Song>,
    /// The song (by playback generation) to notify about: one still fetching its cover
    /// when the next starts is dropped.
    latest: Arc<AtomicU64>,
}

impl Notifier {
    pub fn spawn() -> Option<Self> {
        if !available() {
            return None;
        }
        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::Builder::new().name("notify".into()).spawn(move || run(rx)).ok()?;
        Some(Self { tx, latest: Arc::new(AtomicU64::new(u64::MAX)) })
    }

    /// Generation `generation` is the song to show next.
    pub fn expect(&self, generation: u64) {
        self.latest.store(generation, Ordering::Relaxed);
    }

    /// Show `song`, unless a newer one was expected since.
    pub fn show(&self, generation: u64, song: Song) {
        if self.latest.load(Ordering::Relaxed) == generation {
            let _ = self.tx.send(song);
        }
    }
}

fn run(rx: Receiver<Song>) {
    #[cfg(any(windows, target_os = "linux"))]
    let mut os = os::Os::default();
    while let Ok(song) = rx.recv() {
        // Skipping through songs: only the one that stays.
        std::thread::sleep(Duration::from_millis(300));
        let song = rx.try_iter().last().unwrap_or(song);
        #[cfg(any(windows, target_os = "linux"))]
        if let Err(e) = os.show(&song) {
            log::warn!("song notification: {e}");
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = song;
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Not allowed in XML at all.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

/// The toast's XML: silent, so no chime plays over the music.
#[cfg_attr(not(windows), allow(dead_code))]
fn toast_xml(song: &Song, launch: Option<&str>) -> String {
    let mut xml = String::from("<toast");
    if let Some(l) = launch {
        xml.push_str(&format!(" launch=\"{}\" activationType=\"protocol\"", xml_escape(l)));
    }
    xml.push_str("><visual><binding template=\"ToastGeneric\">");
    xml.push_str(&format!("<text hint-maxLines=\"1\">{}</text>", xml_escape(&song.title)));
    xml.push_str(&format!("<text>{}</text>", xml_escape(&song.artist)));
    if let Some(uri) = song.cover.as_deref().and_then(|p| url::Url::from_file_path(p).ok()) {
        xml.push_str(&format!("<image placement=\"appLogoOverride\" src=\"{}\"/>", xml_escape(uri.as_str())));
    }
    xml.push_str("</binding></visual><audio silent=\"true\"/></toast>");
    xml
}

#[cfg(windows)]
mod os {
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;

    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
    use windows::Win32::UI::Shell::{BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem, KF_FLAG_DEFAULT, SHGetKnownFolderItem, SHGetKnownFolderPath, SIGDN_PARENTRELATIVEPARSING};
    use windows::core::{GUID, HSTRING, PWSTR, h};

    use super::Song;

    /// Windows shows toasts only from an app in the Start menu, under the app ID it gives
    /// that entry: for a shortcut to this exe, its path (or a known folder and the rest of
    /// it). Others are kept but never shown. None: no shortcut points here (a portable copy).
    pub fn start_menu_id() -> Option<&'static str> {
        static ID: OnceLock<Option<String>> = OnceLock::new();
        ID.get_or_init(|| {
            let t = std::time::Instant::now();
            let id = std::env::current_exe().ok().and_then(|exe| find(&exe).inspect_err(|e| log::warn!("song notifications: {e}")).ok().flatten());
            log::info!("song notifications: start menu id {id:?} ({:?})", t.elapsed());
            id
        })
        .as_deref()
    }

    fn find(exe: &Path) -> windows::core::Result<Option<String>> {
        // SAFETY: COM for the shell items below; already set up on this thread is fine too.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let apps: IShellItem = unsafe { SHGetKnownFolderItem(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None) }?;
        let items: IEnumShellItems = unsafe { apps.BindToHandler(None, &BHID_EnumItems) }?;
        loop {
            let mut got = [None];
            let mut n = 0;
            if unsafe { items.Next(&mut got, Some(&mut n)) }.is_err() || n == 0 {
                return Ok(None);
            }
            let Some(item) = got[0].take() else { return Ok(None) };
            let Ok(name) = (unsafe { item.GetDisplayName(SIGDN_PARENTRELATIVEPARSING) }) else { continue };
            let id = take(name);
            if id.as_deref().and_then(path_of).is_some_and(|p| p.as_os_str().eq_ignore_ascii_case(exe.as_os_str())) {
                return Ok(id);
            }
        }
    }

    /// The file an app ID names: a path, or `{known folder}\rest`.
    fn path_of(id: &str) -> Option<PathBuf> {
        let Some(rest) = id.strip_prefix('{') else {
            return id.contains(":\\").then(|| PathBuf::from(id));
        };
        let (guid, rel) = rest.split_once("}\\")?;
        let guid = GUID::try_from(guid).ok()?;
        let base = take(unsafe { SHGetKnownFolderPath(&guid, KF_FLAG_DEFAULT, None) }.ok()?)?;
        Some(PathBuf::from(base).join(rel))
    }

    /// A string the shell allocated, freed.
    fn take(s: PWSTR) -> Option<String> {
        // SAFETY: a valid string from the shell, freed once here.
        let out = unsafe { s.to_string() }.ok();
        unsafe { CoTaskMemFree(Some(s.0 as *const _)) };
        out
    }

    pub struct Os {
        /// Clicking the toast brings the window back, through the `ytm-rs:` link the
        /// installed copy registers (a second launch hands over to the running one).
        launch: Option<&'static str>,
    }

    impl Default for Os {
        fn default() -> Self {
            let linked = crate::update::installed() && !cfg!(debug_assertions);
            Self { launch: linked.then_some("ytm-rs://show") }
        }
    }

    impl Os {
        pub fn show(&mut self, song: &Song) -> windows::core::Result<()> {
            let Some(id) = start_menu_id() else { return Ok(()) };
            let doc = XmlDocument::new()?;
            doc.LoadXml(&HSTRING::from(super::toast_xml(song, self.launch)))?;
            let toast = ToastNotification::CreateToastNotification(&doc)?;
            // One at a time: each replaces the last, in the notification list too.
            toast.SetTag(h!("song"))?;
            toast.SetGroup(h!("now-playing"))?;
            ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(id))?.Show(&toast)
        }
    }
}

#[cfg(target_os = "linux")]
mod os {
    use std::time::Duration;

    use dbus::arg::{PropMap, Variant};
    use dbus::blocking::Connection;

    use super::Song;

    const DEST: &str = "org.freedesktop.Notifications";
    const PATH: &str = "/org/freedesktop/Notifications";

    #[derive(Default)]
    pub struct Os {
        bus: Option<Connection>,
        /// The last notification's id, for the next to replace it.
        last: u32,
        /// The server reads markup in the body, so `&` and `<` there are escaped.
        markup: bool,
    }

    impl Os {
        pub fn show(&mut self, song: &Song) -> Result<(), dbus::Error> {
            if self.bus.is_none() {
                let bus = Connection::new_session()?;
                let caps: Result<(Vec<String>,), _> = bus.with_proxy(DEST, PATH, Duration::from_secs(5)).method_call(DEST, "GetCapabilities", ());
                self.markup = caps.is_ok_and(|(c,)| c.iter().any(|c| c == "body-markup"));
                self.bus = Some(bus);
            }
            let bus = self.bus.as_ref().expect("connected above");
            let body = if self.markup { super::xml_escape(&song.artist) } else { song.artist.clone() };
            let mut hints = PropMap::new();
            // Its icon and name, and a click on it opens the app.
            hints.insert("desktop-entry".into(), Variant(Box::new("ytm-rs".to_owned())));
            hints.insert("suppress-sound".into(), Variant(Box::new(true)));
            hints.insert("urgency".into(), Variant(Box::new(0u8)));
            hints.insert("category".into(), Variant(Box::new("x-gnome.music".to_owned())));
            if let Some(cover) = &song.cover {
                hints.insert("image-path".into(), Variant(Box::new(cover.to_string_lossy().into_owned())));
            }
            let proxy = bus.with_proxy(DEST, PATH, Duration::from_secs(5));
            let res: Result<(u32,), _> = proxy.method_call(DEST, "Notify", ("ytm-rs", self.last, "ytm-rs", song.title.as_str(), body.as_str(), Vec::<String>::new(), hints, -1i32));
            match res {
                Ok((id,)) => {
                    self.last = id;
                    Ok(())
                }
                Err(e) => {
                    // Connect again next time: the bus or the server may have restarted.
                    self.bus = None;
                    Err(e)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_text_is_escaped() {
        let song = Song { title: "Rock & <Roll>".into(), artist: "\"A\" & 'B'\u{1}".into(), cover: None };
        let xml = toast_xml(&song, None);
        assert!(xml.contains("<text hint-maxLines=\"1\">Rock &amp; &lt;Roll&gt;</text>"), "{xml}");
        assert!(xml.contains("<text>&quot;A&quot; &amp; &apos;B&apos;</text>"), "{xml}");
        assert_eq!(xml.matches("<text").count(), 2, "{xml}");
        assert!(xml.contains("<audio silent=\"true\"/>"));
        assert!(!xml.contains("<image"));
        assert!(!xml.contains("launch="));
    }

    #[test]
    fn toast_links_back_and_shows_the_cover() {
        let cover = std::env::temp_dir().join("cover art.jpg");
        let song = Song { title: "T".into(), artist: "A".into(), cover: Some(cover) };
        let xml = toast_xml(&song, Some("ytm-rs://show"));
        assert!(xml.starts_with("<toast launch=\"ytm-rs://show\" activationType=\"protocol\">"), "{xml}");
        assert!(xml.contains("placement=\"appLogoOverride\" src=\"file:///"), "{xml}");
        assert!(xml.contains("cover%20art.jpg"), "{xml}");
    }

    /// Sends two to the session bus's notification server, the second replacing the first:
    /// `cargo test -- --ignored notifies_on_the_session_bus --nocapture`
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore]
    fn notifies_on_the_session_bus() {
        let mut os = os::Os::default();
        let cover = std::env::temp_dir().join("cover.jpg");
        os.show(&Song { title: "First".into(), artist: "<A> & B".into(), cover: Some(cover) }).unwrap();
        os.show(&Song { title: "Second".into(), artist: "C".into(), cover: None }).unwrap();
    }

    #[test]
    fn a_newer_song_drops_the_older() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let n = Notifier { tx, latest: Arc::new(AtomicU64::new(u64::MAX)) };
        n.expect(1);
        n.expect(2);
        n.show(1, Song { title: "old".into(), ..Default::default() });
        n.show(2, Song { title: "new".into(), ..Default::default() });
        let got: Vec<String> = rx.try_iter().map(|s| s.title).collect();
        assert_eq!(got, ["new"]);
    }
}
