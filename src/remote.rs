//! Opt-in remote control for a Stream Deck plugin: a WebSocket on `127.0.0.1` only.
//!
//! Browser pages send an `Origin` header and are refused, so a site the user has open
//! can't drive the player. Any other program on this machine can. There is no auth.
//!
//! Position is whole seconds. Finer than that isn't shown on a key, and the UI would
//! push an update every frame while a song plays.
//! ponytail: one-second position. Send milliseconds if a progress key needs it.

use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::{oneshot, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

/// `ws://127.0.0.1:26540`. Not `localhost`: that can resolve to IPv6, which isn't bound.
pub const PORT: u16 = 26540;

const MAX_MESSAGE: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RepeatMode {
    Off,
    All,
    One,
}

/// What a client is told, as one JSON text frame, on connect and whenever it changes.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Snapshot {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub cover_url: Option<String>,
    pub playing: bool,
    pub liked: bool,
    pub repeat: RepeatMode,
    /// 0.0–1.0.
    pub volume: f32,
    pub position: u64,
    pub duration: u64,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            title: String::new(),
            artist: String::new(),
            album: None,
            cover_url: None,
            playing: false,
            liked: false,
            repeat: RepeatMode::Off,
            volume: 1.0,
            position: 0,
            duration: 0,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Cmd {
    Toggle,
    Next,
    Prev,
    VolumeUp,
    VolumeDown,
    SetVolume(f32),
    /// `None` toggles the heart of what's playing.
    Like(Option<bool>),
    Shuffle,
    /// `None` cycles off, then all, then one.
    Repeat(Option<RepeatMode>),
}

pub struct Remote {
    cmds: Receiver<Cmd>,
    cmd_tx: Sender<Cmd>,
    state: watch::Sender<Snapshot>,
    /// Dropping it (or `stop`) ends the server thread.
    stop: Option<oneshot::Sender<()>>,
}

impl Remote {
    pub fn new() -> Self {
        let (cmd_tx, cmds) = unbounded();
        let (state, rx) = watch::channel(Snapshot::default());
        drop(rx);
        Self {
            cmds,
            cmd_tx,
            state,
            stop: None,
        }
    }

    pub fn is_running(&self) -> bool {
        self.stop.is_some()
    }

    /// Listen until [`Self::stop`]. A port already taken is retried; the previous
    /// thread may still be letting go of it.
    pub fn start(&mut self, wake: impl Fn() + Send + Sync + 'static) {
        if self.stop.is_some() {
            return;
        }
        let (stop_tx, stop_rx) = oneshot::channel();
        let cmd_tx = self.cmd_tx.clone();
        let state = self.state.subscribe();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let spawned = std::thread::Builder::new()
            .name("remote".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        log::warn!("remote control: {e}");
                        return;
                    }
                };
                rt.block_on(run(cmd_tx, state, wake, stop_rx));
            });
        if let Err(e) = spawned {
            log::warn!("remote control: {e}");
            return;
        }
        self.stop = Some(stop_tx);
    }

    pub fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }

    /// Send `snap` to every client, if it differs from the last one.
    pub fn publish(&self, snap: Snapshot) {
        self.state.send_if_modified(|cur| {
            if *cur == snap {
                false
            } else {
                *cur = snap;
                true
            }
        });
    }

    pub fn drain(&self) -> Vec<Cmd> {
        self.cmds.try_iter().collect()
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn run(
    cmds: Sender<Cmd>,
    state: watch::Receiver<Snapshot>,
    wake: Arc<dyn Fn() + Send + Sync>,
    mut stop: oneshot::Receiver<()>,
) {
    let mut logged = false;
    loop {
        tokio::select! {
            _ = &mut stop => return,
            bound = TcpListener::bind(("127.0.0.1", PORT)) => {
                match bound {
                    Ok(listener) => {
                        log::info!("remote control on ws://127.0.0.1:{PORT}");
                        logged = false;
                        tokio::select! {
                            _ = &mut stop => return,
                            _ = accept_loop(listener, state.clone(), cmds.clone(), wake.clone()) => {}
                        }
                    }
                    Err(e) => {
                        if !logged {
                            log::warn!("remote control on 127.0.0.1:{PORT}: {e}");
                            logged = true;
                        }
                        tokio::select! {
                            _ = &mut stop => return,
                            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
                        }
                    }
                }
            }
        }
    }
}

fn socket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE))
}

/// `Err` when the request comes from a browser (`Origin` set). The socket then answers 403.
fn reject_browser_origin(request: &Request, response: Response) -> Result<Response, ErrorResponse> {
    if request.headers().contains_key(http::header::ORIGIN) {
        return Err(http::Response::builder()
            .status(http::StatusCode::FORBIDDEN)
            .body(Some("browser origins are not allowed".into()))
            .expect("403 response"));
    }
    Ok(response)
}

async fn accept_loop(
    listener: TcpListener,
    state: watch::Receiver<Snapshot>,
    cmds: Sender<Cmd>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let state = state.clone();
        let cmds = cmds.clone();
        let wake = wake.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(stream, state, cmds, wake).await {
                log::debug!("remote client: {e}");
            }
        });
    }
}

async fn handle(
    stream: tokio::net::TcpStream,
    mut state: watch::Receiver<Snapshot>,
    cmds: Sender<Cmd>,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<(), tokio_tungstenite::tungstenite::Error> {
    let ws = tokio_tungstenite::accept_hdr_async_with_config(
        stream,
        reject_browser_origin,
        Some(socket_config()),
    )
    .await?;
    let (mut write, mut read) = ws.split();
    let snap = state.borrow_and_update().clone();
    write.send(Message::text(snapshot_json(&snap))).await?;
    loop {
        tokio::select! {
            changed = state.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
                let snap = state.borrow_and_update().clone();
                write.send(Message::text(snapshot_json(&snap))).await?;
            }
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match parse_cmd(&text) {
                            Some(cmd) => {
                                let _ = cmds.send(cmd);
                                wake();
                            }
                            None => log::debug!("remote: ignored {}", preview(&text)),
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e),
                }
            }
        }
    }
}

fn snapshot_json(snap: &Snapshot) -> String {
    serde_json::to_string(snap).unwrap_or_else(|_| "{}".into())
}

fn preview(text: &str) -> String {
    let mut s: String = text.chars().take(80).collect();
    if text.chars().nth(80).is_some() {
        s.push('…');
    }
    s
}

/// `None` for a frame that isn't a command. The connection stays up.
fn parse_cmd(text: &str) -> Option<Cmd> {
    let v: Value = serde_json::from_str(text).ok()?;
    let cmd = v.get("cmd")?.as_str()?;
    let value = v.get("value");
    Some(match cmd {
        "toggle" => Cmd::Toggle,
        "next" => Cmd::Next,
        "prev" => Cmd::Prev,
        "volume_up" => Cmd::VolumeUp,
        "volume_down" => Cmd::VolumeDown,
        "set_volume" => {
            let n = value?.as_f64()?;
            if !n.is_finite() {
                return None;
            }
            Cmd::SetVolume(n as f32)
        }
        "like" => Cmd::Like(match value {
            None => None,
            Some(v) => Some(v.as_bool()?),
        }),
        "shuffle" => Cmd::Shuffle,
        "repeat" => Cmd::Repeat(match value {
            None => None,
            Some(v) => Some(match v.as_str()? {
                "off" => RepeatMode::Off,
                "all" => RepeatMode::All,
                "one" => RepeatMode::One,
                _ => return None,
            }),
        }),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_parse() {
        assert_eq!(parse_cmd(r#"{"cmd":"toggle"}"#), Some(Cmd::Toggle));
        assert_eq!(parse_cmd(r#"{"cmd":"next"}"#), Some(Cmd::Next));
        assert_eq!(parse_cmd(r#"{"cmd":"prev"}"#), Some(Cmd::Prev));
        assert_eq!(parse_cmd(r#"{"cmd":"volume_up"}"#), Some(Cmd::VolumeUp));
        assert_eq!(parse_cmd(r#"{"cmd":"volume_down"}"#), Some(Cmd::VolumeDown));
        assert_eq!(
            parse_cmd(r#"{"cmd":"set_volume","value":0.5}"#),
            Some(Cmd::SetVolume(0.5))
        );
        assert_eq!(parse_cmd(r#"{"cmd":"like"}"#), Some(Cmd::Like(None)));
        assert_eq!(
            parse_cmd(r#"{"cmd":"like","value":false}"#),
            Some(Cmd::Like(Some(false)))
        );
        assert_eq!(parse_cmd(r#"{"cmd":"shuffle"}"#), Some(Cmd::Shuffle));
        assert_eq!(parse_cmd(r#"{"cmd":"repeat"}"#), Some(Cmd::Repeat(None)));
        assert_eq!(
            parse_cmd(r#"{"cmd":"repeat","value":"one"}"#),
            Some(Cmd::Repeat(Some(RepeatMode::One)))
        );
    }

    #[test]
    fn junk_is_not_a_command() {
        for bad in [
            "",
            "nope",
            "{}",
            r#"{"cmd":"fly"}"#,
            r#"{"cmd":"like","value":"yes"}"#,
            r#"{"cmd":"set_volume"}"#,
            r#"{"cmd":"repeat","value":"two"}"#,
        ] {
            assert_eq!(parse_cmd(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_browser_origin_is_refused_and_a_plain_client_is_not() {
        let browser = http::Request::builder()
            .uri("/")
            .header(http::header::ORIGIN, "https://evil.test")
            .body(())
            .unwrap();
        let plain = http::Request::builder().uri("/").body(()).unwrap();
        let response = || http::Response::builder().status(101).body(()).unwrap();
        let refused = reject_browser_origin(&browser, response()).unwrap_err();
        assert_eq!(refused.status(), http::StatusCode::FORBIDDEN);
        assert!(reject_browser_origin(&plain, response()).is_ok());
    }

    #[test]
    fn snapshot_json_uses_the_wire_names() {
        let snap = Snapshot {
            title: "Song".into(),
            artist: "Artist".into(),
            playing: true,
            ..Snapshot::default()
        };
        let v: Value = serde_json::from_str(&snapshot_json(&snap)).unwrap();
        assert_eq!(v["title"], "Song");
        assert_eq!(v["playing"], true);
        assert_eq!(v["repeat"], "off");
        assert!(v["album"].is_null());
    }

    #[tokio::test]
    async fn a_browser_handshake_is_refused_on_the_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (_tx, rx) = watch::channel(Snapshot::default());
        let (cmd_tx, _cmd_rx) = unbounded();
        tokio::spawn(accept_loop(listener, rx, cmd_tx, Arc::new(|| {})));

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        stream
            .write_all(
                b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nOrigin: https://evil.test\r\n\r\n",
            )
            .await
            .unwrap();
        let mut buf = [0u8; 128];
        let n = stream.read(&mut buf).await.unwrap();
        let head = String::from_utf8_lossy(&buf[..n]);
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
    }

    #[tokio::test]
    async fn a_client_gets_the_state_and_its_command_is_queued() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = watch::channel(Snapshot {
            title: "Song".into(),
            ..Snapshot::default()
        });
        let (cmd_tx, cmd_rx) = unbounded();
        tokio::spawn(accept_loop(listener, rx, cmd_tx, Arc::new(|| {})));

        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .unwrap();
        let msg = ws.next().await.unwrap().unwrap();
        let Message::Text(text) = msg else {
            panic!("expected text, got {msg:?}")
        };
        assert!(text.contains("\"title\":\"Song\""), "{text}");
        ws.send(Message::text(r#"{"cmd":"next"}"#)).await.unwrap();
        let cmd = tokio::task::spawn_blocking(move || cmd_rx.recv_timeout(Duration::from_secs(2)))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cmd, Cmd::Next);
        drop(tx);
    }
}
