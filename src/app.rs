use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use egui::{Align, Color32, CornerRadius, Layout, RichText, Sense, Ui, Vec2, vec2};
use rustypipe::model::{
    AlbumItem, ArtistId, ArtistItem, MusicAlbum, MusicArtist, MusicPlaylist,
    MusicPlaylistItem, TrackItem,
};
use souvlaki::MediaControlEvent;

use crate::audio::{AudioCmd, AudioEvent, AudioHandle};
use crate::backend::{Backend, Home, Library, Req, Resp, SearchResults};
use crate::images::pick;
use crate::media::Media;

const ACCENT: Color32 = Color32::from_rgb(0xff, 0x33, 0x4b);
const DIM: Color32 = Color32::from_gray(150);
const ROW_H: f32 = 48.0;
const CARD: f32 = 150.0;

#[derive(Clone, PartialEq)]
enum Page {
    Home,
    Search(String),
    Album(String),
    Artist(String),
    Playlist(String),
    Library,
    Queue,
    Settings,
}

#[derive(Clone, Copy, PartialEq)]
enum Repeat {
    Off,
    All,
    One,
}

/// Something a track row's click or context menu asked for; applied after drawing.
enum Action {
    PlayList(Vec<TrackItem>, usize),
    PlayNext(TrackItem),
    Enqueue(TrackItem),
    Radio(TrackItem),
    Go(Page),
    QueueJump(usize),
    QueueRemove(usize),
}

pub struct App {
    backend: Backend,
    audio: AudioHandle,
    resp_rx: Receiver<Resp>,
    audio_rx: Receiver<AudioEvent>,
    media_rx: Receiver<MediaControlEvent>,
    media: Option<Media>,

    page: Page,
    history: Vec<Page>,
    loading: bool,
    search_text: String,

    home: Option<Home>,
    search: Option<SearchResults>,
    album: Option<MusicAlbum>,
    artist: Option<MusicArtist>,
    playlist: Option<MusicPlaylist>,
    library: Option<Library>,

    queue: Vec<TrackItem>,
    index: Option<usize>,
    generation: u64,
    buffering: bool,
    repeat: Repeat,
    autoplay: bool,
    radio_pending: bool,
    /// Position the user is dragging the seek bar to.
    seek_drag: Option<f32>,

    show_lyrics: bool,
    lyrics: Option<(String, Option<String>)>,

    logged_in: bool,
    cookie_input: String,
    toast: Option<(String, Instant)>,
    actions: Vec<Action>,
    /// Dev hook: `YTM_SMOKE="query"` searches and plays the first song on launch.
    smoke: Option<String>,
}

pub struct Channels {
    pub resp_rx: Receiver<Resp>,
    pub audio_rx: Receiver<AudioEvent>,
    pub media_rx: Receiver<MediaControlEvent>,
}

impl App {
    pub fn new(
        backend: Backend,
        audio: AudioHandle,
        ch: Channels,
        media: Option<Media>,
        storage: Option<&dyn eframe::Storage>,
    ) -> Self {
        let get = |k: &str| storage.and_then(|s| s.get_string(k));
        let logged_in = backend.is_logged_in();
        backend.request(Req::Home);
        let smoke = std::env::var("YTM_SMOKE").ok().filter(|q| !q.is_empty());
        let mut app = Self {
            backend,
            audio,
            resp_rx: ch.resp_rx,
            audio_rx: ch.audio_rx,
            media_rx: ch.media_rx,
            media,
            page: Page::Home,
            history: Vec::new(),
            loading: true,
            search_text: String::new(),
            home: None,
            search: None,
            album: None,
            artist: None,
            playlist: None,
            library: None,
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
            show_lyrics: false,
            lyrics: None,
            logged_in,
            cookie_input: String::new(),
            toast: None,
            actions: Vec::new(),
            smoke: None,
        };
        if let Some(q) = smoke {
            app.search_text = q.clone();
            app.go(Page::Search(q.clone()));
            app.smoke = Some(q);
        }
        app
    }

    // ---------------------------------------------------------------- navigation

    fn go(&mut self, page: Page) {
        if page == self.page {
            return;
        }
        let prev = std::mem::replace(&mut self.page, page);
        self.history.push(prev);
        self.load_page();
    }

    fn back(&mut self) {
        if let Some(p) = self.history.pop() {
            self.page = p;
            self.load_page();
        }
    }

    /// Request data for the current page unless it's already loaded.
    fn load_page(&mut self) {
        let req = match &self.page {
            Page::Home if self.home.is_none() => Some(Req::Home),
            Page::Search(q) if self.search.as_ref().is_none_or(|s| &s.query != q) => Some(Req::Search(q.clone())),
            Page::Album(id) if self.album.as_ref().is_none_or(|a| &a.id != id) => Some(Req::Album(id.clone())),
            Page::Artist(id) if self.artist.as_ref().is_none_or(|a| &a.id != id) => Some(Req::Artist(id.clone())),
            Page::Playlist(id) if self.playlist.as_ref().is_none_or(|p| &p.id != id) => {
                Some(Req::Playlist(id.clone()))
            }
            Page::Library if self.logged_in && self.library.is_none() => Some(Req::Library),
            _ => None,
        };
        self.loading = req.is_some();
        if let Some(r) = req {
            self.backend.request(r);
        }
    }

    // ---------------------------------------------------------------- playback

    fn current(&self) -> Option<&TrackItem> {
        self.index.and_then(|i| self.queue.get(i))
    }

    fn play_list(&mut self, tracks: Vec<TrackItem>, start: usize) {
        self.queue = tracks;
        self.index = Some(start);
        self.play_current();
    }

    fn play_current(&mut self) {
        let Some(track) = self.current().cloned() else { return };
        self.generation += 1;
        self.audio.status.wanted_gen.store(self.generation, std::sync::atomic::Ordering::Relaxed);
        self.audio.send(AudioCmd::Stop);
        self.buffering = true;
        self.seek_drag = None;
        self.backend.request(Req::Play { video_id: track.id.clone(), generation: self.generation });
        if let Some(m) = &mut self.media {
            m.set_track(
                &track.name,
                &artists(&track.artists),
                track.album.as_ref().map(|a| a.name.as_str()),
                pick(&track.cover, 300),
                track.duration,
            );
            m.set_playing(true);
        }
        self.lyrics = None;
        if self.show_lyrics {
            self.backend.request(Req::Lyrics(track.id.clone()));
        }
        // Keep the queue topped up so autoplay never stalls at the end.
        if self.autoplay && !self.radio_pending && self.index.is_some_and(|i| i + 1 >= self.queue.len()) {
            self.radio_pending = true;
            self.backend.request(Req::Radio(track.id));
        }
    }

    /// Start fetching whatever plays after the current track, so skipping is instant.
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
        let playing = self.audio.status.playing.load(std::sync::atomic::Ordering::Relaxed);
        self.audio.send(if playing { AudioCmd::Pause } else { AudioCmd::Resume });
        if let Some(m) = &mut self.media {
            m.set_playing(!playing);
        }
    }

    fn shuffle_upcoming(&mut self) {
        let start = self.index.map_or(0, |i| i + 1);
        if start >= self.queue.len() {
            return;
        }
        shuffle(&mut self.queue[start..]);
    }

    fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    fn apply(&mut self, a: Action) {
        match a {
            Action::PlayList(tracks, i) => self.play_list(tracks, i),
            Action::PlayNext(t) => {
                let at = self.index.map_or(0, |i| i + 1).min(self.queue.len());
                self.queue.insert(at, t);
                if self.index.is_none() {
                    self.index = Some(0);
                    self.play_current();
                }
            }
            Action::Enqueue(t) => {
                self.queue.push(t);
                if self.index.is_none() {
                    self.index = Some(self.queue.len() - 1);
                    self.play_current();
                }
            }
            Action::Radio(t) => {
                self.play_list(vec![t.clone()], 0);
                if !self.radio_pending {
                    self.radio_pending = true;
                    self.backend.request(Req::Radio(t.id));
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
        }
    }

    // ---------------------------------------------------------------- events

    fn pump(&mut self) {
        while let Ok(r) = self.resp_rx.try_recv() {
            match r {
                Resp::Home(h) => {
                    self.home = Some(h);
                    self.loading = false;
                }
                Resp::Search(r) => {
                    if self.smoke.as_ref() == Some(&r.query) {
                        self.smoke = None;
                        if let Some(t) = r.tracks.first() {
                            log::info!("smoke: playing {} ({})", t.name, t.id);
                            self.play_list(r.tracks.clone(), 0);
                        }
                    }
                    self.search = Some(r);
                    self.loading = false;
                }
                Resp::Album(a) => {
                    self.album = Some(a);
                    self.loading = false;
                }
                Resp::Artist(a) => {
                    self.artist = Some(a);
                    self.loading = false;
                }
                Resp::Playlist(p) => {
                    self.playlist = Some(p);
                    self.loading = false;
                }
                Resp::Library(l) => {
                    self.library = Some(l);
                    self.loading = false;
                }
                Resp::Lyrics { video_id, text } => {
                    if self.current().is_some_and(|t| t.id == video_id) {
                        self.lyrics = Some((video_id, text));
                    }
                }
                Resp::Radio { seed, tracks } => {
                    self.radio_pending = false;
                    let have: std::collections::HashSet<String> =
                        self.queue.iter().map(|t| t.id.clone()).collect();
                    let was_at_end = self.index.is_some_and(|i| i + 1 >= self.queue.len());
                    self.queue.extend(tracks.into_iter().filter(|t| t.id != seed && !have.contains(&t.id)));
                    if !self.buffering {
                        self.prefetch_next();
                    }
                    // The track ended while radio was loading: continue now.
                    if was_at_end && self.index.is_some() && !self.buffering
                        && !self.audio.status.playing.load(std::sync::atomic::Ordering::Relaxed)
                    {
                        self.next();
                    }
                }
                Resp::PlayError { generation, msg } => {
                    if generation == self.generation {
                        self.buffering = false;
                        self.toast(format!("Playback failed: {msg}"));
                    }
                }
                Resp::LoggedIn(v) => {
                    self.logged_in = v;
                    self.library = None;
                    self.toast(if v { "Signed in" } else { "Signed out" });
                    if v && self.page == Page::Library {
                        self.load_page();
                    }
                }
                Resp::Error(e) => {
                    self.loading = false;
                    self.radio_pending = false;
                    self.toast(e);
                }
            }
        }
        while let Ok(ev) = self.audio_rx.try_recv() {
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
        while let Ok(ev) = self.media_rx.try_recv() {
            match ev {
                MediaControlEvent::Play => self.audio.send(AudioCmd::Resume),
                MediaControlEvent::Pause => self.audio.send(AudioCmd::Pause),
                MediaControlEvent::Toggle => self.toggle(),
                MediaControlEvent::Next => self.next(),
                MediaControlEvent::Previous => self.prev(),
                MediaControlEvent::Stop => self.audio.send(AudioCmd::Pause),
                _ => {}
            }
        }
        if self.buffering && self.audio.status.playing.load(std::sync::atomic::Ordering::Relaxed) {
            self.buffering = false;
            self.prefetch_next();
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.egui_wants_keyboard_input();
        let (space, back, next, prev) = ctx.input(|i| {
            (
                !typing && i.key_pressed(egui::Key::Space),
                i.pointer.button_pressed(egui::PointerButton::Extra1)
                    || (i.modifiers.alt && i.key_pressed(egui::Key::ArrowLeft)),
                i.modifiers.ctrl && i.key_pressed(egui::Key::ArrowRight),
                i.modifiers.ctrl && i.key_pressed(egui::Key::ArrowLeft),
            )
        });
        if space {
            self.toggle();
        }
        if back {
            self.back();
        }
        if next {
            self.next();
        }
        if prev {
            self.prev();
        }
    }

    // ---------------------------------------------------------------- layout

    fn sidebar(&mut self, ui: &mut Ui) {
        ui.add_space(14.0);
        ui.label(RichText::new("YT Music").size(20.0).strong());
        ui.add_space(14.0);
        let nav = |ui: &mut Ui, label: &str, page: Page, this: &mut Self| {
            let selected = std::mem::discriminant(&this.page) == std::mem::discriminant(&page);
            let resp = ui.add_sized(
                [ui.available_width(), 32.0],
                egui::Button::selectable(selected, RichText::new(label).size(15.0)),
            );
            if resp.clicked() {
                this.go(page);
            }
        };
        nav(ui, "Home", Page::Home, self);
        nav(ui, "Library", Page::Library, self);
        nav(ui, "Queue", Page::Queue, self);
        nav(ui, "Settings", Page::Settings, self);
    }

    fn top_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let can_back = !self.history.is_empty();
            let back = ui.add_enabled(can_back, egui::Button::new(RichText::new("⏴").size(18.0)).frame(false));
            if named(back, "Back").on_hover_text("Back").clicked() {
                self.back();
            }
            let search = ui.add(
                egui::TextEdit::singleline(&mut self.search_text)
                    .hint_text("Search songs, albums, artists")
                    .desired_width(380.0),
            );
            if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::F)) {
                search.request_focus();
            }
            if search.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let q = self.search_text.trim().to_owned();
                if !q.is_empty() {
                    self.go(Page::Search(q));
                }
            }
            if self.loading {
                ui.spinner();
            }
        });
    }

    fn player_bar(&mut self, ui: &mut Ui) {
        let status = self.audio.status.clone();
        let playing = status.playing.load(std::sync::atomic::Ordering::Relaxed);
        let track = self.current().cloned();
        let total = track.as_ref().and_then(|t| t.duration).unwrap_or(0) as f32;

        ui.columns_const(|[left, mid, right]| {
            // Now playing
            left.horizontal(|ui| {
                if let Some(t) = &track {
                    cover(ui, pick(&t.cover, 120), 56.0);
                    ui.vertical(|ui| {
                        ui.add_space(8.0);
                        ui.add(egui::Label::new(RichText::new(&t.name).strong()).truncate());
                        artist_links(ui, &t.artists, &mut self.actions);
                    });
                }
            });

            // Transport + seek bar
            mid.vertical_centered(|ui| {
                ui.horizontal(|ui| {
                    let w = 5.0 * 36.0;
                    ui.add_space((ui.available_width() - w).max(0.0) / 2.0);
                    let rep = match self.repeat {
                        Repeat::Off => RichText::new("🔁").color(DIM),
                        Repeat::All => RichText::new("🔁").color(ACCENT),
                        Repeat::One => RichText::new("🔂").color(ACCENT),
                    };
                    if icon(ui, "🔀", "Shuffle upcoming").clicked() {
                        self.shuffle_upcoming();
                    }
                    if icon(ui, "⏮", "Previous").clicked() {
                        self.prev();
                    }
                    let play_icon = if self.buffering { "…" } else if playing { "⏸" } else { "▶" };
                    let play = ui.add(
                        egui::Button::new(RichText::new(play_icon).size(18.0).color(Color32::BLACK))
                            .fill(Color32::WHITE)
                            .corner_radius(CornerRadius::same(16))
                            .min_size(vec2(32.0, 32.0)),
                    );
                    if named(play, if playing { "Pause" } else { "Play" }).clicked() {
                        self.toggle();
                    }
                    if icon(ui, "⏭", "Next").clicked() {
                        self.next();
                    }
                    let rep_btn = ui.add(egui::Button::new(rep.size(16.0)).frame(false));
                    if named(rep_btn, "Repeat").on_hover_text("Repeat").clicked() {
                        self.repeat = match self.repeat {
                            Repeat::Off => Repeat::All,
                            Repeat::All => Repeat::One,
                            Repeat::One => Repeat::Off,
                        };
                    }
                });
                ui.horizontal(|ui| {
                    let pos = self.seek_drag.unwrap_or(status.position().as_secs_f32()).min(total.max(0.0));
                    ui.label(RichText::new(fmt_time(pos as u32)).color(DIM).small());
                    let mut v = pos;
                    ui.spacing_mut().slider_width = (ui.available_width() - 44.0).max(60.0);
                    let r = ui.add_enabled(
                        total > 0.0,
                        egui::Slider::new(&mut v, 0.0..=total.max(1.0)).show_value(false).trailing_fill(true),
                    );
                    r.widget_info(|| egui::WidgetInfo::slider(total > 0.0, v as f64, "Seek"));
                    if r.dragged() || r.changed() {
                        self.seek_drag = Some(v);
                    }
                    if r.drag_stopped() || (r.changed() && !r.dragged()) {
                        if let Some(to) = self.seek_drag.take() {
                            self.audio.send(AudioCmd::Seek(Duration::from_secs_f32(to)));
                        }
                    }
                    ui.label(RichText::new(fmt_time(total as u32)).color(DIM).small());
                });
            });

            // Volume + panels
            right.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(8.0);
                let lyr = RichText::new("Lyrics").color(if self.show_lyrics { ACCENT } else { DIM });
                if ui.add(egui::Button::new(lyr).frame(false)).clicked() {
                    self.show_lyrics = !self.show_lyrics;
                    if self.show_lyrics && self.lyrics.is_none() {
                        if let Some(t) = self.current() {
                            self.backend.request(Req::Lyrics(t.id.clone()));
                        }
                    }
                }
                let mut vol = self.audio.volume();
                ui.spacing_mut().slider_width = 100.0;
                let vr = ui.add(egui::Slider::new(&mut vol, 0.0..=1.0).show_value(false));
                vr.widget_info(|| egui::WidgetInfo::slider(true, vol as f64, "Volume"));
                if vr.changed() {
                    self.audio.set_volume(vol);
                }
                ui.label(RichText::new(if vol == 0.0 { "🔇" } else { "🔊" }).color(DIM));
            });
        });
    }

    fn lyrics_panel(&mut self, ui: &mut Ui) {
        ui.add_space(10.0);
        ui.heading("Lyrics");
        ui.separator();
        egui::ScrollArea::vertical().show(ui, |ui| match (&self.lyrics, self.current()) {
            (_, None) => {
                ui.label(RichText::new("Nothing playing").color(DIM));
            }
            (Some((_, Some(text))), _) => {
                ui.label(RichText::new(text).size(15.0));
            }
            (Some((_, None)), _) => {
                ui.label(RichText::new("No lyrics for this track").color(DIM));
            }
            (None, _) => {
                ui.spinner();
            }
        });
    }

    fn page(&mut self, ui: &mut Ui) {
        let mut acts = std::mem::take(&mut self.actions);
        match self.page.clone() {
            Page::Home => self.home_page(ui, &mut acts),
            Page::Search(q) => self.search_page(ui, &q, &mut acts),
            Page::Album(_) => self.album_page(ui, &mut acts),
            Page::Artist(_) => self.artist_page(ui, &mut acts),
            Page::Playlist(_) => self.playlist_page(ui, &mut acts),
            Page::Library => self.library_page(ui, &mut acts),
            Page::Queue => self.queue_page(ui, &mut acts),
            Page::Settings => self.settings_page(ui),
        }
        self.actions.append(&mut acts);
    }

    fn home_page(&self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let Some(h) = &self.home else { return };
        scroll(ui, |ui| {
            if !h.new_albums.is_empty() {
                section(ui, "New releases");
                album_cards(ui, "new", &h.new_albums, acts);
            }
            if !h.top_tracks.is_empty() {
                section(ui, "Top songs");
                track_list(ui, &h.top_tracks[..h.top_tracks.len().min(20)], &h.top_tracks, self.current_id(), false, acts);
            }
            if !h.trending.is_empty() {
                section(ui, "Trending");
                track_list(ui, &h.trending[..h.trending.len().min(20)], &h.trending, self.current_id(), false, acts);
            }
            if !h.playlists.is_empty() {
                section(ui, "Charts");
                playlist_cards(ui, "charts", &h.playlists, acts);
            }
        });
    }

    fn search_page(&self, ui: &mut Ui, q: &str, acts: &mut Vec<Action>) {
        let Some(r) = self.search.as_ref().filter(|r| r.query == q) else { return };
        scroll(ui, |ui| {
            if r.tracks.is_empty() && r.albums.is_empty() && r.artists.is_empty() && r.playlists.is_empty() {
                ui.label(RichText::new("No results").color(DIM));
            }
            if !r.artists.is_empty() {
                section(ui, "Artists");
                artist_cards(ui, "s-artists", &r.artists, acts);
            }
            if !r.tracks.is_empty() {
                section(ui, "Songs");
                track_list(ui, &r.tracks, &r.tracks, self.current_id(), false, acts);
            }
            if !r.albums.is_empty() {
                section(ui, "Albums");
                album_cards(ui, "s-albums", &r.albums, acts);
            }
            if !r.playlists.is_empty() {
                section(ui, "Playlists");
                playlist_cards(ui, "s-pl", &r.playlists, acts);
            }
        });
    }

    fn album_page(&self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let Some(a) = self.album.as_ref().filter(|a| self.page == Page::Album(a.id.clone())) else { return };
        scroll(ui, |ui| {
            let sub = format!(
                "{} · {}{} songs",
                artists(&a.artists),
                a.year.map(|y| format!("{y} · ")).unwrap_or_default(),
                a.tracks.len()
            );
            header(ui, pick(&a.cover, 300), &a.name, &sub, &a.tracks, acts);
            track_list(ui, &a.tracks, &a.tracks, self.current_id(), true, acts);
        });
    }

    fn artist_page(&self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let Some(a) = self.artist.as_ref().filter(|a| self.page == Page::Artist(a.id.clone())) else { return };
        scroll(ui, |ui| {
            let sub = a.subscriber_count.map(|n| format!("{} subscribers", compact(n))).unwrap_or_default();
            header(ui, pick(&a.header_image, 300), &a.name, &sub, &a.tracks, acts);
            if let Some(t) = a.tracks.first() {
                if ui.button("Start artist radio").clicked() {
                    acts.push(Action::Radio(t.clone()));
                }
            }
            section(ui, "Songs");
            track_list(ui, &a.tracks, &a.tracks, self.current_id(), false, acts);
            if !a.albums.is_empty() {
                section(ui, "Albums & singles");
                album_cards(ui, "a-albums", &a.albums, acts);
            }
            if !a.playlists.is_empty() {
                section(ui, "Playlists");
                playlist_cards(ui, "a-pl", &a.playlists, acts);
            }
            if !a.similar_artists.is_empty() {
                section(ui, "Fans might also like");
                artist_cards(ui, "a-sim", &a.similar_artists, acts);
            }
        });
    }

    fn playlist_page(&self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let Some(p) = self.playlist.as_ref().filter(|p| self.page == Page::Playlist(p.id.clone())) else { return };
        let tracks = &p.tracks.items;
        scroll(ui, |ui| {
            let sub = format!(
                "{}{} songs",
                p.channel.as_ref().map(|c| format!("{} · ", c.name)).unwrap_or_default(),
                p.track_count.unwrap_or(tracks.len() as u64)
            );
            header(ui, pick(&p.thumbnail, 300), &p.name, &sub, tracks, acts);
            track_list(ui, tracks, tracks, self.current_id(), false, acts);
        });
    }

    fn library_page(&self, ui: &mut Ui, acts: &mut Vec<Action>) {
        if !self.logged_in {
            ui.add_space(20.0);
            ui.label("Sign in to see your liked songs, playlists and albums.");
            if ui.button("Go to Settings").clicked() {
                acts.push(Action::Go(Page::Settings));
            }
            return;
        }
        let Some(l) = &self.library else { return };
        scroll(ui, |ui| {
            if !l.playlists.is_empty() {
                section(ui, "Playlists");
                playlist_cards(ui, "lib-pl", &l.playlists, acts);
            }
            if !l.albums.is_empty() {
                section(ui, "Albums");
                album_cards(ui, "lib-al", &l.albums, acts);
            }
            section(ui, &format!("Liked songs ({})", l.liked.len()));
            track_list(ui, &l.liked, &l.liked, self.current_id(), false, acts);
        });
    }

    fn queue_page(&self, ui: &mut Ui, acts: &mut Vec<Action>) {
        scroll(ui, |ui| {
            section(ui, "Queue");
            if self.queue.is_empty() {
                ui.label(RichText::new("Queue is empty").color(DIM));
            }
            for (i, t) in self.queue.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                let current = Some(i) == self.index;
                let resp = named(resp, &t.name);
                track_row(ui, rect, &resp, t, None, current);
                if resp.clicked() {
                    acts.push(Action::QueueJump(i));
                }
                resp.context_menu(|ui| {
                    if ui.button("Play").clicked() {
                        acts.push(Action::QueueJump(i));
                    }
                    if ui.add_enabled(!current, egui::Button::new("Remove from queue")).clicked() {
                        acts.push(Action::QueueRemove(i));
                    }
                });
            }
        });
    }

    fn settings_page(&mut self, ui: &mut Ui) {
        scroll(ui, |ui| {
            section(ui, "Account");
            if self.logged_in {
                ui.label("Signed in with browser cookies.");
                if ui.button("Sign out").clicked() {
                    self.backend.request(Req::Logout);
                }
            } else {
                ui.label(
                    "Open music.youtube.com in your browser while signed in, open DevTools → Network, \
                     click any request to music.youtube.com and copy the full `cookie` request header.",
                );
                ui.add(
                    egui::TextEdit::multiline(&mut self.cookie_input)
                        .hint_text("Paste cookie header here")
                        .password(true)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                );
                if ui.add_enabled(!self.cookie_input.trim().is_empty(), egui::Button::new("Sign in")).clicked() {
                    let c = std::mem::take(&mut self.cookie_input);
                    self.backend.request(Req::SetCookie(c.trim().to_owned()));
                }
            }
            section(ui, "Playback");
            ui.checkbox(&mut self.autoplay, "Autoplay similar songs when the queue ends");
            section(ui, "Shortcuts");
            for (k, v) in [
                ("Space", "Play / pause"),
                ("Ctrl + → / ←", "Next / previous"),
                ("Ctrl + F", "Search"),
                ("Alt + ← / mouse back", "Go back"),
            ] {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(k).monospace());
                    ui.label(RichText::new(v).color(DIM));
                });
            }
        });
    }

    fn current_id(&self) -> Option<&str> {
        self.current().map(|t| t.id.as_str())
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump();
        for a in std::mem::take(&mut self.actions) {
            self.apply(a);
        }
        // Only tick while audio is playing; an idle player costs no CPU.
        if self.audio.status.playing.load(std::sync::atomic::Ordering::Relaxed) || self.buffering {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        if self.toast.as_ref().is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(5)) {
            self.toast = None;
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.shortcuts(&ctx);

        egui::Panel::bottom("player")
            .exact_size(84.0)
            .frame(egui::Frame::new().fill(Color32::from_gray(18)).inner_margin(10))
            .show(ui, |ui| self.player_bar(ui));
        egui::Panel::left("nav")
            .exact_size(190.0)
            .frame(egui::Frame::new().fill(Color32::from_gray(10)).inner_margin(12))
            .show(ui, |ui| self.sidebar(ui));
        if self.show_lyrics {
            egui::Panel::right("lyrics")
                .default_size(320.0)
                .frame(egui::Frame::new().fill(Color32::from_gray(14)).inner_margin(12))
                .show(ui, |ui| self.lyrics_panel(ui));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Color32::from_gray(5)).inner_margin(16))
            .show(ui, |ui| {
                self.top_bar(ui);
                ui.add_space(8.0);
                self.page(ui);
            });

        if let Some((msg, _)) = &self.toast {
            egui::Area::new("toast".into())
                .anchor(egui::Align2::CENTER_BOTTOM, vec2(0.0, -100.0))
                .show(&ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width(520.0);
                        ui.label(msg);
                    });
                });
            ctx.request_repaint_after(Duration::from_secs(1));
        }

        // Thumbnails are cheap to refetch; cap the decoded cache instead of growing forever.
        let bytes: usize = ctx.loaders().bytes.lock().iter().map(|l| l.byte_size()).sum();
        if bytes > 48 * 1024 * 1024 {
            ctx.forget_all_images();
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string("volume", self.audio.volume().to_string());
        storage.set_string(
            "repeat",
            match self.repeat {
                Repeat::Off => "off",
                Repeat::All => "all",
                Repeat::One => "one",
            }
            .into(),
        );
        storage.set_string("autoplay", if self.autoplay { "1" } else { "0" }.into());
    }
}

// -------------------------------------------------------------------- widgets

fn scroll(ui: &mut Ui, f: impl FnOnce(&mut Ui)) {
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, f);
}

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(18.0);
    ui.label(RichText::new(title).size(20.0).strong());
    ui.add_space(6.0);
}

fn icon(ui: &mut Ui, s: &str, name: &str) -> egui::Response {
    let resp = ui.add(egui::Button::new(RichText::new(s).size(16.0)).frame(false).min_size(vec2(32.0, 32.0)));
    named(resp, name).on_hover_text(name)
}

/// Give an icon-only or custom-painted widget a readable name for screen readers.
fn named(resp: egui::Response, name: &str) -> egui::Response {
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
    resp
}

/// Draws a cover image only when on screen, so off-screen rows never fetch thumbnails.
fn cover(ui: &mut Ui, url: Option<&str>, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint_cover(ui, rect, url);
}

fn paint_cover(ui: &mut Ui, rect: egui::Rect, url: Option<&str>) {
    if !ui.is_rect_visible(rect) {
        return;
    }
    ui.painter().rect_filled(rect, 4.0, Color32::from_gray(35));
    if let Some(url) = url {
        egui::Image::new(url).corner_radius(4).paint_at(ui, rect);
    }
}

fn header(ui: &mut Ui, img: Option<&str>, title: &str, sub: &str, tracks: &[TrackItem], acts: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        cover(ui, img, 160.0);
        ui.add_space(12.0);
        ui.vertical(|ui| {
            ui.add_space(30.0);
            ui.label(RichText::new(title).size(28.0).strong());
            ui.label(RichText::new(sub).color(DIM));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let play = egui::Button::new(RichText::new("▶  Play").color(Color32::BLACK).strong())
                    .fill(Color32::WHITE)
                    .corner_radius(16);
                if ui.add_enabled(!tracks.is_empty(), play).clicked() {
                    acts.push(Action::PlayList(tracks.to_vec(), 0));
                }
                if ui.add_enabled(!tracks.is_empty(), egui::Button::new("🔀  Shuffle").corner_radius(16)).clicked() {
                    let mut t = tracks.to_vec();
                    shuffle(&mut t);
                    acts.push(Action::PlayList(t, 0));
                }
            });
        });
    });
}

/// `shown` is what's drawn; `context` is the list queued when a row is played.
fn track_list(
    ui: &mut Ui,
    shown: &[TrackItem],
    context: &[TrackItem],
    current: Option<&str>,
    numbered: bool,
    acts: &mut Vec<Action>,
) {
    for (i, t) in shown.iter().enumerate() {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
        if !ui.is_rect_visible(rect) {
            continue;
        }
        let resp = named(resp, &t.name);
        let nr = numbered.then(|| t.track_nr.map_or(i as u16 + 1, |n| n));
        track_row(ui, rect, &resp, t, nr, current == Some(t.id.as_str()));
        if resp.clicked() {
            acts.push(Action::PlayList(context.to_vec(), i));
        }
        track_menu(&resp, t, context, i, acts);
    }
}

fn track_row(ui: &mut Ui, rect: egui::Rect, resp: &egui::Response, t: &TrackItem, nr: Option<u16>, current: bool) {
    if resp.hovered() {
        ui.painter().rect_filled(rect, 6.0, Color32::from_gray(28));
    }
    let mut x = rect.left() + 8.0;
    if let Some(n) = nr {
        ui.painter().text(
            egui::pos2(x + 10.0, rect.center().y),
            egui::Align2::CENTER_CENTER,
            n.to_string(),
            egui::FontId::proportional(14.0),
            DIM,
        );
        x += 28.0;
    } else {
        let img = egui::Rect::from_min_size(egui::pos2(x, rect.top() + 4.0), Vec2::splat(40.0));
        paint_cover(ui, img, pick(&t.cover, 60));
        x += 52.0;
    }
    let dur_w = 56.0;
    let text_w = (rect.right() - dur_w - x).max(40.0);
    let title_col = if current { ACCENT } else { Color32::from_gray(235) };
    let galley = |s: &str, size: f32, col: Color32, w: f32| {
        let mut job = egui::text::LayoutJob::simple_singleline(s.to_owned(), egui::FontId::proportional(size), col);
        job.wrap = egui::text::TextWrapping::truncate_at_width(w);
        ui.fonts_mut(|f| f.layout_job(job))
    };
    let mut sub = artists(&t.artists);
    if let Some(a) = &t.album {
        sub.push_str(" · ");
        sub.push_str(&a.name);
    }
    ui.painter().galley(egui::pos2(x, rect.top() + 6.0), galley(&t.name, 15.0, title_col, text_w), title_col);
    ui.painter().galley(egui::pos2(x, rect.top() + 26.0), galley(&sub, 13.0, DIM, text_w), DIM);
    if let Some(d) = t.duration {
        ui.painter().text(
            egui::pos2(rect.right() - 12.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            fmt_time(d),
            egui::FontId::proportional(13.0),
            DIM,
        );
    }
}

fn track_menu(resp: &egui::Response, t: &TrackItem, context: &[TrackItem], i: usize, acts: &mut Vec<Action>) {
    resp.context_menu(|ui| {
        if ui.button("Play").clicked() {
            acts.push(Action::PlayList(context.to_vec(), i));
        }
        if ui.button("Play next").clicked() {
            acts.push(Action::PlayNext(t.clone()));
        }
        if ui.button("Add to queue").clicked() {
            acts.push(Action::Enqueue(t.clone()));
        }
        if ui.button("Start radio").clicked() {
            acts.push(Action::Radio(t.clone()));
        }
        ui.separator();
        if let Some(a) = &t.album {
            if ui.button("Go to album").clicked() {
                acts.push(Action::Go(Page::Album(a.id.clone())));
            }
        }
        if let Some(id) = t.artist_id.clone().or_else(|| t.artists.iter().find_map(|a| a.id.clone())) {
            if ui.button("Go to artist").clicked() {
                acts.push(Action::Go(Page::Artist(id)));
            }
        }
    });
}

fn artist_links(ui: &mut Ui, list: &[ArtistId], acts: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (i, a) in list.iter().enumerate() {
            if i > 0 {
                ui.label(RichText::new(", ").color(DIM));
            }
            match &a.id {
                Some(id) => {
                    if ui.add(egui::Label::new(RichText::new(&a.name).color(DIM)).sense(Sense::click()))
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        acts.push(Action::Go(Page::Artist(id.clone())));
                    }
                }
                None => {
                    ui.label(RichText::new(&a.name).color(DIM));
                }
            }
        }
    });
}

/// Horizontally scrolling row of cards.
fn cards<T>(ui: &mut Ui, id: &str, items: &[T], round: bool, f: impl Fn(&T) -> (Option<&str>, &str, String, Page), acts: &mut Vec<Action>) {
    egui::ScrollArea::horizontal().id_salt(id).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            for it in items {
                let (img, title, sub, page) = f(it);
                let (rect, resp) = ui.allocate_exact_size(vec2(CARD, CARD + 46.0), Sense::click());
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                let resp = named(resp, title);
                if resp.hovered() {
                    ui.painter().rect_filled(rect.expand(4.0), 8.0, Color32::from_gray(24));
                }
                let img_rect = egui::Rect::from_min_size(rect.min, Vec2::splat(CARD));
                ui.painter().rect_filled(img_rect, if round { CARD / 2.0 } else { 6.0 }, Color32::from_gray(35));
                if let Some(url) = img {
                    let r = if round { (CARD / 2.0) as u8 } else { 6 };
                    egui::Image::new(url).corner_radius(r).paint_at(ui, img_rect);
                }
                let trunc = |s: &str, size: f32, col: Color32| {
                    let mut job = egui::text::LayoutJob::simple_singleline(s.to_owned(), egui::FontId::proportional(size), col);
                    job.wrap = egui::text::TextWrapping::truncate_at_width(CARD);
                    ui.fonts_mut(|fo| fo.layout_job(job))
                };
                let white = Color32::from_gray(235);
                ui.painter().galley(egui::pos2(rect.left(), img_rect.bottom() + 6.0), trunc(title, 14.0, white), white);
                ui.painter().galley(egui::pos2(rect.left(), img_rect.bottom() + 25.0), trunc(&sub, 12.0, DIM), DIM);
                if resp.clicked() {
                    acts.push(Action::Go(page));
                }
            }
        });
    });
}

fn album_cards(ui: &mut Ui, id: &str, items: &[AlbumItem], acts: &mut Vec<Action>) {
    cards(ui, id, items, false, |a| {
        let sub = match a.year {
            Some(y) => format!("{y} · {}", artists(&a.artists)),
            None => artists(&a.artists),
        };
        (pick(&a.cover, 226), a.name.as_str(), sub, Page::Album(a.id.clone()))
    }, acts);
}

fn playlist_cards(ui: &mut Ui, id: &str, items: &[MusicPlaylistItem], acts: &mut Vec<Action>) {
    cards(ui, id, items, false, |p| {
        let sub = p.channel.as_ref().map(|c| c.name.clone()).unwrap_or_else(|| "Playlist".into());
        (pick(&p.thumbnail, 226), p.name.as_str(), sub, Page::Playlist(p.id.clone()))
    }, acts);
}

fn artist_cards(ui: &mut Ui, id: &str, items: &[ArtistItem], acts: &mut Vec<Action>) {
    cards(ui, id, items, true, |a| {
        let sub = a.subscriber_count.map(|n| format!("{} subscribers", compact(n))).unwrap_or_else(|| "Artist".into());
        (pick(&a.avatar, 226), a.name.as_str(), sub, Page::Artist(a.id.clone()))
    }, acts);
}

fn artists(list: &[ArtistId]) -> String {
    list.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
}

fn fmt_time(secs: u32) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn compact(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}K", n as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{:.1}B", n as f64 / 1e9),
    }
}

/// Fisher-Yates with a clock-seeded xorshift; shuffling doesn't need a real RNG crate.
fn shuffle<T>(v: &mut [T]) {
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
