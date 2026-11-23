//! Last-resort stream resolution through yt-dlp: age-restricted songs on accounts
//! without Premium (whose signed-in web streams need a PO token), or when the native
//! signed-in path in `webmusic` breaks. yt-dlp solves the URL challenges with Node or
//! Deno, which it can't get the audio without; where neither is installed, with the
//! Node fetched for it (`node`), and with QuickJS while that's on its way.
//!
//! yt-dlp is downloaded into the app's data folder on first use and refreshed
//! weekly; QuickJS likewise, only where there's no Node or Deno, and monthly. Both come
//! from their latest GitHub release and must match the SHA-256 its API gives. The signed-in session is passed as a Netscape cookies file that exists
//! only for the duration of the call.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use serde_json::Value;
use sha2::{Digest, Sha256};

#[cfg(windows)]
const BIN: &str = "yt-dlp.exe";
#[cfg(not(windows))]
const BIN: &str = "yt-dlp";
#[cfg(all(windows, not(target_arch = "aarch64")))]
const ASSET: &str = "yt-dlp.exe";
#[cfg(all(windows, target_arch = "aarch64"))]
const ASSET: &str = "yt-dlp_arm64.exe";
#[cfg(target_os = "macos")]
const ASSET: &str = "yt-dlp_macos";
#[cfg(all(unix, not(target_os = "macos")))]
const ASSET: &str = "yt-dlp_linux";

const REPO: &str = "yt-dlp/yt-dlp";

const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

/// QuickJS-ng's interpreter, for yt-dlp to solve the challenges with when there's no Node
/// or Deno. No build for Windows on Arm: the x64 one runs there.
#[cfg(windows)]
const QJS: &str = "qjs.exe";
#[cfg(not(windows))]
const QJS: &str = "qjs";
#[cfg(all(windows, target_arch = "x86"))]
const QJS_ASSET: &str = "qjs-windows-x86.exe";
#[cfg(all(windows, not(target_arch = "x86")))]
const QJS_ASSET: &str = "qjs-windows-x86_64.exe";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const QJS_ASSET: &str = "qjs-darwin-arm64";
#[cfg(all(target_os = "macos", not(target_arch = "aarch64")))]
const QJS_ASSET: &str = "qjs-darwin-x86_64";
#[cfg(all(unix, not(target_os = "macos"), target_arch = "aarch64"))]
const QJS_ASSET: &str = "qjs-linux-aarch64";
#[cfg(all(unix, not(target_os = "macos"), not(target_arch = "aarch64")))]
const QJS_ASSET: &str = "qjs-linux-x86_64";

const QJS_REPO: &str = "quickjs-ng/quickjs";

const QJS_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 3600);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Format {
    File,
    SafariHls,
}

pub struct Stream {
    pub url: String,
    pub size: u64,
    pub duration_ms: u64,
    pub user_agent: String,
    /// youtube.com cookies yt-dlp saved changed, as `Set-Cookie` lines.
    pub replaced: Vec<String>,
}

/// Path to an up-to-date `name` in the data folder, downloading `asset` from the latest
/// release of GitHub's `repo` if it's missing or older than `max_age`. When a newer one
/// can't be fetched, the one there goes on being used, and the next song tries again.
async fn ensure(http: &reqwest::Client, data_dir: &Path, name: &str, repo: &str, asset: &str, max_age: Duration) -> anyhow::Result<PathBuf> {
    let bin = data_dir.join(name);
    let fresh = std::fs::metadata(&bin)
        .and_then(|m| m.modified())
        .is_ok_and(|t| SystemTime::now().duration_since(t).unwrap_or_default() < max_age);
    if fresh {
        return Ok(bin);
    }
    match fetch(http, &bin, repo, asset).await {
        Err(e) if bin.is_file() => log::warn!("couldn't fetch a newer {name}, using the one here: {e:#}"),
        r => r?,
    }
    Ok(bin)
}

/// Downloads `asset` from `repo`'s latest release to `bin`, checked against the SHA-256
/// GitHub's API gives for it: it's a program this app runs, with the session's cookies.
async fn fetch(http: &reqwest::Client, bin: &Path, repo: &str, asset: &str) -> anyhow::Result<()> {
    let get = |url: String, secs: u64| async move { http.get(url).timeout(Duration::from_secs(secs)).send().await?.error_for_status() };
    let release: Value = crate::update::github_api(http, &format!("https://api.github.com/repos/{repo}/releases/latest")).await?.json().await?;
    let (url, want) = download_of(&release, asset).ok_or_else(|| anyhow::anyhow!("{repo}'s latest release has no {asset} with a SHA-256"))?;
    log::info!("downloading {asset}");
    let bytes = get(url, 120).await?.bytes().await?;
    let got = format!("{:x}", Sha256::digest(&bytes));
    anyhow::ensure!(got == want, "{asset}'s SHA-256 is {got}, not {want}");
    let tmp = bin.with_extension("part");
    std::fs::write(&tmp, &bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, bin)?;
    Ok(())
}

/// Where a release has `asset`, and its SHA-256.
fn download_of(release: &Value, asset: &str) -> Option<(String, String)> {
    let a = release["assets"].as_array()?.iter().find(|a| a["name"] == asset)?;
    Some((a["browser_download_url"].as_str()?.to_owned(), crate::update::asset_sha256(a)?))
}

/// Cookie files a crash left behind, with the session in them: nothing's using them at launch.
pub fn remove_leftover_cookies(data_dir: &Path) {
    let Ok(files) = std::fs::read_dir(data_dir) else { return };
    for f in files.flatten() {
        let name = f.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("cookies-") && name.ends_with(".txt") {
            let _ = std::fs::remove_file(f.path());
        }
    }
}

/// Whether yt-dlp will find a JavaScript runtime of its own: Node or Deno on the `PATH`.
fn has_runtime() -> bool {
    let exe = |name: &str| if cfg!(windows) { format!("{name}.exe") } else { name.to_owned() };
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|dir| ["node", "deno"].iter().any(|n| dir.join(exe(n)).is_file()))
}

pub async fn resolve(http: &reqwest::Client, data_dir: &Path, cookie: &str, video_id: &str, format: Format) -> anyhow::Result<Stream> {
    let bin = ensure(http, data_dir, BIN, REPO, ASSET, MAX_AGE).await?;
    // Signed in, yt-dlp can't get at the audio without solving YouTube's challenges: with
    // Node or Deno installed, or the Node fetched for it. Until that's here, QuickJS, at
    // about twice the time a song.
    let runtime = if has_runtime() {
        None
    } else if let Some(node) = crate::node::installed(http, data_dir) {
        Some(format!("node:{}", node.display()))
    } else {
        crate::node::fetch_later(http, data_dir);
        match ensure(http, data_dir, QJS, QJS_REPO, QJS_ASSET, QJS_MAX_AGE).await {
            Ok(p) => Some(format!("quickjs:{}", p.display())),
            Err(e) => {
                log::warn!("no Node or Deno, and QuickJS couldn't be fetched: {e:#}");
                None
            }
        }
    };
    let data_dir = data_dir.to_owned();
    let (cookie, video_id) = (cookie.to_owned(), video_id.to_owned());
    let mut s = tokio::task::spawn_blocking(move || run(&bin, runtime.as_deref(), &data_dir, &video_id, &cookie, format)).await??;
    // The range downloader needs the exact size; yt-dlp sometimes only estimates it.
    if format == Format::File && s.size == 0 {
        // Errors leave the URL out: it carries the client's IP address and signatures (logs).
        let head = http.head(&s.url).header("User-Agent", &s.user_agent).send().await.and_then(reqwest::Response::error_for_status).map_err(reqwest::Error::without_url)?;
        // Not `content_length()`: for a HEAD response that reports the empty body.
        let len = head.headers().get(reqwest::header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()?.parse::<u64>().ok());
        s.size = len.filter(|&n| n > 0).ok_or_else(|| anyhow::anyhow!("unknown stream size"))?;
    }
    Ok(s)
}

/// `runtime`: a JavaScript runtime yt-dlp is told about (`node:<path>`, `quickjs:<path>`)
/// when there's no Node or Deno on the `PATH` for it to find.
fn run(bin: &Path, runtime: Option<&str>, data_dir: &Path, video_id: &str, cookie: &str, format: Format) -> anyhow::Result<Stream> {
    // Unique name per call; removed on every exit path by the guard below.
    let nonce = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let jar = data_dir.join(format!("cookies-{nonce}.txt"));
    struct Remove<'a>(&'a Path);
    impl Drop for Remove<'_> {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.0);
        }
    }
    let _guard = Remove(&jar);
    let mut txt = String::from("# Netscape HTTP Cookie File\n");
    for kv in cookie.split(';') {
        if let Some((k, v)) = kv.trim().split_once('=') {
            txt.push_str(&format!(".youtube.com\tTRUE\t/\tTRUE\t2147483647\t{k}\t{v}\n"));
        }
    }
    std::fs::write(&jar, txt)?;

    let mut cmd = Command::new(bin);
    let selector = match format {
        Format::File => "140/bestaudio[ext=m4a]",
        Format::SafariHls => {
            cmd.args(["--extractor-args", "youtube:player_client=web_safari"]);
            // The smallest video rendition with AAC-LC. Lower ones use HE-AAC.
            "worst[protocol^=m3u8][acodec=mp4a.40.2]"
        }
    };
    cmd.args(["--js-runtimes", "node", "--no-warnings", "--no-playlist", "--no-cache-dir", "-f", selector, "-j", "--cookies"])
        .arg(&jar)
        .args(runtime.into_iter().flat_map(|r| ["--js-runtimes", r]))
        .args(crate::proxy::ytdlp_args())
        .arg(format!("https://music.youtube.com/watch?v={video_id}"));
    crate::proxy::ytdlp_env(&mut cmd);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    // Its own process group, so the whole tree (the onefile binary unpacks into a child,
    // plus Node) can be ended together.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    // A stuck run (yt-dlp or its Node child hung on the network) would leave the song
    // buffering for good: give it a minute and a half, then end its process tree.
    let child = cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn()?;
    let (pid, done) = (child.id(), std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));
    let watch = done.clone();
    std::thread::spawn(move || {
        for _ in 0..90 {
            std::thread::sleep(Duration::from_secs(1));
            if watch.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
        }
        log::warn!("yt-dlp took too long; stopping it");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let mut kill = Command::new("taskkill");
            kill.args(["/PID", &pid.to_string(), "/T", "/F"]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
            kill.creation_flags(0x0800_0000);
            let _ = kill.status();
        }
        // The process group it leads (see `process_group` above).
        #[cfg(unix)]
        unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGKILL);
        }
    });
    let out = child.wait_with_output();
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    let out = out?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err.lines().rfind(|l| l.contains("ERROR")).unwrap_or("yt-dlp failed");
        // No runtime to solve the challenges with: only the video's preview images were left.
        if line.contains("Requested format is not available") && runtime.is_none() && !has_runtime() {
            anyhow::bail!("this song needs Node.js (nodejs.org) installed to play");
        }
        anyhow::bail!("{}", line.trim_start_matches("ERROR: ").trim());
    }
    let v: Value = serde_json::from_slice(&out.stdout)?;
    // yt-dlp writes the cookies back, with any Google replaced while it ran.
    let replaced = std::fs::read_to_string(&jar)
        .unwrap_or_default()
        .lines()
        // HttpOnly cookies (the session ones) may be written as `#HttpOnly_.youtube.com…`.
        .map(|l| l.strip_prefix("#HttpOnly_").unwrap_or(l))
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f.len() >= 7 && f[0].trim_start_matches('.').eq_ignore_ascii_case("youtube.com")).then(|| format!("{}={}; Domain=.youtube.com", f[5], f[6]))
        })
        .collect();
    Ok(Stream {
        replaced,
        url: v["url"].as_str().ok_or_else(|| anyhow::anyhow!("yt-dlp returned no URL"))?.to_owned(),
        size: v["filesize"].as_u64().unwrap_or(0),
        duration_ms: v["duration"].as_f64().map_or(0, |d| (d * 1000.0) as u64),
        user_agent: v["http_headers"]["User-Agent"].as_str().unwrap_or_default().to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_says_where_its_asset_is_and_its_hash() {
        let release = serde_json::json!({ "assets": [
            { "name": "yt-dlp", "browser_download_url": "https://example.com/yt-dlp", "digest": "sha256:11" },
            { "name": "yt-dlp_linux", "browser_download_url": "https://example.com/yt-dlp_linux", "digest": "sha256:AB12" },
            { "name": "yt-dlp_macos", "browser_download_url": "https://example.com/yt-dlp_macos", "digest": null },
        ] });
        assert_eq!(download_of(&release, "yt-dlp_linux"), Some(("https://example.com/yt-dlp_linux".to_owned(), "ab12".to_owned())));
        // Without a hash to check it against, it isn't downloaded.
        assert_eq!(download_of(&release, "yt-dlp_macos"), None);
        assert_eq!(download_of(&release, "yt-dlp.exe"), None);
    }

    /// The real downloads, into an empty folder: both land there, checked, and run.
    /// `cargo test -- --ignored ytdlp_and_quickjs_fetch --nocapture`
    #[test]
    #[ignore]
    fn ytdlp_and_quickjs_fetch() {
        let dir = std::env::temp_dir().join("ytm-rs-ytdlp-fetch-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let http = reqwest::Client::builder().user_agent("ytm-rs").build().unwrap();
            for (name, repo, asset, flag) in [(BIN, REPO, ASSET, "--version"), (QJS, QJS_REPO, QJS_ASSET, "--help")] {
                let bin = ensure(&http, &dir, name, repo, asset, MAX_AGE).await.unwrap();
                let out = std::process::Command::new(&bin).arg(flag).output().unwrap();
                println!("{name}: {}", String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or_default());
                assert!(!out.stdout.is_empty(), "{name} didn't run");
            }
        });
        let _ = std::fs::remove_dir_all(&dir);
    }
}
