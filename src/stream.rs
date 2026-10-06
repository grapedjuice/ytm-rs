//! Parallel, seek-aware HTTP audio buffer.
//!
//! The file is split into fixed blocks fetched by a few concurrent range requests.
//! Workers always claim the first missing block at or after the one a reader is
//! waiting on, so playback start and seeks are served first and the rest fills in
//! around them. Reads of a missing block block until it arrives (or the download fails).
//!
//! Several readers may share one buffer (the audio engine opens a second, seekable
//! decoder when the user seeks); the download stops once the last reader is dropped.

use std::future::Future;
use std::io::{self, Read, Seek, SeekFrom};
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

const BLOCK: u64 = 256 * 1024;
const WORKERS: usize = 4;
const RETRIES: u32 = 3;
/// googlevideo occasionally starts rejecting a URL partway through (HTTP 403);
/// a freshly resolved URL for the same video resumes the download.
const MAX_REFRESHES: u32 = 2;

/// Resolves a fresh stream URL for the same video.
pub type Refresh = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send>> + Send + Sync>;

struct Inner {
    blocks: Vec<Option<Box<[u8]>>>,
    in_flight: Vec<bool>,
    /// Block a reader most recently needed; workers prioritize from here.
    want: usize,
    error: Option<String>,
    readers: usize,
    /// Set once a reader existed and all were dropped.
    cancelled: bool,
}

pub struct Shared {
    inner: Mutex<Inner>,
    ready: Condvar,
    pub total: u64,
}

impl Shared {
    pub fn new(total: u64) -> Arc<Self> {
        let n = total.div_ceil(BLOCK) as usize;
        Arc::new(Self {
            inner: Mutex::new(Inner {
                blocks: vec![None; n],
                in_flight: vec![false; n],
                want: 0,
                error: None,
                readers: 0,
                cancelled: false,
            }),
            ready: Condvar::new(),
            total,
        })
    }

    /// Stop downloading (e.g. a prefetch that was superseded).
    pub fn cancel(&self) {
        self.inner.lock().unwrap().cancelled = true;
    }

    fn claim(&self) -> Option<usize> {
        let mut inner = self.inner.lock().unwrap();
        if inner.cancelled || inner.error.is_some() {
            return None;
        }
        let n = inner.blocks.len();
        let start = inner.want.min(n.saturating_sub(1));
        let idx = (start..n)
            .chain(0..start)
            .find(|&i| inner.blocks[i].is_none() && !inner.in_flight[i])?;
        inner.in_flight[idx] = true;
        Some(idx)
    }

    fn put(&self, idx: usize, data: Box<[u8]>) {
        let mut inner = self.inner.lock().unwrap();
        inner.blocks[idx] = Some(data);
        inner.in_flight[idx] = false;
        self.ready.notify_all();
    }

    fn fail(&self, err: String) {
        log::error!("download failed: {err}");
        self.inner.lock().unwrap().error = Some(err);
        self.ready.notify_all();
    }

    fn range(&self, idx: usize) -> (u64, u64) {
        let start = idx as u64 * BLOCK;
        (start, (start + BLOCK).min(self.total) - 1)
    }
}

/// The current URL plus how many times it has been replaced.
struct UrlSlot {
    url: tokio::sync::Mutex<(String, u32)>,
    refresh: Option<Refresh>,
}

impl UrlSlot {
    /// Replace the URL if `seen` is still current; returns false when out of refreshes.
    async fn renew(&self, seen: u32) -> bool {
        let mut slot = self.url.lock().await;
        if slot.1 != seen {
            return true; // another worker already refreshed it
        }
        let Some(refresh) = self.refresh.as_ref().filter(|_| slot.1 < MAX_REFRESHES) else { return false };
        match refresh().await {
            Ok(url) => {
                log::info!("stream URL refreshed after 403");
                *slot = (url, slot.1 + 1);
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
pub async fn download(http: reqwest::Client, url: String, ua: String, shared: Arc<Shared>, refresh: Option<Refresh>) {
    let slot = Arc::new(UrlSlot { url: tokio::sync::Mutex::new((url, 0)), refresh });
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..WORKERS {
        let (http, ua, shared, slot) = (http.clone(), ua.clone(), shared.clone(), slot.clone());
        set.spawn(async move {
            while let Some(idx) = shared.claim() {
                let (a, b) = shared.range(idx);
                let mut attempt = 0;
                loop {
                    let (url, version) = slot.url.lock().await.clone();
                    match fetch(&http, &format!("{url}&range={a}-{b}"), &ua, b - a + 1).await {
                        Ok(data) => {
                            shared.put(idx, data);
                            break;
                        }
                        Err(e) if is_forbidden(&e) && slot.renew(version).await => {}
                        Err(e) if attempt < RETRIES && !is_forbidden(&e) => {
                            attempt += 1;
                            log::warn!("range {a}-{b}: {e:#} (retry {attempt})");
                            tokio::time::sleep(Duration::from_millis(250 * attempt as u64)).await;
                        }
                        Err(e) => {
                            shared.fail(format!("range {a}-{b}: {e:#}"));
                            return;
                        }
                    }
                }
            }
        });
    }
    while set.join_next().await.is_some() {}
}

fn is_forbidden(e: &anyhow::Error) -> bool {
    e.downcast_ref::<reqwest::Error>()
        .and_then(reqwest::Error::status)
        .is_some_and(|s| s == reqwest::StatusCode::FORBIDDEN)
}

async fn fetch(http: &reqwest::Client, url: &str, ua: &str, expect: u64) -> anyhow::Result<Box<[u8]>> {
    let bytes = http
        .get(url)
        .header("User-Agent", ua)
        .timeout(Duration::from_secs(15))
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    anyhow::ensure!(bytes.len() as u64 == expect, "short read: {} of {expect} bytes", bytes.len());
    Ok(Box::from(&bytes[..]))
}

pub struct StreamReader {
    shared: Arc<Shared>,
    pos: u64,
}

impl StreamReader {
    pub fn new(shared: Arc<Shared>) -> Self {
        shared.inner.lock().unwrap().readers += 1;
        Self { shared, pos: 0 }
    }
}

impl Drop for StreamReader {
    fn drop(&mut self) {
        let mut inner = self.shared.inner.lock().unwrap();
        inner.readers -= 1;
        if inner.readers == 0 {
            // Stops the workers once their current request finishes.
            inner.cancelled = true;
        }
    }
}

impl Read for StreamReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.pos >= self.shared.total {
            return Ok(0);
        }
        let idx = (self.pos / BLOCK) as usize;
        let mut inner = self.shared.inner.lock().unwrap();
        inner.want = idx;
        while inner.blocks[idx].is_none() {
            if let Some(e) = &inner.error {
                return Err(io::Error::other(e.clone()));
            }
            inner = self.shared.ready.wait(inner).unwrap();
        }
        let block = inner.blocks[idx].as_ref().unwrap();
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

/// Open a decoder over `shared`. Non-seekable decoders start after the first fragment;
/// seekable ones index every fragment first (i.e. effectively wait for the download).
pub fn decoder(shared: Arc<Shared>, seekable: bool) -> Result<rodio::Decoder<StreamReader>, rodio::decoder::DecoderError> {
    let size = shared.total;
    rodio::Decoder::builder()
        .with_data(StreamReader::new(shared))
        .with_byte_len(size)
        .with_seekable(seekable)
        .with_mime_type("audio/mp4")
        .with_hint("m4a")
        .build()
}
