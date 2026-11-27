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

    /// What's playing, as Discord would get it, paused or not.
    pub(super) fn discord_presence(&self) -> Option<crate::discord::Presence> {
        let t = self.playing_track()?;
        let now = crate::unix_ms() as i64;
        // In real time: a song at 2x is over in half its length.
        let speed = self.audio.status.speed() as f64;
        let start_ms = now - (self.audio.status.position().as_millis() as f64 / speed) as i64;
        let dur = (self.audio.status.duration_ms.load(Ordering::Relaxed) as f64 / speed) as i64;
        Some(crate::discord::Presence {
            video_id: t.id.clone(),
            title: t.title.clone(),
            artist: t.artist_line(),
            // Not an uploaded song's artist or album: only its uploader can open those.
            artist_id: t.first_artist_id().filter(|id| !crate::ytm::uploaded(id)).map(str::to_owned),
            album: t.album.as_ref().map(|a| a.name.clone()),
            album_id: t.album.as_ref().and_then(|a| a.id.clone()).filter(|id| !crate::ytm::uploaded(id)),
            art: backend::art_url(t),
            artist_pic: t.first_artist_id().and_then(|id| self.artist_pics.get(id).cloned().flatten()),
            app_icon: self.app_icon,
            start_ms,
            end_ms: (dur > 0).then_some(start_ms + dur),
            paused: !self.playing() || self.buffering,
            jam: self.jam.code().map(crate::jam::share_link),
        })
    }

    /// Mirror playback into Discord. Cheap enough to run every frame: it only sends when
    /// the card would look different, as on a new song, play/pause, a seek or new settings.
    pub(super) fn sync_discord(&mut self, ctx: &egui::Context) {
        if self.discord.is_none() {
            return;
        }
        self.want_artist_pic();
        let Some(d) = &self.discord else { return };
        // A name is sent once it's typed, not a letter at a time: Discord limits how often
        // the card may change.
        let typing = ctx.memory(|m| m.has_focus(super::presence::name_id()));
        let prefs = &self.discord_prefs;
        let typed;
        let prefs = if typing {
            let name = self.discord_sent.as_ref().and_then(|c| c.name.clone()).unwrap_or_default();
            typed = crate::discord::Prefs { app_name: name, ..prefs.clone() };
            &typed
        } else {
            prefs
        };
        let want = self.discord_presence().filter(|p| !p.paused || prefs.when_paused).map(|p| crate::discord::Card::new(&p, prefs));
        let same = match (&want, &self.discord_sent) {
            (Some(a), Some(b)) => a.same(b),
            (a, b) => a.is_none() && b.is_none(),
        };
        if !same {
            d.set(want.clone());
            self.discord_sent = want;
        }
    }

    /// Look up the playing artist's picture once, when the card's badge shows it.
    fn want_artist_pic(&mut self) {
        let p = &self.discord_prefs;
        if p.badge != crate::discord::BadgeShows::Artist || !p.art || p.hide_details {
            return;
        }
        let Some(a) = self.playing_track().and_then(|t| t.artists.iter().find(|a| a.id.is_some())) else { return };
        let id = a.id.clone().unwrap_or_default();
        if !self.artist_pics.contains_key(&id) {
            let name = a.name.clone();
            self.artist_pics.insert(id.clone(), None);
            self.backend.request(backend::Req::ArtistPicture { id, name });
        }
    }

    /// The window's title, which the taskbar and Alt+Tab show: the current song, or the
    /// app's name with none. Named like a browser's picture in picture while the mini
    /// player is the video: Wayland lets no app put itself above others, but a KDE window
    /// rule for that title (and the app, ytm-rs) can.
    pub(super) fn sync_title(&mut self, ctx: &egui::Context) {
        let want = if self.mini.active() && super::mini::video_active() {
            crate::tr!("player-pip").to_owned()
        } else {
            window_title(self.current().map(|t| (t.title.as_str(), t.artist_line())))
        };
        if want != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(want.clone()));
            self.window_title = want;
        }
    }

    /// A notification when a new song starts while the window is hidden, minimized or
    /// behind other windows (the mini player on show counts as seen). Not for the song
    /// picked in the app, nor the same song again (Repeat one).
    pub(super) fn sync_song_notification(&mut self, ctx: &egui::Context) {
        if !self.song_notifications || !self.playing() || self.buffering || !crate::notify::available() {
            return;
        }
        if self.notified.as_ref().is_some_and(|(g, _)| *g == self.generation) {
            return;
        }
        let Some(t) = self.current().cloned() else { return };
        let again = self.notified.as_ref().is_some_and(|(_, id)| *id == t.id);
        self.notified = Some((self.generation, t.id.clone()));
        let focused = ctx.input(|i| i.viewport().focused) != Some(false);
        let seen = !self.window_hidden && (focused || self.mini.showing());
        if again || seen {
            return;
        }
        if self.notifier.is_none() {
            self.notifier = crate::notify::Notifier::spawn();
        }
        let Some(n) = self.notifier.clone() else { return };
        let song = crate::notify::Song {
            title: t.title.clone(),
            artist: t.artist_line(),
            cover: None,
        };
        let (art, id, generation) = (backend::art_url(&t), t.id, self.generation);
        n.expect(generation);
        let dir = self.backend.data_dir().join("notification-cover");
        let http = self.backend.http();
        self.backend.spawn(async move {
            let cover = match art {
                Some(url) => async {
                    let bytes = http.get(&url).timeout(Duration::from_secs(5)).send().await?.error_for_status()?.bytes().await?;
                    // A new name per song: Windows keeps showing an older picture by its path.
                    let _ = std::fs::remove_dir_all(&dir);
                    std::fs::create_dir_all(&dir)?;
                    let path = dir.join(format!("{}.jpg", file_safe(&id)));
                    std::fs::write(&path, &bytes)?;
                    anyhow::Ok(path)
                }
                .await
                .inspect_err(|e| log::debug!("notification cover: {e:#}"))
                .ok(),
                None => None,
            };
            n.show(generation, crate::notify::Song { cover, ..song });
        });
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
            shuffle: self.queue.shuffled(),
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
                    self.toast(crate::tr!("settings-lastfm-connected", name = name.as_str()));
                    // Only when nothing was listening already, so this play isn't sent twice.
                    if self.listenbrainz_user.is_none() {
                        self.scrobble = None;
                    }
                    self.lastfm_user = Some(name);
                }
                Err(e) => self.toast(crate::tr!("backend-error", operation = "Last.fm", error = e.to_string())),
            }
        }
        let result = self.listenbrainz_result.lock().unwrap().take();
        if let Some(r) = result {
            match r {
                Ok(name) => {
                    self.toast(crate::tr!("settings-listenbrainz-connected", name = name.as_str()));
                    if self.lastfm_user.is_none() {
                        self.scrobble = None;
                    }
                    self.listenbrainz_user = Some(name);
                    self.listenbrainz_token.clear();
                }
                Err(e) => self.toast(crate::tr!("backend-error", operation = "ListenBrainz", error = e.to_string())),
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
        let now = crate::unix_secs() as i64;
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
                self.backend.spawn(crate::lastfm::now_playing(&http, &play));
            }
            if listenbrainz {
                self.backend.spawn(crate::listenbrainz::now_playing(&http, &listen));
            }
            return;
        }
        if dur >= Duration::from_secs(30) && pos >= (dur / 2).min(Duration::from_secs(240)) && self.scrobble.is_some_and(|(_, done)| !done) {
            self.scrobble = Some((self.generation, true));
            if lastfm {
                self.backend.spawn(crate::lastfm::scrobble(&http, play));
            }
            if listenbrainz {
                self.backend.spawn(crate::listenbrainz::listen(&http, listen));
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
        // After a check GitHub turned down for its rate limit (a manual one too), again when the limit ends.
        let next = match u.state() {
            State::RateLimited { until } => now + until.saturating_duration_since(std::time::Instant::now()).as_secs_f64(),
            _ => self.update_checked.map_or(8.0, |t| t + 6.0 * 3600.0),
        };
        let due = now >= next && !matches!(u.state(), State::Ready { .. });
        let enabled = self.auto_update && updatable() && dev_var("YTM_BACKGROUND").is_none_or(|_| dev_var("YTM_UPDATE_FORCE").is_some() || dev_var("YTM_UPDATE_FEED").is_some());
        if enabled && due {
            self.update_checked = Some(now);
            u.check();
        } else if enabled && !matches!(u.state(), State::Ready { .. }) {
            // An idle app doesn't repaint on its own: wake up when the next check is due.
            ctx.request_repaint_after(Duration::from_secs_f64((next - now).max(0.5) + 0.1));
        }
        match u.state() {
            State::Ready { version, .. } if !self.update_announced => {
                self.update_announced = true;
                self.notes_dialog = Some(notes::NotesDialog { version, ready: true });
            }
            // A packaged copy (.deb, Arch, Flatpak): once per version, not every launch (the sidebar keeps saying so).
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
            self.tray_error = None;
            match crate::tray::Tray::new(crate::appicon::get(self.app_icon), move || c.request_repaint()) {
                Ok(t) => self.tray = Some(t),
                // Without an icon there'd be no way back: × just closes.
                Err(e) => {
                    log::warn!("tray icon unavailable: {e:#}");
                    if let Some(u) = e.downcast_ref::<crate::tray::Unsupported>() {
                        // It can't work here: off, with the reason under the setting.
                        self.close_to_tray = false;
                        self.tray_error = Some(u.0.clone());
                    } else {
                        self.tray_failed = true;
                    }
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
        let show = |app: &mut Self| app.bring_back(ctx, wayland);
        if ctx.input(|i| i.viewport().close_requested()) && !self.exiting {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            hide(self);
        }
        // Minimized counts as hidden: "Show" is what brings it back.
        let shown = |app: &Self| !app.in_tray && ctx.input(|i| i.viewport().minimized) != Some(true);
        for cmd in crate::tray::take() {
            match cmd {
                crate::tray::Cmd::Click if !self.tray_click_plays => show(self),
                // Playing or pausing waits to see it isn't a double-click, which shows the window.
                crate::tray::Cmd::Click if self.tray_click.take().is_some() => show(self),
                crate::tray::Cmd::Click => {
                    self.tray_click = Some(std::time::Instant::now());
                    ctx.request_repaint_after(crate::tray::double_click_time());
                }
                crate::tray::Cmd::Toggle if shown(self) => hide(self),
                crate::tray::Cmd::Toggle => show(self),
                // As the media keys do.
                crate::tray::Cmd::PlayPause => self.toggle(),
                crate::tray::Cmd::Next => self.next(),
                crate::tray::Cmd::Prev => self.prev(),
                crate::tray::Cmd::Recognize => super::recognize::toggle(self),
                crate::tray::Cmd::Quit => self.quit(ctx),
            }
        }
        if self.tray_click.is_some_and(|at| at.elapsed() >= crate::tray::double_click_time()) {
            self.tray_click = None;
            if self.queue.index.is_some() {
                self.toggle();
            } else {
                show(self);
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

    /// The window back for the tray's Show or a later launch. Wayland ignores Visible,
    /// un-minimizing and Focus, so there it can only ask for attention (see `tray_window`).
    pub(super) fn bring_back(&mut self, ctx: &egui::Context, wayland: bool) {
        if wayland {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
        } else {
            self.show_window(ctx);
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
                let path = dir.join(format!("{}.jpg", file_safe(&video_id)));
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

/// "Title – Artist" for a song, the app's name without one.
fn window_title(song: Option<(&str, String)>) -> String {
    match song {
        Some((title, artist)) if !artist.is_empty() => format!("{title} – {artist}"),
        Some((title, _)) if !title.is_empty() => title.to_owned(),
        _ => "YT Music".to_owned(),
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

/// A song's ID as a file name: only letters, digits, `-` and `_`, so one that isn't a
/// YouTube ID can't point outside the folder.
fn file_safe(id: &str) -> String {
    id.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_title_is_the_song() {
        assert_eq!(window_title(Some(("Get Lucky", "Daft Punk".to_owned()))), "Get Lucky – Daft Punk");
        assert_eq!(window_title(Some(("Get Lucky", String::new()))), "Get Lucky");
        assert_eq!(window_title(Some(("", String::new()))), "YT Music");
        assert_eq!(window_title(None), "YT Music");
    }

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
