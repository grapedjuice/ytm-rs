//! Discord Rich Presence: "Listening to <song>" with cover art and a progress bar.
//!
//! A worker thread owns the IPC pipe so a missing or restarting Discord never blocks the
//! UI. The UI sends the wanted state whenever it changes; the worker coalesces bursts
//! (Discord rate-limits activity updates) and reconnects every 15 s while Discord is
//! closed.

use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use discord_rich_presence::activity::{Activity, ActivityType, Assets, Button, StatusDisplayType, Timestamps};
use discord_rich_presence::{DiscordIpc, DiscordIpcClient};

/// Application ID from the Discord Developer Portal. Its name is what Discord shows
/// as "Listening to <name>" in the profile card.
const CLIENT_ID: &str = "1521756469433991299";

const RETRY: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, PartialEq)]
pub struct Presence {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub art: Option<String>,
    /// Unix ms when the song (virtually) started, so Discord draws elapsed time.
    pub start_ms: i64,
    pub end_ms: Option<i64>,
}

pub struct Discord {
    tx: Sender<Option<Presence>>,
}

impl Discord {
    pub fn spawn() -> Option<Self> {
        let id = std::env::var("YTM_DISCORD_ID").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| CLIENT_ID.to_owned());
        if id.is_empty() {
            return None;
        }
        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::Builder::new().name("discord".into()).spawn(move || run(&id, rx)).ok()?;
        Some(Self { tx })
    }

    /// `None` clears the status (paused / stopped).
    pub fn set(&self, p: Option<Presence>) {
        let _ = self.tx.send(p);
    }
}

fn run(id: &str, rx: Receiver<Option<Presence>>) {
    let mut client: Option<DiscordIpcClient> = None;
    let mut want: Option<Presence> = None;
    let mut dirty = false;
    let mut next_try = Instant::now();
    loop {
        match rx.recv_timeout(RETRY) {
            Ok(p) => {
                // Let a burst (skip, skip, seek) settle, then send only the last state.
                std::thread::sleep(Duration::from_millis(400));
                want = rx.try_iter().last().unwrap_or(p);
                dirty = true;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if !dirty {
            continue;
        }
        if client.is_none() {
            if Instant::now() < next_try {
                continue;
            }
            let mut c = DiscordIpcClient::new(id);
            match c.connect() {
                Ok(()) => {
                    log::info!("discord: connected");
                    client = Some(c);
                }
                Err(_) => {
                    next_try = Instant::now() + RETRY;
                    continue;
                }
            }
        }
        let c = client.as_mut().expect("connected above");
        let res = match &want {
            Some(p) => c.set_activity(activity(p)),
            None => c.clear_activity(),
        };
        // Read Discord's reply: it reports rejected payloads there, and leaving
        // replies unread would fill the pipe over a long session.
        let res = res.and_then(|()| c.recv());
        match res {
            Ok((_, reply)) => {
                if reply["evt"] == "ERROR" {
                    log::warn!("discord rejected activity: {}", reply["data"]);
                } else {
                    log::debug!("discord: activity set");
                }
                dirty = false;
            }
            Err(e) => {
                log::warn!("discord: {e}; reconnecting");
                let _ = c.close();
                client = None;
                next_try = Instant::now() + RETRY;
            }
        }
    }
    if let Some(mut c) = client {
        let _ = c.close();
    }
}

fn activity(p: &Presence) -> Activity<'_> {
    let mut ts = Timestamps::new().start(p.start_ms);
    if let Some(end) = p.end_ms {
        ts = ts.end(end);
    }
    let mut assets = Assets::new();
    if let Some(art) = p.art.as_deref().filter(|u| u.len() <= 256) {
        assets = assets.large_image(art);
        if let Some(album) = &p.album {
            assets = assets.large_text(field(album));
        }
    }
    let url = format!("https://music.youtube.com/watch?v={}", p.video_id);
    Activity::new()
        .activity_type(ActivityType::Listening)
        .status_display_type(StatusDisplayType::Details)
        .details(field(&p.title))
        .details_url(url.clone())
        .state(field(&p.artist))
        .timestamps(ts)
        .assets(assets)
        .buttons(vec![Button::new("Listen on YouTube Music", url)])
}

/// Discord rejects text fields shorter than 2 or longer than 128 characters.
fn field(s: &str) -> String {
    let mut out: String = s.chars().take(128).collect();
    while out.chars().count() < 2 {
        out.push('\u{2800}');
    }
    out
}
