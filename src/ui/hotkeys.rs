//! Settings › Keyboard shortcuts: the system-wide ones, and running them.

use std::path::Path;

use egui::{Align2, Sense, Ui, pos2, vec2};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};

use crate::hotkeys::{Bindings, Cmd, GlobalHotkeys, keycaps, safe_to_grab};
use crate::theme;

use super::{App, integrations, notes, pages, widgets};

/// A combination being picked for `cmd`.
struct Capture {
    cmd: Cmd,
    error: Option<String>,
    /// Ctrl, Alt or the system key is down, so a key may be on its way.
    armed: bool,
}

pub(super) struct State {
    bindings: Bindings,
    started: bool,
    wayland: bool,
    /// `None` until started, and where shortcuts can't work.
    active: Option<GlobalHotkeys>,
    capture: Option<Capture>,
}

impl State {
    pub(super) fn new(data_dir: &Path) -> Self {
        Self { bindings: Bindings::load(data_dir), started: false, wayland: false, active: None, capture: None }
    }

    /// Picking a combination: the player's own keys leave it alone.
    pub(super) fn capturing(&self) -> bool {
        self.capture.is_some()
    }
}

/// Runs every frame, the hidden ones included.
pub(super) fn poll(app: &mut App, ctx: &egui::Context, frame: &eframe::Frame) {
    // Waits for the first frame: it takes the window to know if this is Wayland, where
    // the X11 grab would "succeed" and only ever fire over other X11 windows. That goes
    // for an X11 window of ours in a Wayland session too (Close to tray opens one).
    if !std::mem::replace(&mut app.hotkeys.started, true) {
        app.hotkeys.wayland = integrations::is_wayland(frame) || crate::wayland_session();
        if !app.hotkeys.wayland {
            let wake = ctx.clone();
            match GlobalHotkeys::new(move || wake.request_repaint()) {
                Ok(mut keys) => {
                    keys.register(&app.hotkeys.bindings);
                    app.hotkeys.active = Some(keys);
                }
                Err(e) => log::warn!("system-wide shortcuts unavailable: {e}"),
            }
        }
    }
    // Ctrl + = would zoom instead of being picked. Otherwise, as ever.
    let capturing = app.hotkeys.capturing();
    ctx.options_mut(|o| o.zoom_with_keyboard = !capturing);
    let Some(keys) = &app.hotkeys.active else { return };
    for cmd in keys.pressed() {
        match cmd {
            // These go to the host in a jam, as the buttons do.
            Cmd::TogglePlay => app.toggle(),
            Cmd::Next => app.next(),
            Cmd::Prev => app.prev(),
            Cmd::VolumeUp => app.nudge_volume(true),
            Cmd::VolumeDown => app.nudge_volume(false),
        }
    }
}

fn register(app: &mut App) {
    let App { hotkeys, .. } = app;
    if let Some(keys) = &mut hotkeys.active {
        keys.register(&hotkeys.bindings);
    }
}

/// Saves `combo` and puts it to work at once.
fn set(app: &mut App, cmd: Cmd, combo: String) {
    app.hotkeys.bindings.set(cmd, combo);
    if let Err(e) = app.hotkeys.bindings.save(app.backend.data_dir()) {
        log::warn!("could not save the shortcuts: {e}");
        app.toast = Some(("Couldn't save that shortcut".to_owned(), -1.0));
    }
    app.hotkeys.capture = None;
    register(app);
}

fn cancel(app: &mut App) {
    app.hotkeys.capture = None;
    register(app);
}

/// The dialog that takes the new combination.
pub(super) fn capture_dialog(app: &mut App, ctx: &egui::Context) {
    let Some(capture) = &app.hotkeys.capture else { return };
    let cmd = capture.cmd;
    // Gone to another app: its shortcuts shouldn't stay off behind this dialog.
    if ctx.input(|i| i.viewport().focused == Some(false)) {
        return cancel(app);
    }
    let (events, held) = ctx.input(|i| (i.events.clone(), i.modifiers));
    // The system hands a combination another app holds straight to that app, so all
    // that shows here is the modifiers going down and coming up with nothing between.
    if let Some(capture) = &mut app.hotkeys.capture {
        let down = safe_to_grab(mods_of(held));
        if down != capture.armed {
            capture.armed = down;
            if down {
                capture.error = None;
            } else if capture.error.is_none() {
                capture.error = Some("No key came through. Another app may be using that shortcut, so try a different one.".to_owned());
            }
        }
    }
    for event in events {
        let egui::Event::Key { key, physical_key, pressed: true, repeat: false, modifiers } = event else { continue };
        if key == egui::Key::Escape {
            return cancel(app);
        }
        // With Shift, the key can read as what it types there (`!`, not 1).
        let Some(code) = key_code(key).or(physical_key.and_then(key_code)) else { continue };
        let mods = mods_of(modifiers);
        let hotkey = HotKey::new(Some(mods), code);
        let error = if !safe_to_grab(mods) {
            Some(format!("Hold {} as well, so typing isn't taken from other apps.", if cfg!(target_os = "macos") { "Cmd, Ctrl or Option" } else { "Ctrl or Alt" }))
        } else {
            app.hotkeys.bindings.used_by(hotkey, cmd).map(|other| format!("That's already {}.", other.label().to_lowercase()))
        };
        if let Some(capture) = &mut app.hotkeys.capture
            && error.is_some()
        {
            capture.error = error;
            continue;
        }
        return set(app, cmd, hotkey.into_string());
    }
    let error = app.hotkeys.capture.as_ref().and_then(|c| c.error.clone());
    let mut cancelled = false;
    egui::Modal::new(egui::Id::unique("hotkey-capture")).frame(notes::surface()).backdrop_color(theme::shade(0.55)).show(ctx, |ui| {
        ui.set_width(400.0_f32.min(ctx.content_rect().width() - 48.0));
        egui::Frame::new().inner_margin(egui::Margin { left: 26, right: 26, top: 24, bottom: 22 }).show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            ui.label(egui::RichText::new(cmd.label()).font(theme::bold(22.0)).color(theme::text()));
            ui.add_space(6.0);
            ui.label(egui::RichText::new("Press the new shortcut.").font(theme::regular(13.0)).color(theme::text_dim()));
            ui.add_space(16.0);
            let held = held_names(held);
            let text = if held.is_empty() { "Waiting for a key".to_owned() } else { format!("{held} + …") };
            ui.label(egui::RichText::new(text).font(theme::semibold(17.0)).color(theme::text()));
            if let Some(error) = error {
                ui.add_space(10.0);
                ui.label(egui::RichText::new(error).font(theme::regular(12.5)).color(theme::RED));
            }
            ui.add_space(20.0);
            cancelled = widgets::pill(ui, "Cancel", None, false).clicked();
        });
    });
    if cancelled {
        cancel(app);
    }
}

/// The section above the in-window shortcuts.
pub(super) fn settings_section(app: &mut App, ui: &mut Ui) {
    pages::settings_section(ui, "System-wide shortcuts", None, Some("These work while another app is in front, or the window is minimized or in the tray."), |ui| {
        if app.hotkeys.wayland || app.hotkeys.active.is_none() {
            let why = if app.hotkeys.wayland {
                "Wayland doesn't let apps watch for keys in the background. Add shortcuts in your desktop's settings instead: ytm-rs answers its media keys."
            } else {
                "The system wouldn't let ytm-rs set up shortcuts."
            };
            pages::setting_row(ui, "Not available here", Some(why), |_| {});
            return;
        }
        let mut change = None;
        let mut reset = None;
        let defaults = Bindings::default();
        for (i, cmd) in Cmd::ALL.into_iter().enumerate() {
            let caps = keycaps(app.hotkeys.bindings.get(cmd));
            if !pages::filter_show(ui, &format!("{} {}", cmd.label(), caps.join(" "))) {
                continue;
            }
            if i > 0 {
                pages::divider(ui);
            }
            let refused = app.hotkeys.active.as_ref().is_some_and(|k| k.failed(cmd));
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::hover());
            let mid = rect.center().y;
            // Reset goes under the name, so the keys and Change hold still when it appears.
            let customized = app.hotkeys.bindings.get(cmd) != defaults.get(cmd);
            let label_y = if refused || customized { mid - 9.0 } else { mid };
            ui.painter().text(pos2(rect.left(), label_y), Align2::LEFT_CENTER, cmd.label(), theme::regular(14.0), theme::text());
            let mut detail_x = rect.left();
            if refused {
                let text = ui.painter().text(pos2(detail_x, mid + 10.0), Align2::LEFT_CENTER, "Another app may be using this", theme::regular(12.5), theme::RED);
                detail_x = text.right() + 10.0;
            }
            if customized {
                let size = ui.painter().layout_no_wrap("Reset".to_owned(), theme::regular(12.5), theme::text_dim()).size();
                let area = Align2::LEFT_CENTER.anchor_size(pos2(detail_x, mid + 10.0), size);
                let link = ui.interact(area.expand(3.0), ui.scope_id().with(("hotkey-reset", i)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
                let color = if link.hovered() { theme::text() } else { theme::text_dim() };
                ui.painter().text(area.left_center(), Align2::LEFT_CENTER, "Reset", theme::regular(12.5), color);
                if link.clicked() {
                    reset = Some(cmd);
                }
            }
            let mut controls = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::right_to_left(egui::Align::Center)));
            if widgets::pill(&mut controls, "Change", None, false).clicked() {
                change = Some(cmd);
            }
            let mut x = controls.cursor().right() - 14.0;
            for (j, key) in caps.iter().enumerate().rev() {
                x = pages::keycap(ui, pos2(x, mid), key) - 6.0;
                if j > 0 {
                    let plus = ui.painter().text(pos2(x, mid), Align2::RIGHT_CENTER, "+", theme::regular(13.0), theme::text_dim());
                    x = plus.left() - 6.0;
                }
            }
        }
        if let Some(cmd) = reset {
            set(app, cmd, defaults.get(cmd).to_owned());
        }
        if let Some(cmd) = change {
            // Let go first: pressing a combination still held would never reach the window.
            if let Some(keys) = &mut app.hotkeys.active {
                keys.unregister_all();
            }
            app.hotkeys.capture = Some(Capture { cmd, error: None, armed: false });
        }
    });
}

fn mods_of(m: egui::Modifiers) -> Modifiers {
    let mut out = Modifiers::empty();
    out.set(Modifiers::CONTROL, m.ctrl);
    out.set(Modifiers::ALT, m.alt);
    out.set(Modifiers::SHIFT, m.shift);
    // egui only reports this key on macOS.
    out.set(Modifiers::META, m.mac_cmd);
    out
}

fn held_names(m: egui::Modifiers) -> String {
    let names = [(m.ctrl, "Ctrl"), (m.alt, "Alt"), (m.shift, "Shift"), (m.mac_cmd, "Cmd")];
    names.iter().filter(|(on, _)| *on).map(|(_, name)| *name).collect::<Vec<_>>().join(" + ")
}

/// The keys a media shortcut makes sense on.
fn key_code(key: egui::Key) -> Option<Code> {
    use egui::Key;
    Some(match key {
        Key::A => Code::KeyA,
        Key::B => Code::KeyB,
        Key::C => Code::KeyC,
        Key::D => Code::KeyD,
        Key::E => Code::KeyE,
        Key::F => Code::KeyF,
        Key::G => Code::KeyG,
        Key::H => Code::KeyH,
        Key::I => Code::KeyI,
        Key::J => Code::KeyJ,
        Key::K => Code::KeyK,
        Key::L => Code::KeyL,
        Key::M => Code::KeyM,
        Key::N => Code::KeyN,
        Key::O => Code::KeyO,
        Key::P => Code::KeyP,
        Key::Q => Code::KeyQ,
        Key::R => Code::KeyR,
        Key::S => Code::KeyS,
        Key::T => Code::KeyT,
        Key::U => Code::KeyU,
        Key::V => Code::KeyV,
        Key::W => Code::KeyW,
        Key::X => Code::KeyX,
        Key::Y => Code::KeyY,
        Key::Z => Code::KeyZ,
        Key::Num0 => Code::Digit0,
        Key::Num1 => Code::Digit1,
        Key::Num2 => Code::Digit2,
        Key::Num3 => Code::Digit3,
        Key::Num4 => Code::Digit4,
        Key::Num5 => Code::Digit5,
        Key::Num6 => Code::Digit6,
        Key::Num7 => Code::Digit7,
        Key::Num8 => Code::Digit8,
        Key::Num9 => Code::Digit9,
        Key::F1 => Code::F1,
        Key::F2 => Code::F2,
        Key::F3 => Code::F3,
        Key::F4 => Code::F4,
        Key::F5 => Code::F5,
        Key::F6 => Code::F6,
        Key::F7 => Code::F7,
        Key::F8 => Code::F8,
        Key::F9 => Code::F9,
        Key::F10 => Code::F10,
        Key::F11 => Code::F11,
        Key::F12 => Code::F12,
        Key::ArrowUp => Code::ArrowUp,
        Key::ArrowDown => Code::ArrowDown,
        Key::ArrowLeft => Code::ArrowLeft,
        Key::ArrowRight => Code::ArrowRight,
        Key::Space => Code::Space,
        Key::Enter => Code::Enter,
        Key::Tab => Code::Tab,
        Key::Backspace => Code::Backspace,
        Key::Delete => Code::Delete,
        Key::Insert => Code::Insert,
        Key::Home => Code::Home,
        Key::End => Code::End,
        Key::PageUp => Code::PageUp,
        Key::PageDown => Code::PageDown,
        Key::Minus => Code::Minus,
        Key::Plus | Key::Equals => Code::Equal,
        Key::Comma => Code::Comma,
        Key::Period => Code::Period,
        Key::Semicolon => Code::Semicolon,
        Key::Slash => Code::Slash,
        Key::Backslash => Code::Backslash,
        Key::OpenBracket => Code::BracketLeft,
        Key::CloseBracket => Code::BracketRight,
        Key::Backtick => Code::Backquote,
        _ => return None,
    })
}
