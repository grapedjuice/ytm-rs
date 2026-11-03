//! What the backend answers, applied to the app.

use std::collections::HashSet;
use std::time::Duration;

use souvlaki::MediaControlEvent;

use crate::audio::{AudioCmd, AudioEvent};
use crate::backend::{Req, Resp};
use crate::model::{ShelfKind, Track};
use crate::ytm::HomePage;

use super::{App, Dialog, HomeState, LibTab, Page, PlayingAs, Repeat, dev_var, shuffle};

impl App {
    pub(super) fn pump(&mut self, now: f64) {
        let cover = self.media_cover.lock().unwrap().take();
        // The listed track's art, with what plays's title, artists and length.
        if let Some((id, file)) = cover
            && self.current().is_some_and(|t| t.id == id)
            && let Some(track) = self.playing_track().cloned()
            && let Some(m) = &mut self.media
        {
            m.set_track(&track.title, &track.artist_line(), track.album.as_ref().map(|a| a.name.as_str()), Some(&file), track.duration);
        }
        while let Ok(r) = self.ch.resp_rx.try_recv() {
            self.on_resp(r, now);
        }
        while let Ok(ev) = self.ch.audio_rx.try_recv() {
            match ev {
                AudioEvent::Finished { generation } if generation == self.generation && self.jam.guest() => {}
                AudioEvent::Finished { generation } if generation == self.generation => {
                    if self.queue.repeat == Repeat::One {
                        self.play_current();
                    } else {
                        self.next();
                    }
                }
                AudioEvent::Finished { .. } => {}
                // Crossfade into the next song; repeat-one and the end of the queue wait for the end.
                AudioEvent::Ending { generation } if generation == self.generation && !self.jam.guest() && self.queue.repeat != Repeat::One => {
                    self.handoff = true;
                    self.next();
                    self.handoff = false;
                }
                AudioEvent::Ending { .. } => {}
                AudioEvent::Interrupted { generation, at, reason } if generation == self.generation => self.recover(at, &reason),
                AudioEvent::Interrupted { .. } => {}
                AudioEvent::Error(e) => self.toast(e),
            }
        }
        while let Ok(ev) = self.ch.media_rx.try_recv() {
            match ev {
                MediaControlEvent::Play | MediaControlEvent::Pause if self.jam.guest() => self.toggle(),
                MediaControlEvent::Play if self.resume_at.is_some() => self.toggle(),
                MediaControlEvent::Play => self.audio.send(AudioCmd::Resume),
                MediaControlEvent::Pause | MediaControlEvent::Stop => self.audio.send(AudioCmd::Pause),
                MediaControlEvent::Toggle => self.toggle(),
                MediaControlEvent::Next => self.next(),
                MediaControlEvent::Previous => self.prev(),
                _ => {}
            }
        }
        if self.buffering && self.playing() {
            self.buffering = false;
            self.skip_streak = 0;
            self.prefetch_next();
            // Dev hook: YTM_SEEK=<secs> jumps into the first track once it starts.
            if self.generation == 1 {
                if let Some(s) = std::env::var("YTM_SEEK").ok().and_then(|v| v.parse::<f32>().ok()) {
                    self.audio.send(AudioCmd::Seek(Duration::from_secs_f32(s)));
                }
                // YTM_SEEK_LATER=<secs>@<delay> seeks again that many seconds later.
                if let Some((s, d)) = dev_var("YTM_SEEK_LATER").and_then(|v| v.split_once('@')).and_then(|(s, d)| Some((s.parse::<f32>().ok()?, d.parse::<f32>().ok()?))) {
                    let audio = self.audio.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_secs_f32(d));
                        audio.send(AudioCmd::Seek(Duration::from_secs_f32(s)));
                    });
                }
            }
        }
        if self.suggest_due.is_some_and(|d| now >= d) {
            self.suggest_due = None;
            let q = self.search_text.trim().to_owned();
            // An empty box signed in shows the account's recent searches.
            if !q.is_empty() || self.logged_in {
                self.backend.request(Req::Suggest { query: q, round: self.suggest_round });
            }
        }
    }

    fn on_resp(&mut self, r: Resp, now: f64) {
        match r {
            Resp::Home(page) => self.on_home(page),
            Resp::HomeMore(page) => {
                if let Some(h) = &mut self.home {
                    h.shelves.extend(page.shelves);
                    h.continuation = page.continuation;
                    h.loading_more = None;
                }
            }
            Resp::Explore(e) => {
                self.explore = Some(e);
                self.loading = false;
            }
            Resp::Search(r) => {
                if self.smoke.as_ref() == Some(&r.query) {
                    self.smoke = None;
                    if !r.tracks.is_empty() {
                        log::info!("smoke: playing {}", r.tracks[0].title);
                        self.play_list(r.tracks.clone(), 0);
                    }
                }
                self.search = Some(r);
                self.loading = false;
            }
            Resp::SearchForgotten => {
                // Replies dropped meanwhile left the list behind what's typed: ask again.
                self.forgets_pending = self.forgets_pending.saturating_sub(1);
                if self.forgets_pending == 0 {
                    self.suggest_due = Some(now);
                }
            }
            Resp::Suggest { query, round, terms } => {
                if round == self.suggest_round && self.search_text.trim() == query {
                    self.suggestions = (query, terms);
                    self.suggest_sel = None;
                }
            }
            Resp::Collection { load, c, more } => self.collection_in(load, c, more),
            Resp::CollectionMore { id, load, tracks, set_ids, more } => self.collection_more(id, load, tracks, set_ids, more),
            Resp::Artist(a) => {
                // A slow answer for an artist left meanwhile doesn't replace the one on screen.
                if matches!(&self.page, Page::Artist(id) if *id != a.id) {
                    return;
                }
                if self.page == Page::Artist(a.id.clone()) {
                    self.loading = false;
                }
                self.artist = Some(a);
            }
            Resp::Library(mut l) => {
                // A half that didn't load keeps what was shown (or last launch's sidebar),
                // rather than emptying it.
                if l.playlists_failed {
                    l.playlists = self.library.as_ref().map(|o| o.playlists.clone()).or_else(|| self.sidebar_cache.clone()).unwrap_or_default();
                }
                if l.albums_failed {
                    l.albums = self.library.as_ref().map(|o| o.albums.clone()).unwrap_or_default();
                }
                self.library = Some(l);
                self.loading = false;
            }
            Resp::History(h) => {
                self.history = Some(h);
                self.loading = false;
            }
            Resp::Uploads(t) => {
                self.uploads = Some(t);
                if self.page == Page::Library(LibTab::Uploads) {
                    self.loading = false;
                }
            }
            Resp::Downloaded { title, path } => {
                log::info!("downloaded to {}", path.display());
                self.toast(format!("Downloaded \u{201c}{title}\u{201d}"));
            }
            Resp::Uploaded { done, failed } => {
                let songs = |n: usize| if n == 1 { "1 song".to_owned() } else { format!("{n} songs") };
                self.toast(match (done, failed.len()) {
                    (_, 0) => format!("Uploaded {}. It can take a minute for YouTube Music to process them.", songs(done)),
                    (0, _) => format!("Upload failed: {}", failed.join(", ")),
                    _ => format!("Uploaded {}; failed: {}", songs(done), failed.join(", ")),
                });
            }
            Resp::Lyrics { video_id, lyrics } => {
                if self.playing_id() == Some(video_id.as_str()) {
                    let available = self.lyric_prefs.available(&lyrics);
                    let shown = self.lyric_prefs.choose(&video_id, &lyrics);
                    let missing: Vec<&str> = self.lyric_prefs.order.iter().filter(|(p, on)| *on && !available.contains(p)).map(|(p, _)| p.key()).collect();
                    log::info!(
                        "lyrics: showing {} ({}/{}); available: {}; no lyrics from: {}",
                        shown.map_or("nothing", |p| p.key()),
                        shown.and_then(|p| available.iter().position(|x| *x == p)).map_or(0, |i| i + 1),
                        available.len(),
                        available.iter().map(|p| p.key()).collect::<Vec<_>>().join(", "),
                        missing.join(", ")
                    );
                    self.lyrics = Some((video_id, lyrics));
                }
            }
            Resp::Segments { video_id, segments } => {
                if self.playing_id() == Some(video_id.as_str()) {
                    log::info!("sponsorblock: {} segment(s) for {video_id}", segments.len());
                    self.segments = Some((video_id, segments));
                }
            }
            Resp::LyricsExtras { video_id, source, target, extras } => {
                if self.playing_id() == Some(video_id.as_str()) {
                    self.lyrics_extras = Some((video_id, source, target, extras));
                }
            }
            Resp::Art { key, art } => {
                if self.current_id() == Some(key.as_str()) {
                    self.accent = art.accent;
                    if let Some(bg) = &self.bg {
                        bg.set_art(art);
                    }
                    self.art_key = Some(key);
                    self.art_changed = now;
                }
            }
            Resp::Radio { seed, tracks } => {
                self.radio_pending = false;
                let was_at_end = self.queue.add_radio(&seed, tracks);
                if was_at_end && self.queue.index.is_some() && !self.buffering && !self.playing() {
                    // The track ended while radio was loading: continue now.
                    self.next();
                } else if !self.buffering {
                    self.prefetch_next();
                }
            }
            // Unless something else was asked to play meanwhile; shuffled as last asked.
            Resp::PlayQueue { target, mut tracks } => {
                if let Some((p, shuffled)) = &self.play_pending
                    && *p == target
                {
                    if *shuffled {
                        shuffle(&mut tracks);
                    }
                    self.queue_source = Some(target);
                    self.play_list(tracks, 0);
                }
            }
            Resp::Liked { video_id, like } => {
                self.lists.edits.confirm_likes(std::slice::from_ref(&video_id), &[]);
                if like {
                    self.liked.insert(video_id);
                }
            }
            Resp::QuickPicksMore { seed, tracks } => {
                if let Some(h) = &mut self.home {
                    h.picks_loading = false;
                    if let Some(shelf) = h.shelves.iter_mut().find(|s| s.title == "Quick picks")
                        && let ShelfKind::Songs { tracks: picks, .. } = &mut shelf.kind
                        && picks.iter().any(|t| t.id == seed)
                    {
                        h.picks_expanded = true;
                        let mut seen: HashSet<String> = picks.iter().map(|t| t.id.clone()).collect();
                        picks.extend(tracks.into_iter().filter(|t| seen.insert(t.id.clone())).take(24));
                    }
                }
            }
            Resp::LikeProgress { taken, done, total, like } => {
                if !like {
                    let tracks: Vec<Track> = taken.iter().filter_map(|id| self.unliking.remove(id)).collect();
                    self.set_liked(&tracks, false);
                }
                if done == 0 {
                    // A Spotify transfer is adding to Liked Music (maybe paused): this waits for it.
                    self.toast("Waiting for the transfer to finish adding to Liked Music…");
                } else if total > 1 && done < total {
                    let note = if self.backend.api().limited() { " (YouTube Music is limiting requests; waiting)" } else { "" };
                    self.toast(format!("{} {done} of {total} songs…{note}", if like { "Liking" } else { "Removing likes:" }));
                }
            }
            Resp::LikedMany { video_ids, failed, like } => self.on_liked_many(video_ids, failed, like),
            Resp::PlaylistCreated { id, songs } => {
                self.request_library();
                self.toast(match songs {
                    0 => "Playlist created".to_owned(),
                    1 => "Song added to new playlist".to_owned(),
                    n => format!("{n} songs added to new playlist"),
                });
                self.go(Page::Playlist(id));
            }
            Resp::PlaylistChanged { id, message } => self.on_playlist_changed(id, message),
            Resp::Collaboration(c) => {
                if let Some(Dialog::Collaborate { id, info, busy, .. }) = &mut self.dialog
                    && *id == c.playlist_id
                {
                    *info = Some(c);
                    *busy = false;
                }
            }
            Resp::PlayingAs { generation, listed, song } => {
                // For the track it was looked up for: joining a jam swaps the queue without a new play.
                if generation == self.generation
                    && self.current_id() == Some(listed.as_str())
                    && let Some(track) = self.current().cloned()
                {
                    let shown = match song {
                        // The song keeps the listed track's album and art when it has none.
                        Some(song) => {
                            let song = Track { album: song.album.or(track.album.clone()), thumbs: if song.thumbs.is_empty() { track.thumbs.clone() } else { song.thumbs }, ..song };
                            self.playing_as = Some(PlayingAs { generation, listed: track.id.clone(), song: song.clone() });
                            song
                        }
                        None => {
                            self.playing_as = None;
                            track
                        }
                    };
                    self.show_in_media(&shown);
                    self.refresh_lyrics(shown);
                    // A jam began while it was being looked up: everyone plays the host's upload.
                    if self.jam.active() {
                        self.play_listed_upload();
                    }
                }
            }
            Resp::PlayError { generation, msg } => self.on_play_error(generation, msg),
            Resp::ServerQueue(q) => self.adopt_server_queue(q),
            Resp::Account { name, photo } => {
                self.account_name = name.or(self.account_name.take());
                self.account_avatar = photo.or(self.account_avatar.take());
            }
            Resp::Channels(c) => self.channels = Some(c),
            Resp::SwitchedChannel { page_id, epoch, error } => {
                let name = self.switching_to.take().unwrap_or_else(|| "another channel".to_owned());
                match error {
                    Some(e) => self.toast(e),
                    None => {
                        // Everything reloads as after signing in, but it's the same sign-in
                        // with the same channels: only which one is used changed.
                        let channels = self.channels.take().map(|c| crate::backend::Channels { current: page_id, epoch, ..c });
                        let asked = self.channels_asked;
                        self.on_resp(Resp::LoggedIn(true), now);
                        (self.channels, self.channels_asked) = (channels, asked);
                        self.toast(format!("Switched to {name}"));
                    }
                }
            }
            Resp::LoggedIn(v) => self.on_logged_in(v, now),
            Resp::Error(e) => self.on_error(e),
        }
    }

    /// The home feed, replacing the one shown.
    fn on_home(&mut self, page: HomePage) {
        // Until the account menu answers (Req::Account).
        if self.account_avatar.is_none() {
            self.account_avatar = page.shelves.iter().find(|s| s.title == "Listen again").and_then(|s| s.strap_thumb.clone());
        }
        let mut chips = page.chips;
        // The feed marks the active mood itself; keep the bar when it doesn't send chips.
        if chips.is_empty() {
            chips = self.home.as_ref().map(|h| h.chips.clone()).unwrap_or_default();
        }
        self.home = Some(HomeState {
            chips,
            shelves: page.shelves,
            continuation: page.continuation,
            loading_more: None,
            picks_loading: false,
            picks_expanded: false,
        });
        self.home_loading = false;
        if self.page == Page::Home {
            self.loading = false;
        }
    }

    /// Liking or unliking many songs at once has finished: `failed` are the ones YouTube
    /// Music refused.
    fn on_liked_many(&mut self, video_ids: Vec<String>, failed: Vec<String>, like: bool) {
        // Any left (a retry took them) leave the list now.
        if !like {
            let rest: Vec<Track> = video_ids.iter().filter_map(|id| self.unliking.remove(id)).collect();
            self.set_liked(&rest, false);
            for id in &failed {
                self.unliking.remove(id);
            }
        }
        log::debug!("like={like} answered: {} taken, {} refused {:?}", video_ids.len(), failed.len(), failed.iter().take(5).collect::<Vec<_>>());
        self.lists.edits.confirm_likes(&video_ids, &failed);
        // The refused ones come back as they were with a reload.
        if !failed.is_empty() {
            self.request_liked(true);
        }
        let count = video_ids.len();
        let mut msg = format!("{} {count} {}", if like { "Liked" } else { "Removed like from" }, if count == 1 { "song" } else { "songs" });
        if count == 0 {
            msg = format!("YouTube Music refused all {} songs; try again in a while", failed.len());
        } else if !failed.is_empty() {
            msg += &format!("; YouTube Music refused {}", failed.len());
        }
        self.toast(msg);
    }

    /// A playlist was edited, added to, removed from, saved or deleted; `message` says which.
    fn on_playlist_changed(&mut self, id: String, message: &'static str) {
        self.request_library();
        if message == "Playlist deleted" {
            // Its load is dropped too, and the queue stops waiting for it.
            self.lists.forget(&id);
            if self.queue.fill.as_ref().is_some_and(|f| f.id == id) {
                self.queue.fill = None;
            }
            let gone = Page::Playlist(id);
            if self.page == gone {
                self.go(Page::Library(LibTab::Playlists));
            }
            // Back and Forward skip it.
            self.back.retain(|p| *p != gone);
            self.back.dedup();
            self.forward.retain(|p| *p != gone);
            self.forward.dedup();
        } else {
            if message == "Removed from playlist" {
                self.lists.edits.confirm_removal(&id);
            }
            // Kept for going back (or playing): reload it. What YouTube Music still
            // lists after a removal is dropped as it arrives.
            if self.lists.known(&id) {
                self.load_list(&id, false, true);
            }
        }
        self.toast(message);
    }

    /// A song couldn't play: skip it, or stop when skipping can't help.
    fn on_play_error(&mut self, generation: u64, msg: String) {
        if generation == self.generation {
            self.buffering = false;
            let title = self.current().map(|t| t.title.clone()).unwrap_or_default();
            let reason = if msg.contains("age-restricted") || msg.contains("confirm your age") {
                "it's age-restricted".to_owned()
            } else {
                msg
            };
            let has_next = !self.queue.at_end() && self.queue.index.is_some();
            // Skipping can't help when the account is the problem: the rest of
            // an album like this needs it too. Stop on the song instead.
            if let Some(why) = account_block(&reason, self.logged_in) {
                self.skip_streak = 0;
                self.toast(why.replace("{title}", &title));
            } else if has_next && self.skip_streak < MAX_SKIPS {
                // Don't stall the queue on one unplayable song.
                self.skip_streak += 1;
                self.toast(format!("Skipped \"{title}\": {reason}"));
                self.next();
            } else {
                self.skip_streak = 0;
                self.toast(format!("Can't play \"{title}\": {reason}"));
            }
        }
    }

    /// Signed in or out: what belonged to the last account goes, and the new one's home,
    /// library and likes load.
    fn on_logged_in(&mut self, v: bool, now: f64) {
        self.logged_in = v;
        self.channels = None;
        self.channels_asked = None;
        // Recent searches belong to the account.
        self.suggestions = Default::default();
        self.suggest_sel = None;
        self.forgetting = None;
        self.suggest_round += 1;
        self.forgets_pending = 0;
        self.suggest_due = Some(now);
        self.account_name = None;
        self.account_avatar = None;
        self.sidebar_cache = None;
        self.library = None;
        self.history = None;
        self.uploads = None;
        // Your own playlists show differently (and private ones not at all) to another
        // account, and its likes aren't this one's.
        self.lists.clear();
        self.lists.edits = Default::default();
        self.liked.clear();
        self.unliking.clear();
        self.queue.fill = None;
        self.play_pending = None;
        if self.page_list().is_some() {
            self.refresh();
        }
        self.home = None;
        self.home_loading = true;
        self.backend.request(Req::Home(None));
        if v {
            self.request_library();
            self.request_liked(false);
            self.backend.request(Req::Account);
        }
        self.toast(if v { "Signed in" } else { "Signed out" });
    }

    /// A request failed: pages waiting for it stop waiting, and the error shows.
    fn on_error(&mut self, e: String) {
        if let Some(Dialog::Collaborate { info, busy, .. }) = &mut self.dialog {
            *busy = false;
            // Nothing to show if it never loaded.
            if info.is_none() {
                self.dialog = None;
            }
        }
        // A page still waiting for its first data shows a retry instead of loading forever.
        self.load_failed |= self.loading || (self.page == Page::Home && self.home_loading);
        self.loading = false;
        self.home_loading = false;
        self.radio_pending = false;
        if let Some(h) = &mut self.home {
            h.loading_more = None;
            h.picks_loading = false;
        }
        self.toast(e);
    }
}

/// Unplayable songs skipped in a row before playback stops.
const MAX_SKIPS: u32 = 3;

/// When a song failed because of the account (Premium-only, or needs signing in),
/// the message to show instead of skipping: the rest of an album like that fails the
/// same way.
fn account_block(msg: &str, logged_in: bool) -> Option<&'static str> {
    let m = msg.to_ascii_lowercase();
    if m.contains("premium") {
        Some("\"{title}\" needs YouTube Music Premium")
    } else if !logged_in && (m.contains("sign in") || m.contains("age-restricted")) {
        Some("Sign in to play \"{title}\"")
    } else {
        None
    }
}

#[cfg(test)]
mod account_block_tests {
    use super::account_block;

    #[test]
    fn account_errors_stop_instead_of_skipping() {
        assert!(account_block("sign in to play this song", false).is_some());
        assert!(account_block("sign in to play this song", true).is_none());
        assert!(account_block("This video is only available to Music Premium members", true).is_some());
        assert!(account_block("decode: end of stream", false).is_none());
    }
}
