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

/// The least time between volume reports, so a slider drag doesn't queue one a frame.
const VOLUME_EVERY: Duration = Duration::from_secs(2);

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
    /// The volume last reported, and when (only MPRIS shows the app's volume).
    volume: Option<(f32, Instant)>,
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
            #[cfg(windows)]
            thumbbar,
        })
    }

    pub fn set_track(&mut self, title: &str, artist: &str, album: Option<&str>, cover: Option<&str>, secs: Option<u32>) {
        let _ = self.controls.set_metadata(MediaMetadata {
            title: Some(title),
            artist: Some(artist),
            album,
            cover_url: cover,
            duration: secs.map(|s| Duration::from_secs(s as u64)),
        });
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
        // Until the song plays, the OS keeps the position `start` gave it.
        let pos = match &self.sent {
            Some(s) if loading => s.shown(now, MOVES_ON),
            _ => pos,
        };
        if self.resend || stale(self.sent.as_ref(), playing, pos, now, MOVES_ON) {
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
    /// how soon to call again when a change has to wait its turn.
    pub fn sync_volume(&mut self, volume: f32) -> Option<Duration> {
        if !MPRIS {
            return None;
        }
        let now = Instant::now();
        let wait = volume_wait(self.volume, volume, now)?;
        if !wait.is_zero() {
            return Some(wait);
        }
        self.volume = Some((volume, now));
        #[cfg(all(unix, not(any(target_os = "macos", target_os = "ios", target_os = "android"))))]
        let _ = self.controls.set_volume(volume as f64);
        None
    }
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

/// Whether `volume` is to be reported, given the last report: `None` if the OS has it
/// already, else how long until it may go (zero: now).
fn volume_wait(last: Option<(f32, Instant)>, volume: f32, now: Instant) -> Option<Duration> {
    match last {
        Some((v, _)) if v == volume => None,
        Some((_, at)) => Some(VOLUME_EVERY.saturating_sub(now.saturating_duration_since(at))),
        None => Some(Duration::ZERO),
    }
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
    fn volume_is_reported_at_most_every_so_often() {
        let t = Instant::now();
        assert_eq!(volume_wait(None, 0.5, t), Some(Duration::ZERO));
        assert_eq!(volume_wait(Some((0.5, t)), 0.5, t + s(9)), None);
        assert_eq!(volume_wait(Some((0.5, t)), 0.6, t + Duration::from_millis(500)), Some(Duration::from_millis(1_500)));
        assert_eq!(volume_wait(Some((0.5, t)), 0.6, t + s(3)), Some(Duration::ZERO));
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
