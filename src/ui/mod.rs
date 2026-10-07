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
use crate::model::{Chip, Shelf, Target, Track};
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
    HomeChip(Option<String>),
    OpenNowPlaying(bool),
}

pub struct HomeState {
    pub chips: Vec<Chip>,
    pub shelves: Vec<Shelf>,
    pub continuation: Option<String>,
    pub loading_more: bool,
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
        let logged_in = backend.is_logged_in();
        backend.request(Req::Home(None));
        if logged_in {
            backend.request(Req::Library);
        }
        let mut app = Self {
            backend,
            audio,
            ch,
            media,
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
            smoke: None,
        };
        // Dev hooks: YTM_SMOKE="query" searches and plays the first song; YTM_SEARCH
        // only searches; YTM_PAGE opens a page; YTM_NOWPLAYING opens the overlay.
        if let Some(q) = std::env::var("YTM_SMOKE").ok().filter(|q| !q.is_empty()) {
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
            Page::Explore if self.explore.is_none() => Some(Req::Explore),
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

    fn play_list(&mut self, tracks: Vec<Track>, start: usize) {
        if tracks.is_empty() {
            return;
        }
        self.queue = tracks;
        self.index = Some(start.min(self.queue.len() - 1));
        self.play_current();
    }

    fn play_current(&mut self) {
        let Some(track) = self.current().cloned() else { return };
        self.generation += 1;
        self.audio.status.wanted_gen.store(self.generation, Ordering::Relaxed);
        self.audio.status.duration_ms.store(track.duration.map_or(0, |d| d as u64 * 1000), Ordering::Relaxed);
        self.audio.send(AudioCmd::Stop);
        self.buffering = true;
        self.seek_drag = None;
        self.backend.request(Req::Play { video_id: track.id.clone(), generation: self.generation });
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
        if let Some(url) = backend::art_url(&track) {
            if self.art_key.as_deref() != Some(track.id.as_str()) {
                self.backend.request(Req::Art { key: track.id.clone(), url });
            }
        }
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
            Action::Play(tracks, i) => self.play_list(tracks, i),
            Action::PlayCollection(target, shuffle) => {
                self.toast("Starting playback…");
                self.backend.request(Req::PlayCollection { target, shuffle });
            }
            Action::Open(Target::Song(t)) | Action::Radio(t) => {
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
                self.audio.send(AudioCmd::Seek(Duration::from_secs_f32(secs.max(0.0))));
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
            Resp::PlayQueue { tracks } => self.play_list(tracks, 0),
            Resp::Liked { video_id, like } => {
                if like {
                    self.liked.insert(video_id);
                }
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
                self.library = None;
                self.history = None;
                self.home = None;
                self.home_loading = true;
                self.backend.request(Req::Home(None));
                if v {
                    self.backend.request(Req::Library);
                } else {
                    self.liked.clear();
                    self.account_name = None;
                }
                self.toast(if v { "Signed in" } else { "Signed out" });
            }
            Resp::Error(e) => {
                self.loading = false;
                self.home_loading = false;
                self.radio_pending = false;
                if let Some(h) = &mut self.home {
                    h.loading_more = false;
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
        } else if playing && self.anim_bg {
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
                .frame(egui::Frame::new().inner_margin(egui::Margin { left: 28, right: 20, top: 14, bottom: 0 }))
                .show(ui, |ui| {
                    shell::top_bar(self, ui);
                    ui.add_space(6.0);
                    pages::page(self, ui, now);
                });
        }
        nowplaying::overlay(self, &ctx, now);
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
        storage.set_string("anim_bg", b(self.anim_bg));
        storage.set_string("reactive_bg", b(self.reactive_bg));
    }

    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        if let (Some(bg), Some(gl)) = (&self.bg, gl) {
            bg.destroy(gl);
        }
    }
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

/// Smooth scrolling: mouse-wheel notches (line units) are collected and replayed as
/// small pixel steps with an exponential ease-out (~0.3 s glide), like browsers do.
/// Touchpads already report smooth pixel deltas and pass through untouched, as does
/// Ctrl+wheel zoom.
fn smooth_wheel(app: &mut App, ctx: &egui::Context, raw: &mut egui::RawInput) {
    const NOTCH_PX: f32 = 64.0;
    const GLIDE: f32 = 0.085; // time constant, seconds
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
