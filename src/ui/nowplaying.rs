//! Now-playing overlay: big art + synced lyrics / up next, over the full-strength
//! animated background. The lyrics themselves are drawn by `karaoke` (a port of the
//! better-lyrics renderer); this adds their toolbar and the plain-text fallback.

use std::time::{Duration, Instant};

use egui::{Color32, Rect, Sense, Ui, Vec2, pos2, vec2};

use super::widgets::{self, icon_button, menu_divider, named, text_at};
use super::{Action, App, NpTab, Sleep, karaoke};
use crate::lyrics::{Lyrics, Sync, SyncKind};
use crate::shader::Params;
use crate::theme::{self, icon};

/// A video's glow: its colours now, where they're easing to, and its picture.
pub(super) struct Glow {
    current: Vec<[f32; 3]>,
    target: Vec<[f32; 3]>,
    tex: egui::TextureHandle,
}

/// Now playing's layer. The player bar keeps its own layer just above it while it's up.
pub fn layer() -> egui::LayerId {
    egui::LayerId::new(egui::Order::Middle, egui::Id::unique("now-playing"))
}

pub fn overlay(app: &mut App, ctx: &egui::Context, now: f64) {
    // A spring's overshoot isn't for this: past its place it would leave a gap at the bottom.
    let t = theme::anim_bool_timed(ctx, "np-open", app.np_open && app.current().is_some(), app.look().app.np_open_secs).min(1.0);
    if t <= 0.001 {
        // Closed: nothing to decode for. It picks up from the music's place when opened.
        app.drop_cover_clip(now);
        if app.video.take().is_some() {
            app.video_frame = None;
            app.glow = None;
            app.restore_art();
        }
        return;
    }
    let screen = ctx.content_rect();
    // One sheet the size of the window, coming up from its bottom edge. The player bar
    // stays over it (on its own layer, above this one), so it slides up behind the bar,
    // and around a floating bar it shows the whole way.
    let sheet = screen.translate(vec2(0.0, screen.height() * (1.0 - t)));
    // Its content, where it sits once the sheet is up: above the bar, moving with the sheet.
    let content_h = if app.fullscreen { screen.height() } else { screen.height() - app.bar_height() };
    let rect = Rect::from_min_size(sheet.min, vec2(sheet.width(), content_h));
    let look = app.look();
    let video_only = app.fullscreen && app.video_full;
    let video_only = video_only.then(|| app.shown_video().cloned()).flatten();
    egui::Area::new(layer().id)
        .order(layer().order)
        .fixed_pos(sheet.min)
        // While it's moving, the pointer stays with what's under it, so a click doesn't
        // flash the hand of whatever slides past.
        .interactable(t >= 1.0)
        .show(ctx, |ui| {
            let shown = sheet.intersect(screen);
            ui.set_clip_rect(shown.intersect(ui.clip_rect()));
            // A soft shadow over the page it's covering, so its top edge isn't a hard cut.
            // Once it's all the way up that's above the window.
            if !app.fullscreen && t < 1.0 {
                let band = Rect::from_min_max(pos2(sheet.left(), sheet.top() - 28.0), pos2(sheet.right(), sheet.top()));
                super::shell::vgradient(&ui.ctx().layer_painter(ui.layer_id()).with_clip_rect(screen), band, Color32::TRANSPARENT, theme::shade(0.3));
            }
            // Swallow input so the page underneath doesn't react.
            let sheet_id = ui.allocate_rect(shown, Sense::click_and_drag()).id;
            if let Some(video) = &video_only {
                video_fullscreen(app, ui, rect, video, now);
                return;
            }
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
            let [pad_x, pad_y] = look.now_playing.fullscreen_padding.filter(|_| app.fullscreen).unwrap_or([32.0, 20.0]);
            let area = rect.shrink2(vec2(pad_x, pad_y));
            let behind = behind_panel(app, ui.ctx(), area, now);
            backdrop(app, ui, sheet, shown, now, behind);
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
            body(app, &mut child, now);
            // A finger swiping down it closes it, as on the bar; not one dragging the
            // lyrics, the seek line or the window. After `body`, so those drags are known.
            if t >= 1.0 {
                super::bar_swipe::swipe(app, ctx, rect, "now-playing", Some(sheet_id));
            }
        });
}

/// A full-bleed cover behind the lyrics-only layout (reCover): it's there for the panel.
#[derive(Clone, Copy)]
struct BehindPanel {
    /// On a wide window it fills the height on the left, the panel beside it; else the whole window.
    beside: bool,
    /// How much of it shows: it leaves with the panel, for the cover on its own in the middle.
    vis: f32,
}

/// See [`BehindPanel`]; `None` for the other layouts, whose full-bleed cover always fills the window.
fn behind_panel(app: &App, ctx: &egui::Context, area: Rect, now: f64) -> Option<BehindPanel> {
    if app.look().cover.layout_in(app.fullscreen) != crate::preset::Layout::LyricsOnly {
        return None;
    }
    let track = app.current()?;
    // A video takes the middle instead of the cover.
    let vis = if app.shown_video().is_some() { 1.0 } else { 1.0 - panel_motion(app, ctx, &track.id, app.no_lyrics(), now).art_vis };
    let (_, content) = np_rects(area, app.fullscreen);
    Some(BehindPanel { beside: content_wide(ctx, content), vis })
}

/// Now playing's background laid out over `rect`, drawn only inside `clip`.
fn backdrop(app: &mut App, ui: &mut Ui, rect: Rect, clip: Rect, now: f64, behind: Option<BehindPanel>) {
    use crate::preset::BgMode;
    let look = app.look();
    let b = &look.background;
    let ctx = ui.ctx().clone();
    let painter = ui.painter_at(clip);
    let art = matches!(b.mode, BgMode::Kawarp | BgMode::CoverStatic);
    // One colour: no cover, no gradient, no scrim. The visualizer still draws below.
    if app.flat_bg {
        painter.rect_filled(rect, 0.0, super::shell::base_color(app));
    }
    let themed = !app.flat_bg;
    // A video has the background.
    let on_video = (b.video_backdrop && app.video.as_ref().is_some_and(|(_, player)| player.is_some())).then_some(());
    let over = ((app.art_brightness - b.auto_dim_from) / (1.0 - b.auto_dim_from).max(0.01)).max(0.0);
    // Settings › Appearance › Background brightness goes on top of the theme's dimming.
    let auto_dim = (1.0 - on_video.and(b.video.auto_dim).unwrap_or(b.auto_dim) * over) * app.bg_brightness;
    let theme_fullscreen = b.fullscreen_fullbleed.filter(|_| app.fullscreen);
    let fullbleed_level = (theme_fullscreen.unwrap_or(b.np_dim) * app.bg_brightness).clamp(0.0, 1.0);
    // Then darker still where the art would show too bright for the white text over it,
    // unless the theme sets how bright its full-screen cover is (reCover: at full).
    let readable = if app.dim_bright && theme_fullscreen.is_none() { crate::art::readable_dim(app.art_brightness * shown_level(b, art, fullbleed_level, app.fullscreen, auto_dim)) } else { 1.0 };
    if themed && let (Some(layer), Some(bg)) = (b.layer, app.bg.as_ref()) {
        // The theme's still cover under everything, pulsing if it does.
        // On the background's clock, so Settings › Appearance › Background speed moves it too.
        let t = app.layer_time;
        let pulse = if layer.pulse_secs > 0.0 { (t as f32 * std::f32::consts::TAU / layer.pulse_secs).sin() * layer.pulse } else { 0.0 };
        let rotate = if layer.rotate_secs > 0.0 { (t / layer.rotate_secs as f64 % 1.0) as f32 * std::f32::consts::TAU } else { 0.0 };
        // Turning, it needs room for its corners.
        let room = if layer.rotate_secs > 0.0 { 1.45 } else { 1.0 };
        bg.paint(&painter, rect, Params { time: 0.0, blend: 1.0, intensity: 0.0, saturation: layer.saturate.unwrap_or(b.saturation), dim: (layer.opacity * layer.brightness.unwrap_or(1.0) * auto_dim).min(1.0) * readable, scale: (b.scale * room) + pulse, contrast: 1.0, dither: b.dither, rotate, alpha: 1.0 });
        if (layer.pulse_secs > 0.0 || layer.rotate_secs > 0.0) && app.playing() {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
    }
    if themed && b.mode == BgMode::CoverFullbleed {
        painter.rect_filled(rect, 0.0, look.solid());
    } else if themed && let Some(bg) = app.bg.as_ref().filter(|_| art) {
        let blend = theme::ease_in_out(((now - app.art_changed - b.crossfade_delay as f64) / b.crossfade_secs.max(0.05) as f64) as f32);
        let level = if app.reactive_bg && app.playing() && b.mode != BgMode::CoverStatic { (app.audio.status.level() - b.beat_threshold).max(0.0) * b.reactive } else { 0.0 };
        // Over a layer the dim is the canvas's opacity (the theme's `opacity`): the
        // layer shows through instead of black, so only `auto_dim` darkens it.
        let (dim, alpha) = if b.layer.is_some() { (auto_dim.min(1.0) * readable, b.np_dim) } else { ((b.np_dim * auto_dim).min(1.0) * readable, 1.0) };
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
    } else if themed && b.mode == BgMode::Solid {
        painter.rect_filled(rect, 0.0, crate::preset::hex(&look.app.np_solid).unwrap_or(look.solid()));
    } else if themed {
        painter.rect_filled(rect, 0.0, theme::lerp_color(theme::bg(), app.accent_color(), 0.25));
    }
    let shows = behind.map_or(1.0, |p| p.vis);
    if themed && b.fullbleed(app.fullscreen) {
        // The cover itself fills the screen behind a scrim. With the lyrics-only
        // layout on a wide window it fills the height on the left instead and
        // fades out toward the lyrics (reCover).
        if let Some(track) = app.current() {
            let side_by_side = behind.is_some_and(|p| p.beside);
            let area = if side_by_side { Rect::from_min_size(rect.min, Vec2::splat(rect.height())) } else { rect };
            // Made even while it's hidden, so the buttons after it keep their ids (and focus).
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
            child.set_clip_rect(area.intersect(clip));
            // Kept up while hidden too, so it doesn't replay an old song's crossfade when it's back.
            let fading = b.fullbleed_fade.then(|| fullbleed_fade(&child, &track.id, &track.thumbs, now, b.crossfade_delay as f64, b.crossfade_secs as f64)).flatten();
            if shows > 0.001 {
                let tint = Color32::from_gray((fullbleed_level * readable * 255.0) as u8).gamma_multiply(shows);
                // Over the theme's own background it fades away to show that; alone, into its flat colour.
                let mask = (side_by_side && b.mode != BgMode::CoverFullbleed).then_some(b.fade);
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
    }
    let k = b.scrim / 0.3;
    if themed && let Some([from, to]) = b.scrim_gradient {
        if b.scrim_across {
            super::shell::hgradient(&painter, rect, theme::shade(from), theme::shade(to));
        } else {
            super::shell::vgradient(&painter, rect, theme::shade(from), theme::shade(to));
        }
    } else if themed && k > 0.0 {
        super::shell::vgradient(&painter, rect, theme::shade((0.18 * k).min(0.95)), theme::shade((0.42 * k).min(0.95)));
    }
    if themed {
        for o in &b.overlays {
            super::shell::overlay_light(&painter, rect, o, app.accent_color());
        }
    }
    if themed && app.ambient_light && app.shown_video().is_some() {
        paint_glow(app, ui, rect, clip);
    }
    if app.visualizer && app.playing() && app.shown_video().is_none() {
        visualizer(app, &painter, rect);
        // The bars change every ~21 ms: 30 fps, like the animated layer, without drawing
        // Now playing at the screen's full rate.
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }
}

/// Settings › Appearance › Visualizer: a bar per band along the bottom, in the accent
/// colour, faint enough for the lyrics and art over it.
fn visualizer(app: &App, painter: &egui::Painter, rect: Rect) {
    let bars = app.audio.status.spectrum();
    // On the content's bottom edge: the player bar covers the backdrop below it.
    let bottom = if app.fullscreen { rect.bottom() } else { rect.bottom() - app.bar_height() };
    let (gap, max_h) = (4.0, rect.height() * 0.22);
    let w = (rect.width() - gap * (bars.len() + 1) as f32) / bars.len() as f32;
    let color = theme::with_alpha(app.accent_color(), 0.35);
    for (i, v) in bars.iter().enumerate() {
        let h = (v * max_h).max(2.0);
        let x = rect.left() + gap + i as f32 * (w + gap);
        painter.rect_filled(Rect::from_min_max(pos2(x, bottom - h), pos2(x + w, bottom)), w.min(6.0) / 2.0, color);
    }
}

/// About how much of the art's own brightness now playing's background shows, after the
/// theme's dimming (`auto_dim` included) and scrim: `art`, the theme's background is
/// the art; `fullbleed_level`, the full-screen cover's brightness where there is one.
fn shown_level(b: &crate::preset::Background, art: bool, fullbleed_level: f32, fullscreen: bool, auto_dim: f32) -> f32 {
    // Without the art, what's under it is a flat colour (which covers any layer too).
    let mut level = match b.layer {
        _ if !art => 0.0,
        // Each as painted: no brighter than the art itself.
        None => (b.np_dim * auto_dim).min(1.0),
        // The art at `np_dim` over the layer.
        Some(l) => b.np_dim * auto_dim.min(1.0) + (1.0 - b.np_dim) * (l.opacity * l.brightness.unwrap_or(1.0) * auto_dim).min(1.0),
    };
    if b.fullbleed(fullscreen) {
        level = level.max(fullbleed_level);
    }
    // The scrim's darkening, on average across the screen.
    let k = b.scrim / 0.3;
    let scrim = match b.scrim_gradient {
        Some([from, to]) => (from + to) / 2.0,
        None if k > 0.0 => ((0.18 * k).min(0.95) + (0.42 * k).min(0.95)) / 2.0,
        None => 0.0,
    };
    level * (1.0 - scrim.clamp(0.0, 1.0))
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
    let (header, content) = np_rects(area, app.fullscreen);
    // Decided every frame, a video's too, so what it was is never one from before a resize.
    let wide = content_wide(ui.ctx(), content);
    let video_lyrics = video_mode && look.now_playing.video_lyrics && !app.no_lyrics() && wide;
    let no_lyrics = app.no_lyrics() || (video_mode && !video_lyrics);
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
    let header = header.translate(vec2(0.0, -(header.height() + 20.0) * (1.0 - shown_header)));
    if hide && !quiet {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
    }
    if app.fullscreen && look.now_playing.fullscreen_progress_px > 0.0 {
        fullscreen_progress(app, ui, ui.ctx().content_rect(), look.now_playing.fullscreen_progress_px);
    }
    np_content(app, ui, content, &track, video.as_ref(), video_lyrics, no_lyrics, now);
    // A song gets the switch too once its music video is found (or is playing). Songs, not
    // music videos, with videos off: no switch on a song, and no music video looked up for
    // it. A music video keeps its own: it's the only way from its song back to the video.
    // A song standing in for a liked video is that video's too, as a music video is.
    let songs_only = app.play_songs && !app.show_video && track.stands_for.is_none();
    if look.now_playing.show_video_switch && !app.jam.active() && (track.has_video() || (!songs_only && (app.playing_as().is_some() || app.song_video().is_some()))) {
        song_video_switch(app, ui, header);
    }
    // Its actions (like, download, share…) are for what's heard: a song version
    // standing in for the music video.
    let heard = app.playing_track().cloned().unwrap_or_else(|| track.clone());
    let lyrics_menu = app.np_tab == NpTab::Lyrics && !no_lyrics;
    // With the app's own title bar these go in the notches. While now playing closes the
    // notch is the page's again (the account button): they don't come back to the header
    // meanwhile.
    if super::titlebar::notch_shown(ui.ctx()) {
        if app.np_open {
            notch_header(app, ui, &heard, lyrics_menu, no_lyrics, video_mode);
        }
        return;
    }
    // Its own id: the covers and cards drawn before it come and go, which would shift its buttons' ids (and lose focus).
    let mut hui = ui.new_child(egui::UiBuilder::new().scope_id(egui::Id::unique("np-header")).max_rect(header).layout(egui::Layout::left_to_right(egui::Align::Center)));
    // macOS: past the traffic lights, which stay over now playing.
    let gap = super::titlebar::left_gap(hui.ctx(), hui.max_rect(), 0.0);
    if gap > 0.0 {
        hui.add_space(gap);
    }
    if icon_button(&mut hui, icon::CHEVRON_DOWN, 22.0, crate::tr!("ui-close-now-playing"), false).clicked() {
        app.np_open = false;
        if app.fullscreen {
            app.set_fullscreen(ui.ctx(), false);
        }
    }
    hui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.add_space(super::titlebar::right_gap(ui.ctx(), ui.max_rect(), 0.0));
        let menu_right = ui.cursor().right();
        options_menu(app, ui, &heard, lyrics_menu, false);
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
        let (w, focused) = header_extras(app, ui, full_w, t, t, false, no_lyrics, video_mode);
        ui.ctx().data_mut(|d| d.insert_temp(id, (w, focused)));
    });
}

/// Now playing's buttons in the notches: the close arrow in its own at the top left, the
/// ⋯ menu in the window buttons', and Lyrics, Up next and full screen sliding out beside
/// it when the pointer comes near.
fn notch_header(app: &mut App, ui: &mut Ui, heard: &crate::model::Track, lyrics_menu: bool, no_lyrics: bool, video_mode: bool) {
    use super::titlebar::{NOTCH_BUTTON, Side, notch_button, notch_ui};
    // Now playing's first pass only measures it and draws nothing: the notch would show
    // room for buttons that aren't there.
    if ui.is_sizing_pass() {
        return;
    }
    if let Some(mut n) = notch_ui(ui, Side::Start, NOTCH_BUTTON.x)
        && notch_button(&mut n, icon::CHEVRON_DOWN, 16.0, crate::tr!("ui-close-now-playing"), false).clicked()
    {
        app.np_open = false;
    }
    // Their full width, whether one has focus, and where the pointer brings them out, as
    // they were last frame.
    let state_id = egui::Id::unique("np-notch-extras");
    let (full_w, focused, near, menu_open, shown) = ui.ctx().data(|d| d.get_temp::<(f32, bool, Rect, bool, f32)>(state_id)).unwrap_or((220.0, false, Rect::NOTHING, false, 0.0));
    let hovered = ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| near.contains(p));
    let want = hovered || focused || menu_open;
    // The notch makes room first and they fade in once it has; going, they fade out
    // first and then it closes up. Nothing shows half cut off by its edge.
    let t = theme::anim_bool(ui.ctx(), "np-notch-extras", want || shown > 0.0, 0.16);
    let shown = theme::anim_bool(ui.ctx(), "np-notch-extras-shown", want && t >= 1.0, 0.12);
    // The ⋯ button, a gap, and as much room as the notch has made for them.
    const GAP: f32 = 6.0;
    let shown_w = if t > 0.0 { GAP + full_w * t } else { 0.0 };
    let Some(mut n) = notch_ui(ui, Side::End, NOTCH_BUTTON.x + shown_w) else { return };
    let slot = n.max_rect();
    // Its actions (like, download, share…) are for what's heard: a song version standing
    // in for the music video.
    let open = options_menu(app, &mut n, heard, lyrics_menu, true);
    // Around the ⋯ button and all of them, out or not, and a little below the notch.
    let near = Rect::from_min_max(pos2(slot.right() - NOTCH_BUTTON.x - GAP - full_w - 16.0, slot.top() - 3.0), pos2(slot.right() + 4.0, slot.bottom() + 24.0));
    let (full_w, focused) = if t > 0.0 {
        n.add_space(GAP);
        header_extras(app, &mut n, full_w, t, shown, true, no_lyrics, video_mode)
    } else {
        (full_w, false)
    };
    ui.ctx().data_mut(|d| d.insert_temp(state_id, (full_w, focused, near, open, shown)));
}

/// Lyrics, Up next (and Comments) and full screen, sliding out `t` of their `full_w`
/// to the left of the ⋯ menu, `alpha` seen: `compact` in the window buttons' notch.
/// Their full width now, and whether one has keyboard focus.
#[allow(clippy::too_many_arguments)]
fn header_extras(app: &mut App, ui: &mut Ui, full_w: f32, t: f32, alpha: f32, compact: bool, no_lyrics: bool, video_mode: bool) -> (f32, bool) {
    let h = if compact { super::titlebar::NOTCH_BUTTON.y } else { ui.max_rect().height() };
    let (slot, _) = ui.allocate_exact_size(vec2(full_w * t, h), Sense::hover());
    let mut eui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_max(pos2(slot.right() - full_w, slot.top()), slot.max))
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    eui.set_clip_rect(slot.intersect(ui.clip_rect()));
    eui.multiply_opacity(alpha);
    let ui = &mut eui;
    if compact {
        ui.spacing_mut().item_spacing.x = 4.0;
    }
    let chip = |ui: &mut Ui, label: &str, on: bool| if compact { super::titlebar::notch_chip(ui, label, on) } else { widgets::default_chip(ui, label, on) };
    let (g, name) = if app.fullscreen { (icon::SHRINK, crate::tr!("player-exit-full-screen")) } else { (icon::EXPAND, crate::tr!("player-full-screen")) };
    let fs_btn = if compact { super::titlebar::notch_button(ui, g, 15.0, name, false) } else { icon_button(ui, g, 18.0, name, false) };
    if fs_btn.clicked() {
        let fs = !app.fullscreen;
        app.set_fullscreen(ui.ctx(), fs);
    }
    ui.add_space(if compact { 2.0 } else { 8.0 });
    let mut focused = fs_btn.has_focus();
    // Clicking the active chip again turns it off, leaving just the artwork. These look
    // the same in every theme.
    // Comments, rightmost of the three (right to left: drawn first), with Settings ›
    // Playback › Show comments on.
    if !app.show_comments && app.np_tab == NpTab::Comments {
        app.np_tab = NpTab::Art;
    }
    let com = app.show_comments.then(|| chip(ui, crate::tr!("player-comments"), app.np_tab == NpTab::Comments));
    if com.as_ref().is_some_and(|c| c.clicked()) {
        app.np_tab = if app.np_tab == NpTab::Comments { NpTab::Art } else { NpTab::Comments };
    }
    let up = chip(ui, crate::tr!("ui-up-next"), app.np_tab == NpTab::UpNext);
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
        chip(ui, crate::tr!("settings-lyrics"), app.np_tab == NpTab::Lyrics && !no_lyrics).on_disabled_hover_text(if video_mode { crate::tr!("player-video-lyrics") } else { crate::tr!("player-no-lyrics") })
    });
    if lyr.inner.clicked() {
        app.np_tab = if app.np_tab == NpTab::Lyrics { NpTab::Art } else { NpTab::Lyrics };
    }
    focused |= up.has_focus() || lyr.inner.has_focus() || com.is_some_and(|c| c.has_focus());
    (ui.min_rect().width(), focused)
}

/// Where now playing's side panel is in sliding in or out.
struct PanelMotion {
    /// The panel showing, or the one that was while the Art tab hides it.
    tab: NpTab,
    /// Linear 0..1 over the transition, 1 with the panel up; each part eases its own slice of it.
    p: f32,
    panel_vis: f32,
    /// The artwork alone (in the middle).
    art_vis: f32,
}

/// Whether the lyrics panel should be shown given the track's lyric state:
/// `loaded_lyrics` is `None` while lyrics are still fetching for the new track, in which
/// case we hold `prev_open` so switching between songs without lyrics stays centered
/// without flashing the loading spinner.
fn lyrics_panel_wanted(loaded_lyrics: Option<bool>, wait_done: bool, prev_open: bool) -> bool {
    match loaded_lyrics {
        Some(true) => true,
        Some(false) => !wait_done,
        None => prev_open,
    }
}

#[derive(Clone, Copy)]
struct PanelState {
    pass: u64,
    tab: NpTab,
    last_panel: NpTab,
    lyrics_open: bool,
    want: bool,
}

/// The side panel (lyrics / up next / comments) slides in and out when toggled or when
/// switching between songs with and without lyrics. Called again in the same pass it gives
/// the same answer.
fn panel_motion(app: &App, ctx: &egui::Context, track_id: &str, no_lyrics: bool, now: f64) -> PanelMotion {
    let state_id = egui::Id::unique("np-panel-state");
    let pass = ctx.cumulative_pass_nr();
    let prev = ctx.data(|d| d.get_temp::<PanelState>(state_id));
    let state = match prev {
        Some(s) if s.pass == pass => s,
        prev => {
            let mut last_panel = prev.map_or(NpTab::Lyrics, |s| s.last_panel);
            if app.np_tab != NpTab::Art {
                last_panel = app.np_tab;
            }
            // Manually switching into Lyrics opens the panel even while lyrics are loading.
            let mut lyrics_open = prev.is_none_or(|s| s.lyrics_open);
            if app.np_tab == NpTab::Lyrics && prev.is_some_and(|s| s.tab != NpTab::Lyrics) {
                lyrics_open = true;
            }
            let want = match app.np_tab {
                NpTab::Art => false,
                NpTab::UpNext | NpTab::Comments => true,
                NpTab::Lyrics => {
                    let loaded_lyrics = if no_lyrics {
                        Some(false)
                    } else if app.lyrics.is_some() {
                        Some(true)
                    } else {
                        None
                    };
                    let wait = app.look().now_playing.no_lyrics.center_after;
                    let wait_done = if loaded_lyrics == Some(false) && wait > 0.0 && lyrics_open {
                        // A while in its usual place, then smoothly over to the middle.
                        let since = ctx.memory_mut(|m| *m.data.get_temp_mut_or_insert_with(egui::Id::unique(("no-lyrics-wait", track_id)), || now));
                        if now - since < wait as f64 {
                            ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait as f64 - (now - since) + 0.01));
                            false
                        } else {
                            true
                        }
                    } else {
                        true
                    };
                    let open = lyrics_panel_wanted(loaded_lyrics, wait_done, lyrics_open);
                    lyrics_open = open;
                    open
                }
            };
            let next = PanelState { pass, tab: app.np_tab, last_panel, lyrics_open, want };
            ctx.data_mut(|d| d.insert_temp(state_id, next));
            next
        }
    };
    let p = ctx.animate_bool_with_time(egui::Id::unique("np-panel"), state.want, 0.6 * theme::speed());
    if p > 0.0 && p < 1.0 {
        // Every frame while sliding, not the now-playing view's usual 30 fps.
        ctx.request_repaint();
    }
    // Whichever is leaving slides and fades out over the first 45%, then the other
    // comes in over the last 70%, both easing out, in either direction.
    let (panel_vis, art_vis) = if state.want {
        (theme::ease_out((p - 0.3) / 0.7), 1.0 - theme::ease_out(p / 0.45))
    } else {
        let q = 1.0 - p;
        (1.0 - theme::ease_out(q / 0.45), theme::ease_out((q - 0.3) / 0.7))
    };
    PanelMotion { tab: state.last_panel, p, panel_vis, art_vis }
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
    let PanelMotion { tab: panel_tab, p, panel_vis, art_vis } = panel_motion(app, ui.ctx(), &track.id, no_lyrics, now);
    let ppp = ui.ctx().pixels_per_point();
    let window_w = ui.ctx().content_rect().width();
    // The cover gliding between its two places (wide layouts): a gentle S-curve.
    let glide = p * p * (3.0 - 2.0 * p);
    let portrait = portrait_layout(ui.ctx(), content);
    let wide = content_wide(ui.ctx(), content);
    let narrow = !portrait && !wide;
    let corner = wide && layout == Layout::Corner;
    let fs_corner = wide && app.fullscreen && cs.fullscreen.is_some_and(|f| f.corner_layout);
    let lyrics_only = narrow || corner || fs_corner || layout == Layout::LyricsOnly;
    // reCover: the backdrop cover sits on the left, so the lyrics take the right side.
    let beside_cover = wide && layout == Layout::LyricsOnly && look.background.fullbleed(app.fullscreen);
    let centered = portrait || (lyrics_only && !beside_cover);
    let stacked_portrait = portrait && !lyrics_only;
    // Artwork alone: centered.
    let col_art = Rect::from_center_size(content.center(), vec2(content.width().min(content.height() * 0.9), content.height()));
    let np = &look.now_playing;
    // On its own with the theme's corner card, it keeps below (or above) the card.
    let col_alone = match np.shows_info_card(app.fullscreen).then(|| info_card_room(np, content)).filter(|c| c.right() > col_art.left()) {
        Some(c) if np.info_card_top => Rect::from_min_max(pos2(col_art.left(), c.bottom() + 16.0), col_art.max),
        Some(c) => Rect::from_min_max(col_art.min, pos2(col_art.right(), c.top() - 16.0)),
        None => col_art,
    };
    let slide = 72.0;
    let corner_h = (content.height() * 0.24).clamp(96.0, 190.0);
    // Where the card goes: the cover's column when there is one, else the corner of everything.
    let mut card_at = content;
    let right = if fs_corner {
        // A small cover at the bottom left with the title beside it, the lyrics on the right.
        let share = look.now_playing.fullscreen_panel_frac.unwrap_or(0.5);
        let split = content.right() - content.width() * share;
        // Nothing to go beside (the Art tab, no lyrics): the cover on its own, in the middle.
        if art_vis > 0.001 {
            let px = (art_side(cs, col_alone, app.fullscreen, window_w) * ppp) as u32;
            let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(content));
            cui.multiply_opacity(art_vis);
            cover_column(app, &mut cui, col_alone.translate(vec2(-slide * (1.0 - art_vis), 0.0)), track, no_lyrics, now, px, None);
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
        let px = (art_side(cs, col_alone, app.fullscreen, window_w) * ppp) as u32;
        if art_vis > 0.001 {
            let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(content));
            cui.multiply_opacity(art_vis);
            cover_column(app, &mut cui, col_alone.translate(vec2(-slide * (1.0 - art_vis), 0.0)), track, no_lyrics, now, px, None);
        } else {
            // Hidden behind the lyrics: have it loaded for when they're turned off.
            widgets::preload_cover(ui.ctx(), &track.thumbs, px);
        }
        let full_panel = look.now_playing.fullscreen_panel_frac.filter(|_| app.fullscreen && !portrait);
        if let Some(f) = full_panel {
            let side = content.width() * (1.0 - f);
            if cover_side == Side::Right { Rect::from_min_max(content.min, pos2(content.right() - side, content.bottom())) } else { Rect::from_min_max(pos2(content.left() + side, content.top()), content.max) }
        } else if beside_cover { Rect::from_min_max(pos2(content.left() + content.width() * 0.46, content.top()), content.max) } else { content }
    } else if stacked_portrait {
        let (col_panel, side_panel, card_panel) = portrait_panel(look, content, app.fullscreen);
        let side_alone = art_side(cs, col_alone, app.fullscreen, window_w);
        let col = Rect::from_min_max(col_alone.min.lerp(col_panel.min, glide), col_alone.max.lerp(col_panel.max, glide));
        let side_now = egui::lerp(side_alone..=side_panel, glide);
        let px = (side_alone.max(side_panel) * ppp) as u32;
        cover_column(app, ui, col, track, no_lyrics, now, px, Some((side_now, glide)));
        // The card goes with the cover: in the corner while it's on its own (which leaves the
        // card room), by it once stacked. It fades between the two rather than moving, or it
        // would cross the panel coming in.
        card_at = if glide < 0.5 { content } else { card_panel };
        Rect::from_min_max(pos2(content.left(), (col_panel.bottom() + 12.0).min(content.bottom())), content.max)
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
        cover_column(app, ui, col, track, no_lyrics, now, px, None);
        card_at = col;
        if cover_side == Side::Right {
            Rect::from_min_max(content.min, pos2(col_panel.left() - 28.0, content.bottom()))
        } else {
            Rect::from_min_max(pos2(col_panel.right() + 28.0, content.top()), content.max)
        }
    };
    // The theme's card for the song, in a corner of the cover's place (the cover then has no
    // title under it). It stays with the backdrop's cover too, so it's the one card through the transition.
    let card_vis = if stacked_portrait { (2.0 * glide - 1.0).abs() } else if fs_corner { art_vis } else if corner { 0.0 } else if beside_cover || !lyrics_only { 1.0 } else { art_vis };
    if np.shows_info_card(app.fullscreen) && card_vis > 0.001 {
        let mut card = ui.new_child(egui::UiBuilder::new().max_rect(card_at));
        card.multiply_opacity(card_vis);
        info_card(app, &mut card, card_at, track);
    }
    if panel_vis <= 0.001 {
        return;
    }
    let toward = if !lyrics_only && cover_side == Side::Right { -slide } else { slide };
    let shift = if stacked_portrait { vec2(0.0, slide) } else { vec2(toward, 0.0) };
    let rect = right.translate(shift * (1.0 - panel_vis));
    let inset = look.now_playing.fullscreen_panel_inset.filter(|_| app.fullscreen);
    let rect = inset.map_or(rect, |[top, bottom]| {
        let top = rect.top() + if stacked_portrait { top.max(0.0) } else { top };
        let bottom = (rect.bottom() - bottom).max(top);
        Rect::from_min_max(pos2(rect.left(), top), pos2(rect.right(), bottom))
    });
    // Switching between Lyrics, Up next and Comments: the old one fades out over the first 55%,
    // the new one fades in and rises into place from a third of the way.
    for (tab, key) in [(NpTab::Lyrics, "np-tab-lyrics"), (NpTab::UpNext, "np-tab-upnext"), (NpTab::Comments, "np-tab-comments")] {
        let w = ui.ctx().animate_bool_with_time(egui::Id::unique(key), panel_tab == tab, 0.45 * theme::speed());
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
        let mut clip = if inset.is_some() { ui.clip_rect() } else { content.intersect(ui.clip_rect()) };
        if stacked_portrait {
            clip = clip.with_min_y(clip.min.y.max(right.top()));
        }
        rui.set_clip_rect(clip);
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
    let pos = app.seek_drag.unwrap_or(status.position_smooth().as_secs_f32()).min(total.max(0.0));
    let r = Rect::from_min_max(pos2(screen.left(), screen.bottom() - px), screen.max);
    let resp = ui.interact(r, ui.scope_id().with("full-seek"), Sense::click());
    let resp = named(resp, crate::tr!("ui-seek")).on_hover_cursor(egui::CursorIcon::PointingHand);
    app.seek_wheel.hover(&resp);
    let bar_r = app.look().now_playing.fullscreen_progress_radius;
    ui.painter().rect_filled(r, bar_r, theme::with_alpha(Color32::WHITE, 0.12));
    if total > 0.0 {
        let filled = Rect::from_min_size(r.min, vec2(r.width() * pos / total, r.height()));
        ui.painter().rect_filled(filled, bar_r, theme::with_alpha(Color32::WHITE, 0.32));
        super::ab_loop::paint_loop(app, ui, r, total, pos, bar_r, Color32::WHITE);
        super::ab_loop::loop_menu(app, ui, &resp, r, total, pos);
        let font = theme::semibold((px * 0.55).clamp(10.0, 14.0));
        widgets::time_at(ui, pos2(r.left() + 12.0, r.center().y), egui::Align2::LEFT_CENTER, &super::fmt_time(pos as u32), font.clone(), Color32::WHITE);
        widgets::time_at(ui, pos2(r.right() - 12.0, r.center().y), egui::Align2::RIGHT_CENTER, &super::fmt_end(pos, total, app.time_left), font, Color32::WHITE);
        if resp.clicked()
            && let Some(p) = resp.interact_pointer_pos()
        {
            app.seek_drag = None;
            app.actions.push(Action::Seek(((p.x - r.left()) / r.width()).clamp(0.0, 1.0) * total));
        }
    }
}

/// Header and content rects inside `area`, shared by `behind_panel` and `body`.
fn np_rects(area: Rect, fullscreen: bool) -> (Rect, Rect) {
    let lift = if fullscreen { 0.0 } else { 20.0 - f32::from(super::titlebar::top_margin(20, 44.0)) };
    let header = Rect::from_min_size(area.min - vec2(0.0, lift), vec2(area.width(), 44.0));
    let content = Rect::from_min_max(pos2(area.left(), header.bottom() + 12.0), area.max);
    (header, content)
}

/// Whether the cover stacks over the panel in `content` (a portrait monitor, or a tall,
/// narrow window). It's kept across frames so a window resized near the line doesn't flip.
fn portrait_layout(ctx: &egui::Context, content: Rect) -> bool {
    let id = egui::Id::unique("np-portrait");
    let was = ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let now = is_portrait(content.size(), was);
    if now != was {
        ctx.data_mut(|d| d.insert_temp(id, now));
    }
    now
}

/// The content's height to width from which it stacks, and below which it goes back side
/// by side. Half of a 16:9, 16:10 or 3:2 screen stays under 1.33 however large it is (about 1.2
/// at the usual sizes), so it stays side by side; a portrait monitor comes to about 1.7, a
/// 3:2 tablet stood up to 1.38, a third of a 1440p screen to 1.55.
const PORTRAIT_RATIO: [f32; 2] = [1.35, 1.3];

/// No least size: it's in points, and the smallest window (`MIN_SIZE`) this tall has room
/// for the cover and the panel.
fn is_portrait(content: Vec2, was: bool) -> bool {
    content.y >= content.x * PORTRAIT_RATIO[usize::from(was)]
}

/// The stacked layout's cover column at the top, the artwork's side, and where the theme's
/// card for the song goes: under the cover, or over it with `info_card_top`, at its left edge.
fn portrait_panel(look: &crate::preset::Preset, content: Rect, fullscreen: bool) -> (Rect, f32, Rect) {
    let np = &look.now_playing;
    let text_h = if !np.shows_info_card(fullscreen) && np.shows_title(fullscreen) {
        let [title_px, artist_px] = np.fullscreen_text_px.filter(|_| fullscreen).unwrap_or([np.title_px, np.artist_px]);
        match np.fullscreen_title_card.filter(|_| fullscreen).or(np.title_card) {
            Some(c) => title_px * 1.4 + artist_px * 1.5 + (c.pad * 2.0).max(22.0) + if c.attached { 0.0 } else { 11.0 },
            None => 22.0 + title_px * 1.4 + artist_px * 1.3,
        }
    } else {
        0.0
    };
    // The card has the title, so it takes the text's place (12 apart from the cover).
    let card_h = if np.shows_info_card(fullscreen) { info_card_room(np, content).height() + np.info_card_margin + 12.0 } else { 0.0 };
    let (above, below) = if np.info_card_top { (card_h, text_h) } else { (0.0, text_h + card_h) };
    let top_pad = (content.height() * 0.03).clamp(16.0, 48.0);
    // Up to about half the height, but the panel under it keeps 600 points at least: a few
    // lines even in a theme with large lyrics, in a window only just tall enough to stack.
    let column = (content.height() * 0.48).min(content.height() - 600.0);
    let max_h = (column - top_pad - above - below - 16.0).max(100.0);
    let cs = &look.cover;
    // The theme's limit, but not its steps by window width: here the cover has the whole width.
    let cap = (content.width() * 0.82).min(max_h).min(art_max_px(cs, fullscreen, None));
    let desired = (content.width() * 0.76).min(max_h) * cs.size.clamp(0.65, 1.3);
    let side = desired.min(cap).max(140.0_f32.min(cap));
    let col = Rect::from_min_size(content.min + vec2(0.0, top_pad + above), vec2(content.width(), side + below));
    let edge = (side / 2.0 + np.info_card_margin) * vec2(1.0, 0.0);
    let card = Rect::from_min_max(pos2(col.center().x, col.top() - above) - edge, pos2(col.center().x, col.bottom()) + edge);
    (col, side, card)
}

/// Room for lyrics beside something else.
fn content_wide(ctx: &egui::Context, area: Rect) -> bool {
    !portrait_layout(ctx, area) && area.width() >= 860.0
}

/// Upper pixel bound imposed by the theme on the artwork size; by the window's width
/// (`window_w`) when it has steps.
fn art_max_px(cs: &crate::preset::CoverStyle, fullscreen: bool, window_w: Option<f32>) -> f32 {
    let fs_max = cs.fullscreen.filter(|_| fullscreen).and_then(|f| f.max_px);
    let stepped = window_w.and_then(|w| cs.size_steps.iter().rev().find(|s| w >= s[0])).map(|s| s[1]);
    fs_max.or(stepped).or(if fullscreen && cs.fullscreen.is_some() { None } else { cs.max_px }).unwrap_or(f32::INFINITY)
}

/// Size of the artwork laid out in `col`.
fn art_side(cs: &crate::preset::CoverStyle, col: Rect, fullscreen: bool, window_w: f32) -> f32 {
    let fs = cs.fullscreen.filter(|_| fullscreen).unwrap_or_default();
    let width_frac = fs.width_frac;
    let side = match fs.size_vh.or(cs.size_vh) {
        // As much of the window's height as the theme asks: it may run past the column's
        // top and bottom, but not its sides, or a tall window puts it under the lyrics.
        Some(vh) => ((col.height() + 76.0) * vh).min(col.width()),
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
    side.min(art_max_px(cs, fullscreen, Some(window_w)))
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
    let cover = named(ui.interact(art, ui.scope_id().with(("corner-cover", &track.id)), Sense::click()), if app.playing() { crate::tr!("ui-pause") } else { crate::tr!("ui-play") }).on_hover_cursor(egui::CursorIcon::PointingHand);
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

/// The most room the theme's card takes in `content`: its text-only card is no wider or taller.
fn info_card_room(np: &crate::preset::NowPlayingStyle, content: Rect) -> Rect {
    let size = vec2((content.width() * 0.34).clamp(220.0, 420.0), 76.0);
    let m = np.info_card_margin;
    if np.info_card_top { Rect::from_min_size(content.min + vec2(m, m), size) } else { Rect::from_min_size(pos2(content.left() + m, content.bottom() - size.y - m), size) }
}

/// The corner card with the song, over a cover that fills the view.
fn info_card(app: &mut App, ui: &mut Ui, content: Rect, track: &crate::model::Track) {
    let np = &app.look().now_playing;
    let r = info_card_room(np, content);
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

/// Since when the pointer has been still over full screen's cover.
fn cover_still_id() -> egui::Id {
    egui::Id::unique("np-cover-still-since")
}

/// Back to showing full screen's cover player, as if the pointer had just moved.
pub(super) fn wake_cover_player(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<f64>(cover_still_id()));
}

/// Full screen's cover as the player, while the pointer's on it: the seek bar, then
/// shuffle, previous, play, next and repeat along its bottom, and the volume up its right
/// edge, to drag, click or scroll, mute under it. A disc keeps them inside its circle. All
/// of it goes, the pointer with it, once the pointer has rested a moment away from them;
/// while paused it stays. Otherwise a hairline along the bottom shows how far in the song is.
#[allow(clippy::too_many_arguments)]
fn cover_player(app: &mut App, ui: &mut Ui, track: &crate::model::Track, art: Rect, hit: Rect, (cr, radius): (egui::CornerRadius, f32), round: bool, on_art: bool, now: f64) {
    let ctx = ui.ctx().clone();
    let side = art.width();
    let s = (side / 520.0).clamp(0.75, 1.2);
    let (c, r) = (art.center(), side / 2.0);
    // How far either side of the middle a line at `y` may reach: inside the disc on a round cover.
    let half_w = |y: f32, inset: f32| if round { (r * r - (y - c.y).powi(2)).max(0.0).sqrt() - inset } else { r - inset };
    let row_y = if round { c.y + r * 0.6 } else { art.bottom() - 44.0 * s };
    let seek_y = row_y - 50.0 * s;
    let label_y = seek_y - 16.0 * s;
    let seek_w = half_w(label_y, 30.0 * s);
    let seek_hit = Rect::from_center_size(pos2(c.x, seek_y), vec2(seek_w * 2.0, 24.0));
    // The volume, mute under it, kept clear of the song's length over the seek bar's end.
    let (vol_x, vol_top, vol_h) = if round { (c.x + r * 0.76, c.y - r * 0.4, r * 0.6) } else { (art.right() - 32.0 * s, c.y - side * 0.26, side * 0.38) };
    let mute_at = pos2(vol_x, (vol_top + vol_h + 22.0 * s).min(label_y - 32.0 * s));
    let bar = Rect::from_min_max(pos2(vol_x - 16.0 * s, vol_top), pos2(vol_x + 16.0 * s, mute_at.y - 22.0 * s));
    // A glyph's disc, `px` across: what `cover_glyph` answers on.
    let disc = |px: f32| px + 22.0;
    // Spread no wider than a disc's chord at the row.
    let gap = (66.0 * s).min(half_w(row_y, disc(20.0 * s) / 2.0 + 4.0) / 2.0);
    let row = Rect::from_center_size(pos2(c.x, row_y), vec2(gap * 4.0, 0.0)).expand(disc(36.0 * s) / 2.0 + 4.0);
    let vol_id = ui.scope_id().with("np-cover-volume");

    // Up while the pointer moves over the cover, and while it's on a control, dragging the
    // volume, a menu's open, or nothing's playing.
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let over = on_art && ui.rect_contains_pointer(hit);
    let on_controls = pointer.is_some_and(|p| row.contains(p) || seek_hit.expand(8.0).contains(p) || bar.expand(8.0).contains(p) || p.distance(mute_at) < disc(17.0 * s) / 2.0 + 4.0);
    let dragging = ctx.is_being_dragged(vol_id) || app.seek_drag.is_some();
    let moved = ui.input(|i| i.pointer.delta() != Vec2::ZERO || !i.events.is_empty());
    let since = match ctx.data(|d| d.get_temp::<f64>(cover_still_id())) {
        Some(t) if !moved => t,
        _ => {
            ctx.data_mut(|d| d.insert_temp(cover_still_id(), now));
            now
        }
    };
    let busy = on_controls || dragging || egui::Popup::is_any_open(&ctx) || !app.playing();
    let quiet = !busy && now - since > 2.5;
    // Eased in and out, the dimming and the controls together.
    let shown = theme::anim_bool_soft(&ctx, "np-cover-player", (over || dragging) && !quiet, 0.4);
    if over && !quiet {
        // To see the pointer has been still long enough.
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    } else if over {
        ctx.set_cursor_icon(egui::CursorIcon::None);
    }

    // Dimmed just enough for the white controls: on a square cover from nothing at the top to
    // most at the row, a disc evenly.
    if shown > 0.01 {
        if round {
            ui.painter().circle_filled(c, r, theme::shade(0.22 * shown));
        } else {
            super::shell::cover_vgradient(ui.painter(), art, cr, radius, theme::shade(0.0), theme::shade(0.42 * shown));
        }
    }
    // Always, how far into the song it is: a hairline along the cover's bottom that rises into
    // the seek bar as the controls come up, and settles back as they go. Not where the theme
    // has its own along the screen's bottom.
    let times = song_times(app, track);
    let morphing = shown < 0.99 && times.1 > 0.0 && app.look().now_playing.fullscreen_progress_px <= 0.0;
    if morphing {
        let rest_y = if round { c.y + r * 0.86 } else { art.bottom() - 12.0 * s };
        let rest_w = half_w(rest_y, radius.max(12.0 * s).min(r * 0.3));
        let y = egui::lerp(rest_y..=seek_y, shown);
        let w = egui::lerp(rest_w..=seek_w, shown);
        let thick = egui::lerp(2.0..=3.0, shown);
        let line = Rect::from_center_size(pos2(c.x, y), vec2(w * 2.0, thick));
        // Edged in dark at rest, to stay seen on a light cover; the seek bar's own track as it rises.
        ui.painter().rect_filled(line.expand(1.0), thick / 2.0 + 1.0, theme::shade(0.3 * (1.0 - shown)));
        ui.painter().rect_filled(line, thick / 2.0, theme::with_alpha(Color32::WHITE, 0.25 * shown));
        let filled = Rect::from_min_size(line.min, vec2(line.width() * times.0 / times.1, thick));
        ui.painter().rect_filled(filled, thick / 2.0, theme::with_alpha(Color32::WHITE, egui::lerp(0.9..=1.0, shown)));
    }
    if shown <= 0.01 {
        return;
    }
    let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(art));
    cui.multiply_opacity(shown);
    let ui = &mut cui;

    let playing = app.playing() || app.buffering;
    let (rep_glyph, rep_on) = match app.queue.repeat {
        super::Repeat::Off => (icon::REPEAT, false),
        super::Repeat::All => (icon::REPEAT, true),
        super::Repeat::One => (icon::REPEAT_1, true),
    };
    // The seek bar over the row, the time so far and the song's length above its ends.
    if times.1 > 0.0 {
        let font = theme::semibold(12.0 * s);
        let dim = theme::with_alpha(Color32::WHITE, 0.75);
        widgets::time_at(ui, pos2(seek_hit.left(), label_y), egui::Align2::LEFT_CENTER, &super::fmt_time(times.0 as u32), font.clone(), dim);
        widgets::time_at(ui, pos2(seek_hit.right(), label_y), egui::Align2::RIGHT_CENTER, &super::fmt_end(times.0, times.1, app.time_left), font, dim);
    }
    // Taking over from the rising hairline once it's arrived, in the same place and size.
    if !morphing {
        seek_line(app, ui, seek_hit, ui.scope_id().with("np-cover-seek"), times, Color32::WHITE);
    }

    let at = |k: f32| pos2(c.x + gap * k, row_y);
    if cover_glyph(ui, at(-2.0), icon::SHUFFLE, 20.0 * s, "shuffle", crate::tr!("ui-shuffle"), app.queue.shuffled()).clicked() {
        app.toggle_shuffle();
    }
    if cover_glyph(ui, at(-1.0), icon::SKIP_BACK, 26.0 * s, "prev", crate::tr!("ui-previous"), true).clicked() {
        app.prev();
    }
    if cover_glyph(ui, at(0.0), if playing { icon::PAUSE } else { icon::PLAY }, 36.0 * s, "play", if playing { crate::tr!("ui-pause") } else { crate::tr!("ui-play") }, true).clicked() {
        app.actions.push(Action::TogglePlay);
    }
    if cover_glyph(ui, at(1.0), icon::SKIP_FORWARD, 26.0 * s, "next", crate::tr!("ui-next"), true).clicked() {
        app.next();
    }
    if cover_glyph(ui, at(2.0), rep_glyph, 20.0 * s, "repeat", crate::tr!("ui-repeat"), rep_on).clicked() {
        app.queue.repeat = app.queue.repeat.next();
    }
    if rep_on {
        ui.painter().circle_filled(at(2.0) + vec2(0.0, 19.0 * s), 2.0, Color32::WHITE);
    }

    // The volume: a slim bar that widens inwards under the pointer, its outer edge staying
    // put, filled from the bottom.
    let vol = app.audio.volume();
    let vresp = ui.interact(bar, vol_id, Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::PointingHand);
    vresp.widget_info(|| egui::WidgetInfo::slider(true, vol as f64, crate::tr!("ui-volume")));
    let lit = theme::anim_bool(&ctx, vol_id.with("h"), vresp.hovered() || vresp.dragged(), 0.15);
    let w = (6.0 + 5.0 * lit) * s;
    let outer = vol_x + 3.0 * s;
    let track_r = Rect::from_min_max(pos2(outer - w, bar.top()), pos2(outer, bar.bottom()));
    ui.painter().rect_filled(track_r.expand(1.0), w / 2.0 + 1.0, theme::shade(0.2));
    ui.painter().rect_filled(track_r, w / 2.0, theme::with_alpha(Color32::WHITE, 0.28));
    let fill = Rect::from_min_max(pos2(track_r.left(), track_r.bottom() - track_r.height() * vol), track_r.max);
    ui.painter().rect_filled(fill, w / 2.0, Color32::WHITE);
    if lit > 0.0 {
        let y = fill.top().clamp(track_r.top() + 8.0, track_r.bottom() - 8.0);
        let pct = format!("{}%", (vol * 100.0).round());
        ui.painter().text(pos2(track_r.left() - 10.0 * s, y), egui::Align2::RIGHT_CENTER, pct, theme::semibold(12.0 * s), theme::with_alpha(Color32::WHITE, lit));
    }
    if let Some(p) = vresp.interact_pointer_pos()
        && (vresp.dragged() || vresp.clicked())
    {
        app.audio.set_volume(((track_r.bottom() - p.y) / track_r.height()).clamp(0.0, 1.0));
    }
    let vglyph = if vol == 0.0 { icon::VOLUME_X } else if vol < 0.5 { icon::VOLUME_1 } else { icon::VOLUME_2 };
    let mute = cover_glyph(ui, mute_at, vglyph, 17.0 * s, "mute", if vol > 0.0 { crate::tr!("ui-mute") } else { crate::tr!("ui-unmute") }, true);
    if mute.clicked() {
        super::mini::toggle_mute(app, ui);
    }
    if vresp.hovered() || mute.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            app.audio.set_volume((app.audio.volume() + scroll / 600.0).clamp(0.0, 1.0));
        }
    }
}

/// A white glyph on full screen's cover, a soft disc behind it under the pointer. `on`:
/// fully white; off (as Repeat is), a little dimmer. `key` keeps its id while `name` changes.
fn cover_glyph(ui: &Ui, c: egui::Pos2, glyph: char, px: f32, key: &str, name: &str, on: bool) -> egui::Response {
    let d = px + 22.0;
    let rect = Rect::from_center_size(c, Vec2::splat(d));
    let resp = named(ui.interact(rect, ui.scope_id().with(("np-cover-player", key)), Sense::click()), name).on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
    if h > 0.0 {
        ui.painter().circle_filled(c, d / 2.0, theme::with_alpha(Color32::WHITE, 0.14 * h));
    }
    widgets::focus_ring(ui, &resp, rect, d / 2.0);
    let press = if resp.is_pointer_button_down_on() { 0.9 } else { 1.0 + 0.08 * h };
    let a = if on { 1.0 } else { 0.6 + 0.4 * h };
    widgets::icon_at(ui, c, glyph, px * press, theme::with_alpha(Color32::WHITE, a));
    resp
}

/// Artwork with the title and artists under it, laid out in `col`. `px`: the size to
/// fetch the image at (fixed while the column animates). `stacked`: `(side, glide)` for
/// the stacked portrait layout.
#[allow(clippy::too_many_arguments)]
fn cover_column(app: &mut App, ui: &mut Ui, col: Rect, track: &crate::model::Track, no_lyrics: bool, now: f64, px: u32, stacked: Option<(f32, f32)>) {
    use crate::preset::Shape;
    let look = app.look();
    let cs = &look.cover;
    let np = &look.now_playing;
    let side = stacked.map_or_else(|| art_side(cs, col, app.fullscreen, ui.ctx().content_rect().width()), |(s, _)| s);
    let free = 1.0 - stacked.map_or(0.0, |(_, g)| g);
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
        let full = ui.ctx().animate_bool_with_time(egui::Id::unique("np-cover-full"), app.fullscreen, secs / theme::anim_speed());
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
    let [pad_x, pad_y] = fs.padding.unwrap_or(cs.padding);
    let shift = (vec2(cs.offset_vh[0], cs.offset_vh[1]) * window_h + vec2(pad_x, pad_y)) * free;
    let center_y = col.top() + side / 2.0 + (col.height() - side - 110.0).max(0.0) * 0.35 * free;
    let art = Rect::from_center_size(pos2(col.center().x, center_y), Vec2::splat(side)).translate(shift);
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
            super::shell::cover_vgradient(ui.painter(), art, cr, radius, theme::shade(from), theme::shade(to));
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
    let cover = named(ui.interact(hit, ui.scope_id().with(("cover-playback", &track.id)), Sense::click()), if app.playing() { crate::tr!("ui-pause") } else { crate::tr!("ui-play") });
    let on_art = !round || ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| p.distance(art.center()) <= art.width() / 2.0);
    let cover = if on_art { cover.on_hover_cursor(egui::CursorIcon::PointingHand) } else { cover };
    // Full screen has no player bar: the cover is the player there, its own buttons and dimming.
    let player = app.fullscreen && art.width() >= 280.0;
    let hover = theme::anim_bool(ui.ctx(), ("cover-playback-hover", &track.id), !player && cover.hovered() && on_art, 0.16);
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
    if player {
        cover_player(app, ui, track, art, hit, (cr, radius), round, on_art, now);
    }
    // Previous and next on the cover too, for themes that put the transport there. The bar still has them.
    let near_cover = ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| art.contains(p));
    if !player && np.cover_progress && near_cover {
        let status = app.audio.status.clone();
        let total = status.duration().map_or(0.0, |d| d.as_secs_f32());
        let pos = app.seek_drag.unwrap_or(status.position_smooth().as_secs_f32()).min(total.max(0.0));
        let cap = Rect::from_center_size(pos2(art.center().x, art.bottom() - 34.0), vec2((art.width() - 40.0).max(120.0), 34.0));
        if let Some(g) = look.app.glass.tabs {
            crate::shader::frost(ui.painter(), cap, crate::shader::Frost { look: g, radius: 17.0, opacity: 1.0, fade: 0.0 });
        }
        ui.painter().rect_filled(cap, 17.0, theme::shade(0.4));
        let font = theme::semibold(12.0);
        widgets::time_at(ui, cap.left_center() + vec2(14.0, 0.0), egui::Align2::LEFT_CENTER, &super::fmt_time(pos as u32), font.clone(), Color32::WHITE);
        widgets::time_at(ui, cap.right_center() - vec2(14.0, 0.0), egui::Align2::RIGHT_CENTER, &super::fmt_end(pos, total, app.time_left), font, Color32::WHITE);
        let line = Rect::from_center_size(cap.center(), vec2((cap.width() - 110.0).max(40.0), 4.0));
        let seek = named(ui.interact(line.expand2(vec2(0.0, 10.0)), ui.scope_id().with("cover-seek"), Sense::click()), crate::tr!("ui-seek")).on_hover_cursor(egui::CursorIcon::PointingHand);
        app.seek_wheel.hover(&seek);
        ui.painter().rect_filled(line, 2.0, theme::with_alpha(Color32::WHITE, 0.25));
        if total > 0.0 {
            ui.painter().rect_filled(Rect::from_min_size(line.min, vec2(line.width() * pos / total, 4.0)), 2.0, Color32::WHITE);
            super::ab_loop::paint_loop(app, ui, line, total, pos, 2.0, Color32::WHITE);
            super::ab_loop::loop_menu(app, ui, &seek, line, total, pos);
            if seek.clicked()
                && let Some(p) = seek.interact_pointer_pos()
            {
                app.seek_drag = None;
                app.actions.push(Action::Seek(((p.x - line.left()) / line.width()).clamp(0.0, 1.0) * total));
            }
        }
    }
    let fs_buttons = np.fullscreen_cover_buttons.filter(|_| app.fullscreen);
    if !player && (np.cover_controls || fs_buttons.is_some()) && near_cover {
        let [btn_px, btn_a, btn_grow] = fs_buttons.unwrap_or(np.cover_buttons);
        let btn_px = btn_px.max(36.0);
        for (dx, glyph, name, next) in [(-74.0, icon::SKIP_BACK, crate::tr!("ui-previous"), false), (74.0, icon::SKIP_FORWARD, crate::tr!("ui-next"), true)] {
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
    // With the theme's card (`np_content` puts it in the corner), no title here.
    if !np.shows_info_card(app.fullscreen) && np.shows_title(app.fullscreen) {
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
        let a = if nl.stays { 1.0 } else { (1.0 - ((now - since - 3.0) / theme::secs(1.0)) as f32).clamp(0.0, 1.0) };
        if a > 0.0 {
            let c = theme::with_alpha(theme::text_dim(), a * 0.8);
            let y = title_y + title_px * 1.4 + artist_px * 1.6;
            let message = if np.no_lyrics_text.is_empty() { crate::tr!("player-no-lyrics") } else { &np.no_lyrics_text };
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
    let header = header.translate(vec2(0.0, look.y_offset_full.filter(|_| app.fullscreen).unwrap_or(0.0)));
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
        (word(ui, crate::tr!("ui-song"), -36.0, !picked), word(ui, crate::tr!("ui-video"), 34.0, picked))
    } else {
        (widgets::chip(&mut row, crate::tr!("ui-song"), !picked).clicked(), widgets::chip(&mut row, crate::tr!("ui-video"), picked).clicked())
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
    let queue_w = if side && content.width() > 900.0 && !portrait_layout(ui.ctx(), content) { 360.0 } else { 0.0 };
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
    let cover = named(ui.interact(picture, ui.scope_id().with(("video-playback", &track.id)), Sense::click()), if app.playing() { crate::tr!("ui-pause") } else { crate::tr!("ui-play") }).on_hover_cursor(egui::CursorIcon::PointingHand);
    let hover = theme::anim_bool(ui.ctx(), ("video-playback-hover", &track.id), cover.hovered(), 0.16);
    if hover > 0.0 {
        ui.painter().circle_filled(picture.center(), 38.0, theme::shade(0.58 * hover));
        widgets::icon_at(ui, picture.center(), if app.playing() || app.buffering { icon::PAUSE } else { icon::PLAY }, 37.0, theme::with_alpha(Color32::WHITE, hover));
    }
    if cover.double_clicked() {
        // The first of the two clicks paused it, or played it: back as it was.
        app.actions.push(Action::TogglePlay);
        app.set_video_fullscreen(ui.ctx(), true);
    } else if cover.clicked() {
        app.actions.push(Action::TogglePlay);
    }
    // Picture in picture at the video's top right while it's pointed at, full screen at
    // its bottom right.
    if ui.rect_contains_pointer(picture) || hover > 0.0 {
        let over = if ui.rect_contains_pointer(picture) { 1.0 } else { hover };
        let pip = pos2(picture.right() - 30.0, picture.top() + 30.0);
        if corner_button(ui, pip, icon::PICTURE_IN_PICTURE, &crate::tr!("player-with-shortcut", action = crate::tr!("player-pip"), key = "Ctrl+M"), over).clicked() {
            app.actions.push(Action::Mini(true));
        }
        let full = pos2(picture.right() - 30.0, picture.bottom() - 30.0);
        if corner_button(ui, full, icon::EXPAND, &crate::tr!("player-with-shortcut", action = crate::tr!("player-full-screen"), key = "F"), over).clicked() {
            app.set_video_fullscreen(ui.ctx(), true);
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

/// A round button over the video, as bright as `shown` (the pointer's on the video).
fn corner_button(ui: &Ui, c: egui::Pos2, glyph: char, name: &str, shown: f32) -> egui::Response {
    let b = Rect::from_center_size(c, Vec2::splat(40.0));
    let resp = named(ui.interact(b, ui.scope_id().with(("video-corner", name)), Sense::click()), name).on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand);
    let shown = if resp.hovered() { 1.0 } else { shown };
    ui.painter().circle_filled(c, 20.0, theme::shade(if resp.hovered() { 0.75 } else { 0.5 } * shown));
    widgets::icon_at(ui, c, glyph, 18.0, theme::with_alpha(Color32::WHITE, shown));
    resp
}

/// Since when the pointer has been still over the full-screen video.
fn still_since_id() -> egui::Id {
    egui::Id::unique("video-full-still-since")
}

/// Going into the video's full screen: its controls come up at first.
pub(super) fn wake_video_controls(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<f64>(still_since_id()));
}

/// How far into `track` it's playing, a seek being dragged counted, and its length, in seconds.
fn song_times(app: &App, track: &crate::model::Track) -> (f32, f32) {
    let status = app.audio.status.clone();
    let total = status.duration().map(|d| d.as_secs_f32()).or(track.duration.map(|d| d as f32)).unwrap_or(0.0);
    (app.seek_drag.unwrap_or(status.position_smooth().as_secs_f32()).min(total.max(0.0)), total)
}

/// A seek bar on dark, its line through the middle of `hit`, filled in `fill`, thickening
/// with a knob and the time under the pointer above it while pointed at or dragged.
fn seek_line(app: &mut App, ui: &mut Ui, hit: Rect, id: egui::Id, (pos, total): (f32, f32), fill: Color32) -> egui::Response {
    let line_y = hit.center().y;
    let seek = ui.interact(hit, id, Sense::click_and_drag());
    let seek = if total > 0.0 { seek.on_hover_cursor(egui::CursorIcon::PointingHand) } else { seek };
    seek.widget_info(|| egui::WidgetInfo::slider(total > 0.0, pos as f64, crate::tr!("ui-seek")));
    app.seek_wheel.hover(&seek);
    let lit = theme::anim_bool(ui.ctx(), id.with("h"), seek.hovered() || seek.dragged(), 0.15);
    let thick = 3.0 + 2.0 * lit;
    let track_r = Rect::from_center_size(pos2(hit.center().x, line_y), vec2(hit.width(), thick));
    ui.painter().rect_filled(track_r, thick / 2.0, theme::with_alpha(Color32::WHITE, 0.25));
    let frac = if total > 0.0 { pos / total } else { 0.0 };
    let filled = Rect::from_min_size(track_r.min, vec2(track_r.width() * frac, thick));
    ui.painter().rect_filled(filled, thick / 2.0, fill);
    super::ab_loop::paint_loop(app, ui, track_r, total, pos, thick / 2.0, Color32::WHITE);
    super::ab_loop::loop_menu(app, ui, &seek, track_r, total, pos);
    if lit > 0.0 && total > 0.0 {
        ui.painter().circle_filled(pos2(filled.right(), line_y), 6.5 * lit, fill);
        if let Some(p) = seek.hover_pos() {
            let t = ((p.x - track_r.left()) / track_r.width()).clamp(0.0, 1.0) * total;
            let g = widgets::time_galley(ui, &super::fmt_time(t as u32), theme::semibold(12.0), Color32::WHITE);
            let tip = Rect::from_center_size(pos2(p.x, line_y - 22.0), g.size() + vec2(12.0, 6.0));
            ui.painter().rect_filled(tip, 6.0, theme::shade(0.75));
            ui.painter().galley(tip.min + vec2(6.0, 3.0), g, Color32::WHITE);
        }
    }
    if total > 0.0
        && let Some(p) = seek.interact_pointer_pos()
    {
        let t = ((p.x - track_r.left()) / track_r.width()).clamp(0.0, 1.0) * total;
        // Dragged with the left button: a right-click held or moved a little is the menu's.
        if seek.dragged_by(egui::PointerButton::Primary) {
            app.seek_drag = Some(t);
        }
        if seek.drag_stopped_by(egui::PointerButton::Primary) || seek.clicked() {
            app.seek_drag = None;
            app.actions.push(Action::Seek(t));
        }
    }
    seek
}

/// Full screen with just the video, on black, as on YouTube: the song's title and the
/// controls come up while the pointer moves, and go, the pointer with them, once it has
/// been still a moment away from them. A click plays or pauses, a double-click leaves.
fn video_fullscreen(app: &mut App, ui: &mut Ui, rect: Rect, track: &crate::model::Track, now: f64) {
    let ctx = ui.ctx().clone();
    ui.painter().rect_filled(rect, 0.0, Color32::BLACK);
    let frame = video_frame(app, &ctx, track);
    let (w, h) = frame.as_ref().map_or((16.0, 9.0), |t| (t.size()[0] as f32, t.size()[1] as f32));
    // Not while typing: into the command palette, say.
    if !ctx.egui_wants_keyboard_input() && ui.input(|i| i.key_pressed(egui::Key::Z) && !i.modifiers.any()) {
        set_video_fill(app, !app.video_fill);
    }
    // Filling: the picture inside its own bars, as large as covers the screen, its edges cut.
    let content = if app.video_fill { app.video_content.unwrap_or(Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))) } else { Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)) };
    let (cw, ch) = ((w * content.width()).max(1.0), (h * content.height()).max(1.0));
    let scale = if app.video_fill { (rect.width() / cw).max(rect.height() / ch) } else { (rect.width() / cw).min(rect.height() / ch) };
    let uv = Rect::from_center_size(content.center(), vec2((rect.width() / scale / w).min(content.width()), (rect.height() / scale / h).min(content.height())));
    let picture = Rect::from_center_size(rect.center(), vec2(uv.width() * w * scale, uv.height() * h * scale));
    match &frame {
        Some(tex) => {
            ui.painter().image(tex.id(), picture, uv, Color32::WHITE);
        }
        None => video_placeholder(ui, picture, track, 0.0),
    }
    let stage = named(ui.interact(rect, ui.scope_id().with("video-full-stage"), Sense::click()), if app.playing() { crate::tr!("ui-pause") } else { crate::tr!("ui-play") });
    if stage.double_clicked() {
        // The first of the two clicks paused it, or played it: back as it was.
        app.actions.push(Action::TogglePlay);
        app.set_fullscreen(&ctx, false);
        return;
    } else if stage.clicked() {
        app.actions.push(Action::TogglePlay);
    }
    let bar_h = 104.0;
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let on_controls = pointer.is_some_and(|p| p.y >= rect.bottom() - bar_h || p.y <= rect.top() + 88.0);
    let moved = ui.input(|i| i.pointer.delta() != Vec2::ZERO || !i.events.is_empty());
    let since = ctx.data(|d| d.get_temp::<f64>(still_since_id()));
    let since = match since {
        Some(t) if !moved => t,
        _ => {
            ctx.data_mut(|d| d.insert_temp(still_since_id(), now));
            now
        }
    };
    let busy = on_controls || app.seek_drag.is_some() || egui::Popup::is_any_open(&ctx) || !app.playing();
    let quiet = !busy && now - since > 2.5;
    let shown = theme::anim_bool(&ctx, "video-full-controls", !quiet, 0.25);
    if !quiet {
        // To see the pointer has been still long enough.
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    } else if pointer.is_some() {
        ctx.set_cursor_icon(egui::CursorIcon::None);
    }
    if shown <= 0.02 {
        return;
    }
    let painter = ui.painter();
    let top = Rect::from_min_size(rect.min, vec2(rect.width(), 120.0));
    super::shell::vgradient(painter, top, theme::shade(0.6 * shown), Color32::TRANSPARENT);
    let bottom = Rect::from_min_max(pos2(rect.left(), rect.bottom() - 160.0), rect.max);
    super::shell::vgradient(painter, bottom, Color32::TRANSPARENT, theme::shade(0.75 * shown));
    let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    cui.multiply_opacity(shown);
    let ui = &mut cui;
    let text_w = rect.width() - 56.0;
    widgets::title_at(ui, pos2(rect.left() + 28.0, rect.top() + 22.0), &track.title, track.explicit, theme::bold(20.0), Color32::WHITE, text_w);
    let line = track.artist_line();
    widgets::linked_text(ui, pos2(rect.left() + 28.0, rect.top() + 50.0), &line, &track.artists, theme::regular(15.0), theme::text_dim(), text_w, &mut app.actions);

    let (pos, total) = song_times(app, track);
    let line_y = rect.bottom() - 66.0;
    let hit = Rect::from_min_max(pos2(rect.left() + 20.0, line_y - 8.0), pos2(rect.right() - 20.0, line_y + 8.0));
    let accent = app.accent_color();
    seek_line(app, ui, hit, ui.scope_id().with("video-full-seek"), (pos, total), accent);

    // The buttons under it: playing and the volume on the left, the rest on the right.
    let row = Rect::from_min_max(pos2(rect.left() + 12.0, line_y + 10.0), pos2(rect.right() - 12.0, rect.bottom() - 6.0));
    let mut left = ui.new_child(egui::UiBuilder::new().max_rect(row).layout(egui::Layout::left_to_right(egui::Align::Center)));
    left.spacing_mut().item_spacing.x = 2.0;
    if icon_button(&mut left, icon::SKIP_BACK, 20.0, crate::tr!("ui-previous"), false).clicked() {
        app.prev();
    }
    let playing = app.playing() || app.buffering;
    if icon_button(&mut left, if playing { icon::PAUSE } else { icon::PLAY }, 24.0, if playing { crate::tr!("ui-pause") } else { crate::tr!("ui-play") }, false).clicked() {
        app.actions.push(Action::TogglePlay);
    }
    if icon_button(&mut left, icon::SKIP_FORWARD, 20.0, crate::tr!("ui-next"), false).clicked() {
        app.next();
    }
    let vol = app.audio.volume();
    let vglyph = if vol == 0.0 { icon::VOLUME_X } else if vol < 0.5 { icon::VOLUME_1 } else { icon::VOLUME_2 };
    if icon_button(&mut left, vglyph, 19.0, if vol > 0.0 { crate::tr!("ui-mute") } else { crate::tr!("ui-unmute") }, false).clicked() {
        super::mini::toggle_mute(app, &left);
    }
    let (slider, _) = left.allocate_exact_size(vec2(90.0, 20.0), Sense::hover());
    super::shell::volume_slider(app, &mut left, slider, "video-full-vol");
    left.add_space(14.0);
    if total > 0.0 {
        let g = widgets::time_galley(&left, &super::fmt_running(pos, total, app.time_left), theme::regular(13.5), Color32::WHITE);
        // Room for the widest it gets (all the time left), so a second click doesn't land on
        // the video (pause).
        let w = widgets::time_galley(&left, &super::fmt_running(0.0, total, true), theme::regular(13.5), Color32::WHITE).size().x;
        let (r, _) = left.allocate_exact_size(vec2(w, g.size().y), Sense::hover());
        widgets::time_toggle(&left, r, r.min, g, [Color32::WHITE; 2], &mut app.time_left);
    }
    let mut right = ui.new_child(egui::UiBuilder::new().max_rect(row).layout(egui::Layout::right_to_left(egui::Align::Center)));
    right.spacing_mut().item_spacing.x = 2.0;
    if icon_button(&mut right, icon::SHRINK, 20.0, &crate::tr!("player-with-shortcut", action = crate::tr!("player-exit-full-screen"), key = crate::tr!("settings-esc")), false).clicked() {
        app.set_fullscreen(&ctx, false);
    }
    if icon_button(&mut right, icon::PICTURE_IN_PICTURE, 19.0, &crate::tr!("player-with-shortcut", action = crate::tr!("player-pip"), key = "Ctrl+M"), false).clicked() {
        app.actions.push(Action::Mini(true));
    }
    let fill_action = if app.video_fill { crate::tr!("player-fit-video") } else { crate::tr!("player-fill-video") };
    let fill_tip = crate::tr!("player-with-shortcut", action = fill_action, key = "Z");
    if icon_button(&mut right, icon::MAXIMIZE_2, 18.0, &fill_tip, app.video_fill).clicked() {
        set_video_fill(app, !app.video_fill);
    }
    right.add_space(6.0);
    // What it plays at: a video may not come as sharp as asked.
    let playing_at = app.video.as_ref().and_then(|(_, p)| p.as_ref()).map(crate::video::Player::height).filter(|&h| h > 0);
    quality_button(app, &mut right, playing_at);
}

/// Full screen fills the screen with the video, or fits it whole. Looks for its bars afresh.
fn set_video_fill(app: &mut App, on: bool) {
    app.video_fill = on;
    app.video_content = None;
    app.video_content_at = 0.0;
}

/// The video's quality, a menu of the others above it.
fn quality_button(app: &mut App, ui: &mut Ui, playing_at: Option<u32>) {
    let label = format!("{}p", playing_at.unwrap_or(app.video_quality));
    let g = widgets::galley(ui, &label, theme::semibold(13.0), Color32::WHITE, 80.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 44.0, 32.0), Sense::click());
    let resp = named(resp, crate::tr!("settings-video-quality")).on_hover_text(crate::tr!("settings-video-quality")).on_hover_cursor(egui::CursorIcon::PointingHand);
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&resp));
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() || open, 0.12);
    ui.painter().rect_filled(rect, 16.0, theme::glass(0.10 + 0.08 * h));
    widgets::icon_at(ui, pos2(rect.left() + 17.0, rect.center().y), icon::SETTINGS, 15.0, Color32::WHITE);
    ui.painter().galley(pos2(rect.left() + 30.0, rect.center().y - g.size().y / 2.0), g, Color32::WHITE);
    widgets::focus_ring(ui, &resp, rect, 16.0);
    widgets::menu_popup(&resp).align(egui::RectAlign::TOP_END).show(|ui| {
        widgets::menu_body(ui, 170.0);
        quality_rows(app, ui);
    });
}

/// The video qualities to pick from, in a menu. Settings › Music videos has the same.
fn quality_rows(app: &mut App, ui: &mut Ui) {
    menu_caption(ui, crate::tr!("settings-video-quality"));
    for q in super::VIDEO_QUALITIES {
        if super::pages::option_row(ui, &format!("{q}p"), q == app.video_quality).clicked() {
            set_video_quality(app, q);
            ui.close();
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
    let pill = |size: Vec2| {
        let r = Rect::from_center_size(pos2(picture.center().x, picture.bottom() - 34.0), size + vec2(24.0, 12.0));
        if let Some(look) = app.look().app.glass.resume {
            crate::shader::frost(ui.painter(), r, crate::shader::Frost { look, radius: r.height() / 2.0, opacity: 1.0, fade: 0.0 });
        }
        ui.painter().rect_filled(r, r.height() / 2.0, theme::shade(0.45));
        r.center() - size / 2.0
    };
    if let Some(line) = lyrics.lines.iter().find(|l| !l.instrumental && l.start <= t && t < l.start + l.dur) {
        let g = widgets::galley(ui, &line.text, theme::semibold(15.0), Color32::WHITE, picture.width() - 64.0);
        ui.painter().galley(pill(g.size()), g, Color32::WHITE);
        return;
    }
    // After the last line, Unison's credit, as the lyrics panel has under it.
    let Some(c) = &lyrics.unison else { return };
    if !lyrics.lines.iter().rev().find(|l| !l.instrumental).is_some_and(|l| t >= l.end()) {
        return;
    }
    let parts = karaoke::unison_parts(c);
    let (color, dim) = (theme::with_alpha(Color32::WHITE, 0.85), theme::with_alpha(Color32::WHITE, 0.55));
    let parts: Vec<_> = parts.into_iter().map(|(text, url)| (widgets::galley(ui, &text, theme::regular(11.0), color, f32::INFINITY), url)).collect();
    let mut at = pill(vec2(parts.iter().map(|(g, _)| g.size().x).sum(), parts[0].0.size().y));
    for (n, (g, url)) in parts.into_iter().enumerate() {
        let r = Rect::from_min_size(at, g.size());
        at.x += g.size().x;
        let mut tint = dim;
        if let Some(url) = url {
            let resp = ui.interact(r, ui.scope_id().with(("pill-unison", n)), Sense::click());
            // Announced as the link it is, as the lyrics panel's are.
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Link, true, g.text()));
            if resp.hovered() {
                tint = color;
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                ui.painter().hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0, color));
            }
            if resp.clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
            }
        }
        ui.painter().galley_with_override_text_color(r.min, g, tint);
    }
}

/// A new frame into a texture that's shown frame after frame. The same size, it's written
/// into the texture's storage; `set` would have the GPU driver make that anew for every
/// frame, which some keep the old storage of while frames in flight use it (#107, #324).
fn overwrite(tex: &mut egui::TextureHandle, image: egui::ColorImage) {
    if tex.size() == image.size {
        tex.set_partial([0, 0], image, egui::TextureOptions::LINEAR);
    } else {
        tex.set(image, egui::TextureOptions::LINEAR);
    }
}

/// The video's colours, eased toward the latest frame's, across all of Now playing.
fn paint_glow(app: &mut App, ui: &mut Ui, rect: Rect, clip: Rect) {
    let dim_bright = app.dim_bright;
    let Some(Glow { current, target, tex }) = &mut app.glow else { return };
    let k = (ui.input(|i| i.stable_dt) * 2.5).clamp(0.0, 1.0);
    for (c, t) in current.iter_mut().zip(target.iter()) {
        for i in 0..3 {
            c[i] += (t[i] - c[i]) * k;
        }
    }
    let [w, h] = crate::video::GLOW_SIZE;
    let pixels = current.iter().map(|c| Color32::from_rgb(c[0] as u8, c[1] as u8, c[2] as u8)).collect();
    overwrite(tex, egui::ColorImage { size: [w, h], source_size: vec2(w as f32, h as f32), pixels });
    // Bigger than the view, so its coarse edges stay out of sight.
    let full = rect.expand2(rect.size() * 0.08);
    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    let painter = ui.painter_at(clip);
    // Drawn at 150/255 under a quarter's shade; darker still for a bright picture.
    const LEVEL: f32 = 150.0 / 255.0 * 0.75;
    let readable = if dim_bright && !current.is_empty() {
        let luma = current.iter().map(|c| (0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]) / 255.0).sum::<f32>() / current.len() as f32;
        crate::art::readable_dim(luma * LEVEL)
    } else {
        1.0
    };
    painter.image(tex.id(), full, uv, Color32::from_gray((150.0 * readable) as u8));
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

impl CoverClip {
    /// A frame of it is up.
    pub fn showing(&self) -> bool {
        self.frame.is_some()
    }
}

/// The animated cover's latest frame, and how far it has faded in. Keeps the clip in step
/// with the song.
fn cover_clip(app: &mut App, ctx: &egui::Context, track: &crate::model::Track, px: u32, now: f64) -> Option<(egui::TextureHandle, f32)> {
    if !app.animated_covers {
        app.drop_cover_clip(now);
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
    // The clip is what's on screen, and a song's single and album covers can differ: its
    // first frame sets the colours (kept for the song), crossfading from the still cover's.
    // They stay put while it plays: each theme decides whether its background moves.
    let mut recolour = false;
    if let Some(f) = player.take_due(now, playing) {
        if c.frame.is_none() {
            if app.clip_art.as_ref().is_none_or(|(id, _)| *id != c.id)
                && let Some(mut art) = crate::art::from_cover_frame(&f.image)
            {
                crate::art::soften(&mut art, crate::preset::active().background.blur_passes.saturating_sub(6));
                app.clip_art = Some((c.id.clone(), art));
            }
            recolour = app.clip_art.as_ref().is_some_and(|(id, _)| *id == c.id);
        }
        match &mut c.frame {
            Some((tex, _)) => overwrite(tex, f.image),
            None => c.frame = Some((ctx.load_texture("cover-clip", f.image, egui::TextureOptions::LINEAR), now)),
        }
    }
    let shown = c.frame.as_ref().map_or(0.0, |(_, at)| ((now - at) / theme::secs(0.32)).clamp(0.0, 1.0)) as f32;
    // Now playing runs at 30 fps while the music plays; paused, only the first frame and
    // its fade need drawing.
    if c.frame.is_none() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    } else if shown < 1.0 {
        super::pace::repaint_after(ctx, std::time::Duration::from_millis(16));
    }
    let out = c.frame.as_ref().map(|(tex, _)| (tex.clone(), shown));
    if recolour {
        app.show_cover_colours(now);
    }
    out
}

/// Play videos at most `height` tall, the one on show too: it carries on from where the
/// song is, at the new quality, once that has loaded.
pub(super) fn set_video_quality(app: &mut App, height: u32) {
    if app.video_quality == height {
        return;
    }
    app.video_quality = height;
    if let Some((id, player)) = &mut app.video {
        *player = None;
        app.backend.request(crate::backend::Req::VideoStream { video_id: id.clone(), max_height: height });
    }
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
        app.video_content = None;
        app.glow = None;
        app.backend.request(crate::backend::Req::VideoStream { video_id: track.id.clone(), max_height: app.video_quality });
    }
    // On its way at another quality: the last picture stays up meanwhile.
    let Some(player) = app.video.as_ref()?.1.as_ref() else { return app.video_frame.clone() };
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
                if v.auto_dim.is_some() || app.dim_bright {
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
        // Where its own black bars are, for filling the screen: a few times a second, the
        // picture widening as brighter frames show more of it.
        let now = ctx.input(|i| i.time);
        if app.video_fill && app.video_full && now - app.video_content_at > 0.5 {
            app.video_content_at = now;
            if let Some(seen) = crate::video::content_uv(&frame.image) {
                app.video_content = Some(app.video_content.map_or(seen, |c| c.union(seen)));
            }
        }
        match &mut app.video_frame {
            Some(tex) => overwrite(tex, frame.image),
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
            ui.label(egui::RichText::new(crate::tr!("player-loading-lyrics")).color(theme::text_dim()));
        });
        return;
    };
    let hide_break_only = app.lyric_prefs.style(app.look()).hide_instrumental_only;
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
            ui.label(egui::RichText::new(crate::tr!("player-no-lyrics")).font(theme::semibold(20.0)).color(theme::text_dim()));
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
    let style = app.lyric_prefs.style(app.look());
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
        lyrics.unison.hash(&mut h);
        app.theme_id.hash(&mut h);
        app.lyric_prefs.look.hash(&mut h);
                for r in &roman {
            match r {
                Some(karaoke::Roman::Timed(_, s)) | Some(karaoke::Roman::Text(s)) => s.hash(&mut h),
                None => 0u8.hash(&mut h),
            }
        }
        trans.hash(&mut h);
        h.finish()
    };
    let own = karaoke::Own { scale: app.lyric_prefs.text_scale, weight: app.lyric_prefs.weight, deco_scale: app.lyric_prefs.deco_scale, size: None, anchor: None, padding: None, ink: None, follow: false, active_row: false };
    let fullscreen = app.fullscreen;
    let glass_chip = app.look().app.glass.instrumental;
    let quick_scroll = app.quick_lyric_scroll;
    let frame = karaoke::Frame { lyrics, key, roman, trans, t, playing, now, rect, style, accent, centered, own, fullscreen, glass_chip, quick_scroll, soft: false };
    // The view lives in `app`; the frame borrows `app` for decorations, so swap it out.
    let mut view = std::mem::take(&mut app.lyric_view);
    let out = if karaoke::dev_off("view") { karaoke::Out { seek: None, animating: true, user_scrolling: false, active_h: 0.0 } } else { view.show(ui, frame) };
    app.lyric_view = view;
    if let Some(s) = out.seek {
        // The line's time on the player's clock: `t` above, the other way round.
        let at = s + app.lyric_prefs.offset(id, lyrics.provider, lyrics.sync == Sync::Word) + app.audio.status.latency().as_secs_f64();
        app.actions.push(Action::Seek(at.max(0.0) as f32));
    }
    if out.animating && !app.window_hidden {
        if app.lyrics_full_rate {
            // As fast as the screen shows them: vsync, or `pace::frame`, sets the rate.
            app.full_rate_frame = true;
            ui.ctx().request_repaint();
        } else {
            // Smooth, without following a fast monitor: frame rate is the main CPU and GPU cost.
            super::pace::repaint_after(ui.ctx(), std::time::Duration::from_millis(16));
        }
    }
    // The extension's `.autoscroll-resume-button` (YouTube Music's rem is 10 px): a frosted
    // pill under the top edge in the lyrics font at 600, 1.75rem; padding .5rem 1rem;
    // white 5% fill, 1px white 10% border, radius 1rem, over a 5px backdrop blur (which
    // reads as about twice those on screen). It slides down from -3em to 2em as it fades
    // in, and its text glows on hover.
    let shown = theme::anim_bool(ui.ctx(), "lyr-resume", out.user_scrolling, 0.2);
    if shown > 0.0 {
        let font = theme::lyric_font(&style.font, crate::preset::Weight::Semibold, style.resume_text_px.unwrap_or(17.5));
        let g = widgets::galley(ui, crate::tr!("player-resume-autoscroll"), font, Color32::WHITE, rect.width() - 40.0);
        let size = g.size() + vec2(20.0, 10.0) + Vec2::splat(2.0);
        let em = 17.5;
        let y = rect.top() + 8.0 + egui::lerp(-3.0 * em..=0.5 * em, theme::ease_out(shown));
        let size = vec2(size.x, style.resume_height.unwrap_or(size.y));
        let r = match style.resume_fullscreen.filter(|_| fullscreen) {
            Some([left, down]) => {
                let y = egui::lerp(rect.top() - 3.0 * em..=rect.top() + down * rect.height(), theme::ease_out(shown));
                Rect::from_min_size(pos2((rect.left() + left).min(rect.right() - size.x), y), size)
            }
            None => Rect::from_min_size(pos2(rect.center().x - size.x / 2.0, y), size),
        };
        let resp = ui.interact(r, ui.scope_id().with("lyr-resume-btn"), if out.user_scrolling { Sense::click() } else { Sense::hover() });
        let resp = named(resp, crate::tr!("player-resume-autoscroll")).on_hover_cursor(egui::CursorIcon::PointingHand);
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
        ui.label(egui::RichText::new(crate::tr!("player-lyrics-unsynced")).font(theme::regular(13.0)).color(theme::text_faint()));
        ui.add_space(14.0);
        let style = app.lyric_prefs.style(app.look());
        let size = style.plain_size().min(rect.width() / 14.0).max(18.0);
        for l in &lyrics.lines {
            let font = theme::lyric_font_for(&style.font, app.lyric_prefs.weight.unwrap_or(style.weight), size, &l.text);
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
        if let Some(c) = &lyrics.unison {
            ui.add_space(14.0);
            unison_credit(ui, c);
        }
        ui.add_space(80.0);
    });
}

/// Unison's credit under its unsynced lyrics, as the synced view has it: "Lyrics from
/// Unison" linked to the song there, and who submitted them linked to their page.
fn unison_credit(ui: &mut Ui, c: &crate::lyrics::UnisonCredit) {
    let text = |t: String| egui::RichText::new(t).font(theme::regular(11.5)).color(theme::text_faint());
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (part, url) in karaoke::unison_parts(c) {
            match url {
                Some(url) => ui.add(egui::Hyperlink::from_label_and_url(text(part), url)),
                None => ui.label(text(part)),
            };
        }
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
        SyncKind::Syllable => crate::tr!("player-sync-syllable"),
        SyncKind::Word => crate::tr!("player-sync-word"),
        SyncKind::Line => crate::tr!("player-sync-line"),
        SyncKind::Unsynced => crate::tr!("player-sync-none"),
    }
}

/// Now playing's one options button, right of the chips: the song's actions, and with
/// lyrics showing, the source picker, translation and romanization toggles, and this
/// song's timing offset among them, so the header stays clean until you want them. A
/// round button, or one that fits the window buttons' notch (`compact`). Whether it's open.
fn options_menu(app: &mut App, ui: &mut Ui, track: &crate::model::Track, lyrics: bool, compact: bool) -> bool {
    let size = if compact { super::titlebar::NOTCH_BUTTON } else { vec2(34.0, 34.0) };
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let resp = named(resp, crate::tr!("ui-options")).on_hover_cursor(egui::CursorIcon::PointingHand);
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&resp));
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() || open, 0.12);
    if compact {
        super::titlebar::notch_highlight(ui, rect, h, resp.is_pointer_button_down_on() || open);
        widgets::focus_ring(ui, &resp, rect, 7.0);
    } else {
        ui.painter().circle_filled(rect.center(), 17.0, theme::glass(0.09 + 0.07 * h));
    }
    let (gap, dot) = if compact { (5.0, 1.5) } else { (6.0, 1.9) };
    for dx in [-gap, 0.0, gap] {
        ui.painter().circle_filled(rect.center() + vec2(dx, 0.0), dot, theme::with_alpha(Color32::WHITE, 0.75 + 0.25 * h));
    }
    widgets::menu_popup(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        // Kept short: the song's first actions as icons, the lyrics sources behind one row.
        widgets::menu_body(ui, 256.0);
        let liked = app.heard_liked();
        let asked = app.actions.len();
        widgets::song_action_icons(ui, track, liked, &mut app.actions);
        if let Some(index) = app.actions[asked..].iter().position(|action| matches!(action, Action::Like(..)))
            && let Some(rating) = app.heard_like_action(!liked)
        {
            app.actions[asked + index] = rating;
        }
        menu_divider(ui);
        if lyrics {
            lyric_options(app, ui);
            menu_divider(ui);
        }
        if app.shown_video().is_some() {
            quality_rows(app, ui);
            menu_divider(ui);
        }
        // Not in a jam: pausing would pause everyone in it.
        if !app.jam.active() {
            sleep_row(app, ui);
            menu_divider(ui);
        }
        widgets::song_actions_rest(ui, track, &mut app.actions);
    });
    open
}

/// The sleep timer on one row, with what's left on it; the choices open beside the menu.
fn sleep_row(app: &mut App, ui: &mut Ui) {
    let minutes = |m: u64| crate::tr!("player-sleep-minutes", minutes = m);
    let left = match app.sleep {
        None => crate::tr!("player-sleep-off").to_owned(),
        Some(Sleep::SongEnd) => crate::tr!("player-sleep-song-end").to_owned(),
        Some(Sleep::At(at)) => minutes(at.saturating_duration_since(Instant::now()).as_secs().div_ceil(60)),
    };
    let mut pick = None;
    submenu_row(ui, crate::tr!("player-sleep-timer"), &left, None, true, |ui| {
        widgets::menu_body(ui, 200.0);
        if widgets::menu_choice(ui, crate::tr!("player-sleep-off"), app.sleep.is_none()) {
            pick = Some(None);
        }
        for m in [15, 30, 45, 60] {
            if widgets::menu_choice(ui, &minutes(m), false) {
                pick = Some(Some(Sleep::At(Instant::now() + Duration::from_secs(m * 60))));
            }
        }
        if widgets::menu_choice(ui, crate::tr!("player-sleep-song-end"), app.sleep == Some(Sleep::SongEnd)) {
            pick = Some(Some(Sleep::SongEnd));
        }
    });
    if let Some(sleep) = pick {
        app.set_sleep(sleep);
    }
}

fn lyric_options(app: &mut App, ui: &mut Ui) {
    let Some((id, set)) = app.lyrics.as_ref() else { return };
    let id = id.clone();
    let available = app.lyric_prefs.available(set);
    let Some(shown) = app.lyric_prefs.choose(&id, set) else { return };
    let n = available.len();
    let mut pick = None;
    // One row, the source shown on it; the others open beside the menu.
    let label = if n > 1 { crate::tr!("player-lyrics-source") } else { crate::tr!("player-only-lyrics-source") };
    submenu_row(ui, label, shown.name(), Some(sync_color(shown.sync())), n > 1, |ui| {
        widgets::menu_body(ui, 300.0);
        for p in &available {
            if source_row(ui, p.name(), p.sync(), *p == shown) {
                pick = Some(*p);
                ui.close();
            }
        }
    });
    let name = crate::i18n::lyrics_name(&app.lyric_prefs.target);
    let tip = crate::tr!("player-translation-language", language = name.as_ref());
    switch_row(ui, &tip, &mut app.show_trans);
    switch_row(ui, crate::tr!("settings-romanization"), &mut app.show_roman);
    // This song's timing for the shown source: ‹ +0.0s ›
    let off = app.lyric_prefs.song_offset(&id, shown);
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::hover());
    ui.painter().text(pos2(row.left() + 12.0, row.center().y), egui::Align2::LEFT_CENTER, crate::tr!("player-timing"), theme::regular(14.0), theme::text());
    let mut sub = ui.new_child(egui::UiBuilder::new().max_rect(row.shrink2(vec2(8.0, 0.0))).layout(egui::Layout::right_to_left(egui::Align::Center)));
    sub.spacing_mut().item_spacing.x = 2.0;
    if mini_icon(&mut sub, icon::CHEVRON_RIGHT, crate::tr!("player-lyrics-later"), true).clicked() {
        app.lyric_prefs.set_song_offset(&id, shown, off + 0.1);
    }
    let label = format!("{off:+.1}s").replace("+0.0", "0.0");
    if mini_text(&mut sub, &label, false, crate::tr!("player-timing-reset")).double_clicked() {
        app.lyric_prefs.set_song_offset(&id, shown, 0.0);
    }
    if mini_icon(&mut sub, icon::CHEVRON_LEFT, crate::tr!("player-lyrics-earlier"), true).clicked() {
        app.lyric_prefs.set_song_offset(&id, shown, off - 0.1);
    }
    if let Some(p) = pick.filter(|p| *p != shown) {
        app.lyric_prefs.pick(&id, set, p);
        crate::downloads::lyrics_picked(&id);
        log::info!("lyrics: showing {} ({}/{}) by choice", p.key(), available.iter().position(|x| *x == p).map_or(0, |i| i + 1), n);
        app.lyric_view.reset();
    }
}

/// A row naming something and what it's at (`value`, after `dot` when there is one); when
/// `opens`, an arrow, and `contents` beside the menu on hover or click.
fn submenu_row(ui: &mut Ui, label: &str, value: &str, dot: Option<Color32>, opens: bool, contents: impl FnOnce(&mut Ui)) {
    let (row, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), if opens { Sense::click() } else { Sense::hover() });
    let resp = named(resp, label);
    let open = egui::Popup::is_id_open(ui.ctx(), egui::containers::menu::SubMenu::id_from_widget_id(resp.id));
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), opens && (resp.hovered() || open), 0.12);
    if h > 0.0 {
        ui.painter().rect_filled(row, theme::menu_item_radius(), theme::glass(0.08 * h));
    }
    ui.painter().text(pos2(row.left() + 12.0, row.center().y), egui::Align2::LEFT_CENTER, label, theme::regular(14.0), theme::text());
    let arrow = if opens { 22.0 } else { 0.0 };
    let value_g = widgets::galley(ui, value, theme::regular(13.0), theme::text_dim(), row.width() * 0.5);
    let value_x = row.right() - 12.0 - arrow - value_g.size().x;
    if let Some(c) = dot {
        ui.painter().circle_filled(pos2(value_x - 9.0, row.center().y), 3.0, c);
    }
    let value_pos = pos2(value_x, row.center().y - value_g.size().y / 2.0);
    widgets::galley_tipped(ui, value_pos, value_g, theme::text_dim(), value);
    if opens {
        widgets::icon_at(ui, pos2(row.right() - 18.0, row.center().y), icon::CHEVRON_RIGHT, 14.0, theme::text_dim());
        egui::containers::menu::SubMenu::new().show(ui, &resp, contents);
    }
}

pub(super) fn menu_caption(ui: &mut Ui, text: &str) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    ui.painter().text(pos2(r.left() + 12.0, r.center().y + 2.0), egui::Align2::LEFT_CENTER, text.to_uppercase(), theme::semibold(11.0), theme::text_faint());
}

/// A lyrics source on one line: its sync type's dot, name and sync type, with a check
/// on the one shown.
fn source_row(ui: &mut Ui, name: &str, sync: SyncKind, current: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
    let resp = named(resp, &crate::tr!("player-lyrics-source-name", name = name)).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
    if h > 0.0 || current {
        ui.painter().rect_filled(rect, theme::menu_item_radius(), theme::glass(0.05 * current as u8 as f32 + 0.08 * h));
    }
    ui.painter().circle_filled(pos2(rect.left() + 16.0, rect.center().y), 3.0, sync_color(sync));
    let kind = widgets::galley(ui, sync_label(sync), theme::regular(12.0), theme::text_faint(), 90.0);
    let kind_x = rect.right() - 34.0 - kind.size().x;
    ui.painter().galley(pos2(kind_x, rect.center().y - kind.size().y / 2.0), kind, theme::text_faint());
    let x = rect.left() + 30.0;
    text_at(ui, pos2(x, rect.center().y - 9.0), name, theme::regular(13.5), theme::text(), kind_x - x - 8.0);
    if current {
        widgets::icon_at(ui, pos2(rect.right() - 16.0, rect.center().y), icon::CHECK, 14.0, theme::text());
    }
    resp.clicked()
}

/// The height of the menu's lyrics rows.
const ROW: f32 = 32.0;

/// A label with an on/off switch; the whole row toggles.
fn switch_row(ui: &mut Ui, label: &str, on: &mut bool) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
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
    let track = Rect::from_center_size(pos2(rect.right() - 27.0, rect.center().y), vec2(32.0, 18.0));
    let accent = ui.ctx().data(|d| d.get_temp::<Color32>(egui::Id::unique("accent"))).unwrap_or(theme::RED);
    ui.painter().rect_filled(track, 9.0, theme::lerp_color(theme::glass(0.16 + 0.06 * h), accent, t));
    let knob = pos2(egui::lerp(track.left() + 9.0..=track.right() - 9.0, t), track.center().y);
    ui.painter().circle_filled(knob, 7.0, Color32::WHITE);
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

#[cfg(test)]
mod tests {
    use super::{is_portrait, lyrics_panel_wanted, overwrite};
    use egui::{Vec2, vec2};

    /// Now playing's content in a window of `w`×`h` points: its padding, header and the
    /// player bar taken off. Snapped sizes leave out a 48-point taskbar.
    fn content(w: f32, h: f32) -> Vec2 {
        vec2(w - 64.0, h - 154.0)
    }

    #[test]
    fn half_a_landscape_screen_stays_side_by_side() {
        for (w, h) in [
            (960.0, 1032.0),  // 1080p
            (1280.0, 1392.0), // 1440p, and 4K at 150 %
            (1920.0, 2112.0), // 4K at 100 %
            (960.0, 1152.0),  // 1920×1200
            (1280.0, 1552.0), // 2560×1600
            (1128.0, 1456.0), // 3:2, 2256×1504
            (1920.0, 2512.0), // 3:2, 3840×2560 at 100 %
            (1536.0, 2000.0), // 3:2, 3840×2560 at 125 %
            (1720.0, 1392.0), // 3440×1440 ultrawide
            (1146.0, 1392.0), // a third of it
        ] {
            // Even coming from a taller window.
            assert!(!is_portrait(content(w, h), false) && !is_portrait(content(w, h), true), "{w}×{h}");
        }
    }

    #[test]
    fn portrait_monitors_and_tall_windows_stack() {
        for (w, h) in [
            (1080.0, 1872.0), // 1080×1920
            (864.0, 1488.0),  // 1080×1920 at 125 %
            (1440.0, 2512.0), // 1440×2560
            (1200.0, 1872.0), // 1200×1920
            (960.0, 1392.0),  // a 2880×1920 tablet stood up, at 200 %
            (960.0, 1659.0),  // 1440×2560 at 150 %
            (853.0, 1392.0),  // a third of a 1440p screen
        ] {
            assert!(is_portrait(content(w, h), false), "{w}×{h}");
        }
    }

    #[test]
    fn near_the_line_it_keeps_what_it_was() {
        let c = vec2(1000.0, 1320.0);
        assert!(is_portrait(c, true));
        assert!(!is_portrait(c, false));
    }

    /// A frame the texture's size is written into it; another size makes it anew.
    #[test]
    fn a_same_size_frame_overwrites_its_texture() {
        let ctx = egui::Context::default();
        let img = |w, h| egui::ColorImage::filled([w, h], egui::Color32::RED);
        let mut tex = ctx.load_texture("t", img(4, 4), egui::TextureOptions::LINEAR);
        let id = tex.id();
        let pos = |ctx: &egui::Context| {
            let mut delta = ctx.tex_manager().write().take_delta();
            let pos = delta.set.get(&id).and_then(|d| d.last()).map(|d| d.pos);
            delta.clear();
            pos
        };
        pos(&ctx);
        overwrite(&mut tex, img(4, 4));
        assert_eq!(pos(&ctx), Some(Some([0, 0])));
        overwrite(&mut tex, img(8, 4));
        assert_eq!(pos(&ctx), Some(None));
        assert_eq!(tex.size(), [8, 4]);
    }

    #[test]
    fn lyrics_panel_holds_state_while_loading_and_tracks_lyrics_availability() {
        // Switching between two songs without lyrics stays centered while loading.
        assert!(!lyrics_panel_wanted(None, true, false));
        assert!(!lyrics_panel_wanted(Some(false), true, false));
        // Once lyrics arrive, the panel opens.
        assert!(lyrics_panel_wanted(Some(true), true, false));
        // Switching away from a song with lyrics keeps the panel open while loading, then closes.
        assert!(lyrics_panel_wanted(None, true, true));
        assert!(lyrics_panel_wanted(Some(false), false, true));
        assert!(!lyrics_panel_wanted(Some(false), true, true));
    }
}
