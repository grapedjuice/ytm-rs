//! Now-playing overlay: big art + synced lyrics / up next, over the full-strength
//! animated background. The lyrics themselves are drawn by `karaoke` (a port of the
//! better-lyrics renderer); this adds their toolbar and the plain-text fallback.

use egui::{Color32, Rect, Sense, Ui, Vec2, pos2, vec2};

use super::widgets::{self, icon_button, menu_divider, named, text_at};
use super::{Action, App, NpTab, karaoke};
use crate::lyrics::{Lyrics, Sync, SyncKind};
use crate::shader::Params;
use crate::theme::{self, icon};

/// A video's glow: its colours now, where they're easing to, and its picture.
pub(super) struct Glow {
    current: Vec<[f32; 3]>,
    target: Vec<[f32; 3]>,
    tex: egui::TextureHandle,
}

pub fn overlay(app: &mut App, ctx: &egui::Context, now: f64) {
    // A spring's overshoot isn't for this: past its place it would leave a gap over the bar.
    let t = theme::anim_bool_timed(ctx, "np-open", app.np_open && app.current().is_some(), app.look().app.np_open_secs).min(1.0);
    if t <= 0.001 {
        // Closed: nothing to decode for. It picks up from the music's place when opened.
        app.cover_clip = None;
        if app.video.take().is_some() {
            app.video_frame = None;
            app.glow = None;
            app.restore_art();
        }
        return;
    }
    let screen = ctx.content_rect();
    let full = if app.fullscreen { screen } else { Rect::from_min_max(screen.min, pos2(screen.right(), screen.bottom() - app.bar_height())) };
    let rect = full.translate(vec2(0.0, full.height() * (1.0 - t)));
    let look = app.look();
    egui::Area::new(egui::Id::unique("now-playing"))
        .order(egui::Order::Middle)
        .fixed_pos(rect.min)
        .show(ctx, |ui| {
            // Swallow input so the page underneath doesn't react.
            ui.allocate_rect(rect, Sense::click_and_drag());
            // It covers the window's top too: that still moves the window.
            let top = Rect::from_min_size(rect.min, vec2(rect.width(), super::titlebar::HEIGHT));
            if !app.fullscreen {
                super::titlebar::drag_area(ui, top, "now-playing");
            } else {
                // Full screen has no title bar, but its top works like one: a double-click
                // leaves full screen, and dragging leaves it and moves the window.
                // Clicks only: sensing drags, it would take one started on the buttons over it.
                let strip = ui.interact(top, egui::Id::unique("np-full-top"), Sense::CLICK);
                if strip.double_clicked() {
                    app.set_fullscreen(ctx, false);
                } else if strip.is_pointer_button_down_on() && ui.input(|i| i.pointer.is_decidedly_dragging()) {
                    app.set_fullscreen(ctx, false);
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
            }
            // Laid out as if it ran under the player bar, which shows the rest of it.
            let under_bar = Rect::from_min_max(rect.min, rect.max + vec2(0.0, screen.bottom() - full.bottom()));
            backdrop(app, ui, under_bar, rect, now, true);
            let [pad_x, pad_y] = look.now_playing.fullscreen_padding.filter(|_| app.fullscreen).unwrap_or([32.0, 20.0]);
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(pad_x, pad_y))));
            body(app, &mut child, now);
        });
}

/// Now playing's background laid out over `rect`, drawn only inside `clip`. `step_glow`:
/// this is the frame's main drawing, which moves the video's glow along.
pub(super) fn backdrop(app: &mut App, ui: &mut Ui, rect: Rect, clip: Rect, now: f64, step_glow: bool) {
    use crate::preset::BgMode;
    let look = app.look();
    let b = &look.background;
    let ctx = ui.ctx().clone();
    let painter = ui.painter_at(clip);
    let art = matches!(b.mode, BgMode::Kawarp | BgMode::CoverStatic);
    // A video has the background.
    let on_video = (b.video_backdrop && app.video.as_ref().is_some_and(|(_, player)| player.is_some())).then_some(());
    let over = ((app.art_brightness - b.auto_dim_from) / (1.0 - b.auto_dim_from).max(0.01)).max(0.0);
    let auto_dim = 1.0 - on_video.and(b.video.auto_dim).unwrap_or(b.auto_dim) * over;
    if let (Some(layer), Some(bg)) = (b.layer, app.bg.as_ref()) {
        // The theme's still cover under everything, pulsing if it does.
        let pulse = if layer.pulse_secs > 0.0 { (now as f32 * std::f32::consts::TAU / layer.pulse_secs).sin() * layer.pulse } else { 0.0 };
        let rotate = if layer.rotate_secs > 0.0 { (now / layer.rotate_secs as f64 % 1.0) as f32 * std::f32::consts::TAU } else { 0.0 };
        // Turning, it needs room for its corners.
        let room = if layer.rotate_secs > 0.0 { 1.45 } else { 1.0 };
        bg.paint(&painter, rect, Params { time: 0.0, blend: 1.0, intensity: 0.0, saturation: layer.saturate.unwrap_or(b.saturation), dim: layer.opacity * layer.brightness.unwrap_or(1.0) * auto_dim, scale: (b.scale * room) + pulse, contrast: 1.0, dither: b.dither, rotate, alpha: 1.0 });
        if (layer.pulse_secs > 0.0 || layer.rotate_secs > 0.0) && app.playing() {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
    }
    if b.mode == BgMode::CoverFullbleed {
        painter.rect_filled(rect, 0.0, look.solid());
    } else if let Some(bg) = app.bg.as_ref().filter(|_| art) {
        let blend = theme::ease_in_out(((now - app.art_changed - b.crossfade_delay as f64) / b.crossfade_secs.max(0.05) as f64) as f32);
        let level = if app.reactive_bg && app.playing() && b.mode != BgMode::CoverStatic { (app.audio.status.level() - b.beat_threshold).max(0.0) * b.reactive } else { 0.0 };
        // Over a layer the dim is the canvas's opacity (the theme's `opacity`): the
        // layer shows through instead of black, so only `auto_dim` darkens it.
        let (dim, alpha) = if b.layer.is_some() { (auto_dim, b.np_dim) } else { (b.np_dim * auto_dim, 1.0) };
        bg.paint(
            &painter,
            rect,
            Params {
                time: app.bg_time as f32,
                blend,
                intensity: if b.mode == BgMode::CoverStatic { 0.0 } else { b.intensity },
                saturation: on_video.and(b.video.saturation).unwrap_or(b.saturation),
                dim,
                scale: b.scale + level.min(0.5) * 0.12,
                contrast: b.np_contrast,
                dither: on_video.and(b.video.dither).unwrap_or(b.dither),
                rotate: 0.0,
                alpha,
            },
        );
    } else if b.mode == BgMode::Solid {
        painter.rect_filled(rect, 0.0, crate::preset::hex(&look.app.np_solid).unwrap_or(look.solid()));
    } else {
        painter.rect_filled(rect, 0.0, theme::lerp_color(theme::bg(), app.accent_color(), 0.25));
    }
    if b.fullbleed(app.fullscreen) {
        // The cover itself fills the screen behind a scrim. With the lyrics-only
        // layout on a wide window it fills the height on the left instead and
        // fades out toward the lyrics (reCover).
        if let Some(track) = app.current() {
            let side_by_side = look.cover.layout_in(app.fullscreen) == crate::preset::Layout::LyricsOnly && rect.width() >= 900.0;
            let area = if side_by_side { Rect::from_min_size(rect.min, Vec2::splat(rect.height())) } else { rect };
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
            child.set_clip_rect(area.intersect(clip));
            let tint = Color32::from_gray((b.fullscreen_fullbleed.filter(|_| app.fullscreen).unwrap_or(b.np_dim).clamp(0.0, 1.0) * 255.0) as u8);
            // Over the theme's own background it fades away to show that; alone, into its flat colour.
            let mask = (side_by_side && b.mode != BgMode::CoverFullbleed).then_some(b.fade);
            let fading = b.fullbleed_fade.then(|| fullbleed_fade(&child, &track.id, &track.thumbs, now, b.crossfade_delay as f64, b.crossfade_secs as f64)).flatten();
            match fading {
                Some((old, k)) => {
                    paint_fullbleed(&child, area, &old, tint, mask);
                    paint_fullbleed(&child, area, &track.thumbs, tint.gamma_multiply(k), mask);
                }
                None => paint_fullbleed(&child, area, &track.thumbs, tint, mask),
            }
            if side_by_side && mask.is_none() {
                let solid = look.solid();
                let fade = Rect::from_min_max(pos2(area.left() + area.width() * b.fade[0], area.top()), pos2(area.left() + area.width() * b.fade[1], area.bottom()));
                super::shell::hgradient(&painter, fade, Color32::TRANSPARENT, solid);
                painter.rect_filled(Rect::from_min_max(pos2(fade.right(), area.top()), area.max), 0.0, solid);
            }
        }
    }
    let k = b.scrim / 0.3;
    if let Some([from, to]) = b.scrim_gradient {
        if b.scrim_across {
            super::shell::hgradient(&painter, rect, theme::shade(from), theme::shade(to));
        } else {
            super::shell::vgradient(&painter, rect, theme::shade(from), theme::shade(to));
        }
    } else if k > 0.0 {
        super::shell::vgradient(&painter, rect, theme::shade((0.18 * k).min(0.95)), theme::shade((0.42 * k).min(0.95)));
    }
    for o in &b.overlays {
        super::shell::overlay_light(&painter, rect, o, app.accent_color());
    }
    if app.ambient_light && app.shown_video().is_some() {
        paint_glow(app, ui, rect, clip, step_glow);
    }
}

/// A cover filling `area`. `mask`: it's whole up to the first fraction across and gone by
/// the second (`mask-image`), for what's under it to show through; `area` is then square.
fn paint_fullbleed(ui: &Ui, area: Rect, thumbs: &[crate::model::Thumb], tint: Color32, mask: Option<[f32; 2]>) {
    let Some([from, to]) = mask else { return widgets::paint_cover_fill(ui, area, thumbs, tint) };
    let want = (area.width() * ui.ctx().pixels_per_point()) as u32;
    let px = [360u32, 544, 720, 960, 1200].into_iter().find(|&s| s >= want).unwrap_or(1200);
    let Some(t) = crate::model::pick_thumb(thumbs, px) else { return };
    let url = crate::model::sized(&t.url, px);
    let Ok(egui::load::TexturePoll::Ready { texture }) = egui::Image::new(url).load_for_size(ui.ctx(), area.size()) else { return };
    let uv = t.square_uv();
    let mut mesh = egui::Mesh::with_texture(texture.id);
    for (i, (f, a)) in [(0.0f32, 1.0f32), (from, 1.0), (to, 0.0), (1.0, 0.0)].into_iter().enumerate() {
        let color = tint.gamma_multiply(a);
        let (x, u) = (area.left() + area.width() * f, uv.left() + uv.width() * f);
        mesh.vertices.push(egui::epaint::Vertex { pos: pos2(x, area.top()), uv: pos2(u, uv.top()), color });
        mesh.vertices.push(egui::epaint::Vertex { pos: pos2(x, area.bottom()), uv: pos2(u, uv.bottom()), color });
        if i > 0 {
            let n = i as u32 * 2;
            mesh.add_triangle(n - 2, n - 1, n);
            mesh.add_triangle(n - 1, n + 1, n);
        }
    }
    ui.painter().add(mesh);
}

fn body(app: &mut App, ui: &mut Ui, now: f64) {
    let Some(track) = app.current().cloned() else { return };
    let look = app.look();
    let area = ui.max_rect();
    let video = app.shown_video().cloned();
    let video_mode = video.is_some();
    let video_lyrics = video_mode && look.now_playing.video_lyrics && !app.no_lyrics() && content_wide(area);
    let no_lyrics = app.no_lyrics() || (video_mode && !video_lyrics);
    // Header: on the window buttons' line, when the app draws them.
    let lift = 20.0 - f32::from(super::titlebar::top_margin(20, 44.0));
    let header = Rect::from_min_size(area.min - vec2(0.0, if app.fullscreen { 0.0 } else { lift }), vec2(area.width(), 44.0));
    // In some themes full screen's header leaves until the pointer moves. It comes back at once, and for the keyboard too.
    let hide = look.now_playing.header_autohide && app.fullscreen;
    let moved_id = egui::Id::unique("np-pointer-moved");
    if hide {
        let moving = ui.input(|i| i.pointer.delta() != Vec2::ZERO || !i.events.is_empty());
        if moving {
            ui.ctx().data_mut(|d| d.insert_temp(moved_id, now));
        }
    }
    let quiet = hide && now - ui.ctx().data(|d| d.get_temp::<f64>(moved_id)).unwrap_or(now) > 2.5;
    let shown_header = theme::anim_bool(ui.ctx(), "np-header-shown", !quiet, 0.3);
    let header_real = header;
    let header = header.translate(vec2(0.0, -(header.height() + 20.0) * (1.0 - shown_header)));
    if hide && !quiet {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
    }
    // The content first: some themes run the lyrics up under the header in full screen,
    // and its buttons go over them, taking the clicks.
    let content = Rect::from_min_max(pos2(area.left(), header_real.bottom() + 12.0), area.max);
    if app.fullscreen && look.now_playing.fullscreen_progress_px > 0.0 {
        fullscreen_progress(app, ui, ui.ctx().content_rect(), look.now_playing.fullscreen_progress_px);
    }
    np_content(app, ui, content, &track, video.as_ref(), video_lyrics, no_lyrics, now);
    // A song gets the switch too once its music video is found (or is playing).
    if look.now_playing.show_video_switch && !app.jam.active() && (track.has_video() || app.playing_as().is_some() || app.song_video().is_some()) {
        song_video_switch(app, ui, header);
    }
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
        let mut focused = fs_btn.has_focus();
        // Clicking the active chip again turns it off, leaving just the artwork. These look
        // the same in every theme.
        // Comments, rightmost of the three (right to left: drawn first), with Settings ›
        // Playback › Show comments on.
        if !app.show_comments && app.np_tab == NpTab::Comments {
            app.np_tab = NpTab::Art;
        }
        let com = app.show_comments.then(|| widgets::default_chip(ui, "Comments", app.np_tab == NpTab::Comments));
        if com.as_ref().is_some_and(|c| c.clicked()) {
            app.np_tab = if app.np_tab == NpTab::Comments { NpTab::Art } else { NpTab::Comments };
        }
        let up = widgets::default_chip(ui, "Up next", app.np_tab == NpTab::UpNext);
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
            widgets::default_chip(ui, "Lyrics", app.np_tab == NpTab::Lyrics && !no_lyrics).on_disabled_hover_text(if video_mode { "Lyrics are timed to the song: switch to Song" } else { "No lyrics for this song" })
        });
        if lyr.inner.clicked() {
            app.np_tab = if app.np_tab == NpTab::Lyrics { NpTab::Art } else { NpTab::Lyrics };
        }
        focused |= up.has_focus() || lyr.inner.has_focus() || com.is_some_and(|c| c.has_focus());
        let full_w = ui.min_rect().width();
        ui.ctx().data_mut(|d| d.insert_temp(id, (full_w, focused)));
    });
}

/// What's under the header: the video, or the cover and the panel beside it.
#[allow(clippy::too_many_arguments)]
fn np_content(app: &mut App, ui: &mut Ui, content: Rect, track: &crate::model::Track, video: Option<&crate::model::Track>, video_lyrics: bool, no_lyrics: bool, now: f64) {
    use crate::preset::{Layout, Side};
    let look = app.look();
    let cs = &look.cover;
    let layout = cs.layout_in(app.fullscreen);
    let cover_side = cs.fullscreen.filter(|_| app.fullscreen).and_then(|f| f.side).unwrap_or(cs.side);
    if let Some(video) = video {
        // Lyrics beside the video, with the video keeping the rest of the view.
        if video_lyrics && app.np_tab == NpTab::Lyrics {
            let share = look.now_playing.video_panel_frac.map_or(0.63, |[win, full]| if app.fullscreen { full } else { win });
            let split = content.left() + content.width() * share;
            video_stage(app, ui, Rect::from_min_max(content.min, pos2(split - 24.0, content.bottom())), video);
            let r = Rect::from_min_max(pos2(split, content.top()), content.max);
            let mut rui = ui.new_child(egui::UiBuilder::new().max_rect(r));
            rui.set_clip_rect(r.intersect(ui.clip_rect()));
            lyrics_panel(app, &mut rui, r, now, false);
        } else {
            video_stage(app, ui, content, video);
        }
        return;
    }
    if app.video.take().is_some() {
        app.video_frame = None;
        app.glow = None;
        app.restore_art();
    }
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
    let window_w = ui.ctx().content_rect().width();
    let wait = look.now_playing.no_lyrics.center_after;
    let p = if panel_tab == NpTab::Lyrics && no_lyrics {
        if wait > 0.0 {
            // A while in its usual place, then over to the middle.
            let since = ui.ctx().memory_mut(|m| *m.data.get_temp_mut_or_insert_with(egui::Id::unique(("no-lyrics-wait", &track.id)), || now));
            if now - since < wait as f64 {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(wait as f64 - (now - since) + 0.01));
            }
            1.0 - ui.ctx().animate_bool_with_time(egui::Id::unique(("no-lyrics-move", &track.id)), now - since >= wait as f64, 0.6)
        } else {
            0.0
        }
    } else {
        shown
    };
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
    let corner = !narrow && layout == Layout::Corner;
    let fs_corner = !narrow && app.fullscreen && cs.fullscreen.is_some_and(|f| f.corner_layout);
    let lyrics_only = narrow || corner || fs_corner || layout == Layout::LyricsOnly;
    // reCover: the backdrop cover sits on the left, so the lyrics take the right side.
    let beside_cover = !narrow && layout == Layout::LyricsOnly && look.background.fullbleed(app.fullscreen);
    let centered = lyrics_only && !beside_cover;
    // Artwork alone: centered.
    let col_art = Rect::from_center_size(content.center(), vec2(content.width().min(content.height() * 0.9), content.height()));
    let slide = 72.0;
    let corner_h = (content.height() * 0.24).clamp(96.0, 190.0);
    let right = if fs_corner {
        // A small cover at the bottom left with the title beside it, the lyrics on the right.
        let share = look.now_playing.fullscreen_panel_frac.unwrap_or(0.5);
        let split = content.right() - content.width() * share;
        // Nothing to go beside (the Art tab, no lyrics): the cover on its own, in the middle.
        if art_vis > 0.001 {
            let px = (art_side(cs, col_art, app.fullscreen, window_w) * ppp) as u32;
            let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(content));
            cui.multiply_opacity(art_vis);
            cover_column(app, &mut cui, col_art.translate(vec2(-slide * (1.0 - art_vis), 0.0)), track, no_lyrics, now, px);
        }
        if art_vis < 0.999 {
            let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(content));
            cui.multiply_opacity(1.0 - art_vis);
            corner_block(app, &mut cui, Rect::from_min_max(pos2(content.left(), content.bottom() - corner_h), pos2(split - 24.0, content.bottom())), track);
        }
        Rect::from_min_max(pos2(split, content.top()), content.max)
    } else if corner {
        // A small cover in the corner, the title beside it, the lyrics over them.
        corner_block(app, ui, Rect::from_min_max(pos2(content.left(), content.bottom() - corner_h), content.max), track);
        Rect::from_min_max(content.min, pos2(content.right(), content.bottom() - corner_h - 12.0))
    } else if lyrics_only {
        // No cover column next to the panel: the artwork fades in from the left as the panel leaves.
        let px = (art_side(cs, col_art, app.fullscreen, window_w) * ppp) as u32;
        if art_vis > 0.001 {
            let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(content));
            cui.multiply_opacity(art_vis);
            cover_column(app, &mut cui, col_art.translate(vec2(-slide * (1.0 - art_vis), 0.0)), track, no_lyrics, now, px);
        } else {
            // Hidden behind the lyrics: have it loaded for when they're turned off.
            widgets::preload_cover(ui.ctx(), &track.thumbs, px);
        }
        // The card over the backdrop cover, while the lyrics are up.
        if beside_cover && look.now_playing.info_card_text_only && app.fullscreen && art_vis < 1.0 {
            let mut card = ui.new_child(egui::UiBuilder::new().max_rect(content));
            card.multiply_opacity(1.0 - art_vis);
            info_card(app, &mut card, content, track);
        }
        let full_panel = look.now_playing.fullscreen_panel_frac.filter(|_| app.fullscreen);
        if let Some(f) = full_panel {
            let side = content.width() * (1.0 - f);
            if cover_side == Side::Right { Rect::from_min_max(content.min, pos2(content.right() - side, content.bottom())) } else { Rect::from_min_max(pos2(content.left() + side, content.top()), content.max) }
        } else if beside_cover { Rect::from_min_max(pos2(content.left() + content.width() * 0.46, content.top()), content.max) } else { content }
    } else {
        // Cover column, on the preset's side; it glides to the center as the panel leaves.
        let frac = match look.now_playing.panel_frac {
            Some([win, full]) => if app.fullscreen { full } else { win },
            None if layout == Layout::CoverHero => 0.5,
            None => 0.42,
        };
        // Tabs in a strip sit over the panel, so it starts below them.
        let col_w = content.width() * frac;
        let col_panel = if cover_side == Side::Right {
            Rect::from_min_size(pos2(content.right() - col_w, content.top()), vec2(col_w, content.height()))
        } else {
            Rect::from_min_size(content.min, vec2(col_w, content.height()))
        };
        let col = Rect::from_min_max(col_art.min.lerp(col_panel.min, glide), col_art.max.lerp(col_panel.max, glide));
        // One image size for the whole glide, the bigger of its two ends.
        let px = (art_side(cs, col_art, app.fullscreen, window_w).max(art_side(cs, col_panel, app.fullscreen, window_w)) * ppp) as u32;
        cover_column(app, ui, col, track, no_lyrics, now, px);
        if cover_side == Side::Right {
            Rect::from_min_max(content.min, pos2(col_panel.left() - 28.0, content.bottom()))
        } else {
            Rect::from_min_max(pos2(col_panel.right() + 28.0, content.top()), content.max)
        }
    };
    if panel_vis <= 0.001 {
        return;
    }
    let toward = if !lyrics_only && cover_side == Side::Right { -slide } else { slide };
    let rect = right.translate(vec2(toward * (1.0 - panel_vis), 0.0));
    let inset = look.now_playing.fullscreen_panel_inset.filter(|_| app.fullscreen);
    let rect = inset.map_or(rect, |[top, bottom]| Rect::from_min_max(pos2(rect.left(), rect.top() + top), pos2(rect.right(), rect.bottom() - bottom)));
    // Switching between Lyrics, Up next and Comments: the old one fades out over the first 55%,
    // the new one fades in and rises into place from a third of the way.
    for (tab, key) in [(NpTab::Lyrics, "np-tab-lyrics"), (NpTab::UpNext, "np-tab-upnext"), (NpTab::Comments, "np-tab-comments")] {
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
        rui.set_clip_rect(if inset.is_some() { ui.clip_rect() } else { content.intersect(ui.clip_rect()) });
        rui.multiply_opacity(panel_vis * vis);
        match tab {
            NpTab::Lyrics => lyrics_panel(app, &mut rui, r, now, centered),
            NpTab::Comments => super::comments::panel(app, &mut rui),
            _ => super::shell::queue_list(app, &mut rui, "np"),
        }
    }
}

/// While a fullbleed cover crossfades to the next: the one it's leaving, and how far along.
fn fullbleed_fade(ui: &Ui, id: &str, thumbs: &[crate::model::Thumb], now: f64, delay: f64, secs: f64) -> Option<(Vec<crate::model::Thumb>, f32)> {
    #[derive(Clone)]
    struct Fade {
        id: String,
        thumbs: Vec<crate::model::Thumb>,
        old: Vec<crate::model::Thumb>,
        at: f64,
    }
    let key = egui::Id::unique("fullbleed-fade");
    let mut f = ui.ctx().data(|d| d.get_temp::<Fade>(key)).unwrap_or(Fade { id: id.to_owned(), thumbs: thumbs.to_vec(), old: Vec::new(), at: f64::MIN });
    if f.id != id {
        f = Fade { id: id.to_owned(), thumbs: thumbs.to_vec(), old: f.thumbs, at: now };
        ui.ctx().data_mut(|d| d.insert_temp(key, f.clone()));
    } else if f.at == f64::MIN {
        ui.ctx().data_mut(|d| d.insert_temp(key, f.clone()));
    }
    let k = theme::ease_in_out(((now - f.at - delay) / secs.max(0.05)) as f32);
    if k >= 1.0 || f.old.is_empty() {
        return None;
    }
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
    Some((f.old, k))
}

/// A seek bar along the bottom of full screen, with the times on it.
fn fullscreen_progress(app: &mut App, ui: &mut Ui, screen: Rect, px: f32) {
    let status = app.audio.status.clone();
    let total = status.duration().map_or(0.0, |d| d.as_secs_f32());
    let pos = status.position_smooth().as_secs_f32().min(total.max(0.0));
    let r = Rect::from_min_max(pos2(screen.left(), screen.bottom() - px), screen.max);
    let resp = ui.interact(r, ui.scope_id().with("full-seek"), Sense::click());
    let resp = named(resp, "Seek").on_hover_cursor(egui::CursorIcon::PointingHand);
    let bar_r = app.look().now_playing.fullscreen_progress_radius;
    ui.painter().rect_filled(r, bar_r, theme::with_alpha(Color32::WHITE, 0.12));
    if total > 0.0 {
        let filled = Rect::from_min_size(r.min, vec2(r.width() * pos / total, r.height()));
        ui.painter().rect_filled(filled, bar_r, theme::with_alpha(Color32::WHITE, 0.32));
        let font = theme::semibold((px * 0.55).clamp(10.0, 14.0));
        ui.painter().text(pos2(r.left() + 12.0, r.center().y), egui::Align2::LEFT_CENTER, super::fmt_time(pos as u32), font.clone(), Color32::WHITE);
        ui.painter().text(pos2(r.right() - 12.0, r.center().y), egui::Align2::RIGHT_CENTER, super::fmt_time(total as u32), font, Color32::WHITE);
        if resp.clicked()
            && let Some(p) = resp.interact_pointer_pos()
        {
            app.actions.push(Action::Seek(((p.x - r.left()) / r.width()).clamp(0.0, 1.0) * total));
        }
    }
}

/// Room for lyrics beside something else.
fn content_wide(area: Rect) -> bool {
    area.width() >= 860.0
}

/// Size of the artwork laid out in `col`.
fn art_side(cs: &crate::preset::CoverStyle, col: Rect, fullscreen: bool, window_w: f32) -> f32 {
    let fs = cs.fullscreen.filter(|_| fullscreen).unwrap_or_default();
    let width_frac = fs.width_frac;
    let side = match fs.size_vh.or(cs.size_vh) {
        // As much of the window's height as the theme asks: it may run past the column.
        Some(vh) => (col.height() + 76.0) * vh,
        None => (col.width() * 0.82 * cs.size.clamp(0.5, 1.35)).min(col.height() - 110.0).max(160.0),
    };
    let side = match width_frac {
        Some(f) => col.width() * f,
        None => side,
    };
    let side = match cs.max_frac.filter(|_| width_frac.is_none()) {
        Some(f) => side.min(col.width() * f),
        None => side,
    };
    // By the window's width, when it has steps.
    let stepped = cs.size_steps.iter().rev().find(|s| window_w >= s[0]).map(|s| s[1]);
    side.min(fs.max_px.or(stepped).or(if fullscreen && cs.fullscreen.is_some() { None } else { cs.max_px }).unwrap_or(f32::INFINITY))
}

/// The corner layout's cover and title.
fn corner_block(app: &mut App, ui: &mut Ui, r: Rect, track: &crate::model::Track) {
    let look = app.look();
    let side = r.height();
    let art = Rect::from_min_size(r.min, Vec2::splat(side));
    let radius = look.cover.radius_pct.map_or(look.cover.radius, |p| p * side);
    if look.cover.shadow > 0.0 {
        widgets::soft_shadow(ui, art, radius.min(255.0), look.cover.shadow_blur, look.cover.shadow.min(1.0));
    }
    widgets::paint_cover_px(ui, art, &track.thumbs, radius, 1.0, (side * ui.ctx().pixels_per_point()) as u32);
    let cover = named(ui.interact(art, ui.scope_id().with(("corner-cover", &track.id)), Sense::click()), if app.playing() { "Pause" } else { "Play" }).on_hover_cursor(egui::CursorIcon::PointingHand);
    if cover.clicked() {
        app.actions.push(Action::TogglePlay);
    }
    let x = art.right() + 20.0;
    let w = (r.right() - x).max(40.0);
    let np = &look.now_playing;
    text_at(ui, pos2(x, r.bottom() - np.title_px - np.artist_px - 18.0), &track.title, theme::bold(np.title_px), Color32::WHITE, w);
    let line = track.artist_line();
    widgets::linked_text(ui, pos2(x, r.bottom() - np.artist_px - 8.0), &line, &track.artists, theme::regular(np.artist_px), theme::text_dim(), w, &mut app.actions);
}

/// The corner card with the song, over a cover that fills the view.
fn info_card(app: &mut App, ui: &mut Ui, content: Rect, track: &crate::model::Track) {
    let np = &app.look().now_playing;
    let size = vec2((content.width() * 0.34).clamp(220.0, 420.0), 76.0);
    let m = np.info_card_margin;
    let r = if np.info_card_top { Rect::from_min_size(content.min + vec2(m, m), size) } else { Rect::from_min_size(pos2(content.left() + m, content.bottom() - size.y - m), size) };
    let [card_title, card_artist] = np.info_card_weights.unwrap_or([crate::preset::Weight::Semibold, crate::preset::Weight::Regular]);
    if np.info_card_text_only {
        let line = track.artist_line();
        let (title_px, artist_px) = (np.title_px.min(18.0), np.artist_px.min(14.0));
        let room = (content.width() * 0.34).clamp(220.0, 420.0) - 28.0;
        let title_w = widgets::galley(ui, &track.title, theme::weighted(card_title, title_px), Color32::WHITE, room).size().x;
        let artist_w = widgets::galley(ui, &line, theme::weighted(card_artist, artist_px), Color32::WHITE, room).size().x;
        let r = Rect::from_min_size(r.min, vec2(title_w.max(artist_w).min(room) + 28.0, title_px * 1.3 + artist_px * 1.3 + 30.0));
        if let Some(g) = app.look().app.glass.title_card {
            crate::shader::frost(ui.painter(), r, crate::shader::Frost { look: g, radius: np.info_card_look[1], opacity: 1.0, fade: 0.0 });
        }
        ui.painter().rect_filled(r, np.info_card_look[1], theme::shade(np.info_card_look[0]));
        let at = r.min + vec2(14.0, 14.0);
        if let Some([y, a]) = np.title_shadow {
            text_at(ui, at + vec2(0.0, y), &track.title, theme::weighted(card_title, title_px), theme::shade(a), room);
        }
        text_at(ui, at, &track.title, theme::weighted(card_title, title_px), Color32::WHITE, room);
        widgets::linked_text(ui, at + vec2(0.0, title_px * 1.3 + 2.0), &line, &track.artists, theme::weighted(card_artist, artist_px), theme::text_dim(), room, &mut app.actions);
        return;
    }
    if let Some(g) = app.look().app.glass.title_card {
        crate::shader::frost(ui.painter(), r, crate::shader::Frost { look: g, radius: np.info_card_look[1], opacity: 1.0, fade: 0.0 });
    }
    ui.painter().rect_filled(r, np.info_card_look[1], theme::shade(np.info_card_look[0]));
    let art = Rect::from_min_size(r.min + vec2(10.0, 10.0), Vec2::splat(56.0));
    widgets::paint_cover(ui, art, &track.thumbs, 8.0, 1.0);
    let w = (r.right() - art.right() - 24.0).max(20.0);
    if let Some([y, a]) = np.title_shadow {
        text_at(ui, pos2(art.right() + 12.0, r.top() + 14.0 + y), &track.title, theme::weighted(card_title, np.title_px.min(18.0)), theme::shade(a), w);
    }
    text_at(ui, pos2(art.right() + 12.0, r.top() + 14.0), &track.title, theme::weighted(card_title, np.title_px.min(18.0)), Color32::WHITE, w);
    let line = track.artist_line();
    widgets::linked_text(ui, pos2(art.right() + 12.0, r.top() + 40.0), &line, &track.artists, theme::weighted(card_artist, np.artist_px.min(14.0)), theme::text_dim(), w, &mut app.actions);
}

/// Artwork with the title and artists under it, laid out in `col`. `px`: the size to
/// fetch the image at (fixed while the column animates).
#[allow(clippy::too_many_arguments)]
fn cover_column(app: &mut App, ui: &mut Ui, col: Rect, track: &crate::model::Track, no_lyrics: bool, now: f64, px: u32) {
    use crate::preset::Shape;
    let look = app.look();
    let cs = &look.cover;
    let np = &look.now_playing;
    let side = art_side(cs, col, app.fullscreen, ui.ctx().content_rect().width());
    // Opening now playing, the cover grows into place.
    let opened = if cs.open_scale < 1.0 || cs.open_slide != 0.0 || cs.open_fade { theme::anim_bool_timed(ui.ctx(), "np-cover-open", app.np_open, cs.open_secs) } else { 1.0 };
    let mut opening = if cs.open_scale < 1.0 { egui::lerp(cs.open_scale..=1.0, opened) } else { 1.0 };
    let fs = cs.fullscreen.filter(|_| app.fullscreen).unwrap_or_default();
    // Leaving full screen, it settles from a little bigger.
    // Going full screen, it grows from a little smaller. One value both ways, read every
    // frame: one read only on arriving would already be at its end.
    let mut settling = 0.0;
    let mut settle_fade = 1.0f32;
    if let Some(f) = cs.fullscreen.filter(|f| f.enter.is_some() || f.leave.is_some()) {
        let (step, shift) = if app.fullscreen { (f.enter, f.enter_shift_vh) } else { (f.leave, f.leave_shift_vh) };
        let secs = step.map_or(0.0, |[_, s]| s).max(0.01);
        let full = ui.ctx().animate_bool_with_time(egui::Id::unique("np-cover-full"), app.fullscreen, secs);
        let p = theme::ease_out(if app.fullscreen { full } else { 1.0 - full });
        if let Some([from, _]) = step {
            opening *= egui::lerp(from..=1.0, p);
            settling = shift * (1.0 - p);
            if f.settle_fade > 0.0 {
                settle_fade = (p / f.settle_fade).min(1.0);
            }
        }
    }
    let window_h = ui.ctx().content_rect().height();
    let art = Rect::from_center_size(pos2(col.center().x, col.top() + side / 2.0 + (col.height() - side - 110.0).max(0.0) * 0.35), Vec2::splat(side)).translate(vec2(cs.offset_vh[0], cs.offset_vh[1]) * window_h + {
        let [x, y] = fs.padding.unwrap_or(cs.padding);
        vec2(x, y)
    });
    let art = Rect::from_center_size(art.center(), art.size() * opening).translate(vec2(cs.open_slide * side * (1.0 - opened), settling * window_h));
    if cs.open_fade {
        ui.multiply_opacity(opened.clamp(0.0, 1.0));
    }
    if settle_fade < 1.0 {
        ui.multiply_opacity(settle_fade);
    }
    let round = cs.shape != Shape::Rounded && !fs.plain_shape;
    let radius = if round { side / 2.0 } else { fs.radius.or(fs.radius_pct.map(|p| p * side)).unwrap_or(cs.radius_pct.map_or(cs.radius, |p| p * side)).clamp(0.0, (side / 2.0).max(0.0)) };
    let top_only = cs.top_only || fs.top_only;
    let cr = if top_only && !round { egui::CornerRadius { nw: radius.min(255.0) as u8, ne: radius.min(255.0) as u8, sw: 0, se: 0 } } else { egui::CornerRadius::same(radius.min(255.0) as u8) };
    if cs.glow > 0.0 {
        let s = egui::epaint::Shadow { offset: [0, 16], blur: 90, spread: 6, color: theme::with_alpha(app.accent_color(), cs.glow.min(1.0) * 0.7) };
        ui.painter().add(s.as_shape(art, cr));
    }
    let shadow_a = fs.shadow.unwrap_or(cs.shadow);
    if shadow_a > 0.0 {
        // Corner radii stop at 255: a bigger disc's rounded-rectangle shadow shows as a square.
        // A theme with an offset has said where it goes; with none, the usual drop.
        if round && art.width() / 2.0 > 255.0 {
            widgets::disc_shadow(ui, art.center(), art.width() / 2.0, cs.shadow_blur, shadow_a.min(1.0));
        } else if cs.shadow_offset != 0.0 {
            widgets::soft_shadow_placed(ui, art.translate(vec2(0.0, cs.shadow_offset)), radius.min(255.0), cs.shadow_blur, shadow_a.min(1.0));
        } else {
            widgets::soft_shadow(ui, art, radius.min(255.0), cs.shadow_blur, shadow_a.min(1.0));
        }
    }
    for [y, blur, a] in [cs.shadow2, cs.shadow3, cs.shadow4, cs.shadow5].into_iter().flatten() {
        widgets::soft_shadow_placed(ui, art.translate(vec2(0.0, y)), radius.min(255.0), blur, a);
    }
    if cs.opacity < 1.0 {
        ui.multiply_opacity(cs.opacity);
    }
    let brightness = fs.brightness.unwrap_or(cs.brightness);
    let tint = Color32::from_gray((brightness.clamp(0.1, 1.0) * 255.0) as u8);
    if round {
        if cs.shape == Shape::Record && (app.playing() || cs.spin_when_paused) {
            // Real time since the last frame: egui's smoothed estimate makes uneven frames step.
            let last_id = egui::Id::unique("record-last-frame");
            let dt = (now - ui.ctx().data(|d| d.get_temp::<f64>(last_id)).unwrap_or(now)).clamp(0.0, 0.1);
            ui.ctx().data_mut(|d| d.insert_temp(last_id, now));
            app.record_angle = (app.record_angle + dt * std::f64::consts::TAU / cs.spin_secs.max(4.0) as f64) % std::f64::consts::TAU;
            // At the usual 30 fps a huge disc's edge visibly steps, and paused nothing would draw it at all.
            if !app.window_hidden {
                super::pace::repaint_after(ui.ctx(), std::time::Duration::from_millis(16));
            }
        }
        let angle = if cs.shape == Shape::Record { app.record_angle as f32 } else { 0.0 };
        widgets::paint_disc(ui, art.center(), side / 2.0, &track.thumbs, angle, tint, px);
        if cs.shape == Shape::Record && cs.record_details {
            // Vinyl: faint grooves and a spindle hole.
            let p = ui.painter();
            for i in 1..6 {
                p.circle_stroke(art.center(), side / 2.0 * (0.42 + i as f32 * 0.1), egui::Stroke::new(1.0, theme::shade(0.18)));
            }
            p.circle_filled(art.center(), side * 0.09, theme::shade(0.35));
            p.circle_filled(art.center(), side * 0.022, Color32::from_rgb(10, 10, 12));
        }
    } else {
        if top_only {
            widgets::paint_cover_corners(ui, art, &track.thumbs, cr, px);
        } else {
            widgets::paint_cover_px(ui, art, &track.thumbs, radius, 1.0, px);
        }
        // The animated cover over the still one, faded in once it's moving. Sized to the
        // cover as drawn: `px` is the biggest it may glide to, which can be twice that.
        let drawn = (side * ui.ctx().pixels_per_point()) as u32;
        if let Some((tex, shown)) = cover_clip(app, ui.ctx(), track, drawn, now) {
            egui::Image::from_texture(&tex).corner_radius(cr).tint(Color32::WHITE.gamma_multiply(shown)).paint_at(ui, art);
        }
        if brightness < 0.99 {
            ui.painter().rect_filled(art, cr, theme::shade(1.0 - brightness));
        }
        if let Some([from, to]) = cs.overlay_gradient {
            super::shell::vgradient(ui.painter(), art.shrink(radius.min(art.width() / 4.0) * 0.3), theme::shade(from), theme::shade(to));
        }
    }
    if cs.opacity < 1.0 {
        ui.multiply_opacity(1.0 / cs.opacity.max(0.01));
    }
    if cs.outline[0] > 0.0 && !fs.no_outline {
        if round {
            ui.painter().circle_stroke(art.center(), art.width() / 2.0 - cs.outline[0] / 2.0, egui::Stroke::new(cs.outline[0], theme::with_alpha(Color32::WHITE, cs.outline[1])));
        } else {
            ui.painter().rect_stroke(art, cr, egui::Stroke::new(cs.outline[0], theme::with_alpha(Color32::WHITE, cs.outline[1])), egui::StrokeKind::Inside);
        }
    }
    // Below the header, whose buttons a tall cover would otherwise sit on top of; a disc only
    // answers on the disc, not in the corners of its square.
    let hit = art.with_min_y(art.top().max(col.top()));
    let cover = named(ui.interact(hit, ui.scope_id().with(("cover-playback", &track.id)), Sense::click()), if app.playing() { "Pause" } else { "Play" });
    let on_art = !round || ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| p.distance(art.center()) <= art.width() / 2.0);
    let cover = if on_art { cover.on_hover_cursor(egui::CursorIcon::PointingHand) } else { cover };
    let hover = theme::anim_bool(ui.ctx(), ("cover-playback-hover", &track.id), cover.hovered() && on_art, 0.16);
    if hover > 0.0 {
        if round {
            ui.painter().circle_filled(art.center(), art.width() / 2.0, theme::shade(cs.hover_dim * hover));
        } else {
            ui.painter().rect_filled(art, cr, theme::shade(cs.hover_dim * hover));
        }
        if !np.cover_play_bare {
            ui.painter().circle_filled(art.center(), np.cover_play_px / 2.0, theme::shade(0.58 * hover));
        }
        let grown = if np.cover_play_hover != 1.0 && ui.rect_contains_pointer(Rect::from_center_size(art.center(), Vec2::splat(np.cover_play_px))) { np.cover_play_hover } else { 1.0 };
        widgets::icon_at(ui, art.center(), if app.playing() || app.buffering { icon::PAUSE } else { icon::PLAY }, np.cover_play_px * 0.49 * grown, theme::with_alpha(Color32::WHITE, hover));
    }
    if cover.clicked() && on_art { app.actions.push(Action::TogglePlay); }
    // Previous and next on the cover too, for themes that put the transport there. The bar still has them.
    let near_cover = ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| art.contains(p));
    if np.cover_progress && near_cover {
        let status = app.audio.status.clone();
        let total = status.duration().map_or(0.0, |d| d.as_secs_f32());
        let pos = status.position_smooth().as_secs_f32().min(total.max(0.0));
        let cap = Rect::from_center_size(pos2(art.center().x, art.bottom() - 34.0), vec2((art.width() - 40.0).max(120.0), 34.0));
        if let Some(g) = look.app.glass.tabs {
            crate::shader::frost(ui.painter(), cap, crate::shader::Frost { look: g, radius: 17.0, opacity: 1.0, fade: 0.0 });
        }
        ui.painter().rect_filled(cap, 17.0, theme::shade(0.4));
        let font = theme::semibold(12.0);
        ui.painter().text(cap.left_center() + vec2(14.0, 0.0), egui::Align2::LEFT_CENTER, super::fmt_time(pos as u32), font.clone(), Color32::WHITE);
        ui.painter().text(cap.right_center() - vec2(14.0, 0.0), egui::Align2::RIGHT_CENTER, super::fmt_time(total as u32), font, Color32::WHITE);
        let line = Rect::from_center_size(cap.center(), vec2((cap.width() - 110.0).max(40.0), 4.0));
        let seek = named(ui.interact(line.expand2(vec2(0.0, 10.0)), ui.scope_id().with("cover-seek"), Sense::click()), "Seek").on_hover_cursor(egui::CursorIcon::PointingHand);
        ui.painter().rect_filled(line, 2.0, theme::with_alpha(Color32::WHITE, 0.25));
        if total > 0.0 {
            ui.painter().rect_filled(Rect::from_min_size(line.min, vec2(line.width() * pos / total, 4.0)), 2.0, Color32::WHITE);
            if seek.clicked()
                && let Some(p) = seek.interact_pointer_pos()
            {
                app.actions.push(Action::Seek(((p.x - line.left()) / line.width()).clamp(0.0, 1.0) * total));
            }
        }
    }
    let fs_buttons = np.fullscreen_cover_buttons.filter(|_| app.fullscreen);
    if (np.cover_controls || fs_buttons.is_some()) && near_cover {
        let [btn_px, btn_a, btn_grow] = fs_buttons.unwrap_or(np.cover_buttons);
        let btn_px = btn_px.max(36.0);
        for (dx, glyph, name, next) in [(-74.0, icon::SKIP_BACK, "Previous", false), (74.0, icon::SKIP_FORWARD, "Next", true)] {
            let r = Rect::from_center_size(art.center() + vec2(dx, 0.0), Vec2::splat(btn_px));
            let resp = named(ui.interact(r, ui.scope_id().with(("cover-ctl", name)), Sense::click()), name).on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand);
            ui.painter().circle_filled(r.center(), btn_px / 2.0 * if resp.hovered() { btn_grow } else { 1.0 }, theme::shade(if resp.hovered() { (btn_a + 0.22).min(1.0) } else { btn_a }));
            widgets::icon_at(ui, r.center(), glyph, 20.0, Color32::WHITE);
            if resp.clicked() {
                if next {
                    app.next();
                } else {
                    app.prev();
                }
            }
        }
    }
    let [title_px, artist_px] = np.fullscreen_text_px.filter(|_| app.fullscreen).unwrap_or([np.title_px, np.artist_px]);
    let title_card = np.fullscreen_title_card.filter(|_| app.fullscreen).or(np.title_card);
    let pad = title_card.map_or(0.0, |c| c.pad);
    let w = side.min(col.width()).min(np.title_max_px.unwrap_or(f32::INFINITY)) - pad * 2.0;
    let joined = title_card.is_some_and(|c| c.attached);
    let title_y = art.bottom().min(col.bottom() - title_px - artist_px - 24.0) + if joined { pad.max(12.0) } else { 22.0 };
    let line = track.artist_line();
    if np.info_card && (app.fullscreen || !np.info_card_fullscreen_only) {
        info_card(app, ui, col, track);
    } else if np.shows_title(app.fullscreen) {
        let (title_font, artist_font) = if np.title_lyric_font {
            (theme::lyric_font(&look.lyrics.font, np.title_weight, title_px), theme::lyric_font(&look.lyrics.font, np.artist_weight, artist_px))
        } else {
            (theme::weighted(np.fullscreen_title_weight.filter(|_| app.fullscreen).unwrap_or(np.title_weight), title_px), theme::weighted(np.artist_weight, artist_px))
        };
        // A card around the block, joined to the cover.
        if let Some(card) = title_card {
            let box_h = title_px * 1.4 + artist_px * 1.5 + (pad * 2.0).max(22.0);
            let r = Rect::from_min_size(pos2(art.left(), if card.attached { art.bottom() } else { title_y - 11.0 }), vec2(art.width(), box_h));
            let corners = if card.attached { egui::CornerRadius { nw: 0, ne: 0, sw: card.radius as u8, se: card.radius as u8 } } else { egui::CornerRadius::same(card.radius as u8) };
            if let Some([y, blur, a]) = card.shadow {
                widgets::soft_shadow(ui, r.translate(vec2(0.0, y)), card.radius, blur, a);
            }
            if let Some(g) = look.app.glass.title_card {
                crate::shader::frost(ui.painter(), r, crate::shader::Frost { look: g, radius: if card.attached { 0.0 } else { card.radius }, opacity: 1.0, fade: 0.0 });
            }
            ui.painter().rect_filled(r, corners, theme::with_alpha(Color32::WHITE, card.fill));
            if card.stroke > 0.0 {
                if card.dashed {
                    let c = egui::Stroke::new(1.0, theme::with_alpha(Color32::WHITE, card.stroke));
                    let p = |x: f32, y: f32| pos2(x, y);
                    let path = [p(r.left(), r.top()), p(r.right(), r.top()), p(r.right(), r.bottom()), p(r.left(), r.bottom()), p(r.left(), r.top())];
                    ui.painter().extend(egui::Shape::dashed_line(&path, c, 6.0, 4.0));
                } else {
                    ui.painter().rect_stroke(r, corners, egui::Stroke::new(1.0, theme::with_alpha(Color32::WHITE, card.stroke)), egui::StrokeKind::Inside);
                }
            }
        }
        // Laid out in the block's alignment.
        let at = |text: &str, font: egui::FontId, ui: &Ui| {
            let g = widgets::galley(ui, text, font, Color32::WHITE, w);
            match np.title_align {
                crate::preset::Align::Left => art.left() + pad,
                crate::preset::Align::Center => col.center().x - g.size().x / 2.0,
                crate::preset::Align::Right => art.right() - pad - g.size().x,
            }
        };
        let title_x = at(&track.title, title_font.clone(), ui);
        let artist_x = at(&line, artist_font.clone(), ui);
        if let Some([down, a]) = np.title_shadow {
            text_at(ui, pos2(title_x, title_y + down), &track.title, title_font.clone(), theme::shade(a), w);
        }
        if np.title_edge_fade > 0.0 {
            widgets::faded_text_at(ui, pos2(title_x, title_y), &track.title, title_font, theme::with_alpha(Color32::WHITE, np.title_alpha), w, np.title_edge_fade);
        } else {
            widgets::title_at(ui, pos2(title_x, title_y), &track.title, track.explicit, title_font, theme::with_alpha(Color32::WHITE, np.title_alpha), w);
        }
        // Each artist opens their own page (features included).
        widgets::linked_text(ui, pos2(artist_x, title_y + title_px * 1.4), &line, &track.artists, artist_font, theme::with_alpha(theme::text_dim(), np.artist_alpha), w, &mut app.actions);
    }
    if no_lyrics && app.np_tab == NpTab::Lyrics {
        // A brief note, faded out after a few seconds.
        let since = ui.ctx().memory_mut(|m| *m.data.get_temp_mut_or_insert_with(egui::Id::unique(("no-lyrics", &track.id)), || now));
        let nl = np.no_lyrics;
        let a = if nl.stays { 1.0 } else { (1.0 - ((now - since - 3.0) / 1.0) as f32).clamp(0.0, 1.0) };
        if a > 0.0 {
            let c = theme::with_alpha(theme::text_dim(), a * 0.8);
            let y = title_y + title_px * 1.4 + artist_px * 1.6;
            let message = if np.no_lyrics_text.is_empty() { "No lyrics for this song" } else { &np.no_lyrics_text };
            let y = if nl.icon {
                widgets::icon_at(ui, pos2(art.center().x, y + nl.rem * 14.0), icon::MUSIC, nl.rem * 24.0, c);
                y + nl.rem * 28.0
            } else {
                y
            };
            let size = (nl.rem * 10.0).max(12.0);
            let g = widgets::galley_wrapped(ui, message, theme::regular(size), c, col.width(), 3);
            let x = if nl.rem > 2.0 { col.center().x - g.size().x / 2.0 } else { art.left() };
            ui.painter().galley(pos2(x, y), g, c);
            // Frames only for the fade-out.
            let wait = (since + 3.0 - now).max(0.0);
            if !nl.stays {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(if wait > 0.0 { wait } else { 0.033 }));
            }
        }
    }
}

/// Song or Video, in the middle of the header, as YouTube Music's own apps have it.
fn song_video_switch(app: &mut App, ui: &mut Ui, header: Rect) {
    let look = app.look().now_playing.switch;
    // In the bottom corner, in some themes, rather than in the middle of the header.
    let corner_x = if look.at_right { ui.max_rect().right() - 170.0 } else { ui.max_rect().left() };
    let header = if look.at_corner { Rect::from_min_size(pos2(corner_x, ui.max_rect().bottom() - header.height()), vec2(170.0, header.height())) } else { header };
    let header = header.translate(vec2(0.0, look.y_offset_full.filter(|_| app.fullscreen).unwrap_or(look.y_offset)));
    let rect = Rect::from_center_size(header.center(), vec2(170.0, header.height()));
    let mut row = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
    row.spacing_mut().item_spacing.x = 6.0;
    if look.back.is_some() || look.stroke > 0.0 {
        let back = Rect::from_center_size(header.center(), vec2(150.0, 42.0));
        let radius = look.radius.unwrap_or(21.0);
        if let Some(g) = app.look().app.glass.switch {
            crate::shader::frost(ui.painter(), back, crate::shader::Frost { look: g, radius, opacity: 1.0, fade: 0.0 });
        }
        if let Some([y, blur, a]) = look.shadow {
            widgets::soft_shadow(ui, back.translate(vec2(0.0, y)), radius, blur, a);
        }
        let base = if look.back_black { Color32::BLACK } else { Color32::WHITE };
        ui.painter().rect_filled(back, radius, theme::with_alpha(base, look.back.unwrap_or(0.0)));
        if look.stroke > 0.0 {
            ui.painter().rect_stroke(back, radius, egui::Stroke::new(1.0, theme::with_alpha(Color32::WHITE, look.stroke)), egui::StrokeKind::Inside);
        }
    }
    let picked = app.video_picked();
    if let Some(a) = look.selected {
        let cell = Rect::from_center_size(header.center() + vec2(if picked { 34.0 } else { -36.0 }, 0.0), vec2(68.0, 34.0));
        ui.painter().rect_filled(cell, look.radius.unwrap_or(17.0), theme::with_alpha(Color32::WHITE, a));
    }
    let (song, video) = if look.plain {
        // Plain words in the same places as the selected fill above.
        let word = |ui: &mut Ui, label: &str, x: f32, on: bool| {
            let r = Rect::from_center_size(header.center() + vec2(x, 0.0), vec2(68.0, 34.0));
            let resp = named(ui.interact(r, ui.scope_id().with(("av", label)), Sense::click()), label).on_hover_cursor(egui::CursorIcon::PointingHand);
            let c = if on || resp.hovered() { Color32::WHITE } else { theme::text_dim() };
            if !super::vector::paint(ui, &label.to_lowercase(), r.center(), 18.0, c) {
                ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, label, theme::semibold(13.5), c);
            }
            widgets::focus_ring(ui, &resp, r, look.radius.unwrap_or(17.0));
            resp.clicked()
        };
        (word(ui, "Song", -36.0, !picked), word(ui, "Video", 34.0, picked))
    } else {
        (widgets::chip(&mut row, "Song", !picked).clicked(), widgets::chip(&mut row, "Video", picked).clicked())
    };
    if (song && picked) || (video && !picked) {
        let on_song = app.current().is_some_and(|t| !t.has_video());
        app.actions.push(if on_song { Action::SongVideo(!picked) } else { Action::ToggleVideo });
    }
}

/// The video as the whole view, with Up next or Comments beside it when that's open.
fn video_stage(app: &mut App, ui: &mut Ui, content: Rect, track: &crate::model::Track) {
    let frame = video_frame(app, ui.ctx(), track);
    // Up next or Comments: beside the video, or under it when there's no room.
    let comments = app.np_tab == NpTab::Comments && app.show_comments;
    let side = app.np_tab == NpTab::UpNext || comments;
    let queue_w = if side && content.width() > 900.0 { 360.0 } else { 0.0 };
    let queue_h = if side && queue_w == 0.0 { content.height() * 0.45 } else { 0.0 };
    let stage = Rect::from_min_max(content.min, pos2(content.right() - queue_w, content.bottom() - queue_h));
    let (w, h) = frame.as_ref().map_or((16.0, 9.0), |t| (t.size()[0] as f32, t.size()[1] as f32));
    let room_w = match app.look().now_playing.video_max_frac.filter(|_| app.fullscreen) {
        Some(f) => stage.width() * f,
        None => stage.width(),
    };
    let scale = (room_w / w.max(1.0)).min((stage.height() - 64.0).max(120.0) / h.max(1.0));
    let fit = vec2(w * scale, h * scale);
    // In the middle of what's left over, with the title under it (the 64 kept for it): a
    // window taller than the video is wide leaves room above and below, not all below.
    let top = stage.top() + ((stage.height() - 64.0 - fit.y) / 2.0).max(0.0);
    let picture = Rect::from_center_size(pos2(stage.center().x, top + fit.y / 2.0), fit);
    let look = app.look();
    let radius = egui::CornerRadius::same(look.now_playing.video_radius.unwrap_or(look.cover.radius_pct.map_or(look.cover.radius, |p| p * fit.y)).clamp(0.0, 255.0) as u8);
    match &frame {
        Some(tex) => {
            egui::Image::from_texture((tex.id(), fit)).corner_radius(radius).paint_at(ui, picture);
        }
        None => video_placeholder(ui, picture, track, radius.nw as f32),
    }
    let cover = named(ui.interact(picture, ui.scope_id().with(("video-playback", &track.id)), Sense::click()), if app.playing() { "Pause" } else { "Play" }).on_hover_cursor(egui::CursorIcon::PointingHand);
    let hover = theme::anim_bool(ui.ctx(), ("video-playback-hover", &track.id), cover.hovered(), 0.16);
    if hover > 0.0 {
        ui.painter().circle_filled(picture.center(), 38.0, theme::shade(0.58 * hover));
        widgets::icon_at(ui, picture.center(), if app.playing() || app.buffering { icon::PAUSE } else { icon::PLAY }, 37.0, theme::with_alpha(Color32::WHITE, hover));
    }
    if cover.clicked() {
        app.actions.push(Action::TogglePlay);
    }
    // Picture in picture, at the video's top right while it's pointed at.
    if ui.rect_contains_pointer(picture) || hover > 0.0 {
        let c = pos2(picture.right() - 30.0, picture.top() + 30.0);
        let b = Rect::from_center_size(c, Vec2::splat(40.0));
        let resp = named(ui.interact(b, ui.scope_id().with("pip"), Sense::click()), "Picture in picture (Ctrl+M)")
            .on_hover_text("Picture in picture (Ctrl+M)")
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let shown = if resp.hovered() { 1.0 } else { hover.max(if ui.rect_contains_pointer(picture) { 1.0 } else { 0.0 }) };
        ui.painter().circle_filled(c, 20.0, theme::shade(if resp.hovered() { 0.75 } else { 0.5 } * shown));
        widgets::icon_at(ui, c, icon::PICTURE_IN_PICTURE, 18.0, theme::with_alpha(Color32::WHITE, shown));
        if resp.clicked() {
            app.actions.push(Action::Mini(true));
        }
    }
    let y = picture.bottom() + 12.0;
    let np = &look.now_playing;
    let (title_px, artist_px) = (np.title_px.min(22.0), np.artist_px.min(15.0));
    let line = track.artist_line();
    if np.shows_title(app.fullscreen) {
        let x_of = |text: &str, font: egui::FontId, ui: &Ui| match np.title_align {
            crate::preset::Align::Left => picture.left(),
            crate::preset::Align::Center => picture.center().x - widgets::galley(ui, text, font, Color32::WHITE, fit.x).size().x / 2.0,
            crate::preset::Align::Right => picture.right() - widgets::galley(ui, text, font, Color32::WHITE, fit.x).size().x,
        };
        let tx = x_of(&track.title, theme::bold(title_px), ui);
        let ax = x_of(&line, theme::regular(artist_px), ui);
        widgets::title_at(ui, pos2(tx, y), &track.title, track.explicit, theme::bold(title_px), Color32::WHITE, fit.x);
        widgets::linked_text(ui, pos2(ax, y + title_px * 1.4), &line, &track.artists, theme::regular(artist_px), theme::text_dim(), fit.x, &mut app.actions);
    }
    if look.now_playing.video_pill {
        video_line_pill(app, ui, picture);
    }
    if side {
        let r = if queue_w > 0.0 { Rect::from_min_max(pos2(stage.right() + 24.0, content.top()), content.max) } else { Rect::from_min_max(pos2(content.left(), stage.bottom() + 8.0), content.max) };
        let mut q = ui.new_child(egui::UiBuilder::new().max_rect(r));
        q.set_clip_rect(r.intersect(ui.clip_rect()));
        if comments {
            super::comments::panel(app, &mut q);
        } else {
            super::shell::queue_list(app, &mut q, "np");
        }
    }
}

/// While a video is on its way, or can't be shown: its own thumbnail in its place, the
/// whole 16:9 of it, sharp once YouTube's biggest one has loaded.
pub(super) fn video_placeholder(ui: &Ui, picture: Rect, track: &crate::model::Track, radius: f32) {
    ui.painter().rect_filled(picture, radius, Color32::BLACK);
    if let Some(t) = track.thumbs.iter().max_by_key(|t| t.width) {
        let ratio = t.width as f32 / t.height.max(1) as f32;
        // 16:9 inside a 4:3 frame, as the smaller ones are: the middle three quarters.
        let uv = if (1.2..1.6).contains(&ratio) { Rect::from_min_max(pos2(0.0, 0.125), pos2(1.0, 0.875)) } else { Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)) };
        widgets::paint_remote(ui, &t.url, picture, radius, uv, Color32::WHITE);
        if let Some(big) = crate::model::ytimg_max(&t.url) {
            widgets::paint_when_ready(ui, &big, picture, radius, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)));
        }
    }
}

/// The line being sung over the bottom of the video, in a small frosted pill.
fn video_line_pill(app: &App, ui: &Ui, picture: Rect) {
    let Some((id, set)) = app.lyrics.as_ref() else { return };
    let Some(lyrics) = app.lyric_prefs.choose(id, set).and_then(|p| set.get(p)) else { return };
    // As the lyrics panel has it: what's audible, on the listener's timing.
    let pos = app.audio.status.position_smooth().as_secs_f64() - app.audio.status.latency().as_secs_f64();
    let t = ((pos - app.lyric_prefs.offset(id, lyrics.provider, lyrics.sync == Sync::Word)) * 1000.0) as i32;
    let Some(line) = lyrics.lines.iter().find(|l| !l.instrumental && l.start <= t && t < l.start + l.dur) else { return };
    let g = widgets::galley(ui, &line.text, theme::semibold(15.0), Color32::WHITE, picture.width() - 64.0);
    let r = Rect::from_center_size(pos2(picture.center().x, picture.bottom() - 34.0), g.size() + vec2(24.0, 12.0));
    if let Some(look) = app.look().app.glass.resume {
        crate::shader::frost(ui.painter(), r, crate::shader::Frost { look, radius: r.height() / 2.0, opacity: 1.0, fade: 0.0 });
    }
    ui.painter().rect_filled(r, r.height() / 2.0, theme::shade(0.45));
    ui.painter().galley(r.center() - g.size() / 2.0, g, Color32::WHITE);
}

/// The video's colours, eased toward the latest frame's, across all of Now playing.
fn paint_glow(app: &mut App, ui: &mut Ui, rect: Rect, clip: Rect, step: bool) {
    let Some(Glow { current, target, tex }) = &mut app.glow else { return };
    if step {
        let k = (ui.input(|i| i.stable_dt) * 2.5).clamp(0.0, 1.0);
        for (c, t) in current.iter_mut().zip(target.iter()) {
            for i in 0..3 {
                c[i] += (t[i] - c[i]) * k;
            }
        }
        let [w, h] = crate::video::GLOW_SIZE;
        let pixels = current.iter().map(|c| Color32::from_rgb(c[0] as u8, c[1] as u8, c[2] as u8)).collect();
        tex.set(egui::ColorImage { size: [w, h], source_size: vec2(w as f32, h as f32), pixels }, egui::TextureOptions::LINEAR);
    }
    // Bigger than the view, so its coarse edges stay out of sight.
    let full = rect.expand2(rect.size() * 0.08);
    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    let painter = ui.painter_at(clip);
    painter.image(tex.id(), full, uv, Color32::from_gray(150));
    painter.rect_filled(rect, 0.0, theme::shade(0.25));
}

/// A song's animated cover: asked for, then playing once found.
pub(super) struct CoverClip {
    pub id: String,
    /// The cover's size the clip plays at, which picks its rendition.
    pub px: u32,
    /// The clip's playlist, once found.
    pub url: Option<String>,
    pub player: Option<crate::video::Loop>,
    frame: Option<(egui::TextureHandle, f64)>,
    /// Another size the cover has kept since (s), to play at once it has settled.
    resized: Option<(u32, f64)>,
}

/// The animated cover's latest frame, and how far it has faded in. Keeps the clip in step
/// with the song.
fn cover_clip(app: &mut App, ctx: &egui::Context, track: &crate::model::Track, px: u32, now: f64) -> Option<(egui::TextureHandle, f32)> {
    if !app.animated_covers {
        app.cover_clip = None;
        return None;
    }
    if app.cover_clip.as_ref().is_none_or(|c| c.id != track.id) {
        app.cover_clip = Some(CoverClip { id: track.id.clone(), px: px.max(1), url: None, player: None, frame: None, resized: None });
        app.backend.request(crate::backend::Req::Canvas(track.clone()));
    }
    let playing = app.playing();
    let http = app.backend.http();
    let c = app.cover_clip.as_mut()?;
    // The window resized or went full screen: the clip again at the new size once it has
    // stayed there a moment (a resize passes through many), the last frame up meanwhile.
    let off = px.max(1) as f32 / c.px as f32;
    if c.url.is_some() && !(0.8..=1.25).contains(&off) {
        match c.resized {
            Some((seen, since)) if seen.abs_diff(px) * 10 <= seen => {
                if now - since > 0.5 && let Some(url) = c.url.clone() {
                    c.px = px.max(1);
                    c.player = Some(crate::video::Loop::start(url, http, c.px));
                    c.resized = None;
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
            _ => c.resized = Some((px, now)),
        }
    } else {
        c.resized = None;
    }
    let player = c.player.as_mut().filter(|p| !p.failed())?;
    if let Some(f) = player.take_due(now, playing) {
        match &mut c.frame {
            Some((tex, _)) => tex.set(f.image, egui::TextureOptions::LINEAR),
            None => c.frame = Some((ctx.load_texture("cover-clip", f.image, egui::TextureOptions::LINEAR), now)),
        }
    }
    let shown = c.frame.as_ref().map_or(0.0, |(_, at)| ((now - at) / 0.32).clamp(0.0, 1.0)) as f32;
    // Now playing runs at 30 fps while the music plays; paused, only the first frame and
    // its fade need drawing.
    if c.frame.is_none() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    } else if shown < 1.0 {
        super::pace::repaint_after(ctx, std::time::Duration::from_millis(16));
    }
    c.frame.as_ref().map(|(tex, _)| (tex.clone(), shown))
}

/// The video's latest frame in a texture. Keeps the player, and the glow's colours, in
/// step with what's playing.
pub(super) fn video_frame(app: &mut App, ctx: &egui::Context, track: &crate::model::Track) -> Option<egui::TextureHandle> {
    // Nothing to show it on: `logic` has stopped the decoder and it stays stopped.
    if app.window_hidden || (app.mini.busy() && !super::mini::video_active()) {
        return None;
    }
    if app.video.as_ref().is_none_or(|(id, _)| *id != track.id) {
        app.video = Some((track.id.clone(), None));
        app.video_frame = None;
        app.glow = None;
        app.backend.request(crate::backend::Req::VideoStream { video_id: track.id.clone(), max_height: app.video_quality });
    }
    let player = app.video.as_ref()?.1.as_ref()?;
    if player.failed() {
        return None;
    }
    if let Some(frame) = player.take_due() {
        // The background following the picture, a few times a second.
        if app.look().background.video_backdrop && ctx.input(|i| i.time) - app.video_art_at > 0.25 {
            app.video_art_at = ctx.input(|i| i.time);
            if let (Some(bg), Some(mut art)) = (&app.bg, crate::art::from_frame(&frame.image)) {
                let v = crate::preset::active().background.video;
                // `from_frame` is about as soft as the art's usual six.
                crate::art::soften(&mut art, v.blur_passes.map_or(0, |n| n.saturating_sub(6)));
                if v.auto_dim.is_some() {
                    app.art_brightness = art.brightness;
                }
                bg.set_live(art);
            }
        }
        if app.ambient_light {
            let colors = crate::video::glow_colors(&frame.image);
            match &mut app.glow {
                Some(glow) => glow.target = colors,
                None => {
                    let [w, h] = crate::video::GLOW_SIZE;
                    let image = egui::ColorImage { size: [w, h], source_size: vec2(w as f32, h as f32), pixels: vec![Color32::BLACK; w * h] };
                    let tex = ctx.load_texture("video-glow", image, egui::TextureOptions::LINEAR);
                    app.glow = Some(Glow { current: colors.clone(), target: colors, tex });
                }
            }
        }
        match &mut app.video_frame {
            Some(tex) => tex.set(frame.image, egui::TextureOptions::LINEAR),
            None => app.video_frame = Some(ctx.load_texture("music-video", frame.image, egui::TextureOptions::LINEAR)),
        }
    }
    if app.playing() {
        super::pace::repaint_after(ctx, std::time::Duration::from_millis(16));
    } else if app.video_frame.is_none() {
        // Paused or still loading: the first frame shows when it's here, or after a seek.
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    app.video_frame.clone()
}

// ------------------------------------------------------------------ lyrics

fn lyrics_panel(app: &mut App, ui: &mut Ui, rect: Rect, now: f64, centered: bool) {
    // Lent out for the frame rather than cloned (the drawing needs `app` mutably).
    let Some((id, set)) = app.lyrics.take() else {
        ui.add_space(rect.height() * 0.35);
        ui.vertical_centered(|ui| {
            widgets::loader(ui);
            ui.label(egui::RichText::new("Loading lyrics…").color(theme::text_dim()));
        });
        return;
    };
    let hide_break_only = app.look().lyrics.hide_instrumental_only;
    if let Some(lyrics) = app.lyric_prefs.choose(&id, &set).and_then(|p| set.get(p)).filter(|l| !hide_break_only || !l.lines.iter().all(|x| x.instrumental)) {
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
    let centered = narrow;
    let playing = app.playing();
    // Audible position: the device plays what the player hands it a buffer later.
    let pos = app.audio.status.position_smooth().as_secs_f64() - app.audio.status.latency().as_secs_f64();
    let pos = app.lyric_clock.tick(pos, playing, now, app.audio.status.speed() as f64);
    let t = pos - app.lyric_prefs.offset(id, lyrics.provider, lyrics.sync == Sync::Word);
    let (roman, trans) = decorations(app.show_roman, app.show_trans, &app.lyric_prefs.target, app.lyrics_extras.as_ref(), id, lyrics);
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        id.hash(&mut h);
        lyrics.provider.key().hash(&mut h);
        app.theme_id.hash(&mut h);
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
    let fullscreen = app.fullscreen;
    let glass_chip = app.look().app.glass.instrumental;
    let frame = karaoke::Frame { lyrics, key, roman, trans, t, playing, now, rect, style, accent, centered, scale, fullscreen, glass_chip };
    // The view lives in `app`; the frame borrows `app` for decorations, so swap it out.
    let mut view = std::mem::take(&mut app.lyric_view);
    let out = if karaoke::dev_off("view") { karaoke::Out { seek: None, animating: true, user_scrolling: false } } else { view.show(ui, frame) };
    app.lyric_view = view;
    if let Some(s) = out.seek {
        // The line's time on the player's clock: `t` above, the other way round.
        let at = s + app.lyric_prefs.offset(id, lyrics.provider, lyrics.sync == Sync::Word) + app.audio.status.latency().as_secs_f64();
        app.actions.push(Action::Seek(at.max(0.0) as f32));
    }
    if out.animating && !app.window_hidden {
        // Smooth, without following a fast monitor: frame rate is the main CPU and GPU cost.
        super::pace::repaint_after(ui.ctx(), std::time::Duration::from_millis(16));
    }
    // The extension's `.autoscroll-resume-button` (YouTube Music's rem is 10 px): a frosted
    // pill under the top edge in the lyrics font at 600, 1.75rem; padding .5rem 1rem;
    // white 5% fill, 1px white 10% border, radius 1rem, over a 5px backdrop blur (which
    // reads as about twice those on screen). It slides down from -3em to 2em as it fades
    // in, and its text glows on hover.
    let shown = theme::anim_bool(ui.ctx(), "lyr-resume", out.user_scrolling, 0.2);
    if shown > 0.0 {
        let font = theme::lyric_font(&style.font, crate::preset::Weight::Semibold, style.resume_text_px.unwrap_or(17.5));
        let g = widgets::galley(ui, "Resume Autoscroll", font, Color32::WHITE, rect.width() - 40.0);
        let size = g.size() + vec2(20.0, 10.0) + Vec2::splat(2.0);
        let em = 17.5;
        let y = rect.top() + 8.0 + egui::lerp(-3.0 * em..=0.5 * em, theme::ease_out(shown));
        let size = vec2(size.x, style.resume_height.unwrap_or(size.y));
        let r = match style.resume_fullscreen.filter(|_| fullscreen) {
            Some([left, down]) => {
                let y = egui::lerp(rect.top() - 3.0 * em..=rect.top() + down * rect.height(), theme::ease_out(shown));
                Rect::from_min_size(pos2((rect.left() + left).min(rect.right() - size.x), y), size)
            }
            None => Rect::from_min_size(pos2(rect.center().x - size.x / 2.0, y), size).translate(Vec2::from(style.resume_offset)),
        };
        let resp = ui.interact(r, ui.scope_id().with("lyr-resume-btn"), if out.user_scrolling { Sense::click() } else { Sense::hover() });
        let resp = named(resp, "Resume Autoscroll").on_hover_cursor(egui::CursorIcon::PointingHand);
        let hover = theme::anim_bool(ui.ctx(), "lyr-resume-h", resp.hovered() && out.user_scrolling, 0.25);
        let p = ui.painter().with_clip_rect(rect.expand2(vec2(0.0, 4.0)));
        if let Some([sy, blur, a]) = style.resume_shadow {
            p.add(egui::epaint::Shadow { offset: [0, sy as i8], blur: blur as u8, spread: 0, color: theme::shade(a * shown) }.as_shape(r, egui::CornerRadius::same(style.resume_look[0].min(255.0) as u8)));
        }
        if let Some(g) = app.look().app.glass.resume {
            crate::shader::frost(&p, r, crate::shader::Frost { look: g, radius: style.resume_look[0], opacity: shown, fade: 0.0 });
        }
        let [res_r, res_fill, res_line] = style.resume_look;
        p.rect_filled(r, res_r, theme::with_alpha(if style.resume_black { Color32::BLACK } else { Color32::WHITE }, res_fill * shown));
        p.rect_stroke(r, res_r, egui::Stroke::new(1.0, theme::with_alpha(Color32::WHITE, res_line * shown)), egui::StrokeKind::Inside);
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
        let size = style.plain_size().min(rect.width() / 14.0).max(18.0);
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
    widgets::menu_popup(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        widgets::menu_body(ui, 270.0);
        if lyrics {
            lyric_options(app, ui);
            menu_divider(ui);
        }
        let liked = app.heard_liked();
        let asked = app.actions.len();
        widgets::song_actions(ui, track, liked, None, &mut app.actions);
        for a in &mut app.actions[asked..] {
            if let Action::Like(_, like) = a {
                *a = Action::LikeHeard(*like);
            }
        }
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

/// A lyrics source: its sync type's dot, name and sync type, with a check on the one shown.
fn source_row(ui: &mut Ui, name: &str, sync: SyncKind, current: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
    let resp = named(resp, &format!("Lyrics source {name}")).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
    if h > 0.0 || current {
        ui.painter().rect_filled(rect, theme::menu_item_radius(), theme::glass(0.05 * current as u8 as f32 + 0.08 * h));
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
        ui.painter().rect_filled(rect, theme::menu_item_radius(), theme::glass(0.08 * h));
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
