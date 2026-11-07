//! Dragging a song row to a new place in its list. Rows off screen aren't built, so the
//! drag is kept here rather than in a row's own widget state, and the place it's going
//! comes from the pointer and the rows' fixed height.

use egui::{Id, Pos2, Rect, Sense, Ui, Vec2, pos2, vec2};

use super::widgets::{paint_cover, row_height, text_at};
use crate::model::Track;
use crate::theme;

/// Within this far of the list's top or bottom edge, the list scrolls.
const SCROLL_EDGE: f32 = 56.0;
/// Scrolling speed at the very edge, in points a second.
const SCROLL_SPEED: f32 = 900.0;
/// How far left or right of the list a drop still counts.
const SIDE_MARGIN: f32 = 48.0;

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
pub struct Rows {
    pub top: f32,
    pub first: usize,
    pub lowest: usize,
    pub end: usize,
    pub accent: egui::Color32,
}

/// Lets row `i` of `list` be picked up. Call just before building the row: its buttons
/// stay on top and clicks still reach them; only a real drag lands here. Touch is left
/// alone, where dragging scrolls.
pub fn grip(ui: &mut Ui, drag: &mut Option<RowDrag>, list: Id, i: usize, t: &Track) {
    if !ui.input(|input| input.any_touches()) {
        let rect = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), row_height(ui)));
        // egui counts a drag-only widget as dragged from the press itself: wait until the
        // pointer has moved too far (or held too long) for a click. A drag some other list
        // left behind gives way.
        let grip = ui.interact(rect, ui.scope_id().with("grip"), Sense::DRAG);
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

/// Call after the rows, inside their scroll area: shows where the drag in `list` would
/// land, scrolls near the edges, and ends it when let go.
pub fn feedback(ui: &mut Ui, drag: &mut Option<RowDrag>, list: Id, rows: Rows) -> Option<Dropped> {
    let Rows { top, first, lowest, end, accent } = rows;
    let frame = ui.ctx().cumulative_frame_nr();
    let d = drag.as_mut().filter(|d| d.list == list)?;
    if d.seen + 1 < frame {
        *drag = None;
        return None;
    }
    d.seen = frame;
    let (from, id) = (d.from, d.track.id.clone());
    let (pos, down, dt) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.primary_down(), i.stable_dt));
    let clip = ui.clip_rect();
    let h = row_height(ui);
    let width = ui.max_rect().x_range();
    let over = pos.filter(|p| p.x >= width.min - SIDE_MARGIN && p.x <= width.max + SIDE_MARGIN);
    let gap = over.map(|p| first + gap_at(p.y, top, h, lowest.saturating_sub(first), end.saturating_sub(first)));
    let moves = gap.filter(|&g| g != from && g != from + 1);
    if !down {
        *drag = None;
        return moves.map(|gap| Dropped { from, gap, id });
    }
    if let Some(g) = moves {
        let y = top + (g - first) as f32 * h - ui.spacing().item_spacing.y / 2.0;
        if clip.y_range().contains(y) {
            // Over the song following the pointer, which is often right there.
            let line = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Debug, Id::unique("row-drag-line")));
            line.hline(width, y, egui::Stroke::new(2.0, accent));
            line.circle_filled(pos2(width.min + 3.0, y), 4.0, accent);
        }
    }
    if let Some(p) = over {
        let speed = edge_speed(p.y, clip.top(), clip.bottom());
        if speed != 0.0 {
            ui.scroll_with_delta(Vec2::new(0.0, speed * dt));
        }
    }
    if let Some(p) = pos {
        let width = width.span().min(320.0);
        if let Some(d) = drag.as_ref() {
            ghost(ui, d, p, width);
        }
    }
    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    ui.ctx().request_repaint();
    None
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
    use super::{Dropped, RowDrag, Rows, feedback, grip};
    use crate::model::Track;
    use crate::ui::widgets::{RowOpts, row_height, track_row, visible_rows};
    use egui::{Event, Modifiers, PointerButton, Pos2, Rect, RawInput, pos2, vec2};

    const WIDTH: f32 = 800.0;
    const HEIGHT: f32 = 600.0;

    fn song(i: usize) -> Track {
        Track { id: format!("song{i}"), title: format!("Song {i}"), artists: vec![], album: None, duration: Some(200), thumbs: vec![], track_nr: None, plays: None, music_video: false }
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
    }

    impl List {
        fn new(n: usize) -> Self {
            let ctx = egui::Context::default();
            crate::theme::install_fonts(&ctx, "");
            let mut list = Self { ctx, songs: (0..n).map(song).collect(), drag: None, lowest: 0, shown: true, time: 0.0, at: Pos2::ZERO, top: 0.0, row: 0.0, offset: 0.0, clicked: vec![], dropped: vec![] };
            list.frame(vec![]);
            list.frame(vec![]);
            list
        }

        fn frame(&mut self, events: Vec<Event>) {
            let raw = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(WIDTH, HEIGHT))), time: Some(self.time), predicted_dt: 1.0 / 60.0, events, ..Default::default() };
            self.time += 1.0 / 60.0;
            let ctx = self.ctx.clone();
            let id = egui::Id::unique("test-list");
            let mut output = ctx.run_ui(raw, |ui| {
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
                        let o = RowOpts { number: None, show_album: false, plays_col: false, current: false, playing: false, accent: egui::Color32::RED, time: 0.0, liked: false, added_by: None, pick: false, selected: None, remove: None };
                        if track_row(ui, &songs[i], &o, &mut acts).clicked() {
                            clicked.push(i);
                        }
                    });
                    let rows = Rows { top, first: 0, lowest: self.lowest, end: songs.len(), accent: egui::Color32::RED };
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
