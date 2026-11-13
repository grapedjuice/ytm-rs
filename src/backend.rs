//! Network side: a small tokio runtime talking to YouTube Music (`ytmapi`/`ytm`).
//!
//! The UI sends `Req`s and drains `Resp`s once per frame; every response wakes the UI.
//! Playback resolution also lives here: resolve stream → start progressive download →
//! build the decoder on a blocking thread → hand it to the audio engine.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crossbeam_channel::Sender;
use tokio::runtime::Runtime;

use crate::art::Art;
use crate::audio::{AudioCmd, AudioHandle};
use crate::lyrics::{self, LyricsSet};
use crate::model::{self, Card, Target, Thumb, Track, sized};
use crate::stream::{self, Shared};
use crate::{innertube, jsc, webmusic, ytdlp, ytm, ytmapi};

pub enum Req {
    /// Home feed, optionally filtered by a mood chip's params.
    Home(Option<String>),
    HomeMore(String),
    MoreQuickPicks(String),
    Explore(Vec<String>),
    /// A Moods & genres category, by its browse params.
    Mood(String),
    Search(String),
    /// Suggestions for a query; `round` comes back with them, so stale ones can be dropped.
    Suggest { query: String, round: u64 },
    /// Remove a past search (its token) from the account's history.
    ForgetSearch(String),
    /// `load` comes back with the album, so an older answer can't replace a newer one.
    Album { id: String, load: u64 },
    Artist(String),
    /// `stream`: send the first page as soon as it's in and the rest as they come,
    /// rather than all of it at the end.
    Playlist { id: String, load: u64, stream: bool },
    Library,
    History,
    /// Songs the account uploaded to YouTube Music.
    Uploads,
    /// The signed-in account's name and photo.
    Account,
    /// The channels the signed-in Google account can act as.
    Channels,
    /// Act as another of them from now on: a brand channel's page id, or `None` for the
    /// account's own channel. `epoch`: the list's, picked from.
    SwitchChannel { page_id: Option<String>, epoch: u64 },
    /// The queue another device left off with (YouTube Music's own cross-device queue).
    ServerQueue,
    /// Upload audio files to the account's library.
    Upload(Vec<PathBuf>),
    /// Save a song's audio to the downloads folder. `for_video`: the music video it's the
    /// song version of, which then plays it from the file (offline too).
    Download { track: Track, for_video: Option<String> },
    Lyrics(Track),
    /// SponsorBlock's parts of a video to skip.
    Segments { video_id: String },
    /// Romanization and translation for the lyric lines of one source.
    LyricsExtras { video_id: String, source: String, lines: Vec<String>, lang: Option<String>, target: String },
    /// Process cover art for the background; `key` is echoed back.
    Art { key: String, url: String },
    /// Radio seeded from a track (`seed`, its id); appended to the queue. `song_of`: the
    /// track, when it's a music video to play as its song: the song's radio is asked for.
    Radio { seed: String, song_of: Option<Track> },
    /// A song's first page of comments (now playing's Comments panel).
    Comments { video_id: String },
    /// The next page of a comments list (more comments, a comment's replies, another
    /// sort), from its `next` continuation token.
    CommentsPage { token: String },
    /// A comment call as the account: `comment/create_comment`, `comment/create_comment_reply`
    /// or `comment/perform_comment_action` (like, unlike). `tag` comes back as the
    /// `CommentsPage` token.
    CommentAction { endpoint: String, body: serde_json::Value, tag: String },
    /// Fetch an album/playlist and play it from the start.
    PlayCollection { target: Target },
    /// `at`: start position (non-zero when resuming last session's song). `report`: tell
    /// YouTube about the play (not when picking up a song already counted). `song_of`:
    /// the track, when it's a music video to play as its song version if one is found.
    /// `song`: the song version already playing for the music video `video_id` (picked up
    /// mid-song): played without a lookup, and the video if it won't.
    Play { video_id: String, generation: u64, at: std::time::Duration, report: bool, song_of: Option<Track>, song: Option<Track> },
    /// A video's stream, for showing it: the tallest at most `max_height`.
    VideoStream { video_id: String, max_height: u32 },
    /// The animated cover standing in for a song's cover, if it has one.
    Canvas(Track),
    /// Resolve and download a track ahead of time so skipping to it is instant.
    Prefetch { video_id: String, song_of: Option<Track> },
    Like { video_id: String, like: bool },
    /// Take away the likes of a music video and the song version standing in for it.
    UnlikeBoth { song: String, video: String },
    /// Several likes (or unlikes) one after another, for a selection.
    LikeMany { video_ids: Vec<String>, like: bool },
    Dislike(String),
    CreatePlaylist { title: String, video_ids: Vec<String> },
    EditPlaylist { id: String, title: String, description: String, privacy: Option<String> },
    AddToPlaylist { playlist_id: String, video_ids: Vec<String> },
    /// `(videoId, setVideoId)` entries to take out of the playlist.
    RemoveFromPlaylist { playlist_id: String, items: Vec<(String, String)> },
    /// Move an entry to just before another, or to the end.
    MoveInPlaylist { playlist_id: String, move_id: u64, set_id: String, before: Option<String> },
    DeletePlaylist(String),
    /// A playlist's collaboration settings and invite link.
    Collaboration(String),
    SetCollaboration { info: ytm::Collaboration, change: ytm::CollabChange },
    SaveCollection { id: String, save: bool },
    SetCookie(String),
    Logout,
}

impl Req {
    fn name(&self) -> &'static str {
        match self {
            Req::Home(_) | Req::HomeMore(_) => "Loading home",
            Req::MoreQuickPicks(_) => "Loading more picks",
            Req::Explore(_) => "Loading explore",
            Req::Mood(_) => "Loading the category",
            Req::Search(_) => "Search",
            Req::Suggest { .. } => "Suggest",
            Req::ForgetSearch(_) => "Removing the search",
            Req::Album { .. } => "Loading album",
            Req::Artist(_) => "Loading artist",
            Req::Playlist { .. } => "Loading playlist",
            Req::Library => "Loading library",
            Req::History => "Loading history",
            Req::Uploads => "Loading uploads",
            Req::Account => "Account",
            Req::Channels => "Channels",
            Req::SwitchChannel { .. } => "Switching channel",
            Req::ServerQueue => "Queue from other devices",
            Req::Upload(_) => "Upload",
            Req::Download { .. } => "Download",
            Req::Lyrics(_) => "Lyrics",
            Req::Segments { .. } => "SponsorBlock",
            Req::LyricsExtras { .. } => "Translation",
            Req::Art { .. } => "Art",
            Req::Radio { .. } => "Loading radio",
            Req::Comments { .. } | Req::CommentsPage { .. } | Req::CommentAction { .. } => "Comments",
            Req::PlayCollection { .. } => "Playback",
            Req::Play { .. } => "Playback",
            Req::Prefetch { .. } => "Prefetch",
            Req::VideoStream { .. } => "Video",
            Req::Canvas(_) => "Animated cover",
            Req::Like { .. } | Req::LikeMany { .. } | Req::UnlikeBoth { .. } => "Like",
            Req::Dislike(_) => "Dislike",
            Req::CreatePlaylist { .. } => "Create playlist",
            Req::EditPlaylist { .. } => "Edit playlist",
            Req::AddToPlaylist { .. } => "Add to playlist",
            Req::RemoveFromPlaylist { .. } => "Remove from playlist",
            Req::MoveInPlaylist { .. } => "Move in playlist",
            Req::DeletePlaylist(_) => "Delete playlist",
            Req::Collaboration(_) => "Loading collaboration",
            Req::SetCollaboration { .. } => "Collaboration",
            Req::SaveCollection { .. } => "Save to library",
            Req::SetCookie(_) => "Sign in",
            Req::Logout => "Sign out",
        }
    }

    /// Background requests fail quietly (logged, no toast).
    fn quiet(&self) -> bool {
        matches!(self, Req::Suggest { .. } | Req::Account | Req::Channels | Req::ServerQueue | Req::Art { .. } | Req::Prefetch { .. } | Req::HomeMore(_) | Req::Lyrics(_) | Req::Segments { .. } | Req::LyricsExtras { .. } | Req::Comments { .. } | Req::CommentsPage { .. } | Req::CommentAction { .. })
    }
}

pub struct Explore {
    pub new_albums: Vec<Card>,
    pub charts: Vec<Card>,
    pub top: Vec<Track>,
    /// Moods & genres: the categories it opens.
    pub moods: Vec<ytm::MoodGroup>,
}

pub struct SearchResults {
    pub query: String,
    pub tracks: Vec<Track>,
    /// Videos, including uploads that have no song version (remixes, live sets).
    pub videos: Vec<Track>,
    pub albums: Vec<Card>,
    pub artists: Vec<Card>,
    pub playlists: Vec<Card>,
    /// Podcast shows and episodes, when podcasts are shown.
    pub podcasts: Vec<Card>,
    pub episodes: Vec<Track>,
}

/// An album or playlist page.
pub struct Collection {
    pub id: String,
    pub is_album: bool,
    /// Album's playlist ID for saving it to the account library.
    pub save_id: Option<String>,
    pub title: String,
    pub subtitle: String,
    /// Album artists / playlist owner named in the subtitle, clickable.
    pub artists: Vec<model::Link>,
    pub description: Option<String>,
    pub thumbs: Vec<Thumb>,
    pub tracks: Vec<Track>,
    /// Each track's playlist entry id (`setVideoId`), for removing it; empty for albums.
    pub set_ids: Vec<Option<String>>,
    /// How many songs the playlist's header counts (unplayable ones too, so it can be more
    /// than `tracks`); kept in step with removals and unlikes made here.
    pub count: Option<u64>,
    /// The account's own playlist (editable), not one saved from someone else.
    pub owned: bool,
}

impl Collection {
    /// A podcast show: its page lists episodes.
    pub fn is_show(&self) -> bool {
        ytm::list_id(&self.id) != self.id
    }

    /// "songs", or what this lists instead.
    pub fn things(&self) -> &'static str {
        if self.is_show() { "episodes" } else { "songs" }
    }

    /// The subtitle, with the song count for a playlist.
    pub fn byline(&self) -> String {
        let things = self.things();
        match self.count {
            Some(n) if self.subtitle.is_empty() => format!("{n} {things}"),
            Some(n) => format!("{} • {n} {things}", self.subtitle),
            None => self.subtitle.clone(),
        }
    }

    /// Move row `from` to the gap `gap` between rows (0 is above the first), both counted
    /// before the move. Returns the entry moved and the one now after it (`None` at the
    /// end), which is how YouTube Music takes a move; `None`, with nothing moved, when
    /// they're out of range or it didn't say which entries they are.
    pub fn move_row(&mut self, from: usize, gap: usize) -> Option<(String, Option<String>)> {
        let n = self.tracks.len();
        if from >= n || gap > n || self.set_ids.len() != n {
            return None;
        }
        let entry = self.set_ids[from].clone()?;
        let next = if gap < n { Some(self.set_ids[gap].clone()?) } else { None };
        let to = if gap > from { gap - 1 } else { gap };
        let (t, set) = (self.tracks.remove(from), self.set_ids.remove(from));
        self.tracks.insert(to, t);
        self.set_ids.insert(to, set);
        Some((entry, next))
    }

    /// Take rows out (sorted indices), and out of the count.
    pub fn remove_rows(&mut self, rows: &[usize]) {
        for &i in rows.iter().rev() {
            if i < self.tracks.len() {
                self.tracks.remove(i);
                if i < self.set_ids.len() {
                    self.set_ids.remove(i);
                }
                self.count = self.count.map(|n| n.saturating_sub(1));
            }
        }
    }
}

pub struct ArtistPage {
    pub id: String,
    pub name: String,
    pub banner: Vec<Thumb>,
    pub subscribers: Option<u64>,
    pub description: Option<String>,
    pub top: Vec<Track>,
    /// "Top songs" playlist behind the shelf's Show all button.
    pub top_playlist: Option<String>,
    pub albums: Vec<Card>,
    pub playlists: Vec<Card>,
    pub similar: Vec<Card>,
}

pub struct Library {
    pub playlists: Vec<Card>,
    pub albums: Vec<Card>,
    /// Saved podcast shows, when podcasts are shown.
    pub podcasts: Vec<Card>,
    /// That half didn't load (YouTube Music refused it, say): the one shown stays.
    pub playlists_failed: bool,
    pub albums_failed: bool,
}

/// The signed-in account's channels.
pub struct Channels {
    pub list: Vec<ytm::Channel>,
    /// The page id of the one requests go as (`None`: the account's own).
    pub current: Option<String>,
    /// [`ytmapi::Api::epoch`] when listed: a switch picked from here applies only then.
    pub epoch: u64,
}

pub enum Resp {
    Home(ytm::HomePage),
    HomeMore(ytm::HomePage),
    QuickPicksMore { seed: String, tracks: Vec<Track> },
    Explore(Explore),
    Mood(ytm::MoodPage),
    Search(SearchResults),
    /// `None`: this video can't be shown; the picture stays.
    VideoStream { video_id: String, max_height: u32, stream: Option<innertube::VideoStream> },
    /// `clip`: the HLS playlist of the song's animated cover; `None`: it has none.
    Canvas { video_id: String, clip: Option<String> },
    Suggest { query: String, round: u64, terms: Vec<ytm::Suggestion> },
    /// An album or playlist; `more`: the rest of its songs follow in `CollectionMore`.
    Collection { load: u64, c: Collection, more: bool },
    /// The next page of a playlist sent as `Collection` with the same `load`.
    CollectionMore { id: String, load: u64, tracks: Vec<Track>, set_ids: Vec<Option<String>>, more: bool, failed: bool },
    Artist(ArtistPage),
    Library(Library),
    History(Vec<Track>),
    Uploads(Vec<Track>),
    Account { name: Option<String>, photo: Option<String> },
    Channels(Channels),
    /// A `Req::SwitchChannel` is done (requests go as `epoch` now), or `error` says why it isn't.
    SwitchedChannel { page_id: Option<String>, epoch: u64, error: Option<String> },
    ServerQueue(ytm::ServerQueue),
    /// Upload finished: how many files went up, and which failed.
    Uploaded { done: usize, failed: Vec<String> },
    Downloaded { title: String, path: PathBuf },
    /// Every source's lyrics for a song (empty when nobody has them).
    Lyrics { video_id: String, lyrics: LyricsSet },
    Segments { video_id: String, segments: Vec<crate::sponsorblock::Segment> },
    /// Romanization/translation for one source's lines, arriving after them.
    LyricsExtras { video_id: String, source: String, target: String, extras: crate::translate::Extras },
    Art { key: String, art: Art },
    Radio { seed: String, tracks: Vec<Track> },
    /// A comments response (`Req::Comments`, `CommentsPage` or `CommentAction`) by its tag;
    /// `None`: it failed.
    CommentsPage { token: String, page: Option<serde_json::Value> },
    /// What was asked to play (a `Req::PlayCollection`), its name, and its songs.
    PlayQueue { target: Target, title: String, tracks: Vec<Track> },
    /// `other`: its other version, whose like went with this one.
    Liked { video_id: String, like: bool, other: Option<String> },
    /// A run of likes or unlikes: `taken` just went through, `done` of `total` tried.
    LikeProgress { taken: Vec<String>, done: usize, total: usize, like: bool },
    /// The songs YouTube Music took, and the ones it refused.
    LikedMany { video_ids: Vec<String>, failed: Vec<String>, like: bool, others: Vec<String> },
    PlaylistCreated { id: String, songs: usize },
    PlaylistChanged { id: String, message: &'static str },
    /// Songs weren't taken out of this playlist, though they've left it here already.
    RemoveFailed { id: String, error: String },
    AddedToPlaylist { id: String, video_ids: Vec<String>, error: Option<String> },
    /// YouTube Music answered a song dragged to a new place.
    PlaylistMoved { id: String, move_id: u64, ok: bool },
    Collaboration(ytm::Collaboration),
    /// Playback for `generation` failed before reaching the audio engine.
    PlayError { generation: u64, msg: String },
    /// Play `generation` is the song version of the music video that was asked for
    /// (`None`: that song wouldn't start, and the music video plays after all).
    PlayingAs { generation: u64, listed: String, song: Option<Track> },
    LoggedIn(bool),
    /// A past search left the account's history; the list already shows it gone.
    SearchForgotten,
    Error(String),
}

impl Resp {
    /// An answer that belongs to the signed-in account and channel it was asked as.
    fn is_account_scoped(&self) -> bool {
        matches!(
            self,
            Resp::Home(_)
                | Resp::HomeMore(_)
                | Resp::QuickPicksMore { .. }
                | Resp::Suggest { .. }
                | Resp::Library(_)
                | Resp::History(_)
                | Resp::Uploads(_)
                | Resp::Account { .. }
                | Resp::Channels(_)
                | Resp::ServerQueue(_)
                | Resp::Liked { .. }
                | Resp::LikeProgress { .. }
                | Resp::LikedMany { .. }
                | Resp::AddedToPlaylist { .. }
        )
    }

    /// An album or playlist load that ends without more songs: also sent when it fails,
    /// so the page stops waiting for it.
    fn ended(id: String, load: u64) -> Self {
        Resp::CollectionMore { id, load, tracks: Vec::new(), set_ids: Vec::new(), more: false, failed: true }
    }
}

/// Likes and unlikes go one after another, this far apart. YouTube Music answers faster
/// ones as taken and then drops many: on a test account 9 of 20 unlikes sent back to back
/// stayed liked, against 1 of 20 sent 400 ms apart; on a limited account 576 of 1,296
/// likes sent back to back were dropped, and liking those again took as long as pacing.
pub const LIKE_GAP: std::time::Duration = std::time::Duration::from_millis(400);

/// One run of many likes at a time (a selection here, or a Spotify transfer): run side by
/// side, one undoes the other (a transfer skips songs as liked that an unlike run then
/// takes out).
pub static LIKES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static PLAYLIST_MOVES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());


/// How long one like or unlike may take. Taken ones answer in about a quarter second;
/// ones YouTube Music refuses while it limits the account are held 8–9 s before the 403
/// (two of ten unlikes cost 17 of 27 s; in a transfer one like in six or so hangs).
/// Sending it again is harmless (liking a liked song changes nothing) and usually goes
/// straight through.
pub const LIKE_WAIT: std::time::Duration = std::time::Duration::from_millis(1500);

/// Like or unlike one song; whether YouTube Music took it. Refusals are retried by the
/// API (after its shared pause while the account is limited); one that hangs or fails is
/// sent again, up to three times in all.
async fn like_one(api: &ytmapi::Api, id: &str, like: bool, others: &mut Vec<String>) -> bool {
    let epoch = api.epoch();
    for tries in 0..3 {
        match tokio::time::timeout(LIKE_WAIT, ytm::set_like(api, id, like)).await {
            Ok(Ok(())) => {
                // Outside that wait: it takes longer, and checking Liked Music afterwards
                // catches a like that's still there.
                if !like {
                    match ytm::remove_other_half_like(api, id, epoch).await {
                        Ok(other) => others.extend(other),
                        // Maybe that's the one holding the like: not done, then.
                        Err(e) => {
                            log::warn!("removing the like of {id}'s other version: {e:#}");
                            return false;
                        }
                    }
                }
                return true;
            }
            Ok(Err(e)) => log::debug!("like={like} {id}: {e:#}"),
            // Not refused yet: again right away.
            Err(_) => {
                log::debug!("like={like} {id}: no answer in {LIKE_WAIT:?}; sending it again");
                continue;
            }
        }
        if tries < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
    false
}

/// Like or unlike a selection, one at a time (paced, see `LIKE_GAP`), then check
/// Liked Music and try again what didn't take. `progress(taken, tried)` after each song.
/// Returns the ones taken and the ones refused; unlikes still listed once checking stops
/// getting anywhere count as refused, likes don't (songs that can't be played are liked but not listed).
/// One run at a time, transfers included (`LIKES`); `progress(None, 0)` while waiting its turn.
/// Third: the other versions whose likes went with the unlikes (`ytm::remove_other_half_like`).
pub async fn like_many(api: &ytmapi::Api, ids: Vec<String>, like: bool, mut progress: impl FnMut(Option<&str>, usize)) -> (Vec<String>, Vec<String>, Vec<String>) {
    let epoch = api.epoch();
    let _turn = match LIKES.try_lock() {
        Ok(turn) => turn,
        Err(_) => {
            progress(None, 0);
            LIKES.lock().await
        }
    };
    // A channel switch went first: these were picked in the other channel's lists.
    if api.epoch() != epoch {
        log::info!("likes: {} not sent, the account or channel changed while they waited", ids.len());
        return (Vec::new(), ids, Vec::new());
    }
    let total = ids.len();
    let mut failed = Vec::new();
    let mut others = Vec::new();
    for (i, id) in ids.iter().enumerate() {
        let taken = like_one(api, id, like, &mut others).await;
        if !taken {
            failed.push(id.clone());
        }
        progress(taken.then_some(id.as_str()), i + 1);
        tokio::time::sleep(LIKE_GAP).await;
    }
    // Check and try again while it gets anywhere: a throttled account takes a few rounds.
    let (mut before, mut stuck) = (usize::MAX, 0);
    for round in 0..10 {
        if total < 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let listed: std::collections::HashSet<String> = match ytm::playlist(api, "LM", true, None).await {
            Ok(lm) => lm.tracks.into_iter().map(|t| t.id).collect(),
            Err(e) => {
                log::warn!("checking likes: {e:#}");
                break;
            }
        };
        let left: Vec<String> = ids.iter().filter(|id| listed.contains(*id) != like && !failed.contains(*id)).cloned().collect();
        log::debug!("like={like} check {round}: Liked Music lists {}, {} of {total} not taken yet", listed.len(), left.len());
        if left.is_empty() {
            break;
        }
        stuck = if left.len() < before { 0 } else { stuck + 1 };
        before = left.len();
        if stuck == 2 || round == 9 {
            if !like {
                failed.extend(left);
            }
            break;
        }
        // The second go is paced either way.
        for id in &left {
            like_one(api, id, like, &mut others).await;
            tokio::time::sleep(LIKE_GAP).await;
        }
    }
    let done: Vec<String> = ids.into_iter().filter(|id| !failed.contains(id)).collect();
    (done, failed, others)
}

/// A video id and its stream buffer.
type StreamSlot = Arc<std::sync::Mutex<Option<(String, Arc<Shared>)>>>;
type SongCell = Arc<tokio::sync::OnceCell<Option<Track>>>;

/// How long a play waits for a music video's song version before playing the video.
const PLAY_SONG_WAIT: std::time::Duration = std::time::Duration::from_millis(2500);
/// A prefetch has until the song before it ends.
const PREFETCH_SONG_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

#[derive(Clone)]
pub struct Backend {
    rt: Arc<Runtime>,
    api: Arc<ytmapi::Api>,
    tx: Sender<Resp>,
    audio: AudioHandle,
    wake: Arc<dyn Fn() + Send + Sync>,
    data_dir: PathBuf,
    lyrics: Arc<lyrics::Client>,
    translate: Arc<crate::translate::Client>,
    /// The upcoming track, resolved and downloading ahead of time.
    prefetched: StreamSlot,
    /// The stream most recently started, kept so a replay of a fully downloaded song
    /// (repeat one, restart) doesn't download it again.
    current: StreamSlot,
    jsc: Arc<jsc::Solver>,
    /// Whether the signed-in account has Premium; checked once per sign-in. A failed
    /// check is retried no sooner than `PREMIUM_RETRY` later.
    premium: Arc<tokio::sync::Mutex<Result<bool, Option<std::time::Instant>>>>,
    /// High quality: with Premium, every song goes through the signed-in web player
    /// for its 256 kbps stream instead of the anonymous 128 kbps one.
    hq: Arc<std::sync::atomic::AtomicBool>,
    /// Explicit songs are hidden: a music video's song version has to be clean.
    clean: Arc<std::sync::atomic::AtomicBool>,
    /// The visionOS player's file URLs were refused past 1 MiB this session (YouTube
    /// wants a PO token for them on this network): songs play from its HLS segments.
    visionos_hls: Arc<std::sync::atomic::AtomicBool>,
    /// Song versions of music videos looked up this session (`None`: there's none), by
    /// video and whether the account has Premium (which gets YouTube's own pairs). One
    /// cell each, so a prefetch and the play of the same video share a lookup.
    songs: Arc<std::sync::Mutex<std::collections::HashMap<String, SongCell>>>,
}

impl Backend {
    pub fn new(
        storage: PathBuf,
        tx: Sender<Resp>,
        audio: AudioHandle,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> anyhow::Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("net")
            .enable_all()
            .build()?;
        std::fs::create_dir_all(&storage)?;
        let data_dir = storage;
        ytdlp::remove_leftover_cookies(&data_dir);
        let http = crate::proxy::http();
        let lyrics = Arc::new(lyrics::Client::new(http.clone(), &data_dir));
        let translate = Arc::new(crate::translate::Client::new(http, &data_dir));
        let jsc = Arc::new(jsc::Solver::new(&data_dir));
        let api = Arc::new(ytmapi::Api::new(&data_dir));
        if crate::proxy::needs_relay()
            && let Err(e) = rt.block_on(crate::proxy::start_relay())
        {
            log::warn!("proxy relay: {e:#}");
        }
        let backend = Self { rt: Arc::new(rt), api, tx, audio, wake, prefetched: Default::default(), current: Default::default(), data_dir, lyrics, translate, jsc, premium: Arc::new(tokio::sync::Mutex::new(Err(None))), hq: Default::default(), clean: Default::default(), visionos_hls: Default::default(), songs: Default::default() };
        backend.keep_signed_in();
        Ok(backend)
    }

    /// Refresh the session's cookies every few minutes, as an open YouTube tab does:
    /// Google signs out a copy that stops taking new ones.
    fn keep_signed_in(&self) {
        const EVERY: std::time::Duration = std::time::Duration::from_secs(10 * 60);
        let this = self.clone();
        self.rt.spawn(async move {
            loop {
                if this.api.account().is_some() {
                    this.api.ensure_visitor().await;
                    match this.api.rotate().await {
                        Ok(()) => log::debug!("session cookies refreshed"),
                        Err(e) if e.is::<webmusic::Revoked>() => this.check_session().await,
                        Err(e) => log::info!("refreshing session cookies failed: {e:#}"),
                    }
                }
                tokio::time::sleep(EVERY).await;
            }
        });
    }

    /// Something suggested Google signed the session out. When YouTube confirms it no
    /// longer knows the session, sign out here too and say so, rather than every
    /// signed-in page failing on its own.
    async fn check_session(&self) {
        // Several requests answered at once ask together; one check is enough.
        static CHECKING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let Ok(_one) = CHECKING.try_lock() else { return };
        if self.api.account().is_none() {
            return;
        }
        // Signing out deletes the session, so only once youtube.com shows it signed out
        // twice, a minute apart (each after a rotation, which can bring it back). One
        // signed-out answer can be a hiccup (throttling, a consent or "unusual traffic"
        // page), and a refused rotation alone isn't proof either: the clients whose
        // sessions last never sign out on their own.
        for attempt in 0..2 {
            if let Err(e) = self.api.rotate().await {
                log::info!("rotating cookies while checking the session: {e:#}");
            }
            let Some(account) = self.api.account() else { return };
            match self.api.session_from_cookie(&account.cookie).await {
                Ok(_) => return,
                Err(e) if e.is::<ytmapi::NotSignedIn>() => {
                    log::warn!("signed-in session looks signed out ({}): {e:#}", if attempt == 0 { "checking again in a minute" } else { "twice" });
                    if attempt == 0 {
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    }
                }
                // Offline, throttled or similar: nothing learned.
                Err(e) => {
                    log::info!("couldn't check the session: {e:#}");
                    return;
                }
            }
        }
        log::warn!("signed-in session no longer valid");
        self.forget_session().await;
        let _ = self.tx.send(Resp::LoggedIn(false));
        let _ = self.tx.send(Resp::Error("Your YouTube sign-in expired. Sign in again to get your library back".into()));
        self.wake();
    }

    /// Act as another of the account's channels. Not while likes of a selection or a
    /// transfer are running: they'd go on as the new channel.
    async fn switch_channel(&self, page_id: Option<String>, epoch: u64) -> anyhow::Result<u64> {
        let _likes = LIKES.try_lock().map_err(|_| anyhow::anyhow!("Wait until the likes going through are done"))?;
        // A Premium check running now is for the old channel: let it finish, then forget it.
        let mut premium = self.premium.lock().await;
        let epoch = self.api.switch_channel(page_id, epoch)?;
        *premium = Err(None);
        drop(premium);
        self.warm_signed_in();
        log::info!("switched channel");
        Ok(epoch)
    }

    async fn forget_session(&self) {
        self.api.sign_out();
        *self.premium.lock().await = Err(None);
        let profile = self.login_profile();
        let _ = tokio::task::spawn_blocking(move || crate::login::forget(&profile)).await;
    }

    pub fn api(&self) -> Arc<ytmapi::Api> {
        self.api.clone()
    }

    /// The shared client (see `proxy::http`).
    pub fn http(&self) -> reqwest::Client {
        crate::proxy::http()
    }

    pub fn spawn<F: Future<Output = ()> + Send + 'static>(&self, f: F) {
        self.rt.spawn(f);
    }

    pub fn wake(&self) {
        (self.wake)()
    }

    pub fn request(&self, req: Req) {
        let (what, quiet) = (req.name(), req.quiet());
        let play_gen = match &req {
            Req::Play { generation, .. } => Some(*generation),
            _ => None,
        };
        let collection = match &req {
            Req::Album { id, load } | Req::Playlist { id, load, .. } => Some((id.clone(), *load)),
            _ => None,
        };
        let this = self.clone();
        let t0 = std::time::Instant::now();
        let epoch = self.api.epoch();
        let task = self.rt.spawn(async move {
            let r = this.handle(req).await;
            log::debug!("{what}: done in {:?}", t0.elapsed());
            r
        });
        let this = self.clone();
        self.rt.spawn(async move {
            // A panic in a handler (or a library it calls) fails just this request,
            // reported like any other error.
            let resp = match task.await {
                Ok(resp) => resp,
                Err(e) if e.is_panic() => {
                    log::error!("{what}: request panicked");
                    match play_gen {
                        Some(generation) => Some(Resp::PlayError { generation, msg: "YouTube didn't respond properly".into() }),
                        None => (!quiet).then(|| Resp::Error(format!("{what} failed: YouTube didn't respond properly"))),
                    }
                }
                Err(_) => None,
            };
            // An album or playlist load that failed ends too, so its page stops waiting.
            if let Some((id, load)) = collection
                && !matches!(resp, Some(Resp::Collection { .. } | Resp::CollectionMore { .. }))
            {
                let _ = this.tx.send(Resp::ended(id, load));
            }
            // Asked as another sign-in or channel than the one now used: it isn't this one's.
            let resp = resp.filter(|r| {
                let stale = this.api.epoch() != epoch && r.is_account_scoped();
                if stale {
                    log::debug!("{what}: answer for the previous account or channel dropped");
                }
                !stale
            });
            if let Some(resp) = resp {
                let _ = this.tx.send(resp);
                this.wake();
            }
            if this.api.take_signed_out() {
                this.check_session().await;
            }
        });
    }

    async fn handle(&self, req: Req) -> Option<Resp> {
        let api = &*self.api;
        // Personalised pages ask as the account when signed in, like music.youtube.com.
        let me = self.is_logged_in();
        let (what, quiet) = (req.name(), req.quiet());
        let res: anyhow::Result<Option<Resp>> = async {
            Ok(Some(match req {
                Req::Home(params) => Resp::Home(ytm::home(api, params.as_deref(), me).await?),
                Req::HomeMore(token) => Resp::HomeMore(ytm::home_more(api, &token, me).await?),
                Req::MoreQuickPicks(seed) => {
                    let tracks = ytm::radio(api, &seed, me, 0).await?;
                    Resp::QuickPicksMore { seed, tracks }
                }
                Req::Explore(preferred_artists) => self.explore(&preferred_artists, me).await?,
                Req::Mood(params) => Resp::Mood(ytm::mood(api, &params, me).await?),
                Req::Search(query) => self.search(query, me).await?,
                Req::Suggest { query, round } => {
                    let terms = ytm::suggestions(api, &query, me).await?.into_iter().take(8).collect();
                    Resp::Suggest { query, round, terms }
                }
                Req::ForgetSearch(token) => {
                    ytm::forget_search(api, &token).await?;
                    Resp::SearchForgotten
                }
                Req::Album { id, load } => Resp::Collection { load, c: ytm::album(api, &id).await?, more: false },
                Req::Playlist { id, load, stream: false } => Resp::Collection { load, c: ytm::playlist(api, &id, me, None).await?, more: false },
                Req::Playlist { id, load, stream: true } => self.stream_playlist(id, load, me).await?,
                Req::Artist(id) => Resp::Artist(ytm::artist(api, &id).await?),
                Req::Library => self.library().await?,
                Req::History => Resp::History(ytm::history(api).await?),
                Req::Uploads => Resp::Uploads(ytm::uploads(api).await?),
                Req::Account => {
                    let (name, photo) = ytm::account(api).await?;
                    Resp::Account { name, photo }
                }
                Req::Channels => {
                    let epoch = api.epoch();
                    let list = ytm::channels(api).await?;
                    Resp::Channels(Channels { list, current: api.account().and_then(|a| a.channel().map(str::to_owned)), epoch })
                }
                Req::SwitchChannel { page_id, epoch } => match self.switch_channel(page_id.clone(), epoch).await {
                    Ok(epoch) => Resp::SwitchedChannel { page_id, epoch, error: None },
                    Err(e) => {
                        log::warn!("switching channel failed: {e:#}");
                        Resp::SwitchedChannel { page_id, epoch, error: Some(e.to_string()) }
                    }
                },
                Req::ServerQueue => match ytm::server_queue(api).await? {
                    Some(q) => Resp::ServerQueue(q),
                    None => return Ok(None),
                },
                Req::Upload(files) => self.upload(&files).await?,
                Req::Lyrics(track) => {
                    let video_id = track.id.clone();
                    Resp::Lyrics { lyrics: self.lyrics_for(&track).await, video_id }
                }
                Req::Comments { video_id } => {
                    let page = ytm::comments(api, &video_id, me).await.map(|p| p.unwrap_or_else(|| serde_json::json!({ "commentsOff": true })));
                    if let Err(e) = &page {
                        log::warn!("comments for {video_id}: {e:#}");
                    }
                    Resp::CommentsPage { token: format!("video:{video_id}"), page: page.ok() }
                }
                Req::CommentAction { endpoint, body, tag } => {
                    // As the account: only ever from a click (Comment, Reply, the thumb).
                    let page = api.call_signed_in(&endpoint, body).await;
                    if let Err(e) = &page {
                        log::warn!("comments {endpoint}: {e:#}");
                    }
                    Resp::CommentsPage { token: tag, page: page.ok() }
                }
                Req::CommentsPage { token } => {
                    let page = api.call("next", serde_json::json!({ "continuation": token }), me).await;
                    if let Err(e) = &page {
                        log::warn!("comments page: {e:#}");
                    }
                    Resp::CommentsPage { token, page: page.ok() }
                }
                Req::Segments { video_id } => {
                    let segments = crate::sponsorblock::segments(&self.http(), &video_id).await?;
                    Resp::Segments { video_id, segments }
                }
                Req::LyricsExtras { video_id, source, lines, lang, target } => {
                    let key = format!("{video_id}_{source}");
                    let Some(extras) = self.translate.extras(&key, &lines, lang.as_deref(), &target).await else { return Ok(None) };
                    Resp::LyricsExtras { video_id, source, target, extras }
                }
                Req::Art { key, url } => {
                    // The backdrop is a 64 px blur; fetch near that, not the 544 px art.
                    let url = model::sized(&url, 2 * crate::art::SIZE);
                    let bytes = self.http().get(&url).send().await?.error_for_status()?.bytes().await?;
                    let art = tokio::task::spawn_blocking(move || crate::art::process(&bytes)).await??;
                    Resp::Art { key, art }
                }
                Req::Radio { seed, song_of } => {
                    // A music video's radio is music videos; its song's is songs, as from the
                    // song itself. The lookup is shared with its play's (one per video).
                    let song = self.song_version(&seed, song_of.as_ref(), PREFETCH_SONG_WAIT).await;
                    let from = song.as_ref().map_or(&seed, |s| &s.id);
                    let mut tracks = ytm::radio(api, from, false, 50).await?;
                    // The radio starts with its seed, which is playing already.
                    tracks.retain(|t| t.id != *from);
                    Resp::Radio { seed, tracks }
                }
                Req::PlayCollection { target } => {
                    let (title, tracks) = match &target {
                        Target::Album(id) => ytm::album(api, id).await.map(|c| (c.title, c.tracks))?,
                        Target::Playlist(id) => ytm::playlist(api, id, me, None).await.map(|c| (c.title, c.tracks))?,
                        Target::Artist(id) => ytm::artist(api, id).await.map(|a| (a.name, a.top))?,
                        Target::Song(t) => (t.title.clone(), vec![t.clone()]),
                    };
                    Resp::PlayQueue { target, title, tracks }
                }
                Req::Download { track: t, for_video } => {
                    let path = self.download(&t, for_video).await?;
                    // Its lyrics too, for offline; the download is done without waiting for them.
                    let (this, song) = (self.clone(), t.clone());
                    tokio::spawn(async move {
                        let found = this.lyrics_for(&song).await.sources.len();
                        log::info!("download {}: lyrics from {found} source(s) kept", song.id);
                    });
                    Resp::Downloaded { title: t.title, path }
                }
                Req::Play { video_id, generation, at, report, song_of, song } => match self.start_playback(&video_id, song_of.as_ref(), song, generation, at, report).await {
                    Ok(()) => return Ok(None),
                    Err(e) => Resp::PlayError { generation, msg: format!("{e:#}") },
                },
                Req::VideoStream { video_id, max_height } => {
                    let found = async { innertube::video_stream(&self.http(), &video_id, &api.visitor_data(false).await?, max_height.into()).await }.await;
                    if let Err(e) = &found {
                        log::warn!("video of {video_id}: {e:#}");
                    }
                    Resp::VideoStream { video_id, max_height, stream: found.ok() }
                }
                Req::Canvas(track) => Resp::Canvas { clip: crate::canvas::find(&self.http(), &self.data_dir, &track).await, video_id: track.id },
                Req::Prefetch { video_id, song_of } => {
                    self.prefetch(video_id, song_of).await?;
                    return Ok(None);
                }
                Req::Like { video_id, like } => {
                    let epoch = api.epoch();
                    match ytm::set_like(api, &video_id, like).await {
                        Ok(_) => {
                            let other = if like { None } else { ytm::remove_other_half_like(api, &video_id, epoch).await? };
                            Resp::Liked { video_id, like, other }
                        }
                        // Shown as done already: told apart, so it comes back as it was.
                        Err(e) => {
                            log::warn!("like={like} of {video_id}: {e:#}");
                            Resp::LikedMany { video_ids: Vec::new(), failed: vec![video_id], like, others: Vec::new() }
                        }
                    }
                }
                Req::UnlikeBoth { song, video } => {
                    let epoch = api.epoch();
                    let song_went = ytm::set_like(api, &song, false).await;
                    tokio::time::sleep(LIKE_GAP).await;
                    if api.epoch() != epoch {
                        return Ok(None);
                    }
                    let video_went = ytm::set_like(api, &video, false).await;
                    if song_went.is_ok() && video_went.is_ok() {
                        let _ = self.tx.send(Resp::Liked { video_id: song, like: false, other: None });
                        Resp::Liked { video_id: video, like: false, other: None }
                    } else {
                        // Told apart, so what's still liked comes back with a reload.
                        log::warn!("unliking {song} and {video}: {song_went:?}, {video_went:?}");
                        let (video_ids, failed) = [(song, song_went), (video, video_went)].into_iter().fold((vec![], vec![]), |(mut went, mut stayed), (id, r)| {
                            if r.is_ok() { went.push(id) } else { stayed.push(id) }
                            (went, stayed)
                        });
                        Resp::LikedMany { video_ids, failed, like: false, others: Vec::new() }
                    }
                }
                Req::LikeMany { video_ids, like } => self.like_many(video_ids, like).await?,
                Req::Dislike(video_id) => {
                    match ytm::dislike(api, &video_id).await {
                        Ok(_) => Resp::Liked { video_id, like: false, other: None },
                        Err(e) => {
                            log::warn!("disliking {video_id}: {e:#}");
                            Resp::LikedMany { video_ids: Vec::new(), failed: vec![video_id], like: false, others: Vec::new() }
                        }
                    }
                }
                Req::CreatePlaylist { title, video_ids } => {
                    let id = match video_ids.as_slice() {
                        [] => ytm::create_playlist(api, &title, None).await?,
                        [one] => ytm::create_playlist(api, &title, Some(one)).await?,
                        many => ytm::create_playlist_with(api, &title, many).await?,
                    };
                    Resp::PlaylistCreated { id, songs: video_ids.len() }
                }
                Req::EditPlaylist { id, title, description, privacy } => {
                    ytm::edit_playlist(api, &id, &title, &description, privacy.as_deref()).await?;
                    Resp::PlaylistChanged { id, message: "Playlist updated" }
                }
                Req::AddToPlaylist { playlist_id, video_ids } => {
                    // A failure says which songs too, so they stop showing in the playlist.
                    let error = ytm::add_videos(api, &playlist_id, &video_ids).await.err().map(|e| {
                        log::warn!("{what} failed: {e:#}");
                        format!("{what} failed: {e}")
                    });
                    Resp::AddedToPlaylist { id: playlist_id, video_ids, error }
                }
                Req::RemoveFromPlaylist { playlist_id, items } => {
                    match ytm::remove_videos(api, &playlist_id, &items).await {
                        Ok(_) => Resp::PlaylistChanged { id: playlist_id, message: "Removed from playlist" },
                        Err(e) => {
                            log::warn!("removing from playlist {playlist_id}: {e:#}");
                            Resp::RemoveFailed { id: playlist_id, error: format!("Couldn't remove from the playlist: {e}") }
                        }
                    }
                }
                Req::MoveInPlaylist { playlist_id, move_id, set_id, before } => {
                    let epoch = api.epoch();
                    // One at a time, in the order they were made: each is relative to the others.
                    let _turn = PLAYLIST_MOVES.lock().await;
                    // Waiting, it may have outlived its account or channel: it isn't for this one.
                    let result = match api.epoch() == epoch {
                        true => ytm::move_video(api, &playlist_id, &set_id, before.as_deref()).await,
                        false => Err(anyhow::anyhow!("the account or channel changed")),
                    };
                    if let Err(e) = &result {
                        log::warn!("moving in playlist {playlist_id}: {e:#}");
                    }
                    Resp::PlaylistMoved { id: playlist_id, move_id, ok: result.is_ok() }
                }
                Req::DeletePlaylist(id) => {
                    ytm::delete_playlist(api, &id).await?;
                    Resp::PlaylistChanged { id, message: "Playlist deleted" }
                }
                Req::Collaboration(id) => Resp::Collaboration(ytm::collaboration(api, &id).await?),
                Req::SetCollaboration { info, change } => {
                    ytm::set_collaboration(api, &info, change).await?;
                    Resp::Collaboration(ytm::collaboration(api, &info.playlist_id).await?)
                }
                Req::SaveCollection { id, save } => {
                    ytm::save_collection(api, &id, save).await?;
                    Resp::PlaylistChanged { id, message: if save { "Saved to library" } else { "Removed from library" } }
                }
                Req::SetCookie(input) => {
                    self.set_cookie(&input).await?;
                    *self.premium.lock().await = Err(None);
                    self.warm_signed_in();
                    log::info!("signed in");
                    Resp::LoggedIn(true)
                }
                Req::Logout => {
                    self.forget_session().await;
                    Resp::LoggedIn(false)
                }
            }))
        }
        .await;
        res.unwrap_or_else(|e| {
            log::warn!("{what} failed: {e:#}");
            (!quiet).then(|| Resp::Error(format!("{what} failed: {e}")))
        })
    }

    /// Explore: the charts, new albums by charting artists or the listener's own, and
    /// Moods & genres.
    async fn explore(&self, preferred_artists: &[String], me: bool) -> anyhow::Result<Resp> {
        let api = &*self.api;
        let (charts, new, moods) = tokio::join!(ytm::charts(api), ytm::new_albums(api, me), ytm::moods(api, me));
        let charts = charts?;
        Ok(Resp::Explore(Explore {
            new_albums: popular_new_albums(&new.unwrap_or_default(), &charts.artists, preferred_artists),
            charts: charts.playlists,
            top: charts.top,
            moods: moods.unwrap_or_default(),
        }))
    }

    /// Songs, videos, albums, artists and playlists for a search, asked at once (podcast
    /// shows and episodes too when those are shown). Only the songs must arrive.
    async fn search(&self, query: String, me: bool) -> anyhow::Result<Resp> {
        let api = &*self.api;
        let podcasts = async {
            if !ytm::podcasts() {
                return (Vec::new(), Vec::new());
            }
            let (shows, episodes) = tokio::join!(ytm::search_cards(api, &query, ytm::Filter::Podcasts, me), ytm::search_episodes(api, &query, me));
            (shows.unwrap_or_default(), episodes.unwrap_or_default())
        };
        let (t, v, al, ar, pl, (podcasts, episodes)) = tokio::join!(
            ytm::search_songs(api, &query, me),
            ytm::search_videos(api, &query, me),
            ytm::search_cards(api, &query, ytm::Filter::Albums, me),
            ytm::search_cards(api, &query, ytm::Filter::Artists, me),
            ytm::search_cards(api, &query, ytm::Filter::Playlists, me),
            podcasts,
        );
        Ok(Resp::Search(SearchResults {
            tracks: t?,
            videos: v.unwrap_or_default(),
            albums: al.unwrap_or_default(),
            artists: ar.unwrap_or_default(),
            playlists: pl.unwrap_or_default(),
            podcasts,
            episodes,
            query,
        }))
    }

    /// Saved playlists and albums.
    async fn library(&self) -> anyhow::Result<Resp> {
        let api = &*self.api;
        // Liked songs are Liked Music, loaded like any playlist. Each half shows
        // without the other; it only fails when both do.
        let podcasts = async {
            if !ytm::podcasts() {
                return Vec::new();
            }
            ytm::saved_podcasts(api).await.unwrap_or_else(|e| {
                log::warn!("saved podcasts didn't load: {e:#}");
                Vec::new()
            })
        };
        let (pls, albums, podcasts) = tokio::join!(ytm::saved_playlists(api), ytm::saved_albums(api), podcasts);
        let pls = match (pls, &albums) {
            (Err(e), Err(_)) => return Err(e),
            (pls, _) => pls,
        };
        if let Err(e) = &pls {
            log::warn!("saved playlists didn't load: {e:#}");
        }
        if let Err(e) = &albums {
            log::warn!("saved albums didn't load: {e:#}");
        }
        Ok(Resp::Library(Library {
            playlists_failed: pls.is_err(),
            albums_failed: albums.is_err(),
            playlists: pls.map(|p| p.into_iter().filter(saved_playlist_visible).collect()).unwrap_or_default(),
            albums: albums.unwrap_or_default(),
            podcasts,
        }))
    }

    /// Upload songs, then refresh the uploads while YouTube Music processes them.
    async fn upload(&self, files: &[PathBuf]) -> anyhow::Result<Resp> {
        let api = &*self.api;
        let session = api.account().ok_or_else(|| anyhow::anyhow!("sign in to upload music"))?;
        let (mut done, mut failed) = (0, Vec::new());
        for f in files {
            let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            match session.upload(&self.http(), f).await {
                Ok(()) => done += 1,
                Err(e) => {
                    log::warn!("upload {name}: {e:#}");
                    failed.push(name);
                }
            }
        }
        if done > 0 {
            // YouTube Music needs a little while to process new uploads.
            let this = self.clone();
            self.rt.spawn(async move {
                for wait in [20, 40, 60] {
                    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                    if let Ok(t) = ytm::uploads(&this.api).await {
                        let _ = this.tx.send(Resp::Uploads(t));
                        this.wake();
                    }
                }
            });
        }
        Ok(Resp::Uploaded { done, failed })
    }

    /// Open the next song's stream ahead of time, once the playing one has downloaded.
    /// `song_of`: the music video it stands for, whose song version is what will play.
    async fn prefetch(&self, video_id: String, song_of: Option<Track>) -> anyhow::Result<()> {
        // For the play it was asked during: a skip in the meantime makes it moot.
        let generation = self.audio.status.wanted_gen.load(Ordering::Relaxed);
        // Looked up now, so the play doesn't wait for it.
        let video_id = match self.song_version(&video_id, song_of.as_ref(), PREFETCH_SONG_WAIT).await {
            Some(song) => song.id,
            None => video_id,
        };
        // Let the playing song finish downloading first: it gets the whole bandwidth.
        let playing = self.current.lock().unwrap().as_ref().map(|(_, s)| s.clone());
        if let Some(s) = playing {
            s.finished().await;
        }
        if self.audio.status.wanted_gen.load(Ordering::Relaxed) != generation
            || self.prefetched.lock().unwrap().as_ref().is_some_and(|(id, _)| *id == video_id)
        {
            return Ok(());
        }
        log::debug!("prefetch {video_id}");
        let shared = self.open_stream(&video_id, true).await?;
        if let Some((_, old)) = self.prefetched.lock().unwrap().replace((video_id, shared)) {
            old.cancel();
        }
        Ok(())
    }

    /// A song's lyrics. A downloaded song's are kept: shown at once from what was saved
    /// (offline too), and saved again from a fresh look in the background.
    async fn lyrics_for(&self, t: &Track) -> LyricsSet {
        if crate::downloads::path(&t.id).is_none() {
            return self.fetch_lyrics(t).await;
        }
        let saved = tokio::task::spawn_blocking({
            let id = t.id.clone();
            move || crate::downloads::saved_lyrics(&id)
        })
        .await
        .ok()
        .flatten();
        let (this, t) = (self.clone(), t.clone());
        let fresh = tokio::spawn(async move {
            // A download saved without its plays gets them now, while online.
            if crate::downloads::lacks_plays(&t.id)
                && let Ok(Some(plays)) = ytm::song_plays(&this.api, &t).await
            {
                crate::downloads::set_plays(&t.id, plays);
            }
            let set = this.fetch_lyrics(&t).await;
            let keep = set.clone();
            let _ = tokio::task::spawn_blocking(move || crate::downloads::save_lyrics(&t.id, &keep)).await;
            set
        });
        match saved {
            Some(saved) => saved,
            None => fresh.await.unwrap_or_default(),
        }
    }

    async fn fetch_lyrics(&self, t: &Track) -> LyricsSet {
        let artist = t.artist_line();
        let query = lyrics::Query {
            video_id: &t.id,
            song: &t.title,
            artist: &artist,
            album: t.album.as_ref().map(|a| a.name.as_str()),
            duration: match t.duration.or_else(|| crate::downloads::duration(&t.id)) {
                Some(d) => Some(d),
                None => self.stream_duration(&t.id).await,
            },
        };
        let mut set = self.lyrics.fetch(&query).await.unwrap_or_else(|e| {
            log::warn!("lyrics failed: {e:#}");
            LyricsSet::default()
        });
        // Dev hook: YTM_LYRICS_FAIL=<source key>[,<key>…] makes those sources come back empty.
        if let Ok(fail) = std::env::var("YTM_LYRICS_FAIL") {
            set.sources.retain(|l| !fail.split(',').any(|k| k.trim() == l.provider.key()));
        }
        set
    }

    /// The song's length from its stream, for a track listed without one (some home,
    /// radio and queue rows). Without a duration Better Lyrics only finds plain lyrics,
    /// so wait for playback to resolve the stream, up to 15 s. The player's own duration
    /// counts too: starting a song clears it, and a restored session sets it.
    async fn stream_duration(&self, video_id: &str) -> Option<u32> {
        for _ in 0..150 {
            let ms = self.current.lock().unwrap().as_ref().filter(|(id, _)| id == video_id).map(|(_, s)| s.duration_ms);
            let ms = ms.filter(|ms| *ms > 0).or_else(|| self.audio.status.duration().map(|d| d.as_millis() as u64));
            if let Some(ms) = ms.filter(|ms| *ms > 0) {
                return Some((ms / 1000) as u32);
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        log::info!("lyrics: no duration for {video_id}");
        None
    }

    /// Sign in with the youtube.com cookies the Google sign-in window hands over.
    async fn set_cookie(&self, cookie: &str) -> anyhow::Result<()> {
        let cookie = cookie.trim();
        anyhow::ensure!(!cookie.is_empty(), "no youtube.com cookies found");
        let session = self.api.session_from_cookie(cookie).await.inspect_err(|e| {
            // Names only, never values: enough to see whether SAPISID etc. arrived.
            let names: Vec<&str> = cookie.split(';').filter_map(|kv| kv.trim().split('=').next()).take(40).collect();
            log::warn!("cookie sign-in rejected: {e:#}; cookie names: {names:?}");
        })?;
        self.api.sign_in(session);
        // The sign-in window's copy of the session would go stale as the app takes new
        // cookies, and opening it again would show Google the old ones, which it treats
        // as stolen and signs the session out. Keep only the app's copy.
        let profile = self.login_profile();
        let _ = tokio::task::spawn_blocking(move || crate::login::forget(&profile)).await;
        Ok(())
    }

    async fn start_playback(&self, listed: &str, song_of: Option<&Track>, carried: Option<Track>, generation: u64, at: std::time::Duration, report: bool) -> anyhow::Result<()> {
        let t0 = std::time::Instant::now();
        // A music video plays as its song when one is found in time (or goes on as the
        // one it was). From here on, everything (prefetch, replay, history) goes by what
        // actually plays.
        let was_carried = carried.is_some();
        let mut song = match carried {
            Some(song) => Some(song),
            None => self.song_version(listed, song_of, PLAY_SONG_WAIT).await,
        };
        if self.audio.status.wanted_gen.load(Ordering::Relaxed) != generation {
            return Ok(());
        }
        let mut video_id = song.as_ref().map_or(listed, |s| s.id.as_str()).to_owned();
        let prefetched = self.prefetched.lock().unwrap().take();
        let replay = self.current.lock().unwrap().take().filter(|(id, s)| *id == video_id && s.is_complete()).map(|(_, s)| s);
        let ready = match prefetched {
            // Not one whose download failed (refused, say): that opens afresh.
            // Nor a file stream fetched before file streams turned out to be cut off.
            Some((id, s)) if id == video_id && s.error().is_none() && !(self.visionos_hls.load(Ordering::Relaxed) && s.segments.is_none() && !s.is_complete()) => Some(s),
            // Replaying the current song keeps the next one's prefetch.
            other if replay.is_some() => {
                *self.prefetched.lock().unwrap() = other;
                replay
            }
            // Playback went elsewhere: that prefetch is moot.
            other => {
                if let Some((_, s)) = other {
                    s.cancel();
                }
                None
            }
        };
        let shared = match ready {
            Some(s) => {
                log::debug!("play {video_id} (gen {generation}): using buffered stream");
                s
            }
            None => {
                if self.audio.status.wanted_gen.load(Ordering::Relaxed) != generation {
                    return Ok(());
                }
                match self.open_stream(&video_id, false).await {
                    Ok(s) => s,
                    // The song won't play: the music video still does.
                    Err(e) if song.is_some() => {
                        log::warn!("{listed}'s song {video_id} won't play ({e:#}); playing the music video");
                        (song, video_id) = (None, listed.to_owned());
                        // The UI took the song as playing already.
                        if was_carried {
                            let _ = self.tx.send(Resp::PlayingAs { generation, listed: listed.to_owned(), song: None });
                        }
                        self.open_stream(listed, false).await?
                    }
                    Err(e) => return Err(e),
                }
            }
        };
        // If the song's audio won't start after all, the music video plays instead.
        let fallback = song.is_some().then(|| (self.clone(), listed.to_owned(), report));
        if let Some(song) = song {
            log::info!("play {listed} as its song {video_id}");
            // Carried over, the UI has it already.
            if !was_carried {
                let _ = self.tx.send(Resp::PlayingAs { generation, listed: listed.to_owned(), song: Some(song) });
                (self.wake)();
            }
        }
        let video_id = video_id.as_str();
        *self.current.lock().unwrap() = Some((video_id.to_owned(), shared.clone()));
        log::debug!("play {video_id} (gen {generation}): stream open in {:?}", t0.elapsed());
        if self.audio.status.wanted_gen.load(Ordering::Relaxed) != generation {
            shared.cancel();
            return Ok(());
        }

        let audio = self.audio.clone();
        let tx = self.tx.clone();
        let wake = self.wake.clone();
        // Opening the decoder reads the container header, which blocks on the download.
        // Starting mid-song needs the seekable decoder (it waits for every fragment
        // header); from the top, the non-seekable one starts sooner.
        let seek = !at.is_zero();
        let shared2 = shared.clone();
        let report = (report && self.is_logged_in() && reports_plays()).then(|| (self.clone(), video_id.to_owned()));
        let open = move || -> anyhow::Result<crate::audio::TrackDecoder> {
            let mut d = stream::decoder(shared.clone(), seek)?;
            if seek {
                rodio::Source::try_seek(&mut d, at).map_err(|e| anyhow::anyhow!("seek: {e}"))?;
            }
            Ok(d)
        };
        tokio::task::spawn_blocking(move || match open() {
            Ok(decoder) => {
                log::debug!("play gen {generation}: decoder ready at {at:?} in {:?}", t0.elapsed());
                audio.send(AudioCmd::Start { generation, decoder, shared: shared2, at });
                // Skipped past before it started: that one doesn't count.
                if let Some((this, id)) = report.filter(|_| audio.status.wanted_gen.load(Ordering::Relaxed) == generation) {
                    let rt = this.rt.clone();
                    rt.spawn(async move {
                        match ytm::report_play(&this.api, &id).await {
                            Ok(w) => {
                                log::debug!("reported play of {id}");
                                this.report_watchtime(&id, generation, w).await;
                            }
                            Err(e) => log::warn!("couldn't report play of {id}: {e:#}"),
                        }
                    });
                }
            }
            Err(e) => match fallback {
                Some((this, listed, report)) => {
                    log::warn!("{listed}'s song won't start ({e}); playing the music video");
                    let _ = tx.send(Resp::PlayingAs { generation, listed: listed.clone(), song: None });
                    this.request(Req::Play { video_id: listed, generation, at, report, song_of: None, song: None });
                    wake();
                }
                None => {
                    let _ = tx.send(Resp::PlayError { generation, msg: format!("decode: {e}") });
                    wake();
                }
            },
        });
        Ok(())
    }

    /// Report how much of play `generation` is heard, on the web player's schedule
    /// (10, 20 and 30 s in, then every 40 s), with a last report once it ends or is left.
    /// Each covers the stretch played since the one before; time paused doesn't count.
    async fn report_watchtime(&self, id: &str, generation: u64, w: ytm::Watchtime) {
        // Reported as the sign-in and channel that started it, or not at all.
        let epoch = self.api.epoch();
        let status = &self.audio.status;
        let started = std::time::Instant::now();
        let mut flush = w.flush_at.iter().copied();
        let mut due = flush.next().unwrap_or(w.flush_every);
        // The stretch not yet reported, and where the song was at the last check.
        let pos = || status.position().as_secs_f64();
        let (mut from, mut at) = (pos(), pos());
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let gone = status.wanted_gen.load(Ordering::Relaxed) != generation;
            if !gone {
                let now = pos();
                // A seek starts a new stretch.
                if now < at || now > at + 5.0 {
                    from = now;
                }
                at = now;
            }
            let elapsed = started.elapsed().as_secs();
            if !gone && elapsed < due {
                continue;
            }
            if self.api.epoch() != epoch {
                log::debug!("stopped reporting listening time of {id}: the account or channel changed");
                return;
            }
            if at > from || gone {
                if let Err(e) = ytm::report_watchtime(&self.api, &w, from, at, started.elapsed().as_secs_f64(), gone).await {
                    log::warn!("couldn't report listening time of {id}: {e:#}");
                }
                from = at;
            }
            if gone {
                log::debug!("reported listening time of {id}");
                return;
            }
            due = flush.next().unwrap_or(due + w.flush_every);
        }
    }

    /// Resolve a stream URL and start downloading it into a new buffer (just its start,
    /// for a prefetch, until playback opens it).
    /// A downloaded song opens from its file.
    /// The song version of the music video `listed`, when `song_of` asks for one: from
    /// this session's lookups, YouTube's own pair (Premium), or a sure signed-out search
    /// match. `None` when there's none, it isn't found within `wait`, or the music video
    /// is downloaded (that file plays, offline too). A failed lookup isn't remembered,
    /// so the next play tries again.
    async fn song_version(&self, listed: &str, song_of: Option<&Track>, wait: std::time::Duration) -> Option<Track> {
        let video = song_of.filter(|t| t.id == listed && t.music_video)?;
        if crate::downloads::path(listed).is_some() {
            return None;
        }
        // Its song version downloaded from Now playing: from the file, no lookup (offline too).
        let clean = self.clean.load(Ordering::Relaxed);
        if let Some(song) = crate::downloads::song_for(listed).filter(|s| !(clean && s.explicit)) {
            return Some(song);
        }
        let start = std::time::Instant::now();
        // Premium gets YouTube's own pair; a search needs the video's length to be sure.
        // The check counts toward the wait (the first one after launch asks YouTube): not
        // back in time, this lookup searches. It finishes anyway, for the next song.
        let premium = match self.api.account() {
            Some(session) => {
                let this = self.clone();
                let check = tokio::spawn(async move { this.is_premium(&session).await });
                tokio::time::timeout(wait, check).await.is_ok_and(|p| p.unwrap_or(false))
            }
            None => false,
        };
        if !premium && (video.duration.is_none() || video.artists.is_empty()) {
            return None;
        }
        let cell = {
            let mut songs = self.songs.lock().unwrap();
            let key = format!("{listed} {premium} {clean}");
            match songs.get(&key) {
                Some(cell) => cell.clone(),
                None => {
                    if songs.len() >= 1000 {
                        // Keeps the lookups still running.
                        songs.retain(|_, c| !c.initialized());
                    }
                    songs.entry(key).or_default().clone()
                }
            }
        };
        // Its own task: when the wait runs out the lookup still finishes, so the next play
        // of this video finds the song in the cell.
        let (this, video) = (self.clone(), video.clone());
        let lookup = tokio::spawn(async move {
            cell.get_or_try_init(|| async {
                let found = this.look_up_song(&video, premium, clean).await?;
                match &found {
                    Some(s) => log::info!("song version of {} \"{}\": {} \"{}\"", video.id, video.title, s.id, s.title),
                    None => log::info!("song version of {} \"{}\": none found", video.id, video.title),
                }
                anyhow::Ok(found)
            })
            .await
            .cloned()
        });
        match tokio::time::timeout(wait.saturating_sub(start.elapsed()), lookup).await {
            Ok(Ok(Ok(found))) => found,
            Ok(Ok(Err(e))) => {
                log::debug!("song version of {listed}: {e:#}");
                None
            }
            Ok(Err(e)) => {
                log::debug!("song version of {listed}: lookup stopped: {e}");
                None
            }
            Err(_) => {
                log::debug!("song version of {listed}: not found in {wait:?}, playing the music video");
                None
            }
        }
    }

    /// clean: only a song that isn't explicit.
    async fn look_up_song(&self, video: &Track, premium: bool, clean: bool) -> anyhow::Result<Option<Track>> {
        if premium {
            match ytm::paired_song(&self.api, &video.id).await {
                // YouTube pairs every music video that has a song; trust it when it says none.
                Ok(found) if !(clean && found.as_ref().is_some_and(|s| s.explicit)) => return Ok(found),
                // Paired with its explicit version: a search may find the clean one.
                Ok(_) => {}
                Err(e) => log::debug!("song pair of {}: {e:#}; searching instead", video.id),
            }
        }
        ytm::searched_song(&self.api, video, clean).await
    }

    async fn open_stream(&self, video_id: &str, prefetch: bool) -> anyhow::Result<Arc<Shared>> {
        let id = video_id.to_owned();
        if let Some(shared) = tokio::task::spawn_blocking(move || crate::downloads::open(&id)).await? {
            return Ok(shared);
        }
        let (source, s) = self.resolve(video_id, false).await?;
        match &s.starts {
            Some(starts) => log::info!("stream {video_id}: {} HLS segments from {source:?}", starts.len()),
            None => log::info!("stream {video_id}: {} bytes {} from {source:?}", s.size, if s.opus { "Opus" } else { "AAC" }),
        }
        let shared = s.buffer(prefetch);
        let refresh = Some(self.refresher(source, video_id, s.opus));
        let (this, buffer) = (self.clone(), shared.clone());
        self.rt.spawn(async move {
            stream::download(this.http(), s.media, s.user_agent, buffer.clone(), refresh).await;
            // Fresh visionOS file URLs refused too: YouTube wants a PO token for them
            // here, past the first MiB. Its HLS segments play without one; the song
            // reopens where it stopped, and the rest of the session goes that way.
            if source == Source::VisionOs && buffer.refused() && !this.visionos_hls.swap(true, Ordering::Relaxed) {
                log::warn!("visionOS file streams are refused here past 1 MiB; playing HLS from now on");
            }
        });
        Ok(shared)
    }

    /// Resolves a fresh URL from the same source and format, for a download that got a 403.
    fn refresher(&self, source: Source, video_id: &str, opus: bool) -> stream::Refresh {
        let (this, id) = (self.clone(), video_id.to_owned());
        Arc::new(move || {
            let (this, id) = (this.clone(), id.clone());
            Box::pin(async move {
                let s = this.resolve_from(source, &id, opus).await?;
                // The buffer is laid out for the format it started with.
                anyhow::ensure!(s.opus == opus, "the stream changed format");
                Ok(s.media)
            })
        })
    }

    /// Download a song into the downloads folder, at the best quality the account gets.
    async fn download(&self, t: &Track, for_video: Option<String>) -> anyhow::Result<PathBuf> {
        let (source, s) = self.resolve(&t.id, true).await?;
        log::info!("download {}: {} bytes from {source:?}", t.id, s.size);
        anyhow::ensure!(s.starts.is_none(), "downloads need the file stream");
        let shared = s.buffer(false);
        stream::download(self.http(), s.media, s.user_agent, shared.clone(), Some(self.refresher(source, &t.id, s.opus))).await;
        if let Some(e) = shared.error() {
            anyhow::bail!("{e}");
        }
        let audio = shared.bytes().ok_or_else(|| anyhow::anyhow!("the download stopped early"))?;
        let cover = match model::pick(&t.thumbs, 120).map(|u| sized(u, 1200)) {
            Some(url) => async { self.http().get(url).send().await?.error_for_status()?.bytes().await }.await.ok().map(|b| b.to_vec()),
            None => None,
        };
        let (mut t, duration_ms, loudness) = (t.clone(), shared.duration_ms, shared.loudness_db);
        // Not every list gives a song's plays (album pages often don't): keep them with the download.
        if t.plays.is_none() {
            match ytm::song_plays(&self.api, &t).await {
                Ok(plays) => t.plays = plays,
                Err(e) => log::debug!("download {}: no play count: {e:#}", t.id),
            }
        }
        tokio::task::spawn_blocking(move || crate::downloads::save(&t, &audio, duration_ms, loudness, cover, for_video)).await?
    }

    /// Find a stream URL, trying the fastest source that can play the song. `best`
    /// (downloads) takes the high quality stream whatever the setting, and waits for
    /// the solver it needs. Playback takes Opus where there is one (Premium's 774 at
    /// ~256 kbps, else 251); downloads stay AAC, which saves as a tagged .m4a.
    async fn resolve(&self, video_id: &str, best: bool) -> anyhow::Result<(Source, ResolvedStream)> {
        let hq = best || self.hq.load(Ordering::Relaxed);
        if best
            && !self.jsc.ready()
            && let Some(session) = self.api.account()
            && self.is_premium(&session).await
            && let Err(e) = self.jsc.warm(&self.http()).await
        {
            log::warn!("solver warm-up failed: {e:#}");
        }
        // High quality needs the signed-in player's solver. Until it's prepared (a new
        // install, a new player version: ~5 s or more), play standard quality at once.
        // Without Premium it's never prepared, and standard quality is all there is.
        let solver_ready = self.jsc.ready();
        if hq && !solver_ready && self.api.account().is_some() && !self.known_not_premium() {
            log::info!("{video_id}: solver not ready yet, standard quality for now");
            self.warm_signed_in();
        }
        if hq
            && solver_ready
            && let Some(session) = self.api.account()
            && self.is_premium(&session).await
        {
            match self.resolve_from(Source::WebMusic, video_id, !best).await {
                Ok(s) => return Ok((Source::WebMusic, s)),
                // Not fatal: the usual 128 kbps stream still plays.
                Err(e) => log::warn!("high quality stream failed for {video_id} ({e:#}); using standard quality"),
            }
        }
        // Downloads stay on the file stream: they're saved as the .m4a it is.
        if !best && self.visionos_hls.load(Ordering::Relaxed) {
            match self.resolve_from(Source::VisionOsHls, video_id, false).await {
                Ok(s) => return Ok((Source::VisionOsHls, s)),
                // The signed-in paths below take it.
                Err(e) if needs_account(&e) => {}
                Err(e) => log::warn!("HLS stream failed for {video_id} ({e:#}); trying the file stream"),
            }
        }
        match self.resolve_from(Source::VisionOs, video_id, !best).await {
            Ok(s) => Ok((Source::VisionOs, s)),
            // Age-restricted or Premium-only: only signed-in web players get it.
            Err(e) if needs_account(&e) => {
                log::info!("{video_id} needs a signed-in player ({e:#})");
                let session = self.api.account().ok_or_else(|| anyhow::anyhow!("sign in to play this song"))?;
                if self.is_premium(&session).await {
                    match self.resolve_from(Source::WebMusic, video_id, !best).await {
                        Ok(s) => return Ok((Source::WebMusic, s)),
                        Err(e) => log::warn!("web music player failed ({e:#}); using yt-dlp"),
                    }
                }
                Ok((Source::YtDlp, self.resolve_from(Source::YtDlp, video_id, false).await?))
            }
            Err(e) => Err(e),
        }
    }

    /// `opus`: the Opus stream where there is one (visionOS's 251, or Premium's 774 from
    /// the web player); yt-dlp and HLS are AAC.
    async fn resolve_from(&self, source: Source, video_id: &str, opus: bool) -> anyhow::Result<ResolvedStream> {
        let plain = |s: innertube::Stream, ua: &str| ResolvedStream {
            media: stream::Media::File(s.url),
            starts: None,
            size: s.size,
            duration_ms: s.duration_ms,
            user_agent: ua.to_owned(),
            loudness_db: s.loudness_db,
            opus: s.opus,
        };
        Ok(match source {
            Source::VisionOs => plain(self.visionos_stream(video_id, opus).await?, innertube::UA),
            Source::VisionOsHls => {
                let s = self.visionos_hls(video_id).await?;
                let (starts, urls) = s.segments.into_iter().unzip();
                ResolvedStream { media: stream::Media::Segments(urls), starts: Some(starts), size: 0, duration_ms: s.duration_ms, user_agent: innertube::UA.to_owned(), loudness_db: s.loudness_db, opus: false }
            }
            Source::WebMusic => {
                let session = self.api.account().ok_or_else(|| anyhow::anyhow!("signed out"))?;
                plain(session.audio_stream(&self.http(), &self.jsc, video_id, opus).await?, webmusic::UA)
            }
            Source::YtDlp => {
                let session = self.api.account().ok_or_else(|| anyhow::anyhow!("sign in to play this song"))?;
                let s = ytdlp::resolve(&self.http(), &self.data_dir, &session.cookie, video_id).await?;
                // Cookies Google replaced while yt-dlp used the session.
                self.api.keep(&session, s.replaced.iter().map(String::as_str));
                // yt-dlp's JSON doesn't carry YouTube's loudness figure.
                ResolvedStream { media: stream::Media::File(s.url), starts: None, size: s.size, duration_ms: s.duration_ms, user_agent: s.user_agent, loudness_db: None, opus: false }
            }
        })
    }

    /// Get the signed-in playback path ready in the background, so the first Premium-only
    /// or age-restricted song doesn't wait for the Premium check or the solver setup that
    /// each new YouTube player version needs (~5 s, every few days).
    pub fn warm_signed_in(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(session) = this.api.account() else { return };
            if this.is_premium(&session).await
                && let Err(e) = this.jsc.warm(&this.http()).await
            {
                log::warn!("solver warm-up failed: {e:#}");
            }
        });
    }

    /// The Premium check has answered, and the account doesn't have it. Doesn't ask or
    /// wait: while a check is running, it's not known yet.
    fn known_not_premium(&self) -> bool {
        self.premium.try_lock().is_ok_and(|p| matches!(*p, Ok(false)))
    }

    /// Premium accounts can stream signed-in web player URLs without a PO token.
    async fn is_premium(&self, session: &webmusic::Session) -> bool {
        const PREMIUM_RETRY: std::time::Duration = std::time::Duration::from_secs(300);
        let mut premium = self.premium.lock().await;
        match *premium {
            Ok(p) => return p,
            Err(Some(at)) if at.elapsed() < PREMIUM_RETRY => return false,
            Err(_) => {}
        }
        match session.is_premium(&self.http()).await {
            Ok(p) => {
                log::info!("account has Premium: {p}");
                *premium = Ok(p);
                p
            }
            Err(e) => {
                log::warn!("Premium check failed: {e:#}");
                *premium = Err(Some(std::time::Instant::now()));
                false
            }
        }
    }

    async fn visionos_hls(&self, video_id: &str) -> anyhow::Result<innertube::HlsStream> {
        let vd = self.api.visitor_data(false).await?;
        match innertube::hls_audio(&self.http(), video_id, &vd).await {
            Ok(s) => Ok(s),
            Err(e) if needs_account(&e) => Err(e),
            Err(e) => {
                log::info!("visionOS player (HLS): {e:#}; retrying with new visitor data");
                let vd = self.api.visitor_data(true).await?;
                innertube::hls_audio(&self.http(), video_id, &vd).await
            }
        }
    }

    async fn visionos_stream(&self, video_id: &str, opus: bool) -> anyhow::Result<innertube::Stream> {
        let vd = self.api.visitor_data(false).await?;
        match innertube::audio_stream(&self.http(), video_id, &vd, opus).await {
            Ok(s) => Ok(s),
            Err(e) if needs_account(&e) => Err(e),
            Err(e) => {
                // A stale/flagged visitor ID is the usual cause; retry once with a fresh one.
                log::info!("visionOS player: {e:#}; retrying with new visitor data");
                let vd = self.api.visitor_data(true).await?;
                innertube::audio_stream(&self.http(), video_id, &vd, opus).await
            }
        }
    }

    /// Webview profile used by the sign-in window.
    pub fn login_profile(&self) -> PathBuf {
        self.data_dir.join("webview")
    }

    /// Report an error from outside the request flow (e.g. the sign-in window).
    pub fn report_error(&self, msg: String) {
        let _ = self.tx.send(Resp::Error(msg));
        self.wake();
    }

    pub fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }

    pub fn set_high_quality(&self, on: bool) {
        self.hq.store(on, Ordering::Relaxed);
    }

    /// Explicit songs hidden, and whether YouTube's Restricted Mode is asked for as well.
    pub fn set_hide_explicit(&self, on: bool, restricted: bool) {
        self.clean.store(on, Ordering::Relaxed);
        self.api.set_restricted(on && restricted);
    }

    /// Like or unlike a selection, one at a time, then check Liked Music and try again
    /// what didn't take. Unlikes still listed after that count as refused; likes aren't
    /// counted so (songs that can't be played are liked but not listed).
    async fn like_many(&self, ids: Vec<String>, like: bool) -> anyhow::Result<Resp> {
        let total = ids.len();
        let (done, failed, others) = like_many(&self.api, ids, like, |taken, n| {
            let _ = self.tx.send(Resp::LikeProgress { taken: taken.into_iter().map(str::to_owned).collect(), done: n, total, like });
            self.wake();
        })
        .await;
        // Even when all were refused: the app stops waiting on them and shows them as they are.
        Ok(Resp::LikedMany { video_ids: done, failed, like, others })
    }

    /// Send a playlist's first page right away, then each next page as it arrives
    /// (YouTube takes about half a second per page of 100). The last one is returned.
    async fn stream_playlist(&self, id: String, load: u64, me: bool) -> anyhow::Result<Resp> {
        let (c, mut token, limit) = ytm::playlist_start(&self.api, &id, me, None).await?;
        let mut have = c.tracks.len();
        let mut resp = Resp::Collection { load, c, more: token.is_some() && have < limit };
        while let Some(t) = token.take().filter(|_| have < limit) {
            let _ = self.tx.send(resp);
            self.wake();
            // Failing here, the page keeps what came.
            let (mut tracks, mut set_ids, next) = ytm::playlist_more(&self.api, &t, me).await?;
            tracks.truncate(limit - have);
            set_ids.truncate(limit - have);
            have += tracks.len();
            token = next.filter(|_| !tracks.is_empty());
            resp = Resp::CollectionMore { id: id.clone(), load, more: token.is_some() && have < limit, tracks, set_ids, failed: false };
        }
        Ok(resp)
    }

    pub fn is_logged_in(&self) -> bool {
        self.api.account().is_some()
    }
}

/// Test instances (`YTM_BACKGROUND`) keep plays out of the account's history unless
/// `YTM_REPORT_PLAYS` is set.
fn reports_plays() -> bool {
    std::env::var_os("YTM_BACKGROUND").is_none() || std::env::var_os("YTM_REPORT_PLAYS").is_some()
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Source {
    VisionOs,
    /// The visionOS player's HLS segments: where its file URLs are cut off at 1 MiB.
    VisionOsHls,
    WebMusic,
    YtDlp,
}

struct ResolvedStream {
    media: stream::Media,
    /// HLS: each segment's start (s).
    starts: Option<Vec<f64>>,
    /// Bytes (a file; 0 for segments).
    size: u64,
    duration_ms: u64,
    user_agent: String,
    loudness_db: Option<f32>,
    /// Opus in WebM; otherwise AAC in MP4.
    opus: bool,
}

impl ResolvedStream {
    /// A buffer laid out for this stream. `head_only`: a prefetch's.
    fn buffer(&self, head_only: bool) -> Arc<Shared> {
        match &self.starts {
            Some(starts) => Shared::segmented(starts.clone(), self.duration_ms, self.loudness_db, head_only),
            None => Shared::new(self.size, self.duration_ms, self.loudness_db, self.opus, head_only),
        }
    }
}

/// The anonymous visionOS player refused because the song needs an account:
/// age-restricted (LOGIN_REQUIRED) or Music Premium only.
fn needs_account(e: &anyhow::Error) -> bool {
    let msg = e.to_string();
    msg.contains("LOGIN_REQUIRED") || msg.contains("Premium")
}

/// Keep the signed-in release feed's order, using US charts and the user's home artists.
fn popular_new_albums(albums: &[Card], ranked_artists: &[String], preferred_artists: &[String]) -> Vec<Card> {
    let chart_ids: std::collections::HashSet<&str> = ranked_artists.iter().chain(preferred_artists).map(String::as_str).collect();
    let mut seen = std::collections::HashSet::new();
    albums
        .iter()
        .filter(|album| album.links.iter().any(|a| a.id.as_deref().is_some_and(|id| chart_ids.contains(id))))
        .filter(|album| seen.insert(match &album.target {
            Target::Album(id) => id.as_str(),
            _ => album.title.as_str(),
        }))
        .take(16)
        .cloned()
        .collect()
}

/// "Episodes for later" is listed even when empty; hide it then.
fn saved_playlist_visible(card: &Card) -> bool {
    card.target != Target::Playlist("SE".into()) || card.subtitle.split_whitespace().any(|w| w.parse::<u64>().is_ok_and(|n| n > 0))
}

pub fn compact(n: u64) -> String {
    let (v, unit) = match n {
        0..1_000 => return n.to_string(),
        1_000..1_000_000 => (n as f64 / 1e3, "K"),
        1_000_000..1_000_000_000 => (n as f64 / 1e6, "M"),
        _ => (n as f64 / 1e9, "B"),
    };
    // "6M", not "6.0M"; three significant figures and up need no decimal.
    let s = if v >= 100.0 { format!("{v:.0}") } else { format!("{v:.1}") };
    format!("{}{unit}", s.strip_suffix(".0").unwrap_or(&s))
}

// Larger art URL for the background / now-playing view.
pub fn art_url(t: &Track) -> Option<String> {
    model::pick(&t.thumbs, 120).map(|u| sized(u, 544))
}

#[cfg(test)]
mod explore_tests {
    use super::*;

    fn card(title: &str, target: Target, artist_id: Option<&str>, subtitle: &str) -> Card {
        Card {
            title: title.into(),
            subtitle: subtitle.into(),
            thumbs: vec![],
            target,
            round: false,
            links: artist_id.map(|id| model::Link { name: "Artist".into(), id: Some(id.into()) }).into_iter().collect(),
        }
    }

    #[test]
    fn new_releases_only_include_charting_artists_and_deduplicate() {
        let album = |id: &str, artist_id: &str| card(id, Target::Album(id.into()), Some(artist_id), "Album");
        let albums = vec![album("obscure", "other"), album("favorite-release", "favorite"), album("first-release", "popular"), album("second-release", "popular"), album("first-release", "popular")];
        let found = popular_new_albums(&albums, &["popular".into(), "popular".into()], &["favorite".into()]);
        assert_eq!(found.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(), ["favorite-release", "first-release", "second-release"]);
    }

    #[test]
    fn empty_episodes_for_later_is_hidden() {
        let item = |id: &str, subtitle: &str| card("Episodes for later", Target::Playlist(id.into()), None, subtitle);
        assert!(!saved_playlist_visible(&item("SE", "Auto playlist")));
        assert!(!saved_playlist_visible(&item("SE", "Auto playlist • 0 episodes")));
        assert!(saved_playlist_visible(&item("SE", "Auto playlist • 2 episodes")));
        assert!(saved_playlist_visible(&item("PLcustom", "Playlist")));
    }
}
