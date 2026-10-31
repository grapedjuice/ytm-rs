//! Last-resort stream resolution through yt-dlp: age-restricted songs on accounts
//! without Premium (whose signed-in web streams need a PO token), or when the native
//! signed-in path in `webmusic` breaks. yt-dlp solves the URL challenges with Node.
//!
//! yt-dlp is downloaded into the app's data folder on first use and refreshed
//! weekly. The signed-in session is passed as a Netscape cookies file that exists
//! only for the duration of the call.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use serde_json::Value;

#[cfg(windows)]
const BIN: &str = "yt-dlp.exe";
#[cfg(not(windows))]
const BIN: &str = "yt-dlp";
#[cfg(all(windows, not(target_arch = "aarch64")))]
const URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe";
#[cfg(all(windows, target_arch = "aarch64"))]
const URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_arm64.exe";
#[cfg(target_os = "macos")]
const URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_macos";
#[cfg(all(unix, not(target_os = "macos")))]
const URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_linux";

const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

pub struct Stream {
    pub url: String,
    pub size: u64,
    pub duration_ms: u64,
    pub user_agent: String,
    /// youtube.com cookies yt-dlp saved changed, as `Set-Cookie` lines.
    pub replaced: Vec<String>,
}

/// Path to an up-to-date yt-dlp, downloading it if missing or stale.
async fn ensure(http: &reqwest::Client, data_dir: &Path) -> anyhow::Result<PathBuf> {
    let bin = data_dir.join(BIN);
    let fresh = std::fs::metadata(&bin)
        .and_then(|m| m.modified())
        .is_ok_and(|t| SystemTime::now().duration_since(t).unwrap_or_default() < MAX_AGE);
    if fresh {
        return Ok(bin);
    }
    log::info!("downloading yt-dlp");
    let bytes = http.get(URL).timeout(Duration::from_secs(120)).send().await?.error_for_status()?.bytes().await?;
    anyhow::ensure!(bytes.len() > 1_000_000, "yt-dlp download looks truncated ({} bytes)", bytes.len());
    let tmp = bin.with_extension("part");
    std::fs::write(&tmp, &bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, &bin)?;
    Ok(bin)
}

pub async fn resolve(http: &reqwest::Client, data_dir: &Path, cookie: &str, video_id: &str) -> anyhow::Result<Stream> {
    let bin = ensure(http, data_dir).await?;
    let data_dir = data_dir.to_owned();
    let (cookie, video_id) = (cookie.to_owned(), video_id.to_owned());
    let mut s = tokio::task::spawn_blocking(move || run(&bin, &data_dir, &video_id, &cookie)).await??;
    // The range downloader needs the exact size; yt-dlp sometimes only estimates it.
    if s.size == 0 {
        let head = http.head(&s.url).header("User-Agent", &s.user_agent).send().await?.error_for_status()?;
        // Not `content_length()`: for a HEAD response that reports the empty body.
        let len = head.headers().get(reqwest::header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()?.parse::<u64>().ok());
        s.size = len.filter(|&n| n > 0).ok_or_else(|| anyhow::anyhow!("unknown stream size"))?;
    }
    Ok(s)
}

fn run(bin: &Path, data_dir: &Path, video_id: &str, cookie: &str) -> anyhow::Result<Stream> {
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
    cmd.args(["--js-runtimes", "node", "--no-warnings", "--no-playlist", "--no-cache-dir", "-f", "140/bestaudio[ext=m4a]", "-j", "--cookies"])
        .arg(&jar)
        .args(crate::proxy::ytdlp_args())
        .arg(format!("https://music.youtube.com/watch?v={video_id}"));
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
