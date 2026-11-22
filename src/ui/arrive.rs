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
    if theme::motion() <= 0.0 {
        return None;
    }
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
    let k = f64::from(theme::motion());
    if k <= 0.0 {
        return 1.0;
    }
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
    if p >= 1.0 {
        return add(ui);
    }
    ui.ctx().request_repaint();
    let layer = ui.layer_id();
    let start = ui.painter().add(Shape::Noop);
    let out = ui.scope(|ui| {
        ui.multiply_opacity(smoothstep(0.0, 0.85, p));
        add(ui)
    });
    let dy = rise * (1.0 - ease_out_quint(p));
    if dy > 0.0 {
        ui.ctx().graphics_mut(|g| {
            let list = g.entry(layer);
            let end = list.next_idx();
            list.transform_range(start, end, TSTransform::from_translation(vec2(0.0, dy)));
        });
    }
    out.inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_start_one_after_another_and_stop_at_the_cap() {
        let k = f64::from(theme::motion());
        assert_eq!(progress(0.0, 0, 0.0), 0.0);
        assert!(progress(0.0, 0, STEP * k) > 0.0);
        assert_eq!(progress(0.0, 1, STEP * k), 0.0);
        assert_eq!(progress(0.0, 0, FADE * k), 1.0);
        // The 20th starts with the 14th.
        assert_eq!(progress(0.0, 20, 1.0), progress(0.0, MAX_SEQ, 1.0));
    }
}
