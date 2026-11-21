//! What clicks and keys ask for: the actions pages queue up, and keyboard shortcuts.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::backend::{Req, UnlikeVersions};
use crate::model::{Target, Track};

use super::lists::{self, LIBRARY_SONGS, LIKED_MUSIC};
use super::queue::QueueFill;
use super::undo::Undo;
use super::{Action, App, Dialog, LibTab, NpTab, Page, Unlike, jam, palette, transfer};

impl App {
    pub(super) fn heard_like_action(&self, like: bool) -> Option<Action> {
        Some(Action::LikeHeard { like, video_id: self.playing_id()?.to_owned(), generation: self.generation })
    }

    fn request_unlike(&mut self, unlike: Unlike) {
        if !self.logged_in {
            self.toast(crate::tr!("status-sign-in-like"));
            return;
        }
        match unlike {
            Unlike::Track(track) => {
                if let Some(id) = track.stands_for.clone() {
                    let video = Track { id, video: true, stands_for: None, ..track.clone() };
                    self.unlike_versions(track, video);
                    return;
                }
                let send = Req::Like { video_id: track.id.clone(), like: false };
                self.unlike_soon(vec![track], send);
            }
            Unlike::Versions(song, video) => self.unlike_versions(song, video),
            Unlike::Selection(tracks) => self.set_many_likes(tracks, false),
            Unlike::Rating(track) => {
                let send = Req::RemoveRating { video_id: track.id.clone(), was_disliked: false };
                self.unlike_soon(vec![track], send);
            }
        }
    }

    fn unlike_versions(&mut self, song: Track, video: Track) {
        let send = Req::UnlikeBoth { song: song.id.clone(), video: video.id.clone() };
        self.unlike_soon(vec![song, video], send);
    }

    fn set_many_likes(&mut self, tracks: Vec<Track>, like: bool) {
        if !self.logged_in {
            self.toast(crate::tr!("status-sign-in-like"));
            return;
        }
        // Liking songs whose unlike waits: those stay liked, so it doesn't go.
        if like && tracks.iter().any(|t| self.unliking_soon(&t.id)) {
            self.undo();
        }
        let tracks: Vec<Track> = tracks.into_iter().filter(|t| self.liked.contains(&t.id) != like).collect();
        if tracks.is_empty() { return; }
        let stand_ins = tracks.iter().filter(|_| !like).filter_map(|t| Some((t.id.clone(), t.stands_for.clone()?))).collect();
        let versions = UnlikeVersions { stand_ins, ..Default::default() };
        let send = Req::LikeMany { video_ids: tracks.iter().map(|t| t.id.clone()).collect(), like, versions };
        if !like {
            return self.unlike_soon(tracks, send);
        }
        self.set_liked(&tracks, true);
        if tracks.len() > 10 {
            self.toast(crate::tr!("status-liking", count = tracks.len()));
        }
        self.backend.request(send);
    }

    pub(super) fn apply(&mut self, a: Action) {
        // In someone else's jam, playback changes become requests to the host.
        let a = if self.jam.guest() {
            match jam::guest_action(self, a) {
                Some(a) => a,
                None => return,
            }
        } else {
            a
        };
        match a {
            // Clicking the song that's already playing pauses/resumes it, not restart it.
            Action::Play(tracks, i) | Action::PlayFound(tracks, i) if tracks.get(i).is_some_and(|t| self.current_id() == Some(t.id.as_str())) && self.resume_at.is_none() => {
                self.toggle()
            }
            Action::PlayFound(tracks, i) => {
                self.queue_source = self.page_source();
                self.play_list(tracks, i);
            }
            Action::Play(tracks, i) => {
                self.queue_source = self.page_source();
                let fill = self.page_fill(&tracks);
                self.play_list_filling(tracks, i, fill);
            }
            Action::Shuffle(tracks) => {
                self.queue_source = self.page_source();
                let fill = self.page_fill(&tracks);
                self.play_shuffled(tracks, fill);
            }
            Action::TogglePlay => self.toggle(),
            Action::TransferPick(t) => transfer::use_pick(self, t),
            // Kept already: at once. A playlist that isn't starts with its first page.
            Action::PlayCollection(Target::Album(id) | Target::Playlist(id), shuffle) if self.opened(&id).is_some() => self.play_opened(id, shuffle),
            Action::PlayCollection(target, shuffle) => {
                self.toast(crate::tr!("status-starting-playback"));
                self.play_pending = Some((target.clone(), shuffle));
                match target {
                    Target::Playlist(id) => self.load_list(&id, false, false),
                    target => self.backend.request(Req::PlayCollection { target }),
                }
            }
            Action::Open(Target::Song(t)) | Action::Radio(t) if self.explicit_hidden(&t) => {}
            // Radio from the song playing: it goes on playing, and the radio replaces what
            // was to come, as YouTube Music's Start radio does. Clicking its card (which
            // starts a song's radio) does the same rather than starting it over.
            Action::Open(Target::Song(t)) | Action::Radio(t) if self.current_id() == Some(t.id.as_str()) || self.playing_id() == Some(t.id.as_str()) => {
                let Some(cur) = self.queue.keep_current() else { return };
                self.queue_source = None;
                if !self.radio_pending {
                    self.radio_pending = true;
                    self.backend.request(Req::Radio { song_of: self.song_of(&cur), seed: cur.id });
                }
            }
            Action::Open(Target::Song(t)) | Action::Radio(t) => {
                self.queue_source = None;
                self.play_list(vec![t.clone()], 0);
                if !self.radio_pending {
                    self.radio_pending = true;
                    self.backend.request(Req::Radio { song_of: self.song_of(&t), seed: t.id });
                }
            }
            Action::Open(Target::Album(id)) => self.go(Page::Album(id)),
            // Library › Songs is a tab, not a playlist page.
            Action::Open(Target::Playlist(id)) if id == LIBRARY_SONGS => self.go(Page::Library(LibTab::Saved)),
            Action::Open(Target::Playlist(id)) => self.go(Page::Playlist(id)),
            Action::Open(Target::Artist(id)) => self.go(Page::Artist(id)),
            Action::PlayNext(t) | Action::Enqueue(t) | Action::QueueInsert { track: t, .. } if self.explicit_hidden(&t) => {}
            Action::PlayNext(t) => {
                let start = self.queue.play_next(t);
                self.toast(crate::tr!("status-playing-next"));
                if start {
                    self.play_current();
                } else {
                    self.prefetch_next();
                }
            }
            Action::Enqueue(t) => {
                let start = self.queue.enqueue(t);
                self.toast(crate::tr!("status-added-queue"));
                if start {
                    self.play_current();
                }
            }
            Action::Go(p) => self.go(p),
            Action::QueueJump(i) => {
                // Its row may be gone: the queue changed since it was clicked.
                if i >= self.queue.tracks.len() {
                    return;
                }
                self.play_pending = None;
                self.queue.index = Some(i);
                self.queue.drop_explicit();
                self.play_current();
            }
            Action::QueueRemove(i) => {
                if let Some(saved) = self.queue.remove(i) {
                    self.toast_undo(crate::tr!("status-removed-queue"), Undo::Queue(saved));
                    self.prefetch_next();
                }
            }
            Action::QueueClear => {
                if let Some(saved) = self.queue.clear() {
                    self.toast_undo(crate::tr!("status-cleared-queue"), Undo::Queue(saved));
                }
            }
            Action::Undo => self.undo(),
            Action::QueueInsert { gap, track } => {
                self.queue.insert_at(gap, track);
                self.toast(crate::tr!("status-added-queue"));
                self.prefetch_next();
            }
            Action::QueueMove { from, gap, id } => {
                if self.queue.tracks.get(from).is_some_and(|t| t.id == id) && self.queue.move_track(from, gap) {
                    self.prefetch_next();
                }
            }
            Action::ToggleVideo => {
                self.show_video = !self.show_video;
                // Its song is playing: a video only stays in time with its own audio. A
                // song's music video goes with Video off; Video on leaves a song be.
                if self.show_video == self.current().is_some_and(Track::has_video) {
                    self.play_listed_upload();
                } else if !self.show_video && self.current().is_some_and(|t| t.music_video) && self.play_songs && !self.jam.active() {
                    self.play_song_version();
                }
            }
            // Picked on a song's switch, Video or Song sticks for videos too, as their own
            // switch does.
            Action::SongVideo(video) => {
                self.show_video = video;
                if video {
                    self.play_music_video();
                } else {
                    self.play_listed_upload();
                }
            }
            Action::Seek(secs) => {
                self.seek_to(Duration::from_secs_f32(secs.max(0.0)));
                self.lyric_view.resume();
            }
            Action::Like(t, false) => self.request_unlike(Unlike::Track(t)),
            Action::Like(t, true) => {
                if !self.logged_in {
                    self.toast(crate::tr!("status-sign-in-like"));
                    return;
                }
                // Liked again while its unlike waits: that unlike just doesn't go.
                if self.unliking_soon(&t.id) {
                    return self.undo();
                }
                // Optimistic: update the like state and the cached Liked songs list now.
                self.set_liked(std::slice::from_ref(&t), true);
                self.backend.request(Req::Like { video_id: t.id, like: true });
            }
            // The heart of what's playing stands for both a music video and the song version
            // standing in for it: taking it away takes both likes, whichever are there.
            Action::LikeHeard { like, video_id, generation } => {
                if generation != self.generation || self.playing_id() != Some(video_id.as_str()) {
                    return;
                }
                let Some(heard) = self.playing_track().cloned() else { return };
                let video = self.playing_as().and(self.current()).cloned();
                // Liked again while its unlike waits: both come back as they were.
                if like && [Some(&heard), video.as_ref()].into_iter().flatten().any(|t| self.unliking_soon(&t.id)) {
                    return self.undo();
                }
                match video.filter(|_| !like) {
                    Some(video) => self.request_unlike(Unlike::Versions(heard, video)),
                    _ => self.apply(Action::Like(heard, like)),
                }
            }
            Action::LikeMany(tracks, false) => self.request_unlike(Unlike::Selection(tracks)),
            Action::LikeMany(tracks, true) => self.set_many_likes(tracks, true),
            Action::Dislike(t) => {
                if !self.logged_in { self.toast(crate::tr!("status-sign-in-rate")); return; }
                let id = Some(t.id.as_str());
                let skip_generation = (!self.jam.guest() && (self.current_id() == id || self.playing_id() == id)).then_some(self.generation);
                self.rating(|id| id == t.id);
                self.set_liked(std::slice::from_ref(&t), false);
                self.backend.request(Req::Dislike { video_id: t.id, skip_generation });
            }
            Action::RemoveDislike(video_id) => {
                if !self.logged_in { self.toast(crate::tr!("status-sign-in-rate")); return; }
                self.rating(|id| id == video_id);
                self.backend.request(Req::RemoveRating { video_id, was_disliked: true });
            }
            Action::RemoveLike(track) => self.request_unlike(Unlike::Rating(track)),
            Action::Share(target) => {
                self.share_pending = Some(share_url(&target));
                self.toast(crate::tr!("status-link-copied"));
            }
            Action::Pin(id, on) => {
                self.pinned.retain(|p| *p != id);
                if on {
                    self.pinned.insert(0, id);
                }
            }
            Action::Download(track) => {
                self.toast(crate::tr!("status-downloading", title = track.title.as_str()));
                // The song version playing for a music video: that video plays the file too.
                let for_video = if self.playing_as().is_some_and(|s| s.id == track.id && !s.has_video()) { self.current_id().map(str::to_owned) } else { None };
                self.backend.request(Req::Download { track, for_video });
            }
            Action::DownloadMany(target, title, tracks) => {
                self.backend.request(Req::DownloadMany { target, title, tracks, confirmed: false });
            }
            Action::CancelDownload(key) => self.backend.cancel_downloads(&key),
            Action::AddToPlaylist(tracks) => {
                if self.logged_in { self.dialog = Some(Dialog::PickPlaylist(tracks)); }
                else { self.toast(crate::tr!("status-sign-in-edit")); }
            }
            Action::RemoveFromPlaylist(mut rows) => {
                let Some(c) = self.current_collection().filter(|c| c.owned && !c.is_album) else { return };
                rows.sort_unstable();
                rows.dedup();
                rows.retain(|&i| c.set_ids.get(i).is_some_and(Option::is_some));
                if rows.is_empty() {
                    self.toast(crate::tr!("status-playlist-reload"));
                    return;
                }
                let items: Vec<(String, String)> = rows.iter().map(|&i| (c.tracks[i].id.clone(), c.set_ids[i].clone().unwrap_or_default())).collect();
                let playlist = c.id.clone();
                let taken = lists::taken(c, &rows);
                // Optimistic: drop the rows now. YouTube Music is told once the Undo is gone,
                // and the playlist reloads when it confirms.
                c.remove_rows(&rows);
                log::debug!("list {playlist}: removing {} entries", items.len());
                self.lists.edits.remove(&playlist, &items);
                self.sync_card_count(&playlist);
                self.selection.clear();
                self.toast_undo(crate::tr!("status-removed-playlist"), Undo::Removal { id: playlist, rows: taken });
            }
            Action::PlaylistMove { from, gap, id } => {
                // A load that started during the drag would answer with the old order.
                if self.page_list().is_some_and(|p| self.lists.loading(p)) {
                    return self.toast(crate::tr!("status-playlist-loading"));
                }
                if let Some(list) = self.page_list().map(str::to_owned) {
                    self.settle_removal(&list);
                }
                let Some(c) = self.current_collection().filter(|c| c.owned && !c.is_album && c.tracks.get(from).is_some_and(|t| t.id == id)) else { return };
                let playlist_id = c.id.clone();
                match c.move_row(from, gap) {
                    Some((set_id, before)) => {
                        // Picked rows go by number.
                        self.selection.clear();
                        let move_id = self.lists.move_sent(&playlist_id);
                        self.backend.request(Req::MoveInPlaylist { playlist_id, move_id, set_id, before });
                    }
                    None => self.toast(crate::tr!("status-playlist-reload")),
                }
            }
            Action::NewPlaylist(tracks) => {
                if self.logged_in { self.dialog = Some(Dialog::NewPlaylist { title: String::new(), tracks }); }
                else { self.toast(crate::tr!("status-sign-in-create")); }
            }
            Action::SortPlaylist(id, actions) => {
                self.sorting.insert(id.clone(), super::lists::SortWait::new(actions.clone()));
                self.backend.request(Req::SortPlaylist { id, actions });
            }
            Action::EditPlaylist => {
                if let Some(c) = self.current_collection().filter(|c| !c.is_album) {
                    self.dialog = Some(Dialog::EditPlaylist {
                        id: c.id.clone(), title: c.title.clone(), description: c.description.clone().unwrap_or_default(), privacy: None,
                    });
                }
            }
            Action::Collaborate => {
                if let Some(id) = self.current_collection().filter(|c| !c.is_album && c.owned).map(|c| c.id.clone()) {
                    self.backend.request(Req::Collaboration(id.clone()));
                    self.dialog = Some(Dialog::Collaborate { id, info: None, busy: true, confirm_off: false });
                }
            }
            Action::DeleteUpload(t) => {
                if let Some(entity) = crate::ytm::upload_entity(&t.id) {
                    self.dialog = Some(Dialog::ConfirmDeleteUpload { entity, title: t.title });
                }
            }
            Action::DeletePlaylist => {
                if let Some(c) = self.current_collection().filter(|c| !c.is_album) {
                    self.dialog = Some(Dialog::ConfirmDelete { id: c.id.clone(), title: c.title.clone() });
                }
            }
            Action::SaveCollection(id, save) => {
                if self.logged_in { self.backend.request(Req::SaveCollection { id, save }); }
                else { self.toast(crate::tr!("status-sign-in-save")); }
            }
            Action::Subscribe(on) => {
                if !self.logged_in {
                    self.toast(crate::tr!("status-sign-in-subscribe"));
                } else if let Some(a) = self.artist.as_mut().filter(|a| a.subscribed != on)
                    && let Some(channel_id) = a.channel_id.clone()
                    // One at a time, and not queued: the pill is greyed out while one runs.
                    && self.subscriptions.start(&channel_id, on)
                {
                    // Shown at once; put back if YouTube Music refuses it.
                    a.subscribed = on;
                    self.backend.request(Req::Subscribe { channel_id, on });
                }
            }
            Action::HomeChip(params) => {
                self.home_loading = true;
                if let Some(h) = &mut self.home {
                    h.shelves.clear();
                    for c in &mut h.chips {
                        c.selected = false;
                    }
                }
                self.home_mood = params.is_some();
                self.backend.request(Req::Home(params));
            }
            Action::OpenNowPlaying(open) => {
                if self.current().is_some() {
                    self.np_open = open;
                }
            }
            Action::Mini(on) => self.mini.request(on),
        }
    }

    /// What a play from this page plays from: play buttons and rows live on the page they play from.
    fn page_source(&self) -> Option<Target> {
        match &self.page {
            Page::Album(id) => Some(Target::Album(id.clone())),
            Page::Playlist(id) => Some(Target::Playlist(id.clone())),
            Page::Library(LibTab::Songs) => Some(Target::Playlist(LIKED_MUSIC.into())),
            Page::Library(LibTab::Saved) => Some(Target::Playlist(LIBRARY_SONGS.into())),
            Page::Artist(id) => Some(Target::Artist(id.clone())),
            _ => None,
        }
    }

    /// `tracks` are all of the playlist on screen that's still coming in: the rest follows.
    fn page_fill(&self, tracks: &[Track]) -> Option<QueueFill> {
        let id = self.page_list().filter(|id| self.receiving(id))?;
        let o = self.opened(id).filter(|o| o.c.tracks.len() == tracks.len())?;
        Some(QueueFill { id: id.to_owned(), load: o.load, ran_out: false })
    }

    pub(super) fn shortcuts(&mut self, ctx: &egui::Context) {
        // Space or the arrows are being picked as a shortcut, not pressed.
        if self.hotkeys.capturing() {
            return;
        }
        palette::shortcut(self, ctx);
        let typing = ctx.egui_wants_keyboard_input();
        // In a text box, Ctrl+arrows move the cursor by words (and lines) instead; on a
        // focused button or slider they still skip and change the volume.
        let editing = ctx.text_edit_focused();
        let backup_prompt_open = self.backup_prompt == super::BackupPrompt::Open && self.dialog.is_none();
        let i = ctx.input(|i| {
            // Once per press, and not with Ctrl+F and the like, which mean something else.
            let k = |key: egui::Key| {
                !typing
                    && i.events.iter().any(|e| matches!(e, egui::Event::Key { key: which, pressed: true, repeat: false, modifiers, .. } if *which == key &&!modifiers.ctrl && !modifiers.alt && !modifiers.command))
            };
            let ctrl = |key| !editing && i.modifiers.ctrl && i.key_pressed(key);
            (
                k(egui::Key::Space),
                (!backup_prompt_open && i.pointer.button_pressed(egui::PointerButton::Extra1))
                    || (i.modifiers.alt && i.key_pressed(egui::Key::ArrowLeft)),
                (!backup_prompt_open && i.pointer.button_pressed(egui::PointerButton::Extra2))
                    || (i.modifiers.alt && i.key_pressed(egui::Key::ArrowRight)),
                ctrl(egui::Key::ArrowRight),
                ctrl(egui::Key::ArrowLeft),
                k(egui::Key::L),
                k(egui::Key::F) || k(egui::Key::F11),
                i.key_pressed(egui::Key::Escape),
                ctrl(egui::Key::ArrowUp),
                ctrl(egui::Key::ArrowDown),
                // Once per press: holding it doesn't flip back and forth.
                i.events.iter().any(|e| matches!(e, egui::Event::Key { key: egui::Key::M, pressed: true, repeat: false, modifiers, .. } if modifiers.ctrl)),
                // Once per press too: holding it doesn't fire a request every repeat.
                i.events.iter().any(|e| {
                    matches!(e, egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. }
                        if !editing && (*key == egui::Key::F5 || (*key == egui::Key::R && modifiers.ctrl)))
                }),
                // Quits for real, with Close to tray on too: GNOME has no tray to quit from (#143).
                i.modifiers.command && i.key_pressed(egui::Key::Q),
                // The toast's Undo. With Shift it would be redo, which there isn't.
                !editing && i.modifiers.command && !i.modifiers.shift && i.key_pressed(egui::Key::Z),
            )
        });
        let (space, back, fwd, next, prev, lyr, full, esc, vol_up, vol_down, mini, refresh, quit, undo) = i;
        if undo && self.undo.is_some() {
            self.actions.push(Action::Undo);
        }
        if quit {
            self.quit(ctx);
        }
        if mini {
            self.actions.push(Action::Mini(!self.mini.target()));
        }
        // The mini player has no pages or now playing to go to, nor does a window on
        // its way back until it's there.
        let (back, fwd, lyr, full, esc, refresh) =
            if self.mini.busy() { (false, false, false, false, false, false) } else { (back, fwd, lyr, full, esc, refresh) };
        if refresh {
            self.refresh();
        }
        if space {
            self.toggle();
        }
        if back {
            self.nav_back(ctx);
        }
        if fwd {
            self.nav_forward();
        }
        if next {
            self.next();
        }
        if prev {
            self.prev();
        }
        if lyr {
            self.toggle_lyrics();
        }
        if full && self.np_open {
            self.toggle_fullscreen(ctx);
        }
        // A menu or dialog that's open takes this Esc for itself.
        if esc && !egui::Popup::is_any_open(ctx) && ctx.memory(|m| m.top_modal_layer()).is_none() {
            if self.row_drag.take().is_some() {
                // Just the drag.
            } else if self.fullscreen {
                self.set_fullscreen(ctx, false);
            } else {
                self.np_open = false;
            }
        }
        if vol_up || vol_down {
            self.nudge_volume(vol_up);
        }
    }

    /// Now playing with its lyrics, or back out of it (L).
    pub(super) fn toggle_lyrics(&mut self) {
        if self.current().is_some() {
            self.np_open = !self.np_open;
            self.np_tab = NpTab::Lyrics;
        }
    }

    /// Full screen in now playing, or out of it (F).
    pub(super) fn toggle_fullscreen(&mut self, ctx: &egui::Context) {
        // With a video on show, full screen is the video's, as F is on YouTube.
        if self.fullscreen {
            self.set_fullscreen(ctx, false);
        } else if self.shown_video().is_some() {
            self.set_video_fullscreen(ctx, true);
        } else {
            self.set_fullscreen(ctx, true);
        }
    }

    /// In now playing the queue is its Up next panel; elsewhere, the side panel.
    pub(super) fn queue_shown(&self) -> bool {
        if self.np_open { self.np_tab == NpTab::UpNext } else { self.queue_open }
    }

    pub(super) fn toggle_queue(&mut self) {
        if self.np_open {
            self.np_tab = if self.queue_shown() { NpTab::Art } else { NpTab::UpNext };
        } else {
            self.queue_open = !self.queue_open;
        }
    }

    pub(super) fn nudge_volume(&self, up: bool) {
        let v = (self.audio.volume() + if up { 0.05 } else { -0.05 }).clamp(0.0, 1.0);
        self.audio.set_volume(v);
    }

    /// Into the mini player or back. Full screen ends first, a frame ahead.
    pub(super) fn set_mini(&mut self, ctx: &egui::Context, on: bool, now: f64) {
        if on == self.mini.active() {
            return;
        }
        if on && self.fullscreen {
            self.set_fullscreen(ctx, false);
            self.mini.request(true);
            return;
        }
        if on && !self.pending_unlikes().is_empty() {
            self.toast = None;
            self.settle();
        }
        // With a video on show, the mini player is that video (picture in picture).
        let video = on && self.np_open && self.shown_video().is_some();
        self.mini.toggle(ctx, now, video);
    }

    /// Full screen with just the video, as on YouTube. Leaving it is leaving full screen.
    pub(super) fn set_video_fullscreen(&mut self, ctx: &egui::Context, on: bool) {
        self.set_fullscreen(ctx, on);
        self.video_full = on;
        super::nowplaying::wake_video_controls(ctx);
    }

    pub(super) fn set_fullscreen(&mut self, ctx: &egui::Context, on: bool) {
        self.fullscreen = on;
        self.video_full = false;
        // A seek dragged when the view changed under it ends, and the cover player starts out shown.
        self.seek_drag = None;
        super::nowplaying::wake_cover_player(ctx);
        // Windows: in one step, without winit's unthemed-frame flash.
        if !crate::fullscreen::set(on) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
        }
        ctx.request_repaint();
    }
}

/// Whether a link to it opens for others: an uploaded album's or artist's page opens
/// only for the account that uploaded it.
pub(super) fn shareable(target: &Target) -> bool {
    !matches!(target, Target::Album(id) | Target::Artist(id) if crate::ytm::uploaded(id))
}

fn share_url(target: &Target) -> String {
    match target {
        Target::Song(t) => format!("https://music.youtube.com/watch?v={}", t.id),
        Target::Playlist(id) => format!("https://music.youtube.com/playlist?list={}", crate::ytm::list_id(id)),
        Target::Album(id) => format!("https://music.youtube.com/browse/{id}"),
        Target::Artist(id) => format!("https://music.youtube.com/channel/{id}"),
    }
}

/// Subscribing and unsubscribing from artist pages: one at a time, and what each settled,
/// so an artist page asked for before a change can't undo it.
#[derive(Default)]
pub(super) struct Subscriptions {
    /// The one under way: its channel, and whether it subscribes.
    running: Option<(String, bool)>,
    /// Each channel's last change: where it stands, and when YouTube Music answered.
    settled: HashMap<String, (bool, Instant)>,
}

impl Subscriptions {
    pub(super) fn busy(&self) -> bool {
        self.running.is_some()
    }

    /// Starts one, unless another is under way.
    pub(super) fn start(&mut self, channel: &str, on: bool) -> bool {
        if self.busy() {
            return false;
        }
        self.running = Some((channel.to_owned(), on));
        true
    }

    /// Its answer, which lets the next one go: where the channel now stands.
    pub(super) fn finish(&mut self, channel: &str, on: bool, ok: bool) -> bool {
        self.running = None;
        let now = on == ok;
        self.settled.insert(channel.to_owned(), (now, Instant::now()));
        now
    }

    /// Whether an artist page asked for at `asked` shows the channel subscribed: as the
    /// page says, unless a change under way, or answered since, says otherwise.
    pub(super) fn shown(&self, channel: &str, says: bool, asked: Instant) -> bool {
        match (&self.running, self.settled.get(channel)) {
            (Some((c, on)), _) if c == channel => *on,
            (_, Some(&(on, at))) if asked < at => on,
            _ => says,
        }
    }
}

#[cfg(test)]
mod subscription_tests {
    use super::*;

    #[test]
    fn one_at_a_time() {
        let mut s = Subscriptions::default();
        assert!(s.start("UCa", true));
        assert!(s.busy());
        assert!(!s.start("UCa", false), "a click while one runs does nothing");
        assert!(!s.start("UCb", true));
        assert!(s.finish("UCa", true, true));
        assert!(!s.busy());
        assert!(s.start("UCb", true));
        // A failure lets the next one go too, and puts the old state back.
        assert!(!s.finish("UCb", true, false));
        assert!(!s.busy());
    }

    #[test]
    fn earlier_pages_dont_undo_a_change() {
        let mut s = Subscriptions::default();
        let before = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        assert!(s.shown("UCa", true, before), "nothing changed here: the page says");
        s.start("UCa", true);
        // Under way: what was asked, whenever the page was fetched.
        assert!(s.shown("UCa", false, before));
        assert!(s.shown("UCa", false, Instant::now()));
        assert!(!s.shown("UCb", false, before), "another channel: the page says");
        s.finish("UCa", true, true);
        assert!(s.shown("UCa", false, before), "asked before the answer: the answer stands");
        let after = Instant::now() + Duration::from_secs(1);
        assert!(!s.shown("UCa", false, after), "asked after it: the page says");
        // A refused unsubscribe leaves it subscribed, over a page from before it.
        s.start("UCa", false);
        s.finish("UCa", false, false);
        assert!(s.shown("UCa", false, before));
    }
}

#[cfg(test)]
mod share_tests {
    use super::*;

    #[test]
    fn shares_youtube_music_urls() {
        let song = Track { id: "abc123".into(), title: String::new(), artists: vec![], ..Default::default() };
        assert_eq!(share_url(&Target::Song(song)), "https://music.youtube.com/watch?v=abc123");
        assert_eq!(share_url(&Target::Playlist("PL123".into())), "https://music.youtube.com/playlist?list=PL123");
        // A podcast show is shared as its playlist.
        assert_eq!(share_url(&Target::Playlist("MPSPPL123".into())), "https://music.youtube.com/playlist?list=PL123");
        assert_eq!(share_url(&Target::Album("MPRE123".into())), "https://music.youtube.com/browse/MPRE123");
        assert_eq!(share_url(&Target::Artist("UC123".into())), "https://music.youtube.com/channel/UC123");
    }

    #[test]
    fn uploaded_albums_and_artists_have_no_link_to_share() {
        assert!(!shareable(&Target::Artist("FEmusic_library_privately_owned_artist_detaila_po_CICr2crg7OWpchIIY29sZHBsYXk".into())));
        assert!(!shareable(&Target::Album("FEmusic_library_privately_owned_release_detailb_po_CICr2crg7OWpchIOZ2hvc3Qgc3Rvcmllcw".into())));
        assert!(shareable(&Target::Artist("UC123".into())));
        assert!(shareable(&Target::Album("MPRE123".into())));
    }
}
