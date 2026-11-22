//! AirPlay 2's buffered audio, for speakers that take classic AirPlay's handshake and then
//! play nothing (Sonos). Worked out from AirplayKit's notes and sender, which first got a
//! Sonos to play this way (MIT, github.com/phranck/AirplayKit).
//!
//! After pairing, over the encrypted RTSP connection: a session SETUP (a binary plist),
//! an event channel the speaker talks to us on (opened before RECORD, and answered, or it
//! hangs up within half a minute), RECORD, a stream SETUP for buffered audio (type 103),
//! SETPEERS naming us, at which the speaker starts sending us its PTP clock, and an anchor
//! (SETRATEANCHORTIME) saying which instant on that clock the first frame plays at. The
//! audio goes over TCP in encrypted blocks, `LATENCY` ahead of when it plays.
//!
//! Every step of that can be off and still be accepted, the speaker reporting it's
//! playing and playing silence: the anchor has to name the speaker's own clock, in that
//! clock's own time, and point far enough into the future.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use chacha20poly1305::ChaCha20Poly1305;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use crossbeam_channel::Receiver;
use plist::{Dictionary, Value};

use super::{Ctl, FRAMES, Failure, LATENCY, MAX_MESSAGE, RATE, Rtsp, Speaker, TIMEOUT, Worker, pair, random};

/// How soon after an anchor its first frame plays: the rest of `LATENCY` is sent at once,
/// so starting, seeking or playing again is heard in this long rather than in `LATENCY`.
const LEAD: f64 = 0.5;

/// How long the speaker gets to start sending its clock after SETPEERS.
pub const CLOCK_WAIT: Duration = Duration::from_secs(12);
const PLIST: &str = "application/x-apple-binary-plist";
/// ALAC, 44.1 kHz, 16-bit stereo (bit 18 of the format bits): in the stream SETUP and in
/// every block, whose SSRC says what it holds.
const ALAC: u32 = 0x40000;
const BUFFERED: i64 = 103;

pub struct Buffered {
    rtsp: Rtsp,
    events: Events,
    clock: Clock,
    data: TcpStream,
    /// The audio's key, which the stream SETUP hands the speaker as `shk`.
    key: [u8; 32],
    stream_id: Option<i64>,
    /// The RTP time and the speaker's clock time the last anchor tied together.
    anchor: (u32, f64),
    identity: u64,
    /// The speaker's own volume when connected to, 0 to 1.
    pub(super) volume: Option<f32>,
}

impl Buffered {
    pub fn connect(speaker: &Speaker) -> anyhow::Result<Self> {
        let sid = random() as u32;
        let mut rtsp = Rtsp::open(speaker, sid)?;
        let local = rtsp.tcp.local_addr()?.ip();
        let remote = speaker.addr.ip();
        rtsp.pair()?;
        let shared = rtsp.shared.clone().ok_or_else(|| anyhow!("pairing gave no key"))?;
        // Not derived: the pairing secret's first 32 bytes, which is what an Apple TV uses
        // whatever it's told, so the one choice that suits both kinds of speaker.
        let key: [u8; 32] = shared.get(..32).and_then(|k| k.try_into().ok()).ok_or_else(|| anyhow!("pairing key too short"))?;

        // Asked before the session SETUP, which is refused without it.
        rtsp.request("GET", Some("/info"), &[], None)?;
        let session = Value::Dictionary(session_setup(local, &uuid(), random() as i64 & i64::MAX));
        let res = rtsp.request("SETUP", None, &[], Some((PLIST, &binary(&session)?)))?;
        let answer = parse(&res.body)?;
        let event_port = port(&answer, "eventPort").ok_or_else(|| anyhow!("SETUP gave no event port"))?;

        // Before RECORD, or RECORD gets 500 and the speaker never plays.
        let events = Events::open(SocketAddr::new(remote, event_port), &shared)?;
        // Some speakers answer 500 even when all's well, so whatever it says, on.
        if let Err(e) = rtsp.request("RECORD", None, &[], None) {
            log::debug!("airplay: RECORD: {e:#}");
        }

        let res = rtsp.request("SETUP", None, &[], Some((PLIST, &binary(&stream_setup(&key, sid))?)))?;
        let answer = parse(&res.body)?;
        let stream = answer
            .get("streams")
            .and_then(Value::as_array)
            .and_then(|s| s.first())
            .and_then(Value::as_dictionary)
            .ok_or_else(|| anyhow!("the stream SETUP gave no stream"))?;
        let data_port = port(stream, "dataPort").ok_or_else(|| anyhow!("the stream SETUP gave no data port"))?;
        let stream_id = stream.get("streamID").and_then(Value::as_signed_integer);
        log::debug!("airplay: data port {data_port}, buffer {:?}", stream.get("audioBufferSize").and_then(Value::as_signed_integer));

        // Listening before SETPEERS: the speaker starts on its clock the moment it hears
        // where to send it.
        let clock = Clock::listen(remote)?;
        let peers = Value::Array(vec![Value::String(local.to_string())]);
        rtsp.request("SETPEERS", None, &[], Some(("/peer-list-changed", &binary(&peers)?)))?;
        let identity = clock.wait(CLOCK_WAIT)?;
        log::debug!("airplay: the speaker's clock is {identity:016x}");

        let data = TcpStream::connect_timeout(&SocketAddr::new(remote, data_port), TIMEOUT).context("couldn't reach its data port")?;
        let _ = data.set_nodelay(true);
        data.set_write_timeout(Some(Duration::from_secs(5)))?;

        let volume = rtsp.volume();
        let mut session = Self { rtsp, events, clock, data, key, stream_id, anchor: (0, 0.0), identity, volume };
        session.set_anchor(0)?;
        Ok(session)
    }

    /// Where the speaker is, on its clock: the RTP time it's playing.
    fn playing(&self) -> Option<u32> {
        let (_, now) = self.clock.now()?;
        let (rtp, at) = self.anchor;
        Some(rtp.wrapping_add(((now - at).max(0.0) * f64::from(RATE)) as u32))
    }

    /// Ties RTP time `rtp` to `LEAD` from now on the speaker's clock, playing.
    fn set_anchor(&mut self, rtp: u32) -> anyhow::Result<()> {
        let (identity, now) = self.clock.now().ok_or_else(|| anyhow!("lost the speaker's clock"))?;
        if identity != self.identity {
            bail!("the speaker's clock changed");
        }
        let at = now + LEAD;
        let secs = at.floor();
        // The fraction as a 64-bit binary fraction; over a half, the sign bit, as it should.
        let frac = ((at - secs) * 2f64.powi(64)) as u64;
        let body = Dictionary::from_iter([
            ("networkTimeFlags", Value::from(0i64)),
            ("networkTimeFrac", Value::from(frac as i64)),
            ("networkTimeSecs", Value::from(secs as i64)),
            ("networkTimeTimelineID", Value::from(identity as i64)),
            // Odd plays, even pauses.
            ("rate", Value::from(1i64)),
            ("rtpTime", Value::from(i64::from(rtp))),
        ]);
        self.rtsp.request("SETRATEANCHORTIME", None, &[], Some((PLIST, &binary(&Value::Dictionary(body))?)))?;
        self.anchor = (rtp, at);
        Ok(())
    }

    pub(super) fn run(mut self, mut frames: impl Iterator<Item = Vec<u8>>, stop: &AtomicBool, ctl: &Receiver<Ctl>, lost: impl Fn()) {
        let cipher = ChaCha20Poly1305::new(&self.key.into());
        let mut seq: u32 = 0;
        let mut rtp: u32 = 0;
        let mut nonce: u64 = 0;
        // Paused, where the speaker had got to: nothing's sent till it plays again.
        let mut paused: Option<u32> = None;
        let gone = loop {
            if stop.load(Ordering::Relaxed) {
                break false;
            }
            if self.events.lost.load(Ordering::Relaxed) {
                log::info!("airplay: the speaker hung up");
                break true;
            }
            let mut failed = false;
            let mut volume = None;
            for c in ctl.try_iter() {
                let done = match c {
                    // Drops what it holds up to here. That ends its timeline too (it stays
                    // where it had got to, playing nothing, whatever it's sent), so the next
                    // block gets a fresh anchor; paused, when it plays again.
                    Ctl::Flush => {
                        let body = Dictionary::from_iter([("flushUntilSeq", i64::from(seq)), ("flushUntilTS", i64::from(rtp))]);
                        let flushed = binary(&Value::Dictionary(body)).and_then(|b| self.rtsp.request("FLUSHBUFFERED", None, &[], Some((PLIST, &b))));
                        match paused {
                            Some(_) => {
                                paused = Some(rtp);
                                flushed.map(drop)
                            }
                            None => flushed.and_then(|_| self.set_anchor(rtp)),
                        }
                    }
                    // Rate 0: it stops where it is, keeping what it holds.
                    Ctl::Pause if paused.is_none() => {
                        paused = self.playing();
                        let body = Dictionary::from_iter([("rate", 0i64)]);
                        binary(&Value::Dictionary(body)).and_then(|b| self.rtsp.request("SETRATEANCHORTIME", None, &[], Some((PLIST, &b)))).map(drop)
                    }
                    Ctl::Resume => match paused.take() {
                        Some(at) => self.set_anchor(at),
                        None => Ok(()),
                    },
                    Ctl::Pause => Ok(()),
                    // A drag sends one a frame: only the last is passed on.
                    Ctl::Volume(v) => {
                        volume = Some(v);
                        Ok(())
                    }
                };
                if let Err(e) = done {
                    log::warn!("airplay: {e:#}");
                    failed = true;
                    break;
                }
            }
            if failed {
                break true;
            }
            // A speaker with its volume fixed plays on all the same.
            if let Some(v) = volume
                && let Err(e) = self.rtsp.set_volume(v)
            {
                log::warn!("airplay: the speaker didn't take the volume: {e:#}");
            }
            if paused.is_some() {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }

            // Paced by the speaker's clock, each block going `LATENCY` before it plays, in
            // short sleeps so stopping (and the engine waiting on its mixer) isn't held up.
            let Some((_, now)) = self.clock.now() else {
                log::warn!("airplay: lost the speaker's clock");
                break true;
            };
            let plays = self.anchor.1 + f64::from(rtp.wrapping_sub(self.anchor.0)) / f64::from(RATE);
            let wait = plays - LATENCY.as_secs_f64() - now;
            if wait > 0.0 {
                std::thread::sleep(Duration::from_secs_f64(wait.min(0.02)));
                continue;
            }
            if -wait > LATENCY.as_secs_f64() {
                // Behind by more than the lead (the computer slept): everything from here
                // would arrive late and be dropped, so it's timed afresh.
                log::info!("airplay: fell {:.1} s behind, anchoring again", -wait);
                if let Err(e) = self.set_anchor(rtp) {
                    log::warn!("airplay: {e:#}");
                    break true;
                }
            }
            let block = block(&cipher, seq, rtp, nonce, &frames.next().unwrap_or_default());
            if let Err(e) = self.data.write_all(&block) {
                log::warn!("airplay: sending failed: {e}");
                break true;
            }
            seq = (seq + 1) & 0x7f_ffff;
            rtp = rtp.wrapping_add(FRAMES as u32);
            nonce += 1;
        };
        // Hands the mixer back (its bridge is in `frames`) before the goodbye, which the
        // engine doesn't wait for.
        drop(frames);
        if gone {
            lost();
        } else {
            let _ = self.rtsp.tcp.set_read_timeout(Some(Duration::from_secs(1)));
            if let Some(id) = self.stream_id {
                let stream = Dictionary::from_iter([("streamID", id), ("type", BUFFERED)]);
                let streams = Dictionary::from_iter([("streams", Value::Array(vec![stream.into()]))]);
                if let Ok(b) = binary(&Value::Dictionary(streams)) {
                    let _ = self.rtsp.request("TEARDOWN", None, &[], Some((PLIST, &b)));
                }
            }
            if let Ok(b) = binary(&Value::Dictionary(Dictionary::new())) {
                let _ = self.rtsp.request("TEARDOWN", None, &[], Some((PLIST, &b)));
            }
        }
    }
}

/// One block of buffered audio: its length (counting itself), the marker and a 23-bit
/// sequence number, the RTP time, the format as SSRC, then the ALAC frame encrypted with
/// those last two as associated data, the tag, and the nonce's counter.
fn block(cipher: &ChaCha20Poly1305, seq: u32, rtp: u32, nonce: u64, frame: &[u8]) -> Vec<u8> {
    let mut header = [0u8; 12];
    header[..4].copy_from_slice(&(0x8000_0000 | (seq & 0x7f_ffff)).to_be_bytes());
    header[4..8].copy_from_slice(&rtp.to_be_bytes());
    header[8..].copy_from_slice(&ALAC.to_be_bytes());
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&nonce.to_le_bytes());
    let sealed = cipher.encrypt(&n.into(), Payload { msg: frame, aad: &header[4..] }).expect("encrypting can't fail");
    let len = 2 + header.len() + sealed.len() + 8;
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(&(len as u16).to_be_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(&sealed);
    out.extend_from_slice(&nonce.to_le_bytes());
    out
}

/// Who we are, and that the speaker's PTP clock is the one to go by.
fn session_setup(local: IpAddr, uuid: &str, clock_id: i64) -> Dictionary {
    let peer = || {
        Value::Dictionary(Dictionary::from_iter([
            ("Addresses", Value::Array(vec![Value::String(local.to_string())])),
            ("ID", Value::String(uuid.to_owned())),
            ("ClockID", Value::from(clock_id)),
            ("DeviceType", Value::from(0i64)),
            ("SupportsClockPortMatchingOverride", Value::Boolean(true)),
        ]))
    };
    // A made-up, locally administered address: nothing checks it.
    let mac = format!("02:00:{:02X}:{:02X}:{:02X}:{:02X}", clock_id >> 24 & 0xff, clock_id >> 16 & 0xff, clock_id >> 8 & 0xff, clock_id & 0xff);
    Dictionary::from_iter([
        ("deviceID", Value::String(mac.clone())),
        ("macAddress", Value::String(mac)),
        ("sessionUUID", Value::String(uuid.to_owned())),
        ("groupUUID", Value::String(uuid.to_owned())),
        ("timingProtocol", Value::String("PTP".into())),
        ("timingPeerInfo", peer()),
        ("timingPeerList", Value::Array(vec![peer()])),
        ("timingPort", Value::from(0i64)),
        ("isMultiSelectAirPlay", Value::Boolean(false)),
        ("groupContainsGroupLeader", Value::Boolean(false)),
        ("senderSupportsRelay", Value::Boolean(false)),
        ("statsCollectionEnabled", Value::Boolean(false)),
        ("model", Value::String("ytm-rs".into())),
        ("name", Value::String("ytm-rs".into())),
        ("osName", Value::String(std::env::consts::OS.into())),
        ("osVersion", Value::String(env!("CARGO_PKG_VERSION").into())),
        ("osBuildVersion", Value::String("1".into())),
        ("sourceVersion", Value::String("550.10".into())),
    ])
}

/// One stream of buffered ALAC, keyed with `key`, tied to the session by its id: a Sonos
/// refuses the stream (400) without `streamConnectionID`.
fn stream_setup(key: &[u8; 32], sid: u32) -> Value {
    let stream = Dictionary::from_iter([
        ("type", Value::from(BUFFERED)),
        ("ct", Value::from(2i64)),
        ("audioFormat", Value::from(i64::from(ALAC))),
        ("spf", Value::from(FRAMES as i64)),
        ("sr", Value::from(i64::from(RATE))),
        ("shk", Value::Data(key.to_vec())),
        ("isMedia", Value::Boolean(true)),
        ("audioMode", Value::String("default".into())),
        ("latencyMin", Value::from(11_025i64)),
        ("latencyMax", Value::from(88_200i64)),
        ("supportsDynamicStreamID", Value::Boolean(false)),
        ("streamConnectionID", Value::from(i64::from(sid))),
    ]);
    Value::Dictionary(Dictionary::from_iter([("streams", Value::Array(vec![stream.into()]))]))
}

fn binary(value: &Value) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    value.to_writer_binary(&mut out)?;
    Ok(out)
}

fn parse(body: &[u8]) -> anyhow::Result<Dictionary> {
    match Value::from_reader(std::io::Cursor::new(body)).context("an answer that isn't a plist")? {
        Value::Dictionary(d) => Ok(d),
        _ => bail!("an answer that isn't a dictionary"),
    }
}

fn port(d: &Dictionary, key: &str) -> Option<u16> {
    d.get(key).and_then(Value::as_signed_integer).and_then(|p| u16::try_from(p).ok()).filter(|p| *p != 0)
}

fn uuid() -> String {
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&random().to_le_bytes());
    b[8..].copy_from_slice(&random().to_le_bytes());
    b[6] = b[6] & 0x0f | 0x40;
    b[8] = b[8] & 0x3f | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02X}")).collect();
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

/// The connection the speaker sends us requests on, answered as briefly as can be: more
/// in the answer (even `Content-Length: 0`) and it plays silence.
struct Events {
    lost: Arc<AtomicBool>,
    _worker: Worker,
}

impl Events {
    fn open(addr: SocketAddr, shared: &[u8]) -> anyhow::Result<Self> {
        let mut tcp = TcpStream::connect_timeout(&addr, TIMEOUT).context("couldn't reach its event port")?;
        tcp.set_read_timeout(Some(Duration::from_millis(250)))?;
        tcp.set_write_timeout(Some(TIMEOUT))?;
        let mut cipher = pair::Cipher::events(shared);
        let lost = Arc::new(AtomicBool::new(false));
        let worker = Worker::spawn("airplay-events", {
            let lost = lost.clone();
            move |stop| {
                let (mut raw, mut plain) = (Vec::new(), Vec::new());
                let mut chunk = [0u8; 4096];
                while !stop.load(Ordering::Relaxed) {
                    match tcp.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => raw.extend_from_slice(&chunk[..n]),
                        Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
                        Err(_) => break,
                    }
                    if cipher.open(&mut raw, &mut plain).is_err() {
                        log::warn!("airplay: an event didn't decrypt");
                        break;
                    }
                    let mut failed = false;
                    while let Some((head, len)) = request(&plain) {
                        log::debug!("airplay: event {:?}", head.lines().next().unwrap_or(""));
                        let cseq = head.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("cseq")).map(|(_, v)| v.trim().to_owned()));
                        plain.drain(..len);
                        let mut reply = "RTSP/1.0 200 OK\r\nServer: AirTunes/550.10\r\n".to_owned();
                        if let Some(c) = cseq {
                            reply += &format!("CSeq: {c}\r\n");
                        }
                        reply += "\r\n";
                        if tcp.write_all(&cipher.seal(reply.as_bytes())).is_err() {
                            failed = true;
                            break;
                        }
                    }
                    if failed {
                        break;
                    }
                    // A request that never ends (or a huge one) isn't held on to.
                    if raw.len() + plain.len() > MAX_MESSAGE {
                        log::warn!("airplay: an event is too long");
                        break;
                    }
                }
                if !stop.load(Ordering::Relaxed) {
                    lost.store(true, Ordering::Relaxed);
                }
            }
        })?;
        Ok(Self { lost, _worker: worker })
    }
}

/// A whole request at the front of `plain`: its head, and its length with the body.
fn request(plain: &[u8]) -> Option<(String, usize)> {
    let end = plain.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    let head = String::from_utf8_lossy(&plain[..end]).into_owned();
    let body: usize = head.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.trim().parse().ok())).unwrap_or(0);
    let len = end.checked_add(body)?;
    (plain.len() >= len).then_some((head, len))
}

/// The speaker's PTP clock, as it announces it to us (UDP 319 and 320): its identity, and
/// what it read when we last heard. Followed for as long as the session lasts, so the
/// audio keeps pace with the speaker rather than drifting off this computer's clock.
struct Clock {
    state: Arc<Mutex<ClockState>>,
    _worker: Worker,
}

#[derive(Default)]
struct ClockState {
    identity: Option<u64>,
    /// Its time in seconds, and when we heard it.
    reading: Option<(f64, Instant)>,
}

impl Clock {
    fn listen(from: IpAddr) -> anyhow::Result<Self> {
        let bind = |port| {
            let s = UdpSocket::bind(("0.0.0.0", port)).map_err(|e| anyhow::Error::new(e).context(format!("port {port}")).context(Failure::ClockPorts))?;
            s.set_read_timeout(Some(Duration::from_millis(20)))?;
            anyhow::Ok(s)
        };
        let sockets = [bind(319)?, bind(320)?];
        let state = Arc::new(Mutex::new(ClockState::default()));
        let worker = Worker::spawn("airplay-clock", {
            let state = state.clone();
            move |stop| {
                let mut buf = [0u8; 512];
                while !stop.load(Ordering::Relaxed) {
                    for s in &sockets {
                        if let Ok((n, sender)) = s.recv_from(&mut buf)
                            && sender.ip() == from
                        {
                            heard(&mut state.lock().unwrap(), &buf[..n], Instant::now());
                        }
                    }
                }
            }
        })?;
        Ok(Self { state, _worker: worker })
    }

    /// Waits for the speaker to say who its clock is and what it reads.
    fn wait(&self, timeout: Duration) -> anyhow::Result<u64> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some((identity, _)) = self.now() {
                return Ok(identity);
            }
            if Instant::now() > deadline {
                bail!(Failure::NoClock);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Its identity, and what it reads now.
    fn now(&self) -> Option<(u64, f64)> {
        let state = self.state.lock().unwrap();
        let (time, at) = state.reading?;
        Some((state.identity?, time + at.elapsed().as_secs_f64()))
    }
}

/// Takes the clock's identity from an Announce and its time from a Follow_Up.
fn heard(state: &mut ClockState, msg: &[u8], at: Instant) {
    if msg.len() < 34 {
        return;
    }
    let u64_at = |o: usize| msg[o..o + 8].iter().fold(0u64, |v, b| v << 8 | u64::from(*b));
    match msg[0] & 0x0f {
        0x0b if msg.len() >= 64 => {
            let identity = u64_at(53);
            if state.identity != Some(identity) {
                // Another clock: the time held is the old one's.
                state.identity = Some(identity);
                state.reading = None;
            }
        }
        0x08 if msg.len() >= 44
            && state.identity == Some(u64_at(20)) => {
                let secs = msg[34..40].iter().fold(0u64, |v, b| v << 8 | u64::from(*b));
                let nanos = u32::from_be_bytes([msg[40], msg[41], msg[42], msg[43]]);
                state.reading = Some((secs as f64 + f64::from(nanos) / 1e9, at));
            }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_is_whole_once_its_body_is_in() {
        let event = b"POST /command RTSP/1.0\r\nCSeq: 2\r\nContent-Length: 3\r\n\r\nabc";
        assert_eq!(request(&event[..event.len() - 1]), None);
        assert_eq!(request(event).map(|(_, len)| len), Some(event.len()));
        // A length past anything that could come is never whole (and the reader's cap
        // lets it go), rather than overflowing.
        let huge = format!("POST /command RTSP/1.0\r\nContent-Length: {}\r\n\r\n", usize::MAX);
        assert_eq!(request(huge.as_bytes()), None);
    }

    #[test]
    fn a_block_opens_with_the_speakers_view_of_it() {
        let key = [7u8; 32];
        let cipher = ChaCha20Poly1305::new(&key.into());
        let frame = [1u8, 2, 3, 4, 5];
        let b = block(&cipher, 0x80_0005, 352, 9, &frame);
        assert_eq!(u16::from_be_bytes([b[0], b[1]]) as usize, b.len());
        // Marker set, sequence wrapped to 23 bits.
        assert_eq!(u32::from_be_bytes(b[2..6].try_into().unwrap()), 0x8000_0005);
        assert_eq!(u32::from_be_bytes(b[6..10].try_into().unwrap()), 352);
        assert_eq!(u32::from_be_bytes(b[10..14].try_into().unwrap()), ALAC);
        // As shairport-sync reads it: the last 8 bytes the nonce, the 16 before the tag.
        let nonce = u64::from_le_bytes(b[b.len() - 8..].try_into().unwrap());
        assert_eq!(nonce, 9);
        let mut n = [0u8; 12];
        n[4..].copy_from_slice(&nonce.to_le_bytes());
        let opened = cipher.decrypt(&n.into(), Payload { msg: &b[14..b.len() - 8], aad: &b[6..14] }).unwrap();
        assert_eq!(opened, frame);
    }

    #[test]
    fn the_clock_comes_from_its_announce_and_follow_up() {
        let mut state = ClockState::default();
        let mut announce = vec![0u8; 64];
        announce[0] = 0x1b;
        announce[53..61].copy_from_slice(&0x3842_0bff_fef4_7e38u64.to_be_bytes());
        let mut follow_up = vec![0u8; 44];
        follow_up[0] = 0x18;
        follow_up[20..28].copy_from_slice(&0x3842_0bff_fef4_7e38u64.to_be_bytes());
        follow_up[34..40].copy_from_slice(&[0, 0, 0, 1, 0x62, 0x98]); // 90776 s
        follow_up[40..44].copy_from_slice(&500_000_000u32.to_be_bytes());
        let at = Instant::now();
        // A Follow_Up before any Announce says nothing.
        heard(&mut state, &follow_up, at);
        assert!(state.reading.is_none());
        heard(&mut state, &announce, at);
        heard(&mut state, &follow_up, at);
        assert_eq!(state.identity, Some(0x3842_0bff_fef4_7e38));
        assert_eq!(state.reading.unwrap().0, 90776.5);
        // Another clock's Follow_Up is someone else's time.
        follow_up[27] ^= 1;
        follow_up[39] = 0;
        heard(&mut state, &follow_up, at);
        assert_eq!(state.reading.unwrap().0, 90776.5);
    }

    #[test]
    fn whole_event_requests_are_taken_one_at_a_time() {
        let two = b"POST /command RTSP/1.0\r\nCSeq: 4\r\nContent-Length: 3\r\n\r\nabcGET /x RTSP/1.0\r\nCSeq: 5\r\n\r\n";
        let (head, len) = request(two).unwrap();
        assert!(head.starts_with("POST /command"));
        assert_eq!(&two[len..len + 3], b"GET");
        assert!(request(&two[..40]).is_none());
    }

    #[test]
    fn the_setups_carry_what_a_sonos_wont_do_without() {
        let s = stream_setup(&[1; 32], 77);
        let stream = s.as_dictionary().unwrap()["streams"].as_array().unwrap()[0].as_dictionary().unwrap().clone();
        assert_eq!(stream["type"].as_signed_integer(), Some(103));
        assert_eq!(stream["streamConnectionID"].as_signed_integer(), Some(77));
        assert_eq!(stream["shk"].as_data().map(<[u8]>::len), Some(32));
        let bytes = binary(&s).unwrap();
        assert!(bytes.starts_with(b"bplist00"));
        let session = session_setup("192.0.2.6".parse().unwrap(), &uuid(), 42);
        assert_eq!(session["timingProtocol"].as_string(), Some("PTP"));
        assert_eq!(uuid().len(), 36);
    }
}
