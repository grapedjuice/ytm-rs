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

pub(super) struct Glide {
    /// Points still to scroll, handed out a frame's share at a time.
    pending: Vec2,
    /// The current glide's time constant, seconds.
    tau: f32,
    mods: Modifiers,
    /// The touchpad swipe's last moves (frame time, points), for its lift-off speed.
    swipe: Vec<(f64, Vec2)>,
    /// Whether a touchpad swipe coasts on after the fingers lift.
    kinetic: bool,
}

impl Default for Glide {
    fn default() -> Self {
        Self { pending: Vec2::ZERO, tau: GLIDE, mods: Modifiers::NONE, swipe: Vec::new(), kinetic: !cfg!(target_os = "macos") }
    }
}

impl Glide {
    /// Rewrites one frame's `events`, frame time `time` and length `dt` (seconds). Returns
    /// whether a glide is still going, which needs another frame.
    pub(super) fn filter(&mut self, events: &mut Vec<Event>, time: f64, dt: f32, viewport_h: f32) -> bool {
        let dt = dt.clamp(0.001, 0.05);
        for e in std::mem::take(events) {
            let Event::MouseWheel { unit, delta, phase, modifiers, source } = e else {
                events.push(e);
                continue;
            };
            if modifiers.command || modifiers.ctrl {
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
                            // Fingers on the touchpad stop a coast. egui ignores a Start's
                            // delta, so it goes on as a move of its own.
                            self.pending = Vec2::ZERO;
                            self.swipe.clear();
                            self.swipe.push((time, delta));
                            events.push(e);
                            events.push(Event::MouseWheel { unit, delta, phase: TouchPhase::Move, modifiers, source });
                        }
                        TouchPhase::Move => {
                            self.swipe.retain(|(t, _)| *t > time - SPEED_WINDOW);
                            self.swipe.push((time, delta));
                            events.push(e);
                        }
                        // The fingers lifted: coast. The End itself stays out of egui, which
                        // would drop this frame's moves with it and smooth the coast.
                        TouchPhase::End => {
                            self.pending = delta + lift_off_speed(&self.swipe, time, dt) * COAST;
                            self.tau = COAST;
                            self.mods = modifiers;
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
            // Reversing direction cancels the remaining glide instead of fighting it.
            if self.pending.y * px.y < 0.0 || self.pending.x * px.x < 0.0 {
                self.pending = Vec2::ZERO;
            }
            self.pending += px;
            self.tau = GLIDE;
            self.mods = modifiers;
        }
        if self.pending.length() < 0.5 {
            self.pending = Vec2::ZERO;
            return false;
        }
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
}

/// A swipe's speed at lift-off at `time`, points per second: its moves over the last
/// [`SPEED_WINDOW`], so fingers that stopped before lifting don't coast. `dt` is the
/// frame's length, as the oldest move came in over the frame before it.
fn lift_off_speed(swipe: &[(f64, Vec2)], time: f64, dt: f32) -> Vec2 {
    let recent = swipe.iter().filter(|(t, _)| *t > time - SPEED_WINDOW);
    let Some(first) = recent.clone().map(|(t, _)| *t).reduce(f64::min) else { return Vec2::ZERO };
    recent.fold(Vec2::ZERO, |sum, (_, d)| sum + *d) / ((time - first) as f32 + dt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;

    const FRAME: f32 = 1.0 / 60.0;

    fn wheel(unit: MouseWheelUnit, phase: TouchPhase, y: f32) -> Event {
        Event::MouseWheel { unit, delta: vec2(0.0, y), phase, modifiers: Modifiers::NONE, source: MouseWheelSource::Unknown }
    }

    fn touchpad() -> Glide {
        Glide { kinetic: true, ..Glide::default() }
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
        let mut g = touchpad();
        let first = frame(&mut g, vec![wheel(MouseWheelUnit::Line, TouchPhase::Move, -1.0)], 0.0);
        assert!(first < 0.0 && first > -NOTCH_PX, "{first}");
        let total = first + settle(&mut g, 0.0);
        assert!((total + NOTCH_PX).abs() < 0.5, "{total}");
    }

    /// A swipe at 1200 pt/s, 20 pt a frame, with the fingers lifting at `lift` (s) after
    /// the last move. Returns (distance while touching, coast after).
    fn swipe(lift: f64) -> (f32, f32) {
        let mut g = touchpad();
        let mut touching = frame(&mut g, vec![wheel(MouseWheelUnit::Point, TouchPhase::Start, -20.0)], 0.0);
        let mut t = 0.0;
        for _ in 0..9 {
            t += f64::from(FRAME);
            touching += frame(&mut g, vec![wheel(MouseWheelUnit::Point, TouchPhase::Move, -20.0)], t);
        }
        // Wayland's stop carries no distance; the coast starts in its frame.
        t += lift;
        let coast = frame(&mut g, vec![wheel(MouseWheelUnit::Point, TouchPhase::End, 0.0)], t);
        (touching, coast + settle(&mut g, t))
    }

    #[test]
    fn a_touchpad_swipe_scrolls_every_move_then_coasts() {
        let (touching, coast) = swipe(0.0);
        // Every move counts, the Start's too: egui would drop that one.
        assert!((touching + 200.0).abs() < 0.5, "{touching}");
        // Then about 1200 pt/s × COAST more, in the same direction.
        assert!((coast + 1200.0 * COAST).abs() < 20.0, "{coast}");
    }

    #[test]
    fn fingers_that_stop_before_lifting_dont_coast() {
        let (touching, coast) = swipe(0.5);
        assert!((touching + 200.0).abs() < 0.5, "{touching}");
        assert_eq!(coast, 0.0);
    }

    #[test]
    fn touching_the_touchpad_stops_a_coast() {
        let mut g = touchpad();
        frame(&mut g, vec![wheel(MouseWheelUnit::Point, TouchPhase::Start, -20.0)], 0.0);
        frame(&mut g, vec![wheel(MouseWheelUnit::Point, TouchPhase::Move, -20.0)], 0.016);
        frame(&mut g, vec![wheel(MouseWheelUnit::Point, TouchPhase::End, 0.0)], 0.032);
        assert!(g.pending.y < -100.0, "{:?}", g.pending);
        let moved = frame(&mut g, vec![wheel(MouseWheelUnit::Point, TouchPhase::Start, 0.0)], 0.1);
        assert_eq!(moved, 0.0);
        assert_eq!(settle(&mut g, 0.1), 0.0);
    }

    #[test]
    fn without_kinetic_touchpad_and_zoom_events_pass_untouched() {
        let mut g = Glide { kinetic: false, ..Glide::default() };
        let events = vec![wheel(MouseWheelUnit::Point, TouchPhase::Start, -5.0), wheel(MouseWheelUnit::Point, TouchPhase::End, 0.0)];
        let mut out = events.clone();
        assert!(!g.filter(&mut out, 0.0, FRAME, 800.0));
        assert_eq!(out, events);
        let mut g = touchpad();
        let zoom = vec![Event::MouseWheel { unit: MouseWheelUnit::Line, delta: vec2(0.0, 1.0), phase: TouchPhase::Move, modifiers: Modifiers::CTRL, source: MouseWheelSource::Wheel }];
        let mut out = zoom.clone();
        assert!(!g.filter(&mut out, 0.0, FRAME, 800.0));
        assert_eq!(out, zoom);
    }
}
