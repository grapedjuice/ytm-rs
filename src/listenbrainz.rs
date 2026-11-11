//! ListenBrainz: "playing now" when a song starts, and a listen once it has played half
//! its length or 4 minutes (the same rule as Last.fm; songs under 30 s don't count).
//!
//! There's no browser sign-in: the user pastes the token from listenbrainz.org/settings,
//! which is checked, then kept in Credential Manager. Listens that can't be sent
//! (offline, ListenBrainz down) wait in `listenbrainz_queue.json` and go out with the
//! next one.

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

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
    session: Option<Session>,
    data_dir: PathBuf,
    queue: Vec<Listen>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State { session: None, data_dir: PathBuf::new(), queue: Vec::new() }));
/// One send at a time, so a queued listen never goes out twice.
static SENDING: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(Default::default);

/// Load the saved token and any listens still waiting to be sent.
pub fn init(data_dir: &Path) {
    let session = crate::vault::get(VAULT, data_dir).and_then(|b| {
        let s = String::from_utf8(b).ok()?;
        let (name, token) = s.split_once('\n')?;
        Some(Session { name: name.to_owned(), token: token.to_owned() })
    });
    let queue = std::fs::read(data_dir.join("listenbrainz_queue.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    *STATE.lock().unwrap() = State { session, data_dir: data_dir.to_owned(), queue };
}

/// The connected ListenBrainz user.
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

/// Check the user token with ListenBrainz and keep it. Returns the account name.
pub async fn connect(http: &reqwest::Client, token: &str) -> anyhow::Result<String> {
    let token = token.trim();
    anyhow::ensure!(!token.is_empty(), "paste your user token first");
    let v: Value = http.get(format!("{ROOT}validate-token")).header("Authorization", format!("Token {token}")).timeout(Duration::from_secs(20)).send().await?.json().await?;
    if !v["valid"].as_bool().unwrap_or(false) {
        anyhow::bail!("ListenBrainz didn't accept that token");
    }
    let name = v["user_name"].as_str().unwrap_or_default().to_owned();
    let data_dir = STATE.lock().unwrap().data_dir.clone();
    crate::vault::set(VAULT, &data_dir, format!("{name}\n{token}").as_bytes())?;
    STATE.lock().unwrap().session = Some(Session { name: name.clone(), token: token.to_owned() });
    log::info!("listenbrainz: connected as {name}");
    Ok(name)
}

pub async fn now_playing(http: &reqwest::Client, l: &Listen) {
    let Some(token) = token() else { return };
    let body = json!({ "listen_type": "playing_now", "payload": [{ "track_metadata": metadata(l) }] });
    match submit(http, &token, &body).await {
        Ok(()) => log::debug!("listenbrainz: playing now {} - {}", l.artist, l.track),
        Err(e) => log::warn!("listenbrainz: playing now failed: {e:#}"),
    }
}

/// Submit `l`, along with any earlier listens still waiting.
pub async fn listen(http: &reqwest::Client, l: Listen) {
    let _sending = SENDING.lock().await;
    let Some(token) = token() else { return };
    STATE.lock().unwrap().queue.push(l);
    loop {
        let batch: Vec<Listen> = STATE.lock().unwrap().queue.iter().take(BATCH).cloned().collect();
        if batch.is_empty() {
            break;
        }
        match submit(http, &token, &listens_body(&batch)).await {
            Ok(()) => log::info!("listenbrainz: submitted {} listen(s)", batch.len()),
            Err(e) => match e.downcast_ref::<ApiError>().map(|e| e.0) {
                // The token was reset on listenbrainz.org.
                Some(401) => {
                    log::warn!("listenbrainz: token no longer valid; disconnecting");
                    sign_out();
                    break;
                }
                // ListenBrainz will never take these: don't let them hold up the rest.
                Some(400) => log::warn!("listenbrainz: dropping {} rejected listen(s): {e:#}", batch.len()),
                // Offline, rate limited or ListenBrainz trouble: keep them for next time.
                _ => {
                    log::warn!("listenbrainz: submit failed, keeping {} for later: {e:#}", batch.len());
                    save_queue(&STATE.lock().unwrap());
                    break;
                }
            },
        }
        let mut st = STATE.lock().unwrap();
        st.queue.drain(..batch.len());
        save_queue(&st);
    }
}

fn token() -> Option<String> {
    STATE.lock().unwrap().session.as_ref().map(|s| s.token.clone())
}

fn save_queue(st: &State) {
    let path = st.data_dir.join("listenbrainz_queue.json");
    if st.queue.is_empty() {
        let _ = std::fs::remove_file(path);
    } else if let Ok(json) = serde_json::to_vec(&st.queue) {
        let _ = std::fs::write(path, json);
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
