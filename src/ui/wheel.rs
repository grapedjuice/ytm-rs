//! Smooth scrolling, between the window's wheel events and egui.
//!
//! Mouse-wheel notches (line units) are collected and replayed as small pixel steps with
//! an exponential ease-out, like browsers do. Touchpads report smooth pixel deltas, which
//! pass through as they come; when the fingers lift mid-swipe the content coasts on at the
//! swipe's speed and slows to a stop, as in GTK, Firefox and Chromium. Wayland leaves that
//! to the app (#124: without it, flicking the touchpad barely moved the page); macOS sends
//! its own momentum events, so it doesn't coast here. Ctrl+wheel zoom passes untouched.

use egui::{Event, Modifiers, MouseWheelSource, MouseWheelUnit, TouchPhase, Vec2};

/// How far one wheel notch scrolls, in points.
const NOTCH_PX: f32 = 70.0;
/// A wheel glide's time constant, seconds (settles in ~0.18 s).
const GLIDE: f32 = 0.06;
/// A touchpad coast's time constant, seconds: it goes the lift-off speed × this far.
/// GTK 4's kinetic scrolling slows the same way (friction 4/s).
const COAST: f32 = 0.25;
/// How much of a swipe's end sets its lift-off speed, seconds (GTK 4 uses 150 ms too).
const SPEED_WINDOW: f64 = 0.15;

/// The fastest a lift-off can coast at, points per second (a hard flick).
const MAX_SPEED: f32 = 6000.0;

pub(super) struct Glide {
    /// Points still to scroll, handed out a frame's share at a time.
    pending: Vec2,
    /// The current glide's time constant, seconds.
    tau: f32,
    mods: Modifiers,
    /// Whether `pending` is a touchpad coast, rather than a wheel glide.
    coasting: bool,
    /// The touchpad swipe's last moves (frame time, time since the frame before, points),
    /// for its lift-off speed.
    swipe: Vec<(f64, f32, Vec2)>,
    /// The last frame's time.
    last_time: f64,
    /// Whether a touchpad swipe coasts on after the fingers lift.
    kinetic: bool,
}

impl Default for Glide {
    fn default() -> Self {
        Self {
            pending: Vec2::ZERO,
            tau: GLIDE,
            mods: Modifiers::NONE,
            coasting: false,
            swipe: Vec::new(),
            last_time: f64::NEG_INFINITY,
            kinetic: !cfg!(target_os = "macos"),
        }
    }
}

impl Glide {
    /// Rewrites one frame's `events`, at frame time `time` (seconds). `predicted_dt` paces
    /// a wheel glide, as it always has. Returns whether a glide is still going, which
    /// needs another frame.
    pub(super) fn filter(&mut self, events: &mut Vec<Event>, time: f64, predicted_dt: f32, viewport_h: f32) -> bool {
        // How long this frame's events took to come in. A swipe's first frame is woken by
        // its first event, so however long the window sat idle before, it counts one frame.
        let gap = ((time - self.last_time) as f32).clamp(0.001, SPEED_WINDOW as f32);
        let first_gap = gap.min(predicted_dt.max(0.001));
        self.last_time = time;
        for e in std::mem::take(events) {
            let Event::MouseWheel { unit, delta, phase, modifiers, source } = e else {
                // A click or a key press stops a coast. On Wayland, fingers resting on the
                // touchpad send nothing, so that's the other way to stop one.
                if matches!(e, Event::PointerButton { pressed: true, .. } | Event::Key { pressed: true, .. }) {
                    self.stop_coast();
                }
                events.push(e);
                continue;
            };
            if modifiers.command || modifiers.ctrl {
                // Zooming stops a coast, which would otherwise pick up the Ctrl and zoom on.
                self.stop_coast();
                events.push(e);
                continue;
            }
            let px = match unit {
                MouseWheelUnit::Line => delta * NOTCH_PX,
                MouseWheelUnit::Page => delta * viewport_h * 0.8,
                MouseWheelUnit::Point if !self.kinetic => {
                    events.push(e);
                    continue;
                }
                MouseWheelUnit::Point => {
                    match phase {
                        TouchPhase::Start => {
                            // Fingers on the touchpad stop a coast (a wheel glide plays
                            // out). egui ignores a Start's delta, so it goes on as a move
                            // of its own.
                            self.stop_coast();
                            self.swipe.clear();
                            self.swipe.push((time, first_gap, delta));
                            events.push(e);
                            events.push(Event::MouseWheel { unit, delta, phase: TouchPhase::Move, modifiers, source });
                        }
                        TouchPhase::Move => {
                            // A swipe right after a mouse-wheel notch comes without a Start.
                            if self.swipe.is_empty() {
                                self.stop_coast();
                            }
                            self.swipe.retain(|(t, ..)| *t > time - SPEED_WINDOW);
                            let gap = if self.swipe.is_empty() { first_gap } else { gap };
                            self.swipe.push((time, gap, delta));
                            events.push(e);
                        }
                        // The fingers lifted: coast. The End itself stays out of egui, which
                        // would drop this frame's moves with it and smooth the coast. What's
                        // left of a wheel glide goes on with it.
                        TouchPhase::End => {
                            let coast = delta + lift_off_speed(&self.swipe, time) * COAST;
                            if coast.length() >= 0.5 {
                                self.pending += coast;
                                self.tau = COAST;
                                self.coasting = true;
                                self.mods = modifiers;
                            }
                            self.swipe.clear();
                        }
                        TouchPhase::Cancel => {
                            self.swipe.clear();
                            events.push(e);
                        }
                    }
                    continue;
                }
            };
            // A notch stops a coast rather than speeding up the rest of it. Reversing
            // direction cancels the remaining glide instead of fighting it.
            self.stop_coast();
            if self.pending.y * px.y < 0.0 || self.pending.x * px.x < 0.0 {
                self.pending = Vec2::ZERO;
            }
            self.pending += px;
            self.tau = GLIDE;
            self.coasting = false;
            self.mods = modifiers;
        }
        if self.pending.length() < 0.5 {
            self.coasting = false;
            self.pending = Vec2::ZERO;
            return false;
        }
        // A coast slows down in real time, however fast the frames come.
        let dt = if self.coasting { gap.min(0.05) } else { predicted_dt.clamp(0.001, 0.05) };
        let step = self.pending * (1.0 - (-dt / self.tau).exp());
        self.pending -= step;
        events.push(Event::MouseWheel {
            unit: MouseWheelUnit::Point,
            delta: step,
            phase: TouchPhase::Move,
            modifiers: self.mods,
            source: MouseWheelSource::Wheel,
        });
        true
    }

    fn stop_coast(&mut self) {
        if self.coasting {
            self.pending = Vec2::ZERO;
            self.coasting = false;
        }
    }
}

/// A swipe's speed at lift-off at `time`, points per second: its moves over the last
/// [`SPEED_WINDOW`], so fingers that stopped before lifting don't coast, over the time
/// they took to come in (the oldest one over the frame gap before it). `swipe` is in time
/// order.
fn lift_off_speed(swipe: &[(f64, f32, Vec2)], time: f64) -> Vec2 {
    let Some(i) = swipe.iter().position(|(t, ..)| *t > time - SPEED_WINDOW) else { return Vec2::ZERO };
    let (first, gap, _) = swipe[i];
    let speed = swipe[i..].iter().fold(Vec2::ZERO, |sum, (.., d)| sum + *d) / ((time - first) as f32 + gap);
    if speed.length() > MAX_SPEED { speed * (MAX_SPEED / speed.length()) } else { speed }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;

    const FRAME: f32 = 1.0 / 60.0;

    fn wheel(unit: MouseWheelUnit, phase: TouchPhase, y: f32) -> Event {
        Event::MouseWheel { unit, delta: vec2(0.0, y), phase, modifiers: Modifiers::NONE, source: MouseWheelSource::Unknown }
    }

    fn pad(phase: TouchPhase, y: f32) -> Event {
        wheel(MouseWheelUnit::Point, phase, y)
    }

    /// A glide that coasts, with a frame already drawn at time 0.
    fn glide() -> Glide {
        let mut g = Glide { kinetic: true, ..Glide::default() };
        g.filter(&mut Vec::new(), 0.0, FRAME, 800.0);
        g
    }

    /// Runs one frame of `events` and returns how far egui is told to scroll, counting
    /// the delta a Start carries as egui does (not at all).
    fn frame(g: &mut Glide, mut events: Vec<Event>, time: f64) -> f32 {
        g.filter(&mut events, time, FRAME, 800.0);
        events.iter().map(|e| match e {
            Event::MouseWheel { delta, phase: TouchPhase::Move, .. } => delta.y,
            _ => 0.0,
        }).sum()
    }

    /// Runs frames until the glide stops (at most 10 s of them); returns the distance.
    fn settle(g: &mut Glide, mut time: f64) -> f32 {
        let mut total = 0.0;
        for _ in 0..600 {
            time += f64::from(FRAME);
            let mut events = Vec::new();
            if !g.filter(&mut events, time, FRAME, 800.0) {
                break;
            }
            total += events.iter().map(|e| if let Event::MouseWheel { delta, .. } = e { delta.y } else { 0.0 }).sum::<f32>();
        }
        total
    }

    #[test]
    fn a_wheel_notch_glides_its_whole_distance() {
        let mut g = glide();
        let first = frame(&mut g, vec![wheel(MouseWheelUnit::Line, TouchPhase::Move, -1.0)], 1.0);
        // The first step is a frame's share, as before: predicted_dt paces a wheel glide.
        let share = NOTCH_PX * (1.0 - (-FRAME / GLIDE).exp());
        assert!((first + share).abs() < 0.01, "{first}");
        let total = first + settle(&mut g, 1.0);
        assert!((total + NOTCH_PX).abs() < 0.5, "{total}");
    }

    /// A quick flick on a window idle for a second: 1200 pt/s, 20 pt a frame for six
    /// frames, with the fingers lifting at `lift` (s) after the last move. Returns
    /// (distance while touching, coast after).
    fn swipe(lift: f64) -> (f32, f32) {
        let mut g = glide();
        let mut t = 1.0;
        let mut touching = frame(&mut g, vec![pad(TouchPhase::Start, -20.0)], t);
        for _ in 0..5 {
            t += f64::from(FRAME);
            touching += frame(&mut g, vec![pad(TouchPhase::Move, -20.0)], t);
        }
        // Wayland's stop carries no distance; the coast starts in its frame.
        t += lift;
        let coast = frame(&mut g, vec![pad(TouchPhase::End, 0.0)], t);
        (touching, coast + settle(&mut g, t))
    }

    #[test]
    fn a_touchpad_swipe_scrolls_every_move_then_coasts() {
        let (touching, coast) = swipe(0.0);
        // Every move counts, the Start's too: egui would drop that one.
        assert!((touching + 120.0).abs() < 0.5, "{touching}");
        // Then about 1200 pt/s × COAST more, in the same direction: the idle second
        // before the swipe doesn't slow it.
        assert!((coast + 1200.0 * COAST).abs() < 20.0, "{coast}");
    }

    #[test]
    fn fingers_that_stop_before_lifting_dont_coast() {
        let (touching, coast) = swipe(0.5);
        assert!((touching + 120.0).abs() < 0.5, "{touching}");
        assert_eq!(coast, 0.0);
    }

    #[test]
    fn a_swipe_held_up_by_a_slow_frame_coasts_at_its_real_speed() {
        // 25 pt in the first frame, then 125 pt that piled up over a 120 ms frame:
        // 150 pt over 120 ms and a frame, not 125 pt per 1/60 s.
        let mut g = glide();
        let mut moved = frame(&mut g, vec![pad(TouchPhase::Start, -25.0)], 1.0);
        let rest = (0..5).map(|_| pad(TouchPhase::Move, -25.0)).chain([pad(TouchPhase::End, 0.0)]).collect();
        moved += frame(&mut g, rest, 1.12);
        // Past the fingers' 150 pt, the coast (which starts in the slow frame).
        let coast = moved + settle(&mut g, 1.12) + 150.0;
        let speed = 150.0 / (0.12 + FRAME);
        assert!((coast + speed * COAST).abs() < 20.0, "{coast}");
    }

    #[test]
    fn a_whole_swipe_in_one_frame_coasts_no_faster_than_a_hard_flick() {
        // It can't tell how long the swipe took, so it counts a frame: the cap holds it.
        let mut g = glide();
        let swipe = [pad(TouchPhase::Start, -25.0)].into_iter().chain((0..5).map(|_| pad(TouchPhase::Move, -25.0))).chain([pad(TouchPhase::End, 0.0)]).collect();
        let coast = frame(&mut g, swipe, 1.0) + 150.0 + settle(&mut g, 1.0);
        assert!((coast + MAX_SPEED * COAST).abs() < 20.0, "{coast}");
    }

    /// A touchpad coasting downwards at time 0.1: 40 pt in a frame and 25 ms, so about
    /// 150 pt to go.
    fn coasting() -> Glide {
        let mut g = glide();
        frame(&mut g, vec![pad(TouchPhase::Start, -20.0)], 0.05);
        frame(&mut g, vec![pad(TouchPhase::Move, -20.0)], 0.075);
        frame(&mut g, vec![pad(TouchPhase::End, 0.0)], 0.1);
        assert!(g.coasting && g.pending.y < -50.0, "{:?}", g.pending);
        g
    }

    #[test]
    fn touching_the_touchpad_stops_a_coast() {
        let mut g = coasting();
        assert_eq!(frame(&mut g, vec![pad(TouchPhase::Start, 0.0)], 0.15), 0.0);
        assert_eq!(settle(&mut g, 0.15), 0.0);
        // Or a swipe without a Start, as after a mouse-wheel notch.
        let mut g = coasting();
        assert_eq!(frame(&mut g, vec![pad(TouchPhase::Move, 0.0)], 0.15), 0.0);
        assert_eq!(settle(&mut g, 0.15), 0.0);
    }

    #[test]
    fn a_click_or_a_key_press_stops_a_coast() {
        let click = Event::PointerButton { pos: egui::pos2(10.0, 10.0), button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE };
        let key = Event::Key { key: egui::Key::ArrowDown, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
        for e in [click, key] {
            let mut g = coasting();
            let mut out = vec![e.clone()];
            assert!(!g.filter(&mut out, 0.15, FRAME, 800.0));
            assert_eq!(out, vec![e]);
            assert_eq!(settle(&mut g, 0.15), 0.0);
        }
        // Letting go of a button doesn't: the press already did, or there was no coast.
        let mut g = coasting();
        let release = Event::PointerButton { pos: egui::pos2(10.0, 10.0), button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE };
        assert!(g.filter(&mut vec![release], 0.15, FRAME, 800.0));
    }

    #[test]
    fn a_click_or_a_key_press_leaves_a_wheel_glide_alone() {
        let mut g = glide();
        let first = frame(&mut g, vec![wheel(MouseWheelUnit::Line, TouchPhase::Move, -1.0)], 1.0);
        let key = Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
        let total = first + frame(&mut g, vec![key], 1.0 + f64::from(FRAME)) + settle(&mut g, 1.0 + f64::from(FRAME));
        assert!((total + NOTCH_PX).abs() < 0.5, "{total}");
    }

    #[test]
    fn touching_the_touchpad_leaves_a_wheel_glide_alone() {
        // A notch, then a swipe starting while it glides: both scroll their whole way.
        let mut g = glide();
        let mut total = frame(&mut g, vec![wheel(MouseWheelUnit::Line, TouchPhase::Move, -1.0)], 1.0);
        let t = 1.0 + f64::from(FRAME);
        total += frame(&mut g, vec![pad(TouchPhase::Start, -10.0)], t);
        // The fingers stop a while before lifting, so the swipe itself doesn't coast.
        total += frame(&mut g, vec![pad(TouchPhase::End, 0.0)], t + 0.5);
        total += settle(&mut g, t + 0.5);
        assert!((total + NOTCH_PX + 10.0).abs() < 0.5, "{total}");
    }

    #[test]
    fn a_wheel_notch_stops_a_coast_and_glides_alone() {
        let mut g = coasting();
        let total = frame(&mut g, vec![wheel(MouseWheelUnit::Line, TouchPhase::Move, -1.0)], 0.15) + settle(&mut g, 0.15);
        assert!((total + NOTCH_PX).abs() < 0.5, "{total}");
    }

    #[test]
    fn zooming_stops_a_coast_and_passes_untouched() {
        let mut g = coasting();
        let zoom = vec![Event::MouseWheel { unit: MouseWheelUnit::Line, delta: vec2(0.0, 1.0), phase: TouchPhase::Move, modifiers: Modifiers::CTRL, source: MouseWheelSource::Wheel }];
        let mut out = zoom.clone();
        assert!(!g.filter(&mut out, 0.15, FRAME, 800.0));
        assert_eq!(out, zoom);
    }

    #[test]
    fn without_kinetic_touchpad_events_pass_untouched() {
        let mut g = Glide { kinetic: false, ..Glide::default() };
        let events = vec![pad(TouchPhase::Start, -5.0), pad(TouchPhase::Move, -5.0), pad(TouchPhase::End, 0.0)];
        let mut out = events.clone();
        assert!(!g.filter(&mut out, 0.0, FRAME, 800.0));
        assert_eq!(out, events);
    }
}
