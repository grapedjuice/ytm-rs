//! Find in a song list: the box above a list, narrowing it to the songs that match, and
//! the order it's sorted in. Rows keep their numbers in the whole list, for selecting and
//! removing. Explicit songs, when hidden, are left out here too.

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use egui::{Rect, Sense, Ui, Vec2, pos2, vec2};

use super::widgets::{icon_at, named};
use super::{LibTab, Page};
use crate::model::{Card, Track};
use crate::theme::{self, icon};

/// Lists shorter than this are easy enough to look through: no box.
const MIN_SONGS: usize = 12;
pub const BOX_W: f32 = 260.0;
const BOX_H: f32 = 38.0;
/// How many lists' sorts are remembered; the ones sorted longest ago are forgotten first.
const KEEP_SORTS: usize = 100;

/// The order a list is shown in. `Default` is the list's own: a playlist's, or most
/// recent first in History, Liked songs, Downloads and the Library grids.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug, serde::Serialize, serde::Deserialize)]
pub enum Sort {
    #[default]
    Default,
    /// A to Z.
    Title,
    /// Z to A (the Library grids).
    TitleDesc,
    Artist,
    Album,
    /// Shortest first.
    Duration,
    /// The list's own order turned around: oldest first, where it comes newest first.
    Reversed,
}

/// What's being looked for, in one list at a time: another list starts empty. Sorts
/// are each list's own, and remembered.
#[derive(Default)]
pub struct ListFind {
    page: Option<Page>,
    query: String,
    pub hide_explicit: bool,
    /// Each list's sort but the default, by [`list_id`]; the most recently sorted first.
    pub sorts: Vec<(String, Sort)>,
    /// A list whose sort waits until all of it is in.
    held: Option<Page>,
    /// The last answer: looking through thousands of songs every frame shows.
    memo: RefCell<Option<Memo>>,
    /// The Library grid's last order, the same way: its sort, its cards and their order.
    card_memo: RefCell<Option<(Sort, ListKey, Vec<usize>)>>,
}

struct Memo {
    query: String,
    hide_explicit: bool,
    sort: Sort,
    list: ListKey,
    rows: Vec<usize>,
}

/// Which songs, in which order: where they're kept, how many, and their ids (a row moved
/// in place keeps the first two, and rows go by number).
type ListKey = (usize, usize, u64);

fn list_key(tracks: &[Track]) -> ListKey {
    let mut ids = DefaultHasher::new();
    for t in tracks {
        t.id.hash(&mut ids);
    }
    (tracks.as_ptr() as usize, tracks.len(), ids.finish())
}

impl ListFind {
    /// The rows of `tracks` to show on `page`, in the order shown; `None` when it's
    /// neither narrowed nor sorted.
    pub fn rows(&self, page: &Page, tracks: &[Track]) -> Option<Vec<usize>> {
        let asked = if self.page.as_ref() == Some(page) { self.query.as_str() } else { "" };
        let sort = self.sort(page);
        if asked.trim().is_empty() && sort == Sort::Default && !(self.hide_explicit && tracks.iter().any(|t| t.explicit)) {
            return None;
        }
        let list = list_key(tracks);
        let mut memo = self.memo.borrow_mut();
        if let Some(m) = memo.as_ref().filter(|m| m.query == asked && m.hide_explicit == self.hide_explicit && m.sort == sort && m.list == list) {
            return Some(m.rows.clone());
        }
        let query = fold(asked);
        let words: Vec<&str> = query.split_whitespace().collect();
        let mut rows: Vec<usize> = tracks.iter().enumerate().filter(|(_, t)| !(self.hide_explicit && t.explicit) && matches(t, &words)).map(|(i, _)| i).collect();
        sort_rows(&mut rows, tracks, sort);
        *memo = Some(Memo { query: asked.to_owned(), hide_explicit: self.hide_explicit, sort, list, rows: rows.clone() });
        Some(rows)
    }

    /// All of `tracks` in the order `page` sorts them, for its Play button; `None` when
    /// it isn't sorted.
    pub fn sorted(&self, page: &Page, tracks: &[Track]) -> Option<Vec<usize>> {
        let sort = self.sort(page);
        (sort != Sort::Default).then(|| {
            let mut rows: Vec<usize> = (0..tracks.len()).collect();
            sort_rows(&mut rows, tracks, sort);
            rows
        })
    }

    /// Something is looked for in `page`'s list.
    pub fn searching(&self, page: &Page) -> bool {
        self.page.as_ref() == Some(page) && !self.query.trim().is_empty()
    }

    /// `page`'s list is shown in another order than its own.
    pub fn reordered(&self, page: &Page) -> bool {
        self.sort(page) != Sort::Default
    }

    /// How `page`'s list is sorted.
    pub fn sort(&self, page: &Page) -> Sort {
        if self.held.as_ref() == Some(page) {
            return Sort::Default;
        }
        list_id(page).and_then(|id| self.sorts.iter().find(|(k, _)| *k == id)).map_or(Sort::Default, |(_, s)| *s)
    }

    /// Sort `page`'s list, remembered for next time.
    pub fn set_sort(&mut self, page: &Page, sort: Sort) {
        let Some(id) = list_id(page) else { return };
        self.sorts.retain(|(k, _)| *k != id);
        if sort != Sort::Default {
            self.sorts.insert(0, (id, sort));
            self.sorts.truncate(KEEP_SORTS);
        }
    }

    /// `held`: `page`'s list is still coming in, so it keeps its own order for now (and
    /// plays as it arrives).
    pub fn hold(&mut self, page: &Page, held: bool) {
        if held {
            self.held = Some(page.clone());
        } else if self.held.as_ref() == Some(page) {
            self.held = None;
        }
    }

    /// `page`'s list is held in its own order for now.
    pub fn holding(&self, page: &Page) -> bool {
        self.held.as_ref() == Some(page)
    }

    /// The Library grid on `page` in its sorted order, as indices into `cards`.
    pub fn cards(&self, page: &Page, cards: &[Card]) -> Vec<usize> {
        let mut rows: Vec<usize> = (0..cards.len()).collect();
        let sort = self.sort(page);
        if !matches!(sort, Sort::Title | Sort::TitleDesc) {
            return rows;
        }
        let mut titles = DefaultHasher::new();
        for c in cards {
            c.title.hash(&mut titles);
        }
        let list = (cards.as_ptr() as usize, cards.len(), titles.finish());
        let mut memo = self.card_memo.borrow_mut();
        if let Some((_, _, rows)) = memo.as_ref().filter(|(s, l, _)| *s == sort && *l == list) {
            return rows.clone();
        }
        match sort {
            Sort::Title => rows.sort_by_cached_key(|&i| fold(&cards[i].title)),
            _ => rows.sort_by_cached_key(|&i| std::cmp::Reverse(fold(&cards[i].title))),
        }
        *memo = Some((sort, list, rows.clone()));
        rows
    }

    /// The box, for a list of `tracks`, with how many match inside it: true when what's
    /// looked for changed.
    pub fn show(&mut self, ui: &mut Ui, page: &Page, tracks: &[Track]) -> bool {
        let total = tracks.iter().filter(|t| !(self.hide_explicit && t.explicit)).count();
        if self.page.as_ref() != Some(page) {
            self.page = Some(page.clone());
            self.query.clear();
        }
        if total < MIN_SONGS && self.query.is_empty() {
            return false;
        }
        let w = ui.available_width().min(BOX_W);
        let (rect, field) = ui.allocate_exact_size(vec2(w, BOX_H), Sense::click());
        let field = field.on_hover_cursor(egui::CursorIcon::Text);
        ui.painter().rect_filled(rect, BOX_H / 2.0, theme::glass(0.09));
        icon_at(ui, pos2(rect.left() + 18.0, rect.center().y), icon::SEARCH, 15.0, theme::text_dim());
        let clearable = !self.query.is_empty();
        let end = rect.right() - if clearable { 36.0 } else { 12.0 };
        let count = self.searching(page).then(|| self.rows(page, tracks)).flatten().map(|s| ui.painter().layout_no_wrap(format!("{} of {total}", s.len()), theme::regular(12.5), theme::text_faint()));
        let count_w = count.as_ref().map_or(0.0, |g| g.size().x + 8.0);
        let inner = Rect::from_min_max(pos2(rect.left() + 34.0, rect.top() + 3.0), pos2((end - count_w).max(rect.left() + 60.0), rect.bottom() - 3.0));
        if let Some(g) = count {
            let at = pos2(end - g.size().x, rect.center().y - g.size().y / 2.0);
            ui.painter().galley(at, g, theme::text_faint());
        }
        // In a child, so what comes after the box (the Sort button) isn't drawn over it.
        let edit = ui.new_child(egui::UiBuilder::new().id_salt("find-field").max_rect(inner).layout(egui::Layout::left_to_right(egui::Align::Center))).add(
            egui::TextEdit::singleline(&mut self.query)
                .hint_text(egui::RichText::new("Find in this list").color(theme::text_faint()))
                .font(theme::regular(14.0))
                .frame(egui::Frame::NONE)
                .align(egui::Align2::LEFT_CENTER)
                .desired_width(inner.width()),
        );
        let mut changed = edit.changed();
        if clearable {
            let x = Rect::from_center_size(pos2(rect.right() - 19.0, rect.center().y), Vec2::splat(26.0));
            let resp = named(ui.interact(x, ui.scope_id().with("find-clear"), Sense::click()), "Clear").on_hover_cursor(egui::CursorIcon::PointingHand);
            let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
            if h > 0.0 {
                ui.painter().circle_filled(x.center(), 13.0, theme::glass(0.10 * h));
            }
            icon_at(ui, x.center(), icon::X, 14.0, theme::lerp_color(theme::text_dim(), theme::text(), h));
            if resp.clicked() {
                self.query.clear();
                changed = true;
                edit.request_focus();
            }
        }
        if field.clicked() || ui.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::F)) {
            edit.request_focus();
        }
        changed
    }
}

/// Where a list's sort is remembered: a playlist by its id (Liked Music is "LM"), a
/// Library tab by its name. Albums keep their track order and aren't sorted.
fn list_id(page: &Page) -> Option<String> {
    let tab = match page {
        Page::Playlist(id) => return Some(id.clone()),
        Page::Library(LibTab::Songs) => super::LIKED_MUSIC,
        Page::Library(LibTab::History) => "history",
        Page::Library(LibTab::Uploads) => "uploads",
        Page::Library(LibTab::Downloads) => "downloads",
        Page::Library(LibTab::Playlists) => "playlists",
        Page::Library(LibTab::Albums) => "albums",
        Page::Library(LibTab::Podcasts) => "podcasts",
        _ => return None,
    };
    Some(tab.to_owned())
}

/// Put `rows` of `tracks` in `sort`'s order. Each song's key is made once, and songs
/// alike keep the list's own order. Songs without an artist, album or length go last.
fn sort_rows(rows: &mut [usize], tracks: &[Track], sort: Sort) {
    match sort {
        Sort::Default => {}
        Sort::Reversed => rows.reverse(),
        Sort::Duration => rows.sort_by_key(|&i| tracks[i].duration.unwrap_or(u32::MAX)),
        Sort::TitleDesc => rows.sort_by_cached_key(|&i| std::cmp::Reverse(fold(&tracks[i].title))),
        Sort::Title | Sort::Artist | Sort::Album => rows.sort_by_cached_key(|&i| {
            let t = &tracks[i];
            let by = match sort {
                Sort::Artist => t.artists.first().map(|a| fold(&a.name)),
                Sort::Album => t.album.as_ref().map(|a| fold(&a.name)),
                _ => None,
            };
            (sort != Sort::Title && by.is_none(), by, fold(&t.title))
        }),
    }
}

/// Lowercase without accents, so "Été" sorts among the e's.
fn fold(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    use unicode_normalization::char::is_combining_mark;
    s.nfd().filter(|c| !is_combining_mark(*c)).flat_map(char::to_lowercase).collect()
}

/// The rows a Shift-click picks: from `from` to `to` in the order on screen (`shown`, or
/// the whole list when that's `None`). Just `to` when `from` isn't on show.
pub fn run(shown: Option<&[usize]>, from: usize, to: usize) -> Vec<usize> {
    let Some(s) = shown else { return (from.min(to)..=from.max(to)).collect() };
    match (s.iter().position(|&r| r == from), s.iter().position(|&r| r == to)) {
        (Some(a), Some(b)) => s[a.min(b)..=a.max(b)].to_vec(),
        _ => vec![to],
    }
}

/// Every word (folded) is somewhere in the song's title, artists or album, accents aside.
fn matches(t: &Track, words: &[&str]) -> bool {
    let title = fold(&t.title);
    let artists: Vec<String> = t.artists.iter().map(|a| fold(&a.name)).collect();
    let album = t.album.as_ref().map(|a| fold(&a.name));
    words.iter().all(|w| title.contains(w) || artists.iter().any(|a| a.contains(w)) || album.as_ref().is_some_and(|a| a.contains(w)))
}

#[cfg(test)]
mod tests {
    use super::{KEEP_SORTS, LibTab, ListFind, Page, Sort, Track, run};
    use crate::model::{Card, Link, Target};

    fn link(name: &str) -> Link {
        Link { name: name.into(), id: None }
    }

    fn track(title: &str, artist: &str, album: &str) -> Track {
        Track { id: title.into(), title: title.into(), artists: vec![link(artist)], album: Some(link(album)), ..Default::default() }
    }

    fn songs() -> Vec<Track> {
        vec![track("Around the World", "Daft Punk", "Homework"), track("Levitating", "Dua Lipa", "Future Nostalgia"), track("One More Time", "Daft Punk", "Discovery"), track("ÉTÉ", "Angèle", "Brol")]
    }

    fn find(query: &str) -> Option<Vec<usize>> {
        let f = ListFind { page: Some(Page::Playlist("a".into())), query: query.into(), ..Default::default() };
        f.rows(&Page::Playlist("a".into()), &songs())
    }

    #[test]
    fn finds_by_title_artist_and_album() {
        assert_eq!(find("levitating"), Some(vec![1]));
        assert_eq!(find("daft"), Some(vec![0, 2]));
        assert_eq!(find("Discovery"), Some(vec![2]));
    }

    #[test]
    fn every_word_has_to_match_but_anywhere() {
        assert_eq!(find("punk time"), Some(vec![2]));
        assert_eq!(find("punk levitating"), Some(vec![]));
    }

    #[test]
    fn case_doesnt_matter_in_any_alphabet() {
        assert_eq!(find("été"), Some(vec![3]));
        assert_eq!(find("ANGÈLE"), Some(vec![3]));
    }

    #[test]
    fn nothing_typed_leaves_the_list_whole() {
        assert_eq!(find(""), None);
        assert_eq!(find("   "), None);
    }

    #[test]
    fn it_belongs_to_one_list() {
        let f = ListFind { page: Some(Page::Playlist("a".into())), query: "daft".into(), ..Default::default() };
        assert_eq!(f.rows(&Page::Playlist("b".into()), &songs()), None);
    }

    #[test]
    fn hidden_explicit_songs_are_left_out() {
        let mut tracks = songs();
        tracks[2].explicit = true;
        let page = Page::Playlist("a".into());
        let mut f = ListFind { page: Some(page.clone()), query: String::new(), hide_explicit: true, ..Default::default() };
        assert_eq!(f.rows(&page, &tracks), Some(vec![0, 1, 3]));
        f.query = "daft".into();
        assert_eq!(f.rows(&page, &tracks), Some(vec![0]));
        // Another list's search doesn't apply, but hiding still does.
        assert_eq!(f.rows(&Page::Playlist("b".into()), &tracks), Some(vec![0, 1, 3]));
        assert_eq!(f.rows(&page, &songs()[..2]), Some(vec![0]));
    }

    fn sorted(sort: Sort, query: &str, tracks: &[Track]) -> Option<Vec<usize>> {
        let page = Page::Playlist("a".into());
        let mut f = ListFind { page: Some(page.clone()), query: query.into(), ..Default::default() };
        f.set_sort(&page, sort);
        f.rows(&page, tracks)
    }

    #[test]
    fn sorts_by_title_artist_album_and_length() {
        let mut tracks = songs();
        for (t, secs) in tracks.iter_mut().zip([300, 200, 320, 180]) {
            t.duration = Some(secs);
        }
        // Accents don't count: "ÉTÉ" is among the e's, before "Levitating".
        assert_eq!(sorted(Sort::Title, "", &tracks), Some(vec![0, 3, 1, 2]));
        assert_eq!(sorted(Sort::TitleDesc, "", &tracks), Some(vec![2, 1, 3, 0]));
        // One artist's songs by title.
        assert_eq!(sorted(Sort::Artist, "", &tracks), Some(vec![3, 0, 2, 1]));
        assert_eq!(sorted(Sort::Album, "", &tracks), Some(vec![3, 2, 1, 0]));
        assert_eq!(sorted(Sort::Duration, "", &tracks), Some(vec![3, 1, 0, 2]));
        assert_eq!(sorted(Sort::Default, "", &tracks), None);
        assert_eq!(sorted(Sort::Reversed, "", &tracks), Some(vec![3, 2, 1, 0]));
        // Found songs turn around too.
        assert_eq!(sorted(Sort::Reversed, "daft", &tracks), Some(vec![2, 0]));
        // Nor in what's looked for.
        assert_eq!(sorted(Sort::Default, "ete", &tracks), Some(vec![3]));
    }

    #[test]
    fn songs_without_an_album_or_length_go_last() {
        let mut tracks = songs();
        tracks[3].album = None;
        assert_eq!(sorted(Sort::Album, "", &tracks), Some(vec![2, 1, 0, 3]));
        tracks[1].duration = Some(10);
        assert_eq!(sorted(Sort::Duration, "", &tracks), Some(vec![1, 0, 2, 3]));
    }

    #[test]
    fn found_songs_are_sorted_too() {
        assert_eq!(sorted(Sort::Title, "daft", &songs()), Some(vec![0, 2]));
        assert_eq!(sorted(Sort::TitleDesc, "daft", &songs()), Some(vec![2, 0]));
    }

    #[test]
    fn each_list_keeps_its_own_sort() {
        let (a, b) = (Page::Playlist("a".into()), Page::Library(LibTab::History));
        let mut f = ListFind::default();
        f.set_sort(&a, Sort::Title);
        f.set_sort(&b, Sort::Duration);
        assert_eq!((f.sort(&a), f.sort(&b)), (Sort::Title, Sort::Duration));
        // Albums keep their track order.
        f.set_sort(&Page::Album("c".into()), Sort::Title);
        assert_eq!(f.sort(&Page::Album("c".into())), Sort::Default);
        // Back to the default is forgotten, and only so many are remembered.
        f.set_sort(&a, Sort::Default);
        assert_eq!(f.sorts.len(), 1);
        for i in 0..KEEP_SORTS {
            f.set_sort(&Page::Playlist(i.to_string()), Sort::Album);
        }
        assert_eq!(f.sorts.len(), KEEP_SORTS);
        assert_eq!(f.sort(&b), Sort::Default);
    }

    #[test]
    fn a_list_coming_in_keeps_its_own_order() {
        let page = Page::Playlist("a".into());
        let mut f = ListFind::default();
        f.set_sort(&page, Sort::Title);
        f.hold(&page, true);
        assert_eq!(f.rows(&page, &songs()), None);
        assert!(!f.reordered(&page));
        f.hold(&page, false);
        assert_eq!(f.rows(&page, &songs()), Some(vec![0, 3, 1, 2]));
    }

    #[test]
    fn play_takes_the_whole_list_sorted() {
        let page = Page::Playlist("a".into());
        let mut f = ListFind { page: Some(page.clone()), query: "daft".into(), ..Default::default() };
        assert_eq!(f.sorted(&page, &songs()), None);
        f.set_sort(&page, Sort::TitleDesc);
        assert_eq!(f.sorted(&page, &songs()), Some(vec![2, 1, 3, 0]));
    }

    #[test]
    fn a_shift_click_picks_the_rows_between_on_screen() {
        // In the list's own order: the numbers between.
        assert_eq!(run(None, 4, 1), vec![1, 2, 3, 4]);
        // Sorted: the rows between as they're shown, not the numbers between.
        let shown = [5, 0, 3, 1, 4];
        assert_eq!(run(Some(&shown), 0, 4), vec![0, 3, 1, 4]);
        assert_eq!(run(Some(&shown), 1, 5), vec![5, 0, 3, 1]);
        // From a row no longer on show: just the one clicked.
        assert_eq!(run(Some(&shown), 2, 3), vec![3]);
    }

    fn card(title: &str) -> Card {
        Card { title: title.into(), subtitle: String::new(), thumbs: vec![], target: Target::Playlist(title.into()), round: false, links: vec![] }
    }

    #[test]
    fn library_grids_sort_and_follow_their_cards() {
        let page = Page::Library(LibTab::Playlists);
        let mut f = ListFind::default();
        let mut cards = vec![card("chill"), card("Été"), card("bangers")];
        assert_eq!(f.cards(&page, &cards), vec![0, 1, 2]);
        f.set_sort(&page, Sort::Title);
        assert_eq!(f.cards(&page, &cards), vec![2, 0, 1]);
        f.set_sort(&page, Sort::TitleDesc);
        assert_eq!(f.cards(&page, &cards), vec![1, 0, 2]);
        // A renamed playlist isn't left in its old place.
        cards[1].title = "Aardvark".into();
        assert_eq!(f.cards(&page, &cards), vec![0, 2, 1]);
    }
}
