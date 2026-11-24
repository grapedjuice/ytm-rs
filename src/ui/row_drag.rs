//! Dragging a song row to a new place in its list, or out of it onto a place that takes
//! songs (a playlist in the sidebar). Rows off screen aren't built, so the drag is kept
//! here rather than in a row's own widget state, and the place it's going comes from the
//! pointer and the rows' fixed height.

use egui::{Id, Pos2, Rect, Sense, Ui, Vec2, pos2, vec2};

use super::widgets::{paint_cover, row_height, text_at};
use crate::model::Track;
use crate::theme;

/// Within this far of the list's top or bottom edge, the list scrolls.
const SCROLL_EDGE: f32 = 56.0;
/// Scrolling speed at the very edge, in points a second.
const SCROLL_SPEED: f32 = 900.0;
/// How far left or right of the list a drop still counts.
const SIDE_MARGIN: f32 = 40.0;

pub struct RowDrag {
    list: Id,
    /// The row picked up.
    from: usize,
    track: Track,
    /// The frame its list last showed it: a list that went away ended the drag.
    seen: u64,
}

/// A row let go somewhere new: `gap` is the place between rows it goes to (0 is above
/// the first), counted like `from` in the list as it was. `id` is the song picked up,
/// for checking the list still has it there.
pub struct Dropped {
    pub from: usize,
    pub gap: usize,
    pub id: String,
}

/// Where a list's rows are. Rows are counted the list's own way, and only part of the
/// list may be on show: `first` is the row at `top`, and rows can land from the gap
/// `lowest` down to the gap `end`.
#[derive(Clone, Copy)]
pub struct Rows {
    pub top: f32,
    pub first: usize,
    pub lowest: usize,
    pub end: usize,
    pub accent: egui::Color32,
    /// Rows past row `.0` sit `.1` lower (the queue's space under the song playing).
    pub spaced: Option<(usize, f32)>,
}

impl Rows {
    /// The gap between rows nearest to `y`.
    fn gap_at(&self, y: f32, h: f32) -> usize {
        let y = match self.spaced {
            Some((after, px)) => {
                let edge = self.top + (after + 1).saturating_sub(self.first) as f32 * h;
                if y > edge + px { y - px } else { y.min(edge) }
            }
            None => y,
        };
        self.first + gap_at(y, self.top, h, self.lowest.saturating_sub(self.first), self.end.saturating_sub(self.first))
    }

    /// Where the line for gap `g` goes.
    fn gap_y(&self, g: usize, h: f32, spacing: f32) -> f32 {
        let extra = self.spaced.filter(|&(after, _)| g > after).map_or(0.0, |(_, px)| px);
        self.top + (g - self.first) as f32 * h + extra - spacing / 2.0
    }
}

/// Lets row `i` of `list` be picked up. Call just before building the row: its buttons
/// stay on top and clicks still reach them; only a real drag lands here. Not for a
/// finger, where dragging scrolls (see `finger`).
pub fn grip(ui: &mut Ui, drag: &mut Option<RowDrag>, list: Id, i: usize, t: &Track) {
    let id = list.with(("grip", i));
    let finger = finger(ui);
    let rect = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), row_height(ui)));
    // A finger's first swipe after the mouse came down on a grip still there from it, and
    // egui gave it the press: the grip stays for that swipe, and scrolls as a shelf does.
    if finger && ui.ctx().is_being_dragged(id) {
        super::widgets::shelf_drag(ui, &ui.interact(rect, id, Sense::DRAG), id.with("swipe"));
    } else if !finger {
        // egui counts a drag-only widget as dragged from the press itself: wait until the
        // pointer has moved too far (or held too long) for a click. A drag some other list
        // left behind gives way.
        let grip = ui.interact(rect, id, Sense::DRAG);
        let held = drag.as_ref().is_some_and(|d| d.list == list && d.from == i);
        if grip.dragged_by(egui::PointerButton::Primary) && !held && ui.input(|input| input.pointer.is_decidedly_dragging()) {
            *drag = Some(RowDrag { list, from: i, track: t.clone(), seen: ui.ctx().cumulative_frame_nr() });
        }
    }
    // The picked-up row stays dimmed in its old place.
    if drag.as_ref().is_some_and(|d| d.list == list && d.from == i) {
        ui.multiply_opacity(0.35);
    }
}

/// Whether the pointer is a finger, as last seen (see `note_pointer`). egui gives a press
/// to what was under it the frame before, so the grips have to be gone before a finger
/// comes down: one there would take the swipe, and the list wouldn't scroll (#190).
fn finger(ui: &Ui) -> bool {
    ui.data(|d| d.get_temp::<bool>(finger_id())).unwrap_or(false)
}

fn finger_id() -> Id {
    Id::unique("row-drag-finger")
}

/// Notes from each frame's `events` whether the pointer is a finger: a touch makes it one,
/// and the mouse moving (or a button without a touch) the mouse again. Every frame,
/// whatever is on screen: a list a finger opened (from the search box, say) knows it's a
/// finger before its first swipe.
pub fn note_pointer(ctx: &egui::Context, events: &[egui::Event]) {
    use egui::Event;
    let touched = events.iter().any(|e| matches!(e, Event::Touch { .. }));
    let pointed = events.iter().any(|e| matches!(e, Event::PointerMoved(_) | Event::PointerButton { .. }));
    if touched || pointed {
        ctx.data_mut(|d| d.insert_temp(finger_id(), touched));
    }
}

/// Call after the rows, inside their scroll area: shows where the drag in `list` would
/// land, scrolls near the edges, and ends it when let go.
pub fn feedback(ui: &mut Ui, drag: &mut Option<RowDrag>, list: Id, rows: Rows) -> Option<Dropped> {
    let (from, id) = keep(ui, drag, list)?;
    let (pos, down, dt) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.primary_down(), i.stable_dt));
    let clip = ui.clip_rect();
    let h = row_height(ui);
    let width = ui.max_rect().x_range();
    let over = pos.filter(|p| p.x >= width.min - SIDE_MARGIN && p.x <= width.max + SIDE_MARGIN);
    let gap = over.map(|p| rows.gap_at(p.y, h));
    let moves = gap.filter(|&g| g != from && g != from + 1);
    if !down {
        *drag = None;
        return moves.map(|gap| Dropped { from, gap, id });
    }
    if let Some(g) = moves {
        line(ui, rows.gap_y(g, h, ui.spacing().item_spacing.y), width, rows.accent);
    }
    if let Some(p) = over {
        let speed = edge_speed(p.y, clip.top(), clip.bottom());
        if speed != 0.0 {
            ui.scroll_with_delta(Vec2::new(0.0, speed * dt));
        }
    }
    follow(ui, drag, pos, width.span());
    None
}

/// Call after `feedback`, for a list that also takes songs dragged out of other lists (the
/// queue, from an album or search results): shows where the song held over it would go,
/// and gives the song and the gap it goes to once let go there. Call it before building the
/// list the drag came from, which would otherwise end the drag first.
pub fn receive(ui: &Ui, drag: &mut Option<RowDrag>, list: Id, rows: Rows) -> Option<(usize, Track)> {
    if !live(ui, drag) || drag.as_ref().is_some_and(|d| d.list == list) {
        return None;
    }
    let (pos, down, dt) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.primary_down(), i.stable_dt));
    let clip = ui.clip_rect();
    let p = pos.filter(|p| clip.contains(*p))?;
    let h = row_height(ui);
    let gap = rows.gap_at(p.y, h);
    if !down {
        return drag.take().map(|d| (gap, d.track));
    }
    line(ui, rows.gap_y(gap, h, ui.spacing().item_spacing.y), ui.max_rect().x_range(), rows.accent);
    let speed = edge_speed(p.y, clip.top(), clip.bottom());
    if speed != 0.0 {
        ui.scroll_with_delta(Vec2::new(0.0, speed * dt));
    }
    None
}

/// The line where a dropped song would go, if it's in sight.
fn line(ui: &Ui, y: f32, width: egui::Rangef, accent: egui::Color32) {
    if ui.clip_rect().y_range().contains(y) {
        // Over the song following the pointer, which is often right there.
        let line = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Debug, Id::unique("row-drag-line")));
        line.hline(width, y, egui::Stroke::new(2.0, accent));
        line.circle_filled(pos2(width.min + 3.0, y), 4.0, accent);
    }
}

/// Call after the rows of a list whose order can't change (an album, search results):
/// the song picked up follows the pointer to a place that takes it, and the drag ends
/// when let go anywhere else.
pub fn carry(ui: &Ui, drag: &mut Option<RowDrag>, list: Id) {
    if keep(ui, drag, list).is_none() {
        return;
    }
    let (pos, down) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.primary_down()));
    if !down {
        *drag = None;
        return;
    }
    follow(ui, drag, pos, ui.max_rect().width());
}

/// What a place that takes dragged songs sees of the drag.
pub enum Over {
    /// A song is held over it.
    Held,
    /// This song was let go on it.
    Dropped(Track),
}

/// For a place that takes dragged songs, such as a playlist in the sidebar: whether a song
/// is held over `rect`, or was let go there. Call it before building the list the drag came
/// from, which would otherwise end the drag first.
pub fn target(ui: &Ui, drag: &mut Option<RowDrag>, rect: Rect) -> Option<Over> {
    if !live(ui, drag) {
        return None;
    }
    let (pos, down) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.primary_down()));
    if !pos.is_some_and(|p| rect.intersect(ui.clip_rect()).contains(p)) {
        return None;
    }
    if down {
        return Some(Over::Held);
    }
    drag.take().map(|d| Over::Dropped(d.track))
}

/// Inside a scroll area of drop targets: scrolls it while a song is held near its top or
/// bottom edge, to reach the targets out of view.
pub fn scroll_targets(ui: &Ui, drag: &Option<RowDrag>) {
    if !live(ui, drag) {
        return;
    }
    let clip = ui.clip_rect();
    let Some(p) = ui.input(|i| i.pointer.latest_pos()).filter(|p| clip.contains(*p)) else { return };
    let speed = edge_speed(p.y, clip.top(), clip.bottom());
    if speed != 0.0 {
        ui.scroll_with_delta(Vec2::new(0.0, speed * ui.input(|i| i.stable_dt)));
    }
}

/// Whether a drag is going on: its list was shown last frame, or this one.
fn live(ui: &Ui, drag: &Option<RowDrag>) -> bool {
    drag.as_ref().is_some_and(|d| d.seen + 1 >= ui.ctx().cumulative_frame_nr())
}

/// The row and song of the drag in `list`, marked as seen this frame. A drag whose list
/// wasn't shown last frame is over.
fn keep(ui: &Ui, drag: &mut Option<RowDrag>, list: Id) -> Option<(usize, String)> {
    let frame = ui.ctx().cumulative_frame_nr();
    let d = drag.as_mut().filter(|d| d.list == list)?;
    if d.seen + 1 < frame {
        *drag = None;
        return None;
    }
    d.seen = frame;
    Some((d.from, d.track.id.clone()))
}

/// The song held follows the pointer, as wide as the list up to a point.
fn follow(ui: &Ui, drag: &Option<RowDrag>, pos: Option<Pos2>, width: f32) {
    if let (Some(p), Some(d)) = (pos, drag) {
        ghost(ui, d, p, width.min(320.0));
    }
    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    ui.ctx().request_repaint();
}

/// The song following the pointer.
fn ghost(ui: &Ui, d: &RowDrag, pointer: Pos2, width: f32) {
    egui::Area::new(Id::unique("row-drag-ghost")).order(egui::Order::Tooltip).interactable(false).fixed_pos(pointer + vec2(-28.0, -28.0)).show(ui.ctx(), |ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(width, 56.0), Sense::hover());
        ui.painter().rect_filled(rect, 8.0, theme::panel(0.92));
        let img = Rect::from_min_size(rect.min + vec2(8.0, 8.0), Vec2::splat(40.0));
        paint_cover(ui, img, &d.track.thumbs, theme::thumb_radius(), 1.0);
        let (x, w) = (img.right() + 10.0, rect.right() - img.right() - 18.0);
        text_at(ui, pos2(x, rect.top() + 10.0), &d.track.title, theme::semibold(14.0), theme::text(), w);
        text_at(ui, pos2(x, rect.top() + 30.0), &d.track.artist_line(), theme::regular(12.5), theme::text_dim(), w);
    });
}

/// The gap between rows nearest to `y`, between `lowest` and `end`.
fn gap_at(y: f32, top: f32, row: f32, lowest: usize, end: usize) -> usize {
    (((y - top) / row + 0.5).floor().max(0.0) as usize).clamp(lowest.min(end), end)
}

/// Scroll wanted with the pointer at `y`: up (positive) near the top, down near the
/// bottom, faster the closer to the edge and beyond it.
fn edge_speed(y: f32, top: f32, bottom: f32) -> f32 {
    let near = |dist: f32| ((SCROLL_EDGE - dist) / SCROLL_EDGE).clamp(0.0, 1.0);
    SCROLL_SPEED * (near(y - top) - near(bottom - y))
}

#[cfg(test)]
mod tests {
    use super::{SCROLL_EDGE, SCROLL_SPEED, edge_speed, gap_at};
    use super::{Dropped, Over, RowDrag, Rows, carry, feedback, grip, note_pointer, receive, target};
    use crate::model::Track;
    use crate::ui::widgets::{RowOpts, row_height, track_row, visible_rows};
    use egui::{Event, Modifiers, PointerButton, Pos2, Rect, RawInput, pos2, vec2};

    const WIDTH: f32 = 800.0;
    const HEIGHT: f32 = 600.0;

    fn song(i: usize) -> Track {
        Track { id: format!("song{i}"), title: format!("Song {i}"), artists: vec![], album: None, duration: Some(200), ..Default::default() }
    }

    /// A real list of real rows in a scroll area, run frame by frame on made-up pointer
    /// events: what egui does with a click or a drag over a row and its buttons.
    struct List {
        ctx: egui::Context,
        songs: Vec<Track>,
        drag: Option<RowDrag>,
        lowest: usize,
        shown: bool,
        time: f64,
        at: Pos2,
        top: f32,
        row: f32,
        offset: f32,
        clicked: Vec<usize>,
        dropped: Vec<Dropped>,
        /// Rows can change order; otherwise they're only carried out of the list.
        reorder: bool,
        /// A place that takes songs, built before the list as the sidebar is.
        target: Option<Rect>,
        held: bool,
        landed: Vec<String>,
        /// A list that takes songs dragged in (the queue), built before this one as the
        /// queue is, with 5 rows of its own; and what landed in it, at which gap.
        inbox: Option<Rect>,
        received: Vec<(usize, String)>,
    }

    impl List {
        fn new(n: usize) -> Self {
            let ctx = egui::Context::default();
            crate::theme::install_fonts(&ctx, "", &[]);
            let mut list = Self { ctx, songs: (0..n).map(song).collect(), drag: None, lowest: 0, shown: true, time: 0.0, at: Pos2::ZERO, top: 0.0, row: 0.0, offset: 0.0, clicked: vec![], dropped: vec![], reorder: true, target: None, held: false, landed: vec![], inbox: None, received: vec![] };
            list.frame(vec![]);
            list.frame(vec![]);
            list
        }

        fn frame(&mut self, events: Vec<Event>) {
            let raw = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(WIDTH, HEIGHT))), time: Some(self.time), predicted_dt: 1.0 / 60.0, events, ..Default::default() };
            self.time += 1.0 / 60.0;
            let ctx = self.ctx.clone();
            // As the app's `raw_input_hook` does.
            note_pointer(&ctx, &raw.events);
            let id = egui::Id::unique("test-list");
            let mut output = ctx.run_ui(raw, |ui| {
                self.held = false;
                if let Some(rect) = self.inbox {
                    let rows = Rows { top: rect.top(), first: 0, lowest: 0, end: 5, accent: egui::Color32::RED, spaced: None };
                    let (drag, received) = (&mut self.drag, &mut self.received);
                    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                        ui.set_clip_rect(rect);
                        if let Some((g, t)) = receive(ui, drag, egui::Id::unique("test-inbox"), rows) {
                            received.push((g, t.id));
                        }
                    });
                }
                if let Some(rect) = self.target {
                    match target(ui, &mut self.drag, rect) {
                        Some(Over::Held) => self.held = true,
                        Some(Over::Dropped(t)) => self.landed.push(t.id),
                        None => {}
                    }
                }
                if !self.shown {
                    return;
                }
                let area = egui::ScrollArea::vertical().id_salt("test-scroll").auto_shrink(false).show(ui, |ui| {
                    let top = ui.cursor().top();
                    self.top = top;
                    self.row = row_height(ui);
                    let mut acts = Vec::new();
                    let (songs, drag, clicked) = (&self.songs, &mut self.drag, &mut self.clicked);
                    visible_rows(ui, songs.len(), |ui, i| {
                        grip(ui, drag, id, i, &songs[i]);
                        let o = RowOpts { number: None, show_album: false, plays_col: false, current: false, playing: false, accent: egui::Color32::RED, time: 0.0, liked: false, added: None, pick: false, selected: None, remove: None, queue_remove: None };
                        if track_row(ui, &songs[i], &o, &mut acts).clicked() {
                            clicked.push(i);
                        }
                    });
                    if !self.reorder {
                        carry(ui, drag, id);
                        return None;
                    }
                    let rows = Rows { top, first: 0, lowest: self.lowest, end: songs.len(), accent: egui::Color32::RED, spaced: None };
                    feedback(ui, drag, id, rows)
                });
                self.offset = area.state.clamped_offset().y;
                if let Some(d) = area.inner {
                    self.dropped.push(d);
                }
            });
            output.textures_delta.clear();
        }

        /// The middle of row `i` where it is now, `x` from the left.
        fn on_row(&self, i: usize, x: f32) -> Pos2 {
            pos2(x, self.top + i as f32 * self.row + 28.0)
        }

        fn move_to(&mut self, p: Pos2) {
            self.at = p;
            self.frame(vec![Event::PointerMoved(p)]);
        }

        fn button(&mut self, pressed: bool) {
            self.frame(vec![Event::PointerButton { pos: self.at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
        }

        /// In steps, the way a pointer really moves.
        fn glide_to(&mut self, p: Pos2) {
            let from = self.at;
            for i in 1..=8 {
                self.move_to(from + (p - from) * (i as f32 / 8.0));
            }
        }

        fn pick_up(&mut self, i: usize, x: f32) {
            let p = self.on_row(i, x);
            self.move_to(p);
            self.button(true);
        }

        /// A finger at `p`, as egui-winit sends it: the touch, with the pointer's events.
        fn touch(&mut self, phase: egui::TouchPhase, p: Pos2) {
            let touch = Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase, pos: p, force: None };
            let press = |pressed| Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
            self.frame(match phase {
                egui::TouchPhase::Start => vec![touch, Event::PointerMoved(p), press(true)],
                egui::TouchPhase::Move => vec![touch, Event::PointerMoved(p)],
                _ => vec![touch, press(false), Event::PointerGone],
            });
        }

        /// A finger put on row `i` and swiped up by `dy`, after a tap elsewhere: egui
        /// scrolls by dragging once it has seen a touch screen.
        fn swipe_up(&mut self, i: usize, dy: f32) {
            let far = pos2(WIDTH - 20.0, HEIGHT - 10.0);
            self.touch(egui::TouchPhase::Start, far);
            self.touch(egui::TouchPhase::End, far);
            self.swipe_up_now(i, dy);
        }

        /// A finger put on row `i` and swiped up by `dy`, with nothing before it.
        fn swipe_up_now(&mut self, i: usize, dy: f32) {
            let p = self.on_row(i, 300.0);
            self.touch(egui::TouchPhase::Start, p);
            for s in 1..=10 {
                self.touch(egui::TouchPhase::Move, p - vec2(0.0, dy * s as f32 / 10.0));
            }
            self.touch(egui::TouchPhase::End, p - vec2(0.0, dy));
        }
    }

    #[test]
    fn a_click_is_a_click() {
        let mut l = List::new(40);
        l.pick_up(3, 300.0);
        l.button(false);
        l.frame(vec![]);
        assert_eq!(l.clicked, [3]);
        assert!(l.drag.is_none());
        assert!(l.dropped.is_empty());
    }

    #[test]
    fn pressing_a_row_doesnt_pick_it_up() {
        let mut l = List::new(40);
        l.pick_up(3, 300.0);
        l.frame(vec![]);
        l.move_to(l.at + vec2(2.0, 2.0));
        assert!(l.drag.is_none());
        l.button(false);
    }

    #[test]
    fn the_right_button_doesnt_move_rows() {
        let mut l = List::new(40);
        let p = l.on_row(1, 300.0);
        l.move_to(p);
        l.frame(vec![Event::PointerButton { pos: p, button: PointerButton::Secondary, pressed: true, modifiers: Modifiers::NONE }]);
        l.glide_to(l.on_row(5, 300.0));
        l.frame(vec![Event::PointerButton { pos: l.at, button: PointerButton::Secondary, pressed: false, modifiers: Modifiers::NONE }]);
        assert!(l.dropped.is_empty());
        assert!(l.drag.is_none());
    }

    #[test]
    fn a_drag_left_behind_by_another_list_doesnt_block_a_new_one() {
        let mut l = List::new(40);
        l.drag = Some(RowDrag { list: egui::Id::unique("gone"), from: 0, track: song(0), seen: 0 });
        l.pick_up(1, 300.0);
        l.glide_to(l.on_row(5, 300.0));
        l.button(false);
        l.frame(vec![]);
        assert_eq!(l.dropped.len(), 1);
    }

    #[test]
    fn a_shaky_click_is_still_a_click() {
        let mut l = List::new(40);
        l.pick_up(3, 300.0);
        l.move_to(l.at + vec2(2.0, 2.0));
        l.button(false);
        l.frame(vec![]);
        assert_eq!(l.clicked, [3]);
        assert!(l.dropped.is_empty());
    }

    #[test]
    fn a_drag_lands_in_the_gap_under_the_pointer_and_doesnt_click() {
        let mut l = List::new(40);
        l.pick_up(1, 300.0);
        // Just past the middle of row 4: the gap below it.
        let to = l.on_row(4, 300.0) + vec2(0.0, 6.0);
        l.glide_to(to);
        assert!(l.drag.is_some());
        l.button(false);
        l.frame(vec![]);
        assert!(l.clicked.is_empty());
        assert!(l.drag.is_none());
        let [d] = l.dropped.as_slice() else { panic!("{} drops", l.dropped.len()) };
        assert_eq!((d.from, d.gap, d.id.as_str()), (1, 5, "song1"));
    }

    #[test]
    fn a_drag_can_start_on_the_rows_buttons() {
        let mut l = List::new(40);
        // The ⋯ menu at the row's right end.
        l.pick_up(1, WIDTH - 24.0);
        let to = l.on_row(5, WIDTH - 24.0);
        l.glide_to(to);
        l.button(false);
        l.frame(vec![]);
        assert_eq!(l.dropped.len(), 1);
    }

    #[test]
    fn dragging_a_row_doesnt_scroll_the_list_away_from_the_edges() {
        let mut l = List::new(40);
        l.pick_up(2, 300.0);
        let to = l.on_row(5, 300.0);
        l.glide_to(to);
        assert_eq!(l.offset, 0.0);
        l.button(false);
    }

    #[test]
    fn let_go_where_it_was_nothing_moves() {
        let mut l = List::new(40);
        l.pick_up(2, 300.0);
        let (away, back) = (l.on_row(5, 300.0), l.on_row(2, 300.0));
        l.glide_to(away);
        l.glide_to(back);
        l.button(false);
        l.frame(vec![]);
        assert!(l.dropped.is_empty());
        assert!(l.clicked.is_empty());
        assert!(l.drag.is_none());
    }

    #[test]
    fn the_list_scrolls_at_the_edge_and_the_drag_survives_its_row_going_off_screen() {
        let mut l = List::new(200);
        l.pick_up(1, 300.0);
        let bottom = pos2(300.0, HEIGHT - 4.0);
        l.glide_to(bottom);
        for _ in 0..120 {
            l.move_to(bottom);
        }
        assert!(l.offset > 3.0 * l.row, "scrolled only {}", l.offset);
        assert!(l.drag.is_some(), "the drag ended with its row off screen");
        l.button(false);
        l.frame(vec![]);
        let [d] = l.dropped.as_slice() else { panic!("{} drops", l.dropped.len()) };
        assert_eq!(d.from, 1);
        // The gap is where the pointer ended up in the scrolled list.
        assert!(d.gap as f32 > l.offset / l.row, "gap {} offset {}", d.gap, l.offset);
        assert!(l.clicked.is_empty());
    }

    #[test]
    fn it_scrolls_back_up_too() {
        let mut l = List::new(200);
        l.pick_up(1, 300.0);
        let bottom = pos2(300.0, HEIGHT - 4.0);
        l.glide_to(bottom);
        for _ in 0..120 {
            l.move_to(bottom);
        }
        let far = l.offset;
        let top = pos2(300.0, 4.0);
        l.glide_to(top);
        for _ in 0..60 {
            l.move_to(top);
        }
        assert!(l.offset < far, "still at {} from {far}", l.offset);
    }

    #[test]
    fn let_go_beside_the_list_cancels() {
        let mut l = List::new(40);
        // A list narrower than the window isn't needed: past either side of it counts.
        l.pick_up(2, 300.0);
        l.glide_to(pos2(WIDTH + 200.0, 200.0));
        l.button(false);
        l.frame(vec![]);
        assert!(l.dropped.is_empty());
        assert!(l.drag.is_none());
    }

    #[test]
    fn rows_cant_land_above_the_lowest_gap() {
        let mut l = List::new(40);
        l.lowest = 2;
        l.frame(vec![]);
        l.pick_up(5, 300.0);
        l.glide_to(pos2(300.0, l.top));
        l.button(false);
        l.frame(vec![]);
        let [d] = l.dropped.as_slice() else { panic!("{} drops", l.dropped.len()) };
        assert_eq!((d.from, d.gap), (5, 2));
    }

    #[test]
    fn a_list_that_went_away_ended_the_drag() {
        let mut l = List::new(40);
        l.pick_up(2, 300.0);
        let to = l.on_row(5, 300.0);
        l.glide_to(to);
        l.shown = false;
        l.frame(vec![]);
        l.button(false);
        l.frame(vec![]);
        // It comes back with the button up: no drop out of nowhere.
        l.shown = true;
        l.frame(vec![]);
        l.frame(vec![]);
        assert!(l.dropped.is_empty());
        assert!(l.drag.is_none());
    }

    /// A target down the window's left side, over the list as the sidebar is beside it.
    fn side_target() -> Rect {
        Rect::from_min_max(pos2(0.0, 0.0), pos2(120.0, HEIGHT))
    }

    #[test]
    fn a_song_let_go_on_a_target_lands_there_and_not_in_the_list() {
        let mut l = List::new(40);
        l.target = Some(side_target());
        l.pick_up(1, 300.0);
        l.glide_to(pos2(60.0, l.on_row(5, 0.0).y));
        assert!(l.held);
        l.button(false);
        l.frame(vec![]);
        assert_eq!(l.landed, ["song1"]);
        assert!(l.dropped.is_empty());
        assert!(l.clicked.is_empty());
        assert!(l.drag.is_none());
    }

    #[test]
    fn a_list_that_cant_change_order_still_carries_its_songs_out() {
        let mut l = List::new(40);
        l.reorder = false;
        l.target = Some(side_target());
        l.pick_up(2, 300.0);
        l.glide_to(l.on_row(6, 300.0));
        assert!(l.drag.is_some() && !l.held);
        l.glide_to(pos2(60.0, 200.0));
        l.button(false);
        l.frame(vec![]);
        assert_eq!(l.landed, ["song2"]);
        assert!(l.dropped.is_empty());
    }

    #[test]
    fn let_go_in_a_list_that_cant_change_order_nothing_happens() {
        let mut l = List::new(40);
        l.reorder = false;
        l.target = Some(side_target());
        l.pick_up(2, 300.0);
        l.glide_to(l.on_row(6, 300.0));
        l.button(false);
        l.frame(vec![]);
        assert!(l.dropped.is_empty() && l.landed.is_empty() && l.clicked.is_empty());
        assert!(l.drag.is_none());
    }

    #[test]
    fn a_song_dragged_into_another_list_lands_at_the_gap_it_was_let_go() {
        let mut l = List::new(40);
        l.reorder = false;
        l.inbox = Some(Rect::from_min_max(pos2(600.0, 0.0), pos2(WIDTH, HEIGHT)));
        l.pick_up(2, 300.0);
        l.glide_to(l.on_row(6, 300.0));
        // Just below the line between the inbox's rows 2 and 3.
        l.glide_to(pos2(700.0, 3.0 * l.row + 5.0));
        l.button(false);
        l.frame(vec![]);
        assert_eq!(l.received, [(3, "song2".to_owned())]);
        assert!(l.dropped.is_empty() && l.landed.is_empty());
        assert!(l.drag.is_none());
    }

    #[test]
    fn a_list_doesnt_take_in_its_own_songs() {
        let mut l = List::new(40);
        l.inbox = Some(Rect::from_min_max(pos2(600.0, 0.0), pos2(WIDTH, HEIGHT)));
        l.pick_up(1, 300.0);
        l.glide_to(l.on_row(5, 300.0));
        l.button(false);
        l.frame(vec![]);
        assert!(l.received.is_empty());
    }

    #[test]
    fn rows_below_the_space_count_from_past_it() {
        let rows = Rows { top: 100.0, first: 0, lowest: 0, end: 10, accent: egui::Color32::RED, spaced: Some((2, 30.0)) };
        // Above the space, as without it.
        assert_eq!(rows.gap_at(100.0 + 1.6 * 50.0, 50.0), 2);
        // In the space: the gap under row 2.
        assert_eq!(rows.gap_at(100.0 + 3.0 * 50.0 + 15.0, 50.0), 3);
        // Below it, rows sit 30 lower.
        assert_eq!(rows.gap_at(100.0 + 4.4 * 50.0 + 30.0, 50.0), 4);
        assert_eq!(rows.gap_at(100.0 + 4.6 * 50.0 + 30.0, 50.0), 5);
        // The line for a gap under the space is drawn past it.
        assert_eq!(rows.gap_y(2, 50.0, 0.0), 200.0);
        assert_eq!(rows.gap_y(4, 50.0, 0.0), 330.0);
    }

    #[test]
    fn a_target_ignores_a_drag_whose_list_went_away() {
        let mut l = List::new(40);
        l.target = Some(side_target());
        l.pick_up(2, 300.0);
        l.glide_to(pos2(60.0, 200.0));
        l.shown = false;
        l.frame(vec![]);
        l.frame(vec![]);
        l.button(false);
        l.frame(vec![]);
        assert!(l.landed.is_empty());
    }

    #[test]
    fn touch_leaves_the_rows_alone() {
        let mut l = List::new(40);
        let p = l.on_row(2, 300.0);
        let touch = |phase| Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase, pos: p, force: None };
        l.frame(vec![touch(egui::TouchPhase::Start), Event::PointerMoved(p), Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
        for i in 1..10 {
            let q = p + vec2(0.0, -6.0 * i as f32);
            l.frame(vec![Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase: egui::TouchPhase::Move, pos: q, force: None }, Event::PointerMoved(q)]);
        }
        assert!(l.drag.is_none());
    }

    /// The grips were left out only while a finger was down, which is too late: egui
    /// gives the press to the grip that was there the frame before, and the list didn't
    /// scroll (#190).
    #[test]
    fn a_finger_swiping_over_the_rows_scrolls_the_list() {
        let mut l = List::new(40);
        l.swipe_up(5, 190.0);
        // At least as far as the finger went: let go, it coasts on a little.
        assert!(l.offset >= 189.0, "{}", l.offset);
        assert!(l.drag.is_none() && l.clicked.is_empty());
    }

    /// A finger that last tapped where no list was (the search box, say) is still one
    /// when a list then opens: its first swipe there scrolls it.
    #[test]
    fn a_list_opened_by_a_finger_scrolls_with_its_first_swipe() {
        let mut l = List::new(40);
        l.shown = false;
        let far = pos2(WIDTH - 20.0, HEIGHT - 10.0);
        l.touch(egui::TouchPhase::Start, far);
        l.touch(egui::TouchPhase::End, far);
        l.shown = true;
        l.frame(vec![]);
        l.swipe_up_now(5, 190.0);
        assert!(l.offset >= 189.0, "{}", l.offset);
        assert!(l.drag.is_none() && l.clicked.is_empty());
    }

    /// A finger that comes down where the mouse left the grips: the first swipe scrolls too.
    #[test]
    fn the_first_swipe_after_the_mouse_scrolls_the_list() {
        let mut l = List::new(40);
        let far = pos2(WIDTH - 20.0, HEIGHT - 10.0);
        l.touch(egui::TouchPhase::Start, far);
        l.touch(egui::TouchPhase::End, far);
        l.move_to(l.on_row(8, 300.0));
        l.frame(vec![]);
        l.swipe_up_now(5, 190.0);
        assert!(l.offset >= 189.0, "{}", l.offset);
        assert!(l.drag.is_none() && l.clicked.is_empty());
    }

    #[test]
    fn the_mouse_picks_rows_up_again_after_a_finger() {
        let mut l = List::new(40);
        l.swipe_up(5, 190.0);
        let row = ((l.offset + 10.0) / l.row) as usize + 1;
        l.pick_up(row, 300.0);
        l.glide_to(l.on_row(row + 3, 300.0));
        assert!(l.drag.is_some());
    }


    #[test]
    fn the_nearest_gap_counts() {
        // Rows 100 tall from y = 0: the gap above row 2 is at 200.
        assert_eq!(gap_at(160.0, 0.0, 100.0, 0, 5), 2);
        assert_eq!(gap_at(240.0, 0.0, 100.0, 0, 5), 2);
        assert_eq!(gap_at(260.0, 0.0, 100.0, 0, 5), 3);
    }

    #[test]
    fn gaps_stay_within_their_limits() {
        // Above the current song, and past the end.
        assert_eq!(gap_at(-500.0, 0.0, 100.0, 1, 5), 1);
        assert_eq!(gap_at(40.0, 0.0, 100.0, 1, 5), 1);
        assert_eq!(gap_at(9000.0, 0.0, 100.0, 1, 5), 5);
        // A list that's only the current song.
        assert_eq!(gap_at(40.0, 0.0, 100.0, 1, 0), 0);
    }

    #[test]
    fn the_list_scrolls_only_near_its_edges() {
        assert_eq!(edge_speed(500.0, 0.0, 1000.0), 0.0);
        assert!(edge_speed(SCROLL_EDGE / 2.0, 0.0, 1000.0) > 0.0);
        assert!(edge_speed(1000.0 - SCROLL_EDGE / 2.0, 0.0, 1000.0) < 0.0);
        // Beyond the edge: full speed, no more.
        assert_eq!(edge_speed(-300.0, 0.0, 1000.0), SCROLL_SPEED);
        assert_eq!(edge_speed(1300.0, 0.0, 1000.0), -SCROLL_SPEED);
    }
}
