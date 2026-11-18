//! Read the signed-in YouTube Music ratings shown by the player and visible song rows.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use egui::{Color32, Context, Id, Key, LayerId, Order, PointerButton, Rect, Response, Sense, Ui, pos2, vec2};

use crate::backend::{Backend, Req};
use crate::model::Track;
use crate::theme;
use crate::ytm::SongRating;

use super::{Action, App, widgets};

const MAX_READS: usize = 3;
const READ_GAP: f64 = 0.2;
const RETRY_DELAY: f64 = 2.0;
/// Ratings kept for rows that have scrolled away.
const KEEP: usize = 2000;
/// Seconds before coming back to the window asks for the ratings on show again.
const REFRESH_AFTER: f64 = 600.0;
pub(super) const MIN_UNLIKE_MS: f32 = 200.0;
pub(super) const MAX_UNLIKE_MS: f32 = 3000.0;
pub(super) const DEFAULT_UNLIKE_MS: f32 = 600.0;
const UNLIKE_MS: &str = "ratings_unlike_hold_ms";
const UNLIKE_HINT: &str = "Hold to remove like";

pub(super) struct Settings {
    pub player_button: bool,
    pub list_icons: bool,
    pub dim_disliked: bool,
    pub skip_disliked: bool,
    pub confirm_unlike: bool,
    pub unlike_hold_ms: f32,
    pub unlike_notices: bool,
    pub dislike_notices: bool,
}

impl Settings {
    pub fn load(get: impl Fn(&str) -> Option<String>) -> Self {
        let on = |key| get(key).is_none_or(|v| v == "1");
        let number = |key| get(key).and_then(|v| v.parse::<f32>().ok()).filter(|v| v.is_finite());
        let hold = number(UNLIKE_MS).or_else(|| number("ratings_unlike_hold_seconds").map(|s| s * 1000.0));
        Self {
            player_button: on("ratings_player_button"),
            list_icons: on("ratings_list_icons"),
            dim_disliked: on("ratings_dim_disliked"),
            skip_disliked: get("ratings_skip_disliked").is_some_and(|v| v == "1"),
            confirm_unlike: on("ratings_confirm_unlike"),
            unlike_hold_ms: hold.map_or(DEFAULT_UNLIKE_MS, |v| v.round().clamp(MIN_UNLIKE_MS, MAX_UNLIKE_MS)),
            unlike_notices: on("ratings_unlike_notices"),
            dislike_notices: on("ratings_dislike_notices"),
        }
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        for (key, on) in [
            ("ratings_player_button", self.player_button),
            ("ratings_list_icons", self.list_icons),
            ("ratings_dim_disliked", self.dim_disliked),
            ("ratings_skip_disliked", self.skip_disliked),
            ("ratings_confirm_unlike", self.confirm_unlike),
            ("ratings_unlike_notices", self.unlike_notices),
            ("ratings_dislike_notices", self.dislike_notices),
        ] {
            storage.set_string(key, if on { "1" } else { "0" }.to_owned());
        }
        storage.set_string(UNLIKE_MS, self.unlike_hold_ms.to_string());
    }

    pub fn share_unlike_settings(&self, ctx: &Context, account: u64) {
        ctx.data_mut(|d| d.insert_temp(Id::unique("unlike-gesture-settings"), (self.confirm_unlike, f64::from(self.unlike_hold_ms) / 1000.0, account)));
    }
}

#[derive(Clone, Copy)]
enum HoldPhase {
    Pressed(f64),
    Returning { since: f64, from: f32 },
    Accepted(f64),
}

#[derive(Clone, Copy)]
enum HoldInput {
    Pointer,
    Keyboard,
}

#[derive(Clone, Copy)]
struct UnlikeHold {
    target: Id,
    last_pass: u64,
    input: HoldInput,
    phase: HoldPhase,
    progress: f32,
    hint_until: f64,
}

impl UnlikeHold {
    fn update(&mut self, now: f64, seconds: f64, down: bool, released_here: bool, clicked: bool) -> bool {
        match self.phase {
            HoldPhase::Pressed(start) => {
                let elapsed = ((now - start) / seconds).clamp(0.0, 1.0) as f32;
                if elapsed >= 1.0 && (down || released_here) {
                    self.phase = HoldPhase::Accepted(now);
                    self.progress = 1.0;
                    return true;
                }
                if down {
                    self.progress = elapsed;
                } else {
                    if clicked {
                        self.hint_until = now + 3.0;
                    }
                    if released_here {
                        self.progress = elapsed;
                    }
                    self.phase = HoldPhase::Returning { since: now, from: self.progress };
                }
            }
            HoldPhase::Accepted(_) if !down => {
                self.phase = HoldPhase::Returning { since: now, from: 1.0 };
            }
            HoldPhase::Returning { since, from } => {
                self.progress = from * (1.0 - (now - since).clamp(0.0, 1.0) as f32);
                if clicked {
                    self.hint_until = now + 3.0;
                }
            }
            HoldPhase::Accepted(_) => {}
        }
        false
    }
}

pub(super) struct LikeGesture {
    pub activate: bool,
    pub progress: f32,
    pub hint: bool,
}

// The input owner and target are both part of the identity. A new button or selection
// starts from zero, even when it occupies the same place as the previous one.
// The palette keeps focus in its search field and forwards the initial Enter press.
pub(super) fn like_gesture(
    ui: &Ui,
    response: &Response,
    target: impl std::hash::Hash + std::fmt::Debug,
    liked: bool,
    selected_enter_press: Option<bool>,
) -> LikeGesture {
    let ctx = ui.ctx();
    let (enabled, seconds, account) = ctx.data(|d| d.get_temp::<(bool, f64, u64)>(Id::unique("unlike-gesture-settings"))).unwrap_or_default();
    let target = response.id.with((target, account));
    let state = Id::unique("unlike-gesture");
    let pass = ctx.cumulative_pass_nr();
    let (now, mouse_down, mouse_pressed, mouse_released, key_down, key_pressed, key_released) = ui.input(|i| {
        let keys = [Key::Enter, Key::Space];
        let keys = if selected_enter_press.is_some() { &keys[..1] } else { &keys[..] };
        (
            i.time, i.pointer.primary_down(), i.pointer.button_pressed(PointerButton::Primary), i.pointer.button_released(PointerButton::Primary),
            keys.iter().any(|&key| i.key_down(key)),
            selected_enter_press.unwrap_or_else(|| i.events.iter().any(|event| {
                matches!(event, egui::Event::Key { key, pressed: true, repeat: false, .. } if keys.contains(key))
            })),
            keys.iter().any(|&key| i.key_released(key)),
        )
    });
    let focused = response.enabled() && (response.has_focus() || selected_enter_press.is_some());
    let pointer = response.enabled() && response.contains_pointer();
    let start = if pointer && mouse_down && mouse_pressed && response.is_pointer_button_down_on() {
        // The row's drag grip would otherwise take over when egui's ordinary click times out.
        if enabled && liked {
            ctx.stop_dragging();
        }
        Some(HoldInput::Pointer)
    } else if focused && key_down && key_pressed {
        Some(HoldInput::Keyboard)
    } else {
        None
    };
    let input_state = |input| match input {
        HoldInput::Pointer => (pointer && mouse_down, pointer && mouse_released),
        HoldInput::Keyboard => (focused && key_down, focused && key_released),
    };
    let clicked = response.clicked_by(PointerButton::Primary) || (focused && key_released);
    let regular_click = response.clicked();
    let (activate, progress, hint) = ctx.data_mut(|d| {
        let existing = d.get_temp::<UnlikeHold>(state).filter(|hold| hold.target == target && pass - hold.last_pass <= 1);
        if !enabled || !liked {
            // A completed hold already drew the outline. Keep that look while the
            // player's ordinary unlike animation catches up with the new state.
            let completed = existing.filter(|_| enabled && start.is_none());
            let finish_press = completed.is_some_and(|hold| {
                let (down, released) = input_state(hold.input);
                matches!(hold.phase, HoldPhase::Accepted(_)) && (down || released)
            });
            let progress = if finish_press {
                1.0
            } else {
                completed.map_or(0.0, |hold| match hold.phase {
                    HoldPhase::Accepted(at) if now - at < 0.35 => 1.0,
                    HoldPhase::Returning { since, from: 1.0 } if now - since < 0.35 => 1.0,
                    _ => 0.0,
                })
            };
            if progress == 0.0 && existing.is_some() {
                d.remove::<UnlikeHold>(state);
            } else if let Some(mut hold) = existing {
                hold.last_pass = pass;
                d.insert_temp(state, hold);
            }
            return (regular_click && !finish_press, progress, false);
        }
        let mut hold = if let Some(input) = start {
            UnlikeHold { target, last_pass: pass, input, phase: HoldPhase::Pressed(now), progress: 0.0, hint_until: 0.0 }
        } else if let Some(hold) = existing {
            hold
        } else if clicked {
            UnlikeHold { target, last_pass: pass, input: HoldInput::Pointer, phase: HoldPhase::Returning { since: now, from: 0.0 }, progress: 0.0, hint_until: now + 3.0 }
        } else {
            return (false, 0.0, false);
        };
        let (down, released_here) = input_state(hold.input);
        let activate = hold.update(now, seconds, down, released_here, clicked);
        hold.last_pass = pass;
        let hint = now < hold.hint_until;
        let progress = hold.progress;
        if progress != 0.0 || hint || matches!(hold.phase, HoldPhase::Pressed(_)) {
            d.insert_temp(state, hold);
        } else {
            d.remove::<UnlikeHold>(state);
        }
        (activate, progress, hint)
    });
    if start.is_some() || progress > 0.0 || hint || activate {
        ctx.request_repaint_after(Duration::from_millis(16));
    }
    LikeGesture { activate, progress, hint }
}

pub(super) fn unlike_hint(ui: &Ui, response: &Response) {
    let ctx = ui.ctx();
    let layer = LayerId::new(Order::Tooltip, response.id.with("unlike-hint"));
    let painter = ctx.layer_painter(layer);
    let anchor = ctx.layer_transform_to_global(response.layer_id).map_or(response.rect, |transform| transform * response.rect);
    let screen = ctx.content_rect();
    let text = theme::text();
    let g = painter.layout_no_wrap(UNLIKE_HINT.to_owned(), theme::semibold(12.0), text);
    let size = g.size() + vec2(16.0, 8.0);
    let left = (anchor.center().x - size.x / 2.0).min(screen.right() - size.x - 4.0).max(screen.left() + 4.0);
    let top = (anchor.top() - size.y - 4.0).max(screen.top() + 4.0);
    let rect = Rect::from_min_size(pos2(left, top), size);
    painter.rect_filled(rect, 7.0, theme::menu_fill_solid());
    painter.galley(rect.min + vec2(8.0, 4.0), g, text);
    ctx.move_to_top(layer);
}

pub(super) fn unlike_heart(ui: &Ui, center: egui::Pos2, size: f32, fill: f32, outline: Color32, color: Color32, progress: f32) {
    let gray = theme::with_alpha(theme::text_dim(), f32::from(outline.a()) / 255.0);
    widgets::heart(ui, center, size, fill * (1.0 - progress), theme::lerp_color(outline, gray, progress), theme::lerp_color(color, gray, progress));
}

enum Entry {
    Reading { round: u64, retry_on_failure: bool },
    RetryAt(f64),
    Ready(Option<SongRating>),
    Confirmed(SongRating),
}

/// Answers are kept after their rows scroll away, so scrolling back doesn't ask again,
/// up to [`KEEP`] of them. Requests for scrolled-away tracks still count toward the limit
/// until they finish.
#[derive(Default)]
pub(super) struct Ratings {
    entries: HashMap<String, Entry>,
    in_flight: HashSet<u64>,
    round: u64,
    next_read: f64,
    playing: Option<(String, u64)>,
    refreshed_at: f64,
}

impl Ratings {
    pub fn reset(&mut self) {
        self.entries.clear();
        self.in_flight.clear();
        self.next_read = 0.0;
        self.playing = None;
    }

    /// When the window regains focus, refresh both rating sources at most every
    /// [`REFRESH_AFTER`]. The caller reloads Liked Music when this returns true.
    pub fn refresh_if_stale(&mut self, now: f64) -> bool {
        if now - self.refreshed_at >= REFRESH_AFTER {
            self.refresh();
            self.refreshed_at = now;
            return true;
        }
        false
    }

    pub fn invalidate(&mut self, id: &str) {
        self.entries.remove(id);
    }

    pub fn confirm(&mut self, id: &str, rating: SongRating) {
        // An older read can still finish, but its round no longer matches this entry.
        self.entries.insert(id.to_owned(), Entry::Confirmed(rating));
    }

    pub fn refresh(&mut self) {
        self.entries.clear();
    }

    pub fn get(&self, id: &str) -> Option<SongRating> {
        match self.entries.get(id) {
            Some(Entry::Ready(rating)) => *rating,
            Some(Entry::Confirmed(rating)) => Some(*rating),
            _ => None,
        }
    }

    pub fn finish(&mut self, video_id: String, round: u64, rating: Option<SongRating>, now: f64) {
        self.in_flight.remove(&round);
        if let Some(&Entry::Reading { round: current, retry_on_failure }) = self.entries.get(&video_id)
            && round == current
        {
            let entry = if rating.is_none() && retry_on_failure {
                Entry::RetryAt(now + RETRY_DELAY)
            } else {
                Entry::Ready(rating)
            };
            self.entries.insert(video_id, entry);
        }
    }

    fn request(&mut self, backend: &Backend, video_id: String, now: f64, retry_on_failure: bool) {
        self.round += 1;
        self.in_flight.insert(self.round);
        self.entries.insert(video_id.clone(), Entry::Reading { round: self.round, retry_on_failure });
        self.next_read = now + READ_GAP;
        backend.request(Req::SongRating { video_id, round: self.round });
    }

    fn sync(&mut self, backend: &Backend, ctx: &Context, now: f64, wanted: &[String]) {
        if self.entries.len() > KEEP {
            self.entries.retain(|id, entry| matches!(entry, Entry::Reading { .. }) || wanted.contains(id));
        }
        if self.in_flight.len() >= MAX_READS {
            return;
        }
        // The overlay reports one upload at a time. The player goes first and large
        // lists only ask for rows on screen, spaced out even when scrolling.
        let mut next_retry = f64::INFINITY;
        let next = wanted.iter().find_map(|id| match self.entries.get(id) {
            None => Some((id, true)),
            Some(Entry::RetryAt(at)) if *at <= now => Some((id, false)),
            Some(Entry::RetryAt(at)) => {
                next_retry = next_retry.min(*at);
                None
            }
            _ => None,
        });
        if let Some((id, retry_on_failure)) = next {
            if now >= self.next_read {
                self.request(backend, id.clone(), now, retry_on_failure);
                ctx.request_repaint_after(Duration::from_secs_f64(READ_GAP));
            } else {
                ctx.request_repaint_after(Duration::from_secs_f64(self.next_read - now));
            }
        } else if next_retry.is_finite() {
            ctx.request_repaint_after(Duration::from_secs_f64(next_retry.max(self.next_read) - now));
        }
    }
}

#[derive(Clone, Default)]
struct Rows {
    enabled: bool,
    icons: bool,
    dim: bool,
    ratings: HashMap<String, SongRating>,
    wanted: HashSet<String>,
    visible: Vec<String>,
}

impl Rows {
    fn rating(&self, id: &str, liked: bool) -> Option<SongRating> {
        // Liked Music is already loaded from the account. Show its hearts while
        // the individual requests catch up, and prefer the newer answer when it arrives.
        self.ratings.get(id).copied().or(liked.then_some(SongRating::Like))
    }
}

fn rows_id() -> Id {
    Id::unique("song-ratings")
}

pub(super) fn rating_id(track: &Track) -> &str {
    track.stands_for.as_deref().unwrap_or(&track.id)
}

pub(super) fn rated_track(track: &Track) -> Track {
    let mut track = track.clone();
    if let Some(video) = track.stands_for.take() {
        track.id = video;
        track.video = true;
    }
    track
}

pub(super) fn row(ui: &Ui, track: &Track, liked: bool, hovered: bool) -> (Option<SongRating>, bool, bool) {
    let (rating, icons, dim, added) = ui.data_mut(|d| {
        let rows = d.get_temp_mut_or_default::<Rows>(rows_id());
        if !rows.enabled || track.id.is_empty() {
            return (None, false, false, false);
        }
        let id = rating_id(track);
        let rating = rows.rating(id, liked);
        if rows.icons && hovered && rating.is_none() {
            rows.visible.insert(0, id.to_owned());
        } else {
            rows.visible.push(id.to_owned());
        }
        (rating, rows.icons, rows.dim && rating == Some(SongRating::Dislike), !rows.wanted.contains(id))
    });
    if added {
        ui.ctx().request_repaint();
    }
    (rating, icons, dim)
}

pub(super) fn player_button(app: &mut App, ui: &Ui, rect: Rect, track: &Track, size: f32, visible: bool) {
    let disliked = app.ratings.get(&track.id) == Some(SongRating::Dislike);
    let name = if disliked { "Remove dislike" } else { "Dislike song" };
    let resp = widgets::named(ui.interact(rect, ui.scope_id().with("dislike"), Sense::click()), name)
        .on_hover_text(name)
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
    let a = theme::anim_bool(ui.ctx(), resp.id.with("show"), visible || disliked || resp.has_focus(), 0.15);
    ui.painter().circle_filled(rect.center(), rect.width() / 2.0, theme::glass(0.1 * h));
    let color = if disliked { theme::RED } else { theme::lerp_color(theme::text_dim(), theme::text(), h) };
    super::vector::dislike(ui, rect.center(), size, disliked, theme::with_alpha(color, a));
    widgets::focus_ring(ui, &resp, rect, rect.width() / 2.0);
    if resp.clicked() {
        app.actions.push(if disliked { Action::RemoveDislike(track.id.clone()) } else { Action::Dislike(track.clone()) });
    }
}

impl App {
    // Run during painting, so a logic-only tick cannot consume the visible
    // tracks before the next frame has a chance to report them again.
    pub(super) fn sync_ratings(&mut self, ctx: &Context, now: f64) {
        let settings = &self.rating_settings;
        let visible = ctx.data_mut(|d| std::mem::take(&mut d.get_temp_mut_or_default::<Rows>(rows_id()).visible));
        let enabled = self.logged_in && (settings.list_icons || settings.dim_disliked);
        let mut wanted = Vec::new();
        let playing = self.playing_id().filter(|id| self.logged_in && settings.player_button && !id.is_empty());
        let changed = playing.map(|id| (id, self.generation)) != self.ratings.playing.as_ref().map(|(id, g)| (id.as_str(), *g));
        if changed {
            self.ratings.playing = playing.map(|id| (id.to_owned(), self.generation));
            if let Some((id, _)) = &self.ratings.playing
                && !matches!(self.ratings.entries.get(id), Some(Entry::Confirmed(_)))
            {
                self.ratings.entries.remove(id);
            }
        }
        if let Some((id, _)) = &self.ratings.playing {
            wanted.push(id.clone());
        }
        if enabled {
            wanted.extend(visible);
        }
        // A liked song can't also be disliked: its rating is known without asking, which
        // spares a request for every row of Liked Music. What's playing is always asked.
        let fetch: Vec<String> = wanted
            .iter()
            .enumerate()
            .filter(|(i, id)| (*i == 0 && self.ratings.playing.is_some()) || !self.liked.contains(*id))
            .map(|(_, id)| id.clone())
            .collect();
        self.ratings.sync(&self.backend, ctx, now, &fetch);
        let mut ratings: HashMap<_, _> = wanted.iter().filter_map(|id| Some((id.clone(), self.ratings.get(id)?))).collect();
        ratings.extend(self.pending_unlikes().iter().map(|id| (id.clone(), SongRating::Indifferent)));
        ctx.data_mut(|d| d.insert_temp(rows_id(), Rows {
            enabled,
            icons: settings.list_icons,
            dim: settings.dim_disliked,
            ratings,
            wanted: wanted.into_iter().collect(),
            visible: Vec::new(),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlike_duration_defaults_to_600_ms_and_stays_in_range() {
        let defaults = Settings::load(|_| None);
        assert!(defaults.confirm_unlike);
        assert!(defaults.unlike_notices);
        assert_eq!(defaults.unlike_hold_ms, 600.0);
        for (saved, expected) in [("0", 200.0), ("450", 450.0), ("3000", 3000.0), ("6000", 3000.0), ("NaN", 600.0)] {
            let settings = Settings::load(|key| (key == UNLIKE_MS).then(|| saved.to_owned()));
            assert_eq!(settings.unlike_hold_ms, expected);
        }
        assert_eq!(Settings::load(|key| (key == "ratings_unlike_hold_seconds").then(|| "2".to_owned())).unlike_hold_ms, 2000.0);
        assert!(!Settings::load(|key| (key == "ratings_unlike_notices").then(|| "0".to_owned())).unlike_notices);
    }

    #[test]
    fn an_unlike_hold_scales_to_the_setting_and_restores_in_one_second() {
        for seconds in [0.2, 1.5, 3.0] {
            let target = Id::unique("song");
            let mut hold = UnlikeHold { target, last_pass: 0, input: HoldInput::Pointer, phase: HoldPhase::Pressed(0.0), progress: 0.0, hint_until: 0.0 };
            assert!(!hold.update(seconds / 2.0, seconds, true, false, false));
            assert_eq!(hold.progress, 0.5);
            assert!(!hold.update(seconds / 2.0, seconds, false, true, true));
            assert!(hold.hint_until > seconds / 2.0);
            assert!(!hold.update(seconds / 2.0 + 0.5, seconds, false, false, false));
            assert_eq!(hold.progress, 0.25);
            assert!(!hold.update(seconds / 2.0 + 1.0, seconds, false, false, false));
            assert_eq!(hold.progress, 0.0);
            hold.phase = HoldPhase::Pressed(10.0);
            assert!(hold.update(10.0 + seconds + 0.001, seconds, true, false, false));
            assert_eq!(hold.progress, 1.0);
            assert!(!hold.update(10.0 + seconds + 0.1, seconds, true, false, false));
        }
    }

    #[test]
    fn ordinary_likes_click_and_changing_the_target_requires_a_new_hold() {
        use egui::{Event, Modifiers, RawInput};

        let ctx = Context::default();
        Settings::load(|_| None).share_unlike_settings(&ctx, 0);
        let rect = Rect::from_min_size(pos2(20.0, 20.0), vec2(44.0, 34.0));
        let center = rect.center();
        let frame = |time, target: &str, liked, events| {
            let input = RawInput { time: Some(time), events, ..Default::default() };
            let mut result = None;
            ctx.run_ui(input, |ui| {
                let response = ui.interact(rect, Id::unique("rating-button"), Sense::click());
                result = Some(like_gesture(ui, &response, target, liked, None));
            })
            .textures_delta
            .clear();
            result.unwrap()
        };
        let press = |pressed| Event::PointerButton { pos: center, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        frame(0.0, "first", false, vec![Event::PointerMoved(center)]);
        frame(0.1, "first", false, vec![press(true)]);
        assert!(frame(0.2, "first", false, vec![press(false)]).activate);
        frame(0.3, "first", true, vec![press(true)]);
        let quick = frame(0.4, "first", true, vec![press(false)]);
        assert!(quick.hint);
        assert!(!quick.activate);
        frame(0.5, "first", true, vec![press(true)]);
        assert!(frame(1.0, "first", true, vec![]).progress > 0.0);
        assert_eq!(frame(2.0, "second", true, vec![]).progress, 0.0);
        assert!(!frame(3.6, "first", true, vec![]).activate);
        assert!(!frame(3.7, "second", true, vec![press(false)]).activate);
        frame(4.0, "second", true, vec![press(true)]);
        assert!(!frame(4.59, "second", true, vec![]).activate);
        assert!(frame(4.61, "second", true, vec![]).activate);
        assert!(!frame(4.7, "second", false, vec![press(false)]).activate);
    }

    #[test]
    fn skipping_after_a_dislike_is_opt_in() {
        assert!(!Settings::load(|_| None).skip_disliked);
        for value in ["0", "1"] {
            let settings = Settings::load(|key| (key == "ratings_skip_disliked").then(|| value.to_owned()));
            assert_eq!(settings.skip_disliked, value == "1");
        }
    }

    #[test]
    fn a_rating_from_before_an_edit_cannot_replace_a_new_read() {
        let mut ratings = Ratings::default();
        ratings.entries.insert("song".into(), Entry::Reading { round: 1, retry_on_failure: true });
        ratings.in_flight.insert(1);
        ratings.invalidate("song");
        ratings.entries.insert("song".into(), Entry::Reading { round: 2, retry_on_failure: true });
        ratings.in_flight.insert(2);
        ratings.finish("song".into(), 1, Some(SongRating::Dislike), 0.0);
        assert!(ratings.get("song").is_none());
        assert_eq!(ratings.in_flight.len(), 1);
        ratings.finish("song".into(), 2, Some(SongRating::Indifferent), 0.0);
        assert_eq!(ratings.get("song"), Some(SongRating::Indifferent));
        assert!(ratings.in_flight.is_empty());
    }

    #[test]
    fn a_confirmed_write_replaces_a_read_even_when_its_reply_arrives_later() {
        use SongRating::*;

        for (written, read) in [(Dislike, Some(Indifferent)), (Dislike, None), (Indifferent, Some(Dislike))] {
            let mut ratings = Ratings::default();
            ratings.entries.insert("video".into(), Entry::Reading { round: 1, retry_on_failure: true });
            ratings.in_flight.insert(1);
            ratings.confirm("audio", Dislike);
            ratings.confirm("video", written);
            assert_eq!(ratings.get("video"), Some(written));
            ratings.finish("video".into(), 1, read, 0.0);
            assert_eq!(ratings.get("video"), Some(written));
            assert_eq!(ratings.get("audio"), Some(Dislike));
            assert!(ratings.in_flight.is_empty());
            ratings.refresh();
            assert_eq!(ratings.get("video"), None);
        }
    }

    #[test]
    fn row_rating_shows_liked_music_first_and_prefers_the_fresh_answer() {
        let mut rows = Rows::default();
        assert_eq!(rows.rating("song", true), Some(SongRating::Like));
        rows.ratings.insert("song".into(), SongRating::Dislike);
        assert_eq!(rows.rating("song", true), Some(SongRating::Dislike));
        rows.ratings.insert("song".into(), SongRating::Indifferent);
        assert_eq!(rows.rating("song", true), Some(SongRating::Indifferent));
    }

    #[test]
    fn coming_back_to_the_window_asks_again_only_after_a_while() {
        let mut ratings = Ratings::default();
        let mut focused = false;
        let mut refresh = |ratings: &mut Ratings, now, is_focused| {
            super::super::window_refocused(&mut focused, is_focused) && ratings.refresh_if_stale(now)
        };
        assert!(!refresh(&mut ratings, 0.0, true));
        assert!(!refresh(&mut ratings, REFRESH_AFTER - 1.0, false));
        assert!(refresh(&mut ratings, REFRESH_AFTER, true));
        ratings.confirm("song", SongRating::Dislike);
        assert!(!refresh(&mut ratings, REFRESH_AFTER + 4.0, false));
        assert!(!refresh(&mut ratings, REFRESH_AFTER + 5.0, true));
        assert_eq!(ratings.get("song"), Some(SongRating::Dislike));
        assert!(!refresh(&mut ratings, 2.0 * REFRESH_AFTER, true));
        assert!(!refresh(&mut ratings, 2.0 * REFRESH_AFTER, false));
        assert!(refresh(&mut ratings, 2.0 * REFRESH_AFTER, true));
        assert_eq!(ratings.get("song"), None);
    }

    #[test]
    fn a_missing_rating_is_retried_once() {
        let mut ratings = Ratings::default();
        ratings.entries.insert("song".into(), Entry::Reading { round: 1, retry_on_failure: true });
        ratings.finish("song".into(), 1, None, 10.0);
        assert!(matches!(ratings.entries["song"], Entry::RetryAt(at) if at == 10.0 + RETRY_DELAY));
        ratings.entries.insert("song".into(), Entry::Reading { round: 2, retry_on_failure: false });
        ratings.finish("song".into(), 2, None, 13.0);
        assert!(matches!(ratings.entries["song"], Entry::Ready(None)));
    }

    #[test]
    fn hovering_an_unrated_row_reveals_a_heart_that_likes_without_playing() {
        use egui::{Color32, Event, Modifiers, PointerButton, Pos2, RawInput, Shape, vec2};
        use SongRating::*;

        let track = Track { id: "audio".into(), title: "Song".into(), stands_for: Some("video".into()), ..Default::default() };
        let opts = widgets::RowOpts { number: None, show_album: false, plays_col: false, current: false, playing: false, accent: Color32::RED, time: 0.0, liked: false, added_by: None, pick: false, selected: None, remove: None, queue_remove: None };
        let look = &crate::preset::active().app.row_look;
        // The neutral rating is only known after the account's reply.
        for (rating, icons) in [(None, true), (Some(Indifferent), true), (Some(Like), true), (Some(Dislike), true), (Some(Indifferent), false)] {
            let ctx = Context::default();
            crate::theme::install_fonts(&ctx, "", &[]);
            let ratings = rating.into_iter().map(|rating| ("video".to_owned(), rating)).collect();
            ctx.data_mut(|d| d.insert_temp(rows_id(), Rows { enabled: true, icons, ratings, ..Default::default() }));

            let mut time = 0.0;
            let mut frame = |events| {
                time += 0.1;
                let mut slot = Pos2::ZERO;
                let mut row_middle = Pos2::ZERO;
                let mut played = false;
                let mut acts = Vec::new();
                let input = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 400.0))), time: Some(time), events, ..Default::default() };
                let mut output = ctx.run_ui(input, |ui| {
                    row_middle = ui.cursor().min + vec2(100.0, look.height / 2.0);
                    slot = ui.cursor().min + vec2(ui.available_width() - 41.0 - look.pad - 52.0 - 14.0, look.height / 2.0);
                    played = widgets::track_row(ui, &track, &opts, &mut acts).clicked();
                });
                output.textures_delta.clear();
                let heart = output.shapes.iter().any(|s| {
                    matches!(&s.shape, Shape::LineSegment { points, .. } if points.iter().all(|p| p.distance(slot) < 10.0))
                });
                (slot, row_middle, heart, played, acts)
            };

            let (slot, row_middle, heart, _, _) = frame(vec![]);
            assert_eq!(heart, icons && rating == Some(Like), "idle {rating:?}");
            let (_, _, heart, _, _) = frame(vec![Event::PointerMoved(row_middle)]);
            assert_eq!(heart, icons && matches!(rating, Some(Like | Indifferent)), "hovered {rating:?}");
            frame(vec![Event::PointerMoved(slot)]);
            let click = |pressed| Event::PointerButton { pos: slot, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
            frame(vec![click(true)]);
            let (_, _, _, played, acts) = frame(vec![click(false)]);
            match (rating, icons) {
                (Some(Indifferent), true) => assert!(matches!(&acts[..], [Action::Like(t, true)] if t.id == "video" && t.stands_for.is_none())),
                (Some(Like), true) => assert!(matches!(&acts[..], [Action::RemoveLike(t)] if t.id == "video" && t.stands_for.is_none())),
                (Some(Dislike), true) => assert!(matches!(&acts[..], [Action::RemoveDislike(id)] if id == "video")),
                _ => assert!(acts.is_empty()),
            }
            assert_eq!(played, rating.is_none() || !icons);
        }
    }
}
