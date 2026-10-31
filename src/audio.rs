//! Audio engine: one thread owns the output device and a rodio `Player`.
//!
//! The UI talks to it through `AudioHandle`; status flows back through `AudioStatus`
//! (lock-free reads for the progress bar) plus a `Finished` event for auto-advance.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use rodio::cpal::traits::{DeviceTrait, HostTrait};
use rodio::mixer::{Mixer, MixerSource};
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player};

use crate::stream::{self, Shared};

pub use crate::stream::TrackDecoder;

pub enum AudioCmd {
    /// A decoder is ready for playback generation `generation`; stale generations are dropped.
    /// A decoder started mid-song (`at` > 0) is the seekable kind.
    Start { generation: u64, decoder: TrackDecoder, shared: Arc<Shared>, at: Duration },
    /// A decoder positioned at `at`, replacing the current one; only the latest seek's lands.
    Replace { generation: u64, seek: u64, decoder: TrackDecoder, at: Duration, seekable: bool },
    Pause,
    Resume,
    Stop,
    /// Another song is on its way: with crossfade on, fade this one out instead of
    /// stopping it, and fade the next one in when it starts.
    Handoff,
    Seek(Duration),
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
    /// f32 bits
    volume: AtomicU32,
    /// Turn loud tracks down to YouTube's loudness target.
    pub normalize: AtomicBool,
    /// The volume slider follows loudness (cubic) rather than amplitude.
    log_volume: AtomicBool,
    /// Smoothed output loudness 0..1 (f32 bits), for audio-reactive visuals.
    level: AtomicU32,
    /// How far the output device runs behind the position (its buffer), in ms.
    latency_ms: AtomicU32,
    /// Crossfade length in ms; 0 = off.
    crossfade_ms: AtomicU32,
}

impl AudioStatus {
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
        Duration::from_millis(pos + age)
    }

    pub fn duration(&self) -> Option<Duration> {
        let d = self.duration_ms.load(Ordering::Relaxed);
        (d > 0).then(|| Duration::from_millis(d))
    }

    /// How long audio takes from the player to the speakers (the output buffer).
    pub fn latency(&self) -> Duration {
        Duration::from_millis(self.latency_ms.load(Ordering::Relaxed) as u64)
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

    pub fn set_normalize(&self, on: bool) {
        self.status.normalize.store(on, Ordering::Relaxed);
        self.send(AudioCmd::Volume(self.volume()));
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
}

/// Volume normalization the way YouTube Music does it: tracks mastered louder than
/// the -14 LUFS target are turned down by the difference; quieter ones are left alone
/// (boosting would clip).
/// A seek that needs a new decoder built over the stream buffer (off the audio thread).
struct SeekJob {
    shared: Arc<Shared>,
    pos: Duration,
    /// For a live seek (a jam): the host position and when it was, to catch up after the build.
    live: Option<(Duration, std::time::Instant)>,
    generation: u64,
    seek: u64,
}

/// Build a decoder for `j` (it may wait for the download) and hand it to the audio thread.
fn build_seek(j: SeekJob, tx: &Sender<AudioCmd>, events: &Sender<AudioEvent>) {
    let SeekJob { shared, pos, live, generation, seek } = j;
    let seekable = !pos.is_zero();
    let res = stream::decoder(shared.clone(), seekable).map_err(|e| e.to_string()).and_then(|mut d| {
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
        Err(e) => match shared.error() {
            Some(reason) => {
                let _ = events.send(AudioEvent::Interrupted { generation, at: pos, reason });
            }
            None => {
                let _ = events.send(AudioEvent::Error(format!("Seek failed: {e}")));
            }
        },
    }
}

/// A song fading out under the next one, kept until it's silent.
struct Fading {
    player: Player,
    shared: Option<Arc<Shared>>,
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
        if let Some(s) = &self.shared {
            s.cancel();
        }
    }
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

fn norm_gain(status: &AudioStatus, shared: Option<&Shared>) -> f32 {
    match shared.and_then(|s| s.loudness_db) {
        Some(db) if db > 0.0 && status.normalize.load(Ordering::Relaxed) => 10f32.powf(-db / 20.0),
        _ => 1.0,
    }
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
    /// The device in use (cpal's id), to notice the default changing.
    device: Option<String>,
    /// The device the user picked; the default stands in while it's not connected.
    preferred: Option<String>,
    /// Set by the device's error callback.
    lost: Arc<AtomicBool>,
    checked: Instant,
}

impl Output {
    fn new(tx: Sender<AudioCmd>, status: &AudioStatus, preferred: Option<String>) -> Self {
        // Mix at the device's own format, so the usual case needs no conversion.
        let config = target(preferred.as_deref()).and_then(|d| d.default_output_config().ok());
        let channels = config.as_ref().and_then(|c| std::num::NonZero::new(c.channels())).unwrap_or(std::num::NonZero::new(2).unwrap());
        let rate = config.as_ref().and_then(|c| std::num::NonZero::new(c.sample_rate())).unwrap_or(std::num::NonZero::new(48_000).unwrap());
        let (mixer, source) = rodio::mixer::mixer(channels, rate);
        let lost = Arc::new(AtomicBool::new(false));
        let mut out = Self { mixer, slot: Arc::new(std::sync::Mutex::new(Some(source))), sink: None, device: None, preferred, lost, checked: Instant::now() };
        out.open(tx, status);
        out
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
        sink.mixer().add(Bridge { inner: Some(source), slot: self.slot.clone(), channels, n: 0 });
        // The position counts samples handed to the device; it plays them a buffer later
        // (rodio asks for ~50 ms), plus the shared-mode device period (~10 ms).
        let rate = sink.config().sample_rate().get().max(1);
        let buffer_ms = match sink.config().buffer_size() {
            rodio::cpal::BufferSize::Fixed(n) => *n as u64 * 1000 / rate as u64,
            rodio::cpal::BufferSize::Default => 0,
        };
        status.latency_ms.store((buffer_ms + 10) as u32, Ordering::Relaxed);
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
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
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
}

impl Iterator for Bridge {
    type Item = rodio::Sample;
    fn next(&mut self) -> Option<Self::Item> {
        self.n += 1;
        Some(self.inner.as_mut().and_then(Iterator::next).unwrap_or(0.0))
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
            while self.n % self.channels.max(1) as u64 != 0 {
                source.next();
                self.n += 1;
            }
            *self.slot.lock().unwrap() = Some(source);
        }
    }
}

/// How often to look for a new default device while playing.
const DEVICE_CHECK: Duration = Duration::from_secs(1);

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
    let mut output = Output::new(tx.clone(), &status, device);
    if output.sink.is_none() {
        let _ = events.send(AudioEvent::Error("No audio output device".into()));
        wake();
    }
    let mut player = Player::connect_new(&output.mixer);
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
    // The player counts position from when a source was appended; a swapped-in
    // seeked decoder starts at `offset`.
    let mut offset = Duration::ZERO;
    // Whether the playing decoder can seek in place (it indexed the whole file).
    let mut seekable = false;
    // Latest seek; a slower, older seek's decoder must not land after a newer one.
    let mut seek_seq: u64 = 0;
    let seek_job: Arc<std::sync::Mutex<Option<SeekJob>>> = Default::default();
    let seek_busy = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // Dev hook: YTM_DEVICE_LOST=<secs> acts as if the output device disconnected that
    // long into the first song (the error callback firing for a device that's gone).
    let mut dev_lost = std::env::var("YTM_DEVICE_LOST").ok().and_then(|v| v.parse::<f32>().ok()).map(Duration::from_secs_f32);
    loop {
        // Poll while playing so the position stays fresh and the end is noticed; idle otherwise.
        let timeout = if (current != 0 || !fading.is_empty()) && !player.is_paused() { Duration::from_millis(100) } else { Duration::from_secs(3600) };
        match rx.recv_timeout(timeout) {
            Ok(AudioCmd::Start { generation, decoder, shared: s, at }) => {
                if generation != status.wanted_gen.load(Ordering::Relaxed) {
                    log::debug!("audio: dropping stale generation {generation}");
                    continue;
                }
                log::debug!("audio: start generation {generation}");
                if output.sink.is_none() || output.default_changed() {
                    output.open(tx.clone(), &status);
                }
                player.clear();
                let gain = norm_gain(&status, Some(&s));
                log::info!("audio: loudness {:?} dB, normalization gain {gain:.3}", s.loudness_db);
                player.set_volume(slider_gain(&status, f32::from_bits(status.volume.load(Ordering::Relaxed))) * gain);
                // Fade in only over a song still playing, both over the same length.
                fading.retain(|f| !f.player.empty());
                let crossfade = status.crossfade_ms.load(Ordering::Relaxed);
                let mut fade_in = 0;
                for f in fading.iter_mut().filter(|f| f.waiting.is_some()) {
                    fade_in = f.start(crossfade);
                    log::debug!("audio: crossfade into generation {generation} over {fade_in} ms");
                }
                fade_out = Arc::new(AtomicU32::new(0));
                player.append(Meter::new(decoder, status.clone(), fade_in, fade_out.clone()));
                player.play();
                // 0 means the stream didn't say; keep the length the UI already knows.
                if s.duration_ms > 0 {
                    status.duration_ms.store(s.duration_ms, Ordering::Relaxed);
                }
                length = status.duration();
                if let Some(old) = shared.replace(s.clone())
                    && !Arc::ptr_eq(&old, &s)
                {
                    old.cancel();
                }
                current = generation;
                offset = at;
                seekable = !at.is_zero();
                ending_sent = false;
                status.playing.store(true, Ordering::Relaxed);
            }
            Ok(AudioCmd::Replace { generation, seek, decoder, at, seekable: s }) => {
                if generation != current || seek != seek_seq {
                    continue;
                }
                log::debug!("audio: seek to {at:?} via new decoder (seekable: {s})");
                seekable = s;
                let paused = player.is_paused();
                player.clear();
                fade_out = Arc::new(AtomicU32::new(0));
                player.append(Meter::new(decoder, status.clone(), 0, fade_out.clone()));
                if !paused {
                    player.play();
                }
                offset = at;
            }
            Ok(AudioCmd::Pause) => {
                log::debug!("audio: pause at {:?}", offset + player.get_pos());
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
                        output.open(tx.clone(), &status);
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
                    && fading.iter().all(|f| f.player.empty()) =>
            {
                // Keeps playing at full volume until the next song starts (see `Start`).
                let left = length.map(|d| d.saturating_sub(offset + player.get_pos()));
                log::debug!("audio: handing off from generation {current}, {left:?} left");
                let old = std::mem::replace(&mut player, Player::connect_new(&output.mixer));
                fading.push(Fading { player: old, shared: shared.take(), out: fade_out.clone(), waiting: Some((Instant::now(), left)) });
                current = 0;
                status.playing.store(false, Ordering::Relaxed);
                status.set_position(0);
            }
            Ok(cmd @ (AudioCmd::Stop | AudioCmd::Handoff)) => {
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
                        fading.push(Fading { player: old, shared: shared.take(), out: fade_out.clone(), waiting: None });
                    } else {
                        player.clear();
                    }
                } else {
                    player.clear();
                    fading.clear();
                }
                current = 0;
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
                // Seeking ends a crossfade: the new song alone from here.
                for f in &fading {
                    f.out.store(QUICK_FADE_MS, Ordering::Relaxed);
                }
                // A seekable decoder already indexed the (fully downloaded) file.
                if seekable && player.try_seek(pos).is_ok() {
                    log::debug!("audio: seek to {pos:?} in place");
                    offset = Duration::ZERO;
                    status.set_position(pos.as_millis() as u64);
                    continue;
                }
                // The playing decoder was opened non-seekable for a fast start. Build a
                // seekable one over the same buffer off-thread (it needs every fragment
                // header, so it may wait for the download) and swap it in. Back to the
                // start, a fresh non-seekable decoder is enough and opens at once.
                if let Some(s) = shared.clone() {
                    // One builder at a time, always for the latest seek: a burst of seeks
                    // (a jam catching up) mustn't pile up threads waiting on the download.
                    *seek_job.lock().unwrap() = Some(SeekJob { shared: s, pos, live, generation: current, seek: seek_seq });
                    if !seek_busy.swap(true, Ordering::AcqRel) {
                        let (job, busy, tx, events) = (seek_job.clone(), seek_busy.clone(), tx.clone(), events.clone());
                        std::thread::spawn(move || {
                            loop {
                                let next = job.lock().unwrap().take();
                                match next {
                                    Some(j) => build_seek(j, &tx, &events),
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
            Ok(AudioCmd::DeviceLost) => {
                let was = output.device.clone();
                output.open(tx.clone(), &status);
                // The device itself went away, not a hiccup on the same one: pause, as
                // unplugging headphones does, rather than carry on out of the speakers.
                if output.device != was && !player.is_paused() && (current != 0 || !fading.is_empty()) {
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
                if output.default_changed() || output.sink.is_none() {
                    output.open(tx.clone(), &status);
                }
            }
            Ok(AudioCmd::Volume(v)) => {
                let v = slider_gain(&status, v);
                player.set_volume(v * norm_gain(&status, shared.as_deref()));
                for f in &fading {
                    f.player.set_volume(v * norm_gain(&status, f.shared.as_deref()));
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
        }

        if current != 0 && dev_lost.is_some_and(|at| offset + player.get_pos() >= at) {
            dev_lost = None;
            log::info!("audio: dev hook: output device lost");
            output.sink = None;
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
        {
            if output.sink.is_none() || output.default_changed() {
                output.open(tx.clone(), &status);
            }
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
            let pos = offset + player.get_pos();
            status.set_position(pos.as_millis() as u64);
            // Start the next song early, to crossfade into it. Not for songs barely
            // longer than the fade.
            let crossfade = Duration::from_millis(status.crossfade_ms.load(Ordering::Relaxed) as u64);
            if !ending_sent
                && !crossfade.is_zero()
                && let Some(d) = length
                && d > crossfade * 2
                && pos + crossfade >= d
                && shared.as_ref().is_some_and(|s| s.error().is_none())
            {
                ending_sent = true;
                let _ = events.send(AudioEvent::Ending { generation: current });
                wake();
            }
            if player.empty() {
                // A failed download ends the decoder early: that's not the song ending.
                let event = match shared.as_ref().and_then(|s| s.error()) {
                    Some(reason) => AudioEvent::Interrupted { generation: current, at: offset + player.get_pos(), reason },
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
    fn normalization_only_turns_loud_tracks_down() {
        let status = AudioStatus::default();
        let loud = Shared::new(1, 0, Some(7.29), false, false);
        let quiet = Shared::new(1, 0, Some(-4.0), false, false);
        let unknown = Shared::new(1, 0, None, false, false);
        status.normalize.store(true, Ordering::Relaxed);
        assert!((norm_gain(&status, Some(&loud)) - 0.432).abs() < 0.001);
        assert_eq!(norm_gain(&status, Some(&quiet)), 1.0);
        assert_eq!(norm_gain(&status, Some(&unknown)), 1.0);
        assert_eq!(norm_gain(&status, None), 1.0);
        status.normalize.store(false, Ordering::Relaxed);
        assert_eq!(norm_gain(&status, Some(&loud)), 1.0);
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
