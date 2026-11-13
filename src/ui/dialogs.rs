//! Dialogs: new and edited playlists, collaborators, confirmations, and their parts.

use egui::{Color32, Ui};

use crate::backend::Req;
use crate::model::Target;
use crate::theme;

use super::{Action, App, Dialog, notes, pages, widgets};

impl App {
    pub(super) fn draw_dialog(&mut self, ctx: &egui::Context) {
        if let Some(Dialog::ConfirmDelete { id, title }) = &self.dialog {
            let (id, title) = (id.clone(), title.clone());
            match confirm_delete(ctx, &title, self.accent_color()) {
                Some(true) => {
                    self.backend.request(Req::DeletePlaylist(id));
                    self.dialog = None;
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
            Dialog::PickPlaylist(_) => "Add to playlist",
            Dialog::NewPlaylist { .. } => "New playlist",
            Dialog::EditPlaylist { .. } => "Edit playlist",
            Dialog::ConfirmDelete { .. } => "Delete playlist",
            Dialog::ConfirmDownload { .. } => "Download",
            Dialog::Collaborate { .. } => "Collaborate",
        };
        // The same modal card as "Delete playlist?" and the update dialog.
        widgets::modal_extras(ctx, egui::Id::unique("playlist-dialog"));
        let modal = egui::Modal::new(egui::Id::unique("playlist-dialog")).frame(notes::surface()).backdrop_color(theme::shade(0.55)).show(ctx, |ui| {
            ui.set_width(440.0_f32.min(ctx.content_rect().width() - 48.0));
            egui::Frame::new().inner_margin(egui::Margin { left: 26, right: 26, top: 24, bottom: 22 }).show(ui, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                ui.label(egui::RichText::new(heading).font(theme::bold(22.0)).color(theme::text()));
                ui.add_space(18.0);
                match &mut dialog {
                    Dialog::PickPlaylist(tracks) => {
                        if dialog_row(ui, None, Some(theme::icon::LIST_PLUS), "New playlist") {
                            self.actions.push(Action::NewPlaylist(tracks.clone()));
                            done = true;
                        }
                        ui.add_space(6.0);
                        pages::divider(ui);
                        ui.add_space(6.0);
                        let cards = self.library.as_ref().map(|l| l.playlists.clone()).unwrap_or_default();
                        egui::ScrollArea::vertical().max_height(340.0).auto_shrink([false, true]).show(ui, |ui| {
                            for card in cards {
                                if let Target::Playlist(id) = card.target {
                                    if !id.starts_with("PL") { continue; }
                                    if dialog_row(ui, Some(&card.thumbs), None, &card.title) {
                                        self.add_to_playlist(id, tracks);
                                        done = true;
                                    }
                                }
                            }
                        });
                    }
                    Dialog::NewPlaylist { title, tracks } => {
                        let name = text_field(ui, "Name", title, false, "My playlist");
                        if ui.memory(|m| m.focused().is_none()) { name.request_focus(); }
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("Private · only you can see it").font(theme::regular(12.5)).color(theme::text_faint()));
                        ui.add_space(24.0);
                        let enter = name.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let (ok, cancel) = dialog_buttons(ui, "Create", !title.trim().is_empty(), accent);
                        if (ok || enter) && !title.trim().is_empty() {
                            self.backend.request(Req::CreatePlaylist { title: title.trim().to_owned(), video_ids: tracks.iter().map(|t| t.id.clone()).collect() });
                            done = true;
                        }
                        done |= cancel;
                    }
                    Dialog::EditPlaylist { id, title, description, privacy } => {
                        text_field(ui, "Name", title, false, "Playlist name");
                        ui.add_space(16.0);
                        text_field(ui, "Description", description, true, "Add an optional description");
                        ui.add_space(16.0);
                        field_label(ui, "Visibility");
                        privacy_picker(ui, privacy);
                        ui.add_space(26.0);
                        let (ok, cancel) = dialog_buttons(ui, "Save", !title.trim().is_empty(), accent);
                        if ok {
                            self.backend.request(Req::EditPlaylist { id: id.clone(), title: title.trim().to_owned(), description: description.clone(), privacy: privacy.clone() });
                            done = true;
                        }
                        done |= cancel;
                    }
                    Dialog::ConfirmDownload { target, title, tracks, count } => {
                        let what = if target.is_some() { format!("\u{201c}{title}\u{201d} has {count} songs") } else { format!("{count} songs are") };
                        let body = format!("{what} not downloaded yet. They download in the background, and the song that's playing goes first.");
                        ui.add(egui::Label::new(egui::RichText::new(body).font(theme::regular(14.0)).color(theme::text_dim())).wrap());
                        ui.add_space(22.0);
                        let (ok, cancel) = dialog_buttons(ui, &format!("Download {count} songs"), true, accent);
                        if ok {
                            let (target, title, tracks) = (target.clone(), title.clone(), Some(std::mem::take(tracks)));
                            self.backend.request(Req::DownloadMany { target, title, tracks, confirmed: true });
                        }
                        done = ok || cancel;
                    }
                    Dialog::Collaborate { info, busy, confirm_off, .. } => done = self.collaborate_dialog(ui, info, busy, confirm_off, accent),
                    Dialog::ConfirmDelete { .. } => unreachable!("drawn by confirm_delete"),
                }
            });
        });
        let dismissed = modal.should_close() || ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if !done && !dismissed { self.dialog = Some(dialog); }
    }

    /// The Collaborate dialog's body: the two switches, the invite link and who has
    /// joined. True once it's closed.
    fn collaborate_dialog(&mut self, ui: &mut Ui, info: &mut Option<crate::ytm::Collaboration>, busy: &mut bool, confirm_off: &mut bool, accent: Color32) -> bool {
        use crate::ytm::CollabChange;
        let Some(c) = info.clone() else {
            ui.label(egui::RichText::new("Loading…").font(theme::regular(14.0)).color(theme::text_dim()));
            ui.add_space(24.0);
            return done_button(ui);
        };
        let mut change = None;
        if *confirm_off {
            ui.label(egui::RichText::new("Turn off collaboration?").font(theme::semibold(15.0)).color(theme::text()));
            ui.add_space(6.0);
            ui.label(egui::RichText::new("Collaborators will be removed from the playlist. Songs they added stay.").font(theme::regular(13.0)).color(theme::text_dim()));
            ui.add_space(24.0);
            let (ok, cancel) = dialog_buttons(ui, "Turn off", !*busy, accent);
            if ok {
                change = Some(CollabChange::Collaborate(false));
            }
            if ok || cancel {
                *confirm_off = false;
            }
        } else {
            ui.add_enabled_ui(!*busy, |ui| {
                let desc = if c.private && !c.on { "Collaborators can add songs. Makes this private playlist unlisted." } else { "Collaborators can add songs." };
                pages::setting_row(ui, "Collaborate", Some(desc), |ui| {
                    let mut on = c.on;
                    if pages::toggle(ui, &mut on, "Collaborate").changed() {
                        if on { change = Some(CollabChange::Collaborate(true)) } else { *confirm_off = true }
                    }
                });
                if c.on {
                    pages::setting_row(ui, "Allow new collaborators", Some("Turn off to stop new people joining with the link."), |ui| {
                        let mut on = c.allow_new;
                        if pages::toggle(ui, &mut on, "Allow new collaborators").changed() {
                            change = Some(CollabChange::AllowNew(on));
                        }
                    });
                }
            });
            if c.on {
                ui.add_space(10.0);
                if let Some(link) = c.invite_link.as_deref().filter(|_| c.allow_new) {
                    if widgets::pill(ui, "Copy invite link", Some(theme::icon::LINK), true).clicked() {
                        ui.ctx().copy_text(link.to_owned());
                        self.toast("Invite link copied. Whoever opens it can add songs.");
                    }
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("Send the link to the people you want to add. They join from YouTube Music.").font(theme::regular(12.5)).color(theme::text_faint()));
                } else {
                    ui.label(egui::RichText::new("New collaborators can't join. Turn on Allow new collaborators for an invite link.").font(theme::regular(12.5)).color(theme::text_faint()));
                }
                ui.add_space(16.0);
                field_label(ui, "Collaborators");
                if c.collaborators.is_empty() {
                    ui.label(egui::RichText::new("No one has joined yet.").font(theme::regular(13.5)).color(theme::text_dim()));
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
    let options = [("PRIVATE", "Private"), ("UNLISTED", "Unlisted"), ("PUBLIC", "Public")];
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
        ui.label(egui::RichText::new("Unchanged. Pick one to change who can see it.").font(theme::regular(12.5)).color(theme::text_faint()));
    }
}

/// Cancel and a primary button, right-aligned: (primary clicked, cancel clicked).
fn dialog_buttons(ui: &mut Ui, primary: &str, enabled: bool, accent: Color32) -> (bool, bool) {
    use egui::{Sense, vec2};
    let (mut ok, mut cancel) = (false, false);
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
        cancel = widgets::pill(ui, "Cancel", None, false).clicked();
    });
    (ok, cancel)
}

/// A lone right-aligned Done for a dialog whose changes apply as they're made.
fn done_button(ui: &mut Ui) -> bool {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| widgets::pill(ui, "Done", None, true).clicked()).inner
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

/// "Delete playlist?" as a modal card: Some(true) delete, Some(false) cancel, None still open.
fn confirm_delete(ctx: &egui::Context, title: &str, _accent: Color32) -> Option<bool> {
    let mut answer = None;
    widgets::modal_extras(ctx, egui::Id::unique("confirm-delete"));
    let modal = egui::Modal::new(egui::Id::unique("confirm-delete")).frame(notes::surface()).backdrop_color(theme::shade(0.55)).show(ctx, |ui| {
        ui.set_width(400.0_f32.min(ctx.content_rect().width() - 48.0));
        egui::Frame::new().inner_margin(egui::Margin { left: 26, right: 26, top: 24, bottom: 22 }).show(ui, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
            ui.label(egui::RichText::new("Delete playlist?").font(theme::bold(22.0)).color(theme::text()));
            ui.add_space(10.0);
            let body = format!("\u{201c}{title}\u{201d} will be removed from your YouTube Music library. This can't be undone.");
            ui.add(egui::Label::new(egui::RichText::new(body).font(theme::regular(14.0)).color(theme::text_dim())).wrap());
            ui.add_space(22.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                // Destructive action in red, white text.
                let label = "Delete";
                let g = ui.painter().layout_no_wrap(label.to_owned(), theme::semibold(14.0), Color32::WHITE);
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(g.size().x + 40.0, 38.0), egui::Sense::click());
                let resp = widgets::named(resp, "Delete playlist").on_hover_cursor(egui::CursorIcon::PointingHand);
                let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
                ui.painter().rect_filled(rect, 19.0, theme::lerp_color(Color32::from_rgb(214, 40, 57), Color32::from_rgb(236, 56, 72), h));
                ui.painter().galley(rect.center() - g.size() / 2.0, g, Color32::WHITE);
                if resp.clicked() {
                    answer = Some(true);
                }
                if widgets::pill(ui, "Cancel", None, false).clicked() {
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
