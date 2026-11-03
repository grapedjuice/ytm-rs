//! Now-playing overlay: big art + synced lyrics / up next, over the full-strength
//! animated background. The lyrics themselves are drawn by `karaoke` (a port of the
//! better-lyrics renderer); this adds their toolbar and the plain-text fallback.

use egui::{Color32, Rect, Sense, Ui, Vec2, pos2, vec2};

use super::widgets::{self, icon_button, named, text_at};
use super::{Action, App, NpTab, karaoke};
use crate::lyrics::{Lyrics, Sync, SyncKind};
use crate::shader::Params;
use crate::theme::{self, icon};

pub fn overlay(app: &mut App, ctx: &egui::Context, now: f64) {
    use crate::preset::BgMode;
    let t = theme::anim_bool(ctx, "np-open", app.np_open && app.current().is_some(), 0.42);
    if t <= 0.001 {
        return;
    }
    let screen = ctx.content_rect();
    let full = if app.fullscreen { screen } else { Rect::from_min_max(screen.min, pos2(screen.right(), screen.bottom() - app.bar_height())) };
    let rect = full.translate(vec2(0.0, full.height() * (1.0 - t)));
    let look = app.look();
    let b = &look.background;
    egui::Area::new(egui::Id::unique("now-playing"))
        .order(egui::Order::Middle)
        .fixed_pos(rect.min)
        .show(ctx, |ui| {
            // Swallow input so the page underneath doesn't react.
            ui.allocate_rect(rect, Sense::click_and_drag());
            // It covers the window's top too: that still moves the window.
            if !app.fullscreen {
                super::titlebar::drag_area(ui, Rect::from_min_size(rect.min, vec2(rect.width(), super::titlebar::HEIGHT)), "now-playing");
            }
            let painter = ui.painter_at(rect);
            let themed = !app.default_theme;
            let art = matches!(b.mode, BgMode::Kawarp | BgMode::CoverStatic);
            if themed && b.mode == BgMode::CoverFullbleed {
                // The cover itself fills the screen behind a scrim. With the lyrics-only
                // layout on a wide window it fills the height on the left instead and
                // fades out toward the lyrics (reCover).
                let solid = look.solid();
                painter.rect_filled(rect, 0.0, solid);
                if let Some(track) = app.current() {
                    let side_by_side = look.cover.layout == crate::preset::Layout::LyricsOnly && rect.width() >= 900.0;
                    let area = if side_by_side { Rect::from_min_size(rect.min, Vec2::splat(rect.height())) } else { rect };
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
                    child.set_clip_rect(area);
                    widgets::paint_cover_fill(&child, area, &track.thumbs, Color32::from_gray((b.np_dim.clamp(0.0, 1.0) * 255.0) as u8));
                    if side_by_side {
                        let fade = Rect::from_min_max(pos2(area.center().x - area.width() * 0.1, area.top()), area.max);
                        super::shell::hgradient(&painter, fade, Color32::TRANSPARENT, solid);
                    }
                }
            } else if let Some(bg) = app.bg.as_ref().filter(|_| themed && art) {
                let blend = theme::ease_in_out(((now - app.art_changed) / 1.4) as f32);
                let level = if app.reactive_bg && app.playing() { app.audio.status.level() * b.reactive } else { 0.0 };
                bg.paint(
                    &painter,
                    rect,
                    Params {
                        time: app.bg_time as f32,
                        blend,
                        intensity: if b.mode == BgMode::CoverStatic { 0.0 } else { b.intensity },
                        saturation: b.saturation,
                        dim: b.np_dim,
                        scale: b.scale + level.min(0.5) * 0.12,
                    },
                );
            } else if themed && b.mode == BgMode::Solid {
                painter.rect_filled(rect, 0.0, look.solid());
            } else {
                painter.rect_filled(rect, 0.0, theme::lerp_color(theme::bg(), app.accent_color(), 0.25));
            }
            let k = if themed { b.scrim / 0.3 } else { 1.0 };
            super::shell::vgradient(ui.painter(), rect, theme::shade((0.18 * k).min(0.95)), theme::shade((0.42 * k).min(0.95)));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(32.0, 20.0))));
            body(app, &mut child, now);
        });
}

fn body(app: &mut App, ui: &mut Ui, now: f64) {
    use crate::preset::{Layout, Side};
    let Some(track) = app.current().cloned() else { return };
    let look = app.look();
    let cs = &look.cover;
    let area = ui.max_rect();
    let no_lyrics = app.no_lyrics();
    // Header: on the window buttons' line, when the app draws them.
    let lift = 20.0 - f32::from(super::titlebar::top_margin(20, 44.0));
    let header = Rect::from_min_size(area.min - vec2(0.0, if app.fullscreen { 0.0 } else { lift }), vec2(area.width(), 44.0));
    let mut hui = ui.new_child(egui::UiBuilder::new().max_rect(header).layout(egui::Layout::left_to_right(egui::Align::Center)));
    if icon_button(&mut hui, icon::CHEVRON_DOWN, 22.0, "Close now playing", false).clicked() {
        app.np_open = false;
        if app.fullscreen {
            app.set_fullscreen(ui.ctx(), false);
        }
    }
    hui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.add_space(super::titlebar::right_gap(ui.ctx(), ui.max_rect(), 0.0));
        let menu_right = ui.cursor().right();
        // Its actions (like, download, share…) are for what's heard: a song version
        // standing in for the music video.
        let heard = app.playing_track().cloned().unwrap_or_else(|| track.clone());
        options_menu(app, ui, &heard, app.np_tab == NpTab::Lyrics && !no_lyrics);
        // Just the ⋯ menu until the pointer comes near it; then Lyrics, Up next and full
        // screen slide out to its left, and stay while one of them has keyboard focus.
        // Their full width and whether one has focus, from the last frame they showed.
        let id = egui::Id::unique("np-header-extras-state");
        let (full_w, focused) = ui.ctx().data(|d| d.get_temp::<(f32, bool)>(id)).unwrap_or((220.0, false));
        // Around the ⋯ button and where they'll show, a little below the header too.
        let near = Rect::from_min_max(pos2(ui.cursor().right() - full_w - 48.0, header.top()), pos2(menu_right + 24.0, header.bottom() + 24.0));
        let hovered = ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| near.contains(p));
        let t = theme::anim_bool(ui.ctx(), "np-header-extras", hovered || focused, 0.2);
        if t <= 0.0 {
            ui.ctx().data_mut(|d| d.insert_temp(id, (full_w, false)));
            return;
        }
        ui.add_space(8.0);
        let (slot, _) = ui.allocate_exact_size(vec2(full_w * t, header.height()), Sense::hover());
        let mut eui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_max(pos2(slot.right() - full_w, slot.top()), slot.max))
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        eui.set_clip_rect(slot.intersect(ui.clip_rect()));
        eui.multiply_opacity(t);
        let ui = &mut eui;
        let (g, name) = if app.fullscreen { (icon::SHRINK, "Exit full screen") } else { (icon::EXPAND, "Full screen") };
        let fs_btn = icon_button(ui, g, 18.0, name, false);
        if fs_btn.clicked() {
            let fs = !app.fullscreen;
            app.set_fullscreen(ui.ctx(), fs);
        }
        ui.add_space(8.0);
        // Clicking the active chip again turns it off, leaving just the artwork.
        let up = widgets::chip(ui, "Up next", app.np_tab == NpTab::UpNext);
        if up.clicked() {
            app.np_tab = if app.np_tab == NpTab::UpNext { NpTab::Art } else { NpTab::UpNext };
        }
        // A song without lyrics: the chip is off and does nothing, but the choice stays
        // Lyrics so the next song that has them shows them.
        let lyr = ui.scope(|ui| {
            if no_lyrics {
                ui.disable();
                ui.multiply_opacity(0.45);
            }
            widgets::chip(ui, "Lyrics", app.np_tab == NpTab::Lyrics && !no_lyrics).on_disabled_hover_text("No lyrics for this song")
        });
        if lyr.inner.clicked() {
            app.np_tab = if app.np_tab == NpTab::Lyrics { NpTab::Art } else { NpTab::Lyrics };
        }
        let focused = fs_btn.has_focus() || up.has_focus() || lyr.inner.has_focus();
        let full_w = ui.min_rect().width();
        ui.ctx().data_mut(|d| d.insert_temp(id, (full_w, focused)));
    });

    let content = Rect::from_min_max(pos2(area.left(), header.bottom() + 12.0), area.max);
    // The side panel (lyrics / up next) slides in and out when its chip is toggled.
    // A song without lyrics shows just the artwork, with no transition.
    let last_id = egui::Id::unique("np-last-panel");
    let panel_tab = if app.np_tab == NpTab::Art {
        ui.ctx().data(|d| d.get_temp(last_id)).unwrap_or(NpTab::Lyrics)
    } else {
        ui.ctx().data_mut(|d| d.insert_temp(last_id, app.np_tab));
        app.np_tab
    };
    // Linear 0..1 over the transition; each part eases its own slice of it.
    let shown = ui.ctx().animate_bool_with_time(egui::Id::unique("np-panel"), app.np_tab != NpTab::Art, 0.6 * theme::SPEED);
    if shown > 0.0 && shown < 1.0 {
        // Every frame while sliding, not the now-playing view's usual 30 fps.
        ui.ctx().request_repaint();
    }
    let ppp = ui.ctx().pixels_per_point();
    let p = if panel_tab == NpTab::Lyrics && no_lyrics { 0.0 } else { shown };
    // Whichever is leaving slides and fades out over the first 45%, then the other
    // comes in over the last 70%, both easing out, in either direction.
    let (panel_vis, art_vis) = if app.np_tab != NpTab::Art {
        (theme::ease_out((p - 0.3) / 0.7), 1.0 - theme::ease_out(p / 0.45))
    } else {
        let q = 1.0 - p;
        (1.0 - theme::ease_out(q / 0.45), theme::ease_out((q - 0.3) / 0.7))
    };
    // The cover gliding between its two places (wide layouts): a gentle S-curve.
    let glide = p * p * (3.0 - 2.0 * p);
    let narrow = content.width() < 860.0;
    let lyrics_only = narrow || cs.layout == Layout::LyricsOnly;
    // reCover: the backdrop cover sits on the left, so the lyrics take the right side.
    let beside_cover = !narrow && cs.layout == Layout::LyricsOnly && look.background.mode == crate::preset::BgMode::CoverFullbleed;
    let centered = lyrics_only && !beside_cover;
    // Artwork alone: centered.
    let col_art = Rect::from_center_size(content.center(), vec2(content.width().min(content.height() * 0.9), content.height()));
    let slide = 72.0;
    let right = if lyrics_only {
        // No cover column next to the panel: the artwork fades in from the left as the panel leaves.
        let px = (art_side(cs, col_art) * ppp) as u32;
        if art_vis > 0.001 {
            let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(content));
            cui.multiply_opacity(art_vis);
            cover_column(app, &mut cui, col_art.translate(vec2(-slide * (1.0 - art_vis), 0.0)), &track, no_lyrics, now, px);
        } else {
            // Hidden behind the lyrics: have it loaded for when they're turned off.
            widgets::preload_cover(ui.ctx(), &track.thumbs, px);
        }
        if beside_cover { Rect::from_min_max(pos2(content.left() + content.width() * 0.46, content.top()), content.max) } else { content }
    } else {
        // Cover column, on the preset's side; it glides to the center as the panel leaves.
        let frac = if cs.layout == Layout::CoverHero { 0.5 } else { 0.42 };
        let col_w = content.width() * frac;
        let col_panel = if cs.side == Side::Right {
            Rect::from_min_size(pos2(content.right() - col_w, content.top()), vec2(col_w, content.height()))
        } else {
            Rect::from_min_size(content.min, vec2(col_w, content.height()))
        };
        let col = Rect::from_min_max(col_art.min.lerp(col_panel.min, glide), col_art.max.lerp(col_panel.max, glide));
        // One image size for the whole glide, the bigger of its two ends.
        let px = (art_side(cs, col_art).max(art_side(cs, col_panel)) * ppp) as u32;
        cover_column(app, ui, col, &track, no_lyrics, now, px);
        if cs.side == Side::Right {
            Rect::from_min_max(content.min, pos2(col_panel.left() - 28.0, content.bottom()))
        } else {
            Rect::from_min_max(pos2(col_panel.right() + 28.0, content.top()), content.max)
        }
    };
    if panel_vis <= 0.001 {
        return;
    }
    let toward = if !lyrics_only && cs.side == Side::Right { -slide } else { slide };
    let rect = right.translate(vec2(toward * (1.0 - panel_vis), 0.0));
    // Switching between Lyrics and Up next: the old one fades out over the first 55%,
    // the new one fades in and rises into place from a third of the way.
    for (tab, key) in [(NpTab::Lyrics, "np-tab-lyrics"), (NpTab::UpNext, "np-tab-upnext")] {
        let w = ui.ctx().animate_bool_with_time(egui::Id::unique(key), panel_tab == tab, 0.45 * theme::SPEED);
        if w > 0.0 && w < 1.0 {
            ui.ctx().request_repaint();
        }
        let vis = if panel_tab == tab {
            theme::ease_out((w - 0.35) / 0.65)
        } else {
            let x = ((w - 0.45) / 0.55).clamp(0.0, 1.0);
            x * x * (3.0 - 2.0 * x)
        };
        if vis <= 0.001 || (tab == NpTab::Lyrics && no_lyrics) {
            continue;
        }
        let rise = if panel_tab == tab { 18.0 * (1.0 - vis) } else { 0.0 };
        let r = rect.translate(vec2(0.0, rise));
        let mut rui = ui.new_child(egui::UiBuilder::new().max_rect(r));
        rui.set_clip_rect(content.intersect(ui.clip_rect()));
        rui.multiply_opacity(panel_vis * vis);
        match tab {
            NpTab::Lyrics => lyrics_panel(app, &mut rui, r, now, centered),
            _ => super::shell::queue_list(app, &mut rui, "np"),
        }
    }
}

/// Size of the artwork laid out in `col`.
fn art_side(cs: &crate::preset::CoverStyle, col: Rect) -> f32 {
    (col.width() * 0.82 * cs.size.clamp(0.5, 1.35)).min(col.height() - 110.0).max(160.0)
}

/// Artwork with the title and artists under it, laid out in `col`. `px`: the size to
/// fetch the image at (fixed while the column animates).
#[allow(clippy::too_many_arguments)]
fn cover_column(app: &mut App, ui: &mut Ui, col: Rect, track: &crate::model::Track, no_lyrics: bool, now: f64, px: u32) {
    use crate::preset::Shape;
    let look = app.look();
    let cs = &look.cover;
    let side = art_side(cs, col);
    let art = Rect::from_center_size(pos2(col.center().x, col.top() + side / 2.0 + (col.height() - side - 110.0).max(0.0) * 0.35), Vec2::splat(side));
    let round = cs.shape != Shape::Rounded;
    let radius = if round { side / 2.0 } else { cs.radius.clamp(0.0, side / 2.0) };
    let cr = egui::CornerRadius::same(radius.min(255.0) as u8);
    if cs.glow > 0.0 {
        let s = egui::epaint::Shadow { offset: [0, 16], blur: 90, spread: 6, color: theme::with_alpha(app.accent_color(), cs.glow.min(1.0) * 0.7) };
        ui.painter().add(s.as_shape(art, cr));
    }
    if cs.shadow > 0.0 {
        widgets::soft_shadow(ui, art, radius.min(255.0), 60.0, cs.shadow.min(1.0));
    }
    let tint = Color32::from_gray((cs.brightness.clamp(0.1, 1.0) * 255.0) as u8);
    if round {
        if cs.shape == Shape::Record && app.playing() {
            let dt = ui.input(|i| i.stable_dt).min(0.1) as f64;
            app.record_angle = (app.record_angle + dt * std::f64::consts::TAU / cs.spin_secs.max(4.0) as f64) % std::f64::consts::TAU;
        }
        let angle = if cs.shape == Shape::Record { app.record_angle as f32 } else { 0.0 };
        widgets::paint_disc(ui, art.center(), side / 2.0, &track.thumbs, angle, tint, px);
        if cs.shape == Shape::Record {
            // Vinyl: faint grooves and a spindle hole.
            let p = ui.painter();
            for i in 1..6 {
                p.circle_stroke(art.center(), side / 2.0 * (0.42 + i as f32 * 0.1), egui::Stroke::new(1.0, theme::shade(0.18)));
            }
            p.circle_filled(art.center(), side * 0.09, theme::shade(0.35));
            p.circle_filled(art.center(), side * 0.022, Color32::from_rgb(10, 10, 12));
        }
    } else {
        widgets::paint_cover_px(ui, art, &track.thumbs, radius, 1.0, px);
        if cs.brightness < 0.99 {
            ui.painter().rect_filled(art, cr, theme::shade(1.0 - cs.brightness));
        }
    }
    let cover = named(ui.interact(art, ui.scope_id().with(("cover-playback", &track.id)), Sense::click()), if app.playing() { "Pause" } else { "Play" })
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let hover = theme::anim_bool(ui.ctx(), ("cover-playback-hover", &track.id), cover.hovered(), 0.16);
    if hover > 0.0 {
        ui.painter().rect_filled(art, cr, theme::shade(0.28 * hover));
        ui.painter().circle_filled(art.center(), 38.0, theme::shade(0.58 * hover));
        widgets::icon_at(ui, art.center(), if app.playing() || app.buffering { icon::PAUSE } else { icon::PLAY }, 37.0, theme::with_alpha(Color32::WHITE, hover));
    }
    if cover.clicked() { app.actions.push(Action::TogglePlay); }
    let w = side;
    let title_y = art.bottom() + 22.0;
    text_at(ui, pos2(art.left(), title_y), &track.title, theme::bold(28.0), Color32::WHITE, w);
    // Each artist opens their own page (features included).
    let line = track.artist_line();
    widgets::linked_text(ui, pos2(art.left(), title_y + 40.0), &line, &track.artists, theme::regular(17.0), theme::text_dim(), w, &mut app.actions);
    if no_lyrics && app.np_tab == NpTab::Lyrics {
        // A brief note, faded out after a few seconds.
        let since = ui.ctx().memory_mut(|m| *m.data.get_temp_mut_or_insert_with(egui::Id::unique(("no-lyrics", &track.id)), || now));
        let a = (1.0 - ((now - since - 3.0) / 1.0) as f32).clamp(0.0, 1.0);
        if a > 0.0 {
            let c = theme::with_alpha(theme::text_dim(), a * 0.8);
            text_at(ui, pos2(art.left(), title_y + 68.0), "No lyrics for this song", theme::regular(14.0), c, w);
            // Frames only for the fade-out.
            let wait = (since + 3.0 - now).max(0.0);
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(if wait > 0.0 { wait } else { 0.033 }));
        }
    }
}

// ------------------------------------------------------------------ lyrics

fn lyrics_panel(app: &mut App, ui: &mut Ui, rect: Rect, now: f64, centered: bool) {
    // Lent out for the frame rather than cloned (the drawing needs `app` mutably).
    let Some((id, set)) = app.lyrics.take() else {
        ui.add_space(rect.height() * 0.35);
        ui.vertical_centered(|ui| {
            ui.spinner();
            ui.label(egui::RichText::new("Loading lyrics…").color(theme::text_dim()));
        });
        return;
    };
    if let Some(lyrics) = app.lyric_prefs.choose(&id, &set).and_then(|p| set.get(p)) {
        let panel = Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - 4.0));
        if lyrics.sync == Sync::None {
            plain_lyrics(app, ui, panel, lyrics);
        } else {
            synced(app, ui, panel, &id, lyrics, now, centered);
        }
    } else {
        ui.add_space(rect.height() * 0.4);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("No lyrics found for this song").font(theme::semibold(20.0)).color(theme::text_dim()));
        });
    }
    app.lyrics = Some((id, set));
}

/// Romanization and translation for each line, the way the extension picks them: what
/// the lyrics ship (Apple-style TTML) first, then Unison/Google.
fn decorations<'a>(
    show_roman: bool,
    show_trans: bool,
    target: &'a str,
    extras: Option<&'a (String, String, String, crate::translate::Extras)>,
    id: &str,
    lyrics: &'a Lyrics,
) -> (Vec<Option<karaoke::Roman<'a>>>, Vec<Option<&'a str>>) {
    let extras = extras.filter(|(v, s, t, _)| v == id && s == lyrics.provider.key() && t == target).map(|(_, _, _, e)| e);
    let same = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());
    let roman = lyrics
        .lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if !show_roman || l.instrumental {
                return None;
            }
            if let Some(r) = l.romanization.as_deref().filter(|r| !same(r, &l.text)) {
                return Some(if l.timed_roman.is_empty() { karaoke::Roman::Text(r) } else { karaoke::Roman::Timed(&l.timed_roman, r) });
            }
            let r = extras?.roman.get(i)?.as_deref()?;
            (!same(r, &l.text)).then_some(karaoke::Roman::Text(r))
        })
        .collect();
    let trans = lyrics
        .lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if !show_trans || l.instrumental {
                return None;
            }
            let t = super::lyric_prefs::shipped_translation(l, target).or_else(|| extras?.trans.get(i)?.as_deref())?;
            (!same(t, &l.text)).then_some(t)
        })
        .collect();
    (roman, trans)
}

fn synced(app: &mut App, ui: &mut Ui, rect: Rect, id: &str, lyrics: &Lyrics, now: f64, narrow: bool) {
    let style = &app.look().lyrics;
    let accent = app.accent_color();
    let centered = narrow || style.align == crate::preset::Align::Center;
    let playing = app.playing();
    // Audible position: the device plays what the player hands it a buffer later.
    let pos = app.audio.status.position_smooth().as_secs_f64() - app.audio.status.latency().as_secs_f64();
    let pos = app.lyric_clock.tick(pos, playing, now);
    let t = pos - app.lyric_prefs.offset(id, lyrics.provider, lyrics.sync == Sync::Word);
    let (roman, trans) = decorations(app.show_roman, app.show_trans, &app.lyric_prefs.target, app.lyrics_extras.as_ref(), id, lyrics);
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        id.hash(&mut h);
        lyrics.provider.key().hash(&mut h);
        app.theme_id.hash(&mut h);
        app.default_theme.hash(&mut h);
        for r in &roman {
            match r {
                Some(karaoke::Roman::Timed(_, s)) | Some(karaoke::Roman::Text(s)) => s.hash(&mut h),
                None => 0u8.hash(&mut h),
            }
        }
        trans.hash(&mut h);
        h.finish()
    };
    let scale = app.lyric_prefs.text_scale;
    let frame = karaoke::Frame { lyrics, key, roman, trans, t, playing, now, rect, style, accent, centered, scale };
    // The view lives in `app`; the frame borrows `app` for decorations, so swap it out.
    let mut view = std::mem::take(&mut app.lyric_view);
    let out = if karaoke::dev_off("view") { karaoke::Out { seek: None, animating: true, user_scrolling: false } } else { view.show(ui, frame) };
    app.lyric_view = view;
    if let Some(s) = out.seek {
        app.actions.push(Action::Seek(s as f32));
    }
    if out.animating {
        ui.ctx().request_repaint();
    }
    // The extension's `.autoscroll-resume-button` (YouTube Music's rem is 10 px): a frosted
    // pill under the top edge in the lyrics font at 600, 1.75rem; padding .5rem 1rem;
    // white 5% fill, 1px white 10% border, radius 1rem, over a 5px backdrop blur (which
    // reads as about twice those on screen). It slides down from -3em to 2em as it fades
    // in, and its text glows on hover.
    let shown = theme::anim_bool(ui.ctx(), "lyr-resume", out.user_scrolling, 0.2);
    if shown > 0.0 {
        let font = theme::lyric_font(&style.font, crate::preset::Weight::Semibold, 17.5);
        let g = widgets::galley(ui, "Resume Autoscroll", font, Color32::WHITE, rect.width() - 40.0);
        let size = g.size() + vec2(20.0, 10.0) + Vec2::splat(2.0);
        let em = 17.5;
        let y = rect.top() + 8.0 + egui::lerp(-3.0 * em..=0.5 * em, theme::ease_out(shown));
        let r = Rect::from_min_size(pos2(rect.center().x - size.x / 2.0, y), size);
        let resp = ui.interact(r, ui.scope_id().with("lyr-resume-btn"), if out.user_scrolling { Sense::click() } else { Sense::hover() });
        let resp = named(resp, "Resume Autoscroll").on_hover_cursor(egui::CursorIcon::PointingHand);
        let hover = theme::anim_bool(ui.ctx(), "lyr-resume-h", resp.hovered() && out.user_scrolling, 0.25);
        let p = ui.painter().with_clip_rect(rect.expand2(vec2(0.0, 4.0)));
        p.rect_filled(r, 10.0, theme::with_alpha(Color32::WHITE, 0.10 * shown));
        p.rect_stroke(r, 10.0, egui::Stroke::new(1.0, theme::with_alpha(Color32::WHITE, 0.2 * shown)), egui::StrokeKind::Inside);
        let at = r.center() - g.size() / 2.0;
        if hover > 0.0
            && let Some((tex, glow_rect, uv)) = app.lyric_view.text_glow(ui.ctx(), g.clone(), at, 15.0)
        {
            // `text-shadow: 0 0 1.5rem var(--blyrics-glow-color)`: a real blur of the letters.
            let glow = crate::preset::paint(&style.glow_color, accent);
            p.image(tex, glow_rect, uv, theme::with_alpha(glow, hover * shown));
        }
        p.galley_with_override_text_color(at, g, theme::with_alpha(Color32::WHITE, shown));
        if shown < 1.0 {
            ui.ctx().request_repaint();
        }
        if resp.clicked() {
            app.lyric_view.resume();
        }
    }
}

fn plain_lyrics(app: &mut App, ui: &mut Ui, rect: Rect, lyrics: &Lyrics) {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    egui::ScrollArea::vertical().id_salt("plain-lyrics").auto_shrink(false).show(&mut child, |ui| {
        ui.label(egui::RichText::new("These lyrics aren't synced").font(theme::regular(13.0)).color(theme::text_faint()));
        ui.add_space(14.0);
        let style = &app.look().lyrics;
        let size = (32.0 * style.size.clamp(0.6, 1.6)).min(rect.width() / 14.0).max(18.0);
        for l in &lyrics.lines {
            let font = theme::lyric_font_for(&style.font, style.weight, size, &l.text);
            let color = theme::with_alpha(Color32::WHITE, 0.85);
            if crate::bidi::has_rtl(&l.text) {
                // Wrapped in reading order, each row in display order; RTL lines start on the right.
                let width = ui.available_width();
                let g = super::widgets::galley_wrapped(ui, &l.text, font, color, width, usize::MAX);
                let (r, _) = ui.allocate_exact_size(vec2(width, g.size().y), Sense::hover());
                let x = if crate::bidi::rtl_base(&l.text) { r.right() - g.size().x } else { r.left() };
                ui.painter().galley(pos2(x, r.top()), g, color);
            } else {
                ui.label(egui::RichText::new(&l.text).font(font).color(color));
            }
            ui.add_space(size * 0.25);
        }
        ui.add_space(80.0);
    });
}

/// Sync-type colours from the extension's dock (syllable, word, line, unsynced).
fn sync_color(k: SyncKind) -> Color32 {
    match k {
        SyncKind::Syllable => Color32::from_rgb(196, 160, 255),
        SyncKind::Word => Color32::from_rgb(130, 200, 255),
        SyncKind::Line => Color32::from_rgb(255, 205, 110),
        SyncKind::Unsynced => Color32::from_rgb(170, 170, 170),
    }
}

fn sync_label(k: SyncKind) -> &'static str {
    match k {
        SyncKind::Syllable => "Syllable synced",
        SyncKind::Word => "Word synced",
        SyncKind::Line => "Line synced",
        SyncKind::Unsynced => "Not synced",
    }
}

/// Now playing's one options button, left of the chips: the song's actions, and with
/// lyrics showing, the source picker, translation and romanization toggles, and this
/// song's timing offset above them, so the header stays clean until you want them.
fn options_menu(app: &mut App, ui: &mut Ui, track: &crate::model::Track, lyrics: bool) {
    let (rect, resp) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::click());
    let resp = named(resp, "Options").on_hover_cursor(egui::CursorIcon::PointingHand);
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&resp));
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() || open, 0.12);
    ui.painter().circle_filled(rect.center(), 17.0, theme::glass(0.09 + 0.07 * h));
    for dx in [-6.0, 0.0, 6.0] {
        ui.painter().circle_filled(rect.center() + vec2(dx, 0.0), 1.9, theme::with_alpha(Color32::WHITE, 0.75 + 0.25 * h));
    }
    egui::Popup::menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        ui.set_width(270.0);
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        ui.add_space(4.0);
        if lyrics {
            lyric_options(app, ui);
            menu_divider(ui);
        }
        let liked = app.liked.contains(&track.id);
        widgets::song_actions(ui, track, liked, None, &mut app.actions);
        ui.add_space(4.0);
    });
}

fn lyric_options(app: &mut App, ui: &mut Ui) {
    let Some((id, set)) = app.lyrics.as_ref() else { return };
    let id = id.clone();
    let available = app.lyric_prefs.available(set);
    let Some(shown) = app.lyric_prefs.choose(&id, set) else { return };
    let n = available.len();
    let mut pick = None;
    menu_caption(ui, if n > 1 { "LYRICS SOURCE" } else { "LYRICS SOURCE · ONLY ONE FOUND" });
    for p in &available {
        if source_row(ui, p.name(), p.sync(), *p == shown) {
            pick = Some(*p);
        }
    }
    menu_divider(ui);
    let tip = format!("Translation · {}", lang_name(&app.lyric_prefs.target));
    switch_row(ui, &tip, &mut app.show_trans);
    switch_row(ui, "Romanization", &mut app.show_roman);
    // This song's timing for the shown source: ‹ +0.0s ›
    let off = app.lyric_prefs.song_offset(&id, shown);
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
    ui.painter().text(pos2(row.left() + 12.0, row.center().y), egui::Align2::LEFT_CENTER, "Timing", theme::regular(14.0), theme::text());
    let mut sub = ui.new_child(egui::UiBuilder::new().max_rect(row.shrink2(vec2(8.0, 0.0))).layout(egui::Layout::right_to_left(egui::Align::Center)));
    sub.spacing_mut().item_spacing.x = 2.0;
    if mini_icon(&mut sub, icon::CHEVRON_RIGHT, "Lyrics later (+0.1 s)", true).clicked() {
        app.lyric_prefs.set_song_offset(&id, shown, off + 0.1);
    }
    let label = format!("{off:+.1}s").replace("+0.0", "0.0");
    if mini_text(&mut sub, &label, false, "This song's lyrics timing. Double-click to reset.").double_clicked() {
        app.lyric_prefs.set_song_offset(&id, shown, 0.0);
    }
    if mini_icon(&mut sub, icon::CHEVRON_LEFT, "Lyrics earlier (−0.1 s)", true).clicked() {
        app.lyric_prefs.set_song_offset(&id, shown, off - 0.1);
    }
    if let Some(p) = pick.filter(|p| *p != shown) {
        app.lyric_prefs.pick(&id, set, p);
        log::info!("lyrics: showing {} ({}/{}) by choice", p.key(), available.iter().position(|x| *x == p).map_or(0, |i| i + 1), n);
        app.lyric_view.reset();
    }
}

fn menu_caption(ui: &mut Ui, text: &str) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    ui.painter().text(pos2(r.left() + 12.0, r.center().y + 2.0), egui::Align2::LEFT_CENTER, text, theme::semibold(11.0), theme::text_faint());
}

fn menu_divider(ui: &mut Ui) {
    ui.add_space(5.0);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(r.shrink2(vec2(8.0, 0.0)), 0.0, theme::glass(0.08));
    ui.add_space(5.0);
}

/// A lyrics source: its sync type's dot, name and sync type, with a check on the one shown.
fn source_row(ui: &mut Ui, name: &str, sync: SyncKind, current: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
    let resp = named(resp, &format!("Lyrics source {name}")).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
    if h > 0.0 || current {
        ui.painter().rect_filled(rect, 8.0, theme::glass(0.05 * current as u8 as f32 + 0.08 * h));
    }
    ui.painter().circle_filled(pos2(rect.left() + 19.0, rect.center().y), 3.5, sync_color(sync));
    let x = rect.left() + 40.0;
    text_at(ui, pos2(x, rect.top() + 6.0), name, theme::semibold(13.5), theme::text(), rect.right() - x - 34.0);
    text_at(ui, pos2(x, rect.top() + 24.0), sync_label(sync), theme::regular(12.0), theme::text_dim(), rect.right() - x - 34.0);
    if current {
        widgets::icon_at(ui, pos2(rect.right() - 18.0, rect.center().y), icon::CHECK, 16.0, theme::text());
    }
    resp.clicked()
}

/// A label with an on/off switch; the whole row toggles.
fn switch_row(ui: &mut Ui, label: &str, on: &mut bool) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    let resp = named(resp, label).on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.clicked() {
        *on = !*on;
    }
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
    if h > 0.0 {
        ui.painter().rect_filled(rect, 8.0, theme::glass(0.08 * h));
    }
    ui.painter().text(pos2(rect.left() + 12.0, rect.center().y), egui::Align2::LEFT_CENTER, label, theme::regular(14.0), theme::text());
    let t = theme::anim_bool(ui.ctx(), resp.id.with("t"), *on, 0.16);
    let track = Rect::from_center_size(pos2(rect.right() - 30.0, rect.center().y), vec2(36.0, 20.0));
    let accent = ui.ctx().data(|d| d.get_temp::<Color32>(egui::Id::unique("accent"))).unwrap_or(theme::RED);
    ui.painter().rect_filled(track, 10.0, theme::lerp_color(theme::glass(0.16 + 0.06 * h), accent, t));
    let knob = pos2(egui::lerp(track.left() + 10.0..=track.right() - 10.0, t), track.center().y);
    ui.painter().circle_filled(knob, 8.0, Color32::WHITE);
}

/// A small icon button (24 × 24).
fn mini_icon(ui: &mut Ui, glyph: char, tip: &str, enabled: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(22.0, 24.0), if enabled { Sense::click() } else { Sense::hover() });
    let resp = named(resp, tip).on_hover_text(tip);
    let resp = if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() && enabled, 0.12);
    if h > 0.0 {
        ui.painter().rect_filled(rect, 7.0, theme::glass(0.12 * h));
    }
    let a = if enabled { 0.6 + 0.35 * h } else { 0.25 };
    widgets::icon_at(ui, rect.center(), glyph, 14.0, theme::with_alpha(Color32::WHITE, a));
    resp
}

/// A small text toggle or label (24 px tall), highlighted when `on`.
fn mini_text(ui: &mut Ui, text: &str, on: bool, tip: &str) -> egui::Response {
    let font = theme::semibold(12.0);
    let g = ui.fonts_mut(|f| f.layout_no_wrap(text.to_owned(), font, Color32::WHITE));
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 12.0, 24.0), Sense::click());
    let resp = named(resp, tip).on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
    let gold = Color32::from_rgb(255, 214, 120);
    let fill = if on { theme::with_alpha(Color32::from_rgb(255, 196, 64), 0.16 + 0.06 * h) } else { theme::glass(0.12 * h) };
    if fill.a() > 0 {
        ui.painter().rect_filled(rect, 7.0, fill);
    }
    let color = if on { gold } else { theme::with_alpha(Color32::WHITE, 0.6 + 0.35 * h) };
    ui.painter().galley_with_override_text_color(rect.center() - g.size() / 2.0, g, color);
    resp
}

fn lang_name(code: &str) -> &str {
    super::lyric_prefs::LANGUAGES.iter().find(|(c, _)| *c == code).map_or(code, |(_, n)| n)
}
