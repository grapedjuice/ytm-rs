//! The play queue: its songs, the one playing, repeat, and the songs of a playlist
//! still coming in. Only the state: the app starts the songs these pick, prefetches,
//! and asks for radio.

use crate::model::Track;

#[derive(Clone, Copy, PartialEq)]
pub enum Repeat {
    Off,
    All,
    One,
}

/// A playlist's load whose next pages also go to the queue, and whether that queue
/// is shuffled (they're then mixed into what's still to come). `ran_out`: playback
/// reached the end of what had come, and carries on when more does (or the load ends).
pub struct QueueFill {
    pub id: String,
    pub load: u64,
    pub shuffled: bool,
    pub ran_out: bool,
}

/// More of a playlist joined the queue.
pub struct Filled {
    /// Playback was waiting for it: go on to the next song.
    pub ran_out: bool,
    /// The song playing was the last one before.
    pub was_at_end: bool,
}

pub struct Queue {
    pub tracks: Vec<Track>,
    /// The song playing (or paused, or loading).
    pub index: Option<usize>,
    pub repeat: Repeat,
    /// The queue is a playlist played while its songs were still coming in: the rest
    /// join the queue as they arrive.
    pub fill: Option<QueueFill>,
}

/// How many songs before and after the current one a saved session keeps: liked-songs
/// queues can run to 1000+ songs.
const SAVED_BEFORE: usize = 50;
const SAVED_AFTER: usize = 150;

impl Queue {
    pub fn new(repeat: Repeat) -> Self {
        Self { tracks: Vec::new(), index: None, repeat, fill: None }
    }

    pub fn current(&self) -> Option<&Track> {
        self.index.and_then(|i| self.tracks.get(i))
    }

    /// The song playing is the last one.
    pub fn at_end(&self) -> bool {
        self.index.is_some_and(|i| i + 1 >= self.tracks.len())
    }

    /// Play `tracks` from `start`; `fill`: a playlist still coming in, whose other songs
    /// follow. False (and nothing changes) when there are none.
    pub fn replace(&mut self, tracks: Vec<Track>, start: usize, fill: Option<QueueFill>) -> bool {
        if tracks.is_empty() {
            return false;
        }
        self.fill = fill;
        self.index = Some(start.min(tracks.len() - 1));
        self.tracks = tracks;
        true
    }

    /// Take up a queue from elsewhere (last session, another device), at `index`, which
    /// the caller has checked is one of `tracks`.
    pub fn adopt(&mut self, tracks: Vec<Track>, index: usize) {
        self.tracks = tracks;
        self.index = Some(index);
        self.fill = None;
    }

    /// Next, or the end of a song: true when there's a song to play. At the end of a
    /// playlist still coming in, playback waits for its next songs. (Repeating one song
    /// is the caller's: a song's end replays it rather than coming here.)
    pub fn advance(&mut self) -> bool {
        let Some(i) = self.index else { return false };
        if i + 1 < self.tracks.len() {
            self.index = Some(i + 1);
        } else if let Some(fill) = &mut self.fill {
            fill.ran_out = true;
            return false;
        } else if self.repeat == Repeat::All && !self.tracks.is_empty() {
            self.index = Some(0);
        } else {
            return false;
        }
        true
    }

    /// Back to the song before: false at the first.
    pub fn back(&mut self) -> bool {
        match self.index.filter(|&i| i > 0) {
            Some(i) => {
                self.index = Some(i - 1);
                true
            }
            None => false,
        }
    }

    /// What plays after the current song, to fetch ahead.
    pub fn upcoming(&self) -> Option<&Track> {
        let i = self.index?;
        match self.repeat {
            Repeat::One => None,
            Repeat::All => self.tracks.get(i + 1).or(self.tracks.first()),
            Repeat::Off => self.tracks.get(i + 1),
        }
    }

    /// A song started: it no longer waits for a playlist's next songs.
    pub fn started(&mut self) {
        if let Some(fill) = &mut self.fill {
            fill.ran_out = false;
        }
    }

    /// Shuffle what's still to come (and what of a playlist is still coming in): false
    /// when nothing is.
    pub fn shuffle_upcoming(&mut self) -> bool {
        let start = self.index.map_or(0, |i| i + 1);
        if start >= self.tracks.len() {
            return false;
        }
        super::shuffle(&mut self.tracks[start..]);
        if let Some(fill) = &mut self.fill {
            fill.shuffled = true;
        }
        true
    }

    /// Play `t` after the current song: true when nothing was playing, so it starts now.
    pub fn play_next(&mut self, t: Track) -> bool {
        let at = self.index.map_or(0, |i| i + 1).min(self.tracks.len());
        self.tracks.insert(at, t);
        if self.index.is_some() {
            return false;
        }
        self.index = Some(0);
        true
    }

    /// Add `t` at the end: true when nothing was playing, so it starts now.
    pub fn enqueue(&mut self, t: Track) -> bool {
        self.tracks.push(t);
        if self.index.is_some() {
            return false;
        }
        self.index = Some(self.tracks.len() - 1);
        true
    }

    /// Take song `i` out, unless it's the one playing.
    pub fn remove(&mut self, i: usize) {
        if i < self.tracks.len() && Some(i) != self.index {
            self.tracks.remove(i);
            if let Some(cur) = self.index.filter(|&c| c > i) {
                self.index = Some(cur - 1);
            }
        }
    }

    /// Radio songs for `seed` join the end, without the seed or songs already queued.
    /// Returns whether the song playing was the last one.
    pub fn add_radio(&mut self, seed: &str, tracks: Vec<Track>) -> bool {
        let have: std::collections::HashSet<String> = self.tracks.iter().map(|t| t.id.clone()).collect();
        let was_at_end = self.at_end();
        self.tracks.extend(tracks.into_iter().filter(|t| t.id != seed && !have.contains(&t.id)));
        was_at_end
    }

    /// The next page of the playlist the queue fills from (`id`, `load`) joins it; `more`:
    /// pages still follow. `playing_it`: the queue still plays that playlist. `None` when
    /// the page doesn't apply (though the last one still ends the fill); an empty last
    /// page that does still says whether playback was waiting for it.
    pub fn fill_from(&mut self, id: &str, load: u64, tracks: Vec<Track>, more: bool, playing_it: bool) -> Option<Filled> {
        let fill = self.fill.as_mut().filter(|f| f.id == id && f.load == load)?;
        let (shuffled, ran_out) = (fill.shuffled, std::mem::take(&mut fill.ran_out));
        if !more {
            self.fill = None;
        }
        if !playing_it {
            return None;
        }
        let was_at_end = self.at_end();
        self.tracks.extend(tracks);
        if shuffled {
            // Mixed into what's still to come, past the next song (it may be loading already).
            let from = self.index.map_or(0, |i| i + if was_at_end { 1 } else { 2 }).min(self.tracks.len());
            super::shuffle(&mut self.tracks[from..]);
        }
        Some(Filled { ran_out, was_at_end })
    }

    /// The part of the queue a saved session keeps, and the current song's place in it.
    pub fn saved(&self) -> Option<(&[Track], usize)> {
        let i = self.index?;
        let from = i.saturating_sub(SAVED_BEFORE);
        let to = (i + SAVED_AFTER).min(self.tracks.len());
        Some((&self.tracks[from..to], i - from))
    }
}

#[cfg(test)]
mod tests {
    use super::{Queue, QueueFill, Repeat, Track};

    fn track(id: &str) -> Track {
        Track { id: id.into(), title: id.into(), artists: vec![], album: None, duration: None, thumbs: vec![], track_nr: None, plays: None, music_video: false }
    }

    fn queue(songs: &[&str], index: usize, repeat: Repeat) -> Queue {
        let mut q = Queue::new(repeat);
        assert!(q.replace(songs.iter().map(|s| track(s)).collect(), index, None));
        q
    }

    fn ids(q: &Queue) -> Vec<&str> {
        q.tracks.iter().map(|t| t.id.as_str()).collect()
    }

    fn filling(id: &str, load: u64, shuffled: bool) -> Option<QueueFill> {
        Some(QueueFill { id: id.into(), load, shuffled, ran_out: false })
    }

    #[test]
    fn next_and_back_follow_repeat() {
        let mut q = queue(&["a", "b"], 0, Repeat::Off);
        assert!(q.advance());
        assert_eq!(q.index, Some(1));
        // At the end: stays there, unless repeating all.
        assert!(!q.advance());
        assert_eq!(q.index, Some(1));
        q.repeat = Repeat::All;
        assert!(q.advance());
        assert_eq!(q.index, Some(0));
        // Next moves on even when repeating one song (the song's own end replays it).
        q.repeat = Repeat::One;
        assert!(q.advance());
        assert_eq!(q.index, Some(1));
        assert!(q.back());
        assert!(!q.back());
        assert_eq!(q.index, Some(0));
        // Nothing queued: nothing to do.
        assert!(!Queue::new(Repeat::All).advance());
    }

    #[test]
    fn what_plays_next_is_fetched_ahead() {
        let q = queue(&["a", "b"], 1, Repeat::Off);
        assert!(q.upcoming().is_none());
        let q = queue(&["a", "b"], 1, Repeat::All);
        assert_eq!(q.upcoming().unwrap().id, "a");
        let q = queue(&["a", "b"], 0, Repeat::One);
        assert!(q.upcoming().is_none());
        let q = queue(&["a", "b"], 0, Repeat::Off);
        assert_eq!(q.upcoming().unwrap().id, "b");
    }

    #[test]
    fn playing_nothing_leaves_the_queue_as_it_was() {
        let mut q = Queue::new(Repeat::Off);
        q.replace(vec![track("a")], 0, filling("PL1", 1, false));
        assert!(!q.replace(vec![], 0, None));
        assert_eq!(ids(&q), ["a"]);
        assert_eq!(q.fill.as_ref().unwrap().id, "PL1");
        // A start past the end plays the last song.
        assert!(q.replace(vec![track("x"), track("y")], 9, None));
        assert_eq!(q.current().unwrap().id, "y");
    }

    #[test]
    fn songs_added_by_hand() {
        let mut q = Queue::new(Repeat::Off);
        // Into an empty queue: it starts.
        assert!(q.enqueue(track("a")));
        assert_eq!(q.index, Some(0));
        assert!(!q.enqueue(track("c")));
        assert!(!q.play_next(track("b")));
        assert_eq!(ids(&q), ["a", "b", "c"]);
        let mut empty = Queue::new(Repeat::Off);
        assert!(empty.play_next(track("x")));
        assert_eq!(empty.current().unwrap().id, "x");

        // Taking one out keeps the song playing where it is; the one playing stays.
        q.index = Some(2);
        q.remove(0);
        assert_eq!(ids(&q), ["b", "c"]);
        assert_eq!(q.current().unwrap().id, "c");
        q.remove(1);
        q.remove(5);
        assert_eq!(ids(&q), ["b", "c"]);
    }

    #[test]
    fn radio_adds_only_new_songs() {
        let mut q = queue(&["seed", "a"], 1, Repeat::Off);
        assert!(q.add_radio("seed", vec![track("seed"), track("a"), track("b"), track("c")]));
        assert_eq!(ids(&q), ["seed", "a", "b", "c"]);
        assert!(!q.add_radio("seed", vec![track("d")]));
    }

    #[test]
    fn a_playlist_still_coming_in_fills_the_queue() {
        let mut q = Queue::new(Repeat::Off);
        q.replace(vec![track("a"), track("b")], 0, filling("PL1", 3, false));
        // Another playlist's page, or another load's, doesn't join.
        assert!(q.fill_from("PL2", 3, vec![track("x")], true, true).is_none());
        assert!(q.fill_from("PL1", 2, vec![track("x")], true, true).is_none());
        let f = q.fill_from("PL1", 3, vec![track("c")], true, true).unwrap();
        assert!(!f.ran_out && !f.was_at_end);
        assert_eq!(ids(&q), ["a", "b", "c"]);

        // Playback reached the end before the next page: it waits, then carries on.
        q.index = Some(2);
        assert!(!q.advance());
        assert!(q.fill.as_ref().unwrap().ran_out);
        let f = q.fill_from("PL1", 3, vec![track("d")], false, true).unwrap();
        assert!(f.ran_out && f.was_at_end);
        assert!(q.fill.is_none());
        assert!(q.advance());
        assert_eq!(q.current().unwrap().id, "d");
    }

    #[test]
    fn waiting_for_a_playlist_comes_before_repeating_it() {
        let mut q = Queue::new(Repeat::All);
        q.replace(vec![track("a"), track("b")], 1, filling("PL1", 1, false));
        // At the end of what came: it waits rather than starting over.
        assert!(!q.advance());
        assert_eq!(q.index, Some(1));
        // The load ends with nothing more: playback goes on, and now starts over.
        let f = q.fill_from("PL1", 1, vec![], false, true).unwrap();
        assert!(f.ran_out && f.was_at_end);
        assert!(q.fill.is_none());
        assert!(q.advance());
        assert_eq!(q.index, Some(0));
    }

    #[test]
    fn filling_stops_when_something_else_plays() {
        let mut q = Queue::new(Repeat::Off);
        q.replace(vec![track("a")], 0, filling("PL1", 1, false));
        // The queue plays something else now: the page doesn't join, and the last one ends the fill.
        assert!(q.fill_from("PL1", 1, vec![track("b")], true, false).is_none());
        assert!(q.fill.is_some());
        assert!(q.fill_from("PL1", 1, vec![track("c")], false, false).is_none());
        assert!(q.fill.is_none());
        assert_eq!(ids(&q), ["a"]);
        // A song starting means it no longer waits.
        q.replace(vec![track("a")], 0, filling("PL1", 2, false));
        q.advance();
        q.started();
        assert!(!q.fill.as_ref().unwrap().ran_out);
    }

    #[test]
    fn shuffling_up_next_leaves_what_played() {
        let songs: Vec<String> = (0..40).map(|i| i.to_string()).collect();
        let refs: Vec<&str> = songs.iter().map(String::as_str).collect();
        let mut q = queue(&refs, 5, Repeat::Off);
        q.fill = filling("PL1", 1, false);
        assert!(q.shuffle_upcoming());
        assert!(q.fill.as_ref().unwrap().shuffled);
        assert_eq!(ids(&q)[..6], refs[..6]);
        let mut rest: Vec<&str> = ids(&q)[6..].to_vec();
        // 34 songs coming back in their order by chance is too unlikely to happen.
        assert_ne!(rest, refs[6..]);
        rest.sort_by_key(|s| s.parse::<u32>().unwrap());
        assert_eq!(rest, refs[6..]);
        // At the last song there's nothing to shuffle.
        let mut q = queue(&["a", "b"], 1, Repeat::Off);
        assert!(!q.shuffle_upcoming());
    }

    #[test]
    fn a_shuffled_playlists_next_pages_mix_in_after_the_next_song() {
        let songs: Vec<String> = (0..40).map(|i| i.to_string()).collect();
        let refs: Vec<&str> = songs.iter().map(String::as_str).collect();
        // The song after the current one stays: it may be loading already.
        let mut q = queue(&refs[..10], 2, Repeat::Off);
        q.fill = filling("PL1", 1, true);
        let more: Vec<Track> = (10..40).map(|i| track(&i.to_string())).collect();
        q.fill_from("PL1", 1, more, true, true).unwrap();
        assert_eq!(ids(&q)[..4], refs[..4]);
        let mut rest: Vec<&str> = ids(&q)[4..].to_vec();
        assert_ne!(rest, refs[4..]);
        rest.sort_by_key(|s| s.parse::<u32>().unwrap());
        assert_eq!(rest, refs[4..]);
    }

    #[test]
    fn a_saved_session_keeps_the_songs_around_the_current_one() {
        let songs: Vec<String> = (0..300).map(|i| i.to_string()).collect();
        let refs: Vec<&str> = songs.iter().map(String::as_str).collect();
        let q = queue(&refs, 120, Repeat::Off);
        let (saved, i) = q.saved().unwrap();
        assert_eq!((saved.len(), i), (200, 50));
        assert_eq!(saved[i].id, "120");
        let q = queue(&refs[..10], 3, Repeat::Off);
        let (saved, i) = q.saved().unwrap();
        assert_eq!((saved.len(), i), (10, 3));
        assert!(Queue::new(Repeat::Off).saved().is_none());
    }
}
