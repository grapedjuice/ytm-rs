//! The listener's lyrics settings, the same ones the better-lyrics extension keeps:
//! source order (some switched off) with a per-song pick, translation language, and
//! timing offsets. Also the smoothed clock the lyrics follow.

use std::collections::HashMap;

use crate::lyrics::{Line, Lyrics, LyricsSet, Provider};

/// Offsets are seconds taken off the lyrics clock: positive shows lyrics later.
pub struct Prefs {
    pub order: Vec<(Provider, bool)>,
    pub target: String,
    pub global_offset: f32,
    pub word_trim: f32,
    pub line_trim: f32,
    /// "videoId|source" → offset.
    pub song_offsets: HashMap<String, f32>,
    /// videoId → source picked for that song.
    pub pins: HashMap<String, Provider>,
    /// Lyrics text size, as a multiple of the theme's own size.
    pub text_scale: f32,
}

/// The range and step of the lyrics text size setting.
pub const TEXT_SCALE: (f32, f32, f32) = (0.6, 1.8, 0.1);

/// Languages offered for translation.
pub const LANGUAGES: &[(&str, &str)] = &[
    ("en", "English"),
    ("es", "Spanish"),
    ("pt", "Portuguese"),
    ("fr", "French"),
    ("de", "German"),
    ("it", "Italian"),
    ("nl", "Dutch"),
    ("ru", "Russian"),
    ("uk", "Ukrainian"),
    ("pl", "Polish"),
    ("tr", "Turkish"),
    ("ar", "Arabic"),
    ("hi", "Hindi"),
    ("id", "Indonesian"),
    ("vi", "Vietnamese"),
    ("th", "Thai"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("zh-CN", "Chinese (Simplified)"),
    ("zh-TW", "Chinese (Traditional)"),
];

impl Prefs {
    pub fn load(get: &dyn Fn(&str) -> Option<String>) -> Self {
        // Stored like the extension's `preferredProviderList`: keys in order, "d_" = off;
        // sources it doesn't list are merged in next to their default neighbours.
        let mut order: Vec<(Provider, bool)> = get("lyric_providers")
            .unwrap_or_default()
            .split(',')
            .filter_map(|k| {
                let (k, on) = match k.strip_prefix("d_") {
                    Some(k) => (k, false),
                    None => (k, true),
                };
                Provider::from_key(k).map(|p| (p, on))
            })
            .collect();
        for (di, p) in Provider::ALL.iter().enumerate() {
            if order.iter().any(|(q, _)| q == p) {
                continue;
            }
            let at = Provider::ALL[..di]
                .iter()
                .rev()
                .find_map(|prev| order.iter().position(|(q, _)| q == prev).map(|i| i + 1))
                .or_else(|| Provider::ALL[di + 1..].iter().find_map(|next| order.iter().position(|(q, _)| q == next)))
                .unwrap_or(order.len());
            order.insert(at, (*p, true));
        }
        let num = |k: &str| get(k).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
        Self {
            order,
            target: get("lyric_lang").filter(|l| !l.is_empty()).unwrap_or_else(|| "en".into()),
            global_offset: num("lyric_offset"),
            word_trim: num("lyric_trim_word"),
            line_trim: num("lyric_trim_line"),
            text_scale: get("lyric_scale").and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0).clamp(TEXT_SCALE.0, TEXT_SCALE.1),
            song_offsets: get("lyric_song_offsets").and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
            pins: get("lyric_pins")
                .and_then(|s| serde_json::from_str::<HashMap<String, String>>(&s).ok())
                .map(|m| m.into_iter().filter_map(|(k, v)| Provider::from_key(&v).map(|p| (k, p))).collect())
                .unwrap_or_default(),
        }
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        let order: Vec<String> = self.order.iter().map(|(p, on)| if *on { p.key().to_owned() } else { format!("d_{}", p.key()) }).collect();
        storage.set_string("lyric_providers", order.join(","));
        storage.set_string("lyric_lang", self.target.clone());
        storage.set_string("lyric_offset", format!("{:.1}", self.global_offset));
        storage.set_string("lyric_trim_word", format!("{:.1}", self.word_trim));
        storage.set_string("lyric_trim_line", format!("{:.1}", self.line_trim));
        storage.set_string("lyric_scale", format!("{:.2}", self.text_scale));
        storage.set_string("lyric_song_offsets", serde_json::to_string(&self.song_offsets).unwrap_or_default());
        let pins: HashMap<&String, &str> = self.pins.iter().map(|(k, p)| (k, p.key())).collect();
        storage.set_string("lyric_pins", serde_json::to_string(&pins).unwrap_or_default());
    }

    /// Sources that have lyrics for this song, in the listener's order (switched-off ones
    /// left out). This is what the source counter counts through.
    pub fn available(&self, set: &LyricsSet) -> Vec<Provider> {
        self.order.iter().filter(|(p, on)| *on && set.get(*p).is_some_and(usable)).map(|(p, _)| *p).collect()
    }

    /// The source to show: the song's pick when it has lyrics, else the first in the
    /// listener's order that does (`orderByPin`, then fall through on failure).
    pub fn choose(&self, video_id: &str, set: &LyricsSet) -> Option<Provider> {
        let available = self.available(set);
        self.pins.get(video_id).filter(|p| available.contains(p)).copied().or_else(|| available.first().copied())
    }

    /// Pick a source for one song (`pinForPick`: the listener's own first choice needs no pin).
    pub fn pick(&mut self, video_id: &str, set: &LyricsSet, p: Provider) {
        if self.available(set).first() == Some(&p) {
            self.pins.remove(video_id);
        } else {
            self.pins.insert(video_id.to_owned(), p);
        }
    }

    pub fn song_offset(&self, video_id: &str, source: Provider) -> f32 {
        self.song_offsets.get(&format!("{video_id}|{}", source.key())).copied().unwrap_or(0.0)
    }

    pub fn set_song_offset(&mut self, video_id: &str, source: Provider, v: f32) {
        let k = format!("{video_id}|{}", source.key());
        let v = (v * 10.0).round() / 10.0;
        if v == 0.0 {
            self.song_offsets.remove(&k);
        } else {
            self.song_offsets.insert(k, v);
        }
    }

    /// Seconds to take off the clock for this song and source, as the extension does
    /// (`globalLyricOffset + lyricOffset` and the per-sync-type trim).
    pub fn offset(&self, video_id: &str, source: Provider, richsync: bool) -> f64 {
        (self.global_offset + self.song_offset(video_id, source) + if richsync { self.word_trim } else { self.line_trim }) as f64
    }
}

/// Lyrics worth showing: some line is sung (the extension's `hideInstrumentalOnly`).
pub fn usable(l: &Lyrics) -> bool {
    l.lines.iter().any(|x| !x.instrumental && !x.text.trim().is_empty())
}

/// `findBestLanguageMatch`: a translation shipped with the lyrics in the listener's language.
pub fn shipped_translation<'a>(l: &'a Line, target: &str) -> Option<&'a str> {
    let base = |s: &str| s.split(['-', '_']).next().unwrap_or_default().to_ascii_lowercase();
    l.translations
        .iter()
        .find(|(lang, _)| lang.eq_ignore_ascii_case(target))
        .or_else(|| l.translations.iter().find(|(lang, _)| base(lang) == base(target)))
        .map(|(_, t)| t.as_str())
}

/// The lyrics clock: the audio position smoothed so it never steps backwards while
/// playing (the output device takes audio in ~40 ms blocks, so the raw position jumps).
#[derive(Default)]
pub struct Clock {
    shown: f64,
    wall: f64,
    playing: bool,
}

impl Clock {
    /// `speed`: how much song goes by each second.
    pub fn tick(&mut self, pos: f64, playing: bool, now: f64, speed: f64) -> f64 {
        if !playing || !self.playing || self.wall == 0.0 {
            self.shown = pos;
        } else {
            let dt = (now - self.wall).max(0.0);
            let predicted = self.shown + dt * speed;
            let err = pos - predicted;
            if err.abs() > 0.25 {
                self.shown = pos;
            } else {
                // Ease toward the audio, never backwards.
                self.shown = (predicted + err * (dt * 6.0).min(1.0)).max(self.shown);
            }
        }
        self.wall = now;
        self.playing = playing;
        self.shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_order_merges_new_sources_and_keeps_disabled() {
        let get = |k: &str| (k == "lyric_providers").then(|| "lrclib-synced,d_bLyrics-richsynced".to_owned());
        let p = Prefs::load(&get);
        assert_eq!(p.order.len(), Provider::ALL.len());
        let i = |q: Provider| p.order.iter().position(|(x, _)| *x == q).unwrap();
        assert!(i(Provider::LrclibSynced) < i(Provider::BLyricsRich));
        assert!(!p.order[i(Provider::BLyricsRich)].1);
    }

    fn set(of: &[Provider]) -> LyricsSet {
        use crate::lyrics::Sync;
        let line = Line { start: 1000, dur: 1000, text: "la".into(), ..Default::default() };
        LyricsSet { sources: of.iter().map(|p| Lyrics { lines: vec![line.clone()], sync: Sync::Line, provider: *p, language: None, songwriters: vec![] }).collect() }
    }

    #[test]
    fn falls_back_through_the_order() {
        let p = Prefs::load(&|_| None);
        // The best source failed: the next one in order shows.
        let s = set(&[Provider::MusixmatchRich, Provider::LrclibSynced]);
        assert_eq!(p.choose("v", &s), Some(Provider::MusixmatchRich));
        // A source switched off is skipped.
        let mut p2 = Prefs::load(&|_| None);
        p2.order.iter_mut().find(|(x, _)| *x == Provider::MusixmatchRich).unwrap().1 = false;
        assert_eq!(p2.choose("v", &s), Some(Provider::LrclibSynced));
        // Lyrics with nothing sung don't count.
        let mut s2 = set(&[Provider::BLyricsRich, Provider::LrclibSynced]);
        s2.sources[0].lines[0].instrumental = true;
        assert_eq!(p.choose("v", &s2), Some(Provider::LrclibSynced));
        assert_eq!(p.choose("v", &LyricsSet::default()), None);
    }

    #[test]
    fn a_pick_wins_while_it_has_lyrics() {
        let mut p = Prefs::load(&|_| None);
        let s = set(&[Provider::BLyricsRich, Provider::LrclibSynced, Provider::MusixmatchSynced]);
        p.pick("v", &s, Provider::MusixmatchSynced);
        assert_eq!(p.choose("v", &s), Some(Provider::MusixmatchSynced));
        // Picking the first choice again needs no pin.
        p.pick("v", &s, Provider::BLyricsRich);
        assert!(p.pins.is_empty());
        // A pick whose lyrics failed this time falls back.
        p.pick("v", &s, Provider::MusixmatchSynced);
        let without = set(&[Provider::BLyricsRich, Provider::LrclibSynced]);
        assert_eq!(p.choose("v", &without), Some(Provider::BLyricsRich));
    }

    #[test]
    fn counter_counts_the_shown_source() {
        let mut p = Prefs::load(&|_| None);
        let s = set(&[Provider::BLyricsRich, Provider::PortatoRich, Provider::LrclibSynced]);
        let at = |p: &Prefs| {
            let a = p.available(&s);
            (a.iter().position(|x| Some(*x) == p.choose("v", &s)).unwrap() + 1, a.len())
        };
        assert_eq!(at(&p), (1, 3));
        p.pick("v", &s, Provider::PortatoRich);
        assert_eq!(at(&p), (2, 3));
        p.pick("v", &s, Provider::LrclibSynced);
        assert_eq!(at(&p), (3, 3));
    }

    #[test]
    fn clock_never_steps_back_while_playing() {
        let mut c = Clock::default();
        let a = c.tick(10.0, true, 100.0, 1.0);
        let b = c.tick(10.0, true, 100.016, 1.0);
        let d = c.tick(9.99, true, 100.032, 1.0);
        assert!(b >= a && d >= b);
        // A seek snaps.
        assert_eq!(c.tick(50.0, true, 100.05, 1.0), 50.0);
    }

    #[test]
    fn clock_keeps_up_when_sped_up() {
        let mut c = Clock::default();
        let dt = 1.0 / 60.0;
        let mut shown = 0.0;
        for i in 0..600 {
            let t = i as f64 * dt;
            shown = c.tick(10.0 + t * 2.0, true, 100.0 + t, 2.0);
        }
        let pos = 10.0 + 599.0 * dt * 2.0;
        assert!((pos - shown).abs() < 0.005, "{}", pos - shown);
    }
}
