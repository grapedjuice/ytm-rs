//! Albums, playlists and Liked Music for the pages and playback: loading them
//! (see `lists`), and what arrives.

use std::collections::HashSet;

use crate::backend::{Collection, Req};
use crate::model::{ShelfKind, Target, Track};

use super::lists::{Arrival, Done, LIKED_MUSIC, Opened, Start};
use super::queue::QueueFill;
use super::{App, shuffle};

impl App {
    /// Load an album or playlist, unless a load of it is under way. `changed`: if one is,
    /// it's loaded once more afterwards.
    pub(super) fn load_list(&mut self, id: &str, album: bool, changed: bool) {
        if let Some(s) = self.lists.load(id, album, changed) {
            self.start_load(s);
        }
    }

    /// Opening an album or playlist page: loaded unless a copy from the last few minutes is kept.
    pub(super) fn visit_list(&mut self, id: &str, album: bool) {
        if let Some(s) = self.lists.visit(id, album, std::time::Instant::now()) {
            self.start_load(s);
        }
    }

    fn start_load(&self, s: Start) {
        self.backend.request(match s.album {
            true => Req::Album { id: s.id, load: s.load },
            false => Req::Playlist { id: s.id, load: s.load, stream: s.stream },
        });
    }

    pub(super) fn opened(&self, id: &str) -> Option<&Opened> {
        self.lists.get(id)
    }

    /// More of the kept copy is still coming in, page by page.
    pub(super) fn receiving(&self, id: &str) -> bool {
        self.lists.receiving(id)
    }

    /// Liked Music: kept like any playlist, and also the Library's liked songs and the
    /// hearts everywhere. `changed`: load it again even if a load is under way.
    pub(super) fn request_liked(&mut self, changed: bool) {
        if self.logged_in {
            self.load_list(LIKED_MUSIC, false, changed);
        }
    }

    /// Likes made here show at once, in the hearts and in Liked Music as kept. (A whole
    /// reload asked for just before can still come back without them.)
    pub(super) fn set_liked(&mut self, tracks: &[Track], like: bool) {
        log::debug!("like={like}: {} songs, e.g. {:?}", tracks.len(), tracks.iter().take(3).map(|t| &t.id).collect::<Vec<_>>());
        for t in tracks {
            if like {
                self.liked.insert(t.id.clone());
            } else {
                self.liked.remove(&t.id);
            }
        }
        self.lists.set_liked(tracks, like);
    }

    /// Play a kept album or playlist from the start; one still coming in fills the queue
    /// as the rest arrives.
    pub(super) fn play_opened(&mut self, id: String, shuffled: bool) {
        let Some(o) = self.opened(&id).filter(|o| !o.c.tracks.is_empty()) else { return };
        let mut tracks = o.c.tracks.clone();
        if shuffled {
            shuffle(&mut tracks);
        }
        let fill = self.receiving(&id).then(|| QueueFill { id: id.clone(), load: o.load, shuffled, ran_out: false });
        self.queue_source = Some(if o.c.is_album { Target::Album(id) } else { Target::Playlist(id) });
        self.play_list_filling(tracks, 0, fill);
    }

    /// The album or playlist on screen.
    pub(super) fn current_collection(&mut self) -> Option<&mut Collection> {
        let id = self.page_list()?.to_owned();
        self.lists.collection_mut(&id)
    }

    /// An album or playlist (or a playlist's first page) came in. Only the load under way
    /// counts: one from before a sign-in change or a deletion is dropped.
    pub(super) fn collection_in(&mut self, load: u64, c: Collection, more: bool) {
        let id = c.id.clone();
        let keep = self.keep_lists();
        let keep: Vec<&str> = keep.iter().map(String::as_str).collect();
        let Some(arrival) = self.lists.arrived(load, c, more, &keep) else { return };
        if self.page_list() == Some(id.as_str()) {
            self.loading = false;
        }
        if id == LIKED_MUSIC
            && let Some(o) = self.lists.get(LIKED_MUSIC)
        {
            self.liked = o.c.tracks.iter().map(|t| t.id.clone()).collect();
            log::debug!("hearts: {} liked (from Liked Music's first page or whole copy)", self.liked.len());
        }
        if let Arrival::Done(done) = arrival {
            self.load_done(&id, done);
        }
        // Its card's play button was pressed before it was here (in someone else's jam the host plays).
        if let Some((Target::Playlist(p), shuffled)) = &self.play_pending
            && *p == id
        {
            let shuffled = *shuffled;
            self.play_pending = None;
            if !self.jam.guest() {
                self.play_opened(id, shuffled);
            }
        }
    }

    /// A playlist's next page: onto the kept copy, and onto the queue when that was played
    /// before all of it was in. An empty last one also ends a load that failed.
    pub(super) fn collection_more(&mut self, id: String, load: u64, tracks: Vec<Track>, set_ids: Vec<Option<String>>, more: bool) {
        let keep = self.keep_lists();
        let keep: Vec<&str> = keep.iter().map(String::as_str).collect();
        let Some(page) = self.lists.page(&id, load, tracks, set_ids, more, &keep) else { return };
        if id == LIKED_MUSIC {
            if page.kept {
                self.liked.extend(page.tracks.iter().map(|t| t.id.clone()));
            }
            log::debug!("hearts: {} liked", self.liked.len());
        }
        self.fill_queue(&id, load, page.tracks, more);
        if let Some(done) = page.done {
            self.load_done(&id, done);
        }
    }

    /// An album or playlist load ended. One that changed meanwhile loads again; one that
    /// failed before anything came leaves its page with the retry.
    fn load_done(&mut self, id: &str, done: Done) {
        match done {
            Done::Kept => {}
            Done::Reload(s) => self.start_load(s),
            Done::NotKept => {
                if matches!(&self.play_pending, Some((Target::Playlist(p), _)) if p == id) {
                    self.play_pending = None;
                }
                if self.page_list() == Some(id) {
                    self.loading = false;
                    self.load_failed = true;
                }
            }
        }
    }

    /// The kept albums and playlists not to drop for room: the page on screen and what's playing.
    fn keep_lists(&self) -> Vec<String> {
        let playing = match &self.queue_source {
            Some(Target::Album(id) | Target::Playlist(id)) => Some(id.clone()),
            _ => None,
        };
        self.page_list().map(str::to_owned).into_iter().chain(playing).collect()
    }

    /// The next songs of a playlist played before all of it was in join the queue.
    fn fill_queue(&mut self, id: &str, load: u64, tracks: Vec<Track>, more: bool) {
        let playing_it = self.queue_source == Some(Target::Playlist(id.to_owned()));
        let Some(filled) = self.queue.fill_from(id, load, tracks, more, playing_it) else { return };
        if filled.ran_out {
            self.next();
        } else if filled.was_at_end && !self.buffering {
            self.prefetch_next();
        }
        self.top_up();
    }

    pub(super) fn request_library(&mut self) {
        self.library_requested = std::time::Instant::now();
        self.backend.request(Req::Library);
    }

    pub(super) fn preferred_artist_ids(&self) -> Vec<String> {
        let mut ids = HashSet::new();
        if let Some(home) = &self.home {
            for shelf in &home.shelves {
                if shelf.title != "Listen again" && shelf.title != "Quick picks" { continue; }
                match &shelf.kind {
                    ShelfKind::Cards(cards) => {
                        for card in cards {
                            ids.extend(card.links.iter().filter_map(|link| link.id.clone()));
                            if let Target::Song(track) = &card.target {
                                ids.extend(track.artists.iter().filter_map(|artist| artist.id.clone()));
                            }
                        }
                    }
                    ShelfKind::Songs { tracks, .. } => {
                        for track in tracks {
                            ids.extend(track.artists.iter().filter_map(|artist| artist.id.clone()));
                        }
                    }
                }
            }
        }
        ids.into_iter().collect()
    }
}
