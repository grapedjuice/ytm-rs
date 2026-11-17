//! Listen together ("Jam"): a room on a small relay server (`server/`) that keeps
//! everyone on the same song at the same moment.
//!
//! No audio goes through the server: each member streams the song from YouTube like
//! any other. The host's app is the authority: it sends the queue and play state, and
//! guests follow it. Guests' requests (add a song, and pause/skip/seek when the host
//! allows it) go to the host, which applies them and sends the new state.
//!
//! The socket lives on the backend's tokio runtime; the UI talks to it through
//! channels and is woken when something arrives.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender, unbounded};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::model::Track;

/// The relay. `YTM_JAM_SERVER` overrides it (e.g. `ws://127.0.0.1:8787` for `wrangler dev`).
const SERVER: &str = "wss://ytm-rs-jam.grapedjuice.workers.dev";

/// Room codes: no 0/O, 1/I/L, so they survive being read out loud.
pub const ALPHABET: &str = "ABCDEFGHJKMNPQRSTUVWXYZ23456789";
pub const CODE_LEN: usize = 6;

/// Link that opens the app (through the website) and joins the room.
pub fn share_link(code: &str) -> String {
    format!("https://grapedjuice.github.io/ytm-rs/jam/?c={code}")
}

/// A fresh random room code.
pub fn new_code() -> String {
    use std::hash::{BuildHasher, Hasher};
    // RandomState is seeded randomly per process; mix in the time for each call.
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos());
    let mut n = h.finish();
    let chars = ALPHABET.as_bytes();
    (0..CODE_LEN)
        .map(|_| {
            let c = chars[(n % chars.len() as u64) as usize] as char;
            n /= chars.len() as u64;
            c
        })
        .collect()
}

/// `code` cleaned up (case, spaces, dashes) if it's a valid room code.
pub fn normalize_code(code: &str) -> Option<String> {
    let c: String = code.chars().filter(|c| !c.is_whitespace() && *c != '-').collect::<String>().to_ascii_uppercase();
    (c.len() == CODE_LEN && c.chars().all(|ch| ALPHABET.contains(ch))).then_some(c)
}

#[derive(Clone, Debug, Deserialize)]
pub struct Member {
    pub id: String,
    pub name: String,
}

/// The host's queue and playback, as sent to guests.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RoomState {
    pub queue: Vec<Track>,
    pub index: usize,
    /// Who added which song (by video ID), for songs guests added.
    #[serde(default)]
    pub by: HashMap<String, String>,
    /// Which member added which song (video ID → member ID), for the per-guest cap.
    #[serde(default)]
    pub owner: HashMap<String, String>,
    pub vid: String,
    /// Position in ms at `ts`.
    pub pos: u64,
    pub playing: bool,
}

/// Where playback is, without the queue (the host's periodic heartbeat).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tick {
    pub vid: String,
    pub pos: u64,
    pub playing: bool,
}

impl Tick {
    /// Position now, given when (locally) it was at `pos`.
    pub fn position_at(&self, at: Instant) -> Duration {
        let pos = Duration::from_millis(self.pos);
        if self.playing { pos + at.elapsed() } else { pos }
    }
}

/// What a guest asks the host to do.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "lowercase")]
pub enum Cmd {
    Add { track: Track },
    /// Play next.
    Next { track: Track },
    Toggle,
    Skip,
    Prev,
    Seek { pos: u64 },
    Jump { index: usize },
}

impl Cmd {
    /// Only allowed while the host lets everyone control playback.
    pub fn is_control(&self) -> bool {
        !matches!(self, Cmd::Add { .. } | Cmd::Next { .. })
    }
}

#[derive(Debug)]
pub enum Event {
    /// Joined (or rejoined after a dropped connection).
    Welcome { you: String, host: String, control: bool, queue: bool, members: Vec<Member> },
    Members { host: String, members: Vec<Member> },
    /// What guests may do: control playback, add songs.
    Control { on: bool, queue: bool },
    /// The host's state; `Instant` is when (locally) playback was at `state.pos`.
    State(RoomState, Instant),
    Tick(Tick, Instant),
    /// For the host: a guest's request.
    Req { from: String, name: String, cmd: Cmd },
    /// A request the server turned down (`denied`, `rate`), the room keeps going.
    Notice(String),
    /// The connection is gone for good: a reason to show.
    Closed(String),
    Reconnecting,
}

enum Out {
    Text(String),
    Leave,
}

pub struct Jam {
    pub code: String,
    out: mpsc::UnboundedSender<Out>,
    events: Receiver<Event>,
}

impl Jam {
    /// Connect to room `code`: `create` starts it (we're the host), otherwise join.
    pub fn start(backend: &crate::backend::Backend, code: String, create: bool, name: String) -> Jam {
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let (ev_tx, ev_rx) = unbounded();
        let wake = backend.clone();
        let task = Conn { code: code.clone(), create, name, events: ev_tx, wake: Arc::new(move || wake.wake()), offset: Arc::new(AtomicI64::new(0)) };
        backend.spawn(task.run(out_rx));
        Jam { code, out: out_tx, events: ev_rx }
    }

    pub fn try_recv(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    fn send(&self, v: serde_json::Value) {
        let _ = self.out.send(Out::Text(v.to_string()));
    }

    pub fn send_state(&self, s: &RoomState) {
        let mut v = serde_json::to_value(s).unwrap_or_default();
        v["t"] = "state".into();
        self.send(v);
    }

    pub fn send_tick(&self, t: &Tick) {
        let mut v = serde_json::to_value(t).unwrap_or_default();
        v["t"] = "tick".into();
        self.send(v);
    }

    /// Host: let guests control playback (`on`) and add songs (`queue`).
    pub fn set_control(&self, on: bool, queue: bool) {
        self.send(serde_json::json!({ "t": "control", "on": on, "queue": queue }));
    }

    pub fn request(&self, cmd: Cmd) {
        self.send(serde_json::json!({ "t": "req", "cmd": cmd }));
    }

    /// Host: close the room for everyone.
    pub fn end(&self) {
        self.send(serde_json::json!({ "t": "end" }));
    }

    pub fn leave(&self) {
        let _ = self.out.send(Out::Leave);
    }
}

impl Drop for Jam {
    fn drop(&mut self) {
        self.leave();
    }
}

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
enum In {
    Welcome {
        you: String,
        host: String,
        control: bool,
        #[serde(default = "yes")]
        queue: bool,
        members: Vec<Member>,
        state: Option<Stamped<RoomState>>,
        tick: Option<Stamped<Tick>>,
        now: i64,
    },
    Members { host: String, members: Vec<Member> },
    Control {
        on: bool,
        #[serde(default = "yes")]
        queue: bool,
    },
    State(Stamped<RoomState>),
    Tick(Stamped<Tick>),
    Req { from: String, name: String, cmd: Cmd },
    Pong { c: i64, s: i64 },
    Error { code: String, msg: Option<String> },
    Ended,
}

impl In {
    /// The message with every song in it checked (see `peer_track`): `None` when one
    /// fails, since a queue missing a song would no longer match its index.
    fn checked(self) -> Option<Self> {
        Some(match self {
            In::Welcome { you, host, control, queue, members, state, tick, now } => {
                let state = match state {
                    Some(s) => Some(Stamped { v: s.v.checked()?, ts: s.ts }),
                    None => None,
                };
                In::Welcome { you, host, control, queue, members, state, tick, now }
            }
            In::State(s) => In::State(Stamped { v: s.v.checked()?, ts: s.ts }),
            In::Req { from, name, cmd: Cmd::Add { track } } => In::Req { from, name, cmd: Cmd::Add { track: peer_track(track)? } },
            In::Req { from, name, cmd: Cmd::Next { track } } => In::Req { from, name, cmd: Cmd::Next { track: peer_track(track)? } },
            m => m,
        })
    }
}

impl RoomState {
    fn checked(mut self) -> Option<Self> {
        if !self.vid.is_empty() && !is_video_id(&self.vid) {
            return None;
        }
        self.queue = self.queue.into_iter().map(peer_track).collect::<Option<_>>()?;
        Some(self)
    }
}

/// A song from another member of the room, who could send anything: only a YouTube
/// video ID, which ends up in file names, and only pictures from YouTube's own image
/// hosts over https, which are fetched and handed to the system's media controls.
fn peer_track(mut t: Track) -> Option<Track> {
    if !is_video_id(&t.id) || t.stands_for.as_deref().is_some_and(|v| !is_video_id(v)) {
        return None;
    }
    t.thumbs.retain(|th| youtube_image(&th.url));
    Some(t)
}

/// 11 letters, digits, `-` or `_`.
pub fn is_video_id(id: &str) -> bool {
    id.len() == 11 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// An https link to one of the hosts YouTube's pictures come from.
fn youtube_image(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let ours = |domain: &str| host == domain || host.strip_suffix(domain).is_some_and(|sub| sub.ends_with('.'));
    host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') && (ours("ytimg.com") || ours("googleusercontent.com") || ours("ggpht.com"))
}

#[derive(Deserialize)]
struct Stamped<T> {
    #[serde(flatten)]
    v: T,
    ts: i64,
}

fn yes() -> bool {
    true
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// Why a connection ended.
enum End {
    /// Left, or the room ended: nothing to show or retry.
    Done(Option<String>),
    /// The server refused us (bad code, full...): show it, don't retry.
    Refused(String),
    /// The network dropped: try again.
    Dropped(String),
}

struct Conn {
    code: String,
    create: bool,
    name: String,
    events: Sender<Event>,
    wake: Arc<dyn Fn() + Send + Sync>,
    /// Server clock minus ours, in ms.
    offset: Arc<AtomicI64>,
}

impl Conn {
    fn emit(&self, e: Event) {
        let _ = self.events.send(e);
        (self.wake)();
    }

    /// When (locally) the server time `ts` was.
    fn local_instant(&self, ts: i64) -> Instant {
        let age = (now_ms() + self.offset.load(Ordering::Relaxed) - ts).clamp(0, 60_000);
        Instant::now() - Duration::from_millis(age as u64)
    }

    async fn run(mut self, mut out: mpsc::UnboundedReceiver<Out>) {
        let mut retries = 0;
        loop {
            let started = Instant::now();
            let end = self.session(&mut out).await;
            // Drops spread over a long jam shouldn't add up to giving up.
            if started.elapsed() > Duration::from_secs(120) {
                retries = 0;
            }
            match end {
                End::Done(msg) => {
                    if let Some(m) = msg {
                        self.emit(Event::Closed(m));
                    }
                    return;
                }
                End::Refused(msg) => {
                    self.emit(Event::Closed(msg));
                    return;
                }
                End::Dropped(e) => {
                    log::warn!("jam: connection dropped: {e}");
                    // The room survives a few minutes without anyone in it; rejoin it.
                    self.create = false;
                    retries += 1;
                    if retries > 6 {
                        self.emit(Event::Closed("Lost the connection to the jam".into()));
                        return;
                    }
                    self.emit(Event::Reconnecting);
                    tokio::time::sleep(Duration::from_secs(2 * retries)).await;
                    // Leaving while reconnecting.
                    if matches!(out.try_recv(), Ok(Out::Leave)) || out.is_closed() {
                        return;
                    }
                }
            }
        }
    }

    async fn session(&mut self, out: &mut mpsc::UnboundedReceiver<Out>) -> End {
        let base = std::env::var("YTM_JAM_SERVER").unwrap_or_else(|_| SERVER.to_owned());
        let name: String = url_encode(&self.name);
        let url = format!("{}/jam/{}?create={}&name={name}&v=1", base.trim_end_matches('/'), self.code, self.create as u8);
        // Through the proxy setting, like everything else.
        let connect = async {
            let u = url::Url::parse(&url)?;
            let host = u.host_str().unwrap_or_default().to_owned();
            let port = u.port_or_known_default().unwrap_or(443);
            let tcp = crate::proxy::connect(&host, port).await?;
            anyhow::Ok(tokio_tungstenite::client_async_tls(url, tcp).await?)
        };
        let ws = match tokio::time::timeout(Duration::from_secs(10), connect).await {
            Ok(Ok((ws, _))) => ws,
            Ok(Err(e)) => return End::Dropped(e.to_string()),
            Err(_) => return End::Dropped("timed out".into()),
        };
        let (mut tx, mut rx) = ws.split();
        let mut keepalive = tokio::time::interval(Duration::from_secs(25));
        keepalive.tick().await;
        // Clock sync: a few pings right after joining; the quickest round trip wins.
        let mut sync = tokio::time::interval(Duration::from_millis(200));
        let mut pings_left = 0u32;
        let mut best_rtt = i64::MAX;
        let mut last_rx = Instant::now();
        loop {
            tokio::select! {
                msg = out.recv() => match msg {
                    Some(Out::Text(t)) => {
                        if let Err(e) = tx.send(Message::text(t)).await {
                            return End::Dropped(e.to_string());
                        }
                    }
                    Some(Out::Leave) | None => {
                        let _ = tx.send(Message::text(r#"{"t":"bye"}"#)).await;
                        let _ = tx.close().await;
                        return End::Done(None);
                    }
                },
                _ = keepalive.tick() => {
                    // The server answers every ping; silence means the connection is dead.
                    if last_rx.elapsed() > Duration::from_secs(60) {
                        return End::Dropped("no reply from the server".into());
                    }
                    if let Err(e) = tx.send(Message::text("ping")).await {
                        return End::Dropped(e.to_string());
                    }
                }
                _ = sync.tick(), if pings_left > 0 => {
                    pings_left -= 1;
                    let _ = tx.send(Message::text(format!(r#"{{"t":"ping","c":{}}}"#, now_ms()))).await;
                }
                msg = rx.next() => {
                    last_rx = Instant::now();
                    let text = match msg {
                        Some(Ok(Message::Text(t))) => t,
                        Some(Ok(Message::Close(_))) | None => return End::Dropped("closed by server".into()),
                        Some(Ok(_)) => continue,
                        Some(Err(e)) => return End::Dropped(e.to_string()),
                    };
                    if text.as_str() == "pong" {
                        continue;
                    }
                    let Ok(m) = serde_json::from_str::<In>(&text) else {
                        log::warn!("jam: unreadable message {text}");
                        continue;
                    };
                    // What other members sent is checked before anything uses it.
                    let Some(m) = m.checked() else {
                        log::warn!("jam: dropped a message with a song that isn't a YouTube video");
                        continue;
                    };
                    match m {
                        In::Welcome { you, host, control, queue, members, state, tick, now } => {
                            // A first guess until the pings answer.
                            self.offset.store(now - now_ms(), Ordering::Relaxed);
                            pings_left = 5;
                            self.create = false;
                            self.emit(Event::Welcome { you, host, control, queue, members });
                            if let Some(s) = state {
                                let at = self.local_instant(s.ts);
                                self.emit(Event::State(s.v, at));
                            }
                            if let Some(t) = tick {
                                let at = self.local_instant(t.ts);
                                self.emit(Event::Tick(t.v, at));
                            }
                        }
                        In::Members { host, members } => self.emit(Event::Members { host, members }),
                        In::Control { on, queue } => self.emit(Event::Control { on, queue }),
                        In::State(s) => {
                            let at = self.local_instant(s.ts);
                            self.emit(Event::State(s.v, at));
                        }
                        In::Tick(t) => {
                            let at = self.local_instant(t.ts);
                            self.emit(Event::Tick(t.v, at));
                        }
                        In::Req { from, name, cmd } => self.emit(Event::Req { from, name, cmd }),
                        In::Pong { c, s } => {
                            let rtt = now_ms() - c;
                            if rtt >= 0 && rtt < best_rtt {
                                best_rtt = rtt;
                                self.offset.store(s - (c + rtt / 2), Ordering::Relaxed);
                            }
                        }
                        In::Error { code, msg } => {
                            let text = match code.as_str() {
                                "not_found" => "No jam with that code. Check it and try again".to_owned(),
                                "taken" => "That jam code is taken, try again".to_owned(),
                                "full" => "That jam is full".to_owned(),
                                "denied" => msg.clone().unwrap_or_else(|| "Only the host can do that".to_owned()),
                                "rate" => "Slow down a little".to_owned(),
                                _ => msg.unwrap_or(code.clone()),
                            };
                            if matches!(code.as_str(), "denied" | "rate") {
                                self.emit(Event::Notice(text));
                            } else {
                                return End::Refused(text);
                            }
                        }
                        In::Ended => return End::Done(Some("The host ended the jam".into())),
                    }
                }
            }
        }
    }
}

fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes() {
        for _ in 0..50 {
            let c = new_code();
            assert_eq!(normalize_code(&c).as_deref(), Some(c.as_str()));
        }
        assert_eq!(normalize_code(" abc-234 ").as_deref(), Some("ABC234"));
        assert_eq!(normalize_code("ABC23O"), None);
        assert_eq!(normalize_code("ABC2345"), None);
    }

    #[test]
    fn songs_from_the_room_are_checked() {
        let song = |id: &str, thumb: &str| Track { id: id.into(), thumbs: vec![crate::model::Thumb { url: thumb.into(), width: 60, height: 60 }], ..Default::default() };
        let ok = "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg";
        assert!(peer_track(song("dQw4w9WgXcQ", ok)).is_some_and(|t| t.thumbs.len() == 1));
        // An ID that would be a path, or anything that isn't a video ID.
        for id in ["../../../../x", r"C:\Windows\x", r"\\host\share\x", "/tmp/ytmrs-poc", "dQw4w9WgXcQ/", "short", ""] {
            assert!(peer_track(song(id, ok)).is_none(), "{id}");
        }
        // Pictures from anywhere else are dropped; the song stays.
        for url in [
            r"file://\\host\share\a.jpg",
            "http://i.ytimg.com/vi/x/a.jpg",
            "https://evil.example/a.jpg",
            "https://ytimg.com.evil.example/a.jpg",
            "https://evilytimg.com/a.jpg",
            "https://user@i.ytimg.com/a.jpg",
            "https://i.ytimg.com:8080/a.jpg",
            "https://127.0.0.1/a.jpg",
        ] {
            assert!(peer_track(song("dQw4w9WgXcQ", url)).is_some_and(|t| t.thumbs.is_empty()), "{url}");
        }
        assert!(youtube_image("https://lh3.googleusercontent.com/abc=w120"));
        assert!(youtube_image("https://yt3.ggpht.com/abc"));
        // One bad song drops the whole state, whose index counts its queue.
        let state = |q: Vec<Track>| RoomState { queue: q, vid: "dQw4w9WgXcQ".into(), ..Default::default() };
        assert!(state(vec![song("dQw4w9WgXcQ", ok)]).checked().is_some());
        assert!(state(vec![song("dQw4w9WgXcQ", ok), song("../x", ok)]).checked().is_none());
        assert!(RoomState { vid: "../x".into(), ..Default::default() }.checked().is_none());
        let req = |track| In::Req { from: "a".into(), name: "Sam".into(), cmd: Cmd::Add { track } };
        assert!(req(song("../../x", ok)).checked().is_none());
        assert!(req(song("dQw4w9WgXcQ", ok)).checked().is_some());
    }

    #[test]
    fn parses_server_messages() {
        let m: In = serde_json::from_str(r#"{"t":"tick","vid":"x","pos":1200,"playing":true,"ts":5}"#).unwrap();
        assert!(matches!(m, In::Tick(Stamped { ts: 5, .. })));
        let m: In = serde_json::from_str(r#"{"t":"req","from":"a","name":"Sam","cmd":{"k":"seek","pos":3000}}"#).unwrap();
        assert!(matches!(m, In::Req { cmd: Cmd::Seek { pos: 3000 }, .. }));
        let m: In = serde_json::from_str(r#"{"t":"welcome","you":"a","host":"a","control":false,"members":[{"id":"a","name":"Sam"}],"state":null,"tick":null,"now":1}"#).unwrap();
        assert!(matches!(m, In::Welcome { .. }));
    }
}
