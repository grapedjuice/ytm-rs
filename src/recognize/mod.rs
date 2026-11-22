//! "Recognize song": listen to what the computer is playing, ask Shazam what it is, and
//! find it on YouTube Music. Experimental: Shazam's endpoint is unofficial.
//!
//! It records for up to 12 s and asks at 4, 8 and 12 (each time about all it has heard),
//! stopping at the first answer, as Shazam's own apps do.

mod capture;
mod meter;
mod shazam;
mod signature;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::model::Track;
use capture::Event;
pub use meter::BARS;
pub use shazam::Heard;
use signature::{SAMPLE_RATE, Signature};

/// When it asks Shazam, from the start.
const ASK_AT: [Duration; 3] = [Duration::from_secs(4), Duration::from_secs(8), Duration::from_secs(12)];
/// With nothing to hear this long, nothing is playing.
const QUIET: Duration = Duration::from_secs(10);
/// What's worth asking about.
const ENOUGH: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub enum Phase {
    /// Recording, and asking about what it has so far. `hearing`: something plays (else,
    /// silence so far).
    Listening { hearing: bool },
    /// Done recording: asking about all of it, or looking the answer up on YouTube Music.
    Identifying,
    Found(Box<Track>),
    /// Shazam knew it, YouTube Music had no sure match: worth a search. With its top hit,
    /// shown as itself (a cover, say), not as what Shazam named.
    Unsure(Heard, Option<Box<Track>>),
    NoMatch,
    Failed(Problem),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Problem {
    /// This system can't record what it plays.
    Unsupported,
    /// macOS: the output is a headset, with a microphone of its own, and only an output
    /// without one can be recorded.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    HeadsetOutput,
    NoDevice,
    NothingPlaying,
    DeviceChanged,
    Offline,
    NoAnswer,
    /// Shazam asks to wait until then.
    RateLimited(Instant),
}

#[derive(Default)]
struct State {
    /// `None` when there's nothing to show.
    phase: Option<Phase>,
    /// Counts runs: a closed one's answer is dropped.
    run: u64,
    stop: Arc<AtomicBool>,
    cool_down: Option<Instant>,
    /// What it hears, and when.
    bars: ([f32; BARS], Option<Instant>),
}

#[derive(Default)]
pub struct Recognizer {
    state: Mutex<State>,
}

impl Recognizer {
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    pub fn phase(&self) -> Option<Phase> {
        self.state.lock().unwrap().phase.clone()
    }

    /// What it hears now, 0 to 1 from low to high: flat once nothing comes.
    pub fn bars(&self) -> [f32; BARS] {
        match self.state.lock().unwrap().bars {
            (bars, Some(at)) if at.elapsed() < Duration::from_millis(200) => bars,
            _ => [0.0; BARS],
        }
    }

    pub fn busy(&self) -> bool {
        matches!(self.phase(), Some(Phase::Listening { .. } | Phase::Identifying))
    }

    /// Cancels a run, or puts away its answer.
    pub fn close(&self) {
        let mut s = self.state.lock().unwrap();
        s.stop.store(true, Ordering::Relaxed);
        s.run += 1;
        s.phase = None;
    }

    /// Records what plays from `skip` on (our own pause fading out before then). False when
    /// it doesn't listen: busy already, or Shazam asked to wait.
    pub fn start(self: &Arc<Self>, rt: &crate::backend::Backend, skip: Duration) -> bool {
        let mut s = self.state.lock().unwrap();
        if matches!(s.phase, Some(Phase::Listening { .. } | Phase::Identifying)) {
            return false;
        }
        if let Some(until) = s.cool_down.filter(|u| *u > Instant::now()) {
            s.phase = Some(Phase::Failed(Problem::RateLimited(until)));
            return false;
        }
        s.run += 1;
        s.stop = Arc::new(AtomicBool::new(false));
        s.phase = Some(Phase::Listening { hearing: false });
        let (run, stop) = (s.run, s.stop.clone());
        drop(s);

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let recording = stop.clone();
        let spawned = std::thread::Builder::new().name("recognize".into()).spawn(move || capture::record(&recording, &tx));
        if let Err(e) = spawned {
            log::warn!("recognize: {e}");
            self.finish(run, Phase::Failed(Problem::Unsupported));
            return true;
        }
        let (this, api, http, backend) = (self.clone(), rt.api(), rt.http(), rt.clone());
        rt.spawn(async move {
            // The recording ends with this, however it ends.
            let _stop = StopOnDrop(stop.clone());
            let phase = this.listen(run, rx, skip, &stop, &api, &http).await;
            this.finish(run, phase);
            backend.wake();
        });
        true
    }

    fn closed(&self, run: u64) -> bool {
        self.state.lock().unwrap().run != run
    }

    /// Done once the run is closed (it looks every 100 ms).
    async fn closing(&self, run: u64) {
        while !self.closed(run) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn finish(&self, run: u64, phase: Phase) {
        let mut s = self.state.lock().unwrap();
        if s.run != run {
            return;
        }
        if let Phase::Failed(Problem::RateLimited(until)) = phase {
            s.cool_down = Some(until);
        }
        s.phase = Some(phase);
    }

    fn set_bars(&self, run: u64, bars: [f32; BARS]) {
        let mut s = self.state.lock().unwrap();
        if s.run == run {
            s.bars = (bars, Some(Instant::now()));
        }
    }

    fn set(&self, run: u64, phase: Phase) {
        let mut s = self.state.lock().unwrap();
        if s.run == run {
            s.phase = Some(phase);
        }
    }

    /// Records, and asks at each of `ASK_AT` about what's new since the last ask, while it
    /// keeps recording (one ask at a time: a slow one makes the next wait). Closing the run
    /// drops this, and so any ask under way.
    async fn listen(&self, run: u64, mut rx: tokio::sync::mpsc::UnboundedReceiver<Event>, skip: Duration, stop: &AtomicBool, api: &crate::ytmapi::Api, http: &reqwest::Client) -> Phase {
        let start = Instant::now();
        let mut ear = Ear::default();
        // The next of `ASK_AT`; one is due; how much audio the last ask had.
        let (mut next, mut due, mut asked) = (0, false, 0);
        let mut asking = None;
        loop {
            if self.closed(run) {
                return Phase::NoMatch; // nobody sees it
            }
            let recording = next < ASK_AT.len();
            if due && asking.is_none() {
                due = false;
                // Not about silence, nor again about the same audio.
                if ear.heard && ear.audio.len() > asked && (ear.audio.len() as u64) >= ear.rate as u64 * ENOUGH.as_secs() {
                    asked = ear.audio.len();
                    asking = Some(Box::pin(ask(http, ear.audio.clone(), ear.rate, || self.closed(run))));
                }
            }
            if !recording && !due && asking.is_none() {
                return Phase::NoMatch;
            }
            let at = start + ASK_AT[next.min(ASK_AT.len() - 1)];
            tokio::select! {
                h = async { asking.as_mut().unwrap().await }, if asking.is_some() => {
                    asking = None;
                    match h {
                        Ok(Some(h)) => {
                            stop.store(true, Ordering::Relaxed);
                            self.set(run, Phase::Identifying);
                            let found = tokio::select! {
                                found = crate::ytm::recognized_song(api, &h.title, &h.artist) => found,
                                _ = self.closing(run) => return Phase::NoMatch,
                            };
                            return match found {
                                Ok(Ok(t)) => Phase::Found(Box::new(t)),
                                Ok(Err(top)) => Phase::Unsure(h, top.map(Box::new)),
                                Err(e) => {
                                    log::warn!("recognize: YouTube Music search: {e:#}");
                                    Phase::Unsure(h, None)
                                }
                            };
                        }
                        Ok(None) => {}
                        Err(p) => return Phase::Failed(p),
                    }
                }
                e = rx.recv(), if recording => match e {
                    Some(Event::Rate(r)) => ear.rate = r,
                    Some(Event::Audio(a)) if start.elapsed() >= skip => {
                        if ear.hear(a) {
                            self.set(run, Phase::Listening { hearing: true });
                        }
                        if let Some(bars) = ear.bars() {
                            self.set_bars(run, bars);
                        }
                    }
                    Some(Event::Audio(_)) => {}
                    Some(Event::Failed(p)) => return Phase::Failed(p),
                    None => return Phase::Failed(Problem::DeviceChanged),
                },
                _ = tokio::time::sleep_until(at.into()), if recording => {
                    (next, due) = (next + 1, true);
                    if next == ASK_AT.len() {
                        stop.store(true, Ordering::Relaxed);
                        self.set(run, Phase::Identifying);
                    }
                }
                // To see it closed.
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
            // WASAPI sends nothing at all while nothing plays; the others, silence.
            if !ear.heard && start.elapsed() >= skip + QUIET {
                return Phase::Failed(Problem::NothingPlaying);
            }
        }
    }
}

/// What Shazam makes of `audio`. Not sent should the run be closed while it's fingerprinted.
async fn ask(http: &reqwest::Client, audio: Vec<f32>, rate: u32, closed: impl Fn() -> bool) -> Result<Option<Heard>, Problem> {
    let sig = tokio::task::spawn_blocking(move || Signature::from_samples(&to_16k(audio, rate))).await.map_err(|_| Problem::NoAnswer)?;
    if closed() {
        return Ok(None);
    }
    shazam::identify(http, &sig).await
}

/// What it has recorded.
#[derive(Default)]
struct Ear {
    rate: u32,
    audio: Vec<f32>,
    /// Anything but silence.
    heard: bool,
    meter: meter::Meter,
    /// How much audio the meter last saw.
    metered: usize,
}

impl Ear {
    /// Adds `a`: true when it's the first that isn't silence.
    fn hear(&mut self, a: Vec<f32>) -> bool {
        let first = !self.heard && a.iter().any(|s| s.abs() > 1e-4);
        self.heard |= first;
        self.audio.extend(a);
        first
    }

    /// The meter's bars, about 30 times a second.
    fn bars(&mut self) -> Option<[f32; BARS]> {
        if self.audio.len() < self.metered + self.rate as usize / 30 {
            return None;
        }
        self.metered = self.audio.len();
        self.meter.bars(&self.audio, self.rate)
    }
}

struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Mono samples at any rate, at 16 kHz, through rodio.
fn to_16k(samples: Vec<f32>, rate: u32) -> Vec<f32> {
    let Some(r) = std::num::NonZero::new(rate).filter(|r| r.get() != SAMPLE_RATE) else { return samples };
    let src = rodio::buffer::SamplesBuffer::new(std::num::NonZero::<u16>::MIN, r, samples);
    rodio::source::UniformSourceIterator::new(src, std::num::NonZero::<u16>::MIN, std::num::NonZero::new(SAMPLE_RATE).unwrap()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fingerprints 12 s from the middle of a downloaded song and asks Shazam. Run alone:
    /// `cargo test --bin ytm-rs -- --ignored recognize::tests::shazam_live --exact --nocapture`
    /// (`YTM_RECOGNIZE_FILE` picks the file; else the first song in downloads.json).
    #[tokio::test]
    #[ignore]
    async fn shazam_live() {
        let path = std::env::var("YTM_RECOGNIZE_FILE").unwrap_or_else(|_| {
            let list = std::env::var("APPDATA").unwrap() + r"\ytm-rs\data\downloads.json";
            let v: serde_json::Value = serde_json::from_slice(&std::fs::read(list).unwrap()).unwrap();
            v.as_object().unwrap().values().next().unwrap()["path"].as_str().unwrap().to_owned()
        });
        let decoder = rodio::Decoder::try_from(std::fs::File::open(&path).unwrap()).unwrap();
        let mono: Vec<f32> = rodio::source::UniformSourceIterator::new(decoder, std::num::NonZero::<u16>::MIN, std::num::NonZero::new(SAMPLE_RATE).unwrap()).collect();
        let mid = mono.len() / 2;
        let clip = &mono[mid.saturating_sub(6 * 16_000)..(mid + 6 * 16_000).min(mono.len())];
        let sig = Signature::from_samples(clip);
        println!("{path}: {} samples, {} peaks", clip.len(), sig.peaks());
        let heard = shazam::identify(&crate::proxy::http(), &sig).await.unwrap();
        println!("Shazam heard: {heard:?}");
        let h = heard.unwrap();
        let api = crate::ytmapi::Api::new(&std::env::temp_dir().join("ytm-rs-recognize-test"));
        let song = crate::ytm::recognized_song(&api, &h.title, &h.artist).await.unwrap();
        // Ok: a sure match; Err: the top hit, if any.
        println!("YouTube Music: {:?}", song.map(|t| (t.id, t.title)).map_err(|top| top.map(|t| (t.id, t.title))));
    }

    /// Recognizes what plays the way the app does, off the recording: `YTM_RECOGNIZE_FILE`
    /// played out loud (quietly, from its middle), or else whatever is playing. Run alone:
    /// `cargo test --bin ytm-rs -- --ignored recognize::tests::listen_live --exact --nocapture`
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn listen_live() {
        use rodio::Source;
        let sink = std::env::var("YTM_RECOGNIZE_FILE").ok().map(|path| {
            let sink = rodio::DeviceSinkBuilder::open_default_sink().unwrap();
            let song = rodio::Decoder::try_from(std::fs::File::open(&path).unwrap()).unwrap();
            let half = song.total_duration().unwrap_or_default() / 2;
            sink.mixer().add(song.skip_duration(half).amplify(0.3));
            sink
        });
        let r = Recognizer::default();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let stop = Arc::new(AtomicBool::new(false));
        let recording = stop.clone();
        std::thread::spawn(move || capture::record(&recording, &tx));
        let api = crate::ytmapi::Api::new(&std::env::temp_dir().join("ytm-rs-recognize-test"));
        let t = Instant::now();
        let phase = r.listen(0, rx, Duration::ZERO, &stop, &api, &crate::proxy::http()).await;
        stop.store(true, Ordering::Relaxed);
        drop(sink);
        match phase {
            Phase::Found(t2) => println!("after {:?}: YouTube Music {} {} by {}", t.elapsed(), t2.id, t2.title, t2.artist_line()),
            Phase::Unsure(h, _) => println!("after {:?}: Shazam heard {h:?}; no sure YouTube Music song", t.elapsed()),
            Phase::Failed(p) => panic!("{p:?}"),
            _ => panic!("no match"),
        }
    }

    #[test]
    fn resamples_to_16k() {
        let out = to_16k(vec![0.25; 48_000], 48_000);
        assert!((out.len() as i64 - 16_000).abs() < 10, "{}", out.len());
        assert_eq!(to_16k(vec![0.5; 100], 16_000).len(), 100);
    }
}
