//! Last.fm scrobbling: "now playing" when a song starts, and a scrobble once it has
//! played half its length or 4 minutes (Last.fm's rule; songs under 30 s don't count).
//!
//! Sign-in is Last.fm's desktop flow: get a token, let the user allow ytm-rs in the
//! browser, then trade the token for a session key (kept in Credential Manager).
//! Scrobbles that can't be sent (offline, Last.fm down) wait in `lastfm_queue.json`
//! and go out with the next one.

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const API_KEY: &str = "f66d99b311f6d58e14be93a8a6cd8893";
const SECRET: &str = "403541d654c21a4fcd515c9b3eb474d9";
const ROOT: &str = "https://ws.audioscrobbler.com/2.0/";
const VAULT: &str = "lastfm";
/// Last.fm takes up to 50 scrobbles per request.
const BATCH: usize = 50;
/// How long to wait for the user to allow access in the browser.
const SIGN_IN_WAIT: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Scrobble {
    pub artist: String,
    pub track: String,
    pub album: Option<String>,
    /// Seconds.
    pub duration: Option<u32>,
    /// Unix seconds when the song started playing.
    pub timestamp: i64,
}

struct Session {
    name: String,
    key: String,
}

struct State {
    session: Option<Session>,
    data_dir: PathBuf,
    queue: Vec<Scrobble>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State { session: None, data_dir: PathBuf::new(), queue: Vec::new() }));
/// One send at a time, so a queued scrobble never goes out twice.
static SENDING: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(Default::default);

/// Load the saved session and any scrobbles still waiting to be sent.
pub fn init(data_dir: &Path) {
    let session = crate::vault::get(VAULT, data_dir).and_then(|b| {
        let s = String::from_utf8(b).ok()?;
        let (name, key) = s.split_once('\n')?;
        Some(Session { name: name.to_owned(), key: key.to_owned() })
    });
    let queue = std::fs::read(data_dir.join("lastfm_queue.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    *STATE.lock().unwrap() = State { session, data_dir: data_dir.to_owned(), queue };
}

/// The signed-in Last.fm user.
pub fn user() -> Option<String> {
    STATE.lock().unwrap().session.as_ref().map(|s| s.name.clone())
}

pub fn sign_out() {
    let mut st = STATE.lock().unwrap();
    st.session = None;
    st.queue.clear();
    save_queue(&st);
    crate::vault::delete(VAULT, &st.data_dir);
}

/// Ask the user to allow ytm-rs on last.fm (in their browser) and wait until they do.
/// Returns the account name.
pub async fn sign_in(http: &reqwest::Client) -> anyhow::Result<String> {
    let token = call(http, "auth.getToken", &[], None, false).await?;
    let token = token["token"].as_str().ok_or_else(|| anyhow::anyhow!("no token from Last.fm"))?.to_owned();
    open_browser(&format!("https://www.last.fm/api/auth/?api_key={API_KEY}&token={token}"));
    let t0 = std::time::Instant::now();
    loop {
        tokio::time::sleep(Duration::from_secs(3)).await;
        match call(http, "auth.getSession", &[("token", &token)], None, false).await {
            Ok(v) => {
                let name = v["session"]["name"].as_str().unwrap_or_default().to_owned();
                let key = v["session"]["key"].as_str().ok_or_else(|| anyhow::anyhow!("no session key from Last.fm"))?.to_owned();
                let data_dir = STATE.lock().unwrap().data_dir.clone();
                crate::vault::set(VAULT, &data_dir, format!("{name}\n{key}").as_bytes())?;
                STATE.lock().unwrap().session = Some(Session { name: name.clone(), key });
                log::info!("last.fm: signed in as {name}");
                return Ok(name);
            }
            // 14: not allowed yet.
            Err(e) if code(&e) == Some(14) && t0.elapsed() < SIGN_IN_WAIT => {}
            Err(e) if code(&e) == Some(14) => anyhow::bail!("Last.fm wasn't allowed in time"),
            Err(e) => return Err(e),
        }
    }
}

pub async fn now_playing(http: &reqwest::Client, s: &Scrobble) {
    let Some(sk) = session_key() else { return };
    let duration = s.duration.map(|d| d.to_string());
    let mut params = vec![("artist", s.artist.as_str()), ("track", s.track.as_str())];
    if let Some(album) = &s.album {
        params.push(("album", album));
    }
    if let Some(d) = &duration {
        params.push(("duration", d));
    }
    match call(http, "track.updateNowPlaying", &params, Some(&sk), true).await {
        Ok(_) => log::debug!("last.fm: now playing {} - {}", s.artist, s.track),
        Err(e) => log::warn!("last.fm: now playing failed: {e:#}"),
    }
}

/// Scrobble `s`, along with any earlier ones still waiting.
pub async fn scrobble(http: &reqwest::Client, s: Scrobble) {
    let _sending = SENDING.lock().await;
    let Some(sk) = session_key() else { return };
    STATE.lock().unwrap().queue.push(s);
    loop {
        let batch: Vec<Scrobble> = STATE.lock().unwrap().queue.iter().take(BATCH).cloned().collect();
        if batch.is_empty() {
            break;
        }
        let fields: Vec<(String, String)> = batch
            .iter()
            .enumerate()
            .flat_map(|(i, s)| {
                let mut f = vec![(format!("artist[{i}]"), s.artist.clone()), (format!("track[{i}]"), s.track.clone()), (format!("timestamp[{i}]"), s.timestamp.to_string())];
                if let Some(album) = &s.album {
                    f.push((format!("album[{i}]"), album.clone()));
                }
                if let Some(d) = s.duration {
                    f.push((format!("duration[{i}]"), d.to_string()));
                }
                f
            })
            .collect();
        let params: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        match call(http, "track.scrobble", &params, Some(&sk), true).await {
            Ok(v) => {
                let accepted = v["scrobbles"]["@attr"]["accepted"].as_u64().unwrap_or(0);
                log::info!("last.fm: scrobbled {accepted} of {}", batch.len());
                let mut st = STATE.lock().unwrap();
                st.queue.drain(..batch.len());
                save_queue(&st);
            }
            // 9: the session was revoked on last.fm.
            Err(e) if code(&e) == Some(9) => {
                log::warn!("last.fm: session no longer valid; signing out");
                sign_out();
                break;
            }
            // Offline or Last.fm trouble (11, 16, 29): keep them for next time.
            Err(e) => {
                log::warn!("last.fm: scrobble failed, keeping {} for later: {e:#}", batch.len());
                save_queue(&STATE.lock().unwrap());
                break;
            }
        }
    }
}

fn session_key() -> Option<String> {
    STATE.lock().unwrap().session.as_ref().map(|s| s.key.clone())
}

fn save_queue(st: &State) {
    let path = st.data_dir.join("lastfm_queue.json");
    if st.queue.is_empty() {
        let _ = std::fs::remove_file(path);
    } else if let Ok(json) = serde_json::to_vec(&st.queue) {
        let _ = std::fs::write(path, json);
    }
}

/// Last.fm's error number, from an error `call` returned.
fn code(e: &anyhow::Error) -> Option<u64> {
    e.downcast_ref::<ApiError>().map(|e| e.0)
}

#[derive(Debug)]
struct ApiError(u64, String);

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Last.fm error {}: {}", self.0, self.1)
    }
}

impl std::error::Error for ApiError {}

/// md5 of every parameter as name+value, sorted by name, then the secret.
fn signature(params: &[(&str, &str)]) -> String {
    let mut sorted = params.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    let mut h = Md5::new();
    for (k, v) in sorted {
        h.update(k.as_bytes());
        h.update(v.as_bytes());
    }
    h.update(SECRET.as_bytes());
    format!("{:x}", h.finalize())
}

async fn call(http: &reqwest::Client, method: &str, params: &[(&str, &str)], sk: Option<&str>, post: bool) -> anyhow::Result<Value> {
    let mut all = vec![("method", method), ("api_key", API_KEY)];
    all.extend_from_slice(params);
    if let Some(sk) = sk {
        all.push(("sk", sk));
    }
    let sig = signature(&all);
    all.push(("api_sig", &sig));
    all.push(("format", "json"));
    let req = if post { http.post(ROOT).form(&all) } else { http.get(ROOT).query(&all) };
    let v: Value = req.timeout(Duration::from_secs(20)).send().await?.json().await?;
    if let Some(code) = v["error"].as_u64() {
        return Err(ApiError(code, v["message"].as_str().unwrap_or_default().to_owned()).into());
    }
    Ok(v)
}

fn open_browser(url: &str) {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        use windows::core::{HSTRING, w};
        ShellExecuteW(None, w!("open"), &HSTRING::from(url), None, None, SW_SHOWNORMAL);
    }
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_sorts_params_and_appends_the_secret() {
        let sig = signature(&[("method", "auth.getToken"), ("api_key", API_KEY)]);
        let mut h = Md5::new();
        h.update(format!("api_key{API_KEY}methodauth.getToken{SECRET}"));
        let want = format!("{:x}", h.finalize());
        assert_eq!(sig, want);
    }
}
