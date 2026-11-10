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
/// A prefetch only fetches this much (~16 s of audio) until playback actually opens it:
/// enough to start at once, without holding a whole extra song in memory.
const HEAD_BLOCKS: usize = 2;
/// Files larger than this (about 20 minutes at 256 kbps: long mixes, whole albums in
/// one video) aren't kept whole. Each reader keeps a window around where it reads,
/// `AHEAD` blocks forward (about 5 minutes at 256 kbps) and `BEHIND` back; the rest is dropped and
/// fetched again if a seek goes back to it.
const WINDOWED_FROM: u64 = 40 * 1024 * 1024;
const AHEAD: usize = 40;
const BEHIND: usize = 8;

/// What a download fetches.
pub enum Media {
    /// One file, in `BLOCK`-sized ranges.
    File(String),
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
        Self::build(total.div_ceil(BLOCK) as usize, total, total > WINDOWED_FROM, duration_ms, loudness_db, opus, None, head_only)
    }

    /// A buffer for an HLS stream's segments, starting at `starts` (s).
    pub fn segmented(starts: Vec<f64>, duration_ms: u64, loudness_db: Option<f32>, head_only: bool) -> Arc<Self> {
        // About as many segments (~7 s) as a windowed file has blocks (~8 s at 256 kbps).
        let windowed = starts.len() as u64 > WINDOWED_FROM / BLOCK;
        Self::build(starts.len(), 0, windowed, duration_ms, loudness_db, false, Some(starts.into()), head_only)
    }

    #[allow(clippy::too_many_arguments)]
    fn build(n: usize, total: u64, windowed: bool, duration_ms: u64, loudness_db: Option<f32>, opus: bool, segments: Option<Arc<[f64]>>, head_only: bool) -> Arc<Self> {
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
            segments,
            done: tokio::sync::watch::Sender::new(false),
            changed: tokio::sync::Notify::new(),
        })
    }

    /// A buffer that already holds the whole file (a downloaded song).
    pub fn from_bytes(data: Vec<u8>, duration_ms: u64, loudness_db: Option<f32>) -> Arc<Self> {
        let total = data.len() as u64;
        let data = Bytes::from(data);
        let blocks: Vec<Option<Bytes>> =
            (0..total.div_ceil(BLOCK)).map(|i| Some(data.slice(i as usize * BLOCK as usize..((i + 1) * BLOCK).min(total) as usize))).collect();
        let n = blocks.len();
        // Saved songs are AAC (downloads take that stream).
        let shared = Self::new(total, duration_ms, loudness_db, false, false);
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

    fn fail(&self, err: String, refused: bool) {
        log::error!("download failed: {err}");
        FAIL_FROM.store(usize::MAX, std::sync::atomic::Ordering::Relaxed);
        let mut inner = self.inner.lock().unwrap();
        inner.error = Some(err);
        inner.refused = refused;
        drop(inner);
        self.ready.notify_all();
    }

    fn range(&self, idx: usize) -> (u64, u64) {
        let start = idx as u64 * BLOCK;
        (start, (start + BLOCK).min(self.total) - 1)
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
            Ok(Media::File(_)) if self.segments.is_some() => {
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

/// Download into `shared` with parallel range requests; returns when complete,
/// failed, or every reader was dropped.
pub async fn download(http: reqwest::Client, media: Media, ua: String, shared: Arc<Shared>, refresh: Option<Refresh>) {
    let segments = shared.segments.as_ref().map(|s| s.len());
    let slot = Arc::new(UrlSlot { url: tokio::sync::Mutex::new((media, 0)), refresh, segments });
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..WORKERS {
        let (http, ua, shared, slot) = (http.clone(), ua.clone(), shared.clone(), slot.clone());
        set.spawn(async move {
            loop {
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
                    let (url, expect, version) = {
                        let slot = slot.url.lock().await;
                        match &slot.0 {
                            Media::File(url) => (format!("{url}&range={a}-{b}"), Some(b - a + 1), slot.1),
                            Media::Segments(urls) => (urls[idx].clone(), None, slot.1),
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
                        fetch(&client, &url, &ua, expect).await
                    };
                    match res {
                        Ok(data) => {
                            shared.put(idx, data);
                            break;
                        }
                        Err(e) if is_forbidden(&e) && slot.renew(version).await => {}
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
/// block on with HTTP 403, fresh URLs too, as YouTube does past 1 MiB (block 4) where
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

/// `expect`: the exact length a range must come back with (a segment's isn't known).
async fn fetch(http: &reqwest::Client, url: &str, ua: &str, expect: Option<u64>) -> anyhow::Result<Bytes> {
    let stalled = || anyhow::anyhow!("no data for {} s", STALL.as_secs());
    // Errors leave the URL out: it carries the client's IP address and signatures (logs).
    let mut resp = tokio::time::timeout(STALL, http.get(url).header("User-Agent", ua).send())
        .await
        .map_err(|_| stalled())?
        .and_then(reqwest::Response::error_for_status)
        .map_err(reqwest::Error::without_url)?;
    let mut buf = bytes::BytesMut::with_capacity(expect.or(resp.content_length()).unwrap_or(0) as usize);
    while let Some(chunk) = tokio::time::timeout(STALL, resp.chunk()).await.map_err(|_| stalled())?.map_err(reqwest::Error::without_url)? {
        buf.extend_from_slice(&chunk);
    }
    if let Some(expect) = expect {
        anyhow::ensure!(buf.len() as u64 == expect, "short read: {} of {expect} bytes", buf.len());
    }
    Ok(buf.freeze())
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
        let idx = (self.pos / BLOCK) as usize;
        let block = self.block_at(idx)?;
        let off = (self.pos - idx as u64 * BLOCK) as usize;
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
    Ok(TrackDecoder::Aac(
        rodio::Decoder::builder()
            .with_data(StreamReader::new(shared))
            .with_byte_len(size)
            .with_seekable(seekable)
            .with_mime_type("audio/mp4")
            .with_hint("m4a")
            .build()?,
    ))
}

/// A song's decoder: AAC (rodio's own) or Opus.
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
