//! Things arriving on a page in a cascade: each comes in from a little below, fading in,
//! a moment after the one before it (the cards of a shelf, the rows of a list), as a
//! group first shows on the page. Navigating to a page plays it again.

use std::collections::HashMap;
use std::hash::Hash;

use egui::emath::TSTransform;
use egui::{Id, Rect, Shape, Ui, vec2};

use crate::theme;

/// One item's own fade, and how far apart two items of a group start, before
/// [`theme::motion`] (written at the app's usual speed, these run 0.56 s and 45 ms).
const FADE: f64 = 0.86;
const STEP: f64 = 0.07;
/// Past this many, items come in with the last: a long list doesn't keep arriving.
const MAX_SEQ: usize = 14;
/// A theme's slow fades slow the cascade, up to this much: a shelf still settles in
/// under two seconds.
const SLOWEST: f64 = 1.5;

/// [`theme::motion`], as far as the cascade follows it.
fn speed() -> f64 {
    f64::from(theme::motion()).min(SLOWEST)
}

/// When each group first showed, since the page was last navigated to.
#[derive(Clone, Default)]
struct Shown {
    page: f64,
    groups: HashMap<Id, f64>,
}

fn shown_id() -> Id {
    Id::unique("arrive-shown")
}

/// Once a frame, before the page: the time it was navigated to. A new one starts every
/// group over.
pub fn page(ctx: &egui::Context, changed: f64) {
    ctx.data_mut(|d| {
        let s = d.get_temp_mut_or_default::<Shown>(shown_id());
        if s.page != changed {
            *s = Shown { page: changed, groups: HashMap::new() };
        }
    });
}

/// A group of items about to be laid out where `ui`'s cursor is: when it first came into
/// view, None while it hasn't (what's laid out off screen isn't seen arriving).
pub fn group(ui: &Ui, key: impl Hash + std::fmt::Debug) -> Option<f64> {
    let id = Id::unique("arrive").with(key);
    let at = ui.ctx().data(|d| d.get_temp::<Shown>(shown_id()).and_then(|s| s.groups.get(&id).copied()));
    if at.is_some() {
        return at;
    }
    let top = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width().max(1.0), 24.0));
    if !ui.is_rect_visible(top) {
        return None;
    }
    let now = ui.input(|i| i.time);
    ui.ctx().data_mut(|d| d.get_temp_mut_or_default::<Shown>(shown_id()).groups.insert(id, now));
    Some(now)
}

/// How far along item `seq` of a group shown at `since` is, 0 to 1.
fn progress(since: f64, seq: usize, now: f64) -> f32 {
    let k = speed();
    let delay = seq.min(MAX_SEQ) as f64 * STEP * k;
    ((now - since - delay) / (FADE * k)).clamp(0.0, 1.0) as f32
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// `ease-out-quint`: most of the way there early, then settling.
fn ease_out_quint(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(5)
}

/// Lay out item `seq` of a group shown at `since` (from [`group`]), arriving: from `rise`
/// points below, fading in a little behind the movement so it settles before it's
/// fully there. Only what's drawn moves; it takes its place in the layout at once.
pub fn item<R>(ui: &mut Ui, since: Option<f64>, seq: usize, rise: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let Some(since) = since else { return add(ui) };
    let p = progress(since, seq, ui.input(|i| i.time));
    // Laid out in `ui` itself, arriving or not: its widgets keep their ids, so a press that
    // began as it came in still clicks and a menu opened on it stays open, and a wrapped
    // row still wraps (a child ui would leave the whole row on one line).
    if p >= 1.0 {
        return add(ui);
    }
    ui.ctx().request_repaint();
    let layer = ui.layer_id();
    let start = ui.painter().add(Shape::Noop);
    let dy = rise * (1.0 - ease_out_quint(p));
    // Moving the shapes down moves their clip with them: clipped as far up as they're
    // drawn down, the clip ends up where it was, and nothing shows past the page.
    let (clip, opacity) = (ui.clip_rect(), ui.opacity());
    ui.set_clip_rect(clip.translate(vec2(0.0, -dy)));
    ui.multiply_opacity(smoothstep(0.0, 0.85, p));
    let out = add(ui);
    ui.set_clip_rect(clip);
    ui.set_opacity(opacity);
    if dy > 0.0 {
        ui.ctx().graphics_mut(|g| {
            let list = g.entry(layer);
            let end = list.next_idx();
            list.transform_range(start, end, TSTransform::from_translation(vec2(0.0, dy)));
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_start_one_after_another_and_stop_at_the_cap() {
        let k = speed();
        assert_eq!(progress(0.0, 0, 0.0), 0.0);
        assert!(progress(0.0, 0, STEP * k) > 0.0);
        assert_eq!(progress(0.0, 1, STEP * k), 0.0);
        assert_eq!(progress(0.0, 0, FADE * k), 1.0);
        // The 20th starts with the 14th.
        assert_eq!(progress(0.0, 20, 1.0), progress(0.0, MAX_SEQ, 1.0));
    }

    #[test]
    fn a_wrapped_row_of_items_wraps_as_they_arrive_and_once_they_have() {
        let k = speed();
        for time in [0.1, FADE * k * 0.5, 10.0] {
            let ctx = egui::Context::default();
            let mut rects = Vec::new();
            let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(600.0, 2000.0))), time: Some(time), ..Default::default() };
            ctx.run_ui(raw, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(20.0, 22.0);
                    for i in 0..12 {
                        rects.push(item(ui, Some(0.0), i, 16.0, |ui| ui.allocate_exact_size(vec2(180.0, 238.0), egui::Sense::click()).0));
                    }
                });
            })
            .textures_delta
            .clear();
            assert!(rects.iter().all(|r| r.right() <= 600.0), "at {time}: past the edge: {rects:?}");
            let rows = rects.iter().map(|r| r.top() as i32).collect::<std::collections::BTreeSet<_>>();
            assert_eq!(rows.len(), 4, "at {time}: three to a row, four rows: {rects:?}");
        }
    }

    #[test]
    fn a_press_as_it_arrives_clicks_once_it_has() {
        use egui::{Event, Modifiers, PointerButton, RawInput, pos2};
        let ctx = egui::Context::default();
        let button = std::cell::Cell::new(Rect::NOTHING);
        let clicked = std::cell::Cell::new(false);
        let frame = |time: f64, events: Vec<Event>| {
            let raw = RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(400.0, 300.0))), time: Some(time), events, ..Default::default() };
            ctx.run_ui(raw, |ui| {
                let r = item(ui, Some(0.0), 0, 20.0, |ui| ui.button("Open"));
                button.set(r.rect);
                clicked.set(clicked.get() || r.clicked());
            })
            .textures_delta
            .clear();
        };
        let k = speed();
        frame(0.0, vec![]);
        let at = button.get().center();
        let press = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        // Pressed halfway in, let go just after it's all there.
        frame(FADE * k * 0.6, vec![Event::PointerMoved(at), press(true)]);
        frame(FADE * k * 0.6 + 0.1, vec![Event::PointerMoved(at)]);
        frame(FADE * k + 0.05, vec![press(false)]);
        frame(FADE * k + 0.1, vec![]);
        assert!(clicked.get(), "the press at {:?} was lost (button at {:?})", pos2(at.x, at.y), button.get());
    }
}
