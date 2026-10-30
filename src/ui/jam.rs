//! Listen together: the page (start, join, the room), the host sending its playback
//! to the room, and guests following it. The connection itself is `crate::jam`.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use egui::{Align2, RichText, Sense, Ui, pos2, vec2};

use super::pages::{divider, page_title, section_label, setting_row, settings_card, toggle};
use super::widgets::{icon_at, pill};
use super::{Action, App, Page};
use crate::audio::AudioCmd;
use crate::jam::{Cmd, Event, Jam, Member, RoomState, Tick};
use crate::model::Target;
use crate::theme::{self, icon};

/// Guests further than this from the host jump to where the host is.
const MAX_DRIFT: Duration = Duration::from_millis(350);
/// The host re-sends its position this often while playing, so drift gets corrected.
const TICK_EVERY: Duration = Duration::from_secs(15);
/// Songs before / after the current one sent to guests (liked-songs queues are huge).
const BEFORE: usize = 20;
const AFTER: usize = 100;
/// Songs one guest may have waiting in the queue at once.
const GUEST_WAITING: usize = 5;

#[derive(Default)]
pub struct Session {
    conn: Option<Jam>,
    /// We started the room (until the server says who the host is).
    created: bool,
    welcomed: bool,
    reconnecting: bool,
    you: String,
    host: String,
    control: bool,
    /// Guests may add songs.
    queue: bool,
    members: Vec<Member>,
    /// Who added which song (video ID → name), for songs guests added.
    pub by: HashMap<String, String>,
    /// Who added which song (video ID → member ID).
    owner: HashMap<String, String>,
    /// The code typed on the page.
    code_input: String,
    /// The name others see (saved); empty: the account's name.
    pub name: String,
    error: Option<String>,

    // host
    /// Fingerprint of the last state sent, and where playback was then.
    sent_sig: u64,
    sent: Option<(Tick, Instant)>,
    /// Index in our queue of the first song sent (guests' indexes are relative to it).
    sent_from: usize,

    // guest
    /// The host's latest position.
    last: Option<(Tick, Instant)>,
    /// The song we started for the room, and when we asked for it.
    loaded: Option<String>,
    /// The last song reported to YouTube as played, so pauses don't count it twice.
    reported: Option<String>,
    load_started: Option<Instant>,
    /// How long songs take to start here, so they're opened that far ahead.
    load_ms: u64,
    /// Steps of the YTM_JAM_SCRIPT dev hook already run.
    script_done: usize,
}

impl Session {
    pub fn active(&self) -> bool {
        self.conn.is_some()
    }

    pub fn is_host(&self) -> bool {
        self.active() && if self.welcomed { self.you == self.host } else { self.created }
    }

    /// In a room someone else hosts: playback follows the host.
    pub fn guest(&self) -> bool {
        self.active() && !self.is_host()
    }

    pub fn member_count(&self) -> usize {
        self.members.len()
    }
}

fn my_name(app: &App) -> String {
    [Some(app.jam.name.clone()), app.account_name.clone()]
        .into_iter()
        .flatten()
        .map(|n| n.trim().to_owned())
        .find(|n| !n.is_empty())
        .unwrap_or_else(|| "Guest".to_owned())
}

/// A new session, keeping what the page holds (the typed code, the name).
fn fresh(app: &mut App, created: bool) {
    let code_input = std::mem::take(&mut app.jam.code_input);
    let name = std::mem::take(&mut app.jam.name);
    app.jam = Session { created, code_input, name, queue: true, load_ms: 800, ..Default::default() };
}

pub fn start(app: &mut App) {
    start_with(app, crate::jam::new_code());
}

fn start_with(app: &mut App, code: String) {
    leave(app);
    fresh(app, true);
    app.jam.conn = Some(Jam::start(&app.backend, code, true, my_name(app)));
}

pub fn join(app: &mut App, code: &str) {
    let Some(code) = crate::jam::normalize_code(code) else {
        app.jam.error = Some("Codes are 6 letters and numbers".into());
        return;
    };
    if app.jam.conn.as_ref().is_some_and(|c| c.code == code) {
        return;
    }
    leave(app);
    fresh(app, false);
    app.jam.conn = Some(Jam::start(&app.backend, code, false, my_name(app)));
}

/// Leave the room (the host ends it for everyone). Playback carries on locally.
pub fn leave(app: &mut App) {
    if let Some(c) = app.jam.conn.take() {
        if app.jam.is_host() {
            c.end();
        }
        c.leave();
    }
    fresh(app, false);
}

/// Dev hooks: YTM_JAM_HOST=<code> starts a room with that code, YTM_JAM_JOIN=<code> joins one.
pub fn dev_hooks(app: &mut App) {
    if let Some(code) = super::dev_var("YTM_JAM_HOST").and_then(crate::jam::normalize_code) {
        app.go(Page::Jam);
        start_with(app, code);
    } else if let Some(code) = super::dev_var("YTM_JAM_JOIN") {
        app.go(Page::Jam);
        join(app, code);
    }
}

/// Dev hook: YTM_JAM_SCRIPT="<secs>:<action>,…" presses buttons at those app times, as
/// whoever this instance is (a guest's go to the host as requests). Actions: skip, prev,
/// toggle, seek=<secs>, add=<videoId>, control=on|off.
pub fn dev_script(app: &mut App, ctx: &egui::Context, now: f64) {
    let Some(script) = super::dev_var("YTM_JAM_SCRIPT") else { return };
    for (i, step) in script.split(',').enumerate() {
        let Some((t, act)) = step.split_once(':') else { continue };
        let Ok(t) = t.parse::<f64>() else { continue };
        if i < app.jam.script_done {
            continue;
        }
        if now < t {
            // A paused app doesn't redraw on its own.
            ctx.request_repaint_after(Duration::from_secs_f64(t - now));
            break;
        }
        app.jam.script_done = i + 1;
        log::info!("jam: script {act}");
        match act {
            "skip" => app.next(),
            "prev" => app.prev(),
            "toggle" => app.toggle(),
            "control=on" => set_control(app, true, app.jam.queue),
            "control=off" => set_control(app, false, app.jam.queue),
            "queue=on" => set_control(app, app.jam.control, true),
            "queue=off" => set_control(app, app.jam.control, false),
            a if a.starts_with("add=") => {
                let track = crate::model::Track { id: a[4..].to_owned(), title: "Test song".into(), artists: vec![], album: None, duration: None, thumbs: vec![], track_nr: None, plays: None };
                app.apply(Action::Enqueue(track));
            }
            a if a.starts_with("seek=") => {
                if let Ok(s) = a[5..].parse() {
                    app.apply(Action::Seek(s));
                }
            }
            _ => {}
        }
    }
}

/// A link (or a code from another launch) asked to join a room.
pub fn open_link(app: &mut App, code: String) {
    app.go(Page::Jam);
    join(app, &code);
}

/// Every frame: handle what arrived, then (host) send changes or (guest) catch up.
pub fn tick(app: &mut App) {
    let Some(conn) = &app.jam.conn else { return };
    let mut events = Vec::new();
    while let Some(e) = conn.try_recv() {
        events.push(e);
    }
    for e in events {
        on_event(app, e);
    }
    if !app.jam.welcomed || app.jam.reconnecting {
        return;
    }
    if app.jam.is_host() {
        host_send(app);
    } else {
        guest_follow(app);
    }
}

fn on_event(app: &mut App, e: Event) {
    let s = &mut app.jam;
    match e {
        Event::Welcome { you, host, control, queue, members } => {
            let rejoin = s.welcomed;
            s.welcomed = true;
            s.reconnecting = false;
            s.error = None;
            s.you = you;
            s.host = host;
            s.control = control;
            s.queue = queue;
            s.members = members;
            // Send our state straight away when hosting.
            s.sent_sig = 0;
            if !rejoin && s.created {
                app.toast("Jam started: share the code or link");
            } else if !rejoin {
                app.toast("Joined the jam");
            }
        }
        Event::Members { host, members } => {
            let promoted = s.you != s.host && host == s.you;
            s.host = host;
            s.members = members;
            if promoted {
                s.sent_sig = 0;
                app.toast("You're the host now");
            }
        }
        Event::Control { on, queue } => {
            s.control = on;
            s.queue = queue;
        }
        Event::State(state, at) => {
            if s.is_host() {
                return;
            }
            apply_state(app, state, at);
        }
        Event::Tick(t, at) => {
            if s.is_host() {
                return;
            }
            app.jam.last = Some((t, at));
            sync(app);
        }
        Event::Req { from, name, cmd } => {
            log::info!("jam: {name} asks {cmd:?}");
            host_request(app, from, name, cmd);
        }
        Event::Notice(m) => {
            log::info!("jam: {m}");
            app.toast(m);
        }
        Event::Reconnecting => s.reconnecting = true,
        Event::Closed(msg) => {
            let first = !s.welcomed;
            leave(app);
            if first {
                app.jam.error = Some(msg);
            } else {
                app.toast(msg);
            }
        }
    }
}

// ------------------------------------------------------------------------ host

/// The part of the queue the room sees, the playing song's position, and a signature
/// of what would be sent: it runs every frame, so the state itself is only built
/// (`host_state`) when the signature changed.
fn host_tick(app: &App) -> (std::ops::Range<usize>, Tick, u64) {
    let index = app.index.unwrap_or(0);
    let from = index.saturating_sub(BEFORE);
    let to = (index + AFTER).min(app.queue.len()).max(from);
    let vid = app.current().map(|t| t.id.clone()).unwrap_or_default();
    let (pos, playing) = match app.resume_at {
        Some(at) => (at, false),
        None => (app.audio.status.position(), app.playing()),
    };
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for t in app.queue.get(from..to).unwrap_or_default() {
        t.id.hash(&mut h);
    }
    (index - from).hash(&mut h);
    vid.hash(&mut h);
    playing.hash(&mut h);
    app.generation.hash(&mut h);
    (from..to, Tick { vid, pos: pos.as_millis() as u64, playing }, h.finish() | 1)
}

fn host_state(app: &App, range: std::ops::Range<usize>, tick: &Tick) -> RoomState {
    let index = app.index.unwrap_or(0) - range.start;
    let queue = app.queue.get(range).map(<[_]>::to_vec).unwrap_or_default();
    let ids: std::collections::HashSet<&str> = queue.iter().map(|t| t.id.as_str()).collect();
    let pick = |m: &HashMap<String, String>| m.iter().filter(|(id, _)| ids.contains(id.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect();
    let (by, owner) = (pick(&app.jam.by), pick(&app.jam.owner));
    RoomState { queue, index, by, owner, vid: tick.vid.clone(), pos: tick.pos, playing: tick.playing }
}

fn host_send(app: &mut App) {
    // A song still loading has no real position yet: send once it plays.
    if app.buffering {
        return;
    }
    let (range, tick, sig) = host_tick(app);
    let now = Instant::now();
    if app.jam.conn.is_none() {
        return;
    }
    let state = (sig != app.jam.sent_sig).then(|| host_state(app, range, &tick));
    let s = &mut app.jam;
    let Some(conn) = &s.conn else { return };
    if let Some(state) = state {
        log::info!(
            "jam: sending state ({} songs, {} KB, {} at {} ms, playing {})",
            state.queue.len(),
            serde_json::to_vec(&state).map_or(0, |v| v.len() / 1024),
            state.vid,
            state.pos,
            state.playing
        );
        conn.send_state(&state);
        s.sent_sig = sig;
        s.sent_from = app.index.unwrap_or(0).saturating_sub(BEFORE);
        s.sent = Some((tick, now));
        return;
    }
    let Some((sent, at)) = &s.sent else { return };
    // A seek: the position moved away from where it should be.
    let expected = sent.position_at(*at).as_millis() as i64;
    let jumped = (expected - tick.pos as i64).abs() > 1500;
    if jumped || (tick.playing && at.elapsed() >= TICK_EVERY) {
        log::debug!("jam: sending tick at {} ms", tick.pos);
        conn.send_tick(&tick);
        s.sent = Some((tick, now));
    }
}

/// Songs `member` added that haven't played yet.
fn waiting(app: &App, member: &str) -> usize {
    let start = app.index.map_or(0, |i| i + 1);
    app.queue.get(start..).unwrap_or_default().iter().filter(|t| app.jam.owner.get(&t.id).is_some_and(|m| m == member)).count()
}

fn host_request(app: &mut App, from: String, name: String, cmd: Cmd) {
    if !app.jam.is_host() {
        return;
    }
    if let Cmd::Add { track } | Cmd::Next { track } = &cmd {
        // Their app checks this too; two adds can cross the latest state, though.
        if waiting(app, &from) >= GUEST_WAITING && from != app.jam.you {
            log::info!("jam: {name} already has {GUEST_WAITING} songs waiting");
            return;
        }
        app.jam.owner.insert(track.id.clone(), from);
    }
    match cmd {
        Cmd::Add { track } | Cmd::Next { track } if track.id.is_empty() => {}
        Cmd::Add { track } => {
            app.toast(format!("{name} added \"{}\"", track.title));
            app.jam.by.insert(track.id.clone(), name);
            let Some(i) = app.index else {
                app.queue.push(track);
                app.index = Some(app.queue.len() - 1);
                app.play_current();
                return;
            };
            // After the current song and the other songs people added, ahead of autoplay.
            let mut at = i + 1;
            while app.queue.get(at).is_some_and(|t| app.jam.by.contains_key(&t.id)) {
                at += 1;
            }
            app.queue.insert(at.min(app.queue.len()), track);
            app.prefetch_next();
        }
        Cmd::Next { track } => {
            app.toast(format!("{name} queued \"{}\" next", track.title));
            app.jam.by.insert(track.id.clone(), name);
            let at = app.index.map_or(0, |i| i + 1).min(app.queue.len());
            app.queue.insert(at, track);
            if app.index.is_none() {
                app.index = Some(0);
                app.play_current();
            } else {
                app.prefetch_next();
            }
        }
        // The server only passes these on while guests may control playback.
        Cmd::Toggle => app.toggle(),
        Cmd::Skip => app.next(),
        Cmd::Prev => app.prev(),
        Cmd::Seek { pos } => app.apply(Action::Seek(pos as f32 / 1000.0)),
        Cmd::Jump { index } => {
            let i = app.jam.sent_from + index;
            if i < app.queue.len() {
                app.index = Some(i);
                app.play_current();
            }
        }
    }
}

/// Host: what guests may do (control playback, add songs).
fn set_control(app: &mut App, on: bool, queue: bool) {
    app.jam.control = on;
    app.jam.queue = queue;
    if let Some(c) = &app.jam.conn {
        c.set_control(on, queue);
    }
}

// ----------------------------------------------------------------------- guest

fn apply_state(app: &mut App, state: RoomState, at: Instant) {
    let tick = Tick { vid: state.vid.clone(), pos: state.pos, playing: state.playing };
    app.jam.by = state.by;
    app.jam.owner = state.owner;
    app.queue_source = None;
    app.queue_fill = None;
    app.play_pending = None;
    if state.queue.is_empty() {
        app.queue.clear();
        app.index = None;
        app.jam.loaded = None;
        app.audio.send(AudioCmd::Stop);
        app.jam.last = None;
        return;
    }
    app.index = Some(state.index.min(state.queue.len() - 1));
    app.queue = state.queue;
    app.jam.last = Some((tick, at));
    sync(app);
}

/// Get playback to where the host is.
fn sync(app: &mut App) {
    let Some((tick, at)) = app.jam.last.clone() else { return };
    if tick.vid.is_empty() {
        return;
    }
    // A tick can name a song before its state arrives: wait for the state then.
    if app.current_id() != Some(tick.vid.as_str()) {
        match app.queue.iter().position(|t| t.id == tick.vid) {
            Some(i) => app.index = Some(i),
            None => return,
        }
    }
    let target = tick.position_at(at);
    if app.jam.loaded.as_deref() != Some(tick.vid.as_str()) || app.resume_at.is_some() {
        if tick.playing {
            // Opened a little ahead: by the time it starts, the host is there.
            let ahead = Duration::from_millis(app.jam.load_ms);
            // Joining a song counts as a play; catching up on the same one doesn't.
            let report = app.jam.reported.as_deref() != Some(tick.vid.as_str());
            app.jam.reported = Some(tick.vid.clone());
            app.jam.loaded = Some(tick.vid.clone());
            app.jam.load_started = Some(Instant::now());
            app.play_current_at(target + ahead, report);
        } else if app.jam.loaded.as_deref() != Some(tick.vid.as_str()) || app.resume_at != Some(target) {
            // Paused: show the song at the host's position without loading it.
            app.audio.send(AudioCmd::Stop);
            app.jam.loaded = None;
            app.resume_at = Some(target);
            app.audio.status.position_ms.store(target.as_millis() as u64, Ordering::Relaxed);
            app.audio.status.duration_ms.store(app.current().and_then(|t| t.duration).map_or(0, |d| d as u64 * 1000), Ordering::Relaxed);
            app.request_art();
        }
        return;
    }
    if app.buffering {
        return;
    }
    if tick.playing != app.playing() {
        app.audio.send(if tick.playing { AudioCmd::Resume } else { AudioCmd::Pause });
    }
    let pos = app.audio.status.position_smooth();
    let off = pos.as_millis() as i64 - target.as_millis() as i64;
    log::debug!("jam: {off:+} ms from the host (at {} ms, host {} ms)", pos.as_millis(), target.as_millis());
    if off.unsigned_abs() > MAX_DRIFT.as_millis() as u64 {
        log::info!("jam: {off:+} ms from the host, seeking");
        app.audio.send(if tick.playing { AudioCmd::SeekLive { pos: target, at: Instant::now() } } else { AudioCmd::Seek(target) });
    }
}

fn guest_follow(app: &mut App) {
    // A song we started for the room just began: learn how long that took, and
    // line up with the host.
    if let Some(t) = app.jam.load_started
        && !app.buffering
        && app.playing()
    {
        app.jam.load_started = None;
        let took = t.elapsed().as_millis() as u64;
        app.jam.load_ms = ((app.jam.load_ms + took.min(8000)) / 2).max(200);
        if let Some((tick, at)) = &app.jam.last {
            let off = app.audio.status.position().as_millis() as i64 - tick.position_at(*at).as_millis() as i64;
            log::info!("jam: song started after {took} ms, {off:+} ms from the host");
        }
        sync(app);
    }
}

/// Ask the host to do something (as a guest).
pub fn request(app: &mut App, cmd: Cmd) {
    if cmd.is_control() && !app.jam.control {
        app.toast("Only the host can control playback");
        return;
    }
    if !cmd.is_control() && !app.jam.queue {
        app.toast("The host turned off adding songs");
        return;
    }
    if !cmd.is_control() && waiting(app, &app.jam.you) >= GUEST_WAITING {
        log::info!("jam: {GUEST_WAITING} songs already waiting");
        app.toast(format!("You have {GUEST_WAITING} songs waiting. Add more once one plays"));
        return;
    }
    match &cmd {
        Cmd::Add { .. } => app.toast("Added to the jam"),
        Cmd::Next { .. } => app.toast("Playing next in the jam"),
        _ => {}
    }
    if let Some(c) = &app.jam.conn {
        c.request(cmd);
    }
}

/// A guest's action: things that would change the room's playback go to the host
/// instead. Returns the action if it should run locally.
pub fn guest_action(app: &mut App, a: Action) -> Option<Action> {
    match a {
        Action::Play(tracks, i) => {
            if let Some(t) = tracks.into_iter().nth(i) {
                if app.current_id() == Some(t.id.as_str()) {
                    request(app, Cmd::Toggle);
                } else {
                    request(app, Cmd::Add { track: t });
                }
            }
        }
        Action::Open(Target::Song(t)) | Action::Radio(t) | Action::Enqueue(t) => request(app, Cmd::Add { track: t }),
        Action::PlayNext(t) => request(app, Cmd::Next { track: t }),
        Action::PlayCollection(..) => app.toast("In a jam, add songs one at a time"),
        Action::QueueJump(i) => request(app, Cmd::Jump { index: i }),
        Action::QueueRemove(_) => app.toast("Only the host can remove songs"),
        Action::Seek(secs) => request(app, Cmd::Seek { pos: (secs.max(0.0) * 1000.0) as u64 }),
        Action::TogglePlay => request(app, Cmd::Toggle),
        a => return Some(a),
    }
    None
}

// ------------------------------------------------------------------------ page

pub fn page(app: &mut App, ui: &mut Ui) {
    let avail = ui.available_width();
    let w = avail.min(720.0);
    ui.horizontal(|ui| {
        ui.add_space(((avail - w) / 2.0).max(0.0));
        ui.vertical(|ui| {
            ui.set_width(w);
            page_title(ui, "Listen together");
            if app.jam.active() {
                room(app, ui);
            } else {
                lobby(app, ui);
            }
        });
    });
}

fn dim(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).font(theme::regular(13.5)).color(theme::text_dim()));
}

fn lobby(app: &mut App, ui: &mut Ui) {
    dim(ui, "Start a jam and friends hear what you play, at the same moment. Everyone listens on their own ytm-rs, and anyone can add songs.");
    section_label(ui, "Start a jam", None);
    settings_card(ui, |ui| {
        let hint = app.account_name.clone().unwrap_or_else(|| "Guest".to_owned());
        setting_row(ui, "Your name", Some("What others in the jam see."), |ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(200.0, 36.0), Sense::hover());
            ui.painter().rect_filled(rect, 10.0, theme::glass(0.08));
            let inner = rect.shrink2(vec2(12.0, 4.0));
            ui.put(
                inner,
                egui::TextEdit::singleline(&mut app.jam.name)
                    .hint_text(RichText::new(hint).color(theme::TEXT_FAINT))
                    .font(theme::regular(14.0))
                    .char_limit(32)
                    .frame(egui::Frame::NONE)
                    .align(egui::Align2::LEFT_CENTER)
                    .desired_width(inner.width()),
            );
        });
        divider(ui);
        setting_row(ui, "Host a jam", Some("You get a code and a link to share. You control playback."), |ui| {
            if pill(ui, "Start", Some(icon::USERS), true).clicked() {
                start(app);
            }
        });
    });
    section_label(ui, "Join a jam", None);
    settings_card(ui, |ui| {
        ui.add_space(12.0);
        dim(ui, "Type the code from the host, or open the link they shared.");
        ui.add_space(10.0);
        let mut join_now = false;
        // Button on the right edge, the field filling the rest, both one height.
        ui.allocate_ui_with_layout(vec2(ui.available_width(), 42.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            join_now |= pill(ui, "Join", None, true).clicked();
            ui.add_space(10.0);
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 42.0), Sense::hover());
            ui.painter().rect_filled(rect, 10.0, theme::glass(0.08));
            let inner = rect.shrink2(vec2(14.0, 4.0));
            let edit = ui.put(
                inner,
                egui::TextEdit::singleline(&mut app.jam.code_input)
                    .hint_text(RichText::new("ABC234").color(theme::TEXT_FAINT))
                    .font(theme::semibold(16.0))
                    .char_limit(8)
                    .frame(egui::Frame::NONE)
                    .align(egui::Align2::LEFT_CENTER)
                    .desired_width(inner.width()),
            );
            if edit.changed() {
                app.jam.error = None;
            }
            join_now |= edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        });
        if join_now {
            let code = app.jam.code_input.clone();
            join(app, &code);
        }
        if let Some(e) = &app.jam.error {
            ui.add_space(8.0);
            ui.label(RichText::new(e).font(theme::regular(13.0)).color(theme::RED));
        }
        ui.add_space(14.0);
    });
}

fn room(app: &mut App, ui: &mut Ui) {
    let code = app.jam.conn.as_ref().map(|c| c.code.clone()).unwrap_or_default();
    let host = app.jam.is_host();
    let status = if !app.jam.welcomed {
        Some("Connecting…")
    } else if app.jam.reconnecting {
        Some("Connection lost, reconnecting…")
    } else {
        None
    };
    settings_card(ui, |ui| {
        ui.add_space(16.0);
        dim(ui, if host { "Share this code or link with friends" } else { "You're in the jam" });
        ui.add_space(6.0);
        let spaced: String = code.chars().map(|c| c.to_string()).collect::<Vec<_>>().join(" ");
        ui.label(RichText::new(spaced).font(theme::bold(40.0)).color(theme::TEXT));
        if let Some(s) = status {
            ui.horizontal(|ui| {
                ui.spinner();
                dim(ui, s);
            });
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if pill(ui, "Copy link", Some(icon::LINK), true).clicked() {
                app.share_pending = Some(crate::jam::share_link(&code));
                app.toast("Link copied");
            }
            ui.add_space(8.0);
            if pill(ui, "Copy code", Some(icon::COPY), false).clicked() {
                app.share_pending = Some(code.clone());
                app.toast("Code copied");
            }
        });
        ui.add_space(16.0);
    });

    section_label(ui, "Playback", None);
    settings_card(ui, |ui| {
        if host {
            let (mut queue, mut on) = (app.jam.queue, app.jam.control);
            setting_row(ui, "Guests can add songs", Some("Songs they play go into the queue, right after the current one."), |ui| {
                if toggle(ui, &mut queue, "Guests can add songs").changed() {
                    set_control(app, on, queue);
                }
            });
            divider(ui);
            setting_row(ui, "Guests can control playback", Some("Play and pause, skip, go back, seek, and jump to a song in the queue."), |ui| {
                if toggle(ui, &mut on, "Guests can control playback").changed() {
                    set_control(app, on, queue);
                }
            });
        } else {
            let desc = match (app.jam.queue, app.jam.control) {
                (true, true) => "You can add songs and control playback.",
                (true, false) => "Songs you play are added to the queue. The host controls playback.",
                (false, true) => "You can control playback. The host turned off adding songs.",
                (false, false) => "The host controls the queue and playback.",
            };
            setting_row(ui, "Following the host", Some(desc), |_| {});
        }
        divider(ui);
        let (title, desc, label) = if host {
            ("End the jam", "Everyone leaves; your music keeps playing.", "End jam")
        } else {
            ("Leave the jam", "The music keeps playing here, on your own.", "Leave")
        };
        setting_row(ui, title, Some(desc), |ui| {
            if pill(ui, label, Some(icon::LOG_OUT), false).clicked() {
                leave(app);
            }
        });
    });

    let n = app.jam.members.len();
    section_label(ui, "People", Some(&format!("{n} listening")));
    settings_card(ui, |ui| {
        ui.add_space(6.0);
        for m in &app.jam.members {
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
            let is_host = m.id == app.jam.host;
            icon_at(ui, pos2(rect.left() + 12.0, rect.center().y), if is_host { icon::CROWN } else { icon::HEADPHONES }, 17.0, if is_host { app.accent } else { theme::text_dim() });
            let you = if m.id == app.jam.you { " (you)" } else { "" };
            ui.painter().text(pos2(rect.left() + 34.0, rect.center().y), Align2::LEFT_CENTER, format!("{}{you}", m.name), theme::semibold(14.5), theme::TEXT);
            if is_host {
                ui.painter().text(pos2(rect.right() - 4.0, rect.center().y), Align2::RIGHT_CENTER, "Host", theme::regular(12.5), theme::text_dim());
            }
        }
        ui.add_space(6.0);
    });
}
