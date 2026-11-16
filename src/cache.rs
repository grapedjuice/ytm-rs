//! What Settings › Storage clears: what the app fetched and keeps only to be quicker, all
//! of which it fetches again when needed. Sign-in, settings, downloads (and their lyrics),
//! the helpers playback runs on, and logs stay.

use std::path::Path;
use std::sync::{Arc, Mutex};

/// Folders in the data folder that are only a cache.
const DIRS: &[&str] = &[
    // Covers and thumbnails, kept so the next launch shows them at once.
    "thumbs",
    // Romanized and translated lyrics.
    "translations",
    // The browser profile the lyrics check runs in (its token, `lyrics_jwt`, stays).
    "webview-lyrics",
    // Downloaded updates: an installer, an unpacked build, the last install's log.
    "updates",
    // Error reports from versions up to 0.2.11.
    "rustypipe_reports",
];

/// The folder of downloaded updates, left alone while one downloads or waits to install.
const UPDATES: &str = "updates";

/// Bytes the cache takes up.
pub fn size(data_dir: &Path) -> u64 {
    DIRS.iter().map(|d| dir_size(&data_dir.join(d))).sum()
}

/// Delete the cache, keeping downloaded updates when `keep_updates`. Files in use (the
/// lyrics check's window, if it's open) are skipped. Returns the bytes freed.
pub fn clear(data_dir: &Path, keep_updates: bool) -> u64 {
    DIRS.iter().filter(|d| !(keep_updates && **d == UPDATES)).map(|d| clear_dir(&data_dir.join(d))).sum()
}

fn dir_size(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map_or(0, |m| m.len()),
            _ => 0,
        })
        .sum()
}

/// Everything in `dir`, but `dir` itself stays: what's running writes into it.
fn clear_dir(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    let mut freed = 0;
    for e in rd.flatten() {
        let path = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => {
                freed += clear_dir(&path);
                let _ = std::fs::remove_dir(&path);
            }
            Ok(_) => {
                let len = e.metadata().map_or(0, |m| m.len());
                if std::fs::remove_file(&path).is_ok() {
                    freed += len;
                }
            }
            Err(_) => {}
        }
    }
    freed
}

/// The cache's size as Settings shows it, measured and cleared off the UI thread.
#[derive(Default)]
pub struct Storage {
    inner: Arc<Mutex<Inner>>,
    /// When it was last measured (app time, s).
    measured: Option<f64>,
}

#[derive(Default)]
struct Inner {
    size: Option<u64>,
    clearing: bool,
    /// What the last clear freed, until it's said.
    freed: Option<u64>,
}

/// Measured again this often while Settings shows it: covers keep coming in.
const REMEASURE: f64 = 10.0;

impl Storage {
    /// The size, `None` until it's measured.
    pub fn size(&mut self, ctx: &egui::Context, data_dir: &Path, now: f64) -> Option<u64> {
        let stale = self.measured.is_none_or(|at| now - at > REMEASURE);
        if stale && !self.clearing() {
            self.measured = Some(now);
            let (inner, dir, ctx) = (self.inner.clone(), data_dir.to_owned(), ctx.clone());
            std::thread::spawn(move || {
                let n = size(&dir);
                inner.lock().unwrap().size = Some(n);
                ctx.request_repaint();
            });
        }
        self.inner.lock().unwrap().size
    }

    /// The size last measured, without measuring again.
    pub fn known(&self) -> Option<u64> {
        self.inner.lock().unwrap().size
    }

    pub fn clearing(&self) -> bool {
        self.inner.lock().unwrap().clearing
    }

    pub fn clear(&mut self, ctx: &egui::Context, data_dir: &Path, keep_updates: bool) {
        let mut g = self.inner.lock().unwrap();
        if g.clearing {
            return;
        }
        g.clearing = true;
        drop(g);
        let (inner, dir, ctx) = (self.inner.clone(), data_dir.to_owned(), ctx.clone());
        std::thread::spawn(move || {
            let freed = clear(&dir, keep_updates);
            let left = size(&dir);
            log::info!("storage: cleared the cache, {} freed, {} left", size_text(freed), size_text(left));
            let mut g = inner.lock().unwrap();
            (g.size, g.freed, g.clearing) = (Some(left), Some(freed), false);
            drop(g);
            ctx.request_repaint();
        });
    }

    /// What the last clear freed, once.
    pub fn take_freed(&self) -> Option<u64> {
        self.inner.lock().unwrap().freed.take()
    }
}

/// `bytes` for people: "0 MB", "12 MB", "1.4 GB".
pub fn size_text(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let mb = bytes as f64 / MB;
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else if bytes > 0 && mb < 1.0 {
        "Less than 1 MB".to_owned()
    } else {
        format!("{} MB", mb.round() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clears_the_cache_and_keeps_the_rest() {
        let dir = std::env::temp_dir().join("ytm-rs-cache-test");
        let _ = std::fs::remove_dir_all(&dir);
        for (path, len) in [
            ("thumbs/a", 100),
            ("webview-lyrics/EBWebView/Default/Cookies", 50),
            ("updates/ytm-rs-setup.exe", 1000),
            ("lyrics/song.lrc", 7),
            ("webview/Cookies", 9),
            ("app.ron", 3),
            ("lyrics_jwt", 2),
        ] {
            let p = dir.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, vec![0u8; len]).unwrap();
        }
        assert_eq!(size(&dir), 1150);
        assert_eq!(clear(&dir, true), 150);
        assert!(dir.join("updates/ytm-rs-setup.exe").exists());
        assert!(dir.join("thumbs").is_dir() && !dir.join("thumbs/a").exists());
        assert!(!dir.join("webview-lyrics/EBWebView").exists());
        assert_eq!(clear(&dir, false), 1000);
        assert_eq!(size(&dir), 0);
        for kept in ["lyrics/song.lrc", "webview/Cookies", "app.ron", "lyrics_jwt"] {
            assert!(dir.join(kept).exists(), "{kept}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sizes_read_plainly() {
        assert_eq!(size_text(0), "0 MB");
        assert_eq!(size_text(1000), "Less than 1 MB");
        assert_eq!(size_text(98 * 1024 * 1024 + 300_000), "98 MB");
        assert_eq!(size_text(1536 * 1024 * 1024), "1.5 GB");
    }
}
