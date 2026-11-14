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
//! place of its own as it closes. A copy from the Linux .deb (a `deb.txt` beside the
//! binary, in /usr/lib/ytm-rs) can't replace itself: it only says a new version is out
//! ([`State::Available`]), and the new .deb installs over it. The Arch package (a
//! `pacman.txt` there instead) does the same with the new package. Every platform has a portable build too (a
//! `portable.txt` marker; inside the app bundle on macOS), which updates from the
//! portable download. Dev builds never update themselves.
//!
//! Nothing waits a fixed time for the app to exit. The installer is run by a copy of
//! this exe staged in the updates folder ([`apply_main`]), which waits for this process
//! to exit, runs the installer, and starts the app again. A relaunch is passed this
//! process's id ([`start_after_exit`]) and waits for it to exit before going on.
//!
//! Release notes (the release's Markdown body) are kept per version: the one a check
//! finds, and any other on request ([`Updater::notes`]), for the update dialog, the
//! card shown after updating and Settings.
//!
//! Two channels: stable follows the latest published release; nightly also takes the
//! prereleases built from main every night there are new changes (tagged
//! `v<version>-nightly.<date>`, see the release workflow), whichever is newer. A nightly
//! build knows it is one from `YTM_NIGHTLY`, set when it was built. Switching back to
//! stable from a nightly installs the latest stable release, though its version is lower.
//!
//! Dev hooks: `YTM_UPDATE_FORCE=1` treats any build as installed; `YTM_UPDATE_AS=<ver>`
//! pretends to be that version; `YTM_UPDATE_DRYRUN=1` logs the install command
//! instead of running it; `YTM_UPDATE_FEED=<url>` reads the release (or the list of
//! releases) from there.

use std::cmp::Ordering;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

const LATEST: &str = "https://api.github.com/repos/grapedjuice/ytm-rs/releases/latest";
const ALL_RELEASES: &str = "https://api.github.com/repos/grapedjuice/ytm-rs/releases";
const BY_TAG: &str = "https://api.github.com/repos/grapedjuice/ytm-rs/releases/tags/v";
pub const RELEASES: &str = "https://github.com/grapedjuice/ytm-rs/releases";

/// The first argument of the staged helper (see [`apply_main`]).
const APPLY_FLAG: &str = "--apply-update";
/// `--after=<pid>`: wait for that process to exit first (see [`wait_for_predecessor`]).
const AFTER_FLAG: &str = "--after=";
/// The staged helper, and the note it leaves when an install fails, in the updates folder.
#[cfg_attr(not(windows), allow(dead_code))]
const HELPER: &str = "update.exe";
const FAILED: &str = "install-failed.txt";

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
    /// A newer version is out, for a copy that can't install it itself (the .deb):
    /// `url` is its .deb.
    Available { version: String, url: String },
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
    /// The nightly channel, rather than stable.
    nightly: Arc<AtomicBool>,
}

/// What a nightly's version has between the base version and its build number.
const NIGHTLY: &str = "-nightly.";

/// The version this was built as: a nightly build adds its build number.
pub fn build_version() -> String {
    match option_env!("YTM_NIGHTLY").filter(|n| !n.is_empty()) {
        Some(n) => format!("{}{NIGHTLY}{n}", env!("CARGO_PKG_VERSION")),
        None => env!("CARGO_PKG_VERSION").to_owned(),
    }
}

pub fn current_version() -> String {
    std::env::var("YTM_UPDATE_AS").unwrap_or_else(|_| build_version())
}

/// Whether `version` is a nightly build's.
pub fn is_nightly(version: &str) -> bool {
    version.contains(NIGHTLY)
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

/// Whether this copy was installed from the .deb, which marks it with `deb.txt` beside
/// the binary. Apt owns it, so it never replaces itself.
pub fn deb() -> bool {
    static DEB: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DEB.get_or_init(|| cfg!(target_os = "linux") && std::env::current_exe().ok().and_then(|e| Some(e.parent()?.join("deb.txt").exists())).unwrap_or(false))
}

/// Whether this copy was installed from the Arch package, which marks it with
/// `pacman.txt` beside the binary. Pacman owns it, so like the .deb it never replaces
/// itself.
pub fn pacman() -> bool {
    static PACMAN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PACMAN.get_or_init(|| cfg!(target_os = "linux") && std::env::current_exe().ok().and_then(|e| Some(e.parent()?.join("pacman.txt").exists())).unwrap_or(false))
}

/// Whether a package manager owns this copy (the .deb or the Arch package): it only
/// says a new version is out.
pub fn packaged() -> bool {
    deb() || pacman()
}

/// What to do with the new version, for a copy a package manager owns.
pub fn package_step() -> &'static str {
    if pacman() {
        "Download the new package and install it over this one with sudo pacman -U."
    } else {
        "Download the new .deb and open it to install it over this one."
    }
}

/// On Linux, another copy installed the other way (one from install.sh beside the
/// .deb's or the Arch package's, or the other way round), which would otherwise be left
/// behind: the menu and the `ytm-rs` command open the install.sh one. What it's from,
/// and the command that removes it.
pub fn other_copy() -> Option<(&'static str, &'static str)> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    if packaged() {
        home.join(".local/lib/ytm-rs/install.txt").exists().then_some(("install.sh", "~/.local/lib/ytm-rs/install.sh --uninstall"))
    } else if !installed() {
        None
    } else if Path::new("/usr/lib/ytm-rs/deb.txt").exists() {
        Some(("the .deb", "sudo apt remove ytm-rs"))
    } else {
        Path::new("/usr/lib/ytm-rs/pacman.txt").exists().then_some(("the Arch package", "sudo pacman -R ytm-rs"))
    }
}

/// A version as the .deb has it: a nightly (`0.3.0-nightly.20261111`) comes after its
/// version there too (`0.3.0+nightly.20261111`).
pub fn deb_version(version: &str) -> String {
    version.replacen(NIGHTLY, "+nightly.", 1)
}

/// The Arch package's file, as linux/arch.sh builds it: pkgver can't have a hyphen, so
/// a nightly (`0.3.0-nightly.20261111`) is `0.3.0.nightly.20261111`, which pacman puts
/// after its version too.
fn pacman_package(version: &str, arch: &str) -> String {
    format!("ytm-rs-{}-1-{arch}.pkg.tar.zst", version.replace('-', "."))
}

/// The `.app` bundle a binary is in (`X.app/Contents/MacOS/ytm-rs`).
fn bundle_of(exe: &Path) -> Option<&Path> {
    exe.ancestors().nth(3).filter(|a| a.extension().is_some_and(|x| x == "app"))
}

/// The release asset this copy updates from.
fn asset_name(version: &str) -> String {
    let portable = portable_dir().is_some();
    let arch = std::env::consts::ARCH;
    if cfg!(target_os = "linux") && pacman() {
        return pacman_package(version, arch);
    }
    if cfg!(target_os = "linux") && deb() {
        let arch = if arch == "x86_64" { "amd64" } else if arch == "aarch64" { "arm64" } else { arch };
        return format!("ytm-rs_{}_{arch}.deb", deb_version(version));
    }
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

/// Remove the exe a portable update replaced (it was still running then), and any
/// new one a failed swap left beside it.
pub fn clean_portable() {
    if portable_dir().is_some()
        && let Ok(exe) = std::env::current_exe()
    {
        let _ = std::fs::remove_file(exe.with_extension("exe.old"));
        let _ = std::fs::remove_file(exe.with_extension("exe.new"));
    }
}

/// A relaunch (`--after=<pid>`) waits for the copy that started it to exit, so the two
/// never run at once. Before anything else, even the single-instance handoff.
pub fn wait_for_predecessor() {
    let pid = std::env::args_os().skip(1).find_map(|a| a.to_str()?.strip_prefix(AFTER_FLAG)?.parse().ok());
    if let Some(pid) = pid {
        wait_for_exit(pid);
    }
}

/// Start `exe` once this process has exited: it waits for this one
/// ([`wait_for_predecessor`]), so this process can take as long as it needs to close.
pub fn start_after_exit(exe: &Path) -> bool {
    let after = format!("{AFTER_FLAG}{}", std::process::id());
    #[cfg(target_os = "macos")]
    if let Some(app) = bundle_of(exe) {
        // Through Launch Services; `-n` starts a new copy while this one is still up.
        return spawn(Command::new("open").arg("-n").arg(app).arg("--args").arg(&after));
    }
    spawn(Command::new(exe).arg(after))
}

/// Wait for process `pid` to exit, however long it takes: going ahead early would run
/// two copies at once (or hand off to one that's closing).
#[cfg(windows)]
fn wait_for_exit(pid: u32) {
    use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetProcessTimes, INFINITE, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    // SAFETY: out-params of the right types, and a handle opened, waited on and closed
    // here. One that can't be opened belongs to a process that has already exited.
    unsafe {
        let started = |h: HANDLE| {
            let [mut created, mut exited, mut kernel, mut user] = [FILETIME::default(); 4];
            let _ = GetProcessTimes(h, &mut created, &mut exited, &mut kernel, &mut user);
            (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime)
        };
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            // One started after this process has the id only because ours has exited.
            if started(h) <= started(GetCurrentProcess()) {
                let _ = WaitForSingleObject(h, INFINITE);
            }
            let _ = CloseHandle(h);
        }
    }
}

#[cfg(unix)]
fn wait_for_exit(pid: u32) {
    // Started straight from it (always, but for `open` on macOS), this process is its
    // child until it exits: as a zombie it would still answer `kill`. Through `open`,
    // it's gone once `kill` can't find it.
    let child = cfg!(not(target_os = "macos")) || std::os::unix::process::parent_id() == pid;
    // SAFETY: signal 0 only checks that the process exists.
    let alive = || if child { std::os::unix::process::parent_id() == pid } else { unsafe { libc::kill(pid as libc::pid_t, 0) == 0 } };
    while alive() {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// The staged helper: when this process was started as one, apply the update and
/// return true (the caller exits). Arguments: the old process's id, the installer, the
/// folder to install to, `/ALLUSERS` or `/CURRENTUSER`, and `1` to start the app after.
pub fn apply_main() -> bool {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.first().is_none_or(|a| a != APPLY_FLAG) {
        return false;
    }
    #[cfg(windows)]
    if let [pid, installer, dir, scope, relaunch] = &args[1..] {
        wait_for_exit(pid.to_str().and_then(|p| p.parse().ok()).unwrap_or(0));
        let installer = Path::new(installer);
        let updates = installer.parent().unwrap_or(Path::new("."));
        let arg = |name: &str, value: &std::ffi::OsStr| {
            let mut a = OsString::from(name);
            a.push(value);
            a
        };
        let status = Command::new(installer)
            .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
            .arg(scope)
            .arg(arg("/DIR=", dir.as_os_str()))
            .arg(arg("/LOG=", updates.join("install.log").as_os_str()))
            .status();
        // The next launch logs it (this process has no log of its own).
        let failed = match status {
            Ok(s) if s.success() => None,
            Ok(s) => Some(format!("the installer exited with {s}")),
            Err(e) => Some(format!("couldn't run the installer: {e}")),
        };
        if let Some(why) = failed {
            let _ = std::fs::write(updates.join(FAILED), why);
        }
        // Even after a failure: the installer rolls back, so the old version is still there.
        // After this helper exits, so the next launch can delete it.
        let exe = Path::new(dir).join("ytm-rs.exe");
        if relaunch == "1" && exe.exists() {
            start_after_exit(&exe);
        }
    }
    true
}

/// Start a program that outlives this process; false if it couldn't be.
fn spawn(cmd: &mut Command) -> bool {
    cmd.stdin(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group, so it isn't taken down with this one.
        cmd.process_group(0);
    }
    cmd.spawn().inspect_err(|e| log::warn!("update: couldn't start {}: {e}", cmd.get_program().display())).is_ok()
}

/// `a` is newer than `b` (dotted numeric versions, a leading "v" ignored). A nightly
/// (`0.2.34-nightly.20261110`) is built from main after that version's release, so it's
/// newer than the release and older than the next one.
pub fn newer(a: &str, b: &str) -> bool {
    let nums = |s: &str| s.split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    let parse = |v: &str| {
        let v = v.trim_start_matches('v');
        match v.split_once(NIGHTLY) {
            Some((base, build)) => (nums(base), Some(nums(build))),
            None => (nums(v), None),
        }
    };
    let cmp = |a: &[u64], b: &[u64]| {
        (0..a.len().max(b.len())).map(|i| a.get(i).copied().unwrap_or(0).cmp(&b.get(i).copied().unwrap_or(0))).find(|o| o.is_ne()).unwrap_or(Ordering::Equal)
    };
    let (a, b) = (parse(a), parse(b));
    match cmp(&a.0, &b.0) {
        Ordering::Equal => match (a.1, b.1) {
            (Some(x), Some(y)) => cmp(&x, &y).is_gt(),
            (Some(_), None) => true,
            _ => false,
        },
        o => o.is_gt(),
    }
}

/// The release to update to from what the feed returned: a single release, or the
/// list of them, from which the newest stable one, or on the nightly channel the newest
/// stable or nightly one. Other prereleases (test builds) are never picked.
fn pick_release(feed: serde_json::Value, nightly: bool) -> Option<serde_json::Value> {
    let serde_json::Value::Array(list) = feed else { return Some(feed) };
    let tag = |r: &serde_json::Value| r["tag_name"].as_str().unwrap_or_default().to_owned();
    list.into_iter()
        .filter(|r| !r["draft"].as_bool().unwrap_or(false))
        .filter(|r| if r["prerelease"].as_bool().unwrap_or(false) { nightly && is_nightly(&tag(r)) } else { true })
        .reduce(|best, r| if newer(&tag(&r), &tag(&best)) { r } else { best })
}

/// Whether `tag` is the one to install over `current`: newer, or on the stable channel,
/// the stable release in place of a nightly build.
fn wanted(tag: &str, current: &str, nightly: bool) -> bool {
    newer(tag, current) || (!nightly && is_nightly(current) && !is_nightly(tag) && !tag.is_empty() && tag.trim_start_matches('v') != current)
}

impl Updater {
    pub fn new(data_dir: &Path, nightly: bool, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let dir = data_dir.join("updates");
        // What the last install's helper left behind.
        #[cfg(windows)]
        let _ = std::fs::remove_file(dir.join(HELPER));
        if let Ok(why) = std::fs::read_to_string(dir.join(FAILED)) {
            log::warn!("update: the last update didn't install: {why} (see {})", dir.join("install.log").display());
            let _ = std::fs::remove_file(dir.join(FAILED));
        }
        Self {
            state: Arc::new(Mutex::new(State::Idle)),
            dir,
            wake: Arc::new(wake),
            notes: Default::default(),
            downloads: Default::default(),
            nightly: Arc::new(AtomicBool::new(nightly)),
        }
    }

    /// Switch channel. What was found or downloaded for the other one is dropped; a
    /// check under way starts over on the new channel when it ends.
    pub fn set_nightly(&self, nightly: bool) {
        if self.nightly.swap(nightly, Relaxed) == nightly {
            return;
        }
        let mut state = self.state.lock().unwrap();
        if !matches!(*state, State::Checking | State::Downloading { .. }) {
            *state = State::Idle;
        }
        drop(state);
        (self.wake)();
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
            loop {
                let nightly = this.nightly.load(Relaxed);
                let result = rt.block_on(this.run(nightly));
                // The channel changed while this ran: check again on the new one.
                if this.nightly.load(Relaxed) != nightly {
                    continue;
                }
                match result {
                    Ok(s) => this.set(s),
                    Err(e) => {
                        log::warn!("update check failed: {e:#}");
                        this.set(State::Failed(format!("{e:#}")));
                    }
                }
                break;
            }
        });
    }

    async fn run(&self, nightly: bool) -> anyhow::Result<State> {
        let http = client()?;
        // Nightlies are prereleases, which the latest release never is: they're in the list.
        let feed = std::env::var("YTM_UPDATE_FEED").unwrap_or_else(|_| if nightly { format!("{ALL_RELEASES}?per_page=30") } else { LATEST.to_owned() });
        let feed: serde_json::Value = http.get(feed).send().await?.error_for_status()?.json().await?;
        let release = pick_release(feed, nightly).ok_or_else(|| anyhow::anyhow!("no releases found"))?;
        let tag = release["tag_name"].as_str().unwrap_or_default();
        if let Some(body) = release["body"].as_str() {
            self.notes.lock().unwrap().insert(tag.trim_start_matches('v').to_owned(), Some(body.to_owned()));
        }
        let current = current_version();
        if !wanted(tag, &current, nightly) {
            log::info!("update: {current} is current (latest {tag}, {} channel)", if nightly { "nightly" } else { "stable" });
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
        if packaged() {
            log::info!("update: {version} is out");
            return Ok(State::Available { version, url: url.to_owned() });
        }
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
                let percent = (got * 100).checked_div(size).map_or(0, |p| p.min(100) as u8);
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
        if !self.apply(&installer, &exe, relaunch) && relaunch {
            // Nothing changed: start this version again.
            start_after_exit(&exe);
        }
    }

    /// Replace the app (and start it after, if `relaunch`); false if it couldn't be.
    #[cfg(unix)]
    fn apply(&self, installer: &Path, exe: &Path, relaunch: bool) -> bool {
        #[cfg(target_os = "macos")]
        if let (Some(new), Some(app)) = (bundle_of(installer), bundle_of(exe)) {
            return replace_bundle(new, app, relaunch);
        }
        replace_binary(installer, exe, relaunch)
    }

    #[cfg(windows)]
    fn apply(&self, installer: &Path, exe: &Path, relaunch: bool) -> bool {
        if portable_dir().is_some() {
            return swap_exe(installer, exe, relaunch);
        }
        // Update the copy that's running, where it is (an all-users install asks for
        // admin rights).
        let Some(dir) = exe.parent() else { return false };
        let scope = if all_users(dir) { "/ALLUSERS" } else { "/CURRENTUSER" };
        // A copy of this exe runs it, as this one can't be replaced while it runs.
        let helper = self.dir.join(HELPER);
        let mut cmd = Command::new(&helper);
        cmd.arg(APPLY_FLAG).arg(std::process::id().to_string()).arg(installer).arg(dir).arg(scope).arg(if relaunch { "1" } else { "0" });
        if std::env::var_os("YTM_UPDATE_DRYRUN").is_some() {
            log::info!("update (dry run): {cmd:?}");
            return true;
        }
        let _ = std::fs::remove_file(&helper);
        if let Err(e) = std::fs::copy(exe, &helper) {
            log::warn!("update: couldn't stage the updater: {e}");
            return false;
        }
        log::info!("update: installing {}", installer.display());
        spawn(&mut cmd)
    }
}

/// Whether the copy in `dir` is the all-users install: the installer records where it
/// put that one under HKLM (else it's under Program Files, where per-user ones aren't).
#[cfg(windows)]
fn all_users(dir: &Path) -> bool {
    use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::w;
    let mut buf = [0u16; 1024];
    let mut size = std::mem::size_of_val(&buf) as u32;
    // SAFETY: an out-buffer of the size passed; the strings are static. The key is the
    // installer's AppId.
    let read = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{6E0B7C4A-2D1F-4C8E-9B3A-7F5D1E2A9C40}_is1"),
            w!("InstallLocation"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let recorded = String::from_utf16_lossy(&buf[..len]);
    let here = dir.to_string_lossy();
    let program_files = std::env::var_os("ProgramFiles").map(PathBuf::from);
    (read.is_ok() && recorded.trim_end_matches('\\').eq_ignore_ascii_case(here.trim_end_matches('\\'))) || program_files.is_some_and(|p| dir.starts_with(p))
}

/// The portable update: Windows lets a running exe be renamed, so move this one aside
/// and put the new one in its place (the next launch deletes the old one). The new one
/// is copied beside it first, so the exe is only ever renamed: never missing or half
/// written.
#[cfg(windows)]
fn swap_exe(new: &Path, exe: &Path, relaunch: bool) -> bool {
    let (old, next) = (exe.with_extension("exe.old"), exe.with_extension("exe.new"));
    if std::env::var_os("YTM_UPDATE_DRYRUN").is_some() {
        log::info!("update (dry run): {} aside to {}, {} in its place", exe.display(), old.display(), new.display());
        return true;
    }
    let _ = std::fs::remove_file(&old);
    if let Err(e) = std::fs::copy(new, &next) {
        let _ = std::fs::remove_file(&next);
        log::warn!("update: couldn't copy the new version: {e}");
        return false;
    }
    if let Err(e) = std::fs::rename(exe, &old) {
        let _ = std::fs::remove_file(&next);
        log::warn!("update: couldn't move {} aside: {e}", exe.display());
        return false;
    }
    if let Err(e) = std::fs::rename(&next, exe) {
        let _ = std::fs::rename(&old, exe);
        let _ = std::fs::remove_file(&next);
        log::warn!("update: couldn't put the new version in place: {e}");
        return false;
    }
    log::info!("update: replaced {}", exe.display());
    if relaunch {
        start_after_exit(exe);
    }
    true
}

/// Linux: rename the new binary over this one. The running process keeps its copy
/// open until it exits; the next launch runs the new one.
#[cfg(unix)]
fn replace_binary(new: &Path, exe: &Path, relaunch: bool) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let tmp = exe.with_file_name(".ytm-rs.new");
    if std::env::var_os("YTM_UPDATE_DRYRUN").is_some() {
        log::info!("update (dry run): {} over {}", new.display(), exe.display());
        return true;
    }
    let done = std::fs::copy(new, &tmp)
        .and_then(|_| std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)))
        .and_then(|_| std::fs::rename(&tmp, exe));
    if let Err(e) = done {
        let _ = std::fs::remove_file(&tmp);
        log::warn!("update: couldn't replace {}: {e}", exe.display());
        return false;
    }
    log::info!("update: replaced {}", exe.display());
    if relaunch {
        start_after_exit(exe);
    }
    true
}

/// macOS: put the new app bundle where this one is, so its Info.plist and icon update
/// too. A running app's bundle can be moved; this process keeps running from the old
/// one until it exits.
#[cfg(target_os = "macos")]
fn replace_bundle(new: &Path, app: &Path, relaunch: bool) -> bool {
    let parent = app.parent().unwrap_or(Path::new("/"));
    let (tmp, old) = (parent.join(".ytm-rs.app.new"), parent.join(".ytm-rs.app.old"));
    if std::env::var_os("YTM_UPDATE_DRYRUN").is_some() {
        log::info!("update (dry run): {} in place of {}", new.display(), app.display());
        return true;
    }
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_dir_all(&old);
    // ditto copies a bundle whole: symlinks, permissions, signature.
    let copied = std::process::Command::new("ditto").arg(new).arg(&tmp).status().is_ok_and(|s| s.success());
    if !copied {
        let _ = std::fs::remove_dir_all(&tmp);
        log::warn!("update: couldn't copy {} beside {}", new.display(), app.display());
        return false;
    }
    if let Err(e) = std::fs::rename(app, &old) {
        let _ = std::fs::remove_dir_all(&tmp);
        log::warn!("update: couldn't move {} aside: {e}", app.display());
        return false;
    }
    if let Err(e) = std::fs::rename(&tmp, app) {
        let _ = std::fs::rename(&old, app);
        let _ = std::fs::remove_dir_all(&tmp);
        log::warn!("update: couldn't put the new version in place: {e}");
        return false;
    }
    let _ = std::fs::remove_dir_all(&old);
    log::info!("update: replaced {}", app.display());
    if relaunch {
        start_after_exit(&app.join("Contents/MacOS/ytm-rs"));
    }
    true
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

fn client() -> anyhow::Result<reqwest::Client> {
    Ok(crate::proxy::apply(reqwest::Client::builder()).user_agent(concat!("ytm-rs/", env!("CARGO_PKG_VERSION"))).connect_timeout(std::time::Duration::from_secs(10)).read_timeout(std::time::Duration::from_secs(30)).build()?)
}

#[cfg(test)]
mod tests {
    use super::{deb_version, newer, pacman_package, pick_release, wanted};
    use serde_json::json;

    #[test]
    fn deb_versions() {
        assert_eq!(deb_version("0.3.0"), "0.3.0");
        assert_eq!(deb_version("0.3.0-nightly.20261111"), "0.3.0+nightly.20261111");
        assert_eq!(deb_version("0.3.0-nightly.20261111.2"), "0.3.0+nightly.20261111.2");
    }

    /// The names makepkg gives linux/arch.sh's package, which the Arch copy looks for.
    #[test]
    fn pacman_packages() {
        assert_eq!(pacman_package("0.3.0", "x86_64"), "ytm-rs-0.3.0-1-x86_64.pkg.tar.zst");
        assert_eq!(pacman_package("0.3.0-nightly.20261111", "x86_64"), "ytm-rs-0.3.0.nightly.20261111-1-x86_64.pkg.tar.zst");
        assert_eq!(pacman_package("0.3.0-nightly.20261111.2", "x86_64"), "ytm-rs-0.3.0.nightly.20261111.2-1-x86_64.pkg.tar.zst");
    }

    #[test]
    fn version_order() {
        assert!(newer("v0.2.8", "0.2.7"));
        assert!(newer("v0.10.0", "0.9.9"));
        assert!(newer("1.0", "0.99.99"));
        assert!(!newer("v0.2.7", "0.2.7"));
        assert!(!newer("v0.2.6", "0.2.7"));
        assert!(!newer("", "0.2.7"));
    }

    #[test]
    fn nightly_order() {
        // After its version's release, before the next one.
        assert!(newer("v0.2.34-nightly.20261110", "0.2.34"));
        assert!(!newer("v0.2.34", "0.2.34-nightly.20261110"));
        assert!(newer("v0.2.35", "0.2.34-nightly.20261110"));
        assert!(!newer("v0.2.34-nightly.20261110", "0.2.35"));
        // By build, then a second build the same day.
        assert!(newer("v0.2.34-nightly.20261111", "0.2.34-nightly.20261110"));
        assert!(newer("v0.2.34-nightly.20261110.2", "0.2.34-nightly.20261110"));
        assert!(!newer("v0.2.34-nightly.20261110", "0.2.34-nightly.20261110"));
        assert!(newer("v0.2.35-nightly.20261101", "0.2.34-nightly.20261130"));
    }

    #[test]
    fn channels() {
        let r = |tag: &str, pre: bool| json!({ "tag_name": tag, "prerelease": pre, "draft": false });
        let list = json!([r("test-wayland-tray-1", true), r("v0.2.34-nightly.20261110", true), r("v0.2.34", false), r("v0.2.33", false)]);
        let tag = |nightly| pick_release(list.clone(), nightly).unwrap()["tag_name"].as_str().unwrap().to_owned();
        assert_eq!(tag(false), "v0.2.34");
        assert_eq!(tag(true), "v0.2.34-nightly.20261110");
        // A stable release after the nightly wins on the nightly channel too.
        let list = json!([r("v0.2.35", false), r("v0.2.34-nightly.20261110", true)]);
        assert_eq!(pick_release(list, true).unwrap()["tag_name"], "v0.2.35");
        // Back on stable from a nightly: the stable release, though older.
        assert!(wanted("v0.2.34", "0.2.34-nightly.20261110", false));
        assert!(!wanted("v0.2.34", "0.2.34-nightly.20261110", true));
        assert!(!wanted("v0.2.34", "0.2.34", false));
        assert!(wanted("v0.2.34-nightly.20261111", "0.2.34-nightly.20261110", true));
    }
}
