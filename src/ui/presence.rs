//! Settings › Connections › Discord: what the Rich Presence card shows, and a preview of it.

use egui::{Align2, Color32, Id, Rect, RichText, Sense, Ui, Vec2, pos2, vec2};

use super::pages::{divider, dropdown_button, filtering, option_row, section_label, setting_row, setting_row_wide, settings_card, settings_section, toggle};
use super::{App, widgets};
use crate::discord::{self, Card, Page, Prefs, Presence, Show, Status};
use crate::theme::{self, icon};

/// Dropdowns and the name box share a width, so the column lines up.
const CONTROL_W: f32 = 210.0;
/// A dropdown with its link switch.
const LINKED_W: f32 = CONTROL_W + 8.0 + 34.0;
/// The profile card's width, with the member list beside it.
const CARD_W: f32 = 340.0;

pub(super) fn settings(app: &mut App, ui: &mut Ui) {
    let mut prefs = app.discord_prefs.clone();
    let mut enabled = app.discord_enabled;
    // Where `section_label` puts the heading, for Reset at its right end.
    let heading = Rect::from_min_size(ui.cursor().min + vec2(0.0, 30.0), vec2(ui.available_width(), 20.0));
    settings_section(ui, "Discord", None, None, |ui| {
        setting_row(ui, "Rich Presence", Some("Show the song you're playing on your Discord profile."), |ui| {
            toggle(ui, &mut enabled, "Rich Presence");
        });
        ui.add_enabled_ui(enabled, |ui| {
            divider(ui);
            setting_row(ui, "Show when paused", Some("Keep the card up while the song is paused. Discord has no paused state, so the progress bar goes rather than keep running."), |ui| {
                toggle(ui, &mut prefs.when_paused, "Show when paused");
            });
            divider(ui);
            setting_row(ui, "Hide details", Some("Say only that music is playing. No title, artist, album, artwork, progress bar or buttons leave your computer."), |ui| {
                toggle(ui, &mut prefs.hide_details, "Hide details");
            });
        });
    });
    if !filtering() && prefs != Prefs::default() && reset_link(ui, heading) {
        prefs = Prefs::default();
    }
    if !filtering() {
        preview(app, ui, &prefs, enabled);
    }
    let on = enabled && !prefs.hide_details;
    settings_section(ui, "Discord status", None, None, |ui| {
        ui.add_enabled_ui(on, |ui| {
            setting_row(ui, "Status text", Some("What Discord shows after \"Listening to\" beside your name in the member list. The profile card always shows the app's name."), |ui| {
                let lines = prefs.lines;
                let label = |s: Status| match s {
                    Status::Name => "App name".to_owned(),
                    Status::Line1 => format!("Line 1 · {}", lines[0].label()),
                    Status::Line2 => format!("Line 2 · {}", lines[1].label()),
                };
                dropdown(ui, "Status text", &mut prefs.status, &Status::ALL, label);
            });
        });
        divider(ui);
        ui.add_enabled_ui(enabled, |ui| {
            let desc = format!("Replaces \"{}\" wherever the app's name shows. Leave it empty to keep that.", discord::APP_NAME);
            setting_row(ui, "App name", Some(&desc), |ui| {
                name_field(ui, &mut prefs.app_name);
            });
        });
    });
    let note = "The link beside a line makes it clickable. It opens what it shows on YouTube Music: a line with the artist opens the artist.";
    settings_section(ui, "Discord card", None, Some(note), |ui| {
        ui.add_enabled_ui(on, |ui| {
            setting_row_wide(ui, "Line 1", Some("The bold line, under the header."), LINKED_W, |ui| {
                dropdown(ui, "Line 1", &mut prefs.lines[0], &Show::ALL, |s| s.label().to_owned());
                link_toggle(ui, &mut prefs.links[0], "Line 1 is clickable", prefs.lines[0] != Show::Nothing);
            });
            divider(ui);
            setting_row_wide(ui, "Line 2", Some("The smaller line below it."), LINKED_W, |ui| {
                dropdown(ui, "Line 2", &mut prefs.lines[1], &Show::ALL, |s| s.label().to_owned());
                link_toggle(ui, &mut prefs.links[1], "Line 2 is clickable", prefs.lines[1] != Show::Nothing);
            });
            divider(ui);
            setting_row(ui, "Album art", Some("Show the artwork beside the song. Its link opens the album, or the song when it has none."), |ui| {
                toggle(ui, &mut prefs.art, "Album art");
                link_toggle(ui, &mut prefs.art_link, "Album art is clickable", prefs.art);
            });
            ui.add_enabled_ui(prefs.art, |ui| {
                divider(ui);
                setting_row(ui, "Provider badge", Some("Mark the artwork with a small ytm-rs icon, the way Spotify cards are."), |ui| {
                    toggle(ui, &mut prefs.badge, "Provider badge");
                });
                divider(ui);
                setting_row(ui, "Line 3", Some("The last line. Discord also shows it when the artwork is hovered, so it needs the artwork."), |ui| {
                    dropdown(ui, "Line 3", &mut prefs.lines[2], &Show::ALL, |s| s.label().to_owned());
                });
            });
            divider(ui);
            setting_row(ui, "Progress bar", Some("Show elapsed and remaining time under the song."), |ui| {
                toggle(ui, &mut prefs.progress, "Progress bar");
            });
        });
    });
    let note = "Discord shows at most two, and never on your own profile: only others see them. A button for a page the song doesn't have is left out, and Listen together shows only while you're in a room.";
    settings_section(ui, "Discord buttons", None, Some(note), |ui| {
        ui.add_enabled_ui(on, |ui| {
            setting_row(ui, "First button", None, |ui| {
                dropdown(ui, "First button", &mut prefs.buttons[0], &Page::ALL, |p| p.label().to_owned());
            });
            divider(ui);
            setting_row(ui, "Second button", None, |ui| {
                dropdown(ui, "Second button", &mut prefs.buttons[1], &Page::ALL, |p| p.label().to_owned());
            });
        });
    });
    if enabled != app.discord_enabled {
        app.set_discord_enabled(enabled);
    }
    // `sync_discord` sends what changed.
    app.discord_prefs = prefs;
}

fn dropdown<T: Copy + PartialEq>(ui: &mut Ui, name: &str, value: &mut T, all: &[T], label: impl Fn(T) -> String) {
    let btn = dropdown_button(ui, &label(*value), CONTROL_W, name);
    widgets::menu_popup(&btn).show(|ui| {
        widgets::menu_body(ui, CONTROL_W);
        for &v in all {
            if option_row(ui, &label(v), v == *value).clicked() {
                *value = v;
                ui.close();
            }
        }
    });
}

/// The link switch beside a part of the card: on, that part is clickable.
fn link_toggle(ui: &mut Ui, on: &mut bool, name: &str, enabled: bool) {
    ui.add_space(8.0);
    let enabled = enabled && ui.is_enabled();
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::splat(34.0), if enabled { Sense::click() } else { Sense::hover() });
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let value = *on;
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, enabled, value, name));
    let resp = if enabled { resp.on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
    let t = theme::anim_bool(ui.ctx(), resp.id.with("t"), value && enabled, 0.16);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() && enabled, 0.12);
    // On, it's the accent, like the switches.
    let accent = ui.ctx().data(|d| d.get_temp::<Color32>(egui::Id::unique("accent"))).unwrap_or(theme::RED);
    let fade = if enabled { 1.0 } else { 0.45 };
    let off = theme::glass(0.06 + 0.06 * h);
    ui.painter().circle_filled(rect.center(), 17.0, theme::with_alpha(theme::lerp_color(off, accent, t), fade));
    let color = theme::lerp_color(theme::lerp_color(theme::text_faint(), theme::text(), h), theme::on_fill(accent), t);
    widgets::icon_at(ui, rect.center(), icon::LINK, 16.0, theme::with_alpha(color, fade));
}

/// The app name box: `sync_discord` holds the name back while it has focus.
pub(super) fn name_id() -> Id {
    Id::unique("discord-app-name")
}

fn name_field(ui: &mut Ui, name: &mut String) {
    let (rect, _) = ui.allocate_exact_size(vec2(CONTROL_W, 36.0), Sense::hover());
    // Painted first, so the text box draws over it.
    let bg = ui.painter().add(egui::Shape::Noop);
    let inner = rect.shrink2(vec2(14.0, 4.0));
    let edit = egui::TextEdit::singleline(name)
        .id(name_id())
        .hint_text(RichText::new(discord::APP_NAME).color(theme::text_faint()))
        .font(theme::regular(14.0))
        .frame(egui::Frame::NONE)
        .char_limit(128)
        .align(Align2::LEFT_CENTER)
        .desired_width(inner.width());
    let resp = ui.put(inner, edit);
    let focus = theme::anim_bool(ui.ctx(), resp.id.with("focus"), resp.has_focus(), 0.15);
    ui.painter().set(bg, egui::Shape::rect_filled(rect, 10.0, theme::glass(0.08 + 0.03 * focus)));
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64
}

/// Shown with nothing playing.
fn example() -> Presence {
    let now = now_ms();
    Presence {
        video_id: String::new(),
        title: "Song title".into(),
        artist: "Artist".into(),
        artist_id: None,
        album: Some("Album".into()),
        album_id: None,
        art: Some(discord::BADGE.into()),
        start_ms: now - 79_000,
        end_ms: Some(now + 126_000),
        paused: false,
        jam: None,
    }
}

/// The profile card and member list as Discord would show them, under the Discord section.
fn preview(app: &App, ui: &mut Ui, prefs: &Prefs, enabled: bool) {
    let (p, state) = shown(app, enabled);
    section_label(ui, "Preview", Some(state));
    // Paused, the card is drawn as it would be kept up.
    let card = Card::new(&p, prefs);
    let notes = notes(&p, &card, prefs, enabled);
    settings_card(ui, |ui| {
        ui.add_space(16.0);
        let w = ui.available_width();
        let wide = w >= 600.0;
        let card_w = if wide { CARD_W } else { w.min(380.0) };
        let side = |ui: &mut Ui| {
            caption(ui, "Member list");
            member_row(ui, &card, app.account_avatar.as_deref(), ui.available_width().min(380.0));
            for n in &notes {
                note(ui, n);
            }
        };
        if wide {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(card_w);
                    caption(ui, "Profile card");
                    profile_card(ui, &card, card_w);
                });
                ui.add_space(24.0);
                ui.vertical(side);
            });
        } else {
            caption(ui, "Profile card");
            profile_card(ui, &card, card_w);
            ui.add_space(14.0);
            side(ui);
        }
        ui.add_space(16.0);
    });
    if card.times.is_some() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
    }
}

/// A dim "Reset" at the right end of the heading in `rect`, brighter on hover. True when clicked.
fn reset_link(ui: &mut Ui, rect: Rect) -> bool {
    let font = theme::semibold(13.0);
    let size = ui.painter().layout_no_wrap("Reset".to_owned(), font.clone(), theme::text_dim()).size();
    let area = Align2::RIGHT_CENTER.anchor_size(pos2(rect.right() - 2.0, rect.center().y), size);
    let link = ui.interact(area.expand(4.0), Id::unique("discord-reset"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
    link.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Reset Discord settings to defaults"));
    let color = if link.hovered() { theme::text() } else { theme::text_dim() };
    ui.painter().text(area.right_center(), Align2::RIGHT_CENTER, "Reset", font, color);
    link.clicked()
}

/// The song the preview shows, and what it is.
fn shown(app: &App, enabled: bool) -> (Presence, &'static str) {
    let p = app.discord_presence();
    let state = match &p {
        _ if !enabled => "Off",
        None => "Example",
        Some(p) if p.paused => "Paused",
        Some(_) => "Now playing",
    };
    (p.unwrap_or_else(example), state)
}

/// What the preview can't show by itself.
fn notes(p: &Presence, card: &Card, prefs: &Prefs, enabled: bool) -> Vec<&'static str> {
    let mut notes = Vec::new();
    if !enabled {
        notes.push("Rich Presence is off, so none of this is on your profile.");
    } else if p.paused && !prefs.when_paused {
        notes.push("Paused, so this isn't on your profile right now. Turn on Show when paused to keep it up.");
    }
    if !card.buttons.is_empty() {
        notes.push("Only others see the buttons.");
    }
    notes
}

fn note(ui: &mut Ui, text: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(text).font(theme::regular(12.5)).color(theme::text_dim()));
}

fn caption(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).font(theme::semibold(12.5)).color(theme::text_dim()));
    ui.add_space(6.0);
}

const PAD: f32 = 12.0;
const ART: f32 = 76.0;
const LINE: f32 = 20.0;
const BAR: f32 = 22.0;
const BUTTON: f32 = 32.0;
const GAP: f32 = 8.0;

fn profile_card(ui: &mut Ui, c: &Card, width: f32) {
    let lines: Vec<_> = c.lines.iter().flatten().collect();
    let text_h = lines.len() as f32 * LINE + if c.times.is_some() { BAR } else { 0.0 };
    let body_h = if c.art.is_some() { ART.max(text_h) } else { text_h };
    let buttons_h = c.buttons.len() as f32 * (BUTTON + GAP);
    let h = PAD + 18.0 + if body_h > 0.0 { 10.0 + body_h } else { 0.0 } + if buttons_h > 0.0 { 12.0 + buttons_h - GAP } else { 0.0 } + PAD;
    let (rect, _) = ui.allocate_exact_size(vec2(width, h), Sense::hover());
    ui.painter().rect_filled(rect, 10.0, theme::glass(0.07));
    let inner_w = width - 2.0 * PAD;
    let mut y = rect.top() + PAD;
    widgets::text_at(ui, pos2(rect.left() + PAD, y), &format!("Listening to {}", c.name()), theme::semibold(12.5), theme::text_dim(), inner_w);
    y += 18.0 + 10.0;
    let mut x = rect.left() + PAD;
    if let Some(art) = &c.art {
        let r = Rect::from_min_size(pos2(x, y), Vec2::splat(ART));
        ui.painter().rect_filled(r, 8.0, theme::glass(0.08));
        widgets::paint_remote(ui, art, r, 8.0, full_uv(), Color32::WHITE);
        if c.badge {
            let b = Rect::from_center_size(r.right_bottom() - vec2(3.0, 3.0), Vec2::splat(22.0));
            ui.painter().circle_filled(b.center(), 13.5, Color32::from_black_alpha(200));
            widgets::paint_remote(ui, discord::BADGE, b, 11.0, full_uv(), Color32::WHITE);
        }
        x += ART + 12.0;
    }
    let tw = rect.right() - PAD - x;
    let mut ty = y + ((body_h - text_h) / 2.0).max(0.0);
    for (i, l) in lines.iter().enumerate() {
        let (font, color) = if i == 0 && c.lines[0].is_some() { (theme::semibold(15.0), theme::text()) } else { (theme::regular(13.5), theme::text_dim()) };
        let r = widgets::text_at(ui, pos2(x, ty), &l.text, font, color, tw);
        if l.url.is_some() {
            ui.painter().hline(r.x_range(), r.bottom() + 0.5, egui::Stroke::new(1.0, theme::with_alpha(color, 0.6)));
        }
        ty += LINE;
    }
    if let Some((start, end)) = c.times {
        progress(ui, Rect::from_min_size(pos2(x, ty + 4.0), vec2(tw, BAR - 4.0)), start, end);
    }
    y += body_h + 12.0;
    for (label, _) in &c.buttons {
        let r = Rect::from_min_size(pos2(rect.left() + PAD, y), vec2(inner_w, BUTTON));
        ui.painter().rect_filled(r, 8.0, theme::glass(0.10));
        ui.painter().text(r.center(), Align2::CENTER_CENTER, *label, theme::semibold(13.5), theme::text());
        y += BUTTON + GAP;
    }
}

/// "1:19 ━━━━──── 3:25", or "1:19 elapsed" without an end.
fn progress(ui: &Ui, rect: Rect, start: i64, end: Option<i64>) {
    let secs = |ms: i64| (ms / 1000).max(0) as u32;
    let elapsed = secs(now_ms() - start);
    let font = theme::regular(12.0);
    let p = ui.painter();
    let Some(end) = end else {
        p.text(rect.left_center(), Align2::LEFT_CENTER, format!("{} elapsed", super::fmt_time(elapsed)), font, theme::text_dim());
        return;
    };
    let total = secs(end - start).max(1);
    let elapsed = elapsed.min(total);
    let left = p.text(rect.left_center(), Align2::LEFT_CENTER, super::fmt_time(elapsed), font.clone(), theme::text_dim());
    let right = p.text(rect.right_center(), Align2::RIGHT_CENTER, super::fmt_time(total), font, theme::text_dim());
    let track = Rect::from_min_max(pos2(left.right() + 8.0, rect.center().y - 2.0), pos2(right.left() - 8.0, rect.center().y + 2.0));
    if track.width() > 0.0 {
        p.rect_filled(track, 2.0, theme::glass(0.15));
        let done = track.with_max_x(track.left() + track.width() * elapsed as f32 / total as f32);
        p.rect_filled(done, 2.0, theme::text());
    }
}

fn member_row(ui: &mut Ui, c: &Card, avatar: Option<&str>, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 52.0), Sense::hover());
    ui.painter().rect_filled(rect, 8.0, theme::glass(0.07));
    let pic = Rect::from_center_size(pos2(rect.left() + 26.0, rect.center().y), Vec2::splat(34.0));
    let accent = ui.ctx().data(|d| d.get_temp::<Color32>(egui::Id::unique("accent"))).unwrap_or(theme::RED);
    ui.painter().circle_filled(pic.center(), 17.0, accent);
    if let Some(url) = avatar {
        widgets::paint_remote(ui, url, pic, 17.0, full_uv(), Color32::WHITE);
    }
    let x = pic.right() + 10.0;
    let w = rect.right() - 10.0 - x;
    widgets::text_at(ui, pos2(x, rect.top() + 8.0), "You", theme::semibold(14.0), theme::text(), w);
    let y = rect.top() + 34.0;
    widgets::icon_at(ui, pos2(x + 6.0, y), icon::HEADPHONES, 12.0, theme::text_dim());
    let status = format!("Listening to {}", c.status_text());
    widgets::text_at(ui, pos2(x + 16.0, y - 8.0), &status, theme::regular(12.5), theme::text_dim(), w - 16.0);
}

fn full_uv() -> Rect {
    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))
}
