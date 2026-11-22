//! A finger swiping up the player bar opens Now playing, and swiping down it (or Now
//! playing itself) closes it, as on phones. Only touches count: a mouse or trackpad never
//! sends them, so dragging with one does what it always did. A swipe that something there
//! took for itself (the seek line, the volume, the lyrics) is that thing's, not a swipe.

use egui::{Event, Pos2, Rect, TouchPhase};

use super::{Action, App};

/// How far a finger goes up or down for a swipe, points.
const DISTANCE: f32 = 40.0;
/// How much more up or down than sideways it goes, so a sideways drag isn't one.
const STEEP: f32 = 1.5;
/// How long a swipe takes at most, seconds: a finger resting on the bar and moving
/// later isn't one.
const LONGEST: f64 = 1.0;

/// A finger on the bar, between frames.
#[derive(Clone, Copy, Default)]
struct Swipe {
    /// Where and when it touched down on the bar.
    start: Option<(Pos2, f64)>,
    /// Something on the bar took the drag (the seek line, the volume).
    taken: bool,
}

impl Swipe {
    /// This frame's touches over `bar`, at `now`, with `dragging` whether something is
    /// being dragged: `Some(true)` for a swipe up, `Some(false)` for one down.
    fn feed(&mut self, events: &[Event], bar: Rect, now: f64, dragging: bool) -> Option<bool> {
        if self.start.is_some() && dragging {
            self.taken = true;
        }
        let mut swiped = None;
        for e in events {
            let Event::Touch { phase, pos, .. } = e else { continue };
            match phase {
                TouchPhase::Start => *self = Self { start: bar.contains(*pos).then_some((*pos, now)), taken: false },
                TouchPhase::Move => {}
                TouchPhase::End => {
                    if let Some((from, at)) = self.start.take()
                        && !self.taken
                        && now - at <= LONGEST
                    {
                        let d = *pos - from;
                        if d.y.abs() >= DISTANCE && d.y.abs() >= STEEP * d.x.abs() {
                            swiped = Some(d.y < 0.0);
                        }
                    }
                }
                TouchPhase::Cancel => self.start = None,
            }
        }
        // Something takes a drag on the frame the finger lands (the lyrics' scroll does),
        // and a quick flick has let go by the next.
        if self.start.is_some() && dragging {
            self.taken = true;
        }
        swiped
    }
}

/// Swipes over `area` this frame: one up opens Now playing, one down closes it. `key`
/// keeps each area's finger apart. A drag something there took for itself isn't a swipe,
/// except one taken by `under`, what only swallows input beneath (Now playing's sheet).
pub(super) fn swipe(app: &mut App, ctx: &egui::Context, area: Rect, key: &str, under: Option<egui::Id>) {
    let id = egui::Id::unique(("swipe", key));
    let (events, now) = ctx.input(|i| (i.events.clone(), i.time));
    let dragging = ctx.dragged_id().is_some_and(|d| Some(d) != under);
    if !events.iter().any(|e| matches!(e, Event::Touch { .. })) && ctx.data(|d| d.get_temp::<Swipe>(id)).is_none_or(|s| s.start.is_none()) {
        // No finger on the area: nothing to do (and nothing for a mouse, ever).
        return;
    }
    let mut swipe: Swipe = ctx.data(|d| d.get_temp(id)).unwrap_or_default();
    let swiped = swipe.feed(&events, area, now, dragging);
    ctx.data_mut(|d| d.insert_temp(id, swipe));
    match swiped {
        Some(true) if !app.np_open && app.current().is_some() => app.actions.push(Action::OpenNowPlaying(true)),
        Some(false) if app.np_open => app.actions.push(Action::OpenNowPlaying(false)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{TouchDeviceId, TouchId, pos2};

    const BAR: Rect = Rect { min: pos2(0.0, 500.0), max: pos2(800.0, 580.0) };

    fn touch(phase: TouchPhase, x: f32, y: f32) -> Event {
        Event::Touch { device_id: TouchDeviceId(1), id: TouchId(1), phase, pos: pos2(x, y), force: None }
    }

    /// A finger from (x, y) by (dx, dy) over `secs`, nothing else dragged.
    fn swipe(s: &mut Swipe, (x, y): (f32, f32), (dx, dy): (f32, f32), secs: f64) -> Option<bool> {
        let mut out = s.feed(&[touch(TouchPhase::Start, x, y)], BAR, 0.0, false);
        out = out.or(s.feed(&[touch(TouchPhase::Move, x + dx / 2.0, y + dy / 2.0)], BAR, secs / 2.0, false));
        out.or(s.feed(&[touch(TouchPhase::End, x + dx, y + dy)], BAR, secs, false))
    }

    #[test]
    fn up_opens_and_down_closes() {
        let mut s = Swipe::default();
        assert_eq!(swipe(&mut s, (300.0, 540.0), (5.0, -80.0), 0.2), Some(true));
        assert_eq!(swipe(&mut s, (300.0, 520.0), (-5.0, 60.0), 0.2), Some(false));
    }

    #[test]
    fn a_tap_a_sideways_drag_or_a_slow_one_is_not_a_swipe() {
        let mut s = Swipe::default();
        assert_eq!(swipe(&mut s, (300.0, 540.0), (2.0, -3.0), 0.1), None);
        assert_eq!(swipe(&mut s, (300.0, 540.0), (120.0, -50.0), 0.2), None);
        assert_eq!(swipe(&mut s, (300.0, 540.0), (0.0, -30.0), 0.2), None);
        assert_eq!(swipe(&mut s, (300.0, 540.0), (0.0, -100.0), 1.5), None);
    }

    #[test]
    fn only_from_the_bar() {
        let mut s = Swipe::default();
        assert_eq!(swipe(&mut s, (300.0, 300.0), (0.0, -100.0), 0.2), None);
        // Ending off the bar is fine: a swipe up leaves it.
        assert_eq!(swipe(&mut s, (300.0, 560.0), (0.0, -150.0), 0.2), Some(true));
    }

    #[test]
    fn a_drag_on_the_seek_line_or_volume_is_theirs() {
        let mut s = Swipe::default();
        s.feed(&[touch(TouchPhase::Start, 300.0, 505.0)], BAR, 0.0, false);
        s.feed(&[touch(TouchPhase::Move, 300.0, 470.0)], BAR, 0.1, true);
        assert_eq!(s.feed(&[touch(TouchPhase::End, 300.0, 420.0)], BAR, 0.2, false), None);
        // The next one is a swipe again.
        assert_eq!(swipe(&mut s, (300.0, 540.0), (0.0, -80.0), 0.2), Some(true));
    }

    #[test]
    fn a_flick_something_took_as_the_finger_landed_is_theirs() {
        let mut s = Swipe::default();
        // The lyrics' scroll takes the drag on the frame the finger lands; it's let go
        // by the next, when the finger already has.
        s.feed(&[touch(TouchPhase::Start, 300.0, 520.0)], BAR, 0.0, true);
        assert_eq!(s.feed(&[touch(TouchPhase::End, 300.0, 600.0)], BAR, 0.05, false), None);
        assert_eq!(swipe(&mut s, (300.0, 520.0), (0.0, 80.0), 0.2), Some(false));
    }

    #[test]
    fn a_mouse_sends_no_touches_so_never_swipes() {
        let mut s = Swipe::default();
        let mouse = [Event::PointerMoved(pos2(300.0, 540.0)), Event::PointerMoved(pos2(300.0, 400.0))];
        assert_eq!(s.feed(&mouse, BAR, 0.1, false), None);
        assert!(s.start.is_none());
    }

    #[test]
    fn a_cancelled_touch_is_not_a_swipe() {
        let mut s = Swipe::default();
        s.feed(&[touch(TouchPhase::Start, 300.0, 540.0)], BAR, 0.0, false);
        s.feed(&[touch(TouchPhase::Cancel, 300.0, 460.0)], BAR, 0.1, false);
        assert_eq!(s.feed(&[touch(TouchPhase::End, 300.0, 440.0)], BAR, 0.2, false), None);
    }
}
