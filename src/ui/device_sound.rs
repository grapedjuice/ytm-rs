//! Each output device keeps its own volume and equalizer: headphones and speakers rarely
//! want the same. Switching device in the app hands the one left its settings as they
//! are and gives the one picked what it had last time; the first time, it goes on with
//! the settings as they are. AirPlay speakers are left out: their volume is the
//! speaker's own, kept apart already.

use std::collections::HashMap;

use crate::audio::eq::Gains;

/// What an output device keeps.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Sound {
    pub volume: f32,
    pub eq_on: bool,
    pub eq_gains: Gains,
}

/// Each output device's settings, by its id ("" for the system's default device).
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct ByDevice(HashMap<String, Sound>);

impl ByDevice {
    /// Leaving device `from` (None: the system's default) with the settings `now` for
    /// `to`: `from` keeps `now`, and what `to` had comes back, if it has been played on
    /// before. Nothing changes going to or from an AirPlay speaker.
    pub fn switch(&mut self, from: Option<&str>, now: Sound, to: Option<&str>) -> Option<Sound> {
        let (from, to) = (key(from)?, key(to)?);
        self.0.insert(from.to_owned(), now);
        self.0.get(to).copied()
    }
}

/// The key a device's settings go by; None for an AirPlay speaker.
fn key(device: Option<&str>) -> Option<&str> {
    match device {
        Some(id) if id.starts_with(crate::airplay::PREFIX) => None,
        Some(id) => Some(id),
        None => Some(""),
    }
}

#[cfg(test)]
mod tests {
    use super::{ByDevice, Sound};

    fn sound(volume: f32, eq_on: bool) -> Sound {
        Sound { volume, eq_on, eq_gains: [if eq_on { 3.0 } else { 0.0 }; 10] }
    }

    #[test]
    fn each_device_gets_back_what_it_had() {
        let mut d = ByDevice::default();
        // A device played on for the first time goes on as things are.
        assert_eq!(d.switch(None, sound(0.8, false), Some("hdmi")), None);
        // The default device back: what it had when it was left.
        assert_eq!(d.switch(Some("hdmi"), sound(0.3, true), None), Some(sound(0.8, false)));
        assert_eq!(d.switch(None, sound(0.7, false), Some("hdmi")), Some(sound(0.3, true)));
        assert_eq!(d.switch(Some("hdmi"), sound(0.3, true), None), Some(sound(0.7, false)));
    }

    #[test]
    fn airplay_speakers_are_left_out() {
        let speaker = format!("{}living-room", crate::airplay::PREFIX);
        let mut d = ByDevice::default();
        assert_eq!(d.switch(None, sound(0.8, false), Some(&speaker)), None);
        assert_eq!(d.switch(Some(&speaker), sound(0.2, true), None), None);
        // Neither way was anything kept.
        assert_eq!(d.switch(None, sound(0.5, false), Some("hdmi")), None);
        assert_eq!(d.switch(Some("hdmi"), sound(0.4, false), None), Some(sound(0.5, false)));
    }
}
