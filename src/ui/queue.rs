//! The play queue: its songs, the one playing, repeat, shuffle, and the songs of a
//! playlist still coming in. Only the state: the app starts the songs these pick, prefetches,
//! and asks for radio.

use crate::model::Track;

#[derive(Clone, Copy, PartialEq)]
pub enum Repeat {
    Off,
    All,
    One,
}

impl Repeat {
    /// Where a Repeat button goes next: off, all, one, off.
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }
}

/// A playlist's load whose next pages also go to the queue (mixed into what's still to
/// come while shuffle is on). `ran_out`: playback reached the end of what had come, and
/// carries on when more does (or the load ends).
#[derive(Clone)]
pub struct QueueFill {
    pub id: String,
    pub load: u64,
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
    /// Songs that joined after it started (autoplay's, Play next, Add to queue): while one
    /// plays, it isn't the album or playlist it was started from that's playing.
    elsewhere: std::collections::HashSet<String>,
    /// The song playing (or paused, or loading).
    pub index: Option<usize>,
    pub repeat: Repeat,
    /// Shuffle is on: the songs' ids in the order they had, to go back to when it's off.
    /// A new queue starts with it off.
    unshuffled: Option<Vec<String>>,
    /// The queue is a playlist played while its songs were still coming in: the rest
    /// join the queue as they arrive.
    pub fill: Option<QueueFill>,
    /// Explicit songs are hidden: none join, and a queue started with some leaves them out.
    pub hide_explicit: bool,
}

/// The queue as it was before a removal or a clear, for Undo: it comes back only onto
/// the queue that change left, untouched since.
pub struct Saved {
    tracks: Vec<Track>,
    elsewhere: std::collections::HashSet<String>,
    index: Option<usize>,
    fill: Option<QueueFill>,
    /// Shuffle as it was: the order to go back to.
    unshuffled: Option<Vec<String>>,
    /// What the change left: its songs and the one playing.
    left: (Vec<String>, Option<usize>),
    /// Pages of the playlist it was filling from that came in since (and whether more
    /// were to come): they join it as it comes back.
    pages: Vec<(Vec<Track>, bool)>,
}

/// How many songs before and after the current one a saved session keeps: liked-songs
/// queues can run to 1000+ songs.
const SAVED_BEFORE: usize = 50;
const SAVED_AFTER: usize = 150;

impl Queue {
    pub fn new(repeat: Repeat) -> Self {
        Self { tracks: Vec::new(), elsewhere: Default::default(), index: None, repeat, unshuffled: None, fill: None, hide_explicit: false }
    }

    pub fn shuffled(&self) -> bool {
        self.unshuffled.is_some()
    }

    /// Shuffle off, leaving the songs as they are.
    pub fn clear_shuffle(&mut self) {
        self.unshuffled = None;
    }

    pub fn current(&self) -> Option<&Track> {
        self.index.and_then(|i| self.tracks.get(i))
    }

    /// The song playing joined after the queue started, rather than being what it was
    /// started with.
    pub fn plays_elsewhere(&self) -> bool {
        self.current().is_some_and(|t| self.elsewhere.contains(&t.id))
    }

    fn note_added(&mut self, id: &str) {
        if !self.tracks.iter().any(|t| t.id == id) {
            self.elsewhere.insert(id.to_owned());
        }
    }

    /// The song playing is the last one.
    pub fn at_end(&self) -> bool {
        self.index.is_some_and(|i| i + 1 >= self.tracks.len())
    }

    /// Play `tracks` from `start`; `fill`: a playlist still coming in, whose other songs
    /// follow. False (and nothing changes) when there are none. With explicit songs hidden
    /// they're left out, and an explicit `start` plays the next song that isn't.
    pub fn replace(&mut self, mut tracks: Vec<Track>, mut start: usize, fill: Option<QueueFill>) -> bool {
        if self.hide_explicit {
            start = tracks.iter().take(start).filter(|t| !t.explicit).count();
            tracks.retain(|t| !t.explicit);
        }
        if tracks.is_empty() {
            return false;
        }
        self.fill = fill;
        self.index = Some(start.min(tracks.len() - 1));
        self.tracks = tracks;
        self.elsewhere.clear();
        self.unshuffled = None;
        true
    }

    /// Only the song playing is left, for a radio from it to follow, without it starting
    /// over. Unlike `replace` it stays when it's an explicit one left to finish.
    pub fn keep_current(&mut self) -> Option<Track> {
        let cur = self.current()?.clone();
        self.tracks = vec![cur.clone()];
        self.index = Some(0);
        self.elsewhere.clear();
        self.unshuffled = None;
        self.fill = None;
        Some(cur)
    }

    /// Play `tracks` shuffled, with shuffle on (see `shuffle_to_play`): false as `replace`.
    pub fn replace_shuffled(&mut self, mut tracks: Vec<Track>, fill: Option<QueueFill>) -> bool {
        let order = tracks.iter().map(|t| t.id.clone()).collect();
        super::shuffle_to_play(&mut tracks, self);
        let played = self.replace(tracks, 0, fill);
        if played {
            self.unshuffled = Some(order);
        }
        played
    }

    /// Take up a queue from elsewhere (last session, another device), at `index`, which
    /// the caller has checked is one of `tracks`.
    pub fn adopt(&mut self, tracks: Vec<Track>, index: usize) {
        self.tracks = tracks;
        self.elsewhere.clear();
        self.index = Some(index);
        self.fill = None;
        self.unshuffled = None;
        self.drop_explicit();
    }

    /// With explicit songs hidden, take them out, all but the one playing.
    pub fn drop_explicit(&mut self) {
        if !self.hide_explicit {
            return;
        }
        let mut i = 0;
        let (cur, mut index) = (self.index, self.index);
        self.tracks.retain(|t| {
            let keep = !t.explicit || Some(i) == cur;
            if !keep && cur.is_some_and(|c| i < c) {
                index = index.map(|c| c - 1);
            }
            i += 1;
            keep
        });
        self.index = index;
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
        // An explicit song left to finish when those were hidden goes once it has.
        self.drop_explicit();
        !self.current().is_some_and(|t| self.hide_explicit && t.explicit)
    }

    /// Back to the song before: false at the first.
    pub fn back(&mut self) -> bool {
        match self.index.filter(|&i| i > 0) {
            Some(i) => {
                self.index = Some(i - 1);
                self.drop_explicit();
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

    /// Shuffle on: the song playing first, then all the others shuffled, those already
    /// played too (and what of a playlist is still coming in, as it comes). Off: the songs
    /// back in the order they had, going on from the song playing (see `unshuffle`).
    pub fn toggle_shuffle(&mut self) {
        if let Some(order) = self.unshuffled.take() {
            return self.unshuffle(&order);
        }
        // Nothing to shuffle: the next queue would turn it off anyway.
        if self.tracks.is_empty() {
            return;
        }
        self.unshuffled = Some(self.tracks.iter().map(|t| t.id.clone()).collect());
        let cur = self.index.filter(|&i| i < self.tracks.len());
        if let Some(c) = cur {
            self.tracks[..=c].rotate_right(1);
            self.index = Some(0);
        }
        let (done, rest) = self.tracks.split_at_mut(usize::from(cur.is_some()));
        super::shuffle(rest, done);
    }

    /// Put the songs in `order` (ids, of the songs shuffle started with and those that came
    /// in since), the song playing in its place there, so what comes after it plays next.
    /// Songs that joined while shuffled (Play next, Add to queue, autoplay) stay beside it:
    /// those that were to come right after it, those that played right before.
    fn unshuffle(&mut self, order: &[String]) {
        // Nothing playing: the songs in their order, any that joined last.
        let cur = self.index.filter(|&i| i < self.tracks.len());
        // Each song's place in `order`, a song in it twice taking its places in turn.
        let mut places: std::collections::HashMap<&str, std::collections::VecDeque<usize>> = Default::default();
        for (i, id) in order.iter().enumerate() {
            places.entry(id).or_default().push_back(i);
        }
        let place: Vec<Option<usize>> = self.tracks.iter().map(|t| places.get_mut(t.id.as_str()).and_then(|p| p.pop_front())).collect();
        // Where the song playing goes: its place, or (one that joined) after the nearest
        // song before it that has one.
        let at = match cur {
            None => (order.len(), 2),
            Some(cur) => match place[cur] {
                Some(p) => (p, 1),
                None => place[..cur].iter().rev().flatten().next().map_or((0, 0), |&p| (p, 2)),
            },
        };
        let key = |i: usize| match place[i] {
            Some(p) => (p, 1),
            None if at.1 != 1 => at,
            None => (at.0, if Some(i) < cur { 0 } else { 2 }),
        };
        // Stable: songs with one key keep the order they had.
        let mut new: Vec<usize> = (0..self.tracks.len()).collect();
        new.sort_by_key(|&i| key(i));
        self.index = cur.and_then(|cur| new.iter().position(|&i| i == cur));
        let mut old: Vec<Track> = self.tracks.iter_mut().map(std::mem::take).collect();
        self.tracks = new.into_iter().map(|i| std::mem::take(&mut old[i])).collect();
    }

    /// Play `t` after the current song: true when nothing was playing, so it starts now.
    pub fn play_next(&mut self, t: Track) -> bool {
        let at = self.index.map_or(0, |i| i + 1).min(self.tracks.len());
        self.note_added(&t.id);
        self.tracks.insert(at, t);
        if self.index.is_some() {
            return false;
        }
        self.index = Some(0);
        true
    }

    /// Add `t` at the end: true when nothing was playing, so it starts now.
    pub fn enqueue(&mut self, t: Track) -> bool {
        self.note_added(&t.id);
        self.tracks.push(t);
        if self.index.is_some() {
            return false;
        }
        self.index = Some(self.tracks.len() - 1);
        true
    }

    /// Take song `i` out, unless it's the one playing: the queue before, for Undo.
    pub fn remove(&mut self, i: usize) -> Option<Saved> {
        if i >= self.tracks.len() || Some(i) == self.index {
            return None;
        }
        let before = self.save();
        self.tracks.remove(i);
        if let Some(cur) = self.index.filter(|&c| c > i) {
            self.index = Some(cur - 1);
        }
        Some(self.left(before))
    }

    /// Everything but the song playing goes, the songs played before it too, and a
    /// playlist still coming in stops joining: the queue before, for Undo. `None` when
    /// there's nothing to take out.
    pub fn clear(&mut self) -> Option<Saved> {
        let keep = self.current().cloned();
        if self.tracks.len() <= usize::from(keep.is_some()) && self.fill.is_none() {
            return None;
        }
        let before = self.save();
        self.tracks = keep.into_iter().collect();
        self.index = self.index.map(|_| 0);
        self.fill = None;
        Some(self.left(before))
    }

    /// The queue as it was before a change, if nothing has changed it since: `None`
    /// (and nothing changes) otherwise. True when playback was waiting at the end for more
    /// and now has a next song to go on to.
    /// `playing_it`: what's playing is the playlist it was filling from.
    pub fn restore(&mut self, s: Saved, playing_it: impl Fn(&str) -> bool) -> Option<bool> {
        if self.fingerprint() != s.left {
            return None;
        }
        let waiting = self.fill.as_ref().is_some_and(|f| f.ran_out);
        (self.tracks, self.elsewhere, self.index, self.fill, self.unshuffled) = (s.tracks, s.elsewhere, s.index, s.fill, s.unshuffled);
        for (tracks, more) in s.pages {
            let Some(f) = &self.fill else { break };
            let (id, load) = (f.id.clone(), f.load);
            self.fill_from(&id, load, tracks, more, playing_it(&id));
        }
        if !waiting {
            return Some(false);
        }
        if self.at_end() && let Some(f) = &mut self.fill {
            f.ran_out = true;
        }
        Some(!self.at_end())
    }

    /// [`Self::fill_from`], with `saved` (the queue before a change, for Undo) getting
    /// the page too when it was filling from that load. Joining this queue then isn't a
    /// change that keeps it from coming back.
    pub fn fill_saved(&mut self, saved: Option<&mut Saved>, id: &str, load: u64, tracks: Vec<Track>, more: bool, playing_it: bool) -> Option<Filled> {
        let Some(s) = saved.filter(|s| s.fill.as_ref().is_some_and(|f| f.id == id && f.load == load)) else {
            return self.fill_from(id, load, tracks, more, playing_it);
        };
        let untouched = self.fingerprint() == s.left;
        s.pages.push((tracks.clone(), more));
        let filled = self.fill_from(id, load, tracks, more, playing_it);
        if untouched {
            s.left = self.fingerprint();
        }
        filled
    }

    fn save(&self) -> Saved {
        Saved { tracks: self.tracks.clone(), elsewhere: self.elsewhere.clone(), index: self.index, fill: self.fill.clone(), unshuffled: self.unshuffled.clone(), left: Default::default(), pages: Vec::new() }
    }

    fn left(&self, mut before: Saved) -> Saved {
        before.left = self.fingerprint();
        before
    }

    fn fingerprint(&self) -> (Vec<String>, Option<usize>) {
        (self.tracks.iter().map(|t| t.id.clone()).collect(), self.index)
    }

    /// Move song `from` to the gap `gap` between songs (0 is above the first), both counted
    /// before the move, as YouTube Music's own queue does: a song already played can come
    /// back after the one playing, and one to come can go before it. The song playing goes
    /// on playing wherever it ends up. False when it wouldn't change anything.
    pub fn move_track(&mut self, from: usize, gap: usize) -> bool {
        if from >= self.tracks.len() || gap > self.tracks.len() || gap == from || gap == from + 1 {
            return false;
        }
        let t = self.tracks.remove(from);
        let to = if gap > from { gap - 1 } else { gap };
        self.tracks.insert(to, t);
        self.index = self.index.map(|i| match i {
            _ if i == from => to,
            _ if from < i && to >= i => i - 1,
            _ if from > i && to <= i => i + 1,
            _ => i,
        });
        true
    }

    /// Put `t`, dragged in from another list, at the gap `gap` between songs (0 is above the
    /// first). The song playing goes on playing.
    pub fn insert_at(&mut self, gap: usize, t: Track) {
        let gap = gap.min(self.tracks.len());
        self.note_added(&t.id);
        self.tracks.insert(gap, t);
        if let Some(i) = &mut self.index
            && gap <= *i
        {
            *i += 1;
        }
    }

    /// Radio songs for `seed` join the end, without the seed or songs already queued.
    /// Returns whether the song playing was the last one.
    pub fn add_radio(&mut self, seed: &str, tracks: Vec<Track>) -> bool {
        let have: std::collections::HashSet<String> = self.tracks.iter().map(|t| t.id.clone()).collect();
        let was_at_end = self.at_end();
        let added: Vec<Track> = tracks.into_iter().filter(|t| t.id != seed && !have.contains(&t.id) && !(self.hide_explicit && t.explicit)).collect();
        self.elsewhere.extend(added.iter().map(|t| t.id.clone()));
        self.tracks.extend(added);
        was_at_end
    }

    /// The next page of the playlist the queue fills from (`id`, `load`) joins it; `more`:
    /// pages still follow. `playing_it`: the queue still plays that playlist. `None` when
    /// the page doesn't apply (though the last one still ends the fill); an empty last
    /// page that does still says whether playback was waiting for it.
    pub fn fill_from(&mut self, id: &str, load: u64, tracks: Vec<Track>, more: bool, playing_it: bool) -> Option<Filled> {
        let fill = self.fill.as_mut().filter(|f| f.id == id && f.load == load)?;
        let ran_out = std::mem::take(&mut fill.ran_out);
        if !more {
            self.fill = None;
        }
        if !playing_it {
            return None;
        }
        let was_at_end = self.at_end();
        let hide = self.hide_explicit;
        if let Some(order) = &mut self.unshuffled {
            order.extend(tracks.iter().map(|t| t.id.clone()));
        }
        self.tracks.extend(tracks.into_iter().filter(|t| !(hide && t.explicit)));
        if self.unshuffled.is_some() {
            // Mixed into what's still to come, past the next song (it may be loading already).
            let from = self.index.map_or(0, |i| i + if was_at_end { 1 } else { 2 }).min(self.tracks.len());
            let (done, rest) = self.tracks.split_at_mut(from);
            super::shuffle(rest, done);
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

    #[test]
    fn autoplay_and_added_songs_arent_what_the_queue_was_started_from() {
        let mut q = Queue::new(Repeat::Off);
        q.replace(vec![track("a"), track("b")], 0, None);
        assert!(!q.plays_elsewhere());
        q.play_next(track("next"));
        q.enqueue(track("last"));
        q.add_radio("b", vec![track("radio"), track("a")]);
        let ids: Vec<&str> = q.tracks.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["a", "next", "b", "last", "radio"]);
        for (i, elsewhere) in [false, true, false, true, true].into_iter().enumerate() {
            q.index = Some(i);
            assert_eq!(q.plays_elsewhere(), elsewhere, "{}", ids[i]);
        }
        // Back to a song of its own, and a new queue forgets them.
        assert!(q.back() && q.back());
        assert!(!q.plays_elsewhere());
        q.replace(vec![track("radio")], 0, None);
        assert!(!q.plays_elsewhere());
    }

    #[test]
    fn a_song_already_queued_stays_part_of_it_when_added_again() {
        let mut q = Queue::new(Repeat::Off);
        q.replace(vec![track("a"), track("b")], 0, None);
        q.enqueue(track("b"));
        q.index = Some(1);
        assert!(!q.plays_elsewhere());
    }

    #[test]
    fn hidden_explicit_songs_stay_out_of_the_queue() {
        let e = |id: &str| Track { explicit: true, ..track(id) };
        let mut q = Queue::new(Repeat::Off);
        q.hide_explicit = true;
        // Started at an explicit song: the next clean one plays, or else the last.
        assert!(q.replace(vec![e("a"), track("b"), e("c"), track("d"), e("f")], 2, None));
        assert_eq!((ids(&q), q.current().unwrap().id.as_str()), (vec!["b", "d"], "d"));
        assert!(q.replace(vec![track("b"), e("c")], 1, None));
        assert_eq!(q.current().unwrap().id, "b");
        assert!(!q.replace(vec![e("c")], 0, None));
        // Radio and a playlist's next pages leave them out too.
        q.add_radio("b", vec![e("r1"), track("r2")]);
        q.fill = filling("PL1", 1);
        q.fill_from("PL1", 1, vec![e("p1"), track("p2")], false, true);
        assert_eq!(ids(&q), ["b", "r2", "p2"]);
        // Turned on with some queued: all go but the one playing.
        let mut q = queue(&["a", "b", "c", "d"], 2, Repeat::Off);
        for i in [0, 2, 3] {
            q.tracks[i].explicit = true;
        }
        q.drop_explicit();
        assert_eq!(ids(&q).len(), 4);
        q.hide_explicit = true;
        q.drop_explicit();
        assert_eq!((ids(&q), q.index), (vec!["b", "c"], Some(1)));
        // It goes once playback moves on, and Repeat All doesn't come back to it.
        q.repeat = Repeat::All;
        assert!(q.advance());
        assert_eq!((ids(&q), q.index), (vec!["b"], Some(0)));
        let mut q = queue(&["a"], 0, Repeat::All);
        q.tracks[0].explicit = true;
        q.hide_explicit = true;
        assert!(!q.advance());
    }

    fn track(id: &str) -> Track {
        Track { id: id.into(), title: id.into(), artists: vec![], ..Default::default() }
    }

    fn queue(songs: &[&str], index: usize, repeat: Repeat) -> Queue {
        let mut q = Queue::new(repeat);
        assert!(q.replace(songs.iter().map(|s| track(s)).collect(), index, None));
        q
    }

    fn ids(q: &Queue) -> Vec<&str> {
        q.tracks.iter().map(|t| t.id.as_str()).collect()
    }

    fn filling(id: &str, load: u64) -> Option<QueueFill> {
        Some(QueueFill { id: id.into(), load, ran_out: false })
    }

    #[test]
    fn songs_to_come_move_between_others() {
        let mut q = queue(&["a", "b", "c", "d", "e"], 1, Repeat::Off);
        // Down: the gap above "e", counted before the move.
        assert!(q.move_track(2, 4));
        assert_eq!(ids(&q), ["a", "b", "d", "c", "e"]);
        // Up: right after the song playing.
        assert!(q.move_track(4, 2));
        assert_eq!(ids(&q), ["a", "b", "e", "d", "c"]);
        // To the very end.
        assert!(q.move_track(2, 5));
        assert_eq!(ids(&q), ["a", "b", "d", "c", "e"]);
        assert_eq!(q.index, Some(1));
    }

    #[test]
    fn songs_played_and_to_come_move_round_the_song_playing() {
        // A song already played comes back right after the one playing.
        let mut q = queue(&["a", "b", "c", "d"], 2, Repeat::Off);
        assert!(q.move_track(0, 3));
        assert_eq!(ids(&q), ["b", "c", "a", "d"]);
        assert_eq!(q.current().unwrap().id, "c");
        // One to come goes before it, among those played.
        assert!(q.move_track(3, 0));
        assert_eq!(ids(&q), ["d", "b", "c", "a"]);
        assert_eq!(q.current().unwrap().id, "c");
        // The song playing itself, down and back up: it goes on playing.
        assert!(q.move_track(2, 4));
        assert_eq!(ids(&q), ["d", "b", "a", "c"]);
        assert_eq!(q.current().unwrap().id, "c");
        assert!(q.move_track(3, 0));
        assert_eq!(ids(&q), ["c", "d", "b", "a"]);
        assert_eq!(q.index, Some(0));
        // Where it already is, and out of range.
        let before = ids(&q).join("");
        assert!(!q.move_track(2, 2));
        assert!(!q.move_track(2, 3));
        assert!(!q.move_track(4, 2));
        assert!(!q.move_track(2, 5));
        assert_eq!(ids(&q).join(""), before);
    }

    #[test]
    fn a_song_dragged_in_goes_where_it_was_let_go() {
        let mut q = queue(&["a", "b", "c"], 1, Repeat::Off);
        // Above the song playing, which goes on playing.
        q.insert_at(0, track("x"));
        assert_eq!(ids(&q), ["x", "a", "b", "c"]);
        assert_eq!(q.current().unwrap().id, "b");
        // Right after it: it plays next.
        q.insert_at(3, track("y"));
        assert_eq!(ids(&q), ["x", "a", "b", "y", "c"]);
        assert_eq!(q.upcoming().unwrap().id, "y");
        // Past the end goes at the end.
        q.insert_at(99, track("z"));
        assert_eq!(ids(&q).last(), Some(&"z"));
        assert_eq!(q.current().unwrap().id, "b");
    }

    #[test]
    fn with_nothing_playing_every_song_moves() {
        let mut q = queue(&["a", "b", "c"], 0, Repeat::Off);
        q.index = None;
        assert!(q.move_track(2, 0));
        assert_eq!(ids(&q), ["c", "a", "b"]);
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
        q.replace(vec![track("a")], 0, filling("PL1", 1));
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
        assert!(q.remove(0).is_some());
        assert_eq!(ids(&q), ["b", "c"]);
        assert_eq!(q.current().unwrap().id, "c");
        assert!(q.remove(1).is_none());
        assert!(q.remove(5).is_none());
        assert_eq!(ids(&q), ["b", "c"]);
    }

    #[test]
    fn undoing_a_removal_brings_back_the_queue_as_it_was() {
        // A song played before the current one: the current one moves up a place.
        let mut q = queue(&["a", "b", "c", "d"], 2, Repeat::Off);
        q.enqueue(track("e"));
        let saved = q.remove(0).unwrap();
        assert_eq!((ids(&q), q.index), (vec!["b", "c", "d", "e"], Some(1)));
        assert!(q.restore(saved, |_| true).is_some());
        assert_eq!((ids(&q), q.index), (vec!["a", "b", "c", "d", "e"], Some(2)));
        // "e" still counts as added, not as what the queue was started from.
        q.index = Some(4);
        assert!(q.plays_elsewhere());
    }

    #[test]
    fn undoing_a_clear_brings_back_what_played_and_whats_to_come() {
        let mut q = queue(&["a", "b", "c", "d"], 1, Repeat::Off);
        q.fill = filling("PL1", 7);
        let saved = q.clear().unwrap();
        assert_eq!((ids(&q), q.index, q.fill.is_none()), (vec!["b"], Some(0), true));
        assert!(q.clear().is_none(), "only the song playing is left");
        assert!(q.restore(saved, |_| true).is_some());
        assert_eq!((ids(&q), q.index), (vec!["a", "b", "c", "d"], Some(1)));
        let fill = q.fill.as_ref().unwrap();
        assert_eq!((fill.id.as_str(), fill.load), ("PL1", 7));
        // With nothing playing, all of it goes.
        q.index = None;
        q.clear().unwrap();
        assert!(q.tracks.is_empty() && q.index.is_none());
        assert!(Queue::new(Repeat::Off).clear().is_none());
    }

    #[test]
    fn pages_that_come_in_while_an_undo_waits_join_the_queue_it_brings_back() {
        // Cleared: the page goes nowhere now, and joins the queue back from Undo.
        let mut q = queue(&["a", "b"], 0, Repeat::Off);
        q.fill = filling("PL1", 7);
        let mut saved = q.clear().unwrap();
        assert!(q.fill_saved(Some(&mut saved), "PL2", 7, vec![track("x")], true, true).is_none());
        assert!(q.fill_saved(Some(&mut saved), "PL1", 7, vec![track("c")], true, true).is_none());
        assert!(q.restore(saved, |_| true).is_some());
        assert_eq!(ids(&q), ["a", "b", "c"]);
        assert!(q.fill.is_some(), "more is to come");
        // Taken out of: the page joins now, and isn't a change that stops the Undo.
        let mut saved = q.remove(1).unwrap();
        q.fill_saved(Some(&mut saved), "PL1", 7, vec![track("d")], false, true).unwrap();
        assert_eq!(ids(&q), ["a", "c", "d"]);
        assert!(q.restore(saved, |_| true).is_some());
        assert_eq!(ids(&q), ["a", "b", "c", "d"]);
        assert!(q.fill.is_none(), "that was the last page");
    }

    #[test]
    fn playback_waiting_at_the_end_still_waits_or_goes_on_after_an_undo() {
        // The song after it taken out, then playback reached the end and waits: Undo brings
        // that song back to go on to.
        let mut q = queue(&["a", "b", "c"], 1, Repeat::Off);
        q.fill = filling("PL1", 7);
        let saved = q.remove(2).unwrap();
        assert!(!q.advance());
        assert_eq!(q.restore(saved, |_| true), Some(true));
        assert_eq!(ids(&q), ["a", "b", "c"]);
        // An earlier song taken out: still at the end, so it waits on for the next page.
        let mut q = queue(&["a", "b", "c"], 2, Repeat::Off);
        q.fill = filling("PL1", 7);
        let saved = q.remove(0).unwrap();
        assert!(!q.advance());
        assert_eq!(q.restore(saved, |_| true), Some(false));
        let f = q.fill_from("PL1", 7, vec![track("d")], false, true).unwrap();
        assert!(f.ran_out);
    }

    #[test]
    fn undoing_a_clear_brings_back_shuffle_as_it_was() {
        let songs = numbered(20);
        let refs: Vec<&str> = songs.iter().map(String::as_str).collect();
        let mut q = queue(&refs, 0, Repeat::Off);
        q.toggle_shuffle();
        let shuffled = ids(&q).iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let saved = q.clear().unwrap();
        // Off and on again with only the song playing left: the same songs, still undoable.
        q.toggle_shuffle();
        q.toggle_shuffle();
        assert!(q.restore(saved, |_| true).is_some());
        assert!(q.shuffled());
        assert_eq!(ids(&q), shuffled);
        q.toggle_shuffle();
        assert_eq!(ids(&q), refs);
    }

    #[test]
    fn a_queue_changed_since_isnt_undone() {
        let mut q = queue(&["a", "b", "c"], 0, Repeat::Off);
        let saved = q.remove(2).unwrap();
        q.enqueue(track("x"));
        assert!(q.restore(saved, |_| true).is_none());
        assert_eq!(ids(&q), ["a", "b", "x"]);
        // Playback moving on counts too: the song playing would be another.
        let saved = q.clear().unwrap();
        q.enqueue(track("y"));
        q.advance();
        assert!(q.restore(saved, |_| true).is_none());
        assert_eq!((ids(&q), q.index), (vec!["a", "y"], Some(1)));
    }

    #[test]
    fn radio_adds_only_new_songs() {
        let mut q = queue(&["seed", "a"], 1, Repeat::Off);
        assert!(q.add_radio("seed", vec![track("seed"), track("a"), track("b"), track("c")]));
        assert_eq!(ids(&q), ["seed", "a", "b", "c"]);
        assert!(!q.add_radio("seed", vec![track("d")]));
    }

    #[test]
    fn radio_from_the_song_playing_keeps_it() {
        let mut q = queue(&["a", "b", "c"], 1, Repeat::Off);
        q.fill = filling("PL1", 1);
        q.unshuffled = Some(vec!["c".into(), "b".into(), "a".into()]);
        assert_eq!(q.keep_current().unwrap().id, "b");
        assert_eq!((ids(&q), q.index), (vec!["b"], Some(0)));
        assert!(q.fill.is_none() && !q.shuffled());
        // Its radio follows it, and a later one adds only what's new.
        assert!(q.add_radio("b", vec![track("b"), track("x"), track("y")]));
        assert_eq!(ids(&q), ["b", "x", "y"]);
        assert_eq!(q.current().unwrap().id, "b");

        // An explicit song left to finish once those were hidden stays too.
        let mut q = queue(&["a", "e"], 1, Repeat::Off);
        q.tracks[1].explicit = true;
        q.hide_explicit = true;
        assert_eq!(q.keep_current().unwrap().id, "e");
        assert_eq!(ids(&q), ["e"]);
        assert!(Queue::new(Repeat::Off).keep_current().is_none());
    }

    #[test]
    fn a_playlist_still_coming_in_fills_the_queue() {
        let mut q = Queue::new(Repeat::Off);
        q.replace(vec![track("a"), track("b")], 0, filling("PL1", 3));
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
        q.replace(vec![track("a"), track("b")], 1, filling("PL1", 1));
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
        q.replace(vec![track("a")], 0, filling("PL1", 1));
        // The queue plays something else now: the page doesn't join, and the last one ends the fill.
        assert!(q.fill_from("PL1", 1, vec![track("b")], true, false).is_none());
        assert!(q.fill.is_some());
        assert!(q.fill_from("PL1", 1, vec![track("c")], false, false).is_none());
        assert!(q.fill.is_none());
        assert_eq!(ids(&q), ["a"]);
        // A song starting means it no longer waits.
        q.replace(vec![track("a")], 0, filling("PL1", 2));
        q.advance();
        q.started();
        assert!(!q.fill.as_ref().unwrap().ran_out);
    }

    fn numbered(n: usize) -> Vec<String> {
        (0..n).map(|i| i.to_string()).collect()
    }

    #[test]
    fn shuffle_on_puts_the_song_playing_first_and_off_puts_the_order_back() {
        let songs = numbered(40);
        let refs: Vec<&str> = songs.iter().map(String::as_str).collect();
        let mut q = queue(&refs, 5, Repeat::Off);
        q.toggle_shuffle();
        assert!(q.shuffled());
        // The song playing first, then all the others, those played too.
        assert_eq!((ids(&q)[0], q.index), ("5", Some(0)));
        let mut rest: Vec<&str> = ids(&q)[1..].to_vec();
        let others: Vec<&str> = refs.iter().copied().filter(|&s| s != "5").collect();
        // 39 songs coming back in their order by chance is too unlikely to happen.
        assert_ne!(rest, others);
        rest.sort_by_key(|s| s.parse::<u32>().unwrap());
        assert_eq!(rest, others);
        // A few shuffled songs later, off goes on from the song playing in its own place.
        q.advance();
        q.advance();
        let playing = q.current().unwrap().id.clone();
        q.toggle_shuffle();
        assert!(!q.shuffled());
        assert_eq!(ids(&q), refs);
        assert_eq!(q.current().unwrap().id, playing);
        // With nothing playing, all of it shuffles and comes back.
        q.index = None;
        q.toggle_shuffle();
        assert_ne!(ids(&q), refs);
        q.toggle_shuffle();
        assert_eq!((ids(&q), q.index), (refs.clone(), None));
        // An empty queue has nothing to shuffle.
        let mut empty = Queue::new(Repeat::Off);
        empty.toggle_shuffle();
        assert!(!empty.shuffled());
        // A new queue starts with it off.
        q.toggle_shuffle();
        q.replace(vec![track("a")], 0, None);
        assert!(!q.shuffled());
    }

    #[test]
    fn songs_added_while_shuffled_stay_beside_the_song_playing() {
        let mut q = queue(&["a", "b", "c", "d", "e"], 0, Repeat::Off);
        q.toggle_shuffle();
        // "x" played between two of the shuffled songs, "y" is up next, "z" last.
        let at = q.index.unwrap() + 1;
        q.tracks.insert(at, track("x"));
        q.advance();
        q.advance();
        q.play_next(track("y"));
        q.enqueue(track("z"));
        let playing = q.current().unwrap().id.clone();
        q.toggle_shuffle();
        let order = ids(&q);
        let i = q.index.unwrap();
        assert_eq!(order[i], playing);
        assert_eq!(order[i - 1], "x");
        assert_eq!(order[i + 1..i + 3], ["y", "z"]);
        let theirs: Vec<&str> = order.iter().copied().filter(|s| !["x", "y", "z"].contains(s)).collect();
        assert_eq!(theirs, ["a", "b", "c", "d", "e"]);
        // A song that joined, playing: after the nearest one before it that didn't.
        let mut q = queue(&["a", "b", "c"], 0, Repeat::Off);
        q.toggle_shuffle();
        let before = q.tracks[2].id.clone();
        q.enqueue(track("z"));
        q.index = Some(3);
        q.toggle_shuffle();
        let order = ids(&q);
        let i = q.index.unwrap();
        assert_eq!(order[i], "z");
        assert_eq!(order[i - 1], before);
    }

    #[test]
    fn a_shuffled_playlists_next_pages_mix_in_and_come_back_in_order() {
        let songs = numbered(40);
        let refs: Vec<&str> = songs.iter().map(String::as_str).collect();
        let mut q = Queue::new(Repeat::Off);
        assert!(q.replace_shuffled(refs[..10].iter().map(|s| track(s)).collect(), filling("PL1", 1)));
        q.advance();
        q.advance();
        // The song after the current one stays: it may be loading already.
        let first: Vec<String> = ids(&q)[..4].iter().map(|s| s.to_string()).collect();
        let more: Vec<Track> = (10..40).map(|i| track(&i.to_string())).collect();
        q.fill_from("PL1", 1, more, true, true).unwrap();
        assert_eq!(ids(&q)[..4], first);
        assert_ne!(ids(&q)[4..], refs[4..]);
        let mut all = ids(&q);
        all.sort_by_key(|s| s.parse::<u32>().unwrap());
        assert_eq!(all, refs);
        q.toggle_shuffle();
        assert_eq!(ids(&q), refs);
        // Pages after it's off join in order.
        q.fill_from("PL1", 1, vec![track("40")], false, true).unwrap();
        assert_eq!(ids(&q).last(), Some(&"40"));
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
