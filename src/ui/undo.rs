//! Undo for what has no button to take it back: songs out of the queue, the queue
//! cleared, songs into or out of your playlist, a like taken away, a playlist deleted. The toast
//! saying so has the Undo (Ctrl+Z too, while it shows). YouTube Music is only told once
//! the toast goes, the next one comes or the app closes: until then it's all here, and
//! Undo just puts it back as it was.

use crate::backend::{Req, UnlikeVersions};
use crate::model::{Card, Target, Track};

use super::lists::{LIKED_MUSIC, Taken, Unliked};
use super::{App, LibTab, Page, queue};

pub enum Undo {
    Queue(queue::Saved),
    /// The exact uploads being unrated, their prior liked state and rows, and the request.
    Unlike { targets: Vec<String>, liked: Vec<String>, was: Unliked, send: Box<Req> },
    /// Songs added to a playlist, shown in it already.
    Add { id: String, video_ids: Vec<String> },
    /// Rows taken out of a playlist.
    Removal { id: String, rows: Vec<Taken> },
    /// A playlist deleted, and its card with its place in the library.
    Delete { id: String, card: Option<(usize, Card)> },
}

impl Undo {
    /// What tells YouTube Music, now that it stands.
    fn into_req(self) -> Option<Req> {
        match self {
            Undo::Queue(_) => None,
            Undo::Unlike { send, .. } => Some(*send),
            Undo::Add { id, video_ids } => Some(Req::AddToPlaylist { playlist_id: id, video_ids }),
            Undo::Removal { id, rows } => Some(Req::RemoveFromPlaylist { items: items(&rows), playlist_id: id }),
            Undo::Delete { id, .. } => Some(Req::DeletePlaylist(id)),
        }
    }
}

/// Rows taken out, as YouTube Music is told of them.
fn items(rows: &[Taken]) -> Vec<(String, String)> {
    rows.iter().map(|t| (t.track.id.clone(), t.set_id.clone().unwrap_or_default())).collect()
}

impl App {
    pub(super) fn toast_undo(&mut self, msg: impl Into<String>, undo: Undo) {
        self.toast = None;
        self.settle();
        self.toast = Some((msg.into(), -1.0));
        self.undo = Some(undo);
    }

    fn take_undo_request(&mut self) -> Option<Req> {
        let undo = self.undo.take()?;
        if let Undo::Unlike { targets, .. } = &undo {
            self.confirm_unliked(targets);
        }
        undo.into_req()
    }

    /// The toast's Undo is gone (or about to be): what it could take back stands, and
    /// YouTube Music is told.
    pub(super) fn settle(&mut self) {
        let Some(req) = self.take_undo_request() else { return };
        // Hidden through loads until YouTube Music lists them gone: it's only told now.
        if let Req::RemoveFromPlaylist { playlist_id, items } = &req {
            self.lists.edits.remove(playlist_id, items);
        }
        self.backend.request(req);
    }

    /// The same, waited for: the app closing, or signing out or switching channel,
    /// can't leave it to go after.
    pub(super) fn settle_now(&mut self) {
        if self.undo.is_some() {
            self.toast = None;
        }
        if let Some(req) = self.take_undo_request() {
            self.backend.request_now(req);
        }
    }

    /// Likes of `tracks` taken away here at once, and from YouTube Music (by `send`) once
    /// the Undo is gone.
    pub(super) fn unlike_soon(&mut self, tracks: Vec<Track>, send: Req) {
        let mut targets: Vec<_> = tracks.iter().map(|t| t.id.clone()).collect();
        if let Req::LikeMany { versions, .. } = &send {
            targets.extend(versions.stand_ins.values().cloned());
        }
        if targets.iter().all(|id| self.unliking_soon(id)) {
            return;
        }
        self.rating(|id| targets.iter().any(|target| target == id));
        if !self.rating_settings.unlike_notices || self.mini.busy() {
            self.set_liked(&tracks, false);
            self.confirm_unliked(&targets);
            self.backend.request(send);
            return;
        }
        let liked = tracks.iter().filter(|t| self.liked.contains(&t.id)).map(|t| t.id.clone()).collect();
        let was = self.lists.unliking(&tracks);
        self.set_liked(&tracks, false);
        let msg = match send {
            Req::LikeMany { .. } => tr!("likes-removed", count = tracks.len()),
            _ => crate::tr!("status-removed-liked").to_owned(),
        };
        self.toast_undo(msg, Undo::Unlike { targets, liked, was, send: Box::new(send) });
    }

    fn confirm_unliked(&mut self, targets: &[String]) {
        for id in targets {
            self.ratings.confirm(id, crate::ytm::SongRating::Indifferent);
        }
    }

    pub(super) fn pending_unlikes(&self) -> &[String] {
        match &self.undo {
            Some(Undo::Unlike { targets, .. }) => targets,
            _ => &[],
        }
    }

    /// Clicking the same heart again before its removal was sent cancels the undoable edit.
    pub(super) fn unliking_soon(&self, id: &str) -> bool {
        self.pending_unlikes().iter().any(|target| target == id)
    }

    /// Songs about to be rated anew while an unlike waits for its Undo. That unlike doesn't
    /// go for them (sent after, it would take the new rating away), but it still goes, now,
    /// for the rest of what it was taking away: those stay unliked.
    pub(super) fn rating(&mut self, rated: impl Fn(&str) -> bool) {
        if !self.pending_unlikes().iter().any(|id| rated(id)) {
            return;
        }
        let Some(Undo::Unlike { targets, .. }) = self.undo.take() else { return };
        // Its toast's Undo has nothing left to take back.
        self.toast = None;
        let (keep, rest): (Vec<_>, Vec<_>) = targets.into_iter().partition(|id| rated(id));
        if rest.is_empty() {
            return;
        }
        self.confirm_unliked(&rest);
        let versions = UnlikeVersions { keep: keep.into_iter().collect(), ..Default::default() };
        self.backend.request(Req::LikeMany { video_ids: rest, like: false, versions });
    }

    /// Gone from here at once, and from YouTube Music once the Undo is: it can't bring
    /// a deleted playlist back.
    pub(super) fn delete_soon(&mut self, id: String) {
        let card = self.playlist_cards().and_then(|cards| {
            let i = cards.iter().position(|c| matches!(&c.target, Target::Playlist(p) if *p == id))?;
            Some((i, cards.remove(i)))
        });
        self.leave_playlist(&id);
        self.toast_undo(crate::tr!("status-playlist-deleted"), Undo::Delete { id, card });
    }

    /// Off a deleted playlist's page, and Back and Forward skip it.
    pub(super) fn leave_playlist(&mut self, id: &str) {
        let gone = Page::Playlist(id.to_owned());
        if self.page == gone {
            self.go(Page::Library(LibTab::Playlists));
        }
        self.back.retain(|p| *p != gone);
        self.back.dedup();
        self.forward.retain(|p| *p != gone);
        self.forward.dedup();
    }

    /// Taking songs out of playlist `id` failed: they come back with a reload, but not
    /// ones still waiting for their Undo.
    pub(super) fn removal_failed(&mut self, id: &str) {
        self.lists.edits.removal_failed(id);
        if let Some(Undo::Removal { id: p, rows }) = &self.undo && p == id {
            self.lists.edits.remove(id, &items(rows));
        }
    }

    /// Settled before playlist `id` changes order: rows put back go by the rows near them,
    /// and songs added go to YouTube Music before they're moved.
    pub(super) fn settle_edit(&mut self, id: &str) {
        if matches!(&self.undo, Some(Undo::Removal { id: p, .. } | Undo::Add { id: p, .. }) if p == id) {
            self.settle();
        }
    }

    /// Playlist `id` waits to be deleted: a library load still listing it leaves it out.
    pub(super) fn deleting_soon(&self, id: &str) -> bool {
        matches!(&self.undo, Some(Undo::Delete { id: d, .. }) if d == id)
    }

    pub(super) fn undo(&mut self) {
        let Some(undo) = self.undo.take() else { return };
        match undo {
            Undo::Queue(_) if self.jam.guest() => self.toast(crate::tr!("jam-host-only-queue")),
            Undo::Queue(saved) => {
                let source = &self.queue_source;
                let Some(go_on) = self.queue.restore(saved, |id| matches!(source, Some(Target::Playlist(p)) if p == id)) else {
                    return self.toast(crate::tr!("status-queue-changed"));
                };
                // Its load ended or gave way to another meanwhile: nothing more comes.
                let live = self.queue.fill.as_ref().is_some_and(|f| self.receiving(&f.id) && self.opened(&f.id).is_some_and(|o| o.load == f.load));
                if !live {
                    self.queue.fill = None;
                }
                self.toast = None;
                if go_on {
                    self.next();
                } else {
                    self.prefetch_next();
                }
            }
            Undo::Unlike { liked, was, .. } => {
                self.toast = None;
                self.liked.extend(liked);
                self.lists.unlike_undone(was);
                self.after_rows_changed(LIKED_MUSIC);
            }
            Undo::Add { id, video_ids } => {
                self.toast = None;
                // Never sent: out again, as if YouTube Music hadn't taken them.
                self.lists.add_failed(&id, &video_ids);
                self.after_rows_changed(&id);
            }
            Undo::Removal { id, rows } => {
                self.toast = None;
                self.lists.put_back(&id, rows);
                self.after_rows_changed(&id);
            }
            Undo::Delete { card, .. } => {
                self.toast = None;
                match (self.playlist_cards(), card) {
                    (Some(cards), Some((i, card))) => cards.insert(i.min(cards.len()), card),
                    // Not among the cards shown: it comes back with the library.
                    _ => self.request_library(),
                }
            }
        }
    }

    /// The playlists the sidebar and Library show: the library's, or last launch's.
    fn playlist_cards(&mut self) -> Option<&mut Vec<Card>> {
        self.library.as_mut().map(|l| &mut l.playlists).or(self.sidebar_cache.as_mut())
    }

    /// Rows came back into or went out of `list`: its count, and picks (by row number)
    /// on its page.
    fn after_rows_changed(&mut self, list: &str) {
        self.sync_card_count(list);
        if self.page_list() == Some(list) {
            self.selection.clear();
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::lists::Lists;

    fn track(id: &str) -> Track {
        Track { id: id.into(), title: String::new(), artists: vec![], ..Default::default() }
    }

    #[test]
    fn an_add_is_sent_as_it_was_made_once_its_undo_is_gone() {
        let undo = Undo::Add { id: "PL1".into(), video_ids: vec!["a".into(), "b".into()] };
        assert!(matches!(undo.into_req(), Some(Req::AddToPlaylist { playlist_id, video_ids }) if playlist_id == "PL1" && video_ids == ["a", "b"]));
    }

    #[test]
    fn an_add_undone_leaves_the_playlist_as_it_was() {
        let mut lists = Lists::default();
        // Already in it from before, and added again: Undo takes back only the new add.
        lists.added("PL1", &[track("a")]);
        lists.edits.confirm_add("PL1", &["a".to_owned()]);
        lists.added("PL1", &[track("a"), track("b")]);
        // What Undo does with an add.
        lists.add_failed("PL1", &["a".to_owned(), "b".to_owned()]);
        assert!(lists.edits.pending("PL1"), "the earlier add of a still stands");
        let mut fresh = Lists::default();
        fresh.added("PL1", &[track("b")]);
        fresh.add_failed("PL1", &["b".to_owned()]);
        assert!(!fresh.edits.pending("PL1"), "nothing left waiting once its only add is undone");
    }
}
