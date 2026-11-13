//! Discord, Last.fm, ListenBrainz, remote control, the system media controls, the app icon, updates and the tray.

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
        // In real time: a song at 2x is over in half its length.
        let speed = self.audio.status.speed() as f64;
        let start_ms = now - (self.audio.status.position().as_millis() as f64 / speed) as i64;
        let dur = (self.audio.status.duration_ms.load(Ordering::Relaxed) as f64 / speed) as i64;
        // Position ticks every 100 ms; only a jump of 2 s or more is a real seek.
        let same = match (track, &self.discord_sent) {
            (Some(t), Some(b)) => b.video_id == t.id && (start_ms - b.start_ms).abs() < 2000 && b.end_ms.map_or(dur <= 0, |e| (e - (start_ms + dur)).abs() < 2000),
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

    /// Mirror playback to a Stream Deck plugin, and keep the socket open only while the
    /// setting is on. Cheap every frame: clients are told only when the snapshot changes.
    pub(super) fn sync_remote(&mut self, ctx: &egui::Context) {
        if !self.remote_enabled {
            if self.remote.is_running() {
                self.remote.stop();
            }
            return;
        }
        if !self.remote.is_running() {
            let wake = ctx.clone();
            self.remote.start(move || wake.request_repaint());
        }
        let track = self.playing_track();
        self.remote.publish(crate::remote::Snapshot {
            title: track.map(|t| t.title.clone()).unwrap_or_default(),
            artist: track.map(|t| t.artist_line()).unwrap_or_default(),
            album: track.and_then(|t| t.album.as_ref().map(|a| a.name.clone())),
            cover_url: track.and_then(backend::art_url),
            playing: self.playing(),
            liked: self.heard_liked(),
            repeat: match self.queue.repeat {
                super::Repeat::Off => crate::remote::RepeatMode::Off,
                super::Repeat::All => crate::remote::RepeatMode::All,
                super::Repeat::One => crate::remote::RepeatMode::One,
            },
            volume: (self.audio.volume() * 100.0).round() / 100.0,
            position: self.audio.status.position().as_secs(),
            duration: self.audio.status.duration().map(|d| d.as_secs()).unwrap_or(0),
        });
        // The window may be hidden, and nothing else would move the position along.
        if self.playing() {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    /// Tell Last.fm and ListenBrainz what's playing, and scrobble it once it has played
    /// half its length or 4 minutes (songs under 30 s don't count). Runs every frame, like
    /// Discord's.
    pub(super) fn sync_scrobbles(&mut self) {
        let result = self.lastfm_result.lock().unwrap().take();
        if let Some(r) = result {
            match r {
                Ok(name) => {
                    self.toast(format!("Scrobbling to Last.fm as {name}"));
                    // Only when nothing was listening already, so this play isn't sent twice.
                    if self.listenbrainz_user.is_none() {
                        self.scrobble = None;
                    }
                    self.lastfm_user = Some(name);
                }
                Err(e) => self.toast(format!("Last.fm sign-in failed: {e}")),
            }
        }
        let result = self.listenbrainz_result.lock().unwrap().take();
        if let Some(r) = result {
            match r {
                Ok(name) => {
                    self.toast(format!("Sending listens to ListenBrainz as {name}"));
                    if self.lastfm_user.is_none() {
                        self.scrobble = None;
                    }
                    self.listenbrainz_user = Some(name);
                    self.listenbrainz_token.clear();
                }
                Err(e) => self.toast(format!("Couldn't connect ListenBrainz: {e}")),
            }
        }
        let (lastfm, listenbrainz) = (self.lastfm_user.is_some(), self.listenbrainz_user.is_some());
        if !lastfm && !listenbrainz {
            return;
        }
        // A song's music video picked on its switch is still that song.
        let Some(t) = self.heard_song().filter(|_| self.playing() && !self.buffering) else { return };
        let pos = self.audio.status.position();
        let dur = Duration::from_millis(self.audio.status.duration_ms.load(Ordering::Relaxed));
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
        let play = crate::lastfm::Scrobble {
            artist: t.artists.first().map(|a| a.name.clone()).unwrap_or_default(),
            track: t.title.clone(),
            album: t.album.as_ref().map(|a| a.name.clone()),
            duration: (!dur.is_zero()).then_some(dur.as_secs() as u32),
            // When it started, in real time.
            timestamp: now - (pos.as_secs_f64() / self.audio.status.speed() as f64) as i64,
        };
        if play.artist.is_empty() {
            return;
        }
        let listen = crate::listenbrainz::Listen {
            artist: t.artist_line(),
            track: play.track.clone(),
            album: play.album.clone(),
            duration: play.duration,
            timestamp: play.timestamp,
            video_id: t.id.clone(),
        };
        let http = self.backend.http();
        if self.scrobble.is_none_or(|(g, _)| g != self.generation) {
            self.scrobble = Some((self.generation, false));
            if lastfm {
                let (http, p) = (http.clone(), play.clone());
                self.backend.spawn(async move { crate::lastfm::now_playing(&http, &p).await });
            }
            if listenbrainz {
                self.backend.spawn(async move { crate::listenbrainz::now_playing(&http, &listen).await });
            }
            return;
        }
        if dur >= Duration::from_secs(30) && pos >= (dur / 2).min(Duration::from_secs(240)) && self.scrobble.is_some_and(|(_, done)| !done) {
            self.scrobble = Some((self.generation, true));
            if lastfm {
                let http = http.clone();
                self.backend.spawn(async move { crate::lastfm::scrobble(&http, play).await });
            }
            if listenbrainz {
                self.backend.spawn(async move { crate::listenbrainz::listen(&http, listen).await });
            }
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
        if self.listenbrainz_user.is_none() {
            self.scrobble = None;
        }
    }

    /// Check the pasted token with ListenBrainz, then keep it.
    pub(super) fn listenbrainz_connect(&mut self) {
        if self.listenbrainz_busy.swap(true, Ordering::Relaxed) {
            return;
        }
        let token = self.listenbrainz_token.trim().to_owned();
        let (busy, result, http, backend) = (self.listenbrainz_busy.clone(), self.listenbrainz_result.clone(), self.backend.http(), self.backend.clone());
        self.backend.spawn(async move {
            let r = crate::listenbrainz::connect(&http, &token).await.map_err(|e| format!("{e:#}"));
            *result.lock().unwrap() = Some(r);
            busy.store(false, Ordering::Relaxed);
            backend.wake();
        });
    }

    pub(super) fn listenbrainz_sign_out(&mut self) {
        crate::listenbrainz::sign_out();
        self.listenbrainz_user = None;
        if self.lastfm_user.is_none() {
            self.scrobble = None;
        }
    }

    /// Switch the window, tray and shortcuts to another icon.
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
        // `tray_window` makes it again with the new icon.
        self.tray = None;
        log::info!("app icon: {id}");
    }

    /// Automatic update checks: shortly after launch, then every six hours.
    pub(super) fn tick_updates(&mut self, ctx: &egui::Context, now: f64) {
        use crate::update::{State, Updater};
        if self.updater.is_none() {
            let c = ctx.clone();
            self.updater = Some(Updater::new(self.backend.data_dir(), self.update_nightly, move || c.request_repaint()));
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
        } else if enabled && !matches!(u.state(), State::Ready { .. }) {
            // An idle app doesn't repaint on its own: wake up when the next check is due.
            let next = self.update_checked.map_or(8.0, |t| t + 6.0 * 3600.0);
            ctx.request_repaint_after(Duration::from_secs_f64((next - now).max(0.5) + 0.1));
        }
        match u.state() {
            State::Ready { version, .. } if !self.update_announced => {
                self.update_announced = true;
                self.notes_dialog = Some(notes::NotesDialog { version, ready: true });
            }
            // The .deb: once per version, not every launch (the sidebar keeps saying so).
            State::Available { version, .. } if !self.update_announced && self.update_seen.as_ref() != Some(&version) => {
                self.update_announced = true;
                self.update_seen = Some(version.clone());
                self.notes_dialog = Some(notes::NotesDialog { version, ready: true });
            }
            _ => {}
        }
    }

    /// Switch between the stable and nightly update channels, and look for an update
    /// on the new one if updates are automatic.
    pub(super) fn set_update_nightly(&mut self, nightly: bool) {
        if nightly == self.update_nightly {
            return;
        }
        self.update_nightly = nightly;
        log::info!("update channel: {}", if nightly { "nightly" } else { "stable" });
        if let Some(u) = &self.updater {
            u.set_nightly(nightly);
            if self.auto_update && updatable() {
                u.check();
            }
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

    /// "Close to tray": while it's on there's a tray icon, and × hides the window
    /// behind it. The icon brings the window back or quits. eframe keeps running
    /// `logic` while the window is hidden.
    ///
    /// On Wayland a window can't hide itself, so × minimizes it instead. It can't
    /// bring itself back either: the compositor is only asked for attention, and the
    /// window counts as hidden until it has focus again.
    pub(super) fn tray_window(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        // Turning the setting off and on again tries again after a failure.
        self.tray_failed &= self.close_to_tray;
        let want = self.close_to_tray && !self.exiting && !self.tray_failed;
        if want && self.tray.is_none() {
            let c = ctx.clone();
            match crate::tray::Tray::new(crate::appicon::get(self.app_icon), move || c.request_repaint()) {
                Ok(t) => self.tray = Some(t),
                // Without an icon there'd be no way back: × just closes.
                Err(e) => {
                    log::warn!("tray icon unavailable: {e:#}");
                    self.tray_failed = true;
                }
            }
        } else if !want && self.tray.is_some() {
            self.tray = None;
            if self.in_tray && !self.exiting {
                self.show_window(ctx);
            }
        }
        if self.tray.is_none() {
            return;
        }
        let wayland = is_wayland(frame);
        if wayland && self.in_tray && back_from_tray(ctx.input(|i| i.viewport().focused) != Some(false), &mut self.tray_left_focus) {
            self.in_tray = false;
        }
        let hide = |app: &mut Self| {
            ctx.send_viewport_cmd(hide_command(wayland));
            app.in_tray = true;
            app.tray_left_focus = false;
        };
        let show = |app: &mut Self| {
            if wayland {
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
            } else {
                app.show_window(ctx);
            }
        };
        if ctx.input(|i| i.viewport().close_requested()) && !self.exiting {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            hide(self);
        }
        // Minimized counts as hidden: "Show" is what brings it back.
        let shown = |app: &Self| !app.in_tray && ctx.input(|i| i.viewport().minimized) != Some(true);
        for cmd in crate::tray::take() {
            match cmd {
                crate::tray::Cmd::Show => show(self),
                crate::tray::Cmd::Toggle if shown(self) => hide(self),
                crate::tray::Cmd::Toggle => show(self),
                // As the media keys do.
                crate::tray::Cmd::PlayPause => self.toggle(),
                crate::tray::Cmd::Next => self.next(),
                crate::tray::Cmd::Prev => self.prev(),
                crate::tray::Cmd::Quit => self.quit(ctx),
            }
        }
        let shown = shown(self);
        // "Pause" while a song is starting too, as the media controls have it.
        let playing = self.playing() || self.buffering;
        let song = self.current().map(|t| (t.title.clone(), t.artist_line()));
        if let Some(t) = &mut self.tray {
            t.set_shown(shown);
            t.set_now(song.as_ref().map(|(title, artist)| (title.as_str(), artist.as_str())), playing);
        }
    }

    /// Bring the window back (from the tray, or minimized) and to the front.
    pub(super) fn show_window(&mut self, ctx: &egui::Context) {
        self.in_tray = false;
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

pub(super) fn is_wayland(frame: &eframe::Frame) -> bool {
    use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
    frame.display_handle().is_ok_and(|h| matches!(h.as_raw(), RawDisplayHandle::Wayland(_)))
}

/// `Visible(false)` does nothing on Wayland; minimizing is what's left.
fn hide_command(wayland: bool) -> egui::ViewportCommand {
    if wayland { egui::ViewportCommand::Minimized(true) } else { egui::ViewportCommand::Visible(false) }
}

/// Wayland doesn't say when a minimized window comes back (from the taskbar, say),
/// but it gets focus again. `left_focus` needs the focus lost first, since the window
/// still has it for a moment after being minimized.
fn back_from_tray(focused: bool, left_focus: &mut bool) -> bool {
    if !focused {
        *left_focus = true;
        return false;
    }
    std::mem::take(left_focus)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wayland_minimizes_because_it_cannot_hide() {
        assert_eq!(hide_command(true), egui::ViewportCommand::Minimized(true));
        assert_eq!(hide_command(false), egui::ViewportCommand::Visible(false));
    }

    #[test]
    fn stays_in_tray_until_focus_is_lost_and_regained() {
        let mut left = false;
        // Still focused just after minimizing: not back yet.
        assert!(!back_from_tray(true, &mut left));
        assert!(!back_from_tray(false, &mut left));
        assert!(!back_from_tray(false, &mut left));
        assert!(back_from_tray(true, &mut left));
        // Once only.
        assert!(!back_from_tray(true, &mut left));
    }
}
