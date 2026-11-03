//! Discord, Last.fm, the system media controls, the app icon, updates and the tray.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::backend;

use super::{App, dev_var, notes, updatable};

impl App {
    pub(super) fn set_discord_enabled(&mut self, enabled: bool) {
        self.discord_enabled = enabled;
        if enabled {
            if self.discord.is_none() {
                self.discord = crate::discord::Discord::spawn();
            }
        } else {
            if let Some(discord) = self.discord.take() {
                discord.set(None);
            }
            self.discord_sent = None;
        }
    }

    /// Mirror playback into Discord. Cheap enough to run every frame: it only sends on a
    /// new song, play/pause, a seek, or a newly learned duration.
    pub(super) fn sync_discord(&mut self) {
        let Some(d) = &self.discord else { return };
        let track = self.playing_track().filter(|_| self.playing() && !self.buffering);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64;
        let start_ms = now - self.audio.status.position().as_millis() as i64;
        let dur = self.audio.status.duration_ms.load(Ordering::Relaxed) as i64;
        // Position ticks every 100 ms; only a jump of 2 s or more is a real seek.
        let same = match (track, &self.discord_sent) {
            (Some(t), Some(b)) => b.video_id == t.id && (start_ms - b.start_ms).abs() < 2000 && b.end_ms.is_some() == (dur > 0),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        let want = track.map(|t| crate::discord::Presence {
            video_id: t.id.clone(),
            title: t.title.clone(),
            artist: t.artist_line(),
            album: t.album.as_ref().map(|a| a.name.clone()),
            art: backend::art_url(t),
            start_ms,
            end_ms: (dur > 0).then_some(start_ms + dur),
        });
        d.set(want.clone());
        self.discord_sent = want;
    }

    /// Tell Last.fm what's playing, and scrobble it once it has played half its length
    /// or 4 minutes (songs under 30 s don't count). Runs every frame, like Discord's.
    pub(super) fn sync_lastfm(&mut self) {
        let result = self.lastfm_result.lock().unwrap().take();
        if let Some(r) = result {
            match r {
                Ok(name) => {
                    self.toast(format!("Scrobbling to Last.fm as {name}"));
                    self.lastfm_user = Some(name);
                    self.scrobble = None;
                }
                Err(e) => self.toast(format!("Last.fm sign-in failed: {e}")),
            }
        }
        if self.lastfm_user.is_none() {
            return;
        }
        let Some(t) = self.playing_track().filter(|_| self.playing() && !self.buffering) else { return };
        let pos = self.audio.status.position();
        let dur = Duration::from_millis(self.audio.status.duration_ms.load(Ordering::Relaxed));
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
        let play = crate::lastfm::Scrobble {
            artist: t.artists.first().map(|a| a.name.clone()).unwrap_or_default(),
            track: t.title.clone(),
            album: t.album.as_ref().map(|a| a.name.clone()),
            duration: (!dur.is_zero()).then_some(dur.as_secs() as u32),
            timestamp: now - pos.as_secs() as i64,
        };
        if play.artist.is_empty() {
            return;
        }
        let http = self.backend.http();
        if self.scrobble.is_none_or(|(g, _)| g != self.generation) {
            self.scrobble = Some((self.generation, false));
            let p = play.clone();
            self.backend.spawn(async move { crate::lastfm::now_playing(&http, &p).await });
            return;
        }
        if dur >= Duration::from_secs(30) && pos >= (dur / 2).min(Duration::from_secs(240)) && self.scrobble.is_some_and(|(_, done)| !done) {
            self.scrobble = Some((self.generation, true));
            self.backend.spawn(async move { crate::lastfm::scrobble(&http, play).await });
        }
    }

    pub(super) fn lastfm_sign_in(&mut self) {
        if self.lastfm_busy.swap(true, Ordering::Relaxed) {
            return;
        }
        let (busy, result, http, backend) = (self.lastfm_busy.clone(), self.lastfm_result.clone(), self.backend.http(), self.backend.clone());
        self.backend.spawn(async move {
            let r = crate::lastfm::sign_in(&http).await.map_err(|e| format!("{e:#}"));
            *result.lock().unwrap() = Some(r);
            busy.store(false, Ordering::Relaxed);
            backend.wake();
        });
    }

    pub(super) fn lastfm_sign_out(&mut self) {
        crate::lastfm::sign_out();
        self.lastfm_user = None;
        self.scrobble = None;
    }

    /// Switch the window, tray-to-be and shortcuts to another icon.
    pub(crate) fn set_app_icon(&mut self, ctx: &egui::Context, id: &'static str) {
        if id == self.app_icon {
            return;
        }
        self.app_icon = id;
        let v = crate::appicon::get(id);
        crate::appicon::save(self.backend.data_dir(), id);
        if cfg!(windows) {
            crate::appicon::set_window_icon(v);
        } else if let Some(icon) = v.icon_data() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Icon(Some(Arc::new(icon))));
        }
        crate::appicon::sync_shortcuts(id, self.backend.data_dir());
        log::info!("app icon: {id}");
    }

    /// Automatic update checks: shortly after launch, then every six hours.
    pub(super) fn tick_updates(&mut self, ctx: &egui::Context, now: f64) {
        use crate::update::{State, Updater};
        if self.updater.is_none() {
            let c = ctx.clone();
            self.updater = Some(Updater::new(self.backend.data_dir(), move || c.request_repaint()));
        }
        let Some(u) = self.updater.clone() else { return };
        let due = match self.update_checked {
            None => now > 8.0,
            Some(t) => now - t > 6.0 * 3600.0 && !matches!(u.state(), State::Ready { .. }),
        };
        let enabled = self.auto_update && updatable() && dev_var("YTM_BACKGROUND").is_none_or(|_| dev_var("YTM_UPDATE_FORCE").is_some() || dev_var("YTM_UPDATE_FEED").is_some());
        if enabled && due {
            self.update_checked = Some(now);
            u.check();
        } else if enabled {
            // An idle app doesn't repaint on its own: wake up when the next check is due.
            let next = self.update_checked.map_or(8.0, |t| t + 6.0 * 3600.0);
            ctx.request_repaint_after(Duration::from_secs_f64((next - now).max(0.5) + 0.1));
        }
        if let State::Ready { version, .. } = u.state()
            && !self.update_announced
        {
            self.update_announced = true;
            self.notes_dialog = Some(notes::NotesDialog { version, ready: true });
        }
    }

    /// Close the app so the downloaded update installs, then relaunch it.
    pub(super) fn restart_to_update(&mut self, ctx: &egui::Context) {
        self.restart_for_update = true;
        self.quit(ctx);
    }

    /// Close and start again, e.g. so a new proxy setting takes effect.
    pub(super) fn restart(&mut self, ctx: &egui::Context) {
        self.restart_app = true;
        self.quit(ctx);
    }

    /// Close the app for real, even with "Close to tray" on.
    pub(super) fn quit(&mut self, ctx: &egui::Context) {
        self.exiting = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// "Close to tray": × hides the window behind a tray icon; the icon brings it
    /// back or quits. eframe keeps running `logic` while the window is hidden.
    pub(super) fn tray_window(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && self.close_to_tray && !self.exiting {
            let c = ctx.clone();
            match crate::tray::Tray::new(crate::appicon::get(self.app_icon), move || c.request_repaint()) {
                Ok(t) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    self.tray = Some(t);
                }
                // Without an icon there'd be no way back: just close.
                Err(e) => log::warn!("tray icon unavailable: {e:#}"),
            }
        }
        if self.tray.is_none() {
            return;
        }
        // The first click decides; the icon goes away with it.
        match crate::tray::take().into_iter().next() {
            Some(crate::tray::Cmd::Show) => self.show_window(ctx),
            Some(crate::tray::Cmd::Quit) => {
                self.tray = None;
                self.quit(ctx);
            }
            None => {}
        }
    }

    /// Bring the window back (from the tray, or minimized) and to the front.
    pub(super) fn show_window(&mut self, ctx: &egui::Context) {
        self.tray = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    /// Download the media controls' cover through our own client, into a file.
    pub(super) fn fetch_media_cover(&self, video_id: String, url: String) {
        let (slot, b) = (self.media_cover.clone(), self.backend.clone());
        let dir = self.backend.data_dir().join("media-cover");
        self.backend.spawn(async move {
            let res = async {
                let bytes = b.http().get(&url).send().await?.error_for_status()?.bytes().await?;
                // A new name per song, so Windows doesn't show a cached older cover.
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir)?;
                let path = dir.join(format!("{video_id}.jpg"));
                std::fs::write(&path, &bytes)?;
                anyhow::Ok(path)
            }
            .await;
            match res {
                Ok(path) => {
                    *slot.lock().unwrap() = Some((video_id, format!("file://{}", path.display())));
                    b.wake();
                }
                Err(e) => log::debug!("media cover: {e:#}"),
            }
        });
    }
}
