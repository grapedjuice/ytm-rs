//! Page bodies. Every page fades and slides in when navigated to.

use std::sync::atomic::Ordering;

use egui::{Align2, Color32, Rect, RichText, Sense, Ui, Vec2, pos2, vec2};

use super::widgets::{self, RowOpts, card, chip, h_scroll, named, paint_cover, pill, shelf_header, track_row};
use super::{Action, App, LibTab, Page};
use crate::backend::compact;
use crate::model::{Card, ShelfKind, Target, Track};
use crate::theme::{self, icon};

const CARD: f32 = 180.0;

pub fn page(app: &mut App, ui: &mut Ui, now: f64) {
    let t = theme::ease_out(((now - app.page_changed) / 0.32) as f32);
    ui.set_opacity(0.15 + 0.85 * t);
    let page = app.page.clone();
    let scroll_id = format!("{:?}", std::mem::discriminant(&page));
    let out = egui::ScrollArea::vertical().id_salt(&scroll_id).auto_shrink(false).show(ui, |ui| {
        ui.add_space(18.0 * (1.0 - t));
        match &page {
            Page::Home => home(app, ui, now),
            Page::Explore => explore(app, ui, now),
            Page::Search(q) => search(app, ui, q, now),
            Page::Album(id) | Page::Playlist(id) => collection(app, ui, id, now),
            Page::Artist(id) => artist(app, ui, id, now),
            Page::Library(tab) => library(app, ui, *tab, now),
            Page::Settings => settings(app, ui),
        }
        ui.add_space(40.0);
    });
    // Infinite scroll on home: fetch the next shelves when nearing the bottom.
    if page == Page::Home {
        let remaining = out.content_size.y - (out.state.offset.y + out.inner_rect.height());
        if let Some(h) = &mut app.home {
            if remaining < 700.0 && !h.loading_more {
                if let Some(token) = h.continuation.clone() {
                    h.loading_more = true;
                    app.backend.request(crate::backend::Req::HomeMore(token));
                }
            }
        }
    }
}

/// A page's main Play button. Shows Pause while this page's album / playlist / artist
/// is playing, and toggles instead of restarting when it's already loaded.
fn play_pill(ui: &mut Ui, acts: &mut Vec<Action>, target: &Target, tracks: &[Track]) {
    let active = widgets::active_state(ui, target);
    let (label, glyph) = if active == Some(true) { ("Pause", icon::PAUSE) } else { ("Play", icon::PLAY) };
    if pill(ui, label, Some(glyph), true).clicked() {
        if active.is_some() {
            acts.push(Action::TogglePlay);
        } else if !tracks.is_empty() {
            acts.push(Action::Play(tracks.to_vec(), 0));
        }
    }
}

fn opts(app: &App, t: &Track, number: Option<usize>, show_album: bool, now: f64) -> RowOpts {
    RowOpts {
        number,
        show_album,
        current: app.current_id() == Some(t.id.as_str()),
        playing: app.audio.status.playing.load(Ordering::Relaxed),
        accent: app.accent,
        time: now,
        level: app.audio.status.level(),
    }
}

fn card_row(ui: &mut Ui, id: &str, title: &str, cards: &[Card], acts: &mut Vec<Action>) {
    if cards.is_empty() {
        return;
    }
    let (l, r) = shelf_header(ui, title, None, None, true);
    let nudge = r as i32 - l as i32;
    h_scroll(ui, id, nudge, |ui| {
        ui.spacing_mut().item_spacing.x = 18.0;
        for c in cards {
            card(ui, c, CARD, acts);
        }
    });
}

/// Songs laid out in columns of `rows`, scrolling sideways (Quick picks).
fn song_grid(app: &mut App, ui: &mut Ui, id: &str, title: &str, strap: Option<&str>, tracks: &[Track], rows: usize, now: f64) {
    let (l, r) = shelf_header(ui, title, strap, None, true);
    let nudge = r as i32 - l as i32;
    let col_w = (ui.available_width() / 2.6).clamp(300.0, 420.0);
    let mut picked = None;
    h_scroll(ui, id, nudge, |ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        for (c, col) in tracks.chunks(rows.max(1)).enumerate() {
            ui.allocate_ui(vec2(col_w, rows as f32 * 60.0), |ui| {
                ui.set_width(col_w);
                ui.vertical(|ui| {
                    for (i, t) in col.iter().enumerate() {
                        let o = opts(app, t, None, false, now);
                        let resp = track_row(ui, t, &o, &mut app.actions);
                        if resp.clicked() {
                            picked = Some(c * rows + i);
                        }
                        let acts = &mut app.actions;
                        resp.context_menu(|ui| widgets::song_menu(ui, t, acts));
                    }
                });
            });
        }
    });
    if let Some(i) = picked {
        app.actions.push(Action::Radio(tracks[i].clone()));
    }
}

// ------------------------------------------------------------------ home

fn home(app: &mut App, ui: &mut Ui, now: f64) {
    if let Some(h) = &app.home {
        if !h.chips.is_empty() {
            let chips = h.chips.clone();
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(10.0, 10.0);
                for c in &chips {
                    if chip(ui, &c.title, c.selected).clicked() {
                        let params = if c.selected { None } else { c.params.clone() };
                        app.actions.push(Action::HomeChip(params));
                    }
                }
            });
        }
    }
    if app.home_loading || app.home.is_none() {
        widgets::skeleton(ui, now, 3, CARD);
        return;
    }
    let shelves = app.home.as_ref().map(|h| h.shelves.clone()).unwrap_or_default();
    if shelves.is_empty() {
        ui.add_space(40.0);
        ui.label(RichText::new("Nothing here yet").color(theme::TEXT_DIM));
    }
    for (i, s) in shelves.iter().enumerate() {
        let id = format!("home-{i}-{}", s.title);
        match &s.kind {
            ShelfKind::Cards(cards) => {
                let (l, r) = shelf_header(ui, &s.title, s.strapline.as_deref(), s.strap_thumb.as_deref(), true);
                let nudge = r as i32 - l as i32;
                let acts = &mut app.actions;
                h_scroll(ui, &id, nudge, |ui| {
                    ui.spacing_mut().item_spacing.x = 18.0;
                    for c in cards {
                        card(ui, c, CARD, acts);
                    }
                });
            }
            ShelfKind::Songs { tracks, rows } => {
                song_grid(app, ui, &id, &s.title, s.strapline.as_deref(), tracks, *rows, now);
            }
        }
    }
    if app.home.as_ref().is_some_and(|h| h.loading_more) {
        widgets::skeleton(ui, now, 1, CARD);
    }
}

// ------------------------------------------------------------------ explore

fn explore(app: &mut App, ui: &mut Ui, now: f64) {
    page_title(ui, "Explore");
    let Some(e) = &app.explore else {
        widgets::skeleton(ui, now, 2, CARD);
        return;
    };
    let (albums, charts, top) = (e.new_albums.clone(), e.charts.clone(), e.top.clone());
    card_row(ui, "ex-new", "New releases", &albums, &mut app.actions);
    if !top.is_empty() {
        song_grid(app, ui, "ex-top", "Top songs", None, &top, 4, now);
    }
    card_row(ui, "ex-charts", "Charts", &charts, &mut app.actions);
}

fn page_title(ui: &mut Ui, title: &str) {
    ui.add_space(14.0);
    ui.label(RichText::new(title).font(theme::bold(34.0)).color(theme::TEXT));
}

// ------------------------------------------------------------------ search

fn search(app: &mut App, ui: &mut Ui, q: &str, now: f64) {
    let Some(r) = app.search.as_ref().filter(|r| r.query == q) else {
        widgets::skeleton(ui, now, 2, CARD);
        return;
    };
    let (tracks, artists, albums, playlists) = (r.tracks.clone(), r.artists.clone(), r.albums.clone(), r.playlists.clone());
    if tracks.is_empty() && artists.is_empty() && albums.is_empty() && playlists.is_empty() {
        page_title(ui, "No results");
        return;
    }
    ui.add_space(8.0);
    // Top result: the first artist when the query names one, otherwise the first song.
    ui.horizontal_top(|ui| {
        if let Some(top) = artists.first().filter(|a| a.title.eq_ignore_ascii_case(q.trim())).or(None) {
            top_result(ui, top, &mut app.actions);
        } else if let Some(t) = tracks.first() {
            let c = Card {
                title: t.title.clone(),
                subtitle: format!("Song • {}", t.artist_line()),
                thumbs: t.thumbs.clone(),
                target: Target::Song(t.clone()),
                round: false,
                links: t.artists.clone(),
            };
            top_result(ui, &c, &mut app.actions);
        }
        ui.add_space(24.0);
        ui.vertical(|ui| {
            ui.label(RichText::new("Songs").font(theme::bold(24.0)));
            ui.add_space(8.0);
            for (i, t) in tracks.iter().take(4).enumerate() {
                let o = opts(app, t, None, false, now);
                let resp = track_row(ui, t, &o, &mut app.actions);
                if resp.clicked() {
                    app.actions.push(Action::Play(tracks.clone(), i));
                }
                let acts = &mut app.actions;
                resp.context_menu(|ui| widgets::song_menu(ui, t, acts));
            }
        });
    });
    card_row(ui, "s-artists", "Artists", &artists, &mut app.actions);
    card_row(ui, "s-albums", "Albums", &albums, &mut app.actions);
    if tracks.len() > 4 {
        ui.add_space(26.0);
        ui.label(RichText::new("More songs").font(theme::bold(24.0)));
        ui.add_space(10.0);
        for (i, t) in tracks.iter().enumerate().skip(4) {
            let o = opts(app, t, None, true, now);
            let resp = track_row(ui, t, &o, &mut app.actions);
            if resp.clicked() {
                app.actions.push(Action::Play(tracks.clone(), i));
            }
            let acts = &mut app.actions;
            resp.context_menu(|ui| widgets::song_menu(ui, t, acts));
        }
    }
    card_row(ui, "s-pl", "Playlists", &playlists, &mut app.actions);
}

fn top_result(ui: &mut Ui, c: &Card, acts: &mut Vec<Action>) {
    ui.vertical(|ui| {
        ui.label(RichText::new("Top result").font(theme::bold(24.0)));
        ui.add_space(8.0);
        let (rect, resp) = ui.allocate_exact_size(vec2(420.0, 236.0), Sense::click());
        let resp = named(resp, &c.title);
        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.contains_pointer(), 0.2);
        ui.painter().rect_filled(rect, 14.0, theme::glass(0.07 + 0.05 * h));
        let img = Rect::from_min_size(rect.min + vec2(20.0, 20.0), Vec2::splat(110.0));
        paint_cover(ui, img, &c.thumbs, if c.round { 55.0 } else { 8.0 }, 1.0);
        let x = rect.left() + 20.0;
        widgets::text_at(ui, pos2(x, img.bottom() + 16.0), &c.title, theme::bold(26.0), theme::TEXT, rect.width() - 40.0);
        widgets::linked_text(ui, pos2(x, img.bottom() + 52.0), &c.subtitle, &c.links, theme::regular(14.0), theme::TEXT_DIM, rect.width() - 40.0, acts);
        // Floating play button
        let pc = rect.right_bottom() + vec2(-44.0, -44.0 + 6.0 * (1.0 - h));
        let pr = Rect::from_center_size(pc, Vec2::splat(52.0));
        let active = widgets::active_state(ui, &c.target);
        let playing = active == Some(true);
        let label = format!("{} {}", if playing { "Pause" } else { "Play" }, c.title);
        let p = named(ui.interact(pr, resp.id.with("play"), Sense::click()), &label);
        ui.painter().circle_filled(pc, 26.0, theme::with_alpha(Color32::WHITE, 0.4 + 0.6 * h));
        let nudge = if playing { 0.0 } else { 1.5 };
        widgets::play_pause_glyph(ui, pc + vec2(nudge, 0.0), 10.0, Color32::from_gray(15), playing);
        if p.clicked() {
            acts.push(match &c.target {
                _ if active.is_some() => Action::TogglePlay,
                Target::Song(t) => Action::Radio(t.clone()),
                t => Action::PlayCollection(t.clone(), false),
            });
        } else if resp.clicked() {
            acts.push(Action::Open(c.target.clone()));
        }
    });
}

// ------------------------------------------------------------------ album / playlist

fn collection(app: &mut App, ui: &mut Ui, id: &str, now: f64) {
    let Some(c) = app.collection.as_ref().filter(|c| c.id == id) else {
        widgets::skeleton(ui, now, 1, 232.0);
        return;
    };
    let (title, subtitle, artists, desc, thumbs, tracks, is_album) =
        (c.title.clone(), c.subtitle.clone(), c.artists.clone(), c.description.clone(), c.thumbs.clone(), c.tracks.clone(), c.is_album);
    ui.add_space(18.0);
    ui.horizontal_top(|ui| {
        let (img, _) = ui.allocate_exact_size(Vec2::splat(232.0), Sense::hover());
        widgets::soft_shadow(ui, img, 10.0, 40.0, 0.5);
        paint_cover(ui, img, &thumbs, 10.0, 1.0);
        ui.add_space(26.0);
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width().min(760.0));
            ui.add_space(24.0);
            ui.label(RichText::new(if is_album { "ALBUM" } else { "PLAYLIST" }).font(theme::semibold(12.0)).color(theme::TEXT_DIM));
            ui.label(RichText::new(&title).font(theme::bold(40.0)).color(theme::TEXT));
            ui.add_space(2.0);
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
            widgets::linked_text(ui, r.min, &subtitle, &artists, theme::regular(15.0), theme::TEXT_DIM, r.width(), &mut app.actions);
            let total: u32 = tracks.iter().filter_map(|t| t.duration).sum();
            if total > 0 {
                ui.label(
                    RichText::new(format!("{} songs • {} min", tracks.len(), total / 60)).font(theme::regular(13.5)).color(theme::TEXT_FAINT),
                );
            }
            if let Some(d) = desc.filter(|d| !d.trim().is_empty()) {
                ui.add_space(6.0);
                let g = widgets::galley_wrapped(ui, d.trim(), theme::regular(13.5), theme::TEXT_DIM, ui.available_width(), 2);
                ui.label(g);
            }
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                let target = if is_album { Target::Album(id.to_owned()) } else { Target::Playlist(id.to_owned()) };
                play_pill(ui, &mut app.actions, &target, &tracks);
                if pill(ui, "Shuffle", Some(icon::SHUFFLE), false).clicked() && !tracks.is_empty() {
                    let mut t = tracks.clone();
                    super::shuffle(&mut t);
                    app.actions.push(Action::Play(t, 0));
                }
                if let Some(first) = tracks.first() {
                    if pill(ui, "Radio", Some(icon::RADIO), false).clicked() {
                        app.actions.push(Action::Radio(first.clone()));
                    }
                }
            });
        });
    });
    ui.add_space(26.0);
    for (i, t) in tracks.iter().enumerate() {
        let number = is_album.then(|| t.track_nr.map_or(i + 1, |n| n as usize));
        let o = opts(app, t, number, !is_album, now);
        let resp = track_row(ui, t, &o, &mut app.actions);
        if resp.clicked() {
            app.actions.push(Action::Play(tracks.clone(), i));
        }
        let acts = &mut app.actions;
        resp.context_menu(|ui| widgets::song_menu(ui, t, acts));
    }
}

// ------------------------------------------------------------------ artist

fn artist(app: &mut App, ui: &mut Ui, id: &str, now: f64) {
    let Some(a) = app.artist.as_ref().filter(|a| a.id == id) else {
        widgets::skeleton(ui, now, 2, CARD);
        return;
    };
    let (name, banner, subs, desc, top, top_playlist, albums, playlists, similar) = (
        a.name.clone(),
        a.banner.clone(),
        a.subscribers,
        a.description.clone(),
        a.top.clone(),
        a.top_playlist.clone(),
        a.albums.clone(),
        a.playlists.clone(),
        a.similar.clone(),
    );
    // Hero banner fading into the page.
    let w = ui.available_width();
    let hero_h = (w * 0.34).clamp(240.0, 380.0);
    let (hero, _) = ui.allocate_exact_size(vec2(w, hero_h), Sense::hover());
    if let Some(t) = banner.iter().max_by_key(|t| t.width) {
        let img_ratio = if t.height > 0 { t.width as f32 / t.height as f32 } else { 16.0 / 9.0 };
        // Cover-fit: crop the image's top band to the hero's aspect.
        let vis_h = (img_ratio / (w / hero_h)).min(1.0);
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, vis_h));
        egui::Image::new(t.url.clone()).uv(uv).corner_radius(14).paint_at(ui, hero);
    } else {
        ui.painter().rect_filled(hero, 14.0, theme::with_alpha(app.accent, 0.25));
    }
    super::shell::vgradient(ui, Rect::from_min_max(pos2(hero.left(), hero.center().y - 40.0), hero.max), Color32::TRANSPARENT, theme::shade(0.85));
    let base = hero.left_bottom() + vec2(28.0, -30.0);
    ui.painter().text(base - vec2(0.0, 44.0), Align2::LEFT_BOTTOM, &name, theme::bold(58.0), Color32::WHITE);
    if let Some(s) = subs {
        ui.painter().text(base - vec2(0.0, 18.0), Align2::LEFT_BOTTOM, format!("{} subscribers", compact(s)), theme::regular(14.0), theme::TEXT_DIM);
    }
    ui.add_space(18.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        play_pill(ui, &mut app.actions, &Target::Artist(id.to_owned()), &top);
        if pill(ui, "Shuffle", Some(icon::SHUFFLE), false).clicked() && !top.is_empty() {
            let mut t = top.clone();
            super::shuffle(&mut t);
            app.actions.push(Action::Play(t, 0));
        }
        if let Some(first) = top.first() {
            if pill(ui, "Radio", Some(icon::RADIO), false).clicked() {
                app.actions.push(Action::Radio(first.clone()));
            }
        }
    });
    if !top.is_empty() {
        ui.add_space(26.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Top songs").font(theme::bold(24.0)).color(theme::TEXT));
            if let Some(pl) = &top_playlist {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if pill(ui, "Show all", None, false).clicked() {
                        app.actions.push(Action::Open(Target::Playlist(pl.clone())));
                    }
                });
            }
        });
        ui.add_space(12.0);
        for (i, t) in top.iter().enumerate().take(5) {
            let o = opts(app, t, None, true, now);
            let resp = track_row(ui, t, &o, &mut app.actions);
            if resp.clicked() {
                app.actions.push(Action::Play(top.clone(), i));
            }
            let acts = &mut app.actions;
            resp.context_menu(|ui| widgets::song_menu(ui, t, acts));
        }
    }
    card_row(ui, "a-albums", "Albums & singles", &albums, &mut app.actions);
    card_row(ui, "a-pl", "Featured on", &playlists, &mut app.actions);
    card_row(ui, "a-sim", "Fans might also like", &similar, &mut app.actions);
    if let Some(d) = desc.filter(|d| !d.trim().is_empty()) {
        shelf_header(ui, "About", None, None, false);
        let g = widgets::galley_wrapped(ui, d.trim(), theme::regular(14.5), theme::TEXT_DIM, ui.available_width().min(820.0), 8);
        ui.label(g);
    }
}

// ------------------------------------------------------------------ library

fn library(app: &mut App, ui: &mut Ui, tab: LibTab, now: f64) {
    page_title(ui, "Library");
    if !app.logged_in {
        ui.add_space(10.0);
        ui.label(RichText::new("Sign in to see your playlists, liked songs and history.").color(theme::TEXT_DIM));
        ui.add_space(12.0);
        if pill(ui, "Sign in with Google", Some(icon::CIRCLE_USER), true).clicked() {
            app.start_sign_in(ui.ctx());
        }
        return;
    }
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        for (t, label) in [(LibTab::Playlists, "Playlists"), (LibTab::Songs, "Liked songs"), (LibTab::Albums, "Albums"), (LibTab::History, "History")] {
            if chip(ui, label, tab == t).clicked() && tab != t {
                // Tabs swap in place (no history entry).
                app.page = Page::Library(t);
                app.page_loaded();
            }
        }
    });
    ui.add_space(12.0);
    let grid = |ui: &mut Ui, cards: &[Card], acts: &mut Vec<Action>| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(20.0, 22.0);
            for c in cards {
                card(ui, c, CARD, acts);
            }
        });
    };
    match tab {
        LibTab::History => {
            let Some(h) = app.history.clone() else {
                widgets::skeleton(ui, now, 1, 48.0);
                return;
            };
            list(app, ui, &h, now);
        }
        _ => {
            let Some(lib) = &app.library else {
                widgets::skeleton(ui, now, 2, CARD);
                return;
            };
            match tab {
                LibTab::Playlists => {
                    let cards = lib.playlists.clone();
                    grid(ui, &cards, &mut app.actions)
                }
                LibTab::Albums => {
                    let cards = lib.albums.clone();
                    grid(ui, &cards, &mut app.actions)
                }
                LibTab::Songs => {
                    let liked = lib.liked.clone();
                    if !liked.is_empty() {
                        ui.horizontal(|ui| {
                            if pill(ui, "Play", Some(icon::PLAY), true).clicked() {
                                app.actions.push(Action::Play(liked.clone(), 0));
                            }
                            if pill(ui, "Shuffle", Some(icon::SHUFFLE), false).clicked() {
                                let mut t = liked.clone();
                                super::shuffle(&mut t);
                                app.actions.push(Action::Play(t, 0));
                            }
                        });
                        ui.add_space(10.0);
                    }
                    list(app, ui, &liked, now);
                }
                LibTab::History => unreachable!(),
            }
        }
    }
}

fn list(app: &mut App, ui: &mut Ui, tracks: &[Track], now: f64) {
    for (i, t) in tracks.iter().enumerate() {
        let o = opts(app, t, None, true, now);
        let resp = track_row(ui, t, &o, &mut app.actions);
        if resp.clicked() {
            app.actions.push(Action::Play(tracks.to_vec(), i));
        }
        let acts = &mut app.actions;
        resp.context_menu(|ui| widgets::song_menu(ui, t, acts));
    }
}

// ------------------------------------------------------------------ settings

fn settings(app: &mut App, ui: &mut Ui) {
    page_title(ui, "Settings");
    let section = |ui: &mut Ui, title: &str, body: &mut dyn FnMut(&mut Ui)| {
        ui.add_space(18.0);
        egui::Frame::new().fill(theme::glass(0.06)).corner_radius(14).inner_margin(20).show(ui, |ui| {
            ui.set_width(ui.available_width().min(760.0));
            ui.label(RichText::new(title).font(theme::bold(18.0)));
            ui.add_space(10.0);
            body(ui);
        });
    };
    let busy = app.signing_in.load(Ordering::Relaxed);
    let logged_in = app.logged_in;
    let name = app.account_name.clone();
    let mut sign_in = false;
    let mut sign_out = false;
    let mut cookie_submit = None;
    let cookie = &mut app.cookie_input;
    section(ui, "Account", &mut |ui| {
        if logged_in {
            ui.label(format!("Signed in{}", name.as_ref().map(|n| format!(" as {n}")).unwrap_or_default()));
            ui.add_space(8.0);
            sign_out = pill(ui, "Sign out", Some(icon::LOG_OUT), false).clicked();
        } else {
            ui.label(RichText::new("Sign in to get your home feed, library and likes.").color(theme::TEXT_DIM));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                sign_in = !busy && pill(ui, "Sign in with Google", Some(icon::CIRCLE_USER), true).clicked();
                if busy {
                    ui.spinner();
                    ui.label(RichText::new("Finish signing in in the window that opened").color(theme::TEXT_DIM));
                }
            });
            ui.add_space(10.0);
            egui::CollapsingHeader::new("Other option: paste cookies").show(ui, |ui| {
                ui.label(
                    RichText::new(
                        "Paste the `cookie` request header from a signed-in music.youtube.com tab (DevTools → Network → any request), or a cookies.txt export.",
                    )
                    .color(theme::TEXT_DIM),
                );
                ui.add(egui::TextEdit::multiline(cookie).hint_text("cookie header or cookies.txt").desired_rows(3).desired_width(f32::INFINITY));
                if ui.add_enabled(!cookie.trim().is_empty(), egui::Button::new("Sign in")).clicked() {
                    cookie_submit = Some(std::mem::take(cookie));
                }
            });
        }
    });
    if sign_in {
        app.start_sign_in(ui.ctx());
    }
    if sign_out {
        app.backend.request(crate::backend::Req::Logout);
    }
    if let Some(c) = cookie_submit {
        app.backend.request(crate::backend::Req::SetCookie(c));
    }
    let (autoplay, anim, reactive) = (&mut app.autoplay, &mut app.anim_bg, &mut app.reactive_bg);
    section(ui, "Playback & visuals", &mut |ui| {
        ui.checkbox(autoplay, "Autoplay similar songs when the queue ends");
        ui.checkbox(anim, "Animated album-art background (Kawarp)");
        ui.checkbox(reactive, "Background pulses with the music");
    });
    section(ui, "Shortcuts", &mut |ui| {
        for (k, v) in [
            ("Space", "Play / pause"),
            ("Ctrl + → / ←", "Next / previous"),
            ("Ctrl + ↑ / ↓", "Volume"),
            ("Ctrl + F", "Search"),
            ("L", "Lyrics / now playing"),
            ("F", "Full-screen lyrics (in now playing)"),
            ("Alt + ← / →, mouse back / forward", "Navigate"),
            ("Esc", "Close now playing"),
        ] {
            ui.horizontal(|ui| {
                ui.add_sized([250.0, 20.0], egui::Label::new(RichText::new(k).font(theme::semibold(13.5))));
                ui.label(RichText::new(v).color(theme::TEXT_DIM));
            });
        }
    });
    section(ui, "About", &mut |ui| {
        for line in [
            "Lyrics: Better Lyrics API (Musixmatch, LRCLIB, QQ, KuGou, Better Lyrics TTML)",
            "Background: port of Kawarp, MIT © Better Lyrics",
            "Inter typeface (SIL OFL 1.1) • Lucide icons (ISC)",
        ] {
            ui.label(RichText::new(line).color(theme::TEXT_DIM));
        }
    });
}
