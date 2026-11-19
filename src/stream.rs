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

use bytes::{Bytes, BytesMut};

const BLOCK: u64 = 256 * 1024;
/// The first block is smaller: a decoder opens after a few hundred bytes, so playback
/// waits on this one alone, and it still holds seconds of audio while the next arrive.
const FIRST: u64 = 64 * 1024;
/// Some signed-in file URLs serve only the first MiB.
const WEB_PREVIEW: u64 = 1024 * 1024;
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

#[derive(Debug)]
pub struct DownloadFailure {
    pub reason: String,
    pub offer_backup: bool,
}

impl std::fmt::Display for DownloadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for DownloadFailure {}

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
    /// Verified file bytes past the web preview, even if the buffer has dropped them.
    verified_past_web_preview: bool,
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
                after_bot_check: false,
                readers: 0,
                cancelled: false,
                limit,
                wants: Vec::new(),
                windowed,
                ever: vec![false; n],
                fetched: 0,
                verified_past_web_preview: false,
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
            inner.verified_past_web_preview = total > WEB_PREVIEW;
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

    /// Wait until the download stops, however it ends.
    pub async fn finished(&self) {
        let _ = self.done.subscribe().wait_for(|d| *d).await;
    }

    /// Why the download stopped early, if it failed.
    pub fn error(&self) -> Option<String> {
        self.inner.lock().unwrap().error.clone()
    }

    /// Before replacing a working route, check that this web file actually serves
    /// audio beyond the preview. Observe the normal workers without starting a read.
    pub async fn past_web_preview(&self) -> anyhow::Result<()> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let inner = self.inner.lock().unwrap();
                anyhow::ensure!(!inner.cancelled && inner.error.is_none(), "web stream has stopped");
                if self.total <= WEB_PREVIEW || inner.verified_past_web_preview {
                    return Ok(());
                }
            }
            changed.await;
        }
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
        if inner.cancelled || inner.error.is_some() {
            return;
        }
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
        if self.total > WEB_PREVIEW && idx >= block_of(WEB_PREVIEW) && !inner.verified_past_web_preview {
            inner.verified_past_web_preview = true;
            self.changed.notify_waiters();
        }
    }

    fn fail(&self, err: String, refused: bool) {
        let mut inner = self.inner.lock().unwrap();
        if inner.cancelled || inner.error.is_some() {
            return;
        }
        log::error!("download failed: {err}");
        FAIL_FROM.store(usize::MAX, std::sync::atomic::Ordering::Relaxed);
        inner.error = Some(err);
        inner.refused = refused;
        drop(inner);
        self.ready.notify_all();
        self.changed.notify_waiters();
    }

    async fn until_stopped(&self) {
        loop {
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
}

/// The buffer a background download makes way for (the playing song's), if any.
pub type Ahead = Arc<dyn Fn() -> Option<Arc<Shared>> + Send + Sync>;

/// Download into `shared` with parallel range requests; returns when complete,
/// failed, or every reader was dropped.
pub async fn download(http: reqwest::Client, media: Media, ua: String, shared: Arc<Shared>, refresh: Option<Refresh>) {
    download_blocks(http, media, ua, shared, refresh, None, true).await;
}

/// `download`, but before each block it waits while `ahead`'s buffer still has blocks
/// to come that a reader may need: the playing song gets the bandwidth first.
pub async fn download_behind(http: reqwest::Client, media: Media, ua: String, shared: Arc<Shared>, refresh: Option<Refresh>, ahead: Option<Ahead>) {
    download_blocks(http, media, ua, shared, refresh, ahead, false).await;
}

async fn download_blocks(http: reqwest::Client, media: Media, ua: String, shared: Arc<Shared>, refresh: Option<Refresh>, ahead: Option<Ahead>, playing: bool) {
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
                let mut data = BytesMut::new();
                let mut data_version = 0;
                let mut unavailable = false;
                loop {
                    let (url, range, version) = {
                        let slot = slot.url.lock().await;
                        // Even if the format and length are unchanged, keep a partial
                        // block within one URL generation.
                        if data_version != slot.1 {
                            data.clear();
                            data_version = slot.1;
                            unavailable = false;
                        }
                        let start = a + data.len() as u64;
                        match &slot.0 {
                            Media::File(url) => (format!("{url}&range={start}-{b}"), Range::Param(start, b), slot.1),
                            Media::Feed(url) => (url.clone(), Range::Header(start, b), slot.1),
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
                        fetch(&client, &url, &ua, range, shared.total, &mut data, STALL).await
                            .and_then(|()| {
                                // A segment that fails to unpack is retried whole too.
                                let data = data.split().freeze();
                                if data.is_empty() {
                                    anyhow::ensure!(short_tail && Some(idx + 1) == segments, "the server sent an empty segment");
                                } else if transport_stream {
                                    return crate::hls::unpack_audio(&data);
                                }
                                Ok(data)
                            })
                    };
                    match res {
                        Ok(data) => {
                            shared.put(idx, data);
                            break;
                        }
                        Err(e) if is_forbidden(&e) && slot.renew(version).await => {}
                        Err(e) if attempt < RETRIES && !is_forbidden(&e) => {
                            // An explicit 503 before the first byte of playback can be a
                            // bad rendition. Retry it once before letting another one play.
                            // A slow or partial response keeps the usual retry budget.
                            let unavailable_now = playing && idx == 0 && !shared.feed && data.is_empty()
                                && e.downcast_ref::<reqwest::Error>().and_then(reqwest::Error::status)
                                    == Some(reqwest::StatusCode::SERVICE_UNAVAILABLE);
                            if unavailable && unavailable_now {
                                shared.fail(format!("{what}: {e:#}"), false);
                                return;
                            }
                            unavailable = unavailable_now;
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
    loop {
        tokio::select! {
            _ = shared.until_stopped() => {
                set.abort_all();
                break;
            }
            result = set.join_next() => match result {
                Some(Err(e)) => shared.fail(format!("stream worker stopped: {e}"), false),
                Some(Ok(())) => {}
                None => break,
            }
        }
    }
    // Release in-flight responses and refresh futures before marking a stop done.
    while set.join_next().await.is_some() {}
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

/// A concrete byte range, including the representation's total size.
fn content_range(value: &str) -> Option<(u64, u64, u64)> {
    let (unit, value) = value.split_once(' ')?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    let (range, total) = value.split_once('/')?;
    let (a, b) = range.split_once('-')?;
    let parse = |s: &str| -> Option<u64> {
        if !s.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    };
    let (a, b, total) = (parse(a)?, parse(b)?, parse(total)?);
    (a <= b && b < total).then_some((a, b, total))
}

fn response_range(resp: &reqwest::Response) -> Option<(u64, u64, u64)> {
    let headers = resp.headers().get_all(reqwest::header::CONTENT_RANGE);
    let mut headers = headers.iter();
    let value = headers.next()?.to_str().ok()?;
    if headers.next().is_some() {
        return None;
    }
    content_range(value)
}

/// Append to a block's verified prefix. An interrupted segment starts over.
async fn fetch(http: &reqwest::Client, url: &str, ua: &str, range: Range, total: u64, buf: &mut BytesMut, idle: Duration) -> anyhow::Result<()> {
    let stalled = || anyhow::anyhow!("no data for {} s", idle.as_secs());
    let mut req = http.get(url).header("User-Agent", ua);
    if !matches!(range, Range::Whole) {
        req = req.header("Accept-Encoding", "identity");
    }
    if let Range::Header(a, b) = range {
        req = req.header("Range", format!("bytes={a}-{b}"));
    }
    // Errors leave the URL out: it carries the client's IP address and signatures (logs).
    let mut resp = tokio::time::timeout(idle, req.send())
        .await
        .map_err(|_| stalled())?
        .and_then(reqwest::Response::error_for_status)
        .map_err(reqwest::Error::without_url)?;
    let status = resp.status();
    let partial = status == reqwest::StatusCode::PARTIAL_CONTENT;
    let ok = status == reqwest::StatusCode::OK;
    let (expect, resume) = match range {
        Range::Param(a, b) | Range::Header(a, b) => {
            anyhow::ensure!(partial || (matches!(range, Range::Param(..)) && ok), "the server did not honor the range ({status})");
            let has_range = resp.headers().contains_key(reqwest::header::CONTENT_RANGE);
            // googlevideo also returns 200 for query ranges, without Content-Range.
            if partial || has_range {
                anyhow::ensure!(response_range(&resp) == Some((a, b, total)), "the server sent a different byte range");
            }
            let expect = b - a + 1;
            let length = resp.content_length();
            anyhow::ensure!(length.is_none_or(|n| n == expect), "the server sent a different range length");
            buf.reserve(expect as usize);
            (Some(expect), has_range || length.is_some())
        }
        Range::Whole => {
            anyhow::ensure!((ok || status == reqwest::StatusCode::NO_CONTENT) && !resp.headers().contains_key(reqwest::header::CONTENT_RANGE), "the server did not send a whole segment ({status})");
            (None, false)
        }
    };
    let base = buf.len();
    let result = async {
        while let Some(chunk) = tokio::time::timeout(idle, resp.chunk()).await.map_err(|_| stalled())?.map_err(reqwest::Error::without_url)? {
            if expect.is_some_and(|n| chunk.len() as u64 > n - (buf.len() - base) as u64) {
                buf.truncate(base);
                anyhow::bail!("the server sent more bytes than requested");
            }
            buf.extend_from_slice(&chunk);
        }
        if let Some(expect) = expect {
            anyhow::ensure!((buf.len() - base) as u64 == expect, "short read: {} of {expect} bytes", buf.len() - base);
        }
        Ok(())
    }.await;
    // An unverified prefix or an exact-length body with broken framing can't be
    // resumed. In the latter case retrying the suffix would also invert the range.
    if result.is_err() && (!resume || expect == Some((buf.len() - base) as u64)) {
        buf.truncate(base);
    }
    result
}

/// Where a podcast feed's file ends up after its redirects (trackers, mostly), and its
/// size: asked with a one-byte range, which also shows the server serves ranges.
pub async fn probe_feed(http: &reqwest::Client, url: &str, ua: &str) -> anyhow::Result<(String, u64)> {
    let resp = tokio::time::timeout(STALL, http.get(url).header("User-Agent", ua).header("Range", "bytes=0-0").header("Accept-Encoding", "identity").send())
        .await
        .map_err(|_| anyhow::anyhow!("the podcast's server didn't answer"))?
        .and_then(reqwest::Response::error_for_status)
        .map_err(reqwest::Error::without_url)?;
    anyhow::ensure!(resp.status() == reqwest::StatusCode::PARTIAL_CONTENT, "the podcast's server doesn't serve ranges ({})", resp.status());
    let total = response_range(&resp)
        .filter(|&(a, b, _)| a == 0 && b == 0 && resp.content_length().is_none_or(|n| n == 1))
        .map(|(_, _, total)| total)
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
            // Stops the workers, including their current requests.
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
    let builder = rodio::Decoder::builder().with_data(StreamReader::new(shared)).with_byte_len(size).with_seekable(seekable || feed);
    let builder = if feed { builder.with_coarse_seek(true).with_gapless(false) } else { builder.with_mime_type("audio/mp4").with_hint("m4a") };
    Ok(TrackDecoder::Aac(builder.build()?))
}

/// A song's decoder: rodio's own (AAC, or a podcast feed's MP3) or Opus.
pub enum TrackDecoder {
    Aac(rodio::Decoder<StreamReader>),
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

    mod web_preview {
        use std::future::Future;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::task::{Context, Poll, Wake, Waker};

        use bytes::Bytes;

        use super::super::{Shared, WEB_PREVIEW, block_count, block_of};

        const KIB: u64 = 1024;

        #[derive(Default)]
        struct Notified(AtomicBool);

        impl Wake for Notified {
            fn wake(self: Arc<Self>) {
                self.0.store(true, Ordering::Relaxed);
            }
        }

        fn put_block(shared: &Shared, index: usize) {
            let (first, last) = shared.range(index);
            shared.put(index, Bytes::from(vec![0; (last - first + 1) as usize]));
        }

        #[test]
        fn past_web_preview_waits_for_the_verified_boundary_block() {
            let shared = Shared::new(1088 * KIB, 0, None, false, false);
            assert_eq!(shared.range(0), (0, 64 * KIB - 1));
            assert_eq!(block_of(WEB_PREVIEW), 4);
            assert_eq!(shared.range(4), (832 * KIB, 1088 * KIB - 1));

            let notified = Arc::new(Notified::default());
            let waker = Waker::from(notified.clone());
            let mut cx = Context::from_waker(&waker);
            let mut check = std::pin::pin!(shared.past_web_preview());
            assert!(check.as_mut().poll(&mut cx).is_pending());
            for index in 0..4 {
                put_block(&shared, index);
                assert!(check.as_mut().poll(&mut cx).is_pending());
            }
            assert!(!notified.0.load(Ordering::Relaxed));

            put_block(&shared, 4);
            assert!(notified.0.load(Ordering::Relaxed));
            assert!(matches!(check.as_mut().poll(&mut cx), Poll::Ready(Ok(()))));
        }

        #[test]
        fn past_web_preview_accepts_a_later_block_without_the_boundary_block() {
            let shared = Shared::new(48 * 1024 * KIB, 0, None, false, false);
            let notified = Arc::new(Notified::default());
            let waker = Waker::from(notified.clone());
            let mut cx = Context::from_waker(&waker);
            let mut check = std::pin::pin!(shared.past_web_preview());
            assert!(check.as_mut().poll(&mut cx).is_pending());

            // A far seek can fetch this block before those near the beginning.
            put_block(&shared, 100);
            assert!(notified.0.swap(false, Ordering::Relaxed));
            assert!(matches!(check.as_mut().poll(&mut cx), Poll::Ready(Ok(()))));

            let mut changed = std::pin::pin!(shared.changed.notified());
            assert!(changed.as_mut().poll(&mut cx).is_pending());
            put_block(&shared, 4);
            assert!(!notified.0.load(Ordering::Relaxed));
        }

        #[test]
        fn past_web_preview_wakes_and_fails_on_cancel_or_error() {
            for cancel in [true, false] {
                let shared = Shared::new(WEB_PREVIEW + 1, 0, None, false, false);
                let notified = Arc::new(Notified::default());
                let waker = Waker::from(notified.clone());
                let mut cx = Context::from_waker(&waker);
                let mut check = std::pin::pin!(shared.past_web_preview());
                assert!(check.as_mut().poll(&mut cx).is_pending());

                if cancel {
                    shared.cancel();
                } else {
                    shared.fail("fixture range refused".into(), true);
                }
                assert!(notified.0.load(Ordering::Relaxed));
                assert!(matches!(check.as_mut().poll(&mut cx), Poll::Ready(Err(_))));
            }
        }

        #[test]
        fn past_web_preview_accepts_complete_files_up_to_the_limit() {
            for size in [64 * KIB, WEB_PREVIEW] {
                let shared = Shared::new(size, 0, None, false, false);
                for index in 0..block_count(size) {
                    put_block(&shared, index);
                }
                assert!(shared.is_complete());
                let mut check = std::pin::pin!(shared.past_web_preview());
                let mut cx = Context::from_waker(Waker::noop());
                assert!(matches!(check.as_mut().poll(&mut cx), Poll::Ready(Ok(()))));
            }
        }
        #[test]
        fn past_web_preview_checks_for_stops_before_short_or_verified_success() {
            for size in [64 * KIB, WEB_PREVIEW + 1] {
                for cancel in [true, false] {
                    let shared = Shared::new(size, 0, None, false, false);
                    // A full short file or the block spanning the preview boundary.
                    put_block(&shared, block_count(size) - 1);
                    if cancel {
                        shared.cancel();
                    } else {
                        shared.fail("fixture range refused".into(), true);
                    }
                    let mut check = std::pin::pin!(shared.past_web_preview());
                    let mut cx = Context::from_waker(Waker::noop());
                    assert!(matches!(check.as_mut().poll(&mut cx), Poll::Ready(Err(_))));
                }
            }
        }

        #[test]
        fn past_web_preview_accepts_a_saved_file_over_the_limit() {
            let shared = Shared::from_bytes(vec![0; WEB_PREVIEW as usize + 1], 0, None);
            let mut check = std::pin::pin!(shared.past_web_preview());
            let mut cx = Context::from_waker(Waker::noop());
            assert!(matches!(check.as_mut().poll(&mut cx), Poll::Ready(Ok(()))));
        }
    }

    mod http {
        use super::super::*;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;
        use tokio::sync::{Notify, mpsc};
        use tokio::task::JoinHandle;
        use tokio::time::timeout;

        const WAIT: Duration = Duration::from_secs(4);

        enum Reply {
            Bytes(String),
            Stall,
            Drip,
        }

        fn reply(status: &str, headers: &str, body: &str) -> Reply {
            Reply::Bytes(format!("HTTP/1.1 {status}\r\n{headers}Connection: close\r\n\r\n{body}"))
        }

        struct Server {
            url: String,
            requests: mpsc::UnboundedReceiver<String>,
            task: JoinHandle<()>,
        }

        impl Server {
            async fn new(replies: Vec<Reply>) -> Self {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let url = format!("http://{}/audio?test=1", listener.local_addr().unwrap());
                let (sent, requests) = mpsc::unbounded_channel();
                let task = tokio::spawn(async move {
                    for reply in replies {
                        let (mut socket, _) = listener.accept().await.unwrap();
                        let mut request = Vec::new();
                        while !request.ends_with(b"\r\n\r\n") {
                            let mut chunk = [0; 1024];
                            let n = socket.read(&mut chunk).await.unwrap();
                            assert!(n > 0 && request.len() < 16 * 1024);
                            request.extend_from_slice(&chunk[..n]);
                        }
                        match &reply {
                            Reply::Bytes(bytes) => {
                                socket.write_all(bytes.as_bytes()).await.unwrap();
                            }
                            Reply::Stall | Reply::Drip => {
                                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nA").await.unwrap();
                            }
                        }
                        sent.send(String::from_utf8(request).unwrap()).unwrap();
                        match reply {
                            Reply::Bytes(_) => {}
                            Reply::Stall => {
                                // Dropping the in-flight HTTP response closes its socket.
                                let result = socket.read(&mut [0]).await;
                                assert!(matches!(result, Ok(0)) || result.is_err());
                            }
                            Reply::Drip => {
                                for byte in b"BCDEFG" {
                                    tokio::time::sleep(Duration::from_millis(100)).await;
                                    socket.write_all(&[*byte]).await.unwrap();
                                }
                            }
                        }
                    }
                });
                Self { url, requests, task }
            }

            async fn request(&mut self) -> String {
                timeout(WAIT, self.requests.recv()).await.unwrap().expect("missing request")
            }

            async fn finish(&mut self) {
                timeout(WAIT, &mut self.task).await.unwrap().unwrap();
            }
        }

        impl Drop for Server {
            fn drop(&mut self) {
                self.task.abort();
            }
        }

        fn client() -> reqwest::Client {
            reqwest::Client::builder().no_proxy().build().unwrap()
        }

        #[tokio::test]
        async fn interrupted_file_and_feed_resume_at_the_missing_byte() {
            for feed in [false, true] {
                let replies = if feed {
                    vec![
                        reply("206 Partial Content", "Content-Length: 6\r\nContent-Range: bytes 0-5/6\r\n", "ABC"),
                        reply("206 Partial Content", "Content-Length: 3\r\nContent-Range: bytes 3-5/6\r\n", "DEF"),
                    ]
                } else {
                    vec![
                        reply("200 OK", "Content-Length: 6\r\n", "ABC"),
                        reply("200 OK", "Content-Length: 3\r\n", "DEF"),
                    ]
                };
                let mut server = Server::new(replies).await;
                let (media, shared) = if feed {
                    (Media::Feed(server.url.clone()), Shared::feed(6, 0, false))
                } else {
                    (Media::File(server.url.clone()), Shared::new(6, 0, None, false, false))
                };
                timeout(WAIT, download(client(), media, "test".into(), shared.clone(), None)).await.unwrap();
                let first = server.request().await.to_ascii_lowercase();
                let second = server.request().await.to_ascii_lowercase();
                if feed {
                    assert!(first.contains("\r\nrange: bytes=0-5\r\n"));
                    assert!(second.contains("\r\nrange: bytes=3-5\r\n"));
                } else {
                    assert!(first.starts_with("get /audio?test=1&range=0-5 "));
                    assert!(second.starts_with("get /audio?test=1&range=3-5 "));
                }
                server.finish().await;
                assert_eq!(shared.bytes().unwrap(), b"ABCDEF");
                assert!(shared.error().is_none());
            }
        }

        #[tokio::test]
        async fn interrupted_hls_segment_restarts_whole() {
            let mut server = Server::new(vec![
                reply("200 OK", "Content-Length: 6\r\n", "bad"),
                reply("200 OK", "Content-Length: 6\r\n", "ABCDEF"),
            ]).await;
            let shared = Shared::segmented(vec![0.0], 1000, None, false);
            let media = Media::Segments(vec![server.url.clone()]);
            timeout(WAIT, download(client(), media, "test".into(), shared.clone(), None)).await.unwrap();
            let first = server.request().await;
            let second = server.request().await;
            assert_eq!(first.lines().next(), second.lines().next());
            assert!(!second.to_ascii_lowercase().contains("range"));
            server.finish().await;
            assert_eq!(shared.bytes().unwrap(), b"ABCDEF");
        }

        #[tokio::test]
        async fn only_a_short_transport_tail_can_finish_empty() {
            for transport in [false, true] {
                let empty = reply("204 No Content", "", "");
                let (replies, expected) = if transport {
                    (vec![reply("200 OK", "Content-Length: 3\r\n", "bad"), empty], b"".as_slice())
                } else {
                    (vec![empty, reply("200 OK", "Content-Length: 3\r\n", "AAC")], b"AAC".as_slice())
                };
                let mut server = Server::new(replies).await;
                let urls = vec![server.url.clone()];
                let media = if transport { Media::TransportStream(urls) } else { Media::Segments(urls) };
                let shared = Shared::segmented(vec![0.0], 2, None, false);
                timeout(WAIT, download(client(), media, "test".into(), shared.clone(), None)).await.unwrap();
                assert_eq!(shared.bytes().as_deref(), Some(expected));
                assert!(shared.failure().is_none());
                server.finish().await;
            }
        }

        #[tokio::test]
        async fn invalid_ranges_and_framing_do_not_change_the_verified_prefix() {
            for (range, reply) in [
                (Range::Header(2, 5), reply("200 OK", "Content-Length: 4\r\n", "CDEF")),
                (Range::Header(2, 5), reply("206 Partial Content", "Content-Length: 4\r\n", "CDEF")),
                (Range::Header(2, 5), reply("206 Partial Content", "Content-Range: bytes 2-5/7\r\n", "CDEF")),
                (Range::Param(2, 5), reply("206 Partial Content", "Content-Range: bytes 1-4/6\r\n", "BCDE")),
                (Range::Param(2, 5), reply("206 Partial Content", "Content-Range: bytes 2-5/6\r\nContent-Range: bytes 1-4/6\r\n", "CDEF")),
                (Range::Param(2, 5), reply("200 OK", "Content-Length: 5\r\n", "CDEFG")),
                (Range::Param(2, 5), reply("200 OK", "Transfer-Encoding: chunked\r\n", "2\r\nCD\r\n3\r\nEFG\r\n0\r\n\r\n")),
                (Range::Param(2, 5), reply("200 OK", "Content-Range: bytes 2-5/6\r\nTransfer-Encoding: chunked\r\n", "4\r\nCDEF\r\n")),
                (Range::Param(2, 5), reply("200 OK", "", "CD")),
            ] {
                let mut server = Server::new(vec![reply]).await;
                let mut bytes = BytesMut::from(&b"AB"[..]);
                let result = timeout(WAIT, fetch(&client(), &server.url, "test", range, 6, &mut bytes, STALL)).await.unwrap();
                assert!(result.is_err());
                assert_eq!(&bytes[..], b"AB");
                server.finish().await;
            }
        }

        #[tokio::test]
        async fn feed_probe_checks_requested_byte_and_total() {
            for (range, valid) in [
                ("bytes 0-0/6", true),
                ("bytes 1-1/6", false),
                ("bytes 0-1/6", false),
                ("bytes 0-0/*", false),
                ("bytes 0-0/0", false),
            ] {
                let mut server = Server::new(vec![reply("206 Partial Content", &format!("Content-Length: 1\r\nContent-Range: {range}\r\n"), "A")]).await;
                let result = timeout(WAIT, probe_feed(&client(), &server.url, "test")).await.unwrap();
                if valid {
                    assert_eq!(result.unwrap(), (server.url.clone(), 6));
                } else {
                    assert!(result.is_err(), "accepted {range}");
                }
                server.finish().await;
            }
        }

        #[tokio::test]
        async fn idle_timeout_restarts_on_progress_and_retains_a_stalled_prefix() {
            let idle = Duration::from_millis(400);
            let mut server = Server::new(vec![Reply::Drip, Reply::Stall]).await;
            let mut bytes = BytesMut::new();
            let started = std::time::Instant::now();
            timeout(WAIT, fetch(&client(), &server.url, "test", Range::Param(0, 6), 7, &mut bytes, idle)).await.unwrap().unwrap();
            assert!(started.elapsed() > idle);
            assert_eq!(&bytes[..], b"ABCDEFG");
            bytes.clear();
            let result = timeout(WAIT, fetch(&client(), &server.url, "test", Range::Param(0, 6), 7, &mut bytes, idle)).await.unwrap();
            assert!(result.unwrap_err().to_string().starts_with("no data"));
            assert_eq!(&bytes[..], b"A");
            server.finish().await;
        }

        #[tokio::test]
        async fn last_reader_drop_cancels_an_in_flight_response() {
            let mut server = Server::new(vec![Reply::Stall]).await;
            let shared = Shared::new(7, 0, None, false, false);
            let reader = StreamReader::new(shared.clone());
            let dl = download(client(), Media::File(server.url.clone()), "test".into(), shared.clone(), None);
            timeout(WAIT, async {
                tokio::join!(dl, async {
                    server.request().await;
                    drop(reader);
                    shared.finished().await;
                });
            }).await.unwrap();
            server.finish().await;
            assert!(!shared.is_complete());
            assert!(shared.error().is_none());
        }

        #[tokio::test]
        async fn cancellation_drops_a_hanging_refresh_before_finishing() {
            let mut server = Server::new(vec![reply("403 Forbidden", "Content-Length: 0\r\n", "")]).await;
            let shared = Shared::new(6, 0, None, false, false);
            let entered = Arc::new(Notify::new());
            let (sent, mut dropped) = tokio::sync::oneshot::channel::<()>();
            let sent = Mutex::new(Some(sent));
            let refresh: Refresh = {
                let entered = entered.clone();
                Arc::new(move || {
                    let entered = entered.clone();
                    let sender = sent.lock().unwrap().take().unwrap();
                    Box::pin(async move {
                        entered.notify_one();
                        std::future::pending::<()>().await;
                        let _ = sender.send(());
                        unreachable!()
                    })
                })
            };
            let dl = download(client(), Media::File(server.url.clone()), "test".into(), shared.clone(), Some(refresh));
            timeout(WAIT, async {
                tokio::join!(dl, async {
                    entered.notified().await;
                    shared.cancel();
                    shared.finished().await;
                    assert_eq!(dropped.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Closed));
                });
            }).await.unwrap();
            server.finish().await;
            assert!(shared.error().is_none());
        }

        #[tokio::test]
        async fn repeated_503_at_the_start_ends_playback_before_the_download_budget() {
            for (playing, refusals) in [(true, 1), (true, 2), (false, 2)] {
                let mut replies: Vec<_> = (0..refusals)
                    .map(|_| reply("503 Service Unavailable", "Content-Length: 0\r\n", ""))
                    .collect();
                let fails = playing && refusals == 2;
                if !fails {
                    replies.push(reply("200 OK", "Content-Length: 3\r\n", "AAC"));
                }
                let mut server = Server::new(replies).await;
                let shared = Shared::new(3, 0, None, false, false);
                shared.mark_after_bot_check();
                let media = Media::File(server.url.clone());
                timeout(WAIT, async {
                    if playing {
                        download(client(), media, "test".into(), shared.clone(), None).await;
                    } else {
                        download_behind(client(), media, "test".into(), shared.clone(), None, None).await;
                    }
                }).await.unwrap();
                if fails {
                    let failure = shared.failure().unwrap();
                    assert!(failure.reason.contains("503"));
                    assert!(!failure.offer_backup);
                    assert!(!shared.is_complete());
                } else {
                    assert_eq!(shared.bytes().as_deref(), Some(b"AAC".as_slice()));
                }
                server.finish().await;
            }
        }

        #[tokio::test]
        async fn terminal_prefetch_error_wakes_idle_workers() {
            let mut server = Server::new(vec![reply("403 Forbidden", "Content-Length: 0\r\n", "")]).await;
            let shared = Shared::new(BLOCK * 3, 0, None, false, true);
            timeout(WAIT, async {
                tokio::join!(
                    download(client(), Media::File(server.url.clone()), "test".into(), shared.clone(), None),
                    shared.finished(),
                );
            }).await.unwrap();
            assert!(!shared.is_complete());
            assert!(shared.refused());
            let error = shared.error().unwrap();
            assert!(error.contains("403"));
            assert!(!error.contains(&server.url));
            server.finish().await;
        }
    }
}
