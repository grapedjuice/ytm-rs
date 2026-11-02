//! The Google and Spotify sign-in windows, and uploading songs to the account.

use std::sync::atomic::Ordering;

use crate::backend::Req;

use super::App;

impl App {
    /// Open the system file picker for songs to upload; it runs on its own thread.
    pub(super) fn pick_uploads(&self, ctx: &egui::Context) {
        let (slot, ctx) = (self.picked_uploads.clone(), ctx.clone());
        std::thread::spawn(move || {
            let files = rfd::FileDialog::new().set_title("Upload music").add_filter("Audio", UPLOAD_EXTS).pick_files();
            if let Some(files) = files {
                *slot.lock().unwrap() = Some(files);
                ctx.request_repaint();
            }
        });
    }

    pub(super) fn upload_files(&mut self, files: Vec<std::path::PathBuf>) {
        let (ok, skipped): (Vec<_>, Vec<_>) = files.into_iter().partition(|f| {
            f.extension().and_then(|e| e.to_str()).is_some_and(|e| UPLOAD_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        });
        if !self.logged_in {
            self.toast("Sign in to upload music");
        } else if ok.is_empty() {
            if !skipped.is_empty() {
                self.toast("YouTube Music only takes MP3, M4A, FLAC, OGG and WMA files");
            }
        } else {
            self.toast(if ok.len() == 1 { "Uploading 1 song…".to_owned() } else { format!("Uploading {} songs…", ok.len()) });
            self.backend.request(Req::Upload(ok));
        }
    }

    pub(super) fn start_spotify_sign_in(&self, ctx: &egui::Context) {
        let xfer = self.transfer.clone();
        if xfer.signing_in.swap(true, Ordering::Relaxed) {
            return;
        }
        let (backend, ctx) = (self.backend.clone(), ctx.clone());
        crate::login::spawn(crate::login::Site::Spotify, xfer.spotify_profile(), move |outcome| {
            match outcome {
                crate::login::Outcome::Cookie(c) => xfer.set_cookie(Some(c), &backend),
                crate::login::Outcome::Cancelled => {}
                crate::login::Outcome::Failed(e) => backend.report_error(format!("Spotify sign-in window failed: {e}")),
            }
            xfer.signing_in.store(false, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }

    pub(super) fn start_sign_in(&self, ctx: &egui::Context) {
        if self.signing_in.swap(true, Ordering::Relaxed) {
            return;
        }
        let (backend, busy, ctx) = (self.backend.clone(), self.signing_in.clone(), ctx.clone());
        crate::login::spawn(crate::login::Site::Google, self.backend.login_profile(), move |outcome| {
            match outcome {
                crate::login::Outcome::Cookie(c) => backend.request(Req::SetCookie(c)),
                crate::login::Outcome::Cancelled => {}
                crate::login::Outcome::Failed(e) => backend.report_error(format!("Sign-in window failed: {e}")),
            }
            busy.store(false, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }
}

/// File types YouTube Music accepts for upload.
const UPLOAD_EXTS: &[&str] = &["mp3", "m4a", "flac", "ogg", "wma"];
