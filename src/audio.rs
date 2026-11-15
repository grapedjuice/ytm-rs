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

use crate::stream::{self, ReadInterrupt, Shared};

pub use crate::stream::TrackDecoder;

pub mod eq;
pub mod limiter;
pub mod tempo;

pub enum AudioCmd {
    /// A decoder is ready for playback generation `generation`; stale generations are dropped.
    /// A decoder started mid-song (`at` > 0) is the seekable kind.
    Start { generation: u64, decoder: TrackDecoder, shared: Arc<Shared>, at: Duration },
    /// A decoder positioned at `at`, replacing the current one; only the latest seek's lands.
    Replace { generation: u64, seek: u64, decoder: TrackDecoder, at: Duration, seekable: bool },
    SeekFailed { generation: u64, seek: u64, at: Duration, reason: String },
    Pause,
    Resume,
    Stop,
    /// Stop every source like Stop, retaining the recovery position while reopening.
    StopAt(Duration),
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
    /// Play through this device (cpal's id) whenever it's connected; None follows the
    /// system default.
    SetDevice(Option<String>),
}

pub enum AudioEvent {
    Finished { generation: u64 },
    /// Close enough to the end that the next song should start, to crossfade into it.
    Ending { generation: u64 },
    /// The song's download failed, so playback stopped at `at` (or a seek to `at`
    /// couldn't be served). Reopening the song there resolves a fresh stream.
    Interrupted { generation: u64, at: Duration, reason: String },
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
    /// Generation the engine last started, set once its length is in.
    started_gen: AtomicU64,
    /// The length its stream gave, 0 when it gave none (`duration_ms` then keeps the UI's).
    stream_ms: AtomicU64,
    /// f32 bits
    volume: AtomicU32,
    /// The `Normalize` level songs are brought to.
    normalize: AtomicU8,
    /// The limiter's on, for songs normalization turns up.
    limit: Arc<AtomicBool>,
    /// What normalization does to the playing song, in dB (f32 bits); NaN when it does
    /// nothing (off, or YouTube gave no loudness for it).
    norm_db: AtomicU32,
    /// The volume slider follows loudness (cubic) rather than amplitude.
    log_volume: AtomicBool,
    /// Smoothed output loudness 0..1 (f32 bits), for audio-reactive visuals.
    level: AtomicU32,
    /// How far the output device runs behind the position (its buffer), in ms.
    latency_ms: AtomicU32,
    /// Crossfade length in ms; 0 = off.
    crossfade_ms: AtomicU32,
    /// The equalizer's settings, applied to everything on its way to the device.
    eq: Arc<eq::EqParams>,
    /// Playback speed, applied to each song as it plays.
    speed: Arc<tempo::Speed>,
}

impl AudioStatus {
    pub fn speed(&self) -> f32 {
        self.speed.get()
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

    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    fn set_position(&self, ms: u64) {
        self.position_ms.store(ms, Ordering::Relaxed);
        self.sampled_at.store(now_ms(), Ordering::Relaxed);
    }
}

/// A crossfade ramp in progress, in samples. Linear, like Strawberry's and
/// Liquidsoap's crossfades: the outgoing song gets steadily quieter as the next one
/// comes up (an equal-power curve kept it near full volume for half the fade).
#[derive(Clone, Copy)]
enum Ramp {
    In { at: u32, len: u32 },
    /// Down from `from` (a fade-in may have been cut short) to silence, then the source ends.
    Out { from: f32, at: u32, len: u32 },
}

impl Ramp {
    fn gain(self) -> f32 {
        match self {
            Ramp::In { at, len } => at as f32 / len as f32,
            Ramp::Out { from, at, len } => from * (1.0 - at as f32 / len as f32),
        }
    }
}

/// Pass-through source that tracks loudness (RMS over ~23 ms windows, smoothed) and
/// applies crossfade ramps. `fade_out` is set by the audio thread to the fade-out
/// length in ms (0 = keep playing); a new value restarts it from the current gain.
struct Meter<S> {
    inner: S,
    status: Arc<AudioStatus>,
    fade_out: Arc<AtomicU32>,
    seen_out: u32,
    ramp: Option<Ramp>,
    acc: f32,
    n: u32,
    smooth: f32,
}

impl<S: rodio::Source> Meter<S> {
    fn new(inner: S, status: Arc<AudioStatus>, fade_in_ms: u32, fade_out: Arc<AtomicU32>) -> Self {
        let mut m = Self { inner, status, fade_out, seen_out: 0, ramp: None, acc: 0.0, n: 0, smooth: 0.0 };
        if fade_in_ms > 0 {
            m.ramp = Some(Ramp::In { at: 0, len: m.samples(fade_in_ms) });
        }
        m
    }

    fn samples(&self, ms: u32) -> u32 {
        let per_sec = self.inner.sample_rate().get() as u64 * self.inner.channels().get() as u64;
        (per_sec * ms as u64 / 1000).max(1) as u32
    }
}

impl<S: rodio::Source> Iterator for Meter<S> {
    type Item = rodio::Sample;
    fn next(&mut self) -> Option<Self::Item> {
        let out = self.fade_out.load(Ordering::Relaxed);
        if out != self.seen_out {
            self.seen_out = out;
            let from = self.ramp.map_or(1.0, Ramp::gain);
            self.ramp = Some(Ramp::Out { from, at: 0, len: self.samples(out) });
        }
        if let Some(Ramp::Out { at, len, .. }) = self.ramp
            && at >= len
        {
            return None;
        }
        let mut s = self.inner.next()?;
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
        Some(s)
    }
}

impl<S: rodio::Source> rodio::Source for Meter<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
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
    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        self.inner.try_seek(pos)
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

    pub fn set_normalize(&self, level: Normalize) {
        self.status.normalize.store(level as u8, Ordering::Relaxed);
        self.status.limit.store(level != Normalize::Off, Ordering::Relaxed);
        self.send(AudioCmd::Volume(self.volume()));
    }

    /// How far normalization turns the playing song up (positive) or down, in dB; None
    /// when it leaves it as it is (off, or there's no loudness figure for it).
    pub fn normalization(&self) -> Option<f32> {
        Some(f32::from_bits(self.status.norm_db.load(Ordering::Relaxed))).filter(|d| !d.is_nan())
    }

    /// Switches the slider's curve, moving it so the music stays as loud as it was.
    pub fn set_log_volume(&self, on: bool) {
        if self.status.log_volume.swap(on, Ordering::Relaxed) != on {
            let v = self.volume();
            self.status.volume.store(if on { v.cbrt() } else { v.powi(3) }.to_bits(), Ordering::Relaxed);
        }
        self.send(AudioCmd::Volume(self.volume()));
    }

    pub fn set_crossfade(&self, secs: u32) {
        self.status.crossfade_ms.store(secs * 1000, Ordering::Relaxed);
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

    /// Takes effect at once; the pitch goes with it.
    pub fn set_speed(&self, speed: f32) {
        self.status.speed.set(speed);
    }
}

/// A seek that needs a new decoder built over the stream buffer (off the audio thread).
struct SeekJob {
    shared: Arc<Shared>,
    interrupt: ReadInterrupt,
    pos: Duration,
    /// For a live seek (a jam): the host position and when it was, to catch up after the build.
    live: Option<(Duration, std::time::Instant)>,
    generation: u64,
    seek: u64,
}


/// Build a decoder for `j` (it may wait for the download) and hand it to the audio thread.
fn build_seek(j: SeekJob, tx: &Sender<AudioCmd>) {
    build_seek_using(j, tx, |shared, at, interrupt| {
        stream::decoder_at_with_interrupt(shared, at, interrupt).map_err(|e| e.to_string())
    });
}

fn build_seek_using(
    j: SeekJob, tx: &Sender<AudioCmd>,
    decode: impl FnOnce(Arc<Shared>, Duration, ReadInterrupt) -> Result<TrackDecoder, String>,
) {
    let SeekJob { shared, interrupt, pos, live, generation, seek } = j;
    let seekable = !pos.is_zero();
    let res = catch_decoder(|| {
        let requested = live.map_or(pos, |(p, at)| p + at.elapsed());
        let mut decoder = decode(shared.clone(), requested, interrupt)?;
        let landed = live.map_or(requested, |(p, at)| p + at.elapsed());
        if landed > requested { decoder.advance(landed - requested).map_err(|e| e.to_string())?; }
        decoder.prime_frame().map_err(|e| e.to_string())?;
        Ok((decoder, landed))
    });
    match res {
        Ok((decoder, pos)) => {
            let _ = tx.send(AudioCmd::Replace { generation, seek, decoder, at: pos, seekable });
        }
        Err(e) => {
            // Route failures through the same generation/sequence gate as a
            // successful replacement. Superseded cancellation is not a UI error.
            let reason = shared.error().unwrap_or_else(|| format!("Seek failed: {e}"));
            let _ = tx.send(AudioCmd::SeekFailed { generation, seek, at: pos, reason });
        }
    }
}

fn catch_decoder<T>(decode: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(decode))
        .unwrap_or_else(|_| Err("Audio decoder worker panicked".into()))
}

struct WorkerBusyReset(Arc<AtomicBool>);
impl Drop for WorkerBusyReset {
    fn drop(&mut self) {
        // Normal completion already hands busy over atomically to a successor.
        // An unexpected unwind must not leave future jobs permanently disabled.
        if std::thread::panicking() { self.0.store(false, Ordering::Release); }
    }
}

fn decoder_worker(
    jobs: Arc<std::sync::Mutex<Option<SeekJob>>>, busy: Arc<AtomicBool>, tx: Sender<AudioCmd>,
    build: fn(SeekJob, &Sender<AudioCmd>),
) {
    let _busy_reset = WorkerBusyReset(busy.clone());
    loop {
        let job = jobs.lock().unwrap().take();
        match job {
            Some(job) => build(job, &tx),
            None => {
                busy.store(false, Ordering::Release);
                if jobs.lock().unwrap().is_none() || busy.swap(true, Ordering::AcqRel) { break; }
            }
        }
    }
}

#[derive(Default)]
struct DecoderJobs {
    seq: u64,
    pending: Option<ReadInterrupt>,
    queued: Arc<std::sync::Mutex<Option<SeekJob>>>,
    busy: Arc<AtomicBool>,
}

impl DecoderJobs {
    fn cancel(&mut self) {
        self.seq += 1;
        if let Some(interrupt) = self.pending.take() { interrupt.cancel(); }
        if let Some(job) = self.queued.lock().unwrap().take() { job.interrupt.cancel(); }
    }

    fn submit(&mut self, shared: Arc<Shared>, pos: Duration, live: Option<(Duration, Instant)>, generation: u64, tx: &Sender<AudioCmd>) {
        // Reserve the replacement reader before cancelling an older reservation
        // or playback reader: Shared must never briefly have zero readers.
        let interrupt = shared.read_interrupt();
        self.cancel();
        self.pending = Some(interrupt.clone());
        *self.queued.lock().unwrap() = Some(SeekJob { shared, interrupt, pos, live, generation, seek: self.seq });
        if !self.busy.swap(true, Ordering::AcqRel) {
            let (jobs, busy, tx) = (self.queued.clone(), self.busy.clone(), tx.clone());
            std::thread::spawn(move || decoder_worker(jobs, busy, tx, build_seek));
        }
    }
}

/// A song fading out under the next one, kept until it's silent.
struct Fading {
    player: Player,
    shared: Option<Arc<Shared>>,
    interrupt: Option<ReadInterrupt>,
    out: Arc<AtomicU32>,
    /// Still at full volume, waiting for the next song to start (as Morphe does): when
    /// that was asked for, and how much of this song was left then.
    waiting: Option<(Instant, Option<Duration>)>,
}

impl Fading {
    /// Start the fade-out, over what's left of the song if that's shorter. Returns its length.
    fn start(&mut self, crossfade: u32) -> u32 {
        let left = self.waiting.take().and_then(|(at, left)| Some(left?.saturating_sub(at.elapsed())));
        let len = crossfade.min(left.map_or(u32::MAX, |l| l.as_millis() as u32)).max(QUICK_FADE_MS);
        self.out.store(len, Ordering::Relaxed);
        len
    }
}

impl Drop for Fading {
    fn drop(&mut self) {
        if let Some(interrupt) = self.interrupt.take() { interrupt.cancel(); }
        self.player.stop();
        // Another decoder may use this exact Shared (repeat / recovery).
        // ReadControl's last-reader policy owns whole-download cancellation.
    }
}

fn retire_waiting_faders(fading: &mut Vec<Fading>) {
    // A mixer pulls its sources serially. One old song waiting on the network
    // would otherwise also prevent the ready incoming song from producing PCM.
    fading.retain(|f| !f.interrupt.as_ref().is_some_and(ReadInterrupt::is_waiting));
}

fn abort_player(output: &Output, player: &mut Player, interrupt: &mut Option<ReadInterrupt>) {
    if let Some(interrupt) = interrupt.take() { interrupt.cancel(); }
    crate::audio_source::replace_player(player, &output.mixer, output.channels, output.rate);
}

/// Shared by the completed Stop/StopAt path, after readers and queues retire.
fn finish_stop_status(status: &AudioStatus, command: &AudioCmd) {
    let at = match command {
        AudioCmd::StopAt(at) => *at,
        AudioCmd::Stop | AudioCmd::Handoff => Duration::ZERO,
        _ => unreachable!("only stop commands reach the stopped status path"),
    };
    status.playing.store(false, Ordering::Relaxed);
    status.set_position(at.as_millis() as u64);
}

/// Release every decoder wait before CPAL drops/joins the old device thread.
/// Rebuilding is asynchronous and keeps the same Shared download alive.
struct ReopenState<'a> {
    player: &'a mut Player,
    interrupt: &'a mut Option<ReadInterrupt>,
    jobs: &'a mut DecoderJobs,
    fading: &'a mut Vec<Fading>,
    shared: Option<&'a Arc<Shared>>,
    current: u64,
    at: Duration,
}

fn reopen_output(
    output: &mut Output, state: ReopenState<'_>, status: &AudioStatus, tx: &Sender<AudioCmd>,
) {
    prepare_reopen(output, state, tx);
    output.open(tx.clone(), status);
}

fn prepare_reopen(output: &Output, state: ReopenState<'_>, tx: &Sender<AudioCmd>) {
    let ReopenState { player, interrupt, jobs, fading, shared, current, at } = state;
    if current != 0 && let Some(shared) = shared {
        jobs.submit(shared.clone(), at, None, current, tx);
    }
    abort_player(output, player, interrupt);
    fading.clear();
}

/// Fade-outs shorter than this would click: a crossfade cut short by another skip, or
/// one too close to the end of the song.
const QUICK_FADE_MS: u32 = 40;

/// How long a song keeps playing at full volume for the next one to load before it
/// fades out anyway.
const HANDOFF_WAIT: Duration = Duration::from_secs(3);

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
            Self::Off => "Off",
            Self::Normal => "Normal",
            Self::Quiet => "Quiet",
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
/// default changes (them reconnecting), the fixed-format mixer is retained and
/// the active decoder is rebuilt at the last position before the old device joins.
struct Output {
    mixer: Mixer,
    // Fixed for this mixer's lifetime, including device recovery. Each track
    // converts to these values; Bridge handles a new device's own format.
    channels: rodio::ChannelCount,
    rate: rodio::SampleRate,
    /// Our mixer's output while no device has it; the device's `Bridge` holds it.
    slot: Arc<std::sync::Mutex<Option<MixerSource>>>,
    sink: Option<MixerDeviceSink>,
    /// The device in use (cpal's id), to notice the default changing.
    device: Option<String>,
    /// The device the user picked; the default stands in while it's not connected.
    preferred: Option<String>,
    /// Set by the device's error callback.
    lost: Arc<AtomicBool>,
    checked: Instant,
}

impl Output {
    /// Doesn't open the device: the first song does. Also whether there's one to open.
    fn new(preferred: Option<String>) -> (Self, bool) {
        // Mix at the device's own format, so the usual case needs no conversion.
        let device = target(preferred.as_deref());
        let found = device.is_some();
        let config = device.and_then(|d| d.default_output_config().ok());
        let channels = config.as_ref().and_then(|c| std::num::NonZero::new(c.channels())).unwrap_or(std::num::NonZero::new(2).unwrap());
        let rate = config.as_ref().and_then(|c| std::num::NonZero::new(c.sample_rate())).unwrap_or(std::num::NonZero::new(48_000).unwrap());
        let (mixer, source) = rodio::mixer::mixer(channels, rate);
        let lost = Arc::new(AtomicBool::new(false));
        (Self { mixer, channels, rate, slot: Arc::new(std::sync::Mutex::new(Some(source))), sink: None, device: None, preferred, lost, checked: Instant::now() }, found)
    }

    /// Opens the picked device, or the default one. False if there's none to open.
    fn open(&mut self, tx: Sender<AudioCmd>, status: &AudioStatus) -> bool {
        // The old device's stream gives our mixer back when it's dropped.
        self.sink = None;
        self.device = None;
        self.checked = Instant::now();
        let Some(device) = target(self.preferred.as_deref()) else {
            return false;
        };
        let id = device.id().map(|i| i.to_string()).ok();
        let lost = self.lost.clone();
        lost.store(false, Ordering::Relaxed);
        let on_error = move |e: rodio::cpal::StreamError| {
            log::warn!("audio: output device error: {e}");
            if !lost.swap(true, Ordering::Relaxed) {
                let _ = tx.send(AudioCmd::DeviceLost);
            }
        };
        let sink = match DeviceSinkBuilder::from_device(device).and_then(|b| b.with_error_callback(on_error).open_sink_or_fallback()) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("audio: couldn't open the output device: {e}");
                return false;
            }
        };
        // A stream that failed on its own hands the mixer back as its thread exits,
        // which can be just after it reported the error.
        let waited = Instant::now();
        let source = loop {
            if let Some(source) = self.slot.lock().unwrap().take() {
                break source;
            }
            if waited.elapsed() > Duration::from_secs(1) {
                log::warn!("audio: mixer still held by the old device");
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let mut sink = sink;
        sink.log_on_drop(false);
        let channels = rodio::Source::channels(&source).get();
        let rate = rodio::Source::sample_rate(&source).get();
        let eq = eq::Eq::new(status.eq.clone(), rate, channels);
        let limiter = limiter::Limiter::new(status.limit.clone(), rate, channels);
        sink.mixer().add(Bridge { inner: Some(source), slot: self.slot.clone(), channels, n: 0, eq, limiter });
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
        let want = target(self.preferred.as_deref()).and_then(|d| d.id().ok()).map(|i| i.to_string());
        want.is_some() && want != self.device
    }
}

/// Empties the player without waiting on the device. rodio's `clear` waits for the device
/// to skip what's in it, which never happens while the device is let go (a song paused that
/// long), nor while a device that's gone still looks open (until `DeviceLost` arrives): a new
/// player instead, at the same volume and paused, as `clear` leaves it. The old one's song
/// ends the next time the mixer is pulled.
#[cfg(test)]
fn clear(player: &mut Player, output: &Output) {
    let fresh = crate::audio_source::connect_player(&output.mixer, output.channels, output.rate);
    fresh.set_volume(player.volume());
    fresh.pause();
    *player = fresh;
}

/// The picked device if it's connected, else the system default.
fn target(preferred: Option<&str>) -> Option<rodio::cpal::Device> {
    let host = rodio::cpal::default_host();
    if let Some(want) = preferred
        && let Some(d) = host.output_devices().ok().and_then(|mut all| all.find(|d| d.id().is_ok_and(|i| i.to_string() == want)))
    {
        return Some(d);
    }
    host.default_output_device()
}

/// An output device to offer in settings.
#[derive(Clone, PartialEq)]
pub struct OutputDevice {
    /// cpal's id: stable across reconnects.
    pub id: String,
    pub name: String,
}

/// The connected output devices, by the names Windows' sound settings show.
pub fn output_devices() -> Vec<OutputDevice> {
    let Ok(all) = rodio::cpal::default_host().output_devices() else { return Vec::new() };
    let mut out: Vec<OutputDevice> = all
        .filter_map(|d| {
            let id = d.id().ok()?.to_string();
            let desc = d.description().ok()?;
            // "Speakers (Realtek(R) Audio)" rather than just "Speakers".
            let name = desc.extended().first().cloned().unwrap_or_else(|| desc.name().to_owned());
            Some(OutputDevice { id, name })
        })
        .collect();
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

/// Our mixer's output, as a source in the device's mixer. Never ends, so the device
/// keeps it; gives the mixer back when the device's stream drops it.
struct Bridge {
    inner: Option<MixerSource>,
    slot: Arc<std::sync::Mutex<Option<MixerSource>>>,
    channels: u16,
    /// Samples handed out, to give the mixer back on a frame boundary (else the next
    /// device would get the channels swapped).
    n: u64,
    /// Here, after the mixer, so one equalizer covers both songs in a crossfade and a
    /// new song or a seek doesn't restart it.
    eq: eq::Eq,
    /// Last, after the volume, so it only acts on what would really clip.
    limiter: limiter::Limiter,
}

impl Iterator for Bridge {
    type Item = rodio::Sample;
    fn next(&mut self) -> Option<Self::Item> {
        let channel = (self.n % self.channels.max(1) as u64) as usize;
        self.n += 1;
        let s = self.inner.as_mut().and_then(Iterator::next).unwrap_or(0.0);
        Some(self.limiter.process(self.eq.process(s, channel), channel))
    }
}

impl rodio::Source for Bridge {
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> rodio::ChannelCount {
        self.inner.as_ref().map_or(std::num::NonZero::new(self.channels.max(1)).unwrap(), |s| s.channels())
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
        if let Some(mut source) = self.inner.take() {
            while !self.n.is_multiple_of(self.channels.max(1) as u64) {
                source.next();
                self.n += 1;
            }
            *self.slot.lock().unwrap() = Some(source);
        }
    }
}

/// How often to look for a new default device while playing.
const DEVICE_CHECK: Duration = Duration::from_secs(1);

/// How long nothing plays before the output device is let go. An open stream counts as
/// audio playing to the OS (on Windows it keeps the computer from sleeping), even paused.
const IDLE_CLOSE: Duration = Duration::from_secs(30);

pub fn spawn(volume: f32, log_volume: bool, device: Option<String>, events: Sender<AudioEvent>, wake: impl Fn() + Send + 'static) -> AudioHandle {
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
    device: Option<String>,
    events: Sender<AudioEvent>,
    wake: impl Fn(),
) {
    // An open device costs CPU even while silent, and keeps Windows awake: it waits for
    // the first song, as after `IDLE_CLOSE`.
    let (mut output, found) = Output::new(device);
    if !found {
        let _ = events.send(AudioEvent::Error("No audio output device".into()));
        wake();
    }
    let mut player = crate::audio_source::connect_player(&output.mixer, output.channels, output.rate);
    player.set_volume(slider_gain(&status, volume));
    // The playing source's fade-out control (see `Meter`).
    let mut fade_out = Arc::new(AtomicU32::new(0));
    // Songs fading out under the current one (more than one only for a moment).
    let mut fading: Vec<Fading> = Vec::new();
    // Whether `Ending` went out for this generation.
    let mut ending_sent = false;
    // The playing song's length. Not `status.duration()` at a handoff: the UI has
    // already put the next song's there.
    let mut length: Option<Duration> = None;

    // Generation of the source currently in the player (0 = nothing loaded).
    let mut current: u64 = 0;
    let mut shared: Option<Arc<Shared>> = None;
    // Where the playing song is up to, on its own clock (see `Tempo`).
    let mut song_ms = Arc::new(AtomicU64::new(0));
    let song_pos = |ms: &AtomicU64| Duration::from_millis(ms.load(Ordering::Relaxed));
    let tempo = |source, from, ms: &Arc<AtomicU64>| {
        let channels = rodio::Source::channels(&source);
        let rate = rodio::Source::sample_rate(&source);
        let duration = rodio::Source::total_duration(&source);
        let (speed, ms) = (status.speed.clone(), ms.clone());
        crate::audio_source::defer(channels, rate, duration, move || tempo::Tempo::new(source, speed, from, ms))
    };
    let mut interrupt: Option<ReadInterrupt> = None;
    let mut format_error = Arc::new(AtomicBool::new(false));
    // Latest seek; a slower, older seek's decoder must not land after a newer one.
    let mut jobs = DecoderJobs::default();
    // Dev hook: YTM_DEVICE_LOST=<secs> acts as if the output device disconnected that
    // long into the first song (the error callback firing for a device that's gone).
    let mut dev_lost = std::env::var("YTM_DEVICE_LOST").ok().and_then(|v| v.parse::<f32>().ok()).map(Duration::from_secs_f32);
    // Since when nothing has played, to let go of the device after `IDLE_CLOSE`.
    let mut idle_since: Option<Instant> = Some(Instant::now());
    loop {
        // Poll while playing so the position stays fresh and the end is noticed; idle otherwise,
        // waking to let go of the device.
        let timeout = match idle_since {
            None => Duration::from_millis(100),
            Some(since) if output.sink.is_some() => IDLE_CLOSE.saturating_sub(since.elapsed()).max(Duration::from_millis(10)),
            Some(_) => Duration::from_secs(3600),
        };
        match rx.recv_timeout(timeout) {
            Ok(AudioCmd::Start { generation, decoder, shared: s, at }) => {
                if generation != status.wanted_gen.load(Ordering::Relaxed) {
                    log::debug!("audio: dropping stale generation {generation}");
                    continue;
                }
                log::debug!("audio: start generation {generation}");
                jobs.cancel();
                // The new decoder already owns a reader reservation. Wake the
                // previous callback before clear/drop or a device-thread join.
                abort_player(&output, &mut player, &mut interrupt);
                if let Some(old) = shared.replace(s.clone()) && !Arc::ptr_eq(&old, &s) { old.cancel(); }
                if output.sink.is_none() || output.default_changed() {
                    fading.clear();
                    if !output.open(tx.clone(), &status) {
                        let _ = events.send(AudioEvent::Error("No audio output device".into()));
                        wake();
                    }
                }
                let gain = norm_gain(&status, Some(&s));
                show_norm(&status, Some(&s));
                log::info!("audio: loudness {:?} dB, normalization gain {gain:.3}", s.loudness_db);
                player.set_volume(slider_gain(&status, f32::from_bits(status.volume.load(Ordering::Relaxed))) * gain);
                // Fade in only over a song still playing, both over the same length.
                retire_waiting_faders(&mut fading);
                fading.retain(|f| !f.player.empty());
                let crossfade = status.crossfade_ms.load(Ordering::Relaxed);
                let mut fade_in = 0;
                for f in fading.iter_mut().filter(|f| f.waiting.is_some()) {
                    fade_in = f.start(crossfade);
                    log::debug!("audio: crossfade into generation {generation} over {fade_in} ms");
                }
                fade_out = Arc::new(AtomicU32::new(0));
                format_error = Arc::new(AtomicBool::new(false));
                interrupt = Some(decoder.interrupt_handle());
                song_ms = Arc::new(AtomicU64::new(at.as_millis() as u64));
                let source = crate::audio_source::convert(decoder, output.channels, output.rate, format_error.clone());
                player.append(Meter::new(tempo(source, at, &song_ms), status.clone(), fade_in, fade_out.clone()));
                player.play();
                // 0 means the stream didn't say; keep the length the UI already knows.
                if s.duration_ms > 0 {
                    status.duration_ms.store(s.duration_ms, Ordering::Relaxed);
                }
                length = status.duration();
                current = generation;
                ending_sent = false;
                status.stream_ms.store(s.duration_ms, Ordering::Relaxed);
                status.started_gen.store(generation, Ordering::Release);
                status.playing.store(true, Ordering::Relaxed);
            }
            Ok(AudioCmd::Replace { generation, seek, decoder, at, seekable: s }) => {
                if generation != current || seek != jobs.seq {
                    continue;
                }
                log::debug!("audio: seek to {at:?} via new decoder (seekable: {s})");
                let paused = player.is_paused();
                jobs.pending = None;
                abort_player(&output, &mut player, &mut interrupt);
                fade_out = Arc::new(AtomicU32::new(0));
                format_error = Arc::new(AtomicBool::new(false));
                log::info!("audio: generation {generation}, fresh seek decoder, source {} Hz / {} channels -> private mixer {} Hz / {} channels, continuous per-track resampler", rodio::Source::sample_rate(&decoder), rodio::Source::channels(&decoder), output.rate, output.channels);
                interrupt = Some(decoder.interrupt_handle());
                let source = crate::audio_source::convert(decoder, output.channels, output.rate, format_error.clone());
                song_ms = Arc::new(AtomicU64::new(at.as_millis() as u64));
                player.append(Meter::new(tempo(source, at, &song_ms), status.clone(), 0, fade_out.clone()));
                if !paused {
                    player.play();
                }
                // Paused, nothing else tells the UI (and the system media controls) where it is now.
                status.set_position(at.as_millis() as u64);
                wake();
            }
            Ok(AudioCmd::SeekFailed { generation, seek, at, reason }) => {
                if generation != current || seek != jobs.seq { continue; }
                jobs.pending = None;
                let _ = events.send(AudioEvent::Interrupted { generation, at, reason });
                current = 0;
                shared = None;
                status.playing.store(false, Ordering::Relaxed);
                wake();
            }
            Ok(AudioCmd::Pause) => {
                log::debug!("audio: pause at {:?}", song_pos(&song_ms));
                player.pause();
                fading.iter().for_each(|f| f.player.pause());
                status.playing.store(false, Ordering::Relaxed);
                // So the UI (and the system media controls it syncs) see it even when idle.
                wake();
            }
            Ok(AudioCmd::Resume) => {
                log::debug!("audio: resume (generation {current})");
                if current != 0 {
                    // Headphones may have come back (or gone) while paused.
                    if output.sink.is_none() || output.default_changed() {
                        reopen_output(&mut output, ReopenState { player: &mut player, interrupt: &mut interrupt, jobs: &mut jobs, fading: &mut fading, shared: shared.as_ref(), current, at: song_pos(&song_ms) }, &status, &tx);
                    }
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
                    && !interrupt.as_ref().is_some_and(ReadInterrupt::is_waiting)
                    && fading.iter().all(|f| f.player.empty()) =>
            {
                // Keeps playing at full volume until the next song starts (see `Start`).
                // In real time, at the speed it's playing.
                let left = length.map(|d| d.saturating_sub(song_pos(&song_ms)).div_f32(status.speed()));
                log::debug!("audio: handing off from generation {current}, {left:?} left");
                jobs.cancel();
                let old = std::mem::replace(&mut player, crate::audio_source::connect_player(&output.mixer, output.channels, output.rate));
                fading.push(Fading { player: old, shared: shared.take(), interrupt: interrupt.take(), out: fade_out.clone(), waiting: Some((Instant::now(), left)) });
                current = 0;
                status.playing.store(false, Ordering::Relaxed);
                status.set_position(0);
            }
            Ok(cmd @ (AudioCmd::Stop | AudioCmd::StopAt(_) | AudioCmd::Handoff)) => {
                jobs.cancel();
                if matches!(&cmd, AudioCmd::Handoff) {
                    // Skipping again mid-crossfade: everything playing goes, quickly enough
                    // to sound like a cut but without a click, and the next song comes in
                    // at full volume.
                    for f in fading.iter_mut() {
                        f.waiting = None;
                        f.out.store(QUICK_FADE_MS, Ordering::Relaxed);
                    }
                    log::debug!("audio: skipped mid-crossfade, cutting over");
                    if current != 0 && !player.is_paused() && !player.empty()
                        && !interrupt.as_ref().is_some_and(ReadInterrupt::is_waiting) {
                        fade_out.store(QUICK_FADE_MS, Ordering::Relaxed);
                        let old = std::mem::replace(&mut player, crate::audio_source::connect_player(&output.mixer, output.channels, output.rate));
                        fading.push(Fading { player: old, shared: shared.take(), interrupt: interrupt.take(), out: fade_out.clone(), waiting: None });
                    } else {
                        abort_player(&output, &mut player, &mut interrupt);
                    }
                } else {
                    abort_player(&output, &mut player, &mut interrupt);
                    fading.clear();
                }
                current = 0;
                // New playback may already reserve this exact Shared before
                // this queued Stop arrives. Only retire our reader; last-reader
                // lifetime decides whether the download itself is still needed.
                shared = None;
                finish_stop_status(&status, &cmd);
            }
            Ok(cmd @ (AudioCmd::Seek(_) | AudioCmd::SeekLive { .. })) => {
                let (pos, live) = match cmd {
                    AudioCmd::SeekLive { pos, at } => (pos + at.elapsed(), Some((pos, at))),
                    AudioCmd::Seek(pos) => (pos, None),
                    _ => unreachable!(),
                };
                if current == 0 {
                    continue;
                }
                // Seeking ends a crossfade: the new song alone from here.
                fading.clear();
                // A fresh decoder also resets resampler phase and lookahead.
                // The playing decoder was opened non-seekable for a fast start. Build a
                // seekable one over the same buffer off-thread (it needs every fragment
                // header, so it may wait for the download) and swap it in. Back to the
                // start, a fresh non-seekable decoder is enough and opens at once.
                if let Some(s) = shared.clone() {
                    jobs.submit(s, pos, live, current, &tx);
                    abort_player(&output, &mut player, &mut interrupt);
                    song_ms = Arc::new(AtomicU64::new(pos.as_millis() as u64));
                }
            }
            // Failing here is `Start`'s to report. Opened while idle, the device gets the
            // full `IDLE_CLOSE` again, in case the song never comes.
            Ok(AudioCmd::Warm) => {
                if output.sink.is_none() && output.open(tx.clone(), &status) {
                    idle_since = idle_since.map(|_| Instant::now());
                }
            }
            Ok(AudioCmd::DeviceLost) => {
                let was = output.device.clone();
                let was_playing = !player.is_paused() && (current != 0 || !fading.is_empty());
                reopen_output(&mut output, ReopenState { player: &mut player, interrupt: &mut interrupt, jobs: &mut jobs, fading: &mut fading, shared: shared.as_ref(), current, at: song_pos(&song_ms) }, &status, &tx);
                // The device itself went away, not a hiccup on the same one: pause, as
                // unplugging headphones does, rather than carry on out of the speakers.
                if output.device != was && was_playing {
                    log::info!("audio: output device gone, pausing");
                    player.pause();
                    fading.iter().for_each(|f| f.player.pause());
                    status.playing.store(false, Ordering::Relaxed);
                    wake();
                }
            }
            Ok(AudioCmd::SetDevice(id)) => {
                log::info!("audio: output device set to {id:?}");
                output.preferred = id;
                // A device let go while idle opens again with the next song.
                if (output.sink.is_some() || idle_since.is_none()) && (output.sink.is_none() || output.default_changed()) {
                    reopen_output(&mut output, ReopenState { player: &mut player, interrupt: &mut interrupt, jobs: &mut jobs, fading: &mut fading, shared: shared.as_ref(), current, at: song_pos(&song_ms) }, &status, &tx);
                }
            }
            Ok(AudioCmd::Volume(v)) => {
                let v = slider_gain(&status, v);
                player.set_volume(v * norm_gain(&status, shared.as_deref()));
                // The level may be what changed.
                if shared.is_some() {
                    show_norm(&status, shared.as_deref());
                }
                for f in &fading {
                    f.player.set_volume(v * norm_gain(&status, f.shared.as_deref()));
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                jobs.cancel();
                if let Some(interrupt) = interrupt.take() { interrupt.cancel(); }
                fading.clear();
                player.stop();
                return;
            }
        }

        retire_waiting_faders(&mut fading);
        if current != 0 && dev_lost.is_some_and(|lost| song_pos(&song_ms) >= lost) {
            dev_lost = None;
            log::info!("audio: dev hook: output device lost");
            output.device = Some("dev-hook-gone".into());
            output.lost.store(true, Ordering::Relaxed);
            let _ = tx.send(AudioCmd::DeviceLost);
        }
        // Follow the default device while playing (headphones connecting take over).
        // A failed device is `DeviceLost`'s to handle (it decides whether to pause).
        if (current != 0 || !fading.is_empty())
            && !player.is_paused()
            && output.checked.elapsed() >= DEVICE_CHECK
            && !output.lost.load(Ordering::Relaxed)
            && (output.sink.is_none() || output.default_changed())
        {
            reopen_output(&mut output, ReopenState { player: &mut player, interrupt: &mut interrupt, jobs: &mut jobs, fading: &mut fading, shared: shared.as_ref(), current, at: song_pos(&song_ms) }, &status, &tx);
        }
        // Let go of the device once nothing has played for a while; playing opens it again.
        if (current != 0 || !fading.is_empty()) && !player.is_paused() {
            idle_since = None;
        } else if idle_since.get_or_insert_with(Instant::now).elapsed() >= IDLE_CLOSE && output.sink.is_some() {
            log::info!("audio: nothing playing, letting go of the output device");
            output.sink = None;
            output.device = None;
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
            // Start the next song early, to crossfade into it. Not for songs barely
            // longer than the fade. (The fade's in real time: more of the song when faster.)
            let crossfade = Duration::from_millis(status.crossfade_ms.load(Ordering::Relaxed) as u64).mul_f32(status.speed());
            if jobs.pending.is_none() && !ending_sent
                && !crossfade.is_zero()
                && let Some(d) = length
                && d > crossfade * 2
                && pos + crossfade >= d
                // Not a podcast feed's file: the ads its host puts in make it run longer
                // than YouTube Music's length, so it plays to its real end.
                && shared.as_ref().is_some_and(|s| s.error().is_none() && !s.feed)
            {
                ending_sent = true;
                let _ = events.send(AudioEvent::Ending { generation: current });
                wake();
            }
            if jobs.pending.is_none() && player.empty() {
                // A failed download ends the decoder early: that's not the song ending.
                let failure = if format_error.load(Ordering::Acquire) {
                    Some("Audio format changed or ended inside a frame; playback stopped".into())
                } else {
                    shared.as_ref().and_then(|s| s.error())
                };
                let event = match failure {
                    Some(reason) => AudioEvent::Interrupted { generation: current, at: song_pos(&song_ms), reason },
                    None => AudioEvent::Finished { generation: current },
                };
                let _ = events.send(event);
                current = 0;
                interrupt = None;
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
    fn stop_at_freezes_recovery_position_without_resetting_track_identity() {
        let status = AudioStatus::default();
        status.playing.store(true, Ordering::Relaxed);
        status.duration_ms.store(197_508, Ordering::Relaxed);
        status.wanted_gen.store(191, Ordering::Relaxed);
        status.set_position(900);
        finish_stop_status(&status, &AudioCmd::StopAt(Duration::from_millis(113_195)));
        assert!(!status.playing.load(Ordering::Relaxed));
        assert_eq!(status.position(), Duration::from_millis(113_195));
        assert_eq!(status.position_smooth(), Duration::from_millis(113_195));
        assert_eq!(status.duration(), Some(Duration::from_millis(197_508)));
        assert_eq!(status.wanted_gen.load(Ordering::Relaxed), 191);
    }

    #[test]
    fn ordinary_stop_and_unfaded_handoff_still_reset_position() {
        for command in [AudioCmd::Stop, AudioCmd::Handoff] {
            let status = AudioStatus::default();
            status.playing.store(true, Ordering::Relaxed);
            status.set_position(113_195);
            finish_stop_status(&status, &command);
            assert!(!status.playing.load(Ordering::Relaxed));
            assert_eq!(status.position_smooth(), Duration::ZERO);
        }
    }

    #[test]
    fn decoder_worker_reports_a_typed_panic_and_accepts_the_next_job() {
        fn panicking_decoder(job: SeekJob, tx: &Sender<AudioCmd>) {
            build_seek_using(job, tx, |_, _, _| panic!("synthetic decoder failure"));
        }
        let shared = Shared::new(1, 0, None, false, false);
        let interrupt = shared.read_interrupt();
        let queued = Arc::new(std::sync::Mutex::new(Some(SeekJob {
            shared, interrupt, pos: Duration::ZERO, live: None, generation: 7, seek: 1,
        })));
        let busy = Arc::new(AtomicBool::new(true));
        let (tx, rx) = unbounded();
        let (queue_worker, busy_worker, tx_worker) = (queued.clone(), busy.clone(), tx.clone());
        let worker = std::thread::spawn(move || decoder_worker(queue_worker, busy_worker, tx_worker, panicking_decoder));
        match rx.recv_timeout(Duration::from_secs(1)).unwrap() {
            AudioCmd::SeekFailed { generation, seek, reason, .. } => {
                assert_eq!((generation, seek), (7, 1));
                assert_eq!(reason, "Seek failed: Audio decoder worker panicked");
            }
            _ => panic!("decoder panic was not returned as a typed failure"),
        }
        worker.join().unwrap();
        assert!(!busy.load(Ordering::Acquire));
        // Exercise the actual scheduling path after that panic, rather than
        // only inspecting the catch_unwind result or the busy flag.
        let mut jobs = DecoderJobs { seq: 1, pending: None, queued, busy };
        jobs.submit(Shared::new(1, 0, None, false, false), Duration::ZERO, None, 8, &tx);
        let deadline = Instant::now() + Duration::from_secs(1);
        while jobs.queued.lock().unwrap().is_some() && Instant::now() < deadline { std::thread::yield_now(); }
        let acquired = jobs.queued.lock().unwrap().is_none();
        jobs.cancel();
        assert!(acquired, "next decoder worker did not acquire the queued job");
        match rx.recv_timeout(Duration::from_secs(1)).unwrap() {
            AudioCmd::SeekFailed { generation, seek, .. } => {
                assert_eq!(generation, 8);
                assert_ne!(seek, jobs.seq);
            }
            _ => panic!("next decoder job did not run after the panic"),
        }
    }

    #[test]
    fn retiring_a_network_blocked_fader_releases_ready_incoming_audio() {
        use std::io::Read;
        use std::num::NonZero;
        struct MissingInput(stream::StreamReader);
        impl Iterator for MissingInput {
            type Item = f32;
            fn next(&mut self) -> Option<f32> {
                let mut byte = [0];
                self.0.read(&mut byte).ok().filter(|&n| n > 0).map(|_| 0.0)
            }
        }
        impl rodio::Source for MissingInput {
            fn current_span_len(&self) -> Option<usize> { None }
            fn channels(&self) -> rodio::ChannelCount { NonZero::new(2).unwrap() }
            fn sample_rate(&self) -> rodio::SampleRate { NonZero::new(48000).unwrap() }
            fn total_duration(&self) -> Option<Duration> { None }
        }
        let channels = NonZero::new(2).unwrap();
        let rate = NonZero::new(48000).unwrap();
        let (mixer, mut output) = rodio::mixer::mixer(channels, rate);
        let shared = Shared::new(1, 0, None, false, false);
        let interrupt = shared.read_interrupt();
        let outgoing = crate::audio_source::connect_player(&mixer, channels, rate);
        outgoing.append(MissingInput(stream::StreamReader::with_interrupt(interrupt.clone())));
        let incoming = crate::audio_source::connect_player(&mixer, channels, rate);
        incoming.append(rodio::buffer::SamplesBuffer::new(channels, rate, vec![0.25; 2000]));
        let mut fading = vec![Fading {
            player: outgoing, shared: Some(shared), interrupt: Some(interrupt.clone()),
            out: Arc::new(AtomicU32::new(0)), waiting: None,
        }];
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let samples: Vec<_> = (0..8000).map(|_| output.next().unwrap()).collect();
            let _ = done_tx.send(samples);
        });
        let deadline = Instant::now() + Duration::from_secs(1);
        while !interrupt.is_waiting() && Instant::now() < deadline { std::thread::yield_now(); }
        let reached_wait = interrupt.is_waiting();
        // Even a failed assertion must first release and join the reproducer.
        retire_waiting_faders(&mut fading);
        interrupt.cancel();
        let samples = done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        worker.join().unwrap();
        assert!(reached_wait, "outgoing source did not reach its missing-block wait");
        assert!(fading.is_empty());
        assert_eq!(samples.iter().filter(|&&s| s == 0.25).count(), 2000);
        assert!(incoming.empty());
    }

    #[test]
    fn dropping_fading_reader_does_not_cancel_a_replacement_on_the_same_download() {
        use std::num::NonZero;
        let shared = Shared::new(1, 0, None, false, false);
        let outgoing = shared.read_interrupt();
        let replacement = shared.read_interrupt();
        let (mixer, _output) = rodio::mixer::mixer(NonZero::new(2).unwrap(), NonZero::new(48000).unwrap());
        drop(Fading {
            player: crate::audio_source::connect_player(&mixer, NonZero::new(2).unwrap(), NonZero::new(48000).unwrap()),
            shared: Some(shared.clone()), interrupt: Some(outgoing.clone()),
            out: Arc::new(AtomicU32::new(0)), waiting: None,
        });
        assert!(outgoing.is_cancelled());
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let building = replacement.clone();
        let worker = std::thread::spawn(move || {
            let _ = started_tx.send(());
            let result = stream::decoder_with_interrupt(shared, false, building);
            let _ = done_tx.send(result.is_err());
        });
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        // The missing byte should still wait, rather than immediately fail
        // because Fading globally cancelled the replacement's Shared.
        assert_eq!(done_rx.recv_timeout(Duration::from_millis(30)), Err(std::sync::mpsc::RecvTimeoutError::Timeout));
        replacement.cancel();
        assert_eq!(done_rx.recv_timeout(Duration::from_secs(1)), Ok(true));
        worker.join().unwrap();
    }

    #[test]
    fn recovery_reserves_replacement_and_interrupts_all_old_readers_before_join() {
        use std::num::NonZero;
        let channels = NonZero::new(2).unwrap();
        let rate = NonZero::new(48000).unwrap();
        let (mixer, source) = rodio::mixer::mixer(channels, rate);
        let output = Output {
            mixer, channels, rate,
            slot: Arc::new(std::sync::Mutex::new(Some(source))),
            sink: None, device: None, preferred: None,
            lost: Arc::new(AtomicBool::new(false)), checked: Instant::now(),
        };
        let shared = Shared::new(1, 0, None, false, false);
        let old = shared.read_interrupt();
        let outgoing_shared = Shared::new(1, 0, None, false, false);
        let outgoing = outgoing_shared.read_interrupt();
        let mut interrupt = Some(old.clone());
        let mut player = crate::audio_source::connect_player(&output.mixer, channels, rate);
        player.set_volume(0.375);
        player.pause();
        // A source not consumed by any output used to make clear() block.
        player.append(rodio::buffer::SamplesBuffer::new(channels, rate, vec![0.25; 2000]));
        let mut fading = vec![Fading {
            player: crate::audio_source::connect_player(&output.mixer, channels, rate),
            shared: Some(outgoing_shared), interrupt: Some(outgoing.clone()),
            out: Arc::new(AtomicU32::new(0)), waiting: None,
        }];
        let mut jobs = DecoderJobs::default();
        let at = Duration::from_secs(3);
        let (tx, rx) = unbounded();
        // This is exactly the preparation executed before Output::open drops
        // and joins a WASAPI stream. No real device is opened in this test.
        prepare_reopen(&output, ReopenState { player: &mut player, interrupt: &mut interrupt, jobs: &mut jobs, fading: &mut fading, shared: Some(&shared), current: 7, at }, &tx);
        assert!(old.is_cancelled());
        assert!(outgoing.is_cancelled());
        assert!(interrupt.is_none());
        assert!(fading.is_empty());
        assert!(player.empty());
        assert!(player.is_paused());
        assert_eq!(player.volume(), 0.375);
        assert!(!jobs.pending.as_ref().unwrap().is_cancelled());
        // Cancellation before the worker takes its queued job legitimately
        // produces no completion. Establish an actual reader wait first so
        // this test exercises in-flight cancellation and its stale result.
        let deadline = Instant::now() + Duration::from_secs(1);
        while !jobs.pending.as_ref().unwrap().is_waiting() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let reached_wait = jobs.pending.as_ref().unwrap().is_waiting();
        // Cancel the synthetic decoder waiting for its missing input block.
        // Its completion is now stale and cannot end/recover the current song.
        jobs.cancel();
        assert!(reached_wait, "replacement decoder did not reach its missing-block wait");
        match rx.recv_timeout(Duration::from_secs(1)).unwrap() {
            AudioCmd::SeekFailed { generation, seek, .. } => {
                assert_eq!(generation, 7);
                assert_ne!(seek, jobs.seq);
            }
            _ => panic!("missing-input decoder unexpectedly opened"),
        }
    }

    #[test]
    fn converted_tracks_crossfade_in_the_fixed_mixer_format() {
        use std::num::NonZero;
        use rodio::Source;
        let channels = NonZero::new(2).unwrap();
        let rate = NonZero::new(44100).unwrap();
        let status = Arc::new(AudioStatus::default());
        let outgoing_flag = Arc::new(AtomicBool::new(false));
        let incoming_flag = Arc::new(AtomicBool::new(false));
        // Opus-like 48 kHz stereo and AAC-like 44.1 kHz mono, converted
        // independently before Meter, exactly as Start / Replace do.
        let outgoing = crate::audio_source::convert(
            rodio::buffer::SamplesBuffer::new(channels, NonZero::new(48000).unwrap(), vec![1.0; 19200]),
            channels, rate, outgoing_flag.clone(),
        );
        let incoming = crate::audio_source::convert(
            rodio::buffer::SamplesBuffer::new(NonZero::new(1).unwrap(), rate, vec![1.0; 8820]),
            channels, rate, incoming_flag.clone(),
        );
        let out = Arc::new(AtomicU32::new(100));
        let mut outgoing = Meter::new(outgoing, status.clone(), 0, out);
        let mut incoming = Meter::new(incoming, status, 100, Arc::new(AtomicU32::new(0)));
        assert_eq!(outgoing.sample_rate(), rate);
        assert_eq!(incoming.channels(), channels);
        for _ in 0..8820 {
            let sum = incoming.next().unwrap() + outgoing.next().unwrap();
            assert!((sum - 1.0).abs() < 1e-5);
        }
        assert_eq!(outgoing.next(), None);
        assert_eq!(incoming.next(), Some(1.0));
        assert!(!outgoing_flag.load(Ordering::Acquire));
        assert!(!incoming_flag.load(Ordering::Acquire));
    }

    #[test]
    fn output_bridge_recovery_preserves_fixed_format_and_channel_alignment() {
        use std::num::NonZero;
        use rodio::Source;
        let channels = NonZero::new(2).unwrap();
        let rate = NonZero::new(44100).unwrap();
        let (mixer, source) = rodio::mixer::mixer(channels, rate);
        mixer.add(rodio::buffer::SamplesBuffer::new(channels, rate, vec![0.25, -0.5, 0.75, -0.9]));
        let slot = Arc::new(std::sync::Mutex::new(None));
        let mut old = Bridge { inner: Some(source), slot: slot.clone(), channels: 2, n: 0,
            eq: eq::Eq::new(Default::default(), 44100, 2),
            limiter: limiter::Limiter::new(Arc::new(AtomicBool::new(false)), 44100, 2) };
        assert_eq!(old.next(), Some(0.0)); // The limiter retains its upstream lookahead.
        // Dropping an output device after its left sample must discard only
        // that frame's right sample, then return the original mixer.
        drop(old);
        let mut source = slot.lock().unwrap().take().unwrap();
        assert_eq!(source.next(), Some(0.75));
        assert_eq!(source.next(), Some(-0.9));
        assert_eq!(source.channels(), channels);
        assert_eq!(source.sample_rate(), rate);
        let mut recovered = Bridge { inner: Some(source), slot: slot.clone(), channels: 2, n: 0,
            eq: eq::Eq::new(Default::default(), 44100, 2),
            limiter: limiter::Limiter::new(Arc::new(AtomicBool::new(false)), 44100, 2) };
        assert_eq!(recovered.next(), Some(0.0));
        assert_eq!(recovered.sample_rate(), rate);
        drop(recovered);
        assert!(slot.lock().unwrap().is_some());
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
    fn emptying_the_player_doesnt_wait_on_a_device_let_go() {
        use std::num::NonZero;
        let (mixer, source) = rodio::mixer::mixer(NonZero::new(2).unwrap(), NonZero::new(48_000).unwrap());
        // Let go of while paused: nothing pulls the mixer.
        let output = Output { mixer, channels: NonZero::new(2).unwrap(), rate: NonZero::new(48_000).unwrap(), slot: Arc::new(std::sync::Mutex::new(Some(source))), sink: None, device: None, preferred: None, lost: Default::default(), checked: Instant::now() };
        let mut player = crate::audio_source::connect_player(&output.mixer, output.channels, output.rate);
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
        let output = Output { mixer, channels: NonZero::new(2).unwrap(), rate: NonZero::new(48_000).unwrap(), slot: Arc::new(std::sync::Mutex::new(None)), sink: None, device: None, preferred: None, lost: Default::default(), checked: Instant::now() };
        let mut player = crate::audio_source::connect_player(&output.mixer, output.channels, output.rate);
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
}
