//! Albums and playlists: the copies kept for going back to, the loads under way, and
//! the edits made here that YouTube Music doesn't list yet. Only the state: the app
//! sends the requests these ask for and shows the pages.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::backend::{Collection, PlaylistSort};
use crate::model::Track;

/// Liked Music's playlist id.
pub const LIKED_MUSIC: &str = "LM";

/// Going back to a kept album or playlist reloads it when it's older than this. Edits
/// made here show at once anyway, and a Spotify transfer reloads Liked Music when done.
const REFRESH_EVERY: Duration = Duration::from_secs(5 * 60);

/// How many albums and playlists stay kept. The page on screen, what's playing, one
/// still loading and Liked Music aren't the ones dropped.
const KEEP_OPENED: usize = 8;

/// An album or playlist page kept for going back to.
pub struct Opened {
    pub c: Collection,
    /// The load it came from; only that load's next pages are added.
    pub load: u64,
    /// When that load came in.
    at: Instant,
    /// All of it came. A load that failed partway keeps what came, which isn't the whole
    /// playlist: "the end" of it isn't the playlist's.
    pub complete: bool,
}

/// Songs of a playlist dragged to new places, not answered yet. A load now could answer
/// with the order from before them, so loads wait until all are answered.
struct Moves {
    pending: HashSet<u64>,
    /// A load was asked for meanwhile, or a move failed.
    reload: bool,
    last_sent: Instant,
}

/// Past this, an unanswered move no longer holds back loads of its playlist (which
/// load again when its answer does come).
const MOVE_UNANSWERED: Duration = Duration::from_secs(60);

/// An album or playlist load under way (one each at a time). A playlist not kept yet
/// comes in page by page; a kept one is swapped for the new copy once all of it is in.
struct ListLoad {
    load: u64,
    album: bool,
    /// It changed meanwhile, so it's loaded again afterwards.
    stale: bool,
}

/// A load to ask for. `stream`: a playlist with no kept copy comes page by page.
#[derive(Debug, PartialEq)]
pub struct Start {
    pub id: String,
    pub load: u64,
    pub album: bool,
    pub stream: bool,
}

/// How a load ended.
#[derive(Debug, PartialEq)]
pub enum Done {
    Kept,
    /// It changed while loading: load it again.
    Reload(Start),
    /// Nothing of it is kept: it failed before anything came (or was dropped for room
    /// as it ended).
    NotKept,
}

/// An album or playlist (or a playlist's first page) that came in.
#[derive(Debug, PartialEq)]
pub enum Arrival {
    /// More pages follow.
    More,
    Done(Done),
}

/// A playlist's next page, as taken.
pub struct Page {
    /// The songs left after the edits made here: they go to the queue too when it's
    /// filling from this load.
    pub tracks: Vec<Track>,
    /// They were added to the kept copy (one being replaced takes them when complete).
    pub kept: bool,
    pub done: Option<Done>,
}

#[derive(Default)]
pub struct Lists {
    /// Most recent first.
    opened: Vec<Opened>,
    /// Numbers the loads. Never reset, so an answer from before `clear` can't pass for a new one.
    loads: u64,
    /// The loads under way: only their answers are taken.
    running: HashMap<String, ListLoad>,
    moves: HashMap<String, Moves>,
    /// Numbers the moves. Never reset, so an answer to an old one can't pass for a new one's.
    move_ids: u64,
    /// Likes and playlist additions and removals made here lately. YouTube Music goes on listing the
    /// old state for a while after it takes an edit, so lists that arrive are corrected with these.
    pub edits: RecentEdits,
}

impl Lists {
    pub fn get(&self, id: &str) -> Option<&Opened> {
        self.opened.iter().find(|o| o.c.id == id)
    }

    pub fn collection_mut(&mut self, id: &str) -> Option<&mut Collection> {
        self.opened.iter_mut().find(|o| o.c.id == id).map(|o| &mut o.c)
    }

    /// A load of it is under way.
    pub fn loading(&self, id: &str) -> bool {
        self.running.contains_key(id)
    }

    /// More of the kept copy is still coming in, page by page.
    pub fn receiving(&self, id: &str) -> bool {
        self.running.get(id).is_some_and(|l| self.get(id).is_some_and(|o| o.load == l.load))
    }

    /// Opening its page: it's the most recent.
    pub fn move_to_front(&mut self, id: &str) {
        if let Some(i) = self.opened.iter().position(|o| o.c.id == id) {
            let o = self.opened.remove(i);
            self.opened.insert(0, o);
        }
    }

    /// Load an album or playlist, unless a load of it is under way. `changed`: if one is,
    /// it's loaded once more afterwards.
    pub fn load(&mut self, id: &str, album: bool, changed: bool) -> Option<Start> {
        if let Some(m) = self.moves.get_mut(id) {
            m.reload = true;
            if m.last_sent.elapsed() < MOVE_UNANSWERED {
                log::debug!("list {id}: load waits for {} moves", m.pending.len());
                return None;
            }
        }
        if let Some(l) = self.running.get_mut(id) {
            l.stale |= changed;
            log::debug!("list {id}: load {} under way, stale={}", l.load, l.stale);
            return None;
        }
        self.loads += 1;
        let load = self.loads;
        log::debug!("list {id}: load {load} (kept copy: {}, changed: {changed})", self.get(id).is_some());
        self.running.insert(id.to_owned(), ListLoad { load, album, stale: false });
        Some(Start { id: id.to_owned(), load, album, stream: !album && self.get(id).is_none() })
    }

    /// A song of the playlist was dragged to a new place and YouTube Music is being told.
    /// Returns the number its answer goes by.
    pub fn move_sent(&mut self, id: &str) -> u64 {
        self.move_ids += 1;
        let m = self.moves.entry(id.to_owned()).or_insert(Moves { pending: HashSet::new(), reload: false, last_sent: Instant::now() });
        m.pending.insert(self.move_ids);
        m.last_sent = Instant::now();
        self.move_ids
    }

    /// Moves of it are still being told to YouTube Music: a load of it waits for them.
    pub fn moving(&self, id: &str) -> bool {
        self.moves.get(id).is_some_and(|m| m.last_sent.elapsed() < MOVE_UNANSWERED)
    }

    /// This move's answer is still awaited.
    pub fn awaiting_move(&self, id: &str, move_id: u64) -> bool {
        self.moves.get(id).is_some_and(|m| m.pending.contains(&move_id))
    }

    /// YouTube Music answered a move. Once all are answered, the load to do if one was
    /// asked for meanwhile or a move failed, for its order back.
    pub fn move_answered(&mut self, id: &str, move_id: u64, ok: bool) -> Option<Start> {
        let m = self.moves.get_mut(id)?;
        if !m.pending.remove(&move_id) {
            return None;
        }
        m.reload |= !ok;
        if !m.pending.is_empty() {
            return None;
        }
        let reload = m.reload;
        self.moves.remove(id);
        if reload { self.load(id, false, true) } else { None }
    }

    /// Opening an album or playlist page: loaded unless a copy from the last few minutes is kept.
    pub fn visit(&mut self, id: &str, album: bool, now: Instant) -> Option<Start> {
        // One that didn't all come in is tried again right away.
        match self.get(id).filter(|o| o.complete).map(|o| now.saturating_duration_since(o.at)) {
            Some(age) if age <= REFRESH_EVERY => {
                log::debug!("list {id}: kept copy is {age:?} old, not reloading");
                None
            }
            _ => self.load(id, album, false),
        }
    }

    /// An album or playlist (or a playlist's first page) came in; `keep`: the lists not to
    /// drop for room (the page on screen, what's playing). Only the load under way counts:
    /// one from before a sign-in change or a deletion is dropped (`None`).
    pub fn arrived(&mut self, load: u64, mut c: Collection, more: bool, keep: &[&str]) -> Option<Arrival> {
        if self.running.get(&c.id).is_none_or(|l| l.load != load) {
            return None;
        }
        let sent = c.tracks.len();
        self.edits.correct_collection(&mut c);
        if !more {
            self.edits.complete_load(&mut c);
        }
        let id = c.id.clone();
        log::debug!("list {id}: load {load} in, {sent} songs sent, {} after local edits, more={more}", c.tracks.len());
        let at = Instant::now();
        let complete = !more;
        match self.opened.iter_mut().find(|o| o.c.id == id) {
            Some(o) => *o = Opened { c, load, at, complete },
            None => self.opened.insert(0, Opened { c, load, at, complete }),
        }
        self.trim(keep);
        if more {
            return Some(Arrival::More);
        }
        self.done(&id, keep).map(Arrival::Done)
    }

    /// A playlist's next page: onto the kept copy when it's from that load. An empty last
    /// one also ends a load that failed. `None` when it isn't from the load under way.
    pub fn page(&mut self, id: &str, load: u64, tracks: Vec<Track>, set_ids: Vec<Option<String>>, more: bool, keep: &[&str]) -> Option<Page> {
        if self.running.get(id).is_none_or(|l| l.load != load) {
            return None;
        }
        self.edits.prune();
        let sent = tracks.len();
        let edits = &self.edits;
        let (mut tracks, mut set_ids): (Vec<Track>, Vec<Option<String>>) = tracks.into_iter().zip(set_ids).filter(|(t, set)| edits.keeps(id, t, set.as_deref())).unzip();
        let kept = match self.opened.iter_mut().find(|o| o.c.id == id && o.load == load) {
            Some(o) => {
                // Liked Music has each song once; one liked here meanwhile is at its top already.
                if id == LIKED_MUSIC {
                    let have: HashSet<&str> = o.c.tracks.iter().map(|x| x.id.as_str()).collect();
                    (tracks, set_ids) = tracks.into_iter().zip(set_ids).filter(|(t, _)| !have.contains(t.id.as_str())).unzip();
                }
                o.c.tracks.extend(tracks.iter().cloned());
                o.c.set_ids.extend(set_ids);
                o.complete = !more;
                if !more {
                    // They go to a queue filling from this load too.
                    tracks.extend(edits.complete_load(&mut o.c));
                }
                log::debug!("list {id}: load {load} page, {sent} sent, {} added, {} total, more={more}", tracks.len(), o.c.tracks.len());
                true
            }
            None => {
                log::debug!("list {id}: load {load} page of {sent} for no kept copy (replaced when complete)");
                false
            }
        };
        let done = if more { None } else { self.done(id, keep) };
        Some(Page { tracks, kept, done })
    }

    /// A load failed partway: its copy isn't all of the playlist, even though it ended.
    pub fn load_failed(&mut self, id: &str, load: u64) {
        if let Some(o) = self.opened.iter_mut().find(|o| o.c.id == id && o.load == load) {
            o.complete = false;
        }
    }

    /// A load ended: load it again if it changed meanwhile.
    fn done(&mut self, id: &str, keep: &[&str]) -> Option<Done> {
        let l = self.running.remove(id)?;
        self.trim(keep);
        Some(match (self.get(id).is_some(), l.stale) {
            (false, _) => Done::NotKept,
            (true, true) => Done::Reload(self.load(id, l.album, false)?),
            (true, false) => Done::Kept,
        })
    }

    /// Drop the oldest kept albums and playlists beyond `KEEP_OPENED`, but not those in
    /// `keep`, one loading or Liked Music.
    fn trim(&mut self, keep: &[&str]) {
        let mut i = self.opened.len();
        while self.opened.len() > KEEP_OPENED && i > 0 {
            i -= 1;
            let id = self.opened[i].c.id.as_str();
            if !keep.contains(&id) && id != LIKED_MUSIC && !self.running.contains_key(id) {
                self.opened.remove(i);
            }
        }
    }

    /// Likes made here show at once in Liked Music as kept. (A whole reload asked for just
    /// before can still come back without them.)
    pub fn set_liked(&mut self, tracks: &[Track], like: bool) {
        for t in tracks {
            self.edits.like(&t.id, like, Some(t.clone()));
        }
        let Some(c) = self.collection_mut(LIKED_MUSIC) else { return };
        for t in tracks.iter().rev() {
            match (like, c.tracks.iter().position(|x| x.id == t.id)) {
                (true, None) => {
                    c.tracks.insert(0, t.clone());
                    c.set_ids.insert(0, None);
                    c.count = c.count.map(|n| n + 1);
                }
                (false, Some(i)) => c.remove_rows(&[i]),
                _ => {}
            }
        }
    }

    /// Songs added here show at once at the end of the playlist as kept, and in loads that
    /// don't list them yet.
    pub fn added(&mut self, playlist: &str, tracks: &[Track]) {
        self.edits.add(playlist, tracks);
        if let Some(o) = self.opened.iter_mut().find(|o| o.c.id == playlist && o.complete) {
            self.edits.complete(&mut o.c);
        }
    }

    /// Songs shown as added that YouTube Music didn't take: out of the kept copy too.
    pub fn add_failed(&mut self, playlist: &str, video_ids: &[String]) {
        let gone = self.edits.add_failed(playlist, video_ids);
        if let Some(o) = self.opened.iter_mut().find(|o| o.c.id == playlist) {
            // Its rows that YouTube Music didn't list (no setVideoId).
            let rows: Vec<usize> = (0..o.c.tracks.len()).filter(|&i| !o.c.set_ids.get(i).is_some_and(Option::is_some) && gone.contains(&o.c.tracks[i].id)).collect();
            o.c.remove_rows(&rows);
        }
    }

    /// Kept, or loading.
    pub fn known(&self, id: &str) -> bool {
        self.get(id).is_some() || self.loading(id)
    }

    /// A deleted playlist: its copy and its load go.
    pub fn forget(&mut self, id: &str) {
        self.opened.retain(|o| o.c.id != id);
        self.running.remove(id);
        self.moves.remove(id);
    }

    /// Another account: everything goes but the edits.
    pub fn clear(&mut self) {
        self.opened.clear();
        self.running.clear();
        self.moves.clear();
    }
}

/// How long YouTube Music may go on listing the old state after taking an edit.
const EDIT_LAG: Duration = Duration::from_secs(180);
/// How long a removal or an addition counts while YouTube Music hasn't answered (one it
/// refused shows only as an error), and likes, which a few hundred at a time take a while.
const REMOVAL_UNANSWERED: Duration = Duration::from_secs(60);
const LIKE_UNANSWERED: Duration = Duration::from_secs(600);

#[derive(Default)]
pub struct RecentEdits {
    /// Song: liked or not, the song when known (to put back a like the list lacks yet), and when.
    likes: HashMap<String, (bool, Option<Track>, EditAge)>,
    /// Playlist: the entries (`setVideoId`s) taken out of it, and when.
    removals: HashMap<String, (HashSet<String>, EditAge)>,
    /// Playlist: the songs added to it, each with when, and how many of its adds are
    /// still unanswered (each add is answered on its own).
    adds: HashMap<String, Vec<(Track, EditAge, u32)>>,
}

#[derive(Clone, Copy)]
struct EditAge {
    made: Instant,
    confirmed: Option<Instant>,
}

impl EditAge {
    fn new() -> Self {
        Self { made: Instant::now(), confirmed: None }
    }

    fn live(&self, unanswered: Duration) -> bool {
        match self.confirmed {
            Some(t) => t.elapsed() < EDIT_LAG,
            None => self.made.elapsed() < unanswered,
        }
    }
}

impl RecentEdits {
    fn like(&mut self, id: &str, like: bool, track: Option<Track>) {
        self.likes.insert(id.to_owned(), (like, track, EditAge::new()));
    }

    pub fn confirm_likes(&mut self, done: &[String], refused: &[String]) {
        for id in done {
            if let Some((_, _, age)) = self.likes.get_mut(id) {
                age.confirmed = Some(Instant::now());
            }
        }
        for id in refused {
            self.likes.remove(id);
        }
    }

    /// `(videoId, setVideoId)` entries taken out of a playlist. A song added here lately
    /// and taken out again isn't put back.
    pub fn remove(&mut self, playlist: &str, items: &[(String, String)]) {
        let (set, age) = self.removals.entry(playlist.to_owned()).or_insert_with(|| (HashSet::new(), EditAge::new()));
        set.extend(items.iter().map(|(_, s)| s.clone()));
        *age = EditAge::new();
        if let Some(added) = self.adds.get_mut(playlist) {
            added.retain(|(t, ..)| !items.iter().any(|(v, _)| *v == t.id));
        }
    }

    pub fn confirm_removal(&mut self, playlist: &str) {
        if let Some((_, age)) = self.removals.get_mut(playlist) {
            age.confirmed = Some(Instant::now());
        }
    }

    /// YouTube Music refused a removal from this playlist: what it lists stands.
    pub fn removal_failed(&mut self, playlist: &str) {
        self.removals.remove(playlist);
    }

    fn add(&mut self, playlist: &str, tracks: &[Track]) {
        // Ones that have run out don't come back with these.
        self.prune();
        let added = self.adds.entry(playlist.to_owned()).or_default();
        for (i, t) in tracks.iter().enumerate() {
            // A song twice in one add is one answer.
            if tracks[..i].iter().any(|x| x.id == t.id) {
                continue;
            }
            // One added already keeps its age, and its answer.
            match added.iter_mut().find(|(x, ..)| x.id == t.id) {
                Some((.., unanswered)) => *unanswered += 1,
                None => added.push((t.clone(), EditAge::new(), 1)),
            }
        }
    }

    /// YouTube Music took these songs into the playlist.
    pub fn confirm_add(&mut self, playlist: &str, video_ids: &[String]) {
        for (t, age, unanswered) in self.adds.get_mut(playlist).into_iter().flatten() {
            if video_ids.contains(&t.id) {
                age.confirmed.get_or_insert_with(Instant::now);
                *unanswered = unanswered.saturating_sub(1);
            }
        }
    }

    /// YouTube Music didn't take these songs into the playlist: the ones it hasn't taken,
    /// and isn't still answering another add of, stop showing (and are returned).
    fn add_failed(&mut self, playlist: &str, video_ids: &[String]) -> Vec<String> {
        let mut gone = Vec::new();
        if let Some(added) = self.adds.get_mut(playlist) {
            added.retain_mut(|(t, age, unanswered)| {
                if !video_ids.contains(&t.id) {
                    return true;
                }
                *unanswered = unanswered.saturating_sub(1);
                let keep = age.confirmed.is_some() || *unanswered > 0;
                if !keep {
                    gone.push(t.id.clone());
                }
                keep
            });
        }
        gone
    }

    /// Songs were added to or taken out of the playlist here lately, so what YouTube
    /// Music says of it may not show that yet.
    pub fn pending(&self, playlist: &str) -> bool {
        self.adds.get(playlist).is_some_and(|a| a.iter().any(|(_, age, _)| age.live(REMOVAL_UNANSWERED)))
            || self.removals.get(playlist).is_some_and(|(_, age)| age.live(REMOVAL_UNANSWERED))
    }

    fn prune(&mut self) {
        self.likes.retain(|_, (_, _, age)| age.live(LIKE_UNANSWERED));
        self.removals.retain(|_, (_, age)| age.live(REMOVAL_UNANSWERED));
        self.adds.retain(|_, added| {
            added.retain(|(_, age, _)| age.live(REMOVAL_UNANSWERED));
            !added.is_empty()
        });
    }

    /// All of a playlist as YouTube Music just sent it, completed (see `complete`). Its
    /// header lags behind edits, both ways and more than its list: while some made here
    /// are recent, it counts the songs it has. Otherwise the header's count stands (it
    /// counts songs it hides too), unless it lists more. One with no count (an album)
    /// gets none.
    fn complete_load(&self, c: &mut Collection) -> Vec<Track> {
        let header_lags = self.pending(&c.id);
        let missing = self.complete(c);
        let listed = c.tracks.len() as u64;
        c.count = c.count.map(|n| if header_lags { listed } else { n.max(listed) });
        missing
    }

    /// All of a playlist: the songs added here that it doesn't list yet go at its end, once
    /// each (and are returned), and count on top of it.
    fn complete(&self, c: &mut Collection) -> Vec<Track> {
        let mut missing: Vec<Track> = Vec::new();
        for (t, ..) in self.adds.get(&c.id).into_iter().flatten() {
            if !c.tracks.iter().chain(&missing).any(|x| x.id == t.id) {
                missing.push(t.clone());
            }
        }
        c.tracks.extend(missing.iter().cloned());
        c.set_ids.extend(missing.iter().map(|_| None));
        c.count = c.count.map(|n| (n + missing.len() as u64).max(c.tracks.len() as u64));
        missing
    }

    /// Whether a song row YouTube Music sent still stands: not taken out of that
    /// playlist, nor unliked when it's Liked Music.
    fn keeps(&self, list: &str, t: &Track, set_id: Option<&str>) -> bool {
        let unliked = list == LIKED_MUSIC && self.likes.get(&t.id).is_some_and(|(like, _, _)| !*like);
        let taken = set_id.is_some_and(|s| self.removals.get(list).is_some_and(|(r, _)| r.contains(s)));
        !unliked && !taken
    }

    /// A list as YouTube Music sent it, with the edits made here since; Liked Music also
    /// gets the songs liked here that it doesn't list yet, at the top.
    fn correct_collection(&mut self, c: &mut Collection) {
        self.prune();
        log::debug!(
            "edits: {} likes, {} unlikes, {} removals for {} pending",
            self.likes.values().filter(|(l, _, _)| *l).count(),
            self.likes.values().filter(|(l, _, _)| !*l).count(),
            self.removals.get(&c.id).map_or(0, |(r, _)| r.len()),
            c.id
        );
        let keep: Vec<bool> = c.tracks.iter().enumerate().map(|(i, t)| self.keeps(&c.id, t, c.set_ids.get(i).and_then(Option::as_deref))).collect();
        let dropped: Vec<usize> = keep.iter().enumerate().filter(|(_, k)| !**k).map(|(i, _)| i).collect();
        c.remove_rows(&dropped);
        if c.id == LIKED_MUSIC {
            for (id, (like, track, _)) in &self.likes {
                if let (true, Some(t)) = (*like, track)
                    && !c.tracks.iter().any(|x| &x.id == id)
                {
                    c.tracks.insert(0, t.clone());
                    c.set_ids.insert(0, None);
                    c.count = c.count.map(|n| n + 1);
                }
            }
        }
    }
}

/// How many times a sorted playlist loads again for its new order, after the load right
/// after YouTube Music took it; each waits [`SORT_WAIT`] longer than the last.
const SORT_TRIES: u32 = 4;
const SORT_WAIT: f64 = 2.0;

/// One of YouTube Music's sorts asked of your own playlist, until the playlist loads in
/// that order. Its answer comes before its reads have caught up: a load right after it
/// can still bring the old order (with the old one ticked), so it loads again a little
/// later, a few times at most.
pub struct SortWait {
    /// What was sent: the order that's ticked once it's in.
    actions: serde_json::Value,
    /// YouTube Music took it; the loads from then on are looked at.
    taken: bool,
    /// Loads asked for since, after the first.
    tries: u32,
    /// When to load it again; `None` while a load is awaited.
    due: Option<f64>,
}

impl SortWait {
    pub fn new(actions: serde_json::Value) -> Self {
        Self { actions, taken: false, tries: 0, due: None }
    }

    /// YouTube Music took the sort; the playlist is loading again.
    pub fn taken(&mut self) {
        self.taken = true;
    }

    /// The playlist's first page came in, with its sorts: true when it's in the order
    /// asked for, or when it's been tried enough (it stays as YouTube Music lists it).
    /// Otherwise it's to load again at [`Self::due`].
    pub fn arrived(&mut self, sorts: &[PlaylistSort], now: f64) -> bool {
        if !self.taken {
            return false;
        }
        if sorts.iter().any(|s| s.on && s.actions == self.actions) {
            return true;
        }
        // Another load meanwhile (going back to it) leaves the one waiting as it was.
        if self.due.is_some() {
            return false;
        }
        if self.tries >= SORT_TRIES {
            return true;
        }
        self.tries += 1;
        self.due = Some(now + SORT_WAIT * self.tries as f64);
        false
    }

    /// Taken, and a load of it is awaited, not waiting to load again.
    pub fn awaiting(&self) -> bool {
        self.taken && self.due.is_none()
    }

    /// When it's to load again, if it's waiting to.
    pub fn due(&self) -> Option<f64> {
        self.due
    }

    /// It's time (`now`) to load it again: true once, and it awaits that load.
    pub fn take_due(&mut self, now: f64) -> bool {
        let ready = self.due.is_some_and(|d| now >= d);
        if ready {
            self.due = None;
        }
        ready
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{Arrival, Collection, Done, LIKED_MUSIC, Lists, PlaylistSort, RecentEdits, SORT_TRIES, SortWait, Start, Track};

    fn track(id: &str) -> Track {
        Track { id: id.into(), title: id.into(), artists: vec![], ..Default::default() }
    }

    fn collection(id: &str, songs: &[&str]) -> Collection {
        Collection {
            id: id.into(),
            is_album: false,
            save_id: None,
            title: String::new(),
            subtitle: String::new(),
            artists: vec![],
            description: None,
            thumbs: vec![],
            tracks: songs.iter().map(|s| track(s)).collect(),
            set_ids: songs.iter().map(|s| Some(format!("set-{s}"))).collect(),
            count: Some(songs.len() as u64),
            owned: true,
            sorts: Vec::new(),
        }
    }

    fn start(lists: &mut Lists, id: &str) -> u64 {
        lists.load(id, false, false).unwrap().load
    }

    #[test]
    fn loads_wait_for_moves_and_happen_once_they_are_answered() {
        let mut lists = Lists::default();
        let load = start(&mut lists, "PL1");
        lists.arrived(load, collection("PL1", &["a", "b"]), false, &[]).unwrap();
        let (first, second) = (lists.move_sent("PL1"), lists.move_sent("PL1"));
        // Asked for meanwhile: held back, also past the first answer.
        assert_eq!(lists.load("PL1", false, true), None);
        assert_eq!(lists.move_answered("PL1", first, true), None);
        assert!(lists.moves.contains_key("PL1"));
        assert!(lists.move_answered("PL1", second, true).is_some());
        assert!(!lists.moves.contains_key("PL1"));
    }

    #[test]
    fn moves_that_all_went_through_need_no_load() {
        let mut lists = Lists::default();
        let m = lists.move_sent("PL1");
        assert_eq!(lists.move_answered("PL1", m, true), None);
        assert!(!lists.moves.contains_key("PL1"));
        assert!(!lists.loading("PL1"));
    }

    #[test]
    fn a_failed_move_reloads_only_after_the_rest_are_answered() {
        let mut lists = Lists::default();
        let (first, second) = (lists.move_sent("PL1"), lists.move_sent("PL1"));
        assert_eq!(lists.move_answered("PL1", first, false), None);
        assert!(!lists.loading("PL1"));
        assert!(lists.move_answered("PL1", second, true).is_some());
        assert!(lists.loading("PL1"));
    }

    #[test]
    fn answers_for_moves_of_another_account_do_nothing() {
        let mut lists = Lists::default();
        let old = lists.move_sent("PL1");
        lists.clear();
        assert!(!lists.awaiting_move("PL1", old));
        assert_eq!(lists.move_answered("PL1", old, false), None);
        assert!(!lists.loading("PL1"));
        // Nor does it count for a new move of the same playlist.
        let new = lists.move_sent("PL1");
        assert_eq!(lists.move_answered("PL1", old, false), None);
        assert!(lists.awaiting_move("PL1", new));
        assert_eq!(lists.move_answered("PL1", new, true), None);
        assert!(!lists.loading("PL1"));
    }

    #[test]
    fn a_move_answered_very_late_stops_holding_back_loads_but_still_counts() {
        let mut lists = Lists::default();
        let old = lists.move_sent("PL1");
        lists.moves.get_mut("PL1").unwrap().last_sent -= super::MOVE_UNANSWERED;
        let load = lists.load("PL1", false, false).unwrap().load;
        // Its answer still counts when it comes: that load may have the old order.
        lists.arrived(load, collection("PL1", &["a", "b"]), false, &[]).unwrap();
        assert!(lists.move_answered("PL1", old, true).is_some());
        assert!(!lists.moves.contains_key("PL1"));
    }

    #[test]
    fn a_load_that_failed_partway_isnt_the_whole_playlist() {
        let mut lists = Lists::default();
        let load = start(&mut lists, "PL1");
        lists.arrived(load, collection("PL1", &["a", "b"]), true, &[]).unwrap();
        assert!(!lists.get("PL1").unwrap().complete);
        // How a failed load ends.
        lists.page("PL1", load, vec![], vec![], false, &[]).unwrap();
        lists.load_failed("PL1", load);
        assert!(!lists.get("PL1").unwrap().complete);
        // Opening it again tries again, however recent, and that brings all of it.
        let load = lists.visit("PL1", false, Instant::now()).unwrap().load;
        lists.arrived(load, collection("PL1", &["a", "b", "c"]), false, &[]).unwrap();
        assert!(lists.get("PL1").unwrap().complete);
    }

    #[test]
    fn an_empty_last_page_from_a_load_that_worked_is_the_end_of_the_playlist() {
        let mut lists = Lists::default();
        let load = start(&mut lists, "PL1");
        lists.arrived(load, collection("PL1", &["a"]), true, &[]).unwrap();
        lists.page("PL1", load, vec![], vec![], false, &[]).unwrap();
        assert!(lists.get("PL1").unwrap().complete);
    }

    #[test]
    fn a_failed_reload_leaves_the_kept_copy_as_it_was() {
        let mut lists = Lists::default();
        let load = start(&mut lists, "PL1");
        lists.arrived(load, collection("PL1", &["a"]), false, &[]).unwrap();
        let again = start(&mut lists, "PL1");
        lists.page("PL1", again, vec![], vec![], false, &[]).unwrap();
        lists.load_failed("PL1", again);
        assert!(lists.get("PL1").unwrap().complete);
    }

    #[test]
    fn a_load_whose_last_page_came_is_the_whole_playlist() {
        let mut lists = Lists::default();
        let load = start(&mut lists, "PL1");
        lists.arrived(load, collection("PL1", &["a"]), true, &[]).unwrap();
        lists.page("PL1", load, vec![track("b")], vec![Some("set-b".into())], false, &[]).unwrap();
        assert!(lists.get("PL1").unwrap().complete);
    }

    fn order(c: &Collection) -> Vec<&str> {
        c.tracks.iter().map(|t| t.id.as_str()).collect()
    }

    #[test]
    fn a_moved_row_takes_its_entry_along_and_names_the_one_after_it() {
        let mut c = collection("PL1", &["a", "b", "c", "d"]);
        // Down, to the gap above "d" (counted before the move).
        assert_eq!(c.move_row(0, 3), Some(("set-a".to_owned(), Some("set-d".to_owned()))));
        assert_eq!(order(&c), ["b", "c", "a", "d"]);
        assert_eq!(c.set_ids[2].as_deref(), Some("set-a"));
        // Up.
        assert_eq!(c.move_row(3, 0), Some(("set-d".to_owned(), Some("set-b".to_owned()))));
        assert_eq!(order(&c), ["d", "b", "c", "a"]);
        assert_eq!(c.set_ids[0].as_deref(), Some("set-d"));
    }

    #[test]
    fn a_row_moved_to_the_end_has_no_entry_after_it() {
        let mut c = collection("PL1", &["a", "b", "c"]);
        assert_eq!(c.move_row(0, 3), Some(("set-a".to_owned(), None)));
        assert_eq!(order(&c), ["b", "c", "a"]);
        assert_eq!(c.count, Some(3));
    }

    #[test]
    fn nothing_moves_without_knowing_the_entries() {
        let mut c = collection("PL1", &["a", "b", "c"]);
        c.set_ids[2] = None;
        // The row moved, then the row it would go before.
        assert_eq!(c.move_row(2, 0), None);
        assert_eq!(c.move_row(0, 2), None);
        // Out of range.
        assert_eq!(c.move_row(3, 0), None);
        assert_eq!(c.move_row(0, 4), None);
        assert_eq!(order(&c), ["a", "b", "c"]);
    }

    fn songs(lists: &Lists, id: &str) -> Vec<String> {
        lists.get(id).map(|o| o.c.tracks.iter().map(|t| t.id.clone()).collect()).unwrap_or_default()
    }

    /// A playlist loaded whole.
    fn load_whole(lists: &mut Lists, id: &str, songs: &[&str], keep: &[&str]) {
        let s = lists.load(id, false, false).unwrap();
        assert_eq!(lists.arrived(s.load, collection(id, songs), false, keep), Some(Arrival::Done(Done::Kept)));
    }

    fn page_of(songs: &[&str]) -> (Vec<Track>, Vec<Option<String>>) {
        (songs.iter().map(|s| track(s)).collect(), songs.iter().map(|s| Some(format!("set-{s}"))).collect())
    }

    #[test]
    fn only_the_load_under_way_is_taken() {
        let mut lists = Lists::default();
        let s = lists.load("PL1", false, false).unwrap();
        assert_eq!(s, Start { id: "PL1".into(), load: 1, album: false, stream: true });
        // One load at a time.
        assert_eq!(lists.load("PL1", false, false), None);
        // An answer from another load is dropped.
        assert_eq!(lists.arrived(7, collection("PL1", &["a"]), true, &[]), None);
        assert!(lists.get("PL1").is_none());

        assert_eq!(lists.arrived(1, collection("PL1", &["a"]), true, &[]), Some(Arrival::More));
        assert!(lists.receiving("PL1"));
        let (t, set) = page_of(&["b"]);
        assert!(lists.page("PL1", 7, t.clone(), set.clone(), false, &[]).is_none());
        let page = lists.page("PL1", 1, t, set, false, &[]).unwrap();
        assert!(page.kept);
        assert_eq!(page.done, Some(Done::Kept));
        assert_eq!(songs(&lists, "PL1"), ["a", "b"]);
        assert!(!lists.receiving("PL1") && !lists.loading("PL1"));
        // Its last page came: a late one is dropped.
        let (t, set) = page_of(&["c"]);
        assert!(lists.page("PL1", 1, t, set, false, &[]).is_none());
    }

    #[test]
    fn a_change_during_a_load_loads_it_again() {
        let mut lists = Lists::default();
        let s = lists.load("PL1", false, false).unwrap();
        assert_eq!(lists.load("PL1", false, true), None);
        let again = lists.arrived(s.load, collection("PL1", &["a"]), false, &[]).unwrap();
        // Kept now, so the next one comes whole.
        assert_eq!(again, Arrival::Done(Done::Reload(Start { id: "PL1".into(), load: 2, album: false, stream: false })));
        assert!(lists.loading("PL1"));

        // A playlist streaming in that changes, and whose load then fails: what came is
        // kept, and it's loaded again.
        let s = lists.load("PL2", false, false).unwrap();
        lists.arrived(s.load, collection("PL2", &["a"]), true, &[]).unwrap();
        assert_eq!(lists.load("PL2", false, true), None);
        let page = lists.page("PL2", s.load, vec![], vec![], false, &[]).unwrap();
        assert!(matches!(page.done, Some(Done::Reload(Start { stream: false, .. }))));
        assert_eq!(songs(&lists, "PL2"), ["a"]);
    }

    #[test]
    fn a_load_that_fails_before_anything_came() {
        let mut lists = Lists::default();
        let s = lists.load("PL1", false, false).unwrap();
        // The backend ends a failed load with an empty last page.
        let page = lists.page("PL1", s.load, vec![], vec![], false, &[]).unwrap();
        assert!(!page.kept);
        assert_eq!(page.done, Some(Done::NotKept));
        assert!(!lists.known("PL1"));

        // One that fails after its first page keeps that page.
        let s = lists.load("PL3", false, false).unwrap();
        lists.arrived(s.load, collection("PL3", &["a"]), true, &[]).unwrap();
        assert_eq!(lists.page("PL3", s.load, vec![], vec![], false, &[]).unwrap().done, Some(Done::Kept));
        assert_eq!(songs(&lists, "PL3"), ["a"]);
        assert!(!lists.receiving("PL3"));

        // A reload that fails keeps the copy there was.
        load_whole(&mut lists, "PL2", &["a"], &[]);
        let s = lists.load("PL2", false, false).unwrap();
        assert_eq!(lists.page("PL2", s.load, vec![], vec![], false, &[]).unwrap().done, Some(Done::Kept));
        assert_eq!(songs(&lists, "PL2"), ["a"]);
    }

    #[test]
    fn a_kept_copy_is_swapped_when_the_new_one_is_in() {
        let mut lists = Lists::default();
        load_whole(&mut lists, "PL1", &["a", "b"], &[]);
        // A kept playlist reloads whole, not page by page.
        let s = lists.load("PL1", false, false).unwrap();
        assert!(!s.stream);
        assert_eq!(lists.arrived(s.load, collection("PL1", &["x", "a"]), false, &[]), Some(Arrival::Done(Done::Kept)));
        assert_eq!(songs(&lists, "PL1"), ["x", "a"]);

        // Should a page of the new load come anyway, it doesn't go onto the old copy (it's
        // still handed on for the queue).
        let s = lists.load("PL1", false, false).unwrap();
        let (t, set) = page_of(&["y"]);
        let page = lists.page("PL1", s.load, t, set, true, &[]).unwrap();
        assert!(!page.kept);
        assert_eq!(page.tracks.len(), 1);
        assert_eq!(songs(&lists, "PL1"), ["x", "a"]);
        assert!(!lists.receiving("PL1"));
        lists.arrived(s.load, collection("PL1", &["y", "x", "a"]), false, &[]).unwrap();
        assert_eq!(songs(&lists, "PL1"), ["y", "x", "a"]);
    }

    #[test]
    fn pages_leave_out_edits_made_here() {
        let mut lists = Lists::default();
        let s = lists.load(LIKED_MUSIC, false, false).unwrap();
        lists.arrived(s.load, collection(LIKED_MUSIC, &["a"]), true, &[]).unwrap();
        // Liked here while it loads: at the top at once, and not twice when its page comes.
        lists.set_liked(&[track("z")], true);
        lists.set_liked(&[track("u")], false);
        assert_eq!(songs(&lists, LIKED_MUSIC), ["z", "a"]);
        let (t, set) = page_of(&["b", "z", "u"]);
        let page = lists.page(LIKED_MUSIC, s.load, t, set, false, &[]).unwrap();
        assert_eq!(page.tracks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["b"]);
        assert_eq!(songs(&lists, LIKED_MUSIC), ["z", "a", "b"]);

        // An entry taken out of a playlist here stays out of a page still listing it.
        let s = lists.load("PL1", false, false).unwrap();
        lists.edits.remove("PL1", &[("c".to_owned(), "set-c".to_owned())]);
        lists.arrived(s.load, collection("PL1", &["c", "d"]), true, &[]).unwrap();
        let (t, set) = page_of(&["c", "e"]);
        lists.page("PL1", s.load, t, set, false, &[]).unwrap();
        assert_eq!(songs(&lists, "PL1"), ["d", "e"]);
    }

    #[test]
    fn room_is_made_from_the_oldest_but_not_what_is_in_use() {
        let mut lists = Lists::default();
        load_whole(&mut lists, LIKED_MUSIC, &["a"], &[]);
        load_whole(&mut lists, "PL0", &["a"], &[]);
        let loading = lists.load("PL1", false, false).unwrap();
        lists.arrived(loading.load, collection("PL1", &["a"]), true, &[]).unwrap();
        for i in 2..10 {
            load_whole(&mut lists, &format!("PL{i}"), &["a"], &["PL0"]);
        }
        // Kept: Liked Music, the page or song in use (PL0), the one still loading (PL1)
        // and the newest others, eight in all.
        let kept: Vec<&str> = lists.opened.iter().map(|o| o.c.id.as_str()).collect();
        assert_eq!(kept, ["PL9", "PL8", "PL7", "PL6", "PL5", "PL1", "PL0", LIKED_MUSIC]);

    }

    #[test]
    fn a_load_ending_with_no_room_left_is_dropped() {
        let mut lists = Lists::default();
        load_whole(&mut lists, LIKED_MUSIC, &["a"], &[]);
        // Nine playlists streaming in at once: all held while they load, past the limit.
        let keep = ["PL9", "PL8"];
        let starts: Vec<_> = (1..=9).map(|i| lists.load(&format!("PL{i}"), false, false).unwrap()).collect();
        for s in &starts {
            lists.arrived(s.load, collection(&s.id, &["a"]), true, &keep).unwrap();
        }
        assert_eq!(lists.opened.len(), 10);
        // The oldest ends: no longer held, it makes the room, even though it changed meanwhile.
        assert_eq!(lists.load("PL1", false, true), None);
        let page = lists.page("PL1", starts[0].load, vec![], vec![], false, &keep).unwrap();
        assert_eq!(page.done, Some(Done::NotKept));
        assert!(!lists.known("PL1"));
        // What's on screen or playing stays when its load ends, however full it is.
        let page = lists.page("PL8", starts[7].load, vec![], vec![], false, &keep).unwrap();
        assert_eq!(page.done, Some(Done::Kept));
        assert_eq!(lists.opened.len(), 9);
    }

    #[test]
    fn going_back_reloads_only_an_old_copy() {
        let mut lists = Lists::default();
        let now = Instant::now();
        assert!(lists.visit("PL1", false, now).is_some());
        assert_eq!(lists.visit("PL1", false, now), None);
        lists.arrived(1, collection("PL1", &["a"]), false, &[]).unwrap();
        assert_eq!(lists.visit("PL1", false, Instant::now()), None);
        assert!(lists.visit("PL1", false, Instant::now() + Duration::from_secs(6 * 60)).is_some());
    }

    #[test]
    fn another_account_starts_over() {
        let mut lists = Lists::default();
        load_whole(&mut lists, "PL1", &["a"], &[]);
        let s = lists.load("PL2", false, false).unwrap();
        lists.clear();
        assert!(!lists.known("PL1") && !lists.known("PL2"));
        // The old account's answer is dropped, also once the same list loads again.
        let again = lists.load("PL2", false, false).unwrap();
        assert_ne!(again.load, s.load);
        assert_eq!(lists.arrived(s.load, collection("PL2", &["a"]), false, &[]), None);
        // A deleted playlist: its load's answer is dropped too.
        lists.forget("PL2");
        assert_eq!(lists.arrived(again.load, collection("PL2", &["a"]), false, &[]), None);
    }

    #[test]
    fn stale_lists_drop_recent_removals_and_unlikes() {
        let mut edits = RecentEdits::default();
        edits.remove("PL1", &[("b".to_owned(), "set-b".to_owned()), ("c".to_owned(), "set-c".to_owned())]);
        edits.confirm_removal("PL1");
        let mut c = collection("PL1", &["a", "b", "c", "d"]);
        edits.correct_collection(&mut c);
        assert_eq!(c.tracks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["a", "d"]);
        assert_eq!(c.set_ids, [Some("set-a".to_owned()), Some("set-d".to_owned())]);
        assert_eq!(c.count, Some(2));
        // Another playlist is left alone.
        let mut other = collection("PL2", &["b"]);
        edits.correct_collection(&mut other);
        assert_eq!(other.tracks.len(), 1);

        edits.like("x", false, None);
        edits.like("y", true, Some(track("y")));
        edits.like("z", false, None);
        edits.confirm_likes(&["x".to_owned(), "y".to_owned()], &["z".to_owned()]);
        let mut lm = collection("LM", &["x", "z", "w"]);
        edits.correct_collection(&mut lm);
        // x unliked; y liked but not listed yet; z's unlike was refused, so it stays.
        assert_eq!(lm.tracks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["y", "z", "w"]);
        assert_eq!(lm.set_ids.len(), 3);
        assert_eq!(lm.count, Some(3));
        assert_eq!(lm.byline(), "3 songs");
        // A later page drops an unliked song too.
        assert!(!edits.keeps("LM", &track("x"), Some("set-x")));
        assert!(edits.keeps("PL9", &track("x"), Some("set-x")));
    }

    #[test]
    fn songs_added_here_show_until_the_playlist_lists_them() {
        let mut lists = Lists::default();
        load_whole(&mut lists, "PL1", &["a", "b"], &[]);
        // At the end of the kept copy at once.
        lists.added("PL1", &[track("c")]);
        assert_eq!(songs(&lists, "PL1"), ["a", "b", "c"]);
        assert_eq!(lists.get("PL1").unwrap().c.count, Some(3));
        lists.edits.confirm_add("PL1", &["c".to_owned()]);

        // A reload that doesn't list it yet still has it.
        load_whole(&mut lists, "PL1", &["a", "b"], &[]);
        assert_eq!(songs(&lists, "PL1"), ["a", "b", "c"]);
        // Once it does, it's there once, and a header that lags counts what's listed.
        let s = lists.load("PL1", false, false).unwrap();
        lists.arrived(s.load, Collection { count: Some(2), ..collection("PL1", &["a", "b", "c"]) }, false, &[]).unwrap();
        assert_eq!(songs(&lists, "PL1"), ["a", "b", "c"]);
        assert_eq!(lists.get("PL1").unwrap().c.count, Some(3));

        // A playlist coming page by page gets it after its last page, not in the middle.
        lists.added("PL2", &[track("z")]);
        let s = lists.load("PL2", false, false).unwrap();
        lists.arrived(s.load, collection("PL2", &["a"]), true, &[]).unwrap();
        assert_eq!(songs(&lists, "PL2"), ["a"]);
        let (t, set) = page_of(&["b"]);
        let page = lists.page("PL2", s.load, t, set, false, &[]).unwrap();
        assert_eq!(songs(&lists, "PL2"), ["a", "b", "z"]);
        // A queue filling from the load gets it too.
        assert_eq!(page.tracks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["b", "z"]);

        // One YouTube Music never answered stops showing after a while, and adding
        // another later doesn't bring it back.
        lists.edits.adds.get_mut("PL2").unwrap()[0].1.made = Instant::now() - Duration::from_secs(61);
        lists.added("PL2", &[track("y")]);
        load_whole(&mut lists, "PL2", &["a", "b"], &[]);
        assert_eq!(songs(&lists, "PL2"), ["a", "b", "y"]);
    }

    #[test]
    fn a_song_youtube_music_didnt_take_goes_with_the_next_load() {
        let mut lists = Lists::default();
        load_whole(&mut lists, "PL1", &["a"], &[]);
        lists.added("PL1", &[track("b")]);
        lists.add_failed("PL1", &["b".to_owned()]);
        assert!(!lists.edits.pending("PL1"));
        // Out of the kept copy at once, and the next load doesn't bring it back.
        assert_eq!(songs(&lists, "PL1"), ["a"]);
        assert_eq!(lists.get("PL1").unwrap().c.count, Some(1));
        load_whole(&mut lists, "PL1", &["a"], &[]);
        assert_eq!(songs(&lists, "PL1"), ["a"]);

        // Added again after it went in, a failure leaves the one that went in.
        lists.added("PL1", &[track("c")]);
        lists.edits.confirm_add("PL1", &["c".to_owned()]);
        lists.added("PL1", &[track("c")]);
        lists.add_failed("PL1", &["c".to_owned()]);
        assert_eq!(songs(&lists, "PL1"), ["a", "c"]);

        // Added twice before either is answered: the first failing leaves it to the second.
        lists.added("PL1", &[track("d")]);
        lists.added("PL1", &[track("d")]);
        lists.add_failed("PL1", &["d".to_owned()]);
        assert_eq!(songs(&lists, "PL1"), ["a", "c", "d"]);
        lists.edits.confirm_add("PL1", &["d".to_owned()]);
        load_whole(&mut lists, "PL1", &["a"], &[]);
        assert_eq!(songs(&lists, "PL1"), ["a", "c", "d"]);
    }

    #[test]
    fn a_later_add_failing_doesnt_take_an_earlier_one_with_it() {
        let mut lists = Lists::default();
        lists.added("PL1", &[track("a")]);
        lists.edits.confirm_add("PL1", &["a".to_owned()]);
        lists.edits.adds.get_mut("PL1").unwrap()[0].1.made = Instant::now() - Duration::from_secs(70);
        // "b" is never answered: it goes after a minute, "a" stays its while.
        lists.added("PL1", &[track("b")]);
        assert!(lists.edits.pending("PL1"));
        lists.edits.adds.get_mut("PL1").unwrap()[1].1.made = Instant::now() - Duration::from_secs(61);
        load_whole(&mut lists, "PL1", &["x"], &[]);
        assert_eq!(songs(&lists, "PL1"), ["x", "a"]);
    }

    #[test]
    fn a_song_twice_in_one_failed_add_goes() {
        let mut lists = Lists::default();
        lists.added("PL1", &[track("a"), track("a")]);
        lists.add_failed("PL1", &["a".to_owned(), "a".to_owned()]);
        assert!(!lists.edits.pending("PL1"));
    }

    #[test]
    fn a_song_added_and_taken_out_again_stays_out() {
        let mut lists = Lists::default();
        lists.added("PL1", &[track("c")]);
        lists.edits.confirm_add("PL1", &["c".to_owned()]);
        load_whole(&mut lists, "PL1", &["a", "c"], &[]);
        lists.edits.remove("PL1", &[("c".to_owned(), "set-c".to_owned())]);
        load_whole(&mut lists, "PL1", &["a", "c"], &[]);
        assert_eq!(songs(&lists, "PL1"), ["a"]);
    }

    #[test]
    fn songs_added_count_on_top_of_ones_that_cant_play() {
        let mut lists = Lists::default();
        // The header counts one more than it lists (a song that can't play).
        let s = lists.load("PL1", false, false).unwrap();
        lists.arrived(s.load, Collection { count: Some(3), ..collection("PL1", &["a", "b"]) }, false, &[]).unwrap();
        lists.added("PL1", &[track("c")]);
        assert_eq!(lists.get("PL1").unwrap().c.count, Some(4));
        // Added twice, it's there once.
        lists.added("PL1", &[track("d")]);
        lists.added("PL1", &[track("d")]);
        load_whole(&mut lists, "PL1", &["a", "b"], &[]);
        assert_eq!(songs(&lists, "PL1"), ["a", "b", "c", "d"]);
    }

    #[test]
    fn a_header_still_counting_a_song_taken_out_here_isnt_believed() {
        let mut lists = Lists::default();
        load_whole(&mut lists, "PL1", &["a", "b", "c", "d", "e"], &[]);
        lists.edits.remove("PL1", &[("e".to_owned(), "set-e".to_owned())]);
        lists.edits.confirm_removal("PL1");
        // The list is without it already, its header not yet.
        let s = lists.load("PL1", false, false).unwrap();
        lists.arrived(s.load, Collection { count: Some(5), ..collection("PL1", &["a", "b", "c", "d"]) }, false, &[]).unwrap();
        assert_eq!(lists.get("PL1").unwrap().c.count, Some(4));
        // Nor one already counting a song added here that the list lacks yet.
        lists.added("PL2", &[track("c")]);
        let s = lists.load("PL2", false, false).unwrap();
        lists.arrived(s.load, Collection { count: Some(3), ..collection("PL2", &["a", "b"]) }, false, &[]).unwrap();
        assert_eq!(songs(&lists, "PL2"), ["a", "b", "c"]);
        assert_eq!(lists.get("PL2").unwrap().c.count, Some(3));
    }

    #[test]
    fn a_list_without_a_count_gets_none_from_an_addition() {
        let mut lists = Lists::default();
        let s = lists.load("PL1", false, false).unwrap();
        lists.arrived(s.load, Collection { count: None, ..collection("PL1", &["a"]) }, false, &[]).unwrap();
        lists.added("PL1", &[track("b")]);
        let o = lists.get("PL1").unwrap();
        assert_eq!((o.c.tracks.len(), o.c.count), (2, None));
    }

    fn sorts(on: u64) -> Vec<PlaylistSort> {
        [(0, "Manual"), (3, "Title")]
            .into_iter()
            .map(|(order, name)| PlaylistSort {
                name: name.into(),
                on: order == on,
                manual: order == 0,
                actions: serde_json::json!([{"action": "ACTION_SET_PLAYLIST_VIDEO_ORDER", "playlistVideoOrder": order}]),
            })
            .collect()
    }

    #[test]
    fn a_sorted_playlist_loads_again_until_it_comes_in_that_order() {
        let mut wait = SortWait::new(sorts(3)[1].actions.clone());
        // A load from before YouTube Music took it doesn't count.
        assert!(!wait.arrived(&sorts(0), 0.0));
        assert_eq!(wait.due(), None);
        wait.taken();
        // Right after: still the old order. It loads again in a while, once.
        assert!(!wait.arrived(&sorts(0), 10.0));
        let due = wait.due().unwrap();
        assert!(due > 10.0);
        assert!(!wait.take_due(due - 0.1));
        assert!(wait.take_due(due));
        assert!(!wait.take_due(due + 1.0));
        // Then it's in.
        assert!(wait.arrived(&sorts(3), due + 1.0));
    }

    #[test]
    fn a_sorted_playlist_stops_loading_again_after_a_few_tries() {
        let mut wait = SortWait::new(sorts(3)[1].actions.clone());
        wait.taken();
        let mut now = 0.0;
        let mut waits = Vec::new();
        for _ in 0..SORT_TRIES {
            assert!(!wait.arrived(&sorts(0), now));
            let due = wait.due().unwrap();
            waits.push(due - now);
            assert!(wait.take_due(due));
            now = due;
        }
        assert!(wait.arrived(&sorts(0), now));
        // Each wait is longer than the last.
        assert!(waits.windows(2).all(|w| w[1] > w[0]));
    }

    #[test]
    fn manual_order_counts_as_in_once_it_is_ticked() {
        let mut wait = SortWait::new(sorts(0)[0].actions.clone());
        wait.taken();
        assert!(!wait.arrived(&sorts(3), 0.0));
        assert!(wait.take_due(wait.due().unwrap()));
        assert!(wait.arrived(&sorts(0), 5.0));
    }
}
