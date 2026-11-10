//! Settings › App icon: alternative icons for the window, the tray and the Start menu,
//! desktop and pinned taskbar shortcuts that launch this exe. The exe's own embedded
//! icon (Explorer's file view) stays the classic one.
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

#[cfg(not(windows))]
mod shortcuts {
    pub fn sync(_: &super::Variant, _: &std::path::Path) -> anyhow::Result<usize> {
        Ok(0)
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
