//! "Recognize song": a card like the mini player, while it listens and for a while with
//! what it found. Over the app, above the player bar, while the app is in front; else in a
//! small window of its own kept above the others (the song is most likely playing in
//! another app, or a game). When it paused our own playback to listen, what it found plays
//! instead.

use std::time::Duration;

use egui::{Color32, Pos2, Rect, Sense, Stroke, Ui, Vec2, ViewportBuilder, ViewportCommand, ViewportId, pos2};

use super::widgets::{self, icon_at, named, paint_cover};
use super::{Action, App, Page, mini, shell};
use crate::audio::AudioCmd;
use crate::model::Track;
use crate::recognize::{BARS, Phase, Problem};
use crate::theme::{self, icon};

/// Our own pause fading out (200 ms), and the output's delay: not worth recording.
const PAUSE_SETTLES: Duration = Duration::from_millis(400);
/// How long an answer stays up, in seconds (longer while the pointer is on it).
const HOLD: f64 = 8.0;
/// After Play or Add to playlist: up this long, pointer on it or not, then it fades.
const ACTED_HOLD: f64 = 3.0;
/// Its fade out, in seconds.
const FADE: f64 = 0.4;
/// The mini player's strip.
const SIZE: Vec2 = mini::SIZE;
/// From the screen's corner, and from the mini player.
const MARGIN: f32 = 16.0;
/// The card's corners over the app (its own window's are the system's).
const RADIUS: f32 = 12.0;
/// The player bar and a gap: where the toasts sit too.
const OVER_BAR: f32 = 104.0;

#[derive(Default)]
pub(super) struct Recognizing {
    /// Our playback, paused to listen past it.
    paused: Option<Paused>,
    /// When the answer came, in egui's time.
    answered: Option<f64>,
    /// The pointer is on the card: it stays up.
    hovered: bool,
    /// When Play or Add to playlist was clicked: it fades away a little later.
    acted: Option<f64>,
    /// The bars as drawn: up at once, falling back slowly.
    bars: [f32; BARS],
    /// The app was in front last frame: coming back to it puts the answer away.
    front: bool,
}

struct Paused {
    id: String,
    /// The player has stopped: from then on, playing again is the user's doing.
    settled: bool,
}

/// Starts listening. Our own playback pauses meanwhile, so it hears the other app; not
/// in a jam, where that would pause everyone.
pub(super) fn start(app: &mut App) {
    if app.recognize.busy() {
        return;
    }
    // Canceled and started again before a frame went by: still ours, paused.
    let kept = app.recognizing.paused.take().filter(|p| app.current_id() == Some(p.id.as_str()));
    let pause = kept.is_none() && app.playing() && !app.jam.active();
    let settling = pause || kept.as_ref().is_some_and(|p| !p.settled);
    app.recognizing = Recognizing { paused: kept, ..Default::default() };
    let skip = if settling { PAUSE_SETTLES } else { Duration::ZERO };
    if !app.recognize.start(&app.backend, skip) {
        return;
    }
    if let Some(id) = app.current_id().filter(|_| pause) {
        app.recognizing.paused = Some(Paused { id: id.to_owned(), settled: false });
        app.audio.send(AudioCmd::Pause);
    }
}

/// The shortcut, the tray's item and the search box's button: starts, or stops what's
/// under way.
pub(super) fn toggle(app: &mut App) {
    if app.recognize.busy() {
        app.recognize.close();
    } else {
        start(app);
    }
}

/// Once a frame, drawn or not (the window can be in the tray): takes the answer, and puts
/// it away once it has been up long enough.
pub(super) fn tick(app: &mut App, ctx: &egui::Context) {
    let Some(phase) = app.recognize.phase() else {
        // Canceled while listening: our song again, if still paused as we left it (or the
        // pause isn't through yet: this comes after it).
        if app.recognizing.paused.take().is_some_and(|p| app.current_id() == Some(p.id.as_str()) && (!p.settled || !app.playing())) {
            app.audio.send(AudioCmd::Resume);
        }
        app.recognizing.answered = None;
        app.recognizing.acted = None;
        return;
    };
    // The card's window shows with the app's in the tray or minimized too.
    ctx.send_viewport_cmd(ViewportCommand::RequestPaintWhileHidden);
    ctx.request_repaint_after(Duration::from_millis(250));
    let (now, dt) = ctx.input(|i| (i.time, i.stable_dt.min(0.1)));
    // Another song, or ours again once the pause has settled: the user's doing.
    let (playing, changed) = (app.playing(), app.recognizing.paused.as_ref().is_some_and(|p| app.current_id() != Some(p.id.as_str())));
    let heard = if matches!(phase, Phase::Listening { .. }) { app.recognize.bars() } else { [0.0; BARS] };
    // Where the search it opened shows: not the mini player.
    let front = in_app(app, ctx);
    let r = &mut app.recognizing;
    let back = front && !std::mem::replace(&mut r.front, front);
    let fall = 1.0 - (-dt * 6.0).exp();
    for (shown, heard) in r.bars.iter_mut().zip(heard) {
        *shown = if heard > *shown { heard } else { *shown - (*shown - heard) * fall };
    }
    if let Some(p) = &mut r.paused {
        if changed || (p.settled && playing) {
            r.paused = None;
        } else if !playing {
            p.settled = true;
        }
    }
    match r.answered {
        _ if matches!(phase, Phase::Listening { .. } | Phase::Identifying) => {}
        None => answer(app, phase, now),
        // Played or added: gone once faded, the app coming up for the picker or not.
        Some(_) if r.acted.is_some_and(|at| now - at > ACTED_HOLD + FADE) => app.recognize.close(),
        Some(_) if r.acted.is_some() => ctx.request_repaint(),
        // Seen in the app now, as the search it opened.
        Some(_) if back => app.recognize.close(),
        Some(at) if now - at > HOLD && !r.hovered => app.recognize.close(),
        Some(_) => {}
    }
}

/// What it found plays, if it paused ours (and no jam started meanwhile); else ours plays
/// again. A guess doesn't play: without a sure match, the card shows YouTube Music's top hit
/// to play, as itself. The search for what Shazam named opens too.
fn answer(app: &mut App, phase: Phase, now: f64) {
    app.recognizing.answered = Some(now);
    let ours = app.recognizing.paused.take().is_some() && !app.jam.active();
    let found = match phase {
        Phase::Found(t) => {
            app.actions.push(Action::Go(Page::Search(format!("{} {}", t.title, t.artist_line()))));
            // Explicit ones hidden: it wouldn't play, so ours does.
            Some(t).filter(|t| !(app.hide_explicit && t.explicit))
        }
        Phase::Unsure(h, _) => {
            app.actions.push(Action::Go(Page::Search(format!("{} {}", h.title, h.artist))));
            None
        }
        _ => None,
    };
    match found {
        Some(t) if ours => app.actions.push(Action::Play(vec![*t], 0)),
        _ if ours => app.audio.send(AudioCmd::Resume),
        _ => {}
    }
}

/// The card's opacity: 1 until it fades after Play or Add to playlist.
fn opacity(app: &App, ctx: &egui::Context) -> f32 {
    let Some(at) = app.recognizing.acted else { return 1.0 };
    let t = ((ctx.input(|i| i.time) - at - ACTED_HOLD) / FADE).clamp(0.0, 1.0) as f32;
    1.0 - t * t * (3.0 - 2.0 * t)
}

/// Over the app while it's in front (and not the mini player, too small for it); else in
/// a window of its own.
fn in_app(app: &App, ctx: &egui::Context) -> bool {
    !app.window_hidden && !app.mini.busy() && ctx.input(|i| i.viewport().focused) != Some(false)
}

/// The card's own window. From `App::ui`, before the hidden window's early return.
pub(super) fn popout(app: &mut App, ctx: &egui::Context) {
    if in_app(app, ctx) {
        return;
    }
    let Some(v) = view(app) else { return };
    let mut builder = ViewportBuilder::default()
        .with_title(tr!("recognize-overline"))
        .with_inner_size(SIZE)
        .with_resizable(false)
        .with_decorations(false)
        .with_always_on_top()
        .with_taskbar(false)
        // So it can fade away.
        .with_transparent(true)
        // Not the keyboard: the user is likely in the app playing the song, or a game.
        .with_active(false);
    if let Some(p) = place(app, ctx) {
        builder = builder.with_position(p);
    }
    let fade = opacity(app, ctx);
    let out = ctx.show_viewport_immediate(ViewportId::from_hash_of("recognize"), builder, |ui, _| {
        let ctx = ui.ctx().clone();
        ui.multiply_opacity(fade);
        // Under the card's buttons: the rest of it brings the app up.
        let bg = ui.interact(ctx.content_rect(), egui::Id::unique("recognize-focus"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
        let mut out = card(app, ui, ctx.content_rect(), &v, 0.0);
        out.focus = bg.clicked();
        out.close |= ctx.input(|i| i.viewport().close_requested());
        out.hovered = ctx.input(|i| i.pointer.hover_pos().is_some());
        out
    });
    apply(app, ctx, v.track, out);
}

/// The card over the app, in the bottom right above the player bar.
pub(super) fn inline(app: &mut App, ctx: &egui::Context) {
    if !in_app(app, ctx) {
        return;
    }
    let Some(v) = view(app) else { return };
    let screen = ctx.content_rect();
    let fade = opacity(app, ctx);
    let out = egui::Area::new(egui::Id::unique("recognize-card"))
        .order(egui::Order::Foreground)
        .pivot(egui::Align2::RIGHT_BOTTOM)
        .fixed_pos(pos2(screen.right() - MARGIN, screen.bottom() - OVER_BAR))
        .show(ctx, |ui| {
            ui.multiply_opacity(fade);
            let (r, _) = ui.allocate_exact_size(SIZE, Sense::hover());
            let shadow = egui::epaint::Shadow { offset: [0, 8], blur: 24, spread: 0, color: theme::shade(0.45) };
            ui.painter().add(shadow.as_shape(r, RADIUS));
            let mut out = card(app, ui, r, &v, RADIUS);
            out.hovered = ctx.pointer_hover_pos().is_some_and(|p| r.contains(p));
            out
        })
        .inner;
    apply(app, ctx, v.track, out);
}

/// What the card shows.
struct View {
    /// What it found (or the top hit for what Shazam heard).
    track: Option<Track>,
    title: String,
    line: String,
    /// Rings out from the badge, this fast, while it hears something: it's listening.
    pulse: Option<f64>,
    failed: bool,
    busy: bool,
    /// It's the song in the player: no Play.
    played: bool,
    /// Explicit ones hidden: it wouldn't play.
    playable: bool,
    bars: [f32; BARS],
}

fn view(app: &App) -> Option<View> {
    let phase = app.recognize.phase()?;
    let track = match &phase {
        Phase::Found(t) | Phase::Unsure(_, Some(t)) => Some((**t).clone()),
        _ => None,
    };
    let (title, line): (String, String) = match (&phase, &track) {
        // What Play and Add to playlist would take, not what Shazam named.
        (_, Some(t)) => (t.title.clone(), t.artist_line()),
        (Phase::Unsure(h, _), _) => (h.title.clone(), h.artist.clone()),
        (Phase::Listening { hearing: false }, _) => (tr!("recognize-waiting").into(), tr!("recognize-waiting-line").into()),
        (Phase::Listening { hearing: true }, _) => (tr!("recognize-listening").into(), tr!("recognize-listening-line").into()),
        (Phase::Identifying, _) => (tr!("recognize-identifying").into(), tr!("recognize-identifying-line").into()),
        (Phase::Failed(p), _) => (tr!("recognize-failed").into(), problem(p)),
        _ => (tr!("recognize-no-match").into(), tr!("recognize-no-match-line").into()),
    };
    let pulse = match phase {
        Phase::Listening { hearing: true } => Some(1.1),
        Phase::Identifying => Some(1.8),
        _ => None,
    };
    Some(View {
        failed: matches!(phase, Phase::Failed(_) | Phase::NoMatch),
        busy: matches!(phase, Phase::Listening { .. } | Phase::Identifying),
        played: track.as_ref().is_some_and(|t| app.current_id() == Some(t.id.as_str())),
        playable: track.as_ref().is_some_and(|t| !(app.hide_explicit && t.explicit)),
        bars: app.recognizing.bars,
        track,
        title,
        line,
        pulse,
    })
}

#[derive(Default)]
struct Out {
    close: bool,
    play: bool,
    playlist: bool,
    /// The card's own window was clicked: the app comes up.
    focus: bool,
    hovered: bool,
}

/// The card in `r`, laid out as the mini player's strip: the cover (or a badge while it
/// listens) on the left, title and artists, and under them Play and Add to playlist, or
/// what it hears. Close shows on hover, at the top right.
fn card(app: &App, ui: &mut Ui, r: Rect, v: &View, radius: f32) -> Out {
    let ctx = ui.ctx().clone();
    let mut out = Out::default();
    let (base, accent) = (shell::base_color(app), app.accent_color());
    ui.painter().rect_filled(r, radius, base);
    ui.painter().rect(r, radius, theme::panel(0.18), Stroke::new(1.0, theme::glass(0.14)), egui::StrokeKind::Inside);

    let pad = 12.0;
    let art = Rect::from_min_size(r.min + Vec2::splat(pad), Vec2::splat(r.height() - 2.0 * pad - 4.0));
    let corner = theme::thumb_radius() + 1.0;
    match &v.track {
        Some(t) => paint_cover(ui, art, &t.thumbs, corner, 1.0),
        None => {
            ui.painter().rect_filled(art, corner, theme::glass_panel(0.08));
            let c = art.center();
            if let Some(speed) = v.pulse {
                let now = ctx.input(|i| i.time);
                for k in 0..2 {
                    let t = ((now * speed + k as f64 * 0.5).fract()) as f32;
                    ui.painter().circle_stroke(c, 20.0 + 14.0 * t, Stroke::new(2.0, theme::with_alpha(accent, 0.55 * (1.0 - t))));
                }
                ctx.request_repaint_after(Duration::from_millis(33));
            }
            let lit = v.pulse.is_some();
            ui.painter().circle_filled(c, 20.0, if lit { accent } else { theme::glass_panel(0.10) });
            let glyph = if v.failed { icon::X } else { icon::HEADPHONES };
            icon_at(ui, c, glyph, 20.0, if lit { theme::on_fill(accent) } else { theme::text_dim() });
        }
    }

    // Title and artists, or what it's doing.
    let (x0, x1) = (art.right() + 12.0, r.right() - 10.0);
    let w = x1 - x0;
    let explicit = v.track.as_ref().is_some_and(|t| t.explicit);
    widgets::title_at(ui, pos2(x0, art.top() + 1.0), &v.title, explicit, theme::semibold(14.5), theme::text(), w);
    let buttons = v.track.is_some() && !v.played;
    let line = widgets::galley_wrapped(ui, &v.line, theme::regular(12.5), theme::text_dim(), w, if buttons { 1 } else { 2 });
    ui.painter().galley(pos2(x0, art.top() + 22.0), line, theme::text_dim());

    // Where the strip has its controls.
    let cy = art.bottom() - 17.0;
    if buttons {
        let row = Rect::from_min_max(pos2(x0, cy - 18.0), pos2(x1, cy + 18.0));
        let mut row = ui.new_child(egui::UiBuilder::new().max_rect(row).layout(egui::Layout::left_to_right(egui::Align::Center)));
        row.spacing_mut().item_spacing.x = 8.0;
        out.play = v.playable && widgets::pill(&mut row, tr!("recognize-play"), Some(icon::PLAY), true).clicked();
        out.playlist = widgets::pill(&mut row, tr!("recognize-playlist"), Some(icon::LIST_PLUS), false).clicked();
    } else if v.played {
        widgets::text_at(ui, pos2(x0, cy - 7.0), tr!("recognize-playing"), theme::semibold(11.5), accent, w);
    } else {
        meter(ui, Rect::from_min_max(pos2(x0, cy - 12.0), pos2(x1, cy + 12.0)), &v.bars, if v.failed { None } else { Some(accent) });
        if v.busy {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
    }

    // Close, as the strip's: on hover, over a backdrop so the title goes under it.
    let b = Rect::from_min_size(pos2(r.right() - 34.0, r.top() + 6.0), Vec2::splat(28.0));
    let name = if v.busy { tr!("recognize-cancel") } else { tr!("recognize-dismiss") };
    let resp = named(ui.interact(b, egui::Id::unique("recognize-close"), Sense::click()), name).on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand);
    let over = ctx.pointer_hover_pos().is_some_and(|p| r.contains(p));
    let reveal = theme::anim_bool(&ctx, "recognize-close-reveal", over || resp.has_focus(), 0.15);
    if reveal > 0.0 {
        let backdrop = theme::lerp_color(base, if theme::light() { Color32::WHITE } else { Color32::BLACK }, 0.55);
        ui.painter().rect_filled(b.expand(2.0), 9.0, backdrop.gamma_multiply(reveal));
        let h = theme::anim_bool(&ctx, resp.id.with("h"), resp.hovered(), 0.12);
        if h > 0.0 {
            ui.painter().rect_filled(b, 6.0, Color32::from_rgb(0xC4, 0x2B, 0x1C).gamma_multiply(h * reveal));
        }
        let fg = theme::lerp_color(theme::text_dim(), Color32::WHITE, h);
        icon_at(ui, b.center(), icon::X, 14.0, fg.gamma_multiply(reveal));
    }
    // Not while still hidden: a quick click there was meant for the card.
    out.close = resp.clicked() && (reveal > 0.5 || resp.has_focus());
    out
}

/// What's on the card's bottom row while it listens: bars from low to high, each as loud as
/// what it hears there, in the accent fading to lavender (None: flat and dim).
fn meter(ui: &Ui, r: Rect, bars: &[f32; BARS], accent: Option<Color32>) {
    let gap = 3.0;
    let w = (r.width() - gap * (BARS - 1) as f32) / BARS as f32;
    let (top, bottom) = match accent {
        Some(a) => (a, theme::lerp_color(a, Color32::from_rgb(0xA9, 0x9C, 0xFF), 0.6)),
        None => (theme::glass_panel(0.14), theme::glass_panel(0.14)),
    };
    let mut mesh = egui::Mesh::default();
    for (i, level) in bars.iter().enumerate() {
        let x = r.left() + i as f32 * (w + gap);
        let h = 3.0 + (r.height() - 3.0) * level;
        let bar = Rect::from_min_max(pos2(x, r.bottom() - h), pos2(x + w, r.bottom()));
        let n = mesh.vertices.len() as u32;
        for (p, c) in [(bar.left_top(), top), (bar.right_top(), top), (bar.right_bottom(), bottom), (bar.left_bottom(), bottom)] {
            mesh.colored_vertex(p, c);
        }
        mesh.add_triangle(n, n + 1, n + 2);
        mesh.add_triangle(n, n + 2, n + 3);
    }
    ui.painter().add(mesh);
}

/// Play, or Add to playlist: the picker in the app, brought to the front (out of the mini
/// player too, which has no room for it).
fn apply(app: &mut App, ctx: &egui::Context, track: Option<Track>, out: Out) {
    match track {
        Some(t) if out.play => app.actions.push(Action::Play(vec![t], 0)),
        Some(t) if out.playlist => {
            if app.mini.active() {
                app.actions.push(Action::Mini(false));
            }
            // On Wayland, where the window can't be brought up, the picker waits in it.
            app.bring_back(ctx, crate::wayland_window());
            app.actions.push(Action::AddToPlaylist(vec![t]));
        }
        _ if out.focus => {
            // Where the search it opened shows: not in the mini player. The answer goes once
            // the app is in front (`tick`).
            if app.mini.active() && app.recognizing.answered.is_some() {
                app.actions.push(Action::Mini(false));
            }
            app.bring_back(ctx, crate::wayland_window());
        }
        _ => {}
    }
    let r = &mut app.recognizing;
    r.hovered = out.hovered;
    if (out.play || out.playlist) && r.acted.is_none() {
        r.acted = Some(ctx.input(|i| i.time));
    }
    if out.close {
        app.recognize.close();
    }
}

/// The card's window, in points: beside the mini player while it's up (above it, or below
/// without room), else in the bottom right of the app's screen, clear of the taskbar where
/// the system says where that is.
fn place(app: &App, ctx: &egui::Context) -> Option<Pos2> {
    let ppp = ctx.pixels_per_point();
    let area = crate::fullscreen::work_area()
        .map(|r| Rect::from_min_max((r.min.to_vec2() / ppp).to_pos2(), (r.max.to_vec2() / ppp).to_pos2()))
        .or_else(|| ctx.input(|i| i.viewport().monitor_size).map(|s| Rect::from_min_size(Pos2::ZERO, s)));
    let strip = ctx.input(|i| i.viewport().outer_rect).filter(|_| app.mini.showing() && !app.window_hidden);
    if let Some(w) = strip {
        let left = area.map_or(w.right() - SIZE.x, |a| (w.right() - SIZE.x).max(a.left()));
        let above = w.top() - MARGIN / 2.0 - SIZE.y;
        let y = if area.is_none_or(|a| above >= a.top()) { above } else { w.bottom() + MARGIN / 2.0 };
        return Some(pos2(left, y));
    }
    Some(area?.max - SIZE - Vec2::splat(MARGIN))
}

fn problem(p: &Problem) -> String {
    match p {
        Problem::Unsupported => tr!("recognize-unsupported").into(),
        Problem::HeadsetOutput => tr!("recognize-headset-output").into(),
        Problem::NoDevice => tr!("recognize-no-device").into(),
        Problem::NothingPlaying => tr!("recognize-nothing-playing").into(),
        Problem::DeviceChanged => tr!("recognize-device-changed").into(),
        Problem::Offline => tr!("recognize-offline").into(),
        Problem::NoAnswer => tr!("recognize-no-answer").into(),
        Problem::RateLimited(until) => tr!("recognize-rate-limited", secs = until.saturating_duration_since(std::time::Instant::now()).as_secs().max(1)),
    }
}
