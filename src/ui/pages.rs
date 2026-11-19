//! Page bodies. Every page fades and slides in when navigated to.

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::atomic::Ordering;

use egui::{Align2, Color32, Rect, RichText, Sense, Ui, Vec2, pos2, vec2};

use super::widgets::{self, RowOpts, card, chip, h_scroll, named, paint_cover, pill, shelf_header, track_row};
use super::find::{ListFind, Sort};
use super::row_drag::{self, RowDrag};
use super::{Action, App, LibTab, Page, Selection};
use crate::backend::{PlaylistSort, SearchResults, compact};
use crate::model::{Card, ShelfKind, Target, Track};
use crate::theme::{self, icon};
use crate::ytm::Scope;

const CARD: f32 = 180.0;

pub fn page(app: &mut App, ui: &mut Ui, now: f64) {
    let t = theme::ease_out(((now - app.page_changed) / 0.32) as f32);
    let rise = app.look().app.page_rise_px * (1.0 - t);
    if rise > 0.0 {
        ui.add_space(rise);
    }
    if t < 1.0 {
        // The fade-in finishes even when nothing else is drawing frames.
        ui.ctx().request_repaint();
    }
    ui.set_opacity(0.15 + 0.85 * t);
    let page = app.page.clone();
    let insets = app.page_insets;
    if page == Page::Home && app.look().app.scrollbar_home_hidden {
        let bar = &mut ui.spacing_mut().scroll;
        (bar.dormant_handle_opacity, bar.active_handle_opacity, bar.interact_handle_opacity) = (0.0, 0.0, 0.0);
    }
    // Each page (each album, each search) keeps its own scroll position.
    let mut area = egui::ScrollArea::vertical().id_salt(("page", &page)).auto_shrink(false);
    // At the window's right edge: clear of where it resizes the window.
    ui.spacing_mut().scroll.bar_outer_margin += super::titlebar::edge_inset();
    // Dev hook: YTM_SCROLL=<px> pins the page scroll, for screenshots of lower sections.
    if let Some(y) = super::dev_var("YTM_SCROLL").and_then(|v| v.parse::<f32>().ok()) {
        area = area.vertical_scroll_offset(y);
    }
    // Its list of categories stays put while one scrolls.
    if page == Page::Settings {
        settings(app, ui);
        return;
    }
    let out = area.show(ui, |ui| {
        ui.set_max_width((ui.available_width() - 20.0).max(0.0));
        ui.add_space(18.0 * (1.0 - t) + insets[0]);
        match &page {
            Page::Home => home(app, ui, now),
            Page::Explore => explore(app, ui, now),
            Page::Mood(p) => mood(app, ui, p, now),
            Page::Search(q) => search(app, ui, q, now),
            Page::Album(id) | Page::Playlist(id) => collection(app, ui, id, now),
            Page::Artist(id) => artist(app, ui, id, now),
            Page::Library(tab) => library(app, ui, *tab, now),
            Page::Settings => {}
            Page::Transfer => super::transfer::page(app, ui, now),
            Page::Jam => super::jam::page(app, ui),
        }
        ui.add_space(40.0 + insets[1]);
    });
    app.page_scrolled = out.state.clamped_offset().y;
    selection_bar(app, ui.ctx(), &page, out.inner_rect);
    // Infinite scroll on home: fetch the next shelves when nearing the bottom.
    if page == Page::Home {
        let remaining = out.content_size.y - (out.state.clamped_offset().y + out.inner_rect.height());
        if let Some(h) = &mut app.home {
            // A failed fetch reports nothing (it's a quiet request): allow a retry later.
            if h.loading_more.is_some_and(|t| now - t > 20.0) {
                h.loading_more = None;
            }
            if remaining < 700.0 && h.loading_more.is_none()
                && let Some(token) = h.continuation.clone()
            {
                h.loading_more = Some(now);
                app.backend.request(crate::backend::Req::HomeMore(token));
            }
        }
    }
}

/// What every song row needs from the app, gathered once per frame.
pub(super) struct Rows {
    current: Option<String>,
    playing: bool,
    accent: Color32,
    now: f64,
    hide_explicit: bool,
}

impl Rows {
    pub(super) fn new(app: &App, now: f64) -> Self {
        Self {
            current: app.current_id().map(str::to_owned),
            playing: app.audio.status.playing.load(Ordering::Relaxed),
            accent: app.accent_color(),
            now,
            hide_explicit: app.hide_explicit,
        }
    }

    /// `t` is listed: it isn't an explicit song while those are hidden.
    pub(super) fn shows(&self, t: &Track) -> bool {
        !(self.hide_explicit && t.explicit)
    }

    pub(super) fn opts(&self, liked: &HashSet<String>, t: &Track, number: Option<usize>, show_album: bool) -> RowOpts {
        RowOpts {
            number,
            show_album,
            plays_col: t.plays.is_some(),
            current: self.current.as_deref() == Some(t.id.as_str()),
            playing: self.playing,
            accent: self.accent,
            time: self.now,
            liked: liked.contains(&t.id),
            added_by: None,
            pick: false,
            selected: None,
            remove: None,
            queue_remove: None,
        }
    }
}

/// A page's data hasn't arrived: shimmering placeholders while it loads, a retry if the
/// request failed.
fn placeholder(app: &mut App, ui: &mut Ui, now: f64, rows: usize, card: f32) {
    if app.load_failed {
        ui.add_space(40.0);
        ui.label(RichText::new("Couldn't load this page").color(theme::text_dim()));
        ui.add_space(12.0);
        if pill(ui, "Try again", None, false).clicked() {
            app.refresh();
        }
    } else {
        widgets::skeleton(ui, now, rows, card);
    }
}

/// A page's main Play button. Shows Pause while this page's album / playlist / artist
/// is playing, and toggles instead of restarting when it's already loaded.
fn play_pill(ui: &mut Ui, acts: &mut Vec<Action>, target: &Target, tracks: &[Track], find: &ListFind, page: &Page) {
    let active = widgets::active_state(ui, target);
    let (label, glyph) = if active == Some(true) { ("Pause", icon::PAUSE) } else { ("Play", icon::PLAY) };
    // Under the button, placed once the button has said where it is.
    let shadow_at = ui.painter().add(egui::Shape::Noop);
    let resp = pill(ui, label, Some(glyph), true);
    if let Some([y, blur, a]) = crate::preset::active().app.header_play_shadow {
        let accent = ui.ctx().data(|d| d.get_temp::<Color32>(egui::Id::unique("accent"))).unwrap_or(theme::RED);
        let s = egui::epaint::Shadow { offset: [0, y as i8], blur: blur as u8, spread: 0, color: theme::with_alpha(accent, a) };
        ui.painter().set(shadow_at, s.as_shape(resp.rect, egui::CornerRadius::same((resp.rect.height() / 2.0) as u8)));
    }
    if resp.clicked() {
        if active.is_some() {
            acts.push(Action::TogglePlay);
        } else if !tracks.is_empty() {
            acts.push(play_all(find, page, tracks));
        }
    }
}

/// A list's Play button: all of it from the top, in the order it's sorted on `page`.
fn play_all(find: &ListFind, page: &Page, tracks: &[Track]) -> Action {
    match find.sorted(page, tracks) {
        Some(order) => Action::PlayFound(order.iter().map(|&i| tracks[i].clone()).collect(), 0),
        None => Action::Play(tracks.to_vec(), 0),
    }
}

fn card_row(ui: &mut Ui, id: &str, title: &str, cards: &[Card], acts: &mut Vec<Action>) {
    if cards.is_empty() {
        return;
    }
    let (l, r) = shelf_header(ui, title, None, None, true);
    let nudge = r as i32 - l as i32;
    h_scroll(ui, id, nudge, cards.len(), CARD, 18.0, |ui| {
        ui.spacing_mut().item_spacing.x = 18.0;
        for c in cards {
            card(ui, c, CARD, false, None, acts);
        }
    });
}

/// Songs laid out in columns of `rows`, scrolling sideways (Quick picks). Clicking a
/// song starts its radio. Returns whether the arrows reached the end.
#[allow(clippy::too_many_arguments)]
fn song_grid(ui: &mut Ui, rows_ctx: &Rows, liked: &HashSet<String>, acts: &mut Vec<Action>, id: &str, title: &str, strap: Option<&str>, tracks: &[Track], rows: usize) -> bool {
    let (l, r) = shelf_header(ui, title, strap, None, true);
    let nudge = r as i32 - l as i32;
    let col_w = (ui.available_width() / 2.6).clamp(300.0, 420.0);
    let tracks: Vec<&Track> = tracks.iter().filter(|t| rows_ctx.shows(t)).collect();
    let mut picked = None;
    let (_, reached_end) = h_scroll(ui, id, nudge, tracks.len().div_ceil(rows.max(1)), col_w, 16.0, |ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        for (c, col) in tracks.chunks(rows.max(1)).enumerate() {
            ui.allocate_ui(vec2(col_w, rows as f32 * 60.0), |ui| {
                ui.set_width(col_w);
                ui.vertical(|ui| {
                    for (i, t) in col.iter().enumerate() {
                        let o = rows_ctx.opts(liked, t, None, false);
                        if track_row(ui, t, &o, acts).clicked() {
                            picked = Some(c * rows + i);
                        }
                    }
                });
            });
        }
    });
    if let Some(i) = picked {
        acts.push(Action::Radio(Track::clone(tracks[i])));
    }
    reached_end
}

/// Where a song list lives: the page its selection belongs to, and whether it's your own
/// playlist (so songs can be taken out of it).
struct ListCtx<'a> {
    sel: &'a mut Selection,
    find: &'a ListFind,
    drag: &'a mut Option<RowDrag>,
    /// Rows can be dragged into another order.
    movable: bool,
    page: &'a Page,
    own: bool,
}

/// A long song list; only the rows on screen are built. Clicking one plays `tracks` from it,
/// or picks it while selecting: Ctrl-click picks one song, Shift-click a run of them.
#[allow(clippy::too_many_arguments)]
fn list(ui: &mut Ui, rows: &Rows, liked: &HashSet<String>, tracks: &[Track], show_album: bool, number: impl Fn(usize, &Track) -> Option<usize>, cx: ListCtx, acts: &mut Vec<Action>) {
    let ListCtx { sel, find, drag, movable, page, own } = cx;
    let shown = find.rows(page, tracks);
    let selecting = sel.on(page);
    let mods = ui.input(|i| i.modifiers);
    let plays_col = tracks.iter().any(|t| t.plays.is_some());
    // Only the whole list in its own order has an order to change. Hidden explicit songs
    // aside: rows are dragged among the ones on show, then counted back in the whole list.
    let reordered = find.reordered(page);
    let movable = movable && !selecting && !find.searching(page) && !reordered;
    let n = shown.as_ref().map_or(tracks.len(), Vec::len);
    // Rows on show changing mid-drag (find typed into) make it another list: the drag ends.
    let (drag_list, top) = (egui::Id::unique(("list", page)).with(&shown), ui.cursor().top());
    widgets::visible_rows(ui, n, |ui, k| {
        let i = shown.as_ref().map_or(k, |s| s[k]);
        let t = &tracks[i];
        // Keyed on the song as well as the place: a song's open menu closes when the order
        // changes under it, instead of turning into another song's.
        ui.push_id(&t.id, |ui| {
            // Any song can be dragged onto a playlist in the sidebar.
            row_drag::grip(ui, drag, drag_list, k, t);
            let o = RowOpts {
                selected: selecting.then(|| sel.rows.contains(&i)),
                remove: own.then_some(i),
                plays_col,
                ..rows.opts(liked, t, number(i, t), show_album)
            };
            if track_row(ui, t, &o, acts).clicked() {
                if selecting || mods.command || mods.shift {
                    // A run of picks is of the rows on show, in their order on screen.
                    sel.pick(page, i, mods.shift, shown.as_deref());
                } else if let Some(s) = shown.as_ref().filter(|_| reordered || find.searching(page)) {
                    // What's on show plays, in its order. (Hidden explicit songs alone: the queue
                    // leaves those out.)
                    acts.push(Action::PlayFound(s.iter().map(|&r| tracks[r].clone()).collect(), k));
                } else {
                    acts.push(Action::Play(tracks.to_vec(), i));
                }
            }
        });
    });
    if !movable {
        row_drag::carry(ui, drag, drag_list);
    } else if let Some(d) = row_drag::feedback(ui, drag, drag_list, row_drag::Rows { top, first: 0, lowest: 0, end: n, accent: rows.accent, spaced: None }) {
        // Past the last row on show is the end of the whole list.
        let whole = |r: usize| shown.as_ref().map_or(r, |s| s.get(r).copied().unwrap_or(tracks.len()));
        acts.push(Action::PlaylistMove { from: whole(d.from), gap: whole(d.gap), id: d.id });
    }
}

/// The Sort button and the box that narrow a page's song list, against the right edge of
/// its row of buttons.
fn find_box_right(ui: &mut Ui, find: &mut ListFind, sel: &mut Selection, page: &Page, tracks: &[Track]) {
    find_tools(ui, find, sel, page, tracks, &[], false, false, &mut Vec::new(), true);
}

/// The Sort button, then the find box; `right`: against the right edge of the row. Your own
/// playlist (`saved` has its orders) gets YouTube Music's Sort, the rest the app's own;
/// `busy`: it can't be sorted for now; `sorting`: because a sort of it is under way.
#[allow(clippy::too_many_arguments)]
fn find_tools(ui: &mut Ui, find: &mut ListFind, sel: &mut Selection, page: &Page, tracks: &[Track], saved: &[PlaylistSort], busy: bool, sorting: bool, acts: &mut Vec<Action>, right: bool) {
    let layout = if right { egui::Layout::right_to_left(egui::Align::Center) } else { egui::Layout::left_to_right(egui::Align::Center) };
    // A row of its own height: on a line of its own (under the page's buttons) the rest of
    // the page's height would be taken, with the row centred in it.
    ui.allocate_ui_with_layout(vec2(ui.available_width(), 38.0), layout, |ui| {
        if right {
            find_field(ui, find, sel, page, tracks);
        }
        match page {
            Page::Playlist(id) if !saved.is_empty() => saved_sort_pill(ui, find, page, id, saved, busy, sorting, acts),
            _ => sort_pill(ui, find, page),
        }
        if !right {
            find_field(ui, find, sel, page, tracks);
        }
    });
}

/// What's looked for changing leaves no picks behind that can't be seen.
fn find_field(ui: &mut Ui, find: &mut ListFind, sel: &mut Selection, page: &Page, tracks: &[Track]) {
    if find.show(ui, page, tracks) && sel.page.as_ref() == Some(page) {
        sel.rows.clear();
        sel.anchor = None;
    }
}

/// The room [`sort_pill`] takes, at its widest.
const SORT_W: f32 = 175.0;

/// The orders `page`'s list can be shown in, its own order first. None for an album, which
/// keeps its track order.
fn sort_options(page: &Page) -> Vec<(Sort, &'static str)> {
    // Lists that come newest first can turn around. Loading YouTube Music's own sorts just to
    // look can't be used: past the first 100 songs its pages come back unsorted. (Your own
    // playlists use its menu instead, which saves the order: see `saved_sort_pill`.)
    let (own, newest_first) = match page {
        Page::Playlist(id) if id == super::LIKED_MUSIC => ("Recently liked", true),
        Page::Playlist(_) => ("Custom order", false),
        Page::Library(LibTab::Songs) => ("Recently liked", true),
        Page::Library(LibTab::History) => ("Recently played", true),
        Page::Library(LibTab::Uploads) => ("Recently added", true),
        Page::Library(LibTab::Downloads) => ("Newest first", true),
        Page::Library(LibTab::Playlists | LibTab::Albums | LibTab::Artists | LibTab::Podcasts) => return vec![(Sort::Default, "Recent"), (Sort::Title, "A–Z"), (Sort::TitleDesc, "Z–A")],
        _ => return Vec::new(),
    };
    let mut options = vec![(Sort::Default, own)];
    if newest_first {
        options.push((Sort::Reversed, "Oldest first"));
    }
    options.extend([(Sort::Title, "Title"), (Sort::Artist, "Artist"), (Sort::Album, "Album"), (Sort::Duration, "Duration")]);
    options
}

/// A list's Sort button, with its orders in a menu. Away while the list is still coming in.
fn sort_pill(ui: &mut Ui, find: &mut ListFind, page: &Page) {
    let options = sort_options(page);
    if options.is_empty() || find.holding(page) {
        return;
    }
    let now = find.sort(page);
    let label = match options.iter().find(|(s, _)| *s == now && now != Sort::Default) {
        Some((_, name)) => format!("Sort: {name}"),
        None => "Sort".to_owned(),
    };
    let resp = pill(ui, &label, None, false);
    widgets::menu_popup(&resp).show(|ui| {
        widgets::menu_body(ui, 190.0);
        for (s, name) in options {
            if widgets::menu_choice(ui, name, s == now) {
                find.set_sort(page, s);
            }
        }
    });
}

/// Your own playlist's Sort button: YouTube Music's orders, saved to the playlist as there,
/// which then loads again in the new order. Away while the list is still coming in.
#[allow(clippy::too_many_arguments)]
fn saved_sort_pill(ui: &mut Ui, find: &mut ListFind, page: &Page, id: &str, saved: &[PlaylistSort], busy: bool, sorting: bool, acts: &mut Vec<Action>) {
    if find.holding(page) {
        return;
    }
    // It comes in the order picked here: none of the app's own sorts on top.
    if find.sort(page) != Sort::Default {
        find.set_sort(page, Sort::Default);
    }
    // One sort at a time: what's ticked is stale until the playlist has loaded again.
    let label = match saved.iter().find(|s| s.on && !s.manual) {
        _ if sorting => "Sorting…".to_owned(),
        Some(s) => format!("Sort: {}", s.name),
        None => "Sort".to_owned(),
    };
    let resp = ui.add_enabled_ui(!busy, |ui| pill(ui, &label, None, false)).inner;
    widgets::menu_popup(&resp).show(|ui| {
        widgets::menu_body(ui, 190.0);
        for s in saved {
            if widgets::menu_choice(ui, &s.name, s.on) && !s.on {
                acts.push(Action::SortPlaylist(id.to_owned(), s.actions.clone()));
            }
        }
    });
}

/// A list page's "Select" button: turns select mode on and off.
fn select_pill(ui: &mut Ui, sel: &mut Selection, page: &Page) {
    let on = sel.on(page);
    if pill(ui, if on { "Done" } else { "Select" }, Some(icon::LIST_CHECKS), false).clicked() {
        if on { sel.clear() } else { sel.start(page) }
    }
}

/// The songs a page lists, and whether they're your own playlist's.
fn selectable<'a>(app: &'a App, page: &Page) -> Option<(&'a [Track], bool)> {
    match page {
        Page::Album(id) | Page::Playlist(id) => {
            app.opened(id).map(|o| &o.c).map(|c| (c.tracks.as_slice(), app.logged_in && c.owned && !c.is_album))
        }
        Page::Library(LibTab::Songs) => app.opened(super::LIKED_MUSIC).map(|o| (o.c.tracks.as_slice(), false)),
        Page::Library(LibTab::History) => app.history.as_deref().map(|h| (h, false)),
        Page::Library(LibTab::Uploads) => app.uploads.as_deref().map(|u| (u, false)),
        Page::Library(LibTab::Downloads) => app.downloads.as_deref().map(|d| (d, false)),
        _ => None,
    }
}

/// While selecting: a bar floating over the bottom of the page with what to do with the picks.
fn selection_bar(app: &mut App, ctx: &egui::Context, page: &Page, area: Rect) {
    if !app.selection.on(page) {
        return;
    }
    let Some((tracks, own)) = selectable(app, page) else { return };
    // With a list narrowed by find, "all" is what's on show.
    let shown = app.find.rows(page, tracks);
    let n = shown.as_ref().map_or(tracks.len(), Vec::len);
    // In the order on screen.
    let picked: Vec<Track> = match &shown {
        Some(s) => s.iter().filter(|i| app.selection.rows.contains(i)).map(|&i| tracks[i].clone()).collect(),
        None => app.selection.rows.iter().filter_map(|&i| tracks.get(i).cloned()).collect(),
    };
    let all = !picked.is_empty() && picked.len() == n;
    let all_liked = !picked.is_empty() && picked.iter().all(|t| app.liked.contains(&t.id));
    let unlike_target = all_liked.then(|| egui::Id::unique((page, picked.iter().map(|track| (&track.id, &track.stands_for)).collect::<Vec<_>>())));
    // Labelled buttons when there's room, icons with tooltips when the window is narrow.
    let wide = area.width() > 820.0;
    let button = |ui: &mut Ui, label: &str, glyph: char, primary: bool| {
        if wide { pill(ui, label, Some(glyph), primary) } else { widgets::icon_button(ui, glyph, 18.0, label, false) }
    };
    let mut acts = Vec::new();
    let mut toggle_all = false;
    let mut done = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    egui::Area::new(egui::Id::unique("selection-bar"))
        .order(egui::Order::Foreground)
        .pivot(Align2::CENTER_BOTTOM)
        // Above a floating player bar the page runs under.
        .fixed_pos(pos2(area.center().x, area.bottom() - 18.0 - app.page_insets[1]))
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(theme::menu_fill_solid())
                .corner_radius(ui.visuals().menu_corner_radius)
                .inner_margin(egui::Margin { left: 20, right: 10, top: 8, bottom: 8 })
                .shadow(egui::epaint::Shadow { offset: [0, 8], blur: 24, spread: 0, color: theme::shade(0.45) })
                .show(ui, |ui| {
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let count = if picked.is_empty() { "Select songs".to_owned() } else { format!("{} selected", picked.len()) };
                        ui.label(RichText::new(count).font(theme::semibold(14.0)).color(theme::text()));
                        ui.add_space(10.0);
                        if !picked.is_empty() {
                            if button(ui, "Add to playlist", icon::LIST_PLUS, true).clicked() {
                                acts.push(Action::AddToPlaylist(picked.clone()));
                            }
                            if own && button(ui, "Remove from playlist", icon::LIST_MINUS, false).clicked() {
                                acts.push(Action::RemoveFromPlaylist(app.selection.rows.iter().copied().collect()));
                            }
                            let rate = match unlike_target {
                                Some(target) => widgets::unlike_pill(ui, "Remove like", wide, target),
                                None => button(ui, "Like", icon::HEART, false).clicked(),
                            };
                            if rate {
                                acts.push(Action::LikeMany(picked.clone(), !all_liked));
                            }
                            if button(ui, "Add to queue", icon::LIST_MUSIC, false).clicked() {
                                acts.extend(picked.iter().cloned().map(Action::Enqueue));
                            }
                            if !matches!(page, Page::Library(LibTab::Downloads)) && button(ui, "Download", icon::DOWNLOAD, false).clicked() {
                                acts.push(Action::DownloadMany(None, String::new(), Some(picked.clone())));
                            }
                        }
                        toggle_all = button(ui, if all { "Select none" } else { "Select all" }, icon::LIST_CHECKS, false).clicked();
                        done |= widgets::icon_button(ui, icon::X, 18.0, "Done selecting", false).clicked();
                    });
                });
        });
    if toggle_all {
        let page = page.clone();
        app.selection.start(&page);
        if !all {
            app.selection.rows = shown.map_or_else(|| (0..n).collect(), |s| s.into_iter().collect());
        }
    }
    if done || !acts.is_empty() {
        app.selection.clear();
    }
    app.actions.extend(acts);
}

// ------------------------------------------------------------------ home

fn home(app: &mut App, ui: &mut Ui, now: f64) {
    if let Some(h) = &app.home
        && !h.chips.is_empty()
    {
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(10.0, 10.0);
            for c in &h.chips {
                if chip(ui, &c.title, c.selected).clicked() {
                    let params = if c.selected { None } else { c.params.clone() };
                    app.actions.push(Action::HomeChip(params));
                }
            }
        });
    }
    // A refresh keeps showing the current feed; the skeleton is only for an empty one.
    if app.home.as_ref().is_none_or(|h| app.home_loading && h.shelves.is_empty()) {
        placeholder(app, ui, now, 3, CARD);
        return;
    }
    let rows = Rows::new(app, now);
    let Some(h) = &app.home else { return };
    if h.shelves.is_empty() {
        ui.add_space(40.0);
        ui.label(RichText::new("Nothing here yet").color(theme::text_dim()));
    }
    let mut more_picks = None;
    for (i, s) in h.shelves.iter().enumerate() {
        let id = format!("home-{i}-{}", s.title);
        match &s.kind {
            ShelfKind::Cards(cards) => {
                let cards: Vec<&Card> = cards.iter().filter(|c| !matches!(&c.target, Target::Song(t) if !rows.shows(t))).collect();
                if cards.is_empty() {
                    continue;
                }
                let (strap, thumb) = if s.title == "Listen again" { (None, None) } else { (s.strapline.as_deref(), s.strap_thumb.as_deref()) };
                cards_shelf(ui, &app.liked, &mut app.actions, &id, &s.title, strap, thumb, &cards);
            }
            ShelfKind::Songs { tracks, rows: n } => {
                let reached_end = song_grid(ui, &rows, &app.liked, &mut app.actions, &id, &s.title, s.strapline.as_deref(), tracks, *n);
                if s.title == "Quick picks" && reached_end && !h.picks_loading && !h.picks_expanded {
                    more_picks = tracks.last().map(|t| t.id.clone());
                }
            }
        }
    }
    let loading_more = h.loading_more.is_some();
    if let Some(seed) = more_picks
        && let Some(h) = &mut app.home
    {
        h.picks_loading = true;
        app.backend.request(crate::backend::Req::MoreQuickPicks(seed));
    }
    if loading_more {
        widgets::skeleton(ui, now, 1, CARD);
    }
}

/// Cards scrolling sideways under a shelf's title.
#[allow(clippy::too_many_arguments)]
fn cards_shelf(ui: &mut Ui, liked: &HashSet<String>, acts: &mut Vec<Action>, id: &str, title: &str, strap: Option<&str>, thumb: Option<&str>, cards: &[&Card]) {
    let (l, r) = shelf_header(ui, title, strap, thumb, true);
    let nudge = r as i32 - l as i32;
    h_scroll(ui, id, nudge, cards.len(), CARD, 18.0, |ui| {
        ui.spacing_mut().item_spacing.x = 18.0;
        for c in cards {
            let is_liked = matches!(&c.target, Target::Song(t) if liked.contains(&t.id));
            card(ui, c, CARD, is_liked, None, acts);
        }
    });
}

// ------------------------------------------------------------------ explore

fn explore(app: &mut App, ui: &mut Ui, now: f64) {
    page_title(ui, "Explore");
    if app.explore.is_none() {
        placeholder(app, ui, now, 2, CARD);
        return;
    }
    let rows = Rows::new(app, now);
    let Some(e) = &app.explore else { return };
    card_row(ui, "ex-new", if app.logged_in { "New releases for you" } else { "Popular new releases" }, &e.new_albums, &mut app.actions);
    if !e.top.is_empty() {
        song_grid(ui, &rows, &app.liked, &mut app.actions, "ex-top", "Top songs", None, &e.top, 4);
    }
    card_row(ui, "ex-charts", "Charts", &e.charts, &mut app.actions);
    for g in &e.moods {
        mood_grid(ui, g, &mut app.actions);
    }
}

/// A group of Moods & genres: its categories in as many columns as fit, each opening its page.
fn mood_grid(ui: &mut Ui, g: &crate::ytm::MoodGroup, acts: &mut Vec<Action>) {
    shelf_header(ui, &g.title, None, None, false);
    let gap = 14.0;
    let w = ui.available_width();
    let cols = ((w + gap) / (190.0 + gap)).floor().max(1.0);
    // A little under the exact share, so rounding never pushes the last one to a new line.
    let bw = ((w - gap * (cols - 1.0)) / cols - 0.5).max(60.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(gap, gap);
        for m in &g.moods {
            if mood_button(ui, m, bw).clicked() {
                acts.push(Action::Go(Page::Mood(m.params.clone())));
            }
        }
    });
}

/// A category's button: its name, and the stripe of its own colour down the left edge.
fn mood_button(ui: &mut Ui, m: &crate::ytm::Mood, w: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 48.0), Sense::click());
    let resp = named(resp, &m.title).on_hover_cursor(egui::CursorIcon::PointingHand);
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
    let radius = (crate::preset::active().chrome.radius * 0.4).clamp(4.0, 12.0) as u8;
    let p = ui.painter();
    p.rect_filled(rect, radius, theme::glass(0.08 + 0.06 * h));
    let [_, r, g, b] = m.color.to_be_bytes();
    let stripe = Rect::from_min_size(rect.min, vec2(6.0, rect.height()));
    p.rect_filled(stripe, egui::CornerRadius { nw: radius, sw: radius, ne: 0, se: 0 }, Color32::from_rgb(r, g, b));
    let font = theme::semibold(14.5);
    let y = rect.center().y - ui.fonts_mut(|f| f.row_height(&font)) / 2.0;
    widgets::text_at(ui, pos2(rect.left() + 20.0, y), &m.title, font, theme::text(), w - 28.0);
    resp
}

/// A Moods & genres category: its shelves of playlists.
fn mood(app: &mut App, ui: &mut Ui, params: &str, now: f64) {
    let Some(m) = app.mood.as_ref().filter(|m| m.params == params) else {
        placeholder(app, ui, now, 3, CARD);
        return;
    };
    page_title(ui, &m.title);
    let rows = Rows::new(app, now);
    for (i, s) in m.shelves.iter().enumerate() {
        let id = format!("mood-{i}-{}", s.title);
        match &s.kind {
            ShelfKind::Cards(cards) => {
                let cards: Vec<&Card> = cards.iter().filter(|c| !matches!(&c.target, Target::Song(t) if !rows.shows(t))).collect();
                if !cards.is_empty() {
                    cards_shelf(ui, &app.liked, &mut app.actions, &id, &s.title, s.strapline.as_deref(), s.strap_thumb.as_deref(), &cards);
                }
            }
            ShelfKind::Songs { tracks, rows: n } => {
                song_grid(ui, &rows, &app.liked, &mut app.actions, &id, &s.title, s.strapline.as_deref(), tracks, *n);
            }
        }
    }
}

pub(super) fn page_title(ui: &mut Ui, title: &str) {
    ui.add_space(14.0);
    ui.label(RichText::new(title).font(theme::bold(34.0)).color(theme::text()));
}

// ------------------------------------------------------------------ search

fn search(app: &mut App, ui: &mut Ui, q: &str, now: f64) {
    let pick = super::transfer::pick_banner(app, ui);
    if app.logged_in {
        scope_tabs(app, ui, q);
    }
    let scope = app.scope_for(q);
    if shown_results(&app.search, &app.scoped, q, scope).is_none() {
        placeholder(app, ui, now, 2, CARD);
        return;
    }
    let rows = Rows::new(app, now);
    // A song played from here starts its radio, as in YouTube Music, rather than the other
    // results queued after it; the one playing pauses or resumes.
    let current = app.current_id().map(str::to_owned);
    let play = |t: &Track| {
        if pick {
            Action::TransferPick(t.clone())
        } else if current.as_deref() == Some(t.id.as_str()) {
            Action::TogglePlay
        } else {
            Action::Radio(t.clone())
        }
    };
    let Some(r) = shown_results(&app.search, &app.scoped, q, scope) else { return };
    let acts = &mut app.actions;
    let liked = &app.liked;
    // Songs here can be dragged onto a playlist in the sidebar, but not into another order.
    let drag = &mut app.row_drag;
    let ids = |section: &str| egui::Id::unique(("search", section));
    let tracks: Vec<&Track> = r.tracks.iter().filter(|t| rows.shows(t)).collect();
    let videos: Vec<&Track> = r.videos.iter().filter(|t| rows.shows(t)).collect();
    let episodes: Vec<&Track> = r.episodes.iter().filter(|t| rows.shows(t)).collect();
    if tracks.is_empty() && videos.is_empty() && r.artists.is_empty() && r.albums.is_empty() && r.playlists.is_empty() && r.podcasts.is_empty() && episodes.is_empty() {
        page_title(
            ui,
            match scope {
                Scope::Everywhere => "No results",
                Scope::Library => "No results in your library",
                Scope::Uploads => "No results in your uploads",
            },
        );
        return;
    }
    ui.add_space(8.0);
    // Top result: the first artist when the query names one, otherwise the first song.
    let top_card = |ui: &mut Ui, acts: &mut Vec<Action>| {
        if let Some(top) = r.artists.first().filter(|a| a.title.eq_ignore_ascii_case(q.trim())) {
            top_result(ui, top, false, acts);
        } else if let Some(t) = tracks.first().copied() {
            let c = Card {
                title: t.title.clone(),
                subtitle: format!("Song • {}", t.artist_line()),
                thumbs: t.thumbs.clone(),
                target: Target::Song(t.clone()),
                round: false,
                links: t.artists.clone(),
            };
            top_result(ui, &c, pick, acts);
        }
    };
    let mut songs = |ui: &mut Ui, acts: &mut Vec<Action>| {
        ui.vertical(|ui| {
            ui.label(RichText::new("Songs").font(theme::bold(24.0)));
            ui.add_space(8.0);
            let plays_col = tracks.iter().take(4).any(|t| t.plays.is_some());
            for (i, t) in tracks.iter().copied().take(4).enumerate() {
                // Its own scope, so a picked-up row dims alone.
                ui.push_id(i, |ui| {
                    row_drag::grip(ui, drag, ids("songs"), i, t);
                    let o = RowOpts { pick, plays_col, ..rows.opts(liked, t, None, false) };
                    if track_row(ui, t, &o, acts).clicked() {
                        acts.push(play(t));
                    }
                });
            }
            row_drag::carry(ui, drag, ids("songs"));
        });
    };
    // Songs beside it while there's room for their rows, else under it.
    if ui.available_width() >= TOP_RESULT_W + TOP_RESULT_GAP + SONGS_MIN_W {
        ui.horizontal_top(|ui| {
            top_card(ui, acts);
            ui.add_space(TOP_RESULT_GAP);
            songs(ui, acts);
        });
    } else {
        top_card(ui, acts);
        if !tracks.is_empty() {
            ui.add_space(26.0);
            songs(ui, acts);
        }
    }
    card_row(ui, "s-artists", "Artists", &r.artists, acts);
    card_row(ui, "s-albums", "Albums", &r.albums, acts);
    if !videos.is_empty() && !app.hide_search_videos {
        ui.add_space(26.0);
        ui.label(RichText::new("Videos").font(theme::bold(24.0)));
        ui.add_space(10.0);
        let plays_col = videos.iter().take(6).any(|t| t.plays.is_some());
        for (i, t) in videos.iter().copied().take(6).enumerate() {
            ui.push_id(("videos", i), |ui| {
                row_drag::grip(ui, drag, ids("videos"), i, t);
                let o = RowOpts { pick, plays_col, ..rows.opts(liked, t, None, false) };
                if track_row(ui, t, &o, acts).clicked() {
                    acts.push(play(t));
                }
            });
        }
        row_drag::carry(ui, drag, ids("videos"));
    }
    if tracks.len() > 4 {
        ui.add_space(26.0);
        ui.label(RichText::new("More songs").font(theme::bold(24.0)));
        ui.add_space(10.0);
        let plays_col = tracks.iter().skip(4).any(|t| t.plays.is_some());
        for (i, t) in tracks.iter().copied().skip(4).enumerate() {
            ui.push_id(("more", i), |ui| {
                row_drag::grip(ui, drag, ids("more"), i, t);
                let o = RowOpts { pick, plays_col, ..rows.opts(liked, t, None, true) };
                if track_row(ui, t, &o, acts).clicked() {
                    acts.push(play(t));
                }
            });
        }
        row_drag::carry(ui, drag, ids("more"));
    }
    card_row(ui, "s-pl", "Playlists", &r.playlists, acts);
    card_row(ui, "s-podcasts", "Podcasts", &r.podcasts, acts);
    if !episodes.is_empty() {
        ui.add_space(26.0);
        ui.label(RichText::new("Episodes").font(theme::bold(24.0)));
        ui.add_space(10.0);
        for (i, t) in episodes.iter().copied().take(6).enumerate() {
            ui.push_id(("episodes", i), |ui| {
                row_drag::grip(ui, drag, ids("episodes"), i, t);
                let o = RowOpts { pick, ..rows.opts(liked, t, None, false) };
                if track_row(ui, t, &o, acts).clicked() {
                    // An episode plays alone: a radio from it would queue songs after it.
                    acts.push(match play(t) {
                        Action::Radio(t) => Action::Play(vec![t], 0),
                        other => other,
                    });
                }
            });
        }
        row_drag::carry(ui, drag, ids("episodes"));
    }
}

/// YT Music, Library and Uploads over a search's results, as on music.youtube.com: all of
/// YouTube Music, or only what's in your library or what you uploaded. Like the library's
/// tabs they swap in place, and the one shown refreshes.
fn scope_tabs(app: &mut App, ui: &mut Ui, q: &str) {
    let shown = app.scope_for(q);
    let mut picked = None;
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        for (scope, label) in [(Scope::Everywhere, "YT Music"), (Scope::Library, "Library"), (Scope::Uploads, "Uploads")] {
            if chip(ui, label, scope == shown).clicked() {
                picked = Some(scope);
            }
        }
    });
    ui.add_space(6.0);
    match picked {
        Some(scope) if scope == shown => app.refresh(),
        Some(scope) => {
            app.search_scope = (q.to_owned(), scope);
            app.page_loaded();
        }
        None => {}
    }
}

/// The results of the search tab shown, once they're in.
fn shown_results<'a>(search: &'a Option<SearchResults>, scoped: &'a [SearchResults], q: &str, scope: Scope) -> Option<&'a SearchResults> {
    match scope {
        Scope::Everywhere => search.as_ref().filter(|r| r.query == q),
        _ => scoped.iter().find(|r| r.query == q && r.scope == scope),
    }
}

/// The Top result card's width, and the gap after it when Songs are beside it.
const TOP_RESULT_W: f32 = 420.0;
const TOP_RESULT_GAP: f32 = 24.0;
/// The narrowest Songs column shown beside Top result: below it, Songs go under it.
const SONGS_MIN_W: f32 = 320.0;

/// `pick`: choosing a transfer match, so clicking a song picks it.
fn top_result(ui: &mut Ui, c: &Card, pick: bool, acts: &mut Vec<Action>) {
    ui.vertical(|ui| {
        ui.label(RichText::new("Top result").font(theme::bold(24.0)));
        ui.add_space(8.0);
        // Narrower in a window too narrow for it.
        let w = ui.available_width().clamp(260.0, TOP_RESULT_W);
        let (rect, resp) = ui.allocate_exact_size(vec2(w, 236.0), Sense::click());
        let resp = named(resp, &c.title).on_hover_cursor(egui::CursorIcon::PointingHand);
        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.contains_pointer(), 0.2);
        ui.painter().rect_filled(rect, 14.0, theme::glass(0.07 + 0.05 * h));
        let img = Rect::from_min_size(rect.min + vec2(20.0, 20.0), Vec2::splat(110.0));
        paint_cover(ui, img, &c.thumbs, if c.round { 55.0 } else { theme::card_radius() }, 1.0);
        let x = rect.left() + 20.0;
        widgets::text_at(ui, pos2(x, img.bottom() + 16.0), &c.title, theme::bold(26.0), theme::text(), rect.width() - 40.0);
        widgets::linked_text(ui, pos2(x, img.bottom() + 52.0), &c.subtitle, &c.links, theme::regular(14.0), theme::text_dim(), rect.width() - 40.0, acts);
        // Floating play button
        let pc = rect.right_bottom() + vec2(-44.0, -44.0 + 6.0 * (1.0 - h));
        let pr = Rect::from_center_size(pc, Vec2::splat(52.0));
        let active = widgets::active_state(ui, &c.target);
        let playing = active == Some(true);
        let label = format!("{} {}", if playing { "Pause" } else { "Play" }, c.title);
        let p = named(ui.interact(pr, resp.id.with("play"), Sense::click()), &label).on_hover_cursor(egui::CursorIcon::PointingHand);
        ui.painter().circle_filled(pc, 26.0, theme::with_alpha(Color32::WHITE, 0.4 + 0.6 * h));
        let nudge = if playing { 0.0 } else { 1.5 };
        widgets::play_pause_glyph(ui, pc + vec2(nudge, 0.0), 10.0, Color32::from_gray(15), playing);
        if p.clicked() {
            acts.push(match &c.target {
                _ if active.is_some() => Action::TogglePlay,
                Target::Song(t) => Action::Radio(t.clone()),
                t => Action::PlayCollection(t.clone(), false),
            });
        } else if resp.clicked() {
            acts.push(match &c.target {
                Target::Song(t) if pick => Action::TransferPick(t.clone()),
                t => Action::Open(t.clone()),
            });
        }
    });
}

// ------------------------------------------------------------------ album / playlist

fn collection(app: &mut App, ui: &mut Ui, id: &str, now: f64) {
    if app.opened(id).is_none() {
        placeholder(app, ui, now, 1, 232.0);
        return;
    }
    let rows = Rows::new(app, now);
    let downloaded = all_downloaded(app, id);
    let run = app.download_runs.iter().find(|r| r.key == id).map(|r| (r.done, r.total, r.cancelled));
    let Some(o) = app.lists.get(id) else { return };
    // Sorted once all of it is here: until then it plays as it arrives.
    app.find.hold(&app.page, !o.complete);
    let (c, more) = (&o.c, app.receiving(id));
    // A sort of it is under way. None starts while it loads (what's ticked may be stale) or a
    // drag is still being saved (the load after the sort would wait for it).
    let sorting = app.sorting.contains_key(id);
    let busy = sorting || app.lists.loading(id) || app.lists.moving(id);
    let acts = &mut app.actions;
    let tracks = &c.tracks;
    let is_album = c.is_album;
    let look = &crate::preset::active();
    // Under the art, fading out down the header: one more picture, already loaded for the cover.
    if let Some(a) = look.app.immersive_header {
        let top = ui.cursor().min;
        let band = Rect::from_min_max(pos2(ui.max_rect().left(), top.y), pos2(ui.max_rect().right(), top.y + 340.0));
        // On a pale page the picture stays faint: dark text goes over it.
        let shown = look.app.header_image_alpha.unwrap_or(a) * if theme::light() { 0.35 } else { 1.0 };
        widgets::paint_cover_fill(ui, band, &c.thumbs, theme::with_alpha(Color32::WHITE, shown));
        match look.app.header_gradient {
            Some([from, to]) => super::shell::vgradient(ui.painter(), band, theme::shade(from), theme::shade(to)),
            None => super::shell::vgradient(ui.painter(), band, Color32::TRANSPARENT, look.solid()),
        }
    }
    ui.add_space(18.0);
    ui.horizontal_top(|ui| {
        let (img, img_resp) = ui.allocate_exact_size(Vec2::splat(232.0), Sense::hover());
        let img = if look.app.header_art_hover != 1.0 { Rect::from_center_size(img.center(), img.size() * (1.0 + (look.app.header_art_hover - 1.0) * theme::anim_bool(ui.ctx(), "header-art-h", img_resp.hovered(), 0.2))) } else { img };
        let [sy, sb, sa] = look.app.header_art_shadow.unwrap_or([0.0, 40.0, 0.5]);
        widgets::soft_shadow(ui, img.translate(vec2(0.0, sy)), 10.0, sb, sa);
        paint_cover(ui, img, &c.thumbs, theme::card_radius() + 2.0, 1.0);
        widgets::download_veil(ui, img, theme::card_radius() + 2.0, id);
        ui.add_space(26.0);
        ui.vertical(|ui| {
            let full = ui.available_width();
            ui.add_space(24.0);
            ui.vertical(|ui| {
                ui.set_max_width(full.min(760.0));
                ui.label(RichText::new(if is_album { "ALBUM" } else if c.is_show() { "PODCAST" } else { "PLAYLIST" }).font(theme::semibold(12.0)).color(theme::text_dim()));
                ui.label(RichText::new(&c.title).font(theme::weighted(look.app.header_title_weight, look.app.header_title_px)).color(theme::text()));
                ui.add_space(2.0);
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
                widgets::linked_text(ui, r.min, &c.byline(), &c.artists, theme::regular(15.0), theme::text_dim(), r.width(), acts);
                let total: u32 = tracks.iter().filter_map(|t| t.duration).sum();
                if total > 0 {
                    ui.label(
                        RichText::new(format!("{} {} • {} min", tracks.len(), c.things(), total / 60)).font(theme::regular(13.5)).color(theme::text_faint()),
                    );
                }
                if let Some(d) = c.description.as_deref().filter(|d| !d.trim().is_empty()) {
                    ui.add_space(6.0);
                    let g = widgets::galley_wrapped(ui, d.trim(), theme::regular(13.5), theme::text_dim(), ui.available_width(), 2);
                    ui.label(g);
                }
            });
            ui.add_space(16.0);
            // The most these buttons take (Save to library's the widest); below that, the
            // find box takes a line of its own.
            const BUTTONS_W: f32 = 820.0;
            let tools_w = super::find::BOX_W + if is_album { 0.0 } else { SORT_W };
            let beside = full >= BUTTONS_W + tools_w;
            let page = &app.page;
            let (selection, find) = (&mut app.selection, &mut app.find);
            ui.horizontal(|ui| {
                let width = if beside { full - tools_w - 16.0 } else { full };
                ui.allocate_ui_with_layout(vec2(width, 38.0), egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true), |ui| {
                    ui.spacing_mut().item_spacing = vec2(10.0, 10.0);
                    let target = if is_album { Target::Album(id.to_owned()) } else { Target::Playlist(id.to_owned()) };
                    play_pill(ui, acts, &target, tracks, find, page);
                    if pill(ui, "Shuffle", Some(icon::SHUFFLE), false).clicked() && !tracks.is_empty() {
                        acts.push(Action::Shuffle(tracks.clone()));
                    }
                    if let Some(first) = tracks.first()
                        && pill(ui, "Radio", Some(icon::RADIO), false).clicked() {
                            acts.push(Action::Radio(first.clone()));
                        }
                    if !tracks.is_empty() {
                        select_pill(ui, selection, page);
                    }
                    // Liked Music and Episodes for later are the account's own lists: they can't be
                    // saved, removed, edited or deleted.
                    let auto = matches!(id, "LM" | "SE");
                    if app.logged_in && !auto {
                        let saved = app.library.as_ref().is_some_and(|l| {
                            let cards = if is_album { &l.albums } else { &l.playlists };
                            cards.iter().any(|card| card.target == target)
                        });
                        // Saving only applies to someone else's playlist or an album.
                        let save_id = if is_album { c.save_id.clone() } else { (!c.owned).then(|| id.to_owned()) };
                        if let Some(save_id) = save_id
                            && pill(ui, if saved { "Remove from library" } else { "Save to library" }, None, false).clicked() {
                                acts.push(Action::SaveCollection(save_id, !saved));
                            }
                    }
                    if let Some((.., true)) = run {
                        // Cancelled: the songs on their way finish first.
                        pill(ui, "Stopping\u{2026}", None, false);
                    } else if let Some((done, total, _)) = run {
                        if pill(ui, &format!("Downloading {done} of {total}"), Some(icon::X), false).on_hover_text("Cancel download").clicked() {
                            acts.push(Action::CancelDownload(id.to_owned()));
                        }
                    } else if downloaded {
                        pill(ui, "Downloaded", Some(icon::CHECK), false).on_hover_text(format!("All {} are on this computer", c.things()));
                    } else if !tracks.is_empty() && pill(ui, "Download", Some(icon::DOWNLOAD), false).clicked() {
                        // A whole list goes as it is; a partial one is fetched in full by the backend.
                        acts.push(Action::DownloadMany(Some(target.clone()), c.title.clone(), o.complete.then(|| tracks.clone())));
                    }
                    // Only your own playlists can be edited or deleted (mixes and others' playlists can't).
                    let own = app.logged_in && !auto && !is_album && c.owned;
                    let more = widgets::more_pill(ui, "More options");
                    widgets::menu_popup(&more).show(|ui| {
                        widgets::menu_body(ui, 190.0);
                        if super::actions::shareable(&target) && widgets::menu_action(ui, Some(icon::LINK), "Share") {
                            acts.push(Action::Share(target.clone()));
                        }
                        if (crate::downloads::has_list_folder(id) || crate::downloads::any_listed(tracks))
                            && widgets::menu_action(ui, Some(icon::FOLDER_OPEN), "Show in folder")
                        {
                            crate::downloads::show_list(id, tracks);
                        }
                        if own {
                            if widgets::menu_action(ui, Some(icon::SETTINGS), "Edit") { acts.push(Action::EditPlaylist); }
                            if widgets::menu_action(ui, Some(icon::USERS), "Collaborate") { acts.push(Action::Collaborate); }
                            if widgets::menu_action(ui, Some(icon::TRASH_2), "Delete") { acts.push(Action::DeletePlaylist); }
                        }
                    });
                });
                if beside {
                    find_tools(ui, find, selection, page, tracks, &c.sorts, busy, sorting, acts, true);
                }
            });
            if !beside {
                ui.add_space(10.0);
                find_tools(ui, find, selection, page, tracks, &c.sorts, busy, sorting, acts, false);
            }
        });
    });
    ui.add_space(26.0);
    let number = |i: usize, t: &Track| is_album.then(|| t.track_nr.map_or(i + 1, |n| n as usize));
    let own = app.logged_in && c.owned && !is_album;
    // Only with all of it here and no load that would bring the old order back.
    // And in the custom order, not one of YouTube Music's sorts.
    let movable = own && o.complete && !app.lists.loading(id) && !matches!(id, "LM" | "SE") && c.sorts.iter().all(|s| !s.on || s.manual);
    list(ui, &rows, &app.liked, tracks, !is_album, number, ListCtx { sel: &mut app.selection, find: &app.find, drag: &mut app.row_drag, movable, page: &app.page, own }, acts);
    if more {
        widgets::skeleton_rows(ui, now, 3);
    }
}

/// Whether every song of a fully loaded album or playlist is downloaded; worked out once
/// per list (its songs and the downloads folder) and kept until a download changes it.
fn all_downloaded(app: &mut App, id: &str) -> bool {
    use std::hash::{Hash, Hasher};
    let Some(o) = app.lists.get(id).filter(|o| o.complete) else { return false };
    // Not while it downloads: its files land one by one, and the run says how far it is.
    if app.download_runs.iter().any(|r| r.key == id) {
        return false;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    crate::downloads::folder().hash(&mut h);
    crate::downloads::looks().hash(&mut h);
    o.c.tracks.iter().for_each(|t| t.id.hash(&mut h));
    let seen = h.finish();
    if let Some(&(s, all)) = app.all_downloaded.get(id)
        && s == seen
    {
        return all;
    }
    let all = !o.c.tracks.is_empty() && crate::downloads::all_listed(&o.c.tracks);
    app.all_downloaded.insert(id.to_owned(), (seen, all));
    all
}

/// The cover of an album or playlist being downloaded, from its kept copy or its card.
fn run_thumbs<'a>(app: &'a App, key: &str) -> &'a [crate::model::Thumb] {
    if let Some(o) = app.opened(key) {
        return &o.c.thumbs;
    }
    let lib = app.library.iter().flat_map(|l| l.playlists.iter().chain(&l.albums));
    lib.chain(app.sidebar_cache.iter().flatten())
        .find(|c| matches!(&c.target, Target::Album(id) | Target::Playlist(id) if id == key))
        .map_or(&[], |c| &c.thumbs)
}

/// A download of many songs in Library › Downloads: its cover (veiled as it goes), its
/// name, how far it's got, and a button to cancel it. True when cancelled.
fn download_run(ui: &mut Ui, r: &crate::backend::DownloadRun, thumbs: &[crate::model::Thumb], width: f32) -> bool {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 64.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 10.0, theme::glass(0.04));
    let img = Rect::from_min_size(rect.min + vec2(10.0, 10.0), Vec2::splat(44.0));
    if r.title.is_empty() {
        p.rect_filled(img, theme::thumb_radius(), theme::glass_panel(0.07));
        widgets::icon_at(ui, img.center(), icon::DOWNLOAD, 20.0, theme::text_dim());
    } else {
        paint_cover(ui, img, thumbs, theme::thumb_radius(), 1.0);
        widgets::download_veil(ui, img, theme::thumb_radius(), &r.key);
    }
    let close = Rect::from_center_size(pos2(rect.right() - 26.0, rect.center().y), Vec2::splat(32.0));
    let x = img.right() + 14.0;
    let w = close.left() - 14.0 - x;
    let title = if r.title.is_empty() { "Selected songs" } else { &r.title };
    widgets::text_at(ui, pos2(x, rect.top() + 11.0), title, theme::semibold(14.0), theme::text(), w);
    widgets::text_at(ui, pos2(x, rect.top() + 30.0), &if r.cancelled { "Stopping\u{2026}".to_owned() } else { format!("{} of {} songs", r.done, r.total) }, theme::regular(12.5), theme::text_dim(), w);
    let bar = Rect::from_min_size(pos2(x, rect.bottom() - 12.0), vec2(w, 3.0));
    let f = theme::anim(ui.ctx(), ("download-run-bar", &r.key), r.done as f32 / r.total.max(1) as f32, 0.4);
    let accent = ui.ctx().data(|d| d.get_temp::<Color32>(egui::Id::unique("accent"))).unwrap_or(theme::RED);
    p.rect_filled(bar, 1.5, theme::glass(0.1));
    p.rect_filled(Rect::from_min_size(bar.min, vec2(w * f, bar.height())), 1.5, accent);
    if r.cancelled {
        return false;
    }
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(close));
    widgets::icon_button(&mut child, icon::X, 16.0, "Cancel download", false).clicked()
}

// ------------------------------------------------------------------ artist

fn artist(app: &mut App, ui: &mut Ui, id: &str, now: f64) {
    if app.artist.as_ref().is_none_or(|a| a.id != id) {
        placeholder(app, ui, now, 2, CARD);
        return;
    }
    let rows = Rows::new(app, now);
    let accent = app.accent_color();
    let Some(a) = &app.artist else { return };
    let subscribing = app.subscriptions.busy();
    let acts = &mut app.actions;
    let drag = &mut app.row_drag;
    let top: Vec<Track> = a.top.iter().filter(|t| rows.shows(t)).cloned().collect();
    let top = &top;
    // An artist of your uploads: all your songs by them, as on the website.
    let uploaded = crate::ytm::uploaded(id);
    // Hero banner fading into the page.
    let w = ui.available_width();
    let hero_h = (w * 0.34).clamp(240.0, 380.0);
    let (hero, _) = ui.allocate_exact_size(vec2(w, hero_h), Sense::hover());
    if let Some(t) = a.banner.iter().max_by_key(|t| t.width) {
        let img_ratio = if t.height > 0 { t.width as f32 / t.height as f32 } else { 16.0 / 9.0 };
        // Cover-fit: crop the image's top band to the hero's aspect.
        let vis_h = (img_ratio / (w / hero_h)).min(1.0);
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, vis_h));
        ui.painter().rect_filled(hero, 14.0, theme::glass_panel(0.07));
        widgets::paint_remote(ui, &t.url, hero, 14.0, uv, Color32::WHITE);
    } else {
        ui.painter().rect_filled(hero, 14.0, theme::with_alpha(accent, 0.25));
    }
    let [from, to] = crate::preset::active().app.header_gradient.unwrap_or([0.0, 0.85]);
    super::shell::vgradient(ui.painter(), Rect::from_min_max(pos2(hero.left(), hero.center().y - 40.0), hero.max), theme::shade(from), theme::shade(to));
    let base = hero.left_bottom() + vec2(28.0, -30.0);
    ui.painter().text(base - vec2(0.0, 44.0), Align2::LEFT_BOTTOM, &a.name, theme::bold(58.0), Color32::WHITE);
    if let Some(s) = a.subscribers {
        ui.painter().text(base - vec2(0.0, 18.0), Align2::LEFT_BOTTOM, format!("{} subscribers", compact(s)), theme::regular(14.0), theme::text_dim());
    }
    ui.add_space(18.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        play_pill(ui, acts, &Target::Artist(id.to_owned()), top, &app.find, &app.page);
        if pill(ui, "Shuffle", Some(icon::SHUFFLE), false).clicked() && !top.is_empty() {
            acts.push(Action::Shuffle(top.clone()));
        }
        if let Some(first) = top.first()
            && pill(ui, "Radio", Some(icon::RADIO), false).clicked() {
                acts.push(Action::Radio(first.clone()));
            }
        // Greyed out while a subscribe or unsubscribe runs: a click then wouldn't count.
        if a.channel_id.is_some()
            && ui.add_enabled_ui(!subscribing, |ui| pill(ui, if a.subscribed { "Subscribed" } else { "Subscribe" }, None, false)).inner.clicked()
        {
            acts.push(Action::Subscribe(!a.subscribed));
        }
    });
    if !top.is_empty() {
        ui.add_space(26.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(if uploaded { "Songs" } else { "Top songs" }).font(theme::bold(24.0)).color(theme::text()));
            if let Some(pl) = &a.top_playlist {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if pill(ui, "Show all", None, false).clicked() {
                        acts.push(Action::Open(Target::Playlist(pl.clone())));
                    }
                });
            }
        });
        ui.add_space(12.0);
        let plays_col = top.iter().take(5).any(|t| t.plays.is_some());
        // They can be dragged onto a playlist in the sidebar.
        let list = egui::Id::unique(("artist-top", id));
        // Two columns on a wide page, in the themes that have them.
        let cols = crate::preset::active().app.top_songs_cols.clamp(1, 2) as usize;
        if cols == 2 && !uploaded && ui.available_width() >= 900.0 {
            let shown = top.len().min(8);
            ui.columns(2, |c| {
                for (i, t) in top.iter().enumerate().take(shown) {
                    c[i % 2].push_id(i, |ui| {
                        row_drag::grip(ui, drag, list, i, t);
                        let o = RowOpts { plays_col: false, ..rows.opts(&app.liked, t, None, true) };
                        if track_row(ui, t, &o, acts).clicked() {
                            acts.push(Action::Play(top.clone(), i));
                        }
                    });
                }
            });
        } else {
        for (i, t) in top.iter().enumerate().take(if uploaded { top.len() } else { 5 }) {
            // Its own scope, so a picked-up row dims alone.
            ui.push_id(i, |ui| {
                row_drag::grip(ui, drag, list, i, t);
                let o = RowOpts { plays_col, ..rows.opts(&app.liked, t, None, true) };
                if track_row(ui, t, &o, acts).clicked() {
                    acts.push(Action::Play(top.clone(), i));
                }
            });
        }
        }
        row_drag::carry(ui, drag, list);
    }
    card_row(ui, "a-albums", "Albums & singles", &a.albums, acts);
    // Their songs you've saved, less those hidden as explicit.
    let library: Vec<&Card> = a.library.iter().filter(|c| !matches!(&c.target, Target::Song(t) if !rows.shows(t))).collect();
    if !library.is_empty() {
        cards_shelf(ui, &app.liked, acts, "a-lib", "From your library", None, None, &library);
    }
    card_row(ui, "a-pl", "Featured on", &a.playlists, acts);
    card_row(ui, "a-sim", "Fans might also like", &a.similar, acts);
    if let Some(d) = a.description.as_deref().filter(|d| !d.trim().is_empty()) {
        shelf_header(ui, "About", None, None, false);
        let g = widgets::galley_wrapped(ui, d.trim(), theme::regular(14.5), theme::text_dim(), ui.available_width().min(820.0), 8);
        ui.label(g);
    }
}

// ------------------------------------------------------------------ library

fn library(app: &mut App, ui: &mut Ui, tab: LibTab, now: f64) {
    page_title(ui, "Library");
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        let tabs = [
            (LibTab::Playlists, "Playlists"),
            (LibTab::Songs, "Liked songs"),
            (LibTab::Albums, "Albums"),
            (LibTab::Artists, "Artists"),
            (LibTab::Podcasts, "Podcasts"),
            (LibTab::History, "History"),
            (LibTab::Uploads, "Uploads"),
            (LibTab::Downloads, "Downloads"),
        ];
        let podcasts = app.show_podcasts;
        for (t, label) in tabs.into_iter().filter(|(t, _)| podcasts || *t != LibTab::Podcasts) {
            if chip(ui, label, tab == t).clicked() {
                // Tabs swap in place (no history entry); the current one refreshes.
                if tab == t {
                    app.refresh();
                } else {
                    app.page = Page::Library(t);
                    app.page_loaded();
                }
            }
        }
    });
    ui.add_space(12.0);
    // Downloads are on this computer; the rest is the account's.
    if !app.logged_in && tab != LibTab::Downloads {
        ui.add_space(10.0);
        ui.label(RichText::new("Sign in to see your playlists, liked songs and history.").color(theme::text_dim()));
        ui.add_space(12.0);
        if pill(ui, "Sign in with Google", Some(icon::CIRCLE_USER), true).clicked() {
            app.start_sign_in(ui.ctx());
        }
        return;
    }
    // `pinned`: on Library › Playlists, where playlists are pinned.
    let grid = |ui: &mut Ui, cards: &[Card], order: &[usize], pinned: Option<&[String]>, acts: &mut Vec<Action>| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(20.0, 22.0);
            for &i in order {
                let pin = pinned.map(|p| matches!(&cards[i].target, Target::Playlist(id) if p.contains(id)));
                card(ui, &cards[i], CARD, false, pin, acts);
            }
        });
    };
    let rows = Rows::new(app, now);
    let plain = |_: usize, _: &Track| None;
    match tab {
        LibTab::History => {
            if app.history.is_none() {
                placeholder(app, ui, now, 1, 48.0);
                return;
            }
            let Some(h) = &app.history else { return };
            ui.horizontal(|ui| find_box_right(ui, &mut app.find, &mut app.selection, &app.page, h));
            ui.add_space(8.0);
            list(ui, &rows, &app.liked, h, true, plain, ListCtx { sel: &mut app.selection, find: &app.find, drag: &mut app.row_drag, movable: false, page: &app.page, own: false }, &mut app.actions);
        }
        LibTab::Uploads => {
            ui.horizontal(|ui| {
                if pill(ui, "Upload music", Some(icon::UPLOAD), true).clicked() {
                    app.pick_uploads(ui.ctx());
                }
                if app.uploads.as_ref().is_some_and(|u| !u.is_empty()) && pill(ui, "Shuffle", Some(icon::SHUFFLE), false).clicked() {
                    app.actions.push(Action::Shuffle(app.uploads.clone().unwrap_or_default()));
                }
                if let Some(up) = &app.uploads {
                    find_box_right(ui, &mut app.find, &mut app.selection, &app.page, up);
                }
            });
            ui.add_space(6.0);
            ui.label(RichText::new("Or drop audio files anywhere on the window. MP3, M4A, FLAC, OGG and WMA.").font(theme::regular(13.0)).color(theme::text_dim()));
            ui.add_space(12.0);
            if app.uploads.is_none() {
                placeholder(app, ui, now, 1, 48.0);
                return;
            }
            let Some(up) = &app.uploads else { return };
            if up.is_empty() {
                ui.add_space(20.0);
                ui.label(RichText::new("No uploads yet").color(theme::text_dim()));
            }
            list(ui, &rows, &app.liked, up, true, plain, ListCtx { sel: &mut app.selection, find: &app.find, drag: &mut app.row_drag, movable: false, page: &app.page, own: false }, &mut app.actions);
        }
        LibTab::Downloads => {
            let Some(songs) = &app.downloads else { return };
            ui.horizontal(|ui| {
                if !songs.is_empty() {
                    if pill(ui, "Play", Some(icon::PLAY), true).clicked() {
                        app.actions.push(play_all(&app.find, &app.page, songs));
                    }
                    if pill(ui, "Shuffle", Some(icon::SHUFFLE), false).clicked() {
                        app.actions.push(Action::Shuffle(songs.clone()));
                    }
                }
                if pill(ui, "Open folder", Some(icon::FOLDER_OPEN), false).clicked() {
                    crate::downloads::open_folder();
                }
                if !songs.is_empty() {
                    select_pill(ui, &mut app.selection, &app.page);
                    find_box_right(ui, &mut app.find, &mut app.selection, &app.page, songs);
                }
            });
            // Side by side, as many to a line as fit at 280 wide or more.
            let mut cancel = None;
            let (gap, full) = (8.0, ui.available_width());
            let cols = (((full + gap) / (280.0 + gap)).floor() as usize).max(1);
            let w = (full - gap * (cols - 1) as f32) / cols as f32;
            for line in app.download_runs.chunks(cols) {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for r in line {
                        if download_run(ui, r, run_thumbs(app, &r.key), w) {
                            cancel = Some(r.key.clone());
                        }
                    }
                });
            }
            if let Some(key) = cancel {
                app.actions.push(Action::CancelDownload(key));
            }
            ui.add_space(6.0);
            ui.label(
                RichText::new("Songs saved on this computer. They play from their files, so they work offline and without signing in.")
                    .font(theme::regular(13.0))
                    .color(theme::text_dim()),
            );
            ui.add_space(12.0);
            if songs.is_empty() {
                ui.add_space(20.0);
                ui.label(RichText::new("No downloads yet. Download a song from its ⋯ menu, or a whole album or playlist from its page.").color(theme::text_dim()));
                return;
            }
            list(ui, &rows, &app.liked, songs, true, plain, ListCtx { sel: &mut app.selection, find: &app.find, drag: &mut app.row_drag, movable: false, page: &app.page, own: false }, &mut app.actions);
        }
        // Liked Music, kept like any playlist.
        LibTab::Songs => {
            if app.opened(super::LIKED_MUSIC).is_none() {
                placeholder(app, ui, now, 1, 48.0);
                return;
            }
            let more = app.receiving(super::LIKED_MUSIC);
            let Some(o) = app.lists.get(super::LIKED_MUSIC) else { return };
            app.find.hold(&app.page, !o.complete);
            let liked = &o.c.tracks;
            if !liked.is_empty() {
                ui.horizontal(|ui| {
                    if pill(ui, "Play", Some(icon::PLAY), true).clicked() {
                        app.actions.push(play_all(&app.find, &app.page, liked));
                    }
                    if pill(ui, "Shuffle", Some(icon::SHUFFLE), false).clicked() {
                        app.actions.push(Action::Shuffle(liked.clone()));
                    }
                    select_pill(ui, &mut app.selection, &app.page);
                    find_box_right(ui, &mut app.find, &mut app.selection, &app.page, liked);
                });
                ui.add_space(10.0);
            }
            list(ui, &rows, &app.liked, liked, true, plain, ListCtx { sel: &mut app.selection, find: &app.find, drag: &mut app.row_drag, movable: false, page: &app.page, own: false }, &mut app.actions);
            if more {
                widgets::skeleton_rows(ui, now, 3);
            }
        }
        _ => {
            if app.library.is_none() {
                placeholder(app, ui, now, 2, CARD);
                return;
            }
            let Some(lib) = &app.library else { return };
            let cards = match tab {
                LibTab::Playlists => &lib.playlists,
                LibTab::Albums => &lib.albums,
                LibTab::Artists => &lib.artists,
                LibTab::Podcasts => &lib.podcasts,
                LibTab::Songs | LibTab::History | LibTab::Uploads | LibTab::Downloads => unreachable!(),
            };
            let page = &app.page;
            ui.horizontal(|ui| {
                if tab == LibTab::Playlists && pill(ui, "New playlist", None, false).clicked() {
                    app.actions.push(Action::NewPlaylist(Vec::new()));
                }
                if !cards.is_empty() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| sort_pill(ui, &mut app.find, page));
                }
            });
            ui.add_space(14.0);
            let pinned = (tab == LibTab::Playlists).then_some(app.pinned.as_slice());
            let order = super::collections::pinned_first(pinned.unwrap_or_default(), cards, app.find.cards(page, cards));
            grid(ui, cards, &order, pinned, &mut app.actions);
        }
    }
}

// ------------------------------------------------------------------ settings

/// Settings' categories, listed beside the one on show.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum SettingsTab {
    #[default]
    Account,
    Playback,
    Lyrics,
    Appearance,
    Connections,
    General,
    Shortcuts,
    About,
}

impl SettingsTab {
    /// Most used first, which is how they're listed.
    pub(super) const ALL: [Self; 8] = [Self::Account, Self::Playback, Self::Lyrics, Self::Appearance, Self::Connections, Self::General, Self::Shortcuts, Self::About];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Playback => tr!("settings-playback"),
            Self::Lyrics => tr!("settings-lyrics"),
            Self::Appearance => tr!("settings-appearance"),
            Self::Connections => tr!("settings-connections"),
            Self::Account => tr!("settings-account"),
            Self::General => tr!("settings-general"),
            Self::Shortcuts => tr!("settings-keyboard-shortcuts"),
            Self::About => tr!("settings-about"),
        }
    }
}

/// What Settings' search box is looking for, for the frame being drawn. Only Settings
/// sets it, so the rows and cards other pages borrow stay as they are.
#[derive(Default)]
struct SettingsFilter {
    words: Vec<String>,
    /// The section and category being drawn, which count as part of each row's text.
    section: String,
    tab: &'static str,
    /// What's shown so far, in this section and on the whole page.
    in_section: usize,
    total: usize,
}

thread_local! {
    static FILTER: RefCell<SettingsFilter> = RefCell::default();
}

/// Every word is in `text` or the name of its section. `words` are lowercase.
fn words_match(words: &[String], section: &str, text: &str) -> bool {
    let (section, text) = (section.to_lowercase(), text.to_lowercase());
    words.iter().all(|w| section.contains(w.as_str()) || text.contains(w.as_str()))
}

pub(super) fn filtering() -> bool {
    FILTER.with_borrow(|f| !f.words.is_empty())
}

/// Whether what's looked for is in `text`, and it isn't counted as shown.
fn filter_hit(text: &str) -> bool {
    FILTER.with_borrow(|f| f.words.is_empty() || words_match(&f.words, &f.section, text))
}

/// Whether something described by `text` is shown. In a search it's counted, and set off
/// from what's above it, as the rows that would have gone between aren't there.
pub(super) fn filter_show(ui: &mut Ui, text: &str) -> bool {
    if !filtering() {
        return true;
    }
    if !filter_hit(text) {
        return false;
    }
    let first = FILTER.with_borrow_mut(|f| {
        f.in_section += 1;
        f.total += 1;
        f.in_section == 1
    });
    if !first {
        divider_line(ui);
    }
    true
}

/// A wide window has room for the categories beside what's in them; this is the page's
/// width below which they go above it.
const SETTINGS_SIDE_BY_SIDE: f32 = 680.0;
const SETTINGS_RAIL: f32 = 200.0;
const SETTINGS_GAP: f32 = 32.0;
const SETTINGS_COLUMN: f32 = 800.0;
const SETTINGS_SEARCH_H: f32 = 38.0;

fn settings(app: &mut App, ui: &mut Ui) {
    let words: Vec<String> = app.settings_query.to_lowercase().split_whitespace().map(str::to_owned).collect();
    FILTER.set(SettingsFilter { words, ..Default::default() });
    let full = ui.available_rect_before_wrap();
    // Its rail and heading don't scroll: clear of a frosted top bar and floating player
    // bar, which pages otherwise run under.
    let full = full.with_min_y(full.top() + app.page_insets[0]).with_max_y(full.bottom() - app.page_insets[1]);
    let side_by_side = full.width() >= SETTINGS_SIDE_BY_SIDE;
    let used = SETTINGS_RAIL + SETTINGS_GAP + SETTINGS_COLUMN;
    let left = full.left() + ((full.width() - used) / 2.0).max(0.0);
    let top_down = egui::Layout::top_down(egui::Align::Min);
    let body = if side_by_side {
        let rail = Rect::from_min_max(pos2(left, full.top()), pos2(left + SETTINGS_RAIL, full.bottom()));
        settings_rail(app, &mut ui.new_child(egui::UiBuilder::new().max_rect(rail).layout(top_down)));
        Rect::from_min_max(pos2(rail.right() + SETTINGS_GAP, full.top()), full.max)
    } else {
        let mut head = ui.new_child(egui::UiBuilder::new().max_rect(full.shrink2(vec2(16.0, 0.0))).layout(top_down));
        settings_head(app, &mut head);
        head.horizontal_wrapped(|ui| {
            let mut pick = None;
            for t in SettingsTab::ALL {
                if chip(ui, t.label(), !filtering() && t == app.settings_tab).clicked() {
                    pick = Some(t);
                }
            }
            if let Some(t) = pick {
                settings_open(app, t);
            }
        });
        Rect::from_min_max(pos2(full.left() + 16.0, head.min_rect().bottom()), full.max)
    };
    let mut area = egui::ScrollArea::vertical().id_salt(("settings", if filtering() { None } else { Some(app.settings_tab.label()) })).auto_shrink(false);
    // Dev hook: YTM_SCROLL=<px> pins the page scroll, for screenshots of lower sections.
    if let Some(y) = super::dev_var("YTM_SCROLL").and_then(|v| v.parse::<f32>().ok()) {
        area = area.vertical_scroll_offset(y);
    }
    let mut column = ui.new_child(egui::UiBuilder::new().max_rect(body).layout(top_down));
    area.show_viewport(&mut column, |ui, viewport| {
        ui.set_max_width((ui.available_width() - 20.0).clamp(0.0, SETTINGS_COLUMN));
        if filtering() {
            for tab in SettingsTab::ALL {
                FILTER.with_borrow_mut(|f| f.tab = tab.label());
                settings_tab(app, ui, tab);
            }
            if FILTER.with_borrow(|f| f.total == 0) {
                ui.add_space(40.0);
                ui.label(RichText::new(tr!("settings-no-match", query = app.settings_query.trim())).font(theme::regular(14.0)).color(theme::text_dim()));
            }
        } else {
            settings_tab(app, ui, app.settings_tab);
        }
        ui.add_space(40.0);
        // A search's results follow what's typed.
        if !filtering() {
            let h = ui.min_rect().height();
            let room = settle_room(ui.ctx(), viewport.bottom() - h, (viewport.height() - h).max(0.0), h);
            if room > 0.0 {
                ui.add_space(room);
            }
        }
    });
    ui.advance_cursor_after_rect(full);
    FILTER.take();
}

/// Empty room under the settings, `h` tall now, when a section folds away (Discord's,
/// switched off) while scrolled down: while the page shrinks, all the room it takes to
/// stay where it's scrolled to (`needed`), so the section folds up under the pointer
/// rather than pull the page down; then less and less, so the page glides to its end.
/// `fill`: what only fills out a page shorter than the window, which moves nothing, so
/// it isn't part of the glide.
fn settle_room(ctx: &egui::Context, needed: f32, fill: f32, h: f32) -> f32 {
    #[derive(Clone, Copy, Default)]
    struct Settle {
        h: f32,
        /// The room that keeps the page where it was, past `fill`.
        scroll: f32,
        since: f64,
    }
    const SECS: f64 = 0.45;
    let id = egui::Id::unique("settings-settle");
    let now = ctx.input(|i| i.time);
    let mut s = ctx.data(|d| d.get_temp::<Settle>(id)).unwrap_or_default();
    let needed = needed.max(0.0);
    if h < s.h - 0.5 {
        s.scroll = (needed - fill).max(0.0);
        s.since = now;
    }
    s.h = h;
    ctx.data_mut(|d| d.insert_temp(id, s));
    let t = ((now - s.since) / SECS) as f32;
    // Scrolling back up meanwhile gives it back sooner.
    let room = (fill + s.scroll * (1.0 - theme::ease_in_out(t.clamp(0.0, 1.0)))).min(needed);
    if t < 1.0 && s.scroll > 0.0 {
        ctx.request_repaint();
    }
    room
}

/// Picking a category ends a search: it's where you were looking to go.
fn settings_open(app: &mut App, tab: SettingsTab) {
    app.settings_tab = tab;
    app.settings_query.clear();
}

fn settings_tab(app: &mut App, ui: &mut Ui, tab: SettingsTab) {
    match tab {
        SettingsTab::Playback => settings_playback(app, ui),
        SettingsTab::Lyrics => settings_lyrics(app, ui),
        SettingsTab::Appearance => {
            settings_appearance(app, ui);
            settings_mini(app, ui);
            settings_app_icon(app, ui);
        }
        SettingsTab::Account => settings_account(app, ui),
        SettingsTab::Connections => {
            settings_connections(app, ui);
            super::presence::settings(app, ui);
        }
        SettingsTab::General => {
            settings_language(app, ui);
            settings_window(app, ui);
            settings_downloads(ui);
            super::transfer::settings_section(app, ui);
            settings_section(ui, tr!("settings-network"), None, None, |ui| network(app, ui));
            // Last, as most apps keep it.
            settings_storage(app, ui);
        }
        SettingsTab::Shortcuts => {
            super::hotkeys::settings_section(app, ui);
            settings_shortcuts(ui);
        }
        SettingsTab::About => {
            settings_updates(app, ui);
            settings_about(ui);
            if !filtering() {
                settings_sign_off(app, ui);
            }
        }
    }
}

/// The page's title and its search box.
fn settings_head(app: &mut App, ui: &mut Ui) {
    page_title(ui, tr!("settings-title"));
    ui.add_space(12.0);
    settings_search(app, ui);
    ui.add_space(14.0);
}

/// The title, the search box and the categories, on a wide window.
fn settings_rail(app: &mut App, ui: &mut Ui) {
    settings_head(app, ui);
    let mut pick = None;
    for t in SettingsTab::ALL {
        if settings_rail_row(ui, t.label(), !filtering() && t == app.settings_tab).clicked() {
            pick = Some(t);
        }
    }
    if let Some(t) = pick {
        settings_open(app, t);
    }
}

fn settings_rail_row(ui: &mut Ui, label: &str, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    let resp = named(resp, label).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
    let s = theme::anim_bool(ui.ctx(), resp.id.with("s"), selected, 0.15);
    if h + s > 0.0 {
        ui.painter().rect_filled(rect, 10.0, theme::glass(0.05 * h + 0.10 * s));
    }
    let color = theme::lerp_color(theme::text_dim(), theme::text(), h.max(s));
    ui.painter().text(pos2(rect.left() + 14.0, rect.center().y), Align2::LEFT_CENTER, label, theme::semibold(14.0), color);
    resp
}

fn settings_search(app: &mut App, ui: &mut Ui) {
    let (rect, field) = ui.allocate_exact_size(vec2(ui.available_width(), SETTINGS_SEARCH_H), Sense::click());
    let field = field.on_hover_cursor(egui::CursorIcon::Text);
    ui.painter().rect_filled(rect, SETTINGS_SEARCH_H / 2.0, theme::glass(0.09));
    widgets::icon_at(ui, pos2(rect.left() + 18.0, rect.center().y), icon::SEARCH, 15.0, theme::text_dim());
    let clearable = !app.settings_query.is_empty();
    let end = rect.right() - if clearable { 34.0 } else { 12.0 };
    let inner = Rect::from_min_max(pos2(rect.left() + 34.0, rect.top() + 3.0), pos2(end, rect.bottom() - 3.0));
    let edit = ui.put(
        inner,
        egui::TextEdit::singleline(&mut app.settings_query)
            .hint_text(RichText::new(tr!("settings-search-box")).color(theme::text_faint()))
            .font(theme::regular(14.0))
            .frame(egui::Frame::NONE)
            .align(Align2::LEFT_CENTER)
            .desired_width(inner.width()),
    );
    if clearable {
        let x = Rect::from_center_size(pos2(rect.right() - 19.0, rect.center().y), Vec2::splat(26.0));
        let resp = named(ui.interact(x, ui.scope_id().with("settings-clear"), Sense::click()), tr!("settings-clear")).on_hover_cursor(egui::CursorIcon::PointingHand);
        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
        if h > 0.0 {
            ui.painter().circle_filled(x.center(), 13.0, theme::glass(0.10 * h));
        }
        widgets::icon_at(ui, x.center(), icon::X, 14.0, theme::lerp_color(theme::text_dim(), theme::text(), h));
        if resp.clicked() {
            app.settings_query.clear();
            edit.request_focus();
        }
    }
    if field.clicked() {
        edit.request_focus();
    }
    // Esc takes the focus from a text box: it clears this one too.
    if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.settings_query.clear();
    }
}


/// Settings › Account: signing in and out, and which channel to use.
fn settings_account(app: &mut App, ui: &mut Ui) {
    let busy = app.signing_in.load(Ordering::Relaxed);
    let (mut sign_in, mut sign_out) = (false, false);
    // Asked again now and then while Settings shows: a failed ask, or a channel made since.
    let now = ui.input(|i| i.time);
    if app.logged_in && app.channels_asked.is_none_or(|at| now - at > 60.0) {
        app.channels_asked = Some(now);
        app.backend.request(crate::backend::Req::Channels);
    }
    settings_section(ui, tr!("settings-account"), None, None, |ui| {
        if app.logged_in {
            let name = app.account_name.clone().unwrap_or_else(|| "YouTube Music".to_owned());
            setting_row(ui, &name, Some(tr!("settings-signed-in-desc")), |ui| {
                sign_out = pill(ui, tr!("settings-sign-out"), Some(icon::LOG_OUT), false).clicked();
            });
            // Also with one, when the one in use isn't it (a brand channel since deleted).
            if app.channels.as_ref().is_some_and(|c| c.list.len() > 1 || !c.list.iter().any(|l| l.page_id == c.current)) {
                divider(ui);
                setting_row(ui, tr!("settings-channel"), Some(tr!("settings-channel-desc")), |ui| {
                    channel_picker(app, ui);
                });
            }
        } else {
            setting_row(ui, tr!("settings-not-signed-in"), Some(tr!("settings-not-signed-in-desc")), |ui| {
                if busy {
                    widgets::loader(ui);
                } else {
                    sign_in = pill(ui, tr!("settings-sign-in-with-google"), Some(icon::CIRCLE_USER), true).clicked();
                }
            });
        }
    });
    if busy && !filtering() {
        ui.add_space(6.0);
        ui.label(RichText::new(tr!("settings-sign-in-finish")).font(theme::regular(12.5)).color(theme::text_dim()));
    }
    if sign_in {
        app.start_sign_in(ui.ctx());
    }
    if sign_out {
        app.settle_now();
        app.backend.request(crate::backend::Req::Logout);
    }
}

fn channel_picker(app: &mut App, ui: &mut Ui) {
    let Some(crate::backend::Channels { list, current, epoch }) = &app.channels else { return };
    // A brand channel often has the account's name: the handle, or "No channel" (as
    // YouTube's switcher says), tells them apart.
    let label = |c: &crate::ytm::Channel| match (&c.handle, c.has_channel) {
        (Some(h), _) => format!("{} ({h})", c.name),
        (None, false) => tr!("settings-no-channel", name = c.name.as_str()),
        (None, true) => c.name.clone(),
    };
    // The button is narrower: a handle alone is enough there.
    let shown = match list.iter().find(|c| c.page_id == *current) {
        Some(c) => c.handle.clone().unwrap_or_else(|| label(c)),
        None => tr!("settings-another-channel").to_owned(),
    };
    // A transfer would go on as the new channel.
    if app.transfer.running() {
        ui.label(RichText::new(tr!("settings-channel-not-during-transfer")).font(theme::regular(13.0)).color(theme::text_dim()));
        return;
    }
    let switching = app.switching_to.is_some();
    let btn = dropdown_button(ui, if switching { tr!("settings-switching") } else { &shown }, 220.0, tr!("settings-channel"));
    let mut pick = None;
    widgets::menu_popup(&btn).show(|ui| {
        widgets::menu_body(ui, 300.0);
        dropdown_list(ui, 320.0, |ui| {
            for c in list {
                if option_row(ui, &label(c), c.page_id == *current).clicked() {
                    pick = Some(c.clone());
                    ui.close();
                }
            }
        });
    });
    if let Some(c) = pick.filter(|c| c.page_id != *current && !switching) {
        app.switching_to = Some(label(&c));
        let epoch = *epoch;
        app.settle_now();
        app.backend.request(crate::backend::Req::SwitchChannel { page_id: c.page_id, epoch });
    }
}

/// Settings › Playback.
fn settings_playback(app: &mut App, ui: &mut Ui) {
    settings_section(ui, tr!("settings-playback"), None, None, |ui| {
        setting_row(ui, tr!("settings-autoplay"), Some(tr!("settings-autoplay-desc")), |ui| {
            toggle(ui, &mut app.autoplay, tr!("settings-autoplay"));
        });
        divider(ui);
        setting_row(ui, tr!("settings-crossfade-songs"), Some(tr!("settings-crossfade-songs-desc")), |ui| {
            if toggle(ui, &mut app.crossfade, tr!("settings-crossfade-songs")).changed() {
                app.set_crossfade();
            }
        });
        if app.crossfade {
            divider(ui);
            setting_row_wide(ui, tr!("settings-crossfade-length"), None, SLIDER_W, |ui| {
                if seconds_slider(ui, &mut app.crossfade_secs, 1..=super::CROSSFADE_MAX, tr!("settings-crossfade-length")).changed() {
                    app.set_crossfade();
                }
            });
            divider(ui);
            setting_row(ui, tr!("settings-crossfade-when-skipping"), Some(tr!("settings-crossfade-when-skipping-desc")), |ui| {
                toggle(ui, &mut app.crossfade_skips, tr!("settings-crossfade-when-skipping"));
            });
        }
        divider(ui);
        setting_row(ui, tr!("settings-hide-explicit-songs"), Some(tr!("settings-hide-explicit-songs-desc")), |ui| {
            if toggle(ui, &mut app.hide_explicit, tr!("settings-hide-explicit-songs")).changed() {
                app.set_hide_explicit();
            }
        });
        divider(ui);
        setting_row(ui, tr!("settings-show-podcasts"), Some(tr!("settings-show-podcasts-desc")), |ui| {
            if toggle(ui, &mut app.show_podcasts, tr!("settings-show-podcasts")).changed() {
                app.set_show_podcasts();
            }
        });
        divider(ui);
        setting_row(ui, tr!("settings-show-comments"), Some(tr!("settings-show-comments-desc")), |ui| {
            if toggle(ui, &mut app.show_comments, tr!("settings-show-comments")).changed() && !app.show_comments && app.np_tab == super::NpTab::Comments {
                app.np_tab = super::NpTab::Art;
            }
        });
        divider(ui);
        // Only with hiding on: it's off (and shown off) until then.
        ui.add_enabled_ui(app.hide_explicit, |ui| {
            setting_row(ui, tr!("settings-use-youtubes-restricted-mode"), Some(tr!("settings-use-youtubes-restricted-mode-desc")), |ui| {
                let mut on = app.hide_explicit && app.restricted_mode;
                if toggle(ui, &mut on, tr!("settings-use-youtubes-restricted-mode")).changed() {
                    app.restricted_mode = on;
                    app.set_hide_explicit();
                }
            });
        });
    });
    settings_section(ui, tr!("settings-song-ratings"), None, None, |ui| {
        let ratings = &mut app.rating_settings;
        for (i, (title, description, value)) in [
            (tr!("settings-ratings-player-button"), tr!("settings-ratings-player-button-desc"), &mut ratings.player_button),
            (tr!("settings-ratings-list-icons"), tr!("settings-ratings-list-icons-desc"), &mut ratings.list_icons),
            (tr!("settings-ratings-dim-disliked"), tr!("settings-ratings-dim-disliked-desc"), &mut ratings.dim_disliked),
            (tr!("settings-ratings-skip-disliked"), tr!("settings-ratings-skip-disliked-desc"), &mut ratings.skip_disliked),
            (tr!("settings-ratings-dislike-notices"), tr!("settings-ratings-dislike-notices-desc"), &mut ratings.dislike_notices),
            (tr!("settings-ratings-unlike-notices"), tr!("settings-ratings-unlike-notices-desc"), &mut ratings.unlike_notices),
            (tr!("settings-ratings-safe-likes"), tr!("settings-ratings-safe-likes-desc"), &mut ratings.confirm_unlike),
        ].into_iter().enumerate() {
            if i != 0 {
                divider(ui);
            }
            setting_row(ui, title, Some(description), |ui| {
                toggle(ui, value, title);
            });
        }
        divider(ui);
        setting_row(ui, tr!("settings-ratings-hold-to-remove"), None, |ui| {
            ui.add_enabled_ui(ratings.confirm_unlike, |ui| {
                use super::ratings::{DEFAULT_UNLIKE_MS, MAX_UNLIKE_MS, MIN_UNLIKE_MS};
                let font = egui::TextStyle::Body.resolve(ui.style());
                let width = ui.painter().layout_no_wrap("8888 ms".to_owned(), font, theme::text()).size().x;
                ui.add_sized(vec2(width, 26.0), egui::Label::new(format!("{:.0} ms", ratings.unlike_hold_ms)).halign(egui::Align::RIGHT));
                slider_track(ui, &mut ratings.unlike_hold_ms, (MIN_UNLIKE_MS, MAX_UNLIKE_MS, 50.0), Some(DEFAULT_UNLIKE_MS), 112.0, tr!("settings-ratings-hold-to-remove"));
            });
        });
    });
    settings_section(ui, tr!("settings-sound"), None, None, |ui| {
        let desc = normalization_desc(app);
        setting_row(ui, tr!("settings-volume-normalization"), Some(&desc), |ui| {
            use crate::audio::Normalize;
            let btn = dropdown_button(ui, app.normalize.label(), 120.0, tr!("settings-volume-normalization"));
            widgets::menu_popup(&btn).show(|ui| {
                widgets::menu_body(ui, 120.0);
                for level in Normalize::ALL {
                    if option_row(ui, level.label(), level == app.normalize).clicked() {
                        app.normalize = level;
                        app.audio.set_normalize(level);
                        ui.close();
                    }
                }
            });
        });
        divider(ui);
        setting_row(ui, tr!("settings-logarithmic-volume"), Some(tr!("settings-logarithmic-volume-desc")), |ui| {
            if toggle(ui, &mut app.log_volume, tr!("settings-logarithmic-volume")).changed() {
                app.audio.set_log_volume(app.log_volume);
            }
        });
        divider(ui);
        setting_row(ui, tr!("settings-fade-when-pausing"), Some(tr!("settings-fade-when-pausing-desc")), |ui| {
            if toggle(ui, &mut app.pause_fade, tr!("settings-fade-when-pausing")).changed() {
                app.audio.set_pause_fade(app.pause_fade);
            }
        });
        divider(ui);
        setting_row(ui, tr!("settings-high-quality-audio"), Some(tr!("settings-high-quality-audio-desc")), |ui| {
            if toggle(ui, &mut app.high_quality, tr!("settings-high-quality-audio")).changed() {
                app.backend.set_high_quality(app.high_quality);
            }
        });
        divider(ui);
        stereo_settings(app, ui);
        divider(ui);
        setting_row(ui, tr!("settings-output-device"), Some(tr!("settings-output-device-desc")), |ui| {
            output_device_picker(app, ui);
        });
    });
    settings_equalizer(app, ui);
    settings_tempo(app, ui);
    settings_section(ui, tr!("settings-music-videos"), None, None, |ui| {
        setting_row(ui, tr!("settings-songs-not-videos"), Some(tr!("settings-songs-not-videos-desc")), |ui| {
            toggle(ui, &mut app.play_songs, tr!("settings-songs-not-videos"));
        });
        divider(ui);
        setting_row(ui, tr!("settings-show-music-videos"), Some(tr!("settings-show-music-videos-desc")), |ui| {
            let mut on = app.show_video;
            if toggle(ui, &mut on, tr!("settings-show-music-videos")).changed() {
                app.actions.push(Action::ToggleVideo);
            }
        });
        divider(ui);
        setting_row(ui, tr!("settings-video-quality"), Some(tr!("settings-video-quality-desc")), |ui| {
            let label = |h: u32| format!("{h}p");
            let btn = dropdown_button(ui, &label(app.video_quality), 120.0, tr!("settings-video-quality"));
            widgets::menu_popup(&btn).show(|ui| {
                widgets::menu_body(ui, 120.0);
                for h in super::VIDEO_QUALITIES {
                    if option_row(ui, &label(h), h == app.video_quality).clicked() {
                        super::nowplaying::set_video_quality(app, h);
                        ui.close();
                    }
                }
            });
        });
        divider(ui);
        setting_row(ui, tr!("settings-ambient-light"), Some(tr!("settings-ambient-light-desc")), |ui| {
            toggle(ui, &mut app.ambient_light, tr!("settings-ambient-light"));
        });
        divider(ui);
        setting_row(ui, tr!("settings-animated-covers"), Some(tr!("settings-animated-covers-desc")), |ui| {
            toggle(ui, &mut app.animated_covers, tr!("settings-animated-covers"));
        });
        divider(ui);
        setting_row(ui, tr!("settings-hide-videos-in-search"), Some(tr!("settings-hide-videos-in-search-desc")), |ui| {
            toggle(ui, &mut app.hide_search_videos, tr!("settings-hide-videos-in-search"));
        });
        divider(ui);
        setting_row(ui, tr!("settings-skip-non-music-parts"), Some(tr!("settings-skip-non-music-parts-desc")), |ui| {
            if toggle(ui, &mut app.sponsorblock, tr!("settings-skip-non-music-parts")).changed() {
                app.request_segments();
            }
        });
    });
}

/// Settings › Downloads.
fn settings_downloads(ui: &mut Ui) {
    settings_section(ui, tr!("settings-downloads"), None, Some(tr!("settings-downloads-note")), |ui| {
        let folder = crate::downloads::folder().display().to_string();
        setting_row(ui, tr!("settings-download-folder"), Some(&folder), |ui| {
            if pill(ui, tr!("settings-change"), None, false).clicked() {
                crate::downloads::pick_folder(ui.ctx());
            }
            ui.add_space(6.0);
            if pill(ui, tr!("settings-open"), Some(icon::FOLDER_OPEN), false).clicked() {
                crate::downloads::open_folder();
            }
        });
        divider(ui);
        setting_row(ui, tr!("settings-folder-per-list"), Some(tr!("settings-folder-per-list-desc")), |ui| {
            let mut on = crate::downloads::per_list();
            if toggle(ui, &mut on, tr!("settings-folder-per-list")).changed() {
                crate::downloads::set_per_list(on);
            }
        });
    });
}

/// Settings › Storage: how much room the cache takes, and clearing it.
fn settings_storage(app: &mut App, ui: &mut Ui) {
    if let Some(freed) = app.storage.take_freed() {
        app.toast(tr!("settings-cache-cleared", size = crate::cache::size_text(freed)));
    }
    let now = ui.input(|i| i.time);
    let size = app.storage.size(ui.ctx(), app.backend.data_dir(), now);
    let clearing = app.storage.clearing();
    settings_section(ui, tr!("settings-storage"), None, None, |ui| {
        let desc = match size {
            _ if clearing => tr!("settings-clearing").to_owned(),
            Some(n) => crate::cache::size_text(n),
            None => tr!("settings-measuring").to_owned(),
        };
        setting_row(ui, tr!("settings-cache"), Some(&desc), |ui| {
            if pill(ui, tr!("settings-clear-cache"), None, false).clicked() && !clearing {
                app.dialog = Some(super::Dialog::ConfirmClearCache);
            }
        });
    });
}

/// Settings › Lyrics: the better-lyrics extension's options.
fn settings_lyrics(app: &mut App, ui: &mut Ui) {
    settings_section(ui, tr!("settings-lyrics"), None, None, |ui| {
        setting_row(ui, tr!("settings-romanization"), Some(tr!("settings-romanization-desc")), |ui| {
            toggle(ui, &mut app.show_roman, tr!("settings-romanization"));
        });
        divider(ui);
        setting_row(ui, tr!("settings-translation"), Some(tr!("settings-translation-desc")), |ui| {
            toggle(ui, &mut app.show_trans, tr!("settings-translation"));
        });
        divider(ui);
        setting_row(ui, tr!("settings-translate-into"), None, |ui| {
            let current = super::lyric_prefs::LANGUAGES.iter().find(|(c, _)| *c == app.lyric_prefs.target).map_or(app.lyric_prefs.target.as_str(), |(_, n)| n);
            let btn = dropdown_button(ui, current, 200.0, tr!("settings-translate-into"));
            let just_opened = btn.clicked();
            widgets::menu_popup(&btn).show(|ui| {
                widgets::menu_body(ui, 200.0);
                dropdown_list(ui, 320.0, |ui| {
                    for (code, name) in super::lyric_prefs::LANGUAGES {
                        let on = app.lyric_prefs.target == *code;
                        let row = option_row(ui, name, on);
                        if on && just_opened {
                            row.scroll_to_me(Some(egui::Align::Center));
                        }
                        if row.clicked() {
                            app.lyric_prefs.target = (*code).to_owned();
                            ui.close();
                        }
                    }
                });
            });
        });
        divider(ui);
        setting_row(ui, tr!("settings-text-size"), Some(tr!("settings-text-size-desc")), |ui| {
            let (min, max, step) = super::lyric_prefs::TEXT_SCALE;
            let v = &mut app.lyric_prefs.text_scale;
            if ui.add_enabled(*v < max - 0.01, egui::Button::new("+").min_size(vec2(28.0, 26.0))).on_hover_text(tr!("settings-larger")).clicked() {
                *v = ((*v + step) * 10.0).round() / 10.0;
            }
            ui.label(egui::RichText::new(format!("{:.0}%", *v * 100.0)).font(theme::semibold(13.0)));
            if ui.add_enabled(*v > min + 0.01, egui::Button::new("−").min_size(vec2(28.0, 26.0))).on_hover_text(tr!("settings-smaller")).clicked() {
                *v = ((*v - step) * 10.0).round() / 10.0;
            }
        });
        divider(ui);
        setting_row(ui, tr!("settings-quick-line-changes"), Some(tr!("settings-quick-line-changes-desc")), |ui| {
            toggle(ui, &mut app.quick_lyric_scroll, tr!("settings-quick-line-changes"));
        });
        divider(ui);
        setting_row(ui, tr!("settings-lyrics-full-frame-rate"), Some(tr!("settings-lyrics-full-frame-rate-desc")), |ui| {
            toggle(ui, &mut app.lyrics_full_rate, tr!("settings-lyrics-full-frame-rate"));
        });
    });
    settings_section(ui, tr!("settings-lyrics-timing"), None, None, |ui| {
        // Offsets: seconds taken off the lyrics clock, as the extension's dock sets them.
        for (i, (title, desc, which)) in [
            (tr!("settings-timing-offset"), tr!("settings-timing-offset-desc"), 0),
            (tr!("settings-word-synced-trim"), tr!("settings-word-synced-trim-desc"), 1),
            (tr!("settings-line-synced-trim"), tr!("settings-line-synced-trim-desc"), 2),
        ]
        .into_iter()
        .enumerate()
        {
            if i > 0 {
                divider(ui);
            }
            setting_row(ui, title, Some(desc), |ui| {
                let v = match which {
                    0 => &mut app.lyric_prefs.global_offset,
                    1 => &mut app.lyric_prefs.word_trim,
                    _ => &mut app.lyric_prefs.line_trim,
                };
                if ui.add(egui::Button::new("+").min_size(vec2(28.0, 26.0))).on_hover_text(tr!("settings-later")).clicked() {
                    *v = ((*v + 0.1) * 10.0).round() / 10.0;
                }
                ui.label(egui::RichText::new(format!("{:+.1} s", *v).replace("+0.0", "0.0")).font(theme::semibold(13.0)));
                if ui.add(egui::Button::new("−").min_size(vec2(28.0, 26.0))).on_hover_text(tr!("settings-earlier")).clicked() {
                    *v = ((*v - 0.1) * 10.0).round() / 10.0;
                }
            });
        }
    });
    settings_section(ui, tr!("settings-lyrics-sources"), None, None, |ui| {
        // Source order: the first enabled source with lyrics for a song is shown; the
        // lyrics toolbar can pick another for that song.
        let names = app.lyric_prefs.order.iter().map(|(p, _)| p.name()).collect::<Vec<_>>().join(" ");
        if !filter_show(ui, &format!("Sources tried in this order switch off toolbar {names}")) {
            return;
        }
        ui.add_space(12.0);
        ui.label(RichText::new(tr!("settings-lyrics-sources-note")).font(theme::regular(12.5)).color(theme::text_dim()));
        ui.add_space(8.0);
        let n = app.lyric_prefs.order.len();
        let mut swap: Option<(usize, usize)> = None;
        for (i, entry) in app.lyric_prefs.order.iter_mut().enumerate() {
            let (p, on) = *entry;
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
            let mut row = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
            let mut enabled = on;
            if toggle(&mut row, &mut enabled, p.name()).changed() {
                entry.1 = enabled;
            }
            row.add_space(10.0);
            let kind = match p.sync() {
                crate::lyrics::SyncKind::Syllable => "syllable",
                crate::lyrics::SyncKind::Word => "word",
                crate::lyrics::SyncKind::Line => "line",
                crate::lyrics::SyncKind::Unsynced => "plain",
            };
            row.label(egui::RichText::new(p.name()).font(theme::semibold(13.5)).color(if on { theme::text() } else { theme::text_dim() }));
            row.label(egui::RichText::new(kind).font(theme::regular(12.0)).color(theme::text_dim()));
            row.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(i + 1 < n, egui::Button::new("↓").min_size(vec2(28.0, 24.0))).on_hover_text(tr!("settings-lower")).clicked() {
                    swap = Some((i, i + 1));
                }
                if ui.add_enabled(i > 0, egui::Button::new("↑").min_size(vec2(28.0, 24.0))).on_hover_text(tr!("settings-higher")).clicked() {
                    swap = Some((i, i - 1));
                }
            });
        }
        if let Some((a, b)) = swap {
            app.lyric_prefs.order.swap(a, b);
        }
        ui.add_space(6.0);
    });
}

/// Settings › Appearance and Theme.
fn settings_appearance(app: &mut App, ui: &mut Ui) {
    let was_theme = app.theme_id.clone();
    settings_section(ui, tr!("settings-appearance"), None, None, |ui| {
        let desc = if cfg!(windows) {
            tr!("settings-mode-desc-windows")
        } else {
            tr!("settings-mode-desc")
        };
        let mut pick = None;
        setting_row(ui, tr!("settings-mode"), Some(desc), |ui| {
            let btn = dropdown_button(ui, app.appearance.label(), 160.0, tr!("settings-mode"));
            widgets::menu_popup(&btn).show(|ui| {
                widgets::menu_body(ui, 160.0);
                for a in theme::Appearance::ALL {
                    if option_row(ui, a.label(), a == app.appearance).clicked() {
                        pick = Some(a);
                        ui.close();
                    }
                }
            });
        });
        if let Some(a) = pick {
            app.appearance = a;
        }
        divider(ui);
        {
            use super::AccentSource;
            // System is offered where there's a system accent to follow (Windows).
            let has_system = theme::system_accent().is_some();
            let desc = if app.accent_source == AccentSource::File {
                tr!("settings-accent-desc-file").to_owned()
            } else if has_system {
                tr!("settings-accent-desc-windows").to_owned()
            } else {
                tr!("settings-accent-desc").to_owned()
            };
            setting_row(ui, tr!("settings-accent-colour"), Some(&desc), |ui| {
                let btn = dropdown_button(ui, app.accent_source.label(), 120.0, tr!("settings-accent-colour"));
                let mut pick = None;
                widgets::menu_popup(&btn).show(|ui| {
                    widgets::menu_body(ui, 120.0);
                    let current = app.accent_source;
                    // Custom starts from the colour showing now (before light pages deepen
                    // it, or it would be deepened twice).
                    let options = [AccentSource::Theme, AccentSource::System, AccentSource::File, AccentSource::Custom(app.base_accent())];
                    for o in options.into_iter().filter(|o| has_system || *o != AccentSource::System) {
                        let selected = std::mem::discriminant(&o) == std::mem::discriminant(&current);
                        if option_row(ui, o.label(), selected).clicked() {
                            pick = (!selected).then_some(o);
                            ui.close();
                        }
                    }
                });
                // Picking the file opens it, with a starter one written if there's none yet.
                let mut edit = pick == Some(AccentSource::File);
                if let Some(p) = pick {
                    if p == AccentSource::File
                        && let Err(e) = theme::ensure_theme_file(&app.theme_file(), app.base_accent())
                    {
                        log::warn!("theme file: couldn't write {}: {e}", app.theme_file().display());
                    }
                    app.accent_source = p;
                }
                if app.accent_source == AccentSource::File {
                    ui.add_space(8.0);
                    edit |= pill(ui, tr!("settings-edit-file"), None, false).clicked();
                }
                if edit {
                    let file = app.theme_file();
                    if let Err(e) = theme::ensure_theme_file(&file, app.base_accent()) {
                        log::warn!("theme file: couldn't write {}: {e}", file.display());
                    }
                    theme::open_in_editor(&file);
                }
                if let AccentSource::Custom(c) = &mut app.accent_source {
                    ui.add_space(8.0);
                    super::colorpick::button(ui, c, tr!("settings-custom-accent-colour"));
                }
            });
        }
        divider(ui);
        setting_row(ui, tr!("settings-accent-coloured-controls"), Some(tr!("settings-accent-coloured-controls-desc")), |ui| {
            toggle(ui, &mut app.accent_controls, tr!("settings-accent-coloured-controls"));
        });
        divider(ui);
        setting_row(ui, tr!("settings-always-show-seek-knob"), Some(tr!("settings-always-show-seek-knob-desc")), |ui| {
            toggle(ui, &mut app.seek_knob_always, tr!("settings-always-show-seek-knob"));
        });
        if crate::backdrop::supported() {
            divider(ui);
            settings_backdrop(app, ui);
        }
        divider(ui);
        setting_row(ui, tr!("settings-animated-background"), Some(tr!("settings-animated-background-desc")), |ui| {
            toggle(ui, &mut app.anim_bg, tr!("settings-animated-background"));
        });
        divider(ui);
        setting_row(ui, tr!("settings-frosted-glass"), Some(tr!("settings-frosted-glass-desc")), |ui| {
            toggle(ui, &mut app.glass_on, tr!("settings-frosted-glass"));
        });
        divider(ui);
        setting_row(ui, tr!("settings-pulse-with-the-music"), Some(tr!("settings-pulse-with-the-music-desc")), |ui| {
            toggle(ui, &mut app.reactive_bg, tr!("settings-pulse-with-the-music"));
        });
        divider(ui);
        setting_row(ui, tr!("settings-dim-bright-covers"), Some(tr!("settings-dim-bright-covers-desc")), |ui| {
            toggle(ui, &mut app.dim_bright, tr!("settings-dim-bright-covers"));
        });
        divider(ui);
        setting_row(ui, tr!("settings-font"), Some(tr!("settings-font-desc")), |ui| {
            if let Some(name) = font_picker(ui) {
                theme::set_font_choice(&name);
                // Applied again next frame. Not None: that's the first frame, which adds a texture loader.
                app.chrome_applied = Some(String::new());
            }
        });
        divider(ui);
        settings_zoom(ui);
    });

    let current = crate::preset::get(&app.theme_id).name.clone();
    // Not a card of rows, so it's narrowed here: to the themes that match.
    let shown: Vec<&crate::preset::Preset> = FILTER.with_borrow_mut(|f| {
        f.section = tr!("settings-themes").to_owned();
        crate::preset::all().iter().filter(|p| f.words.is_empty() || words_match(&f.words, &f.section, &format!("{} {} {}", p.name, p.creators.join(" "), p.blurb))).collect()
    });
    if !shown.is_empty() {
        FILTER.with_borrow_mut(|f| f.total += shown.len());
        section_label(ui, tr!("settings-themes"), settings_value(Some(&current)).as_deref());
        if !filtering() {
            ui.label(RichText::new(tr!("settings-themes-note")).font(theme::regular(13.0)).color(theme::text_dim()));
            ui.add_space(12.0);
        }
        themes(app, ui, &shown);
    }
    if was_theme != app.theme_id {
        // The blur is baked into the art when it arrives: the same song needs it again.
        app.art_key = None;
        app.request_art();
    }
}

/// Settings › Appearance › Zoom: the size of everything, as Ctrl + plus / minus sets it.
fn settings_zoom(ui: &mut Ui) {
    const STEPS: [f32; 9] = [0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0];
    let pct = |z: f32| format!("{}%", (z * 100.0).round() as i32);
    let zoom = ui.ctx().zoom_factor();
    let mut pick = None;
    setting_row(ui, tr!("settings-zoom"), Some(tr!("settings-zoom-desc")), |ui| {
        let btn = dropdown_button(ui, &pct(zoom), 120.0, tr!("settings-zoom"));
        widgets::menu_popup(&btn).show(|ui| {
            widgets::menu_body(ui, 120.0);
            for z in STEPS {
                if option_row(ui, &pct(z), (z - zoom).abs() < 0.005).clicked() {
                    pick = Some(z);
                    ui.close();
                }
            }
        });
    });
    if let Some(z) = pick {
        ui.ctx().set_zoom_factor(z);
    }
}

/// Settings › Window.
/// Settings › General › Language: the app's own text. It changes from the next start.
fn settings_language(app: &mut App, ui: &mut Ui) {
    use crate::i18n;
    let system = tr!("settings-language-system", language = i18n::name(i18n::resolve(None)));
    settings_section(ui, tr!("settings-language"), None, None, |ui| {
        setting_row(ui, tr!("settings-language"), Some(tr!("settings-language-desc")), |ui| {
            let current = app.language.as_deref().map_or_else(|| system.clone(), i18n::name);
            let btn = dropdown_button(ui, &current, 210.0, tr!("settings-language"));
            let mut pick: Option<Option<String>> = None;
            widgets::menu_popup(&btn).show(|ui| {
                widgets::menu_body(ui, 260.0);
                dropdown_list(ui, 320.0, |ui| {
                    if option_row(ui, &system, app.language.is_none()).clicked() {
                        pick = Some(None);
                        ui.close();
                    }
                    for lang in i18n::LANGUAGES {
                        if option_row(ui, &i18n::name(lang), app.language.as_deref() == Some(*lang)).clicked() {
                            pick = Some(Some((*lang).to_owned()));
                            ui.close();
                        }
                    }
                });
            });
            if let Some(pick) = pick {
                app.language = pick;
            }
        });
        if i18n::resolve(app.language.as_deref()) != i18n::current() {
            divider(ui);
            let mut restart = false;
            setting_row(ui, tr!("settings-restart-to-apply"), Some(tr!("settings-language-restart-desc")), |ui| {
                restart = pill(ui, tr!("settings-restart-now"), None, true).clicked();
            });
            if restart {
                app.restart(ui.ctx());
            }
        }
    });
}

fn settings_window(app: &mut App, ui: &mut Ui) {
    settings_section(ui, tr!("settings-window"), None, None, |ui| {
        setting_row(ui, tr!("settings-open-on"), Some(tr!("settings-open-on-desc")), |ui| {
            let current = super::START_PAGES.iter().find(|(.., p)| *p == app.start_page).map_or(tr!("settings-home"), |(_, l, _)| l);
            let btn = dropdown_button(ui, current, 200.0, tr!("settings-open-on"));
            widgets::menu_popup(&btn).show(|ui| {
                widgets::menu_body(ui, 200.0);
                for (_, label, page) in super::START_PAGES {
                    // Podcasts is a Library tab only with Settings › Playback › Show podcasts on.
                    if page == Page::Library(LibTab::Podcasts) && !app.show_podcasts && app.start_page != page {
                        continue;
                    }
                    if option_row(ui, label, page == app.start_page).clicked() {
                        app.start_page = page;
                        ui.close();
                    }
                }
            });
        });
        divider(ui);
        // Linux tray icons only open their menu on click.
        let how = if cfg!(target_os = "linux") {
            tr!("settings-close-to-tray-desc-linux")
        } else {
            tr!("settings-close-to-tray-desc")
        };
        setting_row(ui, tr!("settings-close-to-tray"), Some(how), |ui| {
            toggle(ui, &mut app.close_to_tray, tr!("settings-close-to-tray"));
        });
        if let Some(e) = &app.tray_error {
            ui.label(RichText::new(e).font(theme::regular(12.5)).color(theme::RED));
            ui.add_space(8.0);
        }
        if crate::notify::SUPPORTED {
            divider(ui);
            let ok = crate::notify::available();
            let desc = if ok {
                tr!("settings-song-notifications-desc")
            } else {
                tr!("settings-song-notifications-unavailable")
            };
            ui.add_enabled_ui(ok, |ui| {
                setting_row(ui, tr!("settings-song-notifications"), Some(desc), |ui| {
                    toggle(ui, &mut app.song_notifications, tr!("settings-song-notifications"));
                });
            });
        }
        // A Wayland window can only be minimized, and can't bring itself back: from the
        // next start the app opens an X11 window instead while this is on.
        if app.close_to_tray && crate::wayland_window() {
            divider(ui);
            let mut restart = false;
            setting_row(ui, tr!("settings-restart-to-apply"), Some(tr!("settings-close-to-tray-restart-desc")), |ui| {
                restart = pill(ui, tr!("settings-restart-now"), None, true).clicked();
            });
            if restart {
                app.restart(ui.ctx());
            }
        }
        divider(ui);
        if cfg!(target_os = "linux") {
            use super::titlebar::Choice;
            let dir = app.backend.data_dir().to_owned();
            let saved = Choice::load(&dir);
            setting_row(ui, tr!("settings-title-bar"), Some(tr!("settings-title-bar-desc")), |ui| {
                let btn = dropdown_button(ui, saved.label(), 120.0, tr!("settings-title-bar"));
                widgets::menu_popup(&btn).show(|ui| {
                    widgets::menu_body(ui, 120.0);
                    for c in Choice::ALL {
                        if option_row(ui, c.label(), c == saved).clicked() {
                            if let Err(e) = c.save(&dir) {
                                log::warn!("title bar: couldn't save the setting: {e}");
                            }
                            ui.close();
                        }
                    }
                });
            });
            divider(ui);
            if saved.own_here() != super::titlebar::custom() && std::env::var_os("YTM_NATIVE_TITLEBAR").is_none() {
                let mut restart = false;
                setting_row(ui, tr!("settings-restart-to-apply"), Some(tr!("settings-title-bar-restart-desc")), |ui| {
                    restart = pill(ui, tr!("settings-restart-now"), None, true).clicked();
                });
                if restart {
                    app.restart(ui.ctx());
                }
                divider(ui);
            }
        }
        setting_row(ui, tr!("settings-collapse-sidebar"), Some(tr!("settings-collapse-sidebar-desc")), |ui| {
            // What shows: a theme that starts as a rail turns the setting around.
            let mut collapsed = super::shell::is_rail(app);
            if toggle(ui, &mut collapsed, tr!("settings-collapse-sidebar")).changed() {
                app.sidebar_collapsed = !app.sidebar_collapsed;
            }
        });
        divider(ui);
        let api = crate::renderer::api_name();
        let title = tr!("settings-draw-with", api = api);
        let desc = format!(
            "Use {api} (wgpu) instead of OpenGL. It isn't faster for this app and takes about three times the memory; try it if OpenGL draws wrong on this PC. If {api} won't start, the app goes back to OpenGL."
        );
        setting_row(ui, &title, Some(&desc), |ui| {
            if toggle(ui, &mut app.wgpu_renderer, &title).changed() {
                use crate::renderer::Renderer;
                let r = if app.wgpu_renderer { Renderer::Wgpu } else { Renderer::OpenGl };
                if let Err(e) = crate::renderer::save(app.backend.data_dir(), r) {
                    log::warn!("renderer: couldn't save the setting: {e}");
                }
            }
        });
        if app.wgpu_renderer != (crate::renderer::active() == crate::renderer::Renderer::Wgpu) {
            divider(ui);
            let mut restart = false;
            setting_row(ui, tr!("settings-restart-to-apply"), Some(tr!("settings-renderer-restart-desc")), |ui| {
                restart = pill(ui, tr!("settings-restart-now"), None, true).clicked();
            });
            if restart {
                app.restart(ui.ctx());
            }
        }
    });
}

/// Settings › Appearance › Window material: Windows 11's Mica, Mica Alt or Acrylic.
fn settings_mini(app: &mut App, ui: &mut Ui) {
    use super::mini;
    let dir = app.backend.data_dir().to_owned();
    let opengl = crate::renderer::active() == crate::renderer::Renderer::OpenGl;
    let desc = if opengl {
        tr!("settings-pill-mini-player-desc")
    } else {
        tr!("settings-pill-mini-player-desc-needs-opengl")
    };
    settings_section(ui, tr!("settings-mini-player"), None, None, |ui| {
        ui.add_enabled_ui(opengl, |ui| {
            setting_row(ui, tr!("settings-pill-mini-player"), Some(desc), |ui| {
                let mut on = opengl && mini::pill_saved(&dir);
                if toggle(ui, &mut on, tr!("settings-pill-mini-player")).changed()
                    && let Err(e) = mini::save_pill(&dir, on)
                {
                    log::warn!("mini player: couldn't save the style: {e}");
                }
            });
        });
        divider(ui);
        setting_row(ui, tr!("settings-keep-on-top"), Some(tr!("settings-keep-on-top-desc")), |ui| {
            let mut on = app.mini.pinned;
            if toggle(ui, &mut on, tr!("settings-keep-on-top")).changed() {
                app.mini.set_pinned(ui.ctx(), on);
            }
        });
        if opengl && mini::pill_saved(&dir) != mini::pill_active() {
            divider(ui);
            let mut restart = false;
            setting_row(ui, tr!("settings-restart-to-apply"), Some(tr!("settings-pill-mini-player-restart-desc")), |ui| {
                restart = pill(ui, tr!("settings-restart-now"), None, true).clicked();
            });
            if restart {
                app.restart(ui.ctx());
            }
        }
    });
}

fn settings_backdrop(app: &mut App, ui: &mut Ui) {
    use crate::backdrop::{self, Backdrop};
    let saved = backdrop::load(app.backend.data_dir());
    let opengl = crate::renderer::active() == crate::renderer::Renderer::OpenGl;
    let desc = if opengl { tr!("settings-window-material-desc") } else { tr!("settings-window-material-desc-needs-opengl") };
    let mut pick = None;
    ui.add_enabled_ui(opengl, |ui| {
        setting_row(ui, tr!("settings-window-material"), Some(desc), |ui| {
            let btn = dropdown_button(ui, saved.label(), 160.0, tr!("settings-window-material"));
            widgets::menu_popup(&btn).show(|ui| {
                widgets::menu_body(ui, 160.0);
                for b in Backdrop::ALL {
                    if option_row(ui, b.label(), b == saved).clicked() {
                        pick = Some(b);
                        ui.close();
                    }
                }
            });
        });
    });
    if let Some(b) = pick
        && let Err(e) = backdrop::save(app.backend.data_dir(), b)
    {
        log::warn!("backdrop: couldn't save the setting: {e}");
    }
    if opengl && backdrop::load(app.backend.data_dir()) != backdrop::active() {
        divider(ui);
        let mut restart = false;
        setting_row(ui, tr!("settings-restart-to-apply"), Some(tr!("settings-window-material-restart-desc")), |ui| {
            restart = pill(ui, tr!("settings-restart-now"), None, true).clicked();
        });
        if restart {
            app.restart(ui.ctx());
        }
    }
}

/// Settings › App icon.
fn settings_app_icon(app: &mut App, ui: &mut Ui) {
    // Linux desktops take the icon from the app's menu entry.
    let (what, more) = if cfg!(target_os = "linux") {
        (tr!("settings-app-icon-note-linux"), "")
    } else {
        (tr!("settings-app-icon-note"), " A pinned taskbar icon can take a moment to catch up.")
    };
    settings_section(ui, tr!("settings-app-icon"), Some(crate::appicon::get(app.app_icon).name), Some(&format!("{what}.{more}")), |ui| {
        if filter_show(ui, what) {
            ui.add_space(14.0);
            app_icons(app, ui);
            ui.add_space(14.0);
        }
        divider(ui);
        setting_row(ui, tr!("settings-preview"), Some(tr!("settings-preview-desc")), |ui| {
            icon_preview(ui, crate::appicon::get(app.app_icon));
        });
    });
}

/// Settings › Connections, above Discord's sections (`presence::settings`).
fn settings_connections(app: &mut App, ui: &mut Ui) {
    let mut remote_enabled = app.remote_enabled;
    settings_section(ui, tr!("settings-connections"), None, None, |ui| {
        setting_row(ui, tr!("settings-stream-deck"), Some(tr!("settings-stream-deck-desc")), |ui| {
            toggle(ui, &mut remote_enabled, tr!("settings-stream-deck"));
        });
        divider(ui);
        let busy = app.lastfm_busy.load(Ordering::Relaxed);
        let desc = match &app.lastfm_user {
            Some(name) => tr!("settings-lastfm-connected", name = name.as_str()),
            None if busy => tr!("settings-lastfm-allow").to_owned(),
            None => tr!("settings-lastfm-desc").to_owned(),
        };
        let (mut connect, mut disconnect) = (false, false);
        setting_row(ui, "Last.fm", Some(&desc), |ui| {
            if app.lastfm_user.is_some() {
                disconnect = pill(ui, tr!("settings-disconnect"), None, false).clicked();
            } else if busy {
                widgets::loader(ui);
            } else {
                connect = pill(ui, tr!("settings-connect"), None, false).clicked();
            }
        });
        if connect {
            app.lastfm_sign_in();
        }
        if disconnect {
            app.lastfm_sign_out();
        }
        divider(ui);
        let busy = app.listenbrainz_busy.load(Ordering::Relaxed);
        let desc = match &app.listenbrainz_user {
            Some(name) => tr!("settings-listenbrainz-connected", name = name.as_str()),
            None => tr!("settings-listenbrainz-desc").to_owned(),
        };
        let (mut connect, mut disconnect) = (false, false);
        let typed = !app.listenbrainz_token.trim().is_empty();
        setting_row(ui, "ListenBrainz", Some(&desc), |ui| {
            if app.listenbrainz_user.is_some() {
                disconnect = pill(ui, tr!("settings-disconnect"), None, false).clicked();
            } else if busy {
                widgets::loader(ui);
            } else {
                connect = pill(ui, tr!("settings-connect"), None, typed).clicked() && typed;
            }
        });
        // Shown with its row in a search.
        if app.listenbrainz_user.is_none() && filter_hit(&format!("ListenBrainz {desc}")) {
            let resp = super::text_field(ui, tr!("settings-user-token"), &mut app.listenbrainz_token, false, tr!("settings-listenbrainz-token-hint"));
            connect |= typed && !busy && resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.horizontal(|ui| {
                ui.hyperlink_to(RichText::new(tr!("settings-listenbrainz-token-link")).font(theme::regular(12.5)).color(if theme::light() { Color32::from_rgb(0, 102, 204) } else { Color32::from_rgb(62, 166, 255) }), "https://listenbrainz.org/settings/");
            });
            ui.add_space(12.0);
        }
        if connect {
            app.listenbrainz_connect();
        }
        if disconnect {
            app.listenbrainz_sign_out();
        }
    });
    app.remote_enabled = remote_enabled;
}

/// Settings › Updates.
fn settings_updates(app: &mut App, ui: &mut Ui) {
    let state = app.updater.as_ref().map(|u| u.state());
    let installed = super::updatable();
    let (mut check, mut restart, mut channel) = (false, false, None);
    settings_section(ui, tr!("settings-updates"), None, None, |ui| {
        use crate::update::State;
        let status = if !installed {
            tr!("settings-updates-dev-build").to_owned()
        } else {
            match &state {
                Some(State::Checking) => tr!("settings-checking-for-updates").to_owned(),
                Some(State::UpToDate) => tr!("settings-up-to-date", version = crate::update::current_version()),
                Some(State::Downloading { version, percent }) => tr!("settings-update-downloading", version = version.as_str(), percent = *percent),
                Some(State::Ready { version, .. }) => tr!("settings-update-ready", version = version.as_str()),
                Some(State::Available { version, .. }) => tr!("settings-update-available", version = version.as_str(), step = crate::update::package_step()),
                Some(State::Failed(e)) => tr!("settings-update-failed", error = e.to_string()),
                _ => tr!("settings-version", version = crate::update::current_version()),
            }
        };
        let auto_desc = if crate::update::packaged() {
            tr!("settings-automatic-updates-desc-packaged")
        } else {
            tr!("settings-automatic-updates-desc")
        };
        setting_row(ui, tr!("settings-automatic-updates"), Some(auto_desc), |ui| {
            ui.add_enabled_ui(installed, |ui| toggle(ui, &mut app.auto_update, tr!("settings-automatic-updates")));
        });
        divider(ui);
        let desc = "Stable: finished releases, ready for everyday listening.\nNightly: experimental. It can break at any time.";
        setting_row(ui, tr!("settings-update-channel"), Some(desc), |ui| {
            ui.add_enabled_ui(installed, |ui| {
                let btn = dropdown_button(ui, if app.update_nightly { tr!("settings-nightly") } else { tr!("settings-stable") }, 140.0, tr!("settings-update-channel"));
                widgets::menu_popup(&btn).show(|ui| {
                    widgets::menu_body(ui, 140.0);
                    for (label, nightly) in [(tr!("settings-stable"), false), (tr!("settings-nightly"), true)] {
                        if option_row(ui, label, nightly == app.update_nightly).clicked() {
                            channel = Some(nightly);
                            ui.close();
                        }
                    }
                });
            });
        });
        divider(ui);
        setting_row(ui, tr!("settings-status"), Some(&status), |ui| {
            if !installed {
                return;
            }
            match &state {
                Some(State::Ready { .. }) => restart = pill(ui, tr!("settings-restart-to-update"), Some(icon::SPARKLES), true).clicked(),
                Some(State::Available { url, .. }) => {
                    if pill(ui, tr!("settings-download"), Some(icon::SPARKLES), true).clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                    }
                }
                Some(State::Checking | State::Downloading { .. }) => {
                    widgets::loader(ui);
                }
                _ => check = pill(ui, tr!("settings-check-now"), None, false).clicked(),
            }
        });
        // The other kind of Linux install, still there beside this one.
        if let Some((kind, cmd)) = crate::update::other_copy() {
            divider(ui);
            setting_row(ui, tr!("settings-another-copy"), Some(&tr!("settings-another-copy-desc", kind = kind.to_string(), cmd = cmd.to_string())), |_| {});
        }
        // Release notes for the version that's ready, else the one running.
        let (notes_v, ready) = match &state {
            Some(State::Ready { version, .. } | State::Available { version, .. }) => (version.clone(), true),
            _ => (crate::update::current_version(), false),
        };
        if let Some(md) = app.updater.as_ref().and_then(|u| u.notes(&notes_v)).filter(|_| !filtering()) {
            divider(ui);
            let id = egui::Id::unique(("whats-new-open", &notes_v));
            let mut open = ui.data(|d| d.get_temp(id)).unwrap_or(ready);
            ui.add_space(12.0);
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
            let resp = widgets::named(resp, tr!("settings-whats-new")).on_hover_cursor(egui::CursorIcon::PointingHand);
            widgets::icon_at(ui, pos2(rect.left() + 8.0, rect.center().y), if open { icon::CHEVRON_DOWN } else { icon::CHEVRON_RIGHT }, 16.0, theme::text());
            ui.painter().text(pos2(rect.left() + 24.0, rect.center().y), Align2::LEFT_CENTER, tr!("settings-whats-new-in", version = notes_v.to_string()), theme::semibold(14.0), theme::text());
            if resp.clicked() {
                open = !open;
                ui.data_mut(|d| d.insert_temp(id, open));
            }
            if open {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    ui.vertical(|ui| {
                        crate::ui::notes::show(ui, &crate::ui::notes::parse(&md), 13.5);
                        ui.add_space(6.0);
                        crate::ui::notes::github_link(ui);
                    });
                });
            }
            ui.add_space(12.0);
        }
    });
    if let Some(nightly) = channel {
        app.set_update_nightly(nightly);
    }
    if check && let Some(u) = &app.updater {
        u.check();
    }
    if restart {
        app.restart_to_update(ui.ctx());
    }
}

/// Settings › Keyboard shortcuts.
fn settings_shortcuts(ui: &mut Ui) {
    settings_section(ui, tr!("settings-keyboard-shortcuts"), None, None, |ui| {
        let keys: &[(&str, &[&str])] = &[
            (tr!("settings-play-pause"), &[tr!("settings-space")]),
            (tr!("settings-next-previous-song"), &[tr!("settings-ctrl"), "→ / ←"]),
            (tr!("settings-volume-up-down"), &[tr!("settings-ctrl"), "↑ / ↓"]),
            (tr!("settings-shortcut-command-palette"), &[tr!("settings-ctrl"), "K"]),
            (tr!("settings-search"), &[tr!("settings-ctrl"), "F"]),
            (tr!("settings-shortcut-find-in-list"), &[tr!("settings-ctrl"), tr!("settings-shift"), "F"]),
            (tr!("settings-lyrics-and-now-playing"), &["L"]),
            (tr!("settings-shortcut-full-screen"), &["F / F11"]),
            (tr!("settings-shortcut-video-fill"), &["Z"]),
            (tr!("settings-mini-player-on-off"), &[tr!("settings-ctrl"), "M"]),
            (tr!("settings-back-forward"), &[tr!("settings-alt"), "← / →"]),
            (tr!("settings-shortcut-back-forward-mouse"), &[tr!("settings-side-buttons")]),
            (tr!("settings-refresh-the-page"), &[tr!("settings-f5-ctrl-r")]),
            (tr!("settings-zoom-in-out"), &[tr!("settings-ctrl"), "+ / −"]),
            (tr!("settings-shortcut-zoom-reset"), &[tr!("settings-ctrl"), "0"]),
            (tr!("settings-shortcut-leave"), &[tr!("settings-esc")]),
        ];
        for (i, (what, combo)) in keys.iter().enumerate() {
            if !filter_show(ui, &format!("{what} {}", combo.join(" "))) {
                continue;
            }
            if i > 0 {
                divider(ui);
            }
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::hover());
            ui.painter().text(pos2(rect.left(), rect.center().y), Align2::LEFT_CENTER, *what, theme::regular(14.0), theme::text());
            let mut x = rect.right();
            for (j, k) in combo.iter().enumerate().rev() {
                x = keycap(ui, pos2(x, rect.center().y), k) - 6.0;
                if j > 0 {
                    let plus = ui.painter().text(pos2(x, rect.center().y), Align2::RIGHT_CENTER, "+", theme::regular(13.0), theme::text_dim());
                    x = plus.left() - 6.0;
                }
            }
        }
    });
}

/// Settings › About.
fn settings_about(ui: &mut Ui) {
    settings_section(ui, tr!("settings-about"), None, None, |ui| {
        setting_row(ui, &format!("ytm-rs {}", crate::update::build_version()), Some(tr!("settings-about-tagline")), |_| {});
        for (title, desc) in [
            (tr!("settings-lyrics"), tr!("settings-about-lyrics-sources")),
            (tr!("settings-themes"), tr!("settings-about-themes-credit")),
            (tr!("settings-background"), tr!("settings-about-background-credit")),
            (tr!("settings-fonts-and-icons"), tr!("settings-about-fonts-credit")),
        ] {
            divider(ui);
            setting_row(ui, title, Some(desc), |_| {});
        }
    });
}

/// The last thing on the page: the app icon, its name and its download count.
fn settings_sign_off(app: &mut App, ui: &mut Ui) {
    let downloads = app.updater.as_ref().and_then(|u| u.downloads());
    let text = match downloads {
        // The number for the language to pick its plural by; shown with its separators.
        Some(n) => tr!("settings-downloads-and-counting", count = n, shown = thousands(n)),
        None => tr!("settings-counting-downloads").to_owned(),
    };
    ui.add_space(40.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 96.0), Sense::hover());
    icon_at(ui, crate::appicon::get(app.app_icon), Rect::from_center_size(pos2(rect.center().x, rect.top() + 22.0), vec2(44.0, 44.0)));
    ui.painter().text(pos2(rect.center().x, rect.top() + 58.0), Align2::CENTER_CENTER, "ytm-rs", theme::bold(17.0), theme::text());
    ui.painter().text(pos2(rect.center().x, rect.top() + 82.0), Align2::CENTER_CENTER, text, theme::regular(13.0), theme::text_dim());
}

/// Settings › Network: the proxy everything goes through, a connection test, and a
/// restart when the setting changed.
fn network(app: &mut App, ui: &mut Ui) {
    use crate::proxy::{self, Setting};
    let same = |a: &Setting, b: &Setting| std::mem::discriminant(a) == std::mem::discriminant(b);
    let mut pick = None;
    setting_row(ui, tr!("settings-proxy"), Some(tr!("settings-proxy-desc")), |ui| {
        let btn = dropdown_button(ui, app.proxy_mode.label(), 220.0, tr!("settings-proxy"));
        widgets::menu_popup(&btn).show(|ui| {
            widgets::menu_body(ui, 220.0);
            for s in [Setting::System, Setting::Off, Setting::Custom(String::new())] {
                if option_row(ui, s.label(), same(&s, &app.proxy_mode)).clicked() {
                    pick = Some(s);
                    ui.close();
                }
            }
        });
    });
    if let Some(s) = pick {
        app.proxy_error = None;
        *app.proxy_test.lock().unwrap() = None;
        app.proxy_mode = s.clone();
        // System and Off take effect as picked; a custom address once it's saved.
        if !matches!(s, Setting::Custom(_)) {
            match proxy::save(app.backend.data_dir(), &s) {
                Ok(()) => app.proxy_saved = s,
                Err(e) => app.proxy_error = Some(format!("{e:#}")),
            }
        }
    }
    match &app.proxy_mode {
        Setting::System => {
            divider(ui);
            // Read from the registry: every couple of seconds, not every frame.
            let now = ui.input(|i| i.time);
            let id = egui::Id::unique("windows-proxy-desc");
            let cached = ui.data(|d| d.get_temp::<(f64, String)>(id)).filter(|(at, _)| now - at < 2.0);
            let desc = match cached {
                Some((_, desc)) => desc,
                None => {
                    let desc = match (proxy::system(), cfg!(windows)) {
                        (Some(p), true) => tr!("settings-proxy-windows-uses", proxy = proxy::redact(&p)),
                        (None, true) => tr!("settings-proxy-windows-none").to_owned(),
                        (Some(p), false) => tr!("settings-proxy-environment-sets", proxy = proxy::redact(&p)),
                        (None, false) => tr!("settings-proxy-environment-none").to_owned(),
                    };
                    ui.data_mut(|d| d.insert_temp(id, (now, desc.clone())));
                    desc
                }
            };
            setting_row(ui, if cfg!(windows) { tr!("settings-windows-proxy") } else { tr!("settings-system-proxy") }, Some(&desc), |_| {});
        }
        Setting::Custom(_) if !filter_hit(tr!("settings-proxy-address")) => {}
        Setting::Custom(_) => {
            divider(ui);
            ui.add_space(12.0);
            super::text_field(ui, tr!("settings-address"), &mut app.proxy_url, false, "http://127.0.0.1:8080 or socks5://user:pass@host:1080");
            ui.add_space(10.0);
            let custom = Setting::Custom(app.proxy_url.trim().to_owned());
            let saved = app.proxy_saved == custom;
            let mut save = false;
            ui.horizontal(|ui| {
                save = pill(ui, if saved { tr!("settings-saved") } else { tr!("settings-save") }, None, !saved).clicked() && !saved;
                ui.add_space(8.0);
                ui.label(RichText::new(tr!("settings-proxy-password-note")).font(theme::regular(12.5)).color(theme::text_dim()));
            });
            if save {
                match proxy::save(app.backend.data_dir(), &custom) {
                    Ok(()) => {
                        app.proxy_saved = custom;
                        app.proxy_error = None;
                    }
                    Err(e) => app.proxy_error = Some(format!("{e:#}")),
                }
            }
            ui.add_space(12.0);
        }
        Setting::Off => {}
    }
    if let Some(e) = &app.proxy_error {
        ui.label(RichText::new(e).font(theme::regular(12.5)).color(theme::RED));
        ui.add_space(8.0);
    }
    divider(ui);
    let result = app.proxy_test.lock().unwrap().clone();
    let desc = match &result {
        Some(Some((_, msg))) => msg.clone(),
        Some(None) => tr!("settings-checking").to_owned(),
        None => tr!("settings-proxy-test-desc").to_owned(),
    };
    setting_row(ui, tr!("settings-test-connection"), Some(&desc), |ui| {
        if matches!(result, Some(None)) {
            widgets::loader(ui);
        } else if pill(ui, tr!("settings-test"), None, false).clicked() {
            let s = match &app.proxy_mode {
                Setting::Custom(_) => Setting::Custom(app.proxy_url.trim().to_owned()),
                other => other.clone(),
            };
            let (slot, b) = (app.proxy_test.clone(), app.backend.clone());
            *slot.lock().unwrap() = Some(None);
            app.backend.spawn(async move {
                let out = match proxy::test(&s).await {
                    Ok(t) => (true, tr!("settings-test-working", ms = t.as_millis() as u64)),
                    Err(e) => (false, tr!("settings-test-failed", error = format!("{e:#}"))),
                };
                *slot.lock().unwrap() = Some(Some(out));
                b.wake();
            });
        }
    });
    if app.proxy_saved != *proxy::active() {
        divider(ui);
        let mut restart = false;
        setting_row(ui, tr!("settings-restart-to-apply"), Some(tr!("settings-proxy-restart-desc")), |ui| {
            restart = pill(ui, tr!("settings-restart-now"), None, true).clicked();
        });
        if restart {
            app.restart(ui.ctx());
        }
    }
}

/// The icon picker: a tile per icon in even rows, five across (two when that's too tight).
fn app_icons(app: &mut App, ui: &mut Ui) {
    let gap = 12.0;
    let avail = ui.available_width();
    let cols = if avail + gap >= 5.0 * (92.0 + gap) { 5.0 } else { 2.0 };
    let w = ((avail + gap) / cols - gap).floor();
    let mut picked = None;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = vec2(gap, gap);
        ui.horizontal_wrapped(|ui| {
            for v in crate::appicon::ALL {
                if app_icon_tile(ui, v, w, app.app_icon == v.id, app.accent_color()).clicked() {
                    picked = Some(v.id);
                }
            }
        });
    });
    if let Some(id) = picked {
        app.set_app_icon(ui.ctx(), id);
    }
}

fn app_icon_tile(ui: &mut Ui, v: &crate::appicon::Variant, w: f32, selected: bool, accent: Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 112.0), Sense::click());
    let resp = named(resp, &tr!("settings-app-icon-named", name = v.name)).on_hover_cursor(egui::CursorIcon::PointingHand);
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
    let s = theme::anim_bool(ui.ctx(), resp.id.with("s"), selected, 0.2);
    let painter = ui.painter();
    painter.rect_filled(rect, 12.0, theme::glass(0.03 + 0.04 * h).lerp_to_gamma(theme::with_alpha(accent, 0.14), s));
    let ring = theme::glass(0.06).lerp_to_gamma(accent, s);
    painter.rect_stroke(rect, 12.0, egui::Stroke::new(2.0, ring), egui::StrokeKind::Inside);
    icon_at(ui, v, Rect::from_center_size(pos2(rect.center().x, rect.top() + 46.0), Vec2::splat(56.0)));
    // Narrow tiles get a smaller label so "YouTube Music" still fits.
    let font = theme::semibold(if w >= 108.0 { 13.0 } else { 12.0 });
    let label = widgets::galley(ui, v.name, font.clone(), theme::text(), w - 8.0);
    widgets::text_at(ui, pos2(rect.center().x - label.size().x / 2.0, rect.top() + 84.0), v.name, font, theme::text(), w - 8.0);
    resp
}

/// The chosen icon on a dark taskbar, as a running app (24 px) and in the tray (16 px).
fn icon_preview(ui: &mut Ui, v: &crate::appicon::Variant) {
    let (rect, _) = ui.allocate_exact_size(vec2(236.0, 48.0), Sense::hover());
    let rect = Rect::from_min_size(pos2(ui.max_rect().right() - 236.0, rect.top()), rect.size());
    let painter = ui.painter();
    painter.rect(rect, 10.0, Color32::from_rgb(28, 28, 28), egui::Stroke::new(1.0, Color32::from_white_alpha(18)), egui::StrokeKind::Inside);
    let mut x = rect.left() + 12.0;
    for c in [Color32::from_rgb(58, 123, 213), Color32::from_rgb(242, 179, 61)] {
        painter.rect_filled(Rect::from_center_size(pos2(x + 18.0, rect.center().y), Vec2::splat(20.0)), 4.0, c);
        x += 42.0;
    }
    let app = Rect::from_center_size(pos2(x + 20.0, rect.center().y), Vec2::splat(40.0));
    painter.rect_filled(app, 6.0, Color32::from_white_alpha(18));
    icon_at(ui, v, Rect::from_center_size(app.center() - vec2(0.0, 3.0), Vec2::splat(24.0)));
    painter.rect_filled(Rect::from_center_size(pos2(app.center().x, app.bottom() - 5.0), vec2(14.0, 3.0)), 2.0, Color32::from_rgb(108, 180, 255));
    x = app.right() + 10.0;
    painter.vline(x, Rect::from_center_size(rect.center(), vec2(1.0, 24.0)).y_range(), egui::Stroke::new(1.0, Color32::from_white_alpha(30)));
    icon_at(ui, v, Rect::from_center_size(pos2(x + 20.0, rect.center().y), Vec2::splat(16.0)));
    painter.text(pos2(x + 38.0, rect.center().y), Align2::LEFT_CENTER, "9:41 PM", theme::regular(12.0), Color32::from_gray(230));
}

/// Paint `v` into `rect`, from the ICO entry that fits its size in physical pixels.
fn icon_at(ui: &Ui, v: &crate::appicon::Variant, rect: Rect) {
    let px = (rect.width() * ui.ctx().pixels_per_point()).ceil() as u32;
    let png = v.png(px);
    egui::Image::from_bytes(format!("bytes://app-icon/{}-{}.png", v.id, png.len()), png).paint_at(ui, rect);
}

/// The font setting: the theme's own, a bundled face, or one installed on this PC.
/// Returns the new choice ("" for the theme's).
fn font_picker(ui: &mut Ui) -> Option<String> {
    let current = theme::font_choice();
    let btn = dropdown_button(ui, if current.is_empty() { tr!("settings-themes-font") } else { &current }, 220.0, tr!("settings-font"));
    let just_opened = btn.clicked();
    let mut picked = None;
    widgets::menu_popup(&btn).show(|ui| {
        widgets::menu_body(ui, 220.0);
        dropdown_list(ui, 360.0, |ui| {
            let mut row = |ui: &mut Ui, label: &str, value: &str| {
                let on = current.eq_ignore_ascii_case(value);
                let r = option_row(ui, label, on);
                if on && just_opened {
                    r.scroll_to_me(Some(egui::Align::Center));
                }
                if r.clicked() {
                    picked = Some(value.to_owned());
                    ui.close();
                }
            };
            row(ui, tr!("settings-themes-font"), "");
            for name in theme::BUNDLED_FONTS {
                row(ui, name, name);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(RichText::new(tr!("settings-installed-on-this-pc")).font(theme::semibold(12.0)).color(theme::text_faint()));
            });
            ui.add_space(4.0);
            let ctx = ui.ctx().clone();
            match theme::installed_fonts(move || ctx.request_repaint()) {
                Some(fonts) => {
                    for f in fonts {
                        row(ui, &f.name, &f.name);
                    }
                }
                None => {
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        ui.label(RichText::new(tr!("settings-looking-for-fonts")).font(theme::regular(13.0)).color(theme::text_dim()));
                    });
                }
            }
        });
    });
    picked.filter(|p| *p != current)
}

fn system_default() -> &'static str {
    tr!("settings-windows-default")
}

fn output_device_picker(app: &mut App, ui: &mut Ui) {
    let current = match &app.output_device {
        None => system_default().to_owned(),
        Some((id, name)) => match app.output_devices.iter().find(|d| d.id == *id) {
            Some(d) => d.name.clone(),
            None if app.output_devices.is_empty() => name.clone(),
            None => tr!("settings-not-connected", name = name.as_str()),
        },
    };
    let btn = dropdown_button(ui, &current, 210.0, tr!("settings-output-device"));
    // Listed when opened, so a device that just connected shows up.
    if btn.clicked() || app.output_devices.is_empty() {
        app.output_devices = crate::audio::output_devices();
    }
    let mut pick: Option<Option<(String, String)>> = None;
    widgets::menu_popup(&btn).show(|ui| {
        widgets::menu_body(ui, 340.0);
        dropdown_list(ui, 320.0, |ui| {
            if option_row(ui, system_default(), app.output_device.is_none()).clicked() {
                pick = Some(None);
                ui.close();
            }
            for d in &app.output_devices {
                if option_row(ui, &d.name, app.output_device.as_ref().is_some_and(|(id, _)| *id == d.id)).clicked() {
                    pick = Some(Some((d.id.clone(), d.name.clone())));
                    ui.close();
                }
            }
        });
    });
    if let Some(pick) = pick {
        app.audio.send(crate::audio::AudioCmd::SetDevice(pick.as_ref().map(|(id, _)| id.clone())));
        app.output_device = pick;
    }
}

/// Settings › Playback › Equalizer.
fn settings_equalizer(app: &mut App, ui: &mut Ui) {
    use crate::audio::eq;
    settings_section(ui, tr!("settings-equalizer"), None, None, |ui| {
        setting_row(ui, tr!("settings-equalizer"), Some(tr!("settings-equalizer-desc")), |ui| {
            if toggle(ui, &mut app.eq_on, tr!("settings-equalizer")).changed() {
                app.set_eq();
            }
        });
        ui.add_enabled_ui(app.eq_on, |ui| {
            divider(ui);
            setting_row(ui, tr!("settings-preset"), None, |ui| {
                let btn = dropdown_button(ui, eq::preset_name(&app.eq_gains).unwrap_or(tr!("settings-custom")), 180.0, tr!("settings-equalizer-preset"));
                let mut pick = None;
                widgets::menu_popup(&btn).show(|ui| {
                    widgets::menu_body(ui, 200.0);
                    dropdown_list(ui, 380.0, |ui| {
                        for (name, gains) in &eq::PRESETS {
                            if option_row(ui, name, app.eq_gains == *gains).clicked() {
                                pick = Some(*gains);
                                ui.close();
                            }
                        }
                    });
                });
                if let Some(g) = pick {
                    app.eq_gains = g;
                    app.set_eq();
                }
            });
            if filter_show(ui, tr!("settings-equalizer-search-words")) {
                divider(ui);
                if eq_bands(ui, &mut app.eq_gains) {
                    app.set_eq();
                }
            }
            divider(ui);
            setting_row(ui, tr!("settings-prevent-clipping"), Some(tr!("settings-prevent-clipping-desc")), |ui| {
                if toggle(ui, &mut app.eq_no_clip, tr!("settings-prevent-clipping")).changed() {
                    app.set_eq();
                }
            });
        });
    });
}

fn settings_tempo(app: &mut App, ui: &mut Ui) {
    let jam = app.jam.active();
    settings_section(ui, tr!("settings-playback-speed"), None, jam.then(tempo_in_jam), |ui| {
        setting_row(ui, tr!("settings-playback-speed"), Some(tr!("settings-playback-speed-desc")), |ui| {
            toggle(ui, &mut app.tempo_on, tr!("settings-playback-speed"));
        });
        ui.add_enabled_ui(app.tempo_on && !jam, |ui| {
            use crate::audio::tempo::{MAX_SPEED, MIN_SPEED};
            divider(ui);
            setting_row_wide(ui, tr!("settings-speed"), None, SLIDER_W, |ui| {
                step_slider(ui, &mut app.speed, (MIN_SPEED, MAX_SPEED, 0.05), Some(1.0), times, tr!("settings-speed"));
            });
        });
    });
}

pub(super) fn tempo_in_jam() -> &'static str {
    tr!("settings-playback-speed-jam-note")
}

/// A speed as it's shown: 1×, 1.5×, 1.25×.
pub(super) fn times(v: f32) -> String {
    format!("{}×", (v * 100.0).round() / 100.0)
}

/// The equalizer's bands as vertical sliders, over the curve they make together.
/// Drag or click to set one, or double-click for 0 dB.
/// True if one changed.
fn eq_bands(ui: &mut Ui, gains: &mut crate::audio::eq::Gains) -> bool {
    use crate::audio::eq::{BANDS, MAX_DB, response};
    const LABELS: [&str; 10] = ["31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k"];
    const NAMES: [&str; 10] = ["31 Hz", "62 Hz", "125 Hz", "250 Hz", "500 Hz", "1 kHz", "2 kHz", "4 kHz", "8 kHz", "16 kHz"];
    const GUTTER: f32 = 34.0;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 210.0), Sense::hover());
    let fade = if ui.is_enabled() { 1.0 } else { 0.45 };
    let accent = ui.ctx().data(|d| d.get_temp::<Color32>(egui::Id::unique("accent"))).unwrap_or(theme::RED);
    let p = ui.painter();
    let top = rect.top() + 26.0;
    let bottom = rect.bottom() - 30.0;
    let mid = (top + bottom) / 2.0;
    let y_of = |db: f32| mid - db / MAX_DB * (bottom - top) / 2.0;
    let col_w = (rect.width() - GUTTER) / BANDS.len() as f32;
    let x_of = |i: f32| rect.left() + GUTTER + col_w * (i + 0.5);
    for (db, text) in [(MAX_DB, "+12"), (0.0, "0"), (-MAX_DB, "−12")] {
        p.text(pos2(rect.left(), y_of(db)), Align2::LEFT_CENTER, text, theme::regular(11.5), theme::with_alpha(theme::text_dim(), fade));
    }
    p.line_segment([pos2(x_of(-0.5), mid), pos2(x_of(9.5), mid)], egui::Stroke::new(1.0, theme::glass(0.10 * fade)));
    // Bands are an octave apart, so evenly spaced columns sit on a log frequency axis.
    // The curve can go a little past ±12 between bands; it runs off the top rather than
    // flattening there and hiding it. It's drawn from the first band to the last: past
    // them it only shows the filters' tails, which the sliders don't set.
    let db_at = response(gains);
    let curve: Vec<egui::Pos2> = (0..=180)
        .map(|k| {
            let i = 9.0 * k as f32 / 180.0;
            pos2(x_of(i), y_of(db_at(BANDS[0] * 2f32.powf(i))))
        })
        .collect();
    ui.painter_at(rect).add(egui::Shape::line(curve, egui::Stroke::new(2.0, theme::with_alpha(accent, 0.55 * fade))));
    let mut changed = false;
    for (i, g) in gains.iter_mut().enumerate() {
        let x = x_of(i as f32);
        // Just the track and a knob's reach past its ends, so clicking a label doesn't
        // send the band to the bottom.
        let col = Rect::from_min_max(pos2(x - col_w / 2.0, top - 8.0), pos2(x + col_w / 2.0, bottom + 8.0));
        let mut resp = ui.interact(col, ui.scope_id().with(("eq-band", i)), Sense::click_and_drag());
        let name = NAMES[i];
        let old = *g;
        if resp.double_clicked() {
            *g = 0.0;
        } else if let Some(pos) = resp.interact_pointer_pos()
            && (resp.dragged() || resp.clicked())
        {
            *g = (((mid - pos.y) / ((bottom - top) / 2.0) * MAX_DB * 2.0).round() / 2.0).clamp(-MAX_DB, MAX_DB);
        }
        if *g != old {
            changed = true;
            resp.mark_changed();
        }
        let value = *g;
        resp.widget_info(|| egui::WidgetInfo::slider(ui.is_enabled(), value as f64, name));
        let resp = resp.on_hover_cursor(egui::CursorIcon::ResizeVertical);
        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() || resp.dragged() || resp.has_focus(), 0.12);
        let track = Rect::from_center_size(pos2(x, mid), vec2(4.0, bottom - top));
        p.rect_filled(track, 2.0, theme::glass(0.15 * fade));
        let knob = pos2(x, y_of(value));
        p.rect_filled(Rect::from_two_pos(pos2(x - 2.0, mid), pos2(x + 2.0, knob.y)), 2.0, theme::with_alpha(accent, fade));
        p.circle_filled(knob + vec2(0.0, 1.0), 7.0 + h, theme::shade(0.25 * fade));
        p.circle_filled(knob, 7.0 + h, theme::with_alpha(Color32::WHITE, fade));
        if h > 0.0 {
            let text = match value {
                0.0 => tr!("settings-0-db").to_owned(),
                v if v > 0.0 => format!("+{v} dB"),
                v => format!("−{} dB", -v),
            };
            p.text(pos2(x, rect.top() + 9.0), Align2::CENTER_CENTER, text, theme::semibold(12.0), theme::with_alpha(theme::text(), h * fade));
        }
        p.text(pos2(x, rect.bottom() - 12.0), Align2::CENTER_CENTER, LABELS[i], theme::regular(12.0), theme::with_alpha(theme::text_dim(), fade));
    }
    changed
}

/// Volume normalization's description: what the levels do, and what it does to the song playing.
fn normalization_desc(app: &App) -> String {
    use crate::audio::Normalize;
    let what = match app.normalize {
        Normalize::Off => return tr!("settings-normalization-off-desc").to_owned(),
        Normalize::Normal => tr!("settings-normalization-normal-desc"),
        Normalize::Quiet => tr!("settings-normalization-quiet-desc"),
    };
    match (app.current().is_some(), super::shell::normalization_note(app)) {
        (true, Some(note)) => format!("{what} This song: {note}."),
        _ => what.to_owned(),
    }
}

/// Settings › Sound: crossfeed, mono and balance.
fn stereo_settings(app: &mut App, ui: &mut Ui) {
    use crate::audio::stereo::Crossfeed;
    let before = app.stereo;
    // Nothing for it to do on mono: it's off (and shown off) until then.
    ui.add_enabled_ui(!app.stereo.mono, |ui| {
        setting_row(ui, tr!("settings-headphone-crossfeed"), Some(tr!("settings-headphone-crossfeed-desc")), |ui| {
            let shown = if app.stereo.mono { Crossfeed::Off } else { app.stereo.crossfeed };
            let btn = dropdown_button(ui, shown.label(), 120.0, tr!("settings-headphone-crossfeed"));
            widgets::menu_popup(&btn).show(|ui| {
                widgets::menu_body(ui, 120.0);
                for c in Crossfeed::ALL {
                    if option_row(ui, c.label(), c == app.stereo.crossfeed).clicked() {
                        app.stereo.crossfeed = c;
                        ui.close();
                    }
                }
            });
        });
    });
    divider(ui);
    setting_row(ui, tr!("settings-mono-audio"), Some(tr!("settings-mono-audio-desc")), |ui| {
        toggle(ui, &mut app.stereo.mono, tr!("settings-mono-audio"));
    });
    divider(ui);
    setting_row_wide(ui, tr!("settings-balance"), Some(tr!("settings-balance-desc")), SLIDER_W, |ui| {
        let mut b = app.stereo.balance as f32;
        let side = |v: f32| match v {
            0.0 => tr!("settings-centre").to_owned(),
            v if v < 0.0 => format!("L {}", -v),
            v => format!("R {v}"),
        };
        if step_slider(ui, &mut b, (-100.0, 100.0, 5.0), Some(0.0), side, tr!("settings-balance")).changed() {
            app.stereo.balance = b as i8;
        }
    });
    if app.stereo != before {
        app.audio.set_stereo(app.stereo);
    }
}

/// A dropdown's closed state: the chosen value and a chevron on a glass pill.
pub(super) fn dropdown_button(ui: &mut Ui, value: &str, width: f32, name: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 36.0), Sense::click());
    let resp = widgets::named(resp, name).on_hover_cursor(egui::CursorIcon::PointingHand);
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&resp));
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() || open, 0.12);
    ui.painter().rect_filled(rect, 10.0, theme::glass(0.08 + 0.05 * h));
    widgets::text_at(ui, pos2(rect.left() + 14.0, rect.center().y - 9.0), value, theme::regular(14.0), theme::text(), rect.width() - 44.0);
    let glyph = if open { theme::icon::CHEVRON_UP } else { theme::icon::CHEVRON_DOWN };
    widgets::icon_at(ui, pos2(rect.right() - 18.0, rect.center().y), glyph, 15.0, theme::lerp_color(theme::text_dim(), theme::text(), h));
    resp
}

/// A dropdown list that scrolls past `max_height`. While it does, rows stop short of a
/// thin scrollbar instead of running under it.
fn dropdown_list(ui: &mut Ui, max_height: f32, add: impl FnOnce(&mut Ui)) {
    let key = ui.scope_id().with("overflows");
    let overflows = ui.data(|d| d.get_temp::<bool>(key)).unwrap_or(false);
    let scroll = &mut ui.spacing_mut().scroll;
    scroll.bar_width = 6.0;
    let gutter = scroll.bar_width + scroll.bar_outer_margin + 2.0;
    let out = egui::ScrollArea::vertical().max_height(max_height).auto_shrink([false, true]).show(ui, |ui| {
        if overflows {
            ui.set_max_width(ui.available_width() - gutter);
        }
        add(ui);
    });
    ui.data_mut(|d| d.insert_temp(key, out.content_size.y > out.inner_rect.height() + 0.5));
}

/// A dropdown option: hover glass, and a check on the chosen one.
pub(super) fn option_row(ui: &mut Ui, label: &str, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
    let resp = widgets::named(resp, label).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
    if h > 0.0 || selected {
        ui.painter().rect_filled(rect, theme::menu_item_radius(), theme::glass(0.05 * selected as u8 as f32 + 0.08 * h));
    }
    let font = if selected { theme::semibold(14.0) } else { theme::regular(14.0) };
    widgets::text_at(ui, pos2(rect.left() + 12.0, rect.center().y - 9.0), label, font, theme::text(), rect.width() - 44.0);
    if selected {
        widgets::icon_at(ui, pos2(rect.right() - 16.0, rect.center().y), theme::icon::CHECK, 15.0, theme::text());
    }
    resp
}

/// 12345 -> "12,345".
pub(super) fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Small heading above a settings card, with an optional value on the right.
pub(super) fn section_label(ui: &mut Ui, title: &str, value: Option<&str>) {
    ui.add_space(30.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    section_heading(ui, rect, title, value);
    ui.add_space(10.0);
}

fn section_heading(ui: &Ui, rect: Rect, title: &str, value: Option<&str>) {
    ui.painter().text(pos2(rect.left() + 2.0, rect.center().y), Align2::LEFT_CENTER, title, theme::bold(17.0), theme::text());
    if let Some(v) = value {
        ui.painter().text(pos2(rect.right() - 2.0, rect.center().y), Align2::RIGHT_CENTER, v, theme::semibold(13.0), theme::text_dim());
    }
}

/// The value beside a section's heading: in a search, the category it's in.
fn settings_value(value: Option<&str>) -> Option<String> {
    FILTER.with_borrow(|f| if f.words.is_empty() { value.map(str::to_owned) } else { Some(f.tab.to_owned()) })
}

fn settings_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(theme::glass_panel(0.06))
        .stroke(egui::Stroke::new(1.0, theme::glass_panel(0.05)))
        .corner_radius(theme::panel_radius())
        .inner_margin(egui::Margin::symmetric(20, 6))
}

pub(super) fn settings_card(ui: &mut Ui, body: impl FnOnce(&mut Ui)) {
    settings_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        body(ui);
    });
}

/// A section of Settings: its heading, a line about it, and a card of rows. In a search
/// it shows only the rows that match, and nothing at all if none do.
pub(super) fn settings_section(ui: &mut Ui, title: &str, value: Option<&str>, note: Option<&str>, body: impl FnOnce(&mut Ui)) {
    if !filtering() {
        section_label(ui, title, value);
        if let Some(n) = note {
            ui.label(RichText::new(n).font(theme::regular(13.0)).color(theme::text_dim()));
            ui.add_space(12.0);
        }
        settings_card(ui, body);
        return;
    }
    FILTER.with_borrow_mut(|f| {
        f.section = title.to_owned();
        f.in_section = 0;
    });
    // In a child, for whether it had anything to show to decide if it takes up room.
    const HEADING: f32 = 60.0;
    let area = ui.available_rect_before_wrap();
    let mut child = ui.new_child(egui::UiBuilder::new().id_salt(("settings-section", title)).max_rect(area).layout(egui::Layout::top_down(egui::Align::Min)));
    child.add_space(HEADING);
    let mut card = settings_frame().begin(&mut child);
    let w = card.content_ui.available_width();
    card.content_ui.set_width(w);
    body(&mut card.content_ui);
    if FILTER.with_borrow(|f| f.in_section > 0) {
        card.end(&mut child);
        let heading = Rect::from_min_size(pos2(area.left(), area.top() + 30.0), vec2(area.width(), 20.0));
        section_heading(ui, heading, title, settings_value(value).as_deref().filter(|v| *v != title));
        ui.advance_cursor_after_rect(child.min_rect());
    }
}

/// A settings row: title and description on the left, the control on the right. A search
/// that doesn't find it leaves it out.
pub(super) fn setting_row(ui: &mut Ui, title: &str, desc: Option<&str>, control: impl FnOnce(&mut Ui)) {
    setting_row_wide(ui, title, desc, 210.0, control);
}

/// A settings row whose controls take `control_w` rather than one control's 210.
pub(super) fn setting_row_wide(ui: &mut Ui, title: &str, desc: Option<&str>, control_w: f32, control: impl FnOnce(&mut Ui)) {
    if !filter_show(ui, &format!("{title} {}", desc.unwrap_or_default())) {
        return;
    }
    let w = ui.available_width();
    let text_w = (w - control_w - 20.0).max(200.0);
    let title_g = widgets::galley(ui, title, theme::semibold(14.5), theme::text(), text_w);
    // Up to three lines, and as many again for each line the description starts itself.
    let desc_g = desc.map(|d| widgets::galley_wrapped(ui, d, theme::regular(12.5), theme::text_dim(), text_w, 3 * (1 + d.matches('\n').count())));
    let text_h = title_g.size().y + desc_g.as_ref().map_or(0.0, |g| g.size().y + 3.0);
    let h = (text_h + 26.0).max(58.0);
    let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    let fade = if ui.is_enabled() { 1.0 } else { 0.45 };
    let top = rect.center().y - text_h / 2.0;
    ui.painter().galley(pos2(rect.left(), top), title_g.clone(), theme::with_alpha(theme::text(), fade));
    if let Some(g) = desc_g {
        ui.painter().galley(pos2(rect.left(), top + title_g.size().y + 3.0), g, theme::with_alpha(theme::text_dim(), fade));
    }
    let ctl = Rect::from_min_max(pos2(rect.left() + text_w + 20.0, rect.top()), rect.max);
    let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(ctl).layout(egui::Layout::right_to_left(egui::Align::Center)));
    control(&mut cui);
}

pub(super) fn divider(ui: &mut Ui) {
    // A search has rows left out: each one it shows draws its own.
    if !filtering() {
        divider_line(ui);
    }
}

fn divider_line(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::glass_panel(0.07));
}

/// On/off switch tinted with the accent.
pub(super) fn toggle(ui: &mut Ui, on: &mut bool, name: &str) -> egui::Response {
    let tl = crate::preset::active().app.toggle_look;
    let [tw, th] = tl.size.unwrap_or([44.0, 26.0]);
    let (rect, mut resp) = ui.allocate_exact_size(vec2(tw, th), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let value = *on;
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, ui.is_enabled(), value, name));
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let t = theme::anim_bool(ui.ctx(), resp.id.with("t"), value, 0.16);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
    let accent = ui.ctx().data(|d| d.get_temp::<Color32>(egui::Id::unique("accent"))).unwrap_or(theme::RED);
    let fade = if ui.is_enabled() { 1.0 } else { 0.4 };
    let off = theme::glass(0.16 + 0.06 * h);
    let off = if tl.outline { Color32::TRANSPARENT } else { off };
    ui.painter().rect_filled(rect, th / 2.0, theme::with_alpha(theme::lerp_color(off, accent, t), fade));
    if tl.outline && t < 1.0 {
        let line = match tl.off_outline {
            Some([px, a]) => egui::Stroke::new(px, theme::with_alpha(if theme::light() { Color32::BLACK } else { Color32::WHITE }, a * (1.0 - t) * fade)),
            None => egui::Stroke::new(1.5, theme::with_alpha(theme::text_dim(), (1.0 - t) * fade)),
        };
        ui.painter().rect_stroke(rect, th / 2.0, line, egui::StrokeKind::Inside);
    }
    if tl.on_outline && t > 0.0 {
        ui.painter().rect_filled(rect, th / 2.0, theme::with_alpha(theme::shade(1.0), t * fade));
        ui.painter().rect_stroke(rect, th / 2.0, egui::Stroke::new(2.0, theme::with_alpha(Color32::WHITE, t * fade)), egui::StrokeKind::Inside);
    }
    let knob_r = tl.knob.unwrap_or(20.0) / 2.0;
    let knob = pos2(egui::lerp(rect.left() + th / 2.0..=rect.right() - th / 2.0, t), rect.center().y);
    ui.painter().circle_filled(knob + vec2(0.0, 1.0), knob_r, theme::shade(0.25 * fade));
    // On a pale accent a white knob would vanish into its track.
    let pale = accent.r() as u32 + accent.g() as u32 + accent.b() as u32 > 620 && !tl.on_outline;
    let on_knob = if pale { Color32::from_gray(24) } else { Color32::WHITE };
    ui.painter().circle_filled(knob, knob_r, theme::with_alpha(if tl.outline && t < 0.5 { theme::text_dim() } else if t < 0.5 { Color32::WHITE } else { on_knob }, fade));
    resp
}

/// Whole seconds on a track like the volume slider's, with the ends labelled and the
/// value beside it: click or drag to set (scrolling over it scrolls the page).
fn seconds_slider(ui: &mut Ui, v: &mut u32, range: std::ops::RangeInclusive<u32>, name: &str) -> egui::Response {
    let mut f = *v as f32;
    let resp = step_slider(ui, &mut f, (*range.start() as f32, *range.end() as f32, 1.0), None, |v| format!("{v} s"), name);
    *v = f as u32;
    resp
}

/// A slider over `lo..=hi` in steps of `step`, for a settings row: the value, then the
/// track between its ends. It fills from `from` (`lo` when none) to the knob.
fn step_slider(ui: &mut Ui, v: &mut f32, (lo, hi, step): (f32, f32, f32), from: Option<f32>, text: impl Fn(f32) -> String, name: &str) -> egui::Response {
    let label = |ui: &mut Ui, g: std::sync::Arc<egui::Galley>, w: Option<f32>, color: Color32| {
        let (rect, _) = ui.allocate_exact_size(vec2(w.unwrap_or(g.size().x), 26.0), Sense::hover());
        ui.painter().galley(pos2(rect.right() - g.size().x, rect.center().y - g.size().y / 2.0), g, color);
    };
    let end = |ui: &Ui, v: f32| widgets::galley(ui, &text(v), theme::regular(12.0), theme::text_dim(), 60.0);
    let (hi_g, lo_g) = (end(ui, hi), end(ui, lo));
    label(ui, widgets::galley(ui, &text(*v), theme::semibold(13.0), theme::text(), 60.0), Some(40.0), theme::text());
    ui.add_space(6.0);
    label(ui, hi_g, None, theme::text_dim());
    // Shorter where the row hasn't the room (a narrow window), not over its description.
    let width = (ui.available_width() - lo_g.size().x - 2.0 * ui.spacing().item_spacing.x).clamp(60.0, 170.0);
    let resp = slider_track(ui, v, (lo, hi, step), from, width, name);
    label(ui, lo_g, None, theme::text_dim());
    resp
}

/// Room for a step slider in a settings row: its value, its ends and the track.
const SLIDER_W: f32 = 310.0;

/// Just the track and knob of a step slider, `width` wide.
pub(super) fn slider_track(ui: &mut Ui, v: &mut f32, (lo, hi, step): (f32, f32, f32), from: Option<f32>, width: f32, name: &str) -> egui::Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(width, 26.0), Sense::click_and_drag());
    let track = Rect::from_center_size(rect.center(), vec2(rect.width() - 16.0, 4.0));
    let old = *v;
    // Back home on a double-click, as the equalizer's bands go back to 0.
    if let Some(home) = from.filter(|_| resp.double_clicked()) {
        *v = home;
    } else if let Some(p) = resp.interact_pointer_pos()
        && (resp.dragged() || resp.clicked())
    {
        let t = ((p.x - track.left()) / track.width()).clamp(0.0, 1.0);
        *v = (((lo + t * (hi - lo)) / step).round() * step).clamp(lo, hi);
    }
    if *v != old {
        resp.mark_changed();
    }
    let value = *v;
    resp.widget_info(|| egui::WidgetInfo::slider(ui.is_enabled(), value as f64, name));
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() || resp.dragged(), 0.12);
    let accent = ui.ctx().data(|d| d.get_temp::<Color32>(egui::Id::unique("accent"))).unwrap_or(theme::RED);
    let x = |v: f32| egui::lerp(track.left()..=track.right(), (v - lo) / (hi - lo));
    let knob = pos2(x(value), track.center().y);
    let from = x(from.unwrap_or(lo));
    ui.painter().rect_filled(track, 2.0, theme::glass(0.15));
    ui.painter().rect_filled(Rect::from_min_max(pos2(from.min(knob.x), track.min.y), pos2(from.max(knob.x), track.max.y)), 2.0, accent);
    ui.painter().circle_filled(knob + vec2(0.0, 1.0), 7.0 + h, theme::shade(0.25));
    ui.painter().circle_filled(knob, 7.0 + h, Color32::WHITE);
    resp
}

/// Draw a key cap whose right edge is at `right`; returns its left edge.
pub(super) fn keycap(ui: &Ui, right: egui::Pos2, key: &str) -> f32 {
    let g = widgets::galley(ui, key, theme::semibold(12.5), theme::text(), 200.0);
    let size = vec2((g.size().x + 18.0).max(30.0), 26.0);
    let rect = Rect::from_min_size(pos2(right.x - size.x, right.y - size.y / 2.0), size);
    ui.painter().rect_filled(rect, 7.0, theme::glass(0.08));
    ui.painter().rect_stroke(rect, 7.0, egui::Stroke::new(1.0, theme::glass(0.12)), egui::StrokeKind::Inside);
    ui.painter().rect_filled(Rect::from_min_max(pos2(rect.left() + 2.0, rect.bottom() - 2.0), pos2(rect.right() - 2.0, rect.bottom())), 2.0, theme::shade(0.25));
    ui.painter().galley(rect.center() - g.size() / 2.0, g, theme::text());
    rect.left()
}

/// Theme preset gallery: one card per preset with its cover, name and creators.
fn themes(app: &mut App, ui: &mut Ui, shown: &[&crate::preset::Preset]) {
    let avail = ui.available_width();
    let gap = 16.0;
    let cols = ((avail + gap) / (200.0 + gap)).floor().max(1.0);
    let w = ((avail + gap) / cols - gap).floor();
    let mut picked = None;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = vec2(gap, gap + 4.0);
        ui.horizontal_wrapped(|ui| {
            for p in shown {
                let selected = app.theme_id == p.id;
                if theme_card(ui, p, w, selected, app.accent_color()).clicked() {
                    picked = Some(p.id.clone());
                }
            }
        });
    });
    if let Some(id) = picked {
        app.theme_id = id;
    }
}

fn theme_card(ui: &mut Ui, p: &crate::preset::Preset, w: f32, selected: bool, accent: Color32) -> egui::Response {
    let img_h = (w * 0.625).round();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, img_h + 52.0), Sense::click());
    let resp = named(resp, &format!("Theme: {}", p.name)).on_hover_cursor(egui::CursorIcon::PointingHand);
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
    let s = theme::anim_bool(ui.ctx(), resp.id.with("s"), selected, 0.2);
    let img = Rect::from_min_size(rect.min, vec2(w, img_h));
    let painter = ui.painter();
    painter.rect_filled(rect.expand(6.0), 16.0, theme::glass(0.04 * h + 0.06 * s));
    match crate::preset::cover(&p.id) {
        Some(bytes) => {
            egui::Image::from_bytes(format!("bytes://theme-cover/{}.jpg", p.id), bytes)
                .corner_radius(egui::CornerRadius::same(10))
                .paint_at(ui, img);
        }
        None => {
            // The built-in look has no screenshot: a small abstract of it instead.
            painter.rect_filled(img, 10.0, Color32::from_rgb(22, 18, 28));
            let c = img.center();
            painter.circle_filled(c - vec2(w * 0.18, 0.0), img_h * 0.42, theme::with_alpha(accent, 0.35));
            painter.circle_filled(c + vec2(w * 0.2, img_h * 0.1), img_h * 0.36, theme::with_alpha(Color32::from_rgb(110, 80, 200), 0.3));
            painter.rect_filled(Rect::from_center_size(c - vec2(w * 0.22, 0.0), Vec2::splat(img_h * 0.42)), 6.0, theme::glass_panel(0.25));
            for (i, len) in [0.34f32, 0.42, 0.28].iter().enumerate() {
                let y = c.y - img_h * 0.12 + i as f32 * img_h * 0.13;
                let a = if i == 1 { 0.9 } else { 0.35 };
                painter.rect_filled(Rect::from_min_size(pos2(c.x - w * 0.02, y), vec2(w * len, img_h * 0.07)), 4.0, theme::with_alpha(Color32::WHITE, a));
            }
        }
    }
    if h > 0.0 {
        painter.rect_filled(img, 10.0, theme::glass(0.06 * h));
    }
    if s > 0.0 {
        painter.rect_stroke(img.expand(2.0), 12.0, egui::Stroke::new(2.5, theme::with_alpha(Color32::WHITE, s)), egui::StrokeKind::Outside);
        let badge = pos2(img.right() - 18.0, img.top() + 18.0);
        painter.circle_filled(badge, 12.0 * s, Color32::WHITE);
        widgets::icon_at(ui, badge, icon::CHECK, 14.0 * s, Color32::from_gray(15));
    }
    let name_y = img.bottom() + 10.0;
    widgets::text_at(ui, pos2(rect.left() + 2.0, name_y), &p.name, theme::semibold(14.5), theme::text(), w - 4.0);
    let by = if p.creators.is_empty() { "Built in".to_owned() } else { format!("by {}", p.creators.join(", ")) };
    widgets::text_at(ui, pos2(rect.left() + 2.0, name_y + 20.0), &by, theme::regular(12.0), theme::text_faint(), w - 4.0);
    // Just where it comes from, on one small line: the card already has its name and maker.
    resp.on_hover_ui(|ui| {
        ui.add(egui::Label::new(egui::RichText::new(format!("From {}", p.source)).font(theme::regular(12.0)).color(theme::text_dim())).extend());
    })
}

#[cfg(test)]
mod settings_search_tests {
    use super::*;

    fn words(q: &str) -> Vec<String> {
        q.to_lowercase().split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn every_word_has_to_be_there_in_any_case() {
        assert!(words_match(&words("Close TRAY"), "Window", "Close to tray"));
        assert!(!words_match(&words("close discord"), "Window", "Close to tray"));
    }

    #[test]
    fn a_section_s_name_counts_for_its_rows() {
        assert!(words_match(&words("network"), "Network", "Test connection"));
        assert!(words_match(&words("network test"), "Network", "Test connection"));
    }

    /// Whether `control` ran for a row, and how much room the section took, while
    /// searching for `q`.
    fn section_with_a_row(q: &str) -> (bool, f32) {
        let ctx = egui::Context::default();
        let (mut ran, mut height) = (false, 0.0);
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(800.0, 600.0))), ..Default::default() };
        let mut output = ctx.run_ui(raw, |ui| {
            FILTER.set(SettingsFilter { words: words(q), ..Default::default() });
            let top = ui.cursor().top();
            settings_section(ui, "Window", None, None, |ui| {
                setting_row(ui, "Close to tray", Some("Keeps the music playing."), |_| ran = true);
            });
            height = ui.cursor().top() - top;
            FILTER.take();
        });
        output.textures_delta.clear();
        (ran, height)
    }

    #[test]
    fn a_section_with_nothing_found_takes_no_room() {
        let (ran, height) = section_with_a_row("discord");
        assert!(!ran);
        assert_eq!(height, 0.0);
    }

    #[test]
    fn a_section_with_a_match_shows_it() {
        let (ran, height) = section_with_a_row("music");
        assert!(ran);
        assert!(height > 60.0, "the heading and the row need room, got {height}");
    }

    #[test]
    fn rows_elsewhere_are_never_filtered() {
        // Jam and transfer use the same rows, with no search on their pages.
        assert!(!filtering());
        assert!(filter_hit("anything at all"));
    }
}
