//! What the computer is playing, as it plays: mono samples off the default output.
//! Windows: WASAPI loopback, which cpal opens for an output device's input. macOS 14.6
//! on: a Core Audio tap, likewise through cpal. Linux: the default output's monitor, from
//! `parec`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::Problem;

pub enum Event {
    /// The samples' rate, before any.
    Rate(u32),
    Audio(Vec<f32>),
    Failed(Problem),
}

pub type Tx = tokio::sync::mpsc::UnboundedSender<Event>;

/// Records until `stop`. Blocks: run it on a thread of its own.
pub fn record(stop: &AtomicBool, tx: &Tx) {
    if let Err(p) = run(stop, tx) {
        let _ = tx.send(Event::Failed(p));
    }
}

/// How often it looks at `stop`.
const TICK: Duration = Duration::from_millis(50);

#[cfg(any(windows, target_os = "macos"))]
fn run(stop: &AtomicBool, tx: &Tx) -> Result<(), Problem> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{FromSample, SampleFormat, SizedSample};

    let device = cpal::default_host().default_output_device().ok_or(Problem::NoDevice)?;
    // cpal taps an output only when it has no input of its own (a headset's would be its
    // microphone), and its loopback needs macOS 14.6 (CoreAudio's tap, from 14.2, is linked
    // weakly for before).
    #[cfg(target_os = "macos")]
    if !macos_at_least(14, 6) {
        return Err(Problem::Unsupported);
    }
    #[cfg(target_os = "macos")]
    if device.supports_input() {
        return Err(Problem::HeadsetOutput);
    }
    let config = device.default_output_config().map_err(|e| failed(e, Problem::NoDevice))?;
    // Canceled while the device took its time to answer.
    if stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    let channels = config.channels() as usize;
    let _ = tx.send(Event::Rate(config.sample_rate()));

    fn build<T: SizedSample>(device: &cpal::Device, config: cpal::StreamConfig, channels: usize, tx: &Tx) -> Result<cpal::Stream, cpal::Error>
    where
        f32: FromSample<T>,
    {
        let (tx, lost) = (tx.clone(), tx.clone());
        let audio = move |data: &[T], _: &_| {
            let _ = tx.send(Event::Audio(mono(data, channels)));
        };
        // The device went away or changed format: this recording is over. Not a glitch, nor
        // the stream following a new default output.
        let err = move |e: cpal::Error| {
            use cpal::ErrorKind::{DeviceChanged, RealtimeDenied, Xrun};
            if !matches!(e.kind(), Xrun | RealtimeDenied | DeviceChanged) {
                log::warn!("recognize: capture: {e}");
                let _ = lost.send(Event::Failed(Problem::DeviceChanged));
            }
        };
        device.build_input_stream(config, audio, err, None)
    }
    let c = config.config();
    let stream = match config.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, c, channels, tx),
        SampleFormat::I16 => build::<i16>(&device, c, channels, tx),
        SampleFormat::I32 => build::<i32>(&device, c, channels, tx),
        f => {
            log::warn!("recognize: can't record {f}");
            return Err(Problem::Unsupported);
        }
    }
    // No permission to record what plays, on macOS.
    .map_err(|e| failed(e, Problem::Unsupported))?;
    stream.play().map_err(|e| failed(e, Problem::Unsupported))?;
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(TICK);
    }
    Ok(())
}

/// The macOS version is `major.minor` or later.
#[cfg(target_os = "macos")]
fn macos_at_least(major: u32, minor: u32) -> bool {
    let out = std::process::Command::new("sw_vers").arg("-productVersion").output().ok();
    let v = out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()).unwrap_or_default();
    let mut parts = v.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0)) >= (major, minor)
}

/// `parec`, PulseAudio's, which PipeWire's Pulse server answers too: no library linked,
/// so the app starts without either. It mixes down and resamples: the audio comes as the
/// fingerprint wants it.
#[cfg(target_os = "linux")]
fn run(stop: &AtomicBool, tx: &Tx) -> Result<(), Problem> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let rate = super::signature::SAMPLE_RATE;
    // Not installed (pulseaudio-utils; libpulse on Arch).
    let mut child = Command::new("parec")
        .args(["--device=@DEFAULT_MONITOR@", "--raw", "--format=float32le", "--channels=1", "--latency-msec=50", "--client-name=ytm-rs"])
        .arg(format!("--rate={rate}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| failed(format!("parec: {e}"), Problem::Unsupported))?;
    let mut out = child.stdout.take().ok_or(Problem::Unsupported)?;
    let _ = tx.send(Event::Rate(rate));
    let audio = tx.clone();
    // Reads until parec is gone: 50 ms at a time.
    let reader = std::thread::spawn(move || {
        let mut buf = vec![0u8; rate as usize / 20 * 4];
        while out.read_exact(&mut buf).is_ok() {
            let _ = audio.send(Event::Audio(buf.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()));
        }
    });
    let done = loop {
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }
        // No sound server, or no output to listen to.
        if let Ok(Some(status)) = child.try_wait() {
            let mut why = String::new();
            let _ = child.stderr.take().map(|mut e| e.read_to_string(&mut why));
            break Err(failed(format!("parec {status}: {}", why.trim()), Problem::NoDevice));
        }
        std::thread::sleep(TICK);
    };
    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
    done
}
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn run(_stop: &AtomicBool, _tx: &Tx) -> Result<(), Problem> {
    Err(Problem::Unsupported)
}

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
fn failed(e: impl std::fmt::Display, p: Problem) -> Problem {
    log::warn!("recognize: capture: {e}");
    p
}

/// Interleaved frames as mono: the front left and right, which is where stereo music plays
/// on a surround output too (rodio's mono would keep the left alone).
#[cfg(any(windows, target_os = "macos"))]
fn mono<T: Copy>(data: &[T], channels: usize) -> Vec<f32>
where
    f32: cpal::FromSample<T>,
{
    use cpal::FromSample;
    match channels {
        0 => Vec::new(),
        1 => data.iter().map(|&s| f32::from_sample_(s)).collect(),
        n => data.chunks_exact(n).map(|f| (f32::from_sample_(f[0]) + f32::from_sample_(f[1])) / 2.0).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records a second of what plays (silence will do, but not on Windows, where WASAPI
    /// sends nothing for it), and stops when asked. Run alone:
    /// `cargo test --bin ytm-rs -- --ignored recognize::capture::tests::records --nocapture`
    #[test]
    #[ignore]
    fn records() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let recording = stop.clone();
        let t = std::thread::spawn(move || record(&recording, &tx));
        std::thread::sleep(Duration::from_secs(1));
        stop.store(true, Ordering::Relaxed);
        t.join().unwrap();
        let (mut rate, mut samples) = (0, 0);
        while let Ok(e) = rx.try_recv() {
            match e {
                Event::Rate(r) => rate = r,
                Event::Audio(a) => samples += a.len(),
                Event::Failed(p) => panic!("{p:?}"),
            }
        }
        println!("{samples} samples at {rate} Hz");
        assert!(rate > 0 && samples as u32 > rate / 2, "{samples} samples at {rate} Hz");
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn mono_is_the_front_pair() {
        // 5.1: front left, front right, center, LFE, back left, back right.
        let frames = [0.2f32, 0.4, 0.9, 0.9, 0.9, 0.9, -0.2, 0.0, 0.0, 0.0, 0.0, 0.0];
        let m = mono(&frames, 6);
        assert_eq!(m.len(), 2);
        assert!((m[0] - 0.3).abs() < 1e-6 && (m[1] + 0.1).abs() < 1e-6);
        assert_eq!(mono(&[0.5f32, -0.5], 1), vec![0.5, -0.5]);
        assert!((mono(&[i16::MIN, i16::MIN], 2)[0] + 1.0).abs() < 1e-6);
    }
}
