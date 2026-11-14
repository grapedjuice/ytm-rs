//! Settings › App icon: alternative icons for the window, the tray and the Start menu,
//! desktop and pinned taskbar shortcuts that launch this exe. The exe's own embedded
//! icon (Explorer's file view) stays the classic one. On Linux, the app's menu entry
//! (its .desktop file), where the desktop takes the icon from.
//!
//! The choice lives in `<data dir>/app_icon` as plain text, so `main` can read it
//! before the window opens.

use std::path::{Path, PathBuf};

pub struct Variant {
    pub id: &'static str,
    pub name: &'static str,
    /// An ICO whose entries are PNGs, from 16 to 256 px.
    ico: &'static [u8],
}

macro_rules! variant {
    ($id:literal, $name:literal) => {
        Variant { id: $id, name: $name, ico: include_bytes!(concat!("../assets/icon/variants/", $id, ".ico")) }
    };
}

pub const CLASSIC: &str = "classic";

pub static ALL: &[Variant] = &[
    Variant { id: CLASSIC, name: "Classic", ico: include_bytes!("../assets/icon/icon.ico") },
    variant!("official", "YT Music"),
    variant!("smilering", "Smile ring"),
    variant!("gear", "Rust gear"),
    variant!("wave", "Waveform"),
    variant!("vinyl", "Record"),
    variant!("lyrics", "Lit line"),
    variant!("duet", "Duet"),
    variant!("kawarp", "Warp"),
    variant!("fluent", "Fluent"),
];

pub fn get(id: &str) -> &'static Variant {
    ALL.iter().find(|v| v.id == id).unwrap_or(&ALL[0])
}

impl Variant {
    /// The PNG of the smallest entry at least `size` px wide (else the largest).
    pub fn png(&self, size: u32) -> &'static [u8] {
        let entries = ico_entries(self.ico);
        entries
            .iter()
            .filter(|(w, _)| *w >= size)
            .min_by_key(|(w, _)| *w)
            .or_else(|| entries.iter().max_by_key(|(w, _)| *w))
            .map_or(&[][..], |(_, png)| *png)
    }

    /// Pixels at `size` px, for the window and tray icons.
    pub fn rgba(&self, size: u32) -> Option<image::RgbaImage> {
        let img = image::load_from_memory(self.png(size)).ok()?.into_rgba8();
        Some(if img.width() == size { img } else { image::imageops::resize(&img, size, size, image::imageops::FilterType::Lanczos3) })
    }

    pub fn icon_data(&self) -> Option<egui::IconData> {
        let img = self.rgba(256)?;
        Some(egui::IconData { width: img.width(), height: img.height(), rgba: img.into_raw() })
    }
}

/// (width, PNG bytes) of each entry. A width byte of 0 means 256.
fn ico_entries(ico: &'static [u8]) -> Vec<(u32, &'static [u8])> {
    let u16_at = |i: usize| ico.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize);
    let u32_at = |i: usize| ico.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let n = u16_at(4).unwrap_or(0);
    (0..n)
        .filter_map(|i| {
            let e = 6 + 16 * i;
            let w = match *ico.get(e)? {
                0 => 256,
                w => w as u32,
            };
            let (len, off) = (u32_at(e + 8)?, u32_at(e + 12)?);
            Some((w, ico.get(off..off + len)?))
        })
        .collect()
}

pub fn load(data_dir: &Path) -> &'static str {
    let id = std::fs::read_to_string(data_dir.join("app_icon")).unwrap_or_default();
    get(id.trim()).id
}

pub fn save(data_dir: &Path, id: &str) {
    if let Err(e) = std::fs::write(data_dir.join("app_icon"), id) {
        log::warn!("couldn't save the app icon choice: {e}");
    }
}

/// Show `v` on the window: its title bar (small icon) and taskbar button (big icon).
/// egui's icon command only sets the small one, so the taskbar kept the old icon.
#[cfg_attr(not(windows), allow(unused_variables))]
pub fn set_window_icon(v: &Variant) {
    #[cfg(windows)]
    window::set(v);
}

/// The main window's handle, from `main`.
#[cfg_attr(not(windows), allow(unused_variables))]
pub fn set_hwnd(hwnd: *mut std::ffi::c_void) {
    #[cfg(windows)]
    let _ = window::WINDOW.set(hwnd as usize);
}

#[cfg(windows)]
mod window {
    use std::sync::{Mutex, OnceLock};

    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateIconFromResourceEx, DestroyIcon, HICON, ICON_BIG, ICON_SMALL, LR_DEFAULTCOLOR, SM_CXICON, SM_CXSMICON, SendMessageW, WM_SETICON,
    };

    use super::Variant;

    pub static WINDOW: OnceLock<usize> = OnceLock::new();
    /// The icons set last (as raw handles), destroyed once replaced.
    static OURS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

    pub fn set(v: &Variant) {
        let Some(&h) = WINDOW.get() else { return };
        let hwnd = HWND(h as *mut _);
        let mut made = Vec::new();
        // SAFETY: the handle is the app's own window; each icon is created here and
        // destroyed only after the window has switched away from it.
        unsafe {
            let dpi = GetDpiForWindow(hwnd);
            for (kind, metric) in [(ICON_BIG, SM_CXICON), (ICON_SMALL, SM_CXSMICON)] {
                let px = GetSystemMetricsForDpi(metric, dpi).max(16);
                match CreateIconFromResourceEx(v.png(px as u32), true, 0x0003_0000, px, px, LR_DEFAULTCOLOR) {
                    Ok(icon) => {
                        SendMessageW(hwnd, WM_SETICON, Some(WPARAM(kind as usize)), Some(LPARAM(icon.0 as isize)));
                        made.push(icon.0 as usize);
                    }
                    Err(e) => log::warn!("app icon {}: {e}", v.id),
                }
            }
            for old in std::mem::replace(&mut *OURS.lock().unwrap(), made) {
                let _ = DestroyIcon(HICON(old as *mut _));
            }
        }
    }
}

/// Point every shortcut to this exe at the chosen icon, in the background. The
/// installer rewrites its shortcuts on each update, so this also runs at launch.
pub fn sync_shortcuts(id: &'static str, data_dir: &Path) {
    let data_dir = data_dir.to_owned();
    std::thread::Builder::new()
        .name("app-icon".into())
        .spawn(move || match shortcuts::sync(get(id), &data_dir) {
            Ok(0) => log::debug!("app icon {id}: shortcuts already up to date"),
            Ok(n) => log::info!("app icon {id}: updated {n} shortcut(s)"),
            Err(e) => log::warn!("app icon {id}: couldn't update shortcuts: {e:#}"),
        })
        .ok();
}

/// Where the shortcuts' icon file lives: next to the settings, so updates keep it.
#[cfg_attr(not(windows), allow(dead_code))]
fn icon_file(v: &Variant, data_dir: &Path) -> anyhow::Result<PathBuf> {
    let dir = data_dir.join("icons");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.ico", v.id));
    if std::fs::read(&path).ok().as_deref() != Some(v.ico) {
        std::fs::write(&path, v.ico)?;
    }
    Ok(path)
}

#[cfg(windows)]
mod shortcuts {
    use std::path::{Path, PathBuf};

    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, IPersistFile, STGM_READ};
    use windows::Win32::UI::Shell::{
        FOLDERID_CommonPrograms, FOLDERID_Desktop, FOLDERID_Programs, FOLDERID_PublicDesktop, IShellLinkW, KF_FLAG_DEFAULT, SHCNE_ASSOCCHANGED, SHCNE_UPDATEITEM,
        SHCNF_IDLIST, SHCNF_PATHW, SHChangeNotify, SHGetKnownFolderPath, ShellLink,
    };
    use windows::core::{GUID, HSTRING, Interface, PCWSTR};

    use super::Variant;

    /// How many shortcuts were changed.
    pub fn sync(v: &Variant, data_dir: &Path) -> anyhow::Result<usize> {
        let exe = std::env::current_exe()?;
        let icon = if v.id == super::CLASSIC { exe.clone() } else { super::icon_file(v, data_dir)? };
        // SAFETY: COM on this thread only, for the shell link objects below.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
        let mut changed = Vec::new();
        for lnk in shortcut_files() {
            match update(&lnk, &exe, &icon) {
                Ok(true) => changed.push(lnk),
                Ok(false) => {}
                // All-users shortcuts need admin rights; the rest still change.
                Err(e) => log::debug!("{}: {e}", lnk.display()),
            }
        }
        if let (Some(user), Some(common)) = (known_folder(&FOLDERID_Programs), known_folder(&FOLDERID_CommonPrograms)) {
            per_user_copies(&user, &common, &exe, &icon, &mut changed);
        }
        if !changed.is_empty() {
            // SAFETY: plain notifications; the path strings outlive the calls.
            unsafe {
                for lnk in &changed {
                    let p = HSTRING::from(lnk.as_os_str());
                    SHChangeNotify(SHCNE_UPDATEITEM, SHCNF_PATHW, Some(p.as_ptr().cast()), None);
                }
                SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
            }
        }
        Ok(changed.len())
    }

    /// Set `lnk`'s icon if it launches `exe` and doesn't show `icon` yet.
    fn update(lnk: &Path, exe: &Path, icon: &Path) -> anyhow::Result<bool> {
        let (link, file) = open(lnk)?;
        if !launches(&link, exe)? || shows(&link, exe, icon)? {
            return Ok(false);
        }
        // SAFETY: COM calls on objects this function owns.
        unsafe {
            link.SetIconLocation(&HSTRING::from(icon.as_os_str()), 0)?;
            file.Save(PCWSTR::null(), true)?;
        }
        Ok(true)
    }

    /// Without admin rights an all-users Start menu shortcut can't change, and Windows
    /// shows its icon on the taskbar over the window's. A shortcut at the same place in
    /// the user's own Start menu takes its place, so give that one the icon, and remove
    /// it again once the all-users one shows the right icon (always so for classic).
    fn per_user_copies(user: &Path, common: &Path, exe: &Path, icon: &Path, changed: &mut Vec<PathBuf>) {
        let mut shared = Vec::new();
        collect(common, 2, &mut shared);
        for lnk in shared {
            let Ok(rel) = lnk.strip_prefix(common) else { continue };
            let own = user.join(rel);
            let result = open(&lnk).and_then(|(link, file)| {
                if !launches(&link, exe)? {
                    return Ok(false);
                }
                if shows(&link, exe, icon)? {
                    return remove_copy(&own, exe);
                }
                // One that's already there (say, from a per-user install) isn't ours to replace.
                if own.exists() {
                    return Ok(false);
                }
                if let Some(dir) = own.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                // SAFETY: COM calls on objects this closure owns.
                unsafe {
                    link.SetIconLocation(&HSTRING::from(icon.as_os_str()), 0)?;
                    file.Save(&HSTRING::from(own.as_os_str()), true)?;
                }
                Ok(true)
            });
            match result {
                Ok(true) => changed.push(own),
                Ok(false) => {}
                Err(e) => log::warn!("app icon: no shortcut of your own for {}: {e:#}", lnk.display()),
            }
        }
    }

    /// Remove our copy, and its folder if that leaves it empty.
    fn remove_copy(own: &Path, exe: &Path) -> anyhow::Result<bool> {
        if !own.exists() || !launches(&open(own)?.0, exe)? {
            return Ok(false);
        }
        std::fs::remove_file(own)?;
        if let Some(dir) = own.parent() {
            let _ = std::fs::remove_dir(dir);
        }
        Ok(true)
    }

    fn open(lnk: &Path) -> anyhow::Result<(IShellLinkW, IPersistFile)> {
        // SAFETY: COM calls on an object this function creates.
        unsafe {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            let file: IPersistFile = link.cast()?;
            file.Load(&HSTRING::from(lnk.as_os_str()), STGM_READ)?;
            Ok((link, file))
        }
    }

    fn launches(link: &IShellLinkW, exe: &Path) -> anyhow::Result<bool> {
        let mut buf = [0u16; 1024];
        // SAFETY: the buffer outlives the call, which is told its length.
        unsafe { link.GetPath(&mut buf, std::ptr::null_mut(), 0)? };
        Ok(same_file(&from_wide(&buf), exe))
    }

    fn shows(link: &IShellLinkW, exe: &Path, icon: &Path) -> anyhow::Result<bool> {
        let (mut buf, mut index) = ([0u16; 1024], 0);
        // SAFETY: the buffer outlives the call, which is told its length.
        unsafe { link.GetIconLocation(&mut buf, &mut index)? };
        let current = from_wide(&buf);
        // No icon of its own means the exe's, which is the classic one.
        let showing = if current.as_os_str().is_empty() { exe.to_owned() } else { current };
        Ok(same_file(&showing, icon) && index == 0)
    }

    fn shortcut_files() -> Vec<PathBuf> {
        let mut dirs: Vec<(PathBuf, usize)> = [FOLDERID_Programs, FOLDERID_CommonPrograms, FOLDERID_Desktop, FOLDERID_PublicDesktop]
            .iter()
            .filter_map(known_folder)
            .map(|d| (d, 2))
            .collect();
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let pinned = PathBuf::from(appdata).join(r"Microsoft\Internet Explorer\Quick Launch\User Pinned");
            dirs.push((pinned.join("TaskBar"), 1));
            dirs.push((pinned.join("StartMenu"), 1));
        }
        // Dev hook: YTM_SHORTCUT_DIR also looks for shortcuts in that folder, so tests
        // can check this without touching real shortcuts.
        if let Some(d) = std::env::var_os("YTM_SHORTCUT_DIR") {
            dirs.push((PathBuf::from(d), 1));
        }
        let mut out = Vec::new();
        for (dir, depth) in dirs {
            collect(&dir, depth, &mut out);
        }
        out
    }

    fn collect(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                if depth > 1 {
                    collect(&p, depth - 1, out);
                }
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")) {
                out.push(p);
            }
        }
    }

    fn known_folder(id: &GUID) -> Option<PathBuf> {
        // SAFETY: the returned string is copied, then freed with CoTaskMemFree.
        unsafe {
            let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
            let path = PathBuf::from(p.to_string().ok()?);
            CoTaskMemFree(Some(p.0 as *const _));
            Some(path)
        }
    }

    fn from_wide(buf: &[u16]) -> PathBuf {
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        PathBuf::from(String::from_utf16_lossy(&buf[..n]))
    }

    fn same_file(a: &Path, b: &Path) -> bool {
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned());
        canon(a).to_string_lossy().eq_ignore_ascii_case(&canon(b).to_string_lossy())
    }

    #[cfg(test)]
    mod tests {
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};

        use super::*;

        fn make_link(lnk: &Path, target: &Path) {
            std::fs::create_dir_all(lnk.parent().unwrap()).unwrap();
            // SAFETY: COM on this test's thread, on an object it owns.
            unsafe {
                CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap();
                let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
                link.SetPath(&HSTRING::from(target.as_os_str())).unwrap();
                link.cast::<IPersistFile>().unwrap().Save(&HSTRING::from(lnk.as_os_str()), true).unwrap();
            }
        }

        fn scratch(name: &str) -> PathBuf {
            let dir = std::env::temp_dir().join(format!("ytm-appicon-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        #[test]
        fn a_stuck_all_users_shortcut_gets_a_copy_until_classic_is_back() {
            let dir = scratch("copy");
            let (user, common) = (dir.join("user"), dir.join("common"));
            let (exe, icon) = (std::env::current_exe().unwrap(), dir.join("wave.ico"));
            std::fs::write(&icon, b"icon").unwrap();
            let shared = common.join("ytm-rs").join("ytm-rs.lnk");
            let own = user.join("ytm-rs").join("ytm-rs.lnk");
            make_link(&shared, &exe);

            let mut changed = Vec::new();
            per_user_copies(&user, &common, &exe, &icon, &mut changed);
            assert_eq!(changed, std::slice::from_ref(&own));
            let (link, _) = open(&own).unwrap();
            assert!(launches(&link, &exe).unwrap() && shows(&link, &exe, &icon).unwrap());

            // Run again: nothing more to do.
            changed.clear();
            per_user_copies(&user, &common, &exe, &icon, &mut changed);
            assert!(changed.is_empty());

            // Classic: the all-users shortcut is right again, so the copy goes.
            per_user_copies(&user, &common, &exe, &exe, &mut changed);
            assert_eq!(changed, std::slice::from_ref(&own));
            assert!(!own.exists() && !own.parent().unwrap().exists());
            std::fs::remove_dir_all(&dir).unwrap();
        }

        #[test]
        fn a_shortcut_to_another_copy_of_the_app_is_left_alone() {
            let dir = scratch("other");
            let (user, common) = (dir.join("user"), dir.join("common"));
            let (exe, icon) = (std::env::current_exe().unwrap(), dir.join("wave.ico"));
            let other = dir.join("other.ico");
            std::fs::write(&icon, b"icon").unwrap();
            std::fs::write(&other, b"not this exe").unwrap();
            let own = user.join("ytm-rs").join("ytm-rs.lnk");
            make_link(&common.join("ytm-rs").join("ytm-rs.lnk"), &exe);
            make_link(&own, &other);

            let mut changed = Vec::new();
            per_user_copies(&user, &common, &exe, &icon, &mut changed);
            assert!(changed.is_empty());
            per_user_copies(&user, &common, &exe, &exe, &mut changed);
            assert!(changed.is_empty() && own.exists());
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }
}

#[cfg(target_os = "linux")]
mod shortcuts {
    pub use super::desktop::sync;
}

#[cfg(not(any(windows, target_os = "linux")))]
mod shortcuts {
    pub fn sync(_: &super::Variant, _: &std::path::Path) -> anyhow::Result<usize> {
        Ok(0)
    }
}

/// Linux: GNOME and KDE show the icon of the app's .desktop file, not the window's
/// (a Wayland window has none), so the chosen icon goes there (#146): into the user's
/// own ytm-rs.desktop, install.sh's, or else a copy of the system's (the .deb's, the
/// Arch package's) that goes again for Classic. Only an entry that launches this exe.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod desktop {
    use std::path::{Path, PathBuf};

    use super::Variant;

    const FILE: &str = "ytm-rs.desktop";
    /// The icon the entry comes with: the theme's ytm-rs, the classic one.
    const CLASSIC_ICON: &str = "ytm-rs";
    /// Marks a copy of the system's entry as ours, so Classic removes it again.
    const COPY: &str = "X-ytm-rs-Icon-Copy=true";

    /// How many entries were changed (0 or 1).
    pub fn sync(v: &Variant, data_dir: &Path) -> anyhow::Result<usize> {
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
        let Some(data_home) = data_home else { return Ok(0) };
        let dirs = std::env::var("XDG_DATA_DIRS").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        let system: Vec<PathBuf> = dirs.split(':').filter(|d| !d.is_empty()).map(|d| Path::new(d).join("applications")).collect();
        let icon = if v.id == super::CLASSIC { None } else { Some(icon_png(v, data_dir)?) };
        Ok(sync_at(icon.as_deref(), &data_home.join("applications"), &system, &std::env::current_exe()?)? as usize)
    }

    /// The icon as a PNG next to the settings, a file per icon so the desktop doesn't
    /// show one it has cached.
    fn icon_png(v: &Variant, data_dir: &Path) -> anyhow::Result<PathBuf> {
        let dir = data_dir.join("icons");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.png", v.id));
        let png = v.png(256);
        if std::fs::read(&path).ok().as_deref() != Some(png) {
            std::fs::write(&path, png)?;
        }
        Ok(path)
    }

    /// Give the user's entry in `user_dir` the `icon` (`None`: classic). Whether it changed.
    fn sync_at(icon: Option<&Path>, user_dir: &Path, system_dirs: &[PathBuf], exe: &Path) -> anyhow::Result<bool> {
        let user = user_dir.join(FILE);
        let own = std::fs::read_to_string(&user).ok();
        let system = system_dirs.iter().find_map(|d| std::fs::read_to_string(d.join(FILE)).ok());
        let value = icon.map_or_else(|| CLASSIC_ICON.to_owned(), |p| p.display().to_string());
        let new = if let Some(f) = own.as_deref().filter(|f| !is_copy(f)) {
            // install.sh's (or one the user made): the icon changes in place.
            if !launches(f, exe) {
                return Ok(false);
            }
            with_icon(f, &value, false)
        } else if let Some(s) = system.as_deref().filter(|s| icon.is_some() && launches(s, exe)) {
            // Made afresh from the system's each time, so it keeps up with the package's.
            with_icon(s, &value, true)
        } else if own.as_deref().is_some_and(|f| launches(f, exe)) {
            // Our copy, no longer needed: the system's entry has the classic icon.
            std::fs::remove_file(&user)?;
            return Ok(true);
        } else {
            return Ok(false);
        };
        if own.as_deref() == Some(new.as_str()) {
            return Ok(false);
        }
        std::fs::create_dir_all(user_dir)?;
        // Whole or not at all: the desktop watches this folder.
        let tmp = user_dir.join(format!(".{FILE}.tmp"));
        std::fs::write(&tmp, &new)?;
        std::fs::rename(&tmp, &user)?;
        Ok(true)
    }

    fn is_copy(desktop: &str) -> bool {
        main_group(desktop).any(|l| l.trim() == COPY)
    }

    /// The lines of the [Desktop Entry] group (the others are its actions).
    fn main_group(desktop: &str) -> impl Iterator<Item = &str> {
        desktop.lines().skip_while(|l| l.trim() != "[Desktop Entry]").skip(1).take_while(|l| !l.trim_start().starts_with('['))
    }

    fn key_is(line: &str, key: &str) -> bool {
        line.split_once('=').is_some_and(|(k, _)| k.trim() == key)
    }

    /// Whether the entry's Exec runs `exe` (found on PATH when it's a bare name).
    fn launches(desktop: &str, exe: &Path) -> bool {
        let Some(exec) = main_group(desktop).find(|l| key_is(l, "Exec")).and_then(|l| l.split_once('=')).map(|(_, v)| v.trim()) else {
            return false;
        };
        let program = match exec.strip_prefix('"') {
            Some(rest) => rest.split('"').next().unwrap_or(""),
            None => exec.split_whitespace().next().unwrap_or(""),
        };
        let path = Path::new(program);
        let found = if path.is_absolute() || program.contains('/') {
            Some(path.to_owned())
        } else {
            std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(program)).find(|p| p.is_file()))
        };
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned());
        found.is_some_and(|f| canon(&f) == canon(exe))
    }

    /// The entry with Icon= set to `icon` in its main group, marked as our copy or not.
    fn with_icon(desktop: &str, icon: &str, copy: bool) -> String {
        let has_icon = main_group(desktop).any(|l| key_is(l, "Icon"));
        let mut out: Vec<String> = Vec::new();
        let mut in_main = false;
        let place = |out: &mut Vec<String>| {
            out.push(format!("Icon={icon}"));
            if copy {
                out.push(COPY.to_owned());
            }
        };
        for line in desktop.lines() {
            let t = line.trim();
            if t.starts_with('[') {
                in_main = t == "[Desktop Entry]";
                out.push(line.to_owned());
                if in_main && !has_icon {
                    place(&mut out);
                }
            } else if in_main && t == COPY {
                // Placed again with the icon, if at all.
            } else if in_main && key_is(line, "Icon") {
                place(&mut out);
            } else {
                out.push(line.to_owned());
            }
        }
        let mut s = out.join("\n");
        s.push('\n');
        s
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const ENTRY: &str = "[Desktop Entry]\nType=Application\nName=YT Music\nExec=EXE\nIcon=ytm-rs\nStartupWMClass=ytm-rs\n\n[Desktop Action x]\nIcon=other\n";

        fn scratch(name: &str) -> PathBuf {
            let dir = std::env::temp_dir().join(format!("ytm-desktop-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn entry(exe: &Path) -> String {
            ENTRY.replace("EXE", &format!("\"{}\" %U", exe.display()))
        }

        fn icon_line(f: &str) -> String {
            main_group(f).find(|l| key_is(l, "Icon")).unwrap().to_owned()
        }

        #[test]
        fn only_the_main_groups_icon_changes() {
            let f = with_icon(ENTRY, "/d/icons/wave.png", true);
            assert_eq!(icon_line(&f), "Icon=/d/icons/wave.png");
            assert!(is_copy(&f) && f.ends_with("[Desktop Action x]\nIcon=other\n"));
            // Back again: as it was.
            assert_eq!(with_icon(&f, "ytm-rs", false), ENTRY);
            assert_eq!(icon_line(&with_icon("[Desktop Entry]\nName=x\n", "a.png", false)), "Icon=a.png");
        }

        #[test]
        fn install_sh_entry_changes_in_place() {
            let dir = scratch("own");
            let (user, exe) = (dir.join("user"), std::env::current_exe().unwrap());
            std::fs::create_dir_all(&user).unwrap();
            std::fs::write(user.join(FILE), entry(&exe)).unwrap();
            let icon = dir.join("wave.png");
            assert!(sync_at(Some(&icon), &user, &[], &exe).unwrap());
            let f = std::fs::read_to_string(user.join(FILE)).unwrap();
            assert_eq!(icon_line(&f), format!("Icon={}", icon.display()));
            assert!(!is_copy(&f));
            assert!(!sync_at(Some(&icon), &user, &[], &exe).unwrap());
            // Classic puts the theme's icon back, and the entry stays.
            assert!(sync_at(None, &user, &[], &exe).unwrap());
            assert_eq!(std::fs::read_to_string(user.join(FILE)).unwrap(), entry(&exe));
            std::fs::remove_dir_all(&dir).unwrap();
        }

        #[test]
        fn a_package_entry_gets_a_copy_until_classic() {
            let dir = scratch("copy");
            let (user, system, exe) = (dir.join("user"), dir.join("system"), std::env::current_exe().unwrap());
            std::fs::create_dir_all(&system).unwrap();
            std::fs::write(system.join(FILE), entry(&exe)).unwrap();
            let icon = dir.join("vinyl.png");
            let dirs = [dir.join("none"), system.clone()];
            assert!(sync_at(Some(&icon), &user, &dirs, &exe).unwrap());
            let f = std::fs::read_to_string(user.join(FILE)).unwrap();
            assert!(is_copy(&f) && icon_line(&f) == format!("Icon={}", icon.display()));
            assert!(!sync_at(Some(&icon), &user, &dirs, &exe).unwrap());
            assert!(sync_at(None, &user, &dirs, &exe).unwrap());
            assert!(!user.join(FILE).exists());
            assert!(!sync_at(None, &user, &dirs, &exe).unwrap());
            std::fs::remove_dir_all(&dir).unwrap();
        }

        #[test]
        fn another_copy_of_the_apps_entry_is_left_alone() {
            let dir = scratch("other");
            let (user, system, exe) = (dir.join("user"), dir.join("system"), std::env::current_exe().unwrap());
            let other = dir.join("ytm-rs");
            std::fs::write(&other, b"not this exe").unwrap();
            for d in [&user, &system] {
                std::fs::create_dir_all(d).unwrap();
            }
            std::fs::write(system.join(FILE), entry(&other)).unwrap();
            let icon = dir.join("wave.png");
            // No entry of its own made for a package that runs another copy...
            assert!(!sync_at(Some(&icon), &user, std::slice::from_ref(&system), &exe).unwrap());
            assert!(!user.join(FILE).exists());
            // ...and neither another copy's entry nor its icon copy touched.
            std::fs::write(user.join(FILE), with_icon(&entry(&other), "/x.png", true)).unwrap();
            assert!(!sync_at(None, &user, std::slice::from_ref(&system), &exe).unwrap());
            assert!(user.join(FILE).exists());
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_has_every_size() {
        for v in ALL {
            let sizes: Vec<u32> = ico_entries(v.ico).iter().map(|(w, _)| *w).collect();
            assert!(sizes.contains(&16) && sizes.contains(&32) && sizes.contains(&256), "{}: {sizes:?}", v.id);
            for size in [16, 24, 32, 56, 256] {
                let img = v.rgba(size).unwrap_or_else(|| panic!("{} doesn't decode at {size}", v.id));
                assert_eq!(img.dimensions(), (size, size), "{}", v.id);
            }
        }
    }

    #[test]
    fn picks_the_smallest_entry_that_fits() {
        let v = get("gear");
        let w = |png: &[u8]| image::load_from_memory(png).unwrap().width();
        assert_eq!(w(v.png(16)), 16);
        assert_eq!(w(v.png(50)), 64);
        assert_eq!(w(v.png(300)), 256);
        assert_eq!(get("nonsense").id, CLASSIC);
    }
}
