//! Parallel, seek-aware HTTP audio buffer.
//!
//! The file is split into fixed blocks fetched by a few concurrent range requests.
//! Workers always claim the first missing block at or after the one a reader is
//! waiting on, so playback start and seeks are served first and the rest fills in
//! around them. Reads of a missing block block until it arrives (or the download fails).
//!
//! An HLS stream (`Media::Segments`) is buffered the same way, a segment per block.
//!
//! Several readers may share one buffer (the audio engine opens a second, seekable
//! decoder when the user seeks); the download stops once the last reader is dropped.

use std::future::Future;
use std::io::{self, Read, Seek, SeekFrom};
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use bytes::Bytes;

const BLOCK: u64 = 256 * 1024;
/// The first block is smaller: a decoder opens after a few hundred bytes, so playback
/// waits on this one alone, and it still holds seconds of audio while the next arrive.
const FIRST: u64 = 64 * 1024;
const WORKERS: usize = 4;
/// Tries per block before the download gives up, waiting a little longer each time
/// (about half a minute in all): long enough to ride out a dropped connection.
const RETRIES: u32 = 6;
/// A response that delivers nothing for this long is dead; a slow one that keeps
/// delivering is fine, however long the block takes.
const STALL: Duration = Duration::from_secs(20);
/// googlevideo occasionally starts rejecting a URL partway through (HTTP 403);
/// a freshly resolved URL for the same video resumes the download.
const MAX_REFRESHES: u32 = 2;
/// A prefetch only fetches this much (~10 s of audio at 256 kbps, two HLS segments)
/// until playback actually opens it: enough to start at once, without holding a whole
/// extra song in memory.
const HEAD_BLOCKS: usize = 2;
/// Files larger than this (about 20 minutes at 256 kbps: long mixes, whole albums in
/// one video) aren't kept whole. Each reader keeps a window around where it reads,
/// `AHEAD` blocks forward (about 5 minutes at 256 kbps) and `BEHIND` back; the rest is dropped and
/// fetched again if a seek goes back to it.
const WINDOWED_FROM: u64 = 40 * 1024 * 1024;
const AHEAD: usize = 40;
const BEHIND: usize = 8;

/// Block `idx`'s first byte in a file.
fn block_start(idx: usize) -> u64 {
    if idx == 0 { 0 } else { FIRST + (idx as u64 - 1) * BLOCK }
}

/// The block holding byte `pos` of a file.
fn block_of(pos: u64) -> usize {
    if pos < FIRST { 0 } else { 1 + ((pos - FIRST) / BLOCK) as usize }
}

/// How many blocks a file of `total` bytes takes.
fn block_count(total: u64) -> usize {
    if total == 0 { 0 } else { block_of(total - 1) + 1 }
}

/// What a download fetches.
pub enum Media {
    /// One file, in ranges: `FIRST` bytes, then `BLOCK`s.
    File(String),
    /// A podcast feed's file on its own server: the same ranges, asked for in a
    /// `Range` header (googlevideo's `&range=` means nothing there).
    Feed(String),
    /// An HLS playlist's segments, one block each.
    Segments(Vec<String>),
    /// HLS segments carrying audio and video in MPEG-TS. Only the AAC is buffered.
    TransportStream(Vec<String>),
}

/// Resolves fresh URLs for the same video and format.
pub type Refresh = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = anyhow::Result<Media>> + Send>> + Send + Sync>;

pub struct DownloadFailure {
    pub reason: String,
    pub offer_backup: bool,
}

struct Inner {
    blocks: Vec<Option<Bytes>>,
    in_flight: Vec<bool>,
    /// Block a reader most recently needed; workers prioritize from here.
    want: usize,
    error: Option<String>,
    /// The download stopped on HTTP 403s that fresh URLs didn't get past.
    refused: bool,
    after_bot_check: bool,
    readers: usize,
    /// Set once a reader existed and all were dropped.
    cancelled: bool,
    /// Blocks the workers may fetch; lifted to all of them when a reader opens.
    limit: usize,
    /// Each open reader's id and the block it last read (windowed files keep only
    /// what's near these).
    wants: Vec<(u64, usize)>,
    windowed: bool,
    /// Blocks fetched at least once, and how many: the download counts as finished
    /// once each was, even if a windowed file has dropped some since.
    ever: Vec<bool>,
    fetched: usize,
}

impl Inner {
    /// Whether block `i` belongs in memory now.
    fn in_window(&self, i: usize) -> bool {
        !self.windowed || self.wants.is_empty() || self.wants.iter().any(|&(_, w)| i + BEHIND >= w && i < w + AHEAD)
    }
}

enum Claim {
    Block(usize),
    /// Everything allowed so far is fetched or in flight; wait for the limit to lift.
    Wait,
    Done,
}

/// Stream buffers alive, for the `YTM_MEM` dev log.
pub static LIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl Drop for Shared {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

pub struct Shared {
    inner: Mutex<Inner>,
    ready: Condvar,
    pub total: u64,
    /// Track length reported by the stream metadata (0 if unknown).
    pub duration_ms: u64,
    /// YouTube's loudness for this stream, in dB above its -14 LUFS target.
    pub loudness_db: Option<f32>,
    /// Opus in WebM; otherwise AAC in MP4, or ADTS in HLS segments.
    pub opus: bool,
    /// A podcast feed's file (`Media::Feed`): MP3 mostly, which seeks by estimate,
    /// since an exact seek in MP3 reads every frame before the spot.
    pub feed: bool,
    /// HLS: each segment's start (s). Its blocks are the segments.
    pub segments: Option<Arc<[f64]>>,
    /// Flips to true once the download task has stopped (complete, failed or cancelled).
    done: tokio::sync::watch::Sender<bool>,
    /// Wakes waiting workers: `limit` lifted, a reader moved or left, or stopped.
    changed: tokio::sync::Notify,
    /// Set when the download fails, for lock-free checks on the audio callback thread.
    failed: Arc<std::sync::atomic::AtomicBool>,
}

impl Shared {
    /// A buffer for `total` bytes. `head_only` (a prefetch) fetches just the start until
    /// a reader opens it.
    pub fn new(total: u64, duration_ms: u64, loudness_db: Option<f32>, opus: bool, head_only: bool) -> Arc<Self> {
        Self::build(block_count(total), total, total > WINDOWED_FROM, duration_ms, loudness_db, opus, false, None, head_only)
    }

    /// A buffer for a podcast feed's file of `total` bytes.
    pub fn feed(total: u64, duration_ms: u64, head_only: bool) -> Arc<Self> {
        Self::build(block_count(total), total, total > WINDOWED_FROM, duration_ms, None, false, true, None, head_only)
    }

    /// A buffer for an HLS stream's segments, starting at `starts` (s).
    pub fn segmented(starts: Vec<f64>, duration_ms: u64, loudness_db: Option<f32>, head_only: bool) -> Arc<Self> {
        // About as many segments (~7 s) as a windowed file has blocks (~8 s at 256 kbps).
        let windowed = starts.len() as u64 > WINDOWED_FROM / BLOCK;
        Self::build(starts.len(), 0, windowed, duration_ms, loudness_db, false, false, Some(starts.into()), head_only)
    }

    #[allow(clippy::too_many_arguments)]
    fn build(n: usize, total: u64, windowed: bool, duration_ms: u64, loudness_db: Option<f32>, opus: bool, feed: bool, segments: Option<Arc<[f64]>>, head_only: bool) -> Arc<Self> {
        let limit = if head_only { HEAD_BLOCKS.min(n) } else { n };
        LIVE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Arc::new(Self {
            inner: Mutex::new(Inner {
                blocks: vec![None; n],
                in_flight: vec![false; n],
                want: 0,
                error: None,
                refused: false,
                after_bot_check: false,
                readers: 0,
                cancelled: false,
                limit,
                wants: Vec::new(),
                windowed,
                ever: vec![false; n],
                fetched: 0,
            }),
            ready: Condvar::new(),
            total,
            duration_ms,
            loudness_db,
            opus,
            feed,
            segments,
            done: tokio::sync::watch::Sender::new(false),
            changed: tokio::sync::Notify::new(),
            failed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    /// A buffer that already holds the whole file (a downloaded song).
    pub fn from_bytes(data: Vec<u8>, duration_ms: u64, loudness_db: Option<f32>) -> Arc<Self> {
        let total = data.len() as u64;
        let data = Bytes::from(data);
        // Saved songs are AAC (downloads take that stream).
        let shared = Self::new(total, duration_ms, loudness_db, false, false);
        let n = shared.inner.lock().unwrap().blocks.len();
        let blocks: Vec<Option<Bytes>> = (0..n).map(|i| Some(data.slice(block_start(i) as usize..block_start(i + 1).min(total) as usize))).collect();
        {
            let mut inner = shared.inner.lock().unwrap();
            inner.blocks = blocks;
            inner.windowed = false;
            inner.ever = vec![true; n];
            inner.fetched = n;
        }
        shared.done.send_replace(true);
        shared
    }

    /// The whole file, once every block has arrived (a download nobody's reading).
    pub fn bytes(&self) -> Option<Vec<u8>> {
        let inner = self.inner.lock().unwrap();
        let mut out = Vec::with_capacity(self.total as usize);
        for b in &inner.blocks {
            out.extend_from_slice(b.as_ref()?);
        }
        Some(out)
    }

    /// Every block is in memory: readers never wait and the buffer can be replayed.
    pub fn is_complete(&self) -> bool {
        self.inner.lock().unwrap().blocks.iter().all(Option::is_some)
    }

    /// How much of the song's start is in memory, unbroken: reading that far won't wait
    /// on the download. (Estimated from its bytes, outside HLS.)
    pub fn head_in_memory(&self) -> Duration {
        let inner = self.inner.lock().unwrap();
        let n = inner.blocks.iter().take_while(|b| b.is_some()).count();
        let secs = match &self.segments {
            _ if n == inner.blocks.len() => self.duration_ms as f64 / 1000.0,
            Some(starts) => starts.get(n).copied().unwrap_or(0.0),
            None => block_start(n) as f64 / self.total.max(1) as f64 * self.duration_ms as f64 / 1000.0,
        };
        Duration::from_secs_f64(secs.max(0.0))
    }

    /// Wait up to `max_wait` for the first `HEAD_BLOCKS` (~320 KB / ~10 s at 256 kbps, or
    /// two HLS segments) to land in memory, so a staged gapless track doesn't stall the
    /// audio callback on the network right after handoff.
    pub fn warm_head(&self, max_wait: Duration) {
        let deadline = std::time::Instant::now() + max_wait;
        let mut inner = self.inner.lock().unwrap();
        let need = HEAD_BLOCKS.min(inner.blocks.len());
        while !inner.cancelled && inner.error.is_none() && (0..need).any(|i| inner.blocks[i].is_none()) {
            let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) else {
                break;
            };
            let (guard, res) = self.ready.wait_timeout(inner, left).unwrap();
            inner = guard;
            if res.timed_out() {
                break;
            }
        }
    }

    /// Wait until the download stops, however it ends.
    pub async fn finished(&self) {
        let _ = self.done.subscribe().wait_for(|d| *d).await;
    }

    /// Why the download stopped early, if it failed.
    pub fn error(&self) -> Option<String> {
        self.inner.lock().unwrap().error.clone()
    }

    /// The download failed on HTTP 403s that fresh URLs didn't get past.
    pub fn refused(&self) -> bool {
        self.inner.lock().unwrap().refused
    }

    pub fn mark_after_bot_check(&self) {
        self.inner.lock().unwrap().after_bot_check = true;
    }

    pub fn failure(&self) -> Option<DownloadFailure> {
        let inner = self.inner.lock().unwrap();
        Some(DownloadFailure {
            reason: inner.error.as_ref()?.clone(),
            offer_backup: inner.after_bot_check && inner.refused,
        })
    }

    /// A block a reader may need now is still to come (a windowed file counts only its
    /// window): downloads in the background make way for it.
    pub fn filling(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        !inner.cancelled && inner.error.is_none() && (0..inner.limit).any(|i| inner.blocks[i].is_none() && inner.in_window(i))
    }

    /// Stop downloading (e.g. a prefetch that was superseded).
    pub fn cancel(&self) {
        self.inner.lock().unwrap().cancelled = true;
        self.ready.notify_all();
        self.changed.notify_waiters();
    }

    /// Interrupt all the workers, including requests and retries already in progress.
    async fn stopped(&self) {
        loop {
            // Register before checking, so a stop between the check and await is seen.
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let inner = self.inner.lock().unwrap();
                if inner.cancelled || inner.error.is_some() {
                    return;
                }
            }
            changed.await;
        }
    }

    fn claim(&self) -> Claim {
        let mut inner = self.inner.lock().unwrap();
        if inner.cancelled || inner.error.is_some() {
            return Claim::Done;
        }
        let n = inner.blocks.len();
        let start = inner.want.min(n.saturating_sub(1));
        let mut missing = false;
        for i in (start..n).chain(0..start) {
            if inner.blocks[i].is_some() || inner.in_flight[i] {
                continue;
            }
            missing = true;
            if i < inner.limit && inner.in_window(i) {
                inner.in_flight[i] = true;
                return Claim::Block(i);
            }
        }
        // A windowed file may need dropped blocks again while it's being read.
        if missing || (inner.windowed && inner.readers > 0) { Claim::Wait } else { Claim::Done }
    }

    fn put(&self, idx: usize, data: Bytes) {
        let mut inner = self.inner.lock().unwrap();
        inner.blocks[idx] = Some(data);
        inner.in_flight[idx] = false;
        if !std::mem::replace(&mut inner.ever[idx], true) {
            inner.fetched += 1;
            if inner.fetched == inner.blocks.len() {
                self.done.send_replace(true);
            }
        }
        if inner.windowed && !inner.wants.is_empty() {
            for i in 0..inner.blocks.len() {
                if inner.blocks[i].is_some() && !inner.in_window(i) {
                    inner.blocks[i] = None;
                }
            }
        }
        self.ready.notify_all();
    }

    /// Lock-free flag that flips to true if this download fails partway through.
    pub fn failed_flag(&self) -> Arc<std::sync::atomic::AtomicBool> {
        self.failed.clone()
    }

    fn fail(&self, err: String, refused: bool) {
        log::error!("download failed: {err}");
        FAIL_FROM.store(usize::MAX, std::sync::atomic::Ordering::Relaxed);
        self.failed.store(true, std::sync::atomic::Ordering::Release);
        let mut inner = self.inner.lock().unwrap();
        inner.error = Some(err);
        inner.refused = refused;
        drop(inner);
        self.ready.notify_all();
        self.changed.notify_waiters();
    }

    fn range(&self, idx: usize) -> (u64, u64) {
        (block_start(idx), block_start(idx + 1).min(self.total) - 1)
    }
}

/// The current URLs plus how many times they have been replaced.
struct UrlSlot {
    url: tokio::sync::Mutex<(Media, u32)>,
    refresh: Option<Refresh>,
    /// Segments the buffer has: fresh ones must be the same.
    segments: Option<usize>,
}

impl UrlSlot {
    /// Replace the URL if `seen` is still current; returns false when out of refreshes.
    async fn renew(&self, seen: u32) -> bool {
        let mut slot = self.url.lock().await;
        if slot.1 != seen {
            return true; // another worker already refreshed it
        }
        let Some(refresh) = self.refresh.as_ref() else { return false };
        if slot.1 >= MAX_REFRESHES {
            log::warn!("stream URL still refused after {MAX_REFRESHES} refreshes");
            return false;
        }
        match refresh().await {
            // The buffer is laid out for the stream it started with.
            Ok(Media::Segments(urls) | Media::TransportStream(urls)) if Some(urls.len()) != self.segments => {
                log::warn!("stream URL refresh: {} segments for a buffer of {:?}", urls.len(), self.segments);
                false
            }
            Ok(media) if std::mem::discriminant(&media) != std::mem::discriminant(&slot.0) => {
                log::warn!("stream URL refresh: the media type changed");
                false
            }
            Ok(media) => {
                log::info!("stream URL refreshed after 403");
                *slot = (media, slot.1 + 1);
                true
            }
            Err(e) => {
                log::warn!("stream URL refresh failed: {e:#}");
                false
            }
        }
    }

    /// Moves a stream that `tried_host` couldn't be reached on, or that sent nothing from
    /// there, to the other host its URLs name (`second_host`): a file's URL, or all of an
    /// HLS stream's segments. True when it's on another host now, by this or another
    /// worker's doing; false when there's none left to try.
    async fn fall_back(&self, tried_host: &str) -> bool {
        let mut slot = self.url.lock().await;
        let urls: &mut [String] = match &mut slot.0 {
            Media::File(url) => std::slice::from_mut(url),
            Media::Segments(urls) | Media::TransportStream(urls) => urls,
            Media::Feed(_) => return false,
        };
        if !urls.iter().any(|u| host_of(u) == Some(tried_host)) {
            return true;
        }
        // All of them or none: segments on two hosts would be two streams to keep track of.
        let Some(next) = urls.iter().map(|u| second_host(u)).collect::<Option<Vec<String>>>() else { return false };
        log::warn!("stream host unreachable or not answering, trying the other one YouTube named");
        urls.clone_from_slice(&next);
        true
    }
}

fn host_of(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    rest.split(['/', ':', '?']).next()
}

/// The same googlevideo URL on the second host YouTube names for it (#342), as its web
/// player falls back to: `mn` lists the server cluster of the host the URL is on and a
/// second one, `fvip` which server of that second one ("rr{fvip}---{mn[1]}"). `None` for
/// any other URL, and for one already there.
pub(crate) fn second_host(url: &str) -> Option<String> {
    let host = host_of(url)?;
    if !host.ends_with(".googlevideo.com") {
        return None;
    }
    // In the query ("&mn=…"), or in the path as HLS segments have them ("/mn/…/").
    let param = |name: &str| {
        let (path, query) = url.split_once('?').unwrap_or((url, ""));
        query.split('&').find_map(|p| p.strip_prefix(name)?.strip_prefix('=')).or_else(|| {
            let mut parts = path.split('/');
            parts.find(|p| *p == name)?;
            parts.next()
        })
    };
    // Commas come percent-encoded too ("sn-a%2Csn-b").
    let mn = param("mn")?.replace("%2C", ",").replace("%2c", ",");
    let cluster = mn.split(',').nth(1)?;
    let server = param("fvip")?;
    let fits = |s: &str, ok: fn(char) -> bool| !s.is_empty() && s.chars().all(ok);
    if !cluster.starts_with("sn-") || !fits(cluster, |c| c.is_ascii_alphanumeric() || c == '-') || !fits(server, |c| c.is_ascii_digit()) {
        return None;
    }
    let next = format!("rr{server}---{cluster}.googlevideo.com");
    (next != host).then(|| url.replacen(&format!("//{host}"), &format!("//{next}"), 1))
}

/// A host that can't be reached, or that took the request and sent nothing for `STALL`
/// (a broken cache, a blackholed address): the second host is worth a try at once.
fn unresponsive(e: &anyhow::Error) -> bool {
    e.is::<Stalled>() || e.downcast_ref::<reqwest::Error>().is_some_and(reqwest::Error::is_connect)
}

/// Nothing came for `STALL`.
#[derive(Debug)]
struct Stalled;

impl std::fmt::Display for Stalled {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "no data for {} s", STALL.as_secs())
    }
}

impl std::error::Error for Stalled {}

/// The buffer a background download makes way for (the playing song's), if any.
pub type Ahead = Arc<dyn Fn() -> Option<Arc<Shared>> + Send + Sync>;

/// Download into `shared` with parallel range requests; returns when complete,
/// failed, or every reader was dropped.
pub async fn download(http: reqwest::Client, media: Media, ua: String, shared: Arc<Shared>, refresh: Option<Refresh>) {
    download_behind(http, media, ua, shared, refresh, None).await;
}

/// `download`, but before each block it waits while `ahead`'s buffer still has blocks
/// to come that a reader may need: the playing song gets the bandwidth first.
pub async fn download_behind(http: reqwest::Client, media: Media, ua: String, shared: Arc<Shared>, refresh: Option<Refresh>, ahead: Option<Ahead>) {
    let segments = shared.segments.as_ref().map(|s| s.len());
    let transport_stream = matches!(&media, Media::TransportStream(_));
    // YouTube sometimes ends a Safari playlist with an empty segment shorter
    // than one AAC frame (e.g. 2 ms).
    let short_tail = transport_stream && shared.segments.as_ref().and_then(|starts| starts.last())
        .is_some_and(|&start| (shared.duration_ms as f64 - start * 1000.0) < 10.0);
    let slot = Arc::new(UrlSlot { url: tokio::sync::Mutex::new((media, 0)), refresh, segments });
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..WORKERS {
        let (http, ua, shared, slot, ahead) = (http.clone(), ua.clone(), shared.clone(), slot.clone(), ahead.clone());
        set.spawn(async move {
            loop {
                if let Some(ahead) = &ahead {
                    while ahead().is_some_and(|s| !Arc::ptr_eq(&s, &shared) && s.filling()) {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                }
                // Registered before claiming, so a change in between isn't missed.
                let changed = shared.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                let idx = match shared.claim() {
                    Claim::Block(idx) => idx,
                    Claim::Wait => {
                        changed.await;
                        continue;
                    }
                    Claim::Done => break,
                };
                let (a, b) = if segments.is_none() { shared.range(idx) } else { (0, 0) };
                let what = if segments.is_some() { format!("segment {idx}") } else { format!("range {a}-{b}") };
                let mut attempt = 0;
                loop {
                    let (url, range, version) = {
                        let slot = slot.url.lock().await;
                        match &slot.0 {
                            Media::File(url) => (format!("{url}&range={a}-{b}"), Range::Param(a, b), slot.1),
                            Media::Feed(url) => (url.clone(), Range::Header(a, b), slot.1),
                            Media::Segments(urls) | Media::TransportStream(urls) => (urls[idx].clone(), Range::Whole, slot.1),
                        }
                    };
                    // A refreshed URL is tied to the address that resolved it, which is the
                    // current shared client's (a proxy may have switched exits since).
                    let client = if version > 0 { crate::proxy::http() } else { http.clone() };
                    let res = if idx >= FAIL_FROM.load(std::sync::atomic::Ordering::Relaxed) {
                        Err(anyhow::anyhow!("simulated network failure"))
                    } else if segments.is_none() && idx >= REFUSE_FROM.load(std::sync::atomic::Ordering::Relaxed) {
                        Err(anyhow::Error::new(SimulatedRefusal))
                    } else {
                        fetch(&client, &url, &ua, range).await.and_then(|data| {
                            let empty_tail = short_tail && Some(idx + 1) == segments && data.is_empty();
                            if transport_stream && !empty_tail {
                                crate::hls::unpack_audio(&data)
                            } else {
                                Ok(data)
                            }
                        })
                    };
                    match res {
                        Ok(data) => {
                            shared.put(idx, data);
                            break;
                        }
                        Err(e) if is_forbidden(&e) && slot.renew(version).await => {}
                        // A host that can't be reached (one blocked by name) or that sends
                        // nothing (a broken ISP cache) won't do better on a retry: its URL's
                        // second host, at once.
                        Err(e) if unresponsive(&e) && slot.fall_back(host_of(&url).unwrap_or_default()).await => {
                            log::warn!("{what}: {e:#}");
                        }
                        Err(e) if attempt < RETRIES && !is_forbidden(&e) => {
                            attempt += 1;
                            log::warn!("{what}: {e:#} (retry {attempt})");
                            tokio::time::sleep(Duration::from_millis(400 * (attempt * attempt) as u64)).await;
                        }
                        Err(e) => {
                            // How many fresh URLs were tried tells a URL that keeps being
                            // refused (the player's URLs, the address) from a one-off.
                            let refreshed = slot.url.lock().await.1;
                            let refused = is_forbidden(&e);
                            let tail = if refused { format!(" after {refreshed} URL refresh(es)") } else { String::new() };
                            shared.fail(format!("{what}: {e:#}{tail}"), refused);
                            return;
                        }
                    }
                }
            }
        });
    }
    let stopped = shared.stopped();
    tokio::pin!(stopped);
    loop {
        tokio::select! {
            _ = &mut stopped => {
                // Waiting for the workers to finish by themselves can keep a skipped
                // song alive indefinitely (a server trickling data, a URL refresh).
                // Drain them before reporting completion, releasing their buffers too.
                set.shutdown().await;
                break;
            }
            next = set.join_next() => {
                if next.is_none() {
                    break;
                }
            }
        }
    }
    shared.done.send_replace(true);
}

fn is_forbidden(e: &anyhow::Error) -> bool {
    e.is::<SimulatedRefusal>()
        || e.downcast_ref::<reqwest::Error>()
            .and_then(reqwest::Error::status)
            .is_some_and(|s| s == reqwest::StatusCode::FORBIDDEN)
}

/// `YTM_STREAM_REFUSE`'s answer: what googlevideo says to a range it won't serve.
#[derive(Debug)]
struct SimulatedRefusal;

impl std::fmt::Display for SimulatedRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("HTTP status client error (403 Forbidden) (simulated)")
    }
}

impl std::error::Error for SimulatedRefusal {}

/// Dev hook: YTM_STREAM_FAIL=<block> makes the first download fail from that block on,
/// as if the connection died mid-song; later downloads work.
static FAIL_FROM: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(usize::MAX);

/// Dev hook: YTM_STREAM_REFUSE=<block> answers every file stream's ranges from that
/// block on with HTTP 403, fresh URLs too, as YouTube does past 1 MiB (in block 4) where
/// it wants a PO token for the visionOS player's URLs. HLS segments get through.
static REFUSE_FROM: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(usize::MAX);

pub fn init_dev_hooks() {
    if let Some(b) = std::env::var("YTM_STREAM_FAIL").ok().and_then(|v| v.parse().ok()) {
        FAIL_FROM.store(b, std::sync::atomic::Ordering::Relaxed);
    }
    if let Some(b) = std::env::var("YTM_STREAM_REFUSE").ok().and_then(|v| v.parse().ok()) {
        REFUSE_FROM.store(b, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The bytes `a`-`b` a request asks for, and how.
#[derive(Clone, Copy)]
enum Range {
    /// googlevideo's `&range=a-b`, already in the URL.
    Param(u64, u64),
    /// An HTTP `Range` header, which the server must honor (206).
    Header(u64, u64),
    /// A whole segment, its length unknown.
    Whole,
}

/// A range comes back with exactly its length; a segment's isn't known.
async fn fetch(http: &reqwest::Client, url: &str, ua: &str, range: Range) -> anyhow::Result<Bytes> {
    let stalled = || anyhow::Error::new(Stalled);
    let mut req = http.get(url).header("User-Agent", ua);
    if let Range::Header(a, b) = range {
        req = req.header("Range", format!("bytes={a}-{b}"));
    }
    // Errors leave the URL out: it carries the client's IP address and signatures (logs).
    let mut resp = tokio::time::timeout(STALL, req.send())
        .await
        .map_err(|_| stalled())?
        .and_then(reqwest::Response::error_for_status)
        .map_err(reqwest::Error::without_url)?;
    if matches!(range, Range::Header(..)) {
        anyhow::ensure!(resp.status() == reqwest::StatusCode::PARTIAL_CONTENT, "the server sent the whole file, not a range");
    }
    let expect = match range {
        Range::Param(a, b) | Range::Header(a, b) => Some(b - a + 1),
        Range::Whole => None,
    };
    let mut buf = bytes::BytesMut::with_capacity(expect.or(resp.content_length()).unwrap_or(0) as usize);
    while let Some(chunk) = tokio::time::timeout(STALL, resp.chunk()).await.map_err(|_| stalled())?.map_err(reqwest::Error::without_url)? {
        buf.extend_from_slice(&chunk);
    }
    if let Some(expect) = expect {
        anyhow::ensure!(buf.len() as u64 == expect, "short read: {} of {expect} bytes", buf.len());
    }
    Ok(buf.freeze())
}

/// Where a podcast feed's file ends up after its redirects (trackers, mostly), and its
/// size: asked with a one-byte range, which also shows the server serves ranges.
pub async fn probe_feed(http: &reqwest::Client, url: &str, ua: &str) -> anyhow::Result<(String, u64)> {
    let resp = tokio::time::timeout(STALL, http.get(url).header("User-Agent", ua).header("Range", "bytes=0-0").send())
        .await
        .map_err(|_| anyhow::anyhow!("the podcast's server didn't answer"))?
        .and_then(reqwest::Response::error_for_status)
        .map_err(reqwest::Error::without_url)?;
    anyhow::ensure!(resp.status() == reqwest::StatusCode::PARTIAL_CONTENT, "the podcast's server doesn't serve ranges ({})", resp.status());
    let total = resp
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit_once('/'))
        .and_then(|(_, t)| t.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("the podcast's server didn't give the file's size"))?;
    Ok((resp.url().to_string(), total))
}

pub struct StreamReader {
    shared: Arc<Shared>,
    pos: u64,
    id: u64,
    /// The block this reader last read.
    block: usize,
}

impl StreamReader {
    pub fn new(shared: Arc<Shared>) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        {
            let mut inner = shared.inner.lock().unwrap();
            inner.readers += 1;
            inner.limit = inner.blocks.len();
            inner.wants.push((id, 0));
        }
        shared.changed.notify_waiters();
        Self { shared, pos: 0, id, block: 0 }
    }
}

impl Drop for StreamReader {
    fn drop(&mut self) {
        let mut inner = self.shared.inner.lock().unwrap();
        inner.readers -= 1;
        inner.wants.retain(|&(id, _)| id != self.id);
        if inner.readers == 0 {
            // Stops the workers once their current request finishes.
            inner.cancelled = true;
        }
        self.shared.changed.notify_waiters();
    }
}

impl StreamReader {
    /// Block `idx`, once it's here; an error if the download failed or stopped first.
    pub fn block_at(&mut self, idx: usize) -> io::Result<Bytes> {
        let mut inner = self.shared.inner.lock().unwrap();
        inner.want = idx;
        if idx != self.block {
            self.block = idx;
            if let Some(w) = inner.wants.iter_mut().find(|(id, _)| *id == self.id) {
                w.1 = idx;
            }
            if inner.windowed {
                self.shared.changed.notify_waiters();
            }
        }
        loop {
            if let Some(b) = &inner.blocks[idx] {
                return Ok(b.clone());
            }
            if let Some(e) = &inner.error {
                return Err(io::Error::other(e.clone()));
            }
            if inner.cancelled {
                return Err(io::Error::other("download cancelled"));
            }
            inner = self.shared.ready.wait(inner).unwrap();
        }
    }
}

impl Read for StreamReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.pos >= self.shared.total {
            return Ok(0);
        }
        let idx = block_of(self.pos);
        let block = self.block_at(idx)?;
        let off = (self.pos - block_start(idx)) as usize;
        let n = out.len().min(block.len() - off);
        out[..n].copy_from_slice(&block[off..off + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for StreamReader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let total = self.shared.total as i64;
        let new = match to {
            SeekFrom::Start(p) => p as i64,
            SeekFrom::End(off) => total + off,
            SeekFrom::Current(off) => self.pos as i64 + off,
        };
        if new < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before start"));
        }
        self.pos = new as u64;
        Ok(self.pos)
    }
}

/// Open a decoder over `shared`. Non-seekable AAC decoders start after the first
/// fragment; seekable ones index every fragment first (i.e. effectively wait for the
/// download). Opus ones are always seekable: WebM's index is at the start.
pub fn decoder(shared: Arc<Shared>, seekable: bool) -> anyhow::Result<TrackDecoder> {
    if let Some(starts) = shared.segments.clone() {
        let duration_ms = shared.duration_ms;
        return Ok(TrackDecoder::Hls(Box::new(crate::hls::HlsSource::new(StreamReader::new(shared), starts, duration_ms)?)));
    }
    let size = shared.total;
    if shared.opus {
        let duration_ms = shared.duration_ms;
        return Ok(TrackDecoder::Opus(Box::new(crate::opus::OpusSource::new(StreamReader::new(shared), size, duration_ms)?)));
    }
    let feed = shared.feed;
    // A feed's file is probed for what it is: MP3, or AAC in an MP4 whose index may
    // follow the audio (only a seekable reader gets to it). Without a VBR header, an
    // MP3's length is guessed from its first frames, and gapless trimming would cut
    // whatever runs past the guess.
    let mut reader = StreamReader::new(shared);
    // Symphonia's ISO-MP4 demuxer ignores the `elst` edit list, leaving ~36 ms of leading
    // AAC priming silence and ~5-20 ms of trailing padding on YouTube's fragmented MP4s
    // (itag 140/141, including uploads and downloads). Read `elst` from `moov` so `Mp4Trim`
    // trims both ends to exact sample boundaries.
    let elst = if feed { None } else { reader.block_at(0).ok().and_then(|b| parse_mp4_elst(&b)) };
    let builder = rodio::Decoder::builder().with_data(reader).with_byte_len(size).with_seekable(seekable || feed);
    let builder = if feed { builder.with_coarse_seek(true).with_gapless(false) } else { builder.with_mime_type("audio/mp4").with_hint("m4a") };
    let dec = builder.build()?;
    Ok(TrackDecoder::Aac(Mp4Trim::new(dec, elst)))
}

/// Gapless trim points from an MP4 `moov` atom's `elst` edit list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Mp4Elst {
    movie_timescale: u32,
    media_timescale: u32,
    segment_duration: u64,
    media_time: u64,
}

/// Trims leading AAC priming samples and trailing padding from an MP4 decoder using its
/// `elst` edit list, keeping `try_seek` and `current_span_len` aligned to the trimmed timeline.
pub struct Mp4Trim<S> {
    inner: S,
    channels: usize,
    rate: u32,
    lead_samples: usize,
    total_samples: Option<usize>,
    skip: usize,
    remaining: Option<usize>,
}

impl<S: rodio::Source> Mp4Trim<S> {
    fn new(inner: S, elst: Option<Mp4Elst>) -> Self {
        let (channels, rate) = (inner.channels().get() as usize, inner.sample_rate().get());
        let scale = |v: u64, ts: u32| ((v as u128 * rate as u128 + ts as u128 / 2) / ts as u128) as usize;
        let (lead_samples, total_samples) = match elst {
            Some(e) if e.media_timescale > 0 && e.movie_timescale > 0 => {
                let lead = scale(e.media_time, e.media_timescale);
                // Ignore non-gapless edit lists that skip deep into a track.
                if lead <= rate as usize {
                    let total = (e.segment_duration > 0).then(|| scale(e.segment_duration, e.movie_timescale) * channels);
                    (lead * channels, total)
                } else {
                    (0, None)
                }
            }
            _ => (0, None),
        };
        Self { inner, channels, rate, lead_samples, total_samples, skip: lead_samples, remaining: total_samples }
    }

    fn samples_to_dur(&self, samples: usize) -> Duration {
        Duration::from_secs_f64(samples as f64 / (self.channels.max(1) as f64 * self.rate.max(1) as f64))
    }
}

impl<S: rodio::Source> Iterator for Mp4Trim<S> {
    type Item = rodio::Sample;

    fn next(&mut self) -> Option<rodio::Sample> {
        while self.skip > 0 {
            self.inner.next()?;
            self.skip -= 1;
        }
        if let Some(rem) = &mut self.remaining {
            if *rem == 0 {
                return None;
            }
            *rem -= 1;
        }
        self.inner.next()
    }
}

impl<S: rodio::Source> rodio::Source for Mp4Trim<S> {
    fn current_span_len(&self) -> Option<usize> {
        match (self.inner.current_span_len(), self.remaining) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
    fn channels(&self) -> rodio::ChannelCount {
        self.inner.channels()
    }
    fn sample_rate(&self) -> rodio::SampleRate {
        self.inner.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.total_samples.map(|s| self.samples_to_dur(s)).or_else(|| self.inner.total_duration())
    }
    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        if pos.is_zero() {
            self.inner.try_seek(Duration::ZERO)?;
            self.skip = self.lead_samples;
            self.remaining = self.total_samples;
            return Ok(());
        }
        self.inner.try_seek(pos + self.samples_to_dur(self.lead_samples))?;
        self.skip = 0;
        let at_samples = (pos.as_secs_f64() * self.rate as f64).round() as usize * self.channels;
        self.remaining = self.total_samples.map(|t| t.saturating_sub(at_samples));
        Ok(())
    }
}

/// Reads the timescale from an `mvhd` or `mdhd` fullbox body (version 0 or 1).
fn mp4_timescale(body: &[u8]) -> Option<u32> {
    match *body.first()? {
        0 => crate::video::be32(body, 12),
        1 => crate::video::be32(body, 20),
        _ => None,
    }
}

/// Parses a single-entry normal-rate `elst` fullbox body: `(segment_duration, media_time)`.
fn mp4_elst_entry(body: &[u8]) -> Option<(u64, u64)> {
    if crate::video::be32(body, 4)? != 1 {
        return None;
    }
    let (dur, mt, rate_off) = match *body.first()? {
        0 => (crate::video::be32(body, 8)? as u64, crate::video::be32(body, 12)? as i32 as i64, 16),
        1 => (crate::video::be64(body, 8)?, crate::video::be64(body, 16)? as i64, 24),
        _ => return None,
    };
    (crate::video::be32(body, rate_off)? == 0x0001_0000 && mt >= 0).then_some((dur, mt as u64))
}

/// Extracts `elst` from block 0's MP4 header, tolerating `moov` cut off after `trak`
/// (e.g. when embedded cover art in `udta` runs past the first 64 KB).
fn parse_mp4_elst(mut head: &[u8]) -> Option<Mp4Elst> {
    let moov = loop {
        let raw = crate::video::be32(head, 0)? as u64;
        let kind = head.get(4..8)?;
        let (hdr, size) = match raw {
            0 => (8, head.len() as u64),
            1 => (16, crate::video::be64(head, 8)?),
            n => (8, n),
        };
        if size < hdr as u64 {
            return None;
        }
        if kind == b"moov" {
            let end = usize::try_from(size).unwrap_or(head.len()).min(head.len());
            break &head[hdr..end];
        }
        head = head.get(usize::try_from(size).ok()?..)?;
    };
    let movie_timescale = mp4_timescale(crate::video::find(moov, &["mvhd"])?)?;
    crate::video::boxes(moov).filter(|(k, _)| *k == b"trak").find_map(|(_, trak)| {
        let (segment_duration, media_time) = mp4_elst_entry(crate::video::find(trak, &["edts", "elst"])?)?;
        let media_timescale = mp4_timescale(crate::video::find(trak, &["mdia", "mdhd"])?)?;
        Some(Mp4Elst { movie_timescale, media_timescale, segment_duration, media_time })
    })
}

/// A song's decoder: rodio's own (AAC, or a podcast feed's MP3) or Opus.
pub enum TrackDecoder {
    Aac(Mp4Trim<rodio::Decoder<StreamReader>>),
    /// Boxed: its decoder state is about 11 KB.
    Opus(Box<crate::opus::OpusSource>),
    /// AAC from HLS segments.
    Hls(Box<crate::hls::HlsSource>),
}

impl Iterator for TrackDecoder {
    type Item = rodio::Sample;

    fn next(&mut self) -> Option<rodio::Sample> {
        match self {
            Self::Aac(d) => d.next(),
            Self::Opus(d) => d.next(),
            Self::Hls(d) => d.next(),
        }
    }
}

impl rodio::Source for TrackDecoder {
    fn current_span_len(&self) -> Option<usize> {
        match self {
            Self::Aac(d) => d.current_span_len(),
            Self::Opus(d) => d.current_span_len(),
            Self::Hls(d) => d.current_span_len(),
        }
    }
    fn channels(&self) -> rodio::ChannelCount {
        match self {
            Self::Aac(d) => d.channels(),
            Self::Opus(d) => d.channels(),
            Self::Hls(d) => d.channels(),
        }
    }
    fn sample_rate(&self) -> rodio::SampleRate {
        match self {
            Self::Aac(d) => d.sample_rate(),
            Self::Opus(d) => d.sample_rate(),
            Self::Hls(d) => d.sample_rate(),
        }
    }
    fn total_duration(&self) -> Option<Duration> {
        match self {
            Self::Aac(d) => d.total_duration(),
            Self::Opus(d) => d.total_duration(),
            Self::Hls(d) => d.total_duration(),
        }
    }
    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        match self {
            Self::Aac(d) => d.try_seek(pos),
            Self::Opus(d) => d.try_seek(pos),
            Self::Hls(d) => d.try_seek(pos),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use rodio::Source;

    /// One response, with the socket kept open so an incomplete body really stalls.
    async fn serve(response: &'static [u8]) -> (String, tokio::sync::oneshot::Receiver<()>, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/audio?test=1", listener.local_addr().unwrap());
        let (started, waiting) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut chunk = [0; 1024];
                let n = socket.read(&mut chunk).await.unwrap();
                assert!(n > 0, "connection closed before the request headers arrived");
                request.extend_from_slice(&chunk[..n]);
            }
            socket.write_all(response).await.unwrap();
            let _ = started.send(());
            std::future::pending::<()>().await;
        });
        (url, waiting, server)
    }

    #[test]
    fn a_googlevideo_url_has_a_second_host() {
        // As YouTube hands them out: on the first host `mn` names, `mvi` its server.
        let url = "https://rr1---sn-u15hn5-5n.googlevideo.com/videoplayback?expire=1&mvi=1&mn=sn-u15hn5-5n%2Csn-oj5hn5-5g&fvip=3&sig=AB";
        let next = "https://rr3---sn-oj5hn5-5g.googlevideo.com/videoplayback?expire=1&mvi=1&mn=sn-u15hn5-5n%2Csn-oj5hn5-5g&fvip=3&sig=AB";
        assert_eq!(super::second_host(url).as_deref(), Some(next));
        // Once there, there's no other.
        assert_eq!(super::second_host(next), None);
        assert_eq!(super::second_host("http://rr4---sn-a.googlevideo.com:8080/videoplayback?mn=sn-a,sn-b&fvip=2").as_deref(), Some("http://rr2---sn-b.googlevideo.com:8080/videoplayback?mn=sn-a,sn-b&fvip=2"));
        for none in [
            "https://rr1---sn-a.googlevideo.com/videoplayback?mn=sn-a&fvip=3",
            "https://rr1---sn-a.googlevideo.com/videoplayback?mn=sn-a,sn-b",
            "https://example.com/videoplayback?mn=sn-a,sn-b&fvip=3",
            "https://rr1---sn-a.googlevideo.com/videoplayback?mn=sn-a,sn-b.evil.com/x&fvip=3",
            "https://rr1---sn-a.googlevideo.com/videoplayback?mn=sn-a,evil&fvip=3",
            "https://rr1---sn-a.googlevideo.com/videoplayback?mn=sn-a,sn-b&fvip=3@evil",
        ] {
            assert_eq!(super::second_host(none), None, "{none}");
        }
    }

    #[test]
    fn an_hls_segment_has_a_second_host_in_its_path() {
        // As YouTube lists them: the parameters in the path, and hls_chunk_host signed
        // with the rest, so only the URL's own host may change.
        let url = "https://rr1---sn-u15hn5-5h.googlevideo.com/videoplayback/id/x/itag/234/hls_chunk_host/rr1---sn-u15hn5-5h.googlevideo.com/mn/sn-u15hn5-5h%2Csn-aj4g55-52/fvip/2/sig/AB/file/seg.ts";
        let next = "https://rr2---sn-aj4g55-52.googlevideo.com/videoplayback/id/x/itag/234/hls_chunk_host/rr1---sn-u15hn5-5h.googlevideo.com/mn/sn-u15hn5-5h%2Csn-aj4g55-52/fvip/2/sig/AB/file/seg.ts";
        assert_eq!(super::second_host(url).as_deref(), Some(next));
        assert_eq!(super::second_host(next), None);
    }

    /// An HLS stream's segments all move to the second host together.
    #[tokio::test]
    async fn an_hls_streams_segments_fall_back_together() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let _ = socket.read(&mut [0; 4096]).await;
                let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nx").await;
            }
        });
        let http = reqwest::Client::builder()
            .no_proxy()
            .resolve("rr4---sn-first.googlevideo.com", "[::1]:0".parse().unwrap())
            .resolve("rr2---sn-second.googlevideo.com", "127.0.0.1:0".parse().unwrap())
            .build()
            .unwrap();
        let segment = |i: usize| format!("http://rr4---sn-first.googlevideo.com:{port}/videoplayback/mn/sn-first%2Csn-second/fvip/2/seg/{i}");
        let shared = super::Shared::segmented(vec![0.0, 1.0], 2000, None, false);
        let download = super::download(http, super::Media::Segments(vec![segment(0), segment(1)]), String::new(), shared.clone(), None);
        tokio::time::timeout(Duration::from_secs(10), download).await.expect("the download hung");
        server.abort();
        assert_eq!(shared.error(), None);
        assert!(shared.is_complete());
    }

    /// A host that takes the request and sends nothing (a broken cache): after `STALL`,
    /// the download goes on from the second host, rather than waiting it out again there.
    /// Not on macOS: its loopback has no 127.0.0.2, and on ::1 instead paused time runs
    /// out the stall before the socket answers. The fallback is the same code everywhere.
    #[tokio::test(start_paused = true)]
    #[cfg_attr(target_os = "macos", ignore)]
    async fn a_host_that_sends_nothing_falls_back_to_the_second_one() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = silent.local_addr().unwrap().port();
        let answers = tokio::net::TcpListener::bind(("127.0.0.2", port)).await.unwrap();
        // How many times the first host was asked.
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let held = tokio::spawn({
            let asked = asked.clone();
            async move {
                let mut kept = Vec::new();
                while let Ok((socket, _)) = silent.accept().await {
                    asked.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    kept.push(socket);
                }
            }
        });
        let served = tokio::spawn(async move {
            loop {
                let (mut socket, _) = answers.accept().await.unwrap();
                let _ = socket.read(&mut [0; 4096]).await;
                let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nx").await;
            }
        });
        let http = reqwest::Client::builder()
            .no_proxy()
            .resolve("rr4---sn-first.googlevideo.com", "127.0.0.1:0".parse().unwrap())
            .resolve("rr2---sn-second.googlevideo.com", "127.0.0.2:0".parse().unwrap())
            .build()
            .unwrap();
        let url = format!("http://rr4---sn-first.googlevideo.com:{port}/videoplayback?mn=sn-first,sn-second&fvip=2");
        let shared = super::Shared::new(1, 0, None, false, true);
        super::download(http, super::Media::File(url), String::new(), shared.clone(), None).await;
        held.abort();
        served.abort();
        assert_eq!(shared.error(), None);
        assert_eq!(shared.bytes().as_deref(), Some(&b"x"[..]));
        // Asked once, and left: retried there, it would be asked once per try. (Not timed:
        // paused time can count a wait for the second host as a stall too.)
        assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    /// A host that refuses the connection: the download goes on from the URL's second
    /// host (#342). Retried on the first, it never would.
    #[tokio::test]
    async fn an_unreachable_host_falls_back_to_the_second_one() {
        let (served, _, server) = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nx").await;
        let port = served.split(':').nth(2).and_then(|p| p.split('/').next()).unwrap().to_owned();
        let shared = super::Shared::new(1, 0, None, false, true);
        // The first host's address listens on nothing; the second's is the server.
        let http = reqwest::Client::builder()
            .no_proxy()
            .resolve("rr4---sn-first.googlevideo.com", "[::1]:0".parse().unwrap())
            .resolve("rr2---sn-second.googlevideo.com", "127.0.0.1:0".parse().unwrap())
            .build()
            .unwrap();
        let url = format!("http://rr4---sn-first.googlevideo.com:{port}/videoplayback?mn=sn-first,sn-second&fvip=2");
        let download = super::download(http, super::Media::File(url), String::new(), shared.clone(), None);
        // Windows takes about 2 s to give up on a refused connection.
        tokio::time::timeout(Duration::from_secs(10), download).await.expect("the download hung");
        server.abort();
        assert_eq!(shared.error(), None);
        assert_eq!(shared.bytes().as_deref(), Some(&b"x"[..]));
    }

    /// Skipping a song must release its buffer even if the server stops mid-block.
    #[tokio::test]
    async fn dropping_last_reader_stops_a_stalled_download() {
        let (url, waiting, server) = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 262144\r\n\r\nx").await;
        let shared = super::Shared::new(super::FIRST + super::BLOCK, 0, None, false, false);
        shared.put(0, bytes::Bytes::from(vec![0; super::FIRST as usize]));
        let reader = super::StreamReader::new(shared.clone());
        let weak = std::sync::Arc::downgrade(&shared);
        let mut download = tokio::spawn(super::download(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            super::Media::File(url), String::new(), shared.clone(), None,
        ));
        tokio::time::timeout(Duration::from_secs(5), waiting).await.unwrap().unwrap();
        drop(reader);
        drop(shared);
        let stopped = tokio::time::timeout(Duration::from_secs(1), &mut download).await;
        server.abort();
        if stopped.is_err() {
            download.abort();
        }
        stopped.expect("a skipped song kept its stalled download alive").unwrap();
        assert!(weak.upgrade().is_none(), "the old song's buffer is still held");
    }

    #[tokio::test]
    async fn cancelling_a_prefetch_interrupts_url_refresh() {
        use std::sync::Arc;

        let (url, _, server) = serve(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n").await;
        let entered = Arc::new(tokio::sync::Notify::new());
        let refresh: super::Refresh = Arc::new({
            let entered = entered.clone();
            move || {
                entered.notify_one();
                Box::pin(std::future::pending())
            }
        });
        let shared = super::Shared::new(1, 0, None, false, true);
        let weak = Arc::downgrade(&shared);
        let mut download = tokio::spawn(super::download(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            super::Media::File(url), String::new(), shared.clone(), Some(refresh),
        ));
        tokio::time::timeout(Duration::from_secs(5), entered.notified()).await.unwrap();
        shared.cancel();
        drop(shared);
        let stopped = tokio::time::timeout(Duration::from_secs(1), &mut download).await;
        server.abort();
        if stopped.is_err() {
            download.abort();
        }
        stopped.expect("a cancelled prefetch kept refreshing its URL").unwrap();
        assert!(weak.upgrade().is_none(), "the prefetch buffer is still held");
    }

    #[tokio::test]
    async fn cancelled_background_download_stops_waiting_for_playback() {
        use std::sync::Arc;

        let playing = super::Shared::new(1, 0, None, false, false);
        let shared = super::Shared::new(1, 0, None, false, true);
        let entered = Arc::new(tokio::sync::Notify::new());
        let ahead: super::Ahead = Arc::new({
            let entered = entered.clone();
            move || {
                entered.notify_one();
                Some(playing.clone())
            }
        });
        let weak = Arc::downgrade(&shared);
        let mut download = tokio::spawn(super::download_behind(
            reqwest::Client::new(), super::Media::File(String::new()), String::new(),
            shared.clone(), None, Some(ahead),
        ));
        tokio::time::timeout(Duration::from_secs(5), entered.notified()).await.unwrap();
        shared.cancel();
        drop(shared);
        let stopped = tokio::time::timeout(Duration::from_secs(1), &mut download).await;
        if stopped.is_err() {
            download.abort();
        }
        stopped.expect("a cancelled download kept waiting for the playing song").unwrap();
        assert!(weak.upgrade().is_none(), "the cancelled buffer is still held");
    }

    #[tokio::test]
    async fn failure_stops_workers_waiting_on_the_prefetch_limit() {
        let shared = super::Shared::new(super::FIRST + 2 * super::BLOCK, 0, None, false, true);
        shared.put(0, bytes::Bytes::from_static(b"head"));
        shared.put(1, bytes::Bytes::from_static(b"head"));
        let mut download = tokio::spawn(super::download(
            reqwest::Client::new(), super::Media::File(String::new()), String::new(), shared.clone(), None,
        ));
        // Poll the coordinator until it is waiting, then fail the buffer as a worker would.
        assert!(tokio::time::timeout(Duration::from_millis(50), &mut download).await.is_err());
        shared.fail("test download failed".into(), true);
        tokio::time::timeout(Duration::from_secs(1), download).await.expect("workers slept through failure").unwrap();
        shared.finished().await;
        assert!(shared.refused());
        assert_eq!(shared.error().as_deref(), Some("test download failed"));
    }

    #[tokio::test]
    async fn stopping_before_download_starts_is_not_missed() {
        let playing = super::Shared::new(1, 0, None, false, false);
        let ahead: super::Ahead = std::sync::Arc::new(move || Some(playing.clone()));
        for fail in [false, true] {
            let shared = super::Shared::new(1, 0, None, false, true);
            if fail {
                shared.fail("test download failed".into(), false);
            } else {
                shared.cancel();
            }
            tokio::time::timeout(Duration::from_secs(1), super::download_behind(
                reqwest::Client::new(), super::Media::File(String::new()), String::new(), shared.clone(), None, Some(ahead.clone()),
            )).await.expect("a stop before registration was missed");
            shared.finished().await;
            assert_eq!(shared.error().is_some(), fail);
        }
    }

    #[tokio::test]
    async fn completed_download_can_be_read_again() {
        let (url, _, server) = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nx").await;
        let shared = super::Shared::new(1, 0, None, false, false);
        tokio::time::timeout(Duration::from_secs(5), super::download(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            super::Media::File(url), String::new(), shared.clone(), None,
        )).await.expect("a successful download didn't finish");
        server.abort();
        assert!(shared.is_complete());
        assert!(shared.error().is_none());
        for _ in 0..2 {
            assert_eq!(super::StreamReader::new(shared.clone()).block_at(0).unwrap(), b"x"[..]);
        }
    }

    #[tokio::test]
    async fn windowed_download_keeps_workers_until_last_reader_leaves() {
        let shared = super::Shared::build(1, 1, true, 0, None, false, false, None, false);
        let reader = super::StreamReader::new(shared.clone());
        shared.put(0, bytes::Bytes::from_static(b"x"));
        // Fetching every block once signals finished, but seeks may need evicted blocks again.
        shared.finished().await;
        let mut download = tokio::spawn(super::download(
            reqwest::Client::new(), super::Media::File(String::new()), String::new(), shared.clone(), None,
        ));
        assert!(tokio::time::timeout(Duration::from_millis(50), &mut download).await.is_err());
        drop(reader);
        tokio::time::timeout(Duration::from_secs(1), download).await.expect("the windowed download stayed alive").unwrap();
        assert!(shared.error().is_none());
    }

    #[test]
    fn backup_offer_requires_both_a_bot_check_and_a_refused_stream() {
        let shared = || super::Shared::new(1, 0, None, false, false);
        let ordinary = shared();
        ordinary.fail("403".into(), true);
        assert!(!ordinary.failure().unwrap().offer_backup);

        let interrupted = shared();
        interrupted.mark_after_bot_check();
        assert!(interrupted.failure().is_none());
        interrupted.fail("connection lost".into(), false);
        assert!(!interrupted.failure().unwrap().offer_backup);

        let refused = shared();
        refused.mark_after_bot_check();
        refused.fail("403".into(), true);
        assert!(refused.failure().unwrap().offer_backup);
    }

    #[test]
    fn mp4_elst_parses_and_trims_leading_and_trailing_padding() {
        use std::num::NonZero;
        fn box_bytes(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
            let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
            out.extend_from_slice(kind);
            out.extend_from_slice(body);
            out
        }
        // Minimal ftyp + moov (mvhd + trak{edts/elst + mdia/mdhd}) matching YouTube AAC.
        let mut mvhd = vec![0u8; 20];
        mvhd[12..16].copy_from_slice(&44_100u32.to_be_bytes());
        let mut mdhd = vec![0u8; 20];
        mdhd[12..16].copy_from_slice(&44_100u32.to_be_bytes());
        let mut elst = vec![0u8; 20];
        elst[4..8].copy_from_slice(&1u32.to_be_bytes()); // entry_count = 1
        elst[8..12].copy_from_slice(&100u32.to_be_bytes()); // segment_duration = 100 frames
        elst[12..16].copy_from_slice(&16u32.to_be_bytes()); // media_time = 16 frames
        elst[16..18].copy_from_slice(&1i16.to_be_bytes()); // media_rate = 1.0
        let edts = box_bytes(b"edts", &box_bytes(b"elst", &elst));
        let mdia = box_bytes(b"mdia", &box_bytes(b"mdhd", &mdhd));
        let mut trak_body = edts;
        trak_body.extend_from_slice(&mdia);
        let mut moov_body = box_bytes(b"mvhd", &mvhd);
        moov_body.extend_from_slice(&box_bytes(b"trak", &trak_body));
        let mut mp4 = box_bytes(b"ftyp", b"dash\0\0\0\0");
        mp4.extend_from_slice(&box_bytes(b"moov", &moov_body));

        let parsed = super::parse_mp4_elst(&mp4).unwrap();
        assert_eq!(parsed, super::Mp4Elst { movie_timescale: 44_100, media_timescale: 44_100, segment_duration: 100, media_time: 16 });

        // Large embedded cover art in `udta` pushing `moov` past the 64 KB first block still parses.
        let mut big_moov_body = moov_body;
        big_moov_body.extend_from_slice(&box_bytes(b"udta", &vec![0u8; 80_000]));
        let mut big_mp4 = box_bytes(b"ftyp", b"dash\0\0\0\0");
        big_mp4.extend_from_slice(&box_bytes(b"moov", &big_moov_body));
        assert_eq!(super::parse_mp4_elst(&big_mp4[..65_536]), Some(parsed));

        // Stereo stream of 140 frames: 16 leading zeros, 100 ones, 24 trailing zeros.
        let mut samples = vec![0.0f32; 16 * 2];
        samples.extend(std::iter::repeat_n(1.0f32, 100 * 2));
        samples.extend(std::iter::repeat_n(0.0f32, 24 * 2));
        let src = rodio::buffer::SamplesBuffer::new(NonZero::new(2).unwrap(), NonZero::new(44_100).unwrap(), samples);
        let trimmed: Vec<f32> = super::Mp4Trim::new(src, Some(parsed)).collect();
        assert_eq!(trimmed.len(), 100 * 2);
        assert!(trimmed.iter().all(|&s| s == 1.0));
    }

    /// A real podcast's newest episode played the way an RSS episode is: found past its
    /// tracking redirects, downloaded in `Range` requests, decoded as MP3, seeked.
    /// `cargo test --release -- --ignored feed_file_live --nocapture`
    #[tokio::test]
    #[ignore]
    async fn feed_file_live() {
        let (http, ua) = (reqwest::Client::new(), crate::webmusic::UA);
        let rss = http.get("https://feeds.npr.org/510289/podcast.xml").header("User-Agent", ua).send().await.unwrap().text().await.unwrap();
        let tag = &rss[rss.find("<enclosure").unwrap()..];
        let url = tag[tag.find(" url=\"").unwrap() + 6..].split('"').next().unwrap().replace("&amp;", "&");
        let (file, size) = super::probe_feed(&http, &url, ua).await.unwrap();
        println!("{url}\n  -> {file}: {size} bytes");
        let shared = super::Shared::feed(size, 0, false);
        let t = Instant::now();
        super::download(http, super::Media::Feed(file), ua.to_owned(), shared.clone(), None).await;
        assert!(shared.error().is_none(), "{:?}", shared.error());
        println!("  downloaded in {:?}", t.elapsed());
        let mut d = super::decoder(shared.clone(), true).unwrap();
        let (ch, rate) = (d.channels().get() as usize, d.sample_rate().get() as usize);
        let secs = d.by_ref().count() as f64 / (ch * rate) as f64;
        println!("  {ch} ch {rate} Hz, {secs:.1} s");
        assert!(secs > 60.0, "too short: {secs} s");
        let mut d = super::decoder(shared, true).unwrap();
        let half = Duration::from_secs_f64(secs / 2.0);
        let t = Instant::now();
        d.try_seek(half).unwrap();
        println!("  seek to {:.1} s took {:?}", half.as_secs_f64(), t.elapsed());
        let left = d.count() as f64 / (ch * rate) as f64;
        println!("  {left:.1} s left");
        // Coarse: an estimate from the bitrate.
        assert!((left - (secs - half.as_secs_f64())).abs() < 5.0, "seek off");
    }
}
