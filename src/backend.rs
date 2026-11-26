//! Network side: a small tokio runtime talking to YouTube Music (`ytmapi`/`ytm`).
//!
//! The UI sends `Req`s and drains `Resp`s once per frame; every response wakes the UI.
//! Playback resolution also lives here: resolve stream → start progressive download →
//! build the decoder on a blocking thread → hand it to the audio engine.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crossbeam_channel::Sender;
use tokio::runtime::Runtime;

use crate::art::Art;
use crate::audio::{AudioCmd, AudioHandle};
use crate::link::Link;
use crate::lyrics::{self, LyricsSet};
use crate::model::{self, Card, Target, Thumb, Track, sized};
use crate::stream::{self, Shared};
use crate::{innertube, jsc, webmusic, ytdlp, ytm, ytmapi};

#[derive(Default)]
pub struct UnlikeVersions {
    /// The known music videos that Liked Music lists as their audio versions.
    pub stand_ins: HashMap<String, String>,
    /// Uploads that received a new rating while these unlikes were pending.
    pub keep: HashSet<String>,
}

pub enum Req {
    /// Home feed, optionally filtered by a mood chip's params.
    Home(Option<String>),
    HomeMore(String),
    MoreQuickPicks(String),
    Explore(Vec<String>),
    /// A Moods & genres category, by its browse params.
    Mood(String),
    Search(String),
    /// A search of only your library or only your uploads.
    SearchIn { query: String, scope: ytm::Scope },
    /// Suggestions for a query; `round` comes back with them, so stale ones can be dropped.
    Suggest { query: String, round: u64 },
    /// What a pasted link (a `Link::Song` or `Link::Resolve`) stands for.
    Link { link: Link, to: LinkUse },
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
    SongRating { video_id: String, round: u64 },
    /// Upload audio files to the account's library.
    Upload(Vec<PathBuf>),
    /// Save a song's audio to the downloads folder. `for_video`: the music video it's the
    /// song version of, which then plays it from the file (offline too).
    Download { track: Track, for_video: Option<String> },
    /// Download the songs of an album or playlist (`target`; fetched whole when `tracks`
    /// is `None`) or of a selection (`target` `None`). Songs already downloaded are
    /// skipped. More than `LARGE_DOWNLOAD` to go asks first (`Resp::ConfirmDownload`)
    /// unless `confirmed`.
    DownloadMany { target: Option<Target>, title: String, tracks: Option<Vec<Track>>, confirmed: bool },
    Lyrics(Track),
    /// SponsorBlock's parts of a video to skip.
    Segments { video_id: String },
    /// An artist's picture, for the badge on Discord's card: `id`'s, looked up by `name`.
    ArtistPicture { id: String, name: String },
    /// Romanization and translation for the lyric lines of one source.
    LyricsExtras { video_id: String, source: String, lines: Vec<String>, lang: Option<String>, target: String },
    /// Process cover art for the background; `key` is echoed back. `None`: the song has
    /// no cover (answered at once, with none).
    Art { key: String, url: Option<String> },
    /// Radio seeded from a track (`seed`, its id); appended to the queue. `song_of`: the
    /// track, when it's a music video to play as its song: the song's radio is asked for.
    Radio { seed: String, song_of: Option<Track>, smart: bool },
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
    /// A song's music video, for Now playing's Song/Video switch.
    MusicVideo(Track),
    /// The animated cover standing in for a song's cover, if it has one.
    Canvas(Track),
    /// Resolve and download a track ahead of time so skipping to it is instant.
    Prefetch { video_id: String, song_of: Option<Track> },
    /// Open and queue the next track's decoder ahead of the current song's end for gapless playback.
    StageNext { video_id: String, after: u64, generation: u64, song_of: Option<Track>, fallback_duration_ms: u64 },
    /// A staged gapless track began playing: promote its buffer to `current` and report the play.
    GaplessStarted { video_id: String, generation: u64 },
    Like { video_id: String, like: bool },
    /// Take away the likes of a music video and the song version standing in for it.
    UnlikeBoth { song: String, video: String },
    /// Several likes (or unlikes) one after another, for a selection.
    LikeMany { video_ids: Vec<String>, like: bool, versions: UnlikeVersions },
    Dislike { video_id: String, skip_generation: Option<u64> },
    RemoveRating { video_id: String, was_disliked: bool },
    CreatePlaylist { title: String, video_ids: Vec<String> },
    /// `cover`: its picture changed too.
    EditPlaylist { id: String, title: String, description: String, privacy: Option<String>, cover: Option<CoverChange> },
    /// Save one of your playlist's sort orders to it ([`PlaylistSort::actions`]).
    SortPlaylist { id: String, actions: serde_json::Value },
    AddToPlaylist { playlist_id: String, tracks: Vec<Track>, duplicates: ytm::Duplicates },
    /// `(videoId, setVideoId)` entries to take out of the playlist.
    RemoveFromPlaylist { playlist_id: String, items: Vec<(String, String)> },
    /// Move an entry to just before another, or to the end.
    MoveInPlaylist { playlist_id: String, move_id: u64, set_id: String, before: Option<String> },
    DeletePlaylist(String),
    /// An uploaded song (`Track::upload`), and its title for the error.
    DeleteUpload { entity: String, title: String },
    /// A playlist's collaboration settings and invite link.
    Collaboration(String),
    SetCollaboration { info: ytm::Collaboration, change: ytm::CollabChange },
    SaveCollection { id: String, save: bool },
    Subscribe { channel_id: String, on: bool },
    SetCookie(String),
    Logout,
    /// Whether YouTube Music can be reached now, asked while a page says it can't (#366).
    Reachable,
}

impl Req {
    fn name(&self) -> &'static str {
        match self {
            Req::Home(_) | Req::HomeMore(_) => crate::tr!("settings-home"),
            Req::MoreQuickPicks(_) => crate::tr!("nav-quick-picks"),
            Req::Explore(_) => crate::tr!("nav-explore"),
            Req::Mood(_) => crate::tr!("nav-mood-genres"),
            Req::Search(_) | Req::SearchIn { .. } => crate::tr!("settings-search"),
            Req::Suggest { .. } => crate::tr!("settings-search"),
            Req::ForgetSearch(_) => crate::tr!("nav-remove-recent-search"),
            Req::Album { .. } => crate::tr!("ui-album"),
            Req::Artist(_) => crate::tr!("ui-artist"),
            Req::Playlist { .. } => crate::tr!("transfer-playlist"),
            Req::Library => crate::tr!("nav-library"),
            Req::History => crate::tr!("nav-history"),
            Req::Uploads => crate::tr!("nav-uploads"),
            Req::Account => crate::tr!("settings-account"),
            Req::Channels => crate::tr!("settings-channel"),
            Req::SwitchChannel { .. } => crate::tr!("backend-channel-switch"),
            Req::ServerQueue => crate::tr!("ui-queue"),
            Req::SongRating { .. } => crate::tr!("backend-rating"),
            Req::Upload(_) => crate::tr!("nav-upload-music"),
            Req::Download { .. } | Req::DownloadMany { .. } => crate::tr!("settings-download"),
            Req::Lyrics(_) => crate::tr!("settings-lyrics"),
            Req::Segments { .. } => "SponsorBlock",
            Req::ArtistPicture { .. } => crate::tr!("discord-artist-photo"),
            Req::LyricsExtras { .. } => crate::tr!("settings-translation"),
            Req::Art { .. } => crate::tr!("discord-album-art"),
            Req::Radio { .. } => crate::tr!("ui-radio"),
            Req::Comments { .. } | Req::CommentsPage { .. } | Req::CommentAction { .. } => crate::tr!("player-comments"),
            Req::Link { .. } => crate::tr!("link-open"),
            Req::PlayCollection { .. } => crate::tr!("settings-playback"),
            Req::Play { .. } => crate::tr!("settings-playback"),
            Req::Prefetch { .. } | Req::StageNext { .. } | Req::GaplessStarted { .. } => crate::tr!("settings-playback"),
            Req::VideoStream { .. } | Req::MusicVideo(_) => crate::tr!("ui-video"),
            Req::Canvas(_) => crate::tr!("settings-animated-covers"),
            Req::Like { .. } | Req::LikeMany { .. } | Req::UnlikeBoth { .. } => crate::tr!("ui-like"),
            Req::Dislike { .. } => crate::tr!("ui-dislike-song"),
            Req::RemoveRating { .. } => crate::tr!("backend-rating"),
            Req::CreatePlaylist { .. } => crate::tr!("transfer-create-playlist"),
            Req::EditPlaylist { .. } => crate::tr!("dialog-edit-playlist"),
            Req::SortPlaylist { .. } => crate::tr!("ui-sort"),
            Req::AddToPlaylist { .. } => crate::tr!("ui-add-playlist"),
            Req::RemoveFromPlaylist { .. } => crate::tr!("ui-remove-playlist"),
            Req::MoveInPlaylist { .. } => crate::tr!("backend-move-playlist"),
            Req::DeletePlaylist(_) => crate::tr!("dialog-delete-playlist"),
            Req::DeleteUpload { .. } => crate::tr!("dialog-delete-upload"),
            Req::Collaboration(_) => crate::tr!("ui-collaborate"),
            Req::SetCollaboration { .. } => crate::tr!("ui-collaborate"),
            Req::SaveCollection { .. } => crate::tr!("ui-save-library"),
            Req::Subscribe { .. } => crate::tr!("ui-subscribe"),
            Req::SetCookie(_) => crate::tr!("nav-sign-in"),
            Req::Logout => crate::tr!("settings-sign-out"),
            Req::Reachable => crate::tr!("settings-network"),
        }
    }

    /// Background requests fail quietly (logged, no toast).
    fn quiet(&self) -> bool {
        matches!(self, Req::Suggest { .. } | Req::Account | Req::Channels | Req::ServerQueue | Req::SongRating { .. } | Req::Art { .. } | Req::Radio { smart: true, .. } | Req::Prefetch { .. } | Req::StageNext { .. } | Req::GaplessStarted { .. } | Req::HomeMore(_) | Req::Lyrics(_) | Req::Segments { .. } | Req::ArtistPicture { .. } | Req::MusicVideo(_) | Req::LyricsExtras { .. } | Req::Comments { .. } | Req::CommentsPage { .. } | Req::CommentAction { .. } | Req::Reachable)
    }
}

/// What a link is looked up for.
#[derive(Clone, Copy, PartialEq)]
pub enum LinkUse {
    Open,
    /// Its song goes to the queue's end.
    Queue,
    /// Its song shows under the search box.
    Preview,
}

/// What a link turned out to be.
pub enum LinkTo {
    Page(Target),
    Song(Track),
    /// The song at this place in the album or playlist the link names.
    InList(Target, Vec<Track>, usize),
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
    /// All of YouTube Music, or just your library or your uploads.
    pub scope: ytm::Scope,
    pub tracks: Vec<Track>,
    /// Videos, including uploads that have no song version (remixes, live sets).
    pub videos: Vec<Track>,
    pub albums: Vec<Card>,
    pub artists: Vec<Card>,
    pub playlists: Vec<Card>,
    /// Playlists made by YouTube Music's listeners.
    pub community: Vec<Card>,
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
    /// Your own playlist's sort orders, from YouTube Music's menu. Empty for anything else.
    pub sorts: Vec<PlaylistSort>,
}

/// One of YouTube Music's orders for your own playlist. Picking one saves it to the
/// playlist, as YouTube Music does, so it loads in that order everywhere. Manual ordering
/// brings the custom order back as it was.
pub struct PlaylistSort {
    pub name: String,
    /// The order it loads in now.
    pub on: bool,
    /// The custom order, the one songs can be dragged around in.
    pub manual: bool,
    /// What picking it sends to `browse/edit_playlist`.
    pub actions: serde_json::Value,
}

impl Collection {
    /// A podcast show: its page lists episodes.
    pub fn is_show(&self) -> bool {
        ytm::list_id(&self.id) != self.id
    }

    /// The number of tracks or episodes in this collection.
    pub fn item_count(&self, count: u64) -> String {
        if self.is_show() {
            crate::tr!("status-episodes", count = count)
        } else {
            crate::tr!("status-songs", count = count)
        }
    }

    /// The subtitle, with the song count for a playlist.
    pub fn byline(&self) -> String {
        match self.count {
            Some(n) if self.subtitle.is_empty() => self.item_count(n),
            Some(n) => format!("{} • {}", self.subtitle, self.item_count(n)),
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
    /// The account subscribes to it (false signed out).
    pub subscribed: bool,
    /// What subscribing takes; it isn't always the page's id.
    pub channel_id: Option<String>,
    /// When it was asked for: a subscribe change answered since outranks `subscribed`.
    pub asked: std::time::Instant,
    pub description: Option<String>,
    pub top: Vec<Track>,
    /// "Top songs" playlist behind the shelf's Show all button.
    pub top_playlist: Option<String>,
    pub albums: Vec<Card>,
    pub videos: Vec<Card>,
    /// Their songs in your library ("From your library"); signed in only.
    pub library: Vec<Card>,
    pub playlists: Vec<Card>,
    pub similar: Vec<Card>,
}

pub struct Library {
    pub playlists: Vec<Card>,
    pub albums: Vec<Card>,
    /// Saved podcast shows, when podcasts are shown.
    pub podcasts: Vec<Card>,
    /// Subscribed artists.
    pub artists: Vec<Card>,
    /// That part didn't load (YouTube Music refused it, say): the one shown stays.
    pub playlists_failed: bool,
    pub albums_failed: bool,
    pub artists_failed: bool,
}

/// The signed-in account's channels.
pub struct Channels {
    pub list: Vec<ytm::Channel>,
    /// The page id of the one requests go as (`None`: the account's own).
    pub current: Option<String>,
    /// [`ytmapi::Api::epoch`] when listed: a switch picked from here applies only then.
    pub epoch: u64,
}

/// A playlist's picture changed in Edit playlist.
pub enum CoverChange {
    /// A picture of the listener's own, a square JPEG (`art::playlist_cover`).
    Set(Vec<u8>),
    /// Back to YouTube Music's own cover.
    Remove,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PlaylistChange {
    Added,
    Updated,
    Sorted,
    SortFailed,
    Removed,
    Deleted,
    SavedToLibrary,
    RemovedFromLibrary,
}

pub enum Resp {
    Home(ytm::HomePage),
    HomeMore(ytm::HomePage),
    QuickPicksMore { seed: String, tracks: Vec<Track> },
    Explore(Explore),
    Mood(ytm::MoodPage),
    Search(SearchResults),
    /// A search of your library or your uploads (`SearchResults::scope`).
    SearchIn(SearchResults),
    /// `None`: this video can't be shown; the picture stays.
    VideoStream { video_id: String, max_height: u32, stream: Option<innertube::VideoStream> },
    /// `clip`: the HLS playlist of the song's animated cover; `None`: it has none.
    Canvas { video_id: String, clip: Option<String> },
    Suggest { query: String, round: u64, terms: Vec<ytm::Suggestion>, hits: Vec<Card> },
    /// An album or playlist; `more`: the rest of its songs follow in `CollectionMore`.
    Collection { load: u64, c: Collection, more: bool },
    /// The next page of a playlist sent as `Collection` with the same `load`.
    CollectionMore { id: String, load: u64, tracks: Vec<Track>, set_ids: Vec<Option<String>>, more: bool, failed: bool },
    Artist(ArtistPage),
    Library(Library),
    History(Vec<Track>),
    Uploads(Vec<Track>),
    /// An upload was deleted: the uploads as they are now.
    UploadDeleted(Vec<Track>),
    Account { name: Option<String>, photo: Option<String> },
    Channels(Channels),
    /// A `Req::SwitchChannel` is done (requests go as `epoch` now), or `error` says why it isn't.
    SwitchedChannel { page_id: Option<String>, epoch: u64, error: Option<String> },
    ServerQueue(ytm::ServerQueue),
    /// Upload finished: how many files went up, and which failed.
    Uploaded { done: usize, failed: Vec<String> },
    Downloaded { title: String, path: PathBuf },
    /// Every album, playlist or selection being downloaded, after one of them moved on.
    /// A finished one (`DownloadRun::finished`) is listed this once.
    Downloads(Vec<DownloadRun>),
    /// More than `LARGE_DOWNLOAD` songs (`count`) aren't downloaded yet: asked before
    /// sending the same `Req::DownloadMany` again, `confirmed`.
    ConfirmDownload { target: Option<Target>, title: String, tracks: Vec<Track>, count: usize },
    /// Every source's lyrics for a song (empty when nobody has them).
    Lyrics { video_id: String, lyrics: LyricsSet },
    /// Who submitted a song's saved Unison lyrics, found after they were shown crediting
    /// Unison alone: each source's credit.
    UnisonCredit { video_id: String, credits: Vec<(lyrics::Provider, lyrics::UnisonCredit)> },
    Segments { video_id: String, segments: Vec<crate::sponsorblock::Segment> },
    /// `None`: none was found.
    ArtistPicture { id: String, pic: Option<crate::discord::Badge> },
    /// Romanization/translation for one source's lines, arriving after them.
    LyricsExtras { video_id: String, source: String, target: String, extras: crate::translate::Extras },
    /// `None`: there's no cover to take colours from (the song has none, or it couldn't
    /// be fetched).
    Art { key: String, art: Option<Art> },
    Radio { seed: String, tracks: Vec<Track>, smart: bool },
    /// A comments response (`Req::Comments`, `CommentsPage` or `CommentAction`) by its tag;
    /// `None`: it failed.
    CommentsPage { token: String, page: Option<serde_json::Value> },
    /// Or why it can't be opened.
    Link { link: Link, to: LinkUse, found: Result<LinkTo, String> },
    /// What was asked to play (a `Req::PlayCollection`), its name, and its songs.
    PlayQueue { target: Target, title: String, tracks: Vec<Track> },
    /// `other`: its other version, whose like went with this one.
    Liked { video_id: String, like: bool, other: Option<String> },
    Disliked { video_id: String, skip_generation: Option<u64>, ok: bool },
    RatingRemoved { video_id: String, was_disliked: bool, ok: bool },
    SongRating { video_id: String, round: u64, rating: Option<ytm::SongRating> },
    /// A run of likes or unlikes: `taken` just went through, `done` of `total` tried.
    LikeProgress { taken: Vec<String>, done: usize, total: usize, like: bool },
    /// The songs YouTube Music took, and the ones it refused.
    LikedMany { video_ids: Vec<String>, failed: Vec<String>, like: bool, others: Vec<String> },
    /// A subscribe or unsubscribe went through (`ok`), or didn't.
    Subscribed { channel_id: String, on: bool, ok: bool },
    PlaylistCreated { id: String, songs: usize },
    PlaylistChanged { id: String, change: PlaylistChange },
    /// Songs weren't taken out of this playlist, though they've left it here already.
    RemoveFailed { id: String, error: String },
    /// A playlist wasn't deleted, though it's left the library here already.
    DeleteFailed { error: String },
    AddedToPlaylist { id: String, video_ids: Vec<String>, error: Option<String> },
    /// None of `tracks` went in: some are in the playlist already ([`ytm::Duplicates::Ask`]).
    AlreadyInPlaylist { id: String, tracks: Vec<Track> },
    /// YouTube Music answered a song dragged to a new place.
    PlaylistMoved { id: String, move_id: u64, ok: bool },
    Collaboration(ytm::Collaboration),
    /// Playback for `generation` failed before reaching the audio engine.
    PlayError { generation: u64, msg: String, offer_backup_at: Option<std::time::Duration> },
    /// Play `generation` is the song version of the music video that was asked for
    /// (`None`: that song wouldn't start, and the music video plays after all).
    PlayingAs { generation: u64, listed: String, song: Option<Track> },
    /// A staged gapless track resolved to its song version before transitioning.
    StagedSong { generation: u64, listed: String, song: Track },
    /// The song `song`'s music video (`None`: it has none, or none was sure).
    MusicVideo { song: String, video: Option<ytm::MusicVideo> },
    LoggedIn(bool),
    /// A past search left the account's history; the list already shows it gone.
    SearchForgotten,
    Error(String),
    /// A request failed because YouTube Music can't be reached at all (no connection):
    /// one plain message for all of them, not each one's error (#353).
    Offline,
    /// `Req::Reachable`'s answer.
    Reachable(bool),
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
                | Resp::SearchIn(_)
                | Resp::Library(_)
                | Resp::History(_)
                | Resp::Uploads(_)
                | Resp::Account { .. }
                | Resp::Channels(_)
                | Resp::ServerQueue(_)
                | Resp::Liked { .. }
                | Resp::Disliked { .. }
                | Resp::RatingRemoved { .. }
                | Resp::SongRating { .. }
                | Resp::LikeProgress { .. }
                | Resp::LikedMany { .. }
                | Resp::AddedToPlaylist { .. }
                | Resp::AlreadyInPlaylist { .. }
                | Resp::Artist(_)
                | Resp::Link { .. }
                | Resp::Subscribed { .. }
        )
    }

    /// An album or playlist load that ends without more songs: also sent when it fails,
    /// so the page stops waiting for it.
    fn ended(id: String, load: u64) -> Self {
        Resp::CollectionMore { id, load, tracks: Vec::new(), set_ids: Vec::new(), more: false, failed: true }
    }
}

/// A search of your library or uploads, laid out as the search page shows any search:
/// songs apart from videos, and the pages it found by kind.
fn search_in(found: ytm::ScopedResults, query: String, scope: ytm::Scope) -> SearchResults {
    let (videos, tracks) = found.tracks.into_iter().partition(|t| t.video);
    let mut r = SearchResults { query, scope, tracks, videos, albums: Vec::new(), artists: Vec::new(), playlists: Vec::new(), community: Vec::new(), podcasts: Vec::new(), episodes: found.episodes };
    for c in found.cards {
        match &c.target {
            Target::Artist(_) => r.artists.push(c),
            Target::Album(_) => r.albums.push(c),
            Target::Playlist(id) if ytm::list_id(id) != id => r.podcasts.push(c),
            Target::Playlist(_) => r.playlists.push(c),
            Target::Song(_) => {}
        }
    }
    r
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
/// sent again, up to three times in all. An unlike removes the counterpart's like too,
/// unless that version received a new rating.
async fn like_one(api: &ytmapi::Api, id: &str, like: bool, versions: &UnlikeVersions, others: &mut Vec<String>) -> bool {
    let epoch = api.epoch();
    for tries in 0..3 {
        match tokio::time::timeout(LIKE_WAIT, ytm::set_like(api, id, like)).await {
            Ok(Ok(())) => {
                // Outside that wait: it takes longer, and checking Liked Music afterwards
                // catches a like that's still there.
                if !like {
                    let other = match versions.stand_ins.get(id) {
                        Some(video) if versions.keep.contains(video) => Ok(None),
                        Some(video) => {
                            tokio::time::sleep(LIKE_GAP).await;
                            ytm::set_like(api, video, false).await.map(|()| Some(video.clone()))
                        }
                        None => ytm::remove_other_half_like(api, id, epoch, &versions.keep).await,
                    };
                    match other {
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
/// Third: the other versions whose likes went with the unlikes (`ytm::remove_other_half_like`,
/// or the known stand-ins in Liked Music).
pub async fn like_many(
    api: &ytmapi::Api,
    ids: Vec<String>,
    like: bool,
    versions: &UnlikeVersions,
    mut progress: impl FnMut(Option<&str>, usize),
) -> (Vec<String>, Vec<String>, Vec<String>) {
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
        let taken = like_one(api, id, like, versions, &mut others).await;
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
            like_one(api, id, like, versions, &mut others).await;
            tokio::time::sleep(LIKE_GAP).await;
        }
    }
    let done: Vec<String> = ids.into_iter().filter(|id| !failed.contains(id)).collect();
    (done, failed, others)
}

/// Songs of an album or playlist downloaded at once (each over `stream::WORKERS`
/// connections, behind the playing song).
const DOWNLOADS_AT_ONCE: usize = 2;
/// More songs than this to download in one go are asked about first.
pub const LARGE_DOWNLOAD: usize = 100;

/// An album, playlist or selection being downloaded.
#[derive(Clone, Debug, Default)]
pub struct DownloadRun {
    /// The album's or playlist's id, or `selection-N` for a selection.
    pub key: String,
    /// Empty for a selection.
    pub title: String,
    /// All its songs (those downloaded already count as done from the start), and how many are through.
    pub total: usize,
    pub done: usize,
    pub saved: usize,
    /// Already downloaded (or already on the way for another run).
    pub skipped: usize,
    /// Titles of the songs that couldn't be downloaded.
    pub failed: Vec<String>,
    /// Their ids, in the same order: queued again, the run tries them again.
    pub failed_ids: Vec<String>,
    pub cancelled: bool,
    /// The album's or playlist's own folder its songs go in (the setting's on).
    pub folder: Option<crate::downloads::ListFolder>,
}

impl DownloadRun {
    pub fn finished(&self) -> bool {
        self.done >= self.total
    }
}

/// Songs waiting to download, one queue for every run, worked through by up to
/// `DOWNLOADS_AT_ONCE` tasks.
#[derive(Default)]
struct DownloadQueue {
    waiting: std::collections::VecDeque<Track>,
    /// Each song waiting or downloading, with the keys of the runs that want it: it
    /// downloads once, for all of them.
    wanted: std::collections::HashMap<String, Vec<String>>,
    /// Unfinished runs.
    runs: Vec<DownloadRun>,
    workers: usize,
    /// Songs being saved now, by any download.
    saving: std::collections::HashSet<String>,
    /// Selections queued this session, for their keys.
    selections: usize,
}

impl DownloadQueue {
    /// Every run as it stands; a finished one is listed this once.
    fn snapshot(&mut self) -> Resp {
        let runs = self.runs.clone();
        self.runs.retain(|r| !r.finished());
        Resp::Downloads(runs)
    }

    /// Queue the songs `todo` of a list of `listed` songs (the others are downloaded
    /// already) for the run `key`, or a new selection's. One on its way for another run
    /// already downloads for this one too; one on its way for this run isn't queued
    /// twice. Queued again, a run takes on just the songs it didn't have, and tries again
    /// the ones that failed (they're its songs already: not counted twice). Returns the
    /// run's key and how many songs were queued for it.
    fn add(&mut self, key: Option<String>, title: String, listed: usize, todo: Vec<Track>, folder: Option<crate::downloads::ListFolder>) -> (String, usize) {
        let skipped = listed.saturating_sub(todo.len());
        let key = key.unwrap_or_else(|| {
            self.selections += 1;
            format!("selection-{}", self.selections)
        });
        let at = self.runs.iter().position(|r| r.key == key);
        let (mut queued, mut added) = (0, 0);
        for t in todo {
            match self.wanted.get_mut(&t.id) {
                Some(keys) if keys.contains(&key) => continue,
                Some(keys) => keys.push(key.clone()),
                None => {
                    self.wanted.insert(t.id.clone(), vec![key.clone()]);
                    self.waiting.push_back(t.clone());
                }
            }
            queued += 1;
            let run = at.map(|i| &mut self.runs[i]);
            match run.and_then(|r| Some((r.failed_ids.iter().position(|id| *id == t.id)?, r))) {
                Some((n, run)) => {
                    run.failed_ids.remove(n);
                    run.failed.remove(n);
                    run.done -= 1;
                }
                None => added += 1,
            }
        }
        // The ones on disk already count as done, so the progress (and the cover's veil)
        // starts from how much of the list is there.
        match at.map(|i| &mut self.runs[i]) {
            Some(run) => {
                run.total += added;
                run.cancelled = false;
                run.folder = folder;
            }
            None => self.runs.push(DownloadRun { key: key.clone(), title, total: added + skipped, done: skipped, skipped, folder, ..Default::default() }),
        }
        (key, queued)
    }

    /// The folder a song waiting or downloading goes in: that of the first run that
    /// wants it and has one (none: the downloads folder itself).
    fn folder_for(&self, id: &str) -> Option<crate::downloads::ListFolder> {
        self.wanted.get(id)?.iter().find_map(|k| self.runs.iter().find(|r| &r.key == k)?.folder.clone())
    }

    /// The next song to download, or none: then the worker asking stops.
    fn next(&mut self) -> Option<Track> {
        let next = self.waiting.pop_front();
        if next.is_none() {
            self.workers -= 1;
        }
        next
    }

    /// How many more workers to start for the songs waiting.
    fn more_workers(&mut self) -> usize {
        let start = DOWNLOADS_AT_ONCE.min(self.workers + self.waiting.len()).saturating_sub(self.workers);
        self.workers += start;
        start
    }

    /// A song is through, for every run that wanted it: `saved`, there already
    /// (`Ok(false)`), or failed.
    fn finish<E>(&mut self, t: &Track, result: &Result<bool, E>) {
        for key in self.wanted.remove(&t.id).unwrap_or_default() {
            let Some(run) = self.runs.iter_mut().find(|r| r.key == key) else { continue };
            run.done += 1;
            match result {
                Ok(true) => run.saved += 1,
                Ok(false) => run.skipped += 1,
                Err(_) => {
                    run.failed.push(t.title.clone());
                    run.failed_ids.push(t.id.clone());
                }
            }
        }
    }

    /// Stop the run `key`: its waiting songs are dropped (unless another run wants them
    /// too); the ones downloading now finish. False when there's no such run.
    fn cancel(&mut self, key: &str) -> bool {
        let mut dropped = 0;
        let DownloadQueue { waiting, wanted, .. } = self;
        waiting.retain(|t| {
            let Some(keys) = wanted.get_mut(&t.id) else { return true };
            let before = keys.len();
            keys.retain(|k| k != key);
            dropped += before - keys.len();
            let keep = !keys.is_empty();
            if !keep {
                wanted.remove(&t.id);
            }
            keep
        });
        let Some(run) = self.runs.iter_mut().find(|r| r.key == key) else { return false };
        run.total -= dropped;
        run.cancelled = true;
        true
    }
}

/// A song claimed for saving (`DownloadQueue::saving`), let go when dropped.
struct Saving(Arc<std::sync::Mutex<DownloadQueue>>, String);

impl Drop for Saving {
    fn drop(&mut self) {
        self.0.lock().unwrap().saving.remove(&self.1);
    }
}

/// The song's file if it's downloaded already (and then, given `for_video`, that music
/// video plays it too): disk work, off the async workers.
async fn reuse_download(song_id: &str, for_video: Option<String>) -> Option<PathBuf> {
    let id = song_id.to_owned();
    tokio::task::spawn_blocking(move || crate::downloads::reuse(&id, for_video.as_deref())).await.ok().flatten()
}

/// Downloaded already: the song, or a music video's song version.
pub fn is_downloaded(t: &Track) -> bool {
    crate::downloads::path(&t.id).is_some() || (t.music_video && crate::downloads::song_for(&t.id).is_some())
}

/// A video id and its stream buffer.
type StreamSlot = Arc<std::sync::Mutex<Option<(String, Arc<Shared>)>>>;
/// A staged gapless track: generation, video id, and stream buffer.
type StagedSlot = Arc<std::sync::Mutex<Option<(u64, String, Arc<Shared>)>>>;
type SongCell = Arc<tokio::sync::OnceCell<Option<Track>>>;

/// A music video's stream opened while its song was looked up. Left unused, it stops.
struct EarlyVideo(Option<tokio::task::JoinHandle<anyhow::Result<Arc<Shared>>>>);

impl Drop for EarlyVideo {
    fn drop(&mut self) {
        if let Some(open) = self.0.take()
            && let Ok(rt) = tokio::runtime::Handle::try_current()
        {
            rt.spawn(async move {
                if let Ok(Ok(s)) = open.await {
                    s.cancel();
                }
            });
        }
    }
}

/// How long a play waits for a music video's song version before playing the video.
const PLAY_SONG_WAIT: std::time::Duration = std::time::Duration::from_millis(2500);
/// A lookup still going after this starts the music video's own stream alongside, so
/// it's ready to play when no song turns up.
const EARLY_VIDEO_AFTER: std::time::Duration = std::time::Duration::from_millis(200);
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
    /// A track staged into the audio engine for gapless handoff.
    staged: StagedSlot,
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
    backup_stream: Arc<std::sync::atomic::AtomicBool>,
    /// Explicit songs are hidden: a music video's song version has to be clean.
    clean: Arc<std::sync::atomic::AtomicBool>,
    /// The visionOS player's file URLs were refused past 1 MiB this session (YouTube
    /// wants a PO token for them on this network): songs play from its HLS segments.
    visionos_hls: Arc<std::sync::atomic::AtomicBool>,
    /// Song versions of music videos looked up this session (`None`: there's none), by
    /// video and whether the account has Premium (which gets YouTube's own pairs). One
    /// cell each, so a prefetch and the play of the same video share a lookup.
    songs: Arc<std::sync::Mutex<std::collections::HashMap<String, SongCell>>>,
    /// Albums and playlists being downloaded.
    batch: Arc<std::sync::Mutex<DownloadQueue>>,
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
        let backend = Self { rt: Arc::new(rt), api, tx, audio, wake, prefetched: Default::default(), staged: Default::default(), current: Default::default(), data_dir, lyrics, translate, jsc, premium: Arc::new(tokio::sync::Mutex::new(Err(None))), hq: Default::default(), backup_stream: Default::default(), clean: Default::default(), visionos_hls: Default::default(), songs: Default::default(), batch: Default::default() };
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
        let _ = self.tx.send(Resp::Error(crate::tr!("backend-session-expired").to_owned()));
        self.wake();
    }

    /// Act as another of the account's channels. Not while likes of a selection or a
    /// transfer are running: they'd go on as the new channel.
    async fn switch_channel(&self, page_id: Option<String>, epoch: u64) -> anyhow::Result<u64> {
        let _likes = LIKES.try_lock().map_err(|_| anyhow::anyhow!(crate::tr!("backend-channel-wait-likes")))?;
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
        // A subscribe always answers as `Resp::Subscribed`, which lets the next one go.
        let subscribe = match &req {
            Req::Subscribe { channel_id, on } => Some((channel_id.clone(), *on)),
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
                    match (play_gen, subscribe) {
                        (Some(generation), _) => Some(Resp::PlayError { generation, msg: crate::tr!("backend-unexpected-reply").to_owned(), offer_backup_at: None }),
                        (_, Some((channel_id, on))) => Some(Resp::Subscribed { channel_id, on, ok: false }),
                        _ => (!quiet).then(|| Resp::Error(crate::tr!("backend-error", operation = what, error = crate::tr!("backend-unexpected-reply")))),
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

    /// The name of what's asked to play, and its songs.
    async fn tracks_of(&self, target: &Target, me: bool) -> anyhow::Result<(String, Vec<Track>)> {
        let api = &self.api;
        Ok(match target {
            Target::Album(id) => ytm::album(api, id).await.map(|c| (c.title, c.tracks))?,
            Target::Playlist(id) => ytm::playlist(api, id, me, None).await.map(|c| (c.title, c.tracks))?,
            Target::Artist(id) => ytm::artist(api, id, false).await.map(|a| (a.name, a.top))?,
            Target::Song(t) => (t.title.clone(), vec![t.clone()]),
        })
    }

    async fn follow(&self, link: &Link, to: LinkUse, me: bool) -> anyhow::Result<Option<LinkTo>> {
        let api = &self.api;
        match link {
            Link::Song { id, list } => {
                if let Some(list) = list.as_ref().filter(|_| to == LinkUse::Open) {
                    let target = match crate::link::playlist(list) {
                        Link::Resolve(url) => ytm::resolve(api, &url).await.ok().flatten(),
                        Link::Page(target) => Some(target),
                        _ => None,
                    };
                    // A list that's gone or private, or without the song: the song alone.
                    if let Some(target) = target
                        && let Ok((_, tracks)) = self.tracks_of(&target, me).await
                        && let Some(i) = tracks.iter().position(|t| t.id == *id)
                    {
                        return Ok(Some(LinkTo::InList(target, tracks, i)));
                    }
                }
                Ok(ytm::song(api, id, me).await?.map(LinkTo::Song))
            }
            Link::Resolve(url) => Ok(ytm::resolve(api, url).await?.map(LinkTo::Page)),
            Link::Page(_) | Link::Jam(_) => Ok(None),
        }
    }

    /// A request answered before this returns (or given up on after a few seconds), its
    /// answer dropped: for the app closing, which can't wait for one.
    pub fn request_now(&self, req: Req) {
        let what = req.name();
        // In the runtime: a timeout made outside it panics.
        if self.rt.block_on(async { tokio::time::timeout(std::time::Duration::from_secs(3), self.handle(req)).await }).is_err() {
            log::warn!("{what}: no answer before closing");
        }
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
                Req::SearchIn { query, scope } => Resp::SearchIn(search_in(ytm::search_in(api, &query, scope).await?, query, scope)),
                Req::Suggest { query, round } => {
                    let (terms, hits) = ytm::suggestions(api, &query, me).await?;
                    Resp::Suggest { query, round, terms: terms.into_iter().take(8).collect(), hits }
                }
                Req::ForgetSearch(token) => {
                    ytm::forget_search(api, &token).await?;
                    Resp::SearchForgotten
                }
                Req::Album { id, load } => Resp::Collection { load, c: ytm::album(api, &id).await?, more: false },
                Req::Playlist { id, load, stream: false } => Resp::Collection { load, c: ytm::playlist(api, &id, me, None).await?, more: false },
                Req::Playlist { id, load, stream: true } => self.stream_playlist(id, load, me).await?,
                Req::Artist(id) => Resp::Artist(ytm::artist(api, &id, me).await?),
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
                Req::SongRating { video_id, round } => {
                    let rating = match tokio::time::timeout(std::time::Duration::from_secs(10), ytm::song_rating(api, &video_id)).await {
                        Ok(Ok(rating)) => rating,
                        Ok(Err(e)) => {
                            log::debug!("song rating of {video_id}: {e:#}");
                            None
                        }
                        Err(_) => None,
                    };
                    Resp::SongRating { video_id, round, rating }
                }
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
                Req::ArtistPicture { id, name } => {
                    let found = ytm::search_cards(api, &name, ytm::Filter::Artists, false).await?;
                    Resp::ArtistPicture { pic: artist_pic(&found, &id, &name), id }
                }
                Req::LyricsExtras { video_id, source, lines, lang, target } => {
                    let key = format!("{video_id}_{source}");
                    let Some(extras) = self.translate.extras(&key, &lines, lang.as_deref(), &target).await else { return Ok(None) };
                    Resp::LyricsExtras { video_id, source, target, extras }
                }
                Req::Art { key, url } => {
                    let fetched = async {
                        let Some(url) = url else { return Ok(None) };
                        // The backdrop is a 64 px blur; fetch near that, not the 544 px art.
                        let url = model::sized(&url, 2 * crate::art::SIZE);
                        let bytes = self.http().get(&url).send().await?.error_for_status()?.bytes().await?;
                        anyhow::Ok(Some(tokio::task::spawn_blocking(move || crate::art::process(&bytes)).await??))
                    };
                    let art = match fetched.await {
                        Ok(art) => Ok(art),
                        // Offline, or what came wasn't an image: a downloaded song still has
                        // its cover, in its file.
                        Err(e) => {
                            let id = key.clone();
                            async {
                                let saved = tokio::task::spawn_blocking(move || crate::downloads::cover(&id)).await?.ok_or(e)?;
                                anyhow::Ok(Some(tokio::task::spawn_blocking(move || crate::art::process(&saved)).await??))
                            }
                            .await
                        }
                    };
                    // Failed, it's answered all the same: without an answer the colours
                    // would stay the song before's.
                    let art = art.unwrap_or_else(|e| {
                        log::warn!("{what} failed: {e:#}");
                        None
                    });
                    Resp::Art { key, art }
                }
                Req::Radio { seed, song_of, smart } => {
                    // A music video's radio is music videos; its song's is songs, as from the
                    // song itself. The lookup is shared with its play's (one per video).
                    let song = self.song_version(&seed, song_of.as_ref(), PREFETCH_SONG_WAIT).await;
                    let from = song.as_ref().map_or(&seed, |s| &s.id);
                    // Signed in, the account's own radio, as music.youtube.com plays it: it
                    // follows what the account listens to, so it isn't the same list each time.
                    let mut tracks = if me {
                        ytm::radio(api, from, true, 50).await.unwrap_or_else(|e| {
                            log::info!("radio for {from}: signed in failed ({e:#}); asking signed out");
                            Vec::new()
                        })
                    } else {
                        Vec::new()
                    };
                    // The radio starts with its seed, which is playing already.
                    tracks.retain(|t| t.id != *from);
                    if tracks.is_empty() {
                        tracks = ytm::radio(api, from, false, 50).await?;
                        tracks.retain(|t| t.id != *from);
                    }
                    log::info!("radio for {from}: {} songs", tracks.len());
                    Resp::Radio { seed, tracks, smart }
                }
                Req::Link { link, to } => {
                    let found = match self.follow(&link, to, me).await {
                        Ok(Some(to)) => Ok(to),
                        Ok(None) => Err(crate::tr!("link-not-found").to_owned()),
                        Err(e) => {
                            log::warn!("{what} failed: {e:#}");
                            Err(if !unreachable(&e) {
                                crate::tr!("backend-error", operation = what, error = e.to_string())
                            } else if crate::proxy::in_use() {
                                crate::tr!("backend-offline-proxy").to_owned()
                            } else {
                                crate::tr!("backend-offline").to_owned()
                            })
                        }
                    };
                    Resp::Link { link, to, found }
                }
                Req::PlayCollection { target } => {
                    let (title, tracks) = self.tracks_of(&target, me).await?;
                    Resp::PlayQueue { target, title, tracks }
                }
                Req::Download { track: t, for_video } => {
                    let (path, _) = self.download(&t, for_video, false, None).await?;
                    // Its lyrics too, for offline; the download is done without waiting for them.
                    let (this, song) = (self.clone(), t.clone());
                    tokio::spawn(async move {
                        let found = this.lyrics_for(&song).await.sources.len();
                        log::info!("download {}: lyrics from {found} source(s) kept", song.id);
                    });
                    Resp::Downloaded { title: t.title, path }
                }
                Req::DownloadMany { target, title, tracks, confirmed } => match self.queue_downloads(target, title, tracks, confirmed, me).await? {
                    Some(r) => r,
                    None => return Ok(None),
                },
                Req::Play { video_id, generation, at, report, song_of, song } => match self.start_playback(&video_id, song_of.as_ref(), song, generation, at, report).await {
                    Ok(()) => return Ok(None),
                    Err(e) => {
                        log::warn!("play {video_id}: {e:#}");
                        let msg = if unreachable(&e) { crate::tr!("backend-unreachable").to_owned() } else { format!("{e:#}") };
                        Resp::PlayError { generation, msg, offer_backup_at: None }
                    }
                },
                Req::VideoStream { video_id, max_height } => {
                    let found = async { innertube::video_stream(&self.http(), &video_id, &api.visitor_data(false).await?, max_height.into()).await }.await;
                    if let Err(e) = &found {
                        log::warn!("video of {video_id}: {e:#}");
                    }
                    Resp::VideoStream { video_id, max_height, stream: found.ok() }
                }
                Req::MusicVideo(song) => Resp::MusicVideo { video: self.music_video(&song).await?, song: song.id },
                Req::Canvas(track) => Resp::Canvas { clip: crate::canvas::find(&self.http(), &self.data_dir, &track).await, video_id: track.id },
                Req::Prefetch { video_id, song_of } => {
                    self.prefetch(video_id, song_of).await?;
                    return Ok(None);
                }
                Req::StageNext { video_id, after, generation, song_of, fallback_duration_ms } => {
                    self.stage_next(&video_id, after, generation, song_of.as_ref(), fallback_duration_ms).await?;
                    return Ok(None);
                }
                Req::GaplessStarted { video_id, generation } => {
                    self.gapless_started(video_id, generation);
                    return Ok(None);
                }
                Req::Like { video_id, like } => {
                    let epoch = api.epoch();
                    match ytm::set_like(api, &video_id, like).await {
                        Ok(_) => {
                            let other = if like { None } else { ytm::remove_other_half_like(api, &video_id, epoch, &HashSet::new()).await? };
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
                Req::LikeMany { video_ids, like, versions } => self.like_many(video_ids, like, versions).await?,
                Req::Dislike { video_id, skip_generation } => {
                    let result = ytm::dislike(api, &video_id).await;
                    if let Err(e) = &result {
                        log::warn!("disliking {video_id}: {e:#}");
                    }
                    Resp::Disliked { video_id, skip_generation, ok: result.is_ok() }
                }
                Req::RemoveRating { video_id, was_disliked } => {
                    let result = ytm::set_like(api, &video_id, false).await;
                    if let Err(e) = &result {
                        log::warn!("removing rating of {video_id}: {e:#}");
                    }
                    Resp::RatingRemoved { video_id, was_disliked, ok: result.is_ok() }
                }
                Req::CreatePlaylist { title, video_ids } => {
                    let id = match video_ids.as_slice() {
                        [] => ytm::create_playlist(api, &title, None).await?,
                        [one] => ytm::create_playlist(api, &title, Some(one)).await?,
                        many => ytm::create_playlist_with(api, &title, many).await?,
                    };
                    Resp::PlaylistCreated { id, songs: video_ids.len() }
                }
                Req::EditPlaylist { id, title, description, privacy, cover } => {
                    ytm::edit_playlist(api, &id, &title, &description, privacy.as_deref()).await?;
                    // The name and description are saved by now: a picture that fails says
                    // so after the playlist reloads with them, so "updated" doesn't hide it.
                    if let Some(cover) = cover {
                        let changed = async {
                            match cover {
                                CoverChange::Set(jpeg) => {
                                    let session = api.account().ok_or_else(|| anyhow::anyhow!("sign in to change the picture"))?;
                                    let blob = session.upload_playlist_cover(&self.http(), &id, jpeg).await?;
                                    ytm::set_playlist_cover(api, &id, &blob).await
                                }
                                CoverChange::Remove => ytm::remove_playlist_cover(api, &id).await,
                            }
                        };
                        if let Err(e) = changed.await {
                            log::warn!("playlist picture failed: {e:#}");
                            let _ = self.tx.send(Resp::PlaylistChanged { id, change: PlaylistChange::Updated });
                            return Ok(Some(Resp::Error(crate::tr!("backend-error", operation = crate::tr!("dialog-change-picture"), error = e.to_string()))));
                        }
                    }
                    Resp::PlaylistChanged { id, change: PlaylistChange::Updated }
                }
                // A failure is answered too, so the Sort button comes back.
                Req::SortPlaylist { id, actions } => match ytm::sort_playlist(api, &id, actions).await {
                    Ok(()) => Resp::PlaylistChanged { id, change: PlaylistChange::Sorted },
                    Err(e) => {
                        log::warn!("sort playlist failed: {e:#}");
                        Resp::PlaylistChanged { id, change: PlaylistChange::SortFailed }
                    }
                },
                Req::AddToPlaylist { playlist_id, tracks, duplicates } => {
                    let video_ids: Vec<String> = tracks.iter().map(|t| t.id.clone()).collect();
                    // A failure says which songs too, so they stop showing in the playlist.
                    match ytm::add_videos(api, &playlist_id, &video_ids, duplicates).await {
                        Ok(true) => Resp::AddedToPlaylist { id: playlist_id, video_ids, error: None },
                        Ok(false) => Resp::AlreadyInPlaylist { id: playlist_id, tracks },
                        Err(e) => {
                            log::warn!("{what} failed: {e:#}");
                            Resp::AddedToPlaylist { id: playlist_id, video_ids, error: Some(crate::tr!("backend-error", operation = what, error = e.to_string())) }
                        }
                    }
                }
                Req::RemoveFromPlaylist { playlist_id, items } => {
                    match ytm::remove_videos(api, &playlist_id, &items).await {
                        Ok(_) => Resp::PlaylistChanged { id: playlist_id, change: PlaylistChange::Removed },
                        Err(e) => {
                            log::warn!("removing from playlist {playlist_id}: {e:#}");
                            Resp::RemoveFailed { id: playlist_id, error: crate::tr!("backend-remove-failed", error = e.to_string()) }
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
                Req::DeletePlaylist(id) => match ytm::delete_playlist(api, &id).await {
                    Ok(_) => Resp::PlaylistChanged { id, change: PlaylistChange::Deleted },
                    Err(e) => {
                        log::warn!("deleting playlist {id}: {e:#}");
                        Resp::DeleteFailed { error: crate::tr!("backend-delete-playlist-failed", error = e.to_string()) }
                    }
                },
                Req::DeleteUpload { entity, title } => match ytm::delete_upload(api, &entity).await {
                    Ok(()) => Resp::UploadDeleted(ytm::uploads(api).await?),
                    Err(e) => {
                        log::warn!("deleting upload {entity}: {e:#}");
                        Resp::DeleteFailed { error: crate::tr!("backend-delete-upload-failed", title = title, error = e.to_string()) }
                    }
                },
                Req::Collaboration(id) => Resp::Collaboration(ytm::collaboration(api, &id).await?),
                Req::SetCollaboration { info, change } => {
                    ytm::set_collaboration(api, &info, change).await?;
                    Resp::Collaboration(ytm::collaboration(api, &info.playlist_id).await?)
                }
                Req::SaveCollection { id, save } => {
                    ytm::save_collection(api, &id, save).await?;
                    Resp::PlaylistChanged { id, change: if save { PlaylistChange::SavedToLibrary } else { PlaylistChange::RemovedFromLibrary } }
                }
                Req::Subscribe { channel_id, on } => {
                    let result = ytm::set_subscribed(api, &channel_id, on).await;
                    if let Err(e) = &result {
                        log::warn!("subscribe={on} {channel_id}: {e:#}");
                    }
                    Resp::Subscribed { channel_id, on, ok: result.is_ok() }
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
                Req::Reachable => Resp::Reachable(ytmapi::reachable(&self.http()).await),
            }))
        }
        .await;
        res.unwrap_or_else(|e| {
            log::warn!("{what} failed: {e:#}");
            match unreachable(&e) {
                true => (!quiet).then_some(Resp::Offline),
                false => (!quiet).then(|| Resp::Error(crate::tr!("backend-error", operation = what, error = e.to_string()))),
            }
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
        let (t, v, al, ar, pl, cm, (podcasts, episodes)) = tokio::join!(
            ytm::search_songs(api, &query, me),
            ytm::search_videos(api, &query, me),
            ytm::search_cards(api, &query, ytm::Filter::Albums, me),
            ytm::search_cards(api, &query, ytm::Filter::Artists, me),
            ytm::search_cards(api, &query, ytm::Filter::Playlists, me),
            ytm::search_cards(api, &query, ytm::Filter::Community, me),
            podcasts,
        );
        Ok(Resp::Search(SearchResults {
            tracks: t?,
            videos: v.unwrap_or_default(),
            albums: al.unwrap_or_default(),
            artists: ar.unwrap_or_default(),
            playlists: pl.unwrap_or_default(),
            // Only playlists, should the reply carry another kind of shelf too.
            community: cm.unwrap_or_default().into_iter().filter(|c| matches!(c.target, Target::Playlist(_))).collect(),
            podcasts,
            episodes,
            query,
            scope: ytm::Scope::Everywhere,
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
        let artists = async {
            ytm::subscribed_artists(api).await.inspect_err(|e| log::warn!("subscribed artists didn't load: {e:#}"))
        };
        let (pls, albums, podcasts, artists) = tokio::join!(ytm::saved_playlists(api), ytm::saved_albums(api), podcasts, artists);
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
            artists_failed: artists.is_err(),
            playlists: pls.map(|p| p.into_iter().filter(saved_playlist_visible).collect()).unwrap_or_default(),
            albums: albums.unwrap_or_default(),
            podcasts,
            artists: artists.unwrap_or_default(),
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
        // For the play it was asked during: a skip in the meantime makes it moot, and so
        // does a later prefetch (the queue changed: another song is next now).
        static LATEST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let ticket = LATEST.fetch_add(1, Ordering::Relaxed) + 1;
        let generation = self.audio.status.wanted_gen.load(Ordering::Relaxed);
        // Looked up now, so the play doesn't wait for it.
        let video_id = match self.song_version(&video_id, song_of.as_ref(), PREFETCH_SONG_WAIT).await {
            Some(song) => song.id,
            None => video_id,
        };
        let moot = || {
            self.audio.status.wanted_gen.load(Ordering::Relaxed) != generation
                || LATEST.load(Ordering::Relaxed) != ticket
                || self.prefetched.lock().unwrap().as_ref().is_some_and(|(id, _)| *id == video_id)
        };
        if moot() {
            return Ok(());
        }
        // The URL is looked up meanwhile, but the playing song finishes downloading
        // first: it gets the whole bandwidth.
        let backup_stream = self.backup_stream.load(Ordering::Relaxed);
        let playing = self.current.lock().unwrap().as_ref().map(|(_, s)| s.clone());
        let (found, ()) = tokio::join!(self.find_stream(&video_id), async {
            if let Some(s) = playing {
                s.finished().await;
            }
        });
        if moot() {
            return Ok(());
        }
        let mut prefetched = self.prefetched.lock().unwrap();
        if self.backup_stream.load(Ordering::Relaxed) != backup_stream {
            return Ok(());
        }
        log::debug!("prefetch {video_id}");
        let shared = self.start_stream(&video_id, found?, true);
        if let Some((_, old)) = prefetched.replace((video_id, shared)) {
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
        let video_id = t.id.clone();
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
            // Unison lyrics saved before they carried its credit don't wait for the fresh
            // look, which can take long on a poor connection: they credit Unison alone
            // until it names who submitted the same lyrics.
            Some(mut saved) => {
                let uncredited: Vec<_> = saved.sources.iter().filter(|l| l.provider.is_unison() && l.unison.is_none()).cloned().collect();
                if !uncredited.is_empty() {
                    saved.credit_unison(&video_id);
                    let this = self.clone();
                    tokio::spawn(async move {
                        let Ok(fresh) = fresh.await else { return };
                        let credits: Vec<_> = uncredited
                            .iter()
                            .filter_map(|old| {
                                let new = fresh.get(old.provider).filter(|new| new.lines == old.lines)?;
                                new.unison.clone().filter(|c| c.submitter.is_some()).map(|c| (old.provider, c))
                            })
                            .collect();
                        if !credits.is_empty() {
                            let _ = this.tx.send(Resp::UnisonCredit { video_id, credits });
                            this.wake();
                        }
                    });
                }
                saved
            }
            None => fresh.await.unwrap_or_default(),
        }
    }

    async fn fetch_lyrics(&self, t: &Track) -> LyricsSet {
        let artist = t.artist_line();
        let (bare_song, main_artist) = ytm::lyrics_names(t);
        let query = lyrics::Query {
            video_id: &t.id,
            song: &t.title,
            artist: &artist,
            album: t.album.as_ref().map(|a| a.name.as_str()),
            duration: match t.duration.or_else(|| crate::downloads::duration(&t.id)) {
                Some(d) => Some(d),
                None => self.stream_duration(&t.id).await,
            },
            bare_song: &bare_song,
            main_artist: &main_artist,
        };
        let mut set = self.lyrics.fetch(&query, ytm::lyrics(&self.api, &t.id)).await.unwrap_or_else(|e| {
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
        let mut early = EarlyVideo(None);
        let mut song = match carried {
            Some(song) => Some(song),
            None => {
                let lookup = self.song_version(listed, song_of, PLAY_SONG_WAIT);
                tokio::pin!(lookup);
                match tokio::time::timeout(EARLY_VIDEO_AFTER, &mut lookup).await {
                    Ok(song) => song,
                    Err(_) => {
                        // Just its start, until it turns out to be the one that plays.
                        let (this, id) = (self.clone(), listed.to_owned());
                        early.0 = Some(tokio::spawn(async move { this.open_stream(&id, true).await }));
                        lookup.await
                    }
                }
            }
        };
        if self.audio.status.wanted_gen.load(Ordering::Relaxed) != generation {
            return Ok(());
        }
        let mut video_id = song.as_ref().map_or(listed, |s| s.id.as_str()).to_owned();
        // Skipped to while lined up for gapless (its prefetch went into that): play from it.
        let staged = self.staged.lock().unwrap().take_if(|(_, id, s)| *id == video_id && self.stream_usable(s)).map(|(_, _, s)| s);
        self.clear_staged();
        let prefetched = self.prefetched.lock().unwrap().take();
        let replay = self.current.lock().unwrap().take().filter(|(id, s)| *id == video_id && s.is_complete()).map(|(_, s)| s);
        let ready = match prefetched {
            // Not one whose download failed (refused, say): that opens afresh.
            // Nor a file stream fetched before file streams turned out to be cut off.
            Some((id, s)) if id == video_id && self.stream_usable(&s) => Some(s),
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
        let ready = match (ready, staged) {
            (Some(r), Some(st)) => {
                if !Arc::ptr_eq(&r, &st) {
                    self.cancel_if_not_current(&st);
                }
                Some(r)
            }
            (r, st) => r.or(st),
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
                // Taken only for the video; otherwise dropping `early` stops it.
                let opened = match early.0.take_if(|_| song.is_none()) {
                    Some(open) => {
                        log::debug!("play {video_id} (gen {generation}): stream opened during the song lookup");
                        open.await.map_err(anyhow::Error::from).flatten()
                    }
                    None => self.open_stream(&video_id, false).await,
                };
                match opened {
                    Ok(s) => s,
                    // The song won't play: the music video still does.
                    Err(e) if song.is_some() => {
                        log::warn!("{listed}'s song {video_id} won't play ({e:#}); playing the music video");
                        (song, video_id) = (None, listed.to_owned());
                        // The UI took the song as playing already.
                        if was_carried {
                            let _ = self.tx.send(Resp::PlayingAs { generation, listed: listed.to_owned(), song: None });
                        }
                        let opened = match early.0.take() {
                            Some(open) => open.await.ok().and_then(Result::ok),
                            None => None,
                        };
                        match opened {
                            Some(s) => s,
                            None => self.open_stream(listed, false).await?,
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
        };
        // If the song's audio won't start after all, the music video plays instead.
        let fallback = song.is_some().then(|| (self.clone(), listed.to_owned(), report));
        if let Some(song) = song {
            log::info!("play {listed} as its {} {video_id}", if song.has_video() { "music video" } else { "song" });
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
                    this.spawn_report_play(id, generation);
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
                    let (msg, offer_backup_at) = match shared2.failure() {
                        Some(failure) => (failure.reason, failure.offer_backup.then_some(at)),
                        None => (format!("decode: {e}"), None),
                    };
                    let _ = tx.send(Resp::PlayError { generation, msg, offer_backup_at });
                    wake();
                }
            },
        });
        Ok(())
    }

    fn stream_usable(&self, s: &Arc<Shared>) -> bool {
        s.error().is_none() && !(self.visionos_hls.load(Ordering::Relaxed) && s.segments.is_none() && !s.is_complete())
    }

    fn cancel_if_not_current(&self, s: &Arc<Shared>) {
        if self.current.lock().unwrap().as_ref().is_none_or(|(_, c)| !Arc::ptr_eq(c, s)) {
            s.cancel();
        }
    }

    /// Open the next song's decoder ahead of the current track's end and queue it for
    /// gapless handoff. Errors fail quietly so normal end-of-track advance still runs.
    async fn stage_next(&self, listed: &str, after: u64, generation: u64, song_of: Option<&Track>, fallback_duration_ms: u64) -> anyhow::Result<()> {
        let still_staged = || {
            self.audio.status.wanted_gen.load(Ordering::Relaxed) == after
                && self.audio.status.staged_gen.load(Ordering::Acquire) == generation
        };
        if !still_staged() {
            return Ok(());
        }
        let mut song = self.song_version(listed, song_of, PREFETCH_SONG_WAIT).await;
        if !still_staged() {
            return Ok(());
        }
        let mut video_id = song.as_ref().map_or(listed, |s| s.id.as_str()).to_owned();
        let buffered = {
            let mut slot = self.prefetched.lock().unwrap();
            slot.as_ref().is_some_and(|(id, s)| *id == video_id && self.stream_usable(s)).then(|| slot.take().unwrap().1)
        }
        .or_else(|| self.current.lock().unwrap().as_ref().filter(|(id, s)| *id == video_id && s.is_complete()).map(|(_, s)| s.clone()))
        // Staged before a pause or seek near the end let it go: the same stream again.
        .or_else(|| self.staged.lock().unwrap().as_ref().filter(|(_, id, s)| *id == video_id && self.stream_usable(s)).map(|(_, _, s)| s.clone()));
        let shared = match buffered {
            Some(s) => s,
            None => match self.open_stream(&video_id, false).await {
                Ok(s) => s,
                Err(e) if song.is_some() => {
                    log::warn!("stage: {listed}'s song {video_id} failed ({e:#}); falling back to music video");
                    (song, video_id) = (None, listed.to_owned());
                    self.open_stream(listed, false).await?
                }
                Err(e) => return Err(e),
            },
        };
        if !still_staged() {
            self.cancel_if_not_current(&shared);
            return Ok(());
        }
        if let Some(song) = song {
            let _ = self.tx.send(Resp::StagedSong { generation, listed: listed.to_owned(), song });
            (self.wake)();
        }
        if let Some((_, _, old)) = self.staged.lock().unwrap().replace((generation, video_id, shared.clone()))
            && !Arc::ptr_eq(&old, &shared)
        {
            self.cancel_if_not_current(&old);
        }
        let (this, shared2) = (self.clone(), shared.clone());
        tokio::task::spawn_blocking(move || {
            let valid = || {
                this.audio.status.wanted_gen.load(Ordering::Relaxed) == after
                    && this.audio.status.staged_gen.load(Ordering::Acquire) == generation
            };
            let drop_if_stale = || {
                let mut slot = this.staged.lock().unwrap();
                if slot.as_ref().is_some_and(|(g, _, _)| *g == generation) {
                    slot.take();
                }
                drop(slot);
                this.cancel_if_not_current(&shared2);
            };
            match stream::decoder(shared, false) {
                Ok(decoder) => {
                    if !valid() {
                        drop_if_stale();
                        return;
                    }
                    // Pre-decode the first Tempo block and warm head blocks off the audio thread
                    // so the real-time callback never stalls on the network at handoff.
                    let song_ms = Arc::new(std::sync::atomic::AtomicU64::new(0));
                    let tempo = Box::new(crate::audio::tempo::Tempo::new(decoder, this.audio.status.speed_arc(), std::time::Duration::ZERO, song_ms.clone()));
                    shared2.warm_head(std::time::Duration::from_secs(1));
                    if valid() {
                        this.audio.send(AudioCmd::QueueNext { after, generation, tempo, song_ms, shared: shared2, fallback_duration_ms });
                    } else {
                        drop_if_stale();
                    }
                }
                Err(e) => {
                    log::debug!("stage gen {generation}: decoder failed ({e:#}); will fall back at end");
                    drop_if_stale();
                }
            }
        });
        Ok(())
    }

    fn gapless_started(&self, video_id: String, generation: u64) {
        let promoted = {
            let mut slot = self.staged.lock().unwrap();
            slot.as_ref().is_some_and(|(g, _, _)| *g == generation).then(|| {
                let (_, id, s) = slot.take().unwrap();
                (id, s)
            })
        };
        if let Some(cur) = promoted {
            *self.current.lock().unwrap() = Some(cur);
        }
        if self.is_logged_in() && reports_plays() {
            self.spawn_report_play(video_id, generation);
        }
    }

    fn clear_staged(&self) {
        if let Some((_, _, old)) = self.staged.lock().unwrap().take() {
            self.cancel_if_not_current(&old);
        }
    }

    fn spawn_report_play(&self, id: String, generation: u64) {
        let this = self.clone();
        self.rt.spawn(async move {
            // The player request needs the current signatureTimestamp, or YouTube
            // leaves out the stats URL. Without a solver, try anyway as before.
            let sts = match this.jsc.player(&this.http()).await {
                Ok(p) => Some(p.sts),
                Err(e) => {
                    log::debug!("report play of {id} without signatureTimestamp: {e:#}");
                    None
                }
            };
            match ytm::report_play(&this.api, &id, sts).await {
                Ok(w) => {
                    log::debug!("reported play of {id}");
                    this.report_watchtime(&id, generation, w).await;
                }
                Err(e) => log::warn!("couldn't report play of {id}: {e:#}"),
            }
        });
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

    /// A song's music video: YouTube's own pair (Premium), or a sure signed-out search
    /// match, which doesn't know how the two line up.
    async fn music_video(&self, song: &Track) -> anyhow::Result<Option<ytm::MusicVideo>> {
        let premium = match self.api.account() {
            Some(session) => self.is_premium(&session).await,
            None => false,
        };
        if premium {
            match ytm::paired_video(&self.api, &song.id).await {
                // YouTube pairs every song that has a music video; trust it when it says none.
                Ok(found) => return Ok(found),
                Err(e) => log::debug!("video pair of {}: {e:#}; searching instead", song.id),
            }
        }
        let found = ytm::searched_video(&self.api, song).await?;
        match &found {
            Some(v) => log::info!("music video of {} \"{}\": {} \"{}\"", song.id, song.title, v.id, v.title),
            None => log::info!("music video of {} \"{}\": none found", song.id, song.title),
        }
        Ok(found.map(|video| ytm::MusicVideo { video, offset_ms: 0 }))
    }

    async fn open_stream(&self, video_id: &str, prefetch: bool) -> anyhow::Result<Arc<Shared>> {
        let found = self.find_stream(video_id).await?;
        Ok(self.start_stream(video_id, found, prefetch))
    }

    /// The saved download, or a stream URL to download from.
    async fn find_stream(&self, video_id: &str) -> anyhow::Result<Found> {
        let id = video_id.to_owned();
        if let Some(shared) = tokio::task::spawn_blocking(move || crate::downloads::open(&id)).await? {
            return Ok(Found::Saved(shared));
        }
        let (source, s) = self.resolve(video_id, false).await?;
        Ok(Found::Url(source, s))
    }

    fn start_stream(&self, video_id: &str, found: Found, prefetch: bool) -> Arc<Shared> {
        let (source, s) = match found {
            Found::Saved(shared) => return shared,
            Found::Url(source, s) => (source, s),
        };
        match &s.starts {
            Some(starts) => log::info!("stream {video_id}: {} HLS segments from {source:?}", starts.len()),
            None => log::info!("stream {video_id}: {} bytes {} from {source:?}", s.size, s.kind()),
        }
        let shared = s.buffer(prefetch);
        if source == Source::YtDlpAfterBotCheck {
            shared.mark_after_bot_check();
        }
        let refresh = Some(self.refresher(source, video_id, &s));
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
        shared
    }

    /// Resolves a fresh URL from the same source and format, for a download that got a 403.
    fn refresher(&self, source: Source, video_id: &str, stream: &ResolvedStream) -> stream::Refresh {
        let (this, id) = (self.clone(), video_id.to_owned());
        let (opus, size) = (stream.opus, stream.size);
        let starts = stream.starts.as_deref().map(Arc::<[f64]>::from);
        Arc::new(move || {
            let (this, id, starts) = (this.clone(), id.clone(), starts.clone());
            Box::pin(async move {
                let s = this.resolve_from(source, &id, opus).await?;
                // The buffer is laid out for the format it started with.
                anyhow::ensure!(s.opus == opus, "the stream changed format");
                // A podcast host may put other ads in, which makes another file.
                anyhow::ensure!(s.size == size, "the stream changed size");
                anyhow::ensure!(same_segments(source, starts.as_deref(), s.starts.as_deref()), "the stream changed segment boundaries");
                Ok(s.media)
            })
        })
    }

    /// Download a song into the downloads folder, at the best quality the account gets.
    /// `behind_player`: each part waits while the playing song still needs some of its own.
    /// Returns the file, and whether this saved it (false: it was there already).
    /// `folder`: the album's or playlist's own it goes in.
    async fn download(&self, t: &Track, for_video: Option<String>, behind_player: bool, folder: Option<crate::downloads::ListFolder>) -> anyhow::Result<(PathBuf, bool)> {
        // One save of a song at a time: one on its way already, this waits and takes its file.
        let _saving = loop {
            if self.batch.lock().unwrap().saving.insert(t.id.clone()) {
                break Saving(self.batch.clone(), t.id.clone());
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        };
        if let Some(path) = reuse_download(&t.id, for_video.clone()).await {
            return Ok((path, false));
        }
        let (source, s) = self.resolve(&t.id, true).await?;
        log::info!("download {}: {} bytes from {source:?}", t.id, s.size);
        anyhow::ensure!(s.starts.is_none(), "downloads need the file stream");
        // Saved as a tagged .m4a, which a feed's MP3 isn't.
        anyhow::ensure!(!matches!(s.media, stream::Media::Feed(_)), "episodes of podcasts added by RSS can't be downloaded");
        let shared = s.buffer(false);
        let ahead: Option<stream::Ahead> = behind_player.then(|| {
            let current = self.current.clone();
            Arc::new(move || current.lock().unwrap().as_ref().map(|(_, s)| s.clone())) as stream::Ahead
        });
        let refresh = Some(self.refresher(source, &t.id, &s));
        stream::download_behind(self.http(), s.media, s.user_agent, shared.clone(), refresh, ahead).await;
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
        let path = tokio::task::spawn_blocking(move || crate::downloads::save(&t, &audio, duration_ms, loudness, cover, for_video, folder.as_ref())).await??;
        Ok((path, true))
    }

    /// Queue the songs of an album, playlist or selection that aren't downloaded yet (see
    /// `Req::DownloadMany`) and start downloading them. Every run as it stands goes to the
    /// UI from here, in order with the workers' updates; a large one is asked about first.
    async fn queue_downloads(&self, target: Option<Target>, title: String, tracks: Option<Vec<Track>>, confirmed: bool, me: bool) -> anyhow::Result<Option<Resp>> {
        let (title, tracks) = match (tracks, &target) {
            (Some(tracks), _) => (title, tracks),
            (None, Some(Target::Album(id))) => ytm::album(&self.api, id).await.map(|c| (c.title, c.tracks))?,
            (None, Some(Target::Playlist(id))) => ytm::playlist(&self.api, id, me, None).await.map(|c| (c.title, c.tracks))?,
            (None, _) => anyhow::bail!("nothing to download"),
        };
        // An empty playlist (from its card, which can't tell): nothing to run.
        if tracks.is_empty() {
            let _ = self.tx.send(Resp::Error(crate::tr!("backend-empty-download", title = title.as_str())));
            return Ok(None);
        }
        let key = match &target {
            Some(Target::Album(id) | Target::Playlist(id)) => Some(id.clone()),
            _ => None,
        };
        // Each song once. Looking for their files is disk work.
        let (listed, todo) = tokio::task::spawn_blocking(move || {
            let mut seen = std::collections::HashSet::new();
            let listed: Vec<Track> = tracks.into_iter().filter(|t| seen.insert(t.id.clone())).collect();
            let todo: Vec<Track> = listed.iter().filter(|t| !is_downloaded(t)).cloned().collect();
            (listed, todo)
        })
        .await?;
        if !confirmed {
            // Those on their way for this run already aren't asked about again.
            let new = {
                let q = self.batch.lock().unwrap();
                todo.iter().filter(|t| key.as_ref().is_none_or(|k| !q.wanted.get(&t.id).is_some_and(|ks| ks.contains(k)))).count()
            };
            if new > LARGE_DOWNLOAD {
                return Ok(Some(Resp::ConfirmDownload { target, title, count: new, tracks: listed }));
            }
        }
        let skipped = listed.len() - todo.len();
        // An album or playlist of its own, not a selection, with the setting on.
        let folder = key.as_ref().filter(|_| crate::downloads::per_list()).map(|id| crate::downloads::ListFolder {
            id: id.clone(),
            name: crate::downloads::list_name(&title, matches!(target, Some(Target::Album(_))), &listed),
        });
        let mut q = self.batch.lock().unwrap();
        let (key, queued) = q.add(key, title, listed.len(), todo, folder);
        log::info!("download {key}: {queued} songs queued, {skipped} downloaded already");
        for _ in 0..q.more_workers() {
            let this = self.clone();
            tokio::spawn(async move { this.download_worker().await });
        }
        let _ = self.tx.send(q.snapshot());
        drop(q);
        self.wake();
        Ok(None)
    }

    /// Download queued songs, one after another, until none are left.
    async fn download_worker(&self) {
        loop {
            let next = {
                let mut q = self.batch.lock().unwrap();
                q.next().map(|t| {
                    let folder = q.folder_for(&t.id);
                    (t, folder)
                })
            };
            let Some((t, folder)) = next else { return };
            // On a task of its own, so a panic in it fails this song, not the whole queue.
            let (this, song) = (self.clone(), t.clone());
            let result = tokio::spawn(async move { this.download_listed(&song, folder).await }).await.unwrap_or_else(|e| Err(anyhow::anyhow!("{e}")));
            if let Err(e) = &result {
                log::warn!("download {}: {e:#}", t.id);
            }
            {
                let mut q = self.batch.lock().unwrap();
                q.finish(&t, &result);
                let _ = self.tx.send(q.snapshot());
            }
            self.wake();
        }
    }

    /// One song of a run, behind the playing song, unless it's downloaded by now. A music
    /// video saves its song version (with none found it isn't downloaded). True once saved.
    /// `folder`: its album's or playlist's own, to save it in.
    async fn download_listed(&self, t: &Track, folder: Option<crate::downloads::ListFolder>) -> anyhow::Result<bool> {
        let (song, for_video) = match t.music_video {
            true => (self.song_version(&t.id, Some(t), PREFETCH_SONG_WAIT).await.ok_or_else(|| anyhow::anyhow!("no song version of this music video"))?, Some(t.id.clone())),
            false => (t.clone(), None),
        };
        // Its song is downloaded already: the video plays that file too.
        if reuse_download(&song.id, for_video.clone()).await.is_some() {
            return Ok(false);
        }
        if !self.download(&song, for_video, true, folder).await?.1 {
            // Saved meanwhile by another download, lyrics and all.
            return Ok(false);
        }
        // Its lyrics too, for offline: before the next song, so a long run doesn't pile them up.
        let found = self.lyrics_for(&song).await.sources.len();
        log::info!("download {}: lyrics from {found} source(s) kept", song.id);
        Ok(true)
    }

    /// Stop downloading the run `key`: its waiting songs are dropped (unless another run
    /// wants them too), and the ones downloading now finish.
    pub fn cancel_downloads(&self, key: &str) {
        {
            let mut q = self.batch.lock().unwrap();
            if !q.cancel(key) {
                return;
            }
            let _ = self.tx.send(q.snapshot());
        }
        log::info!("download {key}: cancelled");
        self.wake();
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
        // The signed-in web player refused already: it isn't asked again below.
        let mut web_refused = false;
        if hq
            && solver_ready
            && let Some(session) = self.api.account()
            && self.is_premium(&session).await
        {
            match self.resolve_from(Source::WebMusic, video_id, !best).await {
                Ok(s) => return Ok((Source::WebMusic, s)),
                // Not fatal: the usual 128 kbps stream still plays.
                Err(e) => {
                    web_refused = e.downcast_ref::<innertube::Refused>().is_some();
                    log::warn!("high quality stream failed for {video_id} ({e:#}); using standard quality");
                }
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
                // Should visionOS ask to sign in for an episode of a podcast added by RSS
                // (it says those aren't available, below), the web player has its file.
                if let Some(s) = self.rss_episode(&session, video_id).await? {
                    return Ok((Source::Feed, s));
                }
                let blocked = bot_check(&e);
                if !best && self.backup_stream.load(Ordering::Relaxed) && blocked {
                    match self.resolve_from(Source::SafariHls, video_id, false).await {
                        Ok(s) => return Ok((Source::SafariHls, s)),
                        Err(e) => log::warn!("Safari HLS failed for {video_id} ({e:#}); trying the file stream"),
                    }
                }
                let source = if blocked { Source::YtDlpAfterBotCheck } else { Source::YtDlp };
                Ok((source, self.resolve_from(source, video_id, false).await?))
            }
            // visionOS answers the same for a video that's gone, one blocked where the
            // user is (#373), and an episode of a podcast added by RSS. The signed-in web
            // player gets one try: it may play the first two, and has the episode's feed.
            Err(e) if not_available(&e) => {
                let Some(session) = self.api.account().filter(|_| !web_refused) else { return Err(e) };
                match self.signed_in_retry(&session, video_id, best).await {
                    Ok(found) => {
                        log::info!("{video_id}: visionOS said {e}; {:?} plays it", found.0);
                        Ok(found)
                    }
                    Err(web) if web.downcast_ref::<innertube::Refused>().is_some() => {
                        log::info!("{video_id}: the signed-in web player says {web} too");
                        Err(e)
                    }
                    Err(web) => {
                        log::warn!("{video_id}: signed-in retry failed: {web:#}");
                        Err(e)
                    }
                }
            }
            Err(e) => Err(e),
        }
    }

    /// For a song visionOS says isn't available, ask the signed-in web player: an RSS
    /// episode plays from its feed, and a song it plays comes from it with Premium, else
    /// from yt-dlp. A video that's gone costs that one request, as it did before.
    async fn signed_in_retry(&self, session: &webmusic::Session, video_id: &str, best: bool) -> anyhow::Result<(Source, ResolvedStream)> {
        if let Some(feed) = session.feed(&self.http(), &self.jsc, video_id).await? {
            return Ok((Source::Feed, self.feed_stream(feed).await?));
        }
        let source = if self.is_premium(session).await { Source::WebMusic } else { Source::YtDlp };
        Ok((source, self.resolve_from(source, video_id, !best && source == Source::WebMusic).await?))
    }

    /// The feed's file when `video_id` is an episode of a podcast added by RSS, `None`
    /// for anything else. The web player's refusal is only logged: the caller has its own
    /// error to give.
    async fn rss_episode(&self, session: &webmusic::Session, video_id: &str) -> anyhow::Result<Option<ResolvedStream>> {
        match session.feed(&self.http(), &self.jsc, video_id).await {
            Ok(Some(feed)) => self.feed_stream(feed).await.map(Some),
            Ok(None) => Ok(None),
            Err(e) => {
                log::info!("{video_id}: web player: {e:#}");
                Ok(None)
            }
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
                match session.audio_stream(&self.http(), &self.jsc, video_id, opus).await? {
                    webmusic::Audio::YouTube(s) => plain(s, webmusic::UA),
                    webmusic::Audio::Feed(feed) => self.feed_stream(feed).await?,
                }
            }
            Source::Feed => {
                let session = self.api.account().ok_or_else(|| anyhow::anyhow!("signed out"))?;
                let feed = session.feed(&self.http(), &self.jsc, video_id).await?.ok_or_else(|| anyhow::anyhow!("not an RSS podcast episode"))?;
                self.feed_stream(feed).await?
            }
            Source::YtDlp | Source::YtDlpAfterBotCheck | Source::SafariHls => {
                let session = self.api.account().ok_or_else(|| anyhow::anyhow!("sign in to play this song"))?;
                let format = if source == Source::SafariHls { ytdlp::Format::SafariHls } else { ytdlp::Format::File };
                let s = ytdlp::resolve(&self.http(), &self.data_dir, &session.cookie, video_id, format).await?;
                // Cookies Google replaced while yt-dlp used the session.
                self.api.keep(&session, s.replaced.iter().map(String::as_str));
                if format == ytdlp::Format::SafariHls {
                    let (segments, duration_ms) = crate::hls::safari_playlist(&self.http(), &s.url, &s.user_agent).await?;
                    let (starts, urls) = segments.into_iter().unzip();
                    ResolvedStream { media: stream::Media::TransportStream(urls), starts: Some(starts), size: 0, duration_ms, user_agent: s.user_agent, loudness_db: None, opus: false }
                } else {
                    // yt-dlp's JSON doesn't carry YouTube's loudness figure.
                    ResolvedStream { media: stream::Media::File(s.url), starts: None, size: s.size, duration_ms: s.duration_ms, user_agent: s.user_agent, loudness_db: None, opus: false }
                }
            }
        })
    }

    /// An RSS podcast episode's file, fetched from where its redirects (trackers, mostly)
    /// end, so they aren't followed again for every range.
    async fn feed_stream(&self, feed: webmusic::Feed) -> anyhow::Result<ResolvedStream> {
        let (url, size) = stream::probe_feed(&self.http(), &feed.url, webmusic::UA).await?;
        log::info!("podcast feed file on {}", reqwest::Url::parse(&url).ok().as_ref().and_then(reqwest::Url::host_str).unwrap_or("?"));
        Ok(ResolvedStream {
            media: stream::Media::Feed(url),
            starts: None,
            size,
            duration_ms: feed.duration_ms,
            user_agent: webmusic::UA.to_owned(),
            loudness_db: None,
            opus: false,
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

    pub fn set_backup_stream(&self, on: bool) {
        let mut prefetched = self.prefetched.lock().unwrap();
        if self.backup_stream.swap(on, Ordering::Relaxed) != on {
            if let Some((_, stream)) = prefetched.take() {
                stream.cancel();
            }
            drop(prefetched);
            self.audio.status.staged_gen.store(0, Ordering::Release);
            self.audio.send(AudioCmd::ClearNext);
            self.clear_staged();
        }
    }

    /// Explicit songs hidden, and whether YouTube's Restricted Mode is asked for as well.
    pub fn set_hide_explicit(&self, on: bool, restricted: bool) {
        self.clean.store(on, Ordering::Relaxed);
        self.api.set_restricted(on && restricted);
    }

    /// Settings › Song info: the language and country titles and names come in.
    pub fn set_locale(&self, locale: crate::locale::Locale) {
        self.api.set_locale(locale);
    }

    /// Like or unlike a selection, one at a time, then check Liked Music and try again
    /// what didn't take. Unlikes still listed after that count as refused; likes aren't
    /// counted so (songs that can't be played are liked but not listed).
    async fn like_many(&self, ids: Vec<String>, like: bool, versions: UnlikeVersions) -> anyhow::Result<Resp> {
        let total = ids.len();
        let (done, failed, others) = like_many(&self.api, ids, like, &versions, |taken, n| {
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
    /// An RSS podcast episode's file, which the web player finds without Premium.
    Feed,
    YtDlp,
    YtDlpAfterBotCheck,
    SafariHls,
}

enum Found {
    Saved(Arc<Shared>),
    Url(Source, ResolvedStream),
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
    /// Opus in WebM; otherwise AAC in MP4, or a podcast feed's file.
    opus: bool,
}

impl ResolvedStream {
    /// A buffer laid out for this stream. `head_only`: a prefetch's.
    fn buffer(&self, head_only: bool) -> Arc<Shared> {
        match (&self.starts, &self.media) {
            (Some(starts), _) => Shared::segmented(starts.clone(), self.duration_ms, self.loudness_db, head_only),
            (None, stream::Media::Feed(_)) => Shared::feed(self.size, self.duration_ms, head_only),
            (None, _) => Shared::new(self.size, self.duration_ms, self.loudness_db, self.opus, head_only),
        }
    }

    /// For the log.
    fn kind(&self) -> &'static str {
        match (&self.media, self.opus) {
            (stream::Media::Feed(_), _) => "podcast feed file",
            (_, true) => "Opus",
            _ => "AAC",
        }
    }
}

/// The anonymous visionOS player refused because the song needs an account:
/// age-restricted (LOGIN_REQUIRED) or Music Premium only.
fn needs_account(e: &anyhow::Error) -> bool {
    let msg = e.to_string();
    msg.contains("LOGIN_REQUIRED") || msg.contains("Premium")
}

fn bot_check(e: &anyhow::Error) -> bool {
    e.downcast_ref::<innertube::Refused>().is_some_and(|r| {
        r.status == "LOGIN_REQUIRED" && r.reason.to_ascii_lowercase().contains("not a bot")
    })
}

/// Whether a refreshed stream's segments fit the buffer. The download checks their
/// count. A Safari playlist can be cut differently each time, and each block holds
/// one segment's audio, so its start times must match too.
fn same_segments(source: Source, old: Option<&[f64]>, new: Option<&[f64]>) -> bool {
    source != Source::SafariHls || old == new
}

/// The anonymous visionOS player says the video isn't there ("ERROR: This video is not
/// available"), which is also its answer for an episode of a podcast added by RSS.
/// Nothing in that answer tells the two apart.
fn not_available(e: &anyhow::Error) -> bool {
    e.downcast_ref::<innertube::Refused>().is_some_and(|r| matches!(r.status.as_str(), "ERROR" | "UNPLAYABLE"))
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
    // Digits however they're written around ("12 episodes", "12集").
    card.target != Target::Playlist("SE".into()) || card.subtitle.split(|c: char| !c.is_ascii_digit()).any(|w| w.parse::<u64>().is_ok_and(|n| n > 0))
}

pub fn compact(n: u64) -> String {
    if n < 1_000 {
        return n.to_string();
    }
    let units = [(1e3, "K"), (1e6, "M"), (1e9, "B")];
    // The unit the number rounds into: 999,950 is "1M", not "1000K".
    let mut at = match n {
        1_000..1_000_000 => 0,
        1_000_000..1_000_000_000 => 1,
        _ => 2,
    };
    let rounded = |at: usize| {
        let v = n as f64 / units[at].0;
        // "6M", not "6.0M"; three significant figures and up need no decimal.
        if v >= 99.95 { format!("{v:.0}") } else { format!("{v:.1}") }
    };
    let mut s = rounded(at);
    if at + 1 < units.len() && s.parse::<f64>().is_ok_and(|v| v >= 1000.0) {
        at += 1;
        s = rounded(at);
    }
    format!("{}{}", s.strip_suffix(".0").unwrap_or(&s), units[at].1)
}

/// The picture of artist `id` among artists found by `name`: the one with that id, or else
/// one with that very name (a channel's songs can name an id search doesn't list).
fn artist_pic(found: &[Card], id: &str, name: &str) -> Option<crate::discord::Badge> {
    let hit = found.iter().find(|c| matches!(&c.target, Target::Artist(a) if a == id)).or_else(|| found.iter().find(|c| c.title.eq_ignore_ascii_case(name)))?;
    let url = sized(model::pick(&hit.thumbs, 120)?, 160);
    Some(crate::discord::Badge { name: hit.title.clone(), url })
}

// Larger art URL for the background / now-playing view.
/// Whether `e` is that nothing could be reached: no connection, or a name that doesn't
/// resolve. Not a server that answered with an error, nor one that's slow, nor a proxy
/// that answered and wouldn't pass the request on (it wants a sign-in, say): reqwest
/// counts that as connecting too, and its own error says more.
fn unreachable(e: &anyhow::Error) -> bool {
    let proxy_said_no = e.chain().any(|c| matches!(c.to_string().as_str(), "tunnel error: proxy authorization required" | "tunnel error: unsuccessful"));
    !proxy_said_no && e.chain().any(|c| c.downcast_ref::<reqwest::Error>().is_some_and(reqwest::Error::is_connect))
}

pub fn art_url(t: &Track) -> Option<String> {
    model::pick(&t.thumbs, 120).map(|u| sized(u, 544))
}

#[cfg(test)]
mod unreachable_tests {
    use anyhow::Context;

    #[tokio::test]
    async fn no_connection_is_unreachable_and_a_servers_error_isnt() {
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        // Nothing listens there: refused, as with no network.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
        let refused = http.get(format!("http://{closed}/")).send().await.unwrap_err();
        assert!(super::unreachable(&anyhow::Error::new(refused).context("Home")));
        // A server that answered, with an error.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut socket, _) = listener.accept().await.unwrap();
            let _ = socket.read(&mut [0; 1024]).await;
            let _ = socket.write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n").await;
        });
        let status = http.get(format!("http://{addr}/")).send().await.unwrap().error_for_status().unwrap_err();
        assert!(!super::unreachable(&anyhow::Error::new(status)));
        assert!(!super::unreachable(&Err::<(), _>(anyhow::anyhow!("no songs")).context("Search").unwrap_err()));
        // A proxy that answered, and wants a sign-in or couldn't get through itself.
        for answer in ["407 Proxy Authentication Required", "502 Bad Gateway"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let (mut socket, _) = listener.accept().await.unwrap();
                let _ = socket.read(&mut [0; 1024]).await;
                let _ = socket.write_all(format!("HTTP/1.1 {answer}
Content-Length: 0

").as_bytes()).await;
            });
            let http = reqwest::Client::builder().proxy(reqwest::Proxy::all(format!("http://{addr}")).unwrap()).build().unwrap();
            let refused = http.get("https://music.youtube.com/").send().await.unwrap_err();
            assert!(!super::unreachable(&anyhow::Error::new(refused)), "{answer}");
        }
    }
}

#[cfg(test)]
mod artist_pic_tests {
    use super::*;

    fn artist(name: &str, id: &str, url: &str) -> Card {
        let thumbs = vec![Thumb { url: url.into(), width: 120, height: 120 }];
        Card { title: name.into(), subtitle: String::new(), thumbs, target: Target::Artist(id.into()), round: true, links: Vec::new() }
    }

    #[test]
    fn the_artist_with_that_id_or_name() {
        let found = [artist("Drake Bell", "UC2", "https://lh3.googleusercontent.com/b=w120-h120-p-l90-rj"), artist("Drake", "UC1", "https://lh3.googleusercontent.com/a=w120-h120-p-l90-rj")];
        let pic = artist_pic(&found, "UC1", "Drake").unwrap();
        assert_eq!((pic.name.as_str(), pic.url.as_str()), ("Drake", "https://lh3.googleusercontent.com/a=w160-h160-p-l90-rj"));
        assert_eq!(artist_pic(&found, "UCx", "drake bell").unwrap().name, "Drake Bell");
        assert!(artist_pic(&found, "UCx", "Someone else").is_none());
    }
}

#[cfg(test)]
mod download_queue_tests {
    use super::*;

    fn song(id: &str) -> Track {
        Track { id: id.into(), title: id.to_uppercase(), ..Default::default() }
    }

    fn songs(ids: &[&str]) -> Vec<Track> {
        ids.iter().map(|id| song(id)).collect()
    }

    fn run<'a>(q: &'a DownloadQueue, key: &str) -> &'a DownloadRun {
        q.runs.iter().find(|r| r.key == key).unwrap()
    }

    fn waiting(q: &DownloadQueue) -> Vec<&str> {
        q.waiting.iter().map(|t| t.id.as_str()).collect()
    }

    #[test]
    fn a_song_two_runs_want_downloads_once_for_both() {
        let mut q = DownloadQueue::default();
        assert_eq!(q.add(Some("A".into()), "A".into(), 3, songs(&["1", "2"]), None), ("A".to_owned(), 2));
        assert_eq!(q.add(Some("B".into()), "B".into(), 2, songs(&["2", "3"]), None), ("B".to_owned(), 2));
        assert_eq!(waiting(&q), ["1", "2", "3"]);
        let (a, b) = (run(&q, "A"), run(&q, "B"));
        assert_eq!((a.total, a.done, a.skipped), (3, 1, 1));
        assert_eq!((b.total, b.done), (2, 0));

        let two = q.waiting[1].clone();
        q.finish(&two, &Ok::<_, ()>(true));
        assert_eq!((run(&q, "A").done, run(&q, "A").saved), (2, 1));
        assert_eq!((run(&q, "B").done, run(&q, "B").saved), (1, 1));
    }

    #[test]
    fn queued_again_a_run_takes_on_only_what_it_lacks() {
        let mut q = DownloadQueue::default();
        q.add(Some("A".into()), "A".into(), 2, songs(&["1", "2"]), None);
        assert_eq!(q.add(Some("A".into()), "A".into(), 3, songs(&["1", "2", "3"]), None).1, 1);
        assert_eq!(waiting(&q), ["1", "2", "3"]);
        assert_eq!(run(&q, "A").total, 3);
    }

    #[test]
    fn queued_again_a_failed_song_is_retried_not_counted_twice() {
        let mut q = DownloadQueue::default();
        q.add(Some("A".into()), "A".into(), 3, songs(&["1", "2", "3"]), None);
        let one = q.next().unwrap();
        q.finish(&one, &Err::<bool, _>(()));
        assert_eq!((run(&q, "A").done, run(&q, "A").failed.clone()), (1, vec!["1".to_owned()]));

        // Queued again from the card's menu while 2 and 3 are still to go: 1 failed, so
        // it isn't downloaded and comes back; 2 and 3 are on their way already.
        assert_eq!(q.add(Some("A".into()), "A".into(), 3, songs(&["1", "2", "3"]), None).1, 1);
        let a = run(&q, "A");
        assert_eq!((a.total, a.done), (3, 0));
        assert!(a.failed.is_empty() && a.failed_ids.is_empty());
        assert_eq!(waiting(&q), ["2", "3", "1"]);

        // It fails again: listed once.
        for _ in 0..3 {
            let t = q.next().unwrap();
            let failed = t.id == "1";
            q.finish(&t, &if failed { Err(()) } else { Ok(true) });
        }
        let a = run(&q, "A");
        assert_eq!((a.total, a.done, a.saved), (3, 3, 2));
        assert_eq!(a.failed, ["1"]);
        assert!(a.finished());
    }

    #[test]
    fn cancelling_drops_only_the_songs_no_other_run_wants() {
        let mut q = DownloadQueue::default();
        q.add(Some("A".into()), "A".into(), 3, songs(&["1", "2", "3"]), None);
        q.add(Some("B".into()), "B".into(), 1, songs(&["3"]), None);
        // 1 is on its way: it finishes.
        let one = q.next().unwrap();
        assert!(q.cancel("A"));
        assert!(!q.cancel("nothing"));
        assert_eq!(waiting(&q), ["3"]);
        assert_eq!(q.wanted["3"], ["B"]);
        let a = run(&q, "A");
        assert_eq!((a.total, a.done, a.cancelled), (1, 0, true));
        q.finish(&one, &Ok::<_, ()>(true));
        assert!(run(&q, "A").finished());

        // A finished run is listed once more, then let go.
        let Resp::Downloads(runs) = q.snapshot() else { panic!() };
        assert_eq!(runs.len(), 2);
        assert_eq!(q.runs.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), ["B"]);
    }

    #[test]
    fn selections_get_keys_of_their_own_and_workers_are_capped() {
        let mut q = DownloadQueue::default();
        assert_eq!(q.add(None, String::new(), 1, songs(&["1"]), None).0, "selection-1");
        assert_eq!(q.add(None, String::new(), 2, songs(&["2", "3"]), None).0, "selection-2");
        assert_eq!(q.more_workers(), DOWNLOADS_AT_ONCE.min(3));
        assert_eq!(q.more_workers(), 0);
        while q.next().is_some() {}
        assert_eq!(q.workers, DOWNLOADS_AT_ONCE.min(3) - 1);
    }

    #[test]
    fn a_song_goes_in_the_folder_of_the_first_run_that_wants_it() {
        let mut q = DownloadQueue::default();
        let folder = |id: &str| Some(crate::downloads::ListFolder { id: id.into(), name: id.into() });
        q.add(Some("A".into()), "A".into(), 2, songs(&["1", "2"]), folder("A"));
        q.add(Some("B".into()), "B".into(), 2, songs(&["2", "3"]), folder("B"));
        q.add(None, String::new(), 1, songs(&["4"]), None);
        assert_eq!(q.folder_for("2"), folder("A"));
        assert_eq!(q.folder_for("3"), folder("B"));
        assert_eq!(q.folder_for("4"), None);
        // A's waiting songs dropped: B's is the folder now.
        q.cancel("A");
        assert_eq!(q.folder_for("2"), folder("B"));
    }
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

#[cfg(test)]
mod resolve_tests {
    use super::*;

    fn refused(status: &str, reason: &str) -> anyhow::Error {
        innertube::Refused { status: status.into(), reason: reason.into() }.into()
    }

    #[test]
    fn a_video_visionos_says_isnt_there_may_be_an_rss_episode() {
        // What visionOS answers for an episode of a podcast added by RSS.
        assert!(not_available(&refused("ERROR", "This video is not available")));
        assert!(not_available(&refused("UNPLAYABLE", "This video is unavailable")));
        assert!(not_available(&refused("ERROR", "x").context("visionOS player")));
        // Those that need an account keep their own route.
        assert!(!not_available(&refused("LOGIN_REQUIRED", "Sign in to confirm your age")));
        assert!(needs_account(&refused("LOGIN_REQUIRED", "Sign in to confirm your age")));
        assert!(!bot_check(&refused("LOGIN_REQUIRED", "Sign in to confirm your age")));
        assert!(bot_check(&refused("LOGIN_REQUIRED", "Sign in to confirm you’re not a bot")));
        assert!(!needs_account(&refused("ERROR", "This video is not available")));
        // Blocked where the user is: the signed-in player gets a try too (#373).
        let region = refused("UNPLAYABLE", "The uploader has not made this video available in your country");
        assert!(not_available(&region) && !needs_account(&region));
        let premium = refused("UNPLAYABLE", "This video is only available to Music Premium members");
        assert!(needs_account(&premium));
        // Only the player's own answer, not an error that reads like one.
        assert!(!not_available(&anyhow::anyhow!("ERROR: This video is not available")));
    }

    #[test]
    fn only_a_safari_refresh_must_keep_its_segment_starts() {
        let (old, moved) = ([0.0, 7.0, 14.0], [0.0, 6.9, 14.0]);
        // visionOS HLS: the download only checks the count, as before.
        assert!(same_segments(Source::VisionOsHls, Some(&old), Some(&moved)));
        assert!(same_segments(Source::VisionOs, None, None));
        assert!(same_segments(Source::SafariHls, Some(&old), Some(&old)));
        assert!(!same_segments(Source::SafariHls, Some(&old), Some(&moved)));
    }

    /// Read-only, signed in to an account with a podcast added by RSS (NPR's Planet
    /// Money): visionOS says the episode isn't available, and the web player gives its
    /// feed's file, which is reachable.
    /// `YTM_DATA_DIR=<profile> cargo test --release -- --ignored rss_episode_live --nocapture`
    #[tokio::test]
    #[ignore]
    async fn rss_episode_live() {
        let dir = PathBuf::from(std::env::var("YTM_DATA_DIR").unwrap());
        let api = ytmapi::Api::new(&dir);
        let session = api.account().expect("no session in the vault");
        let (http, solver) = (reqwest::Client::new(), jsc::Solver::new(&dir));
        let id = std::env::var("YTM_RSS_EPISODE").unwrap_or_else(|_| "M77cW_SUPm4".into());
        let vd = api.visitor_data(false).await.unwrap();
        let Err(e) = innertube::audio_stream(&http, &id, &vd, false).await else { panic!("visionOS played {id}") };
        println!("visionOS: {e:#}");
        assert!(not_available(&e), "{e:#}");
        let t = std::time::Instant::now();
        let feed = session.feed(&http, &solver, &id).await.unwrap().expect("not an RSS episode");
        println!("web player: {} ms, {} (in {:?})", feed.duration_ms, feed.url, t.elapsed());
        assert!(feed.duration_ms > 0);
        let (file, size) = stream::probe_feed(&http, &feed.url, webmusic::UA).await.unwrap();
        println!("  -> {file}: {size} bytes");
        assert!(size > 0);
    }
}

#[cfg(test)]
mod compact_tests {
    use super::compact;

    #[test]
    fn counts_round_into_the_next_unit() {
        assert_eq!(compact(999), "999");
        assert_eq!(compact(1_000), "1K");
        assert_eq!(compact(1_260), "1.3K");
        assert_eq!(compact(99_960), "100K");
        assert_eq!(compact(123_456), "123K");
        assert_eq!(compact(999_499), "999K");
        assert_eq!(compact(999_500), "1M");
        assert_eq!(compact(999_950), "1M");
        assert_eq!(compact(6_000_000), "6M");
        assert_eq!(compact(999_960_000), "1B");
        assert_eq!(compact(3_600_000_000), "3.6B");
        assert_eq!(compact(1_234_000_000_000), "1234B");
    }
}
