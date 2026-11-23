//! The Spotify transfer: its page (pick, running, review, done), the Settings section
//! that opens it, and the sidebar card that follows it while you browse.

use std::sync::atomic::Ordering;

use egui::{Align2, Color32, Rect, RichText, Sense, Ui, pos2, vec2};

use super::pages::{filter_show, page_title, setting_row, settings_card, settings_section as section};
use super::widgets::{self, icon_at, named, paint_cover, pill, text_at};
use super::{Action, App, LibTab, Page};
use crate::model::{Target, Thumb, Track as YtTrack};
use crate::theme::{self, icon};
use crate::transfer::spotify::{Entry, Source, Track};
use crate::transfer::{Account, Decision, Dest, Job, Phase, Search, is_likes};

/// Spotify's green, for its badge.
const SPOTIFY: Color32 = Color32::from_rgb(30, 215, 96);

/// What's being set up on the start screen (kept while browsing elsewhere).
#[derive(Default)]
pub struct Setup {
    /// The chosen library entry, by source.
    picked: Option<Source>,
    link: String,
    /// Playlist name; follows the picked source until edited.
    name: String,
    name_for: Option<Source>,
    /// For Liked Songs (or a playlist called Likes): add to Liked Music instead of a playlist.
    to_liked: bool,
    /// Running list filter.
    tab: Tab,
    /// The candidate chosen for the doubt on screen: (doubt, candidate).
    pick: Option<(usize, usize)>,
    /// Phase changes already announced (toast, library refresh).
    seen: Option<Phase>,
    /// Dev hooks applied (YTM_TRANSFER_LINK / _START).
    dev: bool,
    /// When the transfer finished (app time); the sidebar card leaves a few seconds later.
    done_at: Option<f64>,
    /// The song shown in the review (its place in the review), when one was picked from the list.
    focus: Option<usize>,
    /// The review list shows the songs that matched instead of the ones to check.
    show_matched: bool,
    /// Height of the review card last frame, for the list beside it.
    card_h: f32,
    /// The song the review list last scrolled to.
    scrolled_to: Option<usize>,
    /// When the last review decision was made (app time), so a double click decides one song, not two.
    decided_at: f64,
    /// The song being matched on the search page (its place in the review); cleared on
    /// leaving search.
    pub picking: Option<usize>,
    /// The review is open; until then the song list stays up so the matches can be looked over.
    reviewing: bool,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum Tab {
    #[default]
    All,
    Matched,
    Look,
    Missing,
}

fn spotify_thumbs(art: &Option<String>) -> Vec<Thumb> {
    art.iter().map(|u| Thumb { url: u.clone(), width: 64, height: 64 }).collect()
}

/// The Spotify logo: a green rounded square with its three arcs.
fn badge(ui: &Ui, rect: Rect) {
    ui.painter().rect_filled(rect, rect.width() * 0.27, SPOTIFY);
    let s = rect.width() / 24.0;
    let c = rect.center();
    let ink = Color32::from_rgb(11, 11, 11);
    for (i, (w, y, curve)) in [(14.0, -3.2, 2.0), (11.6, 0.6, 1.6), (9.0, 4.0, 1.2)].into_iter().enumerate() {
        let a = pos2(c.x - w / 2.0 * s, c.y + y * s);
        let b = pos2(c.x + w / 2.0 * s, c.y + (y + 1.2) * s);
        let mid = pos2(c.x, c.y + (y - curve) * s);
        let pts: Vec<egui::Pos2> = (0..=12)
            .map(|k| {
                let t = k as f32 / 12.0;
                let u = 1.0 - t;
                pos2(u * u * a.x + 2.0 * u * t * mid.x + t * t * b.x, u * u * a.y + 2.0 * u * t * mid.y + t * t * b.y)
            })
            .collect();
        ui.painter().add(egui::Shape::line(pts, egui::Stroke::new((2.3 - 0.25 * i as f32) * s, ink)));
    }
}

fn radio(ui: &Ui, center: egui::Pos2, on: f32, accent: Color32) {
    ui.painter().circle_stroke(center, 9.0, egui::Stroke::new(2.0, theme::lerp_color(theme::glass(0.35), accent, on)));
    if on > 0.0 {
        ui.painter().circle_filled(center, 5.0 * on, accent);
    }
}

fn small_caps(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).font(theme::bold(11.5)).color(theme::text_faint()));
    ui.add_space(8.0);
}

fn dim(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).font(theme::regular(13.0)).color(theme::text_dim()));
}

/// A tag like "VIDEO" or "LOCAL FILE".
fn tag(ui: &Ui, right: egui::Pos2, text: &str, fill: Color32) -> f32 {
    let g = widgets::galley(ui, text, theme::bold(10.5), theme::text(), 200.0);
    let rect = Rect::from_min_size(pos2(right.x - g.size().x - 14.0, right.y - 10.0), vec2(g.size().x + 14.0, 20.0));
    ui.painter().rect_filled(rect, 5.0, fill);
    ui.painter().galley(rect.center() - g.size() / 2.0, g, theme::text());
    rect.left()
}

fn big_button(ui: &mut Ui, label: &str, accent: Color32, enabled: bool, width: f32) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 50.0), if enabled { Sense::click() } else { Sense::hover() });
    let resp = named(resp, label);
    let resp = if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() && enabled, 0.15);
    let fill = if enabled { theme::lerp_color(accent, Color32::WHITE, 0.12 * h) } else { theme::glass(0.10) };
    ui.painter().rect_filled(rect, 25.0, fill);
    let fg = if enabled { widgets::contrast(accent) } else { theme::text_faint() };
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, label, theme::bold(16.0), fg);
    enabled && resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

fn breadcrumb(app: &mut App, ui: &mut Ui) {
    let g = widgets::galley(ui, &[crate::tr!("settings-title"), crate::tr!("settings-general"), crate::tr!("transfer-title")].join(" › "), theme::regular(13.0), theme::text_dim(), 400.0);
    let (rect, resp) = ui.allocate_exact_size(g.size(), Sense::click());
    let resp = named(resp, crate::tr!("transfer-back-settings")).on_hover_cursor(egui::CursorIcon::PointingHand);
    let c = if resp.hovered() { theme::text() } else { theme::text_dim() };
    ui.painter().galley(rect.min, g, c);
    if resp.clicked() {
        app.settings_tab = super::pages::SettingsTab::General;
        app.actions.push(Action::Go(Page::Settings));
    }
}

fn fmt_n(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn numbered(label: &str, count: usize) -> String {
    crate::tr!("transfer-numbered-label", label = label, count = fmt_n(count))
}

fn progress_of(count: usize, state: &str) -> String {
    crate::tr!("transfer-progress-of", count = fmt_n(count), state = state)
}

fn fmt_secs(s: u64) -> String {
    match s {
        0..60 => crate::tr!("transfer-seconds", count = s),
        _ if s.is_multiple_of(60) || s >= 600 => crate::tr!("transfer-minutes", count = (s + 30) / 60),
        _ => crate::tr!("transfer-minutes-seconds", minutes = s / 60, seconds = s % 60),
    }
}

// --------------------------------------------------------------------- the page

pub fn page(app: &mut App, ui: &mut Ui, now: f64) {
    let xfer = app.transfer.clone();
    xfer.ensure_account(&app.backend);
    let avail = ui.available_width();
    let w = avail.min(1080.0);
    ui.horizontal(|ui| {
        ui.add_space(((avail - w) / 2.0).max(0.0));
        ui.vertical(|ui| {
            ui.set_width(w);
            let guard = xfer.job.lock().unwrap();
            let phase = guard.as_ref().map(|j| j.phase.clone());
            drop(guard);
            match phase {
                None => setup(app, ui),
                Some(Phase::Review) if app.xfer.reviewing => review(app, ui),
                Some(Phase::Done) => done(app, ui),
                Some(Phase::Failed(e)) => failed(app, ui, &e),
                Some(_) => running(app, ui, now),
            }
        });
    });
}

// ---------------------------------------------------------------- 1. pick what to copy

fn setup(app: &mut App, ui: &mut Ui) {
    let xfer = app.transfer.clone();
    let accent = app.accent_color();
    ui.add_space(6.0);
    breadcrumb(app, ui);
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
        badge(ui, r);
        ui.add_space(6.0);
        ui.label(RichText::new(crate::tr!("transfer-title")).font(theme::bold(30.0)).color(theme::text()));
    });
    ui.add_space(4.0);
    dim(ui, crate::tr!("transfer-intro"));
    ui.add_space(18.0);

    if !app.logged_in {
        settings_card(ui, |ui| {
            setting_row(ui, crate::tr!("transfer-youtube-sign-in"), Some(crate::tr!("transfer-youtube-account")), |ui| {
                if pill(ui, crate::tr!("transfer-go-settings"), None, true).clicked() {
                    app.settings_tab = super::pages::SettingsTab::Account;
                    app.actions.push(Action::Go(Page::Settings));
                }
            });
        });
        return;
    }

    // An earlier transfer that stopped part way.
    let resumable = xfer.resumable.lock().unwrap().clone();
    if let Some(r) = resumable {
        let mut go = None;
        settings_card(ui, |ui| {
            let desc = crate::tr!("transfer-resume-progress", done = fmt_n(r.searched), total = fmt_n(r.total));
            setting_row(ui, &crate::tr!("transfer-pick-up", name = r.name.as_str()), Some(&desc), |ui| {
                if pill(ui, crate::tr!("transfer-discard"), None, false).clicked() {
                    go = Some(false);
                }
                ui.add_space(8.0);
                if pill(ui, crate::tr!("transfer-resume"), None, true).clicked() {
                    go = Some(true);
                }
            });
        });
        if !app.xfer.dev && super::dev_var("YTM_TRANSFER_START") == Some("resume") {
            app.xfer.dev = true;
            go = Some(true);
        }
        match go {
            Some(true) => xfer.start(&app.backend, r.source, r.dest, true),
            Some(false) => xfer.discard_saved(&r.source),
            None => {}
        }
        ui.add_space(18.0);
    }

    let account = xfer.account.lock().unwrap().clone();
    let library: &[Entry] = match &account {
        Account::SignedIn { library, .. } => library,
        _ => &[],
    };
    // The pasted link, once looked up, is a choice like the library's.
    let link_entry = {
        if !app.xfer.link.trim().is_empty() {
            xfer.look_up(&app.backend, &app.xfer.link);
        }
        xfer.lookup.lock().unwrap().clone().filter(|(l, _)| *l == app.xfer.link.trim())
    };
    let pasted = link_entry.as_ref().and_then(|(_, r)| r.as_ref()?.as_ref().ok().cloned());
    let chosen: Option<Entry> = match &app.xfer.picked {
        Some(s) => library.iter().chain(pasted.iter()).find(|e| &e.source == s).cloned(),
        None => None,
    };
    let chosen = chosen.or_else(|| pasted.clone());

    let wide = ui.available_width() > 820.0;
    let gap = 18.0;
    let left_w = if wide { (ui.available_width() - gap) * 0.58 } else { ui.available_width() };
    let mut start = false;
    let mut body = |ui: &mut Ui, col: usize| {
        if col == 0 {
            settings_card(ui, |ui| {
                ui.add_space(14.0);
                small_caps(ui, "1 · WHAT TO COPY");
                match &account {
                    Account::SignedIn { .. } if library.is_empty() => dim(ui, crate::tr!("transfer-empty-library")),
                    Account::SignedIn { .. } => {
                        // Five rows show at a time; the rest scroll, so a big library doesn't
                        // stretch the page.
                        let row = 64.0 + ui.spacing().item_spacing.y;
                        let h = row * library.len().min(5) as f32 - ui.spacing().item_spacing.y;
                        egui::ScrollArea::vertical()
                            .id_salt("xfer-library")
                            .auto_shrink([false, true])
                            .min_scrolled_height(h)
                            .max_height(h)
                            .show_rows(ui, 64.0, library.len(), |ui, range| {
                                for e in &library[range] {
                                    source_row(app, ui, e, chosen.as_ref().is_some_and(|c| c.source == e.source), accent);
                                }
                            });
                    }
                    Account::Unknown | Account::Loading => {
                        ui.horizontal(|ui| {
                            widgets::loader(ui);
                            dim(ui, crate::tr!("transfer-loading-library"));
                        });
                    }
                    Account::SignedOut | Account::Failed(_) => {
                        dim(ui, crate::tr!("transfer-spotify-sign-in-desc"));
                    }
                }
                ui.add_space(14.0);
                dim(ui, crate::tr!("transfer-paste-link"));
                ui.add_space(6.0);
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 42.0), Sense::hover());
                ui.painter().rect_filled(rect, 10.0, theme::glass(0.08));
                let inner = rect.shrink2(vec2(14.0, 4.0));
                let edit = ui.put(
                    inner,
                    egui::TextEdit::singleline(&mut app.xfer.link)
                        .hint_text(RichText::new("https://open.spotify.com/playlist/…").color(theme::text_faint()))
                        .font(theme::regular(14.0))
                        .frame(egui::Frame::NONE)
                        .align(egui::Align2::LEFT_CENTER)
                        .desired_width(inner.width()),
                );
                if edit.changed() {
                    app.xfer.picked = None;
                }
                match &link_entry {
                    Some((_, None)) => {
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            widgets::loader(ui);
                            dim(ui, crate::tr!("transfer-looking-up"));
                        });
                    }
                    Some((_, Some(Err(e)))) => {
                        ui.add_space(8.0);
                        ui.label(RichText::new(e).font(theme::regular(13.0)).color(theme::RED));
                    }
                    Some((_, Some(Ok(e)))) => {
                        ui.add_space(8.0);
                        source_row(app, ui, e, chosen.as_ref().is_some_and(|c| c.source == e.source), accent);
                    }
                    None => {}
                }
                ui.add_space(14.0);
            });
        } else {
            settings_card(ui, |ui| {
                ui.add_space(14.0);
                small_caps(ui, "2 · SPOTIFY ACCOUNT");
                spotify_account_row(app, ui, &account, true);
                ui.add_space(10.0);
            });
            ui.add_space(gap);
            settings_card(ui, |ui| {
                ui.add_space(14.0);
                destination(app, ui, chosen.as_ref(), accent);
                ui.add_space(14.0);
            });
            ui.add_space(gap);
            // Liked Songs are only readable signed in.
            let needs_sign_in = chosen.as_ref().is_some_and(|c| c.source == Source::Liked) && !matches!(account, Account::SignedIn { .. });
            let ready = chosen.is_some() && !needs_sign_in;
            if big_button(ui, crate::tr!("transfer-start"), accent, ready, ui.available_width()) {
                start = true;
            }
            ui.add_space(8.0);
            let estimate = match chosen.as_ref().and_then(|c| c.count) {
                _ if needs_sign_in => crate::tr!("transfer-sign-in-liked").to_owned(),
                Some(n) => {
                    let secs = (n as u64 / 20).max(5);
                    crate::tr!("transfer-estimate", time = fmt_secs(secs), count = fmt_n(n))
                }
                None if chosen.is_some() => crate::tr!("transfer-listen-while-running").to_owned(),
                None => crate::tr!("transfer-pick-playlist").to_owned(),
            };
            ui.vertical_centered(|ui| dim(ui, &estimate));
        }
    };
    if wide {
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(left_w);
                body(ui, 0);
            });
            ui.add_space(gap);
            ui.vertical(|ui| {
                ui.set_width(ui.available_width());
                body(ui, 1);
            });
        });
    } else {
        body(ui, 0);
        ui.add_space(gap);
        body(ui, 1);
    }
    // Dev hook: YTM_TRANSFER_START=liked|playlist starts as soon as something is chosen.
    if !app.xfer.dev
        && let Some(how) = super::dev_var("YTM_TRANSFER_START")
        && chosen.is_some()
    {
        app.xfer.dev = true;
        app.xfer.to_liked = how == "liked";
        start = true;
    }
    if start && let Some(c) = chosen {
        // (YTM_TRANSFER_START=liked tests Liked Music with any playlist.)
        let liked = app.xfer.to_liked && (likes_like(&c) || super::dev_var("YTM_TRANSFER_START") == Some("liked"));
        let dest = if liked { Dest::Liked } else { Dest::Playlist(app.xfer.name.trim().to_owned()) };
        xfer.start(&app.backend, c.source.clone(), dest, false);
        app.xfer.tab = Tab::All;
        app.xfer.pick = None;
        app.xfer.focus = None;
        app.xfer.scrolled_to = None;
        app.xfer.show_matched = false;
        app.xfer.seen = None;
    }
}

/// Liked Songs, or a playlist called that: offered as Liked Music.
fn likes_like(e: &Entry) -> bool {
    e.source == Source::Liked || is_likes(&e.name)
}

fn source_row(app: &mut App, ui: &mut Ui, e: &Entry, selected: bool, accent: Color32) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 64.0), Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let resp = named(resp, &e.name).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
    let s = theme::anim_bool(ui.ctx(), resp.id.with("s"), selected, 0.15);
    ui.painter().rect_filled(rect, 12.0, theme::glass(0.04 * h + 0.06 * s));
    if s > 0.0 {
        ui.painter().rect_stroke(rect, 12.0, egui::Stroke::new(1.5, theme::with_alpha(accent, s)), egui::StrokeKind::Inside);
    }
    let img = Rect::from_min_size(pos2(rect.left() + 10.0, rect.center().y - 24.0), vec2(48.0, 48.0));
    if e.source == Source::Liked {
        ui.painter().rect_filled(img, 8.0, Color32::from_rgb(80, 56, 200));
        widgets::heart(ui, img.center(), 22.0, 1.0, Color32::WHITE, Color32::WHITE);
    } else {
        paint_cover(ui, img, &spotify_thumbs(&e.art), 8.0, 1.0);
    }
    let x = img.right() + 14.0;
    let tw = rect.right() - x - 44.0;
    text_at(ui, pos2(x, rect.center().y - 19.0), &e.name, theme::semibold(15.0), theme::text(), tw);
    let meta = match (&e.source, e.count) {
        (Source::Liked, Some(n)) => numbered(crate::tr!("nav-songs"), n),
        (Source::Liked, None) => crate::tr!("nav-liked-songs").to_owned(),
        (_, Some(n)) if !e.owner.is_empty() => format!("{} · {} · {}", crate::tr!("transfer-playlist"), e.owner, numbered(crate::tr!("nav-songs"), n)),
        (_, Some(n)) => format!("{} · {}", crate::tr!("transfer-playlist"), numbered(crate::tr!("nav-songs"), n)),
        _ if !e.owner.is_empty() => format!("{} · {}", crate::tr!("transfer-playlist"), e.owner),
        _ => crate::tr!("transfer-playlist").to_owned(),
    };
    text_at(ui, pos2(x, rect.center().y + 2.0), &meta, theme::regular(13.0), theme::text_dim(), tw);
    radio(ui, pos2(rect.right() - 22.0, rect.center().y), s, accent);
    if resp.clicked() {
        app.xfer.picked = Some(e.source.clone());
    }
}

fn spotify_account_row(app: &mut App, ui: &mut Ui, account: &Account, compact: bool) {
    let xfer = app.transfer.clone();
    let busy = xfer.signing_in.load(Ordering::Relaxed);
    let (mut sign_in, mut sign_out) = (false, false);
    let (title, desc) = match account {
        Account::SignedIn { name, .. } if compact => (
            if name.is_empty() { crate::tr!("status-signed-in").to_owned() } else { name.clone() },
            crate::tr!("transfer-signed-in-plan").to_owned(),
        ),
        Account::SignedIn { name, .. } => {
            let who = if name.is_empty() { crate::tr!("status-signed-in").to_owned() } else { crate::tr!("transfer-signed-in-as", name = name.as_str()) };
            (crate::tr!("transfer-spotify-account").to_owned(), format!("{who}. {}", crate::tr!("transfer-credential-storage")))
        }
        Account::Unknown | Account::Loading => (crate::tr!("transfer-spotify-account").to_owned(), crate::tr!("transfer-checking-sign-in").to_owned()),
        Account::SignedOut if compact => (crate::tr!("settings-not-signed-in").to_owned(), crate::tr!("transfer-private-sign-in").to_owned()),
        Account::SignedOut => (crate::tr!("transfer-spotify-account").to_owned(), format!("{}. {}", crate::tr!("settings-not-signed-in"), crate::tr!("transfer-private-sign-in"))),
        Account::Failed(e) => (crate::tr!("transfer-spotify-account").to_owned(), e.clone()),
    };
    let signed_in = matches!(account, Account::SignedIn { .. } | Account::Unknown | Account::Loading);
    setting_row(ui, &title, Some(&desc), |ui| {
        if busy {
            widgets::loader(ui);
        } else if signed_in {
            sign_out = pill(ui, if compact { crate::tr!("settings-change") } else { crate::tr!("settings-sign-out") }, None, false).clicked();
        } else {
            sign_in = pill(ui, crate::tr!("transfer-sign-in-spotify"), None, true).clicked();
        }
    });
    if busy {
        dim(ui, crate::tr!("settings-sign-in-finish"));
    }
    if sign_out {
        xfer.set_cookie(None, &app.backend);
        app.xfer.picked = None;
        // "Change" signs straight in again.
        if compact {
            sign_in = true;
        }
    }
    if sign_in {
        app.start_spotify_sign_in(ui.ctx());
    }
}

fn destination(app: &mut App, ui: &mut Ui, chosen: Option<&Entry>, accent: Color32) {
    // The name follows the picked playlist until it's typed over.
    let src = chosen.map(|c| c.source.clone());
    if app.xfer.name_for != src {
        if let Some(c) = chosen {
            app.xfer.name = crate::tr!("transfer-from-spotify", name = c.name.as_str());
            app.xfer.to_liked = likes_like(c);
        }
        app.xfer.name_for = src;
    }
    let liked_offer = chosen.is_some_and(likes_like);
    if liked_offer {
        small_caps(ui, "3 · WHERE SHOULD THEY GO?");
        for (to_liked, title, desc) in [
            (true, crate::tr!("transfer-dest-liked"), crate::tr!("transfer-dest-liked-desc")),
            (false, crate::tr!("transfer-dest-new"), crate::tr!("transfer-private-playlist")),
        ] {
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 62.0), Sense::click());
            let resp = named(resp, title).on_hover_cursor(egui::CursorIcon::PointingHand);
            let on = theme::anim_bool(ui.ctx(), resp.id.with("on"), app.xfer.to_liked == to_liked, 0.15);
            let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
            ui.painter().rect_filled(rect, 10.0, theme::glass(0.03 * h + 0.05 * on));
            radio(ui, pos2(rect.left() + 18.0, rect.center().y), on, accent);
            let x = rect.left() + 38.0;
            text_at(ui, pos2(x, rect.top() + 11.0), title, theme::semibold(14.5), theme::text(), rect.width() - 46.0);
            let g = widgets::galley_wrapped(ui, desc, theme::regular(12.5), theme::text_dim(), rect.width() - 46.0, 2);
            let pos = pos2(x, rect.top() + 31.0);
            ui.painter().galley(pos, g.clone(), theme::text_dim());
            widgets::tip_if_elided(ui, Rect::from_min_size(pos, g.size()), &g, desc);
            if resp.clicked() {
                app.xfer.to_liked = to_liked;
            }
        }
        if app.xfer.to_liked {
            return;
        }
        ui.add_space(10.0);
    } else {
        small_caps(ui, "3 · NEW YOUTUBE MUSIC PLAYLIST");
    }
    dim(ui, crate::tr!("dialog-name"));
    ui.add_space(6.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 42.0), Sense::hover());
    ui.painter().rect_filled(rect, 10.0, theme::glass(0.08));
    let inner = rect.shrink2(vec2(14.0, 4.0));
    ui.put(
        inner,
        egui::TextEdit::singleline(&mut app.xfer.name)
            .hint_text(RichText::new(crate::tr!("transfer-name-hint")).color(theme::text_faint()))
            .font(theme::regular(14.0))
            .frame(egui::Frame::NONE)
            .align(egui::Align2::LEFT_CENTER)
            .desired_width(inner.width()),
    );
    ui.add_space(10.0);
    dim(ui, crate::tr!("dialog-private-desc"));
}

// ------------------------------------------------------------------- 2. running

fn running(app: &mut App, ui: &mut Ui, _now: f64) {
    let xfer = app.transfer.clone();
    let accent = app.accent_color();
    let guard = xfer.job.lock().unwrap();
    let Some(job) = guard.as_ref() else { return };

    ui.add_space(6.0);
    breadcrumb(app, ui);
    let title = match job.phase {
        Phase::Reading if job.name.is_empty() => crate::tr!("transfer-reading-spotify").to_owned(),
        Phase::Reading => crate::tr!("transfer-reading-name", name = job.name.as_str()),
        Phase::Saving => match &job.dest {
            Dest::Liked => crate::tr!("transfer-adding-liked").to_owned(),
            Dest::Playlist(_) => crate::tr!("transfer-creating-playlist").to_owned(),
        },
        _ => crate::tr!("transfer-copying-name", name = job.name.as_str()),
    };
    let (mut pause, mut cancel, mut check) = (false, false, false);
    let paused = job.paused;
    // Searching is done and some songs need a look: they wait until the user starts on them.
    let to_check = (job.phase == Phase::Review).then(|| job.doubts.iter().filter(|d| d.decision.is_none()).count());
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_width((ui.available_width() - if to_check.is_some() { 400.0 } else { 230.0 }).max(200.0));
            ui.add_space(4.0);
            ui.label(RichText::new(&title).font(theme::bold(28.0)).color(theme::text()));
            ui.add_space(2.0);
            dim(
                ui,
                if to_check.is_some() {
                    crate::tr!("transfer-review-ready")
                } else {
                    crate::tr!("transfer-progress-background")
                },
            );
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            cancel = pill(ui, crate::tr!("ui-cancel"), None, false).clicked();
            ui.add_space(8.0);
            match to_check {
                Some(n) => {
                    let label = crate::tr!("transfer-check-count", count = fmt_n(n));
                    check = big_button(ui, &label, accent, true, 220.0);
                }
                None => pause = pill(ui, if paused { crate::tr!("transfer-resume") } else { crate::tr!("ui-pause") }, None, false).clicked(),
            }
        });
    });
    ui.add_space(16.0);

    // ---- progress card
    let total = job.tracks.len();
    let searched = job.searched();
    let (big, of, frac, eta) = match job.phase {
        Phase::Reading => (fmt_n(job.read.0), progress_of(job.read.1, crate::tr!("transfer-read-from-spotify")), job.read.0 as f32 / job.read.1.max(1) as f32, None),
        Phase::Saving if job.waiting => ("…".to_owned(), "waiting for the likes being changed in the app to finish".to_owned(), 0.0, None),
        Phase::Saving if let Some((done, of)) = job.redo => (
            fmt_n(done),
            progress_of(of, crate::tr!("transfer-readding-reason")),
            done as f32 / of.max(1) as f32,
            Some("checking Liked Music".to_owned()),
        ),
        Phase::Saving => (
            fmt_n(job.saving.0),
            match job.dest {
                Dest::Liked => progress_of(job.saving.1, crate::tr!("transfer-liked")),
                Dest::Playlist(_) => progress_of(job.saving.1, crate::tr!("transfer-added")),
            },
            job.saving.0 as f32 / job.saving.1.max(1) as f32,
            None,
        ),
        Phase::Checking => (fmt_n(searched), progress_of(total, crate::tr!("transfer-progress-searching")), 1.0, Some(crate::tr!("transfer-reviewing-doubtful").to_owned())),
        Phase::Review => (fmt_n(searched), progress_of(total, crate::tr!("transfer-progress-searching")), 1.0, None),
        _ => (fmt_n(searched), progress_of(total, crate::tr!("transfer-progress-searching")), searched as f32 / total.max(1) as f32, job.eta().map(|s| crate::tr!("transfer-remaining", time = fmt_secs(s)))),
    };
    let limited = app.backend.api().limited();
    let eta = if paused {
        Some("paused".to_owned())
    } else if limited && job.phase.running() {
        // YouTube Music refused requests: everything waits a little, then carries on.
        Some(crate::tr!("transfer-rate-limited").to_owned())
    } else {
        eta
    };
    if limited {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
    }
    settings_card(ui, |ui| {
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(&big).font(theme::bold(34.0)).color(theme::text()));
            ui.add_space(4.0);
            ui.label(RichText::new(&of).font(theme::regular(15.0)).color(theme::text_dim()));
            if let Some(e) = &eta {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| dim(ui, e));
            }
        });
        ui.add_space(10.0);
        let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 8.0), Sense::hover());
        ui.painter().rect_filled(bar, 4.0, theme::glass(0.12));
        let shown = theme::anim(ui.ctx(), "xfer-bar", frac.clamp(0.0, 1.0), 0.3);
        ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * shown, bar.height())), 4.0, accent);
        ui.add_space(12.0);
        // Steps
        let steps = [
            (Phase::Reading, match job.read_secs {
                Some(s) => format!("{} · {}", crate::tr!("transfer-read-from-spotify"), crate::tr!("transfer-seconds", count = format!("{s:.1}"))),
                None => crate::tr!("transfer-read-from-spotify").to_owned(),
            }),
            (Phase::Searching, crate::tr!("transfer-search-youtube").to_owned()),
            (Phase::Checking, crate::tr!("transfer-check-doubtful").to_owned()),
            (Phase::Saving, match &job.dest {
                Dest::Liked => crate::tr!("transfer-dest-liked").to_owned(),
                Dest::Playlist(_) => crate::tr!("transfer-create-playlist").to_owned(),
            }),
        ];
        let order = |p: &Phase| match p {
            Phase::Reading => 0,
            Phase::Searching => 1,
            Phase::Checking | Phase::Review => 2,
            _ => 3,
        };
        let at = order(&job.phase);
        ui.horizontal_wrapped(|ui| {
            for (i, (p, label)) in steps.iter().enumerate() {
                let k = order(p);
                if i > 0 {
                    ui.label(RichText::new("—").font(theme::regular(13.0)).color(theme::text_faint()));
                }
                let (text, font, color) = if k < at {
                    (format!("✓ {label}"), theme::regular(13.0), theme::text())
                } else if k == at {
                    (label.clone(), theme::semibold(13.0), theme::text())
                } else {
                    (label.clone(), theme::regular(13.0), theme::text_dim())
                };
                ui.label(RichText::new(text).font(font).color(color));
            }
        });
        ui.add_space(14.0);
    });
    ui.add_space(14.0);

    // ---- counts
    let found = job.count(|r| matches!(r, Search::Found(_)));
    let unsure = job.count(|r| matches!(r, Search::Unsure(_)));
    let missing = job.count(|r| *r == Search::Missing);
    let dups = duplicates(job);
    let stats = [
        (fmt_n(found), crate::tr!("transfer-matched"), false),
        (fmt_n(unsure), crate::tr!("transfer-needs-review"), unsure > 0),
        (fmt_n(missing), crate::tr!("transfer-not-found-yet"), false),
        (fmt_n(dups), crate::tr!("transfer-duplicates"), false),
    ];
    stat_row(ui, &stats, accent);
    ui.add_space(16.0);
    if check {
        app.xfer.reviewing = true;
    }
    if total == 0 {
        drop(guard);
        apply_controls(app, pause, cancel, paused);
        return;
    }

    // ---- filter tabs and the song list
    let mut tab = app.xfer.tab;
    ui.horizontal(|ui| {
        for (t, label) in [
            (Tab::All, crate::tr!("transfer-all").to_owned()),
            (Tab::Matched, numbered(crate::tr!("transfer-matched"), found)),
            (Tab::Look, numbered(crate::tr!("transfer-needs-review"), unsure)),
            (Tab::Missing, numbered(crate::tr!("transfer-not-found"), missing)),
        ] {
            if widgets::chip(ui, &label, tab == t).clicked() {
                tab = t;
            }
        }
    });
    app.xfer.tab = tab;
    ui.add_space(10.0);
    let rows: Vec<usize> = (0..total)
        .filter(|&i| match tab {
            Tab::All => true,
            Tab::Matched => matches!(job.results[i], Search::Found(_)),
            Tab::Look => matches!(job.results[i], Search::Unsure(_)),
            Tab::Missing => job.results[i] == Search::Missing,
        })
        .collect();
    let mut acts = Vec::new();
    egui::Frame::new().fill(theme::glass_panel(0.04)).corner_radius(theme::panel_radius()).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        list_header(ui);
        if rows.is_empty() {
            ui.add_space(10.0);
            dim(ui, crate::tr!("nav-empty"));
            ui.add_space(10.0);
        }
        widgets::visible_rows(ui, rows.len(), |ui, k| {
            let i = rows[k];
            song_row(ui, i, &job.tracks[i], &job.results[i], accent, &mut acts);
        });
    });
    drop(guard);
    app.actions.extend(acts);
    apply_controls(app, pause, cancel, paused);
}

fn apply_controls(app: &mut App, pause: bool, cancel: bool, paused: bool) {
    if pause {
        app.transfer.set_paused(!paused);
    }
    if cancel {
        app.transfer.cancel();
        app.toast(crate::tr!("transfer-cancelled"));
    }
}

fn duplicates(job: &Job) -> usize {
    let mut seen = std::collections::HashSet::new();
    job.results.iter().filter_map(Search::track).filter(|t| !seen.insert(t.id.as_str())).count()
}

fn stat_row(ui: &mut Ui, stats: &[(String, &str, bool)], accent: Color32) {
    let n = stats.len() as f32;
    let gap = 12.0;
    let w = (ui.available_width() - gap * (n - 1.0)) / n;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (value, label, ring) in stats {
            let (rect, _) = ui.allocate_exact_size(vec2(w, 66.0), Sense::hover());
            ui.painter().rect_filled(rect, 12.0, theme::glass_panel(0.06));
            if *ring {
                ui.painter().rect_stroke(rect, 12.0, egui::Stroke::new(1.5, accent), egui::StrokeKind::Inside);
            }
            ui.painter().text(pos2(rect.left() + 16.0, rect.top() + 12.0), Align2::LEFT_TOP, value, theme::bold(22.0), theme::text());
            text_at(ui, pos2(rect.left() + 16.0, rect.top() + 41.0), label, theme::regular(12.5), theme::text_dim(), w - 24.0);
        }
    });
}

/// Column positions of the song list: number, Spotify, arrow, YouTube Music, status.
fn columns(rect: Rect) -> (f32, f32, f32, f32, f32) {
    let status_w = 140.0;
    let num = rect.left() + 8.0;
    let sp = rect.left() + 52.0;
    let half = (rect.width() - 52.0 - 28.0 - status_w - 24.0) / 2.0;
    let arrow = sp + half + 12.0;
    let yt = arrow + 28.0;
    let status = rect.right() - status_w;
    (num, sp, arrow, yt, status.max(yt + 40.0))
}

fn list_header(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
    let (num, sp, _, yt, status) = columns(rect);
    let c = theme::text_faint();
    let f = theme::bold(11.0);
    for (x, t) in [(num, "#"), (sp, "SPOTIFY"), (yt, "YOUTUBE MUSIC"), (status, &crate::tr!("settings-status").to_uppercase())] {
        ui.painter().text(pos2(x, rect.center().y), Align2::LEFT_CENTER, t, f.clone(), c);
    }
}

fn song_row(ui: &mut Ui, i: usize, sp: &Track, result: &Search, accent: Color32, acts: &mut Vec<super::Action>) {
    // Same height as the rows `visible_rows` expects.
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::hover());
    let (num, spx, arrow, ytx, status) = columns(rect);
    let half = arrow - spx - 12.0;
    if resp.hovered() {
        ui.painter().rect_filled(rect, 8.0, theme::glass(0.04));
    }
    ui.painter().text(pos2(num, rect.center().y), Align2::LEFT_CENTER, fmt_n(i + 1), theme::regular(12.5), theme::text_faint());
    let img = Rect::from_min_size(pos2(spx, rect.center().y - 19.0), vec2(38.0, 38.0));
    paint_cover(ui, img, &spotify_thumbs(&sp.art), theme::thumb_radius(), 1.0);
    let tw = half - 50.0;
    text_at(ui, pos2(img.right() + 10.0, rect.top() + 9.0), &sp.name, theme::semibold(13.5), theme::text(), tw);
    let artist = if sp.local { format!("{} · {}", sp.artist_line(), crate::tr!("transfer-local-file")) } else { sp.artist_line() };
    text_at(ui, pos2(img.right() + 10.0, rect.top() + 29.0), &artist, theme::regular(12.0), theme::text_dim(), tw);
    icon_at(ui, pos2(arrow + 10.0, rect.center().y), icon::CHEVRON_RIGHT, 16.0, theme::text_faint());
    let yw = status - ytx - 16.0;
    let (label, dot, fg) = match result {
        Search::Pending => (crate::tr!("transfer-searching"), theme::glass(0.2), theme::text_dim()),
        Search::Found(_) => (crate::tr!("transfer-matched"), theme::text(), theme::text()),
        Search::Unsure(_) => (crate::tr!("transfer-needs-review"), accent, accent),
        Search::Missing => (crate::tr!("transfer-not-found"), theme::text_faint(), theme::text_dim()),
        Search::Skipped => (crate::tr!("transfer-skipped"), theme::text_faint(), theme::text_dim()),
    };
    match result.track() {
        Some(t) => {
            let img = Rect::from_min_size(pos2(ytx, rect.center().y - 19.0), vec2(38.0, 38.0));
            paint_cover(ui, img, &t.thumbs, theme::thumb_radius(), 1.0);
            let r = ui.interact(img, ui.scope_id().with(("prev", i)), Sense::click());
            if named(r.clone(), crate::tr!("transfer-play-match")).on_hover_text(crate::tr!("ui-play")).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                acts.push(Action::Play(vec![t.clone()], 0));
            }
            if r.hovered() {
                ui.painter().rect_filled(img, theme::thumb_radius(), theme::shade(0.45));
                widgets::play_triangle(ui, img.center(), 7.0, Color32::WHITE);
            }
            text_at(ui, pos2(img.right() + 10.0, rect.top() + 9.0), &t.title, theme::semibold(13.5), theme::text(), yw - 48.0);
            let meta = match &t.album {
                Some(a) => format!("{} • {}", t.artist_line(), a.name),
                None => t.artist_line(),
            };
            text_at(ui, pos2(img.right() + 10.0, rect.top() + 29.0), &meta, theme::regular(12.0), theme::text_dim(), yw - 48.0);
        }
        None => {
            let t = if *result == Search::Pending { "" } else { crate::tr!("transfer-no-result") };
            ui.painter().text(pos2(ytx, rect.center().y), Align2::LEFT_CENTER, t, theme::regular(13.0), theme::text_faint());
        }
    }
    ui.painter().circle_filled(pos2(status + 4.0, rect.center().y), 4.0, dot);
    ui.painter().text(pos2(status + 16.0, rect.center().y), Align2::LEFT_CENTER, label, theme::semibold(13.0), fg);
}

// -------------------------------------------------------------------- 3. review

fn review(app: &mut App, ui: &mut Ui) {
    let xfer = app.transfer.clone();
    let accent = app.accent_color();
    let guard = xfer.job.lock().unwrap();
    let Some(job) = guard.as_ref() else { return };
    let total = job.doubts.len();
    let left = job.doubts.iter().filter(|d| d.decision.is_none()).count();
    // The song on screen: one picked from the list, else the first not decided yet.
    let Some(at) = app.xfer.focus.filter(|&k| k < total).or_else(|| job.current_doubt()) else { return };
    let d = &job.doubts[at];
    let sp = &job.tracks[d.index];
    // The current pick: what the search chose (if anything), among the candidates.
    let current = job.results[d.index].track().and_then(|t| d.candidates.iter().position(|c| c.id == t.id));
    let current_label = job.results[d.index].track().filter(|_| current.is_none()).cloned();
    let decided_pick = match d.decision {
        Some(Decision::Pick(k)) => Some(k),
        _ => None,
    };
    let picked = app.xfer.pick.filter(|(k, _)| *k == at).map(|(_, c)| c).or(decided_pick).or(current);
    let query = match sp.artists.first() {
        Some(a) if !a.is_empty() => format!("{} {a}", sp.name),
        _ => sp.name.clone(),
    };
    let mut search = false;

    ui.add_space(6.0);
    breadcrumb(app, ui);
    let mut finish = false;
    let mut list = false;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_width((ui.available_width() - 300.0).max(200.0));
            ui.add_space(4.0);
            let title = match left {
                0 => crate::tr!("transfer-reviewed").to_owned(),
                n => crate::tr!("transfer-check-count", count = fmt_n(n)),
            };
            ui.label(RichText::new(title).font(theme::bold(28.0)).color(theme::text()));
            ui.add_space(2.0);
            dim(ui, crate::tr!("transfer-review-intro"));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            finish = pill(ui, crate::tr!("transfer-keep-rest"), None, false).clicked();
            ui.add_space(8.0);
            if pill(ui, crate::tr!("transfer-all-songs"), Some(icon::CHEVRON_LEFT), false).on_hover_text(crate::tr!("transfer-back-matches")).clicked() {
                list = true;
            }
        });
    });
    ui.add_space(12.0);
    let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 6.0), Sense::hover());
    ui.painter().rect_filled(bar, 3.0, theme::glass(0.12));
    ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * (total - left) as f32 / total.max(1) as f32, 6.0)), 3.0, accent);
    ui.add_space(14.0);

    let mut decision = None;
    let mut back = false;
    let mut focus_to: Option<usize> = None;
    let mut edit: Option<usize> = None;
    let mut acts = Vec::new();
    let wide = ui.available_width() > 820.0;
    let queue_w = if wide { 260.0 } else { 0.0 };
    ui.horizontal_top(|ui| {
        if wide {
            ui.vertical(|ui| {
                ui.set_width(queue_w);
                let card_h = if app.xfer.card_h > 0.0 { app.xfer.card_h } else { 560.0 };
                egui::Frame::new().fill(theme::glass_panel(0.04)).corner_radius(theme::panel_radius()).inner_margin(10).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    // The same height as the card beside it, whatever's in the list.
                    ui.set_height(card_h - 20.0);
                    let matched: Vec<usize> = (0..job.results.len()).filter(|&i| matches!(job.results[i], Search::Found(_))).collect();
                    ui.horizontal(|ui| {
                        if widgets::chip(ui, &numbered(crate::tr!("transfer-needs-review"), total), !app.xfer.show_matched).clicked() {
                            app.xfer.show_matched = false;
                        }
                        if widgets::chip(ui, &numbered(crate::tr!("transfer-matched"), matched.len()), app.xfer.show_matched).clicked() {
                            app.xfer.show_matched = true;
                        }
                    });
                    ui.add_space(8.0);
                    const ROW: f32 = 46.0;
                    let list_h = ui.available_height();
                    let n = if app.xfer.show_matched { matched.len() } else { total };
                    let mut area = egui::ScrollArea::vertical().id_salt(("xfer-queue", app.xfer.show_matched)).auto_shrink([false, false]).max_height(list_h);
                    // Keep the song on screen in view when it changes.
                    if !app.xfer.show_matched && app.xfer.scrolled_to != Some(at) {
                        app.xfer.scrolled_to = Some(at);
                        let spacing = ui.spacing().item_spacing.y;
                        area = area.vertical_scroll_offset((at as f32 * (ROW + spacing) - list_h / 2.0 + ROW / 2.0).max(0.0));
                    }
                    area.show_rows(ui, ROW, n, |ui, range| {
                        for r in range {
                            let (i, mark, on) = if app.xfer.show_matched {
                                let i = matched[r];
                                (i, "", job.doubts.get(at).is_some_and(|d| d.index == i))
                            } else {
                                let mark = match job.doubts[r].decision {
                                    Some(Decision::Skip) => crate::tr!("transfer-skipped"),
                                    Some(_) => "✓",
                                    None => "",
                                };
                                (job.doubts[r].index, mark, r == at)
                            };
                            let t = &job.tracks[i];
                            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
                            let resp = named(resp, &t.name).on_hover_cursor(egui::CursorIcon::PointingHand);
                            let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
                            ui.painter().rect_filled(rect, 8.0, theme::glass(if on { 0.08 } else { 0.04 * h }));
                            let img = Rect::from_min_size(pos2(rect.left() + 6.0, rect.center().y - 17.0), vec2(34.0, 34.0));
                            paint_cover(ui, img, &spotify_thumbs(&t.art), 4.0, 1.0);
                            let mw = if mark.is_empty() { 0.0 } else { 54.0 };
                            let tw = rect.right() - img.right() - 16.0 - mw;
                            text_at(ui, pos2(img.right() + 10.0, rect.top() + 6.0), &t.name, theme::semibold(13.0), theme::text(), tw);
                            text_at(ui, pos2(img.right() + 10.0, rect.top() + 25.0), &t.artist_line(), theme::regular(12.0), theme::text_dim(), tw);
                            if !mark.is_empty() {
                                ui.painter().text(pos2(rect.right() - 6.0, rect.center().y), Align2::RIGHT_CENTER, mark, theme::regular(12.0), theme::text_dim());
                            }
                            if resp.clicked() {
                                if app.xfer.show_matched {
                                    edit = Some(i);
                                } else {
                                    focus_to = Some(r);
                                }
                            }
                        }
                    });
                });
            });
            ui.add_space(18.0);
        }
        let card = ui.vertical(|ui| {
            ui.set_width(ui.available_width());
            settings_card(ui, |ui| {
                ui.add_space(18.0);
                let (head, _) = ui.allocate_exact_size(vec2(ui.available_width(), 64.0), Sense::hover());
                let img = Rect::from_min_size(head.min, vec2(64.0, 64.0));
                paint_cover(ui, img, &spotify_thumbs(&sp.art), 8.0, 1.0);
                let x = img.right() + 16.0;
                let tw = head.right() - x;
                let state = d.decision.map(|decision| match decision {
                    Decision::Skip => crate::tr!("transfer-skipped"),
                    Decision::Keep => crate::tr!("transfer-review-kept"),
                    Decision::Pick(_) => crate::tr!("transfer-review-picked"),
                }).map_or_else(String::new, |label| format!(" · {label}"));
                ui.painter().text(pos2(x, head.top() + 1.0), Align2::LEFT_TOP, format!("{}{state}", crate::tr!("transfer-on-spotify-index", current = fmt_n(at + 1), total = fmt_n(total))).to_uppercase(), theme::bold(11.5), theme::text_faint());
                text_at(ui, pos2(x, head.top() + 17.0), &sp.name, theme::bold(21.0), theme::text(), tw);
                let r = text_at(ui, pos2(x, head.top() + 45.0), &sp.artist_line(), theme::regular(14.0), theme::text_dim(), tw - 100.0);
                if sp.local {
                    let gx = if sp.artists.is_empty() { x } else { r.right() + 10.0 };
                    let label = crate::tr!("transfer-local-file").to_uppercase();
                    let g = widgets::galley(ui, &label, theme::bold(10.5), theme::text(), 200.0);
                    tag(ui, pos2(gx + g.size().x + 14.0, head.top() + 54.0), &label, theme::glass(0.14));
                }
                ui.add_space(10.0);
                let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
                ui.painter().rect_filled(line, 0.0, theme::glass_panel(0.10));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(crate::tr!("transfer-pick-youtube-match").to_uppercase()).font(theme::bold(11.5)).color(theme::text_faint()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        search = pill(ui, crate::tr!("transfer-search-youtube"), Some(icon::SEARCH), false).on_hover_text(crate::tr!("transfer-find-yourself")).clicked();
                    });
                });
                ui.add_space(8.0);
                // A fixed height, the same for every song, so the buttons under the list stay
                // put; longer lists scroll inside it.
                let room = (ui.ctx().content_rect().height() - 560.0).clamp(52.0 * 3.5, 52.0 * 8.0);
                egui::ScrollArea::vertical()
                    .id_salt(("xfer-cands", at))
                    .auto_shrink([false, false])
                    .min_scrolled_height(room)
                    .max_height(room)
                    .show(ui, |ui| {
                        if let Some(t) = &current_label {
                            // The first search's pick isn't among the new results: show it too.
                            let _ = candidate_row(ui, t, false, true, false, accent, &mut acts);
                        }
                        if d.loading {
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                widgets::loader(ui);
                                dim(ui, crate::tr!("transfer-searching-versions"));
                            });
                        } else if d.candidates.is_empty() && current_label.is_none() {
                            dim(ui, crate::tr!("transfer-no-match"));
                        }
                        for (k, c) in d.candidates.iter().enumerate() {
                            if candidate_row(ui, c, d.videos[k], current == Some(k), picked == Some(k), accent, &mut acts) {
                                app.xfer.pick = Some((at, k));
                            }
                        }
                    });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if pill(ui, crate::tr!("transfer-skip-song"), None, false).on_hover_text(crate::tr!("transfer-skip-tip")).clicked() {
                        decision = Some(Decision::Skip);
                    }
                    ui.add_space(4.0);
                    if at > 0 && pill(ui, crate::tr!("player-previous-song"), Some(icon::CHEVRON_LEFT), false).clicked() {
                        back = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let has_current = current.is_some() || current_label.is_some();
                        let (label, choice) = match picked {
                            Some(k) if Some(k) != current => (crate::tr!("transfer-use-selected"), Decision::Pick(k)),
                            _ if has_current => (crate::tr!("transfer-keep-current"), Decision::Keep),
                            _ => (crate::tr!("transfer-leave-out"), Decision::Skip),
                        };
                        if big_button(ui, label, accent, true, 200.0) {
                            decision = Some(choice);
                        }
                    });
                });
                ui.add_space(14.0);
            });
        });
        app.xfer.card_h = card.response.rect.height();
    });
    // After deciding, the next song still to check (after this one, then from the top).
    let next_open = |from: usize| (from + 1..total).chain(0..from).find(|&k| job.doubts[k].decision.is_none());
    let next = next_open(at);
    drop(guard);
    app.actions.extend(acts);
    if list {
        app.xfer.reviewing = false;
        return;
    }
    if search {
        // The search page picks for this song until it's left.
        app.xfer.picking = Some(at);
        app.xfer.pick = None;
        app.actions.push(Action::Go(Page::Search(query)));
        return;
    }
    if let Some(k) = focus_to {
        app.xfer.focus = Some(k);
        app.xfer.pick = None;
        return;
    }
    if let Some(i) = edit {
        app.xfer.focus = xfer.edit_match(&app.backend, i);
        app.xfer.pick = None;
        app.xfer.show_matched = false;
        return;
    }
    let now = ui.input(|i| i.time);
    if (back || decision.is_some() || finish) && now - app.xfer.decided_at < 0.4 {
        return;
    }
    if back || decision.is_some() {
        app.xfer.decided_at = now;
    }
    if back {
        app.xfer.focus = Some(at - 1);
        app.xfer.pick = None;
    } else if let Some(d) = decision {
        app.xfer.focus = next;
        xfer.decide(at, d);
        app.xfer.pick = None;
    } else if finish {
        xfer.finish_review();
    }
}

/// On the search page while choosing a match: which Spotify song it's for, and the way
/// back. Returns whether a match is being chosen.
pub fn pick_banner(app: &mut App, ui: &mut Ui) -> bool {
    let Some(at) = app.xfer.picking else { return false };
    let song = {
        let guard = app.transfer.job.lock().unwrap();
        guard.as_ref().filter(|j| j.phase == Phase::Review).and_then(|j| j.tracks.get(j.doubts.get(at)?.index).cloned())
    };
    let Some(sp) = song else {
        // The review ended meanwhile (cancelled, or finished elsewhere).
        app.xfer.picking = None;
        return false;
    };
    let accent = app.accent_color();
    let mut back = false;
    ui.add_space(8.0);
    egui::Frame::new()
        .fill(theme::with_alpha(accent, 0.12))
        .stroke(egui::Stroke::new(1.0, theme::with_alpha(accent, 0.35)))
        .corner_radius(theme::panel_radius())
        .inner_margin(14)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (img, _) = ui.allocate_exact_size(vec2(52.0, 52.0), Sense::hover());
                paint_cover(ui, img, &spotify_thumbs(&sp.art), 6.0, 1.0);
                ui.add_space(8.0);
                let text_w = (ui.available_width() - 200.0).max(120.0);
                ui.vertical(|ui| {
                    ui.set_width(text_w);
                    ui.label(RichText::new(crate::tr!("transfer-choosing-for").to_uppercase()).font(theme::bold(11.5)).color(theme::text_faint()));
                    ui.add(egui::Label::new(RichText::new(format!("{} · {}", sp.name, sp.artist_line())).font(theme::semibold(15.0)).color(theme::text())).truncate());
                    dim(ui, crate::tr!("transfer-pick-or-play"));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    back = pill(ui, crate::tr!("transfer-back"), Some(icon::CHEVRON_LEFT), false).clicked();
                });
            });
        });
    ui.add_space(6.0);
    if back {
        app.actions.push(Action::Go(Page::Transfer));
    }
    true
}

/// A song was chosen on the search page: it becomes the match, and the review moves on.
pub fn use_pick(app: &mut App, t: YtTrack) {
    let Some(at) = app.xfer.picking.take() else { return };
    app.transfer.decide_with(at, t);
    // Back on the next song still to check (after this one, then from the top).
    app.xfer.focus = app.transfer.job.lock().unwrap().as_ref().and_then(|j| {
        let n = j.doubts.len();
        (at + 1..n).chain(0..at.min(n)).find(|&k| j.doubts[k].decision.is_none())
    });
    app.xfer.pick = None;
    app.go(Page::Transfer);
}

/// One candidate; returns true when it was picked.
fn candidate_row(ui: &mut Ui, t: &YtTrack, video: bool, current: bool, selected: bool, accent: Color32, acts: &mut Vec<Action>) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::click());
    let resp = named(resp, &t.title).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
    let s = theme::anim_bool(ui.ctx(), resp.id.with("s"), selected, 0.15);
    ui.painter().rect_filled(rect, 10.0, theme::glass(0.04 * h + 0.05 * s));
    if s > 0.0 {
        ui.painter().rect_stroke(rect, 10.0, egui::Stroke::new(1.5, theme::with_alpha(accent, s)), egui::StrokeKind::Inside);
    }
    radio(ui, pos2(rect.left() + 20.0, rect.center().y), s, accent);
    let img = Rect::from_min_size(pos2(rect.left() + 40.0, rect.center().y - 20.0), vec2(40.0, 40.0));
    paint_cover(ui, img, &t.thumbs, theme::thumb_radius(), 1.0);
    let play = ui.interact(img, resp.id.with("play"), Sense::click());
    if play.hovered() {
        ui.painter().rect_filled(img, theme::thumb_radius(), theme::shade(0.45));
        widgets::play_triangle(ui, img.center(), 8.0, Color32::WHITE);
    }
    let play = named(play, crate::tr!("ui-play")).on_hover_text(crate::tr!("ui-play")).on_hover_cursor(egui::CursorIcon::PointingHand);
    let mut right = rect.right() - 12.0;
    if let Some(d) = t.duration {
        ui.painter().text(pos2(right, rect.center().y), Align2::RIGHT_CENTER, format!("{}:{:02}", d / 60, d % 60), theme::regular(13.0), theme::text_dim());
        right -= 52.0;
    }
    right = tag(ui, pos2(right, rect.center().y), &if video { crate::tr!("ui-video") } else { crate::tr!("ui-song") }.to_uppercase(), theme::glass(if video { 0.16 } else { 0.10 })) - 8.0;
    if current {
        right = tag(ui, pos2(right, rect.center().y), &crate::tr!("transfer-current-pick").to_uppercase(), theme::with_alpha(accent, 0.35)) - 8.0;
    }
    let x = img.right() + 12.0;
    let tw = right - x - 8.0;
    text_at(ui, pos2(x, rect.top() + 7.0), &t.title, theme::semibold(14.5), theme::text(), tw);
    let mut meta = t.artist_line();
    if let Some(a) = &t.album {
        meta = format!("{meta} • {}", a.name);
    } else if let Some(p) = t.plays {
        meta = format!("{meta} • {}", crate::tr!("transfer-views", count = crate::backend::compact(p)));
    }
    text_at(ui, pos2(x, rect.top() + 28.0), &meta, theme::regular(13.0), theme::text_dim(), tw);
    if play.clicked() {
        acts.push(Action::Play(vec![t.clone()], 0));
        return false;
    }
    resp.clicked()
}

// ---------------------------------------------------------------------- 4. done

fn done(app: &mut App, ui: &mut Ui) {
    let xfer = app.transfer.clone();
    let accent = app.accent_color();
    let guard = xfer.job.lock().unwrap();
    let Some(job) = guard.as_ref() else { return };
    let Some(sum) = &job.summary else { return };

    ui.add_space(6.0);
    breadcrumb(app, ui);
    ui.add_space(16.0);
    let (mut open, mut another) = (false, false);
    let cover: Vec<Thumb> = job.results.iter().find_map(|r| r.track().map(|t| t.thumbs.clone())).unwrap_or_default();
    let title = match &job.dest {
        Dest::Liked => crate::tr!("nav-liked-songs").to_owned(),
        Dest::Playlist(t) => t.clone(),
    };
    let secs = sum.elapsed.as_secs();
    let time = crate::tr!("transfer-finished-time", time = fmt_secs(secs));
    let line = match &job.dest {
        Dest::Liked if sum.already > 0 => format!("{} · {} · {time}", numbered(crate::tr!("transfer-liked"), sum.added), numbered(crate::tr!("transfer-already-liked"), sum.already)),
        Dest::Liked => format!("{} · {time}", numbered(crate::tr!("transfer-liked"), sum.added)),
        Dest::Playlist(_) => format!("{} · {} · {time}", numbered(crate::tr!("transfer-private-playlists"), sum.playlists.len()), numbered(crate::tr!("nav-songs"), sum.added)),
    };
    ui.horizontal(|ui| {
        let (img, _) = ui.allocate_exact_size(vec2(120.0, 120.0), Sense::hover());
        if job.dest == Dest::Liked {
            ui.painter().rect_filled(img, 10.0, Color32::from_rgb(80, 56, 200));
            widgets::heart(ui, img.center(), 48.0, 1.0, Color32::WHITE, Color32::WHITE);
        } else {
            paint_cover(ui, img, &cover, 10.0, 1.0);
        }
        ui.add_space(18.0);
        ui.vertical(|ui| {
            ui.set_width((ui.available_width() - 330.0).max(200.0));
            ui.add_space(22.0);
            ui.label(RichText::new(crate::tr!("transfer-complete").to_uppercase()).font(theme::bold(13.0)).color(accent));
            ui.add_space(2.0);
            ui.label(RichText::new(&title).font(theme::bold(32.0)).color(theme::text()));
            ui.add_space(2.0);
            ui.label(RichText::new(&line).font(theme::regular(14.0)).color(theme::text_dim()));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            another = pill(ui, crate::tr!("transfer-another"), None, false).clicked();
            ui.add_space(8.0);
            open = pill(ui, if job.dest == Dest::Liked { crate::tr!("transfer-open-liked") } else { crate::tr!("transfer-open-playlist") }, Some(icon::PLAY), true).clicked();
        });
    });
    ui.add_space(20.0);
    let skipped = job.count(|r| *r == Search::Skipped);
    let missing = job.count(|r| *r == Search::Missing);
    let mut stats = vec![(fmt_n(sum.added), if job.dest == Dest::Liked { crate::tr!("transfer-liked") } else { crate::tr!("transfer-added") }, false)];
    if job.dest == Dest::Liked {
        stats.push((fmt_n(sum.already), crate::tr!("transfer-already-liked"), false));
    }
    stats.extend([(fmt_n(sum.duplicates), crate::tr!("transfer-duplicates"), false), (fmt_n(skipped), crate::tr!("transfer-skipped-review"), false), (fmt_n(missing), crate::tr!("transfer-not-on-youtube"), false)]);
    stat_row(ui, &stats, accent);
    ui.add_space(20.0);

    let missed: Vec<(&Track, &str)> = job
        .tracks
        .iter()
        .zip(&job.results)
        .filter_map(|(t, r)| match r {
            Search::Missing => Some((t, crate::tr!("transfer-not-on-youtube"))),
            Search::Skipped => Some((t, crate::tr!("transfer-skipped-review"))),
            _ => None,
        })
        .collect();
    let mut copy = None;
    let mut search = None;
    if !missed.is_empty() {
        egui::Frame::new().fill(theme::glass_panel(0.04)).corner_radius(theme::panel_radius()).inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(numbered(crate::tr!("transfer-not-added"), missed.len()).to_uppercase()).font(theme::bold(11.5)).color(theme::text_faint()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if pill(ui, crate::tr!("transfer-copy-list"), None, false).clicked() {
                        copy = Some(missed.iter().map(|(t, _)| format!("{} - {}", t.name, t.artist_line())).collect::<Vec<_>>().join("\n"));
                    }
                });
            });
            ui.add_space(6.0);
            widgets::visible_rows(ui, missed.len(), |ui, k| {
                let (t, why) = missed[k];
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::hover());
                let img = Rect::from_min_size(pos2(rect.left() + 4.0, rect.center().y - 18.0), vec2(36.0, 36.0));
                paint_cover(ui, img, &spotify_thumbs(&t.art), 4.0, 1.0);
                let tw = rect.width() - 340.0;
                text_at(ui, pos2(img.right() + 12.0, rect.top() + 9.0), &t.name, theme::semibold(13.5), theme::text(), tw);
                text_at(ui, pos2(img.right() + 12.0, rect.top() + 29.0), &t.artist_line(), theme::regular(12.0), theme::text_dim(), tw);
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::right_to_left(egui::Align::Center)));
                if pill(&mut child, crate::tr!("settings-search"), Some(icon::SEARCH), false).clicked() {
                    search = Some(format!("{} {}", t.name, t.artist_line()));
                }
                child.add_space(14.0);
                child.label(RichText::new(why).font(theme::regular(13.0)).color(theme::text_dim()));
            });
        });
    }
    let first_playlist = sum.playlists.first().cloned();
    let dest = job.dest.clone();
    drop(guard);
    if let Some(text) = copy {
        ui.ctx().copy_text(text);
        app.toast(crate::tr!("transfer-list-copied"));
    }
    if let Some(q) = search {
        app.actions.push(Action::Go(Page::Search(q)));
    }
    if open {
        match (dest, first_playlist) {
            (Dest::Liked, _) => app.actions.push(Action::Go(Page::Library(LibTab::Songs))),
            (_, Some(id)) => app.actions.push(Action::Open(Target::Playlist(id))),
            _ => {}
        }
    }
    if another {
        xfer.dismiss();
        app.xfer.picked = None;
        app.xfer.link.clear();
        app.xfer.name_for = None;
    }
}

fn failed(app: &mut App, ui: &mut Ui, e: &str) {
    let xfer = app.transfer.clone();
    ui.add_space(6.0);
    breadcrumb(app, ui);
    page_title(ui, crate::tr!("transfer-stopped"));
    ui.add_space(10.0);
    // Only this transfer's own progress (one that failed before saving any has none).
    let source = xfer.job.lock().unwrap().as_ref().map(|j| j.source.clone());
    let resumable = xfer.resumable.lock().unwrap().clone().filter(|r| Some(&r.source) == source.as_ref());
    let (mut retry, mut close) = (false, false);
    settings_card(ui, |ui| {
        setting_row(ui, crate::tr!("transfer-error"), Some(e), |ui| {
            close = pill(ui, crate::tr!("transfer-start-over"), None, false).clicked();
            if resumable.is_some() {
                ui.add_space(8.0);
                retry = pill(ui, crate::tr!("ui-try-again"), None, true).clicked();
            }
        });
    });
    if retry && let Some(r) = resumable {
        xfer.dismiss();
        xfer.start(&app.backend, r.source, r.dest, true);
    } else if close {
        xfer.dismiss();
    }
}

// ------------------------------------------------------------ settings, sidebar

/// Settings › Import.
pub fn settings_section(app: &mut App, ui: &mut Ui) {
    let xfer = app.transfer.clone();
    xfer.ensure_account(&app.backend);
    let account = xfer.account.lock().unwrap().clone();
    let mut open = false;
    section(ui, crate::tr!("transfer-import"), None, None, |ui| {
        if !filter_show(ui, &format!("{} {}", crate::tr!("transfer-title"), crate::tr!("transfer-intro"))) {
            return;
        }
        let w = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(vec2(w, 78.0), Sense::hover());
        let b = Rect::from_min_size(pos2(rect.left(), rect.center().y - 22.0), vec2(44.0, 44.0));
        badge(ui, b);
        let x = b.right() + 16.0;
        let tw = rect.right() - x - 120.0;
        ui.painter().text(pos2(x, rect.center().y - 10.0), Align2::LEFT_CENTER, crate::tr!("transfer-title"), theme::semibold(14.5), theme::text());
        let intro = crate::tr!("transfer-intro");
        let g = widgets::galley_wrapped(ui, intro, theme::regular(12.5), theme::text_dim(), tw, 2);
        let pos = pos2(x, rect.center().y + 2.0);
        ui.painter().galley(pos, g.clone(), theme::text_dim());
        widgets::tip_if_elided(ui, Rect::from_min_size(pos, g.size()), &g, intro);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::right_to_left(egui::Align::Center)));
        open = pill(&mut child, crate::tr!("settings-open"), None, true).clicked();
        super::pages::divider(ui);
        spotify_account_row(app, ui, &account, false);
    });
    if open {
        app.actions.push(Action::Go(Page::Transfer));
    }
}

/// Whether the sidebar shows the transfer card: while it runs, waits or failed, and for
/// a few seconds after it finishes (the summary stays on the transfer page).
pub fn sidebar_visible(app: &App, now: f64) -> bool {
    match app.transfer.job.lock().unwrap().as_ref().map(|j| &j.phase) {
        None => false,
        Some(Phase::Done) => app.xfer.done_at.is_none_or(|t| now - t < DONE_LINGER),
        Some(_) => true,
    }
}

/// Seconds the "Done" card stays in the sidebar.
const DONE_LINGER: f64 = 6.0;

/// The card above Settings in the sidebar while a transfer runs, waits or just finished.
pub fn sidebar_card(app: &mut App, ui: &mut Ui, collapsed: bool, fade: f32, icon_x: f32) {
    let (frac, status, needs_review) = {
        let guard = app.transfer.job.lock().unwrap();
        let Some(job) = guard.as_ref() else { return };
        let (frac, status) = match &job.phase {
            Phase::Reading => (0.0, crate::tr!("transfer-reading").to_owned()),
            Phase::Searching => {
                let f = job.searched() as f32 / job.tracks.len().max(1) as f32;
                (f, if job.paused { crate::tr!("transfer-paused").to_owned() } else { format!("{}%", (f * 100.0) as u32) })
            }
            Phase::Checking => (1.0, crate::tr!("transfer-checking").to_owned()),
            Phase::Review => (1.0, crate::tr!("transfer-review").to_owned()),
            Phase::Saving => match job.redo {
                Some((done, of)) => (done as f32 / of.max(1) as f32, crate::tr!("transfer-readding").to_owned()),
                None => (job.saving.0 as f32 / job.saving.1.max(1) as f32, crate::tr!("transfer-adding").to_owned()),
            },
            Phase::Done => (1.0, crate::tr!("ui-done").to_owned()),
            Phase::Failed(_) => (0.0, crate::tr!("transfer-stopped-status").to_owned()),
        };
        (frac, status, matches!(job.phase, Phase::Review))
    };
    let accent = app.accent_color();
    ui.add_space(6.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 58.0), Sense::click());
    let mut resp = named(resp, crate::tr!("transfer-short-title")).on_hover_cursor(egui::CursorIcon::PointingHand);
    if collapsed {
        resp = resp.on_hover_text(crate::tr!("transfer-sidebar-status", status = status.as_str()));
    } else if needs_review {
        resp = resp.on_hover_text(crate::tr!("transfer-sidebar-review-tip"));
    }
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
    ui.painter().rect_filled(rect, 12.0, theme::glass(0.06 + 0.04 * h));
    ui.painter().rect_stroke(rect, 12.0, egui::Stroke::new(1.0, theme::glass(0.10)), egui::StrokeKind::Inside);
    let b = Rect::from_center_size(pos2(rect.left() + icon_x, rect.top() + 22.0), vec2(20.0, 20.0));
    badge(ui, b);
    if fade > 0.0 {
        let c = theme::with_alpha(theme::text(), fade);
        let st = ui.painter().text(pos2(rect.right() - 12.0, rect.top() + 22.0), Align2::RIGHT_CENTER, &status, theme::regular(12.0), theme::with_alpha(if needs_review { accent } else { theme::text_dim() }, fade));
        text_at(ui, pos2(rect.left() + 48.0, rect.top() + 14.0), crate::tr!("transfer-short-title"), theme::semibold(13.0), c, st.left() - rect.left() - 56.0);
    }
    let bar = Rect::from_min_max(pos2(rect.left() + 12.0, rect.bottom() - 14.0), pos2(rect.right() - 12.0, rect.bottom() - 10.0));
    ui.painter().rect_filled(bar, 2.0, theme::glass(0.14));
    ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * frac.clamp(0.0, 1.0), bar.height())), 2.0, accent);
    if resp.clicked() {
        app.actions.push(Action::Go(Page::Transfer));
    }
}

/// Once a frame: tell the user when the transfer needs them or finished.
pub fn poll(app: &mut App, ctx: &egui::Context, now: f64) {
    // Dev hook: YTM_TRANSFER_LINK=<link> fills in the playlist link.
    if !app.xfer.dev
        && app.xfer.link.is_empty()
        && let Some(l) = super::dev_var("YTM_TRANSFER_LINK")
    {
        app.xfer.link = l.to_owned();
    }
    let (phase, name, doubts) = {
        let guard = app.transfer.job.lock().unwrap();
        match guard.as_ref() {
            Some(j) => (Some(j.phase.clone()), j.name.clone(), j.doubts.len()),
            None => (None, String::new(), 0),
        }
    };
    if let Some(t) = app.xfer.done_at.filter(|_| phase == Some(Phase::Done)) {
        // Redraw once more when the card is due to leave.
        if now - t < DONE_LINGER {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(DONE_LINGER - (now - t) + 0.05));
        }
    }
    if phase == app.xfer.seen {
        return;
    }
    app.xfer.done_at = (phase == Some(Phase::Done)).then_some(now);
    if phase != Some(Phase::Review) {
        app.xfer.reviewing = false;
    }
    let on_page = app.page == Page::Transfer;
    match &phase {
        Some(Phase::Review) if !on_page => app.toast(crate::tr!("transfer-review-notice", count = fmt_n(doubts))),
        Some(Phase::Done) => {
            // Liked Music and the sidebar's playlists changed.
            app.request_library();
            app.request_liked(true);
            if !on_page {
                app.toast(crate::tr!("transfer-complete-notice", name = name.as_str()));
            }
        }
        Some(Phase::Failed(_)) if !on_page => app.toast(crate::tr!("transfer-failed-notice")),
        _ => {}
    }
    app.xfer.seen = phase;
}
