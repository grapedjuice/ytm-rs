//! Where the music plays, "Play on": one of this computer's sound cards, or speakers on the
//! network. Those come in kinds (`Kind`), each its own way of playing (AirPlay for now;
//! Sonos and Chromecast to come). A device found several ways (a Sonos speaker answers
//! AirPlay too) is one `Receiver` with several `ways`, known by its MAC address.
//!
//! A network destination is a group of speakers played on together, one kind of them
//! only: their own system keeps them in step, which nothing could across systems. For now
//! a group is always one speaker.

use std::time::Duration;

use crate::airplay;

/// A system network speakers play by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    AirPlay,
}

impl Kind {
    /// Most wanted first: a speaker found several ways plays the first of them it has.
    pub const ALL: [Self; 1] = [Self::AirPlay];

    fn key(self) -> &'static str {
        match self {
            Self::AirPlay => "airplay",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.key() == key)
    }

    /// Its name, which isn't translated.
    pub fn label(self) -> &'static str {
        match self {
            Self::AirPlay => "AirPlay",
        }
    }

    pub fn icon(self) -> char {
        match self {
            Self::AirPlay => crate::theme::icon::AIRPLAY,
        }
    }

    /// How long after it's sent a speaker of this kind plays the music.
    pub fn latency(self) -> Duration {
        match self {
            Self::AirPlay => airplay::LATENCY,
        }
    }
}

/// Where the music plays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Destination {
    /// A sound card, by cpal's id; `None`, Windows' default (whichever that is now).
    Local(Option<String>),
    /// Speakers on the network (`Receiver::id`s), played on together the `kind` way.
    Remote { kind: Kind, members: Vec<String> },
}

impl Default for Destination {
    fn default() -> Self {
        Self::Local(None)
    }
}

impl Destination {
    /// From what `key` saved, and an AirPlay speaker's `airplay::PREFIX` and mDNS name,
    /// as saved before "Play on".
    pub fn parse(saved: &str) -> Self {
        if saved.is_empty() {
            return Self::default();
        }
        if let Some((kind, members)) = saved.strip_prefix("remote:").and_then(|r| r.split_once(':'))
            && let Some(kind) = Kind::from_key(kind)
        {
            let members: Vec<String> = members.split(',').filter(|m| !m.is_empty()).map(str::to_owned).collect();
            return if members.is_empty() { Self::default() } else { Self::Remote { kind, members } };
        }
        if let Some(name) = saved.strip_prefix(airplay::PREFIX) {
            return Self::Remote { kind: Kind::AirPlay, members: vec![airplay::device_id(name)] };
        }
        // A kind this version doesn't know, saved by a newer one: not a sound card.
        if saved.starts_with("remote:") {
            return Self::default();
        }
        Self::Local(Some(saved.to_owned()))
    }

    /// What to save, for `parse`. A sound card saves as it always has: nothing for Windows'
    /// default, and cpal's id (never "remote:" or "airplay:" anything) for one picked.
    pub fn key(&self) -> String {
        match self {
            Self::Local(None) => String::new(),
            Self::Local(Some(id)) => id.clone(),
            Self::Remote { kind, members } => format!("remote:{}:{}", kind.key(), members.join(",")),
        }
    }

    pub fn kind(&self) -> Option<Kind> {
        match self {
            Self::Local(_) => None,
            Self::Remote { kind, .. } => Some(*kind),
        }
    }

    pub fn is_remote(&self) -> bool {
        self.kind().is_some()
    }

    /// Whether `id` (a sound card's, or a `Receiver`'s) is in it.
    pub fn has(&self, id: &str) -> bool {
        match self {
            Self::Local(card) => card.as_deref() == Some(id),
            Self::Remote { members, .. } => members.iter().any(|m| m == id),
        }
    }

    /// Plays on this one network speaker alone, the best way it has.
    pub fn speaker(r: &Receiver) -> Self {
        Self::Remote { kind: r.best(), members: vec![r.id.clone()] }
    }
}

/// One way of playing on a network speaker.
#[derive(Clone, Debug, PartialEq)]
pub enum Way {
    AirPlay(airplay::Speaker),
}

impl Way {
    pub fn kind(&self) -> Kind {
        match self {
            Self::AirPlay(_) => Kind::AirPlay,
        }
    }
}

/// A speaker on the network.
#[derive(Clone, Debug, PartialEq)]
pub struct Receiver {
    /// Its MAC address (`airplay::device_id`), the same whichever way it's found.
    pub id: String,
    pub name: String,
    /// How it can be played on: at least one way, in `Kind::ALL`'s order.
    pub ways: Vec<Way>,
}

impl Receiver {
    fn way(&self, kind: Kind) -> Option<&Way> {
        self.ways.iter().find(|w| w.kind() == kind)
    }

    /// The way it plays best, alone.
    pub fn best(&self) -> Kind {
        self.ways.first().map_or(Kind::AirPlay, Way::kind)
    }
}

/// The kind to play `members` together by: the most wanted one they all have.
pub fn shared_kind(members: &[Receiver]) -> Option<Kind> {
    Kind::ALL.into_iter().find(|&k| all_have(members, k))
}

/// Whether `members` (at least one) can all be played the `kind` way.
pub fn all_have(members: &[Receiver], kind: Kind) -> bool {
    !members.is_empty() && members.iter().all(|r| r.way(kind).is_some())
}

/// Settings › Find speakers on your network. Off, nothing listens on the network, so the
/// firewall never asks; the speakers found are forgotten.
pub fn set_allowed(on: bool) {
    airplay::set_allowed(on);
}

/// Starts looking for speakers, if that's allowed: they come and go in `receivers()`.
pub fn discover() {
    airplay::discover();
}

/// The speakers on the network, by name: each device once, however many ways it's found.
pub fn receivers() -> Vec<Receiver> {
    let mut all: Vec<Receiver> = Vec::new();
    let found = airplay::speakers().into_iter().map(|s| (s.id.clone(), s.name.clone(), Way::AirPlay(s)));
    for (id, name, way) in found {
        match all.iter_mut().find(|r| r.id == id) {
            Some(r) => r.ways.push(way),
            None => all.push(Receiver { id, name, ways: vec![way] }),
        }
    }
    for r in &mut all {
        r.ways.sort_by_key(|w| Kind::ALL.iter().position(|&k| k == w.kind()));
    }
    all.sort_by_key(|r| r.name.to_lowercase());
    all
}

/// The speakers of `members` that are on the network, if all are.
pub fn find_all(members: &[String]) -> Option<Vec<Receiver>> {
    let all = receivers();
    members.iter().map(|m| all.iter().find(|r| &r.id == m).cloned()).collect()
}

/// A network destination connected to, ready to play.
pub enum Session {
    AirPlay(airplay::Session),
}

impl Session {
    /// Connects to `members` the `kind` way, which takes a while (seconds, for an AirPlay 2
    /// speaker's clock): not on the audio thread.
    pub fn connect(kind: Kind, members: &[Receiver]) -> anyhow::Result<Self> {
        match kind {
            // One speaker: grouping AirPlay speakers isn't offered yet.
            Kind::AirPlay => {
                let Way::AirPlay(speaker) = members.iter().find_map(|r| r.way(kind)).ok_or_else(|| anyhow::anyhow!("no AirPlay speaker"))?;
                airplay::Session::connect(speaker).map(Self::AirPlay)
            }
        }
    }

    /// The speaker's own volume when connected to, 0 to 1, if it said.
    pub fn volume(&self) -> Option<f32> {
        match self {
            Self::AirPlay(s) => s.volume(),
        }
    }

    /// Plays `source` until the returned `Stream` is dropped; `lost` is called once if
    /// the speaker goes away.
    pub fn play<S>(self, source: S, lost: impl Fn() + Send + 'static) -> anyhow::Result<Stream>
    where
        S: rodio::Source + Send + 'static,
    {
        match self {
            Self::AirPlay(s) => s.play(source, lost).map(Stream::AirPlay),
        }
    }
}

/// Why connecting to or playing on a network speaker failed, for people.
pub fn describe(kind: Kind, e: &anyhow::Error) -> String {
    match kind {
        Kind::AirPlay => airplay::describe(e),
    }
}

/// Speakers playing. Dropping it stops the music there and lets them go.
pub enum Stream {
    AirPlay(airplay::Stream),
}

impl Stream {
    /// Drops what the speakers have buffered, so what's next is heard now.
    pub fn flush(&self) {
        match self {
            Self::AirPlay(s) => s.flush(),
        }
    }

    /// Whether they pause themselves, keeping what they have (AirPlay 2): playing again
    /// picks up where they stopped, with no flush.
    pub fn pauses(&self) -> bool {
        match self {
            Self::AirPlay(s) => s.pauses(),
        }
    }

    pub fn pause(&self) {
        match self {
            Self::AirPlay(s) => s.pause(),
        }
    }

    pub fn resume(&self) {
        match self {
            Self::AirPlay(s) => s.resume(),
        }
    }

    /// Sets the speakers' own volume, 0 to 1.
    pub fn set_volume(&self, v: f32) {
        match self {
            Self::AirPlay(s) => s.set_volume(v),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_was_saved_before_play_on_still_reads() {
        assert_eq!(Destination::parse(""), Destination::Local(None));
        let card = "{0.0.0.00000000}.{5f3c3a1e-1f1e-4b8f-9d4e-1a2b3c4d5e6f}";
        assert_eq!(Destination::parse(card), Destination::Local(Some(card.into())));
        assert_eq!(
            Destination::parse("airplay:5CAAFD0A1B2C@Living Room._raop._tcp.local."),
            Destination::Remote { kind: Kind::AirPlay, members: vec!["5caafd0a1b2c".into()] }
        );
    }

    #[test]
    fn a_destination_saves_and_reads_back() {
        let all = [
            Destination::Local(None),
            Destination::Local(Some("wasapi:{abc}".into())),
            Destination::Remote { kind: Kind::AirPlay, members: vec!["5caafd0a1b2c".into()] },
            Destination::Remote { kind: Kind::AirPlay, members: vec!["5caafd0a1b2c".into(), "a1b2c3d4e5f6".into()] },
        ];
        for d in all {
            assert_eq!(Destination::parse(&d.key()), d, "{:?}", d.key());
        }
        // A sound card saves as it always did, so nothing changes for anyone on one.
        assert_eq!(Destination::Local(None).key(), "");
        assert_eq!(Destination::Local(Some("wasapi:{abc}".into())).key(), "wasapi:{abc}");
        // Something unreadable plays on the default.
        assert_eq!(Destination::parse("remote:airplay:"), Destination::Local(None));
        // A kind from a newer version isn't taken for a sound card.
        assert_eq!(Destination::parse("remote:sonos:rincon_5caafd0a1b2c"), Destination::Local(None));
    }
}
