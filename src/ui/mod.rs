//! The application UI: state, playback logic and the per-frame layout.
//!
//! Layout: animated album-art background (shader.rs) → sidebar, top bar and page
//! content → player bar → now-playing overlay (slides up) → toasts.

mod jam;
mod notes;
mod nowplaying;
mod pages;
mod shell;
mod transfer;
mod karaoke;
mod lyric_prefs;
mod widgets;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use crossbeam_channel::Receiver;
use egui::{Color32, Ui};
use souvlaki::MediaControlEvent;

use crate::audio::{AudioCmd, AudioEvent, AudioHandle};
use crate::backend::{self, ArtistPage, Backend, Collection, Explore, Library, Req, Resp, SearchResults};
use crate::lyrics::{Lyrics, LyricsSet};
use crate::media::Media;
use crate::model::{Card, Chip, Shelf, ShelfKind, Target, Track};
use crate::shader::Background;
use crate::theme;

/// File types YouTube Music accepts for upload.
const UPLOAD_EXTS: &[&str] = &["mp3", "m4a", "flac", "ogg", "wma"];

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
pub enum Repeat {
    Off,
    All,
    One,
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

/// An album or playlist page kept for going back to.
pub struct Opened {
    pub c: Collection,
    /// The load it came from; only that load's next pages are added.
    load: u64,
    /// When that load came in.
    at: std::time::Instant,
}

/// An album or playlist load under way (one each at a time). A playlist not kept yet
/// comes in page by page; a kept one is swapped for the new copy once all of it is in.
struct ListLoad {
    load: u64,
    album: bool,
    /// It changed meanwhile, so it's loaded again afterwards.
    stale: bool,
}

/// Going back to a kept album or playlist reloads it when it's older than this. Edits
/// made here show at once anyway, and a Spotify transfer reloads Liked Music when done.
const REFRESH_EVERY: Duration = Duration::from_secs(5 * 60);

/// Liked Music's playlist id.
const LIKED_MUSIC: &str = "LM";

/// How many albums and playlists stay loaded. The page on screen, what's playing, one
/// still loading and Liked Music aren't the ones dropped.
const KEEP_OPENED: usize = 8;

/// A playlist's load whose next pages also go to the queue, and whether that queue
/// is shuffled (they're then mixed into what's still to come). `ran_out`: playback
/// reached the end of what had come, and carries on when more does (or the load ends).
struct QueueFill {
    id: String,
    load: u64,
    shuffled: bool,
    ran_out: bool,
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
    /// The queue is a playlist played while its songs were still coming in: the rest
    /// join the queue as they arrive.
    queue_fill: Option<QueueFill>,
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
    /// Albums and playlists opened lately, most recent first, so going back to one
    /// shows it at once (and refreshes it quietly).
    opened: Vec<Opened>,
    /// Numbers album and playlist loads.
    loads: u64,
    /// Album and playlist loads under way: only their answers are taken.
    running: HashMap<String, ListLoad>,
    artist: Option<ArtistPage>,
    library: Option<Library>,
    /// When the library was last requested; the sidebar spinner gives up after a while.
    library_requested: std::time::Instant,
    history: Option<Vec<Track>>,
    uploads: Option<Vec<Track>>,
    /// Files chosen in the upload dialog, handed over from its thread.
    picked_uploads: Arc<std::sync::Mutex<Option<Vec<std::path::PathBuf>>>>,
    liked: HashSet<String>,
    /// Likes and playlist removals made here lately. YouTube Music goes on listing the old
    /// state for a while after it takes an edit, so lists that arrive are corrected with these.
    edits: RecentEdits,
    selection: Selection,
    account_name: Option<String>,
    account_avatar: Option<String>,
    /// Last launch's playlists, shown in the sidebar until the library arrives.
    sidebar_cache: Option<Vec<Card>>,

    // playback
    queue: Vec<Track>,
    index: Option<usize>,
    generation: u64,
    buffering: bool,
    /// Unplayable songs skipped in a row; a run of them stops playback instead of
    /// racing through the queue.
    skip_streak: u32,
    /// Times the current song was reopened after its download failed.
    recoveries: u32,
    repeat: Repeat,
    autoplay: bool,
    /// Turn loud tracks down to YouTube's loudness target.
    normalize: bool,
    log_volume: bool,
    high_quality: bool,
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
    proxy_test: Arc<std::sync::Mutex<Option<Option<(bool, String)>>>>,
    /// Close and start again (the proxy changed).
    restart_app: bool,
    /// The media controls' cover as a local file (video id, `file://` path), fetched
    /// through the proxy: Windows would otherwise download the URL itself, directly.
    media_cover: Arc<std::sync::Mutex<Option<(String, String)>>>,

    // window
    /// Clicking × hides the window to the tray instead of quitting.
    close_to_tray: bool,
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

/// Unplayable songs skipped in a row before playback stops.
const MAX_SKIPS: u32 = 3;
/// Longest crossfade offered, in seconds.
pub(super) const CROSSFADE_MAX: u32 = 12;

/// When a song failed because of the account (Premium-only, or needs signing in),
/// the message to show instead of skipping: the rest of an album like that fails the
/// same way.
fn account_block(msg: &str, logged_in: bool) -> Option<&'static str> {
    let m = msg.to_ascii_lowercase();
    if m.contains("premium") {
        Some("\"{title}\" needs YouTube Music Premium")
    } else if !logged_in && (m.contains("sign in") || m.contains("age-restricted")) {
        Some("Sign in to play \"{title}\"")
    } else {
        None
    }
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
        // Dev hook: YTM_FONT=<font name> tries a font without saving it.
        theme::set_font_choice(&dev_var("YTM_FONT").map(str::to_owned).or_else(|| get("font")).unwrap_or_default());
        let discord_enabled = get("discord_enabled").is_none_or(|v| v == "1");
        let app_icon = crate::appicon::load(backend.data_dir());
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
            queue_fill: None,
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
            opened: Vec::new(),
            loads: 0,
            running: HashMap::new(),
            artist: None,
            library: None,
            library_requested: std::time::Instant::now(),
            history: None,
            uploads: None,
            picked_uploads: Default::default(),
            liked: HashSet::new(),
            edits: RecentEdits::default(),
            selection: Selection::default(),
            account_name: None,
            account_avatar: None,
            sidebar_cache: None,
            queue: Vec::new(),
            index: None,
            generation: 0,
            buffering: false,
            skip_streak: 0,
            recoveries: 0,
            repeat: match get("repeat").as_deref() {
                Some("all") => Repeat::All,
                Some("one") => Repeat::One,
                _ => Repeat::Off,
            },
            autoplay: get("autoplay").is_none_or(|v| v == "1"),
            normalize: get("normalize").is_none_or(|v| v == "1"),
            log_volume: get("log_volume").is_none_or(|v| v == "1"),
            high_quality: get("high_quality").is_none_or(|v| v == "1"),
            output_device: get("output_device").filter(|d| !d.is_empty()).map(|id| (id, get("output_device_name").unwrap_or_default())),
            output_devices: Vec::new(),
            updater: None,
            auto_update: get("auto_update").is_none_or(|v| v == "1"),
            close_to_tray: get("close_to_tray").is_some_and(|v| v == "1"),
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
            let track = |id: &str| Track { id: id.to_owned(), title: id.to_owned(), artists: vec![], album: None, duration: None, thumbs: vec![], track_nr: None, plays: None };
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
        if let Some(id) = self.page_list()
            && let Some(i) = self.opened.iter().position(|o| o.c.id == id)
        {
            let o = self.opened.remove(i);
            self.opened.insert(0, o);
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
        let coming = self.page_list().is_some_and(|id| self.running.contains_key(id));
        self.loading = (req.is_some() || coming) && !cached;
        self.load_failed = false;
        match req {
            Some(Req::Library) => self.request_library(),
            Some(r) => self.backend.request(r),
            None => {}
        }
    }

    fn next_load(&mut self) -> u64 {
        self.loads += 1;
        self.loads
    }

    /// Load an album or playlist, unless a load of it is under way. `changed`: if one is,
    /// it's loaded once more afterwards.
    fn load_list(&mut self, id: &str, album: bool, changed: bool) {
        if let Some(l) = self.running.get_mut(id) {
            l.stale |= changed;
            return;
        }
        let load = self.next_load();
        self.running.insert(id.to_owned(), ListLoad { load, album, stale: false });
        self.backend.request(match album {
            true => Req::Album { id: id.to_owned(), load },
            false => Req::Playlist { id: id.to_owned(), load, stream: self.opened(id).is_none() },
        });
    }

    /// Opening an album or playlist page: loaded unless a copy from the last few minutes is kept.
    fn visit_list(&mut self, id: &str, album: bool) {
        if self.opened(id).is_none_or(|o| o.at.elapsed() > REFRESH_EVERY) {
            self.load_list(id, album, false);
        }
    }

    fn opened(&self, id: &str) -> Option<&Opened> {
        self.opened.iter().find(|o| o.c.id == id)
    }

    /// More of the kept copy is still coming in, page by page.
    fn receiving(&self, id: &str) -> bool {
        self.running.get(id).is_some_and(|l| self.opened(id).is_some_and(|o| o.load == l.load))
    }

    /// The album or playlist the page shows (the Library's liked songs are Liked Music).
    fn page_list(&self) -> Option<&str> {
        match &self.page {
            Page::Album(id) | Page::Playlist(id) => Some(id),
            Page::Library(LibTab::Songs) => Some(LIKED_MUSIC),
            _ => None,
        }
    }

    /// Liked Music: kept like any playlist, and also the Library's liked songs and the
    /// hearts everywhere. `changed`: load it again even if a load is under way.
    fn request_liked(&mut self, changed: bool) {
        if self.logged_in {
            self.load_list(LIKED_MUSIC, false, changed);
        }
    }

    /// Likes made here show at once, in the hearts and in Liked Music as kept. (A whole
    /// reload asked for just before can still come back without them.)
    fn set_liked(&mut self, tracks: &[Track], like: bool) {
        for t in tracks {
            self.edits.like(&t.id, like, Some(t.clone()));
        }
        let mut lm = self.opened.iter_mut().find(|o| o.c.id == LIKED_MUSIC).map(|o| &mut o.c);
        for t in tracks.iter().rev() {
            if like {
                self.liked.insert(t.id.clone());
            } else {
                self.liked.remove(&t.id);
            }
            let Some(c) = lm.as_deref_mut() else { continue };
            match (like, c.tracks.iter().position(|x| x.id == t.id)) {
                (true, None) => {
                    c.tracks.insert(0, t.clone());
                    c.set_ids.insert(0, None);
                }
                (false, Some(i)) => {
                    c.tracks.remove(i);
                    c.set_ids.remove(i);
                }
                _ => {}
            }
        }
    }

    /// Play a kept album or playlist from the start; one still coming in fills the queue
    /// as the rest arrives.
    fn play_opened(&mut self, id: String, shuffled: bool) {
        let Some(o) = self.opened(&id).filter(|o| !o.c.tracks.is_empty()) else { return };
        let mut tracks = o.c.tracks.clone();
        if shuffled {
            shuffle(&mut tracks);
        }
        let fill = self.receiving(&id).then(|| QueueFill { id: id.clone(), load: o.load, shuffled, ran_out: false });
        self.queue_source = Some(if o.c.is_album { Target::Album(id) } else { Target::Playlist(id) });
        self.play_list_filling(tracks, 0, fill);
    }

    /// The album or playlist on screen.
    fn current_collection(&mut self) -> Option<&mut Collection> {
        let id = self.page_list()?.to_owned();
        self.opened.iter_mut().find(|o| o.c.id == id).map(|o| &mut o.c)
    }

    /// An album or playlist (or a playlist's first page) came in. Only the load under way
    /// counts: one from before a sign-in change or a deletion is dropped.
    fn collection_in(&mut self, load: u64, mut c: Collection, more: bool) {
        if self.running.get(&c.id).is_none_or(|l| l.load != load) {
            return;
        }
        self.edits.correct_collection(&mut c);
        let id = c.id.clone();
        if self.page_list() == Some(id.as_str()) {
            self.loading = false;
        }
        if id == LIKED_MUSIC {
            self.liked = c.tracks.iter().map(|t| t.id.clone()).collect();
        }
        self.keep(load, c);
        if !more {
            self.load_done(&id);
        }
        // Its card's play button was pressed before it was here (in someone else's jam the host plays).
        if let Some((Target::Playlist(p), shuffled)) = &self.play_pending
            && *p == id
        {
            let shuffled = *shuffled;
            self.play_pending = None;
            if !self.jam.guest() {
                self.play_opened(id, shuffled);
            }
        }
    }

    /// A playlist's next page: onto the kept copy, and onto the queue when that was played
    /// before all of it was in. An empty last one also ends a load that failed.
    fn collection_more(&mut self, id: String, load: u64, mut tracks: Vec<Track>, mut set_ids: Vec<Option<String>>, more: bool) {
        if self.running.get(&id).is_none_or(|l| l.load != load) {
            return;
        }
        self.edits.prune();
        let edits = &self.edits;
        (tracks, set_ids) = tracks.into_iter().zip(set_ids).filter(|(t, set)| edits.keeps(&id, t, set.as_deref())).unzip();
        if let Some(o) = self.opened.iter_mut().find(|o| o.c.id == id && o.load == load) {
            // Liked Music has each song once; one liked here meanwhile is at its top already.
            if id == LIKED_MUSIC {
                (tracks, set_ids) = tracks.into_iter().zip(set_ids).filter(|(t, _)| !o.c.tracks.iter().any(|x| x.id == t.id)).unzip();
                self.liked.extend(tracks.iter().map(|t| t.id.clone()));
            }
            o.c.tracks.extend(tracks.iter().cloned());
            o.c.set_ids.extend(set_ids);
        }
        self.fill_queue(&id, load, tracks, more);
        if !more {
            self.load_done(&id);
        }
    }

    /// An album or playlist load ended: load it again if it changed meanwhile. One that
    /// failed before anything came leaves its page with the retry.
    fn load_done(&mut self, id: &str) {
        let Some(l) = self.running.remove(id) else { return };
        self.trim();
        if self.opened(id).is_some() {
            if l.stale {
                self.load_list(id, l.album, false);
            }
        } else {
            // It failed before anything came.
            if matches!(&self.play_pending, Some((Target::Playlist(p), _)) if p == id) {
                self.play_pending = None;
            }
            if self.page_list() == Some(id) {
                self.loading = false;
                self.load_failed = true;
            }
        }
    }

    fn keep(&mut self, load: u64, c: Collection) {
        match self.opened.iter_mut().find(|o| o.c.id == c.id) {
            Some(o) => *o = Opened { c, load, at: std::time::Instant::now() },
            None => self.opened.insert(0, Opened { c, load, at: std::time::Instant::now() }),
        }
        self.trim();
    }

    /// Drop the oldest kept albums and playlists beyond `KEEP_OPENED`.
    fn trim(&mut self) {
        let page = self.page_list().map(str::to_owned);
        let playing = match &self.queue_source {
            Some(Target::Album(id) | Target::Playlist(id)) => Some(id.clone()),
            _ => None,
        };
        let mut i = self.opened.len();
        while self.opened.len() > KEEP_OPENED && i > 0 {
            i -= 1;
            let id = self.opened[i].c.id.as_str();
            if Some(id) != page.as_deref() && Some(id) != playing.as_deref() && id != LIKED_MUSIC && !self.running.contains_key(id) {
                self.opened.remove(i);
            }
        }
    }

    /// The next songs of a playlist played before all of it was in join the queue.
    fn fill_queue(&mut self, id: &str, load: u64, tracks: Vec<Track>, more: bool) {
        let Some(fill) = self.queue_fill.as_mut().filter(|f| f.id == id && f.load == load) else { return };
        let (shuffled, ran_out) = (fill.shuffled, std::mem::take(&mut fill.ran_out));
        if !more {
            self.queue_fill = None;
        }
        if self.queue_source != Some(Target::Playlist(id.to_owned())) {
            return;
        }
        let was_at_end = self.index.is_some_and(|i| i + 1 >= self.queue.len());
        self.queue.extend(tracks);
        if shuffled {
            // Mixed into what's still to come, past the next song (it may be loading already).
            let from = self.index.map_or(0, |i| i + if was_at_end { 1 } else { 2 }).min(self.queue.len());
            shuffle(&mut self.queue[from..]);
        }
        if ran_out {
            self.next();
        } else if was_at_end && !self.buffering {
            self.prefetch_next();
        }
        self.top_up();
    }

    fn request_library(&mut self) {
        self.library_requested = std::time::Instant::now();
        self.backend.request(Req::Library);
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

    /// The lyrics shown for the current song: the song's pick, then the listener's order.
    fn lyric_choice(&self) -> Option<&Lyrics> {
        let (id, set) = self.lyrics.as_ref()?;
        set.get(self.lyric_prefs.choose(id, set)?)
    }

    /// Lyrics have loaded, and no enabled source has any.
    fn no_lyrics(&self) -> bool {
        self.lyrics.is_some() && self.lyric_choice().is_none()
    }

    /// Ask for romanization/translation of the shown source once, when they're switched on
    /// and some line needs one the lyrics don't ship.
    fn request_lyric_extras(&mut self) {
        if !(self.show_roman || self.show_trans) {
            return;
        }
        let Some(l) = self.lyric_choice() else { return };
        let Some((id, _)) = &self.lyrics else { return };
        let target = self.lyric_prefs.target.clone();
        let want = (id.clone(), l.provider.key().to_owned(), target.clone());
        if self.extras_asked.as_ref() == Some(&want) {
            return;
        }
        let needs = l.lines.iter().any(|x| {
            !x.instrumental
                && !x.text.trim().is_empty()
                && ((self.show_roman && x.romanization.is_none() && crate::translate::non_latin(&x.text))
                    || (self.show_trans && lyric_prefs::shipped_translation(x, &target).is_none()))
        });
        let req = needs.then(|| Req::LyricsExtras {
            video_id: id.clone(),
            source: l.provider.key().to_owned(),
            lines: l.lines.iter().map(|x| if x.instrumental { String::new() } else { x.text.clone() }).collect(),
            lang: l.language.clone(),
            target,
        });
        self.extras_asked = Some(want);
        if let Some(r) = req {
            self.backend.request(r);
        }
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
        if self.default_theme { theme::RED } else { self.look().accent(self.accent) }
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
        let track = self.current().filter(|_| self.playing() && !self.buffering);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64;
        let start_ms = now - self.audio.status.position().as_millis() as i64;
        let dur = self.audio.status.duration_ms.load(Ordering::Relaxed) as i64;
        // Position ticks every 100 ms; only a jump of 2 s or more is a real seek.
        let same = match (track, &self.discord_sent) {
            (Some(t), Some(b)) => b.video_id == t.id && (start_ms - b.start_ms).abs() < 2000 && b.end_ms.is_some() == (dur > 0),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        let want = track.map(|t| crate::discord::Presence {
            video_id: t.id.clone(),
            title: t.title.clone(),
            artist: t.artist_line(),
            album: t.album.as_ref().map(|a| a.name.clone()),
            art: backend::art_url(t),
            start_ms,
            end_ms: (dur > 0).then_some(start_ms + dur),
        });
        d.set(want.clone());
        self.discord_sent = want;
    }

    /// Tell Last.fm what's playing, and scrobble it once it has played half its length
    /// or 4 minutes (songs under 30 s don't count). Runs every frame, like Discord's.
    fn sync_lastfm(&mut self) {
        let result = self.lastfm_result.lock().unwrap().take();
        if let Some(r) = result {
            match r {
                Ok(name) => {
                    self.toast(format!("Scrobbling to Last.fm as {name}"));
                    self.lastfm_user = Some(name);
                    self.scrobble = None;
                }
                Err(e) => self.toast(format!("Last.fm sign-in failed: {e}")),
            }
        }
        if self.lastfm_user.is_none() {
            return;
        }
        let Some(t) = self.current().filter(|_| self.playing() && !self.buffering) else { return };
        let pos = self.audio.status.position();
        let dur = Duration::from_millis(self.audio.status.duration_ms.load(Ordering::Relaxed));
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
        let play = crate::lastfm::Scrobble {
            artist: t.artists.first().map(|a| a.name.clone()).unwrap_or_default(),
            track: t.title.clone(),
            album: t.album.as_ref().map(|a| a.name.clone()),
            duration: (!dur.is_zero()).then_some(dur.as_secs() as u32),
            timestamp: now - pos.as_secs() as i64,
        };
        if play.artist.is_empty() {
            return;
        }
        let http = self.backend.http();
        if self.scrobble.is_none_or(|(g, _)| g != self.generation) {
            self.scrobble = Some((self.generation, false));
            let p = play.clone();
            self.backend.spawn(async move { crate::lastfm::now_playing(&http, &p).await });
            return;
        }
        if dur >= Duration::from_secs(30) && pos >= (dur / 2).min(Duration::from_secs(240)) && self.scrobble.is_some_and(|(_, done)| !done) {
            self.scrobble = Some((self.generation, true));
            self.backend.spawn(async move { crate::lastfm::scrobble(&http, play).await });
        }
    }

    fn lastfm_sign_in(&mut self) {
        if self.lastfm_busy.swap(true, Ordering::Relaxed) {
            return;
        }
        let (busy, result, http, backend) = (self.lastfm_busy.clone(), self.lastfm_result.clone(), self.backend.http(), self.backend.clone());
        self.backend.spawn(async move {
            let r = crate::lastfm::sign_in(&http).await.map_err(|e| format!("{e:#}"));
            *result.lock().unwrap() = Some(r);
            busy.store(false, Ordering::Relaxed);
            backend.wake();
        });
    }

    fn lastfm_sign_out(&mut self) {
        crate::lastfm::sign_out();
        self.lastfm_user = None;
        self.scrobble = None;
    }

    fn play_list(&mut self, tracks: Vec<Track>, start: usize) {
        self.play_list_filling(tracks, start, None);
    }

    /// `fill`: the queue is a playlist still coming in, and its other songs follow.
    fn play_list_filling(&mut self, tracks: Vec<Track>, start: usize, fill: Option<QueueFill>) {
        if tracks.is_empty() {
            return;
        }
        // Whatever was about to play doesn't.
        self.queue_fill = fill;
        self.play_pending = None;
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
        self.listened_at = s.listened_at;
        self.request_art();
        self.backend.request(Req::Lyrics(track));
    }

    /// Ask for the queue YouTube Music keeps across devices when the app starts and
    /// whenever its window comes back into focus with nothing playing, so a song
    /// started on the phone is here to carry on with.
    fn sync_server_queue(&mut self, ctx: &egui::Context, now: f64) {
        if self.playing() {
            self.listened_at = unix_now();
        }
        let focused = ctx.input(|i| i.focused);
        let refocused = focused && !std::mem::replace(&mut self.was_focused, focused);
        let idle = !self.playing() && !self.buffering && !self.jam.active() && self.smoke.is_none();
        let due = self.queue_checked.is_none_or(|t| refocused && now - t > 60.0);
        if self.logged_in && idle && due && std::env::var_os("YTM_SMOKE").is_none() {
            self.queue_checked = Some(now);
            self.backend.request(Req::ServerQueue);
        }
    }

    /// Take up what another device was playing, paused at its start like a restored
    /// session, when it played there after anything played here.
    fn adopt_server_queue(&mut self, q: crate::ytm::ServerQueue) {
        let Some(track) = q.tracks.get(q.index).cloned() else { return };
        let idle = !self.playing() && !self.buffering && !self.jam.active();
        // A minute's slack: this app's own plays reach that queue too.
        if !idle || q.watched_at <= self.listened_at + 60 || self.current_id() == Some(track.id.as_str()) {
            return;
        }
        log::info!("picking up {} from another device ({} in its queue)", track.id, q.tracks.len());
        self.queue = q.tracks;
        self.index = Some(q.index);
        self.queue_source = None;
        self.queue_fill = None;
        self.listened_at = q.watched_at;
        self.audio.send(AudioCmd::Stop);
        self.audio.status.duration_ms.store(track.duration.map_or(0, |d| d as u64 * 1000), Ordering::Relaxed);
        self.audio.status.position_ms.store(0, Ordering::Relaxed);
        self.resume_at = Some(Duration::ZERO);
        self.resume_counts = true;
        self.request_art();
        self.backend.request(Req::Lyrics(track.clone()));
        self.toast(format!("Picked up \"{}\" from your other device. Press play to carry on.", track.title));
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
            listened_at: if self.playing() { unix_now() } else { self.listened_at },
        })
        .ok()
    }

    /// The song's download failed partway: pick it up where it stopped from a fresh
    /// stream, rather than letting it end early and skip to the next one. If that keeps
    /// failing, stay on the song, ready to resume from there.
    fn recover(&mut self, at: Duration, reason: &str) {
        const MAX_RECOVERIES: u32 = 3;
        if self.recoveries < MAX_RECOVERIES {
            self.recoveries += 1;
            log::warn!("playback interrupted at {at:?} ({reason}); reopening (try {})", self.recoveries);
            self.play_current_at(at, false);
            return;
        }
        log::warn!("playback interrupted at {at:?} ({reason}); giving up");
        // The player has already run dry; Stop would also reset the position to 0:00.
        self.audio.status.position_ms.store(at.as_millis() as u64, Ordering::Relaxed);
        self.resume_at = Some(at);
        self.buffering = false;
        let title = self.current().map(|t| t.title.clone()).unwrap_or_default();
        self.toast(format!("Lost the connection while playing \"{title}\". Press play to pick up where it stopped."));
    }

    /// Switch the window, tray-to-be and shortcuts to another icon.
    pub(super) fn set_app_icon(&mut self, ctx: &egui::Context, id: &'static str) {
        if id == self.app_icon {
            return;
        }
        self.app_icon = id;
        let v = crate::appicon::get(id);
        crate::appicon::save(self.backend.data_dir(), id);
        if cfg!(windows) {
            crate::appicon::set_window_icon(v);
        } else if let Some(icon) = v.icon_data() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Icon(Some(Arc::new(icon))));
        }
        crate::appicon::sync_shortcuts(id, self.backend.data_dir());
        log::info!("app icon: {id}");
    }

    fn play_current(&mut self) {
        self.recoveries = 0;
        self.play_current_at(Duration::ZERO, true);
    }

    /// Start the current track, opened directly at `at` so nothing plays from 0:00 first.
    /// `report`: a new play for the account's history (not picking up last session's song).
    fn play_current_at(&mut self, at: Duration, report: bool) {
        let Some(track) = self.current().cloned() else { return };
        // Something plays: it no longer waits for a playlist's next songs.
        if let Some(fill) = &mut self.queue_fill {
            fill.ran_out = false;
        }
        self.resume_at = None;
        self.resume_counts = false;
        self.generation += 1;
        self.audio.status.wanted_gen.store(self.generation, Ordering::Relaxed);
        self.audio.status.duration_ms.store(track.duration.map_or(0, |d| d as u64 * 1000), Ordering::Relaxed);
        // A song running into the next crossfades (so does Next, when that's switched
        // on); going back or picking a song cuts over.
        self.audio.send(if self.handoff && at.is_zero() { AudioCmd::Handoff } else { AudioCmd::Stop });
        self.buffering = true;
        self.seek_drag = None;
        self.backend.request(Req::Play { video_id: track.id.clone(), generation: self.generation, at, report });
        if let Some(m) = &mut self.media {
            // Windows fetches a cover URL on its own, through its own proxy setting only.
            let by_url = matches!(crate::proxy::active(), crate::proxy::Setting::System);
            let art = backend::art_url(&track);
            m.set_track(&track.title, &track.artist_line(), track.album.as_ref().map(|a| a.name.as_str()), art.as_deref().filter(|_| by_url), track.duration);
            m.set_playing(true);
            if !by_url && let Some(url) = art {
                self.fetch_media_cover(track.id.clone(), url);
            }
        }
        // Background art and lyrics follow the track.
        self.request_art();
        self.lyrics = None;
        self.lyrics_extras = None;
        self.extras_asked = None;
        self.lyric_view.reset();
        self.backend.request(Req::Lyrics(track.clone()));
        self.segments = None;
        self.request_segments();
        self.top_up();
    }

    /// Keep the queue topped up so autoplay never stalls at the end; a playlist still
    /// coming in goes first.
    fn top_up(&mut self) {
        if self.autoplay && !self.radio_pending && !self.jam.guest() && self.queue_fill.is_none() && self.index.is_some_and(|i| i + 1 >= self.queue.len())
            && let Some(id) = self.current().map(|t| t.id.clone())
        {
            self.radio_pending = true;
            self.backend.request(Req::Radio(id));
        }
    }

    pub(super) fn set_crossfade(&self) {
        self.audio.set_crossfade(if self.crossfade { self.crossfade_secs } else { 0 });
    }

    /// Ask SponsorBlock about the playing song, when that's switched on.
    fn request_segments(&mut self) {
        let Some(t) = self.current().filter(|_| self.sponsorblock && self.segments.is_none()) else { return };
        let (video_id, duration) = (t.id.clone(), t.duration.map(f64::from));
        self.backend.request(Req::Segments { video_id, duration });
    }

    /// Jump over a SponsorBlock segment once playback is inside it; one that runs to the
    /// end finishes the song. A guest in a jam follows the host, who does the skipping.
    fn skip_segments(&mut self, ctx: &egui::Context, now: f64) {
        if !self.sponsorblock || self.jam.guest() || self.resume_at.is_some() || self.buffering || !self.playing() {
            return;
        }
        let Some((id, segs)) = &self.segments else { return };
        // A seek takes a moment to show in the position: don't send it twice.
        if self.current_id() != Some(id.as_str()) || now - self.segment_skipped < 1.5 {
            return;
        }
        let pos = self.audio.status.position().as_secs_f64();
        let Some(seg) = segs.iter().find(|s| pos >= s.start && pos < s.end - 0.3).copied() else {
            // Wake up for the next one even when nothing else repaints.
            if let Some(next) = segs.iter().find(|s| s.start > pos) {
                ctx.request_repaint_after(Duration::from_secs_f64(next.start - pos + 0.05));
            }
            return;
        };
        self.segment_skipped = now;
        let duration = self.audio.status.duration_ms.load(Ordering::Relaxed) as f64 / 1000.0;
        log::info!("sponsorblock: skipping {:.1}–{:.1} s", seg.start, seg.end);
        if duration > 0.0 && seg.end >= duration - 1.0 {
            if self.repeat == Repeat::One {
                self.play_current();
            } else {
                self.next();
            }
        } else {
            self.audio.send(AudioCmd::Seek(Duration::from_secs_f64(seg.end)));
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
        if self.jam.guest() {
            return jam::request(self, crate::jam::Cmd::Skip);
        }
        let Some(i) = self.index else { return };
        if i + 1 < self.queue.len() {
            self.index = Some(i + 1);
        } else if let Some(fill) = &mut self.queue_fill {
            fill.ran_out = true;
            return;
        } else if self.repeat == Repeat::All && !self.queue.is_empty() {
            self.index = Some(0);
        } else {
            return;
        }
        let ending = self.handoff;
        self.handoff = ending || self.crossfade_skips;
        self.play_current();
        self.handoff = ending;
    }

    fn prev(&mut self) {
        if self.jam.guest() {
            return jam::request(self, crate::jam::Cmd::Prev);
        }
        if self.audio.status.position() > Duration::from_secs(3) {
            self.seek_to(Duration::ZERO);
            return;
        }
        if let Some(i) = self.index.filter(|&i| i > 0) {
            self.index = Some(i - 1);
            self.play_current();
        }
    }

    fn seek_to(&mut self, at: Duration) {
        if self.resume_at.is_some() {
            // Restored but not loaded yet (nothing for the player to seek): move the start point instead.
            self.resume_at = Some(at);
            self.audio.status.position_ms.store(at.as_millis() as u64, Ordering::Relaxed);
        } else {
            self.audio.send(AudioCmd::Seek(at));
        }
    }

    fn toggle(&mut self) {
        if self.index.is_none() {
            return;
        }
        if self.jam.guest() {
            return jam::request(self, crate::jam::Cmd::Toggle);
        }
        if let Some(at) = self.resume_at {
            // Last session's song, already counted when it started then (unless it was
            // another device's).
            let report = std::mem::take(&mut self.resume_counts);
            self.play_current_at(at, report);
            return;
        }
        let playing = self.playing();
        // The media controls catch up in `logic` once the player has switched.
        self.audio.send(if playing { AudioCmd::Pause } else { AudioCmd::Resume });
    }

    fn shuffle_upcoming(&mut self) {
        if self.jam.guest() {
            return self.toast("Only the host can shuffle the jam");
        }
        let start = self.index.map_or(0, |i| i + 1);
        if start < self.queue.len() {
            shuffle(&mut self.queue[start..]);
            if let Some(fill) = &mut self.queue_fill {
                fill.shuffled = true;
            }
            self.toast("Shuffled up next");
            self.prefetch_next();
        }
    }

    /// Automatic update checks: shortly after launch, then every six hours.
    fn tick_updates(&mut self, ctx: &egui::Context, now: f64) {
        use crate::update::{State, Updater};
        if self.updater.is_none() {
            let c = ctx.clone();
            self.updater = Some(Updater::new(self.backend.data_dir(), move || c.request_repaint()));
        }
        let Some(u) = self.updater.clone() else { return };
        let due = match self.update_checked {
            None => now > 8.0,
            Some(t) => now - t > 6.0 * 3600.0 && !matches!(u.state(), State::Ready { .. }),
        };
        let enabled = self.auto_update && updatable() && dev_var("YTM_BACKGROUND").is_none_or(|_| dev_var("YTM_UPDATE_FORCE").is_some() || dev_var("YTM_UPDATE_FEED").is_some());
        if enabled && due {
            self.update_checked = Some(now);
            u.check();
        } else if enabled {
            // An idle app doesn't repaint on its own: wake up when the next check is due.
            let next = self.update_checked.map_or(8.0, |t| t + 6.0 * 3600.0);
            ctx.request_repaint_after(Duration::from_secs_f64((next - now).max(0.5) + 0.1));
        }
        if let State::Ready { version, .. } = u.state()
            && !self.update_announced
        {
            self.update_announced = true;
            self.notes_dialog = Some(notes::NotesDialog { version, ready: true });
        }
    }

    /// Close the app so the downloaded update installs, then relaunch it.
    fn restart_to_update(&mut self, ctx: &egui::Context) {
        self.restart_for_update = true;
        self.quit(ctx);
    }

    /// Close and start again, e.g. so a new proxy setting takes effect.
    fn restart(&mut self, ctx: &egui::Context) {
        self.restart_app = true;
        self.quit(ctx);
    }

    /// Close the app for real, even with "Close to tray" on.
    fn quit(&mut self, ctx: &egui::Context) {
        self.exiting = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// "Close to tray": × hides the window behind a tray icon; the icon brings it
    /// back or quits. eframe keeps running `logic` while the window is hidden.
    fn tray_window(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && self.close_to_tray && !self.exiting {
            let c = ctx.clone();
            match crate::tray::Tray::new(crate::appicon::get(self.app_icon), move || c.request_repaint()) {
                Ok(t) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    self.tray = Some(t);
                }
                // Without an icon there'd be no way back: just close.
                Err(e) => log::warn!("tray icon unavailable: {e:#}"),
            }
        }
        if self.tray.is_none() {
            return;
        }
        // The first click decides; the icon goes away with it.
        match crate::tray::take().into_iter().next() {
            Some(crate::tray::Cmd::Show) => self.show_window(ctx),
            Some(crate::tray::Cmd::Quit) => {
                self.tray = None;
                self.quit(ctx);
            }
            None => {}
        }
    }

    /// Bring the window back (from the tray, or minimized) and to the front.
    fn show_window(&mut self, ctx: &egui::Context) {
        self.tray = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), -1.0));
    }

    fn apply(&mut self, a: Action) {
        // In someone else's jam, playback changes become requests to the host.
        let a = if self.jam.guest() {
            match jam::guest_action(self, a) {
                Some(a) => a,
                None => return,
            }
        } else {
            a
        };
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
                    Page::Library(LibTab::Songs) => Some(Target::Playlist(LIKED_MUSIC.into())),
                    Page::Artist(id) => Some(Target::Artist(id.clone())),
                    _ => None,
                };
                // All of a playlist that's still coming in, in order or shuffled: the rest follows.
                let fill = self.page_list().filter(|id| self.receiving(id)).and_then(|id| {
                    let o = self.opened(id).filter(|o| o.c.tracks.len() == tracks.len())?;
                    Some(QueueFill { id: id.to_owned(), load: o.load, shuffled: o.c.tracks.iter().zip(&tracks).any(|(a, b)| a.id != b.id), ran_out: false })
                });
                self.play_list_filling(tracks, i, fill);
            }
            Action::TogglePlay => self.toggle(),
            Action::TransferPick(t) => transfer::use_pick(self, t),
            // Kept already: at once. A playlist that isn't starts with its first page.
            Action::PlayCollection(Target::Album(id) | Target::Playlist(id), shuffle) if self.opened(&id).is_some() => self.play_opened(id, shuffle),
            Action::PlayCollection(target, shuffle) => {
                self.toast("Starting playback…");
                self.play_pending = Some((target.clone(), shuffle));
                match target {
                    Target::Playlist(id) => self.load_list(&id, false, false),
                    target => self.backend.request(Req::PlayCollection { target }),
                }
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
                self.play_pending = None;
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
                self.seek_to(Duration::from_secs_f32(secs.max(0.0)));
                self.lyric_view.resume();
            }
            Action::Like(t, like) => {
                if !self.logged_in {
                    self.toast("Sign in to like songs");
                    return;
                }
                // Optimistic: update the like state and the cached Liked songs list now.
                self.set_liked(std::slice::from_ref(&t), like);
                self.backend.request(Req::Like { video_id: t.id, like });
            }
            Action::LikeMany(tracks, like) => {
                if !self.logged_in {
                    self.toast("Sign in to like songs");
                    return;
                }
                let tracks: Vec<Track> = tracks.into_iter().filter(|t| self.liked.contains(&t.id) != like).collect();
                if tracks.is_empty() { return; }
                self.set_liked(&tracks, like);
                self.backend.request(Req::LikeMany { video_ids: tracks.into_iter().map(|t| t.id).collect(), like });
            }
            Action::Dislike(t) => {
                if !self.logged_in { self.toast("Sign in to rate songs"); return; }
                self.set_liked(std::slice::from_ref(&t), false);
                self.backend.request(Req::Dislike(t.id));
            }
            Action::Share(target) => {
                self.share_pending = Some(share_url(&target));
                self.toast("Link copied");
            }
            Action::Download(track) => {
                self.toast(format!("Downloading \u{201c}{}\u{201d}", track.title));
                self.backend.request(Req::Download(track));
            }
            Action::AddToPlaylist(tracks) => {
                if self.logged_in { self.dialog = Some(Dialog::PickPlaylist(tracks)); }
                else { self.toast("Sign in to edit playlists"); }
            }
            Action::RemoveFromPlaylist(mut rows) => {
                let Some(c) = self.current_collection().filter(|c| c.owned && !c.is_album) else { return };
                rows.sort_unstable();
                rows.dedup();
                rows.retain(|&i| c.set_ids.get(i).is_some_and(Option::is_some));
                if rows.is_empty() {
                    self.toast("YouTube Music didn't say where these songs are in the playlist. Reload it and try again.");
                    return;
                }
                let items: Vec<(String, String)> = rows.iter().map(|&i| (c.tracks[i].id.clone(), c.set_ids[i].clone().unwrap_or_default())).collect();
                let playlist = c.id.clone();
                // Optimistic: drop the rows now; the playlist reloads once YouTube confirms.
                for &i in rows.iter().rev() {
                    c.tracks.remove(i);
                    c.set_ids.remove(i);
                }
                self.edits.remove(&playlist, items.iter().map(|(_, set)| set.clone()));
                self.selection.clear();
                self.backend.request(Req::RemoveFromPlaylist { playlist_id: playlist, items });
            }
            Action::NewPlaylist(tracks) => {
                if self.logged_in { self.dialog = Some(Dialog::NewPlaylist { title: String::new(), tracks }); }
                else { self.toast("Sign in to create playlists"); }
            }
            Action::EditPlaylist => {
                if let Some(c) = self.current_collection().filter(|c| !c.is_album) {
                    self.dialog = Some(Dialog::EditPlaylist {
                        id: c.id.clone(), title: c.title.clone(), description: c.description.clone().unwrap_or_default(), privacy: None,
                    });
                }
            }
            Action::Collaborate => {
                if let Some(id) = self.current_collection().filter(|c| !c.is_album && c.owned).map(|c| c.id.clone()) {
                    self.backend.request(Req::Collaboration(id.clone()));
                    self.dialog = Some(Dialog::Collaborate { id, info: None, busy: true, confirm_off: false });
                }
            }
            Action::DeletePlaylist => {
                if let Some(c) = self.current_collection().filter(|c| !c.is_album) {
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

    /// Download the media controls' cover through our own client, into a file.
    fn fetch_media_cover(&self, video_id: String, url: String) {
        let (slot, b) = (self.media_cover.clone(), self.backend.clone());
        let dir = self.backend.data_dir().join("media-cover");
        self.backend.spawn(async move {
            let res = async {
                let bytes = b.http().get(&url).send().await?.error_for_status()?.bytes().await?;
                // A new name per song, so Windows doesn't show a cached older cover.
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir)?;
                let path = dir.join(format!("{video_id}.jpg"));
                std::fs::write(&path, &bytes)?;
                anyhow::Ok(path)
            }
            .await;
            match res {
                Ok(path) => {
                    *slot.lock().unwrap() = Some((video_id, format!("file://{}", path.display())));
                    b.wake();
                }
                Err(e) => log::debug!("media cover: {e:#}"),
            }
        });
    }

    fn pump(&mut self, now: f64) {
        let cover = self.media_cover.lock().unwrap().take();
        if let Some((id, file)) = cover
            && let Some(track) = self.current().filter(|t| t.id == id).cloned()
            && let Some(m) = &mut self.media
        {
            m.set_track(&track.title, &track.artist_line(), track.album.as_ref().map(|a| a.name.as_str()), Some(&file), track.duration);
        }
        while let Ok(r) = self.ch.resp_rx.try_recv() {
            self.on_resp(r, now);
        }
        while let Ok(ev) = self.ch.audio_rx.try_recv() {
            match ev {
                AudioEvent::Finished { generation } if generation == self.generation && self.jam.guest() => {}
                AudioEvent::Finished { generation } if generation == self.generation => {
                    if self.repeat == Repeat::One {
                        self.play_current();
                    } else {
                        self.next();
                    }
                }
                AudioEvent::Finished { .. } => {}
                // Crossfade into the next song; repeat-one and the end of the queue wait for the end.
                AudioEvent::Ending { generation } if generation == self.generation && !self.jam.guest() && self.repeat != Repeat::One => {
                    self.handoff = true;
                    self.next();
                    self.handoff = false;
                }
                AudioEvent::Ending { .. } => {}
                AudioEvent::Interrupted { generation, at, reason } if generation == self.generation => self.recover(at, &reason),
                AudioEvent::Interrupted { .. } => {}
                AudioEvent::Error(e) => self.toast(e),
            }
        }
        while let Ok(ev) = self.ch.media_rx.try_recv() {
            match ev {
                MediaControlEvent::Play | MediaControlEvent::Pause if self.jam.guest() => self.toggle(),
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
            self.skip_streak = 0;
            self.prefetch_next();
            // Dev hook: YTM_SEEK=<secs> jumps into the first track once it starts.
            if self.generation == 1 {
                if let Some(s) = std::env::var("YTM_SEEK").ok().and_then(|v| v.parse::<f32>().ok()) {
                    self.audio.send(AudioCmd::Seek(Duration::from_secs_f32(s)));
                }
                // YTM_SEEK_LATER=<secs>@<delay> seeks again that many seconds later.
                if let Some((s, d)) = dev_var("YTM_SEEK_LATER").and_then(|v| v.split_once('@')).and_then(|(s, d)| Some((s.parse::<f32>().ok()?, d.parse::<f32>().ok()?))) {
                    let audio = self.audio.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_secs_f32(d));
                        audio.send(AudioCmd::Seek(Duration::from_secs_f32(s)));
                    });
                }
            }
        }
        if self.suggest_due.is_some_and(|d| now >= d) {
            self.suggest_due = None;
            let q = self.search_text.trim().to_owned();
            // An empty box signed in shows the account's recent searches.
            if !q.is_empty() || self.logged_in {
                self.backend.request(Req::Suggest { query: q, round: self.suggest_round });
            }
        }
    }

    fn on_resp(&mut self, r: Resp, now: f64) {
        match r {
            Resp::Home(page) => {
                // Until the account menu answers (Req::Account).
                if self.account_avatar.is_none() {
                    self.account_avatar = page.shelves.iter().find(|s| s.title == "Listen again").and_then(|s| s.strap_thumb.clone());
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
                    loading_more: None,
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
                    h.loading_more = None;
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
            Resp::SearchForgotten => {
                // Replies dropped meanwhile left the list behind what's typed: ask again.
                self.forgets_pending = self.forgets_pending.saturating_sub(1);
                if self.forgets_pending == 0 {
                    self.suggest_due = Some(now);
                }
            }
            Resp::Suggest { query, round, terms } => {
                if round == self.suggest_round && self.search_text.trim() == query {
                    self.suggestions = (query, terms);
                    self.suggest_sel = None;
                }
            }
            Resp::Collection { load, c, more } => self.collection_in(load, c, more),
            Resp::CollectionMore { id, load, tracks, set_ids, more } => self.collection_more(id, load, tracks, set_ids, more),
            Resp::Artist(a) => {
                // A slow answer for an artist left meanwhile doesn't replace the one on screen.
                if matches!(&self.page, Page::Artist(id) if *id != a.id) {
                    return;
                }
                if self.page == Page::Artist(a.id.clone()) {
                    self.loading = false;
                }
                self.artist = Some(a);
            }
            Resp::Library(l) => {
                self.library = Some(l);
                self.loading = false;
            }
            Resp::History(h) => {
                self.history = Some(h);
                self.loading = false;
            }
            Resp::Uploads(t) => {
                self.uploads = Some(t);
                if self.page == Page::Library(LibTab::Uploads) {
                    self.loading = false;
                }
            }
            Resp::Downloaded { title, path } => {
                log::info!("downloaded to {}", path.display());
                self.toast(format!("Downloaded \u{201c}{title}\u{201d}"));
            }
            Resp::Uploaded { done, failed } => {
                let songs = |n: usize| if n == 1 { "1 song".to_owned() } else { format!("{n} songs") };
                self.toast(match (done, failed.len()) {
                    (_, 0) => format!("Uploaded {}. It can take a minute for YouTube Music to process them.", songs(done)),
                    (0, _) => format!("Upload failed: {}", failed.join(", ")),
                    _ => format!("Uploaded {}; failed: {}", songs(done), failed.join(", ")),
                });
            }
            Resp::Lyrics { video_id, lyrics } => {
                if self.current_id() == Some(video_id.as_str()) {
                    let available = self.lyric_prefs.available(&lyrics);
                    let shown = self.lyric_prefs.choose(&video_id, &lyrics);
                    let missing: Vec<&str> = self.lyric_prefs.order.iter().filter(|(p, on)| *on && !available.contains(p)).map(|(p, _)| p.key()).collect();
                    log::info!(
                        "lyrics: showing {} ({}/{}); available: {}; no lyrics from: {}",
                        shown.map_or("nothing", |p| p.key()),
                        shown.and_then(|p| available.iter().position(|x| *x == p)).map_or(0, |i| i + 1),
                        available.len(),
                        available.iter().map(|p| p.key()).collect::<Vec<_>>().join(", "),
                        missing.join(", ")
                    );
                    self.lyrics = Some((video_id, lyrics));
                }
            }
            Resp::Segments { video_id, segments } => {
                if self.current_id() == Some(video_id.as_str()) {
                    log::info!("sponsorblock: {} segment(s) for {video_id}", segments.len());
                    self.segments = Some((video_id, segments));
                }
            }
            Resp::LyricsExtras { video_id, source, target, extras } => {
                if self.current_id() == Some(video_id.as_str()) {
                    self.lyrics_extras = Some((video_id, source, target, extras));
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
            // Unless something else was asked to play meanwhile; shuffled as last asked.
            Resp::PlayQueue { target, mut tracks } => {
                if let Some((p, shuffled)) = &self.play_pending
                    && *p == target
                {
                    if *shuffled {
                        shuffle(&mut tracks);
                    }
                    self.queue_source = Some(target);
                    self.play_list(tracks, 0);
                }
            }
            Resp::Liked { video_id, like } => {
                self.edits.confirm_likes(std::slice::from_ref(&video_id), &[]);
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
            Resp::LikedMany { video_ids, failed, like } => {
                self.edits.confirm_likes(&video_ids, &failed);
                // The refused ones come back as they were with a reload.
                if !failed.is_empty() {
                    self.request_liked(true);
                }
                let count = video_ids.len();
                let mut msg = format!("{} {count} {}", if like { "Liked" } else { "Removed like from" }, if count == 1 { "song" } else { "songs" });
                if !failed.is_empty() {
                    msg += &format!("; YouTube Music refused {}", failed.len());
                }
                self.toast(msg);
            }
            Resp::PlaylistCreated { id, songs } => {
                self.request_library();
                self.toast(match songs {
                    0 => "Playlist created".to_owned(),
                    1 => "Song added to new playlist".to_owned(),
                    n => format!("{n} songs added to new playlist"),
                });
                self.go(Page::Playlist(id));
            }
            Resp::PlaylistChanged { id, message } => {
                self.request_library();
                if message == "Playlist deleted" {
                    // Its load is dropped too, and the queue stops waiting for it.
                    self.opened.retain(|o| o.c.id != id);
                    self.running.remove(&id);
                    if self.queue_fill.as_ref().is_some_and(|f| f.id == id) {
                        self.queue_fill = None;
                    }
                    let gone = Page::Playlist(id);
                    if self.page == gone {
                        self.go(Page::Library(LibTab::Playlists));
                    }
                    // Back and Forward skip it.
                    self.back.retain(|p| *p != gone);
                    self.back.dedup();
                    self.forward.retain(|p| *p != gone);
                    self.forward.dedup();
                } else {
                    if message == "Removed from playlist" {
                        self.edits.confirm_removal(&id);
                    }
                    // Kept for going back (or playing): reload it. What YouTube Music still
                    // lists after a removal is dropped as it arrives.
                    if self.opened(&id).is_some() || self.running.contains_key(&id) {
                        self.load_list(&id, false, true);
                    }
                }
                self.toast(message);
            }
            Resp::Collaboration(c) => {
                if let Some(Dialog::Collaborate { id, info, busy, .. }) = &mut self.dialog
                    && *id == c.playlist_id
                {
                    *info = Some(c);
                    *busy = false;
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
                    // Skipping can't help when the account is the problem: the rest of
                    // an album like this needs it too. Stop on the song instead.
                    if let Some(why) = account_block(&reason, self.logged_in) {
                        self.skip_streak = 0;
                        self.toast(why.replace("{title}", &title));
                    } else if has_next && self.skip_streak < MAX_SKIPS {
                        // Don't stall the queue on one unplayable song.
                        self.skip_streak += 1;
                        self.toast(format!("Skipped \"{title}\": {reason}"));
                        self.next();
                    } else {
                        self.skip_streak = 0;
                        self.toast(format!("Can't play \"{title}\": {reason}"));
                    }
                }
            }
            Resp::ServerQueue(q) => self.adopt_server_queue(q),
            Resp::Account { name, photo } => {
                self.account_name = name.or(self.account_name.take());
                self.account_avatar = photo.or(self.account_avatar.take());
            }
            Resp::LoggedIn(v) => {
                self.logged_in = v;
                // Recent searches belong to the account.
                self.suggestions = Default::default();
                self.suggest_sel = None;
                self.forgetting = None;
                self.suggest_round += 1;
                self.forgets_pending = 0;
                self.suggest_due = Some(now);
                self.account_name = None;
                self.account_avatar = None;
                self.sidebar_cache = None;
                self.library = None;
                self.history = None;
                self.uploads = None;
                // Your own playlists show differently (and private ones not at all) to another account.
                self.opened.clear();
                self.running.clear();
                self.queue_fill = None;
                self.play_pending = None;
                if self.page_list().is_some() {
                    self.refresh();
                }
                self.home = None;
                self.home_loading = true;
                self.backend.request(Req::Home(None));
                if v {
                    self.request_library();
                    self.request_liked(false);
                    self.backend.request(Req::Account);
                } else {
                    self.liked.clear();
                }
                self.toast(if v { "Signed in" } else { "Signed out" });
            }
            Resp::Error(e) => {
                if let Some(Dialog::Collaborate { info, busy, .. }) = &mut self.dialog {
                    *busy = false;
                    // Nothing to show if it never loaded.
                    if info.is_none() {
                        self.dialog = None;
                    }
                }
                // A page still waiting for its first data shows a retry instead of loading forever.
                self.load_failed |= self.loading || (self.page == Page::Home && self.home_loading);
                self.loading = false;
                self.home_loading = false;
                self.radio_pending = false;
                if let Some(h) = &mut self.home {
                    h.loading_more = None;
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
        // Windows: in one step, without winit's unthemed-frame flash.
        if !crate::fullscreen::set(on) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
        }
        ctx.request_repaint();
    }

    /// Open the system file picker for songs to upload; it runs on its own thread.
    fn pick_uploads(&self, ctx: &egui::Context) {
        let (slot, ctx) = (self.picked_uploads.clone(), ctx.clone());
        std::thread::spawn(move || {
            let files = rfd::FileDialog::new().set_title("Upload music").add_filter("Audio", UPLOAD_EXTS).pick_files();
            if let Some(files) = files {
                *slot.lock().unwrap() = Some(files);
                ctx.request_repaint();
            }
        });
    }

    fn upload_files(&mut self, files: Vec<std::path::PathBuf>) {
        let (ok, skipped): (Vec<_>, Vec<_>) = files.into_iter().partition(|f| {
            f.extension().and_then(|e| e.to_str()).is_some_and(|e| UPLOAD_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        });
        if !self.logged_in {
            self.toast("Sign in to upload music");
        } else if ok.is_empty() {
            if !skipped.is_empty() {
                self.toast("YouTube Music only takes MP3, M4A, FLAC, OGG and WMA files");
            }
        } else {
            self.toast(if ok.len() == 1 { "Uploading 1 song…".to_owned() } else { format!("Uploading {} songs…", ok.len()) });
            self.backend.request(Req::Upload(ok));
        }
    }

    fn start_spotify_sign_in(&self, ctx: &egui::Context) {
        let xfer = self.transfer.clone();
        if xfer.signing_in.swap(true, Ordering::Relaxed) {
            return;
        }
        let (backend, ctx) = (self.backend.clone(), ctx.clone());
        crate::login::spawn(crate::login::Site::Spotify, xfer.spotify_profile(), move |outcome| {
            match outcome {
                crate::login::Outcome::Cookie(c) => xfer.set_cookie(Some(c), &backend),
                crate::login::Outcome::Cancelled => {}
                crate::login::Outcome::Failed(e) => backend.report_error(format!("Spotify sign-in window failed: {e}")),
            }
            xfer.signing_in.store(false, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }

    fn start_sign_in(&self, ctx: &egui::Context) {
        if self.signing_in.swap(true, Ordering::Relaxed) {
            return;
        }
        let (backend, busy, ctx) = (self.backend.clone(), self.signing_in.clone(), ctx.clone());
        crate::login::spawn(crate::login::Site::Google, self.backend.login_profile(), move |outcome| {
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
        if let Some(Dialog::ConfirmDelete { id, title }) = &self.dialog {
            let (id, title) = (id.clone(), title.clone());
            match confirm_delete(ctx, &title, self.accent_color()) {
                Some(true) => {
                    self.backend.request(Req::DeletePlaylist(id));
                    self.dialog = None;
                }
                Some(false) => self.dialog = None,
                None => {}
            }
            return;
        }
        let Some(mut dialog) = self.dialog.take() else { return };
        let accent = self.accent_color();
        let mut done = false;
        let heading = match &dialog {
            Dialog::PickPlaylist(_) => "Add to playlist",
            Dialog::NewPlaylist { .. } => "New playlist",
            Dialog::EditPlaylist { .. } => "Edit playlist",
            Dialog::ConfirmDelete { .. } => "Delete playlist",
            Dialog::Collaborate { .. } => "Collaborate",
        };
        // The same modal card as "Delete playlist?" and the update dialog.
        let modal = egui::Modal::new(egui::Id::unique("playlist-dialog")).frame(notes::surface()).backdrop_color(theme::shade(0.55)).show(ctx, |ui| {
            ui.set_width(440.0_f32.min(ctx.content_rect().width() - 48.0));
            egui::Frame::new().inner_margin(egui::Margin { left: 26, right: 26, top: 24, bottom: 22 }).show(ui, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                ui.label(egui::RichText::new(heading).font(theme::bold(22.0)).color(theme::TEXT));
                ui.add_space(18.0);
                match &mut dialog {
                    Dialog::PickPlaylist(tracks) => {
                        if dialog_row(ui, None, Some(theme::icon::LIST_PLUS), "New playlist") {
                            self.actions.push(Action::NewPlaylist(tracks.clone()));
                            done = true;
                        }
                        ui.add_space(6.0);
                        pages::divider(ui);
                        ui.add_space(6.0);
                        let cards = self.library.as_ref().map(|l| l.playlists.clone()).unwrap_or_default();
                        egui::ScrollArea::vertical().max_height(340.0).auto_shrink([false, true]).show(ui, |ui| {
                            for card in cards {
                                if let Target::Playlist(id) = card.target {
                                    if !id.starts_with("PL") { continue; }
                                    if dialog_row(ui, Some(&card.thumbs), None, &card.title) {
                                        let video_ids = tracks.iter().map(|t| t.id.clone()).collect();
                                        self.backend.request(Req::AddToPlaylist { playlist_id: id, video_ids });
                                        done = true;
                                    }
                                }
                            }
                        });
                    }
                    Dialog::NewPlaylist { title, tracks } => {
                        let name = text_field(ui, "Name", title, false, "My playlist");
                        if ui.memory(|m| m.focused().is_none()) { name.request_focus(); }
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("Private · only you can see it").font(theme::regular(12.5)).color(theme::TEXT_FAINT));
                        ui.add_space(24.0);
                        let enter = name.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let (ok, cancel) = dialog_buttons(ui, "Create", !title.trim().is_empty(), accent);
                        if (ok || enter) && !title.trim().is_empty() {
                            self.backend.request(Req::CreatePlaylist { title: title.trim().to_owned(), video_ids: tracks.iter().map(|t| t.id.clone()).collect() });
                            done = true;
                        }
                        done |= cancel;
                    }
                    Dialog::EditPlaylist { id, title, description, privacy } => {
                        text_field(ui, "Name", title, false, "Playlist name");
                        ui.add_space(16.0);
                        text_field(ui, "Description", description, true, "Add an optional description");
                        ui.add_space(16.0);
                        field_label(ui, "Visibility");
                        privacy_picker(ui, privacy);
                        ui.add_space(26.0);
                        let (ok, cancel) = dialog_buttons(ui, "Save", !title.trim().is_empty(), accent);
                        if ok {
                            self.backend.request(Req::EditPlaylist { id: id.clone(), title: title.trim().to_owned(), description: description.clone(), privacy: privacy.clone() });
                            done = true;
                        }
                        done |= cancel;
                    }
                    Dialog::Collaborate { info, busy, confirm_off, .. } => done = self.collaborate_dialog(ui, info, busy, confirm_off, accent),
                    Dialog::ConfirmDelete { .. } => unreachable!("drawn by confirm_delete"),
                }
            });
        });
        let dismissed = modal.should_close() || ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if !done && !dismissed { self.dialog = Some(dialog); }
    }
}

impl App {
    /// The Collaborate dialog's body: the two switches, the invite link and who has
    /// joined. True once it's closed.
    fn collaborate_dialog(&mut self, ui: &mut Ui, info: &mut Option<crate::ytm::Collaboration>, busy: &mut bool, confirm_off: &mut bool, accent: Color32) -> bool {
        use crate::ytm::CollabChange;
        let Some(c) = info.clone() else {
            ui.label(egui::RichText::new("Loading…").font(theme::regular(14.0)).color(theme::text_dim()));
            ui.add_space(24.0);
            return done_button(ui);
        };
        let mut change = None;
        if *confirm_off {
            ui.label(egui::RichText::new("Turn off collaboration?").font(theme::semibold(15.0)).color(theme::TEXT));
            ui.add_space(6.0);
            ui.label(egui::RichText::new("Collaborators will be removed from the playlist. Songs they added stay.").font(theme::regular(13.0)).color(theme::text_dim()));
            ui.add_space(24.0);
            let (ok, cancel) = dialog_buttons(ui, "Turn off", !*busy, accent);
            if ok {
                change = Some(CollabChange::Collaborate(false));
            }
            if ok || cancel {
                *confirm_off = false;
            }
        } else {
            ui.add_enabled_ui(!*busy, |ui| {
                let desc = if c.private && !c.on { "Collaborators can add songs. Makes this private playlist unlisted." } else { "Collaborators can add songs." };
                pages::setting_row(ui, "Collaborate", Some(desc), |ui| {
                    let mut on = c.on;
                    if pages::toggle(ui, &mut on, "Collaborate").changed() {
                        if on { change = Some(CollabChange::Collaborate(true)) } else { *confirm_off = true }
                    }
                });
                if c.on {
                    pages::setting_row(ui, "Allow new collaborators", Some("Turn off to stop new people joining with the link."), |ui| {
                        let mut on = c.allow_new;
                        if pages::toggle(ui, &mut on, "Allow new collaborators").changed() {
                            change = Some(CollabChange::AllowNew(on));
                        }
                    });
                }
            });
            if c.on {
                ui.add_space(10.0);
                if let Some(link) = c.invite_link.as_deref().filter(|_| c.allow_new) {
                    if widgets::pill(ui, "Copy invite link", Some(theme::icon::LINK), true).clicked() {
                        ui.ctx().copy_text(link.to_owned());
                        self.toast("Invite link copied. Whoever opens it can add songs.");
                    }
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("Send the link to the people you want to add. They join from YouTube Music.").font(theme::regular(12.5)).color(theme::TEXT_FAINT));
                } else {
                    ui.label(egui::RichText::new("New collaborators can't join. Turn on Allow new collaborators for an invite link.").font(theme::regular(12.5)).color(theme::TEXT_FAINT));
                }
                ui.add_space(16.0);
                field_label(ui, "Collaborators");
                if c.collaborators.is_empty() {
                    ui.label(egui::RichText::new("No one has joined yet.").font(theme::regular(13.5)).color(theme::text_dim()));
                }
                for name in &c.collaborators {
                    ui.label(egui::RichText::new(name).font(theme::regular(14.0)).color(theme::TEXT));
                    ui.add_space(4.0);
                }
            }
            ui.add_space(24.0);
            // Each switch applies right away, so there's nothing to cancel.
            if done_button(ui) {
                return true;
            }
        }
        if let Some(change) = change {
            *busy = true;
            self.backend.request(Req::SetCollaboration { info: c, change });
        }
        false
    }
}

fn field_label(ui: &mut Ui, label: &str) {
    ui.label(egui::RichText::new(label).font(theme::semibold(13.0)).color(theme::text_dim()));
    ui.add_space(8.0);
}

/// A labelled text box on a glass fill, like the jam and transfer pages' inputs.
fn text_field(ui: &mut Ui, label: &str, text: &mut String, multiline: bool, hint: &str) -> egui::Response {
    use egui::{Sense, vec2};
    field_label(ui, label);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), if multiline { 96.0 } else { 42.0 }), Sense::hover());
    // Painted first, so the text box draws over it.
    let bg = ui.painter().add(egui::Shape::Noop);
    let inner = rect.shrink2(vec2(14.0, if multiline { 10.0 } else { 4.0 }));
    let edit = if multiline { egui::TextEdit::multiline(text).desired_rows(4) } else { egui::TextEdit::singleline(text).align(egui::Align2::LEFT_CENTER) };
    let resp = ui.put(
        inner,
        edit.hint_text(egui::RichText::new(hint).color(theme::TEXT_FAINT)).font(theme::regular(14.0)).frame(egui::Frame::NONE).desired_width(inner.width()),
    );
    let focus = theme::anim_bool(ui.ctx(), resp.id.with("focus"), resp.has_focus(), 0.15);
    ui.painter().set(bg, egui::Shape::Vec(vec![
        egui::Shape::rect_filled(rect, 10.0, theme::glass(0.08 + 0.03 * focus)),
        egui::Shape::rect_stroke(rect, 10.0, egui::Stroke::new(1.0, theme::glass(0.06 + 0.18 * focus)), egui::StrokeKind::Inside),
    ]));
    resp
}

/// Private / Unlisted / Public as one segmented control; nothing picked keeps the current setting.
fn privacy_picker(ui: &mut Ui, privacy: &mut Option<String>) {
    use egui::{Sense, vec2};
    let (track, _) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::hover());
    ui.painter().rect_filled(track, 10.0, theme::glass(0.08));
    let options = [("PRIVATE", "Private"), ("UNLISTED", "Unlisted"), ("PUBLIC", "Public")];
    let w = (track.width() - 6.0) / options.len() as f32;
    for (i, (value, label)) in options.into_iter().enumerate() {
        let r = egui::Rect::from_min_size(track.min + vec2(3.0 + w * i as f32, 3.0), vec2(w, track.height() - 6.0));
        let resp = widgets::named(ui.interact(r, ui.scope_id().with(("privacy", value)), Sense::click()), label).on_hover_cursor(egui::CursorIcon::PointingHand);
        let on = privacy.as_deref() == Some(value);
        let s = theme::anim_bool(ui.ctx(), resp.id.with("s"), on, 0.15);
        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
        let fill = theme::lerp_color(theme::glass(0.06 * h), Color32::from_gray(245), s);
        if fill.a() > 0 { ui.painter().rect_filled(r, 8.0, fill); }
        let color = theme::lerp_color(theme::with_alpha(theme::TEXT, 0.7 + 0.3 * h), Color32::from_gray(15), s);
        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, label, theme::semibold(13.0), color);
        if resp.clicked() {
            *privacy = if on { None } else { Some(value.to_owned()) };
        }
    }
    if privacy.is_none() {
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Unchanged. Pick one to change who can see it.").font(theme::regular(12.5)).color(theme::TEXT_FAINT));
    }
}

/// Cancel and a primary button, right-aligned: (primary clicked, cancel clicked).
fn dialog_buttons(ui: &mut Ui, primary: &str, enabled: bool, accent: Color32) -> (bool, bool) {
    use egui::{Sense, vec2};
    let (mut ok, mut cancel) = (false, false);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        let fg = if accent.r() as u32 + accent.g() as u32 + accent.b() as u32 > 480 { Color32::from_gray(15) } else { Color32::WHITE };
        let g = ui.painter().layout_no_wrap(primary.to_owned(), theme::semibold(14.0), fg);
        let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 40.0, 38.0), if enabled { Sense::click() } else { Sense::hover() });
        let resp = widgets::named(resp, primary);
        let resp = if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() && enabled, 0.15);
        let fill = theme::lerp_color(accent, Color32::WHITE, 0.12 * h);
        ui.painter().rect_filled(rect, 19.0, theme::with_alpha(fill, if enabled { 1.0 } else { 0.35 }));
        ui.painter().galley_with_override_text_color(rect.center() - g.size() / 2.0, g, theme::with_alpha(fg, if enabled { 1.0 } else { 0.6 }));
        ok = resp.clicked();
        cancel = widgets::pill(ui, "Cancel", None, false).clicked();
    });
    (ok, cancel)
}

/// A lone right-aligned Done for a dialog whose changes apply as they're made.
fn done_button(ui: &mut Ui) -> bool {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| widgets::pill(ui, "Done", None, true).clicked()).inner
}

/// A playlist (or "New playlist") row in the Add to playlist dialog.
fn dialog_row(ui: &mut Ui, thumbs: Option<&[crate::model::Thumb]>, glyph: Option<char>, label: &str) -> bool {
    use egui::{Sense, pos2, vec2};
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::click());
    let resp = widgets::named(resp, label).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
    if h > 0.0 { ui.painter().rect_filled(rect, 10.0, theme::glass(0.08 * h)); }
    let art = egui::Rect::from_center_size(pos2(rect.left() + 28.0, rect.center().y), vec2(40.0, 40.0));
    match (thumbs, glyph) {
        (Some(t), _) => widgets::paint_cover(ui, art, t, 6.0, 1.0),
        (None, Some(g)) => {
            ui.painter().rect_filled(art, 6.0, theme::glass(0.1));
            widgets::icon_at(ui, art.center(), g, 18.0, theme::TEXT);
        }
        _ => {}
    }
    let x = art.right() + 14.0;
    widgets::text_at(ui, pos2(x, rect.center().y - 9.0), label, theme::semibold(14.0), theme::TEXT, rect.right() - x - 10.0);
    resp.clicked()
}

/// "Delete playlist?" as a modal card: Some(true) delete, Some(false) cancel, None still open.
fn confirm_delete(ctx: &egui::Context, title: &str, _accent: Color32) -> Option<bool> {
    let mut answer = None;
    let modal = egui::Modal::new(egui::Id::unique("confirm-delete")).frame(notes::surface()).backdrop_color(theme::shade(0.55)).show(ctx, |ui| {
        ui.set_width(400.0_f32.min(ctx.content_rect().width() - 48.0));
        egui::Frame::new().inner_margin(egui::Margin { left: 26, right: 26, top: 24, bottom: 22 }).show(ui, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
            ui.label(egui::RichText::new("Delete playlist?").font(theme::bold(22.0)).color(theme::TEXT));
            ui.add_space(10.0);
            let body = format!("\u{201c}{title}\u{201d} will be removed from your YouTube Music library. This can't be undone.");
            ui.add(egui::Label::new(egui::RichText::new(body).font(theme::regular(14.0)).color(theme::text_dim())).wrap());
            ui.add_space(22.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                // Destructive action in red, white text.
                let label = "Delete";
                let g = ui.painter().layout_no_wrap(label.to_owned(), theme::semibold(14.0), Color32::WHITE);
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(g.size().x + 40.0, 38.0), egui::Sense::click());
                let resp = widgets::named(resp, "Delete playlist").on_hover_cursor(egui::CursorIcon::PointingHand);
                let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
                ui.painter().rect_filled(rect, 19.0, theme::lerp_color(Color32::from_rgb(214, 40, 57), Color32::from_rgb(236, 56, 72), h));
                ui.painter().galley(rect.center() - g.size() / 2.0, g, Color32::WHITE);
                if resp.clicked() {
                    answer = Some(true);
                }
                if widgets::pill(ui, "Cancel", None, false).clicked() {
                    answer = Some(false);
                }
            });
        });
    });
    if answer.is_none() && (modal.should_close() || ctx.input(|i| i.key_pressed(egui::Key::Escape))) {
        answer = Some(false);
    }
    answer
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
        } else if self.np_open && playing {
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
        let accent = self.accent_color();
        ctx.data_mut(|d| d.insert_temp(egui::Id::unique("accent"), accent));
        if self.chrome_applied.as_deref() != Some(look.id.as_str()) {
            if self.chrome_applied.is_none() {
                ctx.add_texture_loader(Arc::new(crate::images::Textures::default()));
            }
            theme::apply_chrome(&ctx, look);
            self.chrome_applied = Some(look.id.clone());
        }
        let app_style = &look.app;
        // Once now playing has slid all the way up it hides everything but the player
        // bar, so the pages and the full-screen background under it aren't drawn.
        let covered = !self.fullscreen && theme::anim_bool(&ctx, "np-open", self.np_open && self.current().is_some(), 0.42) >= 1.0;
        let bar_strip = egui::Rect::from_min_max(egui::pos2(screen.left(), screen.bottom() - self.bar_height()), screen.max);
        shell::background(self, ui, screen, now, covered.then_some(bar_strip));
        if !self.fullscreen {
            let floating = look.chrome.player_bar == crate::preset::PlayerBar::Floating;
            let bar_alpha = app_style.bar_alpha.clamp(0.0, 1.0);
            let frame = if floating {
                egui::Frame::new().inner_margin(egui::Margin { left: 14, right: 14, top: 0, bottom: 14 })
            } else {
                let mut f = egui::Frame::new().fill(theme::shade(bar_alpha));
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
                        ui.painter().rect_filled(r, radius, theme::shade((bar_alpha + 0.07).min(1.0)));
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
            let side_frame = match app_style.sidebar {
                SidebarStyle::Glass => egui::Frame::new().fill(theme::shade(side_alpha)).inner_margin(egui::Margin::symmetric(14, 16)),
                SidebarStyle::Transparent => egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 16)),
                SidebarStyle::Solid => egui::Frame::new().fill(look.solid()).inner_margin(egui::Margin::symmetric(14, 16)),
                SidebarStyle::Floating => egui::Frame::new()
                    .fill(theme::shade(side_alpha.max(0.3)))
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
        notes::card(self, &ctx);
        notes::dialog(self, &ctx);
        if let Some(url) = self.share_pending.take() { ctx.copy_text(url); }
        shell::suggestions(self, &ctx);
        shell::toast(self, &ctx, now);

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
        let repeat = match self.repeat {
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
        storage.set_string("close_to_tray", b(self.close_to_tray));
        storage.set_string("sidebar_collapsed", b(self.sidebar_collapsed));
        // Pretend versions (dev hook) mustn't decide the real "what's new".
        if std::env::var_os("YTM_UPDATE_AS").is_none() {
            storage.set_string("last_version", crate::update::current_version());
        }
        storage.set_string("discord_enabled", b(self.discord_enabled));
        storage.set_string("default_theme", b(self.default_theme));
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
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Dev hook: `YTM_SCREENSHOT=out.png` saves the app's own framebuffer after
/// `YTM_SCREENSHOT_AT` seconds (default 8), independent of OS window capture.
/// `YTM_SCREENSHOT_BURST=<n>` instead keeps the next n frames from `YTM_SCREENSHOT_AT`
/// (in memory, so saving doesn't slow them) and then writes `<stem>_<i>_<ms>.png`,
/// `ms` being each frame's time: a filmstrip of an animation and its frame pacing.
fn dev_screenshot(ctx: &egui::Context, now: f64) {
    let Some(path) = dev_var("YTM_SCREENSHOT") else { return };
    let at: f64 = dev_var("YTM_SCREENSHOT_AT").and_then(|v| v.parse().ok()).unwrap_or(8.0);
    if let Some(n) = dev_var("YTM_SCREENSHOT_BURST").and_then(|v| v.parse::<usize>().ok()) {
        static FRAMES: std::sync::Mutex<Vec<(f64, Arc<egui::ColorImage>)>> = std::sync::Mutex::new(Vec::new());
        static REQUESTED: AtomicUsize = AtomicUsize::new(0);
        static DONE: AtomicBool = AtomicBool::new(false);
        if DONE.load(Ordering::Relaxed) {
            return;
        }
        if now >= at && REQUESTED.load(Ordering::Relaxed) < n {
            REQUESTED.fetch_add(1, Ordering::Relaxed);
            let c = ctx.clone();
            ctx.request_screenshot(move |img| {
                FRAMES.lock().unwrap().push((now, img));
                c.request_repaint();
            });
            ctx.request_repaint();
        } else if now < at {
            ctx.request_repaint_after(Duration::from_secs_f64(at - now));
        } else if FRAMES.lock().unwrap().len() == n {
            let path = std::path::PathBuf::from(path);
            let shots = std::mem::take(&mut *FRAMES.lock().unwrap());
            DONE.store(true, Ordering::Relaxed);
            std::thread::spawn(move || {
                let t0 = shots[0].0;
                for (i, (t, img)) in shots.iter().enumerate() {
                    let [w, h] = img.size;
                    let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                    let out = path.with_file_name(format!("{}_{i:03}_{:04}.png", path.file_stem().unwrap_or_default().to_string_lossy(), ((t - t0) * 1000.0) as u32));
                    if let Some(img) = image::RgbaImage::from_raw(w as u32, h as u32, bytes) {
                        let _ = img.save(out);
                    }
                }
                log::info!("screenshot saved ({} frames)", shots.len());
            });
        }
        return;
    }
    let requested = egui::Id::unique("dev-shot-requested");
    if now >= at && !ctx.data(|d| d.get_temp::<bool>(requested).unwrap_or(false)) {
        ctx.data_mut(|d| d.insert_temp(requested, true));
        ctx.request_screenshot(move |img| {
            let [w, h] = img.size;
            let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            match image::RgbaImage::from_raw(w as u32, h as u32, bytes).map(|i| i.save(&path)) {
                Some(Ok(())) => log::info!("screenshot saved"),
                other => log::warn!("screenshot failed: {other:?}"),
            }
        });
    } else if now < at {
        ctx.request_repaint_after(Duration::from_secs_f64(at - now));
    }
}

impl App {
    /// Dev hook: `YTM_THEME_TOUR=<secs>` steps through every theme preset from
    /// `YTM_SCREENSHOT_AT`, saving `<YTM_SCREENSHOT stem>_<id>.png` for each, then exits.
    fn theme_tour(&mut self, ctx: &egui::Context, now: f64) {
        let Some(path) = dev_var("YTM_SCREENSHOT").map(std::path::PathBuf::from) else { return };
        let at: f64 = dev_var("YTM_SCREENSHOT_AT").and_then(|v| v.parse().ok()).unwrap_or(8.0);
        let step: f64 = dev_var("YTM_THEME_TOUR").and_then(|v| v.parse().ok()).unwrap_or(3.0);
        // YTM_TOUR_ONLY=id,id limits the tour to those presets.
        let only = dev_var("YTM_TOUR_ONLY").unwrap_or_default();
        let all: Vec<&crate::preset::Preset> = crate::preset::all().iter().filter(|p| only.is_empty() || only.split(',').any(|o| o == p.id)).collect();
        ctx.request_repaint_after(Duration::from_millis(100));
        if now < at {
            return;
        }
        let n = ((now - at) / step) as usize;
        if n >= all.len() {
            // Let the last screenshot land, then quit.
            if now - at > step * all.len() as f64 + 1.5 {
                self.quit(ctx);
            }
        } else if self.theme_id != all[n].id {
            self.theme_id = all[n].id.clone();
            self.default_theme = false;
        }
        let shot_id = egui::Id::unique("tour-shot");
        let requested: usize = ctx.data(|d| d.get_temp(shot_id)).unwrap_or(usize::MAX);
        let in_slot = (now - at) - n as f64 * step;
        if n < all.len() && requested != n && in_slot > step * 0.8 {
            ctx.data_mut(|d| d.insert_temp(shot_id, n));
            let id = all[n].id.clone();
            let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let out = path.with_file_name(format!("{stem}_{n:02}_{id}.png"));
            ctx.request_screenshot(move |img| {
                let [w, h] = img.size;
                let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                match image::RgbaImage::from_raw(w as u32, h as u32, bytes).map(|im| im.save(&out)) {
                    Some(Ok(())) => log::info!("tour screenshot saved: {id}"),
                    other => log::warn!("tour screenshot failed: {other:?}"),
                }
            });
        }
    }
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

/// A `YTM_*` dev hook, read once: several are checked every frame.
fn dev_var(name: &str) -> Option<&'static str> {
    static VARS: std::sync::OnceLock<std::collections::HashMap<String, String>> = std::sync::OnceLock::new();
    VARS.get_or_init(|| {
        std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))).filter(|(k, _)| k.starts_with("YTM_")).collect()
    })
    .get(name)
    .map(String::as_str)
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

#[cfg(test)]
mod account_block_tests {
    use super::account_block;

    #[test]
    fn account_errors_stop_instead_of_skipping() {
        assert!(account_block("sign in to play this song", false).is_some());
        assert!(account_block("sign in to play this song", true).is_none());
        assert!(account_block("This video is only available to Music Premium members", true).is_some());
        assert!(account_block("decode: end of stream", false).is_none());
    }
}

#[cfg(test)]
mod recent_edits_tests {
    use super::{Collection, RecentEdits, Track};

    fn track(id: &str) -> Track {
        Track { id: id.into(), title: id.into(), artists: vec![], album: None, duration: None, thumbs: vec![], track_nr: None, plays: None }
    }

    fn collection(id: &str, songs: &[&str]) -> Collection {
        Collection {
            id: id.into(),
            is_album: false,
            save_id: None,
            title: String::new(),
            subtitle: String::new(),
            artists: vec![],
            description: None,
            thumbs: vec![],
            tracks: songs.iter().map(|s| track(s)).collect(),
            set_ids: songs.iter().map(|s| Some(format!("set-{s}"))).collect(),
            owned: true,
        }
    }

    #[test]
    fn stale_lists_drop_recent_removals_and_unlikes() {
        let mut edits = RecentEdits::default();
        edits.remove("PL1", ["set-b".to_owned(), "set-c".to_owned()]);
        edits.confirm_removal("PL1");
        let mut c = collection("PL1", &["a", "b", "c", "d"]);
        edits.correct_collection(&mut c);
        assert_eq!(c.tracks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["a", "d"]);
        assert_eq!(c.set_ids, [Some("set-a".to_owned()), Some("set-d".to_owned())]);
        // Another playlist is left alone.
        let mut other = collection("PL2", &["b"]);
        edits.correct_collection(&mut other);
        assert_eq!(other.tracks.len(), 1);

        edits.like("x", false, None);
        edits.like("y", true, Some(track("y")));
        edits.like("z", false, None);
        edits.confirm_likes(&["x".to_owned(), "y".to_owned()], &["z".to_owned()]);
        let mut lm = collection("LM", &["x", "z", "w"]);
        edits.correct_collection(&mut lm);
        // x unliked; y liked but not listed yet; z's unlike was refused, so it stays.
        assert_eq!(lm.tracks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["y", "z", "w"]);
        assert_eq!(lm.set_ids.len(), 3);
        // A later page drops an unliked song too.
        assert!(!edits.keeps("LM", &track("x"), Some("set-x")));
        assert!(edits.keeps("PL9", &track("x"), Some("set-x")));
    }
}

/// Dev hook: YTM_FPS=<hz> holds each frame to 1/<hz> s, as vsync does on a visible window
/// of that refresh rate (test windows sit off-screen, where Windows gives them ~21 fps);
/// YTM_FPS_LOG logs the rate reached every 5 s.
fn dev_pace() {
    use std::sync::Mutex;
    use std::time::Instant;
    static HZ: std::sync::OnceLock<Option<f64>> = std::sync::OnceLock::new();
    static STATE: Mutex<Option<(Instant, Instant, u32)>> = Mutex::new(None);
    let Some(hz) = *HZ.get_or_init(|| std::env::var("YTM_FPS").ok().and_then(|v| v.parse().ok())) else { return };
    let period = std::time::Duration::from_secs_f64(1.0 / hz);
    let mut st = STATE.lock().unwrap();
    let now = Instant::now();
    let (last, window, frames) = st.get_or_insert((now, now, 0));
    let next = *last + period;
    if next > now {
        std::thread::sleep(next - now);
    }
    *last = Instant::now();
    *frames += 1;
    let span = last.duration_since(*window).as_secs_f64();
    if span >= 5.0 {
        if std::env::var_os("YTM_FPS_LOG").is_some() {
            log::info!("fps {:.1}", *frames as f64 / span);
        }
        *window = *last;
        *frames = 0;
    }
}

/// How long YouTube Music may go on listing the old state after taking an edit.
const EDIT_LAG: Duration = Duration::from_secs(180);
/// How long a removal counts while YouTube Music hasn't answered (one it refused shows only
/// as an error), and likes, which a few hundred at a time take a while.
const REMOVAL_UNANSWERED: Duration = Duration::from_secs(60);
const LIKE_UNANSWERED: Duration = Duration::from_secs(600);

#[derive(Default)]
struct RecentEdits {
    /// Song: liked or not, the song when known (to put back a like the list lacks yet), and when.
    likes: HashMap<String, (bool, Option<Track>, EditAge)>,
    /// Playlist: the entries (`setVideoId`s) taken out of it, and when.
    removals: HashMap<String, (HashSet<String>, EditAge)>,
}

#[derive(Clone, Copy)]
struct EditAge {
    made: std::time::Instant,
    confirmed: Option<std::time::Instant>,
}

impl EditAge {
    fn new() -> Self {
        Self { made: std::time::Instant::now(), confirmed: None }
    }

    fn live(&self, unanswered: Duration) -> bool {
        match self.confirmed {
            Some(t) => t.elapsed() < EDIT_LAG,
            None => self.made.elapsed() < unanswered,
        }
    }
}

impl RecentEdits {
    fn like(&mut self, id: &str, like: bool, track: Option<Track>) {
        self.likes.insert(id.to_owned(), (like, track, EditAge::new()));
    }

    fn confirm_likes(&mut self, done: &[String], refused: &[String]) {
        for id in done {
            if let Some((_, _, age)) = self.likes.get_mut(id) {
                age.confirmed = Some(std::time::Instant::now());
            }
        }
        for id in refused {
            self.likes.remove(id);
        }
    }

    fn remove(&mut self, playlist: &str, set_ids: impl IntoIterator<Item = String>) {
        let (set, age) = self.removals.entry(playlist.to_owned()).or_insert_with(|| (HashSet::new(), EditAge::new()));
        set.extend(set_ids);
        *age = EditAge::new();
    }

    fn confirm_removal(&mut self, playlist: &str) {
        if let Some((_, age)) = self.removals.get_mut(playlist) {
            age.confirmed = Some(std::time::Instant::now());
        }
    }

    fn prune(&mut self) {
        self.likes.retain(|_, (_, _, age)| age.live(LIKE_UNANSWERED));
        self.removals.retain(|_, (_, age)| age.live(REMOVAL_UNANSWERED));
    }

    /// Whether a song row YouTube Music sent still stands: not taken out of that
    /// playlist, nor unliked when it's Liked Music.
    fn keeps(&self, list: &str, t: &Track, set_id: Option<&str>) -> bool {
        let unliked = list == LIKED_MUSIC && self.likes.get(&t.id).is_some_and(|(like, _, _)| !*like);
        let taken = set_id.is_some_and(|s| self.removals.get(list).is_some_and(|(r, _)| r.contains(s)));
        !unliked && !taken
    }

    /// A list as YouTube Music sent it, with the edits made here since; Liked Music also
    /// gets the songs liked here that it doesn't list yet, at the top.
    fn correct_collection(&mut self, c: &mut Collection) {
        self.prune();
        let keep: Vec<bool> = c.tracks.iter().enumerate().map(|(i, t)| self.keeps(&c.id, t, c.set_ids.get(i).and_then(Option::as_deref))).collect();
        let mut k = keep.iter();
        c.tracks.retain(|_| k.next().copied().unwrap_or(true));
        let mut k = keep.iter();
        c.set_ids.retain(|_| k.next().copied().unwrap_or(true));
        if c.id == LIKED_MUSIC {
            for (id, (like, track, _)) in &self.likes {
                if let (true, Some(t)) = (*like, track)
                    && !c.tracks.iter().any(|x| &x.id == id)
                {
                    c.tracks.insert(0, t.clone());
                    c.set_ids.insert(0, None);
                }
            }
        }
    }
}
