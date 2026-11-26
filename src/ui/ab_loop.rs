//! A-B repeat (#370): a stretch of the playing song, played over and over. Set from a
//! seek bar's right-click menu; it belongs to that song and goes when another one plays.

use std::time::{Duration, Instant};

use egui::{Color32, Rect, Response, Ui, pos2, vec2};

use super::{Action, App, widgets};
use crate::theme::{self, icon};

/// The shortest stretch that loops: long enough that going back shows as a seek to a
/// jam's guests (a jump of over 1.5 s) and comes after SponsorBlock's 1.5 s between skips.
const MIN_LEN: f64 = 2.0;

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
    /// The loop of the song playing now, if it has one. By what's heard: the Song/Video
    /// switch puts another upload, with its own timing, in its place.
    pub(super) fn ab_loop(&self) -> Option<&AbLoop> {
        self.ab_loop.as_ref().filter(|l| self.playing_id() == Some(l.id.as_str()))
    }

    /// The loop for the song playing now, a new one if it has none.
    fn loop_here(&mut self) -> Option<&mut AbLoop> {
        let id = self.playing_id()?.to_owned();
        if self.ab_loop.as_ref().is_none_or(|l| l.id != id) {
            self.ab_loop = Some(AbLoop { id, start: 0.0, end: None });
        }
        self.ab_loop.as_mut()
    }

    /// Start at `at`, and play from there.
    pub(super) fn loop_start_at(&mut self, at: f32) {
        let Some(l) = self.loop_here() else { return };
        l.start_at(at as f64);
        let start = l.start;
        self.loop_back(start);
    }

    pub(super) fn loop_end_at(&mut self, at: f32) {
        let wanted = at as f64;
        let at = self.loop_latest().map_or(wanted, |l| wanted.min(l));
        // Picked after the start but kept clear of the end too close to it: no loop to
        // make, and the start stays where it was.
        if at < wanted && self.ab_loop().is_some_and(|l| wanted >= l.start + MIN_LEN && at < l.start + MIN_LEN) {
            return;
        }
        if let Some(l) = self.loop_here() {
            l.end_at(at);
        }
        // Already past it: back to the start now.
        if let Some((start, end)) = self.ab_stretch()
            && self.audio.status.position_smooth().as_secs_f64() >= end
        {
            self.loop_back(start);
        }
    }

    /// The latest a loop can end: clear of where a crossfade would start the next song
    /// (the engine's fade is in real time, so more of the song when faster), and of the
    /// end itself, with half a second to spare for a frame that comes late. A song no longer
    /// than two fades isn't crossfaded (as the engine has it).
    fn loop_latest(&self) -> Option<f64> {
        let speed = self.audio.status.speed() as f64;
        let d = self.audio.status.duration()?.as_secs_f64();
        let fade = if self.crossfade { self.crossfade_secs as f64 * speed } else { 0.0 };
        let fade = if d > fade * 2.0 { fade } else { 0.0 };
        Some(d - fade - 0.5 * speed)
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
    pub(super) fn keep_looping(&mut self, ctx: &egui::Context) {
        // Another song plays: the loop was the last one's. A jam guest plays the host's.
        if self.ab_loop.is_some() && (self.ab_loop().is_none() || self.jam.guest()) {
            self.ab_loop = None;
        }
        if self.resume_at.is_some() || self.buffering || !self.playing() {
            return;
        }
        // A seek takes a moment to show in the position: don't send it twice. By the clock,
        // not egui's: that one stands still while the window is hidden.
        if self.loop_sought.is_some_and(|t| t.elapsed() < Duration::from_millis(500)) {
            return;
        }
        let Some((start, end)) = self.ab_stretch() else { return };
        // Where it is now, between the engine's 100 ms updates: the end comes when it does,
        // not up to a tenth of a second later.
        let pos = self.audio.status.position_smooth().as_secs_f64();
        if pos >= end {
            self.loop_back(start);
        } else {
            // The position moves at the song's speed, the clock in real time.
            let speed = (self.audio.status.speed() as f64).max(0.1);
            ctx.request_repaint_after(Duration::from_secs_f64((end - pos) / speed + 0.002));
        }
    }

    pub(super) fn loop_back(&mut self, start: f64) {
        self.loop_sought = Some(Instant::now());
        self.seek_to(Duration::from_secs_f64(start));
    }
}

/// A seek bar's right-click menu: a loop from or to the point clicked on `line`. Not for a
/// jam's guests, whose playback is the host's.
pub(super) fn loop_menu(app: &mut App, ui: &Ui, resp: &Response, line: Rect, total: f32, pos: f32) {
    if total <= 0.0 || app.jam.guest() {
        return;
    }
    // The point clicked, kept for the song it was clicked on.
    let at_id = resp.id.with(("loop-at", app.playing_id()));
    if resp.secondary_clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let t = ((p.x - line.left()) / line.width()).clamp(0.0, 1.0) * total;
        ui.ctx().data_mut(|d| d.insert_temp(at_id, t));
    }
    let looping = app.ab_loop().is_some();
    widgets::right_click_menu(resp).show(|ui| {
        // Its own width: a menu is otherwise as wide as what it's on, here the whole bar.
        widgets::menu_body(ui, 190.0);
        let t = ui.ctx().data(|d| d.get_temp::<f32>(at_id)).unwrap_or(pos);
        if widgets::menu_action(ui, Some(icon::REPEAT), crate::tr!("ui-loop-start")) {
            app.actions.push(Action::LoopStart(t));
        }
        if widgets::menu_action(ui, Some(icon::REPEAT), crate::tr!("ui-loop-end")) {
            app.actions.push(Action::LoopEnd(t));
        }
        if looping && widgets::menu_action(ui, Some(icon::X), crate::tr!("ui-loop-stop")) {
            app.actions.push(Action::LoopClear);
        }
    });
}

/// The loop's stretch on a seek bar's `line`: lit along it in `mark`, with a tick at each
/// end it has. The ticks have a dark edge, to show on a played part filled in `mark` too.
/// While it loops, the part played before its start (at `pos`) is dimmed: not the loop's.
pub(super) fn paint_loop(app: &App, ui: &Ui, line: Rect, total: f32, pos: f32, radius: f32, mark: Color32) {
    let Some(l) = app.ab_loop().filter(|_| total > 0.0) else { return };
    let x = |s: f64| line.left() + line.width() * (s as f32 / total).clamp(0.0, 1.0);
    let tick = |at: f32| {
        let r = Rect::from_center_size(pos2(at, line.center().y), vec2(2.0, line.height() + 8.0));
        ui.painter().rect_filled(r, 1.0, theme::with_alpha(mark, 0.9));
        ui.painter().rect_stroke(r, 1.0, egui::Stroke::new(1.0, theme::shade(0.35)), egui::StrokeKind::Outside);
    };
    // Looping: to the end it goes back from, kept clear of a crossfade.
    if let Some((_, end)) = app.ab_stretch() {
        let before = Rect::from_min_max(line.min, pos2(x(l.start).min(x(pos as f64)), line.bottom()));
        if before.width() > 0.0 {
            ui.painter().rect_filled(before, radius, theme::shade(0.55));
        }
        let band = Rect::from_min_max(pos2(x(l.start), line.top()), pos2(x(end), line.bottom()));
        ui.painter().rect_filled(band, radius, theme::with_alpha(mark, 0.35));
        tick(x(end));
    }
    tick(x(l.start));
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
