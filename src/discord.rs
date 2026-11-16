//! Discord Rich Presence: "Listening to <song>" with cover art and a progress bar, laid
//! out as the listener sets it in Settings › Connections.
//!
//! A worker thread owns the IPC pipe so a missing or restarting Discord never blocks the
//! UI. The UI sends the wanted state whenever it changes; the worker coalesces bursts
//! (Discord rate-limits activity updates) and reconnects every 15 s while Discord is
//! closed.

use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use discord_rich_presence::activity::{Activity, ActivityType, Assets, Button, StatusDisplayType, Timestamps};
use discord_rich_presence::{DiscordIpc, DiscordIpcClient};
use serde::{Deserialize, Serialize};

/// Application ID from the Discord Developer Portal. Its name is what Discord shows
/// as "Listening to <name>" in the profile card.
const CLIENT_ID: &str = "1521756469433991299";

/// That application's name, unless the listener gives another.
pub const APP_NAME: &str = "Youtube Music";

/// The small badge on the artwork: ytm-rs's icon, from its website.
pub const BADGE: &str = "https://grapedjuice.github.io/ytm-rs/icon-192.png";

const RETRY: Duration = Duration::from_secs(15);

/// What a line of the card shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Show {
    Title,
    Artist,
    Album,
    Nothing,
}

impl Show {
    pub const ALL: [Self; 4] = [Self::Title, Self::Artist, Self::Album, Self::Nothing];

    pub fn label(self) -> &'static str {
        match self {
            Self::Title => "Song title",
            Self::Artist => "Artist",
            Self::Album => "Album",
            Self::Nothing => "Nothing",
        }
    }
}

/// What the member list shows after "Listening to".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Name,
    #[default]
    Line1,
    Line2,
}

impl Status {
    pub const ALL: [Self; 3] = [Self::Name, Self::Line1, Self::Line2];
}

/// A YouTube Music page a button or the artwork opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Page {
    None,
    Song,
    Artist,
    Album,
    /// The Listen together room, while there is one.
    Jam,
}

impl Page {
    pub const ALL: [Self; 5] = [Self::None, Self::Song, Self::Artist, Self::Album, Self::Jam];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Song => "Song",
            Self::Artist => "Artist",
            Self::Album => "Album",
            Self::Jam => "Listen together",
        }
    }

    fn button(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Song => "Listen on YouTube Music",
            Self::Artist => "View artist",
            Self::Album => "View album",
            Self::Jam => "Listen together",
        }
    }
}

/// How the card looks: Settings › Connections › Discord.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub when_paused: bool,
    pub hide_details: bool,
    /// Replaces [`APP_NAME`]; empty keeps it.
    pub app_name: String,
    pub status: Status,
    pub lines: [Show; 3],
    pub art: bool,
    pub badge: bool,
    pub progress: bool,
    /// Lines 1 and 2 open what they show.
    pub links: [bool; 2],
    pub art_link: bool,
    pub buttons: [Page; 2],
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            when_paused: false,
            hide_details: false,
            app_name: String::new(),
            status: Status::Line1,
            lines: [Show::Title, Show::Artist, Show::Album],
            art: true,
            badge: false,
            progress: true,
            links: [true, false],
            art_link: false,
            buttons: [Page::Song, Page::None],
        }
    }
}

/// The song as Discord gets to know it.
#[derive(Clone, Debug, PartialEq)]
pub struct Presence {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    pub artist_id: Option<String>,
    pub album: Option<String>,
    pub album_id: Option<String>,
    pub art: Option<String>,
    /// Unix ms when the song (virtually) started, so Discord draws elapsed time.
    pub start_ms: i64,
    pub end_ms: Option<i64>,
    pub paused: bool,
    /// The share link of the Listen together room we're in.
    pub jam: Option<String>,
}

impl Presence {
    fn url(&self, page: Page) -> Option<String> {
        match page {
            Page::None => None,
            Page::Song => Some(format!("https://music.youtube.com/watch?v={}", self.video_id)),
            Page::Artist => self.artist_id.as_ref().map(|id| format!("https://music.youtube.com/channel/{id}")),
            Page::Album => self.album_id.as_ref().map(|id| format!("https://music.youtube.com/browse/{id}")),
            Page::Jam => self.jam.clone(),
        }
    }

    /// What a line shows, and the page it opens when it's a link.
    fn line(&self, show: Show, link: bool) -> Option<Line> {
        let (text, page) = match show {
            Show::Title => (Some(&self.title), Page::Song),
            Show::Artist => (Some(&self.artist), Page::Artist),
            Show::Album => (self.album.as_ref(), Page::Album),
            Show::Nothing => (None, Page::None),
        };
        let text = text.filter(|t| !t.trim().is_empty())?.clone();
        Some(Line { text, url: link.then(|| self.url(page)).flatten() })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub text: String,
    pub url: Option<String>,
}

/// The card as Discord draws it, from a song and the listener's settings. Settings'
/// preview draws the same thing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Card {
    pub name: Option<String>,
    pub status: Status,
    /// The third is the artwork's hover text, so it needs the artwork.
    pub lines: [Option<Line>; 3],
    pub art: Option<String>,
    pub art_url: Option<String>,
    pub badge: bool,
    /// Start and end, Unix ms.
    pub times: Option<(i64, Option<i64>)>,
    pub buttons: Vec<(&'static str, String)>,
}

impl Card {
    pub fn new(p: &Presence, prefs: &Prefs) -> Self {
        let name = Some(prefs.app_name.trim()).filter(|n| !n.is_empty()).map(str::to_owned);
        if prefs.hide_details {
            return Self { name, status: Status::Name, ..Default::default() };
        }
        // Discord drops artwork with a longer address.
        let art = p.art.clone().filter(|u| prefs.art && u.len() <= 256);
        let lines = [
            p.line(prefs.lines[0], prefs.links[0]),
            p.line(prefs.lines[1], prefs.links[1]),
            art.as_ref().and_then(|_| p.line(prefs.lines[2], false)),
        ];
        // A status line with nothing on it falls back to the app's name, as Discord does.
        let status = match prefs.status {
            Status::Line1 if lines[0].is_none() => Status::Name,
            Status::Line2 if lines[1].is_none() => Status::Name,
            s => s,
        };
        let art_url = art.as_ref().filter(|_| prefs.art_link).and_then(|_| p.url(Page::Album).or_else(|| p.url(Page::Song)));
        let mut buttons: Vec<(&'static str, String)> = Vec::new();
        for page in prefs.buttons {
            if let Some(url) = p.url(page).filter(|u| buttons.iter().all(|(_, b)| b != u)) {
                buttons.push((page.button(), url));
            }
        }
        Self {
            name,
            status,
            lines,
            badge: prefs.badge && art.is_some(),
            art,
            art_url,
            times: (prefs.progress && !p.paused).then_some((p.start_ms, p.end_ms)),
            buttons,
        }
    }

    pub fn name(&self) -> &str {
        self.name.as_deref().unwrap_or(APP_NAME)
    }

    /// Whether Discord already shows `other`. The clock moves on while a song plays, so
    /// only a jump of 2 s or more is a real seek.
    pub fn same(&self, other: &Self) -> bool {
        let Self { name, status, lines, art, art_url, badge, times, buttons } = self;
        let near = |a: i64, b: i64| (a - b).abs() < 2000;
        let near_end = |a: Option<i64>, b: Option<i64>| match (a, b) {
            (Some(a), Some(b)) => near(a, b),
            (a, b) => a.is_none() && b.is_none(),
        };
        let times = match (*times, other.times) {
            (Some((a, x)), Some((b, y))) => near(a, b) && near_end(x, y),
            (a, b) => a.is_none() && b.is_none(),
        };
        times
            && *name == other.name
            && *status == other.status
            && *lines == other.lines
            && *art == other.art
            && *art_url == other.art_url
            && *badge == other.badge
            && *buttons == other.buttons
    }

    /// The member list's "Listening to …".
    pub fn status_text(&self) -> &str {
        let line = |i: usize| self.lines[i].as_ref().map(|l| l.text.as_str());
        match self.status {
            Status::Name => None,
            Status::Line1 => line(0),
            Status::Line2 => line(1),
        }
        .unwrap_or(self.name())
    }
}

pub struct Discord {
    tx: Sender<Option<Card>>,
}

impl Discord {
    pub fn spawn() -> Option<Self> {
        let id = std::env::var("YTM_DISCORD_ID").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| CLIENT_ID.to_owned());
        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::Builder::new().name("discord".into()).spawn(move || run(&id, rx)).ok()?;
        Some(Self { tx })
    }

    /// `None` clears the status (paused / stopped).
    pub fn set(&self, card: Option<Card>) {
        let _ = self.tx.send(card);
    }
}

fn run(id: &str, rx: Receiver<Option<Card>>) {
    let mut client: Option<DiscordIpcClient> = None;
    let mut want: Option<Card> = None;
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
            Some(card) => c.set_activity(activity(card)),
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

fn activity(c: &Card) -> Activity<'_> {
    let mut a = Activity::new().activity_type(ActivityType::Listening).status_display_type(match c.status {
        Status::Name => StatusDisplayType::Name,
        Status::Line1 => StatusDisplayType::Details,
        Status::Line2 => StatusDisplayType::State,
    });
    if let Some(name) = &c.name {
        a = a.name(field(name));
    }
    if let Some(l) = &c.lines[0] {
        a = a.details(field(&l.text));
        if let Some(u) = &l.url {
            a = a.details_url(u.as_str());
        }
    }
    if let Some(l) = &c.lines[1] {
        a = a.state(field(&l.text));
        if let Some(u) = &l.url {
            a = a.state_url(u.as_str());
        }
    }
    if let Some(art) = &c.art {
        let mut assets = Assets::new().large_image(art.as_str());
        if let Some(l) = &c.lines[2] {
            assets = assets.large_text(field(&l.text));
        }
        if let Some(u) = &c.art_url {
            assets = assets.large_url(u.as_str());
        }
        if c.badge {
            assets = assets.small_image(BADGE).small_text(field(c.name()));
        }
        a = a.assets(assets);
    }
    if let Some((start, end)) = c.times {
        let mut ts = Timestamps::new().start(start);
        if let Some(end) = end {
            ts = ts.end(end);
        }
        a = a.timestamps(ts);
    }
    a.buttons(c.buttons.iter().map(|(label, url)| Button::new(*label, url.as_str())).collect())
}

/// Discord rejects text fields shorter than 2 or longer than 128 characters.
fn field(s: &str) -> String {
    let mut out: String = s.chars().take(128).collect();
    while out.chars().count() < 2 {
        out.push('\u{2800}');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song() -> Presence {
        Presence {
            video_id: "v1".into(),
            title: "Believer".into(),
            artist: "Imagine Dragons".into(),
            artist_id: Some("UC1".into()),
            album: Some("Evolve".into()),
            album_id: None,
            art: Some("https://img/1".into()),
            start_ms: 1000,
            end_ms: Some(200_000),
            paused: false,
            jam: None,
        }
    }

    const SONG_URL: &str = "https://music.youtube.com/watch?v=v1";
    const ARTIST_URL: &str = "https://music.youtube.com/channel/UC1";

    #[test]
    fn defaults_keep_the_old_card() {
        let c = Card::new(&song(), &Prefs::default());
        assert_eq!(c.lines[0], Some(Line { text: "Believer".into(), url: Some(SONG_URL.into()) }));
        assert_eq!(c.lines[1], Some(Line { text: "Imagine Dragons".into(), url: None }));
        assert_eq!(c.lines[2].as_ref().map(|l| l.text.as_str()), Some("Evolve"));
        assert_eq!(c.times, Some((1000, Some(200_000))));
        assert_eq!(c.buttons, vec![("Listen on YouTube Music", SONG_URL.to_owned())]);
        assert_eq!(c.status_text(), "Believer");
        assert_eq!(c.name(), APP_NAME);
    }

    #[test]
    fn hidden_details_leave_only_the_name() {
        let prefs = Prefs { hide_details: true, app_name: " Music ".into(), ..Prefs::default() };
        let c = Card::new(&song(), &prefs);
        assert_eq!(c, Card { name: Some("Music".into()), status: Status::Name, ..Default::default() });
        assert_eq!(c.status_text(), "Music");
    }

    #[test]
    fn line_three_and_badge_need_the_art() {
        let prefs = Prefs { art: false, badge: true, ..Prefs::default() };
        let c = Card::new(&song(), &prefs);
        assert!(c.lines[2].is_none() && !c.badge && c.art.is_none());
    }

    #[test]
    fn same_card_allows_clock_drift_not_seeks_or_edits() {
        let prefs = Prefs::default();
        let c = Card::new(&song(), &prefs);
        let at = |start_ms: i64| Card::new(&Presence { start_ms, end_ms: song().end_ms.map(|e| e + start_ms - song().start_ms), ..song() }, &prefs);
        assert!(c.same(&at(song().start_ms + 150)));
        assert!(!c.same(&at(song().start_ms + 5_000)));
        assert!(!c.same(&Card::new(&song(), &Prefs { app_name: "Mine".into(), ..Prefs::default() })));
        // Paused, the clock doesn't show.
        let paused = |start_ms: i64| Card::new(&Presence { paused: true, start_ms, ..song() }, &prefs);
        assert!(paused(0).same(&paused(60_000)));
    }

    #[test]
    fn paused_drops_the_progress_bar() {
        let c = Card::new(&Presence { paused: true, ..song() }, &Prefs::default());
        assert_eq!(c.times, None);
        assert!(Card::new(&song(), &Prefs::default()).times.is_some());
    }

    #[test]
    fn jam_button_only_in_a_room() {
        let prefs = Prefs { buttons: [Page::Jam, Page::None], ..Prefs::default() };
        assert!(Card::new(&song(), &prefs).buttons.is_empty());
        let link = "https://grapedjuice.github.io/ytm-rs/jam/?c=ABC234";
        let c = Card::new(&Presence { jam: Some(link.into()), ..song() }, &prefs);
        assert_eq!(c.buttons, vec![("Listen together", link.to_owned())]);
    }

    #[test]
    fn links_and_buttons_skip_missing_pages() {
        let prefs = Prefs {
            lines: [Show::Album, Show::Artist, Show::Title],
            links: [true, true],
            art_link: true,
            buttons: [Page::Album, Page::Artist],
            ..Prefs::default()
        };
        let c = Card::new(&song(), &prefs);
        // No album id: the album line isn't a link, and the artwork opens the song.
        assert_eq!(c.lines[0].as_ref().and_then(|l| l.url.clone()), None);
        assert_eq!(c.lines[1].as_ref().and_then(|l| l.url.as_deref()), Some(ARTIST_URL));
        assert_eq!(c.art_url.as_deref(), Some(SONG_URL));
        assert_eq!(c.buttons, vec![("View artist", ARTIST_URL.to_owned())]);
    }

    #[test]
    fn same_button_twice_shows_once() {
        let prefs = Prefs { buttons: [Page::Song, Page::Song], ..Prefs::default() };
        assert_eq!(Card::new(&song(), &prefs).buttons.len(), 1);
    }

    #[test]
    fn empty_status_line_falls_back_to_the_name() {
        let prefs = Prefs { lines: [Show::Nothing, Show::Artist, Show::Album], ..Prefs::default() };
        let c = Card::new(&song(), &prefs);
        assert_eq!((c.status, c.status_text()), (Status::Name, APP_NAME));
    }

    #[test]
    fn prefs_saved_before_a_field_existed_still_load() {
        let p: Prefs = serde_json::from_str(r#"{"when_paused":true}"#).unwrap();
        assert_eq!(p, Prefs { when_paused: true, ..Prefs::default() });
    }
}
