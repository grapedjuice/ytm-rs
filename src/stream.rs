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
/// Retry limit within the block's deadline, with backoff for transient failures.
const RETRIES: u32 = 6;
/// A response that delivers nothing for this long is dead; a slow one that keeps
/// delivering may continue until the block deadline. Playback has shorter limits.
const STALL: Duration = Duration::from_secs(8);
const BLOCK_DEADLINE: Duration = Duration::from_secs(30);
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
#[derive(Clone)]
pub enum Media {
    /// One file, in ranges: `FIRST` bytes, then `BLOCK`s.
    File(String),
    /// A podcast feed's file on its own server: the same ranges, asked for in a
    /// `Range` header (googlevideo's `&range=` means nothing there).
    Feed(String),
    /// An HLS playlist's segments, one block each.
    Segments(Vec<String>),
}

/// Resolves fresh URLs for the same video and format.
pub type Refresh = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = anyhow::Result<Media>> + Send>> + Send + Sync>;

struct Inner {
    blocks: Vec<Option<Bytes>>,
    in_flight: Vec<bool>,
    /// Block a reader most recently needed; workers prioritize from here.
    want: usize,
    error: Option<String>,
    /// The download stopped on HTTP 403s that fresh URLs didn't get past.
    refused: bool,
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
    /// Wakes waiting workers: `limit` lifted, a reader moved or left, or cancelled.
    changed: tokio::sync::Notify,
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

    pub fn can_reuse(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.error.is_none() && (!inner.cancelled || inner.blocks.iter().all(Option::is_some))
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

    /// Reserve a decoder reader before starting a blocking constructor. A local
    /// interruption wakes that reader without cancelling another seek's reader.
    pub fn read_interrupt(self: &Arc<Self>) -> ReadInterrupt {
        ReadInterrupt::new(self.clone())
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

    fn fail(&self, err: String) { self.fail_with_refusal(err, false); }

    fn fail_with_refusal(&self, err: String, refused: bool) {
        FAIL_FROM.store(usize::MAX, std::sync::atomic::Ordering::Relaxed);
        {
            let mut inner = self.inner.lock().unwrap();
            if inner.cancelled || inner.error.is_some() { return; }
            log::error!("download failed: {err}");
            inner.error = Some(err);
            inner.refused = refused;
        }
        self.ready.notify_all();
        self.changed.notify_waiters();
    }

    fn stopped(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.cancelled || inner.error.is_some()
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
            Ok(Media::Segments(urls)) if Some(urls.len()) != self.segments => {
                log::warn!("stream URL refresh: {} segments for a buffer of {:?}", urls.len(), self.segments);
                false
            }
            Ok(Media::File(_) | Media::Feed(_)) if self.segments.is_some() => {
                log::warn!("stream URL refresh: a file for a segmented buffer");
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
}

/// The buffer a background download makes way for (the playing song's), if any.
pub type Ahead = Arc<dyn Fn() -> Option<Arc<Shared>> + Send + Sync>;

/// Download into `shared` with parallel range requests; returns when complete,
/// failed, or every reader was dropped.
#[cfg(test)]
pub async fn download(http: reqwest::Client, media: Media, ua: String, shared: Arc<Shared>, refresh: Option<Refresh>) {
    download_behind(http, media, ua, shared, refresh, None).await;
}

/// Background downloads preserve upstream's playing-buffer bandwidth priority.
pub async fn download_behind(http: reqwest::Client, media: Media, ua: String, shared: Arc<Shared>, refresh: Option<Refresh>, ahead: Option<Ahead>) {
    download_media_with_policy(http, media, ua, shared, refresh, ahead, DownloadPolicy { stall: STALL, deadline: BLOCK_DEADLINE, retry_base: Duration::from_millis(400), refresh_http: |_| crate::proxy::http() }).await;
}

pub async fn download_playback(http: reqwest::Client, media: Media, ua: String, shared: Arc<Shared>, refresh: Option<Refresh>) {
    download_media_with_policy(http, media, ua, shared, refresh, None, playback_policy()).await;
}

fn playback_policy() -> DownloadPolicy {
    DownloadPolicy { stall: Duration::from_secs(3), deadline: Duration::from_secs(12), retry_base: Duration::from_millis(200), refresh_http: |_| crate::proxy::http() }
}

#[derive(Clone, Copy)]
struct DownloadPolicy {
    stall: Duration,
    deadline: Duration,
    retry_base: Duration,
    refresh_http: fn(&reqwest::Client) -> reqwest::Client,
}

async fn until_stopped<T>(shared: &Shared, future: impl Future<Output = T>) -> Option<T> {
    tokio::pin!(future);
    loop {
        let changed = shared.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if shared.stopped() { return None; }
        tokio::select! { result = &mut future => return Some(result), _ = changed => {} }
    }
}

#[cfg(test)]
async fn download_with_policy(http: reqwest::Client, url: String, ua: String, shared: Arc<Shared>, refresh: Option<Refresh>, policy: DownloadPolicy) {
    download_media_with_policy(http, Media::File(url), ua, shared, refresh, None, policy).await;
}

async fn download_media_with_policy(http: reqwest::Client, media: Media, ua: String, shared: Arc<Shared>, refresh: Option<Refresh>, ahead: Option<Ahead>, policy: DownloadPolicy) {
    let segments = shared.segments.as_ref().map(|s| s.len());
    let slot = Arc::new(UrlSlot { url: tokio::sync::Mutex::new((media, 0)), refresh, segments });
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..WORKERS {
        let (http, ua, shared, slot, ahead) = (http.clone(), ua.clone(), shared.clone(), slot.clone(), ahead.clone());
        set.spawn(async move {
            loop {
                if let Some(ahead) = &ahead {
                    let wait = async {
                        while ahead().is_some_and(|s| !Arc::ptr_eq(&s, &shared) && s.filling()) {
                            tokio::time::sleep(Duration::from_millis(250)).await;
                        }
                    };
                    if until_stopped(&shared, wait).await.is_none() { return; }
                }
                let changed = shared.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                let idx = match shared.claim() {
                    Claim::Block(idx) => idx,
                    Claim::Wait => { changed.await; continue; }
                    Claim::Done => break,
                };
                let (a, b) = if segments.is_none() { shared.range(idx) } else { (0, 0) };
                let what = if segments.is_some() { format!("segment {idx}") } else { format!("range {a}-{b}") };
                let refused = std::sync::atomic::AtomicBool::new(false);
                let block = async {
                    let mut attempt = 0;
                    let mut data = bytes::BytesMut::new();
                    loop {
                        let received = data.len();
                        let start = a + received as u64;
                        let (url, range, version) = {
                            let current = slot.url.lock().await;
                            match &current.0 {
                                Media::File(url) => (format!("{url}&range={start}-{b}"), Range::Param(start, b), current.1),
                                Media::Feed(url) => (url.clone(), Range::Header(start, b), current.1),
                                Media::Segments(urls) => (urls[idx].clone(), Range::Whole, current.1),
                            }
                        };
                        let client = if version > 0 { (policy.refresh_http)(&http) } else { http.clone() };
                        let result = if idx >= FAIL_FROM.load(std::sync::atomic::Ordering::Relaxed) {
                            Err(anyhow::anyhow!("simulated network failure"))
                        } else if segments.is_none() && idx >= REFUSE_FROM.load(std::sync::atomic::Ordering::Relaxed) {
                            Err(anyhow::Error::new(SimulatedRefusal))
                        } else {
                            fetch(&client, &url, &ua, range, policy.stall, &mut data).await
                        };
                        if let Err(error) = &result { refused.store(is_forbidden(error), std::sync::atomic::Ordering::Relaxed); }
                        if result.is_err() && (segments.is_some() || data.len() as u64 == b - a + 1) {
                            data.truncate(received);
                        }
                        match result {
                            Ok(()) => return Ok(data.freeze()),
                            Err(_) if segments.is_none() && data.len() > received => {},
                            Err(e) if should_renew(&e) && slot.renew(version).await => {},
                            Err(e) if attempt < RETRIES && !is_forbidden(&e) => {
                                attempt += 1;
                                log::warn!("{what}: {e:#} (retry {attempt})");
                                tokio::time::sleep(policy.retry_base * (attempt * attempt)).await;
                            }
                            Err(e) => return Err(e),
                        }
                    }
                };
                match until_stopped(&shared, tokio::time::timeout(policy.deadline, block)).await {
                    None => return,
                    Some(Ok(Ok(data))) => shared.put(idx, data),
                    Some(Ok(Err(e))) => {
                        shared.fail_with_refusal(format!("{what}: {e:#}"), is_forbidden(&e));
                        return;
                    }
                    Some(Err(_)) => {
                        shared.fail_with_refusal(format!("{what}: block deadline exceeded ({} s)", policy.deadline.as_secs()), refused.load(std::sync::atomic::Ordering::Relaxed));
                        return;
                    }
                }
            }
        });
    }
    while let Some(result) = set.join_next().await {
        if result.is_err() { shared.fail("audio download worker stopped unexpectedly".into()); }
    }
    shared.done.send_replace(true);
}
fn is_forbidden(e: &anyhow::Error) -> bool {
    e.is::<SimulatedRefusal>()
        || e.downcast_ref::<reqwest::Error>()
            .and_then(reqwest::Error::status)
            .is_some_and(|s| s == reqwest::StatusCode::FORBIDDEN)
}

fn should_renew(e: &anyhow::Error) -> bool {
    e.is::<Stalled>() || is_forbidden(e) || e.downcast_ref::<reqwest::Error>().is_some_and(|e| {
        e.is_timeout() || e.is_connect() || e.is_body()
            || e.status().is_some_and(|s| s == reqwest::StatusCode::FORBIDDEN || s.is_server_error())
    })
}

#[derive(Debug)]
struct Stalled;
impl std::fmt::Display for Stalled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("audio response stopped delivering data") }
}
impl std::error::Error for Stalled {}

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
async fn fetch(http: &reqwest::Client, url: &str, ua: &str, range: Range, stall: Duration, buf: &mut bytes::BytesMut) -> anyhow::Result<()> {
    let stalled = || anyhow::Error::new(Stalled);
    let mut request = http.get(url).header("User-Agent", ua);
    if let Range::Header(a, b) = range { request = request.header("Range", format!("bytes={a}-{b}")); }
    let mut response = tokio::time::timeout(stall, request.send()).await.map_err(|_| stalled())?
        .and_then(reqwest::Response::error_for_status).map_err(reqwest::Error::without_url)?;
    if let Range::Header(a, b) = range {
        anyhow::ensure!(response.status() == reqwest::StatusCode::PARTIAL_CONTENT, "the server sent the whole file, not a range");
        let expected = format!("{a}-{b}");
        let correct = response.headers().get(reqwest::header::CONTENT_RANGE).and_then(|value| value.to_str().ok())
            .and_then(|value| value.split_once(' ')).is_some_and(|(unit, rest)| {
                unit.eq_ignore_ascii_case("bytes") && rest.split_once('/').is_some_and(|(bounds, _)| bounds == expected)
            });
        anyhow::ensure!(correct, "the server returned a different byte range");
    }
    let expect = match range { Range::Param(a, b) | Range::Header(a, b) => Some(b - a + 1), Range::Whole => None };
    anyhow::ensure!(!expect.is_some_and(|n| response.content_length().is_some_and(|length| length > n)), "server returned more bytes than the requested range");
    let before = buf.len();
    while let Some(chunk) = tokio::time::timeout(stall, response.chunk()).await.map_err(|_| stalled())?.map_err(reqwest::Error::without_url)? {
        if expect.is_some_and(|n| (buf.len() - before) as u64 + chunk.len() as u64 > n) {
            buf.truncate(before);
            anyhow::bail!("server returned more bytes than the requested range");
        }
        buf.extend_from_slice(&chunk);
    }
    if let Some(expect) = expect { anyhow::ensure!((buf.len() - before) as u64 == expect, "short read: {} of {expect} bytes", buf.len() - before); }
    Ok(())
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

struct ReadControl {
    shared: Arc<Shared>,
    stopped: std::sync::atomic::AtomicBool,
    waiting: std::sync::atomic::AtomicBool,
    id: u64,
}

/// A reader reservation and its independent cancellation signal. Keeping the
/// reservation alive bridges the gap between retiring a decoder and opening its
/// replacement on the same download.
#[derive(Clone)]
pub struct ReadInterrupt(Arc<ReadControl>);

impl ReadInterrupt {
    fn new(shared: Arc<Shared>) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        {
            let mut inner = shared.inner.lock().unwrap();
            inner.readers += 1;
            inner.limit = inner.blocks.len();
            inner.wants.push((id, 0));
        }
        shared.changed.notify_waiters();
        Self(Arc::new(ReadControl { shared, stopped: std::sync::atomic::AtomicBool::new(false), waiting: std::sync::atomic::AtomicBool::new(false), id }))
    }

    pub fn cancel(&self) {
        // Pair the flag with the Condvar mutex: a cancellation between the
        // reader's predicate check and wait must not lose its notification.
        let _inner = self.0.shared.inner.lock().unwrap();
        self.0.stopped.store(true, std::sync::atomic::Ordering::Release);
        self.0.shared.ready.notify_all();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.stopped.load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn is_waiting(&self) -> bool {
        self.0.waiting.load(std::sync::atomic::Ordering::Acquire)
    }
}

struct ReadWait<'a>(&'a std::sync::atomic::AtomicBool);
impl Drop for ReadWait<'_> {
    fn drop(&mut self) { self.0.store(false, std::sync::atomic::Ordering::Release); }
}

impl Drop for ReadControl {
    fn drop(&mut self) {
        let mut inner = self.shared.inner.lock().unwrap();
        inner.readers -= 1;
        inner.wants.retain(|&(id, _)| id != self.id);
        if inner.readers == 0 {
            inner.cancelled = true;
        }
        self.shared.ready.notify_all();
        self.shared.changed.notify_waiters();
    }
}

pub struct StreamReader {
    shared: Arc<Shared>, pos: u64, id: u64, block: usize, interrupt: ReadInterrupt,
}

impl StreamReader {
    #[cfg(test)]
    pub fn new(shared: Arc<Shared>) -> Self { Self::with_interrupt(shared.read_interrupt()) }

    pub(crate) fn with_interrupt(interrupt: ReadInterrupt) -> Self {
        Self { shared: interrupt.0.shared.clone(), pos: 0, id: interrupt.0.id, block: 0, interrupt }
    }

    /// Files and HLS share reader-local cancellation and missing-block waits.
    pub fn block_at(&mut self, idx: usize) -> io::Result<Bytes> {
        let mut inner = self.shared.inner.lock().unwrap();
        if idx >= inner.blocks.len() { return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "block beyond stream")); }
        if self.interrupt.is_cancelled() { return Err(io::Error::other("reader interrupted")); }
        inner.want = idx;
        if idx != self.block {
            self.block = idx;
            if let Some(w) = inner.wants.iter_mut().find(|(id, _)| *id == self.id) { w.1 = idx; }
            if inner.windowed { self.shared.changed.notify_waiters(); }
        }
        loop {
            if self.interrupt.is_cancelled() { return Err(io::Error::other("reader interrupted")); }
            if let Some(block) = &inner.blocks[idx] { return Ok(block.clone()); }
            if let Some(error) = &inner.error { return Err(io::Error::other(error.clone())); }
            if inner.cancelled { return Err(io::Error::other("download cancelled")); }
            self.interrupt.0.waiting.store(true, std::sync::atomic::Ordering::Release);
            let waiting = ReadWait(&self.interrupt.0.waiting);
            inner = self.shared.ready.wait(inner).unwrap();
            drop(waiting);
        }
    }
}

impl Read for StreamReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.pos >= self.shared.total { return Ok(0); }
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
/// fragment; seekable ones index every fragment first (effectively waiting for the
/// download). WebM may also put seek-head targets at EOF: opening it without seeking
/// must not scan those targets before the first sample can play.
#[cfg(test)]
pub fn decoder(shared: Arc<Shared>, seekable: bool) -> anyhow::Result<TrackDecoder> {
    let interrupt = shared.read_interrupt();
    decoder_with_interrupt(shared, seekable, interrupt)
}

/// An incomplete file must not need its tail just to resume in its available
/// prefix. Seekable MP4/WebM constructors may index fragments/cues at EOF.
/// Decode and discard up to the target off the audio thread instead.
pub fn decoder_at(shared: Arc<Shared>, at: Duration) -> anyhow::Result<TrackDecoder> {
    let interrupt = shared.read_interrupt();
    decoder_at_with_interrupt(shared, at, interrupt)
}

pub(crate) fn decoder_at_with_interrupt(shared: Arc<Shared>, at: Duration, interrupt: ReadInterrupt) -> anyhow::Result<TrackDecoder> {
    use rodio::Source;
    let indexed = shared.segments.is_some() || shared.feed || shared.is_complete();
    let mut d = decoder_with_interrupt(shared.clone(), indexed && !at.is_zero(), interrupt)?;
    if at.is_zero() {
        return Ok(d);
    }
    if indexed {
        d.try_seek(at).map_err(|e| anyhow::anyhow!("seek: {e}"))?;
    } else {
        let interrupt = d.interrupt_handle();
        discard_to(&mut d, at, || interrupt.is_cancelled() || shared.inner.lock().unwrap().cancelled).map_err(|e| match shared.error() {
            Some(reason) => anyhow::anyhow!("resume: {reason}"),
            None => e,
        })?;
    }
    Ok(d)
}

fn discard_to(source: &mut impl rodio::Source, at: Duration, cancelled: impl Fn() -> bool) -> anyhow::Result<()> {
    let channels = source.channels();
    let rate = source.sample_rate();
    let frames = at.as_nanos() * u128::from(rate.get()) / 1_000_000_000;
    let samples = frames.checked_mul(u128::from(channels.get())).ok_or_else(|| anyhow::anyhow!("resume position is too large"))?;
    for n in 0..samples {
        // A skipped generation cancels Shared, not necessarily this local reader.
        // Check bounded batches without taking a mutex per decoded PCM sample.
        if n % 1024 == 0 {
            anyhow::ensure!(!cancelled(), "resume interrupted");
        }
        anyhow::ensure!(source.next().is_some(), "audio ended before the resume position");
        anyhow::ensure!(source.channels() == channels && source.sample_rate() == rate, "audio format changed while resuming");
    }
    anyhow::ensure!(!cancelled(), "resume interrupted");
    Ok(())
}

pub fn decoder_with_interrupt(shared: Arc<Shared>, seekable: bool, interrupt: ReadInterrupt) -> anyhow::Result<TrackDecoder> {
    anyhow::ensure!(Arc::ptr_eq(&shared, &interrupt.0.shared), "reader belongs to a different download");
    anyhow::ensure!(!interrupt.is_cancelled(), "decoder opening interrupted");
    let size = shared.total;
    let reader = StreamReader::with_interrupt(interrupt.clone());
    let kind = if let Some(starts) = shared.segments.clone() {
        DecoderKind::Hls(Box::new(crate::hls::HlsSource::new(reader, starts, shared.duration_ms)?))
    } else if shared.opus {
        let duration_ms = shared.duration_ms;
        DecoderKind::Opus(Box::new(crate::opus::OpusSource::new(reader, size, duration_ms, seekable)?))
    } else {
        let builder = rodio::Decoder::builder().with_data(reader).with_byte_len(size).with_seekable(seekable || shared.feed);
        let builder = if shared.feed { builder.with_coarse_seek(true).with_gapless(false) }
            else { builder.with_mime_type("audio/mp4").with_hint("m4a") };
        DecoderKind::Aac(builder.build()?)
    };
    Ok(TrackDecoder { kind, interrupt, primed: Default::default() })
}

/// A song's decoder: AAC (rodio's own) or Opus.
pub struct TrackDecoder {
    kind: DecoderKind,
    interrupt: ReadInterrupt,
    /// Target PCM read off the audio thread, retained in interleaved channel order.
    primed: std::collections::VecDeque<rodio::Sample>,
}

impl TrackDecoder {
    /// Verify that the target has one complete PCM frame before publishing a decoder.
    /// Repeated calls while retained PCM remains do not advance it again. Discard the
    /// decoder on error: an incomplete/changed-format frame is never published.
    pub fn prime_frame(&mut self) -> anyhow::Result<()> {
        let interrupt = self.interrupt.clone();
        let cancelled = || interrupt.is_cancelled() || interrupt.0.shared.inner.lock().unwrap().cancelled;
        anyhow::ensure!(!cancelled(), "decoder priming interrupted");
        if !self.primed.is_empty() {
            return Ok(());
        }
        let frame = read_pcm_frame(self, cancelled)?;
        self.primed.extend(frame);
        Ok(())
    }

    /// Catch up a live seek after a streaming constructor, without a second index.
    pub(crate) fn advance(&mut self, by: Duration) -> anyhow::Result<()> {
        let interrupt = self.interrupt.clone();
        discard_to(self, by, || interrupt.is_cancelled() || interrupt.0.shared.inner.lock().unwrap().cancelled)
    }
    pub fn interrupt_handle(&self) -> ReadInterrupt {
        self.interrupt.clone()
    }
}

/// Read one complete interleaved frame atomically for the caller. The local vector
/// is returned only after format and cancellation checks, including the last sample.
fn read_pcm_frame(source: &mut impl rodio::Source, cancelled: impl Fn() -> bool) -> anyhow::Result<Vec<rodio::Sample>> {
    let (channels, rate) = (source.channels(), source.sample_rate());
    let mut frame = Vec::with_capacity(usize::from(channels.get()));
    for _ in 0..channels.get() {
        anyhow::ensure!(!cancelled(), "decoder priming interrupted");
        let sample = source.next().ok_or_else(|| anyhow::anyhow!("audio ended before a complete target frame was available"))?;
        anyhow::ensure!(source.channels() == channels && source.sample_rate() == rate, "audio format changed while priming");
        frame.push(sample);
    }
    anyhow::ensure!(!cancelled(), "decoder priming interrupted");
    Ok(frame)
}

enum DecoderKind {
    Aac(rodio::Decoder<StreamReader>),
    /// Boxed: its decoder state is about 11 KB.
    Opus(Box<crate::opus::OpusSource>),
    Hls(Box<crate::hls::HlsSource>),
}

impl Iterator for TrackDecoder {
    type Item = rodio::Sample;

    fn next(&mut self) -> Option<rodio::Sample> {
        if let Some(sample) = self.primed.pop_front() {
            return Some(sample);
        }
        match &mut self.kind {
            DecoderKind::Aac(d) => d.next(),
            DecoderKind::Opus(d) => d.next(),
            DecoderKind::Hls(d) => d.next(),
        }
    }
}

impl rodio::Source for TrackDecoder {
    fn current_span_len(&self) -> Option<usize> {
        if !self.primed.is_empty() {
            return Some(self.primed.len());
        }
        match &self.kind {
            DecoderKind::Aac(d) => d.current_span_len(),
            DecoderKind::Opus(d) => d.current_span_len(),
            DecoderKind::Hls(d) => d.current_span_len(),
        }
    }
    fn channels(&self) -> rodio::ChannelCount {
        match &self.kind {
            DecoderKind::Aac(d) => d.channels(),
            DecoderKind::Opus(d) => d.channels(),
            DecoderKind::Hls(d) => d.channels(),
        }
    }
    fn sample_rate(&self) -> rodio::SampleRate {
        match &self.kind {
            DecoderKind::Aac(d) => d.sample_rate(),
            DecoderKind::Opus(d) => d.sample_rate(),
            DecoderKind::Hls(d) => d.sample_rate(),
        }
    }
    fn total_duration(&self) -> Option<Duration> {
        match &self.kind {
            DecoderKind::Aac(d) => d.total_duration(),
            DecoderKind::Opus(d) => d.total_duration(),
            DecoderKind::Hls(d) => d.total_duration(),
        }
    }
    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        let result = match &mut self.kind {
            DecoderKind::Aac(d) => d.try_seek(pos),
            DecoderKind::Opus(d) => d.try_seek(pos),
            DecoderKind::Hls(d) => d.try_seek(pos),
        };
        // An unsupported/failed seek must not silently drop already primed PCM.
        if result.is_ok() {
            self.primed.clear();
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn priming_fixture(opus: bool) -> Arc<Shared> {
        if opus {
            let data = include_bytes!("testdata/sine6-opus.webm").to_vec();
            let shared = Shared::new(data.len() as u64, 6_000, None, true, false);
            shared.put(0, Bytes::from(data));
            shared
        } else {
            Shared::from_bytes(include_bytes!("testdata/sine6-fragmented-aac.m4a").to_vec(), 6_000, None)
        }
    }

    #[test]
    fn priming_aac_and_opus_retains_exact_frames_and_is_idempotent() {
        use rodio::Source;
        for opus in [false, true] {
            // Preserve decoder_at's direct iterator contract: only the explicit
            // prime call adds the readiness check and retained target samples.
            let reference = decoder_at(priming_fixture(opus), Duration::from_secs(2)).unwrap();
            let expected: Vec<_> = reference.take(4096).collect();
            assert_eq!(expected.len(), 4096);
            let mut d = decoder_at(priming_fixture(opus), Duration::from_secs(2)).unwrap();
            let format = (d.channels(), d.sample_rate());
            d.prime_frame().unwrap();
            assert_eq!(d.primed.len(), usize::from(format.0.get()));
            assert_eq!(d.current_span_len(), Some(usize::from(format.0.get())));
            assert_eq!(d.primed.iter().copied().collect::<Vec<_>>(), expected[..usize::from(format.0.get())]);
            d.prime_frame().unwrap();
            assert_eq!((d.channels(), d.sample_rate()), format);
            assert_eq!(d.take(4096).collect::<Vec<_>>(), expected, "opus={opus}");
        }
    }

    #[test]
    fn priming_rejects_eof_and_local_or_shared_cancellation() {
        for opus in [false, true] {
            let mut exhausted = decoder(priming_fixture(opus), false).unwrap();
            exhausted.by_ref().for_each(drop);
            assert!(exhausted.prime_frame().is_err());
            assert!(exhausted.primed.is_empty());
            let mut local = decoder(priming_fixture(opus), false).unwrap();
            local.interrupt_handle().cancel();
            assert!(local.prime_frame().is_err());
            assert!(local.primed.is_empty());
            let shared = priming_fixture(opus);
            let mut cancelled = decoder(shared.clone(), false).unwrap();
            shared.cancel();
            assert!(cancelled.prime_frame().is_err());
            assert!(cancelled.primed.is_empty());
        }
    }

    #[test]
    fn primed_pcm_is_cleared_only_after_a_successful_seek() {
        use rodio::Source;
        let mut d = decoder(priming_fixture(false), true).unwrap();
        d.prime_frame().unwrap();
        assert!(!d.primed.is_empty());
        d.try_seek(Duration::from_secs(2)).unwrap();
        assert!(d.primed.is_empty());
        let mut reference = decoder(priming_fixture(false), true).unwrap();
        reference.try_seek(Duration::from_secs(2)).unwrap();
        assert_eq!(d.take(512).collect::<Vec<_>>(), reference.take(512).collect::<Vec<_>>());

        // An incomplete AAC source can start in its prefix but cannot seek into
        // its unavailable final fragment. Failed seek must retain primed samples.
        let original = include_bytes!("testdata/sine6-fragmented-aac.m4a");
        let free_size = BLOCK as usize + 8;
        let mut data = original[..22_500].to_vec();
        data.extend_from_slice(&(free_size as u32).to_be_bytes());
        data.extend_from_slice(b"free");
        data.resize(data.len() + free_size - 8, 0);
        data.extend_from_slice(&original[22_500..26_633]);
        let partial = Shared::new(data.len() as u64, 6_000, None, false, false);
        partial.put(0, Bytes::copy_from_slice(&data[..FIRST as usize]));
        partial.fail("later range unavailable for the failed seek control".into());
        let mut d = decoder(partial, false).unwrap();
        d.prime_frame().unwrap();
        let kept = d.primed.clone();
        assert!(d.try_seek(Duration::from_millis(5500)).is_err());
        assert_eq!(d.primed, kept);
        assert_eq!(d.next(), kept.front().copied());
    }

    #[test]
    fn frame_priming_rejects_partial_format_changes_and_mid_frame_cancellation() {
        use rodio::Source;
        let make = |samples| rodio::buffer::SamplesBuffer::new(
            std::num::NonZero::new(2).unwrap(), std::num::NonZero::new(44_100).unwrap(), samples,
        );
        assert!(read_pcm_frame(&mut make(vec![1.0]), || false).is_err(), "a partial stereo frame is not ready");
        let checks = std::cell::Cell::new(0);
        let mut cancelled = make(vec![1.0, 2.0]);
        assert!(read_pcm_frame(&mut cancelled, || {
            checks.set(checks.get() + 1);
            checks.get() >= 2
        }).is_err());
        assert_eq!(cancelled.next(), Some(2.0), "cancellation is checked between channel samples");
        checks.set(0);
        let mut completed_then_cancelled = make(vec![1.0, 2.0]);
        assert!(read_pcm_frame(&mut completed_then_cancelled, || {
            checks.set(checks.get() + 1);
            checks.get() >= 3
        }).is_err(), "cancellation after the last channel must still reject readiness");
        struct Changing { read: usize, rate_change: bool }
        impl Iterator for Changing {
            type Item = f32;
            fn next(&mut self) -> Option<f32> { self.read += 1; Some(0.0) }
        }
        impl Source for Changing {
            fn current_span_len(&self) -> Option<usize> { Some(2) }
            fn channels(&self) -> rodio::ChannelCount { std::num::NonZero::new(if self.read != 0 && !self.rate_change { 1 } else { 2 }).unwrap() }
            fn sample_rate(&self) -> rodio::SampleRate { std::num::NonZero::new(if self.read != 0 && self.rate_change { 48_000 } else { 44_100 }).unwrap() }
            fn total_duration(&self) -> Option<Duration> { None }
        }
        for rate_change in [false, true] {
            assert!(read_pcm_frame(&mut Changing { read: 0, rate_change }, || false).is_err());
        }
    }
    #[test]
    fn streaming_resume_discards_whole_frames_and_rejects_early_end() {
        use rodio::Source;
        let make = || rodio::buffer::SamplesBuffer::new(
            std::num::NonZero::new(2).unwrap(), std::num::NonZero::new(10).unwrap(),
            (0..40).map(|n| n as f32).collect::<Vec<_>>(),
        );
        let mut d = make();
        discard_to(&mut d, Duration::from_millis(350), || false).unwrap();
        assert_eq!(d.next(), Some(6.0));
        assert_eq!(d.channels().get(), 2);
        let mut too_short = make();
        assert!(discard_to(&mut too_short, Duration::from_secs(3), || false).is_err());
        assert!(discard_to(&mut make(), Duration::from_secs(1), || true).is_err());
    }

    #[test]
    fn shared_cancellation_interrupts_discard_from_an_already_cached_source() {
        struct CancelAfter {
            shared: Arc<Shared>,
            samples: usize,
        }
        impl Iterator for CancelAfter {
            type Item = f32;
            fn next(&mut self) -> Option<f32> {
                self.samples += 1;
                if self.samples == 1500 { self.shared.cancel(); }
                Some(0.0)
            }
        }
        impl rodio::Source for CancelAfter {
            fn current_span_len(&self) -> Option<usize> { None }
            fn channels(&self) -> rodio::ChannelCount { std::num::NonZero::new(2).unwrap() }
            fn sample_rate(&self) -> rodio::SampleRate { std::num::NonZero::new(48_000).unwrap() }
            fn total_duration(&self) -> Option<Duration> { None }
        }
        let shared = Shared::from_bytes(vec![1], 0, None);
        let mut source = CancelAfter { shared: shared.clone(), samples: 0 };
        assert!(discard_to(&mut source, Duration::from_secs(60), || shared.inner.lock().unwrap().cancelled).is_err());
        assert_eq!(source.samples, 2048, "cancelled cached PCM must not decode to the original target");
    }

    #[tokio::test]
    async fn truncated_range_keeps_prefix_and_requests_only_missing_suffix() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for (body, length) in [("ABC", 6), ("DEF", 3)] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let n = socket.read(&mut request).await.unwrap();
                requests.push(String::from_utf8_lossy(&request[..n]).lines().next().unwrap().to_owned());
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n{body}").as_bytes()).await.unwrap();
            }
            requests
        });
        let shared = Shared::new(6, 0, None, false, false);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        tokio::time::timeout(Duration::from_secs(2), download_with_policy(client,
            format!("http://{address}/audio?x=1"), "test".into(), shared.clone(), None, fast_policy())).await.unwrap();
        assert_eq!(server.await.unwrap(), ["GET /audio?x=1&range=0-5 HTTP/1.1", "GET /audio?x=1&range=3-5 HTTP/1.1"]);
        assert_eq!(shared.bytes().unwrap(), b"ABCDEF");
        assert!(shared.error().is_none());
    }

    #[tokio::test]
    async fn overlong_response_is_not_published_as_a_complete_range() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 1024];
            let _ = socket.read(&mut request).await.unwrap();
            socket.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nABCDEF").await.unwrap();
            tokio::time::sleep(Duration::from_millis(5)).await;
            socket.write_all(b"GHI").await.unwrap();
        });
        let shared = Shared::new(6, 0, None, false, false);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let result = tokio::time::timeout(Duration::from_secs(1), download_with_policy(client,
            format!("http://{address}/audio?x=1"), "test".into(), shared.clone(), None, fast_policy())).await;
        server.abort();
        let _ = server.await;
        result.unwrap();
        assert!(shared.bytes().is_none());
        assert!(shared.error().is_some());
    }

    #[tokio::test]
    async fn malformed_chunked_terminator_never_requests_an_empty_suffix() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 1024];
            let _ = socket.read(&mut request).await.unwrap();
            socket.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n6\r\nABCDEF\r\n").await.unwrap();
        });
        let shared = Shared::new(6, 0, None, false, false);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        tokio::time::timeout(Duration::from_secs(1), download_with_policy(client,
            format!("http://{address}/audio?x=1"), "test".into(), shared.clone(), None, fast_policy())).await.unwrap();
        server.await.unwrap();
        assert!(shared.bytes().is_none());
        assert!(shared.error().is_some());
    }

    /// Genuine synthetic fragmented AAC, with a free box before its last fragment.
    /// Native indexing needs the absent tail; streaming resume only needs block 0.
    #[test]
    fn incomplete_aac_resume_uses_available_prefix_without_tail_index() {
        use rodio::Source;
        let original = include_bytes!("testdata/sine6-fragmented-aac.m4a");
        assert_eq!(&original[22_504..22_508], b"moof");
        assert_eq!(&original[26_637..26_641], b"mfra");
        let free_size = BLOCK as usize + 8;
        let mut data = original[..22_500].to_vec();
        data.extend_from_slice(&(free_size as u32).to_be_bytes());
        data.extend_from_slice(b"free");
        data.resize(data.len() + free_size - 8, 0);
        data.extend_from_slice(&original[22_500..26_633]);
        let at = Duration::from_secs(2);
        let complete = Shared::from_bytes(data.clone(), 6_000, None);
        let mut reference = decoder(complete, false).unwrap();
        discard_to(&mut reference, at, || false).unwrap();
        let expected: Vec<_> = reference.by_ref().take(4096).collect();
        assert_eq!(expected.len(), 4096);
        let partial = || {
            let shared = Shared::new(data.len() as u64, 6_000, None, false, false);
            shared.put(0, Bytes::copy_from_slice(&data[..FIRST as usize]));
            shared.fail("later range is unavailable".into());
            shared
        };
        assert!(decoder(partial(), true).is_err(), "negative control must need the missing tail index");
        let resumed = decoder_at(partial(), at).unwrap();
        assert_eq!(resumed.sample_rate(), reference.sample_rate());
        assert_eq!(resumed.take(4096).collect::<Vec<_>>(), expected);
    }

    #[test]
    fn opus_streaming_start_and_resume_decode_the_expected_samples() {
        let data = include_bytes!("testdata/sine6-opus.webm").to_vec();
        let make = || {
            let shared = Shared::new(BLOCK * 2, 6_000, None, true, false);
            let mut prefix = data.clone();
            prefix.resize(FIRST as usize, 0);
            shared.put(0, Bytes::from(prefix));
            shared
        };
        let mut reference = decoder(make(), false).unwrap();
        discard_to(&mut reference, Duration::from_secs(2), || false).unwrap();
        let expected: Vec<_> = reference.take(4096).collect();
        assert_eq!(expected.len(), 4096);
        let resumed = decoder_at(make(), Duration::from_secs(2)).unwrap();
        assert_eq!(resumed.take(4096).collect::<Vec<_>>(), expected);
    }

    #[test]
    fn local_interrupt_wakes_reader_and_preserves_reserved_replacement() {
        let shared = Shared::new(1, 0, None, false, false);
        let old = shared.read_interrupt();
        let replacement = shared.read_interrupt();
        let mut reader = StreamReader::with_interrupt(old.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || { let result = reader.read(&mut [0]); let _ = tx.send(result); });
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while !old.is_waiting() && std::time::Instant::now() < deadline { std::thread::yield_now(); }
        let was_waiting = old.is_waiting();
        old.cancel();
        let result = rx.recv_timeout(Duration::from_secs(1));
        worker.join().unwrap();
        assert!(was_waiting);
        assert!(result.unwrap().is_err());
        assert!(!old.is_waiting());
        drop(old);
        assert!(!shared.stopped());
        shared.put(0, Bytes::from_static(b"Z"));
        let mut next = StreamReader::with_interrupt(replacement.clone());
        let mut out = [0];
        assert_eq!(next.read(&mut out).unwrap(), 1);
        assert_eq!(out, [b'Z']);
        drop(next);
        assert!(!shared.stopped());
        drop(replacement);
        assert!(shared.stopped());
    }

    #[test]
    fn local_interrupt_stops_even_a_fully_cached_reader() {
        let shared = Shared::from_bytes(b"ABC".to_vec(), 0, None);
        let handle = shared.read_interrupt();
        let mut reader = StreamReader::with_interrupt(handle.clone());
        handle.cancel();
        assert!(reader.read(&mut [0]).is_err());
    }

    #[test]
    fn segmented_reader_interrupt_is_local_and_wakes_missing_segment() {
        let shared = Shared::segmented(vec![0.0, 7.0], 14_000, None, false);
        let old = shared.read_interrupt();
        let replacement = shared.read_interrupt();
        let mut reader = StreamReader::with_interrupt(old.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || { let _ = tx.send(reader.block_at(1)); });
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while !old.is_waiting() && std::time::Instant::now() < deadline { std::thread::yield_now(); }
        let waited = old.is_waiting();
        old.cancel();
        assert!(rx.recv_timeout(Duration::from_secs(1)).unwrap().is_err());
        worker.join().unwrap();
        assert!(waited);
        assert!(!replacement.is_cancelled());
        assert!(!shared.stopped());
    }

    #[tokio::test]
    async fn segmented_partial_response_retries_whole_segment_without_splicing() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for body in ["ABC", "ABCDEF"] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 1024];
                let n = socket.read(&mut request).await.unwrap();
                requests.push(String::from_utf8_lossy(&request[..n]).lines().next().unwrap().to_owned());
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\n{body}").as_bytes()).await.unwrap();
            }
            requests
        });
        let shared = Shared::segmented(vec![0.0], 1000, None, false);
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        tokio::time::timeout(Duration::from_secs(2), download_media_with_policy(http,
            Media::Segments(vec![format!("http://{address}/segment")]), "test".into(), shared.clone(), None, None, fast_policy())).await.unwrap();
        assert_eq!(server.await.unwrap(), ["GET /segment HTTP/1.1", "GET /segment HTTP/1.1"]);
        assert_eq!(shared.bytes().unwrap(), b"ABCDEF");
        assert!(shared.error().is_none());
    }

    #[tokio::test]
    async fn cancelling_a_throttled_prefetch_does_not_wait_for_ahead_to_finish() {
        let ahead = Shared::new(1, 0, None, false, false);
        let priority: Ahead = Arc::new(move || Some(ahead.clone()));
        let shared = Shared::new(1, 0, None, false, true);
        let other = shared.clone();
        let task = tokio::spawn(download_media_with_policy(reqwest::Client::new(), Media::File("invalid".into()),
            "test".into(), other, None, Some(priority), fast_policy()));
        tokio::task::yield_now().await;
        shared.cancel();
        tokio::time::timeout(Duration::from_millis(500), task).await.unwrap().unwrap();
        assert!(shared.error().is_none());
    }
    #[test]
    fn hls_decoder_constructor_cancels_its_missing_first_segment() {
        let shared = Shared::segmented(vec![0.0], 7000, None, false);
        let interrupt = shared.read_interrupt();
        let building = interrupt.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || { let _ = tx.send(decoder_with_interrupt(shared, false, building).is_err()); });
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while !interrupt.is_waiting() && std::time::Instant::now() < deadline { std::thread::yield_now(); }
        let waited = interrupt.is_waiting();
        interrupt.cancel();
        assert!(rx.recv_timeout(Duration::from_secs(1)).unwrap());
        worker.join().unwrap();
        assert!(waited, "HLS constructor must use the cancellable segment reader");
    }

    #[tokio::test]
    async fn feed_partial_response_resumes_with_a_validated_range_header() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requested = Vec::new();
            for (body, start, length) in [("ABC", 0, 6), ("DEF", 3, 3)] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 2048];
                let count = socket.read(&mut request).await.unwrap();
                requested.push(String::from_utf8_lossy(&request[..count]).to_ascii_lowercase());
                socket.write_all(format!("HTTP/1.1 206 Partial Content\r\nContent-Length: {length}\r\nContent-Range: bytes {start}-5/6\r\nConnection: close\r\n\r\n{body}").as_bytes()).await.unwrap();
            }
            requested
        });
        let shared = Shared::feed(6, 0, false);
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        tokio::time::timeout(Duration::from_secs(2), download_media_with_policy(http,
            Media::Feed(format!("http://{address}/feed")), "test".into(), shared.clone(), None, None, fast_policy())).await.unwrap();
        let requests = server.await.unwrap();
        assert!(requests[0].contains("range: bytes=0-5"));
        assert!(requests[1].contains("range: bytes=3-5"));
        assert!(requests.iter().all(|request| !request.contains("&range=")));
        assert_eq!(shared.bytes().unwrap(), b"ABCDEF");
        assert!(shared.error().is_none());
    }

    #[tokio::test]
    async fn forbidden_refresh_deadline_retains_hls_fallback_reason() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 1024];
            let _ = socket.read(&mut request).await.unwrap();
            socket.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
        });
        let refresh: Refresh = Arc::new(|| Box::pin(std::future::pending()));
        let shared = Shared::new(1, 0, None, false, false);
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        tokio::time::timeout(Duration::from_secs(1), download_with_policy(http,
            format!("http://{address}/file?itag=140"), "test".into(), shared.clone(), Some(refresh), fast_policy())).await.unwrap();
        server.await.unwrap();
        assert!(shared.error().is_some());
        assert!(shared.refused(), "a timed-out renewal after 403 must retain the upstream HLS fallback trigger");
    }
    fn fast_policy() -> DownloadPolicy {
        DownloadPolicy { stall: Duration::from_millis(50), deadline: Duration::from_millis(300), retry_base: Duration::ZERO, refresh_http: Clone::clone }
    }

    #[tokio::test]
    async fn playback_deadline_does_not_reset_on_tiny_body_progress() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let _ = socket.read(&mut request).await.unwrap();
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 64\r\n\r\n").await.unwrap();
            for _ in 0..64 {
                if socket.write_all(b"x").await.is_err() { break; }
                tokio::time::sleep(Duration::from_millis(900)).await;
            }
        });
        let shared = Shared::new(64, 0, None, false, false);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let started = std::time::Instant::now();
        tokio::time::timeout(Duration::from_secs(14), download_playback(client, Media::File(format!("http://{address}/audio?x=1")), "test".into(), shared.clone(), None)).await.unwrap();
        server.abort();
        let _ = server.await;
        assert!(started.elapsed() >= Duration::from_secs(11));
        assert!(shared.error().unwrap().contains("block deadline exceeded"));
        assert!(shared.bytes().is_none());
    }

    #[tokio::test]
    async fn playback_stall_is_shorter_than_the_offline_file_budget() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        async fn delayed_response() -> (String, tokio::task::JoinHandle<()>) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let _ = socket.read(&mut request).await.unwrap();
                tokio::time::sleep(Duration::from_millis(3500)).await;
                let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nx").await;
            });
            (format!("http://{address}/audio"), server)
        }
        let (play_url, play_server) = delayed_response().await;
        let (file_url, file_server) = delayed_response().await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut play_bytes = bytes::BytesMut::new();
        let mut file_bytes = bytes::BytesMut::new();
        let (play, file) = tokio::join!(
            fetch(&client, &play_url, "test", Range::Param(0, 0), playback_policy().stall, &mut play_bytes),
            fetch(&client, &file_url, "test", Range::Param(0, 0), STALL, &mut file_bytes),
        );
        assert!(play.err().unwrap().is::<Stalled>());
        assert!(file.is_ok());
        assert_eq!(&file_bytes[..], b"x");
        play_server.await.unwrap();
        file_server.await.unwrap();
    }

    #[tokio::test]
    async fn failed_head_prefetch_releases_waiting_workers_and_finished() {
        let shared = Shared::new(BLOCK * 3, 0, None, false, true);
        let run = download_with_policy(reqwest::Client::new(), "invalid-url".into(), "test".into(), shared.clone(), None, fast_policy());
        tokio::time::timeout(Duration::from_secs(1), run).await.unwrap();
        tokio::time::timeout(Duration::from_millis(100), shared.finished()).await.unwrap();
        assert!(shared.error().is_some());
    }

    #[tokio::test]
    async fn cancellation_interrupts_in_flight_http_before_its_deadline() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (accepted, wait_accepted) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let _ = accepted.send(());
            let _socket = socket;
            std::future::pending::<()>().await;
        });
        let shared = Shared::new(1, 0, None, false, false);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let task = tokio::spawn(download_with_policy(client, format!("http://{address}/audio?x=1"), "test".into(), shared.clone(), None,
            DownloadPolicy { stall: Duration::from_secs(30), deadline: Duration::from_secs(60), retry_base: Duration::from_secs(10), refresh_http: Clone::clone }));
        tokio::time::timeout(Duration::from_secs(1), wait_accepted).await.unwrap().unwrap();
        shared.cancel();
        let result = tokio::time::timeout(Duration::from_millis(500), task).await;
        server.abort();
        let _ = server.await;
        result.unwrap().unwrap();
        assert!(shared.error().is_none());
        assert!(*shared.done.borrow());
    }

    #[tokio::test]
    async fn terminal_failure_interrupts_registered_backoff() {
        let shared = Shared::new(1, 0, None, false, false);
        let (entered, wait_entered) = tokio::sync::oneshot::channel();
        let other = shared.clone();
        let task = tokio::spawn(async move { until_stopped(&other, async move {
            let _ = entered.send(());
            tokio::time::sleep(Duration::from_secs(60)).await;
        }).await });
        wait_entered.await.unwrap();
        shared.fail("simulated terminal failure".into());
        assert!(tokio::time::timeout(Duration::from_millis(500), task).await.unwrap().unwrap().is_none());
    }

    #[tokio::test]
    async fn service_error_renews_url_and_fetches_same_representation() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut paths = Vec::new();
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let n = socket.read(&mut request).await.unwrap();
                let good = String::from_utf8_lossy(&request[..n]).starts_with("GET /good?");
                paths.push(good);
                let response: &[u8] = if good { b"HTTP/1.1 206 Partial Content\r\nContent-Length: 1\r\nConnection: close\r\n\r\nZ" }
                    else { b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n" };
                socket.write_all(response).await.unwrap();
            }
            paths
        });
        let renewals = Arc::new(AtomicUsize::new(0));
        let count = renewals.clone();
        let refresh: Refresh = Arc::new(move || {
            count.fetch_add(1, Ordering::Relaxed);
            Box::pin(async move { Ok(Media::File(format!("http://{address}/good?x=1"))) })
        });
        let shared = Shared::new(1, 0, None, false, false);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        tokio::time::timeout(Duration::from_secs(2), download_with_policy(client, format!("http://{address}/bad?x=1"), "test".into(), shared.clone(), Some(refresh), fast_policy())).await.unwrap();
        assert_eq!(server.await.unwrap(), [false, true]);
        assert_eq!(renewals.load(Ordering::Relaxed), 1);
        assert_eq!(shared.bytes().unwrap(), b"Z");
        assert!(shared.error().is_none());
    }
}

#[cfg(test)]
mod upstream_tests {
    use std::time::{Duration, Instant};

    use rodio::Source;

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
