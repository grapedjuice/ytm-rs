//! Long tracks (podcast episodes, long mixes) pick up where they were left: played again
//! later, they start from there instead of from the top. A track is long by its length,
//! since episodes come in through many lists that don't mark them.

use std::time::Duration;

/// From this long on, a track is one people come back to rather than play through.
pub const LONG: u32 = 20 * 60;

/// Less than this far in isn't worth coming back to, and this close to the end the
/// track starts from the top next time.
const MARGIN: u32 = 60;

/// How many tracks are remembered; the one left longest ago goes first.
const KEPT: usize = 200;

/// Where each long track was left, in seconds, the one left longest ago first.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct Points(Vec<(String, u32)>);

impl Points {
    /// Track `id`, `duration` seconds long, was left `at` seconds in. Left in its first
    /// minute (skipped past, or it never got going), where it was left before still holds.
    pub fn note(&mut self, id: &str, at: u32, duration: u32) {
        if at < MARGIN {
            return;
        }
        self.forget(id);
        if duration >= LONG && at + MARGIN < duration {
            self.0.push((id.to_owned(), at));
            if self.0.len() > KEPT {
                self.0.remove(0);
            }
        }
    }

    /// Track `id` was played to its end: it starts from the top next time.
    pub fn forget(&mut self, id: &str) {
        self.0.retain(|(i, _)| i != id);
    }

    /// Where to pick up track `id`, if it was left partway.
    pub fn at(&self, id: &str) -> Option<Duration> {
        self.0.iter().find(|(i, _)| i == id).map(|&(_, at)| Duration::from_secs(at.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::{KEPT, LONG, Points};
    use std::time::Duration;

    #[test]
    fn a_long_track_left_partway_picks_up_there() {
        let mut p = Points::default();
        p.note("ep", 1500, 3600);
        assert_eq!(p.at("ep"), Some(Duration::from_secs(1500)));
        // Left again later: from there.
        p.note("ep", 2000, 3600);
        assert_eq!(p.at("ep"), Some(Duration::from_secs(2000)));
        // Barely begun (skipped past): still from there.
        p.note("ep", 20, 3600);
        assert_eq!(p.at("ep"), Some(Duration::from_secs(2000)));
        // Played to (nearly) the end: from the top next time.
        p.note("ep", 3590, 3600);
        assert_eq!(p.at("ep"), None);
        p.note("ep", 1500, 3600);
        p.forget("ep");
        assert_eq!(p.at("ep"), None);
        // A song isn't long enough to come back to.
        p.note("song", 120, 240);
        assert_eq!(p.at("song"), None);
    }

    #[test]
    fn only_the_latest_are_kept() {
        let mut p = Points::default();
        for i in 0..=KEPT {
            p.note(&i.to_string(), 600, LONG);
        }
        assert_eq!(p.at("0"), None);
        assert!(p.at("1").is_some() && p.at(&KEPT.to_string()).is_some());
    }
}
