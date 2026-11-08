//! Find in a song list: the box above a list, narrowing it to the songs that match.
//! Rows keep their numbers in the whole list, for selecting and removing. Explicit songs,
//! when hidden, are left out here too.

use egui::{Rect, Sense, Ui, Vec2, pos2, vec2};

use super::Page;
use super::widgets::{icon_at, named};
use crate::model::Track;
use crate::theme::{self, icon};

/// Lists shorter than this are easy enough to look through: no box.
const MIN_SONGS: usize = 12;
pub const BOX_W: f32 = 260.0;
const BOX_H: f32 = 38.0;

/// What's being looked for, in one list at a time: another list starts empty.
#[derive(Default)]
pub struct ListFind {
    page: Option<Page>,
    query: String,
    pub hide_explicit: bool,
}

impl ListFind {
    /// The rows of `tracks` to show on `page`; `None` when it isn't narrowed.
    pub fn rows(&self, page: &Page, tracks: &[Track]) -> Option<Vec<usize>> {
        let query = if self.page.as_ref() == Some(page) { self.query.to_lowercase() } else { String::new() };
        let words: Vec<&str> = query.split_whitespace().collect();
        if words.is_empty() && !(self.hide_explicit && tracks.iter().any(|t| t.explicit)) {
            return None;
        }
        Some(tracks.iter().enumerate().filter(|(_, t)| !(self.hide_explicit && t.explicit) && matches(t, &words)).map(|(i, _)| i).collect())
    }

    /// Something is looked for in `page`'s list.
    pub fn searching(&self, page: &Page) -> bool {
        self.page.as_ref() == Some(page) && !self.query.trim().is_empty()
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
        let edit = ui.put(
            inner,
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

/// Every word is somewhere in the song's title, artists or album.
fn matches(t: &Track, words: &[&str]) -> bool {
    let title = t.title.to_lowercase();
    let artists: Vec<String> = t.artists.iter().map(|a| a.name.to_lowercase()).collect();
    let album = t.album.as_ref().map(|a| a.name.to_lowercase());
    words.iter().all(|w| title.contains(w) || artists.iter().any(|a| a.contains(w)) || album.as_ref().is_some_and(|a| a.contains(w)))
}

#[cfg(test)]
mod tests {
    use super::{ListFind, Page, Track};
    use crate::model::Link;

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
        let f = ListFind { page: Some(Page::Playlist("a".into())), query: query.into(), hide_explicit: false };
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
        let f = ListFind { page: Some(Page::Playlist("a".into())), query: "daft".into(), hide_explicit: false };
        assert_eq!(f.rows(&Page::Playlist("b".into()), &songs()), None);
    }

    #[test]
    fn hidden_explicit_songs_are_left_out() {
        let mut tracks = songs();
        tracks[2].explicit = true;
        let page = Page::Playlist("a".into());
        let mut f = ListFind { page: Some(page.clone()), query: String::new(), hide_explicit: true };
        assert_eq!(f.rows(&page, &tracks), Some(vec![0, 1, 3]));
        f.query = "daft".into();
        assert_eq!(f.rows(&page, &tracks), Some(vec![0]));
        // Another list's search doesn't apply, but hiding still does.
        assert_eq!(f.rows(&Page::Playlist("b".into()), &tracks), Some(vec![0, 1, 3]));
        assert_eq!(f.rows(&page, &songs()[..2]), Some(vec![0]));
    }
}
