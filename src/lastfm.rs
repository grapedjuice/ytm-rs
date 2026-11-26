//! Last.fm scrobbling: "now playing" when a song starts, and a scrobble once it has
//! played half its length or 4 minutes (Last.fm's rule; songs under 30 s don't count).
//!
//! Sign-in is Last.fm's desktop flow: get a token, let the user allow ytm-rs in the
//! browser, then trade the token for a session key (kept in Credential Manager).
//! Scrobbles that can't be sent (offline, Last.fm down) wait in `lastfm_queue.json`
//! and go out with the next one.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use crate::scrobble_store::{Credentials, QueueFile, Vault};

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
    session: Option<Arc<Session>>,
    data_dir: PathBuf,
    queue: Vec<Scrobble>,
    credentials: Arc<dyn Credentials>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State { session: None, data_dir: PathBuf::new(), queue: Vec::new(), credentials: Arc::new(Vault) }));
/// One send at a time, so a queued scrobble never goes out twice.
static SENDING: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(Default::default);

/// Load the saved session and any scrobbles still waiting to be sent.
pub fn init(data_dir: &Path) {
    *STATE.lock().unwrap() = load_state(data_dir, Arc::new(Vault));
}

fn load_state(data_dir: &Path, credentials: Arc<dyn Credentials>) -> State {
    let session = credentials.get(VAULT, data_dir).and_then(|b| {
        let s = String::from_utf8(b).ok()?;
        let (name, key) = s.split_once('\n')?;
        if name.trim().is_empty() || key.trim().is_empty() {
            return None;
        }
        Some(Arc::new(Session { name: name.to_owned(), key: key.to_owned() }))
    });
    let queue = match QueueFile(data_dir.join("lastfm_queue.json")).restore(session.as_ref().map(|s| s.name.as_str())) {
        Ok(queue) => queue,
        Err(e) => {
            log::warn!("last.fm: couldn't restore queued plays: {e:#}");
            Vec::new()
        }
    };
    State { session, data_dir: data_dir.to_owned(), queue, credentials }
}

/// The signed-in Last.fm user.
pub fn user() -> Option<String> {
    STATE.lock().unwrap().session.as_ref().map(|s| s.name.clone())
}

pub fn sign_out() {
    disconnect(&mut STATE.lock().unwrap());
}

fn disconnect(st: &mut State) {
    st.queue.clear();
    persist_queue(st);
    st.session = None;
    st.credentials.delete(VAULT, &st.data_dir);
}

/// Save a new login and return its queue. Runs without `STATE` held, since the keyring
/// can wait on an unlock prompt.
fn save_session(data_dir: &Path, credentials: &dyn Credentials, session: &Session) -> anyhow::Result<Vec<Scrobble>> {
    anyhow::ensure!(!session.name.trim().is_empty() && !session.key.trim().is_empty(), "last.fm: incomplete session");
    // Drop any ownerless queue before startup could recover it with the new credentials.
    let queue = QueueFile(data_dir.join("lastfm_queue.json")).prepare(&session.name)?;
    credentials.set(VAULT, data_dir, format!("{}\n{}", session.name, session.key).as_bytes())?;
    Ok(queue)
}

fn use_session(st: &mut State, session: Session, queue: Vec<Scrobble>) {
    // A same-account reconnect keeps the live queue, including unsaved changes.
    if st.session.as_ref().is_none_or(|old| old.name != session.name) {
        st.queue = queue;
    }
    st.session = Some(Arc::new(session));
}

fn is_current(st: &State, session: &Arc<Session>) -> bool {
    st.session.as_ref().is_some_and(|current| Arc::ptr_eq(current, session))
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
                let session = Session { name: name.clone(), key };
                let (data_dir, credentials) = {
                    let st = STATE.lock().unwrap();
                    (st.data_dir.clone(), st.credentials.clone())
                };
                let queue = save_session(&data_dir, credentials.as_ref(), &session)?;
                use_session(&mut STATE.lock().unwrap(), session, queue);
                log::info!("last.fm: signed in as {name}");
                return Ok(name);
            }
            // 14: not allowed yet.
            Err(e) if code(&e) == Some(14) && t0.elapsed() < SIGN_IN_WAIT => {}
            Err(e) if code(&e) == Some(14) => anyhow::bail!(crate::tr!("settings-lastfm-not-allowed")),
            Err(e) => return Err(e),
        }
    }
}

pub fn now_playing(http: &reqwest::Client, s: &Scrobble) -> impl Future<Output = ()> + Send + 'static {
    let (http, s) = (http.clone(), s.clone());
    now_playing_with(&STATE, move |session| async move { send_now_playing(&http, &s, &session).await })
}

fn now_playing_with<F: Future<Output = anyhow::Result<()>> + Send>(state: &Mutex<State>, send: impl FnOnce(Arc<Session>) -> F + Send) -> impl Future<Output = ()> + Send {
    let session = state.lock().unwrap().session.clone();
    async move {
        let Some(session) = session else { return };
        if !is_current(&state.lock().unwrap(), &session) {
            log::debug!("last.fm: skipping now playing from a replaced session");
            return;
        }
        match send(session).await {
            Ok(()) => log::debug!("last.fm: now playing sent"),
            Err(e) => log::warn!("last.fm: now playing failed: {e:#}"),
        }
    }
}

async fn send_now_playing(http: &reqwest::Client, s: &Scrobble, session: &Session) -> anyhow::Result<()> {
    let duration = s.duration.map(|d| d.to_string());
    let mut params = vec![("artist", s.artist.as_str()), ("track", s.track.as_str())];
    if let Some(album) = &s.album {
        params.push(("album", album));
    }
    if let Some(d) = &duration {
        params.push(("duration", d));
    }
    call(http, "track.updateNowPlaying", &params, Some(&session.key), true).await.map(|_| ())
}

/// Scrobble `s`, along with any earlier ones still waiting.
pub fn scrobble(http: &reqwest::Client, s: Scrobble) -> impl Future<Output = ()> + Send + 'static {
    let http = http.clone();
    scrobble_with(&STATE, &SENDING, s, move |batch, session| {
        let http = http.clone();
        async move { send_batch(&http, &batch, &session.key).await }
    })
}

/// Capture the session now, before the returned future is spawned or waits its turn.
fn scrobble_with<F: Future<Output = anyhow::Result<Value>> + Send>(
    state: &Mutex<State>,
    sending: &tokio::sync::Mutex<()>,
    s: Scrobble,
    mut send: impl FnMut(Vec<Scrobble>, Arc<Session>) -> F + Send,
) -> impl Future<Output = ()> + Send {
    let session = state.lock().unwrap().session.clone();
    async move {
        let Some(session) = session else { return };
        {
            let mut st = state.lock().unwrap();
            if !is_current(&st, &session) {
                log::debug!("last.fm: ignoring work from a replaced session");
                return;
            }
            st.queue.push(s);
            persist_queue(&st);
        }
        // Persist even while another request owns the transport lock.
        let _sending = sending.lock().await;
        loop {
            let batch: Vec<Scrobble> = {
                let st = state.lock().unwrap();
                if !is_current(&st, &session) {
                    log::debug!("last.fm: skipping queued work from a replaced session");
                    return;
                }
                st.queue.iter().take(BATCH).cloned().collect()
            };
            if batch.is_empty() {
                break;
            }
            let count = batch.len();
            let result = send(batch, session.clone()).await;
            let mut st = state.lock().unwrap();
            // Check and apply the result under one lock, including revoked keys.
            if !is_current(&st, &session) {
                log::debug!("last.fm: ignoring a response from a replaced session");
                return;
            }
            match result {
                Ok(v) => {
                    let accepted = v["scrobbles"]["@attr"]["accepted"].as_u64().unwrap_or(0);
                    log::info!("last.fm: scrobbled {accepted} of {count}");
                    st.queue.drain(..count);
                    persist_queue(&st);
                }
                // 9: the session was revoked on last.fm.
                Err(e) if code(&e) == Some(9) => {
                    log::warn!("last.fm: session no longer valid; signing out");
                    disconnect(&mut st);
                    break;
                }
                // Offline or Last.fm trouble (11, 16, 29): keep them for next time.
                Err(e) => {
                    log::warn!("last.fm: scrobble failed, keeping {count} for later: {e:#}");
                    break;
                }
            }
        }
    }
}

async fn send_batch(http: &reqwest::Client, batch: &[Scrobble], sk: &str) -> anyhow::Result<Value> {
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
    call(http, "track.scrobble", &params, Some(sk), true).await
}

fn persist_queue(st: &State) {
    if let Some(session) = &st.session
        && let Err(e) = QueueFile(st.data_dir.join("lastfm_queue.json")).save(&session.name, &st.queue)
    {
        log::warn!("last.fm: couldn't save queued plays: {e:#}");
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
mod race_tests {
    crate::scrobble_tests::service_tests!(scrobble_with, key, "lastfm_queue.json", 9, Value::Null, false,
        Scrobble {}
    );
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
