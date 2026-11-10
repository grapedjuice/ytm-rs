//! OS media integration (Windows SMTC / MPRIS / macOS Now Playing) via souvlaki,
//! plus the previous / play-pause / next buttons on the Windows taskbar thumbnail.

#[cfg(windows)]
mod thumbbar;

use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig, SeekDirection};

/// How far a seek from the OS without an amount of its own goes (fast forward and rewind).
const SEEK_STEP: Duration = Duration::from_secs(10);

/// How far the position may wander while playing from where the OS has it before it's
/// sent again.
const DRIFT: Duration = Duration::from_secs(1);

/// The same where the OS keeps it still: there it falls behind as the song goes, and
/// souvlaki's MPRIS only takes about one update a second.
const STILL_DRIFT: Duration = Duration::from_secs(5);

/// How long the volume has to stay put before it's reported, so a slider drag, here or
/// in the OS's own, sends only where it ends: souvlaki's MPRIS takes about one update a
/// second, and the OS's slider would jump back to a value from mid-drag.
const VOLUME_SETTLE: Duration = Duration::from_millis(500);

/// Linux and the BSDs, where souvlaki goes through MPRIS.
const MPRIS: bool = cfg!(all(unix, not(any(target_os = "macos", target_os = "ios", target_os = "android"))));

/// Whether the OS moves the position on by itself while playing. souvlaki's MPRIS answers
/// a query with the position last sent, so there it's sent again as the song goes.
const MOVES_ON: bool = !MPRIS;

pub struct Media {
    controls: MediaControls,
    /// What was last sent, so the per-frame sync only reports changes.
    sent: Option<Sent>,
    /// New metadata went out, which the position has to follow.
    resend: bool,
    /// The play state last shown on the taskbar buttons.
    playing: Option<bool>,
    /// The app's volume, and since when (only MPRIS shows it).
    volume: Option<(f32, Instant)>,
    /// The volume last reported.
    reported: Option<f32>,
    /// The song the OS was last told about.
    track: Option<Track>,
    #[cfg(windows)]
    thumbbar: Option<Box<std::cell::RefCell<Option<thumbbar::ThumbBar>>>>,
}

impl Media {
    pub fn new(
        hwnd: Option<*mut std::ffi::c_void>,
        tx: Sender<MediaControlEvent>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Option<Self> {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let config = PlatformConfig { display_name: "YT Music", dbus_name: "ytm_rs", hwnd };
        let mut controls = MediaControls::new(config)
            .map_err(|e| log::warn!("media controls unavailable: {e:?}"))
            .ok()?;
        let (smtc_tx, smtc_wake) = (tx.clone(), wake.clone());
        controls
            .attach(move |ev| {
                let _ = smtc_tx.send(ev);
                smtc_wake();
            })
            .map_err(|e| log::warn!("media controls attach failed: {e:?}"))
            .ok()?;
        #[cfg(windows)]
        let thumbbar = hwnd.and_then(|h| thumbbar::ThumbBar::install(h, tx, wake));
        #[cfg(not(windows))]
        let _ = (tx, wake);
        Some(Self {
            controls,
            sent: None,
            resend: false,
            playing: None,
            volume: None,
            reported: None,
            track: None,
            #[cfg(windows)]
            thumbbar,
        })
    }

    pub fn set_track(&mut self, title: &str, artist: &str, album: Option<&str>, cover: Option<&str>, secs: Option<u32>) {
        self.track = Some(Track {
            title: title.into(),
            artist: artist.into(),
            album: album.map(Into::into),
            cover: cover.map(Into::into),
            listed: secs.is_some(),
            len: secs.map(|s| Duration::from_secs(s as u64)),
        });
        self.show();
    }

    /// The song's cover as a local file, once downloaded (where the OS can't fetch it).
    pub fn set_cover(&mut self, file: &str) {
        if let Some(t) = &mut self.track {
            t.cover = Some(file.into());
            self.show();
        }
    }

    /// The stream's length for the song, for one listed without a length. A length the
    /// player only falls back to (a music video's, standing in) isn't the song's.
    pub fn sync_length(&mut self, len: Option<Duration>) {
        if let Some(t) = &mut self.track
            && takes_length(t.listed, t.len, len)
        {
            t.len = len;
            self.show();
        }
    }

    fn show(&mut self) {
        if let Some(t) = &self.track {
            let _ = self.controls.set_metadata(MediaMetadata {
                title: Some(&t.title),
                artist: Some(&t.artist),
                album: t.album.as_deref(),
                cover_url: t.cover.as_deref(),
                duration: t.len,
            });
        }
        // Windows puts back the position it had before and macOS drops it: send it again.
        self.resend = true;
    }

    /// A song starts playing at `at`.
    pub fn start(&mut self, at: Duration) {
        self.send(true, at, Instant::now());
    }

    /// Called every frame with the player's state, so pausing or seeking from anywhere (a
    /// headset, the Windows media flyout, a song ending, a SponsorBlock skip) reaches the
    /// OS and the taskbar. The OS moves the position on by itself while playing, so it's
    /// only sent again when that goes wrong. `loading`: a song is starting, and the
    /// player's position is still the last one's.
    pub fn sync(&mut self, playing: bool, loading: bool, pos: Duration) {
        let now = Instant::now();
        if let Some(pos) = due(self.sent.as_ref(), self.resend, playing, loading, pos, now, MOVES_ON) {
            self.send(playing, pos, now);
        }
    }

    fn send(&mut self, playing: bool, pos: Duration, now: Instant) {
        self.resend = false;
        self.sent = Some(Sent { playing, pos, at: now });
        let progress = Some(MediaPosition(pos));
        let _ = self.controls.set_playback(if playing { MediaPlayback::Playing { progress } } else { MediaPlayback::Paused { progress } });
        if self.playing == Some(playing) {
            return;
        }
        self.playing = Some(playing);
        #[cfg(windows)]
        if let Some(bar) = self.thumbbar.as_ref().and_then(|c| c.try_borrow_mut().ok()).as_mut().and_then(|b| b.as_mut()) {
            bar.set_playing(playing);
        }
    }

    /// The app's volume (the slider's 0–1), where the OS shows it: only MPRIS does. Gives
    /// how soon to call again while a change settles.
    pub fn sync_volume(&mut self, volume: f32) -> Option<Duration> {
        if !MPRIS {
            return None;
        }
        let now = Instant::now();
        let since = match self.volume {
            Some((v, at)) if v == volume => at,
            _ => self.volume.insert((volume, now)).1,
        };
        let wait = volume_wait(self.reported, volume, since, now)?;
        if !wait.is_zero() {
            return Some(wait);
        }
        self.reported = Some(volume);
        #[cfg(all(unix, not(any(target_os = "macos", target_os = "ios", target_os = "android"))))]
        let _ = self.controls.set_volume(volume as f64);
        None
    }
}

/// The song as the OS was told it.
struct Track {
    title: String,
    artist: String,
    album: Option<String>,
    cover: Option<String>,
    /// The song came with a length, which the OS keeps: the player's can still be the
    /// last song's or a music video's when its stream doesn't say.
    listed: bool,
    len: Option<Duration>,
}

/// Whether the OS is to take the stream's `len` for a song: only one not `listed` with a
/// length, and when it differs (to the second) from what the OS has.
fn takes_length(listed: bool, shown: Option<Duration>, len: Option<Duration>) -> bool {
    !listed && len.is_some_and(|l| shown.is_none_or(|s| s.as_secs() != l.as_secs()))
}

/// What the OS was told: playing or paused, at `pos`, at `at`.
struct Sent {
    playing: bool,
    pos: Duration,
    at: Instant,
}

impl Sent {
    /// Where the OS has the position by `now`. `moves_on`: it counts on while playing.
    fn shown(&self, now: Instant, moves_on: bool) -> Duration {
        if self.playing && moves_on { self.pos + now.saturating_duration_since(self.at) } else { self.pos }
    }
}

/// The position to send, if the OS needs telling. `resend`: new metadata went out.
fn due(sent: Option<&Sent>, resend: bool, playing: bool, loading: bool, pos: Duration, now: Instant, moves_on: bool) -> Option<Duration> {
    // Until the song plays, the OS keeps the position `start` gave it.
    let pos = match sent {
        Some(s) if loading => s.shown(now, moves_on),
        _ => pos,
    };
    (resend || stale(sent, playing, pos, now, moves_on)).then_some(pos)
}

/// Whether the OS needs telling: nothing sent yet, play or pause, or a position away
/// from where the OS has it by now (a seek, a stall). Paused, any move is a seek.
fn stale(sent: Option<&Sent>, playing: bool, pos: Duration, now: Instant, moves_on: bool) -> bool {
    sent.is_none_or(|s| {
        let slack = match (s.playing, moves_on) {
            (false, _) => Duration::ZERO,
            (true, true) => DRIFT,
            (true, false) => STILL_DRIFT,
        };
        s.playing != playing || pos.abs_diff(s.shown(now, moves_on)) > slack
    })
}

/// How long until `volume`, unchanged `since`, is to be reported (zero: now). `None` if
/// the OS has it already.
fn volume_wait(reported: Option<f32>, volume: f32, since: Instant, now: Instant) -> Option<Duration> {
    (reported != Some(volume)).then(|| VOLUME_SETTLE.saturating_sub(now.saturating_duration_since(since)))
}

/// Where a seek from the OS lands: at a position, or by an amount from `pos`, kept
/// within the song's `len`. `None` for other events.
pub fn seek_target(ev: &MediaControlEvent, pos: Duration, len: Duration) -> Option<Duration> {
    let by = |dir, step| match dir {
        SeekDirection::Forward => pos.saturating_add(step),
        SeekDirection::Backward => pos.saturating_sub(step),
    };
    let to = match *ev {
        MediaControlEvent::SetPosition(MediaPosition(at)) => at,
        MediaControlEvent::Seek(dir) => by(dir, SEEK_STEP),
        MediaControlEvent::SeekBy(dir, step) => by(dir, step),
        _ => return None,
    };
    Some(to.min(len))
}

/// The slider's volume for one the OS asked for, which can be anything.
pub fn volume_from_os(v: f64) -> Option<f32> {
    v.is_finite().then(|| v.clamp(0.0, 1.0) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(secs: u64) -> Duration {
        Duration::from_secs(secs)
    }

    #[test]
    fn seeks_land_within_the_song() {
        let to = |ev, pos| seek_target(&ev, s(pos), s(200));
        assert_eq!(to(MediaControlEvent::SetPosition(MediaPosition(s(42))), 100), Some(s(42)));
        assert_eq!(to(MediaControlEvent::SetPosition(MediaPosition(s(500))), 100), Some(s(200)));
        assert_eq!(to(MediaControlEvent::Seek(SeekDirection::Forward), 100), Some(s(110)));
        assert_eq!(to(MediaControlEvent::Seek(SeekDirection::Backward), 5), Some(s(0)));
        assert_eq!(to(MediaControlEvent::SeekBy(SeekDirection::Forward, s(30)), 195), Some(s(200)));
        assert_eq!(to(MediaControlEvent::SeekBy(SeekDirection::Backward, s(30)), 100), Some(s(70)));
        assert_eq!(to(MediaControlEvent::Next, 100), None);
    }

    #[test]
    fn volume_is_reported_once_it_settles() {
        let t = Instant::now();
        let ms = Duration::from_millis;
        assert_eq!(volume_wait(None, 0.5, t, t), Some(ms(500)));
        assert_eq!(volume_wait(Some(0.4), 0.5, t, t + ms(200)), Some(ms(300)));
        assert_eq!(volume_wait(Some(0.4), 0.5, t, t + s(1)), Some(Duration::ZERO));
        assert_eq!(volume_wait(Some(0.5), 0.5, t, t), None);
    }

    #[test]
    fn the_streams_length_fills_in_for_a_song_listed_without_one() {
        let ms = Duration::from_millis;
        assert!(takes_length(false, None, Some(ms(160_400))));
        assert!(takes_length(false, Some(s(158)), Some(ms(160_400))));
        assert!(!takes_length(false, Some(s(160)), Some(ms(160_400))));
        assert!(!takes_length(false, None, None));
        // A listed length stays, whatever the stream says.
        assert!(!takes_length(true, Some(s(181)), Some(s(300))));
    }

    #[test]
    fn a_loading_song_keeps_its_start_and_new_metadata_resends() {
        let t = Instant::now();
        let started = Sent { playing: true, pos: s(0), at: t };
        // Loading, the player still has the last song's 3:00: the OS keeps the start.
        assert_eq!(due(Some(&started), false, true, true, s(180), t + s(2), true), None);
        assert_eq!(due(Some(&started), false, true, true, s(180), t + s(2), false), None);
        // New metadata mid-load: the start point again, not the last song's position.
        assert_eq!(due(Some(&started), true, true, true, s(180), t + s(2), true), Some(s(2)));
        assert_eq!(due(Some(&started), true, true, true, s(180), t + s(2), false), Some(s(0)));
        // Playing, it goes by the player.
        assert_eq!(due(Some(&started), true, true, false, s(1), t + s(2), true), Some(s(1)));
        assert_eq!(due(None, false, false, false, s(30), t, true), Some(s(30)));
    }

    #[test]
    fn os_volume_is_kept_to_the_slider() {
        assert_eq!(volume_from_os(0.5), Some(0.5));
        assert_eq!(volume_from_os(1.7), Some(1.0));
        assert_eq!(volume_from_os(-0.2), Some(0.0));
        assert_eq!(volume_from_os(f64::NAN), None);
    }

    #[test]
    fn position_is_sent_only_when_the_os_has_it_wrong() {
        let t = Instant::now();
        let playing = Sent { playing: true, pos: s(10), at: t };
        let paused = Sent { playing: false, pos: s(10), at: t };
        assert!(stale(None, true, s(0), t, true));
        // Playing on as the OS expects, or near enough.
        assert!(!stale(Some(&playing), true, s(15), t + s(5), true));
        assert!(!stale(Some(&playing), true, Duration::from_millis(14_500), t + s(5), true));
        // A seek, or a stall the OS kept counting through.
        assert!(stale(Some(&playing), true, s(60), t + s(5), true));
        assert!(stale(Some(&playing), true, s(10), t + s(5), true));
        // Paused stays put until it's moved, however little.
        assert!(!stale(Some(&paused), false, s(10), t + s(60), true));
        assert!(stale(Some(&paused), false, s(30), t + s(60), true));
        assert!(stale(Some(&paused), false, s(11), t, true));
        assert!(stale(Some(&paused), false, Duration::from_millis(9_500), t, true));
        assert!(stale(Some(&paused), false, s(11), t, false));
        // Play and pause always go.
        assert!(stale(Some(&paused), true, s(10), t, true));
        assert!(stale(Some(&playing), false, s(10), t, true));
    }

    #[test]
    fn position_is_sent_as_it_goes_where_the_os_keeps_it_still() {
        let t = Instant::now();
        let playing = Sent { playing: true, pos: s(10), at: t };
        assert!(!stale(Some(&playing), true, s(15), t + s(5), false));
        assert!(stale(Some(&playing), true, Duration::from_millis(15_100), t + s(5), false));
        // A seek further than that still goes.
        assert!(stale(Some(&playing), true, s(60), t + s(1), false));
        assert_eq!(playing.shown(t + s(5), false), s(10));
        assert_eq!(playing.shown(t + s(5), true), s(15));
    }
}
