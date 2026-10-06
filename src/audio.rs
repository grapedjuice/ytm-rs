//! Audio engine: one thread owns the output device and a rodio `Player`.
//!
//! The UI talks to it through `AudioHandle`; status flows back through `AudioStatus`
//! (lock-free reads for the progress bar) plus a `Finished` event for auto-advance.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};
use rodio::{Decoder, DeviceSinkBuilder, Player};

use crate::stream::{self, Shared, StreamReader};

pub type TrackDecoder = Decoder<StreamReader>;

pub enum AudioCmd {
    /// A decoder is ready for playback generation `generation`; stale generations are dropped.
    Start { generation: u64, decoder: TrackDecoder, shared: Arc<Shared> },
    /// A seekable decoder, already positioned at `at`, replacing the current one.
    Replace { generation: u64, decoder: TrackDecoder, at: Duration },
    Pause,
    Resume,
    Stop,
    Seek(Duration),
    Volume(f32),
}

pub enum AudioEvent {
    Finished { generation: u64 },
    Error(String),
}

#[derive(Default)]
pub struct AudioStatus {
    pub position_ms: AtomicU64,
    pub playing: AtomicBool,
    /// Generation the UI most recently asked for; the engine ignores older decoders.
    pub wanted_gen: AtomicU64,
    /// f32 bits
    volume: AtomicU32,
}

impl AudioStatus {
    pub fn position(&self) -> Duration {
        Duration::from_millis(self.position_ms.load(Ordering::Relaxed))
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

    pub fn set_volume(&self, v: f32) {
        self.status.volume.store(v.to_bits(), Ordering::Relaxed);
        self.send(AudioCmd::Volume(v));
    }
}

pub fn spawn(volume: f32, events: Sender<AudioEvent>, wake: impl Fn() + Send + 'static) -> AudioHandle {
    let (tx, rx) = unbounded();
    let status = Arc::new(AudioStatus::default());
    status.volume.store(volume.to_bits(), Ordering::Relaxed);
    let st = status.clone();
    std::thread::Builder::new()
        .name("audio".into())
        .spawn({
            let tx = tx.clone();
            move || run(rx, tx, st, volume, events, wake)
        })
        .expect("spawn audio thread");
    AudioHandle { tx, status }
}

fn run(
    rx: Receiver<AudioCmd>,
    tx: Sender<AudioCmd>,
    status: Arc<AudioStatus>,
    volume: f32,
    events: Sender<AudioEvent>,
    wake: impl Fn(),
) {
    let mut sink = match DeviceSinkBuilder::open_default_sink() {
        Ok(s) => s,
        Err(e) => {
            let _ = events.send(AudioEvent::Error(format!("No audio output device: {e}")));
            wake();
            return;
        }
    };
    sink.log_on_drop(false);
    let player = Player::connect_new(sink.mixer());
    player.set_volume(volume);

    // Generation of the source currently in the player (0 = nothing loaded).
    let mut current: u64 = 0;
    let mut shared: Option<Arc<Shared>> = None;
    // The player counts position from when a source was appended; a swapped-in
    // seeked decoder starts at `offset`.
    let mut offset = Duration::ZERO;
    loop {
        // Poll faster while playing so the position stays fresh; idle otherwise.
        let timeout = if current != 0 { Duration::from_millis(100) } else { Duration::from_secs(3600) };
        match rx.recv_timeout(timeout) {
            Ok(AudioCmd::Start { generation, decoder, shared: s }) => {
                if generation != status.wanted_gen.load(Ordering::Relaxed) {
                    log::debug!("audio: dropping stale generation {generation}");
                    continue;
                }
                log::debug!("audio: start generation {generation}");
                player.clear();
                player.append(decoder);
                player.play();
                current = generation;
                shared = Some(s);
                offset = Duration::ZERO;
                status.playing.store(true, Ordering::Relaxed);
            }
            Ok(AudioCmd::Replace { generation, decoder, at }) => {
                if generation != current {
                    continue;
                }
                let paused = player.is_paused();
                player.clear();
                player.append(decoder);
                if !paused {
                    player.play();
                }
                offset = at;
            }
            Ok(AudioCmd::Pause) => {
                player.pause();
                status.playing.store(false, Ordering::Relaxed);
            }
            Ok(AudioCmd::Resume) => {
                if current != 0 {
                    player.play();
                    status.playing.store(true, Ordering::Relaxed);
                }
            }
            Ok(AudioCmd::Stop) => {
                player.clear();
                current = 0;
                shared = None;
                status.playing.store(false, Ordering::Relaxed);
                status.position_ms.store(0, Ordering::Relaxed);
            }
            Ok(AudioCmd::Seek(pos)) => {
                // The playing decoder was opened non-seekable for a fast start. Build a
                // seekable one over the same buffer off-thread (it needs every fragment
                // header, so it may wait for the download) and swap it in.
                if let Some(s) = shared.clone() {
                    let (tx, events, generation) = (tx.clone(), events.clone(), current);
                    std::thread::spawn(move || {
                        let res = stream::decoder(s, true).map_err(|e| e.to_string()).and_then(|mut d| {
                            rodio::Source::try_seek(&mut d, pos).map_err(|e| e.to_string())?;
                            Ok(d)
                        });
                        match res {
                            Ok(decoder) => {
                                let _ = tx.send(AudioCmd::Replace { generation, decoder, at: pos });
                            }
                            Err(e) => {
                                let _ = events.send(AudioEvent::Error(format!("Seek failed: {e}")));
                            }
                        }
                    });
                }
            }
            Ok(AudioCmd::Volume(v)) => player.set_volume(v),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
        }

        if current != 0 {
            status.position_ms.store((offset + player.get_pos()).as_millis() as u64, Ordering::Relaxed);
            if player.empty() {
                let _ = events.send(AudioEvent::Finished { generation: current });
                current = 0;
                shared = None;
                status.playing.store(false, Ordering::Relaxed);
                wake();
            }
        }
    }
}
