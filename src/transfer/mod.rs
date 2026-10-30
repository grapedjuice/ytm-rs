//! Copy a Spotify playlist, or Liked Songs, into YouTube Music (spot2ytm, built in).
//!
//! A transfer runs on the network runtime while the app keeps playing: read the
//! Spotify playlist, search YouTube Music for every song a few at a time, search the
//! doubtful ones again, let the user review what's still doubtful, then create a
//! private playlist or like the songs. The UI reads the shared [`Job`] each frame.
//! Progress is saved as it goes, so quitting or losing the connection never repeats work.

pub mod spotify;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use futures_util::{StreamExt, stream};

use crate::model::Track as YtTrack;
use crate::ytm;
use crate::ytmapi::Api;
use spotify::{Source, Track};

/// YouTube Music searches in flight at once. More gets the account rate limited on long lists.
const CONCURRENCY: usize = 8;
/// The most songs a YouTube Music playlist holds; longer lists are split into parts.
const PLAYLIST_MAX: usize = 5000;
/// How often progress is written while searching or liking.
const SAVE_EVERY: Duration = Duration::from_secs(5);
/// The credential the Spotify sign-in (`sp_dc` cookie) is saved under.
const VAULT: &str = "spotify sp_dc";

/// How a song came out of the search.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Search {
    Pending,
    Found(YtTrack),
    /// A result whose title or artist doesn't match; offered for review.
    Unsure(YtTrack),
    Missing,
    /// Left out during the review.
    Skipped,
}

impl Search {
    pub fn track(&self) -> Option<&YtTrack> {
        match self {
            Search::Found(t) | Search::Unsure(t) => Some(t),
            _ => None,
        }
    }
}

/// Where the songs go.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Dest {
    /// A new private playlist with this name.
    Playlist(String),
    /// The account's Liked Music.
    Liked,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Phase {
    Reading,
    Searching,
    /// Searching the doubtful songs again, with videos.
    Checking,
    /// Waiting for the user to go through `doubts`.
    Review,
    Saving,
    Done,
    Failed(String),
}

impl Phase {
    pub fn running(&self) -> bool {
        !matches!(self, Phase::Done | Phase::Failed(_))
    }
}

/// A song the user picks for: the alternatives found for it and what they chose.
pub struct Doubt {
    pub index: usize,
    pub candidates: Vec<YtTrack>,
    /// Which candidates are videos rather than songs.
    pub videos: Vec<bool>,
    pub decision: Option<Decision>,
    /// A matched song opened for a change: its alternatives are still being searched.
    pub loading: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Decision {
    Keep,
    Pick(usize),
    Skip,
}

pub struct Summary {
    pub added: usize,
    /// Songs that matched the same YouTube Music song as an earlier one.
    pub duplicates: usize,
    /// Liked Music only: songs that were liked already.
    pub already: usize,
    pub playlists: Vec<String>,
    pub elapsed: Duration,
}

pub struct Job {
    pub source: Source,
    pub name: String,
    pub dest: Dest,
    pub phase: Phase,
    /// Spotify songs read, of the total.
    pub read: (usize, usize),
    pub read_secs: Option<f32>,
    pub tracks: Vec<Track>,
    pub results: Vec<Search>,
    /// Removed songs and podcast episodes in the Spotify playlist.
    pub not_songs: usize,
    /// When the current search pass started, and how many songs it had left then.
    search_started: Option<(Instant, usize)>,
    pub doubts: Vec<Doubt>,
    /// Liking or adding: done, of the total.
    pub saving: (usize, usize),
    pub summary: Option<Summary>,
    pub paused: bool,
    started: Instant,
    /// Playlists already created, so a resumed transfer doesn't make them twice.
    created: Vec<String>,
    /// Songs added to those playlists so far (a batch at a time).
    added: usize,
    /// Songs this transfer has liked so far.
    liked: usize,
    /// The review is done; a resumed transfer goes straight to adding.
    reviewed: bool,
}

impl Job {
    pub fn searched(&self) -> usize {
        self.results.iter().filter(|r| **r != Search::Pending).count()
    }

    pub fn count(&self, f: impl Fn(&Search) -> bool) -> usize {
        self.results.iter().filter(|r| f(r)).count()
    }

    /// Seconds left in the search, from the pace so far.
    pub fn eta(&self) -> Option<u64> {
        let (at, left_then) = self.search_started?;
        let left = self.results.len() - self.searched();
        let done = left_then.saturating_sub(left);
        let secs = at.elapsed().as_secs_f64();
        (done >= 8 && secs > 1.0).then(|| (left as f64 * secs / done as f64).ceil() as u64)
    }

    /// The doubt shown now: the first without a decision.
    pub fn current_doubt(&self) -> Option<usize> {
        self.doubts.iter().position(|d| d.decision.is_none())
    }
}

/// Whether a Spotify playlist is someone's liked songs under another name, which the
/// transfer offers to add to Liked Music instead of a playlist.
pub fn is_likes(name: &str) -> bool {
    let n = name.trim().to_lowercase();
    matches!(n.as_str(), "liked songs" | "likes")
}

/// The Spotify side: the signed-in account and its library.
#[derive(Clone)]
pub enum Account {
    /// Signed in, not read yet (read when the transfer page or Settings first shows it).
    Unknown,
    SignedOut,
    Loading,
    SignedIn { name: String, library: Vec<spotify::Entry> },
    Failed(String),
}

/// A transfer that stopped part way, from an earlier run.
#[derive(Clone)]
pub struct Resumable {
    pub source: Source,
    pub name: String,
    pub dest: Dest,
    pub searched: usize,
    pub total: usize,
}

pub struct Transfer {
    pub job: Mutex<Option<Job>>,
    pub account: Mutex<Account>,
    pub resumable: Mutex<Option<Resumable>>,
    /// A pasted playlist link and what it turned out to be.
    pub lookup: Mutex<Option<(String, Option<Result<spotify::Entry, String>>)>>,
    /// Spotify sign-in window open.
    pub signing_in: AtomicBool,
    /// Bumped on every sign-in or sign-out, so a slow account read can't undo a newer one.
    account_gen: std::sync::atomic::AtomicU64,
    cancel: AtomicBool,
    review_done: tokio::sync::Notify,
    data_dir: PathBuf,
    wake: Arc<dyn Fn() + Send + Sync>,
}

/// The progress file: everything needed to pick a transfer up again.
#[derive(serde::Serialize, serde::Deserialize)]
struct Saved {
    version: u32,
    key: String,
    name: String,
    dest: Dest,
    tracks: Vec<Track>,
    results: Vec<Search>,
    not_songs: usize,
    created: Vec<String>,
    #[serde(default)]
    added: usize,
    #[serde(default)]
    liked: usize,
    #[serde(default)]
    reviewed: bool,
}

const SAVED_VERSION: u32 = 1;

impl Transfer {
    pub fn new(data_dir: &Path, wake: Arc<dyn Fn() + Send + Sync>) -> Arc<Self> {
        let this = Arc::new(Self {
            job: Mutex::new(None),
            account: Mutex::new(if crate::vault::get(VAULT, data_dir).is_some() { Account::Unknown } else { Account::SignedOut }),
            lookup: Mutex::new(None),
            resumable: Mutex::new(None),
            signing_in: AtomicBool::new(false),
            account_gen: Default::default(),
            cancel: AtomicBool::new(false),
            review_done: tokio::sync::Notify::new(),
            data_dir: data_dir.to_owned(),
            wake,
        });
        *this.resumable.lock().unwrap() = this.find_resumable();
        this
    }

    fn dir(&self) -> PathBuf {
        self.data_dir.join("transfers")
    }

    fn find_resumable(&self) -> Option<Resumable> {
        let newest = std::fs::read_dir(self.dir()).ok()?.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "json")).max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())?;
        let s: Saved = serde_json::from_slice(&std::fs::read(newest.path()).ok()?).ok()?;
        let source = if s.key == "liked" { Source::Liked } else { Source::Playlist(s.key.clone()) };
        Some(Resumable {
            source,
            name: s.name,
            dest: s.dest,
            searched: s.results.iter().filter(|r| **r != Search::Pending).count(),
            total: s.tracks.len(),
        })
    }

    fn wake(&self) {
        (self.wake)()
    }

    pub fn running(&self) -> bool {
        self.job.lock().unwrap().as_ref().is_some_and(|j| j.phase.running())
    }

    /// Sign the Spotify side in or out; `None` signs out.
    pub fn set_cookie(self: &Arc<Self>, sp_dc: Option<String>, rt: &crate::backend::Backend) {
        match sp_dc {
            Some(c) => {
                if let Err(e) = crate::vault::set(VAULT, &self.data_dir, c.as_bytes()) {
                    *self.account.lock().unwrap() = Account::Failed(format!("Couldn't save the Spotify sign-in: {e}"));
                    return;
                }
                self.load_account(rt);
            }
            None => {
                self.account_gen.fetch_add(1, Ordering::Relaxed);
                crate::vault::delete(VAULT, &self.data_dir);
                crate::login::forget(&self.spotify_profile());
                *self.account.lock().unwrap() = Account::SignedOut;
            }
        }
    }

    /// Where the Spotify sign-in window keeps its browser profile.
    pub fn spotify_profile(&self) -> PathBuf {
        self.data_dir.join("webview-spotify")
    }

    fn cookie(&self) -> Option<String> {
        crate::vault::get(VAULT, &self.data_dir).and_then(|b| String::from_utf8(b).ok())
    }

    /// Read the signed-in Spotify account and its playlists.
    pub fn load_account(self: &Arc<Self>, rt: &crate::backend::Backend) {
        let Some(cookie) = self.cookie() else {
            *self.account.lock().unwrap() = Account::SignedOut;
            return;
        };
        *self.account.lock().unwrap() = Account::Loading;
        let generation = self.account_gen.fetch_add(1, Ordering::Relaxed) + 1;
        let this = self.clone();
        rt.spawn(async move {
            let res = async {
                let client = spotify::Client::connect(Some(&cookie), &this.data_dir).await?;
                client.account().await
            }
            .await;
            let mut account = this.account.lock().unwrap();
            if this.account_gen.load(Ordering::Relaxed) != generation {
                return; // signed out or in again meanwhile
            }
            *account = match res {
                Ok(a) => Account::SignedIn { name: a.name, library: a.library },
                Err(e) if e.is::<spotify::SignedOut>() => {
                    crate::vault::delete(VAULT, &this.data_dir);
                    Account::Failed(e.to_string())
                }
                Err(e) => {
                    log::warn!("spotify account: {e:#}");
                    Account::Failed(format!("Couldn't read your Spotify library: {e}"))
                }
            };
            drop(account);
            this.wake();
        });
    }

    /// Read the account the first time something shows it.
    pub fn ensure_account(self: &Arc<Self>, rt: &crate::backend::Backend) {
        if matches!(*self.account.lock().unwrap(), Account::Unknown) {
            self.load_account(rt);
        }
    }

    /// Look up a pasted playlist link (once per link), so the page can show what it is.
    pub fn look_up(self: &Arc<Self>, rt: &crate::backend::Backend, link: &str) {
        let link = link.trim().to_owned();
        {
            let mut slot = self.lookup.lock().unwrap();
            if slot.as_ref().is_some_and(|(l, _)| *l == link) {
                return;
            }
            let source = match Source::parse(&link) {
                Ok(s) => s,
                Err(e) => {
                    *slot = (!link.is_empty()).then(|| (link, Some(Err(e.to_string()))));
                    return;
                }
            };
            if source == Source::Liked {
                let entry = spotify::Entry { source, name: "Liked Songs".into(), owner: String::new(), count: None, art: None };
                *slot = Some((link, Some(Ok(entry))));
                return;
            }
            *slot = Some((link.clone(), None));
        }
        let this = self.clone();
        rt.spawn(async move {
            let res = async {
                let Source::Playlist(id) = Source::parse(&link)? else { unreachable!() };
                let client = spotify::Client::connect(this.cookie().as_deref(), &this.data_dir).await?;
                client.playlist_info(&id).await
            }
            .await
            .map_err(|e| e.to_string());
            let mut slot = this.lookup.lock().unwrap();
            if slot.as_ref().is_some_and(|(l, _)| *l == link) {
                *slot = Some((link, Some(res)));
            }
            drop(slot);
            this.wake();
        });
    }

    pub fn set_paused(&self, paused: bool) {
        if let Some(j) = self.job.lock().unwrap().as_mut() {
            j.paused = paused;
        }
    }

    /// Stop the transfer and forget its progress.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.review_done.notify_one();
    }

    /// Close a finished or failed transfer, back to the start screen.
    pub fn dismiss(&self) {
        let mut job = self.job.lock().unwrap();
        if job.as_ref().is_some_and(|j| !j.phase.running()) {
            *job = None;
        }
    }

    /// Record a review decision for the current doubt.
    pub fn decide(&self, doubt: usize, decision: Decision) {
        let mut guard = self.job.lock().unwrap();
        let Some(job) = guard.as_mut() else { return };
        if let Some(d) = job.doubts.get_mut(doubt) {
            d.decision = Some(decision);
            if job.current_doubt().is_none() {
                self.review_done.notify_one();
            }
        }
    }

    /// Decide a doubt with a song found by hand (a search outside the candidates): it joins
    /// the candidates and is picked.
    pub fn decide_with(&self, doubt: usize, track: YtTrack) {
        {
            let mut guard = self.job.lock().unwrap();
            let Some(job) = guard.as_mut().filter(|j| j.phase == Phase::Review) else { return };
            let Some(d) = job.doubts.get_mut(doubt) else { return };
            let k = match d.candidates.iter().position(|c| c.id == track.id) {
                Some(k) => k,
                None => {
                    // Search results without an album are videos.
                    d.videos.push(track.album.is_none());
                    d.candidates.push(track);
                    d.candidates.len() - 1
                }
            };
            d.loading = false;
            d.decision = Some(Decision::Pick(k));
            if job.current_doubt().is_none() {
                self.review_done.notify_one();
            }
        }
        self.wake();
    }

    /// Open a song that matched for a change: it joins the review (its current match is
    /// kept unless another is picked) and its alternatives are searched. Returns its place
    /// in the review.
    pub fn edit_match(self: &Arc<Self>, rt: &crate::backend::Backend, index: usize) -> Option<usize> {
        let track = {
            let mut guard = self.job.lock().unwrap();
            let job = guard.as_mut()?;
            if let Some(k) = job.doubts.iter().position(|d| d.index == index) {
                return Some(k);
            }
            job.doubts.push(Doubt { index, candidates: Vec::new(), videos: Vec::new(), decision: None, loading: true });
            job.tracks.get(index)?.clone()
        };
        let (this, api) = (self.clone(), rt.api());
        rt.spawn(async move {
            let found = candidates(&api, &track).await.unwrap_or_default();
            if let Some(job) = this.job.lock().unwrap().as_mut()
                && let Some(d) = job.doubts.iter_mut().find(|d| d.index == index)
            {
                (d.candidates, d.videos) = found.into_iter().unzip();
                d.loading = false;
            }
            this.wake();
        });
        Some(self.job.lock().unwrap().as_ref()?.doubts.len() - 1)
    }

    /// Keep the current picks for every song not reviewed yet.
    pub fn finish_review(&self) {
        let mut guard = self.job.lock().unwrap();
        let Some(job) = guard.as_mut() else { return };
        for d in &mut job.doubts {
            d.decision.get_or_insert(Decision::Keep);
        }
        self.review_done.notify_one();
    }

    /// Start a transfer; `resume` picks up the saved progress for the same source and
    /// destination, otherwise it starts over.
    pub fn start(self: &Arc<Self>, rt: &crate::backend::Backend, source: Source, dest: Dest, resume: bool) {
        if self.running() {
            return;
        }
        if !resume {
            self.forget(&source);
        }
        self.cancel.store(false, Ordering::Relaxed);
        *self.resumable.lock().unwrap() = None;
        *self.job.lock().unwrap() = Some(Job {
            name: String::new(),
            source: source.clone(),
            dest,
            phase: Phase::Reading,
            read: (0, 0),
            read_secs: None,
            tracks: Vec::new(),
            results: Vec::new(),
            not_songs: 0,
            search_started: None,
            doubts: Vec::new(),
            saving: (0, 0),
            summary: None,
            paused: false,
            started: Instant::now(),
            created: Vec::new(),
            added: 0,
            liked: 0,
            reviewed: false,
        });
        let (this, api) = (self.clone(), rt.api());
        rt.spawn(async move {
            let res = this.run(&api).await;
            let mut guard = this.job.lock().unwrap();
            if let Some(job) = guard.as_mut() {
                match res {
                    Ok(()) => {}
                    Err(_) if this.cancel.load(Ordering::Relaxed) => {
                        this.forget(&job.source);
                        *guard = None;
                    }
                    Err(e) => {
                        log::warn!("spotify transfer: {e:#}");
                        let saved = !job.tracks.is_empty();
                        job.phase = Phase::Failed(if saved { format!("{e}. Your progress is saved; try again to pick up where it stopped.") } else { e.to_string() });
                        drop(guard);
                        *this.resumable.lock().unwrap() = this.find_resumable();
                    }
                }
            }
            this.wake();
        });
    }

    fn path(&self, source: &Source) -> PathBuf {
        self.dir().join(format!("{}.json", source.key()))
    }

    fn forget(&self, source: &Source) {
        let _ = std::fs::remove_file(self.path(source));
    }

    /// Throw away a stopped transfer's progress.
    pub fn discard_saved(&self, source: &Source) {
        self.forget(source);
        *self.resumable.lock().unwrap() = self.find_resumable();
    }

    fn save(&self) {
        let saved = {
            let guard = self.job.lock().unwrap();
            let Some(j) = guard.as_ref() else { return };
            if j.tracks.is_empty() {
                return;
            }
            Saved {
                version: SAVED_VERSION,
                key: j.source.key().to_owned(),
                name: j.name.clone(),
                dest: j.dest.clone(),
                tracks: j.tracks.clone(),
                results: j.results.clone(),
                not_songs: j.not_songs,
                created: j.created.clone(),
                added: j.added,
                liked: j.liked,
                reviewed: j.reviewed,
            }
        };
        let path = self.path(&Source::parse_key(&saved.key));
        let write = || -> Result<()> {
            std::fs::create_dir_all(self.dir())?;
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec(&saved)?)?;
            std::fs::rename(&tmp, &path)?;
            Ok(())
        };
        if let Err(e) = write() {
            log::warn!("couldn't save transfer progress: {e:#}");
        }
    }

    fn load_saved(&self, source: &Source) -> Option<Saved> {
        let s: Saved = serde_json::from_slice(&std::fs::read(self.path(source)).ok()?).ok()?;
        (s.version == SAVED_VERSION && s.key == source.key() && s.results.len() == s.tracks.len()).then_some(s)
    }

    fn with_job<R>(&self, f: impl FnOnce(&mut Job) -> R) -> R {
        f(self.job.lock().unwrap().as_mut().expect("transfer job"))
    }

    /// Cancelled: stop. Paused: wait here.
    async fn checkpoint(&self) -> Result<()> {
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                bail!("cancelled");
            }
            if !self.with_job(|j| j.paused) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn run(&self, api: &Api) -> Result<()> {
        anyhow::ensure!(api.account().is_some(), "Sign in to YouTube Music first");
        let source = self.with_job(|j| j.source.clone());

        // ---- read the Spotify side, or pick up saved progress (for the same destination)
        let dest = self.with_job(|j| j.dest.clone());
        match self.load_saved(&source).filter(|s| s.dest == dest) {
            Some(s) => self.with_job(|j| {
                j.read = (s.tracks.len(), s.tracks.len());
                j.name = s.name;
                j.tracks = s.tracks;
                j.results = s.results;
                j.not_songs = s.not_songs;
                j.created = s.created;
                j.added = s.added;
                j.liked = s.liked;
                j.reviewed = s.reviewed;
            }),
            None => {
                let t = Instant::now();
                let cookie = self.cookie();
                anyhow::ensure!(cookie.is_some() || source != Source::Liked, "Sign in to Spotify to copy your Liked Songs");
                let client = spotify::Client::connect(cookie.as_deref(), &self.data_dir).await?;
                let progress = |done: usize, total: usize| {
                    self.with_job(|j| j.read = (done, total));
                    self.wake();
                };
                let playlist = match &source {
                    Source::Playlist(id) => client.playlist(id, progress).await?,
                    Source::Liked => client.liked(progress).await?,
                };
                let mut playlist = playlist;
                // Dev hook: YTM_TRANSFER_LIMIT=<n> keeps the first n songs.
                if let Some(n) = std::env::var("YTM_TRANSFER_LIMIT").ok().and_then(|n| n.parse().ok()) {
                    playlist.tracks.truncate(n);
                }
                self.with_job(|j| {
                    j.read_secs = Some(t.elapsed().as_secs_f32());
                    j.read = (playlist.tracks.len(), playlist.tracks.len());
                    j.results = vec![Search::Pending; playlist.tracks.len()];
                    j.name = playlist.name;
                    j.tracks = playlist.tracks;
                    j.not_songs = playlist.skipped;
                });
                self.save();
            }
        }

        // ---- search YouTube Music; a pass that hits network trouble leaves songs pending for the next
        self.with_job(|j| j.phase = Phase::Searching);
        self.wake();
        for pass in 0..3 {
            if pass > 0 {
                tokio::time::sleep(Duration::from_secs(3 * pass)).await;
            }
            if self.search_pass(api).await? == 0 {
                break;
            }
        }
        let pending = self.with_job(|j| j.count(|r| *r == Search::Pending));
        anyhow::ensure!(pending == 0, "{pending} songs couldn't be searched (no connection, or YouTube Music is limiting requests)");

        // ---- search the doubtful songs again, with videos, for the review
        let doubtful: Vec<usize> = self.with_job(|j| {
            j.phase = Phase::Checking;
            if j.reviewed {
                return Vec::new();
            }
            (0..j.results.len()).filter(|&i| matches!(j.results[i], Search::Unsure(_) | Search::Missing)).collect()
        });
        self.wake();
        let tracks: Vec<Track> = self.with_job(|j| doubtful.iter().map(|&i| j.tracks[i].clone()).collect());
        let mut found: Vec<(usize, Vec<(YtTrack, bool)>)> = stream::iter(doubtful.iter().copied().zip(tracks))
            .map(|(i, t)| async move {
                if self.checkpoint().await.is_err() {
                    return (i, Vec::new());
                }
                (i, candidates(api, &t).await.unwrap_or_default())
            })
            .buffer_unordered(CONCURRENCY)
            .collect()
            .await;
        self.checkpoint().await?;
        found.sort_by_key(|(i, _)| *i);
        self.with_job(|j| {
            for (i, cands) in found {
                // YouTube's results vary between searches, so the second one often has the exact song,
                // or at least a likelier pick than the first.
                let t = &j.tracks[i];
                let songs: Vec<YtTrack> = cands.iter().filter(|(_, video)| !video).map(|(h, _)| h.clone()).collect();
                if let Some((hit, sure)) = best(&songs, t) {
                    if sure {
                        j.results[i] = Search::Found(hit);
                        continue;
                    }
                    if j.results[i].track().is_none_or(|cur| score(&hit, t) > score(cur, t)) {
                        j.results[i] = Search::Unsure(hit);
                    }
                }
                let (candidates, videos) = cands.into_iter().unzip();
                j.doubts.push(Doubt { index: i, candidates, videos, decision: None, loading: false });
            }
        });
        self.save();

        // ---- the user goes through what's still doubtful
        if self.with_job(|j| !j.doubts.is_empty()) {
            self.with_job(|j| j.phase = Phase::Review);
            self.wake();
            while self.with_job(|j| j.current_doubt().is_some()) {
                self.review_done.notified().await;
                self.checkpoint().await?;
            }
            self.with_job(|j| {
                for d in std::mem::take(&mut j.doubts) {
                    match d.decision {
                        Some(Decision::Pick(k)) if k < d.candidates.len() => j.results[d.index] = Search::Found(d.candidates[k].clone()),
                        Some(Decision::Skip) => j.results[d.index] = Search::Skipped,
                        _ => {}
                    }
                }
            });
        }
        self.with_job(|j| j.reviewed = true);
        self.save();

        // ---- add them
        self.with_job(|j| j.phase = Phase::Saving);
        self.wake();
        let (ids, picked, dest) = self.with_job(|j| {
            if j.dest == Dest::Playlist(String::new()) {
                j.dest = Dest::Playlist(format!("{} from Spotify", j.name));
            }
            let mut seen = HashSet::new();
            let ids: Vec<String> = j.results.iter().filter_map(Search::track).filter(|t| seen.insert(t.id.clone())).map(|t| t.id.clone()).collect();
            (ids, j.results.iter().filter(|r| r.track().is_some()).count(), j.dest.clone())
        });
        anyhow::ensure!(!ids.is_empty(), "None of the songs were found on YouTube Music");
        let duplicates = picked - ids.len();
        // Dev hook: YTM_TRANSFER_DRY=1 does everything but add the songs.
        let dry = std::env::var_os("YTM_TRANSFER_DRY").is_some();
        let (playlists, already) = match &dest {
            _ if dry => (Vec::new(), 0),
            Dest::Playlist(title) => (self.create_playlists(api, title, &ids).await?, 0),
            Dest::Liked => (Vec::new(), self.like_all(api, &ids).await?),
        };

        let elapsed = self.with_job(|j| j.started.elapsed());
        self.with_job(|j| {
            let added = if dest == Dest::Liked && !dry { j.liked } else { ids.len() };
            j.summary = Some(Summary { added, duplicates, already, playlists, elapsed });
            j.phase = Phase::Done;
        });
        self.forget(&source);
        Ok(())
    }

    /// One pass over the pending songs; returns how many are still pending (failed).
    async fn search_pass(&self, api: &Api) -> Result<usize> {
        let todo: Vec<(usize, Track)> = self.with_job(|j| {
            let todo: Vec<(usize, Track)> = (0..j.tracks.len()).filter(|&i| j.results[i] == Search::Pending).map(|i| (i, j.tracks[i].clone())).collect();
            j.search_started = Some((Instant::now(), todo.len()));
            todo
        });
        let mut results = stream::iter(todo)
            .map(|(i, t)| async move {
                if let Err(e) = self.checkpoint().await {
                    return (i, Err(e));
                }
                (i, search_song(api, &t).await)
            })
            .buffer_unordered(CONCURRENCY);
        let (mut failed, mut last_save, mut last_wake) = (0, Instant::now(), Instant::now());
        while let Some((i, res)) = results.next().await {
            self.checkpoint().await?;
            let result = match res {
                Ok(Some((t, true))) => Search::Found(t),
                Ok(Some((t, false))) => Search::Unsure(t),
                Ok(None) => Search::Missing,
                Err(e) if signed_out(&e) => return Err(e.context("YouTube Music signed you out; sign in again and try again")),
                Err(e) => {
                    log::debug!("transfer search: {e:#}");
                    failed += 1;
                    Search::Pending
                }
            };
            self.with_job(|j| j.results[i] = result);
            if last_save.elapsed() > SAVE_EVERY {
                self.save();
                last_save = Instant::now();
            }
            // Repaint about 8 times a second, not once a song.
            if last_wake.elapsed() > Duration::from_millis(120) {
                self.wake();
                last_wake = Instant::now();
            }
        }
        self.save();
        self.wake();
        Ok(failed)
    }

    /// Create the playlist (parts of 5,000 for longer lists) and add the songs a batch at
    /// a time. Progress is saved after each step, so a retry never makes a second
    /// playlist or adds a batch twice.
    async fn create_playlists(&self, api: &Api, title: &str, ids: &[String]) -> Result<Vec<String>> {
        const BATCH: usize = 200;
        let parts: Vec<&[String]> = ids.chunks(PLAYLIST_MAX).collect();
        self.with_job(|j| j.saving = (j.added.min(ids.len()), ids.len()));
        for (n, part) in parts.iter().enumerate() {
            let base = n * PLAYLIST_MAX;
            let id = match self.with_job(|j| j.created.get(n).cloned()) {
                Some(id) => id,
                None => {
                    self.checkpoint().await?;
                    let part_title = if parts.len() > 1 { format!("{title} ({})", n + 1) } else { title.to_owned() };
                    let first = &part[..part.len().min(BATCH)];
                    // Not retried here: one that timed out may still have made the playlist.
                    let id = ytm::create_playlist_with(api, &part_title, first).await.context("Creating the playlist failed")?;
                    self.with_job(|j| {
                        j.created.push(id.clone());
                        j.added = base + first.len();
                        j.saving.0 = j.added;
                    });
                    self.save();
                    self.wake();
                    id
                }
            };
            loop {
                let done = self.with_job(|j| j.added.saturating_sub(base)).min(part.len());
                if done == part.len() {
                    break;
                }
                self.checkpoint().await?;
                let batch = &part[done..(done + BATCH).min(part.len())];
                // Adding skips songs already in the playlist, so a repeated batch is harmless.
                retry(|| ytm::add_videos(api, &id, batch)).await.context("Adding songs to the playlist failed")?;
                self.with_job(|j| {
                    j.added = base + done + batch.len();
                    j.saving.0 = j.added;
                });
                self.save();
                self.wake();
            }
        }
        Ok(self.with_job(|j| j.created.clone()))
    }

    /// Like every song that isn't liked yet, oldest first, so Liked Music ends up in
    /// Spotify's order (newest at the top). Returns how many were liked before the
    /// transfer. Resuming skips what's liked by then, this transfer's likes included.
    async fn like_all(&self, api: &Api, ids: &[String]) -> Result<usize> {
        let liked: HashSet<String> = retry(|| ytm::playlist(api, "LM", true, Some(20_000))).await.context("Reading your Liked Music failed")?.tracks.into_iter().map(|t| t.id).collect();
        let todo: Vec<&String> = ids.iter().rev().filter(|id| !liked.contains(*id)).collect();
        let earlier = self.with_job(|j| {
            j.saving = (j.liked, j.liked + todo.len());
            j.liked
        });
        let already = (ids.len() - todo.len()).saturating_sub(earlier);
        let mut last_save = Instant::now();
        for id in todo {
            self.checkpoint().await?;
            let mut tries = 0;
            loop {
                tries += 1;
                match ytm::set_like(api, id, true).await {
                    Ok(()) => break,
                    Err(e) if signed_out(&e) => return Err(e.context("YouTube Music signed you out; sign in again and try again")),
                    Err(e) if tries < 4 => {
                        log::debug!("like {id}: {e:#}");
                        tokio::time::sleep(Duration::from_millis(800 * tries)).await;
                    }
                    Err(e) => return Err(e.context("Liking songs failed")),
                }
            }
            self.with_job(|j| {
                j.liked += 1;
                j.saving.0 = j.liked;
            });
            if last_save.elapsed() > SAVE_EVERY {
                self.save();
                last_save = Instant::now();
            }
            self.wake();
        }
        self.save();
        Ok(already)
    }
}

impl Source {
    fn parse_key(key: &str) -> Self {
        if key == "liked" { Source::Liked } else { Source::Playlist(key.to_owned()) }
    }
}

/// A signed-in request, tried up to three times: YouTube Music now and then refuses one
/// (a 403 or a timeout) and takes the same request a moment later.
async fn retry<T, F: std::future::Future<Output = Result<T>>>(mut f: impl FnMut() -> F) -> Result<T> {
    let mut tries = 0;
    loop {
        tries += 1;
        match f().await {
            Err(e) if tries < 3 && !signed_out(&e) => {
                log::debug!("transfer: {e:#}; retrying");
                tokio::time::sleep(Duration::from_millis(1000 * tries)).await;
            }
            r => return r,
        }
    }
}

fn signed_out(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<crate::ytmapi::ApiError>().is_some_and(|a| a.status == 401))
}

/// What to search for, best first: the plain title and the main artist, the way a
/// person types it ("GO The Kid LAROI"), then everything Spotify lists. YouTube Music
/// answers a query full of "(feat. ...)" and every featured artist with the featured
/// artist's other songs, not the song.
fn queries(t: &Track) -> Vec<String> {
    let title = plain_title(&t.name);
    let first = t.artists.first().map(String::as_str).unwrap_or_default();
    let mut out = vec![format!("{title} {first}").trim().to_owned()];
    let full = format!("{} {}", t.name, t.artists.join(" ")).trim().to_owned();
    if !out.contains(&full) {
        out.push(full);
    }
    out
}

/// The title without "(feat. ...)", "[Remastered]" or " - 2011 Remaster".
fn plain_title(s: &str) -> &str {
    let cut = s.find(['(', '[']).into_iter().chain(s.find(" - ")).min().unwrap_or(s.len());
    match s[..cut].trim() {
        "" => s.trim(),
        t => t,
    }
}

/// The best song result for a Spotify track, and whether it's surely the same song.
/// YouTube's top hit is sometimes another song by the same artist or someone's cover,
/// so the first five of each search are ranked by title and artist and the top hit only
/// wins ties. The fuller query runs only when the plain one finds nothing sure.
async fn search_song(api: &Api, t: &Track) -> Result<Option<(YtTrack, bool)>> {
    let mut hits: Vec<YtTrack> = Vec::new();
    for q in queries(t) {
        let found = ytm::search_tracks_signed_in(api, &q, ytm::Filter::Songs).await?;
        hits.extend(found.into_iter().take(5).filter(|h| !hits.iter().any(|x| x.id == h.id)).collect::<Vec<_>>());
        if let Some(b) = best(&hits, t).filter(|(_, sure)| *sure) {
            return Ok(Some(b));
        }
    }
    Ok(best(&hits, t))
}

fn best(hits: &[YtTrack], t: &Track) -> Option<(YtTrack, bool)> {
    // max_by_key keeps the last of equal scores, so walk from the bottom up.
    let hit = hits.iter().rev().max_by_key(|h| score(h, t))?;
    Some((hit.clone(), matches(hit, t)))
}

/// What to offer for a doubtful song: the top songs, likeliest first, then videos.
async fn candidates(api: &Api, t: &Track) -> Result<Vec<(YtTrack, bool)>> {
    let q = queries(t).swap_remove(0);
    let (songs, videos) = tokio::try_join!(ytm::search_tracks_signed_in(api, &q, ytm::Filter::Songs), ytm::search_tracks_signed_in(api, &q, ytm::Filter::Videos))?;
    let mut out: Vec<(YtTrack, bool)> = Vec::new();
    for (hit, video) in songs.into_iter().take(5).map(|s| (s, false)).chain(videos.into_iter().take(3).map(|v| (v, true))) {
        // The same song is often listed twice, from the album and a single.
        let label = |t: &YtTrack| (t.title.to_lowercase(), t.artist_line().to_lowercase());
        if !out.iter().any(|(h, _)| h.id == hit.id || (!video && label(h) == label(&hit))) {
            out.push((hit, video));
        }
    }
    // Likeliest songs first; videos stay last, in YouTube's order.
    out.sort_by_key(|(h, video)| (*video, if *video { 0 } else { -score(h, t) }));
    Ok(out)
}

/// Whether a result is surely the track: its title, one of its artists (or, when YouTube
/// writes the artist in another script, the same length to a few seconds), and not a
/// recording of a very different length.
fn matches(hit: &YtTrack, t: &Track) -> bool {
    let same_length = gap(hit, t).is_some_and(|g| g <= 3);
    title_matches(hit, t) && !too_long_or_short(hit, t) && (artist_matches(hit, t) || (same_length && other_script(hit, t)))
}

fn title_matches(hit: &YtTrack, t: &Track) -> bool {
    title_score(hit, t) > 0
}

/// 4 for the same title, 3 for one a letter or so off ("Mais Que Nada" for "Mas Que
/// Nada"), 2 when one holds the other. YouTube writes Japanese and other titles as
/// "はつこい - Hatsukoi": either half may be the Spotify title.
fn title_score(hit: &YtTrack, t: &Track) -> i32 {
    let want = base_title(&t.name);
    let parts = std::iter::once(hit.title.as_str()).chain(hit.title.split(" - "));
    parts
        .map(base_title)
        .map(|got| match () {
            _ if want.is_empty() || got.is_empty() => 0,
            _ if want == got => 4,
            _ if similar(&want, &got) => 3,
            _ if want.contains(&got) || got.contains(&want) => 2,
            _ => 0,
        })
        .max()
        .unwrap_or(0)
}

/// At most one edit per seven letters, for titles of six letters or more.
fn similar(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let longest = a.len().max(b.len());
    if a.len().min(b.len()) < 6 || a.len().abs_diff(b.len()) * 7 > longest {
        return false;
    }
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut diag = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let next = (diag + (ca != cb) as usize).min(row[j] + 1).min(row[j + 1] + 1);
            diag = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()] * 7 <= longest
}

fn artist_matches(hit: &YtTrack, t: &Track) -> bool {
    t.artists.iter().any(|a| same_artist(hit, a))
}

/// Whether a result credits this artist: by name, give or take "The" and a typo, or as
/// part of a group ("The Miracles" for "Smokey Robinson & The Miracles").
fn same_artist(hit: &YtTrack, artist: &str) -> bool {
    let name = |s: &str| {
        let s = s.trim();
        norm(s.get(..4).filter(|p| p.eq_ignore_ascii_case("the ")).map_or(s, |_| &s[4..]))
    };
    let (want, line) = (name(artist), norm(&hit.artist_line()));
    let members: Vec<String> = artist.split(" & ").flat_map(|p| p.split(" and ")).flat_map(|p| p.split(", ")).map(name).collect();
    !want.is_empty()
        && (line.contains(&want)
            || hit.artists.iter().map(|a| name(&a.name)).any(|got| similar(&want, &got) || (members.len() > 1 && members.contains(&got))))
}

/// One side names the artist in Latin letters and the other doesn't ("Junko Yagami" and
/// "八神純子"), so the names can't be compared.
fn other_script(hit: &YtTrack, t: &Track) -> bool {
    let latin = |s: &str| s.chars().any(|c| c.is_ascii_alphabetic());
    latin(&t.artist_line()) != latin(&hit.artist_line())
}

/// How many seconds apart the two lengths are, when both are known.
fn gap(hit: &YtTrack, t: &Track) -> Option<u32> {
    Some(hit.duration?.abs_diff(t.duration?))
}

/// A different recording (a live take, an extended mix), not just another master.
fn too_long_or_short(hit: &YtTrack, t: &Track) -> bool {
    gap(hit, t).is_some_and(|g| g > 30.max(t.duration.unwrap_or(0) / 5))
}

/// Lowercase alphanumerics, with "(feat. ...)" / " - Remastered" style suffixes cut off.
/// A title that starts with brackets keeps them: "(Sittin' On) the Dock of the Bay".
fn base_title(s: &str) -> String {
    let s = s.trim();
    let from = if s.starts_with(['(', '[']) { s.find([')', ']']).map_or(s.len(), |i| i + 1) } else { 0 };
    let rest = &s[from..];
    let cut = rest.find(['(', '[']).into_iter().chain(rest.find(" - ")).min().map_or(s.len(), |i| from + i);
    norm(&s[..cut])
}

/// Lowercase letters and digits, with accents dropped ("RŮDE" is "rude") and full-width
/// forms made plain. Japanese voicing marks stay: they change the word.
fn norm(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    s.replace('&', "and").nfkd().filter(|c| !('\u{300}'..='\u{36f}').contains(c) && c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Versions that aren't the original recording unless the Spotify title says so.
const VARIANTS: &[&str] = &[
    "remix", "edit", "mix", "live", "karaoke", "instrumental", "cover", "sped up", "slowed", "acoustic", "version", "made famous", "originally performed", "in the style of", "tribute", "tv size",
];

fn score(hit: &YtTrack, t: &Track) -> i32 {
    let title = title_score(hit, t);
    let mut score = title;
    for (i, artist) in t.artists.iter().enumerate() {
        if same_artist(hit, artist) {
            score += if i == 0 { 3 } else { 1 };
        }
    }
    // The length only says something about a song with the title (or the title in another script).
    score += match gap(hit, t) {
        Some(0..=3) if title > 0 => 3,
        Some(4..=10) if title > 0 => 1,
        _ if too_long_or_short(hit, t) => -3,
        _ => 0,
    };
    // A remix on Spotify should find that remix, and an original shouldn't find one.
    let (spotify, youtube) = (t.name.to_lowercase(), hit.title.to_lowercase());
    for v in VARIANTS {
        score += match (youtube.contains(v), spotify.contains(v)) {
            (true, false) => -1,
            (true, true) => 1,
            _ => 0,
        };
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Link;

    fn hit(title: &str, artists: &str) -> YtTrack {
        YtTrack {
            id: title.into(),
            title: title.into(),
            artists: vec![Link { name: artists.into(), id: None }],
            album: None,
            duration: None,
            thumbs: vec![],
            track_nr: None,
            plays: None,
        }
    }

    fn track(name: &str, artists: &[&str]) -> Track {
        Track { name: name.into(), artists: artists.iter().map(|a| a.to_string()).collect(), local: false, art: None, duration: None }
    }

    #[test]
    fn ranking() {
        let pick = |t: &Track, hits: Vec<YtTrack>| best(&hits, t).unwrap().0.id;
        // Another song by the same artist on top.
        let t = track("One Dance", &["Drake", "Wizkid", "Kyla"]);
        assert_eq!(pick(&t, vec![hit("Passionfruit", "Drake"), hit("One Dance", "Drake, Wizkid & Kyla")]), "One Dance");
        // A cover or edit by someone else.
        let t = track("No Lie", &["Sean Paul", "Dua Lipa"]);
        assert_eq!(pick(&t, vec![hit("No Lie (Relax Edit)", "Release"), hit("No Lie", "Sean Paul & Dua Lipa")]), "No Lie");
        // Ties go to YouTube's own order.
        let t = track("Numb", &["Linkin Park"]);
        let (a, mut b) = (hit("Numb", "Linkin Park"), hit("Numb", "Linkin Park"));
        b.id = "second".into();
        assert_eq!(pick(&t, vec![a, b]), "Numb");
        // Only a different song by the same artist: picked, but flagged for review.
        let t = track("Chimp", &["Juice WRLD"]);
        assert!(!best(&[hit("Lucid Dreams", "Juice WRLD")], &t).unwrap().1);
        assert!(best(&[hit("Chimp", "Juice WRLD")], &t).unwrap().1);
        // Original-script titles with the romanized one after a dash, and "&" for "and".
        assert!(best(&[hit("死ぬのがいいわ - Shinunoga E-Wa", "Fujii Kaze")], &track("Shinunoga E-Wa", &["Fujii Kaze"])).unwrap().1);
        let t = track("Silk & Cologne (EI8HT & Offset) - Spider-Verse Remix", &["EI8HT", "Offset"]);
        assert!(best(&[hit("Silk and Cologne (Spider-Verse Remix)", "EI8HT & Offset")], &t).unwrap().1);
    }

    fn timed(mut h: YtTrack, id: &str, secs: u32) -> YtTrack {
        h.id = id.into();
        h.duration = Some(secs);
        h
    }

    fn track_of(name: &str, artists: &[&str], secs: u32) -> Track {
        Track { duration: Some(secs), ..track(name, artists) }
    }

    #[test]
    fn ranking_with_lengths() {
        let pick = |t: &Track, hits: Vec<YtTrack>| best(&hits, t).map(|(h, sure)| (h.id, sure)).unwrap();
        // The 1967 single, not a 4:35 compilation recording or a smooth-jazz cover.
        let t = track_of("Respect", &["Aretha Franklin"], 147);
        let hits = vec![timed(hit("Respect", "Aretha Franklin"), "long", 275), timed(hit("Respect", "Smooth Jazz All Stars"), "cover", 180), timed(hit("Respect (Mono)", "Aretha Franklin"), "mono", 146)];
        assert_eq!(pick(&t, hits), ("mono".into(), true));
        // Only a recording of a very different length: picked, but flagged.
        assert!(!best(&[timed(hit("Respect", "Aretha Franklin"), "long", 275)], &t).unwrap().1);
        // A spelling a letter off by the right artist beats the exact title by someone else.
        let t = track_of("Mas Que Nada", &["Sergio Mendes & Brasil '66"], 159);
        let hits = vec![timed(hit("Mas Que Nada", "Styline, Raul Mendes"), "styline", 275), timed(hit("Mais Que Nada", "Sergio Mendes & Brasil '66"), "real", 159)];
        assert_eq!(pick(&t, hits), ("real".into(), true));
        // Accents: "RŮDE" is "Rude.".
        assert!(best(&[hit("Loneliness", "Rude.")], &track("Loneliness", &["RŮDE"])).unwrap().1);
        // The artist in another script, with the same length.
        let t = track_of("夜空のイヤリング", &["Junko Yagami"], 238);
        assert_eq!(pick(&t, vec![timed(hit("夜空のイヤリング", "八神純子"), "a", 239)]), ("a".into(), true));
        // ...but not someone else's song of the same name and length in the same script.
        let t = track_of("Come To Me", &["ITSA THEIS"], 250);
        assert!(!pick(&t, vec![timed(hit("Come To Me", "itsnottheis"), "a", 251)]).1);
        // A cover with the English title vs the artist's own "原題 - English title".
        let t = track_of("Fleeting Lullaby", &["Ado"], 173);
        let hits = vec![timed(hit("Fleeting Lullaby (from \"One Piece Film: Red\")", "AmaLee"), "cover", 173), timed(hit("ウタカタララバイ - Fleeting Lullaby", "Ado"), "ado", 174)];
        assert_eq!(pick(&t, hits), ("ado".into(), true));
        // The remix Spotify names, not the original.
        let t = track_of("You're My Angel - Hypnose Remix Edit", &["Styles & Breeze", "Hypnose"], 170);
        let hits = vec![timed(hit("You're My Angel", "Styles & Breeze"), "orig", 315), timed(hit("You're My Angel (Hypnose Remix Edit)", "Styles & Breeze"), "remix", 171)];
        assert_eq!(pick(&t, hits), ("remix".into(), true));
        // A length alone doesn't make an unrelated song likelier than the artist's others.
        let t = track_of("Friends Die", &["F1guss"], 205);
        let hits = vec![timed(hit("Burn", "F1guss"), "same artist", 218), timed(hit("Stolen", "ABress"), "same length", 205)];
        assert_eq!(pick(&t, hits), ("same artist".into(), false));
    }

    #[test]
    fn bracketed_titles_and_groups() {
        assert_eq!(base_title("(Sittin' On) the Dock of the Bay"), "sittinonthedockofthebay");
        assert_eq!(base_title("(Love Is Like A) Heat Wave (Mono) - 2005 Remaster"), "loveislikeaheatwave");
        assert_eq!(base_title("Respect (Mono)"), "respect");
        assert!(best(&[hit("I Left My Heart in San Francisco", "Tony Bennett")], &track("(I Left My Heart) In San Francisco", &["Tony Bennett"])).unwrap().1);
        assert!(best(&[hit("Wonderful World", "Sam Cooke")], &track("(What A) Wonderful World", &["Sam Cooke"])).unwrap().1);
        assert!(best(&[hit("Who's Lovin' You", "Jackson 5")], &track("Who's Lovin' You", &["The Jackson 5"])).unwrap().1);
        assert!(best(&[hit("The Tracks of My Tears", "The Miracles")], &track("The Tracks Of My Tears", &["Smokey Robinson & The Miracles"])).unwrap().1);
        assert!(best(&[hit("A Natural Woman", "Aretha Frankiln")], &track("A Natural Woman", &["Aretha Franklin"])).unwrap().1);
        // Someone else whose name is in the group's isn't the group.
        assert!(!best(&[hit("Black Magic Woman", "Madeaux")], &track("Black Magic Woman", &["Santana"])).unwrap().1);
        assert!(!best(&[hit("Found a Way", "Drake")], &track("Found a Way", &["Drake Bell"])).unwrap().1);
    }

    #[test]
    fn near_titles() {
        assert!(similar("masquenada", "maisquenada"));
        assert!(!similar("loveme", "loveyou"));
        assert!(!similar("numb", "numa"));
    }

    #[test]
    fn plain_queries() {
        assert_eq!(queries(&track("GO (feat. Juice WRLD)", &["The Kid LAROI", "Juice WRLD"]))[0], "GO The Kid LAROI");
        assert_eq!(queries(&track("Drunk And Nasty (feat. Sharc)", &["Pi'erre Bourne", "Sharc"]))[0], "Drunk And Nasty Pi'erre Bourne");
        assert_eq!(queries(&track("Numb", &["Linkin Park"])), vec!["Numb Linkin Park"]);
        assert_eq!(plain_title("Wonderwall - Remastered"), "Wonderwall");
        assert_eq!(plain_title("(What's the Story) Morning Glory?"), "(What's the Story) Morning Glory?");
    }

    #[test]
    fn likes_playlists() {
        for name in ["Liked Songs", "likes", " Likes ", "LIKED SONGS"] {
            assert!(is_likes(name), "{name}");
        }
        for name in ["Liked Songs 2", "Vibe", "My likes mix", "Liked Music"] {
            assert!(!is_likes(name), "{name}");
        }
    }
}

#[cfg(test)]
mod live {
    /// The songs that came back doubtful with the old query: `cargo test --release -- --ignored matching_live --nocapture`
    #[tokio::test]
    #[ignore]
    async fn matching_live() {
        let dir = directories::ProjectDirs::from("", "", "ytm-rs").unwrap().data_dir().to_path_buf();
        let api = crate::ytmapi::Api::new(&dir);
        assert!(api.account().is_some(), "not signed in");
        let songs: &[(&str, &[&str])] = &[
            ("GO (feat. Juice WRLD)", &["The Kid LAROI", "Juice WRLD"]),
            ("Drunk And Nasty (feat. Sharc)", &["Pi'erre Bourne", "Sharc"]),
            ("Miss The Rage (feat. Playboi Carti)", &["Trippie Redd", "Playboi Carti"]),
            ("Nuketown (feat. Juice WRLD)", &["Ski Mask The Slump God", "Juice WRLD"]),
            ("One Dance", &["Drake", "Wizkid", "Kyla"]),
            ("Shinunoga E-Wa", &["Fujii Kaze"]),
        ];
        let mut sure = 0;
        for (name, artists) in songs {
            let t = super::Track { name: name.to_string(), artists: artists.iter().map(|a| a.to_string()).collect(), local: false, art: None, duration: None };
            let r = super::search_song(&api, &t).await.unwrap();
            println!("{name} -> {:?}", r.as_ref().map(|(h, s)| (h.title.clone(), h.artist_line(), *s)));
            sure += r.is_some_and(|(_, s)| s) as usize;
        }
        assert_eq!(sure, songs.len());
    }

    /// `YTM_DATA_DIR=<profile> cargo test --release -- --ignored liked_music_live --nocapture`
    #[tokio::test]
    #[ignore]
    async fn liked_music_live() {
        let dir = std::path::PathBuf::from(std::env::var("YTM_DATA_DIR").unwrap());
        let api = crate::ytmapi::Api::new(&dir);
        assert!(api.account().is_some(), "no session in the vault");
        for limit in [100, 1000, 20_000] {
            let t = std::time::Instant::now();
            match crate::ytm::playlist(&api, "LM", true, Some(limit)).await {
                Ok(p) => println!("limit {limit}: {} tracks in {:?}", p.tracks.len(), t.elapsed()),
                Err(e) => println!("limit {limit}: {e:#}"),
            }
        }
    }
}
