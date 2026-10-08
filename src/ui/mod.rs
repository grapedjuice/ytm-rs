//! The application UI: state, playback logic and the per-frame layout.
//!
//! Layout: animated album-art background (shader.rs) → sidebar, top bar and page
//! content → player bar → now-playing overlay (slides up) → toasts.

mod nowplaying;
mod pages;
mod shell;
mod widgets;

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossbeam_channel::Receiver;
use egui::{Color32, Ui};
use souvlaki::MediaControlEvent;

use crate::audio::{AudioCmd, AudioEvent, AudioHandle};
use crate::backend::{self, ArtistPage, Backend, Collection, Explore, Library, Req, Resp, SearchResults};
use crate::lyrics::Lyrics;
use crate::media::Media;
use crate::model::{Chip, Shelf, ShelfKind, Target, Track};
use crate::shader::Background;
use crate::theme;

#[derive(Clone, PartialEq, Debug)]
pub enum Page {
    Home,
    Explore,
    Library(LibTab),
    Search(String),
    Album(String),
    Playlist(String),
    Artist(String),
    Settings,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LibTab {
    Playlists,
    Songs,
    Albums,
    History,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Repeat {
    Off,
    All,
    One,
}

#[derive(Clone, Copy, PartialEq)]
pub enum NpTab {
    Lyrics,
    UpNext,
}

/// User intents collected while drawing; applied after the frame's UI pass.
pub enum Action {
    Play(Vec<Track>, usize),
    PlayCollection(Target, bool),
    /// Click on a card: songs start a radio, everything else opens its page.
    Open(Target),
    PlayNext(Track),
    Enqueue(Track),
    Radio(Track),
    Go(Page),
    QueueJump(usize),
    QueueRemove(usize),
    Seek(f32),
    Like(String, bool),
    Dislike(String),
    Share(Target),
    AddToPlaylist(Track),
    NewPlaylist(Option<Track>),
    EditPlaylist,
    DeletePlaylist,
    SaveCollection(String, bool),
    HomeChip(Option<String>),
    OpenNowPlaying(bool),
    /// Play/pause what's already loaded (a card's button on the playing item).
    TogglePlay,
}

enum Dialog {
    PickPlaylist(Track),
    NewPlaylist { title: String, track: Option<Track> },
    EditPlaylist { id: String, title: String, description: String, privacy: Option<String> },
    ConfirmDelete { id: String, title: String },
}

pub struct HomeState {
    pub chips: Vec<Chip>,
    pub shelves: Vec<Shelf>,
    pub continuation: Option<String>,
    pub loading_more: bool,
    pub picks_loading: bool,
    pub picks_expanded: bool,
}

pub struct Channels {
    pub resp_rx: Receiver<Resp>,
    pub audio_rx: Receiver<AudioEvent>,
    pub media_rx: Receiver<MediaControlEvent>,
}

pub struct App {
    backend: Backend,
    audio: AudioHandle,
    ch: Channels,
    media: Option<Media>,
    discord: Option<crate::discord::Discord>,
    discord_enabled: bool,
    /// Last state sent to Discord, to send only real changes.
    discord_sent: Option<crate::discord::Presence>,
    /// Last session's song, restored paused: Play starts it from here.
    resume_at: Option<Duration>,
    /// Album / playlist / artist the queue was started from, so its card shows pause.
    queue_source: Option<Target>,
    /// Source of a PlayCollection request until its tracks arrive.
    pending_source: Option<Target>,
    bg: Option<Background>,

    // navigation
    page: Page,
    back: Vec<Page>,
    forward: Vec<Page>,
    page_changed: f64,
    loading: bool,
    search_text: String,
    suggestions: (String, Vec<String>),
    suggest_due: Option<f64>,

    // page data
    home: Option<HomeState>,
    home_loading: bool,
    explore: Option<Explore>,
    search: Option<SearchResults>,
    collection: Option<Collection>,
    artist: Option<ArtistPage>,
    library: Option<Library>,
    history: Option<Vec<Track>>,
    liked: HashSet<String>,
    account_name: Option<String>,
    account_avatar: Option<String>,

    // playback
    queue: Vec<Track>,
    index: Option<usize>,
    generation: u64,
    buffering: bool,
    repeat: Repeat,
    autoplay: bool,
    radio_pending: bool,
    seek_drag: Option<f32>,

    // visuals
    art_key: Option<String>,
    art_changed: f64,
    accent: Color32,
    default_theme: bool,
    anim_bg: bool,
    reactive_bg: bool,
    /// Background animation clock: advances only while music plays, so pausing
    /// freezes the motion instead of jumping when resumed.
    bg_time: f64,
    last_frame: f64,
    search_rect: Option<egui::Rect>,
    search_focused: bool,
    /// Mouse-wheel distance still to be scrolled (smooth scrolling).
    wheel_pending: egui::Vec2,
    wheel_mods: egui::Modifiers,

    // now playing
    np_open: bool,
    np_tab: NpTab,
    queue_open: bool,
    fullscreen: bool,
    lyrics: Option<(String, Option<Lyrics>)>,
    lyrics_user_scroll: f64,

    // account / misc
    logged_in: bool,
    signing_in: Arc<AtomicBool>,
    cookie_input: String,
    toast: Option<(String, f64)>,
    actions: Vec<Action>,
    dialog: Option<Dialog>,
    share_pending: Option<String>,
    smoke: Option<String>,
}

impl App {
    pub fn new(
        backend: Backend,
        audio: AudioHandle,
        ch: Channels,
        media: Option<Media>,
        bg: Option<Background>,
        storage: Option<&dyn eframe::Storage>,
    ) -> Self {
        let get = |k: &str| storage.and_then(|s| s.get_string(k));
        let discord_enabled = get("discord_enabled").is_none_or(|v| v == "1");
        let logged_in = backend.is_logged_in();
        backend.request(Req::Home(None));
        if logged_in {
            backend.request(Req::Library);
            backend.warm_signed_in();
        }
        let mut app = Self {
            backend,
            audio,
            ch,
            media,
            discord: discord_enabled.then(crate::discord::Discord::spawn).flatten(),
            discord_enabled,
            discord_sent: None,
            resume_at: None,
            queue_source: None,
            pending_source: None,
            bg,
            page: Page::Home,
            back: Vec::new(),
            forward: Vec::new(),
            page_changed: 0.0,
            loading: false,
            search_text: String::new(),
            suggestions: Default::default(),
            suggest_due: None,
            home: None,
            home_loading: true,
            explore: None,
            search: None,
            collection: None,
            artist: None,
            library: None,
            history: None,
            liked: HashSet::new(),
            account_name: None,
            account_avatar: None,
            queue: Vec::new(),
            index: None,
            generation: 0,
            buffering: false,
            repeat: match get("repeat").as_deref() {
                Some("all") => Repeat::All,
                Some("one") => Repeat::One,
                _ => Repeat::Off,
            },
            autoplay: get("autoplay").is_none_or(|v| v == "1"),
            radio_pending: false,
            seek_drag: None,
            art_key: None,
            art_changed: -10.0,
            accent: theme::RED,
            default_theme: get("default_theme").is_some_and(|v| v == "1"),
            anim_bg: get("anim_bg").is_none_or(|v| v == "1"),
            reactive_bg: get("reactive_bg").is_none_or(|v| v == "1"),
            bg_time: 0.0,
            last_frame: 0.0,
            search_rect: None,
            search_focused: false,
            wheel_pending: egui::Vec2::ZERO,
            wheel_mods: egui::Modifiers::NONE,
            np_open: false,
            np_tab: NpTab::Lyrics,
            queue_open: false,
            fullscreen: false,
            lyrics: None,
            lyrics_user_scroll: -10.0,
            logged_in,
            signing_in: Default::default(),
            cookie_input: String::new(),
            toast: None,
            actions: Vec::new(),
            dialog: None,
            share_pending: None,
            smoke: None,
        };
        // Dev hooks: YTM_SMOKE="query" searches and plays the first song ("id:<videoId>"
        // plays that video); YTM_SEARCH
        // only searches; YTM_PAGE opens a page; YTM_NOWPLAYING opens the overlay.
        if let Some(s) = get(SESSION_KEY).filter(|_| std::env::var_os("YTM_SMOKE").is_none()) {
            app.restore_session(&s);
        }
        let smoke = std::env::var("YTM_SMOKE").ok().filter(|q| !q.is_empty());
        if let Some(id) = smoke.as_deref().and_then(|q| q.strip_prefix("id:")) {
            let track = Track { id: id.to_owned(), title: id.to_owned(), artists: vec![], album: None, duration: None, thumbs: vec![], track_nr: None, plays: None };
            app.play_list(vec![track], 0);
        } else if let Some(q) = smoke {
            app.search_text = q.clone();
            app.go(Page::Search(q.clone()));
            app.smoke = Some(q);
        } else if let Some(q) = std::env::var("YTM_SEARCH").ok().filter(|q| !q.is_empty()) {
            app.search_text = q.clone();
            app.go(Page::Search(q));
        }
        match std::env::var("YTM_PAGE").as_deref() {
            Ok("home") => app.go(Page::Home),
            Ok("explore") => app.go(Page::Explore),
            Ok("library") => app.go(Page::Library(LibTab::Playlists)),
            Ok("settings") => app.go(Page::Settings),
            Ok(p) if p.starts_with("album:") => app.go(Page::Album(p[6..].to_owned())),
            Ok(p) if p.starts_with("artist:") => app.go(Page::Artist(p[7..].to_owned())),
            Ok(p) if p.starts_with("playlist:") => app.go(Page::Playlist(p[9..].to_owned())),
            _ => {}
        }
        if std::env::var("YTM_NOWPLAYING").is_ok() {
            app.np_open = true;
        }
        app
    }

    // ---------------------------------------------------------------- navigation

    fn go(&mut self, page: Page) {
        self.np_open = false;
        if page == self.page {
            return;
        }
        let prev = std::mem::replace(&mut self.page, page);
        self.back.push(prev);
        self.forward.clear();
        self.page_loaded();
    }

    fn nav_back(&mut self) {
        if self.np_open {
            self.np_open = false;
        } else if let Some(p) = self.back.pop() {
            self.forward.push(std::mem::replace(&mut self.page, p));
            self.page_loaded();
        }
    }

    fn nav_forward(&mut self) {
        if let Some(p) = self.forward.pop() {
            self.back.push(std::mem::replace(&mut self.page, p));
            self.page_loaded();
        }
    }

    /// Start the page transition and request data the page doesn't have yet.
    fn page_loaded(&mut self) {
        self.page_changed = -1.0; // stamped with the frame time on the next pass
        let req = match &self.page {
            Page::Home if self.home.is_none() && !self.home_loading => Some(Req::Home(None)),
            Page::Explore if self.explore.is_none() => Some(Req::Explore(self.preferred_artist_ids())),
            Page::Search(q) if self.search.as_ref().is_none_or(|s| &s.query != q) => Some(Req::Search(q.clone())),
            Page::Album(id) if self.collection.as_ref().is_none_or(|c| &c.id != id) => Some(Req::Album(id.clone())),
            Page::Playlist(id) if self.collection.as_ref().is_none_or(|c| &c.id != id) => {
                Some(Req::Playlist(id.clone()))
            }
            Page::Artist(id) if self.artist.as_ref().is_none_or(|a| &a.id != id) => Some(Req::Artist(id.clone())),
            // The library changes outside the app (likes, other devices): refresh on every
            // visit, showing the cached copy meanwhile.
            Page::Library(LibTab::History) if self.logged_in => Some(Req::History),
            Page::Library(_) if self.logged_in => Some(Req::Library),
            _ => None,
        };
        self.loading = req.is_some()
            && !matches!(self.page, Page::Library(LibTab::History) if self.history.is_some())
            && !matches!(self.page, Page::Library(t) if t != LibTab::History && self.library.is_some());
        if let Some(r) = req {
            self.backend.request(r);
        }
    }

    fn preferred_artist_ids(&self) -> Vec<String> {
        let mut ids = HashSet::new();
        if let Some(home) = &self.home {
            for shelf in &home.shelves {
                if shelf.title != "Listen again" && shelf.title != "Quick picks" { continue; }
                match &shelf.kind {
                    ShelfKind::Cards(cards) => {
                        for card in cards {
                            ids.extend(card.links.iter().filter_map(|link| link.id.clone()));
                            if let Target::Song(track) = &card.target {
                                ids.extend(track.artists.iter().filter_map(|artist| artist.id.clone()));
                            }
                        }
                    }
                    ShelfKind::Songs { tracks, .. } => {
                        for track in tracks {
                            ids.extend(track.artists.iter().filter_map(|artist| artist.id.clone()));
                        }
                    }
                }
            }
        }
        ids.into_iter().collect()
    }

    // ---------------------------------------------------------------- playback

    pub fn current(&self) -> Option<&Track> {
        self.index.and_then(|i| self.queue.get(i))
    }

    fn current_id(&self) -> Option<&str> {
        self.current().map(|t| t.id.as_str())
    }

    fn playing(&self) -> bool {
        self.audio.status.playing.load(Ordering::Relaxed)
    }

    /// Fetch the current song's art for the background and accent. The default theme
    /// shows neither, so it skips the download and blur until the theme is turned off.
    fn request_art(&mut self) {
        if self.default_theme {
            return;
        }
        let Some(track) = self.current() else { return };
        if self.art_key.as_deref() == Some(track.id.as_str()) {
            return;
        }
        if let Some(url) = backend::art_url(track) {
            let key = track.id.clone();
            self.backend.request(Req::Art { key, url });
        }
    }

    fn accent_color(&self) -> Color32 {
        if self.default_theme { theme::RED } else { self.accent }
    }

    fn set_discord_enabled(&mut self, enabled: bool) {
        self.discord_enabled = enabled;
        if enabled {
            if self.discord.is_none() {
                self.discord = crate::discord::Discord::spawn();
            }
        } else {
            if let Some(discord) = self.discord.take() {
                discord.set(None);
            }
            self.discord_sent = None;
        }
    }

    /// Mirror playback into Discord. Cheap enough to run every frame: it only sends on a
    /// new song, play/pause, a seek, or a newly learned duration.
    fn sync_discord(&mut self) {
        let Some(d) = &self.discord else { return };
        let want = self.current().filter(|_| self.playing() && !self.buffering).map(|t| {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64;
            let start_ms = now - self.audio.status.position().as_millis() as i64;
            let dur = self.audio.status.duration_ms.load(Ordering::Relaxed) as i64;
            crate::discord::Presence {
                video_id: t.id.clone(),
                title: t.title.clone(),
                artist: t.artist_line(),
                album: t.album.as_ref().map(|a| a.name.clone()),
                art: backend::art_url(t),
                start_ms,
                end_ms: (dur > 0).then_some(start_ms + dur),
            }
        });
        // Position ticks every 100 ms; only a jump of 2 s or more is a real seek.
        let same = match (&want, &self.discord_sent) {
            (Some(a), Some(b)) => {
                (a.start_ms - b.start_ms).abs() < 2000
                    && a.end_ms.is_some() == b.end_ms.is_some()
                    && crate::discord::Presence { start_ms: b.start_ms, end_ms: b.end_ms, ..a.clone() } == *b
            }
            (None, None) => true,
            _ => false,
        };
        if !same {
            d.set(want.clone());
            self.discord_sent = want;
        }
    }

    fn play_list(&mut self, tracks: Vec<Track>, start: usize) {
        if tracks.is_empty() {
            return;
        }
        self.queue = tracks;
        self.index = Some(start.min(self.queue.len() - 1));
        self.play_current();
    }

    /// Put last session's queue back, paused at the saved position, without loading audio.
    fn restore_session(&mut self, json: &str) {
        let Ok(s) = serde_json::from_str::<Session>(json) else { return };
        let Some(track) = s.queue.get(s.index).cloned() else { return };
        self.queue = s.queue;
        self.index = Some(s.index);
        let dur = track.duration.map_or(s.duration_ms, |d| d as u64 * 1000);
        self.audio.status.duration_ms.store(dur, Ordering::Relaxed);
        self.audio.status.position_ms.store(s.position_ms, Ordering::Relaxed);
        self.resume_at = Some(Duration::from_millis(s.position_ms));
        self.request_art();
        self.backend.request(Req::Lyrics(track));
    }

    fn session_json(&self) -> Option<String> {
        let i = self.index?;
        // Keep the saved queue small: liked-songs queues can run to 1000+ tracks.
        let from = i.saturating_sub(50);
        let to = (i + 150).min(self.queue.len());
        serde_json::to_string(&Session {
            queue: self.queue[from..to].to_vec(),
            index: i - from,
            position_ms: self.audio.status.position().as_millis() as u64,
            duration_ms: self.audio.status.duration_ms.load(Ordering::Relaxed),
        })
        .ok()
    }

    fn play_current(&mut self) {
        self.play_current_at(Duration::ZERO);
    }

    /// Start the current track, opened directly at `at` so nothing plays from 0:00 first.
    fn play_current_at(&mut self, at: Duration) {
        let Some(track) = self.current().cloned() else { return };
        self.resume_at = None;
        self.generation += 1;
        self.audio.status.wanted_gen.store(self.generation, Ordering::Relaxed);
        self.audio.status.duration_ms.store(track.duration.map_or(0, |d| d as u64 * 1000), Ordering::Relaxed);
        self.audio.send(AudioCmd::Stop);
        self.buffering = true;
        self.seek_drag = None;
        self.backend.request(Req::Play { video_id: track.id.clone(), generation: self.generation, at });
        if let Some(m) = &mut self.media {
            m.set_track(
                &track.title,
                &track.artist_line(),
                track.album.as_ref().map(|a| a.name.as_str()),
                backend::art_url(&track).as_deref(),
                track.duration,
            );
            m.set_playing(true);
        }
        // Background art and lyrics follow the track.
        self.request_art();
        self.lyrics = None;
        self.backend.request(Req::Lyrics(track.clone()));
        // Keep the queue topped up so autoplay never stalls at the end.
        if self.autoplay && !self.radio_pending && self.index.is_some_and(|i| i + 1 >= self.queue.len()) {
            self.radio_pending = true;
            self.backend.request(Req::Radio(track.id));
        }
    }

    /// Fetch whatever plays after the current track so skipping is instant.
    fn prefetch_next(&self) {
        let Some(i) = self.index else { return };
        let next = match self.repeat {
            Repeat::One => None,
            Repeat::All => self.queue.get(i + 1).or(self.queue.first()),
            Repeat::Off => self.queue.get(i + 1),
        };
        if let Some(t) = next {
            self.backend.request(Req::Prefetch(t.id.clone()));
        }
    }

    fn next(&mut self) {
        let Some(i) = self.index else { return };
        if i + 1 < self.queue.len() {
            self.index = Some(i + 1);
        } else if self.repeat == Repeat::All && !self.queue.is_empty() {
            self.index = Some(0);
        } else {
            return;
        }
        self.play_current();
    }

    fn prev(&mut self) {
        if self.audio.status.position() > Duration::from_secs(3) {
            self.audio.send(AudioCmd::Seek(Duration::ZERO));
            return;
        }
        if let Some(i) = self.index.filter(|&i| i > 0) {
            self.index = Some(i - 1);
            self.play_current();
        }
    }

    fn toggle(&mut self) {
        if self.index.is_none() {
            return;
        }
        if let Some(at) = self.resume_at {
            self.play_current_at(at);
            return;
        }
        let playing = self.playing();
        self.audio.send(if playing { AudioCmd::Pause } else { AudioCmd::Resume });
        if let Some(m) = &mut self.media {
            m.set_playing(!playing);
        }
    }

    fn shuffle_upcoming(&mut self) {
        let start = self.index.map_or(0, |i| i + 1);
        if start < self.queue.len() {
            shuffle(&mut self.queue[start..]);
            self.toast("Shuffled up next");
            self.prefetch_next();
        }
    }

    fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), -1.0));
    }

    fn apply(&mut self, a: Action) {
        match a {
            // Clicking the song that's already playing pauses/resumes it, not restart it.
            Action::Play(tracks, i) if tracks.get(i).is_some_and(|t| self.current_id() == Some(t.id.as_str())) && self.resume_at.is_none() => {
                self.toggle()
            }
            Action::Play(tracks, i) => {
                // Play buttons and rows live on the page they play from.
                self.queue_source = match &self.page {
                    Page::Album(id) => Some(Target::Album(id.clone())),
                    Page::Playlist(id) => Some(Target::Playlist(id.clone())),
                    Page::Artist(id) => Some(Target::Artist(id.clone())),
                    _ => None,
                };
                self.play_list(tracks, i);
            }
            Action::TogglePlay => self.toggle(),
            Action::PlayCollection(target, shuffle) => {
                self.toast("Starting playback…");
                self.pending_source = Some(target.clone());
                self.backend.request(Req::PlayCollection { target, shuffle });
            }
            Action::Open(Target::Song(t)) | Action::Radio(t) => {
                self.queue_source = None;
                self.play_list(vec![t.clone()], 0);
                if !self.radio_pending {
                    self.radio_pending = true;
                    self.backend.request(Req::Radio(t.id));
                }
            }
            Action::Open(Target::Album(id)) => self.go(Page::Album(id)),
            Action::Open(Target::Playlist(id)) => self.go(Page::Playlist(id)),
            Action::Open(Target::Artist(id)) => self.go(Page::Artist(id)),
            Action::PlayNext(t) => {
                let at = self.index.map_or(0, |i| i + 1).min(self.queue.len());
                self.queue.insert(at, t);
                self.toast("Playing next");
                if self.index.is_none() {
                    self.index = Some(0);
                    self.play_current();
                } else {
                    self.prefetch_next();
                }
            }
            Action::Enqueue(t) => {
                self.queue.push(t);
                self.toast("Added to queue");
                if self.index.is_none() {
                    self.index = Some(self.queue.len() - 1);
                    self.play_current();
                }
            }
            Action::Go(p) => self.go(p),
            Action::QueueJump(i) => {
                self.index = Some(i);
                self.play_current();
            }
            Action::QueueRemove(i) => {
                if i < self.queue.len() && Some(i) != self.index {
                    self.queue.remove(i);
                    if let Some(cur) = self.index.filter(|&c| c > i) {
                        self.index = Some(cur - 1);
                    }
                }
            }
            Action::Seek(secs) => {
                let at = Duration::from_secs_f32(secs.max(0.0));
                if self.resume_at.is_some() {
                    // Restored but not loaded yet: move the start point instead.
                    self.resume_at = Some(at);
                    self.audio.status.position_ms.store(at.as_millis() as u64, Ordering::Relaxed);
                } else {
                    self.audio.send(AudioCmd::Seek(at));
                }
                self.lyrics_user_scroll = -10.0;
            }
            Action::Like(id, like) => {
                if !self.logged_in {
                    self.toast("Sign in to like songs");
                    return;
                }
                // Optimistic: update the like state and the cached Liked songs list now.
                if like {
                    self.liked.insert(id.clone());
                    let track = self.queue.iter().find(|t| t.id == id).cloned();
                    if let (Some(lib), Some(t)) = (&mut self.library, track) {
                        if !lib.liked.iter().any(|x| x.id == id) {
                            lib.liked.insert(0, t);
                        }
                    }
                } else {
                    self.liked.remove(&id);
                    if let Some(lib) = &mut self.library {
                        lib.liked.retain(|t| t.id != id);
                    }
                }
                self.backend.request(Req::Like { video_id: id, like });
            }
            Action::Dislike(id) => {
                if !self.logged_in { self.toast("Sign in to rate songs"); return; }
                self.liked.remove(&id);
                if let Some(lib) = &mut self.library { lib.liked.retain(|t| t.id != id); }
                self.backend.request(Req::Dislike(id));
            }
            Action::Share(target) => {
                self.share_pending = Some(share_url(&target));
                self.toast("Link copied");
            }
            Action::AddToPlaylist(track) => {
                if self.logged_in { self.dialog = Some(Dialog::PickPlaylist(track)); }
                else { self.toast("Sign in to edit playlists"); }
            }
            Action::NewPlaylist(track) => {
                if self.logged_in { self.dialog = Some(Dialog::NewPlaylist { title: String::new(), track }); }
                else { self.toast("Sign in to create playlists"); }
            }
            Action::EditPlaylist => {
                if let Some(c) = self.collection.as_ref().filter(|c| !c.is_album) {
                    self.dialog = Some(Dialog::EditPlaylist {
                        id: c.id.clone(), title: c.title.clone(), description: c.description.clone().unwrap_or_default(), privacy: None,
                    });
                }
            }
            Action::DeletePlaylist => {
                if let Some(c) = self.collection.as_ref().filter(|c| !c.is_album) {
                    self.dialog = Some(Dialog::ConfirmDelete { id: c.id.clone(), title: c.title.clone() });
                }
            }
            Action::SaveCollection(id, save) => {
                if self.logged_in { self.backend.request(Req::SaveCollection { id, save }); }
                else { self.toast("Sign in to save to library"); }
            }
            Action::HomeChip(params) => {
                self.home_loading = true;
                if let Some(h) = &mut self.home {
                    h.shelves.clear();
                    for c in &mut h.chips {
                        c.selected = false;
                    }
                }
                self.backend.request(Req::Home(params));
            }
            Action::OpenNowPlaying(open) => {
                if self.current().is_some() {
                    self.np_open = open;
                }
            }
        }
    }

    // ---------------------------------------------------------------- events

    fn pump(&mut self, now: f64) {
        while let Ok(r) = self.ch.resp_rx.try_recv() {
            self.on_resp(r, now);
        }
        while let Ok(ev) = self.ch.audio_rx.try_recv() {
            match ev {
                AudioEvent::Finished { generation } if generation == self.generation => {
                    if self.repeat == Repeat::One {
                        self.play_current();
                    } else {
                        self.next();
                    }
                }
                AudioEvent::Finished { .. } => {}
                AudioEvent::Error(e) => self.toast(e),
            }
        }
        while let Ok(ev) = self.ch.media_rx.try_recv() {
            match ev {
                MediaControlEvent::Play if self.resume_at.is_some() => self.toggle(),
                MediaControlEvent::Play => self.audio.send(AudioCmd::Resume),
                MediaControlEvent::Pause | MediaControlEvent::Stop => self.audio.send(AudioCmd::Pause),
                MediaControlEvent::Toggle => self.toggle(),
                MediaControlEvent::Next => self.next(),
                MediaControlEvent::Previous => self.prev(),
                _ => {}
            }
        }
        if self.buffering && self.playing() {
            self.buffering = false;
            self.prefetch_next();
            // Dev hook: YTM_SEEK=<secs> jumps into the first track once it starts.
            if self.generation == 1 {
                if let Some(s) = std::env::var("YTM_SEEK").ok().and_then(|v| v.parse::<f32>().ok()) {
                    self.audio.send(AudioCmd::Seek(Duration::from_secs_f32(s)));
                }
            }
        }
        if self.suggest_due.is_some_and(|d| now >= d) {
            self.suggest_due = None;
            let q = self.search_text.trim().to_owned();
            if !q.is_empty() {
                self.backend.request(Req::Suggest(q));
            }
        }
    }

    fn on_resp(&mut self, r: Resp, now: f64) {
        match r {
            Resp::Home(page) => {
                if self.account_name.is_none() {
                    self.account_name = page.shelves.iter().find_map(|s| s.strapline.clone());
                }
                if let Some(avatar) = page.shelves.iter().find(|s| s.title == "Listen again").and_then(|s| s.strap_thumb.clone()) {
                    self.account_avatar = Some(avatar);
                }
                let mut chips = page.chips;
                // The feed marks the active mood itself; keep the bar when it doesn't send chips.
                if chips.is_empty() {
                    chips = self.home.as_ref().map(|h| h.chips.clone()).unwrap_or_default();
                }
                self.home = Some(HomeState {
                    chips,
                    shelves: page.shelves,
                    continuation: page.continuation,
                    loading_more: false,
                    picks_loading: false,
                    picks_expanded: false,
                });
                self.home_loading = false;
                if self.page == Page::Home {
                    self.loading = false;
                }
            }
            Resp::HomeMore(page) => {
                if let Some(h) = &mut self.home {
                    h.shelves.extend(page.shelves);
                    h.continuation = page.continuation;
                    h.loading_more = false;
                }
            }
            Resp::Explore(e) => {
                self.explore = Some(e);
                self.loading = false;
            }
            Resp::Search(r) => {
                if self.smoke.as_ref() == Some(&r.query) {
                    self.smoke = None;
                    if !r.tracks.is_empty() {
                        log::info!("smoke: playing {}", r.tracks[0].title);
                        self.play_list(r.tracks.clone(), 0);
                    }
                }
                self.search = Some(r);
                self.loading = false;
            }
            Resp::Suggest { query, terms } => {
                if self.search_text.trim() == query {
                    self.suggestions = (query, terms);
                }
            }
            Resp::Collection(c) => {
                self.collection = Some(c);
                self.loading = false;
            }
            Resp::Artist(a) => {
                self.artist = Some(a);
                self.loading = false;
            }
            Resp::Library(l) => {
                self.liked = l.liked.iter().map(|t| t.id.clone()).collect();
                self.library = Some(l);
                self.loading = false;
            }
            Resp::History(h) => {
                self.history = Some(h);
                self.loading = false;
            }
            Resp::Lyrics { video_id, lyrics } => {
                if self.current_id() == Some(video_id.as_str()) {
                    self.lyrics = Some((video_id, lyrics));
                }
            }
            Resp::Art { key, art } => {
                if self.current_id() == Some(key.as_str()) {
                    self.accent = art.accent;
                    if let Some(bg) = &self.bg {
                        bg.set_art(art);
                    }
                    self.art_key = Some(key);
                    self.art_changed = now;
                }
            }
            Resp::Radio { seed, tracks } => {
                self.radio_pending = false;
                let have: HashSet<String> = self.queue.iter().map(|t| t.id.clone()).collect();
                let was_at_end = self.index.is_some_and(|i| i + 1 >= self.queue.len());
                self.queue.extend(tracks.into_iter().filter(|t| t.id != seed && !have.contains(&t.id)));
                if was_at_end && self.index.is_some() && !self.buffering && !self.playing() {
                    // The track ended while radio was loading: continue now.
                    self.next();
                } else if !self.buffering {
                    self.prefetch_next();
                }
            }
            Resp::PlayQueue { tracks } => {
                self.queue_source = self.pending_source.take();
                self.play_list(tracks, 0);
            }
            Resp::Liked { video_id, like } => {
                if like {
                    self.liked.insert(video_id);
                }
            }
            Resp::QuickPicksMore { seed, tracks } => {
                if let Some(h) = &mut self.home {
                    h.picks_loading = false;
                    if let Some(shelf) = h.shelves.iter_mut().find(|s| s.title == "Quick picks") {
                        if let ShelfKind::Songs { tracks: picks, .. } = &mut shelf.kind {
                            if picks.iter().any(|t| t.id == seed) {
                                h.picks_expanded = true;
                                let mut seen: HashSet<String> = picks.iter().map(|t| t.id.clone()).collect();
                                picks.extend(tracks.into_iter().filter(|t| seen.insert(t.id.clone())).take(24));
                            }
                        }
                    }
                }
            }
            Resp::PlaylistCreated { id, had_song } => {
                self.backend.request(Req::Library);
                self.toast(if had_song { "Song added to new playlist" } else { "Playlist created" });
                self.go(Page::Playlist(id));
            }
            Resp::PlaylistChanged { id, message } => {
                self.backend.request(Req::Library);
                if let Some(id) = id {
                    if self.page == Page::Playlist(id.clone()) { self.backend.request(Req::Playlist(id)); }
                } else if matches!(self.page, Page::Playlist(_)) {
                    self.go(Page::Library(LibTab::Playlists));
                }
                self.toast(message);
            }
            Resp::PlayError { generation, msg } => {
                if generation == self.generation {
                    self.buffering = false;
                    let title = self.current().map(|t| t.title.clone()).unwrap_or_default();
                    let reason = if msg.contains("age-restricted") || msg.contains("confirm your age") {
                        "it's age-restricted".to_owned()
                    } else {
                        msg
                    };
                    let has_next = self.index.is_some_and(|i| i + 1 < self.queue.len());
                    // Don't stall the queue on one unplayable song.
                    if has_next {
                        self.toast(format!("Skipped \"{title}\": {reason}"));
                        self.next();
                    } else {
                        self.toast(format!("Can't play \"{title}\": {reason}"));
                    }
                }
            }
            Resp::LoggedIn(v) => {
                self.logged_in = v;
                self.account_name = None;
                self.account_avatar = None;
                self.library = None;
                self.history = None;
                self.home = None;
                self.home_loading = true;
                self.backend.request(Req::Home(None));
                if v {
                    self.backend.request(Req::Library);
                } else {
                    self.liked.clear();
                }
                self.toast(if v { "Signed in" } else { "Signed out" });
            }
            Resp::Error(e) => {
                self.loading = false;
                self.home_loading = false;
                self.radio_pending = false;
                if let Some(h) = &mut self.home {
                    h.loading_more = false;
                    h.picks_loading = false;
                }
                self.toast(e);
            }
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.egui_wants_keyboard_input();
        let i = ctx.input(|i| {
            let k = |key| !typing && i.key_pressed(key);
            (
                k(egui::Key::Space),
                i.pointer.button_pressed(egui::PointerButton::Extra1)
                    || (i.modifiers.alt && i.key_pressed(egui::Key::ArrowLeft)),
                i.pointer.button_pressed(egui::PointerButton::Extra2)
                    || (i.modifiers.alt && i.key_pressed(egui::Key::ArrowRight)),
                i.modifiers.ctrl && i.key_pressed(egui::Key::ArrowRight),
                i.modifiers.ctrl && i.key_pressed(egui::Key::ArrowLeft),
                k(egui::Key::L),
                k(egui::Key::F),
                i.key_pressed(egui::Key::Escape),
                i.modifiers.ctrl && i.key_pressed(egui::Key::ArrowUp),
                i.modifiers.ctrl && i.key_pressed(egui::Key::ArrowDown),
            )
        });
        let (space, back, fwd, next, prev, lyr, full, esc, vol_up, vol_down) = i;
        if space {
            self.toggle();
        }
        if back {
            self.nav_back();
        }
        if fwd {
            self.nav_forward();
        }
        if next {
            self.next();
        }
        if prev {
            self.prev();
        }
        if lyr && self.current().is_some() {
            self.np_open = !self.np_open;
            self.np_tab = NpTab::Lyrics;
        }
        if full && self.np_open {
            self.set_fullscreen(ctx, !self.fullscreen);
        }
        if esc {
            if self.fullscreen {
                self.set_fullscreen(ctx, false);
            } else {
                self.np_open = false;
            }
        }
        if vol_up || vol_down {
            let v = (self.audio.volume() + if vol_up { 0.05 } else { -0.05 }).clamp(0.0, 1.0);
            self.audio.set_volume(v);
        }
    }

    fn set_fullscreen(&mut self, ctx: &egui::Context, on: bool) {
        self.fullscreen = on;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
    }

    fn start_sign_in(&self, ctx: &egui::Context) {
        if self.signing_in.swap(true, Ordering::Relaxed) {
            return;
        }
        let (backend, busy, ctx) = (self.backend.clone(), self.signing_in.clone(), ctx.clone());
        crate::login::spawn(self.backend.login_profile(), move |outcome| {
            match outcome {
                crate::login::Outcome::Cookie(c) => backend.request(Req::SetCookie(c)),
                crate::login::Outcome::Cancelled => {}
                crate::login::Outcome::Failed(e) => backend.report_error(format!("Sign-in window failed: {e}")),
            }
            busy.store(false, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }
}

fn share_url(target: &Target) -> String {
    match target {
        Target::Song(t) => format!("https://music.youtube.com/watch?v={}", t.id),
        Target::Playlist(id) => format!("https://music.youtube.com/playlist?list={id}"),
        Target::Album(id) => format!("https://music.youtube.com/browse/{id}"),
        Target::Artist(id) => format!("https://music.youtube.com/channel/{id}"),
    }
}

impl App {
    fn draw_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.dialog.take() else { return };
        let mut open = true;
        let mut done = false;
        let heading = match &dialog {
            Dialog::PickPlaylist(_) => "Add to playlist",
            Dialog::NewPlaylist { .. } => "New playlist",
            Dialog::EditPlaylist { .. } => "Edit playlist",
            Dialog::ConfirmDelete { .. } => "Delete playlist",
        };
        egui::Window::new(heading).collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
            ui.set_min_width(340.0);
            match &mut dialog {
                Dialog::PickPlaylist(track) => {
                    if ui.button("+ New playlist").clicked() {
                        self.actions.push(Action::NewPlaylist(Some(track.clone())));
                        done = true;
                    }
                    ui.separator();
                    let cards = self.library.as_ref().map(|l| l.playlists.clone()).unwrap_or_default();
                    egui::ScrollArea::vertical().max_height(330.0).show(ui, |ui| {
                        for card in cards {
                            if let Target::Playlist(id) = card.target {
                                if !id.starts_with("PL") { continue; }
                                if ui.button(&card.title).clicked() {
                                    self.backend.request(Req::AddToPlaylist { playlist_id: id, video_id: track.id.clone() });
                                    done = true;
                                }
                            }
                        }
                    });
                }
                Dialog::NewPlaylist { title, track } => {
                    ui.label("Name");
                    ui.text_edit_singleline(title);
                    if ui.add_enabled(!title.trim().is_empty(), egui::Button::new("Create")).clicked() {
                        self.backend.request(Req::CreatePlaylist { title: title.trim().to_owned(), video_id: track.as_ref().map(|t| t.id.clone()) });
                        done = true;
                    }
                }
                Dialog::EditPlaylist { id, title, description, privacy } => {
                    ui.label("Name");
                    ui.text_edit_singleline(title);
                    ui.label("Description");
                    ui.text_edit_multiline(description);
                    egui::ComboBox::from_label("Visibility").selected_text(privacy.as_deref().unwrap_or("Keep current"))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(privacy, None, "Keep current");
                            for value in ["PRIVATE", "UNLISTED", "PUBLIC"] {
                                ui.selectable_value(privacy, Some(value.to_owned()), value);
                            }
                        });
                    if ui.add_enabled(!title.trim().is_empty(), egui::Button::new("Save")).clicked() {
                        self.backend.request(Req::EditPlaylist { id: id.clone(), title: title.trim().to_owned(), description: description.clone(), privacy: privacy.clone() });
                        done = true;
                    }
                }
                Dialog::ConfirmDelete { id, title } => {
                    ui.label(format!("Delete \"{title}\"?"));
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() { done = true; }
                        if ui.button("Delete playlist").clicked() {
                            self.backend.request(Req::DeletePlaylist(id.clone()));
                            done = true;
                        }
                    });
                }
            }
        });
        if open && !done { self.dialog = Some(dialog); }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = ctx.input(|i| i.time);
        self.pump(now);
        for a in std::mem::take(&mut self.actions) {
            self.apply(a);
        }
        if self.page_changed < 0.0 {
            self.page_changed = now;
        }
        fit_window_to_monitor(ctx);
        self.sync_discord();
        let np = widgets::NowPlaying {
            song: self.current_id().map(str::to_owned),
            source: self.queue_source.clone(),
            playing: self.playing() || self.buffering,
        };
        ctx.data_mut(|d| d.insert_temp(widgets::NowPlaying::id(), np));
        if let Some((_, t)) = &mut self.toast {
            if *t < 0.0 {
                *t = now;
            } else if now - *t > 4.5 {
                self.toast = None;
            }
        }
        // Frame pacing: idle costs nothing. The background drifts slowly, so ~15 fps
        // reads as smooth on pages; the now-playing view runs at 30 fps for the lyric
        // sweep. Each egui frame re-lays-out the UI, so frame rate is the main CPU cost.
        let playing = self.playing();
        if self.np_open && playing {
            ctx.request_repaint_after(Duration::from_millis(33));
        } else if playing && self.anim_bg && !self.default_theme {
            ctx.request_repaint_after(Duration::from_millis(66));
        } else if playing || self.buffering {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.shortcuts(&ctx);
        let now = ctx.input(|i| i.time);
        let screen = ctx.content_rect();

        shell::background(self, ui, screen, now);
        if !self.fullscreen {
            egui::Panel::bottom("player")
                .exact_size(86.0)
                .frame(egui::Frame::new().fill(theme::shade(0.55)))
                .show_separator_line(false)
                .show(ui, |ui| shell::player_bar(self, ui, now));
            egui::Panel::left("nav")
                .exact_size(236.0)
                .frame(egui::Frame::new().fill(theme::shade(0.28)).inner_margin(egui::Margin::symmetric(14, 16)))
                .show_separator_line(false)
                .show(ui, |ui| shell::sidebar(self, ui));
            let q = theme::anim_bool(&ctx, "queue-drawer", self.queue_open && !self.np_open, 0.25);
            if q > 0.01 {
                egui::Panel::right("queue")
                    .exact_size(340.0 * q)
                    .frame(egui::Frame::new().fill(theme::shade(0.42)).inner_margin(egui::Margin::symmetric(14, 16)))
                    .show_separator_line(false)
                    .show(ui, |ui| shell::queue_drawer(self, ui));
            }
            egui::CentralPanel::default()
                .frame(egui::Frame::new().inner_margin(egui::Margin { left: 28, right: 0, top: 14, bottom: 0 }))
                .show(ui, |ui| {
                    shell::top_bar(self, ui);
                    ui.add_space(6.0);
                    pages::page(self, ui, now);
                });
        }
        nowplaying::overlay(self, &ctx, now);
        self.draw_dialog(&ctx);
        if let Some(url) = self.share_pending.take() { ctx.copy_text(url); }
        shell::suggestions(self, &ctx);
        shell::toast(self, &ctx, now);

        dev_screenshot(&ctx, now);

        // Thumbnails are cheap to refetch; cap the decoded cache instead of growing forever.
        let bytes: usize = ctx.loaders().bytes.lock().iter().map(|l| l.byte_size()).sum();
        if bytes > 64 * 1024 * 1024 {
            ctx.forget_all_images();
        }
    }

    /// Dev hook: `YTM_FAKE_POINTER=x,y` parks a synthetic pointer there, for testing
    /// hover behaviour without touching the real mouse.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        // Dev hook: YTM_FAKE_WHEEL="secs,notches" injects one wheel event, for testing.
        if let Some((at, n)) = std::env::var("YTM_FAKE_WHEEL").ok().and_then(|v| {
            let (a, n) = v.split_once(',')?;
            Some((a.parse::<f64>().ok()?, n.parse::<f32>().ok()?))
        }) {
            let fired = egui::Id::new("dev-wheel-fired");
            let t = raw.time.unwrap_or(0.0);
            if t >= at && !ctx.data(|d| d.get_temp::<bool>(fired).unwrap_or(false)) {
                ctx.data_mut(|d| d.insert_temp(fired, true));
                raw.events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: egui::vec2(0.0, -n),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::NONE,
                });
            } else if t < at {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
        smooth_wheel(self, ctx, raw);
        if let Some((x, y)) = std::env::var("YTM_FAKE_POINTER").ok().and_then(|v| {
            let (x, y) = v.split_once(',')?;
            Some((x.trim().parse::<f32>().ok()?, y.trim().parse::<f32>().ok()?))
        }) {
            raw.events.retain(|e| !matches!(e, egui::Event::PointerMoved(_) | egui::Event::PointerGone));
            raw.events.push(egui::Event::PointerMoved(egui::pos2(x, y)));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if std::env::var_os("YTM_MUTE").is_none() {
            storage.set_string("volume", self.audio.volume().to_string());
        }
        let repeat = match self.repeat {
            Repeat::Off => "off",
            Repeat::All => "all",
            Repeat::One => "one",
        };
        storage.set_string("repeat", repeat.into());
        let b = |v: bool| if v { "1" } else { "0" }.to_owned();
        storage.set_string("autoplay", b(self.autoplay));
        storage.set_string("discord_enabled", b(self.discord_enabled));
        storage.set_string("default_theme", b(self.default_theme));
        storage.set_string("anim_bg", b(self.anim_bg));
        storage.set_string("reactive_bg", b(self.reactive_bg));
        // Background test runs leave the user's real "last song" alone unless asked.
        let test_run = std::env::var_os("YTM_BACKGROUND").is_some() && std::env::var_os("YTM_SAVE_SESSION").is_none();
        if let Some(s) = self.session_json().filter(|_| !test_run) {
            storage.set_string(SESSION_KEY, s);
        }
    }

    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        if let (Some(bg), Some(gl)) = (&self.bg, gl) {
            bg.destroy(gl);
        }
    }
}

const SESSION_KEY: &str = "session";

/// What's saved between launches so the player picks up where it left off.
#[derive(serde::Serialize, serde::Deserialize)]
struct Session {
    queue: Vec<Track>,
    index: usize,
    position_ms: u64,
    duration_ms: u64,
}

/// Dev hook: `YTM_SCREENSHOT=out.png` saves the app's own framebuffer after
/// `YTM_SCREENSHOT_AT` seconds (default 8), independent of OS window capture.
fn dev_screenshot(ctx: &egui::Context, now: f64) {
    let Some(path) = std::env::var_os("YTM_SCREENSHOT") else { return };
    let at: f64 = std::env::var("YTM_SCREENSHOT_AT").ok().and_then(|v| v.parse().ok()).unwrap_or(8.0);
    let requested = egui::Id::new("dev-shot-requested");
    if now >= at && !ctx.data(|d| d.get_temp::<bool>(requested).unwrap_or(false)) {
        ctx.data_mut(|d| d.insert_temp(requested, true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
    } else if now < at {
        ctx.request_repaint_after(Duration::from_secs_f64(at - now));
    }
    let shot = ctx.input(|i| {
        i.events.iter().find_map(|e| match e {
            egui::Event::Screenshot { image, .. } => Some(image.clone()),
            _ => None,
        })
    });
    if let Some(img) = shot {
        let [w, h] = img.size;
        let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
        match image::RgbaImage::from_raw(w as u32, h as u32, bytes).map(|i| i.save(&path)) {
            Some(Ok(())) => log::info!("screenshot saved"),
            other => log::warn!("screenshot failed: {other:?}"),
        }
    }
}

/// A restored window size can exceed the monitor (e.g. saved after maximizing),
/// leaving the player bar partly off-screen. Once at startup, maximize instead.
fn fit_window_to_monitor(ctx: &egui::Context) {
    let done = egui::Id::new("fit-window-checked");
    if ctx.data(|d| d.get_temp::<bool>(done).unwrap_or(false)) {
        return;
    }
    if std::env::var_os("YTM_BACKGROUND").is_some() {
        // eframe re-applies the saved (often maximized) window state regardless of the
        // builder, so push the test window off-screen here, before it's ever shown.
        ctx.data_mut(|d| d.insert_temp(done, true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(-4000.0, 0.0)));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(1920.0, 1057.0)));
        return;
    }
    let (outer, monitor, maximized) = ctx.input(|i| (i.viewport().outer_rect, i.viewport().monitor_size, i.viewport().maximized));
    let (Some(outer), Some(monitor)) = (outer, monitor) else { return };
    ctx.data_mut(|d| d.insert_temp(done, true));
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, monitor);
    // A little slack for the invisible resize borders Windows adds around the frame.
    if maximized != Some(true) && !screen.expand(12.0).contains_rect(outer) {
        log::info!("window {outer:?} exceeds monitor {monitor:?}; maximizing");
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
    }
}

/// Smooth scrolling: mouse-wheel notches (line units) are collected and replayed as
/// small pixel steps with an exponential ease-out (~0.3 s glide), like browsers do.
/// Touchpads already report smooth pixel deltas and pass through untouched, as does
/// Ctrl+wheel zoom.
fn smooth_wheel(app: &mut App, ctx: &egui::Context, raw: &mut egui::RawInput) {
    const NOTCH_PX: f32 = 70.0;
    const GLIDE: f32 = 0.06; // time constant, seconds (settles in ~0.18 s)
    let viewport_h = raw.screen_rect.map_or(800.0, |r| r.height());
    raw.events.retain(|e| match e {
        egui::Event::MouseWheel { unit, delta, modifiers, .. } if !modifiers.command && !modifiers.ctrl => {
            let px = match unit {
                egui::MouseWheelUnit::Line => *delta * NOTCH_PX,
                egui::MouseWheelUnit::Page => *delta * viewport_h * 0.8,
                egui::MouseWheelUnit::Point => return true,
            };
            // Reversing direction cancels the remaining glide instead of fighting it.
            if app.wheel_pending.y * px.y < 0.0 || app.wheel_pending.x * px.x < 0.0 {
                app.wheel_pending = egui::Vec2::ZERO;
            }
            app.wheel_pending += px;
            app.wheel_mods = *modifiers;
            false
        }
        _ => true,
    });
    if app.wheel_pending.length() < 0.5 {
        app.wheel_pending = egui::Vec2::ZERO;
        return;
    }
    let dt = raw.predicted_dt.clamp(0.001, 0.05);
    let step = app.wheel_pending * (1.0 - (-dt / GLIDE).exp());
    app.wheel_pending -= step;
    raw.events.push(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: step,
        phase: egui::TouchPhase::Move,
        modifiers: app.wheel_mods,
    });
    ctx.request_repaint();
}

/// Fisher-Yates with a clock-seeded xorshift; shuffling doesn't need a real RNG crate.
pub fn shuffle<T>(v: &mut [T]) {
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0x9e37_79b9_7f4a_7c15, |d| d.as_nanos() as u64)
        | 1;
    for i in (1..v.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.swap(i, (x % (i as u64 + 1)) as usize);
    }
}

pub fn fmt_time(secs: u32) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

#[cfg(test)]
mod share_tests {
    use super::*;

    #[test]
    fn shares_youtube_music_urls() {
        let song = Track { id: "abc123".into(), title: String::new(), artists: vec![], album: None, duration: None, thumbs: vec![], track_nr: None, plays: None };
        assert_eq!(share_url(&Target::Song(song)), "https://music.youtube.com/watch?v=abc123");
        assert_eq!(share_url(&Target::Playlist("PL123".into())), "https://music.youtube.com/playlist?list=PL123");
        assert_eq!(share_url(&Target::Album("MPRE123".into())), "https://music.youtube.com/browse/MPRE123");
        assert_eq!(share_url(&Target::Artist("UC123".into())), "https://music.youtube.com/channel/UC123");
    }
}
