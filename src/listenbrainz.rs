//! ListenBrainz: "playing now" when a song starts, and a listen once it has played half
//! its length or 4 minutes (the same rule as Last.fm; songs under 30 s don't count).
//!
//! There's no browser sign-in: the user pastes the token from listenbrainz.org/settings,
//! which is checked, then kept in Credential Manager. Listens that can't be sent
//! (offline, ListenBrainz down) wait in `listenbrainz_queue.json` and go out with the
//! next one.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use crate::scrobble_store::{Credentials, QueueFile, Vault};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const ROOT: &str = "https://api.listenbrainz.org/1/";
const VAULT: &str = "listenbrainz";
/// ListenBrainz takes up to 1000 listens per request; this keeps each one small.
const BATCH: usize = 100;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Listen {
    pub artist: String,
    pub track: String,
    pub album: Option<String>,
    /// Seconds.
    pub duration: Option<u32>,
    /// Unix seconds when the song started playing.
    pub timestamp: i64,
    pub video_id: String,
}

struct Session {
    name: String,
    token: String,
}

struct State {
    session: Option<Arc<Session>>,
    data_dir: PathBuf,
    queue: Vec<Listen>,
    credentials: Arc<dyn Credentials>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State { session: None, data_dir: PathBuf::new(), queue: Vec::new(), credentials: Arc::new(Vault) }));
/// One send at a time, so a queued listen never goes out twice.
static SENDING: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(Default::default);

/// Load the saved token and any listens still waiting to be sent.
pub fn init(data_dir: &Path) {
    *STATE.lock().unwrap() = load_state(data_dir, Arc::new(Vault));
}

fn load_state(data_dir: &Path, credentials: Arc<dyn Credentials>) -> State {
    let session = credentials.get(VAULT, data_dir).and_then(|b| {
        let s = String::from_utf8(b).ok()?;
        let (name, token) = s.split_once('\n')?;
        if name.trim().is_empty() || token.trim().is_empty() {
            return None;
        }
        Some(Arc::new(Session { name: name.to_owned(), token: token.to_owned() }))
    });
    let queue = match QueueFile(data_dir.join("listenbrainz_queue.json")).restore(session.as_ref().map(|s| s.name.as_str())) {
        Ok(queue) => queue,
        Err(e) => {
            log::warn!("listenbrainz: couldn't restore queued plays: {e:#}");
            Vec::new()
        }
    };
    State { session, data_dir: data_dir.to_owned(), queue, credentials }
}

/// The connected ListenBrainz user.
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
fn save_session(data_dir: &Path, credentials: &dyn Credentials, session: &Session) -> anyhow::Result<Vec<Listen>> {
    anyhow::ensure!(!session.name.trim().is_empty() && !session.token.trim().is_empty(), "listenbrainz: incomplete session");
    // Drop any ownerless queue before startup could recover it with the new credentials.
    let queue = QueueFile(data_dir.join("listenbrainz_queue.json")).prepare(&session.name)?;
    credentials.set(VAULT, data_dir, format!("{}\n{}", session.name, session.token).as_bytes())?;
    Ok(queue)
}

fn use_session(st: &mut State, session: Session, queue: Vec<Listen>) {
    // A same-account reconnect keeps the live queue, including unsaved changes.
    if st.session.as_ref().is_none_or(|old| old.name != session.name) {
        st.queue = queue;
    }
    st.session = Some(Arc::new(session));
}

fn is_current(st: &State, session: &Arc<Session>) -> bool {
    st.session.as_ref().is_some_and(|current| Arc::ptr_eq(current, session))
}

/// Check the user token with ListenBrainz and keep it. Returns the account name.
pub async fn connect(http: &reqwest::Client, token: &str) -> anyhow::Result<String> {
    let token = token.trim();
    anyhow::ensure!(!token.is_empty(), "paste your user token first");
    let v: Value = http.get(format!("{ROOT}validate-token")).header("Authorization", format!("Token {token}")).timeout(Duration::from_secs(20)).send().await?.json().await?;
    if !v["valid"].as_bool().unwrap_or(false) {
        anyhow::bail!("ListenBrainz didn't accept that token");
    }
    let name = v["user_name"].as_str().unwrap_or_default().to_owned();
    let session = Session { name: name.clone(), token: token.to_owned() };
    let (data_dir, credentials) = {
        let st = STATE.lock().unwrap();
        (st.data_dir.clone(), st.credentials.clone())
    };
    let queue = save_session(&data_dir, credentials.as_ref(), &session)?;
    use_session(&mut STATE.lock().unwrap(), session, queue);
    log::info!("listenbrainz: connected as {name}");
    Ok(name)
}

pub fn now_playing(http: &reqwest::Client, s: &Listen) -> impl Future<Output = ()> + Send + 'static {
    let (http, s) = (http.clone(), s.clone());
    now_playing_with(&STATE, move |session| async move { send_now_playing(&http, &s, &session).await })
}

fn now_playing_with<F: Future<Output = anyhow::Result<()>> + Send>(state: &Mutex<State>, send: impl FnOnce(Arc<Session>) -> F + Send) -> impl Future<Output = ()> + Send {
    let session = state.lock().unwrap().session.clone();
    async move {
        let Some(session) = session else { return };
        if !is_current(&state.lock().unwrap(), &session) {
            log::debug!("listenbrainz: skipping now playing from a replaced session");
            return;
        }
        match send(session).await {
            Ok(()) => log::debug!("listenbrainz: now playing sent"),
            Err(e) => log::warn!("listenbrainz: now playing failed: {e:#}"),
        }
    }
}

async fn send_now_playing(http: &reqwest::Client, s: &Listen, session: &Session) -> anyhow::Result<()> {
    let body = json!({ "listen_type": "playing_now", "payload": [{ "track_metadata": metadata(s) }] });
    submit(http, &session.token, &body).await
}

/// Submit `l`, along with any earlier listens still waiting.
pub fn listen(http: &reqwest::Client, l: Listen) -> impl Future<Output = ()> + Send + 'static {
    let http = http.clone();
    listen_with(&STATE, &SENDING, l, move |batch, session| {
        let http = http.clone();
        async move { submit(&http, &session.token, &listens_body(&batch)).await }
    })
}

/// Capture the session now, before the returned future is spawned or waits its turn.
fn listen_with<F: Future<Output = anyhow::Result<()>> + Send>(
    state: &Mutex<State>,
    sending: &tokio::sync::Mutex<()>,
    l: Listen,
    mut send: impl FnMut(Vec<Listen>, Arc<Session>) -> F + Send,
) -> impl Future<Output = ()> + Send {
    let session = state.lock().unwrap().session.clone();
    async move {
        let Some(session) = session else { return };
        {
            let mut st = state.lock().unwrap();
            if !is_current(&st, &session) {
                log::debug!("listenbrainz: ignoring work from a replaced session");
                return;
            }
            st.queue.push(l);
            persist_queue(&st);
        }
        // Persist even while another request owns the transport lock.
        let _sending = sending.lock().await;
        loop {
            let batch: Vec<Listen> = {
                let st = state.lock().unwrap();
                if !is_current(&st, &session) {
                    log::debug!("listenbrainz: skipping queued work from a replaced session");
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
            // Check and apply the result under one lock, including revoked tokens.
            if !is_current(&st, &session) {
                log::debug!("listenbrainz: ignoring a response from a replaced session");
                return;
            }
            match result {
                Ok(()) => log::info!("listenbrainz: submitted {count} listen(s)"),
                Err(e) => match e.downcast_ref::<ApiError>().map(|e| e.0) {
                    // The token was reset on listenbrainz.org.
                    Some(401) => {
                        log::warn!("listenbrainz: token no longer valid; disconnecting");
                        disconnect(&mut st);
                        break;
                    }
                    // ListenBrainz will never take these: don't let them hold up the rest.
                    Some(400) => log::warn!("listenbrainz: dropping {count} rejected listen(s): {e:#}"),
                    // Offline, rate limited or ListenBrainz trouble: keep them for next time.
                    _ => {
                        log::warn!("listenbrainz: submit failed, keeping {count} for later: {e:#}");
                        break;
                    }
                },
            }
            st.queue.drain(..count);
            persist_queue(&st);
        }
    }
}

fn persist_queue(st: &State) {
    if let Some(session) = &st.session
        && let Err(e) = QueueFile(st.data_dir.join("listenbrainz_queue.json")).save(&session.name, &st.queue)
    {
        log::warn!("listenbrainz: couldn't save queued plays: {e:#}");
    }
}

fn metadata(l: &Listen) -> Value {
    let mut info = json!({
        "media_player": "ytm-rs",
        "submission_client": "ytm-rs",
        "submission_client_version": env!("CARGO_PKG_VERSION"),
        "music_service": "music.youtube.com",
        "music_service_name": "YouTube Music",
        "origin_url": format!("https://music.youtube.com/watch?v={}", l.video_id),
    });
    if let Some(d) = l.duration {
        info["duration_ms"] = json!(u64::from(d) * 1000);
    }
    let mut m = json!({ "artist_name": l.artist, "track_name": l.track, "additional_info": info });
    if let Some(album) = &l.album {
        m["release_name"] = json!(album);
    }
    m
}

/// "single" for one listen, "import" for a backlog.
fn listens_body(batch: &[Listen]) -> Value {
    let payload: Vec<Value> = batch.iter().map(|l| json!({ "listened_at": l.timestamp, "track_metadata": metadata(l) })).collect();
    json!({ "listen_type": if batch.len() == 1 { "single" } else { "import" }, "payload": payload })
}

#[derive(Debug)]
struct ApiError(u16, String);

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ListenBrainz error {}: {}", self.0, self.1)
    }
}

impl std::error::Error for ApiError {}

async fn submit(http: &reqwest::Client, token: &str, body: &Value) -> anyhow::Result<()> {
    let resp = http.post(format!("{ROOT}submit-listens")).header("Authorization", format!("Token {token}")).json(body).timeout(Duration::from_secs(20)).send().await?;
    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    let v: Value = resp.json().await.unwrap_or_default();
    Err(ApiError(status.as_u16(), v["error"].as_str().unwrap_or(status.canonical_reason().unwrap_or_default()).to_owned()).into())
}

#[cfg(test)]
mod race_tests {
    crate::scrobble_tests::service_tests!(listen_with, token, "listenbrainz_queue.json", 401, (), true,
        Listen { video_id: "test".into(), }
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listen(album: Option<&str>, duration: Option<u32>) -> Listen {
        Listen { artist: "Artist".into(), track: "Song".into(), album: album.map(Into::into), duration, timestamp: 1_700_000_000, video_id: "abc123".into() }
    }

    #[test]
    fn one_listen_is_single_and_a_backlog_is_an_import() {
        let one = listens_body(&[listen(Some("Album"), Some(200))]);
        assert_eq!(one["listen_type"], "single");
        let p = &one["payload"][0];
        assert_eq!(p["listened_at"], 1_700_000_000);
        assert_eq!(p["track_metadata"]["artist_name"], "Artist");
        assert_eq!(p["track_metadata"]["track_name"], "Song");
        assert_eq!(p["track_metadata"]["release_name"], "Album");
        assert_eq!(p["track_metadata"]["additional_info"]["duration_ms"], 200_000);
        assert_eq!(p["track_metadata"]["additional_info"]["origin_url"], "https://music.youtube.com/watch?v=abc123");
        let two = listens_body(&[listen(None, None), listen(None, None)]);
        assert_eq!(two["listen_type"], "import");
        assert_eq!(two["payload"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn missing_album_and_duration_are_left_out() {
        let m = metadata(&listen(None, None));
        assert!(m.get("release_name").is_none());
        assert!(m["additional_info"].get("duration_ms").is_none());
    }
}
