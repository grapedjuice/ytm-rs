//! The application UI: state, playback logic and the per-frame layout.
//!
//! Layout: animated album-art background (shader.rs) → sidebar, top bar and page
//! content → player bar → now-playing overlay (slides up) → toasts.

mod account;
mod actions;
mod collections;
mod colorpick;
mod comments;
mod dev;
mod dialogs;
mod find;
mod hotkeys;
mod integrations;
mod jam;
mod karaoke;
mod lists;
mod lyric_prefs;
mod mini;
mod notes;
mod nowplaying;
mod pace;
mod pages;
mod playback;
mod queue;
mod responses;
mod row_drag;
mod shell;
mod titlebar;
pub use titlebar::{custom as custom_titlebar, init as init_titlebar, set_hwnd as set_titlebar_hwnd};
mod transfer;
mod vector;
mod wheel;
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
use crate::ytm::Scope;
use lists::{LIKED_MUSIC, Lists};
use queue::Queue;
use dev::{dev_screenshot, dev_var};
use dialogs::text_field;
pub use queue::Repeat;

#[derive(Clone, PartialEq, Hash, Debug)]
pub enum Page {
    Home,
    Explore,
    /// A Moods & genres category, by its browse params.
    Mood(String),
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
    /// Subscribed artists.
    Artists,
    /// Saved podcast shows (with Settings › Playback › Show podcasts on).
    Podcasts,
    History,
    Uploads,
    /// Songs saved on this computer: played from their files, so offline and signed out too.
    Downloads,
}

#[derive(Clone, Copy, PartialEq)]
pub enum NpTab {
    Lyrics,
    UpNext,
    Comments,
    /// Neither panel: just the artwork.
    Art,
}

pub use mini::{init_pill, set_hwnd as set_mini_hwnd};

/// Settings › General › Window › Open on: the pages the app can start on, as saved and
/// as listed.
pub(super) const START_PAGES: [(&str, &str, Page); 10] = [
    ("home", "Home", Page::Home),
    ("explore", "Explore", Page::Explore),
    ("library", "Library › Playlists", Page::Library(LibTab::Playlists)),
    ("liked", "Library › Liked songs", Page::Library(LibTab::Songs)),
    ("albums", "Library › Albums", Page::Library(LibTab::Albums)),
    ("artists", "Library › Artists", Page::Library(LibTab::Artists)),
    ("podcasts", "Library › Podcasts", Page::Library(LibTab::Podcasts)),
    ("history", "Library › History", Page::Library(LibTab::History)),
    ("uploads", "Library › Uploads", Page::Library(LibTab::Uploads)),
    ("downloads", "Library › Downloads", Page::Library(LibTab::Downloads)),
];

/// The page picked to open on, from the saved `start_page` key (Home for anything unknown).
fn start_choice(saved: &str) -> Page {
    START_PAGES.into_iter().find(|(k, ..)| *k == saved).map_or(Page::Home, |(.., p)| p)
}

/// The page the app opens on: the one picked, or Home when it can't show this time (the
/// account's Library signed out, Podcasts hidden). The pick itself stays for next time.
fn start_page(choice: &Page, logged_in: bool, podcasts: bool) -> Page {
    match choice {
        Page::Library(LibTab::Podcasts) if !podcasts => Page::Home,
        Page::Library(t) if !logged_in && *t != LibTab::Downloads => Page::Home,
        p => p.clone(),
    }
}

/// Where the accent colour comes from (Settings › Appearance › Accent colour).
#[derive(Clone, Copy, PartialEq)]
pub enum AccentSource {
    /// The theme's own: a fixed colour, the cover's, or the system's.
    Theme,
    /// The system accent colour, whatever the theme.
    System,
    /// `theme.conf` (every colour) or `accent.txt` (just this one) in the data folder, for
    /// wallust, pywal and the like to write.
    File,
    Custom(Color32),
}

impl AccentSource {
    /// Saved as "" (theme), "system", "file" or "#rrggbb"; older versions read "system"
    /// and "file" as the theme's.
    fn from_key(v: &str) -> Self {
        match v {
            "system" => Self::System,
            "file" => Self::File,
            v => crate::preset::hex(v).map_or(Self::Theme, Self::Custom),
        }
    }

    fn key(self) -> String {
        match self {
            Self::Theme => String::new(),
            Self::System => "system".into(),
            Self::File => "file".into(),
            Self::Custom(c) => format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b()),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Theme => "Theme",
            Self::System => "System",
            Self::File => "From file",
            Self::Custom(_) => "Custom",
        }
    }
}

/// User intents collected while drawing; applied after the frame's UI pass.
pub enum Action {
    Play(Vec<Track>, usize),
    /// The songs a list was narrowed to: they stay the whole queue, however much more of
    /// the list comes in.
    PlayFound(Vec<Track>, usize),
    PlayCollection(Target, bool),
    /// Click on a card: songs start a radio, everything else opens its page.
    Open(Target),
    PlayNext(Track),
    Enqueue(Track),
    Radio(Track),
    Go(Page),
    QueueJump(usize),
    QueueRemove(usize),
    /// A song to come dragged to the gap between songs (see `Queue::move_track`), if the
    /// song `id` is still at `from`.
    QueueMove { from: usize, gap: usize, id: String },
    Seek(f32),
    /// Moving video or the picture, for music videos.
    ToggleVideo,
    /// The Song/Video switch on a song: its music video in its place (`true`), or the
    /// song again.
    SongVideo(bool),
    Like(Track, bool),
    /// The heart of what's playing, in the player bar, the mini player and Now playing's menu.
    LikeHeard(bool),
    /// Like (or unlike) a selection of songs.
    LikeMany(Vec<Track>, bool),
    Dislike(Track),
    Share(Target),
    AddToPlaylist(Vec<Track>),
    /// Rows (indices into the open playlist) to take out of it.
    RemoveFromPlaylist(Vec<usize>),
    /// A song of your own playlist dragged to the gap between rows, if `id` is still at `from`.
    PlaylistMove { from: usize, gap: usize, id: String },
    Download(Track),
    /// Download an album or playlist (fetched whole when the songs are `None`), or a
    /// selection (no target), by its name.
    DownloadMany(Option<Target>, String, Option<Vec<Track>>),
    /// Stop a download of many songs, by its `DownloadRun::key`.
    CancelDownload(String),
    NewPlaylist(Vec<Track>),
    EditPlaylist,
    /// Save one of your playlist's sort orders to it: (playlist id, what to send).
    SortPlaylist(String, serde_json::Value),
    /// The open playlist's collaboration settings.
    Collaborate,
    DeletePlaylist,
    SaveCollection(String, bool),
    /// Subscribe to the open artist (true), or unsubscribe.
    Subscribe(bool),
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
    /// A large download (`count` songs to go) asked about before it starts.
    ConfirmDownload { target: Option<Target>, title: String, tracks: Vec<Track>, count: usize },
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

    /// Toggle row `i`, or with `range` pick every row from the last one toggled to it, as
    /// they're `shown` (a list found in or sorted; `None` is the whole list in its order).
    pub fn pick(&mut self, page: &Page, i: usize, range: bool, shown: Option<&[usize]>) {
        if self.page.as_ref() != Some(page) {
            *self = Self { page: Some(page.clone()), ..Self::default() };
        }
        match self.anchor.filter(|_| range) {
            Some(a) => self.rows.extend(find::run(shown, a, i)),
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
    /// Keeps the computer awake while playing.
    awake: Option<crate::awake::Awake>,
    discord: Option<crate::discord::Discord>,
    discord_enabled: bool,
    /// Last state sent to Discord, to send only real changes.
    discord_sent: Option<crate::discord::Presence>,
    /// Local WebSocket for a Stream Deck plugin. Off until Connections enables it.
    remote: crate::remote::Remote,
    remote_enabled: bool,
    /// The Last.fm account scrobbles go to; a sign-in in progress and its outcome.
    lastfm_user: Option<String>,
    lastfm_busy: Arc<AtomicBool>,
    lastfm_result: Arc<std::sync::Mutex<Option<Result<String, String>>>>,
    /// The ListenBrainz account listens go to; the token being typed, a check of it in
    /// progress and its outcome.
    listenbrainz_user: Option<String>,
    listenbrainz_token: String,
    listenbrainz_busy: Arc<AtomicBool>,
    listenbrainz_result: Arc<std::sync::Mutex<Option<Result<String, String>>>>,
    /// The play generation Last.fm and ListenBrainz last heard about, and whether it was
    /// scrobbled.
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
    /// The name of a source played straight from its card, which isn't kept (see `playing_from`).
    played_title: Option<(Target, String)>,
    /// Asked to play (shuffled?) before its songs are here: a playlist's first page, or
    /// an album's or artist's songs (`Req::PlayCollection`).
    play_pending: Option<(Target, bool)>,
    bg: Option<Background>,

    // navigation
    page: Page,
    /// Settings › General › Window › Open on: the page the app starts on.
    start_page: Page,
    back: Vec<Page>,
    forward: Vec<Page>,
    page_changed: f64,
    /// Room the page leaves at its top and bottom for glass it scrolls under.
    page_insets: [f32; 2],
    /// When a video's picture last went to the background, and the cover to go back to.
    video_art_at: f64,
    track_art: Option<crate::art::Art>,
    /// How far the page is scrolled.
    page_scrolled: f32,
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
    /// The Moods & genres category last opened.
    mood: Option<crate::ytm::MoodPage>,
    search: Option<SearchResults>,
    /// The search tab picked (YT Music, Library, Uploads) and the search it was picked on:
    /// another search starts on YT Music.
    search_scope: (String, Scope),
    /// Library and Uploads results for the search shown, each loaded when its tab is first picked.
    scoped: Vec<SearchResults>,
    /// Albums and playlists opened lately, so going back to one shows it at once (and
    /// refreshes it quietly), and their loads under way.
    lists: Lists,
    /// Playlists a sort was asked of, until they load in that order (or it fails).
    sorting: HashMap<String, lists::SortWait>,
    artist: Option<ArtistPage>,
    /// Subscribes and unsubscribes: the one under way, and what they settled.
    subscriptions: actions::Subscriptions,
    library: Option<Library>,
    /// When the library was last requested; the sidebar spinner gives up after a while.
    library_requested: std::time::Instant,
    history: Option<Vec<Track>>,
    uploads: Option<Vec<Track>>,
    /// Library › Downloads, read from the download index when the tab opens.
    downloads: Option<Vec<Track>>,
    /// How many of this session's saves `downloads` has (`downloads::saved_since`).
    downloads_seen: usize,
    /// Albums, playlists and selections downloading now.
    download_runs: Vec<crate::backend::DownloadRun>,
    /// Whether a collection's songs are all downloaded, by its id, as its page last
    /// found (with how many songs it had then); cleared when a download finishes.
    all_downloaded: HashMap<String, (u64, bool)>,
    /// Files chosen in the upload dialog, handed over from its thread.
    picked_uploads: Arc<std::sync::Mutex<Option<Vec<std::path::PathBuf>>>>,
    liked: HashSet<String>,
    /// Songs whose unlike is queued and hasn't gone through yet.
    unliking: HashMap<String, Track>,
    selection: Selection,
    find: find::ListFind,
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
    /// Show music videos moving, playing their own audio to stay in time.
    show_video: bool,
    /// The tallest a music video plays at, one of [`VIDEO_QUALITIES`].
    video_quality: u32,
    /// The video on show or on its way, by id.
    video: Option<(String, Option<crate::video::Player>)>,
    video_frame: Option<egui::TextureHandle>,
    /// The glow around a video, from its colours.
    ambient_light: bool,
    glow: Option<nowplaying::Glow>,
    /// Animated covers in Now playing, where Apple Music has one for the song.
    animated_covers: bool,
    /// The song's animated cover: looked up, then playing.
    cover_clip: Option<nowplaying::CoverClip>,
    hide_search_videos: bool,
    /// Leave explicit songs out of lists and the queue.
    hide_explicit: bool,
    /// Podcast shows and episodes on Home, in search and in the library.
    show_podcasts: bool,
    /// Now playing's Comments panel. Off by default.
    show_comments: bool,
    /// With them, ask YouTube for its Restricted Mode too.
    restricted_mode: bool,
    /// The song version playing in place of the current music video, or the music video
    /// playing in place of the current song.
    playing_as: Option<PlayingAs>,
    /// Songs' music videos, for the Song/Video switch: looked up once each (`None`: it has
    /// none, or the answer hasn't come yet).
    music_videos: std::collections::HashMap<String, Option<crate::ytm::MusicVideo>>,
    /// The output device picked in settings (cpal's id and its name, for when it's not
    /// connected), and the devices last listed there.
    output_device: Option<(String, String)>,
    output_devices: Vec<crate::audio::OutputDevice>,
    radio_pending: bool,
    /// The last song ended with nothing after it: radio carries on from there.
    ended: bool,
    seek_drag: Option<f32>,

    // visuals
    art_key: Option<String>,
    art_changed: f64,
    art_brightness: f32,
    accent: Color32,
    /// Selected theme (see `preset.rs`).
    theme_id: String,
    /// Preset whose chrome (glass, corner radius) is installed in the egui style.
    chrome_applied: Option<String>,
    /// Light or dark pages (the ytm-rs theme's; see `light_pages`).
    appearance: theme::Appearance,
    /// The theme's accent colour, the system's or the listener's own.
    accent_source: AccentSource,
    /// The play button and volume slider in the accent colour, like the progress line.
    accent_controls: bool,
    anim_bg: bool,
    /// Panels frost what's behind them, in themes that have them do so.
    glass_on: bool,
    reactive_bg: bool,
    /// Skip the parts of music videos SponsorBlock marks as not music.
    sponsorblock: bool,
    /// Crossfade between songs, and over how many seconds.
    crossfade: bool,
    crossfade_secs: u32,
    /// Also crossfade when skipping or picking a song.
    crossfade_skips: bool,
    /// The equalizer: whether it's on, its bands (dB) and whether it keeps clear of clipping.
    eq_on: bool,
    eq_gains: crate::audio::eq::Gains,
    eq_no_clip: bool,
    /// Playback speed: whether it's on (saved, and it puts a button in the player bar),
    /// and the speed (not saved: 1× each launch).
    tempo_on: bool,
    speed: f32,
    /// The next song starts because this one is ending: crossfade into it.
    handoff: bool,
    /// SponsorBlock segments for a video, and when one was last skipped.
    segments: Option<(String, Vec<crate::sponsorblock::Segment>)>,
    segment_skipped: f64,
    /// The system-wide media shortcuts.
    hotkeys: hotkeys::State,
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
    /// Smooth scrolling: wheel distance still to be scrolled, or a touchpad coast.
    wheel: wheel::Glide,

    // now playing
    np_open: bool,
    np_tab: NpTab,
    queue_open: bool,
    /// A song row being dragged.
    row_drag: Option<row_drag::RowDrag>,
    /// Settings: the category on show, and what its search box is looking for.
    settings_tab: pages::SettingsTab,
    settings_query: String,
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
    /// Updates from the nightly channel rather than stable.
    update_nightly: bool,
    /// When the last automatic update check ran (app time, s).
    update_checked: Option<f64>,
    update_announced: bool,
    /// The last version the .deb copy said was out, so it says so once per version.
    update_seen: Option<String>,
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
    /// The tray icon, while "Close to tray" is on.
    tray: Option<crate::tray::Tray>,
    /// The tray icon couldn't be made (e.g. no GTK): don't try again, × quits.
    tray_failed: bool,
    /// × has hidden the window behind the tray icon.
    in_tray: bool,
    /// Wayland: the minimized window has lost focus, so focus means it's back.
    tray_left_focus: bool,
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

/// The equalizer's bands as saved: ten comma-separated dB values.
fn parse_gains(s: &str) -> Option<crate::audio::eq::Gains> {
    let mut out = [0.0; 10];
    let mut parts = s.split(',');
    for g in &mut out {
        *g = parts.next()?.trim().parse::<f32>().ok().filter(|v| v.is_finite())?.clamp(-crate::audio::eq::MAX_DB, crate::audio::eq::MAX_DB);
    }
    parts.next().is_none().then_some(out)
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
        let remote_enabled = get("remote_enabled").is_some_and(|v| v == "1");
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
        let hide_explicit = get("hide_explicit").is_some_and(|v| v == "1");
        crate::listenbrainz::init(backend.data_dir());
        let restricted_mode = get("restricted_mode").is_some_and(|v| v == "1");
        backend.set_hide_explicit(hide_explicit, restricted_mode);
        let show_podcasts = get("podcasts").is_some_and(|v| v == "1");
        let show_comments = get("comments").is_some_and(|v| v == "1");
        crate::ytm::set_podcasts(show_podcasts);
        backend.request(Req::Home(None));
        let start_choice = start_choice(&get("start_page").unwrap_or_default());
        let start = start_page(&start_choice, logged_in, show_podcasts);
        // Opening on the Library asks for it below, as visiting the page does.
        let start_asks_library = matches!(start, Page::Library(LibTab::Playlists | LibTab::Albums | LibTab::Artists | LibTab::Podcasts));
        if logged_in {
            if !start_asks_library {
                backend.request(Req::Library);
            }
            backend.request(Req::Account);
            backend.warm_signed_in();
        }
        let transfer_handle = {
            let b = backend.clone();
            crate::transfer::Transfer::new(backend.data_dir(), Arc::new(move || b.wake()))
        };
        let hotkeys = hotkeys::State::new(backend.data_dir());
        let mut app = Self {
            backend,
            audio,
            ch,
            media,
            awake: crate::awake::Awake::spawn(),
            discord: discord_enabled.then(crate::discord::Discord::spawn).flatten(),
            discord_enabled,
            discord_sent: None,
            remote: crate::remote::Remote::new(),
            remote_enabled,
            lastfm_user: crate::lastfm::user(),
            lastfm_busy: Default::default(),
            lastfm_result: Default::default(),
            scrobble: None,
            resume_at: None,
            resume_counts: false,
            listened_at: 0,
            listenbrainz_user: crate::listenbrainz::user(),
            listenbrainz_token: String::new(),
            listenbrainz_busy: Default::default(),
            listenbrainz_result: Default::default(),
            queue_checked: None,
            was_focused: false,
            queue_source: None,
            played_title: None,
            play_pending: None,
            bg,
            page: Page::Home,
            start_page: start_choice,
            back: Vec::new(),
            forward: Vec::new(),
            page_changed: 0.0,
            page_insets: [0.0, 0.0],
            page_scrolled: 0.0,
            video_art_at: 0.0,
            track_art: None,
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
            mood: None,
            search: None,
            search_scope: Default::default(),
            scoped: Vec::new(),
            lists: Lists::default(),
            sorting: HashMap::new(),
            artist: None,
            subscriptions: actions::Subscriptions::default(),
            library: None,
            library_requested: std::time::Instant::now(),
            history: None,
            uploads: None,
            downloads: None,
            downloads_seen: 0,
            download_runs: Vec::new(),
            all_downloaded: HashMap::new(),
            picked_uploads: Default::default(),
            liked: HashSet::new(),
            unliking: HashMap::new(),
            selection: Selection::default(),
            find: Default::default(),
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
            show_video: dev_var("YTM_VIDEO").is_some() || get("show_video").is_some_and(|v| v == "1"),
            video_quality: get("video_quality").and_then(|v| v.parse().ok()).filter(|h| VIDEO_QUALITIES.contains(h)).unwrap_or(720),
            video: None,
            video_frame: None,
            ambient_light: get("ambient_light").is_none_or(|v| v == "1"),
            glow: None,
            animated_covers: std::env::var_os("YTM_CANVAS").is_some() || get("animated_covers").is_some_and(|v| v == "1"),
            cover_clip: None,
            hide_search_videos: get("hide_search_videos").is_some_and(|v| v == "1"),
            hide_explicit,
            show_podcasts,
            show_comments,
            restricted_mode,
            playing_as: None,
            music_videos: Default::default(),
            output_device: get("output_device").filter(|d| !d.is_empty()).map(|id| (id, get("output_device_name").unwrap_or_default())),
            output_devices: Vec::new(),
            updater: None,
            auto_update: get("auto_update").is_none_or(|v| v == "1"),
            // A nightly build stays on nightlies until told otherwise.
            update_nightly: get("update_channel").map_or_else(|| crate::update::is_nightly(&crate::update::build_version()), |v| v == "nightly"),
            close_to_tray: get("close_to_tray").is_some_and(|v| v == "1"),
            wgpu_renderer,
            app_icon,
            tray: None,
            tray_failed: false,
            in_tray: false,
            tray_left_focus: false,
            exiting: false,
            window_hidden: false,
            good_window: get("window"),
            sidebar_collapsed: get("sidebar_collapsed").is_some_and(|v| v == "1"),
            update_checked: None,
            update_announced: false,
            update_seen: get("update_seen"),
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
            ended: false,
            seek_drag: None,
            art_key: None,
            art_changed: -10.0,
            art_brightness: 0.0,
            accent: theme::RED,
            // Dev hook: YTM_THEME=<preset id> tries a theme without saving it.
            // The old "Classic theme" switch is the Classic theme now.
            theme_id: std::env::var("YTM_THEME").ok().or_else(|| get("default_theme").filter(|v| v == "1").map(|_| "classic".to_owned())).or_else(|| get("theme")).unwrap_or_else(|| crate::preset::DEFAULT_ID.to_owned()),
            chrome_applied: None,
            // Dev hook: YTM_APPEARANCE=light|dark|system tries a mode without saving it.
            appearance: theme::Appearance::from_key(&dev_var("YTM_APPEARANCE").map(str::to_owned).or_else(|| get("appearance")).unwrap_or_default()),
            accent_source: AccentSource::from_key(&get("custom_accent").unwrap_or_default()),
            accent_controls: get("accent_controls").is_some_and(|v| v == "1"),
            anim_bg: get("anim_bg").is_none_or(|v| v == "1"),
            glass_on: get("glass_on").is_none_or(|v| v == "1"),
            reactive_bg: get("reactive_bg").is_none_or(|v| v == "1"),
            sponsorblock: get("sponsorblock").is_some_and(|v| v == "1"),
            crossfade: get("crossfade").is_some_and(|v| v == "1"),
            handoff: false,
            crossfade_skips: get("crossfade_skips").is_some_and(|v| v == "1"),
            crossfade_secs: get("crossfade_secs").and_then(|v| v.parse().ok()).unwrap_or(5).clamp(1, CROSSFADE_MAX),
            eq_on: get("eq").is_some_and(|v| v == "1"),
            eq_gains: get("eq_gains").and_then(|v| parse_gains(&v)).unwrap_or_default(),
            eq_no_clip: get("eq_no_clip").is_none_or(|v| v == "1"),
            tempo_on: get("tempo").is_some_and(|v| v == "1"),
            speed: 1.0,
            segments: None,
            segment_skipped: f64::NEG_INFINITY,
            hotkeys,
            bg_time: 0.0,
            last_frame: 0.0,
            search_rect: None,
            search_focused: false,
            search_refocus: false,
            forget_held: false,
            forgetting: None,
            wheel: wheel::Glide::default(),
            np_open: false,
            np_tab: if get("np_art").is_some_and(|v| v == "1") { NpTab::Art } else { NpTab::Lyrics },
            queue_open: false,
            row_drag: None,
            settings_tab: Default::default(),
            settings_query: String::new(),
            // Dev hook: the full screen layout, leaving the window where it is.
            fullscreen: dev_var("YTM_FAKE_FULLSCREEN").is_some(),
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
        app.queue.hide_explicit = hide_explicit;
        app.find.hide_explicit = hide_explicit;
        app.find.sorts = get("list_sorts").and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default();
        app.request_liked(false);
        // Settings › Open on: in place, not a visit (nothing to go back to), and without
        // the page transition.
        if start != Page::Home {
            app.page = start;
            app.page_loaded();
            app.page_changed = 0.0;
        }
        // Dev hooks: YTM_SMOKE="query" searches and plays the first song ("id:<videoId>[,<videoId>…]"
        // plays that video; with YTM_VIDEO, shown as a video); YTM_SEARCH
        // only searches; YTM_PAGE opens a page; YTM_NOWPLAYING opens the overlay.
        if let Some(s) = get(SESSION_KEY).filter(|_| std::env::var_os("YTM_SMOKE").is_none()) {
            app.restore_session(&s);
        }
        let smoke = std::env::var("YTM_SMOKE").ok().filter(|q| !q.is_empty());
        if let Some(ids) = smoke.as_deref().and_then(|q| q.strip_prefix("id:")) {
            // "id:a,b,c" queues several.
            let track = |id: &str| Track { id: id.to_owned(), title: id.to_owned(), music_video: dev_var("YTM_VIDEO").is_some(), ..Default::default() };
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
            Ok("artists") => app.go(Page::Library(LibTab::Artists)),
            Ok("uploads") => app.go(Page::Library(LibTab::Uploads)),
            Ok("liked") => app.go(Page::Library(LibTab::Songs)),
            Ok("history") => app.go(Page::Library(LibTab::History)),
            Ok("downloads") => app.go(Page::Library(LibTab::Downloads)),
            Ok("settings") => app.go(Page::Settings),
            Ok("transfer") => app.go(Page::Transfer),
            Ok("jam") => app.go(Page::Jam),
            Ok(p) if p.starts_with("album:") => app.go(Page::Album(p[6..].to_owned())),
            Ok(p) if p.starts_with("mood:") => app.go(Page::Mood(p[5..].to_owned())),
            Ok(p) if p.starts_with("artist:") => app.go(Page::Artist(p[7..].to_owned())),
            Ok(p) if p.starts_with("playlist:") => app.go(Page::Playlist(p[9..].to_owned())),
            Ok(p) if p.starts_with("search:") => app.go(Page::Search(p[7..].to_owned())),
            _ => {}
        }
        // YTM_UPLOAD=<file> uploads that file as if it had been picked.
        if let Some(f) = std::env::var_os("YTM_UPLOAD") {
            *app.picked_uploads.lock().unwrap() = Some(vec![f.into()]);
        }
        // Dev hook: YTM_PROXY_INPUT=<address> opens Settings › Network on Custom with it typed in.
        if let Some(v) = dev_var("YTM_PROXY_INPUT") {
            app.settings_tab = pages::SettingsTab::General;
            app.proxy_mode = crate::proxy::Setting::Custom(String::new());
            app.proxy_url = v.to_owned();
        }
        if std::env::var("YTM_NOWPLAYING").is_ok() {
            app.np_open = true;
        }
        // Dev hook: YTM_NP_TAB=comments|upnext|art opens that panel.
        match std::env::var("YTM_NP_TAB").as_deref() {
            Ok("comments") => app.np_tab = NpTab::Comments,
            Ok("upnext") => app.np_tab = NpTab::UpNext,
            Ok("art") => app.np_tab = NpTab::Art,
            _ => {}
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
        app.set_eq();
        app
    }

    // ---------------------------------------------------------------- navigation

    fn go(&mut self, page: Page) {
        // An episode's show stands where a song's artist does; it opens as the playlist it is.
        let page = match page {
            Page::Artist(id) if crate::ytm::list_id(&id) != id => Page::Playlist(id),
            p => p,
        };
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
    /// it is until the new data arrives. Unlike a visit, it also reloads a list kept from
    /// the last few minutes and the search already shown.
    fn refresh(&mut self) {
        let liked_out = self.page == Page::Library(LibTab::Songs) && !self.logged_in;
        if let Some(id) = self.page_list().filter(|_| !liked_out).map(str::to_owned)
            && let Some(s) = self.lists.load(&id, matches!(self.page, Page::Album(_)), false)
        {
            self.start_load(s);
        }
        // The search tab shown; one not in yet is asked for by the page's load.
        if let Page::Search(q) = &self.page {
            match self.scope_for(q) {
                Scope::Everywhere if self.search.as_ref().is_some_and(|s| &s.query == q) => self.backend.request(Req::Search(q.clone())),
                Scope::Everywhere => {}
                scope if self.scoped.iter().any(|r| &r.query == q && r.scope == scope) => self.backend.request(Req::SearchIn { query: q.clone(), scope }),
                _ => {}
            }
        }
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
        self.settings_query.clear();
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
            Page::Mood(p) if self.mood.as_ref().is_none_or(|m| &m.params != p) => Some(Req::Mood(p.clone())),
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
            // On this computer: no request, and the files are read again on each visit.
            Page::Library(LibTab::Downloads) => {
                self.read_downloads();
                self.all_downloaded.clear();
                None
            }
            Page::Library(_) if self.logged_in => Some(Req::Library),
            _ => None,
        };
        // The spinner is only for a page with nothing to show yet.
        let cached = match &self.page {
            Page::Home => self.home.is_some(),
            Page::Explore => self.explore.is_some(),
            Page::Mood(p) => self.mood.as_ref().is_some_and(|m| &m.params == p),
            Page::Album(id) | Page::Playlist(id) => self.opened(id).is_some(),
            Page::Artist(id) => self.artist.as_ref().is_some_and(|a| &a.id == id),
            Page::Library(LibTab::History) => self.history.is_some(),
            Page::Library(LibTab::Uploads) => self.uploads.is_some(),
            Page::Library(LibTab::Songs) => self.opened(LIKED_MUSIC).is_some(),
            Page::Library(LibTab::Downloads) => true,
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
        // The Library or Uploads tab of a search: only asked for once it's picked.
        if let Page::Search(q) = &self.page {
            let q = q.clone();
            self.scoped.retain(|r| r.query == q);
            let scope = self.scope_for(&q);
            if scope != Scope::Everywhere && !self.scoped.iter().any(|r| r.scope == scope) {
                self.loading = true;
                self.backend.request(Req::SearchIn { query: q, scope });
            }
        }
    }

    /// The search tab shown for the search `q`: the one picked on it, while signed in.
    fn scope_for(&self, q: &str) -> Scope {
        match &self.search_scope {
            (on, scope) if on == q && self.logged_in => *scope,
            _ => Scope::Everywhere,
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

    /// Where the "From file" accent colour is read from.
    fn accent_file(&self) -> std::path::PathBuf {
        self.backend.data_dir().join("accent.txt")
    }

    /// Where "From file" reads the rest of the colours from (and the accent, if it's there).
    fn theme_file(&self) -> std::path::PathBuf {
        self.backend.data_dir().join("theme.conf")
    }

    /// The theme file's colours, while "From file" is picked.
    fn file_theme(&self) -> theme::FileTheme {
        if self.accent_source == AccentSource::File { theme::file_theme(&self.theme_file()) } else { theme::FileTheme::default() }
    }

    /// The accent before `accent_color` deepens it for light pages.
    fn base_accent(&self) -> Color32 {
        match self.accent_source {
            AccentSource::Custom(c) => c,
            AccentSource::System => theme::system_accent().unwrap_or_else(|| self.look().accent(self.accent)),
            AccentSource::File => (self.file_theme().accent).or_else(|| theme::file_accent(&self.accent_file())).unwrap_or_else(|| self.look().accent(self.accent)),
            AccentSource::Theme => self.look().accent(self.accent),
        }
    }

    fn accent_color(&self) -> Color32 {
        let c = self.base_accent();
        // A pale cover colour would barely show on light pages: deepen it.
        if theme::light() { theme::deepen(c, theme::bg(), 3.0) } else { c }
    }

    /// Whether pages draw light: Settings › Appearance › Mode, in any theme.
    fn light_pages(&self, ctx: &egui::Context) -> bool {
        // A theme file's background says which it is.
        if let Some(light) = theme::file_light() {
            return light;
        }
        self.appearance.is_light(ctx)
    }

    /// The active theme. Synced once per frame (`sync_look`), so this is a plain index, not a search.
    fn look(&self) -> &'static crate::preset::Preset {
        crate::preset::active()
    }

    fn sync_look(&self) {
        if crate::preset::active().id != self.theme_id {
            crate::preset::set_active(&self.theme_id);
        }
    }

    /// Height of the bottom player area (a floating bar leaves a margin around it).
    /// Back to the cover after a video had the background.
    fn restore_art(&mut self) {
        if self.look().background.video_backdrop
            && let (Some(bg), Some(art)) = (&self.bg, &self.track_art)
        {
            bg.set_live(art.clone());
            self.art_brightness = art.brightness;
        }
    }

    fn bar_height(&self) -> f32 {
        let c = &self.look().chrome;
        if c.player_bar == crate::preset::PlayerBar::Floating { c.bar_height + c.bar_margin[1] } else { c.bar_height }
    }

    fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), -1.0));
    }


}



impl eframe::App for App {
    /// With a window material, nothing under the pages but what they paint.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if self.mini.pill_showing() || crate::backdrop::active() != crate::backdrop::Backdrop::None {
            return [0.0; 4];
        }
        // Opaque: a see-through window (made for the pill) mustn't show the desktop here.
        let [r, g, b, _] = Color32::from_rgba_unmultiplied(12, 12, 12, 180).to_normalized_gamma_f32();
        [r, g, b, 1.0]
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        pace::frame(ctx);
        self.lyric_view.release_hidden(ctx.cumulative_pass_nr());
        let now = ctx.input(|i| i.time);
        self.sync_look();
        self.pump(now);
        // A sorted playlist that loads again in a while, for its new order.
        if let Some(t) = self.sorting.values().filter_map(lists::SortWait::due).reduce(f64::min) {
            ctx.request_repaint_after(Duration::from_secs_f64((t - now).max(0.0)));
        }
        self.skip_segments(ctx, now);
        hotkeys::poll(self, ctx, frame);
        // System media controls and taskbar buttons follow the player however it changed.
        let playing = self.playing() || self.buffering;
        if let Some(m) = &mut self.media {
            // Not for a song shown without loading, while the last one may still be stopping.
            if self.resume_at.is_none() {
                m.sync_length(self.audio.status.stream_duration(self.generation));
            }
            // A song not loaded yet is at its start point; the player may still be stopping.
            m.sync(playing, self.buffering, self.resume_at.unwrap_or_else(|| self.audio.status.position()));
            if let Some(wait) = m.sync_volume(self.audio.volume()) {
                ctx.request_repaint_after(wait);
            }
        }
        // No sleep while playing, and the screen stays on for a video on show: this song's
        // (picture in picture keeps the last one's player for a song after it).
        let video = self.shown_video().is_some_and(|t| {
            self.video.as_ref().is_some_and(|(id, p)| *id == t.id && p.as_ref().is_some_and(|p| !p.failed()))
        });
        if let Some(a) = &mut self.awake {
            a.set(crate::awake::Keep::of(playing, video));
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
        // Dev hook: YTM_FAKE_FULLSCREEN=<secs> lays out as full screen from then on, without touching the window.
        if let Some(t) = dev_var("YTM_FAKE_FULLSCREEN").and_then(|v| v.parse::<f64>().ok()) {
            self.fullscreen = now >= t;
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
        self.tray_window(ctx, frame);
        self.window_hidden = self.in_tray || ctx.input(|i| i.viewport().minimized == Some(true));
        // Nothing takes a video's frames now, while its decoder would go on working in bursts.
        if self.window_hidden || self.mini.busy() {
            self.cover_clip = None;
        }
        // Picture in picture goes on with the video, on the way there and back included.
        if (self.window_hidden || (self.mini.busy() && !mini::video_active())) && self.video.take().is_some() {
            self.video_frame = None;
            self.glow = None;
            self.restore_art();
        }
        self.sync_remote(ctx);
        self.sync_tempo();
        self.sync_discord();
        self.sync_scrobbles();
        self.sync_server_queue(ctx, now);
        self.tick_updates(ctx, now);
        let np = widgets::NowPlaying {
            song: self.current_id().map(str::to_owned),
            // Autoplay moved on from it, say: its play button starts it again.
            source: self.queue_source.clone().filter(|_| !self.queue.plays_elsewhere()),
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
        // reads as smooth on pages; the now-playing view and a page showing the playing
        // row's bars run at 30 fps, and the synced lyrics ask for 60 while they animate
        // (the karaoke sweep must be smooth). Each egui frame re-lays-out the UI, so frame rate is the main CPU cost.
        let playing = self.playing();
        if self.window_hidden {
            // In the tray or minimized: nothing to draw, only the player bookkeeping.
            if playing || self.buffering {
                ctx.request_repaint_after(Duration::from_millis(250));
            }
        } else if self.np_open && playing && !self.mini.busy() {
            pace::repaint_after(ctx, Duration::from_millis(33));
        } else if playing && widgets::equalizer_shown(ctx) {
            // The playing row's bars need a steady, full rate to read as smooth.
            pace::repaint_after(ctx, Duration::from_millis(16));
        } else if playing && self.anim_bg && shell::pages_bg_moves(self.look()) {
            pace::repaint_after(ctx, Duration::from_millis(66));
        } else if playing || self.buffering {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.shortcuts(&ctx);
        crate::shader::set_glass_on(self.glass_on);
        let now = ctx.input(|i| i.time);
        let screen = ctx.content_rect();

        self.sync_look();
        let look = self.look();
        // A spring's overshoot isn't for this: it would say "covered" before it is.
        let np = theme::anim_bool_timed(&ctx, "np-open", self.np_open && self.current().is_some(), look.app.np_open_secs).min(1.0);
        // The pages keep their mode while now playing slides over them. Now playing is dark;
        // once it's most of the way up, so are the player bar and what opens over it.
        theme::set_file_theme(&ctx, self.file_theme());
        let light = self.light_pages(&ctx) && !self.fullscreen;
        let bar_light = light && np < 0.5;
        theme::set_mode(&ctx, light);
        // The mini player, or nothing but the background while the window changes size.
        let layout = self.mini.layout(&ctx);
        // Only the player bar shows the window's material once the pages are covered.
        crate::backdrop::follow(crate::backdrop::shows() && !self.mini.pill_showing(), light && (np < 1.0 || layout != mini::Layout::Full));
        // The installed fonts arrived: the theme's can be loaded now. Not the first frame,
        // which is applying anyway and adds a texture loader.
        if self.chrome_applied.is_some() && theme::fonts_became_ready() {
            self.chrome_applied = Some(String::new());
        }
        if self.chrome_applied.as_deref() != Some(look.id.as_str()) {
            if self.chrome_applied.is_none() {
                ctx.add_texture_loader(Arc::new(crate::images::Textures::default()));
            }
            theme::apply_chrome(&ctx, look);
            self.chrome_applied = Some(look.id.clone());
        }
        let accent = self.accent_color();
        ctx.data_mut(|d| d.insert_temp(egui::Id::unique("accent"), accent));
        // How far each album or playlist being downloaded has got, for its cover's veil.
        let progress: Arc<Vec<(String, f32)>> = Arc::new(self.download_runs.iter().filter(|r| !r.title.is_empty()).map(|r| (r.key.clone(), r.done as f32 / r.total.max(1) as f32)).collect());
        ctx.data_mut(|d| d.insert_temp(egui::Id::unique("download-progress"), progress));
        // The file can change with nothing else going on here.
        if self.accent_source == AccentSource::File {
            ctx.request_repaint_after(Duration::from_secs(2));
        }
        let app_style = &look.app;
        // Once now playing has slid all the way up it hides everything but the player
        // bar, so the pages and the full-screen background under it aren't drawn.
        let covered = !self.fullscreen && np >= 1.0;
        let bar_strip = egui::Rect::from_min_max(egui::pos2(screen.left(), screen.bottom() - self.bar_height()), screen.max);
        titlebar::set_fullscreen(&ctx, self.fullscreen);
        dev::dev_memory(&ctx, now);
        dev::dev_probe(&ctx);
        if layout == mini::Layout::Mini && mini::video_active() {
            mini::video_pip(self, ui, now);
            // Any shape: by every edge and corner, unlike the strip.
            titlebar::resize_edges(&ctx);
            shell::toast(self, &ctx, now);
            self.mini.fade_in(&ctx, now, Color32::BLACK);
            dev_screenshot(&ctx, now);
            return;
        }
        if layout == mini::Layout::Mini && mini::pill_active() {
            mini::pill(self, ui, now);
            dev_screenshot(&ctx, now);
            return;
        }
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
        // Full-screen now playing paints over all of it.
        if self.fullscreen && np >= 1.0 {
            // Not drawn, but now playing's own backdrop runs on the same clock.
            shell::background_clock(self, now);
        } else {
            shell::background(self, ui, screen, now, covered.then_some(bar_strip));
        }
        if !self.fullscreen && np >= 0.5 {
            // What shows around and through the player bar carries on from now playing above it,
            // from the moment the bar turns dark: a light bar keeps the page under it.
            let pages = theme::set_mode(&ctx, false);
            nowplaying::backdrop(self, ui, screen, bar_strip, now, false);
            theme::set_mode(&ctx, pages);
        }
        if !self.fullscreen && !covered {
            // The top of the window, down to the search box's row: what's on it comes after.
            titlebar::drag_area(ui, egui::Rect::from_min_size(screen.min, egui::vec2(screen.width(), titlebar::HEIGHT)), "main");
        }
        let mut floating_slot = None;
        if !self.fullscreen {
            let pages = theme::set_mode(&ctx, bar_light);
            let floating = look.chrome.player_bar == crate::preset::PlayerBar::Floating;
            let bar_alpha = app_style.bar_alpha.clamp(0.0, 1.0);
            let frame = if floating {
                let [x, y] = look.chrome.bar_margin;
                egui::Frame::new().inner_margin(egui::Margin { left: x as i8, right: x as i8, top: 0, bottom: y as i8 })
            } else {
                let fill = match look.chrome.bar_np_alpha.filter(|_| self.np_open) {
                    Some(a) => theme::shade(a),
                    None => shell::bar_fill(look, bar_alpha),
                };
                let mut f = egui::Frame::new().fill(fill);
                if (app_style.bar_border || look.chrome.bar_border_alpha.is_some()) && (look.chrome.bar_np_border || !self.np_open) && !look.chrome.bar_edge_top {
                    f = f.stroke(egui::Stroke::new(1.0, theme::glass(look.chrome.bar_border_alpha.unwrap_or(0.1))));
                }
                f
            };
            egui::Panel::bottom("player").resizable(false)
                .exact_size(self.bar_height())
                .frame(frame)
                .show_separator_line(false)
                .show(ui, |ui| {
                    if bar_light != light {
                        ui.style_mut().visuals = ctx.global_style().visuals.clone();
                    }
                    if floating {
                        // Drawn over the page once that's down, for it to frost.
                        floating_slot = Some(ui.max_rect());
                        return;
                    }
                    shell::player_bar(self, ui, now)
                });
            theme::set_mode(&ctx, pages);
        }
        if !self.fullscreen && !covered {
            use crate::preset::SidebarStyle;
            let side_alpha = app_style.sidebar_alpha.clamp(0.0, 1.0);
            // Clear over the top of a page, filling in as it scrolls.
            let side_alpha = if app_style.nav_item.scroll_fill { side_alpha * (self.page_scrolled / 40.0).clamp(0.0, 1.0) } else { side_alpha };
            // The logo's row is 32 points tall.
            let side_margin = egui::Margin { left: 14, right: 14, top: titlebar::top_margin(16, 32.0), bottom: 16 };
            let border = egui::Stroke::new(if app_style.sidebar_border > 0.0 { 1.0 } else { 0.0 }, theme::glass(app_style.sidebar_border));
            let side_frame = match app_style.sidebar {
                SidebarStyle::Glass => egui::Frame::new().fill(theme::panel(side_alpha)).inner_margin(side_margin),
                SidebarStyle::Transparent => egui::Frame::new().inner_margin(side_margin),
                SidebarStyle::Solid => egui::Frame::new().fill(look.solid()).stroke(border).inner_margin(side_margin),
                SidebarStyle::Floating => egui::Frame::new()
                    .fill(theme::panel(side_alpha.max(0.3)))
                    .stroke(egui::Stroke::new(1.0, theme::glass_panel(0.08)))
                    .corner_radius(egui::CornerRadius::same(theme::panel_radius().max(12)))
                    .outer_margin(egui::Margin { left: 10, right: 4, top: 10, bottom: 10 })
                    .inner_margin(egui::Margin::symmetric(12, 14)),
            };
            egui::Panel::left("nav").resizable(false)
                .exact_size(shell::sidebar_w(self) - (shell::sidebar_w(self) - shell::rail_w(self)) * shell::sidebar_collapse(self, &ctx))
                .frame(side_frame)
                .show_separator_line(false)
                .show(ui, |ui| {
                    if let (Some([a, px]), Some(bg)) = (app_style.nav_item.cover_tint.filter(|_| !light), self.bg.as_ref()) {
                        // The cover's blur showing through the top of the sidebar, fading out.
                        let r = ui.max_rect().expand2(egui::vec2(14.0, 0.0));
                        let band = egui::Rect::from_min_size(egui::pos2(r.left(), screen.top()), egui::vec2(r.width(), px));
                        let b = &look.background;
                        bg.paint(&ui.painter().with_clip_rect(band), screen, crate::shader::Params { time: 0.0, blend: 1.0, intensity: 0.0, saturation: b.saturation, dim: a, scale: b.scale, contrast: 1.0, dither: b.dither, rotate: 0.0, alpha: 1.0 });
                        shell::vgradient(ui.painter(), band, Color32::TRANSPARENT, shell::base_color(self));
                    }
                    shell::sidebar(self, ui)
                });
            let q = theme::anim_bool(&ctx, "queue-drawer", self.queue_open && !self.np_open, 0.25).min(1.0);
            if q > 0.01 && screen.width() < 1000.0 {
                // No room to give it without crushing the page: it floats over that instead.
                let r = egui::Rect::from_min_size(egui::pos2(screen.right() - 340.0 * q, screen.top()), egui::vec2(340.0, screen.height() - self.bar_height()));
                egui::Area::new(egui::Id::unique("queue-float")).order(egui::Order::Middle).fixed_pos(r.min).show(&ctx, |ui| {
                    ui.allocate_rect(r, egui::Sense::click_and_drag());
                    ui.painter().rect_filled(r, 0.0, theme::panel(0.94));
                    // Its top is still the window's top: the window moves by it, around the heading.
                    titlebar::drag_area(ui, egui::Rect::from_min_size(r.min, egui::vec2(r.width(), titlebar::HEIGHT)), "queue-float");
                    let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink2(egui::vec2(14.0, 16.0))));
                    inner.set_clip_rect(r);
                    shell::queue_drawer(self, &mut inner);
                });
            } else if q > 0.01 {
                egui::Panel::right("queue").resizable(false)
                    .exact_size(340.0 * q)
                    .frame(egui::Frame::new().fill(theme::panel(0.42)).inner_margin(egui::Margin::symmetric(14, 16)))
                    .show_separator_line(false)
                    .show(ui, |ui| shell::queue_drawer(self, ui));
            }
            let sheet = app_style.sheet.map_or((Color32::TRANSPARENT, 0), |[a, r]| (theme::glass(a), r as u8));
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(sheet.0).corner_radius(egui::CornerRadius { nw: sheet.1, ne: 0, sw: 0, se: 0 }).inner_margin(egui::Margin { left: 28, right: 0, top: titlebar::top_margin(14, 42.0), bottom: 0 }))
                .show(ui, |ui| {
                    let glass = crate::shader::glass_ready() && !light;
                    let under_top = glass && app_style.glass.top_bar.is_some();
                    let under_bar = glass && app_style.glass.bar.is_some() && floating_slot.is_some();
                    // The page is one place in either arrangement, so where it's scrolled to (and
                    // the rest of what it remembers) stays when a theme switches between them.
                    let page_id = egui::Id::unique("main-page");
                    if !under_top && !under_bar {
                        self.page_insets = [0.0, 0.0];
                        shell::top_bar(self, ui);
                        ui.add_space(6.0);
                        ui.scope_builder(egui::UiBuilder::new().scope_id(page_id), |ui| pages::page(self, ui, now));
                        return;
                    }
                    // The page first, running under the glass, then what's over it.
                    let panel = ui.max_rect();
                    let row_h = app_style.search_look.height.max(42.0) + 6.0;
                    let top = if under_top { screen.top() } else { panel.top() + row_h };
                    let bottom = if under_bar { screen.bottom() } else { panel.bottom() };
                    let page_rect = egui::Rect::from_min_max(egui::pos2(panel.left(), top), egui::pos2(panel.right(), bottom));
                    self.page_insets = [if under_top { panel.top() + row_h - screen.top() } else { 0.0 }, if under_bar { self.bar_height() } else { 0.0 }];
                    let mut page_ui = ui.new_child(egui::UiBuilder::new().scope_id(page_id).max_rect(page_rect));
                    page_ui.set_clip_rect(page_rect);
                    pages::page(self, &mut page_ui, now);
                    if under_top {
                        // Dragging the window still works over what's scrolled up there.
                        titlebar::drag_area(ui, egui::Rect::from_min_size(screen.min, egui::vec2(screen.width(), titlebar::HEIGHT)), "main-over");
                    }
                    let mut row = ui.new_child(egui::UiBuilder::new().max_rect(panel));
                    row.set_clip_rect(egui::Rect::from_min_max(screen.min, panel.max));
                    shell::top_bar(self, &mut row);
                });
        }
        if let Some(r) = floating_slot {
            let mut bar = ui.new_child(egui::UiBuilder::new().max_rect(r));
            bar.set_clip_rect(screen);
            let pages = theme::set_mode(&ctx, bar_light);
            if bar_light != light {
                bar.style_mut().visuals = ctx.global_style().visuals.clone();
            }
            shell::floating_bar(self, &mut bar, r, now);
            theme::set_mode(&ctx, pages);
        }
        if np > 0.001 {
            theme::set_mode(&ctx, false);
            nowplaying::overlay(self, &ctx, now);
        } else if self.cover_clip.take().is_some() || self.video.take().is_some() {
            // Now playing's closed, and its own cleanup only runs while it's drawn.
            self.video_frame = None;
            self.glow = None;
            self.restore_art();
        }
        theme::set_mode(&ctx, bar_light);
        self.draw_dialog(&ctx);
        hotkeys::capture_dialog(self, &ctx);
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
        // Next frame's root starts from the pages' mode.
        theme::set_mode(&ctx, light);
    }

    /// Dev hook: `YTM_FAKE_POINTER=x,y` parks a synthetic pointer there, for testing
    /// hover behaviour without touching the real mouse.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        dev::dev_script(ctx, raw);
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
        // Dev hook: YTM_LOG_WHEEL=1 logs the wheel events the window sends.
        if dev_var("YTM_LOG_WHEEL").is_some() {
            for e in raw.events.iter().filter(|e| matches!(e, egui::Event::MouseWheel { .. })) {
                log::info!("wheel t={:.3} {e:?}", raw.time.unwrap_or(0.0));
            }
        }
        let viewport_h = raw.screen_rect.map_or(800.0, |r| r.height());
        if self.wheel.filter(&mut raw.events, raw.time.unwrap_or(0.0), raw.predicted_dt, viewport_h) {
            ctx.request_repaint();
        }
        frame_pace(raw);
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
        storage.set_string("show_video", b(self.show_video));
        storage.set_string("video_quality", self.video_quality.to_string());
        storage.set_string("ambient_light", b(self.ambient_light));
        storage.set_string("animated_covers", b(self.animated_covers));
        storage.set_string("hide_search_videos", b(self.hide_search_videos));
        storage.set_string("hide_explicit", b(self.hide_explicit));
        storage.set_string("list_sorts", serde_json::to_string(&self.find.sorts).unwrap_or_default());
        storage.set_string("podcasts", b(self.show_podcasts));
        storage.set_string("comments", b(self.show_comments));
        storage.set_string("restricted_mode", b(self.restricted_mode));
        let folder = crate::downloads::folder();
        storage.set_string("download_dir", if folder == crate::downloads::default_folder() { String::new() } else { folder.display().to_string() });
        let (device, device_name) = self.output_device.clone().unzip();
        storage.set_string("output_device", device.unwrap_or_default());
        storage.set_string("output_device_name", device_name.unwrap_or_default());
        storage.set_string("auto_update", b(self.auto_update));
        storage.set_string("update_channel", if self.update_nightly { "nightly" } else { "stable" }.to_owned());
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
        let start = START_PAGES.iter().find(|(.., p)| *p == self.start_page).map_or("home", |(k, ..)| k);
        storage.set_string("start_page", start.to_owned());
        storage.set_string("sidebar_collapsed", b(self.sidebar_collapsed));
        // Pretend versions (dev hook) mustn't decide the real "what's new".
        if std::env::var_os("YTM_UPDATE_AS").is_none() {
            storage.set_string("last_version", crate::update::current_version());
        }
        if let Some(v) = &self.update_seen {
            storage.set_string("update_seen", v.clone());
        }
        storage.set_string("discord_enabled", b(self.discord_enabled));
        storage.set_string("remote_enabled", b(self.remote_enabled));
        storage.set_string("default_theme", b(false));
        if dev_var("YTM_APPEARANCE").is_none() {
            storage.set_string("appearance", self.appearance.key().to_owned());
            storage.set_string("custom_accent", self.accent_source.key());
        }
        if std::env::var_os("YTM_THEME").is_none() {
            storage.set_string("theme", self.theme_id.clone());
        }
        if dev_var("YTM_FONT").is_none() {
            storage.set_string("font", theme::font_choice());
        }
        storage.set_string("accent_controls", b(self.accent_controls));
        storage.set_string("anim_bg", b(self.anim_bg));
        storage.set_string("glass_on", b(self.glass_on));
        storage.set_string("reactive_bg", b(self.reactive_bg));
        storage.set_string("sponsorblock", b(self.sponsorblock));
        storage.set_string("crossfade", b(self.crossfade));
        storage.set_string("crossfade_secs", self.crossfade_secs.to_string());
        storage.set_string("crossfade_skips", b(self.crossfade_skips));
        storage.set_string("eq", b(self.eq_on));
        storage.set_string("eq_gains", self.eq_gains.map(|g| g.to_string()).join(","));
        storage.set_string("eq_no_clip", b(self.eq_no_clip));
        storage.set_string("tempo", b(self.tempo_on));
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
            crate::shader::destroy_glass(gl);
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

/// The heights offered for music videos. Decoding is on the CPU: 720p is easy, 1080p takes
/// about three times as much.
const VIDEO_QUALITIES: [u32; 4] = [1080, 720, 480, 360];

/// The pointer moving wakes a frame per event, up to the monitor's rate, and each of those
/// asks for more: frames wait for 60 fps unless the listener is doing something that wants
/// every frame (clicking, typing, scrolling).
fn frame_pace(raw: &egui::RawInput) {
    use egui::Event as E;
    static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    let mut last = LAST.lock().unwrap();
    let acting = raw.events.iter().any(|e| matches!(e, E::PointerButton { .. } | E::MouseWheel { .. } | E::Key { .. } | E::Text(_) | E::Ime(_) | E::Zoom(_) | E::Touch { .. }));
    if !acting && let Some(wait) = last.and_then(|l| Duration::from_millis(16).checked_sub(l.elapsed())) {
        std::thread::sleep(wait);
    }
    *last = Some(std::time::Instant::now());
}

/// Start this exe again once this process has saved and exited.
fn relaunch() {
    let Ok(exe) = std::env::current_exe() else { return };
    log::info!("restarting");
    crate::update::start_after_exit(&exe);
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
    /// The song version that was playing for the current music video (or the music video
    /// for the current song): the position is its.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    playing_as: Option<Track>,
}

/// The song version playing in place of the music video `listed`, in play `generation`;
/// or the music video playing in place of the song `listed`, picked on its switch.
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
    if maximized != Some(true) && too_big(outer.size(), monitor) {
        log::info!("window {outer:?} exceeds monitor {monitor:?}; maximizing");
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
    }
}

/// Whether a window of `size` can't fit on a monitor of `monitor` size. Sizes only: the
/// window's position is on the whole desktop, so a window on a monitor other than the
/// one at its origin (0, 0) isn't off-screen. (eframe pulls a saved one back onto a
/// monitor on Windows and drops an off-screen one on a Mac; on X11 that's the window manager's.)
fn too_big(size: egui::Vec2, monitor: egui::Vec2) -> bool {
    // A little slack for the invisible resize borders Windows adds around the frame.
    size.x > monitor.x + 24.0 || size.y > monitor.y + 24.0
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

/// Whether this is the installed or the portable app, or the .deb or the Arch package
/// (which only say a new version is out): updates only apply there. It
/// can't change while running, and the check touches the disk, so it runs once.
fn updatable() -> bool {
    static UPDATABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *UPDATABLE.get_or_init(|| crate::update::installed() || crate::update::portable_dir().is_some() || crate::update::packaged())
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
        assert!(!s.queue[0].music_video && !s.queue[0].explicit && s.playing_as.is_none());
        // A song (or a session without a song version) saves as before, so an older
        // version, or a jam guest on one, reads it too.
        let saved = serde_json::to_string(&s).unwrap();
        assert!(!saved.contains("music_video") && !saved.contains("explicit") && !saved.contains("playing_as"), "{saved}");
        let video = Track { music_video: true, explicit: true, ..s.queue[0].clone() };
        let back: Track = serde_json::from_str(&serde_json::to_string(&video).unwrap()).unwrap();
        assert!(back.music_video && back.explicit);
    }
}

#[cfg(test)]
mod window_tests {
    use super::too_big;
    use egui::vec2;

    #[test]
    fn only_a_window_larger_than_its_monitor_is_maximized_at_start() {
        let monitor = vec2(1920.0, 1080.0);
        // A floating window, wherever it is on the desktop (#99: on a second monitor it was
        // taken for off-screen, so the app opened maximized and saved itself that way).
        assert!(!too_big(vec2(1216.0, 819.0), monitor));
        // Within the slack, or a pixel past the screen's edge (quit in video full screen).
        assert!(!too_big(vec2(1935.0, 1090.0), monitor));
        assert!(!too_big(vec2(1920.0, 1081.0), monitor));
        // Saved on a bigger monitor.
        assert!(too_big(vec2(2576.0, 1408.0), monitor));
        assert!(too_big(vec2(1200.0, 1200.0), monitor));
    }
}

#[cfg(test)]
mod eq_tests {
    use super::parse_gains;

    #[test]
    fn stored_gains_read_back_or_fall_back_to_flat() {
        let g = [1.5, -2.25, 0.0, 12.0, -12.0, 0.1, 0.0, 3.0, 0.0, -0.5];
        assert_eq!(parse_gains(&g.map(|g| g.to_string()).join(",")), Some(g));
        assert_eq!(parse_gains("40,-40,0,0,0,0,0,0,0,0").map(|g| (g[0], g[1])), Some((12.0, -12.0)));
        for bad in ["", "0,0,0,0,0,0,0,0,0", "0,0,0,0,0,0,0,0,0,0,0", "NaN,0,0,0,0,0,0,0,0,0", "inf,0,0,0,0,0,0,0,0,0", "x,0,0,0,0,0,0,0,0,0"] {
            assert_eq!(parse_gains(bad), None, "{bad}");
        }
    }
}

#[cfg(test)]
mod start_page_tests {
    use super::*;

    #[test]
    fn opens_on_the_picked_page_or_home_when_it_cant_show() {
        for (key, _, page) in START_PAGES {
            assert_eq!(start_choice(key), page, "{key}");
        }
        assert_eq!(start_choice(""), Page::Home);
        assert_eq!(start_choice("nowhere"), Page::Home);
        let lib = |t| Page::Library(t);
        assert_eq!(start_page(&lib(LibTab::Playlists), true, false), lib(LibTab::Playlists));
        assert_eq!(start_page(&lib(LibTab::Playlists), false, false), Page::Home);
        assert_eq!(start_page(&lib(LibTab::Downloads), false, false), lib(LibTab::Downloads));
        assert_eq!(start_page(&Page::Explore, false, false), Page::Explore);
        assert_eq!(start_page(&lib(LibTab::Podcasts), true, false), Page::Home);
        assert_eq!(start_page(&lib(LibTab::Podcasts), true, true), lib(LibTab::Podcasts));
    }
}
