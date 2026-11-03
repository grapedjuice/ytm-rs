//! Playing the queue: starting songs, Next and Back, pausing and seeking, picking up
//! after a dropped download, SponsorBlock, the art and lyrics that follow a song, and
//! the session kept across launches and devices.

use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::audio::AudioCmd;
use crate::backend::{self, Req};
use crate::lyrics::Lyrics;
use crate::model::Track;

use super::queue::QueueFill;
use super::{App, PlayingAs, Repeat, Session, jam, lyric_prefs};

impl App {
    pub fn current(&self) -> Option<&Track> {
        self.queue.current()
    }

    /// The lyrics shown for the current song: the song's pick, then the listener's order.
    fn lyric_choice(&self) -> Option<&Lyrics> {
        let (id, set) = self.lyrics.as_ref()?;
        set.get(self.lyric_prefs.choose(id, set)?)
    }

    /// Lyrics have loaded, and no enabled source has any.
    pub(super) fn no_lyrics(&self) -> bool {
        self.lyrics.is_some() && self.lyric_choice().is_none()
    }

    /// Ask for romanization/translation of the shown source once, when they're switched on
    /// and some line needs one the lyrics don't ship.
    pub(super) fn request_lyric_extras(&mut self) {
        if !(self.show_roman || self.show_trans) {
            return;
        }
        let Some(l) = self.lyric_choice() else { return };
        let Some((id, _)) = &self.lyrics else { return };
        let target = self.lyric_prefs.target.clone();
        let want = (id.clone(), l.provider.key().to_owned(), target.clone());
        if self.extras_asked.as_ref() == Some(&want) {
            return;
        }
        let needs = l.lines.iter().any(|x| {
            !x.instrumental
                && !x.text.trim().is_empty()
                && ((self.show_roman && x.romanization.is_none() && crate::translate::non_latin(&x.text))
                    || (self.show_trans && lyric_prefs::shipped_translation(x, &target).is_none()))
        });
        let req = needs.then(|| Req::LyricsExtras {
            video_id: id.clone(),
            source: l.provider.key().to_owned(),
            lines: l.lines.iter().map(|x| if x.instrumental { String::new() } else { x.text.clone() }).collect(),
            lang: l.language.clone(),
            target,
        });
        self.extras_asked = Some(want);
        if let Some(r) = req {
            self.backend.request(r);
        }
    }

    pub(super) fn current_id(&self) -> Option<&str> {
        self.current().map(|t| t.id.as_str())
    }

    /// The song version standing in for the current music video. Only for the play it
    /// came with: a new play, or a queue that changed under it, leaves it behind.
    pub(super) fn playing_as(&self) -> Option<&Track> {
        self.playing_as.as_ref().filter(|p| p.generation == self.generation && self.current_id() == Some(p.listed.as_str())).map(|p| &p.song)
    }

    /// What actually plays: the current track, or the song version standing in for it.
    /// Lyrics, SponsorBlock, Last.fm, Discord, media controls and Now playing's actions go
    /// by this (timing, length and title are the upload's own); lists and the queue keep
    /// the track as listed.
    pub(super) fn playing_track(&self) -> Option<&Track> {
        self.playing_as().or(self.current())
    }

    pub(super) fn playing_id(&self) -> Option<&str> {
        self.playing_track().map(|t| t.id.as_str())
    }

    /// Play the current music video as itself again, from where its song is now: a jam
    /// plays everyone the host's exact upload.
    pub(super) fn play_listed_upload(&mut self) {
        if self.playing_as().is_none() {
            return;
        }
        let at = self.audio.status.position();
        self.playing_as = None;
        if self.playing() || self.buffering {
            self.play_current_at(at, false);
        } else {
            self.resume_at = Some(at);
        }
    }

    /// `t`, when it's a music video to play as its song version. Not in a jam: everyone
    /// there plays the host's exact upload at the host's position.
    pub(super) fn song_of(&self, t: &Track) -> Option<Track> {
        (self.play_songs && t.music_video && !self.jam.active()).then(|| t.clone())
    }

    pub(super) fn playing(&self) -> bool {
        self.audio.status.playing.load(Ordering::Relaxed)
    }

    /// Fetch the current song's art for the background and accent. The default theme
    /// shows neither, so it skips the download and blur until the theme is turned off.
    pub(super) fn request_art(&mut self) {
        if self.default_theme {
            return;
        }
        let Some(track) = self.current() else { return };
        if self.art_key.as_deref() == Some(track.id.as_str()) {
            return;
        }
        if let Some(url) = backend::art_url(track) {
            let key = track.id.clone();
            self.backend.request(Req::Art { key, url });
        }
    }

    pub(super) fn play_list(&mut self, tracks: Vec<Track>, start: usize) {
        self.play_list_filling(tracks, start, None);
    }

    /// `fill`: the queue is a playlist still coming in, and its other songs follow.
    pub(super) fn play_list_filling(&mut self, tracks: Vec<Track>, start: usize, fill: Option<QueueFill>) {
        if !self.queue.replace(tracks, start, fill) {
            return;
        }
        // Whatever was about to play doesn't.
        self.play_pending = None;
        self.play_current();
    }

    /// Put last session's queue back, paused at the saved position, without loading audio.
    pub(super) fn restore_session(&mut self, json: &str) {
        let Ok(s) = serde_json::from_str::<Session>(json) else { return };
        let Some(track) = s.queue.get(s.index).cloned() else { return };
        self.queue.adopt(s.queue, s.index);
        let shown = s.playing_as.clone().unwrap_or_else(|| track.clone());
        self.playing_as = s.playing_as.map(|song| PlayingAs { generation: self.generation, listed: track.id.clone(), song });
        let dur = shown.duration.map_or(s.duration_ms, |d| d as u64 * 1000);
        self.audio.status.duration_ms.store(dur, Ordering::Relaxed);
        self.audio.status.position_ms.store(s.position_ms, Ordering::Relaxed);
        self.resume_at = Some(Duration::from_millis(s.position_ms));
        self.listened_at = s.listened_at;
        self.request_art();
        self.backend.request(Req::Lyrics(shown));
    }

    /// Ask for the queue YouTube Music keeps across devices when the app starts and
    /// whenever its window comes back into focus with nothing playing, so a song
    /// started on the phone is here to carry on with.
    pub(super) fn sync_server_queue(&mut self, ctx: &egui::Context, now: f64) {
        if self.playing() {
            self.listened_at = unix_now();
        }
        let focused = ctx.input(|i| i.focused);
        let refocused = focused && !std::mem::replace(&mut self.was_focused, focused);
        let idle = !self.playing() && !self.buffering && !self.jam.active() && self.smoke.is_none();
        let due = self.queue_checked.is_none_or(|t| refocused && now - t > 60.0);
        if self.logged_in && idle && due && std::env::var_os("YTM_SMOKE").is_none() {
            self.queue_checked = Some(now);
            self.backend.request(Req::ServerQueue);
        }
    }

    /// Take up what another device was playing, paused at its start like a restored
    /// session, when it played there after anything played here.
    pub(super) fn adopt_server_queue(&mut self, q: crate::ytm::ServerQueue) {
        let Some(track) = q.tracks.get(q.index).cloned() else { return };
        let idle = !self.playing() && !self.buffering && !self.jam.active();
        // A minute's slack: this app's own plays reach that queue too.
        if !idle || q.watched_at <= self.listened_at + 60 || self.current_id() == Some(track.id.as_str()) {
            return;
        }
        log::info!("picking up {} from another device ({} in its queue)", track.id, q.tracks.len());
        self.queue.adopt(q.tracks, q.index);
        // Not a new play: an old song version mustn't come back with its video.
        self.playing_as = None;
        self.queue_source = None;
        self.listened_at = q.watched_at;
        self.audio.send(AudioCmd::Stop);
        self.audio.status.duration_ms.store(track.duration.map_or(0, |d| d as u64 * 1000), Ordering::Relaxed);
        self.audio.status.position_ms.store(0, Ordering::Relaxed);
        self.resume_at = Some(Duration::ZERO);
        self.resume_counts = true;
        self.request_art();
        self.backend.request(Req::Lyrics(track.clone()));
        self.toast(format!("Picked up \"{}\" from your other device. Press play to carry on.", track.title));
    }

    pub(super) fn session_json(&self) -> Option<String> {
        let (queue, index) = self.queue.saved()?;
        serde_json::to_string(&Session {
            queue: queue.to_vec(),
            index,
            position_ms: self.audio.status.position().as_millis() as u64,
            duration_ms: self.audio.status.duration_ms.load(Ordering::Relaxed),
            listened_at: if self.playing() { unix_now() } else { self.listened_at },
            playing_as: self.playing_as().cloned(),
        })
        .ok()
    }

    /// The song's download failed partway: pick it up where it stopped from a fresh
    /// stream, rather than letting it end early and skip to the next one. If that keeps
    /// failing, stay on the song, ready to resume from there.
    pub(super) fn recover(&mut self, at: Duration, reason: &str) {
        const MAX_RECOVERIES: u32 = 3;
        if self.recoveries < MAX_RECOVERIES {
            self.recoveries += 1;
            log::warn!("playback interrupted at {at:?} ({reason}); reopening (try {})", self.recoveries);
            self.play_current_at(at, false);
            return;
        }
        log::warn!("playback interrupted at {at:?} ({reason}); giving up");
        // The player has already run dry; Stop would also reset the position to 0:00.
        self.audio.status.position_ms.store(at.as_millis() as u64, Ordering::Relaxed);
        self.resume_at = Some(at);
        self.buffering = false;
        let title = self.current().map(|t| t.title.clone()).unwrap_or_default();
        self.toast(format!("Lost the connection while playing \"{title}\". Press play to pick up where it stopped."));
    }

    pub(super) fn play_current(&mut self) {
        self.recoveries = 0;
        self.play_current_at(Duration::ZERO, true);
    }

    /// Start the current track, opened directly at `at` so nothing plays from 0:00 first.
    /// `report`: a new play for the account's history (not picking up last session's song).
    pub(super) fn play_current_at(&mut self, at: Duration, report: bool) {
        let Some(track) = self.current().cloned() else { return };
        // Picking up mid-song (a resume, a download that broke off): the position is the
        // song version's when one was playing, so that goes on; a music video stays one.
        let carried = if at.is_zero() || self.jam.active() { None } else { self.playing_as().cloned() };
        self.queue.started();
        self.resume_at = None;
        self.resume_counts = false;
        self.generation += 1;
        self.audio.status.wanted_gen.store(self.generation, Ordering::Relaxed);
        let shown = carried.clone().unwrap_or_else(|| track.clone());
        self.audio.status.duration_ms.store(shown.duration.map_or(0, |d| d as u64 * 1000), Ordering::Relaxed);
        // A song running into the next crossfades (so does Next, when that's switched
        // on); going back or picking a song cuts over.
        self.audio.send(if self.handoff && at.is_zero() { AudioCmd::Handoff } else { AudioCmd::Stop });
        self.buffering = true;
        self.seek_drag = None;
        // A carried song version goes with its music video, which plays if the song won't.
        let song_of = if carried.is_none() && at.is_zero() { self.song_of(&track) } else { None };
        self.playing_as = carried.clone().map(|song| PlayingAs { generation: self.generation, listed: track.id.clone(), song });
        self.backend.request(Req::Play { video_id: track.id.clone(), generation: self.generation, at, report, song_of, song: carried });
        self.show_in_media(&shown);
        if let Some(m) = &mut self.media {
            m.set_playing(true);
        }
        // Background art follows the track; lyrics and segments, what plays.
        self.request_art();
        self.refresh_lyrics(shown);
        self.top_up();
    }

    /// The playing track's title, artist and length for the system's media controls,
    /// with the listed track's art (what the app shows too).
    pub(super) fn show_in_media(&mut self, shown: &Track) {
        let Some(track) = self.current().cloned() else { return };
        let Some(m) = &mut self.media else { return };
        // Windows fetches a cover URL on its own, through its own proxy setting only.
        let by_url = matches!(crate::proxy::active(), crate::proxy::Setting::System);
        let art = backend::art_url(&track);
        m.set_track(&shown.title, &shown.artist_line(), shown.album.as_ref().map(|a| a.name.as_str()), art.as_deref().filter(|_| by_url), shown.duration);
        if !by_url && let Some(url) = art {
            self.fetch_media_cover(track.id.clone(), url);
        }
    }

    /// Lyrics and SponsorBlock segments for `playing`, which timing is its own.
    pub(super) fn refresh_lyrics(&mut self, playing: Track) {
        self.lyrics = None;
        self.lyrics_extras = None;
        self.extras_asked = None;
        self.lyric_view.reset();
        self.backend.request(Req::Lyrics(playing));
        self.segments = None;
        self.request_segments();
    }

    /// Keep the queue topped up so autoplay never stalls at the end; a playlist still
    /// coming in goes first.
    pub(super) fn top_up(&mut self) {
        if self.autoplay && !self.radio_pending && !self.jam.guest() && self.queue.fill.is_none() && self.queue.at_end()
            && let Some(t) = self.current().cloned()
        {
            self.radio_pending = true;
            self.backend.request(Req::Radio { song_of: self.song_of(&t), seed: t.id });
        }
    }

    pub(crate) fn set_crossfade(&self) {
        self.audio.set_crossfade(if self.crossfade { self.crossfade_secs } else { 0 });
    }

    /// Ask SponsorBlock about the playing song, when that's switched on.
    pub(super) fn request_segments(&mut self) {
        if !self.sponsorblock || self.segments.is_some() {
            return;
        }
        // What plays: a song version's segments are its own.
        let Some(video_id) = self.playing_id().map(str::to_owned) else { return };
        self.backend.request(Req::Segments { video_id });
    }

    /// Jump over a SponsorBlock segment once playback is inside it; one that runs to the
    /// end finishes the song. A guest in a jam follows the host, who does the skipping.
    pub(super) fn skip_segments(&mut self, ctx: &egui::Context, now: f64) {
        if !self.sponsorblock || self.jam.guest() || self.resume_at.is_some() || self.buffering || !self.playing() {
            return;
        }
        let Some((id, segs)) = &self.segments else { return };
        // A seek takes a moment to show in the position: don't send it twice.
        if self.playing_id() != Some(id.as_str()) || now - self.segment_skipped < 1.5 {
            return;
        }
        // The stream's own length: the song row's can be another cut's (the song's,
        // where the video plays), and a segment marked on another cut skips wrong.
        let Some(duration) = self.audio.status.duration().map(|d| d.as_secs_f64()) else { return };
        let pos = self.audio.status.position().as_secs_f64();
        let mut segs = segs.iter().filter(|s| s.fits(duration));
        let Some(seg) = segs.clone().find(|s| pos >= s.start && pos < s.end - 0.3).copied() else {
            // Wake up for the next one even when nothing else repaints.
            if let Some(next) = segs.find(|s| s.start > pos) {
                ctx.request_repaint_after(Duration::from_secs_f64(next.start - pos + 0.05));
            }
            return;
        };
        self.segment_skipped = now;
        log::info!("sponsorblock: skipping {:.1}–{:.1} s", seg.start, seg.end);
        if seg.end >= duration - 1.0 {
            if self.queue.repeat == Repeat::One {
                self.play_current();
            } else {
                self.next();
            }
        } else {
            self.audio.send(AudioCmd::Seek(Duration::from_secs_f64(seg.end)));
        }
    }

    /// Fetch whatever plays after the current track so skipping is instant.
    pub(super) fn prefetch_next(&self) {
        if let Some(t) = self.queue.upcoming() {
            self.backend.request(Req::Prefetch { video_id: t.id.clone(), song_of: self.song_of(t) });
        }
    }

    pub(super) fn next(&mut self) {
        if self.jam.guest() {
            return jam::request(self, crate::jam::Cmd::Skip);
        }
        if !self.queue.advance() {
            return;
        }
        let ending = self.handoff;
        self.handoff = ending || self.crossfade_skips;
        self.play_current();
        self.handoff = ending;
    }

    pub(super) fn prev(&mut self) {
        if self.jam.guest() {
            return jam::request(self, crate::jam::Cmd::Prev);
        }
        if self.audio.status.position() > Duration::from_secs(3) {
            self.seek_to(Duration::ZERO);
            return;
        }
        if self.queue.back() {
            self.play_current();
        }
    }

    pub(super) fn seek_to(&mut self, at: Duration) {
        if self.resume_at.is_some() {
            // Restored but not loaded yet (nothing for the player to seek): move the start point instead.
            self.resume_at = Some(at);
            self.audio.status.position_ms.store(at.as_millis() as u64, Ordering::Relaxed);
        } else {
            self.audio.send(AudioCmd::Seek(at));
        }
    }

    pub(super) fn toggle(&mut self) {
        if self.queue.index.is_none() {
            return;
        }
        if self.jam.guest() {
            return jam::request(self, crate::jam::Cmd::Toggle);
        }
        if let Some(at) = self.resume_at {
            // Last session's song, already counted when it started then (unless it was
            // another device's).
            let report = std::mem::take(&mut self.resume_counts);
            self.play_current_at(at, report);
            return;
        }
        let playing = self.playing();
        // The media controls catch up in `logic` once the player has switched.
        self.audio.send(if playing { AudioCmd::Pause } else { AudioCmd::Resume });
    }

    pub(super) fn shuffle_upcoming(&mut self) {
        if self.jam.guest() {
            return self.toast("Only the host can shuffle the jam");
        }
        if self.queue.shuffle_upcoming() {
            self.toast("Shuffled up next");
            self.prefetch_next();
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}
