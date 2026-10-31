//! Automatic updates from GitHub releases.
//!
//! The installed app (Inno Setup, per-user) checks the latest release at startup and
//! every few hours, downloads a newer installer in the background, and runs it
//! silently when the app closes, or right away when the user picks "Restart to
//! update". The portable build (a `portable.txt` beside the exe) downloads the new
//! portable zip instead and swaps the exe in place as it closes. On Linux, a copy
//! put in place by `install.sh` (an `install.txt` beside the binary) downloads the new
//! tarball and renames the new binary over itself as it closes. On macOS, the copy
//! installed from the disk image downloads the new one and puts the app from it in
//! place of its own as it closes. Every platform has a portable build too (a
//! `portable.txt` marker; inside the app bundle on macOS), which updates from the
//! portable download. Dev builds never update themselves.
//!
//! Release notes (the release's Markdown body) are kept per version: the one a check
//! finds, and any other on request ([`Updater::notes`]), for the update dialog, the
//! card shown after updating and Settings.
//!
//! Dev hooks: `YTM_UPDATE_FORCE=1` treats any build as installed; `YTM_UPDATE_AS=<ver>`
//! pretends to be that version; `YTM_UPDATE_DRYRUN=1` logs the install command
//! instead of running it; `YTM_UPDATE_FEED=<url>` reads the latest release from there.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const LATEST: &str = "https://api.github.com/repos/grapedjuice/ytm-rs/releases/latest";
const ALL_RELEASES: &str = "https://api.github.com/repos/grapedjuice/ytm-rs/releases";
const BY_TAG: &str = "https://api.github.com/repos/grapedjuice/ytm-rs/releases/tags/v";
pub const RELEASES: &str = "https://github.com/grapedjuice/ytm-rs/releases";

/// Release notes by version: `None` while fetching or when there are none.
type Notes = Arc<Mutex<std::collections::HashMap<String, Option<String>>>>;

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Idle,
    Checking,
    UpToDate,
    Downloading { version: String, percent: u8 },
    /// `installer` is the setup exe, or for the portable build the new ytm-rs.exe.
    Ready { version: String, installer: PathBuf },
    Failed(String),
}

#[derive(Clone)]
pub struct Updater {
    state: Arc<Mutex<State>>,
    dir: PathBuf,
    wake: Arc<dyn Fn() + Send + Sync>,
    notes: Notes,
    /// Total downloads of every release asset: None until asked, Some(None) while fetching.
    downloads: Arc<Mutex<Option<Option<u64>>>>,
}

pub fn current_version() -> String {
    std::env::var("YTM_UPDATE_AS").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_owned())
}

/// Whether this copy was installed by the installer (which leaves its uninstaller
/// beside the exe, or on Linux `install.sh` its `install.txt`), or on macOS runs from
/// an app bundle that isn't the portable one, so it's safe to update in place.
pub fn installed() -> bool {
    if std::env::var_os("YTM_UPDATE_FORCE").is_some() {
        return true;
    }
    let Ok(exe) = std::env::current_exe() else { return false };
    if cfg!(target_os = "macos") {
        return bundle_of(&exe).is_some() && portable_dir().is_none();
    }
    let marker = if cfg!(windows) { "unins000.exe" } else { "install.txt" };
    exe.parent().is_some_and(|d| d.join(marker).exists())
}

/// The `.app` bundle a binary is in (`X.app/Contents/MacOS/ytm-rs`).
fn bundle_of(exe: &Path) -> Option<&Path> {
    exe.ancestors().nth(3).filter(|a| a.extension().is_some_and(|x| x == "app"))
}

/// The release asset this copy updates from.
fn asset_name(version: &str) -> String {
    let portable = portable_dir().is_some();
    let arch = std::env::consts::ARCH;
    if cfg!(target_os = "linux") {
        let kind = if portable { "portable-" } else { "" };
        return format!("ytm-rs-{kind}{version}-linux-{arch}.tar.gz");
    }
    // ARM64 Windows builds have their own assets; x64 keeps the original names.
    let (suffix, setup) = if cfg!(target_os = "macos") {
        (format!("-macos-{arch}"), "dmg")
    } else {
        (if cfg!(target_arch = "aarch64") { "-arm64" } else { "" }.to_owned(), "exe")
    };
    if portable { format!("ytm-rs-portable-{version}{suffix}.zip") } else { format!("ytm-rs-setup-{version}{suffix}.{setup}") }
}

/// The portable build's folder: the download ships a `portable.txt` beside the exe
/// (on macOS inside the app bundle, where code signing allows it), which keeps
/// settings in `data` next to it (next to the bundle) and has updates replace it in place.
pub fn portable_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    if let Some(app) = bundle_of(&exe) {
        // Opened without being moved out of quarantine, macOS runs the app from a
        // random read-only copy; there's no folder beside it to keep data in.
        if app.to_string_lossy().contains("/AppTranslocation/") {
            return None;
        }
        if !app.join("Contents/Resources/portable.txt").exists() {
            return None;
        }
        return app.parent().map(Path::to_path_buf);
    }
    let dir = exe.parent()?.to_path_buf();
    dir.join("portable.txt").exists().then_some(dir)
}

/// Remove the exe a portable update replaced (it was still running then).
pub fn clean_portable() {
    if portable_dir().is_some()
        && let Ok(exe) = std::env::current_exe()
    {
        let _ = std::fs::remove_file(exe.with_extension("exe.old"));
    }
}

/// `a` is newer than `b` (dotted numeric versions, a leading "v" ignored).
pub fn newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| v.trim_start_matches('v').split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    let (a, b) = (parse(a), parse(b));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

impl Updater {
    pub fn new(data_dir: &Path, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self { state: Arc::new(Mutex::new(State::Idle)), dir: data_dir.join("updates"), wake: Arc::new(wake), notes: Default::default(), downloads: Default::default() }
    }

    pub fn state(&self) -> State {
        self.state.lock().unwrap().clone()
    }

    fn set(&self, s: State) {
        *self.state.lock().unwrap() = s;
        (self.wake)();
    }

    /// Release notes (Markdown) for `version`, fetched in the background the first time
    /// they're asked for; `None` until they arrive, or if the release has none.
    pub fn notes(&self, version: &str) -> Option<String> {
        let version = version.trim_start_matches('v').to_owned();
        {
            let mut notes = self.notes.lock().unwrap();
            if let Some(n) = notes.get(&version) {
                return n.clone();
            }
            notes.insert(version.clone(), None);
        }
        let this = self.clone();
        std::thread::spawn(move || {
            let fetch = async {
                let http = client()?;
                let release: serde_json::Value = http.get(format!("{BY_TAG}{version}")).send().await?.error_for_status()?.json().await?;
                anyhow::Ok(release["body"].as_str().map(str::to_owned))
            };
            let body = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt.block_on(fetch).unwrap_or_else(|e| {
                    log::warn!("update: notes for {version}: {e:#}");
                    None
                }),
                Err(_) => None,
            };
            if body.is_some() {
                this.notes.lock().unwrap().insert(version.clone(), body);
                (this.wake)();
            }
        });
        None
    }

    /// How many times ytm-rs has been downloaded: every release asset's GitHub download
    /// count added up (installers, plus the copies the updater fetched). Fetched in the
    /// background the first time it's asked for; `None` until it arrives.
    pub fn downloads(&self) -> Option<u64> {
        {
            let mut d = self.downloads.lock().unwrap();
            if let Some(n) = *d {
                return n;
            }
            *d = Some(None);
        }
        let this = self.clone();
        std::thread::spawn(move || {
            let fetch = async {
                let http = client()?;
                let mut total = 0;
                for page in 1.. {
                    let releases: Vec<serde_json::Value> = http.get(format!("{ALL_RELEASES}?per_page=100&page={page}")).send().await?.error_for_status()?.json().await?;
                    total += releases.iter().flat_map(|r| r["assets"].as_array().into_iter().flatten()).filter_map(|a| a["download_count"].as_u64()).sum::<u64>();
                    if releases.len() < 100 {
                        break;
                    }
                }
                anyhow::Ok(total)
            };
            let total = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(anyhow::Error::from).and_then(|rt| rt.block_on(fetch));
            match total {
                Ok(n) => {
                    *this.downloads.lock().unwrap() = Some(Some(n));
                    (this.wake)();
                }
                Err(e) => log::warn!("update: download count: {e:#}"),
            }
        });
        None
    }

    /// Check for a newer release and download it, in the background.
    pub fn check(&self) {
        if matches!(self.state(), State::Checking | State::Downloading { .. } | State::Ready { .. }) {
            return;
        }
        self.set(State::Checking);
        let this = self.clone();
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => return this.set(State::Failed(e.to_string())),
            };
            match rt.block_on(this.run()) {
                Ok(s) => this.set(s),
                Err(e) => {
                    log::warn!("update check failed: {e:#}");
                    this.set(State::Failed(format!("{e:#}")));
                }
            }
        });
    }

    async fn run(&self) -> anyhow::Result<State> {
        let http = client()?;
        let feed = std::env::var("YTM_UPDATE_FEED").unwrap_or_else(|_| LATEST.to_owned());
        let release: serde_json::Value = http.get(feed).send().await?.error_for_status()?.json().await?;
        let tag = release["tag_name"].as_str().unwrap_or_default();
        if let Some(body) = release["body"].as_str() {
            self.notes.lock().unwrap().insert(tag.trim_start_matches('v').to_owned(), Some(body.to_owned()));
        }
        let current = current_version();
        if !newer(tag, &current) {
            log::info!("update: {current} is current (latest {tag})");
            return Ok(State::UpToDate);
        }
        let version = tag.trim_start_matches('v').to_owned();
        let portable = portable_dir().is_some();
        let want = asset_name(&version);
        // A release published before its installer was built: nothing to install
        // yet, so this isn't an error. The next check picks it up.
        let Some(asset) = release["assets"].as_array().and_then(|a| a.iter().find(|x| x["name"] == want.as_str())) else {
            log::info!("update: release {tag} has no {want} yet");
            return Ok(State::UpToDate);
        };
        let url = asset["browser_download_url"].as_str().ok_or_else(|| anyhow::anyhow!("asset has no URL"))?;
        let size = asset["size"].as_u64().unwrap_or(0);
        std::fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(&want);
        if std::fs::metadata(&path).map(|m| m.len()).ok() != Some(size) || size == 0 {
            log::info!("update: downloading {version} ({size} bytes)");
            self.set(State::Downloading { version: version.clone(), percent: 0 });
            let mut resp = http.get(url).send().await?.error_for_status()?;
            // Stream to a file beside, then rename, so a half-written file is never taken as ready.
            let part = path.with_extension("part");
            let mut file = std::io::BufWriter::new(std::fs::File::create(&part)?);
            let (mut got, mut shown) = (0u64, 0u8);
            while let Some(chunk) = resp.chunk().await? {
                std::io::Write::write_all(&mut file, &chunk)?;
                got += chunk.len() as u64;
                let percent = if size > 0 { (got * 100 / size).min(100) as u8 } else { 0 };
                if percent != shown {
                    shown = percent;
                    self.set(State::Downloading { version: version.clone(), percent });
                }
            }
            std::io::Write::flush(&mut file)?;
            drop(file);
            if size > 0 && got != size {
                let _ = std::fs::remove_file(&part);
                anyhow::bail!("download incomplete ({got} of {size} bytes)");
            }
            std::fs::rename(&part, &path)?;
        }
        // Old downloads are no longer needed.
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p != path {
                    let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
                }
            }
        }
        let unpacked = self.dir.join(format!("ytm-rs-{version}"));
        let ready = if cfg!(target_os = "macos") && !portable {
            open_dmg(&path, &unpacked)?
        } else if portable || cfg!(unix) {
            unzip_exe(&path, &unpacked)?
        } else {
            path
        };
        log::info!("update: {version} ready at {}", ready.display());
        Ok(State::Ready { version, installer: ready })
    }

    /// Run the downloaded installer silently once this process has exited, then
    /// (if `relaunch`) start the updated app.
    pub fn install(&self, relaunch: bool) {
        let State::Ready { installer, .. } = self.state() else { return };
        let exe = std::env::current_exe().unwrap_or_default();
        #[cfg(target_os = "macos")]
        if let (Some(new), Some(app)) = (bundle_of(&installer), bundle_of(&exe)) {
            return replace_bundle(new, app, relaunch);
        }
        #[cfg(unix)]
        replace_binary(&installer, &exe, relaunch);
        #[cfg(windows)]
        self.run_installer(&installer, &exe, relaunch);
    }

    #[cfg(windows)]
    fn run_installer(&self, installer: &Path, exe: &Path, relaunch: bool) {
        if portable_dir().is_some() {
            return swap_exe(installer, exe, relaunch);
        }
        // Update the copy that's running: an all-users install lives under Program Files
        // (and the installer will ask for admin rights); otherwise it's per-user.
        let program_files = std::env::var_os("ProgramFiles").map(PathBuf::from);
        let scope = if program_files.is_some_and(|p| exe.starts_with(p)) { "/ALLUSERS" } else { "/CURRENTUSER" };
        // Give this process a moment to save and exit before the installer replaces it.
        let mut script = format!(
            "ping -n 3 127.0.0.1 >NUL & \"{}\" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART {scope}",
            installer.display()
        );
        if relaunch {
            script += &format!(" & start \"\" \"{}\" {}", exe.display(), crate::deeplink::RELAUNCH_FLAG);
        }
        if std::env::var_os("YTM_UPDATE_DRYRUN").is_some() {
            log::info!("update (dry run): cmd /C {script}");
            return;
        }
        log::info!("update: installing {}", installer.display());
        run_detached(&script);
    }
}

/// The portable update: Windows lets a running exe be renamed, so move this one aside
/// and put the new one in its place (the next launch deletes the old one).
#[cfg(windows)]
fn swap_exe(new: &Path, exe: &Path, relaunch: bool) {
    let old = exe.with_extension("exe.old");
    if std::env::var_os("YTM_UPDATE_DRYRUN").is_some() {
        log::info!("update (dry run): {} aside to {}, {} in its place", exe.display(), old.display(), new.display());
        return;
    }
    let _ = std::fs::remove_file(&old);
    if let Err(e) = std::fs::rename(exe, &old) {
        return log::warn!("update: couldn't move {} aside: {e}", exe.display());
    }
    if let Err(e) = std::fs::copy(new, exe) {
        log::warn!("update: couldn't copy the new version: {e}");
        let _ = std::fs::rename(&old, exe);
        return;
    }
    log::info!("update: replaced {}", exe.display());
    if relaunch {
        // Once this process has saved and exited.
        run_detached(&format!("ping -n 3 127.0.0.1 >NUL & start \"\" \"{}\" {}", exe.display(), crate::deeplink::RELAUNCH_FLAG));
    }
}

/// Linux: rename the new binary over this one. The running process keeps its copy
/// open until it exits; the next launch runs the new one.
#[cfg(unix)]
fn replace_binary(new: &Path, exe: &Path, relaunch: bool) {
    use std::os::unix::fs::PermissionsExt;
    let tmp = exe.with_file_name(".ytm-rs.new");
    if std::env::var_os("YTM_UPDATE_DRYRUN").is_some() {
        log::info!("update (dry run): {} over {}", new.display(), exe.display());
        return;
    }
    let done = std::fs::copy(new, &tmp)
        .and_then(|_| std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)))
        .and_then(|_| std::fs::rename(&tmp, exe));
    if let Err(e) = done {
        let _ = std::fs::remove_file(&tmp);
        return log::warn!("update: couldn't replace {}: {e}", exe.display());
    }
    log::info!("update: replaced {}", exe.display());
    if relaunch {
        // Once this process has saved and exited.
        let quoted = exe.display().to_string().replace('\'', r"'\''");
        run_detached(&format!("sleep 2; exec '{quoted}'"));
    }
}

/// macOS: put the new app bundle where this one is, so its Info.plist and icon update
/// too. A running app's bundle can be moved; this process keeps running from the old
/// one until it exits.
#[cfg(target_os = "macos")]
fn replace_bundle(new: &Path, app: &Path, relaunch: bool) {
    let parent = app.parent().unwrap_or(Path::new("/"));
    let (tmp, old) = (parent.join(".ytm-rs.app.new"), parent.join(".ytm-rs.app.old"));
    if std::env::var_os("YTM_UPDATE_DRYRUN").is_some() {
        log::info!("update (dry run): {} in place of {}", new.display(), app.display());
        return;
    }
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_dir_all(&old);
    // ditto copies a bundle whole: symlinks, permissions, signature.
    let copied = std::process::Command::new("ditto").arg(new).arg(&tmp).status().is_ok_and(|s| s.success());
    if !copied {
        let _ = std::fs::remove_dir_all(&tmp);
        return log::warn!("update: couldn't copy {} beside {}", new.display(), app.display());
    }
    if let Err(e) = std::fs::rename(app, &old) {
        let _ = std::fs::remove_dir_all(&tmp);
        return log::warn!("update: couldn't move {} aside: {e}", app.display());
    }
    if let Err(e) = std::fs::rename(&tmp, app) {
        let _ = std::fs::rename(&old, app);
        let _ = std::fs::remove_dir_all(&tmp);
        return log::warn!("update: couldn't put the new version in place: {e}");
    }
    let _ = std::fs::remove_dir_all(&old);
    log::info!("update: replaced {}", app.display());
    if relaunch {
        // Once this process has saved and exited.
        let quoted = app.display().to_string().replace('\'', r"'\''");
        run_detached(&format!("sleep 2; open '{quoted}'"));
    }
}

/// macOS: the app from a downloaded installer disk image, copied out into `dir`.
fn open_dmg(dmg: &Path, dir: &Path) -> anyhow::Result<PathBuf> {
    let _ = std::fs::remove_dir_all(dir);
    let mnt = dir.join("mnt");
    std::fs::create_dir_all(&mnt)?;
    let attached = std::process::Command::new("hdiutil").args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-quiet", "-mountpoint"]).arg(&mnt).arg(dmg).status()?;
    anyhow::ensure!(attached.success(), "couldn't open {}", dmg.display());
    let copied = std::process::Command::new("ditto").arg(mnt.join("ytm-rs.app")).arg(dir.join("ytm-rs.app")).status();
    let _ = std::process::Command::new("hdiutil").args(["detach", "-quiet"]).arg(&mnt).status();
    let exe = dir.join("ytm-rs.app/Contents/MacOS/ytm-rs");
    anyhow::ensure!(copied.is_ok_and(|s| s.success()) && exe.exists(), "couldn't copy the app out of {}", dmg.display());
    Ok(exe)
}

/// The new binary from a downloaded update (the portable zip, or on Linux the
/// tarball), unpacked into `dir` with tar (on Windows, its own; macOS's reads zips).
fn unzip_exe(zip: &Path, dir: &Path) -> anyhow::Result<PathBuf> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)?;
    #[cfg(windows)]
    let mut cmd = {
        let system = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        std::process::Command::new(system.join(r"System32\tar.exe"))
    };
    #[cfg(not(windows))]
    let mut cmd = std::process::Command::new("tar");
    cmd.arg("-xf").arg(zip).arg("-C").arg(dir);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let status = cmd.status()?;
    #[cfg(windows)]
    let exe = dir.join("ytm-rs.exe");
    // Everything is in one top-level folder: the binary, or on macOS the app bundle.
    #[cfg(not(windows))]
    let exe = std::fs::read_dir(dir)?
        .flatten()
        .flat_map(|e| [e.path().join("ytm-rs"), e.path().join("ytm-rs.app/Contents/MacOS/ytm-rs")])
        .find(|p| p.is_file())
        .unwrap_or_else(|| dir.join("ytm-rs"));
    anyhow::ensure!(status.success() && exe.exists(), "couldn't unpack {}", zip.display());
    Ok(exe)
}

/// Run a cmd script detached and windowless, so it outlives this process.
fn run_detached(script: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        if let Err(e) = std::process::Command::new("cmd").arg("/C").raw_arg(script).creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS).spawn() {
            log::warn!("update: couldn't run the update: {e}");
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group, so it isn't taken down with this one.
        if let Err(e) = std::process::Command::new("sh").arg("-c").arg(script).process_group(0).spawn() {
            log::warn!("update: couldn't run the update: {e}");
        }
    }
}

fn client() -> anyhow::Result<reqwest::Client> {
    Ok(crate::proxy::apply(reqwest::Client::builder()).user_agent(concat!("ytm-rs/", env!("CARGO_PKG_VERSION"))).connect_timeout(std::time::Duration::from_secs(10)).read_timeout(std::time::Duration::from_secs(30)).build()?)
}

#[cfg(test)]
mod tests {
    use super::newer;

    #[test]
    fn version_order() {
        assert!(newer("v0.2.8", "0.2.7"));
        assert!(newer("v0.10.0", "0.9.9"));
        assert!(newer("1.0", "0.99.99"));
        assert!(!newer("v0.2.7", "0.2.7"));
        assert!(!newer("v0.2.6", "0.2.7"));
        assert!(!newer("", "0.2.7"));
    }
}
