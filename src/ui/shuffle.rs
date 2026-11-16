//! Shuffle that sounds random to a person: one artist's songs spread out across the
//! queue instead of landing back to back, and their albums spread out among them.
//! Spotify's "dithered" shuffle (2014): each artist's songs get evenly spaced places
//! from a random start, each a little off its spot, and all merge by place.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use super::queue::Queue;
use crate::model::Track;

/// Shuffle `v`, which plays after `before`. No two songs by one artist in a row, nor the
/// first by the artist that ends `before`, where any order can manage that; where none
/// can (mostly one artist), as few in a row as can be.
pub fn shuffle(v: &mut [Track], before: &[Track]) {
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    spread(v, before, &mut Rng::new(seed));
}

/// A list about to play shuffled in place of `q`'s: after its song playing, which doesn't
/// go first (its Play would only pause it), and with the explicit songs `q` leaves out
/// put last, so leaving them out doesn't bring two by one artist together.
pub fn shuffle_to_play(t: &mut [Track], q: &Queue) {
    let mut shown = t.len();
    if q.hide_explicit {
        t.sort_by_key(|t| t.explicit);
        shown = t.iter().filter(|t| !t.explicit).count();
    }
    let current = q.current();
    shuffle(&mut t[..shown], current.map_or(&[][..], std::slice::from_ref));
    if shown > 1 && current.is_some_and(|c| c.id == t[0].id) {
        t.swap(0, 1);
    }
}

fn spread(v: &mut [Track], before: &[Track], rng: &mut Rng) {
    let artists = groups(v.iter().enumerate().map(|(i, t)| (i, artist(t))));
    // Each artist's songs in order: their albums taking turns, each album's songs shuffled.
    let songs: Vec<Vec<usize>> = artists
        .iter()
        .map(|songs| {
            let mut albums = groups(songs.iter().map(|&i| (i, key(v[i].album.as_ref().map(|a| a.name.as_str())))));
            for a in &mut albums {
                for j in (1..a.len()).rev() {
                    a.swap(j, rng.below(j + 1));
                }
            }
            merge(&albums, None, rng)
        })
        .collect();
    // The artist that ends `before`, and how many of theirs in a row it ends with.
    let last = before.last().and_then(artist).and_then(|k| {
        let g = artists.iter().position(|s| artist(&v[s[0]]).as_ref() == Some(&k))?;
        Some((g, before.iter().rev().take_while(|t| artist(t).as_ref() == Some(&k)).count()))
    });
    let order = merge(&songs, last, rng);
    let mut old: Vec<Track> = v.iter_mut().map(std::mem::take).collect();
    for (slot, i) in order.into_iter().enumerate() {
        v[slot] = std::mem::take(&mut old[i]);
    }
}

/// All of `groups`' items: each group's given evenly spaced places (in its order) and
/// all merged by place, with as few from one group in a row as can be, counting `last`:
/// the group just before, and how many of its items in a row that ended with.
///
/// Heaps keep it quick for a big library: each item costs a few steps whatever the
/// number of groups, where looking through every group for each item took seconds.
fn merge(groups: &[Vec<usize>], last: Option<(usize, usize)>, rng: &mut Rng) -> Vec<usize> {
    let placed: Vec<Vec<(f64, usize)>> = groups.iter().map(|g| dither(g, rng)).collect();
    let n = placed.iter().map(Vec::len).sum::<usize>();
    // The fewest in a row any order manages: a group's runs (for `last`'s, the first going
    // on from before) need one of the others between each two.
    let ahead = |g| last.filter(|l| l.0 == g).map_or(0, |l| l.1);
    let most = placed.iter().enumerate().map(|(g, q)| (q.len() + ahead(g)).div_ceil(n - q.len() + 1)).max().unwrap_or(1).max(1);
    // How far into each group, and how many it has left.
    let mut taken = vec![0; placed.len()];
    let left_in = |g: usize, taken: &[usize]| placed[g].len() - taken[g];
    // Each group's next place, earliest first; and each group by how many it has left, most
    // first. An entry for an item already taken, or a count since changed, is skipped.
    let mut fronts: BinaryHeap<Reverse<Front>> = placed.iter().enumerate().filter(|(_, q)| !q.is_empty()).map(|(g, q)| Reverse(Front { place: q[0].0, group: g, at: 0 })).collect();
    let mut sizes: BinaryHeap<(usize, Reverse<usize>)> = placed.iter().enumerate().map(|(g, q)| (q.len(), Reverse(g))).collect();
    let (mut order, mut last) = (Vec::with_capacity(n), last.map(|l| l.0));
    for left in (1..=n).rev() {
        // A group with too many left for the others to keep its runs that short goes now
        // (only the biggest can be: two at once would need more than are left); otherwise
        // the earliest place by another group than the last.
        while sizes.peek().is_some_and(|&(len, Reverse(g))| len != left_in(g, &taken)) {
            sizes.pop();
        }
        let tight = sizes.peek().filter(|&&(len, _)| len > most * (left - len)).map(|&(_, Reverse(g))| g);
        let Some(g) = tight.or_else(|| earliest(&mut fronts, &taken, last)).or_else(|| earliest(&mut fronts, &taken, None)) else {
            break;
        };
        order.push(placed[g][taken[g]].1);
        taken[g] += 1;
        last = Some(g);
        if let Some(&(place, _)) = placed[g].get(taken[g]) {
            fronts.push(Reverse(Front { place, group: g, at: taken[g] }));
        }
        sizes.push((left_in(g, &taken), Reverse(g)));
    }
    order
}

/// A group's next item, by its place (and the group, for a tie).
struct Front {
    place: f64,
    group: usize,
    /// Which of the group's items: no longer its next once that's taken.
    at: usize,
}

impl Ord for Front {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.place.total_cmp(&other.place).then(self.group.cmp(&other.group))
    }
}

impl PartialOrd for Front {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Front {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for Front {}

/// The group whose next item has the earliest place, other than `skip`.
fn earliest(fronts: &mut BinaryHeap<Reverse<Front>>, taken: &[usize], skip: Option<usize>) -> Option<usize> {
    let mut held = None;
    let found = loop {
        let Some(Reverse(f)) = fronts.peek() else { break None };
        if f.at != taken[f.group] {
            fronts.pop();
        } else if Some(f.group) == skip && held.is_none() {
            held = fronts.pop();
        } else {
            break Some(f.group);
        }
    };
    fronts.extend(held);
    found
}

/// Evenly spaced places in 0..1 for `items`, in their order: from a random start, each up
/// to a tenth of the spacing off its spot (so never past its neighbours).
fn dither(items: &[usize], rng: &mut Rng) -> Vec<(f64, usize)> {
    let (n, start) = (items.len() as f64, rng.unit());
    items.iter().enumerate().map(|(j, &i)| ((j as f64 + start + (rng.unit() - 0.5) * 0.2) / n, i)).collect()
}

/// `items` grouped by key, in order of first appearance; one without a key is a group
/// of its own.
fn groups(items: impl Iterator<Item = (usize, Option<String>)>) -> Vec<Vec<usize>> {
    let mut at = HashMap::new();
    let mut out: Vec<Vec<usize>> = Vec::new();
    for (i, k) in items {
        match k.map(|k| *at.entry(k).or_insert(out.len())) {
            Some(g) if g < out.len() => out[g].push(i),
            _ => out.push(vec![i]),
        }
    }
    out
}

/// The song's (first) artist, by name: some rows name one without a channel id.
fn artist(t: &Track) -> Option<String> {
    key(t.artists.first().map(|a| a.name.as_str()))
}

fn key(name: Option<&str>) -> Option<String> {
    name.map(|n| n.trim().to_lowercase()).filter(|n| !n.is_empty())
}

/// xorshift64: shuffling doesn't need a real RNG crate.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// In 0..1.
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::{Rng, dither, merge, shuffle_to_play, spread};
    use crate::model::{Link, Track};
    use crate::ui::queue::{Queue, Repeat};

    /// `songs` as "artist/album/title".
    fn tracks(songs: &[&str]) -> Vec<Track> {
        songs
            .iter()
            .map(|s| {
                let p: Vec<&str> = s.split('/').collect();
                let link = |name: &str| Link { name: name.into(), id: None };
                Track { id: s.to_string(), artists: vec![link(p[0])].into_iter().filter(|a| !a.name.is_empty()).collect(), album: Some(link(p[1])), ..Default::default() }
            })
            .collect()
    }

    fn shuffled(songs: &[&str], before: &[&str], seed: u64) -> Vec<Track> {
        let mut v = tracks(songs);
        spread(&mut v, &tracks(before), &mut Rng::new(seed));
        let (mut a, mut b): (Vec<String>, Vec<String>) = (v.iter().map(|t| t.id.clone()).collect(), songs.iter().map(|s| s.to_string()).collect());
        a.sort();
        b.sort();
        assert_eq!(a, b, "every song once");
        v
    }

    fn artists(v: &[Track]) -> Vec<String> {
        v.iter().map(|t| t.artists.first().map_or(String::new(), |a| a.name.clone())).collect()
    }

    fn repeats(v: &[Track]) -> usize {
        artists(v).windows(2).filter(|w| w[0] == w[1] && !w[0].is_empty()).count()
    }

    /// The most by one artist in a row.
    fn longest(v: &[Track]) -> usize {
        let a = artists(v);
        (0..a.len()).map(|i| a[i..].iter().take_while(|x| **x == a[i]).count()).max().unwrap_or(0)
    }

    #[test]
    fn no_artist_plays_twice_in_a_row_when_that_can_be_helped() {
        let songs = ["a/x/1", "a/x/2", "a/y/3", "a/y/4", "b/z/1", "b/z/2", "b/z/3", "c/w/1", "c/w/2", "d/v/1"];
        for seed in 0..500 {
            assert_eq!(repeats(&shuffled(&songs, &[], seed)), 0, "seed {seed}");
        }
        // Exactly half by one artist: every other song.
        let half = ["a/x/1", "a/x/2", "a/x/3", "b/y/1", "c/z/1", "d/w/1"];
        for seed in 0..500 {
            assert_eq!(repeats(&shuffled(&half, &[], seed)), 0, "seed {seed}");
        }
    }

    #[test]
    fn the_first_song_isnt_by_the_artist_playing() {
        for seed in 0..200 {
            let v = shuffled(&["a/x/1", "a/x/2", "b/y/1", "c/z/1"], &["a/x/0"], seed);
            assert_ne!(artists(&v)[0], "a", "seed {seed}");
            assert_eq!(repeats(&v), 0, "seed {seed}");
        }
        // When some repeat can't be helped, it isn't right after the song playing.
        for seed in 0..200 {
            assert_eq!(artists(&shuffled(&["a/x/1", "a/x/2", "b/y/1"], &["a/x/0"], seed)), ["b", "a", "a"], "seed {seed}");
        }
    }

    #[test]
    fn a_run_before_counts_whole() {
        // The song playing and the next are by one artist: no third of theirs after them.
        for seed in 0..200 {
            let mut v = tracks(&["a/x/0", "a/x/9"]);
            v.extend(shuffled(&["a/x/1", "a/x/2", "b/y/1"], &["a/x/0", "a/x/9"], seed));
            assert_eq!(longest(&v), 2, "seed {seed}: {:?}", artists(&v));
        }
    }

    #[test]
    fn mostly_one_artist_still_spreads_out() {
        // Six by one artist among nine: never more than two in a row.
        let songs = ["a/x/1", "a/x/2", "a/x/3", "a/x/4", "a/x/5", "a/x/6", "b/y/1", "c/z/1", "d/w/1"];
        for seed in 0..500 {
            let v = shuffled(&songs, &[], seed);
            assert_eq!(longest(&v), 2, "seed {seed}: {:?}", artists(&v));
        }
    }

    #[test]
    fn an_artists_albums_take_turns() {
        // Two albums of two: alternating (whichever comes first).
        for seed in 0..200 {
            let v = shuffled(&["a/x/1", "a/x/2", "a/y/1", "a/y/2"], &[], seed);
            let albums: Vec<String> = v.iter().map(|t| t.album.as_ref().unwrap().name.clone()).collect();
            assert!(albums.windows(2).all(|w| w[0] != w[1]), "seed {seed}: {albums:?}");
        }
    }

    #[test]
    fn the_same_seed_gives_the_same_order_and_others_differ() {
        let songs = ["a/x/1", "a/x/2", "b/y/1", "b/y/2", "c/z/1", "c/z/2", "d/w/1", "e/v/1"];
        let ids = |seed| shuffled(&songs, &[], seed).into_iter().map(|t| t.id).collect::<Vec<_>>();
        assert_eq!(ids(7), ids(7));
        let orders: std::collections::HashSet<_> = (0..50).map(ids).collect();
        assert!(orders.len() > 20, "only {} orders", orders.len());
    }

    #[test]
    fn any_mix_of_artists_has_the_fewest_in_a_row() {
        let mut rng = Rng::new(42);
        for seed in 0..300 {
            let (count, by) = (2 + rng.below(30), 1 + rng.below(8));
            let songs: Vec<String> = (0..count).map(|i| format!("{}/{}/{i}", rng.below(by), rng.below(3))).collect();
            let songs: Vec<&str> = songs.iter().map(String::as_str).collect();
            let v = shuffled(&songs, &[], seed);
            // The most-played artist's songs need the others between their runs.
            let a = artists(&v);
            let most = a.iter().map(|x| a.iter().filter(|y| *y == x).count()).max().unwrap();
            assert_eq!(longest(&v), most.div_ceil(a.len() - most + 1), "seed {seed}: {a:?}");
        }
    }

    #[test]
    fn playing_shuffled_leaves_hidden_explicit_songs_out_of_the_way() {
        let mut q = Queue::new(Repeat::Off);
        q.replace(tracks(&["d/w/0"]), 0, None);
        q.hide_explicit = true;
        for _ in 0..50 {
            let mut t = tracks(&["a/x/1", "a/x/2", "b/y/1", "c/z/1"]);
            t[3].explicit = true;
            shuffle_to_play(&mut t, &q);
            let mut played = Queue::new(Repeat::Off);
            played.hide_explicit = true;
            played.replace(t, 0, None);
            assert_eq!(artists(&played.tracks), ["a", "b", "a"]);
        }
        // Nor does the song playing go first.
        let mut q = Queue::new(Repeat::Off);
        q.replace(tracks(&["a/x/1"]), 0, None);
        for _ in 0..50 {
            let mut t = tracks(&["a/x/1", "a/x/2", "a/y/3"]);
            shuffle_to_play(&mut t, &q);
            assert_ne!(t[0].id, "a/x/1");
        }
    }

    #[test]
    fn a_big_library_shuffles_quickly() {
        // The slowest kind: every song by an artist of its own (or none).
        for artist in ["", "a"] {
            let songs: Vec<String> = (0..5000).map(|i| format!("{artist}{}/x/{i}", if artist.is_empty() { String::new() } else { i.to_string() })).collect();
            let songs: Vec<&str> = songs.iter().map(String::as_str).collect();
            let start = std::time::Instant::now();
            assert_eq!(repeats(&shuffled(&songs, &[], 1)), 0);
            // About 1 s in a debug build; far more room than that for slow CI runners.
            assert!(start.elapsed().as_secs() < 30, "{:?}", start.elapsed());
        }
    }

    #[test]
    fn edge_cases() {
        assert!(shuffled(&[], &["a/x/0"], 1).is_empty());
        assert_eq!(shuffled(&["a/x/1"], &["a/x/0"], 1)[0].id, "a/x/1");
        // One artist: still every song, in some order of all of them.
        let one: std::collections::HashSet<_> = (0..50).map(|s| shuffled(&["a/x/1", "a/x/2", "a/x/3"], &[], s)[0].id.clone()).collect();
        assert_eq!(one.len(), 3);
        // Songs with no artist never count as a repeat.
        assert_eq!(repeats(&shuffled(&["/x/1", "/x/2", "/x/3", "a/y/1"], &[], 3)), 0);
    }

    /// The merge as first written, looking through every group for each item: slow for a
    /// big library, but plainly right.
    fn merge_simple(groups: &[Vec<usize>], last: Option<(usize, usize)>, rng: &mut Rng) -> Vec<usize> {
        use std::collections::VecDeque;
        let mut queues: Vec<VecDeque<(f64, usize)>> = groups.iter().map(|g| dither(g, rng).into()).collect();
        let n = queues.iter().map(VecDeque::len).sum::<usize>();
        let ahead = |g| last.filter(|l| l.0 == g).map_or(0, |l| l.1);
        let most = queues.iter().enumerate().map(|(g, q)| (q.len() + ahead(g)).div_ceil(n - q.len() + 1)).max().unwrap_or(1).max(1);
        let earliest = |queues: &[VecDeque<(f64, usize)>], skip: Option<usize>| {
            let fronts = queues.iter().enumerate().filter(|&(g, _)| Some(g) != skip);
            fronts.filter_map(|(g, q)| Some((g, q.front()?.0))).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(g, _)| g)
        };
        let (mut order, mut last) = (Vec::with_capacity(n), last.map(|l| l.0));
        for left in (1..=n).rev() {
            let tight = queues.iter().position(|q| q.len() > most * (left - q.len()));
            let next = tight.or_else(|| earliest(&queues, last)).or_else(|| earliest(&queues, None));
            let Some((g, (_, i))) = next.and_then(|g| Some((g, queues[g].pop_front()?))) else {
                break;
            };
            order.push(i);
            last = Some(g);
        }
        order
    }

    #[test]
    fn the_quick_merge_gives_the_same_order_as_the_simple_one() {
        let mut make = Rng::new(5);
        for seed in 0..3000 {
            // A few groups to a few hundred, of any sizes, some empty; sometimes a run before.
            let count = 1 + make.below(if seed % 10 == 0 { 300 } else { 12 });
            let mut next = 0;
            let groups: Vec<Vec<usize>> = (0..count)
                .map(|_| {
                    let big = make.below(4) == 0;
                    let len = make.below(if big { 40 } else { 5 });
                    next += len;
                    (next - len..next).collect()
                })
                .collect();
            let last = (make.below(2) == 0).then(|| (make.below(count), 1 + make.below(3)));
            let (quick, simple) = (merge(&groups, last, &mut Rng::new(seed)), merge_simple(&groups, last, &mut Rng::new(seed)));
            assert_eq!(quick, simple, "seed {seed}: {:?}", groups.iter().map(Vec::len).collect::<Vec<_>>());
        }
    }
}
