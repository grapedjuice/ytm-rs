//! A-B repeat (#370): a stretch of the playing song, played over and over. Set from the
//! seek bar's right-click menu; it belongs to that song and goes when another one plays.

use std::time::Duration;

use super::App;

/// The shortest stretch that loops: anything shorter would seek back before the seek
/// had shown in the position.
const MIN_LEN: f64 = 1.0;

/// A stretch of song `id`: from `start` (its beginning until one is set) to `end`, in
/// seconds. It loops once it has an end.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct AbLoop {
    pub id: String,
    pub start: f64,
    pub end: Option<f64>,
}

impl AbLoop {
    /// Start at `at`. An end that would come before it, or too soon after, goes.
    fn start_at(&mut self, at: f64) {
        self.start = at.max(0.0);
        if self.end.is_some_and(|e| e < self.start + MIN_LEN) {
            self.end = None;
        }
    }

    /// End at `at`. A start that would come after it, or too close before, goes back to
    /// the song's beginning; an end too close to that is no loop at all.
    fn end_at(&mut self, at: f64) {
        if at < self.start + MIN_LEN {
            self.start = 0.0;
        }
        self.end = (at >= MIN_LEN).then_some(at);
    }

    /// Where to go back to at `pos`: the start, once the end is reached.
    #[cfg(test)]
    fn back_to(&self, pos: f64) -> Option<f64> {
        self.end.filter(|&e| pos >= e).map(|_| self.start)
    }
}

impl App {
    /// The loop of the song playing now, if it has one.
    pub(super) fn ab_loop(&self) -> Option<&AbLoop> {
        self.ab_loop.as_ref().filter(|l| self.current_id() == Some(l.id.as_str()))
    }

    /// The loop for the song playing now, a new one if it has none.
    fn loop_here(&mut self) -> Option<&mut AbLoop> {
        let id = self.current_id()?.to_owned();
        if self.ab_loop.as_ref().is_none_or(|l| l.id != id) {
            self.ab_loop = Some(AbLoop { id, start: 0.0, end: None });
        }
        self.ab_loop.as_mut()
    }

    pub(super) fn loop_start_at(&mut self, at: f32) {
        if let Some(l) = self.loop_here() {
            l.start_at(at as f64);
        }
    }

    pub(super) fn loop_end_at(&mut self, at: f32) {
        let at = self.loop_latest().map_or(at as f64, |l| (at as f64).min(l));
        if let Some(l) = self.loop_here() {
            l.end_at(at);
        }
    }

    /// The latest a loop can end: clear of where a crossfade would start the next song
    /// (the engine's fade is in real time, so more of the song when faster), and of the
    /// end itself, with half a second to spare for a frame that comes late.
    fn loop_latest(&self) -> Option<f64> {
        let fade = if self.crossfade { self.crossfade_secs as f64 } else { 0.0 };
        let speed = self.audio.status.speed() as f64;
        self.audio.status.duration().map(|d| d.as_secs_f64() - (fade + 0.5) * speed)
    }

    /// The playing song's loop as it plays now, start and end: its end kept clear of a
    /// crossfade or speed set since it was picked.
    pub(super) fn ab_stretch(&self) -> Option<(f64, f64)> {
        let l = self.ab_loop()?;
        let end = l.end?.min(self.loop_latest().unwrap_or(f64::INFINITY));
        (end >= l.start + MIN_LEN).then_some((l.start, end))
    }

    /// Back to the start at the end, as `skip_segments` skips: checked each frame, and
    /// woken for the end when nothing else draws.
    pub(super) fn keep_looping(&mut self, ctx: &egui::Context, now: f64) {
        // Another song plays: the loop was the last one's. A jam guest plays the host's.
        if self.ab_loop.is_some() && (self.ab_loop().is_none() || self.jam.guest()) {
            self.ab_loop = None;
        }
        if self.resume_at.is_some() || self.buffering || !self.playing() {
            return;
        }
        // A seek takes a moment to show in the position: don't send it twice.
        if now - self.loop_sought < 0.5 {
            return;
        }
        let Some((start, end)) = self.ab_stretch() else { return };
        let pos = self.audio.status.position().as_secs_f64();
        if pos >= end {
            self.loop_back(start, now);
        } else {
            // The position moves at the song's speed, the clock in real time.
            let speed = (self.audio.status.speed() as f64).max(0.1);
            ctx.request_repaint_after(Duration::from_secs_f64((end - pos) / speed + 0.02));
        }
    }

    pub(super) fn loop_back(&mut self, start: f64, now: f64) {
        self.loop_sought = now;
        self.seek_to(Duration::from_secs_f64(start));
    }
}

#[cfg(test)]
mod tests {
    use super::AbLoop;

    fn song() -> AbLoop {
        AbLoop { id: "a".into(), start: 0.0, end: None }
    }

    #[test]
    fn it_loops_once_it_has_an_end() {
        let mut l = song();
        l.start_at(30.0);
        assert_eq!(l.back_to(200.0), None, "a start alone doesn't loop");
        l.end_at(45.0);
        assert_eq!((l.back_to(44.9), l.back_to(45.0), l.back_to(50.0)), (None, Some(30.0), Some(30.0)));
    }

    #[test]
    fn an_end_alone_loops_from_the_beginning() {
        let mut l = song();
        l.end_at(20.0);
        assert_eq!(l.back_to(20.0), Some(0.0));
    }

    #[test]
    fn points_the_wrong_way_round_drop_the_other() {
        // A start after the end: the end goes.
        let mut l = song();
        l.end_at(20.0);
        l.start_at(25.0);
        assert_eq!((l.start, l.end), (25.0, None));
        // An end before the start: the start goes back to the beginning.
        let mut l = song();
        l.start_at(30.0);
        l.end_at(10.0);
        assert_eq!((l.start, l.end), (0.0, Some(10.0)));
        // Too short to loop: no end.
        let mut l = song();
        l.end_at(0.5);
        assert_eq!(l.end, None);
    }
}
