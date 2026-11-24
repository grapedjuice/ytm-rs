//! The mouse wheel over a seek bar seeks, as it sets the volume over the volume, and so
//! do ← and →.
//!
//! A seek bar pointed at takes the frame's wheel events before smooth scrolling sees them,
//! so nothing scrolls under it and a touchpad flick has no coast to seek on with. The
//! position shown moves at once, as while dragging; the seek itself goes once the wheel
//! rests, so a fast spin is one seek, not dozens (an HLS or MP3 seek takes a while). The
//! position shown stays there until playback gets there, so turning on goes on from it.

use egui::{Event, MouseWheelSource, MouseWheelUnit, Pos2, Rect};

use super::wheel::{NOTCH_PX, PAUSE, TOUCHPAD_SCALE};
use super::{Action, App};

/// How far a wheel notch or an arrow key seeks, seconds: YouTube's arrow-key step. Fine enough to hear a
/// line again, and ten notches go most of a minute.
const STEP: f32 = 5.0;
/// A touchpad's points per notch: a swipe that would scroll a page by a notch seeks a step.
const NOTCH_PT: f32 = NOTCH_PX / TOUCHPAD_SCALE;
/// Less than this, a touchpad's drift, moves nothing, seconds.
const MIN: f32 = 1.0;
/// How long the wheel rests before the seek goes, seconds.
const SETTLE: f64 = 0.3;
/// How long the position shown waits for a seek to land before giving up on it, seconds.
const HOLD: f64 = 2.0;
/// How near playback counts as there, seconds.
const NEAR: f32 = 1.0;
/// How far short of the end the wheel stops, seconds: a seek to the very end finishes the
/// song, which a notch too many near the end shouldn't.
const END: f32 = 1.0;

pub(super) struct SeekWheel {
    /// The seek bar pointed at, last frame (set while drawing them).
    over: Option<Rect>,
    /// Seconds turned that haven't moved the position yet (under [`MIN`]).
    secs: f32,
    /// Where it's seeking to, shown until playback gets there; and the song it's in.
    target: Option<f32>,
    song: Option<String>,
    /// When the seek went, if it has.
    sent: Option<f64>,
    /// When the wheel last turned, and last turned a whole notch.
    last: f64,
    notched: f64,
}

impl Default for SeekWheel {
    fn default() -> Self {
        Self { over: None, secs: 0.0, target: None, song: None, sent: None, last: f64::NEG_INFINITY, notched: f64::NEG_INFINITY }
    }
}

impl SeekWheel {
    /// A seek bar drawn: pointed at, the wheel over it seeks next frame.
    pub(super) fn hover(&mut self, resp: &egui::Response) {
        if resp.hovered() {
            self.over = Some(self.over.map_or(resp.interact_rect, |r| r.union(resp.interact_rect)));
        }
    }

    /// Whether it has a position to show: one it's seeking to, or will.
    pub(super) fn seeking(&self) -> bool {
        self.target.is_some()
    }

    /// Takes the wheel events over the seek bar pointed at last frame out of one frame's
    /// `events`, at `time`, the pointer starting at `pointer`, and moves the target from `pos`
    /// within `0..=total`, short of its end (seconds). Ctrl+wheel zoom stays. Returns whether it took any.
    fn take(&mut self, events: &mut Vec<Event>, time: f64, mut pointer: Option<Pos2>, pos: f32, total: f32) -> bool {
        let Some(over) = self.over.take().filter(|_| total > 0.0) else { return false };
        // A notch anywhere in the frame makes it the wheel's, whatever order its events are in.
        if events.iter().any(|e| matches!(e, Event::MouseWheel { unit: MouseWheelUnit::Line | MouseWheelUnit::Page, modifiers, .. } if !(modifiers.command || modifiers.ctrl))) {
            self.notched = time;
        }
        // Drift from a turn that rested is gone, not added to the next.
        if time - self.last >= SETTLE {
            self.secs = 0.0;
        }
        let (before, mut turned) = (events.len(), false);
        events.retain(|e| {
            match e {
                Event::PointerMoved(p) => pointer = Some(*p),
                Event::PointerGone => pointer = None,
                _ => {}
            }
            let Event::MouseWheel { unit, delta, modifiers, source, .. } = e else { return true };
            if modifiers.command || modifiers.ctrl || !pointer.is_some_and(|p| over.contains(p)) {
                return true;
            }
            // macOS's coast after a flick: nothing scrolls under the bar, and nothing seeks.
            if *source == MouseWheelSource::Momentum {
                return false;
            }
            let secs = STEP
                * match unit {
                    MouseWheelUnit::Line | MouseWheelUnit::Page => delta.y,
                    // A high-resolution wheel's part notches, between its whole ones (see
                    // `wheel::Glide`): the notches count already.
                    MouseWheelUnit::Point if time - self.notched <= PAUSE => 0.0,
                    MouseWheelUnit::Point => delta.y / NOTCH_PT,
                };
            // A sideways swipe turns nothing.
            if secs != 0.0 {
                self.secs += secs;
                turned = true;
            }
            false
        });
        if turned {
            self.last = time;
            // Turning on from a seek sent takes more than drift too.
            if (self.target.is_some() && self.sent.is_none()) || self.secs.abs() >= MIN {
                let secs = std::mem::take(&mut self.secs);
                self.shift(secs, pos, total);
            }
        }
        events.len() != before
    }

    /// The target moved `secs` on from where it is, or from `pos`, within `0..=total`, short of its end.
    fn shift(&mut self, secs: f32, pos: f32, total: f32) {
        let from = self.target.unwrap_or(pos);
        // Already within `END` of the end, onward goes nowhere (and back still works).
        let last = (total - END).max(from.min(total));
        let t = (from + secs).clamp(0.0, last);
        // At the end, a seek there already sent isn't sent again, nor one to where
        // playback already is.
        if self.target != Some(t) && (self.target.is_some() || t != pos) {
            (self.target, self.sent) = (Some(t), None);
        }
    }

    /// ← or → (`back`) at `time`, playback at `pos` of `total`. A press with no seek under
    /// way seeks at once, returning where to; a held key's repeats, and presses while one
    /// is under way, move the target and seek once the key rests, as the wheel does.
    fn key(&mut self, back: bool, repeat: bool, time: f64, pos: f32, total: f32) -> Option<f32> {
        if total <= 0.0 {
            return None;
        }
        let idle = self.target.is_none();
        self.last = time;
        self.shift(if back { -STEP } else { STEP }, pos, total);
        if idle && !repeat && self.target.is_some() {
            self.sent = Some(time);
            return self.target;
        }
        None
    }

    /// The target to seek to, once the wheel has rested at `time`; drift too small to move
    /// is dropped.
    fn settled(&mut self, time: f64) -> Option<f32> {
        if self.sent.is_some() || time - self.last < SETTLE {
            return None;
        }
        self.secs = 0.0;
        self.sent = self.target.map(|_| time);
        self.target
    }

    /// Whether the seek sent has landed, playback being at `pos` at `time`, or given up on.
    fn landed(&self, time: f64, pos: f32) -> bool {
        self.sent.is_some_and(|sent| self.target.is_some_and(|t| (pos - t).abs() < NEAR) || time - sent >= HOLD)
    }

    /// How long until the wheel counts as rested, or the seek as landed, seconds.
    fn wait(&self, time: f64) -> Option<f32> {
        let until = match self.sent {
            Some(sent) => sent + HOLD,
            None if self.target.is_some() || self.secs != 0.0 => self.last + SETTLE,
            None => return None,
        };
        Some((until - time).max(0.0) as f32)
    }

    /// Whether something else moved the position shown (`shown`: a drag, a click) or the
    /// song changed under the target.
    fn stale(&self, shown: Option<f32>, song: Option<&str>) -> bool {
        self.target.is_some_and(|t| shown != Some(t) || self.song.as_deref() != song)
    }

    fn reset(&mut self) {
        (self.target, self.secs, self.sent) = (None, 0.0, None);
    }
}

impl App {
    fn seek_length(&self) -> f32 {
        let status = &self.audio.status;
        status.duration().map(|d| d.as_secs_f32()).or(self.current().and_then(|t| t.duration).map(|d| d as f32)).unwrap_or(0.0)
    }

    /// The target dropped; the position shown, if still its, back to playback's.
    fn seek_wheel_drop(&mut self) {
        if self.seek_drag == self.seek_wheel.target {
            self.seek_drag = None;
        }
        self.seek_wheel.reset();
    }

    /// The target, unless something else moved the position shown or the song changed.
    fn seek_wheel_keep(&mut self) {
        let song = self.current_id().map(str::to_owned);
        if self.seek_wheel.stale(self.seek_drag, song.as_deref()) {
            self.seek_wheel_drop();
        }
        self.seek_wheel.song = song;
    }

    /// A seek to `at`, seconds: one elsewhere than the wheel's (Previous back to the start,
    /// a jam guest's) ends the wheel's.
    pub(super) fn seek_wheel_seeked(&mut self, at: f32) {
        if self.seek_wheel.target.is_some_and(|t| (t - at).abs() > 0.01) {
            self.seek_wheel_drop();
        }
    }

    /// Before the frame: the wheel over a seek bar moves the position shown. While a button
    /// is held (a drag) the wheel does nothing there.
    pub(super) fn seek_by_wheel(&mut self, ctx: &egui::Context, events: &mut Vec<Event>, time: f64) {
        self.seek_wheel_keep();
        let total = self.seek_length();
        let pos = self.seek_drag.unwrap_or(self.audio.status.position_smooth().as_secs_f32()).min(total.max(0.0));
        let (down, pointer) = ctx.input(|i| (i.pointer.any_down(), i.pointer.latest_pos()));
        // Nor for a jam guest the host hasn't given control: the bar would move, then go
        // back once the host said no.
        if down || self.jam.guest_without_control() {
            self.seek_wheel.over = None;
        }
        if self.seek_wheel.take(events, time, pointer, pos, total) {
            // Nor does a page coasting under it scroll on.
            self.wheel.stop_coast();
            if self.seek_wheel.target.is_some() {
                self.seek_drag = self.seek_wheel.target;
            }
        }
    }

    /// ← or → pressed (`back`), held if `repeat`: 5 seconds back or on. Nothing playing,
    /// a live stream or a length not known yet has nowhere to go, nor does a queue played
    /// out (the player has stopped, and drops a seek).
    pub(super) fn seek_by_key(&mut self, ctx: &egui::Context, back: bool, repeat: bool) {
        // A drag on a seek bar has the position, as it does from the wheel.
        let (down, time) = ctx.input(|i| (i.pointer.any_down(), i.time));
        let played_out = self.ended && self.resume_at.is_none();
        if self.current().is_none() || played_out || down || self.jam.guest_without_control() {
            return;
        }
        let total = self.seek_length();
        let pos = self.seek_drag.unwrap_or(self.audio.status.position_smooth().as_secs_f32()).min(total.max(0.0));
        let now = self.seek_wheel.key(back, repeat, time, pos, total);
        if self.seek_wheel.target.is_some() {
            self.seek_drag = self.seek_wheel.target;
        }
        if let Some(t) = now {
            self.apply(Action::Seek(t));
        }
        // The seek goes, or the position shown goes back, from the next frame's `seek_wheel_rest`.
        ctx.request_repaint();
    }

    /// In the frame, after the actions and the jam's news (a song changed by them isn't seeked
    /// in): the seek once the wheel rests, and the position shown back to playback's once it's
    /// there.
    pub(super) fn seek_wheel_rest(&mut self, ctx: &egui::Context, now: f64) {
        self.seek_wheel_keep();
        if let Some(t) = self.seek_wheel.settled(now) {
            self.apply(Action::Seek(t));
        }
        if self.seek_wheel.landed(now, self.audio.status.position_smooth().as_secs_f32()) {
            self.seek_wheel_drop();
        }
        if let Some(wait) = self.seek_wheel.wait(now) {
            ctx.request_repaint_after_secs(wait);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Modifiers, TouchPhase, pos2, vec2};

    const BAR: Rect = Rect { min: pos2(0.0, 0.0), max: pos2(100.0, 10.0) };
    const IN: Option<Pos2> = Some(pos2(50.0, 5.0));

    fn wheel(unit: MouseWheelUnit, y: f32) -> Event {
        Event::MouseWheel { unit, delta: vec2(0.0, y), phase: TouchPhase::Move, modifiers: Modifiers::NONE, source: MouseWheelSource::Unknown }
    }

    fn notch(n: f32) -> Event {
        wheel(MouseWheelUnit::Line, n)
    }

    /// One frame over a seek bar at `pos` of a 200 s song; returns the target.
    fn turn(w: &mut SeekWheel, mut events: Vec<Event>, time: f64, pos: f32) -> Option<f32> {
        w.over = Some(BAR);
        assert!(w.take(&mut events, time, IN, pos, 200.0));
        assert!(events.is_empty(), "{events:?}");
        w.target
    }

    #[test]
    fn notches_seek_a_step_each_and_settle_once() {
        let mut w = SeekWheel::default();
        assert_eq!(turn(&mut w, vec![notch(1.0)], 1.0, 60.0), Some(65.0));
        // The position shown is the target now; another notch goes on from it.
        assert_eq!(turn(&mut w, vec![notch(1.0), notch(1.0)], 1.1, 65.0), Some(75.0));
        assert_eq!(turn(&mut w, vec![notch(-1.0)], 1.2, 75.0), Some(70.0));
        assert_eq!(w.settled(1.2 + SETTLE - 0.01), None);
        assert_eq!(w.wait(1.3), Some((SETTLE - 0.1) as f32));
        assert_eq!(w.settled(1.2 + SETTLE), Some(70.0));
        assert_eq!(w.settled(5.0), None);
    }

    #[test]
    fn the_position_shown_waits_for_the_seek_to_land() {
        let mut w = SeekWheel::default();
        turn(&mut w, vec![notch(1.0)], 1.0, 60.0);
        assert_eq!(w.settled(2.0), Some(65.0));
        // A slow seek, playback still where it was: another turn goes on from the target.
        assert!(!w.landed(2.5, 60.0));
        assert_eq!(turn(&mut w, vec![notch(1.0)], 2.5, 65.0), Some(70.0));
        assert!(!w.landed(5.0, 60.0));
        assert_eq!(w.settled(5.0), Some(70.0));
        assert!(w.landed(5.1, 70.2));
        // Or it never lands.
        assert!(!w.landed(5.0 + HOLD - 0.01, 60.0));
        assert_eq!(w.wait(5.0 + HOLD - 0.5), Some(0.5));
        assert!(w.landed(5.0 + HOLD, 60.0));
    }

    #[test]
    fn a_touchpad_seeks_in_proportion_past_its_drift() {
        let mut w = SeekWheel::default();
        // A Windows touchpad's fractional lines, a tenth of a notch at a time: nothing
        // until a second's worth, then every bit.
        assert_eq!(turn(&mut w, vec![notch(0.1)], 1.0, 60.0), None);
        assert_eq!(turn(&mut w, vec![notch(0.1)], 1.02, 60.0), Some(61.0));
        assert_eq!(turn(&mut w, vec![notch(0.1)], 1.04, 61.0), Some(61.5));
        // Points elsewhere, a notch's worth scrolled being a step's.
        let mut w = SeekWheel::default();
        let t = turn(&mut w, vec![wheel(MouseWheelUnit::Point, -2.0 * NOTCH_PT)], 1.0, 60.0).unwrap();
        assert!((t - 50.0).abs() < 1e-3, "{t}");
    }

    #[test]
    fn drift_too_small_to_move_is_dropped() {
        let mut w = SeekWheel::default();
        assert_eq!(turn(&mut w, vec![notch(0.1)], 1.0, 60.0), None);
        assert!(w.wait(1.0).is_some());
        assert_eq!(w.settled(1.0 + SETTLE), None);
        assert_eq!(w.wait(1.0 + SETTLE), None);
        assert_eq!(turn(&mut w, vec![notch(0.1)], 5.0, 60.0), None);
    }

    #[test]
    fn a_macos_coast_seeks_nothing_and_scrolls_nothing() {
        let mut w = SeekWheel::default();
        let coast = Event::MouseWheel { unit: MouseWheelUnit::Point, delta: vec2(0.0, 40.0), phase: TouchPhase::Move, modifiers: Modifiers::NONE, source: MouseWheelSource::Momentum };
        assert_eq!(turn(&mut w, vec![coast], 1.0, 60.0), None);
        assert_eq!(w.wait(1.0), None);
    }

    #[test]
    fn it_stops_at_the_start_and_the_end() {
        let mut w = SeekWheel::default();
        assert_eq!(turn(&mut w, vec![notch(-3.0)], 1.0, 4.0), Some(0.0));
        let mut w = SeekWheel::default();
        // A second short of the end, so the song doesn't finish.
        assert_eq!(turn(&mut w, vec![notch(10.0)], 1.0, 190.0), Some(199.0));
        // Back from there goes from there, not from past it.
        assert_eq!(turn(&mut w, vec![notch(-1.0)], 1.1, 199.0), Some(194.0));
    }

    #[test]
    fn in_the_last_second_onward_goes_nowhere() {
        let mut w = SeekWheel::default();
        assert_eq!(turn(&mut w, vec![notch(1.0)], 1.0, 199.5), None);
        assert_eq!(w.wait(1.0 + SETTLE), None);
        assert_eq!(w.settled(1.0 + SETTLE), None);
        assert_eq!(turn(&mut w, vec![notch(-1.0)], 2.0, 199.5), Some(194.5));
    }

    #[test]
    fn a_high_res_wheels_part_notches_dont_count_twice() {
        let mut w = SeekWheel::default();
        // A part notch ahead of the notch in its frame, then one later in the spin.
        assert_eq!(turn(&mut w, vec![wheel(MouseWheelUnit::Point, 1.25), notch(1.0)], 1.0, 60.0), Some(65.0));
        assert_eq!(turn(&mut w, vec![wheel(MouseWheelUnit::Point, 1.25)], 1.1, 65.0), Some(65.0));
    }

    #[test]
    fn the_wheel_scrolls_on_elsewhere_or_without_a_length() {
        let mut w = SeekWheel::default();
        let mut events = vec![notch(1.0)];
        assert!(!w.take(&mut events, 1.0, IN, 60.0, 200.0));
        assert_eq!(events.len(), 1);
        // A live stream, or a length not known yet.
        w.over = Some(BAR);
        assert!(!w.take(&mut events, 1.0, IN, 0.0, 0.0));
        assert_eq!(events.len(), 1);
        // Pointed at only counts for the next frame.
        assert_eq!(w.over, None);
    }

    #[test]
    fn the_pointer_moving_off_the_bar_takes_the_wheel_with_it() {
        let mut w = SeekWheel { over: Some(BAR), ..SeekWheel::default() };
        let mut events = vec![notch(1.0), Event::PointerMoved(pos2(50.0, 40.0)), notch(1.0), Event::PointerGone, notch(1.0)];
        assert!(w.take(&mut events, 1.0, IN, 60.0, 200.0));
        assert_eq!(events.len(), 4);
        assert_eq!(w.target, Some(65.0));
        // Already off it.
        let mut w = SeekWheel { over: Some(BAR), ..SeekWheel::default() };
        let mut events = vec![notch(1.0)];
        assert!(!w.take(&mut events, 1.0, Some(pos2(50.0, 40.0)), 60.0, 200.0));
    }

    #[test]
    fn zooming_passes_through() {
        let mut w = SeekWheel { over: Some(BAR), ..SeekWheel::default() };
        let zoom = Event::MouseWheel { unit: MouseWheelUnit::Line, delta: vec2(0.0, 1.0), phase: TouchPhase::Move, modifiers: Modifiers::CTRL, source: MouseWheelSource::Wheel };
        let mut events = vec![zoom.clone()];
        assert!(!w.take(&mut events, 1.0, IN, 60.0, 200.0));
        assert_eq!(events, vec![zoom]);
        assert_eq!(w.target, None);
    }

    #[test]
    fn something_else_moving_the_bar_or_a_new_song_makes_it_stale() {
        let mut w = SeekWheel { song: Some("a".into()), ..SeekWheel::default() };
        assert!(!w.stale(None, Some("b")));
        turn(&mut w, vec![notch(1.0)], 1.0, 60.0);
        assert!(!w.stale(Some(65.0), Some("a")));
        // A drag, a click clearing it, another song.
        assert!(w.stale(Some(30.0), Some("a")));
        assert!(w.stale(None, Some("a")));
        assert!(w.stale(Some(65.0), Some("b")));
    }

    #[test]
    fn nothing_turned_sends_nothing_again() {
        let mut w = SeekWheel::default();
        turn(&mut w, vec![notch(1.0)], 1.0, 60.0);
        assert_eq!(w.settled(2.0), Some(65.0));
        // A sideways swipe, then a touchpad's drift, while the seek lands.
        let sideways = Event::MouseWheel { unit: MouseWheelUnit::Point, delta: vec2(30.0, 0.0), phase: TouchPhase::Move, modifiers: Modifiers::NONE, source: MouseWheelSource::Unknown };
        assert_eq!(turn(&mut w, vec![sideways], 2.1, 65.0), Some(65.0));
        assert_eq!(turn(&mut w, vec![notch(0.1)], 2.2, 65.0), Some(65.0));
        assert_eq!(w.settled(3.0), None);
        // More than drift goes on from the target.
        assert_eq!(turn(&mut w, vec![notch(0.1)], 2.3, 65.0), Some(66.0));
        assert_eq!(w.settled(3.0), Some(66.0));
        // Drift a while apart doesn't add up.
        assert_eq!(turn(&mut w, vec![notch(0.1)], 3.1, 66.0), Some(66.0));
        assert_eq!(turn(&mut w, vec![notch(0.1)], 4.1, 66.0), Some(66.0));
        assert_eq!(w.settled(5.0), None);
    }

    #[test]
    fn an_arrow_key_seeks_a_step_at_once() {
        let mut w = SeekWheel::default();
        assert_eq!(w.key(false, false, 1.0, 60.0, 200.0), Some(65.0));
        // Sent already: resting doesn't send it again.
        assert_eq!(w.settled(2.0), None);
        assert!(w.landed(2.0, 65.1));
        w.reset();
        assert_eq!(w.key(true, false, 3.0, 65.0, 200.0), Some(60.0));
    }

    #[test]
    fn a_held_arrow_key_seeks_once_it_rests() {
        let mut w = SeekWheel::default();
        assert_eq!(w.key(false, false, 1.0, 60.0, 200.0), Some(65.0));
        // The repeats move the target from the last one, and nothing goes until they stop.
        for (n, time) in [1.5, 1.53, 1.56].into_iter().enumerate() {
            assert_eq!(w.key(false, true, time, 60.0, 200.0), None);
            assert_eq!(w.target, Some(65.0 + 5.0 * (n + 1) as f32));
        }
        assert_eq!(w.settled(1.56 + SETTLE - 0.01), None);
        assert_eq!(w.settled(1.56 + SETTLE), Some(80.0));
        // Quick presses while a seek is under way add up the same way.
        assert_eq!(w.key(true, false, 2.0, 60.0, 200.0), None);
        assert_eq!(w.key(true, false, 2.25, 60.0, 200.0), None);
        assert_eq!(w.settled(2.25 + SETTLE - 0.01), None);
        assert_eq!(w.settled(2.6), Some(70.0));
    }

    #[test]
    fn arrow_keys_stop_at_the_start_and_short_of_the_end() {
        let mut w = SeekWheel::default();
        assert_eq!(w.key(true, false, 1.0, 3.0, 200.0), Some(0.0));
        let mut w = SeekWheel::default();
        assert_eq!(w.key(false, false, 1.0, 197.0, 200.0), Some(199.0));
        // At the start, or in the last second, the way it goes goes nowhere.
        let mut w = SeekWheel::default();
        assert_eq!(w.key(true, false, 1.0, 0.0, 200.0), None);
        assert_eq!(w.key(false, false, 1.0, 199.5, 200.0), None);
        assert_eq!(w.wait(1.0), None);
        // Held into the end: the one seek there, once.
        let mut w = SeekWheel::default();
        assert_eq!(w.key(false, false, 1.0, 190.0, 200.0), Some(195.0));
        assert_eq!(w.key(false, true, 1.5, 190.0, 200.0), None);
        assert_eq!(w.key(false, true, 1.53, 190.0, 200.0), None);
        assert_eq!(w.target, Some(199.0));
        assert_eq!(w.settled(2.0), Some(199.0));
        assert_eq!(w.key(false, true, 2.1, 195.0, 200.0), None);
        assert_eq!(w.settled(3.0), None);
    }

    #[test]
    fn arrow_keys_do_nothing_without_a_length() {
        let mut w = SeekWheel::default();
        assert_eq!(w.key(false, false, 1.0, 0.0, 0.0), None);
        assert_eq!(w.target, None);
        assert_eq!(w.wait(1.0), None);
    }

    #[test]
    fn past_the_end_a_seek_sent_there_isnt_sent_again() {
        let mut w = SeekWheel::default();
        turn(&mut w, vec![notch(3.0)], 1.0, 190.0);
        assert_eq!(w.settled(2.0), Some(199.0));
        assert_eq!(turn(&mut w, vec![notch(1.0)], 2.1, 199.0), Some(199.0));
        assert_eq!(w.settled(3.0), None);
    }
}
