//! The command palette (Ctrl+K, Cmd+K on macOS): pages, settings, playlists and actions,
//! found by typing a few letters of them.

use egui::{Align2, Key, Modifiers, Rect, RichText, Sense, Ui, Vec2, pos2, vec2};

use crate::model::{Target, Thumb};
use crate::theme::{self, icon};

use super::pages::SettingsTab;
use super::{Action, App, LibTab, Page, Repeat, START_PAGES, jam, notes, start_page, widgets};

/// How many commands run lately rank higher.
const RECENT: usize = 8;

#[derive(Default)]
pub(super) struct Palette {
    open: bool,
    query: String,
    selected: usize,
    /// Just opened: the list starts from its top.
    fresh: bool,
    /// Up, Down, Enter and Esc this frame, taken before anything else sees them.
    keys: (bool, bool, bool, bool),
    /// The commands run lately, most recent first: for this run of the app only.
    recent: Vec<Command>,
}

fn id() -> egui::Id {
    egui::Id::unique("command-palette")
}

#[derive(Clone, PartialEq)]
enum Command {
    Go(Page),
    NowPlaying,
    /// A Settings category, or a section of it by its title.
    Setting(SettingsTab, &'static str),
    Open(Target),
    Search(String),
    Act(Act),
}

#[derive(Clone, Copy, PartialEq)]
enum Act {
    TogglePlay,
    Next,
    Prev,
    Shuffle,
    Repeat,
    Like,
    Lyrics,
    Queue,
    Mini,
    FullScreen,
    Jam,
    NewPlaylist,
}

/// Listed in this order when nothing ranks one above another.
#[derive(Clone, Copy, PartialEq)]
enum Group {
    Pages,
    Settings,
    Playlists,
    Actions,
    Search,
}

impl Group {
    fn label(self) -> &'static str {
        match self {
            Self::Pages => "Pages",
            Self::Settings => "Settings",
            Self::Playlists => "Playlists",
            Self::Actions => "Actions",
            Self::Search => "Search",
        }
    }
}

struct Entry {
    cmd: Command,
    group: Group,
    label: String,
    /// Dim beside it: its shortcut, or the category a setting is in.
    detail: &'static str,
    glyph: char,
    /// A playlist's cover, shown instead of the glyph.
    cover: Vec<Thumb>,
}

/// Settings' categories and sections, by their titles. One named as its category opens
/// that; any other is looked for with Settings' own search.
const SECTIONS: [(&str, SettingsTab); 22] = {
    use SettingsTab::*;
    [
        ("Account", Account),
        ("Connections", Account),
        ("Playback", Playback),
        ("Sound", Playback),
        ("Music videos", Playback),
        ("Equalizer", Playback),
        ("Playback speed", Playback),
        ("Lyrics", Lyrics),
        ("Lyrics timing", Lyrics),
        ("Lyrics sources", Lyrics),
        ("Appearance", Appearance),
        ("Mini player", Appearance),
        ("App icon", Appearance),
        ("General", General),
        ("Window", General),
        ("Downloads", General),
        ("Network", General),
        ("Import", General),
        ("Keyboard shortcuts", Shortcuts),
        ("System-wide shortcuts", Shortcuts),
        ("Updates", About),
        ("About", About),
    ]
};

/// Ctrl+K opens and closes it. Taken before any text box sees it: there, it would delete
/// to the end of the line (Cmd+K on macOS leaves Ctrl+K to do that). Its own keys are
/// taken here too, at the start of the frame: else the page under it would see Esc, and
/// the text box would move its cursor or give up its focus.
pub(super) fn shortcut(app: &mut App, ctx: &egui::Context) {
    let p = &mut app.palette;
    // The mini player has no room for it, and a dialog or the release notes come first.
    let other_modal = ctx.memory(|m| m.top_modal_layer()).is_some_and(|l| l.id != id());
    if app.mini.busy() || app.dialog.is_some() || other_modal {
        p.open = false;
        return;
    }
    if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::K)) {
        p.open = !p.open;
        p.query.clear();
        p.selected = 0;
        p.fresh = true;
    }
    p.keys = if p.open {
        ctx.input_mut(|i| {
            let mut key = |k| i.consume_key(Modifiers::NONE, k);
            (key(Key::ArrowUp), key(Key::ArrowDown), key(Key::Enter), key(Key::Escape))
        })
    } else {
        Default::default()
    };
}

/// Everything it offers right now.
fn entries(app: &App) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut add = |group, cmd, label: &str, detail, glyph| out.push(Entry { cmd, group, label: label.to_owned(), detail, glyph, cover: Vec::new() });
    for (_, label, page) in START_PAGES {
        // Not the account's Library signed out, nor Podcasts hidden.
        if start_page(&page, app.logged_in, app.show_podcasts) == page {
            let glyph = match page {
                Page::Home => icon::HOUSE,
                Page::Explore => icon::COMPASS,
                Page::Library(LibTab::Downloads) => icon::DOWNLOAD,
                _ => icon::LIBRARY,
            };
            add(Group::Pages, Command::Go(page), label, "", glyph);
        }
    }
    add(Group::Pages, Command::Go(Page::Jam), "Listen together", "", icon::USERS);
    add(Group::Pages, Command::Go(Page::Transfer), "Transfer from Spotify", "", icon::UPLOAD);
    add(Group::Pages, Command::Go(Page::Settings), "Settings", "", icon::SETTINGS);
    let song = app.current().is_some();
    if song {
        add(Group::Pages, Command::NowPlaying, "Now playing", "", icon::MUSIC);
    }
    for (name, tab) in SECTIONS {
        add(Group::Settings, Command::Setting(tab, name), name, if name == tab.label() { "" } else { tab.label() }, icon::SETTINGS);
    }
    let lists: Vec<_> =
        app.library.as_ref().map(|l| &l.playlists).or(app.sidebar_cache.as_ref()).into_iter().flatten().filter(|c| !matches!(c.target, Target::Song(_))).collect();
    for c in &lists {
        add(Group::Playlists, Command::Open(c.target.clone()), &c.title, "", icon::LIST_MUSIC);
    }
    let act = |a| Command::Act(a);
    if song {
        let playing = app.playing();
        add(Group::Actions, act(Act::TogglePlay), if playing { "Pause" } else { "Play" }, "Space", if playing { icon::PAUSE } else { icon::PLAY });
        add(Group::Actions, act(Act::Next), "Next song", "Ctrl + →", icon::SKIP_FORWARD);
        add(Group::Actions, act(Act::Prev), "Previous song", "Ctrl + ←", icon::SKIP_BACK);
        add(Group::Actions, act(Act::Shuffle), if app.queue.shuffled() { "Shuffle off" } else { "Shuffle on" }, "", icon::SHUFFLE);
        add(Group::Actions, act(Act::Like), if app.heard_liked() { "Remove like" } else { "Like this song" }, "", icon::HEART);
        add(Group::Actions, act(Act::Lyrics), "Lyrics and now playing", "L", icon::MIC_VOCAL);
        let (label, glyph) = if app.fullscreen { ("Leave full screen", icon::SHRINK) } else { ("Full screen", icon::EXPAND) };
        add(Group::Actions, act(Act::FullScreen), label, "F", glyph);
    }
    // What it turns repeat to.
    let (label, glyph) = match app.queue.repeat.next() {
        Repeat::Off => ("Repeat off", icon::REPEAT),
        Repeat::All => ("Repeat all", icon::REPEAT),
        Repeat::One => ("Repeat this song", icon::REPEAT_1),
    };
    add(Group::Actions, act(Act::Repeat), label, "", glyph);
    add(Group::Actions, act(Act::Queue), if app.queue_shown() { "Hide queue" } else { "Show queue" }, "", icon::LIST_MUSIC);
    add(Group::Actions, act(Act::Mini), "Mini player", "Ctrl + M", icon::PICTURE_IN_PICTURE);
    let jam = match (app.jam.active(), app.jam.is_host()) {
        (false, _) => "Start a jam",
        (true, true) => "End the jam",
        (true, false) => "Leave the jam",
    };
    add(Group::Actions, act(Act::Jam), jam, "", icon::USERS);
    if app.logged_in {
        add(Group::Actions, act(Act::NewPlaylist), "New playlist", "", icon::LIST_PLUS);
    }
    // The playlists' covers, as in the sidebar.
    for (e, c) in out.iter_mut().filter(|e| e.group == Group::Playlists).zip(lists) {
        e.cover = c.thumbs.clone();
    }
    out
}

/// How well `query` matches `text`, ignoring case: each of its words has its letters in
/// `text` in order, not always together. Higher is better; letters starting a word, and
/// ones together, count for more. `None` is no match.
fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let text: Vec<char> = text.to_lowercase().chars().collect();
    query.to_lowercase().split_whitespace().try_fold(0, |sum, word| Some(sum + word_score(&word.chars().collect::<Vec<_>>(), &text)?))
}

/// The best of the matches of `word` that start at each place its first letter is.
fn word_score(word: &[char], text: &[char]) -> Option<i32> {
    let starts = text.iter().enumerate().filter(|(_, c)| **c == word[0]).map(|(i, _)| i);
    starts
        .filter_map(|start| {
            let (mut score, mut at, mut last) = (if start == 0 { 4 } else { 0 }, start, None);
            for &c in word {
                let i = at + text[at..].iter().position(|&t| t == c)?;
                score += 1;
                if i == 0 || !text[i - 1].is_alphanumeric() {
                    score += 8;
                }
                if last.is_some_and(|l| l + 1 == i) {
                    score += 6;
                }
                (last, at) = (Some(i), i + 1);
            }
            Some(score)
        })
        .max()
}

/// The entries that match `query`, best first and kept together by group, the groups in
/// the order of their best entries. Commands run lately rank higher.
fn rank(entries: Vec<Entry>, query: &str, recent: &[Command]) -> Vec<Entry> {
    let mut scored: Vec<(i32, Entry)> = entries
        .into_iter()
        .filter_map(|e| {
            let score = fuzzy_score(query, &format!("{} {}", e.label, e.detail))?;
            let lately = recent.iter().position(|c| *c == e.cmd).map_or(0, |i| 2 * (RECENT - i) as i32);
            Some((score + lately, e))
        })
        .collect();
    // Stable: a tie keeps the order they're listed in.
    scored.sort_by_key(|(score, _)| -score);
    let mut groups = Vec::new();
    for (_, e) in &scored {
        if !groups.contains(&e.group) {
            groups.push(e.group);
        }
    }
    scored.sort_by_key(|(_, e)| groups.iter().position(|g| *g == e.group));
    scored.into_iter().map(|(_, e)| e).collect()
}

/// What it lists for `query`, with searching for it last.
fn results(entries: Vec<Entry>, query: &str, recent: &[Command]) -> Vec<Entry> {
    let mut list = rank(entries, query, recent);
    let query = query.trim();
    if !query.is_empty() {
        let label = format!("Search for \u{201c}{query}\u{201d}");
        list.push(Entry { cmd: Command::Search(query.to_owned()), group: Group::Search, label, detail: "", glyph: icon::SEARCH, cover: Vec::new() });
    }
    list
}

pub(super) fn show(app: &mut App, ctx: &egui::Context) {
    if !app.palette.open {
        return;
    }
    let all = entries(app);
    let p = &mut app.palette;
    let (up, down, enter, esc) = std::mem::take(&mut p.keys);
    let screen = ctx.content_rect();
    widgets::modal_extras(ctx, id());
    let area = egui::Modal::default_area(id()).anchor(Align2::CENTER_TOP, vec2(0.0, (screen.height() * 0.12).max(24.0)));
    let modal = egui::Modal::new(id()).area(area).frame(notes::surface()).backdrop_color(theme::shade(0.45)).show(ctx, |ui| {
        ui.set_width(560.0_f32.min(screen.width() - 32.0));
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 54.0), Sense::hover());
        widgets::icon_at(ui, pos2(rect.left() + 24.0, rect.center().y), icon::SEARCH, 17.0, theme::text_dim());
        let inner = Rect::from_min_max(pos2(rect.left() + 46.0, rect.top()), pos2(rect.right() - 18.0, rect.bottom()));
        let edit = ui.put(
            inner,
            egui::TextEdit::singleline(&mut p.query)
                .hint_text(RichText::new("Search pages, settings, playlists and actions").color(theme::text_faint()))
                .font(theme::regular(16.0))
                .frame(egui::Frame::NONE)
                .align(Align2::LEFT_CENTER)
                .desired_width(inner.width()),
        );
        // The one text box while it's open: Ctrl+F's search box doesn't keep it. Only
        // when it's lost, as asking breaks off typing with an IME.
        if !edit.has_focus() {
            edit.request_focus();
        }
        // From the top on opening and as the query changes.
        let top = std::mem::take(&mut p.fresh) || edit.changed();
        if top {
            p.selected = 0;
        }
        // Ranked after the text box takes this frame's typing: Enter runs what's shown.
        let list = results(all, &p.query, &p.recent);
        let n = list.len();
        p.selected = p.selected.min(n.saturating_sub(1));
        if n > 0 && down {
            p.selected = (p.selected + 1) % n;
        }
        if n > 0 && up {
            p.selected = (p.selected + n - 1) % n;
        }
        let mut run = enter.then_some(p.selected).filter(|_| n > 0);
        let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
        ui.painter().rect_filled(line, 0.0, theme::glass(0.08));
        let mut chosen = None;
        // Never empty: a blank query lists everything, any other searches for itself too.
        egui::Frame::new().inner_margin(egui::Margin::same(8)).show(ui, |ui| {
            let moved = ui.input(|i| i.pointer.delta() != Vec2::ZERO);
            // Its full height asked for, then shrunk to the list: else it could only shrink, as
            // the modal offers just the room it took the frame before.
            let max = (screen.height() * 0.6).min(420.0);
            let mut scroll = egui::ScrollArea::vertical().max_height(max).min_scrolled_height(max).auto_shrink([false, true]);
            if top {
                scroll = scroll.vertical_scroll_offset(0.0);
            }
            scroll.show(ui, |ui| {
                let mut group = None;
                for (i, e) in list.iter().enumerate() {
                    if group != Some(e.group) {
                        group = Some(e.group);
                        heading(ui, e.group.label());
                    }
                    let resp = row(ui, e, i == p.selected);
                    if i == p.selected {
                        chosen = Some(resp.id);
                        if up || down {
                            resp.scroll_to_me(None);
                        }
                    }
                    // Hovering picks a row only when the pointer moves: not when the
                    // list scrolls under it from the keys.
                    if resp.hovered() && moved {
                        p.selected = i;
                    }
                    if resp.clicked() {
                        run = Some(i);
                    }
                }
            });
        });
        (edit.id, chosen, run.and_then(|i| list.into_iter().nth(i)))
    });
    // Screen readers hear the row picked with the arrows, while the text box keeps the focus.
    let (edit, chosen, run) = modal.inner;
    ctx.accesskit_node_builder(edit, |node| {
        node.set_label("Command palette");
        if let Some(row) = chosen {
            node.set_active_descendant(row.accesskit_id());
        }
    });
    if esc || modal.backdrop_response.clicked() {
        app.palette.open = false;
    }
    let Some(entry) = run else { return };
    let p = &mut app.palette;
    p.open = false;
    if !matches!(entry.cmd, Command::Search(_)) {
        p.recent.retain(|c| *c != entry.cmd);
        p.recent.insert(0, entry.cmd.clone());
        p.recent.truncate(RECENT);
    }
    run_command(app, ctx, entry.cmd);
    ctx.request_repaint();
}

fn heading(ui: &mut Ui, label: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
    ui.painter().text(pos2(rect.left() + 10.0, rect.bottom() - 7.0), Align2::LEFT_BOTTOM, label.to_uppercase(), theme::semibold(11.0), theme::text_faint());
}

fn row(ui: &mut Ui, e: &Entry, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::click());
    let name = [e.label.as_str(), e.detail, e.group.label()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", ");
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::ListBoxOption, true, selected, &name));
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let s =theme::anim_bool(ui.ctx(), resp.id.with("s"), selected, 0.1);
    if s > 0.0 {
        ui.painter().rect_filled(rect, 8.0, theme::glass(0.10 * s));
    }
    let mid = rect.center().y;
    let at = pos2(rect.left() + 20.0, mid);
    if e.cover.is_empty() {
        widgets::icon_at(ui, at, e.glyph, 16.0, theme::lerp_color(theme::text_dim(), theme::text(), s));
    } else {
        // The sidebar's corners, scaled to this smaller cover.
        widgets::paint_cover(ui, Rect::from_center_size(at, Vec2::splat(26.0)), &e.cover, theme::thumb_radius() * 26.0 / 36.0, 1.0);
    }
    let detail_w = if e.detail.is_empty() {
        0.0
    } else {
        ui.painter().text(pos2(rect.right() - 12.0, mid), Align2::RIGHT_CENTER, e.detail, theme::regular(12.5), theme::text_faint()).width() + 16.0
    };
    let x = rect.left() + 42.0;
    let g = widgets::galley(ui, &e.label, theme::regular(14.0), theme::text(), rect.right() - 12.0 - detail_w - x);
    ui.painter().galley(pos2(x, mid - g.size().y / 2.0), g, theme::text());
    resp
}

/// Does what it says, the way its button or key does.
fn run_command(app: &mut App, ctx: &egui::Context, cmd: Command) {
    // Full screen covers the pages: out of it first, as Esc would.
    let to_page = matches!(cmd, Command::Go(_) | Command::Setting(..) | Command::Open(_) | Command::Search(_))
        || (cmd == Command::Act(Act::Jam) && !app.jam.active());
    if to_page && app.fullscreen {
        app.set_fullscreen(ctx, false);
    }
    match cmd {
        Command::Go(page) => app.actions.push(Action::Go(page)),
        Command::NowPlaying => app.actions.push(Action::OpenNowPlaying(true)),
        Command::Setting(tab, name) => {
            // Going there clears Settings' search, so this comes after.
            app.go(Page::Settings);
            app.settings_tab = tab;
            if name != tab.label() {
                app.settings_query = name.to_owned();
            }
        }
        Command::Open(target) => app.actions.push(Action::Open(target)),
        Command::Search(q) => app.actions.push(Action::Go(Page::Search(q))),
        Command::Act(a) => match a {
            Act::TogglePlay => app.actions.push(Action::TogglePlay),
            Act::Next => app.next(),
            Act::Prev => app.prev(),
            Act::Shuffle => app.toggle_shuffle(),
            Act::Repeat => app.queue.repeat = app.queue.repeat.next(),
            Act::Like => app.actions.push(Action::LikeHeard(!app.heard_liked())),
            Act::Lyrics => app.toggle_lyrics(),
            Act::Queue => app.toggle_queue(),
            Act::Mini => app.actions.push(Action::Mini(!app.mini.target())),
            Act::FullScreen => {
                app.np_open = true;
                app.toggle_fullscreen(ctx);
            }
            Act::Jam if app.jam.active() => jam::leave(app),
            Act::Jam => {
                jam::start(app);
                // Its code and link are there, to share.
                app.actions.push(Action::Go(Page::Jam));
            }
            Act::NewPlaylist => app.actions.push(Action::NewPlaylist(Vec::new())),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(group: Group, label: &str) -> Entry {
        Entry { cmd: Command::Search(label.to_owned()), group, label: label.to_owned(), detail: "", glyph: icon::PLAY, cover: Vec::new() }
    }

    fn labels(list: &[Entry]) -> Vec<&str> {
        list.iter().map(|e| e.label.as_str()).collect()
    }

    #[test]
    fn sections_are_in_settings() {
        // A renamed section would else turn into a plain search for its old name.
        let code = [include_str!("pages.rs"), include_str!("hotkeys.rs"), include_str!("transfer.rs")].concat();
        for (name, tab) in SECTIONS.into_iter().filter(|(name, tab)| *name != tab.label()) {
            assert!(code.contains(&format!("(ui, \"{name}\"")), "no section titled {name:?} in {:?}", tab.label());
        }
    }

    #[test]
    fn matches_letters_in_order() {
        assert!(fuzzy_score("nxt", "Next song").is_some());
        assert!(fuzzy_score("NEXT", "next song").is_some());
        assert_eq!(fuzzy_score("txn", "Next song"), None, "out of order");
        assert_eq!(fuzzy_score("nexts", "Next"), None, "a letter too many");
        assert_eq!(fuzzy_score("", "anything"), Some(0), "nothing typed matches everything");
    }

    #[test]
    fn every_word_in_any_order() {
        assert!(fuzzy_score("song next", "Next song").is_some());
        assert_eq!(fuzzy_score("next radio", "Next song"), None);
    }

    #[test]
    fn word_starts_and_runs_count_more() {
        assert!(fuzzy_score("pl", "Playlists") > fuzzy_score("pl", "Apple"), "a word's start over its middle");
        assert!(fuzzy_score("mu", "Music") > fuzzy_score("mu", "Mini player menu"), "together over apart");
        assert!(fuzzy_score("lib", "Library") > fuzzy_score("lib", "Public library"), "the start of it all over a later word");
        // From the best place, not the first one the letter is at.
        assert_eq!(fuzzy_score("pla", "Apple Playlist"), fuzzy_score("pla", "x Playlist"));
    }

    #[test]
    fn groups_go_by_their_best_match() {
        let list = vec![entry(Group::Pages, "Library › Liked songs"), entry(Group::Settings, "Lyrics"), entry(Group::Actions, "Like this song"), entry(Group::Pages, "Home")];
        assert_eq!(labels(&rank(list, "like", &[])), ["Like this song", "Library › Liked songs"]);
    }

    #[test]
    fn ties_keep_the_listed_order() {
        let list = vec![entry(Group::Pages, "Home"), entry(Group::Pages, "Explore"), entry(Group::Actions, "Play"), entry(Group::Pages, "Settings")];
        assert_eq!(labels(&rank(list, "", &[])), ["Home", "Explore", "Settings", "Play"]);
    }

    #[test]
    fn recent_ones_rank_higher() {
        let list = || vec![entry(Group::Pages, "Home"), entry(Group::Pages, "Explore"), entry(Group::Actions, "Play"), entry(Group::Actions, "Pause")];
        let recent = [Command::Search("Pause".into()), Command::Search("Explore".into())];
        assert_eq!(labels(&rank(list(), "", &recent)), ["Pause", "Play", "Explore", "Home"]);
        // A better match still beats one run lately.
        assert_eq!(labels(&rank(list(), "pla", &recent))[0], "Play");
    }
}
