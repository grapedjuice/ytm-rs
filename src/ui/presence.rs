//! Settings › Connections › Discord: what the Rich Presence card shows, and a preview of it.

use egui::{Align2, Color32, Id, Layout, Rect, RichText, Sense, Ui, UiBuilder, Vec2, pos2, vec2};

use super::pages::{divider, dropdown_button, filtering, option_row, section_label, setting_row, setting_row_wide, settings_card, settings_section, toggle};
use super::{App, widgets};
use crate::discord::{self, BadgeShows, Card, Page, Prefs, Presence, Show, Status};
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
    // Off, the rest folds away; a search shows its rows either way.
    let shown = if filtering() { 1.0 } else { theme::anim_bool_soft(ui.ctx(), "discord-settings-shown", enabled, 0.3) };
    // Where `section_label` puts the heading, for Reset at its right end.
    let heading = Rect::from_min_size(ui.cursor().min + vec2(0.0, 30.0), vec2(ui.available_width(), 20.0));
    settings_section(ui, "Discord", None, None, |ui| {
        setting_row(ui, "Rich Presence", Some("Show the song you're playing on your Discord profile."), |ui| {
            toggle(ui, &mut enabled, "Rich Presence");
        });
        reveal(ui, "discord-settings-top", shown, |ui| {
            divider(ui);
            setting_row(ui, "Show when paused", Some("Keep the card up while the song is paused. Discord has no paused state, so the time goes rather than keep running."), |ui| {
                toggle(ui, &mut prefs.when_paused, "Show when paused");
            });
            divider(ui);
            setting_row(ui, "Hide details", Some("Say only that music is playing. No title, artist, album, artwork, time or buttons leave your computer."), |ui| {
                toggle(ui, &mut prefs.hide_details, "Hide details");
            });
        });
    });
    if !filtering() && shown > 0.0 && prefs != Prefs::default() {
        ui.scope(|ui| {
            ui.multiply_opacity(shown);
            if reset_link(ui, heading) {
                prefs = Prefs::default();
            }
        });
    }
    reveal(ui, "discord-settings-rest", shown, |ui| rest(app, ui, &mut prefs));
    if enabled != app.discord_enabled {
        app.set_discord_enabled(enabled);
    }
    // `sync_discord` sends what changed.
    app.discord_prefs = prefs;
}

/// Everything under the Rich Presence switch's section, which shows only while it's on.
fn rest(app: &App, ui: &mut Ui, prefs: &mut Prefs) {
    if !filtering() {
        preview(app, ui, prefs);
    }
    let on = !prefs.hide_details;
    settings_section(ui, "Discord status", None, None, |ui| {
        ui.add_enabled_ui(on, |ui| {
            setting_row(ui, "Status text", Some("Which line comes first under your name in the member list. The profile card always shows the app's name."), |ui| {
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
        let desc = format!("Replaces \"{}\" wherever the app's name shows. Leave it empty to keep that.", discord::APP_NAME);
        setting_row(ui, "App name", Some(&desc), |ui| {
            name_field(ui, &mut prefs.app_name);
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
            setting_row_wide(ui, "Line 2", Some("The line below it."), LINKED_W, |ui| {
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
                setting_row(ui, "Badge", Some("A small picture on the artwork's corner: the artist's, or the app icon picked in Appearance."), |ui| {
                    dropdown(ui, "Badge", &mut prefs.badge, &BadgeShows::ALL, |b| b.label().to_owned());
                });
                divider(ui);
                setting_row(ui, "Line 3", Some("The last line. Discord also shows it when the artwork is hovered, so it needs the artwork."), |ui| {
                    dropdown(ui, "Line 3", &mut prefs.lines[2], &Show::ALL, |s| s.label().to_owned());
                });
            });
            divider(ui);
            setting_row(ui, "Time", Some("Show how far into the song you are. Discord's full profile draws it as a progress bar."), |ui| {
                toggle(ui, &mut prefs.progress, "Time");
            });
        });
    });
    let note = "Discord shows at most two, and never on your own profile: only others see them. A button for a page the song doesn't have is left out, and Listen together shows only while you're in a room.";
    settings_section(ui, "Discord buttons", None, Some(note), |ui| {
        ui.add_enabled_ui(on, |ui| {
            setting_row(ui, "Buttons", Some("Put buttons under the card, such as one that opens the song on YouTube Music."), |ui| {
                toggle(ui, &mut prefs.show_buttons, "Buttons");
            });
            ui.add_enabled_ui(prefs.show_buttons, |ui| {
                divider(ui);
                setting_row(ui, "First button", None, |ui| {
                    dropdown(ui, "First button", &mut prefs.buttons[0], &Page::ALL, |p| p.label().to_owned());
                });
                divider(ui);
                setting_row(ui, "Second button", None, |ui| {
                    dropdown(ui, "Second button", &mut prefs.buttons[1], &Page::ALL, |p| p.label().to_owned());
                });
            });
        });
    });
}

/// `body`, faded and folded to `shown` of its height: 0 leaves it out, 1 draws it as is.
/// Its full height is remembered from when it was last drawn.
fn reveal(ui: &mut Ui, id: &str, shown: f32, body: impl FnOnce(&mut Ui)) {
    let id = Id::unique(id);
    if shown >= 1.0 {
        let top = ui.cursor().top();
        body(ui);
        let h = ui.min_rect().bottom() - top;
        ui.ctx().data_mut(|d| d.insert_temp(id, h));
        return;
    }
    if shown <= 0.0 {
        return;
    }
    let top = ui.cursor().min;
    let w = ui.available_width();
    let mut child = ui.new_child(UiBuilder::new().max_rect(Rect::from_min_size(top, vec2(w, f32::INFINITY))).layout(Layout::top_down(egui::Align::Min)));
    child.multiply_opacity(shown);
    // Never drawn whole yet, it's measured as it's drawn.
    let full = ui.ctx().data(|d| d.get_temp::<f32>(id));
    if let Some(full) = full {
        child.set_clip_rect(ui.clip_rect().intersect(Rect::from_min_size(top - vec2(8.0, 0.0), vec2(w + 16.0, full * shown))));
    }
    body(&mut child);
    let full = full.unwrap_or_else(|| child.min_rect().bottom() - top.y);
    ui.ctx().data_mut(|d| d.insert_temp(id, full));
    ui.advance_cursor_after_rect(Rect::from_min_size(top, vec2(w, full * shown)));
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
fn example(app_icon: &'static str) -> Presence {
    let now = now_ms();
    Presence {
        video_id: String::new(),
        title: "Song title".into(),
        artist: "Artist".into(),
        artist_id: None,
        album: Some("Album".into()),
        album_id: None,
        art: Some(discord::ICON.into()),
        artist_pic: None,
        app_icon,
        start_ms: now - 79_000,
        end_ms: Some(now + 126_000),
        paused: false,
        jam: None,
    }
}

/// The profile card and member list as Discord would show them, under the Discord section.
fn preview(app: &App, ui: &mut Ui, prefs: &Prefs) {
    let (p, state) = shown(app);
    section_label(ui, "Preview", Some(state));
    // Paused, the card is drawn as it would be kept up.
    let card = Card::new(&p, prefs);
    let notes = notes(&p, &card, prefs);
    let name = app.account_name.as_deref().unwrap_or("You");
    settings_card(ui, |ui| {
        ui.add_space(16.0);
        let w = ui.available_width();
        let wide = w >= 600.0;
        let card_w = if wide { CARD_W } else { w.min(380.0) };
        let side = |ui: &mut Ui| {
            caption(ui, "Member list");
            member_row(ui, &card, name, app.account_avatar.as_deref(), ui.available_width().min(380.0));
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
            // Stacked, in a column down the middle.
            ui.horizontal_top(|ui| {
                ui.add_space(((w - card_w) / 2.0).max(0.0));
                ui.vertical(|ui| {
                    ui.set_width(card_w);
                    caption(ui, "Profile card");
                    profile_card(ui, &card, card_w);
                    ui.add_space(14.0);
                    side(ui);
                });
            });
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
fn shown(app: &App) -> (Presence, &'static str) {
    let p = app.discord_presence();
    let state = match &p {
        None => "Example",
        Some(p) if p.paused => "Paused",
        Some(_) => "Now playing",
    };
    (p.unwrap_or_else(|| example(app.app_icon)), state)
}

/// What the preview can't show by itself.
fn notes(p: &Presence, card: &Card, prefs: &Prefs) -> Vec<&'static str> {
    let mut notes = Vec::new();
    if p.paused && !prefs.when_paused {
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
const TIME: f32 = 22.0;
const BUTTON: f32 = 32.0;
const GAP: f32 = 8.0;

/// Discord's green: the online dot, and the music note before what's playing.
const GREEN: Color32 = Color32::from_rgb(0x23, 0xa5, 0x5a);

fn profile_card(ui: &mut Ui, c: &Card, width: f32) {
    let lines: Vec<_> = c.lines.iter().flatten().collect();
    let text_h = lines.len() as f32 * LINE + if c.times.is_some() { TIME } else { 0.0 };
    let body_h = if c.art.is_some() { ART.max(text_h) } else { text_h };
    let buttons_h = c.buttons.len() as f32 * (BUTTON + GAP);
    let h = PAD + 18.0 + if body_h > 0.0 { 10.0 + body_h } else { 0.0 } + if buttons_h > 0.0 { 12.0 + buttons_h - GAP } else { 0.0 } + PAD;
    let (rect, _) = ui.allocate_exact_size(vec2(width, h), Sense::hover());
    ui.painter().rect_filled(rect, 10.0, theme::glass(0.07));
    let inner_w = width - 2.0 * PAD;
    let mut y = rect.top() + PAD;
    widgets::text_at(ui, pos2(rect.left() + PAD, y), &format!("Listening to {}", c.name()), theme::semibold(12.5), theme::text_dim(), inner_w - 28.0);
    // The card's ··· menu.
    for i in 0..3 {
        ui.painter().circle_filled(pos2(rect.right() - PAD - 2.0 - 6.0 * i as f32, y + 9.0), 1.6, theme::text_dim());
    }
    y += 18.0 + 10.0;
    let mut x = rect.left() + PAD;
    if let Some(art) = &c.art {
        let r = Rect::from_min_size(pos2(x, y), Vec2::splat(ART));
        ui.painter().rect_filled(r, 8.0, theme::glass(0.08));
        widgets::paint_remote(ui, art, r, 8.0, full_uv(), Color32::WHITE);
        if let Some(pic) = &c.badge {
            let b = Rect::from_center_size(r.right_bottom() - vec2(3.0, 3.0), Vec2::splat(22.0));
            ui.painter().circle_filled(b.center(), 13.5, Color32::from_black_alpha(200));
            ui.painter().circle_filled(b.center(), 11.0, theme::glass(0.12));
            // The app icon's badge is in the app: the website may not serve it yet.
            match crate::appicon::Variant::by_badge_url(&pic.url) {
                Some(v) => egui::Image::from_bytes(format!("bytes://discord-badge/{}.png", v.id), v.badge).corner_radius(11.0).paint_at(ui, b),
                None => widgets::paint_remote(ui, &pic.url, b, 11.0, full_uv(), Color32::WHITE),
            }
        }
        x += ART + 12.0;
    }
    let tw = rect.right() - PAD - x;
    let mut ty = y + ((body_h - text_h) / 2.0).max(0.0);
    for (i, l) in lines.iter().enumerate() {
        let (font, color) = if i == 0 && c.lines[0].is_some() { (theme::bold(15.0), theme::text()) } else { (theme::regular(13.5), theme::text_dim()) };
        widgets::text_at(ui, pos2(x, ty), &l.text, font, color, tw);
        ty += LINE;
    }
    if let Some((start, _)) = c.times {
        elapsed(ui, pos2(x, ty + TIME / 2.0), start);
    }
    y += body_h + 12.0;
    for (label, _) in &c.buttons {
        let r = Rect::from_min_size(pos2(rect.left() + PAD, y), vec2(inner_w, BUTTON));
        ui.painter().rect_filled(r, 8.0, theme::glass(0.10));
        ui.painter().text(r.center(), Align2::CENTER_CENTER, *label, theme::semibold(13.5), theme::text());
        y += BUTTON + GAP;
    }
}

/// "♫ 1:04": how long the song has been playing, left-centred on `at`.
fn elapsed(ui: &Ui, at: egui::Pos2, start: i64) {
    let secs = ((now_ms() - start) / 1000).max(0) as u32;
    widgets::icon_at(ui, at + vec2(6.0, 0.0), icon::MUSIC, 13.0, GREEN);
    ui.painter().text(at + vec2(17.0, 0.0), Align2::LEFT_CENTER, super::fmt_time(secs), theme::semibold(12.5), GREEN);
}

fn member_row(ui: &mut Ui, c: &Card, name: &str, avatar: Option<&str>, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 52.0), Sense::hover());
    ui.painter().rect_filled(rect, 8.0, theme::glass(0.07));
    let pic = Rect::from_center_size(pos2(rect.left() + 26.0, rect.center().y), Vec2::splat(34.0));
    let accent = ui.ctx().data(|d| d.get_temp::<Color32>(egui::Id::unique("accent"))).unwrap_or(theme::RED);
    ui.painter().circle_filled(pic.center(), 17.0, accent);
    if let Some(url) = avatar {
        widgets::paint_remote(ui, url, pic, 17.0, full_uv(), Color32::WHITE);
    }
    // Online: a green dot in a ring cut out of the picture.
    let dot = pic.right_bottom() - vec2(4.0, 4.0);
    ui.painter().circle_filled(dot, 7.0, Color32::from_black_alpha(220));
    ui.painter().circle_filled(dot, 4.5, GREEN);
    let x = pic.right() + 10.0;
    let w = rect.right() - 10.0 - x;
    widgets::text_at(ui, pos2(x, rect.top() + 8.0), name, theme::semibold(14.0), theme::text(), w);
    let y = rect.top() + 36.0;
    widgets::icon_at(ui, pos2(x + 6.0, y), icon::MUSIC, 12.0, GREEN);
    widgets::text_at(ui, pos2(x + 17.0, y - 8.0), &c.member_text(), theme::regular(12.5), theme::text_dim(), w - 17.0);
}

fn full_uv() -> Rect {
    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))
}
