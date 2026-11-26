//! What the backend answers, applied to the app.

use std::collections::HashSet;
use std::time::Duration;

use souvlaki::MediaControlEvent;

use crate::audio::{AudioCmd, AudioEvent};
use crate::backend::{LinkTo, LinkUse, PlaylistChange, Req, Resp};
use crate::link::Link;
use crate::model::{ShelfKind, Target, Track};
use crate::ytm::HomePage;

use crate::remote::{Cmd, RepeatMode};

use super::{Action, App, Dialog, HomeState, LibTab, Page, PlayingAs, Repeat, Sleep, dev_var};

impl App {
    pub(super) fn pump(&mut self, ctx: &egui::Context, now: f64) {
        let cover = self.media_cover.lock().unwrap().take();
        // The listed track's art.
        if let Some((id, file)) = cover
            && self.current().is_some_and(|t| t.id == id)
            && let Some(m) = &mut self.media
        {
            m.set_cover(&file);
        }
        while let Ok(r) = self.ch.resp_rx.try_recv() {
            self.on_resp(ctx, r, now);
        }
        while let Ok(ev) = self.ch.audio_rx.try_recv() {
            match ev {
                AudioEvent::Finished { generation } if generation == self.generation && self.jam.guest() => {}
                AudioEvent::Finished { generation } if generation == self.generation && self.sleep == Some(Sleep::SongEnd) => {
                    self.left_at_end();
                    self.sleep_at_song_end();
                }
                // A-B repeat comes before the next song: a song that ran out (or began
                // fading out) inside its loop goes back to the loop's start.
                AudioEvent::Finished { generation } if generation == self.generation && self.ab_stretch().is_some() => {
                    if let Some((start, _)) = self.ab_stretch() {
                        self.loop_sought = Some(std::time::Instant::now());
                        self.play_current_at(Duration::from_secs_f64(start), false);
                    }
                }
                AudioEvent::Finished { generation } if generation == self.generation => {
                    self.left_at_end();
                    if self.repeats_one() {
                        self.play_current();
                    } else {
                        self.next();
                        // The last song ran out: radio's next songs carry on when they come.
                        self.ended = self.queue.at_end() && !self.buffering;
                        if self.ended {
                            self.top_up();
                        }
                    }
                }
                AudioEvent::Finished { .. } => {}
                // Crossfade into the next song, or into the song again on Repeat one; the
                // end of the queue waits for the end.
                // With the sleep timer set to the song's end, it plays out instead (`Finished`).
                // Looping a stretch, `keep_looping` goes back at its end: a song whose music
                // ends before its length asks for the next one before that end (and asks
                // again after the seek back). Missed, `Finished` goes back.
                AudioEvent::Ending { generation } if generation == self.generation && !self.jam.guest() && self.sleep != Some(Sleep::SongEnd) && self.ab_stretch().is_some() => {}
                AudioEvent::Ending { generation } if generation == self.generation && !self.jam.guest() && self.sleep != Some(Sleep::SongEnd) => {
                    self.handoff = true;
                    if self.repeats_one() {
                        self.play_current();
                    } else {
                        self.next();
                    }
                    self.handoff = false;
                }
                AudioEvent::Ending { .. } => {}
                AudioEvent::PrepareNext { generation } => self.stage_next(generation),
                AudioEvent::GaplessAdvanced { generation } => self.on_gapless_advanced(generation),
                AudioEvent::Interrupted { generation, at, failure } if generation == self.generation => self.recover(at, failure),
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
                // The window from the OS's media controls (KDE's, say; GNOME's use the .desktop file).
                MediaControlEvent::Raise => self.show_window(ctx),
                MediaControlEvent::Quit => self.quit(ctx),
                // As the Shuffle and Repeat buttons do (a jam guest can't shuffle); either
                // way the widget hears back what the app has now.
                MediaControlEvent::SetShuffle(on) => {
                    if on != self.queue.shuffled() {
                        self.toggle_shuffle();
                    }
                    if let Some(m) = &mut self.media {
                        m.resend_shuffle();
                    }
                }
                MediaControlEvent::SetLoopStatus(status) => {
                    self.queue.repeat = crate::media::repeat_from_os(status);
                    if let Some(m) = &mut self.media {
                        m.resend_repeat();
                    }
                }
                MediaControlEvent::SetVolume(v) => {
                    if let Some(v) = crate::media::volume_from_os(v) {
                        self.audio.set_volume(v);
                    }
                }
                // Seeks go the way the seek bar's do: to the host in a jam, to the start
                // point of a song not loaded yet, and none without a length to keep them in.
                ev => {
                    let len = self.audio.status.duration().or(self.current().and_then(|t| t.duration).map(|d| Duration::from_secs(d as u64)));
                    if let Some(len) = len
                        && let Some(at) = crate::media::seek_target(&ev, self.audio.status.position(), len)
                    {
                        self.apply(Action::Seek(at.as_secs_f32()));
                    }
                }
            }
        }
        for cmd in self.remote.drain() {
            self.on_remote(cmd);
        }
        // Playing here (not at a jam host's position): where it's left is noted.
        if self.left_from.is_none() && !self.jam.guest() && self.audio.status.started(self.generation) {
            self.left_from = self.current().map(|t| t.id.clone());
        }
        if self.buffering && self.audio.status.started(self.generation) {
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
        // Sorted playlists load again when it's time, for their new order. One whose load
        // ended without its first page (it failed) stops waiting, so Sort comes back.
        let mut due = Vec::new();
        let lists = &self.lists;
        self.sorting.retain(|id, w| {
            if w.take_due(now) {
                due.push(id.clone());
                return true;
            }
            !w.awaiting() || lists.loading(id) || lists.moving(id)
        });
        for id in due {
            self.load_list(&id, false, true);
        }
        if self.suggest_due.is_some_and(|d| now >= d) {
            self.suggest_due = None;
            let q = self.search_text.trim().to_owned();
            match crate::link::parse(&q) {
                // A link isn't searched for: its song shows instead.
                Some(link) => {
                    if matches!(link, Link::Song { .. }) && self.link_preview.as_ref().is_none_or(|(l, song)| *l != link || song.is_err()) {
                        self.backend.request(Req::Link { link, to: LinkUse::Preview });
                    }
                }
                // An empty box signed in shows the account's recent searches.
                None if !q.is_empty() || self.logged_in => self.backend.request(Req::Suggest { query: q, round: self.suggest_round }),
                None => {}
            }
        }
    }

    fn on_remote(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Toggle => self.toggle(),
            Cmd::Next => self.next(),
            Cmd::Prev => self.prev(),
            Cmd::VolumeUp => self.nudge_volume(true),
            Cmd::VolumeDown => self.nudge_volume(false),
            Cmd::SetVolume(v) => self.audio.set_volume(v.clamp(0.0, 1.0)),
            Cmd::Like(v) => {
                // Asked for outright: holding the heart is for clicks that might slip.
                if let Some(action) = self.heard_like_action(v.unwrap_or(!self.heard_liked())) {
                    self.apply(action);
                }
            }
            Cmd::Shuffle => self.toggle_shuffle(),
            Cmd::Repeat(mode) => {
                self.queue.repeat = match mode {
                    Some(RepeatMode::Off) => Repeat::Off,
                    Some(RepeatMode::All) => Repeat::All,
                    Some(RepeatMode::One) => Repeat::One,
                    None => self.queue.repeat.next(),
                };
            }
        }
    }

    fn on_resp(&mut self, ctx: &egui::Context, r: Resp, now: f64) {
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
            Resp::Mood(m) => {
                if self.page == Page::Mood(m.params.clone()) {
                    self.loading = false;
                }
                self.mood = Some(m);
            }
            Resp::VideoStream { video_id, max_height, stream } => {
                // Not one asked for before the quality changed.
                if let Some((id, player)) = &mut self.video
                    && *id == video_id
                    && max_height == self.video_quality
                {
                    let none = stream.is_none();
                    *player = stream.map(|s| crate::video::Player::start(s, self.backend.http(), self.audio.status.clone()));
                    if none {
                        // Not the picture from before a change of quality, frozen.
                        self.video_frame = None;
                        self.toast(crate::tr!("status-video-unavailable"));
                    }
                }
            }
            Resp::Canvas { video_id, clip } => {
                if let Some(c) = self.cover_clip.as_mut().filter(|c| c.id == video_id) {
                    c.player = clip.clone().map(|url| crate::video::Loop::start(url, self.backend.http(), c.px));
                    c.url = clip;
                }
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
            // Kept while its search is the page shown.
            Resp::SearchIn(r) if self.page == Page::Search(r.query.clone()) => {
                if self.scope_for(&r.query) == r.scope {
                    self.loading = false;
                }
                self.scoped.retain(|s| s.scope != r.scope);
                self.scoped.push(r);
            }
            Resp::SearchIn(_) => {}
            Resp::SearchForgotten => {
                // Replies dropped meanwhile left the list behind what's typed: ask again.
                self.forgets_pending = self.forgets_pending.saturating_sub(1);
                if self.forgets_pending == 0 {
                    self.suggest_due = Some(now);
                }
            }
            Resp::Suggest { query, round, terms, mut hits } => {
                if round == self.suggest_round && self.search_text.trim() == query {
                    // Explicit songs stay out while those are hidden. (Episodes come only
                    // with podcasts shown: see `ytm::suggestions`.)
                    hits.retain(|c| !matches!(&c.target, Target::Song(t) if self.hide_explicit && t.explicit));
                    self.suggestions = (query, terms, hits);
                    self.suggest_sel = None;
                }
            }
            Resp::Collection { load, c, more } => {
                // Picked rows go by number: they only stand while the rows stay where they are.
                let id = c.id.clone();
                let rows = |app: &Self| app.opened(&id).map(|o| o.c.tracks.iter().map(|t| t.id.clone()).collect::<Vec<_>>());
                let before = rows(&*self);
                self.collection_in(load, c, more);
                // A sort waiting for its order, from this load (an older one isn't kept).
                let sorted = self.lists.get(&id).filter(|o| o.load == load).zip(self.sorting.get_mut(&id)).is_some_and(|(o, w)| w.arrived(&o.c.sorts, now));
                if sorted {
                    self.sorting.remove(&id);
                }
                if self.page_list() == Some(id.as_str()) && rows(&*self) != before {
                    self.selection.clear();
                }
            }
            Resp::CollectionMore { id, load, tracks, set_ids, more, failed } => {
                let failed = failed.then(|| id.clone());
                self.collection_more(id, load, tracks, set_ids, more);
                // Afterwards: its ending counted it as all there.
                if let Some(id) = failed {
                    self.lists.load_failed(&id, load);
                }
            }
            Resp::Artist(mut a) => {
                // A slow answer for an artist left meanwhile doesn't replace the one on screen.
                if matches!(&self.page, Page::Artist(id) if *id != a.id) {
                    return;
                }
                if self.page == Page::Artist(a.id.clone()) {
                    self.loading = false;
                }
                // Asked for before a subscribe here was answered, it can be from before that.
                if let Some(channel) = &a.channel_id {
                    a.subscribed = self.subscriptions.shown(channel, a.subscribed, a.asked);
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
                if l.artists_failed {
                    l.artists = self.library.as_ref().map(|o| o.artists.clone()).unwrap_or_default();
                }
                // What it says of a playlist edited here lately can be from before that.
                let edited: Vec<String> = l.playlists.iter().filter_map(|c| match &c.target {
                    Target::Playlist(id) if self.lists.edits.pending(id) => Some(id.clone()),
                    _ => None,
                }).collect();
                // One deleted here, waiting on its Undo, stays out.
                l.playlists.retain(|c| !matches!(&c.target, Target::Playlist(p) if self.deleting_soon(p)));
                self.library = Some(l);
                for id in edited {
                    self.sync_card_count(&id);
                }
                self.loading = false;
            }
            Resp::History(h) => {
                self.history = Some(h);
                self.loading = false;
            }
            Resp::UploadDeleted(t) => {
                self.uploads = Some(t);
                self.toast(crate::tr!("status-upload-deleted"));
            }
            Resp::Uploads(t) => {
                self.uploads = Some(t);
                if self.page == Page::Library(LibTab::Uploads) {
                    self.loading = false;
                }
            }
            Resp::Downloaded { title, path } => {
                log::info!("downloaded to {}", path.display());
                self.toast(crate::tr!("status-downloaded", title = title.as_str()));
                self.all_downloaded.clear();
                if self.page == Page::Library(LibTab::Downloads) {
                    self.reload_downloads();
                }
            }
            Resp::Downloads(runs) => self.on_download_runs(runs),
            Resp::ConfirmDownload { target, title, tracks, count } => {
                // Edit playlist's picked picture isn't shown any more.
                if let Some(Dialog::EditPlaylist { cover: Some(super::NewCover::Picked(uri, _)), .. }) = &self.dialog {
                    ctx.forget_image(uri);
                }
                self.dialog = Some(Dialog::ConfirmDownload { target, title, tracks, count });
            }
            Resp::Uploaded { done, failed } => {
                self.toast(match (done, failed.len()) {
                    (_, 0) => crate::tr!("status-upload-complete", count = done),
                    (0, _) => crate::tr!("status-upload-failed", details = failed.join(", ")),
                    _ => crate::tr!("status-upload-partial", count = done, details = failed.join(", ")),
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
            Resp::UnisonCredit { video_id, credits } => {
                if let Some((id, set)) = &mut self.lyrics
                    && *id == video_id
                {
                    for (provider, credit) in credits {
                        if let Some(l) = set.sources.iter_mut().find(|l| l.provider == provider && l.unison.as_ref().is_some_and(|c| c.submitter.is_none())) {
                            l.unison = Some(credit);
                        }
                    }
                }
            }
            Resp::CommentsPage { token, page } => super::comments::on_page(&token, page),
            Resp::ArtistPicture { id, pic } => {
                self.artist_pics.insert(id, pic);
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
                // An older request's failure doesn't take away a cover already showing.
                let covered = self.art_key.as_deref() == Some(key.as_str()) && !self.art_uncovered;
                if self.current_id() == Some(key.as_str()) && (art.is_some() || !covered) {
                    self.art_uncovered = art.is_none();
                    // No cover: the idle backdrop's colours, not the song before's (#356).
                    let mut art = art.unwrap_or_else(crate::art::idle);
                    crate::art::soften(&mut art, self.look().background.blur_passes.saturating_sub(6));
                    self.mini.cover_changed();
                    self.track_art = Some(art);
                    self.art_key = Some(key);
                    // An animated cover on screen keeps its own colours.
                    self.show_cover_colours(now);
                }
            }
            // Smart shuffle's: only the latest one asked for, while the queue is still
            // shuffled with that song playing. Never autoplay's, which adds to the end.
            Resp::Radio { seed, tracks, smart: true } => {
                if self.smart_seed.as_ref() == Some(&seed) {
                    self.smart_seed = None;
                    if self.smart_shuffle && self.current_id() == Some(seed.as_str()) {
                        self.queue.mix_in(tracks);
                    }
                }
            }
            Resp::Radio { seed, tracks, smart: false } => {
                self.radio_pending = false;
                // For a queue since replaced: the one playing now gets its own.
                if !self.queue.tracks.iter().any(|t| t.id == seed) {
                    self.top_up();
                    return;
                }
                let had = self.queue.tracks.len();
                self.queue.add_radio(&seed, tracks);
                let grew = self.queue.tracks.len() > had;
                if std::mem::take(&mut self.ended) && grew {
                    // The track ended while radio was loading: continue now.
                    self.next();
                } else if !self.buffering {
                    self.prefetch_next();
                }
            }
            Resp::Link { link, to: LinkUse::Preview, found } => {
                if crate::link::parse(&self.search_text).as_ref() == Some(&link) {
                    self.link_preview = match found {
                        Ok(LinkTo::Song(t)) => Some((link, Ok(t))),
                        Ok(_) => None,
                        // Drawn as it is under the search box, not through `toast`.
                        Err(e) => Some((link, Err(super::without_urls(e)))),
                    };
                }
            }
            Resp::Link { found: Ok(LinkTo::Song(t)), to: LinkUse::Queue, .. } => self.apply(Action::Enqueue(t)),
            Resp::Link { found: Err(e), to: LinkUse::Queue, .. } => self.toast(e),
            Resp::Link { link, found, .. } => {
                // Unless something else was asked for meanwhile.
                if self.link_pending.take_if(|l| *l == link).is_none() {
                    return;
                }
                match found {
                    Err(e) => self.toast(e),
                    Ok(LinkTo::Page(target)) => self.apply(Action::Open(target)),
                    Ok(LinkTo::Song(t)) => self.play_linked(t),
                    // Someone else's jam takes just the song.
                    Ok(LinkTo::InList(_, mut tracks, i)) if self.jam.guest() => self.play_linked(tracks.swap_remove(i)),
                    Ok(LinkTo::InList(source, tracks, i)) => {
                        self.queue_source = Some(source);
                        self.play_list(tracks, i);
                    }
                }
            }
            // Unless something else was asked to play meanwhile; shuffled as last asked.
            Resp::PlayQueue { target, title, tracks } => {
                if let Some((p, shuffled)) = &self.play_pending
                    && *p == target
                {
                    let shuffled = *shuffled;
                    self.played_title = Some((target.clone(), title));
                    self.queue_source = Some(target);
                    if shuffled {
                        self.play_shuffled(tracks, None);
                    } else {
                        self.play_list(tracks, 0);
                    }
                }
            }
            Resp::Liked { video_id, like, other } => {
                self.ratings.invalidate(&video_id);
                if !self.unliking_soon(&video_id) {
                    self.lists.edits.confirm_likes(std::slice::from_ref(&video_id), &[]);
                    if like {
                        self.liked.insert(video_id);
                    }
                }
                self.other_half_unliked(other);
            }
            Resp::Disliked { video_id, skip_generation, ok } => {
                let id = std::slice::from_ref(&video_id);
                if ok {
                    self.ratings.confirm(&video_id, crate::ytm::SongRating::Dislike);
                    self.lists.edits.confirm_likes(id, &[]);
                    if self.rating_settings.dislike_notices {
                        self.toast(crate::tr!("status-disliked"));
                    }
                    let rated = Some(video_id.as_str());
                    if self.rating_settings.skip_disliked
                        && skip_generation == Some(self.generation)
                        && (self.current_id() == rated || self.playing_id() == rated)
                    {
                        self.skip_disliked();
                    }
                } else {
                    self.lists.edits.confirm_likes(&[], id);
                    self.request_liked(true);
                    self.toast(crate::tr!("status-dislike-failed"));
                }
            }
            Resp::RatingRemoved { video_id, was_disliked, ok } => {
                if ok {
                    self.ratings.confirm(&video_id, crate::ytm::SongRating::Indifferent);
                    if was_disliked {
                        if self.rating_settings.dislike_notices {
                            self.toast(crate::tr!("status-undisliked"));
                        }
                    } else {
                        self.liked.remove(&video_id);
                        self.lists.edits.forget_like_edit(&video_id);
                        // YouTube Music may list a video like under its matched audio track.
                        self.request_liked(true);
                    }
                } else {
                    if !was_disliked {
                        self.ratings.invalidate(&video_id);
                        self.lists.edits.confirm_likes(&[], std::slice::from_ref(&video_id));
                        self.request_liked(true);
                    }
                    self.toast(if was_disliked { crate::tr!("status-undislike-failed") } else { crate::tr!("status-unlike-failed") });
                }
            }
            Resp::SongRating { video_id, round, rating } => self.ratings.finish(video_id, round, rating, now),
            Resp::QuickPicksMore { seed, tracks } => {
                if let Some(h) = &mut self.home {
                    h.picks_loading = false;
                    if let Some(shelf) = crate::model::quick_picks(&h.shelves, self.locale.hl).map(|i| &mut h.shelves[i])
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
                for id in &taken {
                    self.ratings.invalidate(id);
                }
                if done == 0 {
                    // A Spotify transfer is adding to Liked Music (maybe paused): this waits for it.
                    self.toast(crate::tr!("status-waiting-transfer"));
                } else if total > 1 && done < total && (like || self.rating_settings.unlike_notices) {
                    let mut note = if like {
                        crate::tr!("status-liking-progress", done = done, total = total)
                    } else {
                        crate::tr!("status-unliking-progress", done = done, total = total)
                    };
                    if self.backend.api().limited() {
                        note.push(' ');
                        note.push_str(crate::tr!("transfer-rate-limited"));
                    }
                    self.toast(note);
                }
            }
            Resp::LikedMany { video_ids, failed, like, others } => {
                for id in &video_ids {
                    self.ratings.invalidate(id);
                }
                self.on_liked_many(video_ids, failed, like);
                self.other_half_unliked(others);
            }
            Resp::PlaylistCreated { id, songs } => {
                self.request_library();
                self.toast(match songs {
                    0 => crate::tr!("status-playlist-created").to_owned(),
                    n => crate::tr!("status-new-playlist-tracks", count = n),
                });
                self.go(Page::Playlist(id));
            }
            Resp::PlaylistChanged { id, change } => self.on_playlist_changed(id, change),
            Resp::Subscribed { channel_id, on, ok } => {
                // As it now stands, over a reload fetched before the change.
                let now = self.subscriptions.finish(&channel_id, on, ok);
                if let Some(a) = self.artist.as_mut().filter(|a| a.channel_id.as_deref() == Some(&channel_id)) {
                    a.subscribed = now;
                }
                // Library › Artists lists it now (or no longer).
                if ok {
                    self.request_library();
                }
                self.toast(match (ok, on) {
                    (true, true) => crate::tr!("ui-subscribed"),
                    (true, false) => crate::tr!("status-unsubscribed"),
                    (false, true) => crate::tr!("status-subscribe-failed"),
                    (false, false) => crate::tr!("status-unsubscribe-failed"),
                });
            }
            Resp::DeleteFailed { error } => {
                // It's still there: the library lists it again.
                self.request_library();
                self.toast(error);
            }
            Resp::RemoveFailed { id, error } => {
                // Its songs are still there: they come back with a reload.
                self.removal_failed(&id);
                if self.lists.known(&id) {
                    self.load_list(&id, false, true);
                }
                self.toast(error);
            }
            Resp::AddedToPlaylist { id, video_ids, error: None } => {
                self.lists.edits.confirm_add(&id, &video_ids);
                self.on_playlist_changed(id, PlaylistChange::Added);
            }
            Resp::AlreadyInPlaylist { id, tracks } => self.already_in_playlist(id, tracks),
            Resp::AddedToPlaylist { id, video_ids, error: Some(e) } => {
                // Shown as added: out again, and a reload makes sure of the rest.
                self.lists.add_failed(&id, &video_ids);
                self.sync_card_count(&id);
                if self.lists.get(&id).is_some() {
                    self.load_list(&id, false, true);
                }
                self.on_error(e);
            }
            Resp::PlaylistMoved { id, move_id, ok } => {
                // Not for moves of an account or channel left since.
                if !ok && self.lists.awaiting_move(&id, move_id) {
                    self.toast(crate::tr!("status-playlist-move-failed"));
                }
                if let Some(s) = self.lists.move_answered(&id, move_id, ok) {
                    self.start_load(s);
                }
            }
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
            Resp::StagedSong { generation, listed, song } => {
                if let Some(staged) = &mut self.staged
                    && staged.generation == generation
                    && staged.track.id == listed
                {
                    staged.song = Some(song);
                }
            }
            Resp::MusicVideo { song, video } => {
                self.music_videos.insert(song, video);
            }
            Resp::PlayError { generation, msg, offer_backup_at } => self.on_play_error(generation, msg, offer_backup_at),
            Resp::ServerQueue(q) => self.adopt_server_queue(q),
            Resp::Account { name, photo } => {
                self.account_name = name.or(self.account_name.take());
                self.account_avatar = photo.or(self.account_avatar.take());
            }
            Resp::Channels(c) => self.channels = Some(c),
            Resp::SwitchedChannel { page_id, epoch, error } => {
                let name = self.switching_to.take().unwrap_or_else(|| crate::tr!("status-another-channel").to_owned());
                match error {
                    Some(e) => self.toast(e),
                    None => {
                        // Everything reloads as after signing in, but it's the same sign-in
                        // with the same channels: only which one is used changed.
                        let channels = self.channels.take().map(|c| crate::backend::Channels { current: page_id, epoch, ..c });
                        let asked = self.channels_asked;
                        self.on_resp(ctx, Resp::LoggedIn(true), now);
                        (self.channels, self.channels_asked) = (channels, asked);
                        self.toast(crate::tr!("status-switched-channel", name = name.as_str()));
                    }
                }
            }
            Resp::LoggedIn(v) => self.on_logged_in(v, now),
            // Only what the page was waiting for says why it failed, not some other request.
            Resp::Error(e) => {
                if self.page_waiting() {
                    self.offline = false;
                }
                self.on_error(e);
            }
            // Back: the page that couldn't load loads, as Try again would. Should it still
            // fail, the checks go on as far apart as they'd got.
            Resp::Reachable(true) if self.offline && self.load_failed => {
                let wait = self.online_wait;
                self.refresh();
                self.online_wait = wait;
            }
            Resp::Reachable(_) => {}
            Resp::Offline => {
                if self.page_waiting() {
                    self.offline = true;
                }
                // On Downloads already, it doesn't point there.
                let here = self.page == super::Page::Library(super::LibTab::Downloads);
                self.on_error(super::offline_text(here).to_owned());
            }
        }
    }

    /// The home feed, replacing the one shown.
    fn on_home(&mut self, page: HomePage) {
        // Until the account menu answers (Req::Account).
        if self.account_avatar.is_none() {
            self.account_avatar = page.shelves.iter().find(|s| s.own).and_then(|s| s.strap_thumb.clone());
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
        // Kept for the next launch, unless a mood filters it.
        if let Some(h) = self.home.as_ref().filter(|h| !self.home_mood && !h.shelves.is_empty() && !h.chips.iter().any(|c| c.selected)) {
            let cache = super::HomeCache { logged_in: self.logged_in, hl: self.locale.hl.to_owned(), gl: self.locale.gl.to_owned(), chips: h.chips.clone(), shelves: h.shelves.clone() };
            super::HomeCache::store(self.backend.data_dir(), Some(cache));
        }
    }

    /// Songs whose likes went along with their other versions': not liked here either.
    fn other_half_unliked(&mut self, ids: impl IntoIterator<Item = String>) {
        for id in ids {
            self.ratings.invalidate(&id);
            self.liked.remove(&id);
            let listed = self.opened(super::LIKED_MUSIC).and_then(|o| o.c.tracks.iter().find(|t| t.id == id).cloned());
            if let Some(t) = listed {
                self.set_liked(&[t], false);
                self.lists.edits.confirm_likes(std::slice::from_ref(&id), &[]);
            }
        }
    }

    /// Library › Downloads read whole: every file looked at (disk work, so on a visit,
    /// not as each song lands).
    pub(super) fn read_downloads(&mut self) {
        // Counted first: a song saved meanwhile is taken on again next time, not missed.
        self.downloads_seen = crate::downloads::saves();
        self.downloads = Some(crate::downloads::all());
    }

    /// Library › Downloads takes on the songs saved since it was read, at the top, from
    /// the index (no look at the other files). New files come first, so the picked rows
    /// move: the same songs stay picked.
    fn reload_downloads(&mut self) {
        let Some(old) = self.downloads.take() else { return self.read_downloads() };
        let (seen, mut new) = crate::downloads::saved_since(self.downloads_seen);
        self.downloads_seen = seen;
        if new.is_empty() {
            self.downloads = Some(old);
            return;
        }
        let fresh: HashSet<String> = new.iter().map(|t| t.id.clone()).collect();
        new.extend(old.iter().filter(|t| !fresh.contains(&t.id)).cloned());
        if self.selection.page == Some(Page::Library(LibTab::Downloads)) {
            let id = |i: usize| old.get(i).map(|t| t.id.as_str());
            let at = |id: &str| new.iter().position(|t| t.id == id);
            let picked: HashSet<&str> = self.selection.rows.iter().filter_map(|&i| id(i)).collect();
            self.selection.rows = new.iter().enumerate().filter(|(_, t)| picked.contains(t.id.as_str())).map(|(i, _)| i).collect();
            self.selection.anchor = self.selection.anchor.and_then(id).and_then(at);
        }
        self.downloads = Some(new);
    }

    /// Downloads of albums, playlists and selections moved on. One toast as each starts
    /// and one as it ends, not one per song: their pages and Library › Downloads show
    /// how far they are.
    fn on_download_runs(&mut self, runs: Vec<crate::backend::DownloadRun>) {
        let mut notes = Vec::new();
        for r in &runs {
            if r.finished() {
                notes.push(download_summary(r, &r.title));
            } else if !self.download_runs.iter().any(|o| o.key == r.key) {
                let count = r.total - r.done;
                notes.push(if r.title.is_empty() {
                    crate::tr!("status-download-batch", count = count)
                } else {
                    crate::tr!("status-download-collection", title = r.title.as_str(), count = count)
                });
            }
        }
        // Lists that finished are looked at again for their "Downloaded".
        if runs.iter().any(|r| r.finished()) {
            self.all_downloaded.clear();
        }
        let saved = |rs: &[crate::backend::DownloadRun], key: &str| rs.iter().find(|r| r.key == key).map_or(0, |r| r.saved);
        let more = runs.iter().any(|r| r.saved > saved(&self.download_runs, &r.key));
        self.download_runs = runs.into_iter().filter(|r| !r.finished()).collect();
        if !notes.is_empty() {
            self.toast(notes.join(" \u{b7} "));
        }
        if more && self.page == Page::Library(LibTab::Downloads) {
            self.reload_downloads();
        }
    }

    /// Liking or unliking many songs at once has finished: `failed` are the ones YouTube
    /// Music refused.
    fn on_liked_many(&mut self, video_ids: Vec<String>, failed: Vec<String>, like: bool) {
        for id in &failed {
            self.ratings.invalidate(id);
        }
        log::debug!("like={like} answered: {} taken, {} refused {:?}", video_ids.len(), failed.len(), failed.iter().take(5).collect::<Vec<_>>());
        self.lists.edits.confirm_likes(&video_ids, &failed);
        // The refused ones come back as they were with a reload.
        if !failed.is_empty() {
            self.request_liked(true);
        }
        if !like && (!self.rating_settings.unlike_notices || video_ids.len() <= 1) && failed.is_empty() {
            return;
        }
        let count = video_ids.len();
        if count == 0 {
            let note = crate::tr!("status-like-refused");
            return self.toast(if failed.len() > 1 {
                format!("{}. {note}", crate::tr!("status-refused-count", count = failed.len()))
            } else {
                note.to_owned()
            });
        }
        let mut msg = if like { crate::tr!("status-like-success", count = count) } else { crate::tr!("likes-removed", count = count) };
        if !failed.is_empty() {
            msg += &format!(" · {}", crate::tr!("status-refused-count", count = failed.len()));
        }
        self.toast(msg);
    }

    /// Apply an acknowledged playlist change.
    fn on_playlist_changed(&mut self, id: String, change: PlaylistChange) {
        self.request_library();
        // A sort taken waits on, for the reload in its order; anything else ends it.
        match self.sorting.get_mut(&id) {
            Some(w) if change == PlaylistChange::Sorted => w.taken(),
            _ => {
                self.sorting.remove(&id);
            }
        }
        if change == PlaylistChange::Deleted {
            // Its load is dropped too, and the queue stops waiting for it.
            self.lists.forget(&id);
            // Kept downloaded, it would be loaded at every launch, and fail each time.
            self.kept_downloaded.retain(|k| *k != id);
            if self.queue.fill.as_ref().is_some_and(|f| f.id == id) {
                self.queue.fill = None;
            }
            self.leave_playlist(&id);
        } else {
            if change == PlaylistChange::Removed {
                self.lists.edits.confirm_removal(&id);
            }
            // Kept for going back (or playing): reload it. What YouTube Music still
            // lists after a removal is dropped as it arrives.
            if self.lists.known(&id) {
                self.load_list(&id, false, true);
            }
        }
        let message = match change {
            // Their earlier toasts offer Undo.
            PlaylistChange::Removed | PlaylistChange::Deleted => return,
            PlaylistChange::Added => crate::tr!("status-playlist-added"),
            PlaylistChange::Updated => crate::tr!("status-playlist-updated"),
            PlaylistChange::Sorted => crate::tr!("status-playlist-sorted"),
            PlaylistChange::SortFailed => crate::tr!("status-playlist-sort-failed"),
            PlaylistChange::SavedToLibrary => crate::tr!("status-library-saved"),
            PlaylistChange::RemovedFromLibrary => crate::tr!("status-library-removed"),
        };
        self.toast(message);
    }

    /// A song couldn't play: skip it, or stop when skipping can't help.
    fn on_play_error(&mut self, generation: u64, msg: String, offer_backup_at: Option<Duration>) {
        if generation == self.generation {
            if offer_backup_at.is_some_and(|at| self.offer_backup_stream(at)) {
                return;
            }
            self.buffering = false;
            let title = self.current().map(|t| t.title.clone()).unwrap_or_default();
            let has_next = !self.queue.at_end() && self.queue.index.is_some();
            let age_restricted = msg.contains("age-restricted") || msg.contains("confirm your age");
            // Skipping can't help when the account is the problem: the rest of
            // an album like this needs it too. Stop on the song instead.
            if let Some(block) = account_block(&msg, self.logged_in) {
                self.skip_streak = 0;
                self.toast(match block {
                    AccountBlock::Premium => crate::tr!("status-play-premium", title = title.as_str()),
                    AccountBlock::SignIn => crate::tr!("status-play-sign-in", title = title.as_str()),
                });
                return;
            }
            let reason = if age_restricted { crate::tr!("status-age-restricted") } else { msg.as_str() };
            if has_next && !self.jam.guest() && self.skip_streak < MAX_SKIPS {
                // Don't stall the queue on one unplayable song.
                self.skip_streak += 1;
                self.toast(crate::tr!("status-skipped-track", title = title.as_str(), reason = reason));
                self.next();
            } else {
                self.skip_streak = 0;
                // The last song: radio's next songs, when they come, go on from it.
                self.ended = !has_next;
                self.toast(crate::tr!("status-track-failed", title = title.as_str(), reason = reason));
            }
        }
    }

    /// Signed in or out: what belonged to the last account goes, and the new one's home,
    /// library and likes load.
    fn on_logged_in(&mut self, v: bool, now: f64) {
        // An Undo made meanwhile was the last account's: it isn't sent as this one.
        self.undo = None;
        // So is a question about adding to its playlist.
        if matches!(self.dialog, Some(Dialog::ConfirmAddDuplicates { .. })) {
            self.dialog = None;
        }
        self.toast_after_unlike = None;
        self.ratings.reset();
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
        self.scoped.clear();
        // Whether it's subscribed is this account's (or channel's) to say.
        self.artist = None;
        self.subscriptions = Default::default();
        // Your own playlists show differently (and private ones not at all) to another
        // account, and its likes aren't this one's.
        self.lists.clear();
        self.lists.edits = Default::default();
        self.liked.clear();
        // The playlists kept downloaded are the last account's (or channel's), maybe its
        // private ones: they aren't loaded for this one. What's downloaded stays.
        self.kept_downloaded.clear();
        self.queue.fill = None;
        self.play_pending = None;
        (self.link_pending, self.link_preview) = (None, None);
        if self.page_list().is_some() || matches!(self.page, Page::Artist(_)) {
            self.refresh();
        }
        self.home = None;
        self.home_loading = true;
        self.home_mood = false;
        // The last account's: the next launch waits for this one's.
        super::HomeCache::store(self.backend.data_dir(), None);
        self.backend.request(Req::Home(None));
        if v {
            self.request_library();
            self.request_liked(false);
            self.backend.request(Req::Account);
        }
        self.toast(if v { crate::tr!("status-signed-in") } else { crate::tr!("status-signed-out") });
    }

    /// A request failed: pages waiting for it stop waiting, and the error shows.
    /// Whether the page is still waiting for its first data: an error now is its own.
    fn page_waiting(&self) -> bool {
        self.loading || (self.page == Page::Home && self.home_loading)
    }

    fn on_error(&mut self, e: String) {
        if let Some(Dialog::Collaborate { info, busy, .. }) = &mut self.dialog {
            *busy = false;
            // Nothing to show if it never loaded.
            if info.is_none() {
                self.dialog = None;
            }
        }
        // A page still waiting for its first data shows a retry instead of loading forever.
        self.load_failed |= self.page_waiting();
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

/// What a finished download of many songs did. A selection has an empty title.
fn download_summary(r: &crate::backend::DownloadRun, title: &str) -> String {
    let name = if title.is_empty() {
        String::new()
    } else {
        format!(" {}", crate::tr!("status-download-quoted-title", title = title))
    };
    let songs = crate::tr!("status-songs", count = r.saved);
    let mut msg = if r.cancelled {
        crate::tr!("status-download-stopped", name = name.as_str(), songs = songs.as_str())
    } else if r.saved == 0 && r.failed.is_empty() {
        return if title.is_empty() { crate::tr!("status-download-already").to_owned() } else { crate::tr!("status-download-name-already", title = title) };
    } else {
        crate::tr!("status-download-done", name = name.as_str(), songs = songs.as_str())
    };
    if r.skipped > 0 && !r.cancelled {
        msg += &format!(", {}", crate::tr!("status-download-already-count", count = r.skipped));
    }
    if !r.failed.is_empty() {
        let few: Vec<&str> = r.failed.iter().take(3).map(String::as_str).collect();
        let more = if r.failed.len() > 3 { ", …" } else { "" };
        let details = format!("{}{more}", few.join(", "));
        msg += &format!("; {}", crate::tr!("status-download-fail-count", count = r.failed.len(), details = details));
    }
    msg
}

#[cfg(test)]
mod download_summary_tests {
    use super::download_summary;
    use crate::backend::DownloadRun;

    fn run(saved: usize, skipped: usize, failed: &[&str], cancelled: bool) -> DownloadRun {
        let failed: Vec<String> = failed.iter().map(|s| s.to_string()).collect();
        DownloadRun { key: "PL1".into(), title: "Mix".into(), total: saved + failed.len(), done: saved + failed.len(), saved, skipped, failed, cancelled, ..Default::default() }
    }

    #[test]
    fn says_what_a_run_did() {
        let name = "Mix";
        assert_eq!(download_summary(&run(37, 3, &[], false), name), "Downloaded \u{201c}Mix\u{201d}: 37 songs, 3 already there");
        assert_eq!(download_summary(&run(0, 12, &[], false), name), "\u{201c}Mix\u{201d} is downloaded already");
        assert_eq!(download_summary(&run(0, 2, &[], false), ""), "Downloaded already");
        assert_eq!(download_summary(&run(5, 0, &[], true), name), "Stopped downloading \u{201c}Mix\u{201d}: 5 songs saved");
        assert_eq!(
            download_summary(&run(1, 0, &["A", "B", "C", "D"], false), ""),
            "Downloaded: 1 song; 4 couldn't be downloaded (A, B, C, …)"
        );
    }
}

/// When a song failed because of the account (Premium-only, or needs signing in),
/// the message to show instead of skipping: the rest of an album like that fails the
/// same way.
enum AccountBlock {
    Premium,
    SignIn,
}

fn account_block(msg: &str, logged_in: bool) -> Option<AccountBlock> {
    let m = msg.to_ascii_lowercase();
    if m.contains("premium") {
        Some(AccountBlock::Premium)
    } else if !logged_in && (m.contains("sign in") || m.contains("age-restricted") || m.contains("confirm your age")) {
        Some(AccountBlock::SignIn)
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
