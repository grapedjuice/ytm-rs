//! Node.js for yt-dlp to solve YouTube's challenges with, fetched the first time a song
//! needs it and where there's no Node or Deno installed. yt-dlp solves them with QuickJS
//! too, but at about twice Node's time a song.
//!
//! The newest LTS from nodejs.org's release index, as version managers pick it; checked
//! against the release's SHA-256 sums; only the `node` binary out of the archive kept, in
//! the data folder. Looked at again monthly for a newer LTS. While it downloads (30-40
//! MB), songs play with QuickJS as before.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use serde_json::Value;
use sha2::{Digest, Sha256};

const DIST: &str = "https://nodejs.org/dist";
/// How often a newer LTS is looked for.
const MAX_AGE: Duration = Duration::from_secs(30 * 24 * 3600);

/// This platform's archive: its name in the release index's `files`, its file name's
/// platform part, and whether it's a zip (Windows) or a tar.xz.
struct Build {
    key: &'static str,
    platform: &'static str,
    zip: bool,
}

#[cfg(all(windows, target_arch = "x86_64"))]
const BUILD: Option<Build> = Some(Build { key: "win-x64-zip", platform: "win-x64", zip: true });
#[cfg(all(windows, target_arch = "aarch64"))]
const BUILD: Option<Build> = Some(Build { key: "win-arm64-zip", platform: "win-arm64", zip: true });
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const BUILD: Option<Build> = Some(Build { key: "osx-arm64-tar", platform: "darwin-arm64", zip: false });
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const BUILD: Option<Build> = Some(Build { key: "osx-x64-tar", platform: "darwin-x64", zip: false });
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const BUILD: Option<Build> = Some(Build { key: "linux-x64", platform: "linux-x64", zip: false });
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const BUILD: Option<Build> = Some(Build { key: "linux-arm64", platform: "linux-arm64", zip: false });
/// Node doesn't build for 32-bit Windows any more, and others: QuickJS it is.
#[cfg(not(any(all(windows, any(target_arch = "x86_64", target_arch = "aarch64")), all(any(target_os = "macos", target_os = "linux"), any(target_arch = "x86_64", target_arch = "aarch64")))))]
const BUILD: Option<Build> = None;

#[cfg(windows)]
const EXE: &str = "node.exe";
#[cfg(not(windows))]
const EXE: &str = "node";

/// A fetch already going: one at a time, whatever asks.
static FETCHING: AtomicBool = AtomicBool::new(false);

fn bin(data_dir: &Path) -> PathBuf {
    data_dir.join("node").join(EXE)
}

/// The downloaded Node, if there is one. Past `MAX_AGE`, a newer LTS is looked for in
/// the background; this one goes on being used meanwhile.
pub fn installed(http: &reqwest::Client, data_dir: &Path) -> Option<PathBuf> {
    let bin = bin(data_dir);
    let age = std::fs::metadata(&bin).and_then(|m| m.modified()).ok()?;
    if SystemTime::now().duration_since(age).unwrap_or_default() > MAX_AGE {
        fetch_later(http, data_dir);
    }
    Some(bin)
}

/// Fetches Node in the background, if this platform has a build and a fetch isn't going
/// already. A failure is only logged: songs go on with QuickJS, and the next one tries again.
pub fn fetch_later(http: &reqwest::Client, data_dir: &Path) {
    if BUILD.is_none() || FETCHING.swap(true, Ordering::SeqCst) {
        return;
    }
    let (http, data_dir) = (http.clone(), data_dir.to_owned());
    tokio::spawn(async move {
        match fetch(&http, &data_dir).await {
            Ok(v) => log::info!("node: {v} ready"),
            Err(e) => log::warn!("node: couldn't fetch it: {e:#}"),
        }
        FETCHING.store(false, Ordering::SeqCst);
    });
}

/// The newest LTS, downloaded and checked, its binary put in place; or, when that's the
/// one there already, marked fresh. Returns its version.
async fn fetch(http: &reqwest::Client, data_dir: &Path) -> anyhow::Result<String> {
    let build = BUILD.as_ref().ok_or_else(|| anyhow::anyhow!("no Node build for this platform"))?;
    let get = |url: String, secs: u64| async move { http.get(url).timeout(Duration::from_secs(secs)).send().await?.error_for_status() };
    let index: Value = get(format!("{DIST}/index.json"), 30).await?.json().await?;
    let version = newest_lts(&index, build.key).ok_or_else(|| anyhow::anyhow!("no LTS release has {}", build.key))?;
    let dir = data_dir.join("node");
    let bin = bin(data_dir);
    let have = std::fs::read_to_string(dir.join("version")).unwrap_or_default();
    if have.trim() == version && bin.is_file() {
        std::fs::File::options().write(true).open(&bin)?.set_modified(SystemTime::now())?;
        return Ok(version);
    }
    let file = archive_name(&version, build);
    let sums = get(format!("{DIST}/{version}/SHASUMS256.txt"), 30).await?.text().await?;
    let want = sha256_of(&sums, &file).ok_or_else(|| anyhow::anyhow!("{file} isn't in SHASUMS256.txt"))?;
    log::info!("node: downloading {file}");
    // Slow connections included: it's 30-40 MB.
    let bytes = get(format!("{DIST}/{version}/{file}"), 900).await?.bytes().await?;
    let got = hex(&Sha256::digest(&bytes));
    anyhow::ensure!(got == want, "{file}'s SHA-256 is {got}, not {want}");
    let node = tokio::task::spawn_blocking({
        let (version, build_zip, platform) = (version.clone(), build.zip, build.platform);
        move || extract(&bytes, &format!("node-{version}-{platform}"), build_zip)
    })
    .await??;
    std::fs::create_dir_all(&dir)?;
    let tmp = bin.with_extension("part");
    std::fs::write(&tmp, &node)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, &bin)?;
    std::fs::write(dir.join("version"), &version)?;
    Ok(version)
}

/// The newest release in nodejs.org's `index.json` that's an LTS and has `key`'s build.
/// Newest first is how the index lists them.
fn newest_lts(index: &Value, key: &str) -> Option<String> {
    index.as_array()?.iter().find(|r| r["lts"].is_string() && r["files"].as_array().is_some_and(|f| f.iter().any(|f| f == key))).and_then(|r| r["version"].as_str()).map(str::to_owned)
}

fn archive_name(version: &str, build: &Build) -> String {
    format!("node-{version}-{}.{}", build.platform, if build.zip { "zip" } else { "tar.xz" })
}

/// `file`'s hash in a release's SHASUMS256.txt (`<hash>  <file>` lines).
fn sha256_of(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|l| l.split_once("  ").filter(|(_, f)| f.trim() == file).map(|(h, _)| h.trim().to_ascii_lowercase()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The `node` binary out of a release archive whose top folder is `top`.
fn extract(archive: &[u8], top: &str, zip: bool) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    if zip {
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(archive))?;
        z.by_name(&format!("{top}/node.exe"))?.read_to_end(&mut out)?;
    } else {
        let want = format!("{top}/bin/node");
        let mut tar = tar::Archive::new(liblzma::read::XzDecoder::new(archive));
        let mut entry = tar.entries()?.filter_map(Result::ok).find(|e| e.path().is_ok_and(|p| p.to_string_lossy() == want)).ok_or_else(|| anyhow::anyhow!("no {want} in the archive"))?;
        entry.read_to_end(&mut out)?;
    }
    anyhow::ensure!(out.len() > 10_000_000, "node binary looks truncated ({} bytes)", out.len());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_newest_lts_with_this_build() {
        let index: Value = serde_json::json!([
            { "version": "v27.0.0", "lts": false, "files": ["win-x64-zip", "linux-x64"] },
            { "version": "v26.13.1", "lts": "Lithium", "files": ["linux-x64", "osx-arm64-tar"] },
            { "version": "v26.13.0", "lts": "Lithium", "files": ["win-x64-zip", "linux-x64"] },
        ]);
        assert_eq!(newest_lts(&index, "linux-x64").as_deref(), Some("v26.13.1"));
        // The newest LTS without a Windows zip is passed over for one that has it.
        assert_eq!(newest_lts(&index, "win-x64-zip").as_deref(), Some("v26.13.0"));
        assert_eq!(newest_lts(&index, "aix-ppc64"), None);
    }

    #[test]
    fn finds_a_files_sum() {
        let sums = "aec2514ea1  node-v26.13.1-darwin-arm64.tar.gz\nAB12  node-v26.13.1-win-x64.zip\nf5109  win-x64/node.exe\n";
        assert_eq!(sha256_of(sums, "node-v26.13.1-win-x64.zip").as_deref(), Some("ab12"));
        assert_eq!(sha256_of(sums, "node-v26.13.1-darwin-arm64.tar.xz"), None);
    }

    #[test]
    fn archive_names_match_nodejs_org() {
        let b = Build { key: "osx-arm64-tar", platform: "darwin-arm64", zip: false };
        assert_eq!(archive_name("v26.13.1", &b), "node-v26.13.1-darwin-arm64.tar.xz");
        let b = Build { key: "win-x64-zip", platform: "win-x64", zip: true };
        assert_eq!(archive_name("v26.13.1", &b), "node-v26.13.1-win-x64.zip");
    }

    /// The fetch as the app does it, into an empty data folder: Node lands there and runs,
    /// and fetching again with nothing newer only marks it fresh. Downloads 30-40 MB:
    /// `cargo test -- --ignored node_fetch_into_the_data_folder --nocapture`
    #[test]
    #[ignore]
    fn node_fetch_into_the_data_folder() {
        let dir = std::env::temp_dir().join("ytm-rs-node-fetch-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let http = reqwest::Client::new();
            assert!(installed(&http, &dir).is_none());
            let t = std::time::Instant::now();
            let version = fetch(&http, &dir).await.unwrap();
            println!("fetched {version} in {:.1} s", t.elapsed().as_secs_f64());
            let node = installed(&http, &dir).expect("installed after the fetch");
            let out = std::process::Command::new(&node).arg("--version").output().unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), version);
            // A month on, nothing newer: no download, just marked fresh.
            let old = SystemTime::now() - MAX_AGE - Duration::from_secs(60);
            std::fs::File::options().write(true).open(&node).unwrap().set_modified(old).unwrap();
            let t = std::time::Instant::now();
            assert_eq!(fetch(&http, &dir).await.unwrap(), version);
            println!("checked again in {:.1} s", t.elapsed().as_secs_f64());
            let age = SystemTime::now().duration_since(std::fs::metadata(&node).unwrap().modified().unwrap()).unwrap();
            assert!(age < Duration::from_secs(60), "not marked fresh");
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The real downloads for Windows and Linux, checked and unpacked, and the Windows
    /// one run (on Windows). Downloads about 70 MB:
    /// `cargo test -- --ignored node_downloads_unpack --nocapture`
    #[test]
    #[ignore]
    fn node_downloads_unpack() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let http = reqwest::Client::new();
            let index: Value = http.get(format!("{DIST}/index.json")).send().await.unwrap().json().await.unwrap();
            for build in [Build { key: "win-x64-zip", platform: "win-x64", zip: true }, Build { key: "linux-x64", platform: "linux-x64", zip: false }] {
                let version = newest_lts(&index, build.key).unwrap();
                let file = archive_name(&version, &build);
                let sums = http.get(format!("{DIST}/{version}/SHASUMS256.txt")).send().await.unwrap().text().await.unwrap();
                let bytes = http.get(format!("{DIST}/{version}/{file}")).send().await.unwrap().bytes().await.unwrap();
                assert_eq!(hex(&Sha256::digest(&bytes)), sha256_of(&sums, &file).unwrap(), "{file}");
                let node = extract(&bytes, &format!("node-{version}-{}", build.platform), build.zip).unwrap();
                println!("{file}: {:.1} MB download, node {:.1} MB", bytes.len() as f64 / 1e6, node.len() as f64 / 1e6);
                if cfg!(windows) && build.zip {
                    let dir = std::env::temp_dir().join("ytm-rs-node-test");
                    std::fs::create_dir_all(&dir).unwrap();
                    let exe = dir.join("node.exe");
                    std::fs::write(&exe, &node).unwrap();
                    let out = std::process::Command::new(&exe).arg("--version").output().unwrap();
                    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), version);
                    let _ = std::fs::remove_dir_all(&dir);
                }
            }
        });
    }
}
