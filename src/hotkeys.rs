//! System-wide media shortcuts: they work while another app has the keyboard, or the
//! window is minimized or in the tray.
//!
//! The combinations live in `hotkeys.json` in the app data folder, one readable string
//! each (`"shift+alt+KeyP"`).

use std::fs;
use std::path::Path;

use crossbeam_channel::{Receiver, unbounded};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use serde::{Deserialize, Serialize};

const FILE: &str = "hotkeys.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    TogglePlay,
    Next,
    Prev,
    VolumeUp,
    VolumeDown,
}

impl Cmd {
    pub const ALL: [Cmd; 5] = [Self::TogglePlay, Self::Next, Self::Prev, Self::VolumeUp, Self::VolumeDown];

    pub fn label(self) -> &'static str {
        match self {
            Self::TogglePlay => "Play / pause",
            Self::Next => "Next song",
            Self::Prev => "Previous song",
            Self::VolumeUp => "Volume up",
            Self::VolumeDown => "Volume down",
        }
    }
}

/// The combination for each command. A file missing one gets the default for it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct Bindings {
    play_pause: String,
    next: String,
    prev: String,
    volume_up: String,
    volume_down: String,
}

impl Default for Bindings {
    fn default() -> Self {
        let combo = |key: &str| format!("shift+alt+{key}");
        Self {
            play_pause: combo("KeyP"),
            next: combo("KeyN"),
            prev: combo("KeyB"),
            volume_up: combo("Equal"),
            volume_down: combo("Minus"),
        }
    }
}

impl Bindings {
    pub fn get(&self, cmd: Cmd) -> &str {
        match cmd {
            Cmd::TogglePlay => &self.play_pause,
            Cmd::Next => &self.next,
            Cmd::Prev => &self.prev,
            Cmd::VolumeUp => &self.volume_up,
            Cmd::VolumeDown => &self.volume_down,
        }
    }

    pub fn set(&mut self, cmd: Cmd, combo: String) {
        let slot = match cmd {
            Cmd::TogglePlay => &mut self.play_pause,
            Cmd::Next => &mut self.next,
            Cmd::Prev => &mut self.prev,
            Cmd::VolumeUp => &mut self.volume_up,
            Cmd::VolumeDown => &mut self.volume_down,
        };
        *slot = combo;
    }

    /// The other command already on `hotkey`, if any.
    pub fn used_by(&self, hotkey: HotKey, except: Cmd) -> Option<Cmd> {
        Cmd::ALL.into_iter().find(|c| *c != except && self.get(*c).parse::<HotKey>().is_ok_and(|h| h.id() == hotkey.id()))
    }

    /// The saved combinations, or the defaults when there's no readable file.
    pub fn load(dir: &Path) -> Self {
        let path = dir.join(FILE);
        let Ok(text) = fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_else(|e| {
            log::warn!("ignoring unreadable {}: {e}", path.display());
            Self::default()
        })
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        fs::create_dir_all(dir)?;
        let path = dir.join(FILE);
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        // Beside it and renamed, so a crash can't leave half a file.
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &path)
    }
}

/// Without Ctrl, Alt or the system key, a combination would take a plain key or a
/// capital letter away from every app.
pub fn safe_to_grab(mods: Modifiers) -> bool {
    mods.intersects(Modifiers::CONTROL | Modifiers::ALT | Modifiers::META)
}

/// The keycaps for a saved combination (`["Alt", "Shift", "P"]`). One that doesn't
/// parse shows as it's written, so the file's mistake can be found.
pub fn keycaps(combo: &str) -> Vec<String> {
    let Ok(hotkey) = combo.parse::<HotKey>() else {
        return vec![combo.to_owned()];
    };
    let mac = cfg!(target_os = "macos");
    let meta = if mac { "Cmd" } else if cfg!(windows) { "Win" } else { "Super" };
    let mods = [
        (Modifiers::CONTROL, "Ctrl"),
        (Modifiers::ALT, if mac { "Option" } else { "Alt" }),
        (Modifiers::SHIFT, "Shift"),
        (Modifiers::META, meta),
    ];
    let mut caps: Vec<String> = mods.iter().filter(|(m, _)| hotkey.mods.contains(*m)).map(|(_, name)| (*name).to_owned()).collect();
    caps.push(key_name(hotkey.key));
    caps
}

fn key_name(key: Code) -> String {
    let symbol = match key {
        Code::Equal => "=",
        Code::Minus => "-",
        Code::Comma => ",",
        Code::Period => ".",
        Code::Semicolon => ";",
        Code::Slash => "/",
        Code::Backslash => "\\",
        Code::BracketLeft => "[",
        Code::BracketRight => "]",
        Code::Backquote => "`",
        Code::ArrowUp => "↑",
        Code::ArrowDown => "↓",
        Code::ArrowLeft => "←",
        Code::ArrowRight => "→",
        _ => "",
    };
    if !symbol.is_empty() {
        return symbol.to_owned();
    }
    let name = key.to_string();
    name.strip_prefix("Key").or_else(|| name.strip_prefix("Digit")).unwrap_or(&name).to_owned()
}

pub struct GlobalHotkeys {
    manager: GlobalHotKeyManager,
    events: Receiver<GlobalHotKeyEvent>,
    bound: Vec<(HotKey, Cmd)>,
    failed: Vec<Cmd>,
}

impl GlobalHotkeys {
    /// `wake` runs on every press: egui draws no frames on its own while the window is
    /// minimized or hidden, so nothing would read the press until it was shown again.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Result<Self, global_hotkey::Error> {
        let manager = GlobalHotKeyManager::new()?;
        let (tx, events) = unbounded();
        GlobalHotKeyEvent::set_event_handler(Some(move |event| {
            let _ = tx.send(event);
            wake();
        }));
        Ok(Self { manager, events, bound: Vec::new(), failed: Vec::new() })
    }

    /// Replaces what's registered with `bindings`. Another app holding a combination is
    /// common: the others still register, and [`Self::failed`] says which didn't.
    pub fn register(&mut self, bindings: &Bindings) {
        self.unregister_all();
        for cmd in Cmd::ALL {
            let combo = bindings.get(cmd);
            let result = combo.parse::<HotKey>().map_err(|e| e.to_string()).and_then(|hotkey| {
                self.manager.register(hotkey).map(|()| hotkey).map_err(|e| e.to_string())
            });
            match result {
                Ok(hotkey) => self.bound.push((hotkey, cmd)),
                Err(e) => {
                    log::warn!("could not register {combo} for {}: {e}", cmd.label());
                    self.failed.push(cmd);
                }
            }
        }
    }

    /// Lets go of every combination, and forgets presses not read yet.
    pub fn unregister_all(&mut self) {
        for (hotkey, _) in self.bound.drain(..) {
            if let Err(e) = self.manager.unregister(hotkey) {
                log::warn!("could not unregister {hotkey}: {e}");
            }
        }
        self.failed.clear();
        while self.events.try_recv().is_ok() {}
    }

    pub fn failed(&self, cmd: Cmd) -> bool {
        self.failed.contains(&cmd)
    }

    /// The commands pressed since the last call, oldest first.
    pub fn pressed(&self) -> Vec<Cmd> {
        self.events
            .try_iter()
            .filter(|e| e.state == HotKeyState::Pressed)
            .filter_map(|e| self.bound.iter().find(|(h, _)| h.id() == e.id).map(|(_, cmd)| *cmd))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings_round_trip_through_disk() {
        let dir = std::env::temp_dir().join(format!("ytm-rs-hotkeys-{}", std::process::id()));
        let mut bindings = Bindings::default();
        bindings.set(Cmd::Next, HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyJ).into_string());
        bindings.save(&dir).unwrap();
        assert_eq!(Bindings::load(&dir), bindings);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_file_gives_the_defaults() {
        assert_eq!(Bindings::load(Path::new("no-such-folder-for-ytm-rs")), Bindings::default());
    }

    #[test]
    fn a_partial_file_keeps_the_defaults_for_the_rest() {
        let bindings: Bindings = serde_json::from_str(r#"{ "next": "control+alt+KeyJ" }"#).unwrap();
        assert_eq!(bindings.get(Cmd::Next), "control+alt+KeyJ");
        assert_eq!(bindings.get(Cmd::Prev), Bindings::default().get(Cmd::Prev));
    }

    #[test]
    fn every_default_is_a_distinct_valid_combination() {
        let d = Bindings::default();
        for cmd in Cmd::ALL {
            let hotkey: HotKey = d.get(cmd).parse().unwrap();
            assert!(safe_to_grab(hotkey.mods));
            assert_eq!(d.used_by(hotkey, cmd), None);
        }
    }

    #[test]
    fn a_saved_combination_survives_its_own_string() {
        let hotkey = HotKey::new(Some(Modifiers::SHIFT | Modifiers::ALT), Code::Equal);
        assert_eq!(hotkey.into_string().parse::<HotKey>().unwrap().id(), hotkey.id());
    }

    #[test]
    fn used_by_finds_the_other_command_only() {
        let d = Bindings::default();
        let next: HotKey = d.get(Cmd::Next).parse().unwrap();
        assert_eq!(d.used_by(next, Cmd::Prev), Some(Cmd::Next));
        assert_eq!(d.used_by(next, Cmd::Next), None);
    }

    #[test]
    fn typing_keys_are_not_safe_to_grab() {
        assert!(!safe_to_grab(Modifiers::empty()));
        assert!(!safe_to_grab(Modifiers::SHIFT));
        assert!(safe_to_grab(Modifiers::SHIFT | Modifiers::ALT));
        assert!(safe_to_grab(Modifiers::CONTROL));
    }

    #[test]
    fn keycaps_read_like_the_keyboard() {
        let alt = if cfg!(target_os = "macos") { "Option" } else { "Alt" };
        assert_eq!(keycaps("shift+alt+KeyP"), [alt, "Shift", "P"]);
        assert_eq!(keycaps("control+Digit7"), ["Ctrl", "7"]);
        assert_eq!(keycaps("alt+Equal"), [alt, "="]);
        assert_eq!(keycaps("alt+F9"), [alt, "F9"]);
        assert_eq!(keycaps("not a key"), ["not a key"]);
    }
}
