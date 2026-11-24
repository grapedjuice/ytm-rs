//! Dialogs: new and edited playlists, collaborators, confirmations, and their parts.

use egui::{Color32, Ui};

use crate::backend::Req;
use crate::model::Target;
use crate::theme;
use crate::ytm::Duplicates;

use super::{Action, App, Dialog, Page, BackupPrompt, notes, pages, widgets};

impl App {
    pub(super) fn draw_dialog(&mut self, ctx: &egui::Context) {
        if self.dialog.is_none() && self.backup_prompt == BackupPrompt::Open {
            if self.backup_stream {
                self.backup_prompt = BackupPrompt::Hidden;
            } else {
                self.backup_stream_dialog(ctx);
                return;
            }
        }
        if let Some(Dialog::ConfirmClearCache) = &self.dialog {
            let body = match self.storage.known() {
                Some(n) => crate::tr!("dialog-cache-freed-size", size = crate::cache::size_text(n)),
                None => crate::tr!("dialog-cache-frees-space").to_owned(),
            };
            match confirm(ctx, crate::tr!("dialog-clear-cache-title"), &body, crate::tr!("settings-clear"), crate::tr!("settings-clear-cache")) {
                Some(true) => {
                    self.clear_cache(ctx);
                    self.dialog = None;
                }
                Some(false) => self.dialog = None,
                None => {}
            }
            return;
        }
        if let Some(Dialog::ConfirmDelete { id, title }) = &self.dialog {
            let (id, title) = (id.clone(), title.clone());
            let body = crate::tr!("dialog-delete-playlist-body", title = title.as_str());
            match confirm(ctx, crate::tr!("dialog-delete-playlist-title"), &body, crate::tr!("ui-delete"), crate::tr!("dialog-delete-playlist")) {
                Some(true) => {
                    self.dialog = None;
                    self.delete_soon(id);
                }
                Some(false) => self.dialog = None,
                None => {}
            }
            return;
        }
        if let Some(Dialog::ConfirmDeleteUpload { entity, title }) = &self.dialog {
            let (entity, title) = (entity.clone(), title.clone());
            let body = crate::tr!("dialog-delete-upload-body", title = title.as_str());
            match confirm(ctx, crate::tr!("dialog-delete-upload-title"), &body, crate::tr!("ui-delete"), crate::tr!("dialog-delete-upload")) {
                Some(true) => {
                    self.dialog = None;
                    self.backend.request(Req::DeleteUpload { entity, title });
                }
                Some(false) => self.dialog = None,
                None => {}
            }
            return;
        }
        let Some(mut dialog) = self.dialog.take() else { return };
        let accent = self.accent_color();
        let mut done = false;
        let heading = match &dialog {
            Dialog::PickPlaylist(_) => crate::tr!("ui-add-playlist"),
            Dialog::NewPlaylist { .. } => crate::tr!("ui-new-playlist"),
            Dialog::EditPlaylist { .. } => crate::tr!("dialog-edit-playlist"),
            Dialog::ConfirmDelete { .. } => crate::tr!("dialog-delete-playlist"),
            Dialog::ConfirmDeleteUpload { .. } => crate::tr!("dialog-delete-upload"),
            Dialog::ConfirmClearCache => crate::tr!("settings-clear-cache"),
            Dialog::ConfirmDownload { .. } => crate::tr!("settings-download"),
            Dialog::ConfirmAddDuplicates { .. } => crate::tr!("dialog-add-duplicates-title"),
            Dialog::Collaborate { .. } => crate::tr!("ui-collaborate"),
        };
        // A distinct area id per dialog kind. egui keeps an area's previous size when its
        // content shrinks, so a small dialog opened after a big one (the duplicates
        // question after "Add to playlist") would otherwise get the stale, taller rect
        // and draw with a large empty gap below its buttons.
        let modal_id = egui::Id::unique((
            "playlist-dialog",
            match &dialog {
                Dialog::PickPlaylist(_) => 0,
                Dialog::NewPlaylist { .. } => 1,
                Dialog::EditPlaylist { .. } => 2,
                Dialog::ConfirmAddDuplicates { .. } => 3,
                Dialog::ConfirmDownload { .. } => 4,
                Dialog::Collaborate { .. } => 5,
                Dialog::ConfirmDelete { .. } | Dialog::ConfirmDeleteUpload { .. } | Dialog::ConfirmClearCache => 6,
            },
        ));
        // The same modal card as "Delete playlist?" and the update dialog.
        widgets::modal_extras(ctx, modal_id);
        let modal = egui::Modal::new(modal_id).frame(notes::surface()).backdrop_color(theme::shade(0.55)).show(ctx, |ui| {
            ui.set_width(440.0_f32.min(ctx.content_rect().width() - 48.0));
            egui::Frame::new().inner_margin(egui::Margin { left: 26, right: 26, top: 24, bottom: 22 }).show(ui, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                ui.label(egui::RichText::new(heading).font(theme::bold(22.0)).color(theme::text()));
                ui.add_space(18.0);
                match &mut dialog {
                    Dialog::PickPlaylist(tracks) => {
                        if dialog_row(ui, None, Some(theme::icon::LIST_PLUS), crate::tr!("ui-new-playlist")) {
                            self.actions.push(Action::NewPlaylist(tracks.clone()));
                            done = true;
                        }
                        ui.add_space(6.0);
                        pages::divider(ui);
                        ui.add_space(6.0);
                        let cards = self.library.as_ref().map(|l| l.playlists.clone()).unwrap_or_default();
                        let order = super::collections::pinned_first(&self.pinned, &cards, 0..cards.len());
                        egui::ScrollArea::vertical().max_height(340.0).auto_shrink([false, true]).show(ui, |ui| {
                            for card in order.into_iter().map(|i| &cards[i]) {
                                if let Target::Playlist(id) = &card.target {
                                    if !id.starts_with("PL") { continue; }
                                    if dialog_row(ui, Some(&card.thumbs), None, &card.title) {
                                        self.add_to_playlist(id.clone(), tracks, Duplicates::Ask);
                                        done = true;
                                    }
                                }
                            }
                        });
                    }
                    Dialog::NewPlaylist { title, tracks } => {
                        let name = text_field(ui, crate::tr!("dialog-name"), title, false, crate::tr!("dialog-my-playlist"));
                        if ui.memory(|m| m.focused().is_none()) { name.request_focus(); }
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new(crate::tr!("dialog-private-desc")).font(theme::regular(12.5)).color(theme::text_faint()));
                        ui.add_space(24.0);
                        let enter = name.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let (ok, _, cancel) = dialog_buttons(ui, crate::tr!("ui-create"), None, !title.trim().is_empty(), accent);
                        if (ok || enter) && !title.trim().is_empty() {
                            self.backend.request(Req::CreatePlaylist { title: title.trim().to_owned(), video_ids: tracks.iter().map(|t| t.id.clone()).collect() });
                            done = true;
                        }
                        done |= cancel;
                    }
                    Dialog::EditPlaylist { id, title, description, privacy } => {
                        text_field(ui, crate::tr!("dialog-name"), title, false, crate::tr!("dialog-playlist-name"));
                        ui.add_space(16.0);
                        text_field(ui, crate::tr!("dialog-description"), description, true, crate::tr!("dialog-optional-description"));
                        ui.add_space(16.0);
                        field_label(ui, crate::tr!("dialog-visibility"));
                        privacy_picker(ui, privacy);
                        ui.add_space(26.0);
                        let (ok, _, cancel) = dialog_buttons(ui, crate::tr!("settings-save"), None, !title.trim().is_empty(), accent);
                        if ok {
                            self.backend.request(Req::EditPlaylist { id: id.clone(), title: title.trim().to_owned(), description: description.clone(), privacy: privacy.clone() });
                            done = true;
                        }
                        done |= cancel;
                    }
                    Dialog::ConfirmDownload { target, title, tracks, count } => {
                        let body = if target.is_some() {
                            crate::tr!("dialog-download-collection-body", title = title.as_str(), count = *count)
                        } else {
                            crate::tr!("dialog-download-body", count = *count)
                        };
                        ui.add(egui::Label::new(egui::RichText::new(body).font(theme::regular(14.0)).color(theme::text_dim())).wrap());
                        ui.add_space(22.0);
                        let (ok, _, cancel) = dialog_buttons(ui, &crate::tr!("dialog-download-count", count = *count), None, true, accent);
                        if ok {
                            let (target, title, tracks) = (target.clone(), title.clone(), Some(std::mem::take(tracks)));
                            self.backend.request(Req::DownloadMany { target, title, tracks, confirmed: true });
                        }
                        done = ok || cancel;
                    }
                    Dialog::ConfirmAddDuplicates { playlist_id, playlist_title, tracks, duplicates } => {
                        let body = duplicates_body(playlist_title, duplicates);
                        ui.add(egui::Label::new(egui::RichText::new(body).font(theme::regular(14.0)).color(theme::text_dim())).wrap());
                        ui.add_space(22.0);
                        // Of several songs, adding just the new ones is the likely answer.
                        let (add, skip, cancel) = if tracks.len() > 1 {
                            let (skip, add, cancel) = dialog_buttons(ui, crate::tr!("dialog-skip-duplicates"), Some(crate::tr!("dialog-add-anyway")), true, accent);
                            (add, skip, cancel)
                        } else {
                            dialog_buttons(ui, crate::tr!("dialog-add-anyway"), None, true, accent)
                        };
                        if add || skip {
                            let how = if add { Duplicates::Add } else { Duplicates::Skip };
                            self.add_to_playlist(playlist_id.clone(), tracks, how);
                        }
                        done = add || skip || cancel;
                    }
                    Dialog::Collaborate { info, busy, confirm_off, .. } => done = self.collaborate_dialog(ui, info, busy, confirm_off, accent),
                    Dialog::ConfirmDelete { .. } | Dialog::ConfirmDeleteUpload { .. } | Dialog::ConfirmClearCache => unreachable!("drawn by confirm"),
                }
            });
        });
        let dismissed = modal.should_close() || ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if !done && !dismissed { self.dialog = Some(dialog); }
    }

    fn backup_stream_dialog(&mut self, ctx: &egui::Context) {
        let id = egui::Id::unique("backup-stream-dialog");
        widgets::modal_extras(ctx, id);
        let modal = egui::Modal::new(id).frame(notes::surface()).backdrop_color(theme::shade(0.55)).show(ctx, |ui| {
            ui.set_width(400.0_f32.min(ctx.content_rect().width() - 48.0));
            egui::Frame::new().inner_margin(egui::Margin { left: 26, right: 26, top: 24, bottom: 22 }).show(ui, |ui| {
                ui.label(egui::RichText::new(tr!("settings-backup-stream-prompt-title")).font(theme::bold(22.0)).color(theme::text()));
                ui.add_space(10.0);
                let body = tr!("settings-backup-stream-prompt-body");
                ui.add(egui::Label::new(egui::RichText::new(body).font(theme::regular(14.0)).color(theme::text_dim())).wrap());
                ui.add_space(22.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    let enable = widgets::pill(ui, tr!("settings-backup-stream-prompt-enable"), None, true).clicked();
                    let open = widgets::pill(ui, tr!("settings-backup-stream-prompt-settings"), None, false).clicked();
                    let dismiss = widgets::pill(ui, tr!("settings-backup-stream-prompt-dismiss"), None, false).clicked();
                    (enable, open, dismiss)
                }).inner
            }).inner
        });
        let (enable, open, dismiss) = modal.inner;
        let pressed_outside = modal.is_top_modal && ctx.input(|input| {
            input.events.iter().any(|event| {
                matches!(event, egui::Event::PointerButton { pos, pressed: true, .. } if !modal.response.rect.contains(*pos))
            })
        });
        if enable || open || dismiss || pressed_outside || modal.should_close() {
            self.backup_prompt = BackupPrompt::Dismissed;
        }
        if enable {
            self.backup_stream = true;
            self.backend.set_backup_stream(true);
            // The song waits where it stopped: reopen it there, now on the backup stream.
            if let Some(at) = self.resume_at {
                self.play_current_at(at, false);
            }
        }
        if open {
            self.settings_query.clear();
            self.settings_tab = pages::SettingsTab::General;
            self.reveal_backup_setting = true;
            self.go(Page::Settings);
        }
    }

    /// The Collaborate dialog's body: the two switches, the invite link and who has
    /// joined. True once it's closed.
    fn collaborate_dialog(&mut self, ui: &mut Ui, info: &mut Option<crate::ytm::Collaboration>, busy: &mut bool, confirm_off: &mut bool, accent: Color32) -> bool {
        use crate::ytm::CollabChange;
        let Some(c) = info.clone() else {
            ui.label(egui::RichText::new(crate::tr!("ui-loading")).font(theme::regular(14.0)).color(theme::text_dim()));
            ui.add_space(24.0);
            return done_button(ui);
        };
        let mut change = None;
        if *confirm_off {
            ui.label(egui::RichText::new(crate::tr!("dialog-stop-collaboration")).font(theme::semibold(15.0)).color(theme::text()));
            ui.add_space(6.0);
            ui.label(egui::RichText::new(crate::tr!("dialog-stop-collaboration-desc")).font(theme::regular(13.0)).color(theme::text_dim()));
            ui.add_space(24.0);
            let (ok, _, cancel) = dialog_buttons(ui, crate::tr!("dialog-turn-off"), None, !*busy, accent);
            if ok {
                change = Some(CollabChange::Collaborate(false));
            }
            if ok || cancel {
                *confirm_off = false;
            }
        } else {
            ui.add_enabled_ui(!*busy, |ui| {
                let desc = if c.private && !c.on { crate::tr!("dialog-collaboration-allow-private") } else { crate::tr!("dialog-collaboration-allow") };
                pages::setting_row(ui, crate::tr!("ui-collaborate"), Some(desc), |ui| {
                    let mut on = c.on;
                    if pages::toggle(ui, &mut on, crate::tr!("ui-collaborate")).changed() {
                        if on { change = Some(CollabChange::Collaborate(true)) } else { *confirm_off = true }
                    }
                });
                if c.on {
                    pages::setting_row(ui, crate::tr!("dialog-new-collaborators"), Some(crate::tr!("dialog-new-collaborators-desc")), |ui| {
                        let mut on = c.allow_new;
                        if pages::toggle(ui, &mut on, crate::tr!("dialog-new-collaborators")).changed() {
                            change = Some(CollabChange::AllowNew(on));
                        }
                    });
                }
            });
            if c.on {
                ui.add_space(10.0);
                if let Some(link) = c.invite_link.as_deref().filter(|_| c.allow_new) {
                    if widgets::pill(ui, crate::tr!("dialog-copy-invite"), Some(theme::icon::LINK), true).clicked() {
                        ui.ctx().copy_text(link.to_owned());
                        self.toast(crate::tr!("dialog-invite-copied"));
                    }
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(crate::tr!("dialog-invite-instructions")).font(theme::regular(12.5)).color(theme::text_faint()));
                } else {
                    ui.label(egui::RichText::new(crate::tr!("dialog-invites-off")).font(theme::regular(12.5)).color(theme::text_faint()));
                }
                ui.add_space(16.0);
                field_label(ui, crate::tr!("dialog-collaborators"));
                if c.collaborators.is_empty() {
                    ui.label(egui::RichText::new(crate::tr!("dialog-no-collaborators")).font(theme::regular(13.5)).color(theme::text_dim()));
                }
                for name in &c.collaborators {
                    ui.label(egui::RichText::new(name).font(theme::regular(14.0)).color(theme::text()));
                    ui.add_space(4.0);
                }
            }
            ui.add_space(24.0);
            // Each switch applies right away, so there's nothing to cancel.
            if done_button(ui) {
                return true;
            }
        }
        if let Some(change) = change {
            *busy = true;
            self.backend.request(Req::SetCollaboration { info: c, change });
        }
        false
    }
}

fn field_label(ui: &mut Ui, label: &str) {
    ui.label(egui::RichText::new(label).font(theme::semibold(13.0)).color(theme::text_dim()));
    ui.add_space(8.0);
}

/// A labelled text box on a glass fill, like the jam and transfer pages' inputs.
pub(super) fn text_field(ui: &mut Ui, label: &str, text: &mut String, multiline: bool, hint: &str) -> egui::Response {
    use egui::{Sense, vec2};
    field_label(ui, label);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), if multiline { 96.0 } else { 42.0 }), Sense::hover());
    // Painted first, so the text box draws over it.
    let bg = ui.painter().add(egui::Shape::Noop);
    let inner = rect.shrink2(vec2(14.0, if multiline { 10.0 } else { 4.0 }));
    let edit = if multiline { egui::TextEdit::multiline(text).desired_rows(4) } else { egui::TextEdit::singleline(text).align(egui::Align2::LEFT_CENTER) };
    let resp = ui.put(
        inner,
        edit.hint_text(egui::RichText::new(hint).color(theme::text_faint())).font(theme::regular(14.0)).frame(egui::Frame::NONE).desired_width(inner.width()),
    );
    let focus = theme::anim_bool(ui.ctx(), resp.id.with("focus"), resp.has_focus(), 0.15);
    ui.painter().set(bg, egui::Shape::Vec(vec![
        egui::Shape::rect_filled(rect, 10.0, theme::glass(0.08 + 0.03 * focus)),
        egui::Shape::rect_stroke(rect, 10.0, egui::Stroke::new(1.0, theme::glass(0.06 + 0.18 * focus)), egui::StrokeKind::Inside),
    ]));
    resp
}

/// Private / Unlisted / Public as one segmented control; nothing picked keeps the current setting.
fn privacy_picker(ui: &mut Ui, privacy: &mut Option<String>) {
    use egui::{Sense, vec2};
    let (track, _) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::hover());
    ui.painter().rect_filled(track, 10.0, theme::glass(0.08));
    let options = [("PRIVATE", crate::tr!("dialog-private")), ("UNLISTED", crate::tr!("dialog-unlisted")), ("PUBLIC", crate::tr!("dialog-public"))];
    let w = (track.width() - 6.0) / options.len() as f32;
    for (i, (value, label)) in options.into_iter().enumerate() {
        let r = egui::Rect::from_min_size(track.min + vec2(3.0 + w * i as f32, 3.0), vec2(w, track.height() - 6.0));
        let resp = widgets::named(ui.interact(r, ui.scope_id().with(("privacy", value)), Sense::click()), label).on_hover_cursor(egui::CursorIcon::PointingHand);
        let on = privacy.as_deref() == Some(value);
        let s = theme::anim_bool(ui.ctx(), resp.id.with("s"), on, 0.15);
        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
        let fill = theme::lerp_color(theme::glass(0.06 * h), theme::text(), s);
        if fill.a() > 0 { ui.painter().rect_filled(r, 8.0, fill); }
        let color = theme::lerp_color(theme::with_alpha(theme::text(), 0.7 + 0.3 * h), theme::on_text(), s);
        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, label, theme::semibold(13.0), color);
        if resp.clicked() {
            *privacy = if on { None } else { Some(value.to_owned()) };
        }
    }
    if privacy.is_none() {
        ui.add_space(8.0);
        ui.label(egui::RichText::new(crate::tr!("dialog-visibility-prompt")).font(theme::regular(12.5)).color(theme::text_faint()));
    }
}

/// Which songs `playlist` has already, by title: the first three, and how many more.
/// None named: YouTube Music only said some are.
pub(super) fn duplicates_body(playlist: &str, duplicates: &[String]) -> String {
    if duplicates.is_empty() {
        return crate::tr!("dialog-add-duplicates-some", title = playlist);
    }
    let shown = duplicates.len().min(3);
    let songs = duplicates[..shown].join(", ");
    crate::tr!("dialog-add-duplicates-body", title = playlist, songs = songs.as_str(), count = duplicates.len(), more = duplicates.len() - shown)
}

/// Cancel, an optional middle pill and a primary button, right-aligned:
/// (primary clicked, middle clicked, cancel clicked).
fn dialog_buttons(ui: &mut Ui, primary: &str, middle: Option<&str>, enabled: bool, accent: Color32) -> (bool, bool, bool) {
    use egui::{Sense, vec2};
    let (mut ok, mut middle_clicked, mut cancel) = (false, false, false);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        let fg = theme::on_fill(accent);
        let g = ui.painter().layout_no_wrap(primary.to_owned(), theme::semibold(14.0), fg);
        let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 40.0, 38.0), if enabled { Sense::click() } else { Sense::hover() });
        let resp = widgets::named(resp, primary);
        let resp = if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() && enabled, 0.15);
        let fill = theme::lerp_color(accent, Color32::WHITE, 0.12 * h);
        ui.painter().rect_filled(rect, 19.0, theme::with_alpha(fill, if enabled { 1.0 } else { 0.35 }));
        ui.painter().galley_with_override_text_color(rect.center() - g.size() / 2.0, g, theme::with_alpha(fg, if enabled { 1.0 } else { 0.6 }));
        ok = resp.clicked();
        if let Some(pill) = middle {
            middle_clicked = widgets::pill(ui, pill, None, false).clicked();
        }
        cancel = widgets::pill(ui, crate::tr!("ui-cancel"), None, false).clicked();
    });
    (ok, middle_clicked, cancel)
}

/// A lone right-aligned Done for a dialog whose changes apply as they're made.
fn done_button(ui: &mut Ui) -> bool {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| widgets::pill(ui, crate::tr!("ui-done"), None, true).clicked()).inner
}

/// A playlist (or "New playlist") row in the Add to playlist dialog.
fn dialog_row(ui: &mut Ui, thumbs: Option<&[crate::model::Thumb]>, glyph: Option<char>, label: &str) -> bool {
    use egui::{Sense, pos2, vec2};
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::click());
    let resp = widgets::named(resp, label).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
    if h > 0.0 { ui.painter().rect_filled(rect, 10.0, theme::glass(0.08 * h)); }
    let art = egui::Rect::from_center_size(pos2(rect.left() + 28.0, rect.center().y), vec2(40.0, 40.0));
    match (thumbs, glyph) {
        (Some(t), _) => widgets::paint_cover(ui, art, t, 6.0, 1.0),
        (None, Some(g)) => {
            ui.painter().rect_filled(art, 6.0, theme::glass_panel(0.1));
            widgets::icon_at(ui, art.center(), g, 18.0, theme::text());
        }
        _ => {}
    }
    let x = art.right() + 14.0;
    widgets::text_at(ui, pos2(x, rect.center().y - 9.0), label, theme::semibold(14.0), theme::text(), rect.right() - x - 10.0);
    resp.clicked()
}

/// A question like "Delete playlist?" as a modal card, its answer in red: Some(true)
/// yes, Some(false) cancel, None still open. `name` is the button's, for screen readers.
fn confirm(ctx: &egui::Context, heading: &str, body: &str, label: &str, name: &str) -> Option<bool> {
    let mut answer = None;
    widgets::modal_extras(ctx, egui::Id::unique("confirm-delete"));
    let modal = egui::Modal::new(egui::Id::unique("confirm-delete")).frame(notes::surface()).backdrop_color(theme::shade(0.55)).show(ctx, |ui| {
        ui.set_width(400.0_f32.min(ctx.content_rect().width() - 48.0));
        egui::Frame::new().inner_margin(egui::Margin { left: 26, right: 26, top: 24, bottom: 22 }).show(ui, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
            ui.label(egui::RichText::new(heading).font(theme::bold(22.0)).color(theme::text()));
            ui.add_space(10.0);
            ui.add(egui::Label::new(egui::RichText::new(body).font(theme::regular(14.0)).color(theme::text_dim())).wrap());
            ui.add_space(22.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                // Destructive action in red, white text.
                let g = ui.painter().layout_no_wrap(label.to_owned(), theme::semibold(14.0), Color32::WHITE);
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(g.size().x + 40.0, 38.0), egui::Sense::click());
                let resp = widgets::named(resp, name).on_hover_cursor(egui::CursorIcon::PointingHand);
                let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
                ui.painter().rect_filled(rect, 19.0, theme::lerp_color(Color32::from_rgb(214, 40, 57), Color32::from_rgb(236, 56, 72), h));
                ui.painter().galley(rect.center() - g.size() / 2.0, g, Color32::WHITE);
                if resp.clicked() {
                    answer = Some(true);
                }
                if widgets::pill(ui, crate::tr!("ui-cancel"), None, false).clicked() {
                    answer = Some(false);
                }
            });
        });
    });
    if answer.is_none() && (modal.should_close() || ctx.input(|i| i.key_pressed(egui::Key::Escape))) {
        answer = Some(false);
    }
    answer
}
