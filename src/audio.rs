//! Audio engine: one thread owns the output device and a rodio `Player`.
//!
//! The UI talks to it through `AudioHandle`; status flows back through `AudioStatus`
//! (lock-free reads for the progress bar) plus a `Finished` event for auto-advance.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use rodio::cpal::traits::{DeviceTrait, HostTrait};
use rodio::mixer::{Mixer, MixerSource};
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player};

use crate::destination::{self, Destination, Kind};
use crate::stream::{self, Shared};

pub use crate::stream::TrackDecoder;

pub mod eq;
pub mod limiter;
pub mod spectrum;
pub mod stereo;
pub mod tempo;

pub enum AudioCmd {
    /// A decoder is ready for playback generation `generation`; stale generations are dropped.
    /// A decoder started mid-song (`at` > 0) is the seekable kind.
    Start { generation: u64, decoder: TrackDecoder, shared: Arc<Shared>, at: Duration },
    /// Queue the next song's pre-warmed `Tempo` source directly behind `after` for gapless playback.
    QueueNext { after: u64, generation: u64, tempo: Box<tempo::Tempo<TrackDecoder>>, song_ms: Arc<AtomicU64>, shared: Arc<Shared>, fallback_duration_ms: u64 },
    /// Drop any queued gapless next track that hasn't started yet (queue/settings changed).
    ClearNext,
    /// Internal: the queued gapless track `generation` just started pulling samples.
    GaplessStarted(u64),
    /// A decoder positioned at `at`, replacing the current one; only the latest seek's lands.
    Replace { generation: u64, seek: u64, decoder: TrackDecoder, at: Duration, seekable: bool },
    Pause,
    Resume,
    Stop,
    /// Another song is on its way: with crossfade on, fade this one out instead of
    /// stopping it, and fade the next one in when it starts.
    Handoff,
    Seek(Duration),
    /// A song was asked for: open the device now, while its stream is still on the way.
    Warm,
    /// Seek to where `pos` will have moved on to by the time the seek lands (`pos` was
    /// current at `at`): following someone else's playback (listen together).
    SeekLive { pos: Duration, at: std::time::Instant },
    Volume(f32),
    /// The output device failed (unplugged, headphones disconnected): sent by the
    /// device's error callback so the audio thread reopens one.
    DeviceLost,
    /// Play there: a sound card whenever it's connected (Windows' default otherwise), or
    /// network speakers whenever they're found.
    SetDestination(Destination),
    /// Pause at this moment (the sleep timer); None calls it off. Kept here rather than
    /// in the UI, which may not draw a frame while the window is in the tray.
    SleepAt(Option<Instant>),
    /// A setting the players' gain depends on changed (normalization, the slider's curve),
    /// or something's just to be looked at again (the pause fade): sets it afresh, leaving
    /// a speaker's own volume alone.
    Refresh,
    /// Connecting to network speakers (`key`, called `name`) is done, on a thread of its
    /// own so the engine stays responsive meanwhile.
    /// `attempt` is `Output::connecting`'s, so an older one for the same speakers (picked,
    /// then another, then them again) isn't taken for it.
    Connected { attempt: u64, name: String, kind: Kind, session: anyhow::Result<Box<destination::Session>> },
}

/// How the network speakers picked are doing.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum RemoteState {
    /// None picked, or none asked for yet.
    #[default]
    Idle,
    Connecting,
    Playing,
    /// Couldn't connect, or they went: why, for people. The sound card plays meanwhile.
    Failed(String),
}

pub enum AudioEvent {
    Finished { generation: u64 },
    /// Close enough to the end that the next song should start, to crossfade into it.
    Ending { generation: u64 },
    /// Close enough to the end that the next song's decoder should be staged for gapless handoff.
    PrepareNext { generation: u64 },
    /// The staged gapless track `generation` is now playing seamlessly after the previous one.
    GaplessAdvanced { generation: u64 },
    /// The song's download failed, so playback stopped at `at` (or a seek to `at`
    /// couldn't be served). Reopening the song there resolves a fresh stream.
    Interrupted { generation: u64, at: Duration, failure: stream::DownloadFailure },
    Error(String),
}

static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

fn now_ms() -> u64 {
    EPOCH.elapsed().as_millis() as u64
}

#[derive(Default)]
pub struct AudioStatus {
    pub position_ms: AtomicU64,
    /// When `position_ms` was sampled (ms since EPOCH), for smooth extrapolation.
    sampled_at: AtomicU64,
    pub duration_ms: AtomicU64,
    pub playing: AtomicBool,
    /// Generation the UI most recently asked for; the engine ignores older decoders.
    pub wanted_gen: AtomicU64,
    /// Generation of the gapless next track currently staged by the UI (0 = none).
    pub staged_gen: AtomicU64,
    /// Generation the engine last started, set once its length is in.
    started_gen: AtomicU64,
    /// The length its stream gave, 0 when it gave none (`duration_ms` then keeps the UI's).
    stream_ms: AtomicU64,
    /// f32 bits
    volume: AtomicU32,
    /// The app's own volume, kept aside while an AirPlay speaker's is on the slider: its
    /// f32 bits with bit 32 set, 0 when there's none (see `kept`).
    kept_volume: AtomicU64,
    /// The `Normalize` level songs are brought to.
    normalize: AtomicU8,
    /// The limiter's on, for songs normalization turns up.
    limit: Arc<AtomicBool>,
    /// What normalization does to the playing song, in dB (f32 bits); NaN when it does
    /// nothing (off, or YouTube gave no loudness for it).
    norm_db: AtomicU32,
    /// Current normalization gain being applied by the active `Meter` (f32 bits; 0 = unset),
    /// so a gapless next track glides smoothly from the previous song's gain instead of stepping.
    active_norm: AtomicU32,
    /// The volume slider follows loudness (cubic) rather than amplitude.
    log_volume: AtomicBool,
    /// Smoothed output loudness 0..1 (f32 bits), for audio-reactive visuals.
    level: AtomicU32,
    /// The visualizer's bars (see `spectrum`), measured only while it's on.
    spectrum: spectrum::Bars,
    spectrum_on: AtomicBool,
    /// How far the output device runs behind the position (its buffer), in ms.
    latency_ms: AtomicU32,
    /// How the network speakers picked are doing, for "Play on" to show.
    remote: std::sync::Mutex<RemoteState>,
    /// Crossfade length in ms; 0 = off.
    crossfade_ms: AtomicU32,
    /// The sleep timer waits for the song's end: nothing runs into the next song (no
    /// crossfade's `Ending`), so it plays out and ends with `Finished`. Anything that would
    /// start the next song early has to check this.
    pub stop_at_end: AtomicBool,
    /// Chain consecutive songs sample-accurately without a gap (Settings â€º Playback).
    gapless: AtomicBool,
    /// Paused, or on the way there: songs fade to silence, and back up when it's cleared
    /// (see `Meter`).
    hushed: AtomicBool,
    /// How far that fade has turned the playing song down, 0..1 (f32 bits), for a decoder
    /// put in meanwhile (a seek's) to carry on from.
    faded: AtomicU32,
    /// Counts the players pausing outright: a song coming back from that fades in from
    /// silence, whatever its fade had got to (a pause cut short by a stalled download, or
    /// by headphones unplugged).
    silenced: AtomicU32,
    /// Pausing and playing again are instant, with no fade (Settings â€º Sound).
    instant_pause: AtomicBool,
    /// A song ends where it goes silent for good (Settings â€º Playback).
    skip_silence: AtomicBool,
    /// The equalizer's settings, applied to everything on its way to the device.
    eq: Arc<eq::EqParams>,
    /// Crossfeed, mono and balance, after the equalizer.
    stereo: Arc<stereo::StereoParams>,
    /// Playback speed, applied to each song as it plays.
    speed: Arc<tempo::Speed>,
}

impl AudioStatus {
    /// Not connecting to the speakers, nor playing on them, any more: the menu says neither.
    /// Why they failed, if they did, still holds.
    fn remote_gone(&self) {
        let mut remote = self.remote.lock().unwrap();
        if matches!(*remote, RemoteState::Connecting | RemoteState::Playing) {
            *remote = RemoteState::Idle;
        }
    }

    pub fn speed(&self) -> f32 {
        self.speed.get()
    }

    pub fn speed_arc(&self) -> Arc<tempo::Speed> {
        self.speed.clone()
    }

    pub fn position(&self) -> Duration {
        Duration::from_millis(self.position_ms.load(Ordering::Relaxed))
    }

    /// Position extrapolated to now while playing, so progress moves smoothly between
    /// the engine's 100 ms updates.
    pub fn position_smooth(&self) -> Duration {
        let pos = self.position_ms.load(Ordering::Relaxed);
        if !self.playing.load(Ordering::Relaxed) {
            return Duration::from_millis(pos);
        }
        let age = now_ms().saturating_sub(self.sampled_at.load(Ordering::Relaxed)).min(250);
        Duration::from_millis(pos + (age as f32 * self.speed()) as u64)
    }

    pub fn duration(&self) -> Option<Duration> {
        let d = self.duration_ms.load(Ordering::Relaxed);
        (d > 0).then(|| Duration::from_millis(d))
    }

    /// Whether the song asked for as `generation` is playing, not the one before it still
    /// going until the engine gets to the new one.
    pub fn started(&self, generation: u64) -> bool {
        self.started_gen.load(Ordering::Acquire) == generation && self.playing.load(Ordering::Relaxed)
    }

    /// The length the song asked for as `generation` has by its stream, once started.
    pub fn stream_duration(&self, generation: u64) -> Option<Duration> {
        // Read after `started`, so the length is that song's, not the one before.
        if !self.started(generation) {
            return None;
        }
        let d = self.stream_ms.load(Ordering::Relaxed);
        (d > 0).then(|| Duration::from_millis(d))
    }

    /// How far the speakers run behind the position (the output buffer), on the song's
    /// clock: more of the song at a faster speed.
    pub fn latency(&self) -> Duration {
        Duration::from_millis(self.latency_ms.load(Ordering::Relaxed) as u64).mul_f32(self.speed())
    }

    /// The app's own volume while a speaker's is on the slider.
    fn kept(&self) -> Option<f32> {
        let v = self.kept_volume.load(Ordering::Relaxed);
        (v != 0).then(|| f32::from_bits(v as u32))
    }

    fn keep(&self, v: Option<f32>) {
        self.kept_volume.store(v.map_or(0, |v| 1 << 32 | u64::from(v.to_bits())), Ordering::Relaxed);
    }

    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    /// The visualizer's bars, 0..1 each, low to high.
    pub fn spectrum(&self) -> [f32; spectrum::BARS] {
        std::array::from_fn(|i| f32::from_bits(self.spectrum[i].load(Ordering::Relaxed)))
    }

    fn set_position(&self, ms: u64) {
        self.position_ms.store(ms, Ordering::Relaxed);
        self.sampled_at.store(now_ms(), Ordering::Relaxed);
    }

    /// The position back to what the device has played of `pos` (its buffer not yet heard).
    fn set_heard(&self, pos: Duration) -> Duration {
        let heard = pos.saturating_sub(self.latency());
        self.set_position(heard.as_millis() as u64);
        heard
    }

    /// The players are pausing outright: they come back up from silence.
    fn silence(&self) {
        self.faded.store(1f32.to_bits(), Ordering::Relaxed);
        self.silenced.fetch_add(1, Ordering::Relaxed);
    }
}

/// A crossfade ramp in progress, in samples. Linear, like Strawberry's and
/// Liquidsoap's crossfades: the outgoing song gets steadily quieter as the next one
/// comes up (an equal-power curve kept it near full volume for half the fade).
#[derive(Clone, Copy)]
enum Ramp {
    In { at: u32, len: u32 },
    /// Down from `from` (a fade-in may have been cut short) to silence, then the source
    /// ends. Held at `from` for the first `hold` samples.
    Out { from: f32, at: u32, len: u32, hold: u32 },
}

impl Ramp {
    fn gain(self) -> f32 {
        match self {
            Ramp::In { at, len } => at as f32 / len as f32,
            Ramp::Out { from, at, len, hold } => from * (1.0 - at.saturating_sub(hold) as f32 / (len - hold) as f32),
        }
    }
}

/// In a fade-out's length: keep full volume and fade only over its last
/// `QUICK_FADE_MS`, for a song that's already fading out by itself.
const HOLD: u32 = 1 << 31;

/// How long pausing fades out, and resuming fades back in, like foobar2000 and Spotify:
/// short enough not to hold anything up, long enough not to sound cut off.
const PAUSE_FADE_MS: u32 = 200;

/// With the fade off (Settings â€º Sound), pausing and playing again still ramp, this
/// briefly: too short to hear, but cutting off at full volume would click.
const INSTANT_FADE_MS: u32 = 5;

/// Time constant for smoothing normalization gain changes across gapless transitions
/// (or when toggling normalization mid-song), preventing an audible step click.
const NORM_GLIDE_TAU_MS: u32 = 5;

/// How long a seek crossfades from where it was to where it's going. Cutting from one
/// spot straight to another clicks; this is too short to hear as a fade, and nothing waits for it.
const SEEK_FADE_MS: u32 = 10;

/// What a seek in place read from the old spot before it went (`at` of them played): the
/// first `from` as they are (the rest of a stereo frame, so the new spot starts on its left
/// channel), the rest crossfaded into the new spot. All as they are if the seek failed.
struct SeekTail {
    old: Vec<f32>,
    at: usize,
    from: usize,
}

/// One-shot gate checked on sample #0 of a queued gapless track: aborts cleanly if
/// cancelled, pausing, or the previous download failed; otherwise signals handoff.
struct GaplessGate {
    generation: u64,
    cancelled: Arc<AtomicBool>,
    prev_failed: Arc<AtomicBool>,
    started: Arc<AtomicBool>,
    tx: Sender<AudioCmd>,
}

/// Pass-through source that tracks loudness (RMS over ~23 ms windows, smoothed) and
/// applies per-song normalization gain and crossfade ramps. `fade_out` is set by the audio
/// thread to the fade-out length in ms (0 = keep playing, maybe with `HOLD`); a new value
/// restarts it from the current gain. On top of that, it fades to silence while `hushed`, and back up from
/// wherever it got to when that's cleared, so pausing and playing again quickly never jumps.
struct Meter<S> {
    inner: S,
    status: Arc<AudioStatus>,
    /// Target normalization gain (f32 bits), applied per-source with a short glide so
    /// gapless loudness changes never step.
    norm: Arc<AtomicU32>,
    cur_norm: f32,
    norm_alpha: f32,
    fade_out: Arc<AtomicU32>,
    seen_out: u32,
    ramp: Option<Ramp>,
    gate: Option<GaplessGate>,
    aborted: bool,
    /// The pause fade's gain, how far it moves each sample (with the fade on, and off),
    /// and `silenced` as last seen.
    pause: f32,
    pause_step: f32,
    instant_step: f32,
    seen_silenced: u32,
    acc: f32,
    n: u32,
    smooth: f32,
    /// Set up the first time the visualizer is on for this song.
    spectrum: Option<spectrum::Spectrum>,
    tail: Option<SeekTail>,
    /// Which channel of its frame the next sample out is.
    phase: usize,
}

impl<S: rodio::Source> Meter<S> {
    fn new(inner: S, status: Arc<AudioStatus>, fade_in_ms: u32, fade_out: Arc<AtomicU32>) -> Self {
        // A decoder put in mid-fade (a seek's) carries on from there; while paused, from silence.
        let pause = 1.0 - f32::from_bits(status.faded.load(Ordering::Relaxed));
        let seen_silenced = status.silenced.load(Ordering::Relaxed);
        let mut m = Self {
            inner, status, norm: Arc::new(AtomicU32::new(1f32.to_bits())), cur_norm: 1.0, norm_alpha: 1.0,
            fade_out, seen_out: 0, ramp: None, gate: None, aborted: false,
            pause, pause_step: 0.0, instant_step: 0.0, seen_silenced, acc: 0.0, n: 0, smooth: 0.0, spectrum: None, tail: None, phase: 0,
        };
        m.pause_step = 1.0 / m.samples(PAUSE_FADE_MS) as f32;
        m.instant_step = 1.0 / m.samples(INSTANT_FADE_MS) as f32;
        m.norm_alpha = 1.0 - (-1.0 / m.samples(NORM_GLIDE_TAU_MS) as f32).exp();
        if fade_in_ms > 0 {
            m.ramp = Some(Ramp::In { at: 0, len: m.samples(fade_in_ms) });
        }
        m
    }

    fn with_norm(mut self, norm: Arc<AtomicU32>) -> Self {
        self.cur_norm = f32::from_bits(norm.load(Ordering::Relaxed));
        self.norm = norm;
        self
    }

    fn with_gate(mut self, gate: GaplessGate) -> Self {
        self.gate = Some(gate);
        self
    }

    fn samples(&self, ms: u32) -> u32 {
        let per_sec = self.inner.sample_rate().get() as u64 * self.inner.channels().get() as u64;
        (per_sec * ms as u64 / 1000).max(1) as u32
    }

    /// The next sample from `inner`, or from a seek's `SeekTail`.
    fn raw(&mut self) -> Option<f32> {
        let Some(t) = &mut self.tail else { return self.inner.next() };
        let old = t.old[t.at];
        t.at += 1;
        let s = if t.at > t.from {
            let g = (t.at - t.from) as f32 / (t.old.len() - t.from + 1) as f32;
            // The new spot may be the song's last few ms: the old one still fades out.
            self.inner.next().unwrap_or(0.0) * g + old * (1.0 - g)
        } else {
            old
        };
        if t.at == t.old.len() {
            self.tail = None;
        }
        Some(s)
    }
}

impl<S: rodio::Source> Iterator for Meter<S> {
    type Item = rodio::Sample;
    fn next(&mut self) -> Option<Self::Item> {
        if let Some(gate) = self.gate.take() {
            if gate.cancelled.load(Ordering::Acquire) || gate.prev_failed.load(Ordering::Acquire) || self.status.hushed.load(Ordering::Acquire) || self.status.stop_at_end.load(Ordering::Relaxed) {
                self.aborted = true;
                return None;
            }
            gate.started.store(true, Ordering::Release);
            let _ = gate.tx.send(AudioCmd::GaplessStarted(gate.generation));
            // Resync pause and normalization gain with the outgoing song so sample #0 is seamless.
            self.pause = 1.0 - f32::from_bits(self.status.faded.load(Ordering::Relaxed));
            self.seen_silenced = self.status.silenced.load(Ordering::Relaxed);
            let prev = self.status.active_norm.load(Ordering::Relaxed);
            if prev != 0 {
                self.cur_norm = f32::from_bits(prev);
            }
        }
        if self.aborted {
            return None;
        }
        let out = self.fade_out.load(Ordering::Relaxed);
        if out != self.seen_out {
            self.seen_out = out;
            let from = self.ramp.map_or(1.0, Ramp::gain);
            let len = self.samples(out & !HOLD);
            let hold = if out & HOLD != 0 { len.saturating_sub(self.samples(QUICK_FADE_MS)) } else { 0 };
            self.ramp = Some(Ramp::Out { from, at: 0, len, hold });
        }
        if let Some(Ramp::Out { at, len, .. }) = self.ramp
            && at >= len
        {
            return None;
        }
        let target_norm = f32::from_bits(self.norm.load(Ordering::Relaxed));
        if self.cur_norm != target_norm {
            let diff = target_norm - self.cur_norm;
            self.cur_norm = if diff.abs() < 1e-5 { target_norm } else { self.cur_norm + diff * self.norm_alpha };
        }
        if self.seen_out == 0 {
            self.status.active_norm.store(self.cur_norm.to_bits(), Ordering::Relaxed);
        }
        let mut s = self.raw()? * self.cur_norm;
        if let Some(ramp) = &mut self.ramp {
            s *= ramp.gain();
            match ramp {
                Ramp::In { at, len } => {
                    *at += 1;
                    if *at >= *len {
                        self.ramp = None;
                    }
                }
                Ramp::Out { at, .. } => *at += 1,
            }
        }
        let silenced = self.status.silenced.load(Ordering::Relaxed);
        if silenced != self.seen_silenced {
            self.seen_silenced = silenced;
            self.pause = 0.0;
        }
        let target = if self.status.hushed.load(Ordering::Relaxed) { 0.0 } else { 1.0 };
        if self.pause != target {
            let step = if self.status.instant_pause.load(Ordering::Relaxed) { self.instant_step } else { self.pause_step };
            self.pause = if target > self.pause { (self.pause + step).min(1.0) } else { (self.pause - step).max(0.0) };
            // Not a song fading out: the playing one is what a new decoder follows.
            if self.seen_out == 0 {
                self.status.faded.store((1.0 - self.pause).to_bits(), Ordering::Relaxed);
            }
        }
        s *= self.pause;
        // A song fading out is no longer the one the visuals follow.
        if self.seen_out == 0 && self.status.spectrum_on.load(Ordering::Relaxed) {
            let inner = &self.inner;
            self.spectrum.get_or_insert_with(|| spectrum::Spectrum::new(inner.sample_rate().get(), inner.channels().get())).push(s, &self.status.spectrum);
        }
        self.acc += s * s;
        self.n += 1;
        if self.n == 2048 {
            let rms = (self.acc / 2048.0).sqrt();
            // Fast attack, slow release, like a VU meter.
            let k = if rms > self.smooth { 0.6 } else { 0.08 };
            self.smooth += (rms - self.smooth) * k;
            // A song fading out is no longer the one the visuals follow.
            if self.seen_out == 0 {
                self.status.level.store(self.smooth.min(1.0).to_bits(), Ordering::Relaxed);
            }
            self.acc = 0.0;
            self.n = 0;
        }
        self.phase = (self.phase + 1) % self.inner.channels().get() as usize;
        Some(s)
    }
}

impl<S: rodio::Source> rodio::Source for Meter<S> {
    fn current_span_len(&self) -> Option<usize> {
        match &self.tail {
            Some(t) if t.at < t.from => Some(t.from - t.at),
            _ => self.inner.current_span_len(),
        }
    }
    fn channels(&self) -> rodio::ChannelCount {
        self.inner.channels()
    }
    fn sample_rate(&self) -> rodio::SampleRate {
        self.inner.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
    /// It reads on to the end of the stereo frame it's in first: rodio can seek between left
    /// and right, and the new spot starts on the left. Heard, a few ms more, to crossfade into it.
    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        let ch = self.inner.channels().get() as usize;
        let pad = (ch - self.phase) % ch;
        let n = pad + if self.pause > 0.0 { self.samples(SEEK_FADE_MS) as usize / ch * ch } else { 0 };
        let old: Vec<f32> = std::iter::from_fn(|| self.raw()).take(n).collect();
        let done = self.inner.try_seek(pos);
        let from = if done.is_ok() { pad.min(old.len()) } else { old.len() };
        self.tail = (!old.is_empty()).then_some(SeekTail { old, at: 0, from });
        done
    }
}

#[derive(Clone)]
pub struct AudioHandle {
    tx: Sender<AudioCmd>,
    pub status: Arc<AudioStatus>,
}

impl AudioHandle {
    pub fn send(&self, cmd: AudioCmd) {
        let _ = self.tx.send(cmd);
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.status.volume.load(Ordering::Relaxed))
    }

    /// How the network speakers picked are doing.
    pub fn remote_state(&self) -> RemoteState {
        self.status.remote.lock().unwrap().clone()
    }

    /// The app's own volume, to save: not a speaker's, while one's is on the slider.
    pub fn own_volume(&self) -> f32 {
        self.status.kept().unwrap_or_else(|| self.volume())
    }

    pub fn set_normalize(&self, level: Normalize) {
        self.status.normalize.store(level as u8, Ordering::Relaxed);
        self.status.limit.store(level != Normalize::Off, Ordering::Relaxed);
        self.send(AudioCmd::Refresh);
    }

    /// How far normalization turns the playing song up (positive) or down, in dB; None
    /// when it leaves it as it is (off, or there's no loudness figure for it).
    pub fn normalization(&self) -> Option<f32> {
        Some(f32::from_bits(self.status.norm_db.load(Ordering::Relaxed))).filter(|d| !d.is_nan())
    }

    /// Switches the slider's curve, moving it so the music stays as loud as it was.
    pub fn set_log_volume(&self, on: bool) {
        if self.status.log_volume.swap(on, Ordering::Relaxed) != on {
            // The app's own level; a speaker's, on the slider meanwhile, isn't on this curve.
            let moved = |v: f32| if on { v.cbrt() } else { v.powi(3) };
            match self.status.kept() {
                Some(v) => self.status.keep(Some(moved(v))),
                None => self.status.volume.store(moved(self.volume()).to_bits(), Ordering::Relaxed),
            }
        }
        self.send(AudioCmd::Refresh);
    }

    pub fn set_crossfade(&self, secs: u32) {
        self.status.crossfade_ms.store(secs * 1000, Ordering::Relaxed);
        if secs > 0 {
            self.send(AudioCmd::ClearNext);
        }
    }

    /// Whether consecutive songs chain sample-accurately without a pause between them.
    pub fn set_gapless(&self, on: bool) {
        self.status.gapless.store(on, Ordering::Relaxed);
        if !on {
            self.send(AudioCmd::ClearNext);
        }
    }

    /// Whether the visualizer's bars are measured (Settings â€º Appearance â€º Visualizer).
    pub fn set_spectrum(&self, on: bool) {
        self.status.spectrum_on.store(on, Ordering::Relaxed);
    }

    /// Whether pausing fades out and playing again fades back in; off, both are instant.
    pub fn set_pause_fade(&self, on: bool) {
        self.status.instant_pause.store(!on, Ordering::Relaxed);
        // Wakes the audio thread: a pause fading out now finishes quickly.
        self.send(AudioCmd::Refresh);
    }

    pub fn set_skip_silence(&self, on: bool) {
        self.status.skip_silence.store(on, Ordering::Relaxed);
    }

    pub fn set_volume(&self, v: f32) {
        self.status.volume.store(v.to_bits(), Ordering::Relaxed);
        self.send(AudioCmd::Volume(v));
    }

    /// Takes effect at once, gliding there. `no_clip` turns everything down by the
    /// curve's highest boost, leaving room for it in loud songs.
    pub fn set_eq(&self, on: bool, gains: &eq::Gains, no_clip: bool) {
        self.status.eq.set(on, gains, no_clip);
    }

    /// Takes effect at once, gliding there.
    pub fn set_stereo(&self, s: stereo::Settings) {
        self.status.stereo.set(s);
    }

    /// Takes effect at once.
    pub fn set_speed(&self, speed: f32) {
        self.status.speed.set(speed);
    }

    /// Whether the speed leaves the pitch alone, or takes it along like a record.
    pub fn set_keep_pitch(&self, on: bool) {
        self.status.speed.set_keep_pitch(on);
    }

    /// Transpose, in semitones, without changing the tempo. Takes effect at once.
    pub fn set_pitch(&self, semitones: i32) {
        self.status.speed.set_pitch(semitones);
    }
}

/// A seek that needs a new decoder built over the stream buffer (off the audio thread).
struct SeekJob {
    shared: Arc<Shared>,
    /// The song's seekable decoder being prepared in the background, if it's a long AAC
    /// file and no seek has taken it yet. Taken when the build starts, not when the seek
    /// is asked for, so a seek that's replaced before it's built doesn't lose it.
    prepared: Prepared,
    pos: Duration,
    /// For a live seek (a jam): the host position and when it was, to catch up after the build.
    live: Option<(Duration, std::time::Instant)>,
    generation: u64,
    seek: u64,
}

/// A long AAC song's seekable decoder, built in the background from when it starts.
type Prepared = Arc<std::sync::Mutex<Option<Receiver<anyhow::Result<TrackDecoder>>>>>;

/// Build a decoder for `j` (it may wait for the download) and hand it to the audio thread.
/// `pending`: the seek asked for since, if any.
fn build_seek(mut j: SeekJob, pending: &std::sync::Mutex<Option<SeekJob>>, tx: &Sender<AudioCmd>, events: &Sender<AudioEvent>) {
    let seekable = !j.pos.is_zero();
    let prepared = if seekable { j.prepared.lock().unwrap().take() } else { None };
    let decoder = prepared.and_then(|rx| rx.recv().ok()).unwrap_or_else(|| stream::decoder(j.shared.clone(), seekable));
    // That may have waited a long time (the download, the index). A seek in the same song
    // asked for meanwhile takes this decoder, which can go anywhere, rather than have it
    // dropped as stale and another built.
    if seekable && decoder.is_ok() {
        let mut next = pending.lock().unwrap();
        if next.as_ref().is_some_and(|n| n.generation == j.generation && !n.pos.is_zero()) {
            j = next.take().unwrap();
        }
    }
    let SeekJob { shared, pos, live, generation, seek, .. } = j;
    let res = decoder.map_err(|e| e.to_string()).and_then(|mut d| {
        // Building it may have waited for the download: catch up.
        let pos = live.map_or(pos, |(p, at)| p + at.elapsed());
        if seekable {
            rodio::Source::try_seek(&mut d, pos).map_err(|e| e.to_string())?;
        }
        Ok((d, pos))
    });
    match res {
        Ok((decoder, pos)) => {
            let _ = tx.send(AudioCmd::Replace { generation, seek, decoder, at: pos, seekable });
        }
        Err(e) => match shared.failure() {
            Some(failure) => {
                let _ = events.send(AudioEvent::Interrupted { generation, at: pos, failure });
            }
            None => {
                let _ = events.send(AudioEvent::Error(crate::tr!("audio-seek-failed", error = e)));
            }
        },
    }
}

/// A song fading out under the next one, kept until it's silent.
struct Fading {
    player: Player,
    shared: Option<Arc<Shared>>,
    norm: Arc<AtomicU32>,
    out: Arc<AtomicU32>,
    /// Still at full volume, waiting for the next song to start (as Morphe does): when
    /// that was asked for, and how much of this song was left then.
    waiting: Option<(Instant, Option<Duration>)>,
    /// It was already fading out by itself (its outro) when the next song was asked for.
    outro: bool,
}

/// A song queued right behind the playing one in `player` for gapless handoff.
struct Queued {
    generation: u64,
    shared: Arc<Shared>,
    fallback_duration_ms: u64,
    fade_out: Arc<AtomicU32>,
    song_ms: Arc<AtomicU64>,
    norm: Arc<AtomicU32>,
    cancelled: Arc<AtomicBool>,
    started: Arc<AtomicBool>,
}

/// How far before a song ends to open and queue the next song's decoder for gapless playback.
const GAPLESS_STAGE_BEFORE: Duration = Duration::from_secs(10);

impl Fading {
    /// Start the fade-out, over what's left of the song's music if that's shorter.
    /// Returns its length. A song fading out by itself isn't faded again on top: it
    /// plays out to where its music ends.
    fn start(&mut self, crossfade: u32) -> u32 {
        let left = self.waiting.take().and_then(|(at, left)| Some(left?.saturating_sub(at.elapsed())));
        let left = left.map_or(u32::MAX, |l| l.as_millis() as u32);
        let len = crossfade.min(left).max(QUICK_FADE_MS);
        self.out.store(if self.outro && left <= crossfade { len | HOLD } else { len }, Ordering::Relaxed);
        len
    }
}

/// How a song ends, from its last `TAIL`: the silence after its music, and how long
/// its music was fading out before that (since it was last near its loudest).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Tail {
    silence: Duration,
    fade: Duration,
}

/// How much of a song's end is looked at for where its music ends.
const TAIL: Duration = Duration::from_secs(30);
const TAIL_WINDOW: Duration = Duration::from_millis(50);
/// Silence is at least this quiet (dBFS).
const SILENT_DB: f32 = -60.0;
/// The longest silence skipped at the start of a song crossfaded into.
const MAX_LEAD: Duration = Duration::from_secs(8);
/// Kept clear of the end of a song's start in memory, when skipping its silence: room
/// for what the decoder and `Tempo` read ahead.
const LEAD_MARGIN: Duration = Duration::from_secs(2);

impl Tail {
    /// From the loudness of each `window` of the tail (`levels`). The music ends after
    /// its last window above both `SILENT_DB` and 50 dB under the tail's loud level
    /// (its 95th percentile, past any stray peak): a quiet song's last decay is still
    /// music. Its fade-out starts after its last window within 6 dB of that level.
    fn measure(levels: &[f32], window: Duration) -> Self {
        let mut sorted = levels.to_vec();
        sorted.sort_by(f32::total_cmp);
        let Some(&loud) = sorted.get(sorted.len() * 95 / 100) else { return Self::default() };
        let quiet = (loud - 50.0).min(SILENT_DB);
        let end = levels.iter().rposition(|&l| l > quiet).map_or(0, |i| i + 1);
        let fade_from = levels[..end].iter().rposition(|&l| l >= loud - 6.0).map_or(0, |i| i + 1);
        Self { silence: window * (levels.len() - end) as u32, fade: window * (end - fade_from) as u32 }
    }

    /// How `shared`'s song (`length` long) ends. Only its tail is decoded, by a second
    /// decoder over the same buffer.
    fn scan(shared: Arc<Shared>, length: Duration) -> Option<Self> {
        let mut d = stream::decoder(shared, true).ok()?;
        let from = length.saturating_sub(TAIL);
        if !from.is_zero() {
            rodio::Source::try_seek(&mut d, from).ok()?;
        }
        let (ch, rate) = (rodio::Source::channels(&d).get() as usize, rodio::Source::sample_rate(&d).get());
        Some(Self::measure(&levels(&mut d, ch, rate), TAIL_WINDOW))
    }

    /// It's fading out by itself through at least half of what's `left` of its music.
    fn outro(self, left: Duration) -> bool {
        self.fade * 2 >= left
    }
}

/// Where a song `length` long has its music end: its silence at the end trimmed, once known.
fn music_end(length: Option<Duration>, tail: Option<Tail>) -> Option<Duration> {
    Some(length?.saturating_sub(tail.map_or(Duration::ZERO, |t| t.silence)))
}

/// The loudness of each `TAIL_WINDOW` of `src`, to its end.
fn levels(src: &mut impl Iterator<Item = f32>, channels: usize, rate: u32) -> Vec<f32> {
    let n = ((TAIL_WINDOW.as_secs_f64() * rate as f64) as usize).max(1) * channels;
    std::iter::from_fn(|| window_db(src, n)).collect()
}

/// Loudness (dB of the RMS) of the next `n` samples of `src`; None once it's over.
fn window_db(src: &mut impl Iterator<Item = f32>, n: usize) -> Option<f32> {
    let (sum, count) = src.take(n).fold((0.0, 0), |(sum, count), s| (sum + s * s, count + 1));
    (count > 0).then(|| 10.0 * (sum / count as f32).log10())
}

/// Drops `src`'s leading silence, up to `max`, 10 ms at a time (whole frames, so the
/// channels stay in step). Returns how much went. The first 10 ms with sound in them go
/// too: they're under the very start of a fade-in, from silence.
fn skip_lead(src: &mut impl Iterator<Item = f32>, channels: usize, rate: u32, max: Duration) -> Duration {
    let frames = (rate as usize / 100).max(1);
    let most = (max.as_secs_f64() * rate as f64) as usize;
    let mut skipped = 0;
    while skipped < most
        && let Some(db) = window_db(src, frames * channels)
    {
        skipped += frames;
        if db > SILENT_DB {
            break;
        }
    }
    Duration::from_secs(1) * skipped as u32 / rate
}

impl Drop for Fading {
    fn drop(&mut self) {
        if let Some(s) = &self.shared {
            s.cancel();
        }
    }
}

/// Fade-outs shorter than this would click: a crossfade cut short by another skip, or
/// one too close to the end of the song.
const QUICK_FADE_MS: u32 = 40;

/// How long after a pause the players really pause: once the fade has gone through the
/// device's buffer.
const PAUSE_AFTER: Duration = Duration::from_millis(PAUSE_FADE_MS as u64 + 60);
/// The same with the fade off.
const INSTANT_PAUSE_AFTER: Duration = Duration::from_millis(INSTANT_FADE_MS as u64 + 60);

/// When a pause due at `pause_at` really pauses the players: with the fade turned off
/// on the way (`instant`), once the rest of it has gone the quick way.
fn pause_due(pause_at: Option<Instant>, instant: bool, now: Instant) -> Option<Instant> {
    match instant {
        true => pause_at.map(|at| at.min(now + INSTANT_PAUSE_AFTER)),
        false => pause_at,
    }
}

/// How long before the sleep timer pauses the music starts turning down, so it fades
/// away instead of stopping in the middle of a bar.
const SLEEP_FADE: Duration = Duration::from_secs(60);

/// The sleep timer's minutes, running in the audio thread.
#[derive(Clone, Copy, Debug)]
struct SleepTimer {
    at: Instant,
    /// `SLEEP_FADE`, or what's left when set if that's less, so a short timer starts loud.
    fade: Duration,
}

impl SleepTimer {
    fn new(at: Instant, now: Instant) -> Self {
        Self { at, fade: SLEEP_FADE.min(at.saturating_duration_since(now)) }
    }

    /// The players' share of the volume at `now`: full until the fade, then down to nothing
    /// when it pauses. Cubic, like the logarithmic slider, so it sounds like an even fade and
    /// doesn't stay loud for most of it.
    fn gain(&self, now: Instant) -> f32 {
        let left = self.at.saturating_duration_since(now);
        if left >= self.fade {
            return 1.0;
        }
        (left.as_secs_f32() / self.fade.as_secs_f32()).powi(3)
    }
}

/// What the sleep timer leaves of the volume. `hush` is set once the timer has gone off,
/// until the players have paused, so the music doesn't come back up during the pause's own
/// fade; after that, playing again is at full volume.
fn sleep_gain(sleep: Option<SleepTimer>, hush: bool, paused: bool, now: Instant) -> f32 {
    match sleep {
        Some(t) => t.gain(now),
        None if hush && !paused => 0.0,
        None => 1.0,
    }
}

/// How long the volume takes to come all the way back up when the timer is called off.
const SLEEP_RISE: Duration = Duration::from_millis(300);

/// How long a song keeps playing at full volume for the next one to load before it
/// fades out anyway.
const HANDOFF_WAIT: Duration = Duration::from_secs(3);

/// The players' share of the volume: the slider's, or nothing (full) while an AirPlay
/// speaker plays, the slider then setting the speaker's own volume, which it changes at once.
fn bar_gain(status: &AudioStatus, output: &Output, v: f32) -> f32 {
    if output.remote.is_some() { 1.0 } else { slider_gain(status, v) }
}

/// Gain for a volume slider position. Ears hear amplitude roughly logarithmically,
/// so on a linear slider nearly all the change is in its bottom few pixels; the cubic
/// curve (PulseAudio's) spreads it out: half way is -18 dB.
fn slider_gain(status: &AudioStatus, v: f32) -> f32 {
    if status.log_volume.load(Ordering::Relaxed) { v.powi(3) } else { v }
}

/// The level volume normalization brings songs to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Normalize {
    Off,
    /// YouTube's (and Spotify's) -14 LUFS.
    #[default]
    Normal,
    /// -19 LUFS, like Spotify's Quiet: more songs line up without being turned up.
    Quiet,
}

impl Normalize {
    pub const ALL: [Self; 3] = [Self::Off, Self::Normal, Self::Quiet];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => crate::tr!("ui-off"),
            Self::Normal => crate::tr!("ui-normal"),
            Self::Quiet => crate::tr!("ui-quiet"),
        }
    }

    /// As saved: "1" and "0" are what the on/off switch saved before.
    pub fn key(self) -> &'static str {
        match self {
            Self::Off => "0",
            Self::Normal => "1",
            Self::Quiet => "quiet",
        }
    }

    pub fn from_key(key: &str) -> Self {
        Self::ALL.into_iter().find(|l| l.key() == key).unwrap_or_default()
    }

    fn from_u8(v: u8) -> Self {
        Self::ALL.get(v as usize).copied().unwrap_or(Self::Off)
    }

    /// The level, in dB from YouTube's -14 LUFS target.
    fn offset_db(self) -> f32 {
        if self == Self::Quiet { -5.0 } else { 0.0 }
    }
}

/// The most normalization turns a quiet song up. Past this the limiter would be holding
/// it down all the time, which flattens it.
const MAX_BOOST_DB: f32 = 6.0;

/// What volume normalization does to a song, in dB: brings it from the loudness YouTube
/// measured to the level picked, turning loud songs down and quiet ones up (up to
/// `MAX_BOOST_DB`; the limiter keeps their peaks from clipping). None when it leaves it
/// as it is: off, or no loudness figure (yt-dlp's streams, podcast feeds).
fn norm_db(status: &AudioStatus, shared: Option<&Shared>) -> Option<f32> {
    let level = Normalize::from_u8(status.normalize.load(Ordering::Relaxed));
    if level == Normalize::Off {
        return None;
    }
    Some((level.offset_db() - shared?.loudness_db?).min(MAX_BOOST_DB))
}

fn norm_gain(status: &AudioStatus, shared: Option<&Shared>) -> f32 {
    norm_db(status, shared).map_or(1.0, |db| 10f32.powf(db / 20.0))
}

/// Keeps what normalization does to the playing song, for the UI to show.
fn show_norm(status: &AudioStatus, shared: Option<&Shared>) {
    status.norm_db.store(norm_db(status, shared).unwrap_or(f32::NAN).to_bits(), Ordering::Relaxed);
}

/// The output device. Players play into a mixer of our own that the device pulls
/// from, so when the device goes away (Bluetooth headphones disconnecting) or the
/// default changes (them reconnecting), a new one carries on from the same samples
/// without touching the players or the songs in them.
struct Output {
    mixer: Mixer,
    /// Our mixer's output while no device has it; the device's `Bridge` holds it.
    slot: Arc<std::sync::Mutex<Option<MixerSource>>>,
    sink: Option<MixerDeviceSink>,
    /// The network speakers playing, in place of `sink`.
    remote: Option<destination::Stream>,
    /// The network speakers being connected to (`Destination::key`): a sound card playing
    /// meanwhile goes on till they're ready.
    /// Each attempt has its own number (`AudioCmd::Connected`'s).
    connecting: Option<(String, u64)>,
    /// The last handshake started, till it's back. Still theirs if the speakers are picked
    /// again meanwhile (another picked, then them): a second one would find them busy with it.
    handshake: Option<(String, u64)>,
    /// Whether to try the network speakers picked. Off once they failed or hung up, so the
    /// check for a new device doesn't keep taking them back (from another device playing
    /// on them); on again when the music's asked for.
    remote_retry: bool,
    /// What went wrong with the network speakers, for the UI to show.
    notice: Option<String>,
    /// The level the speaker playing said it was at, for the slider to take on.
    speaker_level: Option<f32>,
    /// The device in use (cpal's id, or the speakers' `Destination::key`), to notice the
    /// default changing.
    device: Option<String>,
    /// Where the user picked; Windows' default stands in while it's not there.
    destination: Destination,
    /// Set by the device's error callback.
    lost: Arc<AtomicBool>,
    checked: Instant,
}

impl Output {
    /// Doesn't open the device: the first song does. Also whether there's one to open.
    fn new(destination: Destination) -> (Self, bool) {
        // Mix at the device's own format, so the usual case needs no conversion.
        let device = target(card(&destination));
        let found = device.is_some();
        let config = device.and_then(|d| d.default_output_config().ok());
        let channels = config.as_ref().and_then(|c| std::num::NonZero::new(c.channels())).unwrap_or(std::num::NonZero::new(2).unwrap());
        let rate = config.as_ref().and_then(|c| std::num::NonZero::new(c.sample_rate())).unwrap_or(std::num::NonZero::new(48_000).unwrap());
        let (mixer, source) = rodio::mixer::mixer(channels, rate);
        let lost = Arc::new(AtomicBool::new(false));
        if destination.is_remote() {
            destination::discover();
        }
        let output = Self {
            mixer,
            slot: Arc::new(std::sync::Mutex::new(Some(source))),
            sink: None,
            remote: None,
            connecting: None,
            handshake: None,
            remote_retry: true,
            notice: None,
            speaker_level: None,
            device: None,
            destination,
            lost,
            checked: Instant::now(),
        };
        (output, found)
    }

    /// Whether a device (or speaker) has the mixer, or a speaker's about to.
    fn is_open(&self) -> bool {
        self.pulled() || self.connecting.is_some()
    }

    /// Whether something's taking the mixer's samples, so what's done to a player happens.
    fn pulled(&self) -> bool {
        self.sink.is_some() || self.remote.is_some()
    }

    /// Lets go of the device or speaker. A handshake still going is forgotten: its session
    /// is dropped when it comes.
    fn close(&mut self) {
        self.sink = None;
        self.remote = None;
        self.connecting = None;
    }

    /// Whether the speakers in use pause themselves: see `destination::Stream::pauses`.
    fn pauses(&self) -> bool {
        self.remote.as_ref().is_some_and(destination::Stream::pauses)
    }

    /// Drops what the speaker has buffered, so a pause, seek or new song is heard now
    /// rather than their `Kind::latency` later. A sound card's buffer is too short to matter.
    fn flush(&self) {
        if let Some(a) = &self.remote {
            a.flush();
        }
    }

    /// The network speakers picked, if they're all on the network and worth trying: what
    /// to call them, and the way to play them together.
    fn remote_target(&self) -> Option<(String, Kind, Vec<destination::Receiver>)> {
        let Destination::Remote { kind, members } = &self.destination else { return None };
        let found = destination::find_all(members).filter(|_| self.remote_retry)?;
        // The way picked, while they all still have it; otherwise the best they share.
        let kind = Some(*kind).filter(|&k| destination::all_have(&found, k)).or_else(|| destination::shared_kind(&found))?;
        Some((self.destination.key(), kind, found))
    }

    /// Our mixer's output, back from the device that had it. A stream that failed on its
    /// own hands it back as its thread exits, which can be just after it reported the error.
    fn take_source(&self) -> Option<MixerSource> {
        let waited = Instant::now();
        loop {
            if let Some(source) = self.slot.lock().unwrap().take() {
                return Some(source);
            }
            if waited.elapsed() > Duration::from_secs(1) {
                log::warn!("audio: mixer still held by the old device");
                return None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Sends `DeviceLost` once, when the device (or speaker) in use goes.
    fn reporter(&self, tx: Sender<AudioCmd>) -> impl Fn() + Clone + Send + 'static {
        let lost = self.lost.clone();
        lost.store(false, Ordering::Relaxed);
        move || {
            if !lost.swap(true, Ordering::Relaxed) {
                let _ = tx.send(AudioCmd::DeviceLost);
            }
        }
    }

    /// Connecting to the speakers came back (`AudioCmd::Connected`): plays through them, or
    /// says why not (in `notice`) and returns false, for the sound card to stand in.
    fn connected(&mut self, name: &str, kind: Kind, session: anyhow::Result<Box<destination::Session>>, tx: Sender<AudioCmd>, status: &AudioStatus) -> bool {
        let session = match session {
            Ok(s) => s,
            Err(e) => {
                log::warn!("audio: {} speaker {name:?}: {e:#}", kind.label());
                self.remote_retry = false;
                let reason = destination::describe(kind, &e);
                self.notice = Some(crate::tr!("airplay-couldnt-play", name = name, reason = reason.as_str()));
                *status.remote.lock().unwrap() = RemoteState::Failed(reason);
                return false;
            }
        };
        // The sound card that played meanwhile gives the mixer back.
        self.sink = None;
        // Failing from here, the sound card stands in, and isn't left to try them again
        // (and fail again) at once: `open` would otherwise connect anew.
        let Some(source) = self.take_source() else {
            self.remote_retry = false;
            *status.remote.lock().unwrap() = RemoteState::Failed(destination::describe(kind, &anyhow::anyhow!("the audio wasn't handed over")));
            return false;
        };
        let bridge = Bridge::new(source, self.slot.clone(), status, true);
        let volume = session.volume();
        match (*session).play(bridge, self.reporter(tx)) {
            Ok(stream) => {
                self.speaker_level = volume;
                status.latency_ms.store(kind.latency().as_millis() as u32 + limiter::LOOKAHEAD_MS, Ordering::Relaxed);
                log::info!("audio: playing on {} speaker {name:?}", kind.label());
                *status.remote.lock().unwrap() = RemoteState::Playing;
                self.remote = Some(stream);
                true
            }
            // The bridge, dropped, gave the mixer back.
            Err(e) => {
                log::warn!("audio: {}: {e:#}", kind.label());
                self.remote_retry = false;
                *status.remote.lock().unwrap() = RemoteState::Failed(destination::describe(kind, &e));
                false
            }
        }
    }

    /// Opens the picked device, or the default one. False if there's none to open.
    fn open(&mut self, tx: Sender<AudioCmd>, status: &AudioStatus) -> bool {
        self.checked = Instant::now();
        // Connecting to speakers takes a while (seconds, for an AirPlay 2 one's clock), so
        // it goes on elsewhere and comes back as `AudioCmd::Connected`. Failing, the sound
        // card stands in, as for one unplugged.
        if let Some((key, kind, members)) = self.remote_target() {
            if self.connecting.as_ref().is_some_and(|(k, _)| *k == key) {
                return true;
            }
            // A sound card playing goes on till the speakers are ready (`connected`).
            self.remote = None;
            self.device = Some(key.clone());
            *status.remote.lock().unwrap() = RemoteState::Connecting;
            if let Some(going) = self.handshake.clone().filter(|(k, _)| *k == key) {
                self.connecting = Some(going);
                return true;
            }
            static ATTEMPTS: AtomicU64 = AtomicU64::new(0);
            let attempt = ATTEMPTS.fetch_add(1, Ordering::Relaxed);
            self.connecting = Some((key, attempt));
            self.handshake = self.connecting.clone();
            let back = tx.clone();
            let spawned = std::thread::Builder::new().name("speaker-connect".into()).spawn(move || {
                let name = members.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join(", ");
                let session = destination::Session::connect(kind, &members).map(Box::new);
                let _ = back.send(AudioCmd::Connected { attempt, name, kind, session });
            });
            if let Err(e) = spawned {
                log::warn!("audio: couldn't start connecting to {}: {e}", kind.label());
                self.connecting = None;
                self.handshake = None;
                self.remote_retry = false;
                *status.remote.lock().unwrap() = RemoteState::Failed(destination::describe(kind, &e.into()));
            } else {
                return true;
            }
        }
        // The old device's stream gives our mixer back when it's dropped.
        self.close();
        self.device = None;
        status.remote_gone();
        let report = self.reporter(tx);
        let Some(device) = target(card(&self.destination)) else {
            return false;
        };
        let id = device_id(&device);
        let on_error = move |e: rodio::cpal::StreamError| {
            log::warn!("audio: output device error: {e}");
            report();
        };
        let sink = match DeviceSinkBuilder::from_device(device).and_then(|b| b.with_error_callback(on_error).open_sink_or_fallback()) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("audio: couldn't open the output device: {e}");
                // A picked device failing isn't the default's (but the default standing in
                // for a picked device that's gone is).
                forget_stand_in(id.as_deref().filter(|&i| card(&self.destination) != Some(i)));
                return false;
            }
        };
        let Some(source) = self.take_source() else { return false };
        let mut sink = sink;
        sink.log_on_drop(false);
        // A mono device would keep only the left of what it's given: balanced right, nothing.
        let stereo = sink.config().channel_count().get() >= 2;
        sink.mixer().add(Bridge::new(source, self.slot.clone(), status, stereo));
        // The position counts samples handed to the device; it plays them a buffer later
        // (rodio asks for ~50 ms), plus the shared-mode device period (~10 ms) and the
        // limiter's lookahead.
        let rate = sink.config().sample_rate().get().max(1);
        let buffer_ms = match sink.config().buffer_size() {
            rodio::cpal::BufferSize::Fixed(n) => *n as u64 * 1000 / rate as u64,
            rodio::cpal::BufferSize::Default => 0,
        };
        status.latency_ms.store((buffer_ms + 10) as u32 + limiter::LOOKAHEAD_MS, Ordering::Relaxed);
        log::info!("audio: output device {:?} at {} Hz", id, rate);
        self.sink = Some(sink);
        self.device = id;
        true
    }

    /// Whether the device to play through (the picked one if it's connected, else the
    /// default) differs from the one in use.
    fn default_changed(&mut self) -> bool {
        self.checked = Instant::now();
        // Speakers playing stay till they hang up (`DeviceLost`, which pauses): mDNS
        // forgetting them for a while isn't them going.
        if self.remote.is_some() && self.device.as_ref() == Some(&self.destination.key()) {
            return false;
        }
        let want = match self.remote_target() {
            Some((key, ..)) => Some(key),
            None => target(card(&self.destination)).and_then(|d| device_id(&d)),
        };
        want.is_some() && want != self.device
    }
}

/// Quieter than this (about -60 dBFS) is silence, for `silent_tail`.
const SILENT_LEVEL: f32 = 0.001;
/// How long silence lasts before the rest of the song is skipped.
const SILENT_FOR: Duration = Duration::from_secs(3);

/// Whether the song has been silent for `SILENT_FOR` of its own time (so a stalled
/// download doesn't count), in its second half: a quiet intro or a pause mid-song isn't
/// an ending. `quiet_from` keeps where the silence began.
fn silent_tail(quiet_from: &mut Option<Duration>, level: f32, pos: Duration, length: Option<Duration>) -> bool {
    if level >= SILENT_LEVEL || length.is_none_or(|d| pos * 2 < d) {
        *quiet_from = None;
        return false;
    }
    pos.saturating_sub(*quiet_from.get_or_insert(pos)) >= SILENT_FOR
}

/// Empties the player without waiting on the device. rodio's `clear` waits for the device
/// to skip what's in it, which never happens while the device is let go (a song paused that
/// long), nor while a device that's gone still looks open (until `DeviceLost` arrives): a new
/// player instead, at the same volume and paused, as `clear` leaves it. The old one's song
/// ends the next time the mixer is pulled.
fn clear(player: &mut Player, output: &Output) {
    let fresh = Player::connect_new(&output.mixer);
    fresh.set_volume(player.volume());
    fresh.pause();
    *player = fresh;
}

/// Puts `song`, the playing song at another spot (a seek's new decoder), in place of what
/// `player` has. `heard`: the old spot is still going out, so it crossfades into the new one.
#[allow(clippy::too_many_arguments)]
fn put_in<S: rodio::Source + Send + 'static>(player: &mut Player, mixer: &Mixer, fading: &mut Vec<Fading>, fade_out: &mut Arc<AtomicU32>, norm: &Arc<AtomicU32>, status: &Arc<AudioStatus>, song: S, heard: bool) {
    let paused = player.is_paused();
    let fade = heard && !paused;
    // The song in before the mixer gets it: the mixer takes the format it starts with.
    let (fresh, out) = Player::new();
    fresh.set_volume(player.volume());
    if paused {
        fresh.pause();
    }
    let old_out = std::mem::replace(fade_out, Arc::new(AtomicU32::new(0)));
    fresh.append(Meter::new(song, status.clone(), if fade { SEEK_FADE_MS } else { 0 }, fade_out.clone()).with_norm(norm.clone()));
    let old = std::mem::replace(player, fresh);
    mixer.add(out);
    if fade {
        old_out.store(SEEK_FADE_MS, Ordering::Relaxed);
        // Its own `norm`: a volume change sets a fading song's from its `shared`, none here,
        // which would turn the playing one's normalization off.
        let norm = Arc::new(AtomicU32::new(norm.load(Ordering::Relaxed)));
        fading.push(Fading { player: old, shared: None, norm, out: old_out, waiting: None, outro: false });
    }
}

/// The sound card picked, if one is.
fn card(destination: &Destination) -> Option<&str> {
    match destination {
        Destination::Local(id) => id.as_deref(),
        Destination::Remote { .. } => None,
    }
}

/// The picked device if it's connected, else the system default.
fn target(preferred: Option<&str>) -> Option<rodio::cpal::Device> {
    let host = rodio::cpal::default_host();
    if let Some(want) = preferred
        && let Some(d) = host.output_devices().ok().and_then(|mut all| all.find(|d| device_id(d).is_some_and(|i| i == want)))
    {
        return Some(d);
    }
    // Only ALSA's default can point at an output that can't open (`hw:0` where the sound
    // server's ALSA plugin isn't set as the default). Other systems' default is what they
    // play on, so it's kept even while it fails, as before. (By the host's id: see `device_id`.)
    if host.id().name() != "Alsa" {
        return host.default_output_device();
    }
    let default = Pcm::new(host.default_output_device()?)?;
    let stand_ins = || {
        let mut all: Vec<Pcm> = host.output_devices().map(|list| list.filter_map(Pcm::new).collect()).unwrap_or_default();
        let ids = all.iter().map(|p| p.id.clone()).collect();
        fallbacks(ids)
            .into_iter()
            .filter_map(|id| {
                let at = all.iter().position(|p| p.id == id)?;
                Some(all.swap_remove(at))
            })
            .collect()
    };
    let mut state = STAND_IN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let plays = |p: &Pcm| p.device.default_output_config().is_ok();
    resolve(&mut state, Instant::now(), default, plays, stand_ins).map(|p| p.device)
}

/// cpal's id of `d`, as settings keep the device picked. On Windows cpal 0.17 leaks the
/// string the system gives it for each one it's asked (cpal#1389, fixed after 0.17), and
/// `default_changed` asks every second while music plays: there each device's id is asked
/// once and kept, and found again by comparing devices, which frees theirs.
fn device_id(d: &rodio::cpal::Device) -> Option<String> {
    #[cfg(windows)]
    {
        use rodio::cpal::platform::DeviceInner;
        static KNOWN: std::sync::Mutex<Vec<(rodio::cpal::Device, String)>> = std::sync::Mutex::new(Vec::new());
        let same = |a: &rodio::cpal::Device, b: &rodio::cpal::Device| match (a.as_inner(), b.as_inner()) {
            (DeviceInner::Wasapi(a), DeviceInner::Wasapi(b)) => a == b,
            // ASIO or JACK, where cpal has them turned on: asked as they are.
            #[allow(unreachable_patterns)]
            _ => false,
        };
        let mut known = KNOWN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, id)) = known.iter().find(|(k, _)| same(k, d)) {
            return Some(id.clone());
        }
        let id = d.id().ok()?.to_string();
        if matches!(d.as_inner(), DeviceInner::Wasapi(_)) {
            known.push((d.clone(), id.clone()));
        }
        Some(id)
    }
    #[cfg(not(windows))]
    d.id().ok().map(|i| i.to_string())
}

/// An ALSA output, told apart by its id.
#[derive(Clone)]
struct Pcm {
    id: String,
    device: rodio::cpal::Device,
}

impl Pcm {
    fn new(device: rodio::cpal::Device) -> Option<Self> {
        Some(Self { id: device.id().ok()?.to_string(), device })
    }
}

impl PartialEq for Pcm {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl AsRef<str> for Pcm {
    fn as_ref(&self) -> &str {
        &self.id
    }
}

static STAND_IN: std::sync::Mutex<StandIn<Pcm>> = std::sync::Mutex::new(StandIn { resolved: None, failed: Vec::new() });

/// How long a stand-in plays before what's better is tried again: the default, or the
/// sound server, may only have failed for a while (not up yet at login, or restarting).
/// Each try of a default that can't open prints an ALSA error.
const STAND_IN_FOR: Duration = Duration::from_secs(30);

/// How long an output that opened for `resolve`'s try but then couldn't play is passed
/// over: else it'd be picked again every time, and what plays never would.
const FAILED_FOR: Duration = Duration::from_secs(300);

/// What ALSA's default resolved to (`resolve`), and the outputs that lately couldn't play.
struct StandIn<D> {
    resolved: Option<Resolved<D>>,
    failed: Vec<(String, Instant)>,
}

/// `None` in `stand_in`: the default plays itself. Since when, for `STAND_IN_FOR`.
struct Resolved<D> {
    stand_in: Option<D>,
    since: Instant,
}

/// The device "System default" gave, `id`, couldn't play: the next `target` tries again,
/// passing it over for `FAILED_FOR`.
fn forget_stand_in(id: Option<&str>) {
    forget(&mut STAND_IN.lock().unwrap_or_else(std::sync::PoisonError::into_inner), id, Instant::now());
}

fn forget<D>(state: &mut StandIn<D>, id: Option<&str>, now: Instant) {
    state.resolved = None;
    state.failed.retain(|(_, at)| now.duration_since(*at) < FAILED_FOR);
    if let Some(id) = id {
        state.failed.push((id.to_owned(), now));
    }
}

/// The default if it `plays`, else the first of `stand_ins` (best first) that does, with
/// `state` sparing the tries `default_changed` would make every second. Nothing's kept
/// while nothing plays.
///
/// A default that played is kept till `forget`, so the device in use isn't opened again
/// (a card's `hw` would be busy). Each `STAND_IN_FOR`, a stand-in gives way to the default
/// or a better stand-in that plays now (the sound server come up after a card took over);
/// the stand-in itself isn't tried again then, being in use.
fn resolve<D: Clone + PartialEq + AsRef<str>>(state: &mut StandIn<D>, now: Instant, default: D, plays: impl Fn(&D) -> bool, stand_ins: impl FnOnce() -> Vec<D>) -> Option<D> {
    let kept = match &state.resolved {
        Some(Resolved { stand_in: None, .. }) => return Some(default),
        Some(Resolved { stand_in: Some(d), since }) if now.duration_since(*since) < STAND_IN_FOR => return Some(d.clone()),
        Some(Resolved { stand_in, .. }) => stand_in.clone(),
        None => None,
    };
    state.failed.retain(|(_, at)| now.duration_since(*at) < FAILED_FOR);
    let failed = |d: &D| state.failed.iter().any(|(id, _)| id == d.as_ref());
    if !failed(&default) && plays(&default) {
        state.resolved = Some(Resolved { stand_in: None, since: now });
        return Some(default);
    }
    let found = stand_ins().into_iter().find(|d| match &kept {
        Some(k) if k == d => true,
        // Only for a better one: another card as good would switch cards mid-song.
        Some(k) if stand_in_rank(d.as_ref()) >= stand_in_rank(k.as_ref()) => false,
        _ => !failed(d) && plays(d),
    });
    state.resolved = found.clone().map(|d| Resolved { stand_in: Some(d), since: now });
    found
}

/// What can stand in for ALSA's default, best first: the sound server (PipeWire, else
/// PulseAudio), then the cards' outputs as settings ranks them (`alsa_rank`).
fn fallbacks(ids: Vec<String>) -> Vec<String> {
    let mut ranked: Vec<(u8, String)> = ids.into_iter().filter_map(|id| Some((stand_in_rank(&id)?, id))).collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, id)| id).collect()
}

/// Where an output comes in `fallbacks`, lower better; `None`: it can't stand in.
fn stand_in_rank(id: &str) -> Option<u8> {
    match id {
        "alsa:pipewire" => Some(0),
        "alsa:pulse" => Some(1),
        _ => Some(alsa_rank(id)? + 2),
    }
}

/// An output device to offer in settings.
#[derive(Clone, PartialEq)]
pub struct OutputDevice {
    /// cpal's id: stable across reconnects.
    pub id: String,
    pub name: String,
}

/// The connected output devices, by the names Windows' sound settings show. `picked`, the
/// one picked in settings, is listed even where it'd be left out (see `offered`).
pub fn output_devices(picked: Option<&str>) -> Vec<OutputDevice> {
    let Ok(all) = rodio::cpal::default_host().output_devices() else { return Vec::new() };
    let all = all
        .filter_map(|d| {
            let id = device_id(&d)?;
            let desc = d.description().ok()?;
            // "Speakers (Realtek(R) Audio)" rather than just "Speakers".
            let name = desc.extended().first().cloned().unwrap_or_else(|| desc.name().to_owned());
            Some(OutputDevice { id, name })
        })
        .collect();
    offered(all, picked)
}

/// What of `all` to offer, by name. Linux's ALSA lists each output many ways (#337): its
/// card's default, front, the surround layouts, S/PDIF, plughw and hw, all under the
/// output's one name; and plugins that aren't outputs at all. Each output is offered
/// once, the way that plays best, and plugins not at all. A sound server (PipeWire, else
/// PulseAudio) is offered only where ALSA's default isn't it already: there the default
/// plays on a card directly, and the server is the only way to its other outputs, such
/// as Bluetooth headphones (#397).
fn offered(all: Vec<OutputDevice>, picked: Option<&str>) -> Vec<OutputDevice> {
    let server = sound_server(&all);
    // Ranked, the picked one first of all: what's picked is shown as listed.
    let mut out: Vec<(u8, OutputDevice)> = Vec::new();
    for d in all {
        let rank = match alsa_rank(&d.id) {
            _ if picked == Some(d.id.as_str()) => 0,
            _ if server == Some(d.id.as_str()) => 1,
            Some(rank) => rank + 1,
            None => continue,
        };
        // Only ALSA lists one output several ways. Elsewhere, two with one name (two of the
        // same headset on a Mac) are two outputs.
        let alsa = d.id.starts_with("alsa:");
        match out.iter_mut().find(|(_, o)| alsa && o.name == d.name) {
            Some(best) if rank < best.0 => *best = (rank, d),
            Some(_) => {}
            None => out.push((rank, d)),
        }
    }
    let mut out: Vec<OutputDevice> = out.into_iter().map(|(_, d)| d).collect();
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

/// The sound server ALSA lists (`alsa:pipewire`, else `alsa:pulse`), when its default
/// output isn't that server: ALSA says what its default is in the default's name ("Default
/// ALSA Output (currently PipeWire Media Server)").
fn sound_server(all: &[OutputDevice]) -> Option<&'static str> {
    let default = all.iter().find(|d| d.id == "alsa:default").map(|d| d.name.to_lowercase()).unwrap_or_default();
    [("alsa:pipewire", "pipewire"), ("alsa:pulse", "pulse")]
        .into_iter()
        .find(|(id, _)| all.iter().any(|d| d.id == *id))
        .filter(|(_, named)| !default.contains(named))
        .map(|(id, _)| id)
}

/// How well an output listed by ALSA (`alsa:<pcm>`) plays, lower better; `None`: not offered.
/// Other systems' outputs are each offered as they are.
fn alsa_rank(id: &str) -> Option<u8> {
    let Some(pcm) = id.strip_prefix("alsa:") else { return Some(0) };
    // A card's outputs name it ("front:CARD=PCH,DEV=0"); the rest are plugins, or sound
    // servers on the default output (default, pipewire, pulse).
    if !pcm.contains("CARD=") {
        return None;
    }
    match pcm.split_once(':').map_or(pcm, |(kind, _)| kind) {
        // Mixed with what else plays and converted to what the card takes.
        "sysdefault" => Some(0),
        "front" | "hdmi" => Some(1),
        "iec958" => Some(2),
        "plughw" => Some(3),
        // hw takes only the card's own format; the surround layouts are the front output
        // with more channels; usbstream is for a few USB devices' own drivers.
        "hw" | "usbstream" => None,
        kind if kind.starts_with("surround") => None,
        _ => Some(4),
    }
}

/// Our mixer's output, as a source in the device's mixer. Never ends, so the device
/// keeps it; gives the mixer back when the device's stream drops it.
struct Bridge {
    inner: Option<MixerSource>,
    slot: Arc<std::sync::Mutex<Option<MixerSource>>>,
    /// A frame is taken from the mixer whole, so crossfeed, mono and balance can mix its
    /// sides, and the mixer's always given back on a frame boundary (else the next device
    /// would get the channels swapped). `at` is the next sample of it to hand out.
    frame: Vec<f32>,
    at: usize,
    /// Here, after the mixer, so one equalizer covers both songs in a crossfade and a
    /// new song or a seek doesn't restart it.
    eq: eq::Eq,
    /// None on a mono device.
    stereo: Option<stereo::Stereo>,
    /// Last, after the volume, so it only acts on what would really clip.
    limiter: limiter::Limiter,
}

impl Bridge {
    fn new(source: MixerSource, slot: Arc<std::sync::Mutex<Option<MixerSource>>>, status: &AudioStatus, stereo: bool) -> Self {
        let channels = rodio::Source::channels(&source).get();
        let rate = rodio::Source::sample_rate(&source).get();
        let eq = eq::Eq::new(status.eq.clone(), rate, channels);
        let stereo = stereo.then(|| stereo::Stereo::new(status.stereo.clone(), rate));
        let limiter = limiter::Limiter::new(status.limit.clone(), rate, channels);
        Self { inner: Some(source), slot, frame: vec![0.0; channels.max(1) as usize], at: 0, eq, stereo, limiter }
    }
}

impl Iterator for Bridge {
    type Item = rodio::Sample;
    fn next(&mut self) -> Option<Self::Item> {
        let channel = self.at;
        self.at = (self.at + 1) % self.frame.len();
        if channel == 0 {
            for (c, s) in self.frame.iter_mut().enumerate() {
                *s = self.eq.process(self.inner.as_mut().and_then(Iterator::next).unwrap_or(0.0), c);
            }
            if let Some(st) = &mut self.stereo {
                st.process(&mut self.frame);
                if st.boosts() {
                    self.limiter.boost();
                }
            }
        }
        Some(self.limiter.process(self.frame[channel], channel))
    }
}

impl rodio::Source for Bridge {
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> rodio::ChannelCount {
        self.inner.as_ref().map_or(std::num::NonZero::new(self.frame.len() as u16).unwrap(), |s| s.channels())
    }
    fn sample_rate(&self) -> rodio::SampleRate {
        self.inner.as_ref().map_or(std::num::NonZero::new(48_000).unwrap(), |s| s.sample_rate())
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        if let Some(source) = self.inner.take() {
            *self.slot.lock().unwrap() = Some(source);
        }
    }
}

/// How often to look for a new default device while playing.
const DEVICE_CHECK: Duration = Duration::from_secs(1);

/// How long nothing plays before the output device is let go. An open stream counts as
/// audio playing to the OS (on Windows it keeps the computer from sleeping), even paused.
const IDLE_CLOSE: Duration = Duration::from_secs(30);

pub fn spawn(volume: f32, log_volume: bool, device: Destination, events: Sender<AudioEvent>, wake: impl Fn() + Send + 'static) -> AudioHandle {
    let (tx, rx) = unbounded();
    let status = Arc::new(AudioStatus::default());
    status.volume.store(volume.to_bits(), Ordering::Relaxed);
    status.log_volume.store(log_volume, Ordering::Relaxed);
    let st = status.clone();
    std::thread::Builder::new()
        .name("audio".into())
        .spawn({
            let tx = tx.clone();
            move || run(rx, tx, st, volume, device, events, wake)
        })
        .expect("spawn audio thread");
    AudioHandle { tx, status }
}

fn run(
    rx: Receiver<AudioCmd>,
    tx: Sender<AudioCmd>,
    status: Arc<AudioStatus>,
    volume: f32,
    device: Destination,
    events: Sender<AudioEvent>,
    wake: impl Fn(),
) {
    // An open device costs CPU even while silent, and keeps Windows awake: it waits for
    // the first song, as after `IDLE_CLOSE`.
    let (mut output, found) = Output::new(device);
    if !found {
        let _ = events.send(AudioEvent::Error(crate::tr!("audio-no-device").to_owned()));
        wake();
    }
    let mut player = Player::connect_new(&output.mixer);
    player.set_volume(slider_gain(&status, volume));
    // The playing source's fade-out and normalization controls (see `Meter`).
    let mut fade_out = Arc::new(AtomicU32::new(0));
    let mut norm = Arc::new(AtomicU32::new(1f32.to_bits()));
    // Songs fading out under the current one (more than one only for a moment).
    let mut fading: Vec<Fading> = Vec::new();
    // Next song queued right behind the current one in `player` for gapless playback.
    let mut queued: Option<Queued> = None;
    // Whether `Ending` (crossfade) or `PrepareNext` (gapless) went out for this generation.
    let mut ending_sent = false;
    let mut prepare_sent = false;
    // The playing song's length. Not `status.duration()` at a handoff: the UI has
    // already put the next song's there.
    let mut length: Option<Duration> = None;
    // How the playing song ends, once its tail is scanned (None: not asked for yet).
    let mut tail: Option<Arc<std::sync::OnceLock<Tail>>> = None;
    let tail_known = |t: &Option<Arc<std::sync::OnceLock<Tail>>>| t.as_ref().and_then(|t| t.get().copied());

    // Generation of the source currently in the player (0 = nothing loaded).
    let mut current: u64 = 0;
    let mut shared: Option<Arc<Shared>> = None;
    // Where the playing song is up to, on its own clock (see `Tempo`).
    let mut song_ms = Arc::new(AtomicU64::new(0));
    let song_pos = |ms: &AtomicU64| Duration::from_millis(ms.load(Ordering::Relaxed));
    let tempo = |decoder, from, ms: &Arc<AtomicU64>| tempo::Tempo::new(decoder, status.speed.clone(), from, ms.clone());
    // Drops any staged or queued gapless track and resets staging state. With `discard`
    // true (the player itself is being cleared, replaced, or ran out), drops a queued entry
    // even if it just started; with `discard` false (ClearNext, Pause, Seek, DeviceLost),
    // keeps an already-started entry so its pending `GaplessStarted` message still promotes it.
    // Its download goes on: the backend still holds it, to play from when that song is
    // skipped to or staged again, and cancels it once it's moot.
    let clear_queued = |queued: &mut Option<Queued>, prepare_sent: &mut bool, discard: bool| -> bool {
        status.staged_gen.store(0, Ordering::Release);
        *prepare_sent = false;
        let Some(q) = queued.as_ref() else { return false };
        if !discard && q.started.load(Ordering::Acquire) {
            return false;
        }
        q.cancelled.store(true, Ordering::Release);
        queued.take();
        true
    };
    let start_prepared_seek = |s: &Arc<Shared>, is_seekable: bool| -> Prepared {
        let prepared = Prepared::default();
        // (HLS segments need no index: their decoder seeks as it is.)
        if !is_seekable && !s.opus && s.segments.is_none() && s.duration_ms >= 20 * 60 * 1000 {
            let (sender, receiver) = crossbeam_channel::bounded(1);
            let shared = s.clone();
            std::thread::spawn(move || {
                let _ = sender.send(stream::decoder(shared, true));
            });
            *prepared.lock().unwrap() = Some(receiver);
        }
        prepared
    };
    // Whether the playing decoder can seek in place (it indexed the whole file).
    let mut seekable = false;
    // Long AAC files need an index before seeking. Prepare it while they play.
    let mut prepared_seek = Prepared::default();
    // Latest seek; a slower, older seek's decoder must not land after a newer one.
    let mut seek_seq: u64 = 0;
    let seek_job: Arc<std::sync::Mutex<Option<SeekJob>>> = Default::default();
    let seek_busy = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // Dev hook: YTM_DEVICE_LOST=<secs> acts as if the output device disconnected that
    // long into the first song (the error callback firing for a device that's gone).
    let mut dev_lost = std::env::var("YTM_DEVICE_LOST").ok().and_then(|v| v.parse::<f32>().ok()).map(Duration::from_secs_f32);
    // Since when nothing has played, to let go of the device after `IDLE_CLOSE`.
    let mut idle_since: Option<Instant> = Some(Instant::now());
    // When a pause fading out really pauses the players.
    let mut pause_at: Option<Instant> = None;
    // When the sleep timer pauses, whether it has just gone off (see `sleep_gain`), and
    // what it leaves of the volume now.
    let mut sleep: Option<SleepTimer> = None;
    let mut sleep_hush = false;
    let mut sleep_vol = 1.0f32;
    let mut sleep_rise = None;
    // Where the playing song went silent, for `silent_tail`.
    let mut quiet_from: Option<Duration> = None;
    // Where to carry on from after a pause dropped an AirPlay speaker's buffer.
    let mut rewind: Option<Duration> = None;
    // Paused by an AirPlay 2 speaker pausing itself, which holds what it was sent.
    let mut held = false;
    // Whether the volume was the speaker's, last time round: it changes hands with the output.
    let mut on_speaker = false;
    loop {
        // Poll while playing so the position stays fresh and the end is noticed; idle otherwise,
        // waking to let go of the device.
        let timeout = match idle_since {
            _ if sleep_rise.is_some() => Duration::from_millis(10),
            None => Duration::from_millis(100),
            Some(since) if output.is_open() => IDLE_CLOSE.saturating_sub(since.elapsed()).max(Duration::from_millis(10)),
            Some(_) => Duration::from_secs(3600),
        };
        let timeout = [pause_at, sleep.map(|t| t.at)].into_iter().flatten().fold(timeout, |t, at| t.min(at.saturating_duration_since(Instant::now())));
        match rx.recv_timeout(timeout) {
            Ok(AudioCmd::Start { generation, mut decoder, shared: s, at }) => {
                if generation != status.wanted_gen.load(Ordering::Relaxed) {
                    log::debug!("audio: dropping stale generation {generation}");
                    continue;
                }
                log::debug!("audio: start generation {generation}");
                // Starting up again after being let go is the listener's doing, not the next
                // song coming along: a speaker that failed gets another go.
                if !output.is_open() {
                    output.remote_retry = true;
                }
                if (!output.is_open() || output.default_changed()) && !output.open(tx.clone(), &status) {
                    let _ = events.send(AudioEvent::Error(crate::tr!("audio-no-device").to_owned()));
                    wake();
                }
                clear_queued(&mut queued, &mut prepare_sent, true);
                clear(&mut player, &output);
                // A song asked for while paused plays. What was fading out under the paused
                // one goes, rather than come back up.
                pause_at = None;
                if std::mem::take(&mut held) {
                    output.remote.as_ref().inspect(|a| a.resume());
                }
                rewind = None;
                if status.hushed.swap(false, Ordering::Relaxed) {
                    fading.clear();
                }
                let gain = norm_gain(&status, Some(&s));
                show_norm(&status, Some(&s));
                log::info!("audio: loudness {:?} dB, normalization gain {gain:.3}", s.loudness_db);
                // The timer's fade carries on across songs; one started after it went off
                // plays at full volume.
                if sleep_hush && sleep.is_none() {
                    sleep_vol = 1.0;
                } else if fading.iter().all(|f| f.player.empty())
                    && let Some(timer) = sleep
                {
                    sleep_vol = sleep_vol.min(timer.gain(Instant::now()));
                }
                sleep_hush = false;
                player.set_volume(bar_gain(&status, &output, f32::from_bits(status.volume.load(Ordering::Relaxed))) * sleep_vol);
                // Fade in only over a song still playing, both over the same length.
                fading.retain(|f| !f.player.empty());
                // Crossfaded into from its start, a song's silence before its music is
                // skipped, so the fade lands on the music. Only as far as its start is in
                // memory (a prefetched song's is): this thread mustn't wait on the download.
                let lead = if at.is_zero() && !s.feed && fading.iter().any(|f| f.waiting.is_some()) {
                    let (ch, rate) = (rodio::Source::channels(&decoder).get() as usize, rodio::Source::sample_rate(&decoder).get());
                    skip_lead(&mut decoder, ch, rate, s.head_in_memory().saturating_sub(LEAD_MARGIN).min(MAX_LEAD))
                } else {
                    Duration::ZERO
                };
                let crossfade = status.crossfade_ms.load(Ordering::Relaxed);
                let mut fade_in = 0;
                for f in fading.iter_mut().filter(|f| f.waiting.is_some()) {
                    fade_in = f.start(crossfade);
                    log::debug!("audio: crossfade into generation {generation} over {fade_in} ms, {lead:?} of silence skipped");
                }
                fade_out = Arc::new(AtomicU32::new(0));
                norm = Arc::new(AtomicU32::new(gain.to_bits()));
                song_ms = Arc::new(AtomicU64::new(0));
                player.append(Meter::new(tempo(decoder, at + lead, &song_ms), status.clone(), fade_in, fade_out.clone()).with_norm(norm.clone()));
                player.play();
                // 0 means the stream didn't say; keep the length the UI already knows.
                if s.duration_ms > 0 {
                    status.duration_ms.store(s.duration_ms, Ordering::Relaxed);
                }
                length = status.duration();
                quiet_from = None;
                tail = None;
                if let Some(old) = shared.replace(s.clone())
                    && !Arc::ptr_eq(&old, &s)
                {
                    old.cancel();
                }
                current = generation;
                seekable = !at.is_zero();
                // A new slot: a seek still being built for the last song keeps the old one.
                prepared_seek = start_prepared_seek(&s, seekable);
                ending_sent = false;
                status.stream_ms.store(s.duration_ms, Ordering::Relaxed);
                status.started_gen.store(generation, Ordering::Release);
                status.playing.store(true, Ordering::Relaxed);
            }
            Ok(AudioCmd::QueueNext { after, generation, tempo: next_tempo, song_ms: next_song_ms, shared: s, fallback_duration_ms }) => {
                let prev_failed = (after == current
                    && current != 0
                    && status.staged_gen.load(Ordering::Acquire) == generation
                    && !player.empty()
                    && status.gapless.load(Ordering::Relaxed)
                    && !status.hushed.load(Ordering::Relaxed)
                    && queued.is_none())
                    .then(|| shared.as_ref().map(|c| c.failed_flag()))
                    .flatten();
                let Some(prev_failed) = prev_failed else {
                    if shared.as_ref().is_none_or(|c| !Arc::ptr_eq(c, &s)) {
                        s.cancel();
                    }
                    continue;
                };
                let gain = norm_gain(&status, Some(&s));
                let (next_fade_out, next_norm) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(gain.to_bits())));
                let (cancelled, started) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
                let gate = GaplessGate { generation, cancelled: cancelled.clone(), prev_failed, started: started.clone(), tx: tx.clone() };
                player.append(Meter::new(*next_tempo, status.clone(), 0, next_fade_out.clone()).with_norm(next_norm.clone()).with_gate(gate));
                log::debug!("audio: queued gapless generation {generation} behind {after} (norm {gain:.3})");
                queued = Some(Queued { generation, shared: s, fallback_duration_ms, fade_out: next_fade_out, song_ms: next_song_ms, norm: next_norm, cancelled, started });
            }
            Ok(AudioCmd::ClearNext) => {
                if clear_queued(&mut queued, &mut prepare_sent, false) {
                    log::debug!("audio: cleared queued gapless track");
                }
            }
            Ok(AudioCmd::GaplessStarted(generation)) => {
                if !queued.as_ref().is_some_and(|q| q.generation == generation) {
                    continue;
                }
                let q = queued.take().unwrap();
                // If the UI concurrently asked for a different song, don't override it.
                if status.wanted_gen.compare_exchange(current, q.generation, Ordering::AcqRel, Ordering::Relaxed).is_err() {
                    log::debug!("audio: dropping gapless generation {generation}, wanted_gen moved on");
                    clear(&mut player, &output);
                    if shared.as_ref().is_none_or(|c| !Arc::ptr_eq(c, &q.shared)) {
                        q.shared.cancel();
                    }
                    current = 0;
                    continue;
                }
                let _ = status.staged_gen.compare_exchange(q.generation, 0, Ordering::AcqRel, Ordering::Relaxed);
                log::debug!("audio: gapless switched {current} -> {generation}");
                if let Some(old) = shared.replace(q.shared.clone())
                    && !Arc::ptr_eq(&old, &q.shared)
                {
                    old.cancel();
                }
                current = q.generation;
                fade_out = q.fade_out;
                song_ms = q.song_ms;
                norm = q.norm;
                seekable = false;
                prepared_seek = start_prepared_seek(&q.shared, false);
                quiet_from = None;
                tail = None;
                ending_sent = false;
                prepare_sent = false;
                let dur_ms = if q.shared.duration_ms > 0 { q.shared.duration_ms } else { q.fallback_duration_ms };
                status.duration_ms.store(dur_ms, Ordering::Relaxed);
                length = status.duration();
                show_norm(&status, Some(&q.shared));
                status.stream_ms.store(q.shared.duration_ms, Ordering::Relaxed);
                status.started_gen.store(q.generation, Ordering::Release);
                status.set_position(song_pos(&song_ms).as_millis() as u64);
                status.playing.store(true, Ordering::Relaxed);
                let _ = events.send(AudioEvent::GaplessAdvanced { generation: q.generation });
                wake();
            }
            Ok(AudioCmd::Replace { generation, seek, decoder, at, seekable: s }) => {
                if generation != current || seek != seek_seq {
                    continue;
                }
                log::debug!("audio: seek to {at:?} via new decoder (seekable: {s})");
                seekable = s;
                // The next song may have started behind this one meanwhile: it goes with it.
                if let Some(q) = &queued {
                    q.fade_out.store(SEEK_FADE_MS, Ordering::Relaxed);
                }
                clear_queued(&mut queued, &mut prepare_sent, true);
                // What went out from the old spot while the decoder was being built.
                output.flush();
                song_ms = Arc::new(AtomicU64::new(0));
                let heard = output.pulled();
                put_in(&mut player, &output.mixer, &mut fading, &mut fade_out, &norm, &status, tempo(decoder, at, &song_ms), heard);
                // Paused, nothing else tells the UI (and the system media controls) where it is now.
                status.set_position(at.as_millis() as u64);
                wake();
            }
            Ok(AudioCmd::SleepAt(at)) => sleep = at.map(|at| SleepTimer::new(at, Instant::now())),
            Ok(AudioCmd::Pause) if output.pauses() && !player.is_paused() => {
                // It stops at once, at what it was playing, and keeps the rest; playing
                // again picks up there. No fade: what it holds was sent before this.
                log::debug!("audio: pause at {:?}, the speaker pausing itself", song_pos(&song_ms));
                // As any pause: the next song chained behind for gapless doesn't start, nor
                // is it staged again until playing resumes (`hushed`).
                clear_queued(&mut queued, &mut prepare_sent, false);
                status.hushed.store(true, Ordering::Relaxed);
                output.remote.as_ref().inspect(|a| a.pause());
                held = true;
                pause_at = None;
                player.pause();
                fading.iter().for_each(|f| f.player.pause());
                status.set_heard(song_pos(&song_ms));
                status.playing.store(false, Ordering::Relaxed);
                wake();
            }
            Ok(AudioCmd::Pause) => {
                log::debug!("audio: pause at {:?}", song_pos(&song_ms));
                // Cancel any unstarted or in-flight gapless track so pausing near the end
                // can't start the next song; resuming re-stages it if still near the end.
                clear_queued(&mut queued, &mut prepare_sent, false);
                // Fade out first; the players pause once it's done (below). Another pause
                // meanwhile (the media controls' and the window's) doesn't put that off.
                // With the fade off, it's over in a few ms.
                status.hushed.store(true, Ordering::Relaxed);
                if !player.is_paused() && pause_at.is_none() {
                    let after = if status.instant_pause.load(Ordering::Relaxed) { INSTANT_PAUSE_AFTER } else { PAUSE_AFTER };
                    pause_at = Some(Instant::now() + after);
                }
                status.playing.store(false, Ordering::Relaxed);
                // So the UI (and the system media controls it syncs) see it even when idle.
                wake();
            }
            Ok(AudioCmd::Resume) => {
                log::debug!("audio: resume (generation {current})");
                if current != 0 {
                    output.remote_retry = true;
                    // Headphones may have come back (or gone) while paused.
                    if !output.is_open() || output.default_changed() {
                        output.open(tx.clone(), &status);
                    }
                    if std::mem::take(&mut held) {
                        output.remote.as_ref().inspect(|a| a.resume());
                    }
                    // The speaker never played what it dropped at the pause: back to there.
                    if let Some(pos) = rewind.take() {
                        let _ = tx.send(AudioCmd::Seek(pos));
                    }
                    // The sleep pause can still be fading out.
                    if sleep_hush && sleep.is_none() {
                        sleep_vol = 1.0;
                        let v = bar_gain(&status, &output, f32::from_bits(status.volume.load(Ordering::Relaxed)));
                        player.set_volume(v);
                        fading.iter().for_each(|f| f.player.set_volume(v));
                    }
                    // Fades back in from where the pause got to (in a few ms with the fade off).
                    status.hushed.store(false, Ordering::Relaxed);
                    sleep_hush = false;
                    pause_at = None;
                    player.play();
                    fading.iter().for_each(|f| f.player.play());
                    status.playing.store(true, Ordering::Relaxed);
                    wake();
                }
            }
            // One crossfade at a time: skipping again mid-fade cuts over (below) instead
            // of piling a third song on.
            Ok(AudioCmd::Handoff)
                if status.crossfade_ms.load(Ordering::Relaxed) > 0
                    && current != 0
                    && !player.is_paused()
                    && !player.empty()
                    && fading.iter().all(|f| f.player.empty()) =>
            {
                // Cancel any staged gapless track before handing the old player over to fade out.
                clear_queued(&mut queued, &mut prepare_sent, true);
                // Keeps playing at full volume until the next song starts (see `Start`).
                // Left: of its music, in real time, at the speed it's playing.
                let (pos, known) = (song_pos(&song_ms), tail_known(&tail));
                let end = music_end(length, known);
                let left = end.map(|e| e.saturating_sub(pos).div_f32(status.speed()));
                let outro = known.zip(end).is_some_and(|(t, e)| t.outro(e.saturating_sub(pos)));
                log::debug!("audio: handing off from generation {current}, {left:?} left (ends {known:?}, in its outro: {outro})");
                let old = std::mem::replace(&mut player, Player::connect_new(&output.mixer));
                fading.push(Fading { player: old, shared: shared.take(), norm: norm.clone(), out: fade_out.clone(), waiting: Some((Instant::now(), left)), outro });
                current = 0;
                prepared_seek = Prepared::default();
                status.playing.store(false, Ordering::Relaxed);
                status.set_position(0);
            }
            Ok(cmd @ (AudioCmd::Stop | AudioCmd::Handoff)) => {
                clear_queued(&mut queued, &mut prepare_sent, true);
                // Cut short (a skip, or stop), not a song that ended: a speaker's buffer of
                // it goes too. One that ended keeps it, the end of the song still to be heard.
                if (current != 0 && !player.empty()) || fading.iter().any(|f| !f.player.empty()) {
                    output.flush();
                }
                rewind = None;
                if matches!(cmd, AudioCmd::Handoff) {
                    // Skipping again mid-crossfade: everything playing goes, quickly enough
                    // to sound like a cut but without a click, and the next song comes in
                    // at full volume.
                    for f in fading.iter_mut() {
                        f.waiting = None;
                        f.out.store(QUICK_FADE_MS, Ordering::Relaxed);
                    }
                    log::debug!("audio: skipped mid-crossfade, cutting over");
                    if current != 0 && !player.is_paused() && !player.empty() {
                        fade_out.store(QUICK_FADE_MS, Ordering::Relaxed);
                        let old = std::mem::replace(&mut player, Player::connect_new(&output.mixer));
                        fading.push(Fading { player: old, shared: shared.take(), norm: norm.clone(), out: fade_out.clone(), waiting: None, outro: false });
                    } else {
                        clear(&mut player, &output);
                    }
                } else {
                    clear(&mut player, &output);
                    fading.clear();
                }
                current = 0;
                prepared_seek = Prepared::default();
                // Also stops a seek still waiting on this download.
                if let Some(old) = shared.take() {
                    old.cancel();
                }
                status.playing.store(false, Ordering::Relaxed);
                status.set_position(0);
            }
            Ok(cmd @ (AudioCmd::Seek(_) | AudioCmd::SeekLive { .. })) => {
                let (pos, live) = match cmd {
                    AudioCmd::SeekLive { pos, at } => (pos + at.elapsed(), Some((pos, at))),
                    AudioCmd::Seek(pos) => (pos, None),
                    _ => unreachable!(),
                };
                seek_seq += 1;
                if current == 0 {
                    continue;
                }
                // Silence before the seek isn't silence after it (a hidden track's start).
                quiet_from = None;
                clear_queued(&mut queued, &mut prepare_sent, false);
                // Back from the end (an A-B loop going round, say): the crossfade into the
                // next song is due again when it gets there.
                ending_sent = false;
                // A seek while paused is where to carry on from.
                rewind = None;
                output.flush();
                // Seeking ends a crossfade: the new song alone from here.
                for f in &fading {
                    f.out.store(QUICK_FADE_MS, Ordering::Relaxed);
                }
                // Paused long enough, the device was let go: a seek in place waits on it.
                if !output.is_open() && output.open(tx.clone(), &status) {
                    idle_since = Some(Instant::now());
                }
                // A seekable decoder already indexed the (fully downloaded) file. Without a
                // device to take the seek, a new decoder (below) instead. Likewise for a
                // podcast feed's file not all in memory: its seek reads at the new spot,
                // which would wait on the download inside the device's callback.
                let in_place = seekable && shared.as_ref().is_none_or(|s| !s.feed || s.is_complete());
                if in_place && output.pulled() && player.try_seek(pos).is_ok() {
                    log::debug!("audio: seek to {pos:?} in place");
                    status.set_position(pos.as_millis() as u64);
                    wake();
                    continue;
                }
                // The playing decoder was opened non-seekable for a fast start. Build a
                // seekable one over the same buffer off-thread (it needs every fragment
                // header, so it may wait for the download) and swap it in. Back to the
                // start, a fresh non-seekable decoder is enough and opens at once.
                if let Some(s) = shared.clone() {
                    // One builder at a time, always for the latest seek: a burst of seeks
                    // (a jam catching up) mustn't pile up threads waiting on the download.
                    *seek_job.lock().unwrap() = Some(SeekJob { shared: s, prepared: prepared_seek.clone(), pos, live, generation: current, seek: seek_seq });
                    if !seek_busy.swap(true, Ordering::AcqRel) {
                        let (job, busy, tx, events) = (seek_job.clone(), seek_busy.clone(), tx.clone(), events.clone());
                        std::thread::spawn(move || {
                            loop {
                                let next = job.lock().unwrap().take();
                                match next {
                                    Some(j) => build_seek(j, &job, &tx, &events),
                                    None => {
                                        busy.store(false, Ordering::Release);
                                        // A seek that came in just now finds `busy` already false, or is taken here.
                                        if job.lock().unwrap().is_none() || busy.swap(true, Ordering::AcqRel) {
                                            break;
                                        }
                                    }
                                }
                            }
                        });
                    }
                }
            }
            // Failing here is `Start`'s to report. Opened while idle, the device gets the
            // full `IDLE_CLOSE` again, in case the song never comes.
            Ok(AudioCmd::Warm) => {
                if !output.is_open() {
                    output.remote_retry = true;
                }
                if !output.is_open() && output.open(tx.clone(), &status) {
                    idle_since = idle_since.map(|_| Instant::now());
                }
            }
            Ok(AudioCmd::DeviceLost) => {
                let was = output.device.clone();
                // A speaker that hung up (on to another device, or turned off) isn't taken
                // straight back: playing again tries it.
                if output.remote.is_some() {
                    output.remote_retry = false;
                    output.notice = Some(crate::tr!("airplay-lost").to_owned());
                    *status.remote.lock().unwrap() = RemoteState::Failed(crate::tr!("airplay-lost").to_owned());
                }
                output.open(tx.clone(), &status);
                // The device itself went away, not a hiccup on the same one: pause, as
                // unplugging headphones does, rather than carry on out of the speakers.
                if output.device != was && !player.is_paused() && (current != 0 || !fading.is_empty()) {
                    log::info!("audio: output device gone, pausing");
                    clear_queued(&mut queued, &mut prepare_sent, false);
                    status.hushed.store(true, Ordering::Relaxed);
                    status.silence();
                    pause_at = None;
                    player.pause();
                    fading.iter().for_each(|f| f.player.pause());
                    status.playing.store(false, Ordering::Relaxed);
                    wake();
                }
            }
            Ok(AudioCmd::SetDestination(to)) => {
                log::info!("audio: playing on {to:?}");
                if to.is_remote() {
                    destination::discover();
                }
                // The same picked again (the menu stays open) leaves a connection going on
                // as it is. A retry after a failure still connects: `remote_retry` is on
                // again, and the card in use isn't the speakers.
                if to != output.destination {
                    *status.remote.lock().unwrap() = RemoteState::Idle;
                }
                output.destination = to;
                output.remote_retry = true;
                // Speakers no longer picked aren't to be played on once they're connected,
                // even with no sound card to open in their place.
                if output.connecting.as_ref().is_some_and(|(k, _)| *k != output.destination.key()) {
                    output.connecting = None;
                }
                // A device let go while idle opens again with the next song.
                if (output.is_open() || idle_since.is_none()) && (!output.is_open() || output.default_changed()) {
                    output.open(tx.clone(), &status);
                }
            }
            Ok(AudioCmd::Connected { attempt, name, kind, session }) => {
                if output.handshake.as_ref().is_some_and(|(_, a)| *a == attempt) {
                    output.handshake = None;
                }
                // Let go of (or another picked) meanwhile: its session goes.
                if output.connecting.as_ref().map(|(_, a)| *a) != Some(attempt) {
                    continue;
                }
                output.connecting = None;
                if !output.connected(&name, kind, session, tx.clone(), &status) && !output.open(tx.clone(), &status) {
                    let _ = events.send(AudioEvent::Error(crate::tr!("audio-no-device").to_owned()));
                    wake();
                }
            }
            Ok(cmd @ (AudioCmd::Volume(_) | AudioCmd::Refresh)) => {
                let v = f32::from_bits(status.volume.load(Ordering::Relaxed));
                if let (AudioCmd::Volume(v), Some(a)) = (cmd, &output.remote) {
                    a.set_volume(v);
                }
                let v = bar_gain(&status, &output, v) * sleep_vol;
                player.set_volume(v);
                norm.store(norm_gain(&status, shared.as_deref()).to_bits(), Ordering::Relaxed);
                // The level may be what changed.
                if shared.is_some() {
                    show_norm(&status, shared.as_deref());
                }
                if let Some(q) = &queued {
                    q.norm.store(norm_gain(&status, Some(&q.shared)).to_bits(), Ordering::Relaxed);
                }
                for f in &fading {
                    f.player.set_volume(v);
                    f.norm.store(norm_gain(&status, f.shared.as_deref()).to_bits(), Ordering::Relaxed);
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
        }

        if sleep.is_some_and(|t| t.at <= Instant::now()) {
            sleep = None;
            sleep_hush = true;
            log::info!("audio: sleep timer, pausing");
            let _ = tx.send(AudioCmd::Pause);
        }
        if current != 0 && dev_lost.is_some_and(|lost| song_pos(&song_ms) >= lost) {
            dev_lost = None;
            log::info!("audio: dev hook: output device lost");
            output.close();
            output.device = Some("dev-hook-gone".into());
            output.lost.store(true, Ordering::Relaxed);
            let _ = tx.send(AudioCmd::DeviceLost);
        }
        // A pause has faded out (the quick way, if the fade was turned off on the way).
        pause_at = pause_due(pause_at, status.instant_pause.load(Ordering::Relaxed), Instant::now());
        if pause_at.is_some_and(|at| at <= Instant::now()) {
            pause_at = None;
            status.silence();
            player.pause();
            fading.iter().for_each(|f| f.player.pause());
            // A speaker would otherwise play on for its buffer's length: it stops now, and
            // playing again goes back to what it last played.
            if output.remote.is_some() && !output.pauses() && current != 0 {
                output.flush();
                rewind = Some(status.set_heard(song_pos(&song_ms)));
            }
        }
        // The sleep timer's fade, and the volume back once it's over or called off.
        let paused = player.is_paused();
        sleep_hush &= !paused;
        let now = Instant::now();
        let target = sleep_gain(sleep, sleep_hush, paused, now);
        let next = if target > sleep_vol && !paused {
            let last = sleep_rise.replace(now).unwrap_or(now);
            (sleep_vol + (now - last).as_secs_f32() / SLEEP_RISE.as_secs_f32()).min(target)
        } else {
            target
        };
        if next == target {
            sleep_rise = None;
        }
        if next != sleep_vol {
            sleep_vol = next;
            let v = bar_gain(&status, &output, f32::from_bits(status.volume.load(Ordering::Relaxed))) * sleep_vol;
            player.set_volume(v);
            for f in &fading {
                f.player.set_volume(v);
            }
        }
        if let Some(notice) = output.notice.take() {
            let _ = events.send(AudioEvent::Error(notice));
            wake();
        }
        // Follow the default device while playing (headphones connecting take over).
        // A failed device is `DeviceLost`'s to handle (it decides whether to pause).
        if (current != 0 || !fading.is_empty())
            && !player.is_paused()
            && output.checked.elapsed() >= DEVICE_CHECK
            && !output.lost.load(Ordering::Relaxed)
            && (!output.is_open() || output.default_changed())
        {
            output.open(tx.clone(), &status);
        }
        // A speaker that held a pause gone (let go after a while, or another device picked):
        // what it held is lost, so playing again goes back to what it last played.
        if held && !output.pauses() {
            held = false;
            if current != 0 {
                rewind = Some(status.set_heard(song_pos(&song_ms)));
            }
        }
        // The volume's the speaker's while one plays, and the players' otherwise. The slider
        // shows the speaker's own level meanwhile, the app's kept aside, and back after.
        if output.remote.is_some() != on_speaker {
            on_speaker = !on_speaker;
            if let Some(level) = output.speaker_level.take().filter(|_| on_speaker) {
                if status.kept().is_none() {
                    status.keep(Some(f32::from_bits(status.volume.load(Ordering::Relaxed))));
                }
                status.volume.store(level.to_bits(), Ordering::Relaxed);
            } else if let Some(kept) = status.kept().filter(|_| !on_speaker) {
                status.volume.store(kept.to_bits(), Ordering::Relaxed);
                status.keep(None);
            }
            // Normalization is each song's `Meter`'s, not the players'.
            let v = bar_gain(&status, &output, f32::from_bits(status.volume.load(Ordering::Relaxed))) * sleep_vol;
            player.set_volume(v);
            for f in &fading {
                f.player.set_volume(v);
            }
            // The slider may have just become the speaker's own level.
            wake();
        }
        // Let go of the device once nothing has played for a while; playing opens it again.
        if (current != 0 || !fading.is_empty()) && !player.is_paused() {
            idle_since = None;
        } else if idle_since.get_or_insert_with(Instant::now).elapsed() >= IDLE_CLOSE && output.is_open() {
            log::info!("audio: nothing playing, letting go of the output device");
            output.close();
            output.device = None;
            status.remote_gone();
        }
        if !fading.is_empty() {
            // The next song is taking too long: don't hold this one up any longer.
            for f in fading.iter_mut().filter(|f| f.waiting.is_some_and(|(at, _)| at.elapsed() > HANDOFF_WAIT)) {
                log::debug!("audio: next song not ready, fading out over {} ms", f.start(status.crossfade_ms.load(Ordering::Relaxed)));
            }
            fading.retain(|f| !f.player.empty());
            if fading.is_empty() {
                log::debug!("audio: crossfade done");
            }
        }
        if current != 0 {
            let pos = song_pos(&song_ms);
            status.set_position(pos.as_millis() as u64);
            // Start the next song early, to crossfade into it, ending where its music
            // does. Not for songs barely longer than the fade, nor while a pause fades out
            // (it'd start the next one playing; and the end can come closer then, once
            // it's known). (The fade's in real time: more of the song when faster.) Not a
            // podcast feed's file: the ads its host puts in make it run longer than YouTube
            // Music's length, so it plays to its real end.
            let crossfade = Duration::from_millis(status.crossfade_ms.load(Ordering::Relaxed) as u64).mul_f32(status.speed());
            let fades = !crossfade.is_zero() && shared.as_ref().is_some_and(|s| s.error().is_none() && !s.feed);
            // Where its music ends is looked for a while before, once it's all here: a
            // second decoder then never waits on (or steers) the download. (Long mixes,
            // never kept whole, end as before.)
            if fades
                && tail.is_none()
                && let (Some(d), Some(s)) = (length, &shared)
                && pos + TAIL * 2 >= d
                && s.is_complete()
            {
                let (slot, s) = (Arc::new(std::sync::OnceLock::new()), s.clone());
                tail = Some(slot.clone());
                std::thread::spawn(move || {
                    let found = Tail::scan(s, d);
                    log::debug!("audio: song ends {found:?}");
                    if let Some(t) = found {
                        let _ = slot.set(t);
                    }
                });
            }
            if !ending_sent
                && fades
                && !status.hushed.load(Ordering::Relaxed)
                && !status.stop_at_end.load(Ordering::Relaxed)
                // A song with hardly any music before its silence crossfades from its
                // end, as before.
                && let Some(end) = music_end(length, tail_known(&tail)).filter(|e| *e > crossfade * 2).or(length)
                && end > crossfade * 2
                && pos + crossfade >= end
            {
                ending_sent = true;
                let _ = events.send(AudioEvent::Ending { generation: current });
                wake();
            }
            // Stage the next song's decoder ahead of the end so it chains sample-accurately.
            let stage_before = GAPLESS_STAGE_BEFORE.mul_f32(status.speed());
            if !prepare_sent
                && queued.is_none()
                && crossfade.is_zero()
                && status.gapless.load(Ordering::Relaxed)
                && !status.stop_at_end.load(Ordering::Relaxed)
                && !status.hushed.load(Ordering::Relaxed)
                && let Some(d) = length
                && pos + stage_before >= d
                && shared.as_ref().is_some_and(|s| s.error().is_none() && !s.feed)
            {
                prepare_sent = true;
                let _ = events.send(AudioEvent::PrepareNext { generation: current });
                wake();
            }
            // Silence to the end (or before a hidden track): over now, as if it had ended.
            // Not once the next song chained behind it has started (the level and position
            // are still catching up to it), nor while a seek is on its way.
            let next_started = queued.as_ref().is_some_and(|q| q.started.load(Ordering::Acquire));
            if status.skip_silence.load(Ordering::Relaxed)
                && !status.hushed.load(Ordering::Relaxed)
                && !next_started
                && !seek_busy.load(Ordering::Acquire)
                && silent_tail(&mut quiet_from, status.level(), pos, length)
            {
                if queued.is_some() {
                    // The next song is chained behind for gapless: end only this one, and the
                    // next goes on from it without a gap, as at its real end.
                    if fade_out.load(Ordering::Relaxed) != QUICK_FADE_MS {
                        log::info!("audio: silent from {:?} on, going on to the next song", quiet_from);
                        fade_out.store(QUICK_FADE_MS, Ordering::Relaxed);
                    }
                } else {
                    log::info!("audio: silent from {:?} on, ending the song there", quiet_from);
                    clear(&mut player, &output);
                }
            }
            // Paused just before the end, the song's over only once it's played again:
            // the next one mustn't start playing.
            if player.empty() && !status.hushed.load(Ordering::Relaxed) {
                clear_queued(&mut queued, &mut prepare_sent, true);
                // A failed download ends the decoder early: that's not the song ending.
                let event = match shared.as_ref().and_then(|s| s.failure()) {
                    Some(failure) => AudioEvent::Interrupted { generation: current, at: song_pos(&song_ms), failure },
                    None => AudioEvent::Finished { generation: current },
                };
                let _ = events.send(event);
                current = 0;
                shared = None;
                status.playing.store(false, Ordering::Relaxed);
                wake();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ALSA as it lists a PC's outputs under PipeWire (this one's, cut down): the plugins
    /// and servers, then each card's ways to one output.
    fn alsa_listing() -> Vec<OutputDevice> {
        let generic = "HD-Audio Generic, ALC897 Analog";
        [
            ("null", "Discard all samples (playback) or generate zero samples (capture)"),
            ("samplerate", "Rate Converter Plugin Using Samplerate Library"),
            ("jack", "JACK Audio Connection Kit"),
            ("pipewire", "PipeWire Sound Server"),
            ("pulse", "PulseAudio Sound Server"),
            ("default", "Default ALSA Output (currently PipeWire Media Server)"),
            ("hdmi:CARD=HDMI,DEV=0", "HDA ATI HDMI, DELL S2421HGF"),
            ("hdmi:CARD=HDMI,DEV=1", "HDA ATI HDMI, BenQ GW2490E"),
            ("usbstream:CARD=HDMI", "HDA ATI HDMI"),
            ("sysdefault:CARD=Generic", generic),
            ("front:CARD=Generic,DEV=0", generic),
            ("surround51:CARD=Generic,DEV=0", generic),
            ("iec958:CARD=Generic,DEV=0", "HD-Audio Generic, ALC897 Digital"),
            ("hw:CARD=1,DEV=3", "HDA ATI HDMI, DELL S2421HGF"),
            ("plughw:CARD=1,DEV=3", "HDA ATI HDMI, DELL S2421HGF"),
            ("hw:CARD=2,DEV=0", generic),
            ("plughw:CARD=2,DEV=0", generic),
            ("hw:CARD=2,DEV=1", "HD-Audio Generic, ALC897 Digital"),
            ("plughw:CARD=2,DEV=1", "HD-Audio Generic, ALC897 Digital"),
        ]
        .into_iter()
        .map(|(pcm, name)| OutputDevice { id: format!("alsa:{pcm}"), name: name.into() })
        .collect()
    }

    fn ids(list: &[OutputDevice]) -> Vec<&str> {
        list.iter().map(|d| d.id.as_str()).collect()
    }

    #[test]
    fn alsa_offers_each_output_once() {
        let offered = offered(alsa_listing(), None);
        // By name: "HD-Audio â€¦" before "HDA â€¦".
        assert_eq!(ids(&offered), ["alsa:sysdefault:CARD=Generic", "alsa:iec958:CARD=Generic,DEV=0", "alsa:hdmi:CARD=HDMI,DEV=1", "alsa:hdmi:CARD=HDMI,DEV=0"]);
        assert_eq!(offered[0].name, "HD-Audio Generic, ALC897 Analog");
    }

    #[test]
    fn a_picked_output_stays_listed_as_it_was_picked() {
        // Picked before the list was cut down: shown, not "not connected", and in place of
        // the way the list would offer that output.
        for picked in ["alsa:hw:CARD=2,DEV=0", "alsa:pipewire"] {
            let offered = offered(alsa_listing(), Some(picked));
            assert!(ids(&offered).contains(&picked), "{picked}");
            assert_eq!(offered.len(), if picked.contains("pipewire") { 5 } else { 4 }, "{picked}");
        }
    }

    #[test]
    fn a_sound_server_is_offered_where_alsas_default_isnt_it() {
        // ALSA's default plays on the card itself (PipeWire's plugin there, but not made the
        // default): ALSA then lists no `default`, only each card's. PipeWire is the only way
        // to its other outputs, Bluetooth headphones among them (#397).
        let on_card = |list: Vec<OutputDevice>| list.into_iter().filter(|d| d.id != "alsa:default").collect::<Vec<_>>();
        let offered_ids = |list| ids(&offered(list, None)).iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let listed = offered_ids(on_card(alsa_listing()));
        assert!(listed.contains(&"alsa:pipewire".to_owned()) && !listed.contains(&"alsa:pulse".to_owned()), "{listed:?}");
        assert_eq!(listed.len(), 5);
        // Only PulseAudio: that's offered.
        let no_pipewire = |list: Vec<OutputDevice>| list.into_iter().filter(|d| d.id != "alsa:pipewire").collect::<Vec<_>>();
        assert!(offered_ids(on_card(no_pipewire(alsa_listing()))).contains(&"alsa:pulse".to_owned()));
        // The default is the server already: not offered twice (`alsa_offers_each_output_once`).
        let pulse_default = no_pipewire(alsa_listing()).into_iter().map(|d| if d.id == "alsa:default" { OutputDevice { name: "Default ALSA Output (currently PulseAudio Sound Server)".into(), ..d } } else { d }).collect();
        assert!(!offered_ids(pulse_default).contains(&"alsa:pulse".to_owned()));
    }

    /// `resolve` with `up` the outputs that play, returning what it picked and what it tried.
    fn resolve_with(state: &mut StandIn<&'static str>, now: Instant, up: &[&str]) -> (Option<&'static str>, Vec<&'static str>) {
        let tried = std::cell::RefCell::new(Vec::new());
        let plays = |d: &&'static str| {
            tried.borrow_mut().push(*d);
            up.contains(d)
        };
        let picked = resolve(state, now, "alsa:default", plays, || vec!["alsa:pipewire", "alsa:sysdefault:CARD=Generic"]);
        (picked, tried.into_inner())
    }

    fn stand_in() -> StandIn<&'static str> {
        StandIn { resolved: None, failed: Vec::new() }
    }

    #[test]
    fn a_default_that_cant_play_is_tried_again_while_nothing_plays() {
        let (mut state, now) = (stand_in(), Instant::now());
        // At login, before the sound server is up: nothing plays, and nothing's kept.
        assert_eq!(resolve_with(&mut state, now, &[]).0, None);
        assert!(state.resolved.is_none());
        assert_eq!(resolve_with(&mut state, now, &["alsa:pipewire"]), (Some("alsa:pipewire"), vec!["alsa:default", "alsa:pipewire"]));
    }

    #[test]
    fn a_stand_in_gives_way_to_the_default_once_it_plays_again() {
        let (mut state, now) = (stand_in(), Instant::now());
        assert_eq!(resolve_with(&mut state, now, &["alsa:pipewire"]).0, Some("alsa:pipewire"));
        // Kept without trying anything for a while, though the default plays again.
        assert_eq!(resolve_with(&mut state, now + STAND_IN_FOR / 2, &["alsa:default", "alsa:pipewire"]), (Some("alsa:pipewire"), vec![]));
        // Then the default's tried, not the stand-in in use (a card's `hw` would be busy).
        let later = now + STAND_IN_FOR;
        assert_eq!(resolve_with(&mut state, later, &["alsa:pipewire"]), (Some("alsa:pipewire"), vec!["alsa:default"]));
        assert_eq!(resolve_with(&mut state, later + STAND_IN_FOR / 2, &["alsa:default"]), (Some("alsa:pipewire"), vec![]));
        assert_eq!(resolve_with(&mut state, later + STAND_IN_FOR, &["alsa:default"]), (Some("alsa:default"), vec!["alsa:default"]));
    }

    #[test]
    fn a_card_gives_way_to_the_sound_server_come_up() {
        let (mut state, now) = (stand_in(), Instant::now());
        let card = "alsa:sysdefault:CARD=Generic";
        assert_eq!(resolve_with(&mut state, now, &[card]).0, Some(card));
        // The server's not up yet at the next check; the card in use isn't tried.
        assert_eq!(resolve_with(&mut state, now + STAND_IN_FOR, &[card]), (Some(card), vec!["alsa:default", "alsa:pipewire"]));
        assert_eq!(resolve_with(&mut state, now + STAND_IN_FOR * 2, &["alsa:pipewire"]), (Some("alsa:pipewire"), vec!["alsa:default", "alsa:pipewire"]));
    }

    #[test]
    fn a_card_isnt_swapped_for_another_as_good() {
        let mut state = stand_in();
        let (now, first, kept) = (Instant::now(), "alsa:sysdefault:CARD=USB", "alsa:sysdefault:CARD=Generic");
        state.resolved = Some(Resolved { stand_in: Some(kept), since: now });
        let tried = std::cell::RefCell::new(Vec::new());
        let plays = |d: &&'static str| {
            tried.borrow_mut().push(*d);
            true
        };
        let later = now + STAND_IN_FOR;
        assert_eq!(resolve(&mut state, later, "alsa:default", |d| *d != "alsa:default" && plays(d), || vec![first, kept]), Some(kept));
        assert!(tried.borrow().is_empty());
    }

    #[test]
    fn a_default_that_plays_is_kept_till_forgotten() {
        let (mut state, now) = (stand_in(), Instant::now());
        assert_eq!(resolve_with(&mut state, now, &["alsa:default"]).0, Some("alsa:default"));
        // In use, so not opened again, even long after.
        assert_eq!(resolve_with(&mut state, now + STAND_IN_FOR * 10, &[]), (Some("alsa:default"), vec![]));
        // It couldn't open: tried again, and the first that plays stands in.
        forget(&mut state, None, now);
        assert_eq!(resolve_with(&mut state, now, &["alsa:sysdefault:CARD=Generic"]).0, Some("alsa:sysdefault:CARD=Generic"));
    }

    #[test]
    fn an_output_that_passed_the_try_but_couldnt_play_is_passed_over_for_a_while() {
        let (mut state, now) = (stand_in(), Instant::now());
        let up = ["alsa:default", "alsa:pipewire"];
        assert_eq!(resolve_with(&mut state, now, &up).0, Some("alsa:default"));
        // It opened for the try, but no stream could be built on it.
        forget(&mut state, Some("alsa:default"), now);
        assert_eq!(resolve_with(&mut state, now, &up), (Some("alsa:pipewire"), vec!["alsa:pipewire"]));
        assert_eq!(resolve_with(&mut state, now + FAILED_FOR, &up), (Some("alsa:default"), vec!["alsa:default"]));
    }

    #[test]
    fn a_default_that_cant_play_gives_way_to_the_sound_server_then_a_card() {
        let ids = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let listed = ids(&["alsa:default", "alsa:hdmi:CARD=NVidia,DEV=1", "alsa:pulse", "alsa:pipewire", "alsa:hw:CARD=Generic,DEV=0", "alsa:sysdefault:CARD=Generic", "alsa:null"]);
        assert_eq!(fallbacks(listed), ["alsa:pipewire", "alsa:pulse", "alsa:sysdefault:CARD=Generic", "alsa:hdmi:CARD=NVidia,DEV=1"]);
        assert_eq!(fallbacks(ids(&["wasapi:{b}", "wasapi:{a}"])), ["wasapi:{b}", "wasapi:{a}"]);
    }

    #[test]
    fn other_systems_outputs_are_offered_as_they_are() {
        let wasapi = |id: &str, name: &str| OutputDevice { id: id.into(), name: name.into() };
        // Two of one name are two outputs, not one listed twice.
        let all = vec![wasapi("wasapi:{b}", "Speakers (Realtek(R) Audio)"), wasapi("wasapi:{a}", "Headphones (WH-1000XM4)"), wasapi("wasapi:{c}", "Speakers (Realtek(R) Audio)")];
        assert_eq!(ids(&offered(all, None)), ["wasapi:{a}", "wasapi:{b}", "wasapi:{c}"]);
    }

    #[test]
    fn a_song_counts_as_started_only_once_the_engine_starts_it() {
        let status = AudioStatus::default();
        status.started_gen.store(1, Ordering::Release);
        status.playing.store(true, Ordering::Relaxed);
        // The UI asked for 2, but the engine hasn't stopped 1 yet.
        assert!(!status.started(2));
        status.started_gen.store(2, Ordering::Release);
        assert!(status.started(2));
        status.playing.store(false, Ordering::Relaxed);
        assert!(!status.started(2));
    }

    #[test]
    fn the_sleep_timer_pauses_when_it_runs_out_and_not_once_called_off() {
        let (events, _rx) = unbounded();
        let audio = spawn(1.0, false, Destination::default(), events, || {});
        // The engine finds the output device before it reads a command, which can take
        // longer than the timer on a slow machine: wait until it has taken one.
        audio.send(AudioCmd::SleepAt(None));
        let up = Instant::now();
        while !audio.tx.is_empty() {
            assert!(up.elapsed() < Duration::from_secs(30), "the engine never started");
            std::thread::sleep(Duration::from_millis(5));
        }
        // Called off: nothing happens at the time it was set for.
        audio.send(AudioCmd::SleepAt(Some(Instant::now() + Duration::from_millis(500))));
        audio.send(AudioCmd::SleepAt(None));
        std::thread::sleep(Duration::from_millis(800));
        assert!(!audio.status.hushed.load(Ordering::Relaxed));
        // Idle, the engine still wakes for it.
        audio.send(AudioCmd::SleepAt(Some(Instant::now() + Duration::from_millis(30))));
        std::thread::sleep(Duration::from_millis(300));
        assert!(audio.status.hushed.load(Ordering::Relaxed));
    }

    #[test]
    fn the_sleep_timer_fades_out_over_its_last_minute() {
        let now = Instant::now();
        let t = SleepTimer::new(now + Duration::from_secs(15 * 60), now);
        // Full volume until the last minute.
        assert_eq!(t.gain(now), 1.0);
        assert_eq!(t.gain(t.at - SLEEP_FADE), 1.0);
        // Then down, a little at first and steadily on: half way it's an eighth (-18 dB).
        let half = t.gain(t.at - SLEEP_FADE / 2);
        assert!((half - 0.125).abs() < 1e-3, "{half}");
        assert!(t.gain(t.at - SLEEP_FADE / 4) < half);
        assert_eq!(t.gain(t.at), 0.0);
        assert_eq!(t.gain(t.at + Duration::from_secs(1)), 0.0);
        // A timer shorter than the fade starts at full volume and fades over what it has.
        let short = SleepTimer::new(now + Duration::from_secs(10), now);
        assert_eq!(short.gain(now), 1.0);
        let half = short.gain(now + Duration::from_secs(5));
        assert!((half - 0.125).abs() < 1e-3, "{half}");
    }

    #[test]
    fn after_the_sleep_timer_the_volume_comes_back_once_paused() {
        let now = Instant::now();
        // Gone off and pausing: it stays down through the pause's own fade.
        assert_eq!(sleep_gain(None, true, false, now), 0.0);
        // Paused, or no timer at all: full volume for the next time it plays.
        assert_eq!(sleep_gain(None, true, true, now), 1.0);
        assert_eq!(sleep_gain(None, false, false, now), 1.0);
        // A running timer decides, paused or not.
        let t = SleepTimer::new(now + SLEEP_FADE, now);
        assert!(sleep_gain(Some(t), false, true, now + SLEEP_FADE / 2) < 0.2);
    }

    #[test]
    fn only_silence_lasting_to_the_end_of_the_second_half_skips() {
        let s = Duration::from_secs;
        let len = Some(s(200));
        let mut from = None;
        // A quiet first half never counts.
        assert!(!silent_tail(&mut from, 0.0, s(10), len));
        assert!(!silent_tail(&mut from, 0.0, s(50), len));
        // A short pause in the second half, then music again, starts over.
        assert!(!silent_tail(&mut from, 0.0, s(120), len));
        assert!(!silent_tail(&mut from, 0.2, s(122), len));
        assert!(!silent_tail(&mut from, 0.0, s(124), len));
        assert!(!silent_tail(&mut from, 0.0, s(126), len));
        assert!(silent_tail(&mut from, 0.0, s(127), len));
        // No length, no way to tell the second half.
        let mut from = None;
        assert!(!silent_tail(&mut from, 0.0, s(190), None));
        assert!(!silent_tail(&mut from, 0.0, s(195), None));
    }

    #[test]
    fn only_a_length_the_stream_gave_counts_as_its_own() {
        let status = AudioStatus::default();
        // A music video's 5:00 the UI put in, for a song whose stream gives no length.
        status.duration_ms.store(300_000, Ordering::Relaxed);
        status.started_gen.store(2, Ordering::Release);
        status.playing.store(true, Ordering::Relaxed);
        assert_eq!(status.stream_duration(2), None);
        status.stream_ms.store(181_000, Ordering::Relaxed);
        assert_eq!(status.stream_duration(2), Some(Duration::from_secs(181)));
        // Not for the next song, until the engine starts it, with its own (here none).
        assert_eq!(status.stream_duration(3), None);
        status.stream_ms.store(0, Ordering::Relaxed);
        status.started_gen.store(3, Ordering::Release);
        assert_eq!(status.stream_duration(3), None);
    }

    #[test]
    fn crossfade_ramps_mirror_each_other_and_end_the_fading_song() {
        use std::num::NonZero;
        // 1 kHz mono of full-scale samples: 1 ms = 1 sample.
        let ones = || rodio::buffer::SamplesBuffer::new(NonZero::new(1).unwrap(), NonZero::new(1000).unwrap(), vec![1.0; 1000]);
        let status = Arc::new(AudioStatus::default());
        let out = Arc::new(AtomicU32::new(0));
        let mut incoming = Meter::new(ones(), status.clone(), 100, Arc::new(AtomicU32::new(0)));
        let mut outgoing = Meter::new(ones(), status, 0, out.clone());
        assert_eq!(outgoing.next(), Some(1.0));
        out.store(100, Ordering::Relaxed);
        let (a, b): (Vec<f32>, Vec<f32>) = (0..100).map(|_| (incoming.next().unwrap(), outgoing.next().unwrap())).unzip();
        assert_eq!(a[0], 0.0);
        assert_eq!(b[0], 1.0);
        // One comes up exactly as the other goes down.
        for (x, y) in a.iter().zip(&b) {
            assert!((x + y - 1.0).abs() < 1e-4);
        }
        assert!((b[50] - 0.5).abs() < 1e-4);
        assert_eq!(incoming.next(), Some(1.0));
        assert_eq!(outgoing.next(), None);
    }

    #[test]
    fn pausing_fades_out_and_playing_again_fades_back_from_there() {
        use std::num::NonZero;
        // 1 kHz mono of full-scale samples: 1 ms = 1 sample.
        let ones = || rodio::buffer::SamplesBuffer::new(NonZero::new(1).unwrap(), NonZero::new(1000).unwrap(), vec![1.0; 1000]);
        let fade = PAUSE_FADE_MS as usize;
        let status = Arc::new(AudioStatus::default());
        let mut m = Meter::new(ones(), status.clone(), 0, Arc::new(AtomicU32::new(0)));
        assert_eq!(m.next(), Some(1.0));
        status.hushed.store(true, Ordering::Relaxed);
        let down: Vec<f32> = m.by_ref().take(fade / 2).collect();
        assert!(down.windows(2).all(|w| w[1] < w[0]));
        assert!((down[fade / 2 - 1] - 0.5).abs() < 0.01);
        // A seek's new decoder put in now carries on from there.
        assert!((Meter::new(ones(), status.clone(), 0, Arc::new(AtomicU32::new(0))).pause - 0.5).abs() < 0.01);
        // Played again half way down: back up from there, not from silence or full.
        status.hushed.store(false, Ordering::Relaxed);
        let up: Vec<f32> = m.by_ref().take(fade / 2).collect();
        assert!((up[0] - 0.5).abs() < 0.01);
        assert!((up[fade / 2 - 1] - 1.0).abs() < 1e-4);
        status.hushed.store(true, Ordering::Relaxed);
        // Silent once the fade is through, and it stays there.
        assert!(m.by_ref().skip(fade).take(10).all(|s| s == 0.0));
        // A new decoder put in while paused (a seek) waits silent, then fades in.
        let mut seeked = Meter::new(ones(), status.clone(), 0, Arc::new(AtomicU32::new(0)));
        assert_eq!(seeked.next(), Some(0.0));
        status.hushed.store(false, Ordering::Relaxed);
        assert!(seeked.next().unwrap() < 0.01);
        assert_eq!(seeked.nth(fade), Some(1.0));
        // Paused outright before a fade got anywhere (headphones unplugged): back from silence.
        let mut cut = Meter::new(ones(), status.clone(), 0, Arc::new(AtomicU32::new(0)));
        assert_eq!(cut.next(), Some(1.0));
        status.silence();
        assert!(cut.next().unwrap() < 0.01);
    }

    #[test]
    fn with_the_fade_off_pausing_and_playing_again_take_a_few_ms() {
        use std::num::NonZero;
        // 1 kHz mono of full-scale samples: 1 ms = 1 sample.
        let ones = || rodio::buffer::SamplesBuffer::new(NonZero::new(1).unwrap(), NonZero::new(1000).unwrap(), vec![1.0; 1000]);
        let quick = INSTANT_FADE_MS as usize;
        let status = Arc::new(AudioStatus::default());
        status.instant_pause.store(true, Ordering::Relaxed);
        let mut m = Meter::new(ones(), status.clone(), 0, Arc::new(AtomicU32::new(0)));
        assert_eq!(m.next(), Some(1.0));
        // Down to silence in a few samples, not cut off at full (that would click).
        status.hushed.store(true, Ordering::Relaxed);
        let down: Vec<f32> = m.by_ref().take(quick).collect();
        assert!(down[0] < 1.0 && down[0] > 0.5);
        assert!(down.windows(2).all(|w| w[1] < w[0]));
        assert!(down[quick - 1] < 1e-4);
        // The players pause once that's through.
        status.silence();
        assert_eq!(m.next(), Some(0.0));
        // Played again: back up as quickly, as is a seek's decoder put in while paused.
        let mut seeked = Meter::new(ones(), status.clone(), 0, Arc::new(AtomicU32::new(0)));
        status.hushed.store(false, Ordering::Relaxed);
        let up: Vec<f32> = m.by_ref().take(quick).collect();
        assert!(up[0] > 0.0 && up[0] < 0.5);
        assert!(up.windows(2).all(|w| w[1] > w[0]));
        assert!((up[quick - 1] - 1.0).abs() < 1e-4);
        assert!(seeked.next().unwrap() < 0.5);
        assert!((seeked.nth(quick - 2).unwrap() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn turning_the_fade_off_mid_pause_finishes_it_the_quick_way() {
        use std::num::NonZero;
        let ones = || rodio::buffer::SamplesBuffer::new(NonZero::new(1).unwrap(), NonZero::new(1000).unwrap(), vec![1.0; 1000]);
        let status = Arc::new(AudioStatus::default());
        let mut m = Meter::new(ones(), status.clone(), 0, Arc::new(AtomicU32::new(0)));
        status.hushed.store(true, Ordering::Relaxed);
        let half = m.by_ref().take(PAUSE_FADE_MS as usize / 2).last().unwrap();
        assert!((half - 0.5).abs() < 0.01);
        // Turned off half way down: the rest goes in a few samples, from where it got to.
        status.instant_pause.store(true, Ordering::Relaxed);
        let rest: Vec<f32> = m.by_ref().take(INSTANT_FADE_MS as usize).collect();
        assert!(rest[0] < half && rest[0] > 0.0);
        assert!(rest.last().unwrap() < &1e-4);

        // And the audio thread pauses the players once that's through, not the long fade.
        let now = Instant::now();
        let long = Some(now + PAUSE_AFTER);
        assert_eq!(pause_due(long, false, now), long);
        assert_eq!(pause_due(long, true, now), Some(now + INSTANT_PAUSE_AFTER));
        // One due sooner stays as it is; none stays none.
        let soon = Some(now + Duration::from_millis(1));
        assert_eq!(pause_due(soon, true, now), soon);
        assert_eq!(pause_due(None, true, now), None);
    }

    #[test]
    fn a_pause_in_a_crossfade_fades_both_songs_and_keeps_the_crossfade() {
        use std::num::NonZero;
        let ones = || rodio::buffer::SamplesBuffer::new(NonZero::new(1).unwrap(), NonZero::new(1000).unwrap(), vec![1.0; 2000]);
        let fade = PAUSE_FADE_MS as usize;
        let status = Arc::new(AudioStatus::default());
        let out = Arc::new(AtomicU32::new(0));
        let mut incoming = Meter::new(ones(), status.clone(), 1000, Arc::new(AtomicU32::new(0)));
        let mut outgoing = Meter::new(ones(), status.clone(), 0, out.clone());
        out.store(1000, Ordering::Relaxed);
        let mut both = |n: usize| (0..n).map(|_| (incoming.next().unwrap(), outgoing.next().unwrap())).last();
        both(100);
        status.hushed.store(true, Ordering::Relaxed);
        assert_eq!(both(fade + 1), Some((0.0, 0.0)));
        status.hushed.store(false, Ordering::Relaxed);
        both(fade);
        // Back at full: still mirroring, half way into the crossfade.
        let (a, b) = both(1).unwrap();
        assert!((a + b - 1.0).abs() < 1e-4);
        assert!((a - 0.501).abs() < 1e-3);
    }

    #[test]
    fn a_held_fade_out_keeps_full_volume_until_its_last_moment() {
        use std::num::NonZero;
        let ones = rodio::buffer::SamplesBuffer::new(NonZero::new(1).unwrap(), NonZero::new(1000).unwrap(), vec![1.0; 1000]);
        let out = Arc::new(AtomicU32::new(100 | HOLD));
        let gains: Vec<f32> = Meter::new(ones, Arc::new(AudioStatus::default()), 0, out).collect();
        assert_eq!(gains.len(), 100);
        assert!(gains[..60].iter().all(|g| *g == 1.0));
        assert!((gains[80] - 0.5).abs() < 1e-4);
    }

    #[test]
    fn a_song_in_its_outro_isnt_faded_again() {
        use std::num::NonZero;
        let (mixer, _source) = rodio::mixer::mixer(NonZero::new(2).unwrap(), NonZero::new(48_000).unwrap());
        let fading = |left: u64, outro| Fading {
            player: Player::connect_new(&mixer),
            shared: None,
            norm: Arc::new(AtomicU32::new(1f32.to_bits())),
            out: Arc::new(AtomicU32::new(0)),
            waiting: Some((Instant::now(), Some(Duration::from_secs(left)))),
            outro,
        };
        // (Less a moment: the time since the handoff.)
        let about = |ms: u32| ms - 100..=ms;
        let mut f = fading(3, true);
        assert!(about(3000).contains(&f.start(5000)));
        let out = f.out.load(Ordering::Relaxed);
        assert!(out & HOLD != 0 && about(3000).contains(&(out & !HOLD)));
        // A skip in the middle of a long outro fades it as usual, over the crossfade.
        let mut f = fading(10, true);
        assert_eq!(f.start(5000), 5000);
        assert_eq!(f.out.load(Ordering::Relaxed), 5000);
        let mut f = fading(3, false);
        f.start(5000);
        assert!(about(3000).contains(&f.out.load(Ordering::Relaxed)));
    }

    /// `secs` of a level (dB) per `TAIL_WINDOW`.
    fn flat(db: f32, secs: f32) -> Vec<f32> {
        vec![db; (secs / TAIL_WINDOW.as_secs_f32()).round() as usize]
    }

    fn close(a: Duration, secs: f32) -> bool {
        (a.as_secs_f32() - secs).abs() <= 2.0 * TAIL_WINDOW.as_secs_f32()
    }

    #[test]
    fn the_silence_after_a_songs_music_is_found() {
        let levels = [flat(-12.0, 20.0), flat(f32::NEG_INFINITY, 4.0), flat(-75.0, 1.0)].concat();
        assert_eq!(Tail::measure(&levels, TAIL_WINDOW), Tail { silence: Duration::from_secs(5), fade: Duration::ZERO });
        // Music to its last moment.
        assert_eq!(Tail::measure(&flat(-12.0, 30.0), TAIL_WINDOW), Tail::default());
        // A quiet song's quiet end is still music.
        let levels = [flat(-30.0, 20.0), flat(-55.0, 5.0), flat(-80.0, 5.0)].concat();
        let t = Tail::measure(&levels, TAIL_WINDOW);
        assert!(close(t.silence, 5.0) && close(t.fade, 5.0), "{t:?}");
        // All silence: all of it goes; nothing to measure, nothing does.
        assert_eq!(Tail::measure(&flat(f32::NEG_INFINITY, 30.0), TAIL_WINDOW).silence, TAIL);
        assert_eq!(Tail::measure(&[], TAIL_WINDOW), Tail::default());
    }

    #[test]
    fn a_songs_own_fade_out_is_found() {
        // Loud, a drum hit, then 10 s fading from -10 to -70 dB, then 2 s of silence.
        let fade: Vec<f32> = (0..200).map(|i| -10.0 - 60.0 * i as f32 / 200.0).collect();
        let mut levels = [flat(-10.0, 15.0), fade, flat(f32::NEG_INFINITY, 2.0)].concat();
        levels[50] = 0.0;
        let t = Tail::measure(&levels, TAIL_WINDOW);
        // The music ends 50 dB down, 5/6 of the way; the fade starts 6 dB down.
        assert!(close(t.silence, 10.0 / 6.0 + 2.0), "{t:?}");
        assert!(close(t.fade, 10.0 * 44.0 / 60.0), "{t:?}");
    }

    #[test]
    fn a_fade_out_is_an_outro_and_a_hard_ending_isnt() {
        let db = |a: f32| 20.0 * a.log10();
        // 10 s fading linearly in amplitude, as most editors' fades do.
        let linear: Vec<f32> = (0..200).map(|i| db(1.0 - i as f32 / 200.0)).collect();
        let t = Tail::measure(&[flat(0.0, 15.0), linear, flat(f32::NEG_INFINITY, 2.0)].concat(), TAIL_WINDOW);
        assert!(t.outro(Duration::from_secs(5)) && t.outro(Duration::from_secs(8)), "{t:?}");
        // Loud to the end, then a second of reverb dying away.
        let reverb: Vec<f32> = (0..20).map(|i| -2.0 * i as f32).collect();
        let t = Tail::measure(&[flat(0.0, 25.0), reverb, flat(f32::NEG_INFINITY, 4.0)].concat(), TAIL_WINDOW);
        assert!(!t.outro(Duration::from_secs(2)), "{t:?}");
    }

    #[test]
    fn levels_come_from_the_samples() {
        // 1 kHz stereo: a second of music, then half a second of silence.
        let music = (0..1000).flat_map(|i| {
            let s = 0.5 * (i as f32 * 0.3).sin();
            [s, -s]
        });
        let samples: Vec<f32> = music.chain(std::iter::repeat_n(0.0, 1000)).collect();
        let levels = levels(&mut samples.into_iter(), 2, 1000);
        assert_eq!(levels.len(), 30);
        assert!((levels[0] + 9.0).abs() < 0.5, "{}", levels[0]);
        assert_eq!(Tail::measure(&levels, TAIL_WINDOW).silence, Duration::from_millis(500));
    }

    #[test]
    fn leading_silence_is_skipped_in_whole_frames() {
        // 1 kHz stereo: 1.5 s of silence (or near it), then left 0.5 and right 0.25.
        let song = |quiet: usize| std::iter::repeat_n(1e-4, quiet * 2).chain([0.5, 0.25].into_iter().cycle().take(2000));
        let mut s = song(1500);
        assert_eq!(skip_lead(&mut s, 2, 1000, MAX_LEAD), Duration::from_millis(1510));
        assert_eq!(s.next(), Some(0.5));
        // No longer than the most.
        let mut s = song(10_000);
        assert_eq!(skip_lead(&mut s, 2, 1000, MAX_LEAD), MAX_LEAD);
        // Music from the start: only what's under the very start of the fade-in.
        let mut s = song(0);
        assert_eq!(skip_lead(&mut s, 2, 1000, MAX_LEAD), Duration::from_millis(10));
        assert_eq!(s.next(), Some(0.5));
    }

    #[test]
    fn emptying_the_player_doesnt_wait_on_a_device_let_go() {
        use std::num::NonZero;
        let (mixer, source) = rodio::mixer::mixer(NonZero::new(2).unwrap(), NonZero::new(48_000).unwrap());
        // Let go of while paused: nothing pulls the mixer.
        let output = Output { mixer, slot: Arc::new(std::sync::Mutex::new(Some(source))), sink: None, remote: None, connecting: None, handshake: None, remote_retry: true, notice: None, speaker_level: None, device: None, destination: Destination::default(), lost: Default::default(), checked: Instant::now() };
        let mut player = Player::connect_new(&output.mixer);
        player.set_volume(0.5);
        player.append(rodio::buffer::SamplesBuffer::new(NonZero::new(2).unwrap(), NonZero::new(48_000).unwrap(), vec![0.0; 96_000]));
        player.pause();
        let (done, finished) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            clear(&mut player, &output);
            let _ = done.send((player.empty(), player.is_paused(), player.volume()));
        });
        assert_eq!(finished.recv_timeout(Duration::from_secs(2)).ok(), Some((true, true, 0.5)));
    }

    #[test]
    fn emptying_the_player_doesnt_wait_on_a_stalled_device() {
        use std::num::NonZero;
        let (mixer, source) = rodio::mixer::mixer(NonZero::new(2).unwrap(), NonZero::new(48_000).unwrap());
        // Playing, through a device that stopped pulling before it reported itself gone.
        let output = Output { mixer, slot: Arc::new(std::sync::Mutex::new(None)), sink: None, remote: None, connecting: None, handshake: None, remote_retry: true, notice: None, speaker_level: None, device: None, destination: Destination::default(), lost: Default::default(), checked: Instant::now() };
        let mut player = Player::connect_new(&output.mixer);
        player.set_volume(0.25);
        player.append(rodio::buffer::SamplesBuffer::new(NonZero::new(2).unwrap(), NonZero::new(48_000).unwrap(), vec![0.5; 96_000]));
        player.play();
        let (done, finished) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            clear(&mut player, &output);
            let _ = done.send((player.empty(), player.is_paused(), player.volume()));
        });
        assert_eq!(finished.recv_timeout(Duration::from_secs(2)).ok(), Some((true, true, 0.25)));
        // Once the device pulls again, the old song is gone within rodio's 5 ms check.
        let mut source = source;
        let tail: Vec<f32> = source.by_ref().take(9_600).skip(4_800).collect();
        assert!(tail.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn neutral_stereo_leaves_the_chain_as_it_was() {
        use std::num::NonZero;
        let status = AudioStatus::default();
        status.eq.set(true, &eq::PRESETS[1].1, true);
        status.limit.store(true, Ordering::Relaxed);
        let samples: Vec<f32> = (0..9_600).map(|k| (k as f32 * 0.03).sin() * if k % 2 == 0 { 1.2 } else { 0.4 }).collect();
        let (mixer, source) = rodio::mixer::mixer(NonZero::new(2).unwrap(), NonZero::new(48_000).unwrap());
        mixer.add(rodio::buffer::SamplesBuffer::new(NonZero::new(2).unwrap(), NonZero::new(48_000).unwrap(), samples.clone()));
        let slot = Arc::new(std::sync::Mutex::new(None));
        let (eq, stereo, limiter) = (eq::Eq::new(status.eq.clone(), 48_000, 2), Some(stereo::Stereo::new(status.stereo.clone(), 48_000)), limiter::Limiter::new(status.limit.clone(), 48_000, 2));
        let bridge = Bridge { inner: Some(source), slot: slot.clone(), frame: vec![0.0; 2], at: 0, eq, stereo, limiter };
        let (mut eq, mut limiter) = (eq::Eq::new(status.eq.clone(), 48_000, 2), limiter::Limiter::new(status.limit.clone(), 48_000, 2));
        let want = samples.iter().enumerate().map(|(n, s)| limiter.process(eq.process(*s, n % 2), n % 2));
        assert!(bridge.take(samples.len()).map(f32::to_bits).eq(want.map(f32::to_bits)));
        // Given back once dropped.
        assert!(slot.lock().unwrap().is_some());
    }

    #[test]
    fn normalization_brings_loud_and_quiet_songs_to_the_level() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let audio = AudioHandle { tx, status: Arc::new(AudioStatus::default()) };
        let status = &audio.status;
        let loud = Shared::new(1, 0, Some(7.29), false, false);
        let quiet = Shared::new(1, 0, Some(-4.0), false, false);
        let very_quiet = Shared::new(1, 0, Some(-15.0), false, false);
        let unknown = Shared::new(1, 0, None, false, false);
        audio.set_normalize(Normalize::Normal);
        assert!(status.limit.load(Ordering::Relaxed));
        assert!((norm_gain(status, Some(&loud)) - 0.432).abs() < 0.001);
        assert!((norm_gain(status, Some(&quiet)) - 1.585).abs() < 0.001);
        // Up by no more than 6 dB.
        assert_eq!(norm_db(status, Some(&very_quiet)), Some(MAX_BOOST_DB));
        assert_eq!(norm_gain(status, Some(&unknown)), 1.0);
        assert_eq!(norm_gain(status, None), 1.0);
        // Quiet: 5 dB lower, so a song 4 dB under the target comes down a little.
        audio.set_normalize(Normalize::Quiet);
        assert_eq!(norm_db(status, Some(&quiet)), Some(-1.0));
        assert!((norm_db(status, Some(&loud)).unwrap() + 12.29).abs() < 1e-4);
        audio.set_normalize(Normalize::Off);
        assert!(!status.limit.load(Ordering::Relaxed));
        assert_eq!(norm_gain(status, Some(&loud)), 1.0);
        assert_eq!(norm_gain(status, Some(&quiet)), 1.0);
        show_norm(status, Some(&loud));
        assert_eq!(audio.normalization(), None);
    }

    #[test]
    fn the_old_switch_keeps_its_meaning() {
        assert_eq!(Normalize::from_key("1"), Normalize::Normal);
        assert_eq!(Normalize::from_key("0"), Normalize::Off);
        assert_eq!(Normalize::from_key("quiet"), Normalize::Quiet);
        assert_eq!(Normalize::from_key(""), Normalize::Normal);
        for l in Normalize::ALL {
            assert_eq!(Normalize::from_u8(l as u8), l);
        }
    }

    #[test]
    fn the_apps_volume_is_kept_while_a_speakers_is_on_the_slider() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let audio = AudioHandle { tx, status: Arc::new(AudioStatus::default()) };
        audio.set_volume(0.125);
        assert_eq!(audio.own_volume(), 0.125);
        // A speaker at 0.9 takes the slider.
        audio.status.keep(Some(audio.volume()));
        audio.set_volume(0.9);
        assert_eq!(audio.own_volume(), 0.125);
        // The curve changes the app's level, not the speaker's.
        audio.set_log_volume(true);
        assert!((audio.own_volume() - 0.5).abs() < 1e-6);
        assert_eq!(audio.volume(), 0.9);
        // Muted is a level too.
        audio.status.keep(Some(0.0));
        assert_eq!(audio.own_volume(), 0.0);
    }

    #[test]
    fn log_volume_keeps_loudness_when_switched() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let audio = AudioHandle { tx, status: Arc::new(AudioStatus::default()) };
        audio.set_volume(0.125);
        audio.set_log_volume(true);
        assert!((audio.volume() - 0.5).abs() < 1e-6);
        assert!((slider_gain(&audio.status, audio.volume()) - 0.125).abs() < 1e-6);
        audio.set_log_volume(true);
        assert!((audio.volume() - 0.5).abs() < 1e-6);
        audio.set_log_volume(false);
        assert!((audio.volume() - 0.125).abs() < 1e-6);
    }

    #[test]
    fn gapless_chains_without_a_silent_sample_and_glides_normalization_smoothly() {
        use std::num::NonZero;
        let (mixer, mut source) = rodio::mixer::mixer(NonZero::new(1).unwrap(), NonZero::new(48_000).unwrap());
        let player = Player::connect_new(&mixer);
        player.set_volume(1.0);
        let status = Arc::new(AudioStatus::default());
        let (tx, rx) = unbounded();
        let started = Arc::new(AtomicBool::new(false));
        let first = rodio::buffer::SamplesBuffer::new(NonZero::new(1).unwrap(), NonZero::new(48_000).unwrap(), vec![1.0; 128]);
        let second = rodio::buffer::SamplesBuffer::new(NonZero::new(1).unwrap(), NonZero::new(48_000).unwrap(), vec![1.0; 2400]);
        player.append(Meter::new(first, status.clone(), 0, Arc::new(AtomicU32::new(0))).with_norm(Arc::new(AtomicU32::new(0.5f32.to_bits()))));
        player.append(
            Meter::new(second, status.clone(), 0, Arc::new(AtomicU32::new(0)))
                .with_norm(Arc::new(AtomicU32::new(0.25f32.to_bits())))
                .with_gate(GaplessGate {
                    generation: 2,
                    cancelled: Arc::new(AtomicBool::new(false)),
                    prev_failed: Arc::new(AtomicBool::new(false)),
                    started: started.clone(),
                    tx,
                }),
        );
        player.play();

        let out: Vec<f32> = source.by_ref().take(128 + 2400).collect();
        // First 128 samples at 0.5; zero silent samples at the boundary; normalization glides
        // smoothly from 0.5 down to 0.25 without a step click.
        assert!(out[..128].iter().all(|&s| (s - 0.5).abs() < 1e-5));
        assert!((out[128] - out[127]).abs() < 0.005, "step click at boundary: {} -> {}", out[127], out[128]);
        assert!(out[128..].iter().all(|s| (0.249..=0.501).contains(s)));
        assert!((out.last().copied().unwrap() - 0.25).abs() < 1e-4);
        assert!(started.load(Ordering::Acquire));
        assert!(matches!(rx.try_recv(), Ok(AudioCmd::GaplessStarted(2))));
    }

    #[test]
    fn cancelled_or_failed_gapless_track_plays_nothing() {
        use std::num::NonZero;
        for (cancel, fail) in [(true, false), (false, true)] {
            let (mixer, mut source) = rodio::mixer::mixer(NonZero::new(1).unwrap(), NonZero::new(48_000).unwrap());
            let player = Player::connect_new(&mixer);
            player.set_volume(1.0);
            let status = Arc::new(AudioStatus::default());
            let (tx, rx) = unbounded();
            let started = Arc::new(AtomicBool::new(false));
            let first = rodio::buffer::SamplesBuffer::new(NonZero::new(1).unwrap(), NonZero::new(48_000).unwrap(), vec![0.8; 64]);
            let second = rodio::buffer::SamplesBuffer::new(NonZero::new(1).unwrap(), NonZero::new(48_000).unwrap(), vec![0.4; 64]);
            player.append(Meter::new(first, status.clone(), 0, Arc::new(AtomicU32::new(0))));
            player.append(Meter::new(second, status.clone(), 0, Arc::new(AtomicU32::new(0))).with_gate(GaplessGate {
                generation: 2,
                cancelled: Arc::new(AtomicBool::new(cancel)),
                prev_failed: Arc::new(AtomicBool::new(fail)),
                started: started.clone(),
                tx,
            }));
            player.play();
            let out: Vec<f32> = source.by_ref().take(128).collect();
            assert!(out[..64].iter().all(|&s| (s - 0.8).abs() < 1e-5));
            assert!(out[64..].iter().all(|&s| s == 0.0));
            assert!(!started.load(Ordering::Acquire));
            assert!(rx.try_recv().is_err());
            assert!(player.empty());
        }
    }

    /// Two seconds of a 173.3 Hz stereo sine at 48 kHz: from one spot to another, its
    /// samples don't line up, so cutting straight across would jump.
    fn sine() -> rodio::buffer::SamplesBuffer {
        use std::num::NonZero;
        let wave = (0..96_000).flat_map(|i| [(i as f32 * 173.3 * std::f32::consts::TAU / 48_000.0).sin() * 0.5; 2]).collect::<Vec<f32>>();
        rodio::buffer::SamplesBuffer::new(NonZero::new(2).unwrap(), NonZero::new(48_000).unwrap(), wave)
    }

    /// The largest step from a sample to the next one on its channel. The sine's own is about 0.011.
    fn largest_step(out: &[f32]) -> f32 {
        out.windows(3).map(|w| (w[2] - w[0]).abs()).fold(0.0, f32::max)
    }

    #[test]
    fn a_seek_in_place_crossfades_instead_of_jumping() {
        let mut m = Meter::new(sine(), Arc::new(AudioStatus::default()), 0, Arc::new(AtomicU32::new(0)));
        let mut out: Vec<f32> = m.by_ref().take(48_000).collect();
        rodio::Source::try_seek(&mut m, Duration::from_millis(213)).unwrap();
        out.extend(m.by_ref().take(4_800));
        assert!(largest_step(&out) < 0.03, "{}", largest_step(&out));
        // Nothing's lost or added: the new spot's samples, one for one.
        assert_eq!(out.len(), 52_800);
    }

    #[test]
    fn a_seek_between_left_and_right_keeps_them_apart() {
        // Left at 0.5, right at -0.5. At 44.1 kHz rodio seeks every 441 samples, so between
        // a left and its right every other time.
        use std::num::NonZero;
        let wave = (0..88_200).flat_map(|_| [0.5, -0.5]).collect::<Vec<f32>>();
        let buf = rodio::buffer::SamplesBuffer::new(NonZero::new(2).unwrap(), NonZero::new(44_100).unwrap(), wave);
        // Through `Tempo`, as songs play: its seek starts the new spot on the left.
        let t = tempo::Tempo::new(buf, Arc::new(tempo::Speed::default()), Duration::ZERO, Arc::new(AtomicU64::new(0)));
        let mut m = Meter::new(t, Arc::new(AudioStatus::default()), 0, Arc::new(AtomicU32::new(0)));
        let mut out: Vec<f32> = m.by_ref().take(441).collect();
        rodio::Source::try_seek(&mut m, Duration::from_millis(500)).unwrap();
        out.extend(m.by_ref().take(4_410));
        for (i, s) in out.iter().enumerate() {
            assert!((s - if i % 2 == 0 { 0.5 } else { -0.5 }).abs() < 1e-5, "sample {i}: {s}");
        }
    }

    #[test]
    fn a_failed_seek_in_place_plays_what_it_read_ahead() {
        // Ones, then twos: what's read before seeking past the end must still come out, in order.
        use std::num::NonZero;
        let samples: Vec<f32> = (0..2000).map(|i| if i < 1000 { 1.0 } else { 2.0 }).collect();
        let buf = rodio::buffer::SamplesBuffer::new(NonZero::new(1).unwrap(), NonZero::new(1000).unwrap(), samples.clone());
        let mut m = Meter::new(rodio::Source::buffered(buf), Arc::new(AudioStatus::default()), 0, Arc::new(AtomicU32::new(0)));
        let mut out: Vec<f32> = m.by_ref().take(995).collect();
        // A buffered source can't seek.
        assert!(rodio::Source::try_seek(&mut m, Duration::from_millis(10)).is_err());
        out.extend(m.by_ref());
        assert_eq!(out, samples);
    }

    #[test]
    fn a_seek_to_a_new_decoder_crossfades_instead_of_jumping() {
        use std::num::NonZero;
        let (mixer, mut source) = rodio::mixer::mixer(NonZero::new(2).unwrap(), NonZero::new(48_000).unwrap());
        let status = Arc::new(AudioStatus::default());
        let norm = Arc::new(AtomicU32::new(1f32.to_bits()));
        let mut fade_out = Arc::new(AtomicU32::new(0));
        let mut fading = Vec::new();
        let mut player = Player::connect_new(&mixer);
        player.append(Meter::new(sine(), status.clone(), 0, fade_out.clone()).with_norm(norm.clone()));
        player.play();
        let mut out: Vec<f32> = source.by_ref().take(48_000).collect();
        let mut later = sine();
        rodio::Source::try_seek(&mut later, Duration::from_millis(213)).unwrap();
        put_in(&mut player, &mixer, &mut fading, &mut fade_out, &norm, &status, later, true);
        out.extend(source.by_ref().take(9_600));
        assert!(largest_step(&out) < 0.03, "{}", largest_step(&out));
        // The old spot has faded out and is done.
        assert!(fading.iter().all(|f| f.player.empty()));
    }
}
