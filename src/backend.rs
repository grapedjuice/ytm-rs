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
    /// The queue another device left off with (YouTube Music's own cross-device queue).
    ServerQueue,
    /// Upload audio files to the account's library.
    Upload(Vec<PathBuf>),
    /// Save a song's audio to the downloads folder.
    Download(Track),
    Lyrics(Track),
    /// SponsorBlock's parts of a video to skip; `duration` in seconds, if known.
    Segments { video_id: String, duration: Option<f64> },
    /// Romanization and translation for the lyric lines of one source.
    LyricsExtras { video_id: String, source: String, lines: Vec<String>, lang: Option<String>, target: String },
    /// Process cover art for the background; `key` is echoed back.
    Art { key: String, url: String },
    /// Radio seeded from a track; appended to the queue.
    Radio(String),
    /// Fetch an album/playlist and play it from the start.
    PlayCollection { target: Target },
    /// `at`: start position (non-zero when resuming last session's song). `report`: tell
    /// YouTube about the play (not when picking up a song already counted).
    Play { video_id: String, generation: u64, at: std::time::Duration, report: bool },
    /// Resolve and download a track ahead of time so skipping to it is instant.
    Prefetch(String),
    Like { video_id: String, like: bool },
    /// Several likes (or unlikes) one after another, for a selection.
    LikeMany { video_ids: Vec<String>, like: bool },
    Dislike(String),
    CreatePlaylist { title: String, video_ids: Vec<String> },
    EditPlaylist { id: String, title: String, description: String, privacy: Option<String> },
    AddToPlaylist { playlist_id: String, video_ids: Vec<String> },
    /// `(videoId, setVideoId)` entries to take out of the playlist.
    RemoveFromPlaylist { playlist_id: String, items: Vec<(String, String)> },
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
            Req::ServerQueue => "Queue from other devices",
            Req::Upload(_) => "Upload",
            Req::Download(_) => "Download",
            Req::Lyrics(_) => "Lyrics",
            Req::Segments { .. } => "SponsorBlock",
            Req::LyricsExtras { .. } => "Translation",
            Req::Art { .. } => "Art",
            Req::Radio(_) => "Loading radio",
            Req::PlayCollection { .. } => "Playback",
            Req::Play { .. } => "Playback",
            Req::Prefetch(_) => "Prefetch",
            Req::Like { .. } | Req::LikeMany { .. } => "Like",
            Req::Dislike(_) => "Dislike",
            Req::CreatePlaylist { .. } => "Create playlist",
            Req::EditPlaylist { .. } => "Edit playlist",
            Req::AddToPlaylist { .. } => "Add to playlist",
            Req::RemoveFromPlaylist { .. } => "Remove from playlist",
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
        matches!(self, Req::Suggest { .. } | Req::Account | Req::ServerQueue | Req::Art { .. } | Req::Prefetch(_) | Req::HomeMore(_) | Req::Lyrics(_) | Req::Segments { .. } | Req::LyricsExtras { .. })
    }
}

pub struct Explore {
    pub new_albums: Vec<Card>,
    pub charts: Vec<Card>,
    pub top: Vec<Track>,
}

pub struct SearchResults {
    pub query: String,
    pub tracks: Vec<Track>,
    pub albums: Vec<Card>,
    pub artists: Vec<Card>,
    pub playlists: Vec<Card>,
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
    /// The subtitle, with the song count for a playlist.
    pub fn byline(&self) -> String {
        match self.count {
            Some(n) if self.subtitle.is_empty() => format!("{n} songs"),
            Some(n) => format!("{} • {n} songs", self.subtitle),
            None => self.subtitle.clone(),
        }
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
    /// That half didn't load (YouTube Music refused it, say): the one shown stays.
    pub playlists_failed: bool,
    pub albums_failed: bool,
}

pub enum Resp {
    Home(ytm::HomePage),
    HomeMore(ytm::HomePage),
    QuickPicksMore { seed: String, tracks: Vec<Track> },
    Explore(Explore),
    Search(SearchResults),
    Suggest { query: String, round: u64, terms: Vec<ytm::Suggestion> },
    /// An album or playlist; `more`: the rest of its songs follow in `CollectionMore`.
    Collection { load: u64, c: Collection, more: bool },
    /// The next page of a playlist sent as `Collection` with the same `load`.
    CollectionMore { id: String, load: u64, tracks: Vec<Track>, set_ids: Vec<Option<String>>, more: bool },
    Artist(ArtistPage),
    Library(Library),
    History(Vec<Track>),
    Uploads(Vec<Track>),
    Account { name: Option<String>, photo: Option<String> },
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
    /// What was asked to play (a `Req::PlayCollection`), and its songs.
    PlayQueue { target: Target, tracks: Vec<Track> },
    Liked { video_id: String, like: bool },
    /// A run of likes or unlikes: `taken` just went through, `done` of `total` tried.
    LikeProgress { taken: Vec<String>, done: usize, total: usize, like: bool },
    /// The songs YouTube Music took, and the ones it refused.
    LikedMany { video_ids: Vec<String>, failed: Vec<String>, like: bool },
    PlaylistCreated { id: String, songs: usize },
    PlaylistChanged { id: String, message: &'static str },
    Collaboration(ytm::Collaboration),
    /// Playback for `generation` failed before reaching the audio engine.
    PlayError { generation: u64, msg: String },
    LoggedIn(bool),
    /// A past search left the account's history; the list already shows it gone.
    SearchForgotten,
    Error(String),
}

impl Resp {
    /// An album or playlist load that ends without more songs: also sent when it fails,
    /// so the page stops waiting for it.
    fn ended(id: String, load: u64) -> Self {
        Resp::CollectionMore { id, load, tracks: Vec::new(), set_ids: Vec::new(), more: false }
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

/// How long one like or unlike may take. Taken ones answer in about a quarter second;
/// ones YouTube Music refuses while it limits the account are held 8–9 s before the 403
/// (two of ten unlikes cost 17 of 27 s; in a transfer one like in six or so hangs).
/// Sending it again is harmless (liking a liked song changes nothing) and usually goes
/// straight through.
pub const LIKE_WAIT: std::time::Duration = std::time::Duration::from_millis(1500);

/// Like or unlike one song; whether YouTube Music took it. Refusals are retried by the
/// API (after its shared pause while the account is limited); one that hangs or fails is
/// sent again, up to three times in all.
async fn like_one(api: &ytmapi::Api, id: &str, like: bool) -> bool {
    for tries in 0..3 {
        match tokio::time::timeout(LIKE_WAIT, ytm::set_like(api, id, like)).await {
            Ok(Ok(())) => return true,
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
pub async fn like_many(api: &ytmapi::Api, ids: Vec<String>, like: bool, mut progress: impl FnMut(Option<&str>, usize)) -> (Vec<String>, Vec<String>) {
    let _turn = match LIKES.try_lock() {
        Ok(turn) => turn,
        Err(_) => {
            progress(None, 0);
            LIKES.lock().await
        }
    };
    let total = ids.len();
    let mut failed = Vec::new();
    for (i, id) in ids.iter().enumerate() {
        let taken = like_one(api, id, like).await;
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
            like_one(api, id, like).await;
            tokio::time::sleep(LIKE_GAP).await;
        }
    }
    let done: Vec<String> = ids.into_iter().filter(|id| !failed.contains(id)).collect();
    (done, failed)
}

/// A video id and its stream buffer.
type StreamSlot = Arc<std::sync::Mutex<Option<(String, Arc<Shared>)>>>;

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
        let backend = Self { rt: Arc::new(rt), api, tx, audio, wake, prefetched: Default::default(), current: Default::default(), data_dir, lyrics, translate, jsc, premium: Arc::new(tokio::sync::Mutex::new(Err(None))), hq: Default::default() };
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
        let _one = CHECKING.lock().await;
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

    async fn forget_session(&self) {
        webmusic::Session::remove(&self.data_dir);
        self.api.set_account(None);
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
                Req::Explore(preferred_artists) => {
                    let (charts, new) = tokio::join!(ytm::charts(api), ytm::new_albums(api, me));
                    let charts = charts?;
                    Resp::Explore(Explore {
                        new_albums: popular_new_albums(&new.unwrap_or_default(), &charts.artists, &preferred_artists),
                        charts: charts.playlists,
                        top: charts.top,
                    })
                }
                Req::Search(query) => {
                    let (t, al, ar, pl) = tokio::join!(
                        ytm::search_songs(api, &query, me),
                        ytm::search_cards(api, &query, ytm::Filter::Albums, me),
                        ytm::search_cards(api, &query, ytm::Filter::Artists, me),
                        ytm::search_cards(api, &query, ytm::Filter::Playlists, me),
                    );
                    Resp::Search(SearchResults {
                        tracks: t?,
                        albums: al.unwrap_or_default(),
                        artists: ar.unwrap_or_default(),
                        playlists: pl.unwrap_or_default(),
                        query,
                    })
                }
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
                Req::Library => {
                    // Liked songs are Liked Music, loaded like any playlist. Each half shows
                    // without the other; it only fails when both do.
                    let (pls, albums) = tokio::join!(ytm::saved_playlists(api), ytm::saved_albums(api));
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
                    Resp::Library(Library {
                        playlists_failed: pls.is_err(),
                        albums_failed: albums.is_err(),
                        playlists: pls.map(|p| p.into_iter().filter(saved_playlist_visible).collect()).unwrap_or_default(),
                        albums: albums.unwrap_or_default(),
                    })
                }
                Req::History => Resp::History(ytm::history(api).await?),
                Req::Uploads => Resp::Uploads(ytm::uploads(api).await?),
                Req::Account => {
                    let (name, photo) = ytm::account(api).await?;
                    Resp::Account { name, photo }
                }
                Req::ServerQueue => match ytm::server_queue(api).await? {
                    Some(q) => Resp::ServerQueue(q),
                    None => return Ok(None),
                },
                Req::Upload(files) => {
                    let session = api.account().ok_or_else(|| anyhow::anyhow!("sign in to upload music"))?;
                    let (mut done, mut failed) = (0, Vec::new());
                    for f in &files {
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
                    Resp::Uploaded { done, failed }
                }
                Req::Lyrics(track) => {
                    let video_id = track.id.clone();
                    Resp::Lyrics { lyrics: self.lyrics_for(&track).await, video_id }
                }
                Req::Segments { video_id, duration } => {
                    let segments = crate::sponsorblock::segments(&self.http(), &video_id, duration).await?;
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
                Req::Radio(seed) => {
                    let tracks = ytm::radio(api, &seed, false, 50).await?;
                    Resp::Radio { seed, tracks }
                }
                Req::PlayCollection { target } => {
                    let tracks = match &target {
                        Target::Album(id) => ytm::album(api, id).await?.tracks,
                        Target::Playlist(id) => ytm::playlist(api, id, me, None).await?.tracks,
                        Target::Artist(id) => ytm::artist(api, id).await?.top,
                        Target::Song(t) => vec![t.clone()],
                    };
                    Resp::PlayQueue { target, tracks }
                }
                Req::Download(t) => {
                    let path = self.download(&t).await?;
                    Resp::Downloaded { title: t.title, path }
                }
                Req::Play { video_id, generation, at, report } => match self.start_playback(&video_id, generation, at, report).await {
                    Ok(()) => return Ok(None),
                    Err(e) => Resp::PlayError { generation, msg: format!("{e:#}") },
                },
                Req::Prefetch(video_id) => {
                    // Let the playing song finish downloading first: it gets the whole
                    // bandwidth, and a skip in the meantime makes this prefetch moot.
                    let generation = self.audio.status.wanted_gen.load(Ordering::Relaxed);
                    let playing = self.current.lock().unwrap().as_ref().map(|(_, s)| s.clone());
                    if let Some(s) = playing {
                        s.finished().await;
                    }
                    if self.audio.status.wanted_gen.load(Ordering::Relaxed) != generation
                        || self.prefetched.lock().unwrap().as_ref().is_some_and(|(id, _)| *id == video_id)
                    {
                        return Ok(None);
                    }
                    log::debug!("prefetch {video_id}");
                    let shared = self.open_stream(&video_id, true).await?;
                    if let Some((_, old)) = self.prefetched.lock().unwrap().replace((video_id, shared)) {
                        old.cancel();
                    }
                    return Ok(None);
                }
                Req::Like { video_id, like } => {
                    ytm::set_like(api, &video_id, like).await?;
                    Resp::Liked { video_id, like }
                }
                Req::LikeMany { video_ids, like } => self.like_many(video_ids, like).await?,
                Req::Dislike(video_id) => {
                    ytm::dislike(api, &video_id).await?;
                    Resp::Liked { video_id, like: false }
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
                    ytm::add_videos(api, &playlist_id, &video_ids).await?;
                    Resp::PlaylistChanged { id: playlist_id, message: "Added to playlist" }
                }
                Req::RemoveFromPlaylist { playlist_id, items } => {
                    ytm::remove_videos(api, &playlist_id, &items).await?;
                    Resp::PlaylistChanged { id: playlist_id, message: "Removed from playlist" }
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

    async fn lyrics_for(&self, t: &Track) -> LyricsSet {
        let artist = t.artist_line();
        let query = lyrics::Query {
            video_id: &t.id,
            song: &t.title,
            artist: &artist,
            album: t.album.as_ref().map(|a| a.name.as_str()),
            duration: match t.duration {
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
        session.save(&self.data_dir);
        self.api.set_account(Some(session));
        // The sign-in window's copy of the session would go stale as the app takes new
        // cookies, and opening it again would show Google the old ones, which it treats
        // as stolen and signs the session out. Keep only the app's copy.
        let profile = self.login_profile();
        let _ = tokio::task::spawn_blocking(move || crate::login::forget(&profile)).await;
        Ok(())
    }

    async fn start_playback(&self, video_id: &str, generation: u64, at: std::time::Duration, report: bool) -> anyhow::Result<()> {
        let t0 = std::time::Instant::now();
        let prefetched = self.prefetched.lock().unwrap().take();
        let replay = self.current.lock().unwrap().take().filter(|(id, s)| id == video_id && s.is_complete()).map(|(_, s)| s);
        let ready = match prefetched {
            Some((id, s)) if id == video_id => Some(s),
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
                self.open_stream(video_id, false).await?
            }
        };
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
            Err(e) => {
                let _ = tx.send(Resp::PlayError { generation, msg: format!("decode: {e}") });
                wake();
            }
        });
        Ok(())
    }

    /// Report how much of play `generation` is heard, on the web player's schedule
    /// (10, 20 and 30 s in, then every 40 s), with a last report once it ends or is left.
    /// Each covers the stretch played since the one before; time paused doesn't count.
    async fn report_watchtime(&self, id: &str, generation: u64, w: ytm::Watchtime) {
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
    async fn open_stream(&self, video_id: &str, prefetch: bool) -> anyhow::Result<Arc<Shared>> {
        let id = video_id.to_owned();
        if let Some(shared) = tokio::task::spawn_blocking(move || crate::downloads::open(&id)).await? {
            return Ok(shared);
        }
        let (source, s) = self.resolve(video_id, false).await?;
        let shared = Shared::new(s.size, s.duration_ms, s.loudness_db, s.opus, prefetch);
        self.rt.spawn(stream::download(self.http(), s.url, s.user_agent, shared.clone(), Some(self.refresher(source, video_id, s.opus))));
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
                Ok(s.url)
            })
        })
    }

    /// Download a song into the downloads folder, at the best quality the account gets.
    async fn download(&self, t: &Track) -> anyhow::Result<PathBuf> {
        let (source, s) = self.resolve(&t.id, true).await?;
        log::info!("download {}: {} bytes from {source:?}", t.id, s.size);
        let shared = Shared::new(s.size, s.duration_ms, s.loudness_db, s.opus, false);
        stream::download(self.http(), s.url, s.user_agent, shared.clone(), Some(self.refresher(source, &t.id, s.opus))).await;
        if let Some(e) = shared.error() {
            anyhow::bail!("{e}");
        }
        let audio = shared.bytes().ok_or_else(|| anyhow::anyhow!("the download stopped early"))?;
        let cover = match model::pick(&t.thumbs, 120).map(|u| sized(u, 1200)) {
            Some(url) => async { self.http().get(url).send().await?.error_for_status()?.bytes().await }.await.ok().map(|b| b.to_vec()),
            None => None,
        };
        let (t, duration_ms, loudness) = (t.clone(), shared.duration_ms, shared.loudness_db);
        tokio::task::spawn_blocking(move || crate::downloads::save(&t, &audio, duration_ms, loudness, cover)).await?
    }

    /// Find a stream URL, trying the fastest source that can play the song. `best`
    /// (downloads) takes the high quality stream whatever the setting, and waits for
    /// the solver it needs. Playback without Premium takes Opus where there is one;
    /// downloads stay AAC, which saves as a tagged .m4a.
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
        let solver_ready = self.jsc.ready();
        if hq && !solver_ready && self.api.account().is_some() {
            log::info!("{video_id}: solver not ready yet, standard quality for now");
            self.warm_signed_in();
        }
        if hq
            && solver_ready
            && let Some(session) = self.api.account()
            && self.is_premium(&session).await
        {
            match self.resolve_from(Source::WebMusic, video_id, false).await {
                Ok(s) => return Ok((Source::WebMusic, s)),
                // Not fatal: the usual 128 kbps stream still plays.
                Err(e) => log::warn!("high quality stream failed for {video_id} ({e:#}); using standard quality"),
            }
        }
        match self.resolve_from(Source::VisionOs, video_id, !best).await {
            Ok(s) => Ok((Source::VisionOs, s)),
            // Age-restricted or Premium-only: only signed-in web players get it.
            Err(e) if needs_account(&e) => {
                log::info!("{video_id} needs a signed-in player ({e:#})");
                let session = self.api.account().ok_or_else(|| anyhow::anyhow!("sign in to play this song"))?;
                if self.is_premium(&session).await {
                    match self.resolve_from(Source::WebMusic, video_id, false).await {
                        Ok(s) => return Ok((Source::WebMusic, s)),
                        Err(e) => log::warn!("web music player failed ({e:#}); using yt-dlp"),
                    }
                }
                Ok((Source::YtDlp, self.resolve_from(Source::YtDlp, video_id, false).await?))
            }
            Err(e) => Err(e),
        }
    }

    /// `opus`: the visionOS player's Opus stream where there is one (the others are AAC).
    async fn resolve_from(&self, source: Source, video_id: &str, opus: bool) -> anyhow::Result<ResolvedStream> {
        let plain = |s: innertube::Stream, ua: &str| ResolvedStream {
            url: s.url,
            size: s.size,
            duration_ms: s.duration_ms,
            user_agent: ua.to_owned(),
            loudness_db: s.loudness_db,
            opus: s.opus,
        };
        Ok(match source {
            Source::VisionOs => plain(self.visionos_stream(video_id, opus).await?, innertube::UA),
            Source::WebMusic => {
                let session = self.api.account().ok_or_else(|| anyhow::anyhow!("signed out"))?;
                plain(session.audio_stream(&self.http(), &self.jsc, video_id).await?, webmusic::UA)
            }
            Source::YtDlp => {
                let session = self.api.account().ok_or_else(|| anyhow::anyhow!("sign in to play this song"))?;
                let s = ytdlp::resolve(&self.http(), &self.data_dir, &session.cookie, video_id).await?;
                // Cookies Google replaced while yt-dlp used the session.
                self.api.keep(&session, s.replaced.iter().map(String::as_str));
                // yt-dlp's JSON doesn't carry YouTube's loudness figure.
                ResolvedStream { url: s.url, size: s.size, duration_ms: s.duration_ms, user_agent: s.user_agent, loudness_db: None, opus: false }
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

    /// Like or unlike a selection, one at a time, then check Liked Music and try again
    /// what didn't take. Unlikes still listed after that count as refused; likes aren't
    /// counted so (songs that can't be played are liked but not listed).
    async fn like_many(&self, ids: Vec<String>, like: bool) -> anyhow::Result<Resp> {
        let total = ids.len();
        let (done, failed) = like_many(&self.api, ids, like, |taken, n| {
            let _ = self.tx.send(Resp::LikeProgress { taken: taken.into_iter().map(str::to_owned).collect(), done: n, total, like });
            self.wake();
        })
        .await;
        // Even when all were refused: the app stops waiting on them and shows them as they are.
        Ok(Resp::LikedMany { video_ids: done, failed, like })
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
            resp = Resp::CollectionMore { id: id.clone(), load, more: token.is_some() && have < limit, tracks, set_ids };
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

#[derive(Clone, Copy, Debug)]
enum Source {
    VisionOs,
    WebMusic,
    YtDlp,
}

struct ResolvedStream {
    url: String,
    size: u64,
    duration_ms: u64,
    user_agent: String,
    loudness_db: Option<f32>,
    /// Opus in WebM; otherwise AAC in MP4.
    opus: bool,
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
