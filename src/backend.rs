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
    /// A search of only your library or only your uploads.
    SearchIn { query: String, scope: ytm::Scope },
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
    /// Download the songs of an album or playlist (`target`; fetched whole when `tracks`
    /// is `None`) or of a selection (`target` `None`). Songs already downloaded are
    /// skipped. More than `LARGE_DOWNLOAD` to go asks first (`Resp::ConfirmDownload`)
    /// unless `confirmed`.
    DownloadMany { target: Option<Target>, title: String, tracks: Option<Vec<Track>>, confirmed: bool },
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
    /// A song's music video, for Now playing's Song/Video switch.
    MusicVideo(Track),
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
    /// Save one of your playlist's sort orders to it ([`PlaylistSort::actions`]).
    SortPlaylist { id: String, actions: serde_json::Value },
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
    Subscribe { channel_id: String, on: bool },
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
            Req::Search(_) | Req::SearchIn { .. } => "Search",
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
            Req::Download { .. } | Req::DownloadMany { .. } => "Download",
            Req::Lyrics(_) => "Lyrics",
            Req::Segments { .. } => "SponsorBlock",
            Req::LyricsExtras { .. } => "Translation",
            Req::Art { .. } => "Art",
            Req::Radio { .. } => "Loading radio",
            Req::Comments { .. } | Req::CommentsPage { .. } | Req::CommentAction { .. } => "Comments",
            Req::PlayCollection { .. } => "Playback",
            Req::Play { .. } => "Playback",
            Req::Prefetch { .. } => "Prefetch",
            Req::VideoStream { .. } | Req::MusicVideo(_) => "Video",
            Req::Canvas(_) => "Animated cover",
            Req::Like { .. } | Req::LikeMany { .. } | Req::UnlikeBoth { .. } => "Like",
            Req::Dislike(_) => "Dislike",
            Req::CreatePlaylist { .. } => "Create playlist",
            Req::EditPlaylist { .. } => "Edit playlist",
            Req::SortPlaylist { .. } => "Sort playlist",
            Req::AddToPlaylist { .. } => "Add to playlist",
            Req::RemoveFromPlaylist { .. } => "Remove from playlist",
            Req::MoveInPlaylist { .. } => "Move in playlist",
            Req::DeletePlaylist(_) => "Delete playlist",
            Req::Collaboration(_) => "Loading collaboration",
            Req::SetCollaboration { .. } => "Collaboration",
            Req::SaveCollection { .. } => "Save to library",
            Req::Subscribe { .. } => "Subscribe",
            Req::SetCookie(_) => "Sign in",
            Req::Logout => "Sign out",
        }
    }

    /// Background requests fail quietly (logged, no toast).
    fn quiet(&self) -> bool {
        matches!(self, Req::Suggest { .. } | Req::Account | Req::Channels | Req::ServerQueue | Req::Art { .. } | Req::Prefetch { .. } | Req::HomeMore(_) | Req::Lyrics(_) | Req::Segments { .. } | Req::MusicVideo(_) | Req::LyricsExtras { .. } | Req::Comments { .. } | Req::CommentsPage { .. } | Req::CommentAction { .. })
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
    /// All of YouTube Music, or just your library or your uploads.
    pub scope: ytm::Scope,
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
    /// Every album, playlist or selection being downloaded, after one of them moved on.
    /// A finished one (`DownloadRun::finished`) is listed this once.
    Downloads(Vec<DownloadRun>),
    /// More than `LARGE_DOWNLOAD` songs (`count`) aren't downloaded yet: asked before
    /// sending the same `Req::DownloadMany` again, `confirmed`.
    ConfirmDownload { target: Option<Target>, title: String, tracks: Vec<Track>, count: usize },
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
    /// A subscribe or unsubscribe went through (`ok`), or didn't.
    Subscribed { channel_id: String, on: bool, ok: bool },
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
    /// The song `song`'s music video (`None`: it has none, or none was sure).
    MusicVideo { song: String, video: Option<ytm::MusicVideo> },
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
                | Resp::Radio { .. }
                | Resp::Suggest { .. }
                | Resp::SearchIn(_)
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
                | Resp::Artist(_)
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
    let mut r = SearchResults { query, scope, tracks, videos, albums: Vec::new(), artists: Vec::new(), playlists: Vec::new(), podcasts: Vec::new(), episodes: found.episodes };
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
    fn add(&mut self, key: Option<String>, title: String, listed: usize, todo: Vec<Track>) -> (String, usize) {
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
            }
            None => self.runs.push(DownloadRun { key: key.clone(), title, total: added + skipped, done: skipped, skipped, ..Default::default() }),
        }
        (key, queued)
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
/// Owns only a decoder being opened, never a source handed to the audio engine.
/// Serializes generation changes with publication of a ready decoder.
#[derive(Default)]
struct PlaybackLoads {
    state: std::sync::Mutex<PlaybackLoad>,
}

#[derive(Default)]
struct PlaybackLoad {
    generation: u64,
    pending: Option<Arc<Shared>>,
}

impl PlaybackLoads {
    fn begin(&self, generation: u64, wanted: &std::sync::atomic::AtomicU64) -> bool {
        let mut state = self.state.lock().unwrap();
        if wanted.load(Ordering::Relaxed) != generation || generation < state.generation {
            return false;
        }
        state.generation = generation;
        if let Some(old) = state.pending.take() {
            old.cancel();
        }
        true
    }

    fn current<T>(&self, generation: u64, wanted: &std::sync::atomic::AtomicU64, action: impl FnOnce() -> T) -> Option<T> {
        let state = self.state.lock().unwrap();
        (state.generation == generation && wanted.load(Ordering::Relaxed) == generation).then(action)
    }

    fn register(&self, generation: u64, wanted: &std::sync::atomic::AtomicU64, shared: &Arc<Shared>) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.generation != generation || wanted.load(Ordering::Relaxed) != generation {
            shared.cancel();
            return false;
        }
        state.pending = Some(shared.clone());
        true
    }

    fn finish(&self, generation: u64, wanted: &std::sync::atomic::AtomicU64, shared: &Arc<Shared>, publish: impl FnOnce()) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.generation != generation || wanted.load(Ordering::Relaxed) != generation
            || !state.pending.as_ref().is_some_and(|s| Arc::ptr_eq(s, shared))
        {
            shared.cancel();
            return false;
        }
        state.pending = None;
        publish();
        true
    }
}

#[derive(Debug)]
struct DecoderPanicked;

impl std::fmt::Display for DecoderPanicked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Audio decoder failed while opening the stream")
    }
}

impl std::error::Error for DecoderPanicked {}

fn open_decoder_guarded<T>(open: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(open))
        .map_err(|_| anyhow::Error::new(DecoderPanicked))?
}

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

const RADIO_HISTORY_SEEDS: usize = 32;
const RADIO_HISTORY_IDS: usize = 200;

/// Recent server recommendations, only in RAM and only for this account/channel epoch.
/// The async mutex is held through each radio fetch and reservation, so simultaneous
/// requests cannot both select against the same old history.
#[derive(Default)]
struct RadioHistory {
    epoch: u64,
    seeds: std::collections::VecDeque<(String, std::collections::VecDeque<String>)>,
}

impl RadioHistory {
    fn scope(&mut self, epoch: u64) {
        if self.epoch != epoch {
            self.seeds.clear();
            self.epoch = epoch;
        }
    }

    fn exclusions(&mut self, epoch: u64, seed: &str) -> std::collections::HashSet<String> {
        self.scope(epoch);
        let mut avoid: std::collections::HashSet<_> = self.seeds.iter().find(|(id, _)| id == seed)
            .into_iter().flat_map(|(_, ids)| ids.iter().cloned()).collect();
        avoid.insert(seed.to_owned());
        avoid
    }

    fn remember(&mut self, epoch: u64, seed: &str, returned: impl IntoIterator<Item = String>) {
        self.scope(epoch);
        let old = self.seeds.iter().position(|(id, _)| id == seed).and_then(|i| self.seeds.remove(i));
        let mut ids = old.map_or_else(std::collections::VecDeque::new, |(_, ids)| ids);
        for id in returned {
            if let Some(i) = ids.iter().position(|old| old == &id) {
                ids.remove(i);
            }
            ids.push_back(id);
            while ids.len() > RADIO_HISTORY_IDS {
                ids.pop_front();
            }
        }
        if !ids.is_empty() {
            self.seeds.push_back((seed.to_owned(), ids));
        }
        while self.seeds.len() > RADIO_HISTORY_SEEDS {
            self.seeds.pop_front();
        }
    }
}

#[cfg(test)]
mod radio_history_tests {
    use super::*;

    #[test]
    fn history_excludes_returned_ids_per_seed_and_resets_at_account_epoch() {
        let mut h = RadioHistory::default();
        h.remember(2, "seed-a", ["a".into(), "b".into()]);
        assert_eq!(h.exclusions(2, "seed-a"), ["seed-a", "a", "b"].map(str::to_owned).into_iter().collect());
        assert_eq!(h.exclusions(2, "seed-b"), ["seed-b"].map(str::to_owned).into_iter().collect());
        assert_eq!(h.exclusions(3, "seed-a"), ["seed-a"].map(str::to_owned).into_iter().collect());
        assert!(h.seeds.is_empty());
    }

    #[test]
    fn radio_history_bounds_ids_and_seeds_without_erasing_latest_results() {
        let mut h = RadioHistory::default();
        h.remember(1, "seed", (0..250).map(|i| format!("track-{i}")));
        let avoid = h.exclusions(1, "seed");
        assert_eq!(avoid.len(), RADIO_HISTORY_IDS + 1);
        assert!(!avoid.contains("track-49") && avoid.contains("track-50") && avoid.contains("track-249"));
        for i in 0..RADIO_HISTORY_SEEDS {
            h.remember(1, &format!("seed-{i}"), ["candidate".into()]);
        }
        assert_eq!(h.seeds.len(), RADIO_HISTORY_SEEDS);
        assert_eq!(h.exclusions(1, "seed").len(), 1);
        assert!(h.exclusions(1, "seed-31").contains("candidate"));
    }
}
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
    loads: Arc<PlaybackLoads>,
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
    /// Albums and playlists being downloaded.
    batch: Arc<std::sync::Mutex<DownloadQueue>>,
    radios: Arc<tokio::sync::Mutex<RadioHistory>>,
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
        let backend = Self { rt: Arc::new(rt), api, tx, audio, wake, prefetched: Default::default(), current: Default::default(), loads: Default::default(), data_dir, lyrics, translate, jsc, premium: Arc::new(tokio::sync::Mutex::new(Err(None))), hq: Default::default(), clean: Default::default(), visionos_hls: Default::default(), songs: Default::default(), batch: Default::default(), radios: Default::default() };
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
        if let Some(generation) = play_gen
            && !self.loads.begin(generation, &self.audio.status.wanted_gen)
        {
            return;
        }
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
                        (Some(generation), _) => Some(Resp::PlayError { generation, msg: "YouTube didn't respond properly".into() }),
                        (_, Some((channel_id, on))) => Some(Resp::Subscribed { channel_id, on, ok: false }),
                        _ => (!quiet).then(|| Resp::Error(format!("{what} failed: YouTube didn't respond properly"))),
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
                Req::SearchIn { query, scope } => Resp::SearchIn(search_in(ytm::search_in(api, &query, scope).await?, query, scope)),
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
                    let epoch = api.epoch();
                    let mut history = self.radios.lock().await;
                    if api.epoch() != epoch {
                        return Ok(None);
                    }
                    // A music video's song version keeps the radio in the song domain.
                    let song = self.song_version(&seed, song_of.as_ref(), PREFETCH_SONG_WAIT).await;
                    let from = song.as_ref().map_or(&seed, |s| &s.id);
                    let mut avoid = history.exclusions(epoch, from);
                    avoid.insert(seed.clone());
                    if api.epoch() != epoch {
                        return Ok(None);
                    }
                    let tracks = ytm::radio_with_recent(api, from, self.is_logged_in(), 50, &avoid).await?;
                    if api.epoch() != epoch {
                        return Ok(None);
                    }
                    history.remember(epoch, from, tracks.iter().map(|t| t.id.clone()));
                    log::debug!("radio: seed_count=1 recent_count={} returned_count={}", avoid.len(), tracks.len());
                    Resp::Radio { seed, tracks }
                }
                Req::PlayCollection { target } => {
                    let (title, tracks) = match &target {
                        Target::Album(id) => ytm::album(api, id).await.map(|c| (c.title, c.tracks))?,
                        Target::Playlist(id) => ytm::playlist(api, id, me, None).await.map(|c| (c.title, c.tracks))?,
                        Target::Artist(id) => ytm::artist(api, id, false).await.map(|a| (a.name, a.top))?,
                        Target::Song(t) => (t.title.clone(), vec![t.clone()]),
                    };
                    Resp::PlayQueue { target, title, tracks }
                }
                Req::Download { track: t, for_video } => {
                    let (path, _) = self.download(&t, for_video, false).await?;
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
                        Resp::PlayError { generation, msg: format!("{e:#}") }
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
                // A failure is answered too, so the Sort button comes back.
                Req::SortPlaylist { id, actions } => match ytm::sort_playlist(api, &id, actions).await {
                    Ok(()) => Resp::PlaylistChanged { id, message: "Playlist sorted" },
                    Err(e) => {
                        log::warn!("sort playlist failed: {e:#}");
                        Resp::PlaylistChanged { id, message: "Couldn't sort the playlist" }
                    }
                },
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
        let playing = self.current.lock().unwrap().as_ref().map(|(_, s)| s.clone());
        let (found, ()) = tokio::join!(self.find_stream(&video_id), async {
            if let Some(s) = playing {
                s.finished().await;
            }
        });
        if moot() {
            return Ok(());
        }
        log::debug!("prefetch {video_id}");
        let shared = self.start_stream(&video_id, found?, true)?;
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
        let Some(ready) = self.loads.current(generation, &self.audio.status.wanted_gen, || {
            let prefetched = self.prefetched.lock().unwrap().take().filter(|(_, s)| s.can_reuse());
            let replay = self.current.lock().unwrap().as_ref().filter(|(id, s)| *id == video_id && s.is_complete()).map(|(_, s)| s.clone());
            match prefetched {
                Some((id, s)) if id == video_id && !(self.visionos_hls.load(Ordering::Relaxed) && s.segments.is_none() && !s.is_complete()) => Some(s),
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
            }
        }) else {
            return Ok(());
        };
        let opened = match ready {
            Some(s) => {
                log::debug!("play {video_id} (gen {generation}): using buffered stream");
                OpenedStream { shared: s, decoder: None }
            }
            None => {
                if self.audio.status.wanted_gen.load(Ordering::Relaxed) != generation {
                    return Ok(());
                }
                // Taken only for the video; otherwise dropping `early` stops it.
                let opened = match early.0.take_if(|_| song.is_none() && at.is_zero()) {
                    Some(open) => {
                        log::debug!("play {video_id} (gen {generation}): stream opened during the song lookup");
                        open.await.map_err(anyhow::Error::from).flatten().map(|shared| OpenedStream { shared, decoder: None })
                    }
                    None => self.open_prepared_stream(&video_id, at, generation).await,
                };
                match opened {
                    Ok(s) => s,
                    // The song won't play: the music video still does.
                    Err(e) if song.is_some() && at.is_zero() => {
                        if self.loads.current(generation, &self.audio.status.wanted_gen, || ()).is_none() {
                            return Ok(());
                        }
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
                            Some(shared) => OpenedStream { shared, decoder: None },
                            None => self.open_prepared_stream(listed, at, generation).await?,
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
        };
        let OpenedStream { shared, decoder: prepared } = opened;
        // A skipped resolver must not publish a stream or open another decoder.
        // Only this pending load is cancelled by the next Play; the audio engine
        // retains ownership of an outgoing song while it fades out.
        if !self.loads.register(generation, &self.audio.status.wanted_gen, &shared) {
            return Ok(());
        }
        // If the song's audio won't start after all, the music video plays instead.
        let fallback = (song.is_some() && at.is_zero()).then(|| (self.clone(), listed.to_owned(), report));
        if let Some(song) = song {
            log::info!("play {listed} as its {} {video_id}", if song.has_video() { "music video" } else { "song" });
            // Carried over, the UI has it already.
            if !was_carried {
                let _ = self.tx.send(Resp::PlayingAs { generation, listed: listed.to_owned(), song: Some(song) });
                (self.wake)();
            }
        }
        let video_id = video_id.as_str();
        log::debug!("play {video_id} (gen {generation}): stream open in {:?}", t0.elapsed());
        if self.audio.status.wanted_gen.load(Ordering::Relaxed) != generation {
            shared.cancel();
            return Ok(());
        }

        let audio = self.audio.clone();
        let tx = self.tx.clone();
        let wake = self.wake.clone();
        // Opening the decoder reads the container header, which blocks on the download.
        // Incomplete files resume by streaming to the target: building a seek
        // index first would make an available prefix depend on missing tail data.
        let shared2 = shared.clone();
        let (loads, current) = (self.loads.clone(), self.current.clone());
        let playing_id = video_id.to_owned();
        let report = (report && self.is_logged_in() && reports_plays()).then(|| (self.clone(), video_id.to_owned()));
        let open = move || -> anyhow::Result<crate::audio::TrackDecoder> {
            if let Some(decoder) = prepared {
                return Ok(decoder);
            }
            let mut decoder = stream::decoder_at(shared.clone(), at)?;
            if !at.is_zero() {
                decoder.prime_frame()?;
            }
            Ok(decoder)
        };
        tokio::task::spawn_blocking(move || match open_decoder_guarded(open) {
            Ok(decoder) => {
                if !loads.finish(generation, &audio.status.wanted_gen, &shared2, || {
                    *current.lock().unwrap() = Some((playing_id, shared2.clone()));
                    audio.send(AudioCmd::Start { generation, decoder, shared: shared2.clone(), at });
                }) {
                    return;
                }
                log::debug!("play gen {generation}: decoder ready at {at:?} in {:?}", t0.elapsed());
                // Skipped past before it started: that one doesn't count.
                if let Some((this, id)) = report.filter(|_| audio.status.wanted_gen.load(Ordering::Relaxed) == generation) {
                    let rt = this.rt.clone();
                    rt.spawn(async move {
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
            }
            Err(e) => {
                if !loads.finish(generation, &audio.status.wanted_gen, &shared2, || shared2.cancel()) {
                    return;
                }
                // A library panic fails this play; don't silently launch another
                // detached decoder through the song/video fallback.
                let fallback = fallback.filter(|_| !e.is::<DecoderPanicked>());
                match fallback {
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
                }
            }
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
        self.start_stream(video_id, found, prefetch)
    }

    /// The saved download, or a stream URL to download from.
    async fn find_stream(&self, video_id: &str) -> anyhow::Result<Found> {
        let id = video_id.to_owned();
        if let Some(shared) = tokio::task::spawn_blocking(move || crate::downloads::open(&id)).await? {
            return Ok(Found::Saved(shared));
        }
        let (source, s) = self.resolve(video_id, false).await?;
        let s = if source == Source::VisionOs && s.opus {
            choose_visionos_stream(&self.http(), s, || self.resolve_from(Source::VisionOs, video_id, false)).await?
        } else { s };
        Ok(Found::Url(source, s))
    }

    fn start_stream(&self, video_id: &str, found: Found, prefetch: bool) -> anyhow::Result<Arc<Shared>> {
        let (source, s) = match found {
            Found::Saved(shared) => return Ok(shared),
            Found::Url(source, s) => (source, s),
        };
        match &s.starts {
            Some(starts) => log::info!("stream {video_id}: {} HLS segments from {source:?}", starts.len()),
            None => log::info!("stream {video_id}: {} bytes {} from {source:?}", s.size, s.kind()),
        }
        let shared = s.buffer(prefetch);
        let refresh = Some(self.refresher(source, video_id, StreamIdentity::capture(&s)?));
        let (this, buffer) = (self.clone(), shared.clone());
        self.rt.spawn(async move {
            stream::download_playback(this.http(), s.media, s.user_agent, buffer.clone(), refresh).await;
            // Fresh visionOS file URLs refused too: YouTube wants a PO token for them
            // here, past the first MiB. Its HLS segments play without one; the song
            // reopens where it stopped, and the rest of the session goes that way.
            if source == Source::VisionOs && buffer.refused() && !this.visionos_hls.swap(true, Ordering::Relaxed) {
                log::warn!("visionOS file streams are refused here past 1 MiB; playing HLS from now on");
            }
        });
        Ok(shared)
    }

    /// Recovery first tries upstream HLS where a progressive file failed. Every
    /// candidate still needs a real target decoder before the generation can publish.
    async fn open_prepared_stream(&self, video_id: &str, at: std::time::Duration, generation: u64) -> anyhow::Result<OpenedStream> {
        let failed = (!at.is_zero()).then(|| self.current.lock().unwrap().as_ref()
            .filter(|(id, shared)| id == video_id && shared.error().is_some() && !shared.feed)
            .map(|(_, shared)| (shared.opus, shared.segments.is_none()))).flatten();
        let Some((failed_opus, failed_file)) = failed else {
            return self.open_stream(video_id, false).await.map(|shared| OpenedStream { shared, decoder: None });
        };
        let mut lookups = futures_util::stream::FuturesUnordered::new();
        let hls = async {
            if failed_file {
                self.resolve_from(Source::VisionOsHls, video_id, false).await.map(|candidate| vec![candidate])
            } else { Ok(Vec::new()) }
        };
        let files = self.visionos_streams(video_id);
        lookups.push(std::pin::Pin::from(Box::new(hls) as Box<dyn std::future::Future<Output = anyhow::Result<Vec<ResolvedStream>>> + Send + '_>));
        lookups.push(std::pin::Pin::from(Box::new(files) as Box<dyn std::future::Future<Output = anyhow::Result<Vec<ResolvedStream>>> + Send + '_>));
        let collect = async {
            use futures_util::StreamExt;
            let mut candidates = Vec::new();
            let deadline = tokio::time::sleep(std::time::Duration::from_secs(6));
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    result = lookups.next() => match result {
                        Some(Ok(mut found)) => candidates.append(&mut found),
                        Some(Err(_)) => {},
                        None => break,
                    },
                    _ = &mut deadline => break,
                    _ = self.wait_superseded(generation) => anyhow::bail!("audio recovery superseded"),
                }
            }
            anyhow::Ok(candidates)
        };
        let candidates = collect.await?;
        drop(lookups); // Cancel a losing resolver before starting actual decoding.
        if candidates.is_empty() && self.api.account().is_some() {
            // Age/Premium-only media retain the upstream signed-in resolver path.
            return self.open_stream(video_id, false).await.map(|shared| OpenedStream { shared, decoder: None });
        }
        anyhow::ensure!(!candidates.is_empty(), "no recovery audio candidates resolved");
        let opened = prepare_recovery_candidates(self.http(), candidates, failed_opus, at, &self.audio.status.wanted_gen, generation,
            |chosen| {
                let source = if chosen.starts.is_some() { Source::VisionOsHls } else { Source::VisionOs };
                Ok(Some(self.refresher(source, video_id, StreamIdentity::capture(chosen)?)))
            }).await?;
        if opened.shared.segments.is_some() {
            self.visionos_hls.store(true, Ordering::Relaxed);
        }
        Ok(opened)
    }

    async fn wait_superseded(&self, generation: u64) {
        while self.audio.status.wanted_gen.load(Ordering::Relaxed) == generation {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    /// Renew exactly the buffered representation; never splice another quality or
    /// another HLS segment layout into an existing decoder's Shared.
    fn refresher(&self, source: Source, video_id: &str, identity: StreamIdentity) -> stream::Refresh {
        let (this, id) = (self.clone(), video_id.to_owned());
        Arc::new(move || {
            let (this, id, identity) = (this.clone(), id.clone(), identity.clone());
            Box::pin(async move {
                let s = if source == Source::VisionOs && let Some(itag) = identity.itag() {
                    this.visionos_streams(&id).await?.into_iter().find(|s| stream_itag(s) == Some(itag))
                        .ok_or_else(|| anyhow::anyhow!("the buffered audio representation is no longer offered"))?
                } else { this.resolve_from(source, &id, identity.opus).await? };
                identity.validate(&s)?;
                Ok(s.media)
            })
        })
    }

    /// Download a song into the downloads folder, at the best quality the account gets.
    /// `behind_player`: each part waits while the playing song still needs some of its own.
    /// Returns the file, and whether this saved it (false: it was there already).
    async fn download(&self, t: &Track, for_video: Option<String>, behind_player: bool) -> anyhow::Result<(PathBuf, bool)> {
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
        let identity = StreamIdentity::capture(&s)?;
        log::info!("download {}: {} bytes from {source:?}", t.id, s.size);
        anyhow::ensure!(s.starts.is_none(), "downloads need the file stream");
        // Saved as a tagged .m4a, which a feed's MP3 isn't.
        anyhow::ensure!(!matches!(s.media, stream::Media::Feed(_)), "episodes of podcasts added by RSS can't be downloaded");
        let shared = s.buffer(false);
        let ahead: Option<stream::Ahead> = behind_player.then(|| {
            let current = self.current.clone();
            Arc::new(move || current.lock().unwrap().as_ref().map(|(_, s)| s.clone())) as stream::Ahead
        });
        stream::download_behind(self.http(), s.media, s.user_agent, shared.clone(), Some(self.refresher(source, &t.id, identity)), ahead).await;
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
        let path = tokio::task::spawn_blocking(move || crate::downloads::save(&t, &audio, duration_ms, loudness, cover, for_video)).await??;
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
            let _ = self.tx.send(Resp::Error(format!("\u{201c}{title}\u{201d} has no songs to download")));
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
        let mut q = self.batch.lock().unwrap();
        let (key, queued) = q.add(key, title, listed.len(), todo);
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
            let next = self.batch.lock().unwrap().next();
            let Some(t) = next else { return };
            // On a task of its own, so a panic in it fails this song, not the whole queue.
            let (this, song) = (self.clone(), t.clone());
            let result = tokio::spawn(async move { this.download_listed(&song).await }).await.unwrap_or_else(|e| Err(anyhow::anyhow!("{e}")));
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
    async fn download_listed(&self, t: &Track) -> anyhow::Result<bool> {
        let (song, for_video) = match t.music_video {
            true => (self.song_version(&t.id, Some(t), PREFETCH_SONG_WAIT).await.ok_or_else(|| anyhow::anyhow!("no song version of this music video"))?, Some(t.id.clone())),
            false => (t.clone(), None),
        };
        // Its song is downloaded already: the video plays that file too.
        if reuse_download(&song.id, for_video.clone()).await.is_some() {
            return Ok(false);
        }
        if !self.download(&song, for_video, true).await?.1 {
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
                // Should visionOS ask to sign in for an episode of a podcast added by RSS
                // (it says those aren't available, below), the web player has its file.
                if let Some(s) = self.rss_episode(&session, video_id).await? {
                    return Ok((Source::Feed, s));
                }
                Ok((Source::YtDlp, self.resolve_from(Source::YtDlp, video_id, false).await?))
            }
            // An episode of a podcast added by RSS: YouTube doesn't have its audio, and
            // visionOS answers as for a video that's gone. Only the signed-in web player
            // tells the two apart (Premium or not), and plays the episode from its feed.
            Err(e) if not_available(&e) => match self.api.account() {
                Some(session) => match self.rss_episode(&session, video_id).await? {
                    Some(s) => Ok((Source::Feed, s)),
                    None => Err(e),
                },
                None => Err(e),
            },
            Err(e) => Err(e),
        }
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
            itag: Some(s.itag),
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
                ResolvedStream { itag: Some(234), media: stream::Media::Segments(urls), starts: Some(starts), size: 0, duration_ms: s.duration_ms, user_agent: innertube::UA.to_owned(), loudness_db: s.loudness_db, opus: false }
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
            Source::YtDlp => {
                let session = self.api.account().ok_or_else(|| anyhow::anyhow!("sign in to play this song"))?;
                let s = ytdlp::resolve(&self.http(), &self.data_dir, &session.cookie, video_id).await?;
                // Cookies Google replaced while yt-dlp used the session.
                self.api.keep(&session, s.replaced.iter().map(String::as_str));
                // yt-dlp's JSON doesn't carry YouTube's loudness figure.
                ResolvedStream { itag: None, media: stream::Media::File(s.url), starts: None, size: s.size, duration_ms: s.duration_ms, user_agent: s.user_agent, loudness_db: None, opus: false }
            }
        })
    }

    /// An RSS podcast episode's file, fetched from where its redirects (trackers, mostly)
    /// end, so they aren't followed again for every range.
    async fn feed_stream(&self, feed: webmusic::Feed) -> anyhow::Result<ResolvedStream> {
        let (url, size) = stream::probe_feed(&self.http(), &feed.url, webmusic::UA).await?;
        log::info!("podcast feed file on {}", reqwest::Url::parse(&url).ok().as_ref().and_then(reqwest::Url::host_str).unwrap_or("?"));
        Ok(ResolvedStream {
            itag: None,
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

    async fn visionos_streams(&self, video_id: &str) -> anyhow::Result<Vec<ResolvedStream>> {
        let vd = self.api.visitor_data(false).await?;
        let streams = match innertube::audio_streams(&self.http(), video_id, &vd, true).await {
            Ok(streams) => streams,
            Err(e) if needs_account(&e) => return Err(e),
            Err(_) => {
                let vd = self.api.visitor_data(true).await?;
                innertube::audio_streams(&self.http(), video_id, &vd, true).await?
            }
        };
        Ok(streams.into_iter().filter(|s| matches!(s.itag, 139..=140 | 249..=251)).map(|s| ResolvedStream {
            itag: Some(s.itag),
            media: stream::Media::File(s.url), starts: None, size: s.size, duration_ms: s.duration_ms,
            user_agent: innertube::UA.to_owned(), loudness_db: s.loudness_db, opus: s.opus,
        }).collect())
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
    /// An RSS podcast episode's file, which the web player finds without Premium.
    Feed,
    YtDlp,
}

enum Found {
    Saved(Arc<Shared>),
    Url(Source, ResolvedStream),
}

#[derive(Clone)]
struct ResolvedStream {
    itag: Option<u64>,
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
    fn file_url(&self) -> anyhow::Result<&str> {
        match &self.media {
            stream::Media::File(url) => Ok(url),
            _ => anyhow::bail!("this representation is not a progressive file"),
        }
    }

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

struct OpenedStream {
    shared: Arc<Shared>,
    decoder: Option<crate::audio::TrackDecoder>,
}

/// Dropping a timed-out/superseded preparation wakes its blocking decoder and
/// stops its download. Successful handoff explicitly disarms this guard.
struct RecoveryBuffer(Option<Arc<Shared>>);

impl Drop for RecoveryBuffer {
    fn drop(&mut self) {
        if let Some(shared) = self.0.take() {
            shared.cancel();
        }
    }
}

async fn prepare_recovery_candidates(
    http: reqwest::Client, mut candidates: Vec<ResolvedStream>, failed_opus: bool, at: std::time::Duration,
    wanted: &std::sync::atomic::AtomicU64, generation: u64,
    refresh: impl Fn(&ResolvedStream) -> anyhow::Result<Option<stream::Refresh>> + Send + Sync,
) -> anyhow::Result<OpenedStream> {
    use std::time::Duration;
    anyhow::ensure!(wanted.load(Ordering::Relaxed) == generation, "audio recovery superseded");
    let preparation = async {
        for _ in 0..3 {
            let chosen = choose_recovery_stream(&http, candidates.clone(), failed_opus, at).await?;
            let renewal = refresh(&chosen)?;
            let itag = stream_itag(&chosen);
            candidates.retain(|candidate| stream_itag(candidate) != itag);
            let shared = chosen.buffer(false);
            let mut guard = RecoveryBuffer(Some(shared.clone()));
            tokio::spawn(stream::download_playback(http.clone(), chosen.media, chosen.user_agent, shared.clone(), renewal));
            let decode_shared = shared.clone();
            let decoder = tokio::task::spawn_blocking(move || open_decoder_guarded(|| {
                let mut decoder = stream::decoder_at(decode_shared, at)?;
                decoder.prime_frame()?;
                Ok(decoder)
            }));
            match tokio::time::timeout(Duration::from_secs(3), decoder).await {
                Ok(Ok(Err(error))) if error.is::<DecoderPanicked>() => return Err(error),
                Ok(Ok(Ok(decoder))) => {
                    anyhow::ensure!(wanted.load(Ordering::Relaxed) == generation, "audio recovery superseded");
                    guard.0 = None;
                    log::info!("recovery: prepared itag={} at {}ms", itag.unwrap_or(0), at.as_millis());
                    return Ok(OpenedStream { shared, decoder: Some(decoder) });
                }
                _ => log::warn!("recovery: itag={} could not decode the target; trying another representation", itag.unwrap_or(0)),
            }
        }
        anyhow::bail!("no audio representation could decode the recovery position")
    };
    tokio::pin!(preparation);
    let deadline = tokio::time::sleep(Duration::from_secs(8));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            result = &mut preparation => return result,
            _ = &mut deadline => anyhow::bail!("audio recovery deadline exceeded"),
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                anyhow::ensure!(wanted.load(Ordering::Relaxed) == generation, "audio recovery superseded");
            }
        }
    }
}

/// Probe candidates before Shared exists; bytes from different formats never mix.
async fn probe_visionos_stream(http: &reqwest::Client, stream: &ResolvedStream) -> anyhow::Result<()> {
    anyhow::ensure!(stream.starts.is_none(), "progressive probe requires a file stream");
    anyhow::ensure!(stream.size > 0, "empty audio stream");
    let expect = stream.size.min(1024) as usize;
    let mut url = url::Url::parse(stream.file_url()?).map_err(|_| anyhow::anyhow!("invalid audio stream address"))?;
    url.query_pairs_mut().append_pair("range", &format!("0-{}", expect - 1));
    let mut response = http.get(url).header("User-Agent", &stream.user_agent).send().await
        .and_then(reqwest::Response::error_for_status).map_err(reqwest::Error::without_url)?;
    let mut received = 0;
    while received < expect {
        let chunk = response.chunk().await.map_err(reqwest::Error::without_url)?
            .ok_or_else(|| anyhow::anyhow!("audio probe ended early"))?;
        received += chunk.len();
        anyhow::ensure!(received <= expect, "audio probe returned an unexpected range");
    }
    // Exact bytes suffice: a CDN may withhold EOF after delivering the range.
    Ok(())
}

/// Healthy Opus avoids another player request. Slow Opus races a bounded AAC
/// resolve plus exact body probe; merely resolving an AAC URL cannot win.
async fn choose_visionos_stream<F, Fut>(http: &reqwest::Client, first: ResolvedStream, resolve_aac: F) -> anyhow::Result<ResolvedStream>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<ResolvedStream>>,
{
    use std::time::Duration;
    if !first.opus {
        return Ok(first);
    }
    let opus = tokio::time::timeout(Duration::from_secs(3), probe_visionos_stream(http, &first));
    tokio::pin!(opus);
    let opus_failed = match tokio::time::timeout(Duration::from_millis(300), &mut opus).await {
        Ok(Ok(Ok(()))) => return Ok(first.clone()),
        Ok(_) => true,
        Err(_) => false,
    };
    let fallback = std::sync::Mutex::new(None);
    let aac = async {
        let candidate = resolve_aac().await?;
        anyhow::ensure!(!candidate.opus, "AAC fallback returned an unexpected representation");
        *fallback.lock().unwrap() = Some(candidate.clone());
        probe_visionos_stream(http, &candidate).await?;
        Ok::<_, anyhow::Error>(candidate)
    };
    // Resolution and probe share this budget; a stuck resolver cannot extend it.
    let aac = tokio::time::timeout(Duration::from_secs(2), aac);
    tokio::pin!(aac);
    let unverified_aac = || {
        let candidate = fallback.lock().unwrap().take()
            .ok_or_else(|| anyhow::anyhow!("Opus audio probe failed and no AAC stream resolved"))?;
        log::info!("visionOS audio probes unavailable; leaving unverified AAC to the bounded playback loader");
        Ok(candidate)
    };
    if opus_failed {
        return match aac.await { Ok(Ok(stream)) => Ok(stream), _ => unverified_aac() };
    }
    tokio::select! {
        result = &mut opus => match result {
            Ok(Ok(())) => Ok(first.clone()),
            _ => match aac.await { Ok(Ok(stream)) => Ok(stream), _ => unverified_aac() },
        },
        result = &mut aac => match result {
            Ok(Ok(stream)) => Ok(stream),
            _ => match opus.await { Ok(Ok(())) => Ok(first.clone()), _ => unverified_aac() },
        },
    }
}

fn stream_itag(stream: &ResolvedStream) -> Option<u64> {
    if stream.itag.is_some() { return stream.itag; }
    url::Url::parse(stream.file_url().ok()?).ok()?.query_pairs()
        .find(|(key, _)| key == "itag").and_then(|(_, value)| value.parse().ok())
}

/// Recovery checks both the header and a small region near the stopped time.
/// The byte estimate only rejects obvious CDN failures: decoder_at plus the
/// retained target PCM frame still verify the actual container position.
async fn probe_recovery_stream(http: &reqwest::Client, stream: &ResolvedStream, at: std::time::Duration) -> anyhow::Result<()> {
    // HLS has no progressive byte ranges. Its real decoder/target frame below
    // validates segment access; this shortlisting step never publishes readiness.
    if stream.starts.is_some() { return Ok(()); }
    probe_visionos_stream(http, stream).await?;
    if at.is_zero() || stream.duration_ms == 0 {
        return Ok(());
    }
    let start = ((stream.size as u128 * at.as_millis().min(stream.duration_ms as u128)) / stream.duration_ms as u128) as u64;
    let start = start.min(stream.size.saturating_sub(1));
    let expect = (stream.size - start).min(16 * 1024) as usize;
    let mut url = url::Url::parse(stream.file_url()?).map_err(|_| anyhow::anyhow!("invalid recovery audio address"))?;
    url.query_pairs_mut().append_pair("range", &format!("{start}-{}", start + expect as u64 - 1));
    let mut response = http.get(url).header("User-Agent", &stream.user_agent).send().await
        .and_then(reqwest::Response::error_for_status).map_err(reqwest::Error::without_url)?;
    let mut received = 0;
    while received < expect {
        let bytes = response.chunk().await.map_err(reqwest::Error::without_url)?
            .ok_or_else(|| anyhow::anyhow!("recovery audio probe ended early"))?;
        received += bytes.len();
        anyhow::ensure!(received <= expect, "recovery probe returned an unexpected range");
    }
    Ok(())
}

/// Prefer another codec after a terminal failure, including the lower bitrate
/// Opus variants. A freshly resolved former codec can still recover, but only
/// after its header and target-region bodies pass. No unverified fallback wins.
async fn choose_recovery_stream(http: &reqwest::Client, mut candidates: Vec<ResolvedStream>, failed_opus: bool, at: std::time::Duration) -> anyhow::Result<ResolvedStream> {
    use futures_util::{StreamExt, stream::FuturesUnordered};
    use std::time::Duration;
    candidates.sort_by_key(|candidate| (candidate.starts.is_none(), candidate.opus == failed_opus));
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|candidate| seen.insert(stream_itag(candidate)));
    let mut probes = FuturesUnordered::new();
    for (rank, candidate) in candidates.into_iter().take(5).enumerate() {
        probes.push(async move {
            // Give the preferred quality a small head start without serially
            // waiting seconds on every unavailable representation.
            tokio::time::sleep(Duration::from_millis(rank as u64 * 100)).await;
            tokio::time::timeout(Duration::from_secs(3), probe_recovery_stream(http, &candidate, at)).await
                .map_err(|_| anyhow::anyhow!("recovery representation probe timed out"))??;
            anyhow::Ok(candidate)
        });
    }
    while let Some(result) = probes.next().await {
        if let Ok(candidate) = result {
            // Dropping FuturesUnordered cancels the other probes immediately.
            return Ok(candidate);
        }
    }
    anyhow::bail!("no audio representation could serve the recovery position")
}

/// Byte representation identity, excluding signatures and routing parameters.
/// Deliberately not Debug: query values must never enter diagnostic output.
#[derive(Clone)]
struct StreamIdentity {
    itag: Option<u64>,
    opus: bool,
    size: u64,
    duration_ms: u64,
    query: std::collections::BTreeMap<String, Vec<String>>,
    kind: u8,
    starts: Option<Vec<f64>>,
}

impl StreamIdentity {
    fn itag(&self) -> Option<u64> {
        if self.itag.is_some() { return self.itag; }
        let values = self.query.get("itag")?;
        (values.len() == 1).then(|| values[0].parse().ok()).flatten()
    }
    fn capture(stream: &ResolvedStream) -> anyhow::Result<Self> {
        let mut query = std::collections::BTreeMap::<String, Vec<String>>::new();
        let kind = match &stream.media {
            stream::Media::File(address) => {
                let url = url::Url::parse(address).map_err(|_| anyhow::anyhow!("the stream changed representation"))?;
                for (key, value) in url.query_pairs().filter(|(key, _)| matches!(key.as_ref(), "itag" | "lmt" | "clen" | "mime" | "dur")) {
                    query.entry(key.into_owned()).or_default().push(value.into_owned());
                }
                0
            }
            stream::Media::Feed(_) => 1,
            stream::Media::Segments(urls) => {
                anyhow::ensure!(stream.starts.as_ref().is_some_and(|starts| starts.len() == urls.len()), "invalid HLS segment layout");
                2
            }
        };
        for values in query.values_mut() { values.sort(); }
        Ok(Self { itag: stream_itag(stream), opus: stream.opus, size: stream.size, duration_ms: stream.duration_ms, query,
            kind, starts: stream.starts.clone() })
    }
    fn validate(&self, stream: &ResolvedStream) -> anyhow::Result<()> {
        let next = Self::capture(stream)?;
        anyhow::ensure!(
            self.kind == next.kind && self.starts == next.starts && self.itag == next.itag && self.opus == next.opus && self.size == next.size && self.duration_ms == next.duration_ms
                && self.query.iter().all(|(key, value)| next.query.get(key) == Some(value)),
            "the stream changed representation"
        );
        Ok(())
    }
}

/// The anonymous visionOS player refused because the song needs an account:
/// age-restricted (LOGIN_REQUIRED) or Music Premium only.
fn needs_account(e: &anyhow::Error) -> bool {
    let msg = e.to_string();
    msg.contains("LOGIN_REQUIRED") || msg.contains("Premium")
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
        assert_eq!(q.add(Some("A".into()), "A".into(), 3, songs(&["1", "2"])), ("A".to_owned(), 2));
        assert_eq!(q.add(Some("B".into()), "B".into(), 2, songs(&["2", "3"])), ("B".to_owned(), 2));
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
        q.add(Some("A".into()), "A".into(), 2, songs(&["1", "2"]));
        assert_eq!(q.add(Some("A".into()), "A".into(), 3, songs(&["1", "2", "3"])).1, 1);
        assert_eq!(waiting(&q), ["1", "2", "3"]);
        assert_eq!(run(&q, "A").total, 3);
    }

    #[test]
    fn queued_again_a_failed_song_is_retried_not_counted_twice() {
        let mut q = DownloadQueue::default();
        q.add(Some("A".into()), "A".into(), 3, songs(&["1", "2", "3"]));
        let one = q.next().unwrap();
        q.finish(&one, &Err::<bool, _>(()));
        assert_eq!((run(&q, "A").done, run(&q, "A").failed.clone()), (1, vec!["1".to_owned()]));

        // Queued again from the card's menu while 2 and 3 are still to go: 1 failed, so
        // it isn't downloaded and comes back; 2 and 3 are on their way already.
        assert_eq!(q.add(Some("A".into()), "A".into(), 3, songs(&["1", "2", "3"])).1, 1);
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
        q.add(Some("A".into()), "A".into(), 3, songs(&["1", "2", "3"]));
        q.add(Some("B".into()), "B".into(), 1, songs(&["3"]));
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
        assert_eq!(q.add(None, String::new(), 1, songs(&["1"])).0, "selection-1");
        assert_eq!(q.add(None, String::new(), 2, songs(&["2", "3"])).0, "selection-2");
        assert_eq!(q.more_workers(), DOWNLOADS_AT_ONCE.min(3));
        assert_eq!(q.more_workers(), 0);
        while q.next().is_some() {}
        assert_eq!(q.workers, DOWNLOADS_AT_ONCE.min(3) - 1);
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
        assert!(!needs_account(&refused("ERROR", "This video is not available")));
        // Only the player's own answer, not an error that reads like one.
        assert!(!not_available(&anyhow::anyhow!("ERROR: This video is not available")));
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
        println!("web player: {} ms (in {:?})", feed.duration_ms, t.elapsed());
        assert!(feed.duration_ms > 0);
        let (_file, size) = stream::probe_feed(&http, &feed.url, webmusic::UA).await.unwrap();
        println!("feed file: {size} bytes");
        assert!(size > 0);
    }
}

#[cfg(test)]
mod visionos_fallback_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    fn resolved(url: String, opus: bool, size: u64) -> ResolvedStream {
        ResolvedStream { itag: None, media: stream::Media::File(url), starts: None, size, duration_ms: 1000, user_agent: "probe-test".into(), loudness_db: None, opus }
    }

    fn probe_server(status: u16) -> (String, std::thread::JoinHandle<()>) {
        delayed_probe_server(status, Duration::ZERO, 1024)
    }

    fn delayed_probe_server(status: u16, delay: Duration, bytes: usize) -> (String, std::thread::JoinHandle<()>) {
        probe_response_server(status, delay, bytes, false)
    }

    fn probe_response_server(status: u16, delay: Duration, bytes: usize, hold_eof: bool) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/audio", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let worker = std::thread::spawn(move || {
            let start = std::time::Instant::now();
            let (mut socket, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && start.elapsed() < Duration::from_secs(4) => std::thread::sleep(Duration::from_millis(5)),
                    Err(e) => panic!("probe mock did not receive a request: {e}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
            let mut request = [0; 4096];
            let n = socket.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..n]).contains("range=0-1023"));
            std::thread::sleep(delay);
            let body = if status == 200 { vec![1; bytes] } else { vec![] };
            // A winning alternate may drop the slow connection before its response.
            if hold_eof {
                let header = format!("HTTP/1.1 {status} Result\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n", body.len());
                let _ = socket.write_all(header.as_bytes())
                    .and_then(|_| socket.write_all(&body))
                    .and_then(|_| socket.write_all(b"\r\n"));
                // Deliberately omit the final zero chunk, but keep the mock finite.
                std::thread::sleep(Duration::from_millis(800));
            } else {
                let header = format!("HTTP/1.1 {status} Result\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = socket.write_all(header.as_bytes()).and_then(|_| socket.write_all(&body));
            }
        });
        (url, worker)
    }

    #[tokio::test]
    async fn unavailable_opus_selects_aac_before_creating_a_fresh_buffer() {
        let (url, server) = probe_server(503);
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let old = Shared::new(1024, 1000, None, true, false);
        let (aac_url, aac_server) = probe_server(503);
        let expected_url = aac_url.clone();
        let chosen = choose_visionos_stream(&http, resolved(url, true, old.total), || async {
            Ok(resolved(aac_url, false, 2048))
        }).await.unwrap();
        // Both body probes failed; the resolved AAC remains an unverified fallback.
        assert!(!chosen.opus);
        assert_eq!(chosen.file_url().unwrap(), expected_url);
        aac_server.join().unwrap();
        let fresh = Shared::new(chosen.size, chosen.duration_ms, chosen.loudness_db, chosen.opus, false);
        assert!(!Arc::ptr_eq(&old, &fresh));
        assert_eq!(fresh.total, 2048);
        assert!(!fresh.opus);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn healthy_opus_does_not_resolve_aac() {
        let (url, server) = probe_server(200);
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let called = AtomicBool::new(false);
        let chosen = choose_visionos_stream(&http, resolved(url, true, 1024), || async {
            called.store(true, Ordering::Relaxed);
            anyhow::bail!("healthy Opus must not resolve AAC")
        }).await.unwrap();
        assert!(chosen.opus);
        assert!(!called.load(Ordering::Relaxed));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn slow_opus_hedges_aac_before_the_opus_timeout() {
        let (opus_url, opus_server) = delayed_probe_server(200, Duration::from_secs(2), 1024);
        let (aac_url, aac_server) = probe_server(200);
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let started = std::time::Instant::now();
        let chosen = choose_visionos_stream(&http, resolved(opus_url, true, 1024), || async {
            Ok(resolved(aac_url, false, 1024))
        }).await.unwrap();
        assert!(!chosen.opus);
        assert!(started.elapsed() < Duration::from_millis(1500));
        aac_server.join().unwrap();
        opus_server.join().unwrap();
    }

    #[tokio::test]
    async fn truncated_aac_cannot_mask_slow_healthy_opus() {
        let (opus_url, opus_server) = delayed_probe_server(200, Duration::from_millis(650), 1024);
        let (aac_url, aac_server) = delayed_probe_server(200, Duration::ZERO, 512);
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let called = AtomicBool::new(false);
        let chosen = choose_visionos_stream(&http, resolved(opus_url, true, 1024), || async {
            called.store(true, Ordering::Relaxed);
            Ok(resolved(aac_url, false, 1024))
        }).await.unwrap();
        assert!(called.load(Ordering::Relaxed));
        assert!(chosen.opus);
        aac_server.join().unwrap();
        opus_server.join().unwrap();
    }

    #[tokio::test]
    async fn failed_opus_and_failed_aac_resolution_return_a_safe_error() {
        let (opus_url, opus_server) = probe_server(503);
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let result = choose_visionos_stream(&http, resolved(opus_url, true, 1024), || async {
            anyhow::bail!("resolver error containing a private address must stay private")
        }).await;
        let message = result.err().expect("neither candidate was available").to_string();
        assert!(message.contains("no AAC stream resolved"));
        assert!(!message.contains("private address"));
        assert!(!message.contains("http://"));
        opus_server.join().unwrap();
    }

    #[tokio::test]
    async fn healthy_opus_drops_the_losing_hung_aac_resolver() {
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) { self.0.store(true, Ordering::Relaxed); }
        }
        let (opus_url, opus_server) = delayed_probe_server(200, Duration::from_millis(650), 1024);
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let dropped = Arc::new(AtomicBool::new(false));
        let resolver_dropped = dropped.clone();
        let chosen = choose_visionos_stream(&http, resolved(opus_url, true, 1024), || async move {
            let _guard = DropFlag(resolver_dropped);
            std::future::pending::<anyhow::Result<ResolvedStream>>().await
        }).await.unwrap();
        assert!(chosen.opus);
        assert!(dropped.load(Ordering::Relaxed));
        opus_server.join().unwrap();
    }

    #[tokio::test]
    async fn exact_probe_prefix_is_available_without_waiting_for_eof() {
        let (url, server) = probe_response_server(200, Duration::ZERO, 1024, true);
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let called = AtomicBool::new(false);
        let started = std::time::Instant::now();
        let chosen = choose_visionos_stream(&http, resolved(url, true, 1024), || async {
            called.store(true, Ordering::Relaxed);
            anyhow::bail!("an exact availability prefix must not trigger AAC")
        }).await.unwrap();
        assert!(chosen.opus);
        assert!(!called.load(Ordering::Relaxed));
        assert!(started.elapsed() < Duration::from_millis(500));
        // Availability only: Shared's full block downloader remains responsible
        // for validating the eventual audio representation and complete download.
        server.join().unwrap();
    }

    /// Explicit network acceptance for the reported track; no Api, profile, files,
    /// credentials, hardware output, or account history are involved.
    #[tokio::test]
    #[ignore = "live anonymous service test for the reported playback stall"]
    async fn reported_track_visionos_fallback_decodes_in_memory() -> anyhow::Result<()> {
        use rodio::Source;
        let id = std::env::var("YTM_REPORTED_TRACK").unwrap_or_else(|_| "qYxLQ1YNu-g".to_owned());
        let http = crate::proxy::http();
        let response = http.get("https://music.youtube.com/").header("User-Agent", innertube::UA)
            .timeout(Duration::from_secs(20))
            .send().await.and_then(reqwest::Response::error_for_status).map_err(reqwest::Error::without_url)?;
        let html = response.text().await.map_err(reqwest::Error::without_url)?;
        let visitor = html.split_once("\"VISITOR_DATA\":\"").and_then(|(_, tail)| tail.split('"').next()).map(str::to_owned)
            .filter(|s| !s.is_empty()).ok_or_else(|| anyhow::anyhow!("no anonymous visitor data on page"))?;
        let convert = |s: innertube::Stream| ResolvedStream {
            itag: Some(s.itag),
            media: stream::Media::File(s.url), starts: None, size: s.size, duration_ms: s.duration_ms, user_agent: innertube::UA.into(), loudness_db: s.loudness_db, opus: s.opus,
        };
        let first = convert(innertube::audio_stream(&http, &id, &visitor, true).await?);
        let chosen = choose_visionos_stream(&http, first, || async {
            innertube::audio_stream(&http, &id, &visitor, false).await.map(convert)
        }).await?;
        let aac = convert(innertube::audio_stream(&http, &id, &visitor, false).await?);
        anyhow::ensure!(!aac.opus, "AAC check returned an unexpected representation");
        // Availability can change between runs. Decode the format selected by
        // the production helper, plus a separately resolved AAC safety path.
        for chosen in [chosen, aac] {
            let shared = Shared::new(chosen.size, chosen.duration_ms, chosen.loudness_db, chosen.opus, false);
            tokio::time::timeout(Duration::from_secs(90), stream::download(http.clone(), chosen.media, chosen.user_agent, shared.clone(), None)).await
                .map_err(|_| anyhow::anyhow!("reported track download exceeded test deadline"))?;
            anyhow::ensure!(shared.error().is_none() && shared.is_complete(), "reported track download failed");
            let mut decoder = stream::decoder(shared, true)?;
            let channels = decoder.channels().get() as usize;
            let rate = decoder.sample_rate().get() as f64;
            let mut samples = 0usize;
            let max_samples = ((chosen.duration_ms as f64 / 1000.0 + 5.0) * rate * channels as f64) as usize;
            for sample in decoder.by_ref().take(max_samples + 1) {
                anyhow::ensure!(sample.is_finite(), "decoder returned a non-finite sample");
                samples += 1;
            }
            anyhow::ensure!(samples > 0 && samples <= max_samples && samples.is_multiple_of(channels), "decoder did not produce a finite sequence of complete frames");
            let seconds = samples as f64 / (rate * channels as f64);
            anyhow::ensure!((seconds - chosen.duration_ms as f64 / 1000.0).abs() < 1.0, "decoded duration differs from stream metadata");
            println!("codec={}, rate={rate}, frames={}, seconds={seconds:.3}", if chosen.opus { "Opus" } else { "AAC" }, samples / channels);
        }
        Ok(())
    }
}

#[cfg(test)]
mod stream_identity_tests {
    use super::*;

    fn stream(query: &str) -> ResolvedStream {
        ResolvedStream {
            itag: None,
            media: stream::Media::File(format!("https://media.invalid/audio?{query}")), starts: None,
            size: 1024,
            duration_ms: 1000,
            user_agent: "test".into(),
            loudness_db: None,
            opus: true,
        }
    }

    #[test]
    fn same_rep_new_signature_allowed() {
        let old = stream("itag=251&lmt=123&clen=1024&mime=audio%2Fwebm&dur=1.0&sig=old&expire=1&ip=old&n=old&id=old");
        let identity = StreamIdentity::capture(&old).unwrap();
        let mut new = stream("dur=1.0&mime=audio%2Fwebm&clen=1024&lmt=123&itag=251&sig=new&expire=2&ip=new&n=new&id=new");
        new.media = stream::Media::File(new.file_url().unwrap().replace("media.invalid", "another-media.invalid"));
        assert!(identity.validate(&new).is_ok());
    }

    #[test]
    fn same_codec_changed_size_rejected() {
        let old = stream("itag=251&lmt=123");
        let identity = StreamIdentity::capture(&old).unwrap();
        let mut new = stream("itag=251&lmt=123");
        new.size += 1;
        assert_eq!(identity.validate(&new).unwrap_err().to_string(), "the stream changed representation");
        new.size = old.size;
        new.duration_ms += 1;
        assert!(identity.validate(&new).is_err());
        new.duration_ms = old.duration_ms;
        new.opus = false;
        assert!(identity.validate(&new).is_err());
    }

    #[test]
    fn itag_or_lmt_changed_or_missing_rejected() {
        let old = stream("itag=251&lmt=123&clen=1024&mime=audio%2Fwebm&dur=1.0");
        let identity = StreamIdentity::capture(&old).unwrap();
        for query in [
            "itag=140&lmt=123&clen=1024&mime=audio%2Fwebm&dur=1.0",
            "itag=251&lmt=124&clen=1024&mime=audio%2Fwebm&dur=1.0",
            "itag=251&clen=1024&mime=audio%2Fwebm&dur=1.0",
            "lmt=123&clen=1024&mime=audio%2Fwebm&dur=1.0",
            "itag=251&lmt=123&clen=2048&mime=audio%2Fwebm&dur=1.0",
            "itag=251&lmt=123&clen=1024&mime=audio%2Fmp4&dur=1.0",
            "itag=251&lmt=123&clen=1024&mime=audio%2Fwebm&dur=2.0",
        ] {
            assert_eq!(identity.validate(&stream(query)).unwrap_err().to_string(), "the stream changed representation");
        }
    }
}

#[cfg(test)]
mod recovery_stream_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::time::Duration;

    struct Server {
        address: std::net::SocketAddr,
        stop: Arc<AtomicBool>,
        target_requests: Arc<AtomicUsize>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl Server {
        fn new(target_ok: bool) -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            listener.set_nonblocking(true).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let target_requests = Arc::new(AtomicUsize::new(0));
            let (stopped, requests) = (stop.clone(), target_requests.clone());
            let worker = std::thread::spawn(move || {
                while !stopped.load(Ordering::Relaxed) {
                    let (mut socket, _) = match listener.accept() {
                        Ok(connection) => connection,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                            continue;
                        }
                        Err(e) => panic!("mock accept failed: {e}"),
                    };
                    socket.set_nonblocking(false).unwrap();
                    socket.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
                    let mut bytes = Vec::new();
                    loop {
                        let mut buffer = [0; 1024];
                        let count = socket.read(&mut buffer).unwrap();
                        if count == 0 { break; }
                        bytes.extend_from_slice(&buffer[..count]);
                        if bytes.windows(4).any(|w| w == b"\r\n\r\n") { break; }
                        assert!(bytes.len() < 8192);
                    }
                    let request = String::from_utf8(bytes).unwrap();
                    let target = request.split_whitespace().nth(1).unwrap();
                    let url = url::Url::parse(&format!("http://localhost{target}")).unwrap();
                    let range = url.query_pairs().find(|(key, _)| key == "range").unwrap().1.into_owned();
                    let (a, b) = range.split_once('-').unwrap();
                    let (a, b) = (a.parse::<usize>().unwrap(), b.parse::<usize>().unwrap());
                    let is_target = a > 0;
                    if is_target { requests.fetch_add(1, Ordering::Relaxed); }
                    let status = if is_target && !target_ok { 503 } else { 200 };
                    let body = if status == 200 { vec![1; b - a + 1] } else { vec![] };
                    let header = format!("HTTP/1.1 {status} Result\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                    let _ = socket.write_all(header.as_bytes()).and_then(|_| socket.write_all(&body));
                }
            });
            Self { address, stop, target_requests, worker: Some(worker) }
        }

        fn stream(&self, itag: u64, opus: bool) -> ResolvedStream {
            ResolvedStream { itag: Some(itag), media: stream::Media::File(format!("http://{}/audio?itag={itag}", self.address)), starts: None, size: 64 * 1024,
                duration_ms: 200_000, user_agent: "test".into(), loudness_db: None, opus }
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            self.worker.take().unwrap().join().unwrap();
        }
    }

    #[tokio::test]
    async fn healthy_header_cannot_hide_a_broken_resume_region() {
        let broken_aac = Server::new(false);
        let healthy_opus = Server::new(true);
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let chosen = choose_recovery_stream(&http, vec![broken_aac.stream(140, false), healthy_opus.stream(250, true)],
            true, Duration::from_millis(113195)).await.unwrap();
        assert_eq!(stream_itag(&chosen), Some(250));
        assert!(broken_aac.target_requests.load(Ordering::Relaxed) > 0);
        assert!(healthy_opus.target_requests.load(Ordering::Relaxed) > 0);
    }

    #[tokio::test]
    async fn failed_aac_prefers_an_available_opus_representation() {
        let aac = Server::new(true);
        let opus = Server::new(true);
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let chosen = choose_recovery_stream(&http, vec![aac.stream(140, false), opus.stream(251, true)],
            false, Duration::from_secs(113)).await.unwrap();
        assert_eq!(stream_itag(&chosen), Some(251));
    }

    #[tokio::test]
    async fn a_fresh_former_codec_can_recover_only_after_target_body_passes() {
        let server = Server::new(true);
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let chosen = choose_recovery_stream(&http, vec![server.stream(140, false)],
            false, Duration::from_secs(113)).await.unwrap();
        assert_eq!(stream_itag(&chosen), Some(140));
        assert!(server.target_requests.load(Ordering::Relaxed) > 0);
    }

    #[tokio::test]
    async fn no_unverified_fallback_when_every_target_region_fails() {
        let server = Server::new(false);
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let result = choose_recovery_stream(&http, vec![server.stream(140, false), server.stream(251, true)],
            false, Duration::from_secs(113)).await;
        assert!(result.is_err());
        assert_eq!(server.target_requests.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn renewal_keeps_the_exact_lower_bitrate_itag() {
        let stream = ResolvedStream { itag: None, media: stream::Media::File("https://media.invalid/audio?itag=250&lmt=123".into()), starts: None, size: 1024,
            duration_ms: 1000, user_agent: "test".into(), loudness_db: None, opus: true };
        let identity = StreamIdentity::capture(&stream).unwrap();
        assert_eq!(identity.itag(), Some(250));
        let changed = ResolvedStream { media: stream::Media::File("https://media.invalid/audio?itag=251&lmt=123".into()), starts: None, ..stream };
        assert!(identity.validate(&changed).is_err());
    }

    /// Distinct actual-body server: probes and the decoder download see the same
    /// bytes. It has finite socket deadlines and explicit shutdown, never media files.
    struct BodyServer {
        address: std::net::SocketAddr,
        size: usize,
        downloads: Arc<AtomicUsize>,
        download_started: Arc<tokio::sync::Notify>,
        cancelled_connection: Arc<AtomicBool>,
        worker: Option<tokio::task::JoinHandle<()>>,
    }

    impl BodyServer {
        async fn new(body: Vec<u8>, hang_download: bool) -> Self {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let size = body.len();
            let downloads = Arc::new(AtomicUsize::new(0));
            let download_started = Arc::new(tokio::sync::Notify::new());
            let cancelled_connection = Arc::new(AtomicBool::new(false));
            let count = downloads.clone();
            let started = download_started.clone();
            let cancelled = cancelled_connection.clone();
            let worker = tokio::spawn(async move {
                // Probe races can revisit a candidate; do not assume exactly 3 GETs.
                for _ in 0..32 {
                    let Ok(Ok((mut socket, _))) = tokio::time::timeout(Duration::from_secs(3), listener.accept()).await else { break; };
                    let mut request = Vec::new();
                    let read = async {
                        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                            let mut buffer = [0; 1024];
                            let n = socket.read(&mut buffer).await?;
                            if n == 0 { break; }
                            request.extend_from_slice(&buffer[..n]);
                            assert!(request.len() < 8192);
                        }
                        Ok::<_, std::io::Error>(())
                    };
                    if !matches!(tokio::time::timeout(Duration::from_secs(1), read).await, Ok(Ok(()))) { continue; }
                    let request = String::from_utf8(request).unwrap();
                    let target = request.split_whitespace().nth(1).unwrap();
                    let url = url::Url::parse(&format!("http://localhost{target}")).unwrap();
                    let range = url.query_pairs().find(|(key, _)| key == "range").unwrap().1.into_owned();
                    let (a, b) = range.split_once('-').unwrap();
                    let (a, b) = (a.parse::<usize>().unwrap(), b.parse::<usize>().unwrap());
                    assert!(a <= b && b < body.len());
                    let full_download = a == 0 && b + 1 == body.len();
                    if full_download { count.fetch_add(1, Ordering::Relaxed); }
                    let header = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", b - a + 1);
                    if socket.write_all(header.as_bytes()).await.is_err() { continue; }
                    if full_download && hang_download {
                        started.notify_one();
                        let mut byte = [0];
                        if matches!(tokio::time::timeout(Duration::from_secs(2), socket.read(&mut byte)).await, Ok(Ok(0))) {
                            cancelled.store(true, Ordering::Relaxed);
                        }
                        continue;
                    }
                    let _ = tokio::time::timeout(Duration::from_secs(1), socket.write_all(&body[a..=b])).await;
                }
            });
            Self { address, size, downloads, download_started, cancelled_connection, worker: Some(worker) }
        }

        fn stream(&self, itag: u64) -> ResolvedStream {
            ResolvedStream { itag: Some(itag), media: stream::Media::File(format!("http://{}/audio?itag={itag}", self.address)), starts: None, size: self.size as u64,
                duration_ms: 6000, user_agent: "actual-recovery-test".into(), loudness_db: None, opus: false }
        }

        async fn shutdown(mut self) {
            let worker = self.worker.take().unwrap();
            worker.abort();
            let _ = worker.await;
        }
    }

    impl Drop for BodyServer {
        fn drop(&mut self) {
            if let Some(worker) = self.worker.take() { worker.abort(); }
        }
    }

    #[tokio::test]
    async fn recovery_rejects_probe_only_aac_then_hands_off_real_target_pcm() {
        use rodio::Source;
        let fixture = include_bytes!("testdata/sine6-fragmented-aac.m4a");
        let bad = BodyServer::new(vec![0x55; fixture.len()], false).await;
        let good = BodyServer::new(fixture.to_vec(), false).await;
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let wanted = std::sync::atomic::AtomicU64::new(7);
        let at = Duration::from_secs(2);
        let mut opened = tokio::time::timeout(Duration::from_secs(5), prepare_recovery_candidates(http,
            vec![bad.stream(140), good.stream(139)], false, at, &wanted, 7, |_| Ok(None))).await.unwrap().unwrap();
        assert!(bad.downloads.load(Ordering::Relaxed) > 0, "first candidate passed both probes and reached actual decoding");
        assert!(good.downloads.load(Ordering::Relaxed) > 0);
        let mut decoder = opened.decoder.take().expect("only a prepared decoder may be handed off");
        assert_eq!(decoder.current_span_len(), Some(usize::from(decoder.channels().get())), "the target frame must remain primed");
        let channels = usize::from(decoder.channels().get());
        let rate = f64::from(decoder.sample_rate().get());
        let expected_seconds = 6.0 - at.as_secs_f64();
        // In-flight forward discard and a completed buffer's native seek can
        // legitimately use different AAC preroll. Test the actual handoff's
        // retained frame and complete remaining duration, not cross-path PCM.
        let sample_limit = ((expected_seconds + 0.5) * rate * channels as f64) as usize;
        let mut samples = 0;
        for sample in decoder.by_ref().take(sample_limit + channels) {
            assert!(sample.is_finite(), "recovered PCM must be finite");
            samples += 1;
        }
        assert!(samples > 0 && samples <= sample_limit, "unexpected recovered PCM sample count: {samples}");
        assert_eq!(samples % channels, 0, "recovered PCM must contain whole channel frames");
        let seconds = samples as f64 / (rate * channels as f64);
        assert!((seconds - expected_seconds).abs() < 0.15,
            "expected about {expected_seconds:.3}s after recovery, decoded {seconds:.3}s ({samples} samples)");
        assert!(opened.shared.error().is_none());
        opened.shared.cancel();
        bad.shutdown().await;
        good.shutdown().await;
    }

    #[tokio::test]
    async fn recovery_never_returns_opened_when_all_probe_passing_containers_fail() {
        let first = BodyServer::new(vec![0x55; 26795], false).await;
        let second = BodyServer::new(vec![0x33; 26795], false).await;
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let wanted = std::sync::atomic::AtomicU64::new(9);
        let result = tokio::time::timeout(Duration::from_secs(5), prepare_recovery_candidates(http,
            vec![first.stream(140), second.stream(139)], false, Duration::from_secs(2), &wanted, 9, |_| Ok(None))).await.unwrap();
        assert!(result.is_err(), "probe bytes must not masquerade as decoded readiness");
        assert!(first.downloads.load(Ordering::Relaxed) > 0);
        assert!(second.downloads.load(Ordering::Relaxed) > 0);
        first.shutdown().await;
        second.shutdown().await;
    }

    #[tokio::test]
    async fn superseding_recovery_cancels_its_pending_download_and_decoder() {
        let fixture = include_bytes!("testdata/sine6-fragmented-aac.m4a");
        let server = BodyServer::new(fixture.to_vec(), true).await;
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let wanted = std::sync::atomic::AtomicU64::new(11);
        let preparation = prepare_recovery_candidates(http, vec![server.stream(140)], false,
            Duration::from_secs(2), &wanted, 11, |_| Ok(None));
        tokio::pin!(preparation);
        tokio::select! {
            started = tokio::time::timeout(Duration::from_secs(2), server.download_started.notified()) => started.expect("actual download must start after probes"),
            result = &mut preparation => panic!("preparation ended before its download was exercised: {}", result.is_ok()),
        }
        assert_eq!(server.downloads.load(Ordering::Relaxed), 1);
        wanted.store(12, Ordering::Relaxed);
        let result = tokio::time::timeout(Duration::from_millis(700), &mut preparation).await.unwrap();
        assert!(result.err().expect("superseded prepare must not hand off").to_string().contains("superseded"));
        tokio::time::timeout(Duration::from_millis(700), async {
            while !server.cancelled_connection.load(Ordering::Relaxed) { tokio::time::sleep(Duration::from_millis(10)).await; }
        }).await.expect("dropping the recovery buffer must cancel the in-flight HTTP body");
        server.shutdown().await;
    }

    #[test]
    fn exact_lower_bitrate_identity_survives_a_url_without_an_itag_query() {
        let original = ResolvedStream { itag: Some(250), media: stream::Media::File("https://media.invalid/audio?lmt=123&clen=1024".into()), starts: None,
            size: 1024, duration_ms: 1000, user_agent: "test".into(), loudness_db: None, opus: true };
        let identity = StreamIdentity::capture(&original).unwrap();
        assert_eq!(identity.itag(), Some(250));
        let renewed = ResolvedStream { media: stream::Media::File("https://route.invalid/audio?clen=1024&lmt=123&expire=456".into()), starts: None, ..original.clone() };
        identity.validate(&renewed).unwrap();
        assert_eq!(stream_itag(&renewed), Some(250));
        let wrong_quality = ResolvedStream { itag: Some(251), ..renewed.clone() };
        assert!(identity.validate(&wrong_quality).is_err(), "same codec is not exact representation identity");
        let metadata_lost = ResolvedStream { itag: None, ..renewed };
        assert!(identity.validate(&metadata_lost).is_err(), "explicit itag must survive URL renewal when the URL omits it");
    }

    #[tokio::test]
    async fn hls_is_ranked_ahead_of_progressive_for_a_known_cutoff() {
        let hls = ResolvedStream { itag: Some(234), media: stream::Media::Segments(vec!["https://media.invalid/segment".into()]),
            starts: Some(vec![0.0]), size: 0, duration_ms: 6000, user_agent: "test".into(), loudness_db: None, opus: false };
        let server = Server::new(true);
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let chosen = choose_recovery_stream(&http, vec![server.stream(250, true), hls], false, Duration::from_secs(2)).await.unwrap();
        assert_eq!(stream_itag(&chosen), Some(234));
        assert!(chosen.starts.is_some());
        // Shortlisting is not readiness: prepare_recovery_candidates still has to
        // open its real segment decoder and retain a frame before any handoff.
        assert_eq!(server.target_requests.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn hls_renewal_changes_routes_but_never_the_segment_layout_or_media_kind() {
        let original = ResolvedStream { itag: Some(234), media: stream::Media::Segments(vec!["https://media.invalid/a".into(), "https://media.invalid/b".into()]),
            starts: Some(vec![0.0, 3.0]), size: 0, duration_ms: 6000, user_agent: "test".into(), loudness_db: None, opus: false };
        let identity = StreamIdentity::capture(&original).unwrap();
        let renewed = ResolvedStream { media: stream::Media::Segments(vec!["https://route.invalid/a".into(), "https://route.invalid/b".into()]), ..original.clone() };
        identity.validate(&renewed).unwrap();
        let shifted = ResolvedStream { starts: Some(vec![0.0, 4.0]), ..renewed.clone() };
        assert!(identity.validate(&shifted).is_err());
        let missing_segment = ResolvedStream { media: stream::Media::Segments(vec!["https://route.invalid/a".into()]), ..renewed.clone() };
        assert!(identity.validate(&missing_segment).is_err());
        let file = ResolvedStream { media: stream::Media::File("https://route.invalid/audio".into()), starts: None, ..renewed };
        assert!(identity.validate(&file).is_err());
    }

    /// Native anonymous recovery from the exact user-reported frozen position.
    /// No profile, account cookies, audio output, history, or media file is used.
    #[tokio::test]
    #[ignore = "live recovery acceptance depends on external YouTube CDN"]
    async fn reported_track_resumes_past_113_seconds_and_decodes_to_end() -> anyhow::Result<()> {
        use rodio::Source;
        const ID: &str = "o23rb5dHB5s";
        let at = Duration::from_millis(113195);
        let http = crate::proxy::http();
        let html = http.get("https://music.youtube.com/").header("User-Agent", innertube::UA)
            .timeout(Duration::from_secs(20)).send().await
            .and_then(reqwest::Response::error_for_status).map_err(reqwest::Error::without_url)?
            .text().await.map_err(reqwest::Error::without_url)?;
        let visitor = html.split_once("\"VISITOR_DATA\":\"").and_then(|(_, tail)| tail.split('"').next())
            .filter(|s| !s.is_empty()).ok_or_else(|| anyhow::anyhow!("no anonymous visitor data"))?;
        let candidates = innertube::audio_streams(&http, ID, visitor, true).await?.into_iter().map(|s| ResolvedStream {
            itag: Some(s.itag),
            media: stream::Media::File(s.url), starts: None, size: s.size, duration_ms: s.duration_ms, user_agent: innertube::UA.into(), loudness_db: s.loudness_db, opus: s.opus,
        }).collect();
        let wanted = std::sync::atomic::AtomicU64::new(1);
        let opened = prepare_recovery_candidates(http, candidates, false, at, &wanted, 1, |_| Ok(None)).await?;
        let shared = opened.shared;
        let expected_seconds = shared.duration_ms as f64 / 1000.0 - at.as_secs_f64();
        let mut prepared = opened.decoder.ok_or_else(|| anyhow::anyhow!("recovery decoder was not prepared"))?;
        let decode_shared = shared.clone();
        let decoding = tokio::task::spawn_blocking(move || -> anyhow::Result<(usize, f64)> {
            let decoder = &mut prepared;
            let channels = decoder.channels().get() as usize;
            let rate = decoder.sample_rate().get() as f64;
            let limit = ((expected_seconds + 2.0) * rate * channels as f64) as usize;
            let mut samples = 0;
            for sample in decoder.by_ref().take(limit + 1) {
                anyhow::ensure!(sample.is_finite(), "non-finite recovery PCM");
                samples += 1;
            }
            anyhow::ensure!(decode_shared.error().is_none(), "recovery download failed");
            anyhow::ensure!(samples > 0 && samples <= limit && samples.is_multiple_of(channels), "invalid resumed PCM frames");
            let seconds = samples as f64 / (rate * channels as f64);
            anyhow::ensure!((seconds - expected_seconds).abs() < 1.0, "recovery stopped before the track ended");
            Ok((samples / channels, seconds))
        });
        let (frames, seconds) = tokio::time::timeout(Duration::from_secs(90), decoding).await
            .map_err(|_| anyhow::anyhow!("live recovery decode exceeded deadline"))???;
        anyhow::ensure!(shared.is_complete(), "resumed audio did not finish downloading");
        println!("resumed_from_ms=113195, frames={frames}, decoded_remaining_seconds={seconds:.3}");
        Ok(())
    }
}

#[cfg(test)]
mod playback_load_tests {
    use super::*;
    use std::io::Read;
    use std::sync::{atomic::AtomicU64, mpsc};
    use std::time::Duration;

    fn pending_stream() -> Arc<Shared> {
        Shared::new(1024, 1000, None, false, false)
    }

    fn read_in_background(shared: &Arc<Shared>) -> (mpsc::Receiver<std::io::Result<usize>>, std::thread::JoinHandle<()>) {
        let mut reader = stream::StreamReader::new(shared.clone());
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = tx.send(reader.read(&mut [0]));
        });
        (rx, worker)
    }

    #[test]
    fn superseding_a_pending_decoder_wakes_its_blocked_reader() {
        let loads = PlaybackLoads::default();
        let wanted = AtomicU64::new(1);
        let old = pending_stream();
        assert!(loads.begin(1, &wanted));
        assert!(loads.register(1, &wanted, &old));
        let (rx, worker) = read_in_background(&old);
        wanted.store(2, Ordering::Relaxed);
        assert!(loads.begin(2, &wanted));
        let result = rx.recv_timeout(Duration::from_secs(1));
        old.cancel();
        worker.join().unwrap();
        assert!(result.unwrap().is_err());
        assert!(!loads.finish(1, &wanted, &old, || panic!("stale decoder published")));
    }

    #[test]
    fn a_late_old_resolver_cannot_replace_a_new_ready_stream() {
        let loads = PlaybackLoads::default();
        let wanted = AtomicU64::new(1);
        let current: StreamSlot = Default::default();
        assert!(loads.begin(1, &wanted));
        // The second resolution and decoder finish before the first resolver.
        wanted.store(2, Ordering::Relaxed);
        assert!(loads.begin(2, &wanted));
        let new = pending_stream();
        assert!(loads.register(2, &wanted, &new));
        assert!(loads.finish(2, &wanted, &new, || {
            *current.lock().unwrap() = Some(("new".into(), new.clone()));
        }));
        let old = pending_stream();
        assert!(!loads.register(1, &wanted, &old));
        assert!(loads.current(1, &wanted, || panic!("stale resolver changed state")).is_none());
        assert!(Arc::ptr_eq(&current.lock().unwrap().as_ref().unwrap().1, &new));
        let (rx, worker) = read_in_background(&old);
        let result = rx.recv_timeout(Duration::from_secs(1));
        old.cancel();
        worker.join().unwrap();
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn handing_a_decoder_to_audio_removes_it_from_pending_cancellation() {
        let loads = PlaybackLoads::default();
        let wanted = AtomicU64::new(1);
        let outgoing = pending_stream();
        assert!(loads.begin(1, &wanted));
        assert!(loads.register(1, &wanted, &outgoing));
        assert!(loads.finish(1, &wanted, &outgoing, || ()));
        let (rx, worker) = read_in_background(&outgoing);
        wanted.store(2, Ordering::Relaxed);
        assert!(loads.begin(2, &wanted));
        // This reader represents the still-playing outgoing crossfade source.
        let untouched = rx.recv_timeout(Duration::from_millis(50));
        outgoing.cancel();
        let ended = rx.recv_timeout(Duration::from_secs(1));
        worker.join().unwrap();
        assert!(matches!(untouched, Err(mpsc::RecvTimeoutError::Timeout)));
        assert!(ended.unwrap().is_err());
    }

    #[test]
    fn decoder_constructor_panic_is_an_error_for_only_the_current_play() {
        let loads = PlaybackLoads::default();
        let wanted = AtomicU64::new(1);
        let shared = pending_stream();
        assert!(loads.begin(1, &wanted));
        assert!(loads.register(1, &wanted, &shared));
        let error = open_decoder_guarded::<()>(|| panic!("synthetic constructor failure")).unwrap_err();
        assert!(error.is::<DecoderPanicked>());
        let (tx, rx) = mpsc::channel();
        assert!(loads.finish(1, &wanted, &shared, || {
            shared.cancel();
            tx.send(Resp::PlayError { generation: 1, msg: format!("decode: {error}") }).unwrap();
        }));
        assert!(matches!(rx.recv().unwrap(), Resp::PlayError { generation: 1, .. }));
        wanted.store(2, Ordering::Relaxed);
        assert!(loads.begin(2, &wanted));
        assert!(!loads.finish(1, &wanted, &shared, || panic!("stale constructor sent an error")));
        assert!(open_decoder_guarded(|| Ok(7)).is_ok_and(|n| n == 7));
    }
}
