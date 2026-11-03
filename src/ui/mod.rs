//! The application UI: state, playback logic and the per-frame layout.
//!
//! Layout: animated album-art background (shader.rs) → sidebar, top bar and page
//! content → player bar → now-playing overlay (slides up) → toasts.

mod account;
mod actions;
mod collections;
mod dev;
mod dialogs;
mod integrations;
mod jam;
mod karaoke;
mod lists;
mod lyric_prefs;
mod mini;
mod notes;
mod nowplaying;
mod pages;
mod playback;
mod queue;
mod responses;
mod shell;
mod titlebar;
pub use titlebar::{custom as custom_titlebar, set_hwnd as set_titlebar_hwnd};
mod transfer;
mod widgets;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crossbeam_channel::Receiver;
use egui::{Color32, Ui};
use souvlaki::MediaControlEvent;

use crate::audio::{AudioEvent, AudioHandle};
use crate::backend::{ArtistPage, Backend, Explore, Library, Req, Resp, SearchResults};
use crate::lyrics::LyricsSet;
use crate::media::Media;
use crate::model::{Card, Chip, Shelf, Target, Track};
use crate::shader::Background;
use crate::theme;
use lists::{LIKED_MUSIC, Lists};
use queue::Queue;
use dev::{dev_pace, dev_screenshot, dev_var};
use dialogs::text_field;
pub use queue::Repeat;

#[derive(Clone, PartialEq, Hash, Debug)]
pub enum Page {
    Home,
    Explore,
    Library(LibTab),
    Search(String),
    Album(String),
    Playlist(String),
    Artist(String),
    Settings,
    /// Transfer from Spotify.
    Transfer,
    /// Listen together.
    Jam,
}

#[derive(Clone, Copy, PartialEq, Hash, Debug)]
pub enum LibTab {
    Playlists,
    Songs,
    Albums,
    History,
    Uploads,
}

#[derive(Clone, Copy, PartialEq)]
pub enum NpTab {
    Lyrics,
    UpNext,
    /// Neither panel: just the artwork.
    Art,
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
    Like(Track, bool),
    /// Like (or unlike) a selection of songs.
    LikeMany(Vec<Track>, bool),
    Dislike(Track),
    Share(Target),
    AddToPlaylist(Vec<Track>),
    /// Rows (indices into the open playlist) to take out of it.
    RemoveFromPlaylist(Vec<usize>),
    Download(Track),
    NewPlaylist(Vec<Track>),
    EditPlaylist,
    /// The open playlist's collaboration settings.
    Collaborate,
    DeletePlaylist,
    SaveCollection(String, bool),
    HomeChip(Option<String>),
    OpenNowPlaying(bool),
    /// Into the mini player (true) or back to the full window.
    Mini(bool),
    /// Play/pause what's already loaded (a card's button on the playing item).
    TogglePlay,
    /// A song chosen on the search page as the Spotify transfer's match.
    TransferPick(Track),
}

enum Dialog {
    PickPlaylist(Vec<Track>),
    NewPlaylist { title: String, tracks: Vec<Track> },
    EditPlaylist { id: String, title: String, description: String, privacy: Option<String> },
    ConfirmDelete { id: String, title: String },
    /// `info` until it loads; `busy` while a change is on its way; `confirm_off` asks
    /// before turning collaboration off (that removes the collaborators).
    Collaborate { id: String, info: Option<crate::ytm::Collaboration>, busy: bool, confirm_off: bool },
}

/// Songs picked in a list for a batch action: Ctrl-click toggles one, Shift-click a range,
/// or the Select button makes every click pick.
#[derive(Default)]
pub struct Selection {
    /// The page the picks belong to; on any other page there are none.
    pub page: Option<Page>,
    /// Row indices into that page's list.
    pub rows: std::collections::BTreeSet<usize>,
    pub anchor: Option<usize>,
    /// Select mode: a plain click picks instead of playing.
    pub active: bool,
}

impl Selection {
    pub fn on(&self, page: &Page) -> bool {
        self.page.as_ref() == Some(page) && (self.active || !self.rows.is_empty())
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Start select mode on `page` with nothing picked yet.
    pub fn start(&mut self, page: &Page) {
        *self = Self { page: Some(page.clone()), active: true, ..Self::default() };
    }

    /// Toggle row `i`, or with `range` pick every row from the last one toggled to it.
    pub fn pick(&mut self, page: &Page, i: usize, range: bool) {
        if self.page.as_ref() != Some(page) {
            *self = Self { page: Some(page.clone()), ..Self::default() };
        }
        match self.anchor.filter(|_| range) {
            Some(a) => self.rows.extend(a.min(i)..=a.max(i)),
            None => {
                if !self.rows.remove(&i) {
                    self.rows.insert(i);
                }
                self.anchor = Some(i);
            }
        }
    }
}

pub struct HomeState {
    pub chips: Vec<Chip>,
    pub shelves: Vec<Shelf>,
    pub continuation: Option<String>,
    /// When the next shelves were requested (app time, s), while they load.
    pub loading_more: Option<f64>,
    pub picks_loading: bool,
    pub picks_expanded: bool,
}

pub struct Channels {
    pub resp_rx: Receiver<Resp>,
    pub audio_rx: Receiver<AudioEvent>,
    pub media_rx: Receiver<MediaControlEvent>,
}

/// Settings › Network's connection test: not run (`None`), running (`Some(None)`), or
/// its answer (works, message).
type ProxyTest = Option<Option<(bool, String)>>;

pub struct App {
    backend: Backend,
    audio: AudioHandle,
    ch: Channels,
    media: Option<Media>,
    discord: Option<crate::discord::Discord>,
    discord_enabled: bool,
    /// Last state sent to Discord, to send only real changes.
    discord_sent: Option<crate::discord::Presence>,
    /// The Last.fm account scrobbles go to; a sign-in in progress and its outcome.
    lastfm_user: Option<String>,
    lastfm_busy: Arc<AtomicBool>,
    lastfm_result: Arc<std::sync::Mutex<Option<Result<String, String>>>>,
    /// The play generation Last.fm last heard about, and whether it was scrobbled.
    scrobble: Option<(u64, bool)>,
    /// Last session's song, restored paused: Play starts it from here.
    resume_at: Option<Duration>,
    /// That song came from another device's queue, so playing it here is a new play.
    resume_counts: bool,
    /// When something last played here (unix seconds), to tell whether another
    /// device has played since. When the cross-device queue was last asked for (`now`).
    listened_at: u64,
    queue_checked: Option<f64>,
    was_focused: bool,
    /// Album / playlist / artist the queue was started from, so its card shows pause.
    queue_source: Option<Target>,
    /// Asked to play (shuffled?) before its songs are here: a playlist's first page, or
    /// an album's or artist's songs (`Req::PlayCollection`).
    play_pending: Option<(Target, bool)>,
    bg: Option<Background>,

    // navigation
    page: Page,
    back: Vec<Page>,
    forward: Vec<Page>,
    page_changed: f64,
    loading: bool,
    /// The current page's request failed with nothing cached: show a retry, not a skeleton.
    load_failed: bool,
    search_text: String,
    suggestions: (String, Vec<crate::ytm::Suggestion>),
    /// The suggestion picked with the arrow keys, which Enter searches.
    suggest_sel: Option<usize>,
    /// Bumped when a past search is removed or the account changes: suggestions
    /// requested before that are dropped when they arrive, rather than bring it back.
    suggest_round: u64,
    /// Removals sent and not yet confirmed: the list is asked for again once they're done.
    forgets_pending: u32,
    suggest_due: Option<f64>,

    // page data
    home: Option<HomeState>,
    home_loading: bool,
    explore: Option<Explore>,
    search: Option<SearchResults>,
    /// Albums and playlists opened lately, so going back to one shows it at once (and
    /// refreshes it quietly), and their loads under way.
    lists: Lists,
    artist: Option<ArtistPage>,
    library: Option<Library>,
    /// When the library was last requested; the sidebar spinner gives up after a while.
    library_requested: std::time::Instant,
    history: Option<Vec<Track>>,
    uploads: Option<Vec<Track>>,
    /// Files chosen in the upload dialog, handed over from its thread.
    picked_uploads: Arc<std::sync::Mutex<Option<Vec<std::path::PathBuf>>>>,
    liked: HashSet<String>,
    /// Songs whose unlike is queued and hasn't gone through yet.
    unliking: HashMap<String, Track>,
    selection: Selection,
    account_name: Option<String>,
    account_avatar: Option<String>,
    /// The signed-in account's channels and which one is used, once Settings asked.
    channels: Option<crate::backend::Channels>,
    /// When they were last asked for (`ui.input` time).
    channels_asked: Option<f64>,
    /// The channel being switched to, named for the toast once it's done.
    switching_to: Option<String>,
    /// Last launch's playlists, shown in the sidebar until the library arrives.
    sidebar_cache: Option<Vec<Card>>,

    // playback
    queue: Queue,
    generation: u64,
    buffering: bool,
    /// Unplayable songs skipped in a row; a run of them stops playback instead of
    /// racing through the queue.
    skip_streak: u32,
    /// Times the current song was reopened after its download failed.
    recoveries: u32,
    autoplay: bool,
    /// Turn loud tracks down to YouTube's loudness target.
    normalize: bool,
    log_volume: bool,
    high_quality: bool,
    /// Play an official music video as its song version when one is found.
    play_songs: bool,
    /// The song version playing in place of the current music video.
    playing_as: Option<PlayingAs>,
    /// The output device picked in settings (cpal's id and its name, for when it's not
    /// connected), and the devices last listed there.
    output_device: Option<(String, String)>,
    output_devices: Vec<crate::audio::OutputDevice>,
    radio_pending: bool,
    seek_drag: Option<f32>,

    // visuals
    art_key: Option<String>,
    art_changed: f64,
    accent: Color32,
    default_theme: bool,
    /// Selected theme preset (see `preset.rs`); applies unless `default_theme`.
    theme_id: String,
    /// Preset whose chrome (glass, corner radius) is installed in the egui style.
    chrome_applied: Option<String>,
    /// Light or dark pages (the ytm-rs theme's; see `light_pages`).
    appearance: theme::Appearance,
    anim_bg: bool,
    reactive_bg: bool,
    /// Skip the parts of music videos SponsorBlock marks as not music.
    sponsorblock: bool,
    /// Crossfade between songs, and over how many seconds.
    crossfade: bool,
    crossfade_secs: u32,
    /// Also crossfade when skipping or picking a song.
    crossfade_skips: bool,
    /// The next song starts because this one is ending: crossfade into it.
    handoff: bool,
    /// SponsorBlock segments for a video, and when one was last skipped.
    segments: Option<(String, Vec<crate::sponsorblock::Segment>)>,
    segment_skipped: f64,
    /// Background animation clock: advances only while music plays, so pausing
    /// freezes the motion instead of jumping when resumed.
    bg_time: f64,
    last_frame: f64,
    search_rect: Option<egui::Rect>,
    search_focused: bool,
    /// Give the search box its focus back (removing a past search takes it away).
    search_refocus: bool,
    /// A past search was removed during the current press: the rows below slide up
    /// under the held pointer, and must not go too.
    forget_held: bool,
    /// The past search being removed (its token) and when: it folds away first.
    forgetting: Option<(String, f64)>,
    /// Mouse-wheel distance still to be scrolled (smooth scrolling).
    wheel_pending: egui::Vec2,
    wheel_mods: egui::Modifiers,

    // now playing
    np_open: bool,
    np_tab: NpTab,
    queue_open: bool,
    fullscreen: bool,
    /// The mini player: the window shrunk to a strip.
    mini: mini::Mini,
    /// Every source's lyrics for the current song (by video ID).
    lyrics: Option<(String, LyricsSet)>,
    /// Romanization/translation for one source's lines: (video ID, source key, language, extras).
    lyrics_extras: Option<(String, String, String, crate::translate::Extras)>,
    /// The extras last asked for, so each is requested once.
    extras_asked: Option<(String, String, String)>,
    show_roman: bool,
    show_trans: bool,
    lyric_prefs: lyric_prefs::Prefs,
    lyric_view: karaoke::View,
    lyric_clock: lyric_prefs::Clock,
    /// Rotation of a "record" cover (radians); advances only while playing.
    record_angle: f64,

    // account / misc
    logged_in: bool,
    /// The Spotify transfer, running on the network runtime.
    transfer: Arc<crate::transfer::Transfer>,
    xfer: transfer::Setup,
    /// Listen together: the room we're in, if any.
    jam: jam::Session,
    signing_in: Arc<AtomicBool>,
    toast: Option<(String, f64)>,

    // updates
    updater: Option<crate::update::Updater>,
    auto_update: bool,
    /// When the last automatic update check ran (app time, s).
    update_checked: Option<f64>,
    update_announced: bool,
    /// Release notes dialog (update ready, or "See all notes").
    notes_dialog: Option<notes::NotesDialog>,
    /// Version whose "Updated to" card is showing, on the first launch after an update.
    whats_new: Option<String>,
    /// The user picked "Restart to update": install on exit, then relaunch.
    restart_for_update: bool,
    /// Settings › Network: the saved proxy setting (`proxy::active()` is this run's),
    /// the mode picked on the page, the custom address being typed, its error, and
    /// the last connection test (`Some(None)` while one runs).
    proxy_saved: crate::proxy::Setting,
    proxy_mode: crate::proxy::Setting,
    proxy_url: String,
    proxy_error: Option<String>,
    proxy_test: Arc<std::sync::Mutex<ProxyTest>>,
    /// Close and start again (the proxy changed).
    restart_app: bool,
    /// The media controls' cover as a local file (video id, `file://` path), fetched
    /// through the proxy: Windows would otherwise download the URL itself, directly.
    media_cover: Arc<std::sync::Mutex<Option<(String, String)>>>,

    // window
    /// Clicking × hides the window to the tray instead of quitting.
    close_to_tray: bool,
    /// Settings › Window: draw with wgpu (Vulkan) from the next start (renderer.rs).
    pub(super) wgpu_renderer: bool,
    /// Settings › App icon (`appicon::ALL` id).
    app_icon: &'static str,
    /// The tray icon, while the window is hidden.
    tray: Option<crate::tray::Tray>,
    /// Closing for real (Quit from the tray, restart to update): don't hide.
    exiting: bool,
    /// Minimized, or hidden in the tray: the window's geometry isn't worth saving.
    window_hidden: bool,
    /// eframe's saved window geometry ("window") from the last time it was visible.
    good_window: Option<String>,
    /// The sidebar is a narrow icon rail.
    sidebar_collapsed: bool,
    actions: Vec<Action>,
    dialog: Option<Dialog>,
    share_pending: Option<String>,
    smoke: Option<String>,
}

/// Longest crossfade offered, in seconds.
pub(super) const CROSSFADE_MAX: u32 = 12;

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
        // Dev hook: YTM_FONT=<font name> tries a font without saving it.
        theme::set_font_choice(&dev_var("YTM_FONT").map(str::to_owned).or_else(|| get("font")).unwrap_or_default());
        let discord_enabled = get("discord_enabled").is_none_or(|v| v == "1");
        let app_icon = crate::appicon::load(backend.data_dir());
        let wgpu_renderer = crate::renderer::load(backend.data_dir()) == crate::renderer::Renderer::Wgpu;
        // Updates reinstall the shortcuts with the classic icon.
        if app_icon != crate::appicon::CLASSIC {
            crate::appicon::sync_shortcuts(app_icon, backend.data_dir());
        }
        crate::downloads::init(backend.data_dir(), &get("download_dir").unwrap_or_default());
        crate::lastfm::init(backend.data_dir());
        let logged_in = backend.is_logged_in();
        // Before any request: a song started at launch (dev hooks) must see this setting.
        backend.set_high_quality(get("high_quality").is_none_or(|v| v == "1"));
        backend.request(Req::Home(None));
        if logged_in {
            backend.request(Req::Library);
            backend.request(Req::Account);
            backend.warm_signed_in();
        }
        let transfer_handle = {
            let b = backend.clone();
            crate::transfer::Transfer::new(backend.data_dir(), Arc::new(move || b.wake()))
        };
        let mut app = Self {
            backend,
            audio,
            ch,
            media,
            discord: discord_enabled.then(crate::discord::Discord::spawn).flatten(),
            discord_enabled,
            discord_sent: None,
            lastfm_user: crate::lastfm::user(),
            lastfm_busy: Default::default(),
            lastfm_result: Default::default(),
            scrobble: None,
            resume_at: None,
            resume_counts: false,
            listened_at: 0,
            queue_checked: None,
            was_focused: false,
            queue_source: None,
            play_pending: None,
            bg,
            page: Page::Home,
            back: Vec::new(),
            forward: Vec::new(),
            page_changed: 0.0,
            loading: false,
            load_failed: false,
            search_text: String::new(),
            suggestions: Default::default(),
            suggest_sel: None,
            suggest_round: 0,
            forgets_pending: 0,
            suggest_due: None,
            home: None,
            home_loading: true,
            explore: None,
            search: None,
            lists: Lists::default(),
            artist: None,
            library: None,
            library_requested: std::time::Instant::now(),
            history: None,
            uploads: None,
            picked_uploads: Default::default(),
            liked: HashSet::new(),
            unliking: HashMap::new(),
            selection: Selection::default(),
            account_name: None,
            account_avatar: None,
            channels: None,
            channels_asked: None,
            switching_to: None,
            sidebar_cache: None,
            queue: Queue::new(match get("repeat").as_deref() {
                Some("all") => Repeat::All,
                Some("one") => Repeat::One,
                _ => Repeat::Off,
            }),
            generation: 0,
            buffering: false,
            skip_streak: 0,
            recoveries: 0,
            autoplay: get("autoplay").is_none_or(|v| v == "1"),
            normalize: get("normalize").is_none_or(|v| v == "1"),
            log_volume: get("log_volume").is_none_or(|v| v == "1"),
            high_quality: get("high_quality").is_none_or(|v| v == "1"),
            play_songs: get("play_songs").is_none_or(|v| v == "1"),
            playing_as: None,
            output_device: get("output_device").filter(|d| !d.is_empty()).map(|id| (id, get("output_device_name").unwrap_or_default())),
            output_devices: Vec::new(),
            updater: None,
            auto_update: get("auto_update").is_none_or(|v| v == "1"),
            close_to_tray: get("close_to_tray").is_some_and(|v| v == "1"),
            wgpu_renderer,
            app_icon,
            tray: None,
            exiting: false,
            window_hidden: false,
            good_window: get("window"),
            sidebar_collapsed: get("sidebar_collapsed").is_some_and(|v| v == "1"),
            update_checked: None,
            update_announced: false,
            notes_dialog: None,
            // First launch of a newer version than last time: show what changed.
            // YTM_WHATS_NEW=1 shows the card for this version regardless.
            whats_new: {
                let current = crate::update::current_version();
                let last = get("last_version");
                (std::env::var_os("YTM_WHATS_NEW").is_some() || last.is_some_and(|v| crate::update::newer(&current, &v))).then_some(current)
            },
            restart_for_update: false,
            proxy_saved: crate::proxy::active().clone(),
            proxy_mode: crate::proxy::active().clone(),
            proxy_url: match crate::proxy::active() {
                crate::proxy::Setting::Custom(u) => u.clone(),
                _ => String::new(),
            },
            proxy_error: None,
            proxy_test: Default::default(),
            restart_app: false,
            media_cover: Default::default(),
            radio_pending: false,
            seek_drag: None,
            art_key: None,
            art_changed: -10.0,
            accent: theme::RED,
            default_theme: get("default_theme").is_some_and(|v| v == "1") && std::env::var_os("YTM_THEME").is_none(),
            // Dev hook: YTM_THEME=<preset id> tries a theme without saving it.
            theme_id: std::env::var("YTM_THEME").ok().or_else(|| get("theme")).unwrap_or_else(|| crate::preset::DEFAULT_ID.to_owned()),
            chrome_applied: None,
            // Dev hook: YTM_APPEARANCE=light|dark|system tries a mode without saving it.
            appearance: theme::Appearance::from_key(&dev_var("YTM_APPEARANCE").map(str::to_owned).or_else(|| get("appearance")).unwrap_or_default()),
            anim_bg: get("anim_bg").is_none_or(|v| v == "1"),
            reactive_bg: get("reactive_bg").is_none_or(|v| v == "1"),
            sponsorblock: get("sponsorblock").is_some_and(|v| v == "1"),
            crossfade: get("crossfade").is_some_and(|v| v == "1"),
            handoff: false,
            crossfade_skips: get("crossfade_skips").is_some_and(|v| v == "1"),
            crossfade_secs: get("crossfade_secs").and_then(|v| v.parse().ok()).unwrap_or(5).clamp(1, CROSSFADE_MAX),
            segments: None,
            segment_skipped: f64::NEG_INFINITY,
            bg_time: 0.0,
            last_frame: 0.0,
            search_rect: None,
            search_focused: false,
            search_refocus: false,
            forget_held: false,
            forgetting: None,
            wheel_pending: egui::Vec2::ZERO,
            wheel_mods: egui::Modifiers::NONE,
            np_open: false,
            np_tab: if get("np_art").is_some_and(|v| v == "1") { NpTab::Art } else { NpTab::Lyrics },
            queue_open: false,
            fullscreen: false,
            mini: mini::Mini::load(get),
            lyrics: None,
            lyrics_extras: None,
            extras_asked: None,
            show_roman: get("show_roman").is_none_or(|v| v == "1"),
            show_trans: get("show_trans").is_none_or(|v| v == "1"),
            lyric_prefs: lyric_prefs::Prefs::load(&get),
            lyric_view: Default::default(),
            lyric_clock: Default::default(),
            record_angle: 0.0,
            logged_in,
            signing_in: Default::default(),
            transfer: transfer_handle,
            xfer: Default::default(),
            jam: Default::default(),
            toast: None,
            actions: Vec::new(),
            dialog: None,
            share_pending: None,
            smoke: None,
        };
        app.request_liked(false);
        // Dev hooks: YTM_SMOKE="query" searches and plays the first song ("id:<videoId>[,<videoId>…]"
        // plays that video); YTM_SEARCH
        // only searches; YTM_PAGE opens a page; YTM_NOWPLAYING opens the overlay.
        if let Some(s) = get(SESSION_KEY).filter(|_| std::env::var_os("YTM_SMOKE").is_none()) {
            app.restore_session(&s);
        }
        let smoke = std::env::var("YTM_SMOKE").ok().filter(|q| !q.is_empty());
        if let Some(ids) = smoke.as_deref().and_then(|q| q.strip_prefix("id:")) {
            // "id:a,b,c" queues several.
            let track = |id: &str| Track { id: id.to_owned(), title: id.to_owned(), artists: vec![], album: None, duration: None, thumbs: vec![], track_nr: None, plays: None, music_video: false };
            app.play_list(ids.split(',').map(track).collect(), 0);
        } else if let Some(q) = smoke {
            app.go(Page::Search(q.clone()));
            app.smoke = Some(q);
        } else if let Some(q) = std::env::var("YTM_SEARCH").ok().filter(|q| !q.is_empty()) {
            app.go(Page::Search(q));
        }
        match std::env::var("YTM_PAGE").as_deref() {
            Ok("home") => app.go(Page::Home),
            Ok("explore") => app.go(Page::Explore),
            Ok("library") => app.go(Page::Library(LibTab::Playlists)),
            Ok("uploads") => app.go(Page::Library(LibTab::Uploads)),
            Ok("liked") => app.go(Page::Library(LibTab::Songs)),
            Ok("history") => app.go(Page::Library(LibTab::History)),
            Ok("settings") => app.go(Page::Settings),
            Ok("transfer") => app.go(Page::Transfer),
            Ok("jam") => app.go(Page::Jam),
            Ok(p) if p.starts_with("album:") => app.go(Page::Album(p[6..].to_owned())),
            Ok(p) if p.starts_with("artist:") => app.go(Page::Artist(p[7..].to_owned())),
            Ok(p) if p.starts_with("playlist:") => app.go(Page::Playlist(p[9..].to_owned())),
            _ => {}
        }
        // YTM_UPLOAD=<file> uploads that file as if it had been picked.
        if let Some(f) = std::env::var_os("YTM_UPLOAD") {
            *app.picked_uploads.lock().unwrap() = Some(vec![f.into()]);
        }
        // Dev hook: YTM_PROXY_INPUT=<address> opens Settings › Network on Custom with it typed in.
        if let Some(v) = dev_var("YTM_PROXY_INPUT") {
            app.proxy_mode = crate::proxy::Setting::Custom(String::new());
            app.proxy_url = v.to_owned();
        }
        if std::env::var("YTM_NOWPLAYING").is_ok() {
            app.np_open = true;
        }
        app.jam.name = get("jam_name").unwrap_or_default();
        // Last launch's name, picture and playlists, so they show at once; the account
        // menu and the library replace them when they arrive.
        if app.logged_in
            && let Some(c) = get("account_cache").and_then(|j| serde_json::from_str::<AccountCache>(&j).ok())
        {
            app.account_name = c.name;
            app.account_avatar = c.avatar;
            app.sidebar_cache = Some(c.playlists).filter(|p| !p.is_empty());
        }
        // Dev hook: YTM_QUEUE opens the queue drawer.
        if std::env::var_os("YTM_QUEUE").is_some() {
            app.queue_open = true;
        }
        jam::dev_hooks(&mut app);
        app.audio.set_normalize(app.normalize);
        app.set_crossfade();
        app
    }

    // ---------------------------------------------------------------- navigation

    fn go(&mut self, page: Page) {
        self.np_open = false;
        if page == self.page {
            // Clicking the page you're on refreshes it.
            self.refresh();
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

    /// Reload the current page's data in place: no page transition, the page stays as
    /// it is until the new data arrives.
    fn refresh(&mut self) {
        let changed = self.page_changed;
        self.page_loaded();
        self.page_changed = changed;
    }

    /// Start the page transition and (re)load the page's data. Every page refreshes on
    /// each visit (things change outside the app: likes, other devices, new releases),
    /// showing the cached copy meanwhile.
    fn page_loaded(&mut self) {
        self.page_changed = -1.0; // stamped with the frame time on the next pass
        // The search box holds the query of the search page you're on, and is empty elsewhere.
        self.search_text = match &self.page {
            Page::Search(q) => q.clone(),
            _ => String::new(),
        };
        self.suggestions = Default::default();
        self.suggest_sel = None;
        // Choosing a transfer match lasts while searching; any other page ends it.
        if !matches!(self.page, Page::Search(_)) {
            self.xfer.picking = None;
        }
        if let Some(id) = self.page_list().map(str::to_owned) {
            self.lists.move_to_front(&id);
        }
        let req = match &self.page.clone() {
            Page::Home if !self.home_loading => {
                self.home_loading = true;
                Some(Req::Home(None))
            }
            Page::Explore => Some(Req::Explore(self.preferred_artist_ids())),
            Page::Search(q) if self.search.as_ref().is_none_or(|s| &s.query != q) => Some(Req::Search(q.clone())),
            Page::Album(id) | Page::Playlist(id) => {
                self.visit_list(id, matches!(self.page, Page::Album(_)));
                None
            }
            Page::Artist(id) => Some(Req::Artist(id.clone())),
            Page::Library(LibTab::History) if self.logged_in => Some(Req::History),
            Page::Library(LibTab::Uploads) if self.logged_in => Some(Req::Uploads),
            Page::Library(LibTab::Songs) => {
                if self.logged_in {
                    self.visit_list(LIKED_MUSIC, false);
                }
                None
            }
            Page::Library(_) if self.logged_in => Some(Req::Library),
            _ => None,
        };
        // The spinner is only for a page with nothing to show yet.
        let cached = match &self.page {
            Page::Home => self.home.is_some(),
            Page::Explore => self.explore.is_some(),
            Page::Album(id) | Page::Playlist(id) => self.opened(id).is_some(),
            Page::Artist(id) => self.artist.as_ref().is_some_and(|a| &a.id == id),
            Page::Library(LibTab::History) => self.history.is_some(),
            Page::Library(LibTab::Uploads) => self.uploads.is_some(),
            Page::Library(LibTab::Songs) => self.opened(LIKED_MUSIC).is_some(),
            Page::Library(_) => self.library.is_some(),
            _ => false,
        };
        let coming = self.page_list().is_some_and(|id| self.lists.loading(id));
        self.loading = (req.is_some() || coming) && !cached;
        self.load_failed = false;
        match req {
            Some(Req::Library) => self.request_library(),
            Some(r) => self.backend.request(r),
            None => {}
        }
    }

    /// The album or playlist the page shows (the Library's liked songs are Liked Music).
    fn page_list(&self) -> Option<&str> {
        match &self.page {
            Page::Album(id) | Page::Playlist(id) => Some(id),
            Page::Library(LibTab::Songs) => Some(LIKED_MUSIC),
            _ => None,
        }
    }

    fn accent_color(&self) -> Color32 {
        let c = if self.default_theme { theme::RED } else { self.look().accent(self.accent) };
        if theme::light() {
            // A pale cover colour would barely show on light pages: deepen it.
            let luma = 0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32;
            if luma > 150.0 {
                return theme::lerp_color(c, Color32::BLACK, ((luma - 150.0) / 105.0 * 0.45).min(0.45));
            }
        }
        c
    }

    /// Whether pages draw light: only the ytm-rs theme has a light mode, and it follows
    /// Settings › Appearance › Mode.
    fn light_pages(&self, ctx: &egui::Context) -> bool {
        !self.default_theme && self.look().id == crate::preset::DEFAULT_ID && self.appearance.is_light(ctx)
    }

    /// The active theme preset; the classic red theme uses the built-in look's layout.
    /// Synced once per frame (`sync_look`), so this is a plain index, not a search.
    fn look(&self) -> &'static crate::preset::Preset {
        crate::preset::active()
    }

    fn sync_look(&self) {
        let id = if self.default_theme { crate::preset::DEFAULT_ID } else { &self.theme_id };
        if crate::preset::active().id != id {
            crate::preset::set_active(id);
        }
    }

    /// Height of the bottom player area (a floating bar leaves a margin around it).
    fn bar_height(&self) -> f32 {
        if self.look().chrome.player_bar == crate::preset::PlayerBar::Floating { 100.0 } else { 86.0 }
    }

    fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), -1.0));
    }


}



impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        dev_pace();
        self.lyric_view.release_hidden(ctx.cumulative_pass_nr());
        let now = ctx.input(|i| i.time);
        self.sync_look();
        self.pump(now);
        self.skip_segments(ctx, now);
        // System media controls and taskbar buttons follow the player however it changed.
        let playing = self.playing() || self.buffering;
        if let Some(m) = &mut self.media {
            m.set_playing(playing);
        }
        self.request_lyric_extras();
        // Upload: files from the picker, or dropped onto the window.
        let mut files = self.picked_uploads.lock().unwrap().take().unwrap_or_default();
        files.extend(ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_owned()).filter(|p| !p.as_os_str().is_empty()).collect::<Vec<_>>()));
        if !files.is_empty() {
            self.upload_files(files);
        }
        transfer::poll(self, ctx, now);
        if crate::deeplink::take_show() {
            self.show_window(ctx);
        }
        for code in crate::deeplink::take() {
            self.show_window(ctx);
            jam::open_link(self, code);
        }
        for a in std::mem::take(&mut self.actions) {
            self.apply(a);
        }
        jam::tick(self);
        jam::dev_script(self, ctx, now);
        if self.page_changed < 0.0 {
            self.page_changed = now;
        }
        // Dev hook: YTM_NP_TOGGLE_AT=<secs>[,<secs>…] clicks the Lyrics chip at those times.
        if let Some(v) = dev_var("YTM_NP_TOGGLE_AT") {
            let times: Vec<f64> = v.split(',').filter_map(|t| t.parse().ok()).collect();
            let id = egui::Id::unique("dev-np-toggles");
            let done: usize = ctx.data(|d| d.get_temp(id)).unwrap_or(0);
            if let Some(&t) = times.get(done) {
                if now >= t {
                    ctx.data_mut(|d| d.insert_temp(id, done + 1));
                    self.np_tab = if self.np_tab == NpTab::Lyrics { NpTab::Art } else { NpTab::Lyrics };
                } else {
                    ctx.request_repaint_after(Duration::from_secs_f64(t - now));
                }
            }
        }
        fit_window_to_monitor(ctx);
        // Dev hook: YTM_MINI=<secs>[,<secs>…] switches between the mini player and the
        // full window at those times (YTM_MINI=1: the mini player after a second).
        if let Some(v) = dev_var("YTM_MINI") {
            let times: Vec<f64> = v.split(',').filter_map(|t| t.parse().ok()).collect();
            let id = egui::Id::unique("dev-mini");
            let done: usize = ctx.data(|d| d.get_temp(id)).unwrap_or(0);
            if let Some(&t) = times.get(done) {
                if now >= t {
                    ctx.data_mut(|d| d.insert_temp(id, done + 1));
                    self.mini.request(done.is_multiple_of(2));
                } else {
                    ctx.request_repaint_after(Duration::from_secs_f64((t - now).min(0.2)));
                }
            }
        }
        if let Some(on) = self.mini.take_request() {
            self.set_mini(ctx, on, now);
        }
        self.mini.tick(ctx, now);
        self.tray_window(ctx);
        self.window_hidden = self.tray.is_some() || ctx.input(|i| i.viewport().minimized == Some(true));
        self.sync_discord();
        self.sync_lastfm();
        self.sync_server_queue(ctx, now);
        self.tick_updates(ctx, now);
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
        // reads as smooth on pages; the now-playing view runs at 30 fps (25 on a page
        // showing the playing row's bars), and the synced
        // lyrics ask for every frame while they animate (the karaoke sweep must be
        // smooth). Each egui frame re-lays-out the UI, so frame rate is the main CPU cost.
        let playing = self.playing();
        if self.window_hidden {
            // In the tray or minimized: nothing to draw, only the player bookkeeping.
            if playing || self.buffering {
                ctx.request_repaint_after(Duration::from_millis(250));
            }
        } else if self.np_open && playing && !self.mini.busy() {
            ctx.request_repaint_after(Duration::from_millis(33));
        } else if playing && widgets::equalizer_shown(ctx) {
            // The playing row's bars need a steady, full rate to read as smooth.
            ctx.request_repaint_after(Duration::from_millis(16));
        } else if playing && self.anim_bg && !self.default_theme && shell::pages_bg_moves(self.look()) {
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

        self.sync_look();
        let look = self.look();
        // Now playing is dark; once it's most of the way up, so is the player bar under it.
        let np = theme::anim_bool(&ctx, "np-open", self.np_open && self.current().is_some(), 0.42);
        let light = self.light_pages(&ctx) && np < 0.5 && !self.fullscreen;
        theme::set_mode(&ctx, light);
        if self.chrome_applied.as_deref() != Some(look.id.as_str()) {
            if self.chrome_applied.is_none() {
                ctx.add_texture_loader(Arc::new(crate::images::Textures::default()));
            }
            theme::apply_chrome(&ctx, look);
            self.chrome_applied = Some(look.id.clone());
        }
        let accent = self.accent_color();
        ctx.data_mut(|d| d.insert_temp(egui::Id::unique("accent"), accent));
        let app_style = &look.app;
        // Once now playing has slid all the way up it hides everything but the player
        // bar, so the pages and the full-screen background under it aren't drawn.
        let covered = !self.fullscreen && np >= 1.0;
        let bar_strip = egui::Rect::from_min_max(egui::pos2(screen.left(), screen.bottom() - self.bar_height()), screen.max);
        titlebar::set_fullscreen(&ctx, self.fullscreen);
        // The mini player, or nothing but the background while the window changes size.
        let layout = self.mini.layout(&ctx);
        if layout != mini::Layout::Full {
            shell::background(self, ui, screen, now, None);
            if layout == mini::Layout::Mini {
                mini::strip(self, ui, now);
                titlebar::resize_sides(&ctx);
                // "Sign in to like songs" and such: shown here too, not timed out unseen.
                shell::toast(self, &ctx, now);
                self.mini.fade_in(&ctx, now, shell::base_color(self));
            }
            dev_screenshot(&ctx, now);
            return;
        }
        shell::background(self, ui, screen, now, covered.then_some(bar_strip));
        if !self.fullscreen && !covered {
            // The top of the window, down to the search box's row: what's on it comes after.
            titlebar::drag_area(ui, egui::Rect::from_min_size(screen.min, egui::vec2(screen.width(), titlebar::HEIGHT)), "main");
        }
        if !self.fullscreen {
            let floating = look.chrome.player_bar == crate::preset::PlayerBar::Floating;
            let bar_alpha = app_style.bar_alpha.clamp(0.0, 1.0);
            let frame = if floating {
                egui::Frame::new().inner_margin(egui::Margin { left: 14, right: 14, top: 0, bottom: 14 })
            } else {
                let mut f = egui::Frame::new().fill(theme::panel(bar_alpha));
                if app_style.bar_border {
                    f = f.stroke(egui::Stroke::new(1.0, theme::glass(0.1)));
                }
                f
            };
            egui::Panel::bottom("player").resizable(false)
                .exact_size(self.bar_height())
                .frame(frame)
                .show_separator_line(false)
                .show(ui, |ui| {
                    if floating {
                        let r = ui.max_rect();
                        let radius = look.chrome.radius.clamp(10.0, 26.0);
                        widgets::soft_shadow(ui, r, radius, 24.0, 0.45);
                        ui.painter().rect_filled(r, radius, theme::panel((bar_alpha + 0.07).min(1.0)));
                        let edge = if app_style.bar_border { 0.14 } else { 0.08 };
                        ui.painter().rect_stroke(r, radius, egui::Stroke::new(1.0, theme::glass(edge)), egui::StrokeKind::Inside);
                        ui.set_clip_rect(r);
                    }
                    shell::player_bar(self, ui, now)
                });
        }
        if !self.fullscreen && !covered {
            use crate::preset::SidebarStyle;
            let side_alpha = app_style.sidebar_alpha.clamp(0.0, 1.0);
            // The logo's row is 32 points tall.
            let side_margin = egui::Margin { left: 14, right: 14, top: titlebar::top_margin(16, 32.0), bottom: 16 };
            let side_frame = match app_style.sidebar {
                SidebarStyle::Glass => egui::Frame::new().fill(theme::panel(side_alpha)).inner_margin(side_margin),
                SidebarStyle::Transparent => egui::Frame::new().inner_margin(side_margin),
                SidebarStyle::Solid => egui::Frame::new().fill(look.solid()).inner_margin(side_margin),
                SidebarStyle::Floating => egui::Frame::new()
                    .fill(theme::panel(side_alpha.max(0.3)))
                    .stroke(egui::Stroke::new(1.0, theme::glass(0.08)))
                    .corner_radius(egui::CornerRadius::same(theme::panel_radius().max(12)))
                    .outer_margin(egui::Margin { left: 10, right: 4, top: 10, bottom: 10 })
                    .inner_margin(egui::Margin::symmetric(12, 14)),
            };
            egui::Panel::left("nav").resizable(false)
                .exact_size(shell::SIDEBAR_W - (shell::SIDEBAR_W - shell::RAIL_W) * shell::sidebar_collapse(self, &ctx))
                .frame(side_frame)
                .show_separator_line(false)
                .show(ui, |ui| shell::sidebar(self, ui));
            let q = theme::anim_bool(&ctx, "queue-drawer", self.queue_open && !self.np_open, 0.25);
            if q > 0.01 {
                egui::Panel::right("queue").resizable(false)
                    .exact_size(340.0 * q)
                    .frame(egui::Frame::new().fill(theme::panel(0.42)).inner_margin(egui::Margin::symmetric(14, 16)))
                    .show_separator_line(false)
                    .show(ui, |ui| shell::queue_drawer(self, ui));
            }
            egui::CentralPanel::default()
                .frame(egui::Frame::new().inner_margin(egui::Margin { left: 28, right: 0, top: titlebar::top_margin(14, 42.0), bottom: 0 }))
                .show(ui, |ui| {
                    shell::top_bar(self, ui);
                    ui.add_space(6.0);
                    pages::page(self, ui, now);
                });
        }
        if np > 0.001 {
            let pages = theme::set_mode(&ctx, false);
            nowplaying::overlay(self, &ctx, now);
            theme::set_mode(&ctx, pages);
        }
        self.draw_dialog(&ctx);
        notes::card(self, &ctx);
        notes::dialog(self, &ctx);
        if let Some(url) = self.share_pending.take() { ctx.copy_text(url); }
        shell::suggestions(self, &ctx);
        shell::toast(self, &ctx, now);

        titlebar::window_buttons(&ctx);
        titlebar::resize_edges(&ctx);
        // Back from the mini player.
        self.mini.fade_in(&ctx, now, shell::base_color(self));

        if dev_var("YTM_THEME_TOUR").is_some() {
            self.theme_tour(&ctx, now);
        } else {
            dev_screenshot(&ctx, now);
        }
    }

    /// Dev hook: `YTM_FAKE_POINTER=x,y` parks a synthetic pointer there, for testing
    /// hover behaviour without touching the real mouse.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        // Dev hook: YTM_FAKE_WHEEL="secs,notches" injects one wheel event, for testing.
        if let Some((at, n)) = dev_var("YTM_FAKE_WHEEL").and_then(|v| {
            let (a, n) = v.split_once(',')?;
            Some((a.parse::<f64>().ok()?, n.parse::<f32>().ok()?))
        }) {
            let fired = egui::Id::unique("dev-wheel-fired");
            let t = raw.time.unwrap_or(0.0);
            if t >= at && !ctx.data(|d| d.get_temp::<bool>(fired).unwrap_or(false)) {
                ctx.data_mut(|d| d.insert_temp(fired, true));
                raw.events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: egui::vec2(0.0, -n),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::NONE,
                    source: egui::MouseWheelSource::Wheel,
                });
            } else if t < at {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
        smooth_wheel(self, ctx, raw);
        if let Some((x, y)) = dev_var("YTM_FAKE_POINTER").and_then(|v| {
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
        let repeat = match self.queue.repeat {
            Repeat::Off => "off",
            Repeat::All => "all",
            Repeat::One => "one",
        };
        storage.set_string("repeat", repeat.into());
        let b = |v: bool| if v { "1" } else { "0" }.to_owned();
        storage.set_string("autoplay", b(self.autoplay));
        storage.set_string("jam_name", self.jam.name.clone());
        let cache = self.logged_in.then(|| AccountCache {
            name: self.account_name.clone(),
            avatar: self.account_avatar.clone(),
            playlists: self.library.as_ref().map(|l| l.playlists.clone()).or_else(|| self.sidebar_cache.clone()).unwrap_or_default(),
        });
        storage.set_string("account_cache", cache.and_then(|c| serde_json::to_string(&c).ok()).unwrap_or_default());
        storage.set_string("normalize", b(self.normalize));
        storage.set_string("log_volume", b(self.log_volume));
        storage.set_string("high_quality", b(self.high_quality));
        storage.set_string("play_songs", b(self.play_songs));
        let folder = crate::downloads::folder();
        storage.set_string("download_dir", if folder == crate::downloads::default_folder() { String::new() } else { folder.display().to_string() });
        let (device, device_name) = self.output_device.clone().unzip();
        storage.set_string("output_device", device.unwrap_or_default());
        storage.set_string("output_device_name", device_name.unwrap_or_default());
        storage.set_string("auto_update", b(self.auto_update));
        // eframe has just saved the window's geometry. Minimized or in the tray that's
        // 0×0 at -32000, which would reopen as a speck: keep the last real one instead.
        if self.window_hidden {
            if let Some(w) = &self.good_window {
                storage.set_string("window", w.clone());
            }
        } else {
            self.good_window = storage.get_string("window");
        }
        // In the mini player, the full window's place is the one to reopen at.
        self.mini.save(storage);
        if let Some(w) = self.mini.full_window() {
            storage.set_string("window", w.clone());
            self.good_window = Some(w);
        }
        storage.set_string("close_to_tray", b(self.close_to_tray));
        storage.set_string("sidebar_collapsed", b(self.sidebar_collapsed));
        // Pretend versions (dev hook) mustn't decide the real "what's new".
        if std::env::var_os("YTM_UPDATE_AS").is_none() {
            storage.set_string("last_version", crate::update::current_version());
        }
        storage.set_string("discord_enabled", b(self.discord_enabled));
        storage.set_string("default_theme", b(self.default_theme));
        if dev_var("YTM_APPEARANCE").is_none() {
            storage.set_string("appearance", self.appearance.key().to_owned());
        }
        if std::env::var_os("YTM_THEME").is_none() {
            storage.set_string("theme", self.theme_id.clone());
        }
        if dev_var("YTM_FONT").is_none() {
            storage.set_string("font", theme::font_choice());
        }
        storage.set_string("anim_bg", b(self.anim_bg));
        storage.set_string("reactive_bg", b(self.reactive_bg));
        storage.set_string("sponsorblock", b(self.sponsorblock));
        storage.set_string("crossfade", b(self.crossfade));
        storage.set_string("crossfade_secs", self.crossfade_secs.to_string());
        storage.set_string("crossfade_skips", b(self.crossfade_skips));
        storage.set_string("show_roman", b(self.show_roman));
        storage.set_string("show_trans", b(self.show_trans));
        // Background test runs mustn't change the listener's sources or per-song picks.
        if std::env::var_os("YTM_BACKGROUND").is_none() || std::env::var_os("YTM_SAVE_SESSION").is_some() {
            self.lyric_prefs.save(storage);
        }
        storage.set_string("np_art", b(self.np_tab == NpTab::Art));
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
        // A downloaded update installs as the app closes (and relaunches it when the
        // user asked to restart).
        let update_ready = self.updater.as_ref().is_some_and(|u| matches!(u.state(), crate::update::State::Ready { .. }));
        if let Some(u) = &self.updater
            && (self.restart_for_update || self.auto_update || (self.restart_app && update_ready))
        {
            u.install(self.restart_for_update || self.restart_app);
        }
        if self.restart_app && !update_ready {
            relaunch();
        }
    }
}

const SESSION_KEY: &str = "session";

/// Start this exe again once this process has saved and exited.
fn relaunch() {
    let Ok(exe) = std::env::current_exe() else { return };
    log::info!("restarting");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        let script = format!("ping -n 3 127.0.0.1 >NUL & start \"\" \"{}\"", exe.display());
        let _ = std::process::Command::new("cmd").arg("/C").raw_arg(&script).creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new(exe).spawn();
    }
}

/// What the sidebar and the account button show, kept for the next launch.
#[derive(serde::Serialize, serde::Deserialize)]
struct AccountCache {
    name: Option<String>,
    avatar: Option<String>,
    playlists: Vec<Card>,
}

/// What's saved between launches so the player picks up where it left off.
#[derive(serde::Serialize, serde::Deserialize)]
struct Session {
    queue: Vec<Track>,
    index: usize,
    position_ms: u64,
    duration_ms: u64,
    #[serde(default)]
    listened_at: u64,
    /// The song version that was playing for the current music video: the position is its.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    playing_as: Option<Track>,
}

/// The song version playing in place of the music video `listed`, in play `generation`.
struct PlayingAs {
    generation: u64,
    listed: String,
    song: Track,
}


/// A restored window size can exceed the monitor (e.g. saved after maximizing),
/// leaving the player bar partly off-screen. Once at startup, maximize instead.
fn fit_window_to_monitor(ctx: &egui::Context) {
    let done = egui::Id::unique("fit-window-checked");
    if ctx.data(|d| d.get_temp::<bool>(done).unwrap_or(false)) {
        return;
    }
    if std::env::var_os("YTM_BACKGROUND").is_some() {
        // eframe re-applies the saved (often maximized) window state regardless of the
        // builder, so push the test window off-screen here, before it's ever shown.
        ctx.data_mut(|d| d.insert_temp(done, true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(-4000.0, 0.0)));
        // YTM_SIZE=WxH tests other window sizes (e.g. the narrow layout).
        let size = std::env::var("YTM_SIZE").ok().and_then(|v| {
            let (w, h) = v.split_once('x')?;
            Some(egui::vec2(w.parse().ok()?, h.parse().ok()?))
        });
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size.unwrap_or(egui::vec2(1920.0, 1057.0))));
        return;
    }
    let (outer, monitor, maximized) = ctx.input(|i| (i.viewport().outer_rect, i.viewport().monitor_size, i.viewport().maximized));
    let (Some(outer), Some(monitor)) = (outer, monitor) else { return };
    ctx.data_mut(|d| d.insert_temp(done, true));
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, monitor);
    // A saved size below the minimum (closed while minimized saves a tiny one, which
    // eframe restores as-is) would open a window too small to see: start over centred.
    let min = egui::Vec2::from(crate::MIN_SIZE);
    if maximized != Some(true) && (outer.width() < min.x || outer.height() < min.y) {
        let size = egui::Vec2::from(crate::DEFAULT_SIZE).min(monitor * 0.9).max(min);
        log::info!("window {outer:?} is below the minimum size; resetting to {size:?}");
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(((monitor - size) / 2.0).max(egui::Vec2::ZERO).to_pos2()));
        return;
    }
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
        source: egui::MouseWheelSource::Wheel,
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

/// Whether this is the installed or the portable app (updates only apply there). It
/// can't change while running, and the check touches the disk, so it runs once.
fn updatable() -> bool {
    static UPDATABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *UPDATABLE.get_or_init(|| crate::update::installed() || crate::update::portable_dir().is_some())
}

pub fn fmt_time(secs: u32) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

#[cfg(test)]
mod session_tests {
    use super::*;

    #[test]
    fn sessions_saved_before_song_versions_still_load() {
        let old = r#"{"queue":[{"id":"a","title":"A","artists":[],"album":null,"duration":null,"thumbs":[],"track_nr":null,"plays":null}],"index":0,"position_ms":5000,"duration_ms":0}"#;
        let s: Session = serde_json::from_str(old).unwrap();
        assert!(!s.queue[0].music_video && s.playing_as.is_none());
        // A song (or a session without a song version) saves as before, so an older
        // version, or a jam guest on one, reads it too.
        let saved = serde_json::to_string(&s).unwrap();
        assert!(!saved.contains("music_video") && !saved.contains("playing_as"), "{saved}");
        let video = Track { music_video: true, ..s.queue[0].clone() };
        let back: Track = serde_json::from_str(&serde_json::to_string(&video).unwrap()).unwrap();
        assert!(back.music_video);
    }
}
