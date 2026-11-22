//! AirPlay speakers as an output device. Classic AirPlay (RAOP), which AirPort Express,
//! shairport-sync and most AirPlay speakers and receivers take; and for AirPlay 2 speakers
//! that play buffered audio (Sonos), AirPlay 2's own way (`ap2`), after pairing the
//! transient way (`pair`), which needs no PIN. Speakers that want a PIN or a lasting
//! pairing (an Apple TV for its home's people only) aren't offered.
//!
//! The audio engine hands a speaker the same mixed, equalized samples a sound card gets;
//! a thread of its own pulls them at the pace they play, packs them as uncompressed Apple
//! Lossless and sends them over RTP. The speaker plays each packet `LATENCY` after it's
//! sent, which `AudioStatus::latency` accounts for.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, anyhow, bail};
use crossbeam_channel::{Receiver, Sender, unbounded};

mod ap2;
mod pair;

/// Output device ids of AirPlay speakers start with this; sound cards' are cpal's.
pub const PREFIX: &str = "airplay:";

pub const RATE: u32 = 44_100;
/// Frames in a packet, as the SDP below tells the speaker.
const FRAMES: usize = 352;
/// How long after it's sent the speaker plays a packet: the 2 s AirPort Express keeps
/// whatever it's told, so everyone gets the same.
const LATENCY_FRAMES: u32 = 2 * RATE;
pub const LATENCY: Duration = Duration::from_secs(2);
/// Packets kept for the speaker to ask for again (about 8 s).
const HISTORY: usize = 1024;
/// Waiting on the speaker's answers, and on connecting to it.
const TIMEOUT: Duration = Duration::from_secs(3);
/// The most a message from the speaker (head and body) may be: its answers and events are
/// a few KB, so anything past this is a speaker gone wrong, not one to wait on.
const MAX_MESSAGE: usize = 1 << 20;

const SERVICE: &str = "_raop._tcp.local.";

/// A speaker found on the network.
#[derive(Clone, Debug, PartialEq)]
pub struct Speaker {
    /// `PREFIX` and its mDNS name: stable, as that starts with the speaker's MAC address.
    pub id: String,
    pub name: String,
    addr: SocketAddr,
    /// An AirPlay 2 speaker (it gives a pairing key, `pk`): pair before playing.
    pair: bool,
    /// It plays AirPlay 2's buffered audio (`ft` bit 40): the path Sonos renders.
    buffered: bool,
    /// It takes MFi-SAP (`et` has 4): an `/auth-setup` before ANNOUNCE.
    auth_setup: bool,
}

static SPEAKERS: LazyLock<Mutex<HashMap<String, Speaker>>> = LazyLock::new(Default::default);
static BROWSING: AtomicBool = AtomicBool::new(false);
/// Settings › Network › Allow local network scan. Off, nothing here listens on the network,
/// so Windows never asks to let the app through its firewall.
static ALLOWED: AtomicBool = AtomicBool::new(false);
/// The mDNS listener while it runs, to stop it with the setting.
static DAEMON: Mutex<Option<mdns_sd::ServiceDaemon>> = Mutex::new(None);

/// Allows looking for speakers, or stops looking and forgets the ones found.
pub fn set_allowed(on: bool) {
    ALLOWED.store(on, Ordering::Relaxed);
    if !on {
        if let Some(d) = DAEMON.lock().unwrap().take() {
            let _ = d.shutdown();
        }
        SPEAKERS.lock().unwrap().clear();
        BROWSING.store(false, Ordering::Relaxed);
    }
}

/// Starts looking for speakers, once: they come and go in `speakers()` from then on.
/// Only once AirPlay's wanted (the output device list opened, or a speaker picked), and
/// the local network scan's allowed.
pub fn discover() {
    if !ALLOWED.load(Ordering::Relaxed) || BROWSING.swap(true, Ordering::Relaxed) {
        return;
    }
    let spawned = std::thread::Builder::new().name("airplay-mdns".into()).spawn(|| {
        let daemon = match mdns_sd::ServiceDaemon::new() {
            Ok(d) => d,
            Err(e) => {
                log::warn!("airplay: can't look for speakers: {e}");
                BROWSING.store(false, Ordering::Relaxed);
                return;
            }
        };
        *DAEMON.lock().unwrap() = Some(daemon.clone());
        let events = match daemon.browse(SERVICE) {
            Ok(r) => r,
            Err(e) => {
                log::warn!("airplay: can't look for speakers: {e}");
                return;
            }
        };
        while let Ok(event) = events.recv() {
            match event {
                mdns_sd::ServiceEvent::ServiceResolved(s) => {
                    let instance = mdns_sd::instance_name_unescaped(&s.fullname);
                    let props = &s.txt_properties;
                    let txt = |k| props.get_property_val_str(k);
                    let found = usable(txt("et"), txt("cn"), txt("pw"), txt("sf"))
                        .then(|| s.get_addresses_v4().into_iter().min())
                        .flatten()
                        .map(|ip| Speaker {
                            id: format!("{PREFIX}{}", s.fullname),
                            name: speaker_name(&instance).to_owned(),
                            addr: SocketAddr::new(IpAddr::V4(ip), s.port),
                            pair: txt("pk").is_some(),
                            buffered: features(txt("ft").or(txt("features"))) & (1 << 40) != 0,
                            auth_setup: txt("et").is_some_and(|et| et.split(',').any(|v| v.trim() == "4")),
                        });
                    let mut all = SPEAKERS.lock().unwrap();
                    // Turned off meanwhile: the speakers were just forgotten.
                    if !ALLOWED.load(Ordering::Relaxed) {
                        break;
                    }
                    match found {
                        Some(sp) => {
                            if all.get(&s.fullname) != Some(&sp) {
                                log::info!("airplay: found {:?} at {}", sp.name, sp.addr);
                            }
                            all.insert(s.fullname.clone(), sp);
                        }
                        None => {
                            log::info!("airplay: {instance:?} wants what isn't supported (et {:?}, cn {:?}, pw {:?}, sf {:?})", txt("et"), txt("cn"), txt("pw"), txt("sf"));
                            all.remove(&s.fullname);
                        }
                    }
                }
                mdns_sd::ServiceEvent::ServiceRemoved(_, fullname) => {
                    SPEAKERS.lock().unwrap().remove(&fullname);
                }
                _ => {}
            }
        }
        // Already, if the setting went off; otherwise its socket closes here.
        let _ = daemon.shutdown();
    });
    if let Err(e) = spawned {
        log::warn!("airplay: can't look for speakers: {e}");
        BROWSING.store(false, Ordering::Relaxed);
    }
}

/// The speakers on the network, by name.
pub fn speakers() -> Vec<Speaker> {
    let mut all: Vec<Speaker> = SPEAKERS.lock().unwrap().values().cloned().collect();
    all.sort_by_key(|s| s.name.to_lowercase());
    all
}

/// The speaker with this output device id, if it's on the network.
pub fn find(id: &str) -> Option<Speaker> {
    SPEAKERS.lock().unwrap().get(id.strip_prefix(PREFIX)?).cloned()
}

/// "AABBCCDDEEFF@Kitchen" → "Kitchen".
fn speaker_name(instance: &str) -> &str {
    instance.split_once('@').map_or(instance, |(_, name)| name)
}

/// Whether a speaker takes what this sends, by its TXT record: unencrypted audio (`et`
/// has 0), Apple Lossless (`cn` has 1), no password, and no pairing beyond the transient
/// kind: its status flags (`sf`) don't ask for a PIN (bit 3) or a one-time pairing (bit 9),
/// as an Apple TV that lets only its home's people AirPlay to it does.
fn usable(et: Option<&str>, cn: Option<&str>, pw: Option<&str>, sf: Option<&str>) -> bool {
    let has = |list: Option<&str>, want: &str| list.is_none_or(|l| l.split(',').any(|v| v.trim() == want));
    let flags = sf.and_then(hex).unwrap_or(0);
    has(et, "0") && has(cn, "1") && pw.is_none_or(|p| !p.eq_ignore_ascii_case("true")) && flags & (1 << 3 | 1 << 9) == 0
}

/// A speaker's feature bits, from "0xLOW,0xHIGH" (or one number).
fn features(ft: Option<&str>) -> u64 {
    let Some(ft) = ft else { return 0 };
    match ft.split_once(',') {
        Some((low, high)) => hex(low).unwrap_or(0) | hex(high).unwrap_or(0) << 32,
        None => hex(ft).unwrap_or(0),
    }
}

/// A TXT record's hex number, "0x" or not.
fn hex(s: &str) -> Option<u64> {
    let s = s.trim();
    u64::from_str_radix(s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s), 16).ok()
}

/// For ids and sequence numbers, nothing that has to be secret.
fn random() -> u64 {
    getrandom::u64().unwrap_or_default()
}

/// Why a speaker wouldn't play: told to the listener, in their language.
#[derive(Debug)]
pub enum Failure {
    Unreachable,
    Busy,
    /// Wants a password, or a pairing other than the transient kind.
    NeedsPairing,
    /// Turned down the transient pairing (an Apple TV for its home's people only).
    PairingRefused,
    /// Didn't send its PTP clock, which AirPlay 2's audio is timed by.
    NoClock,
    /// Couldn't listen on the PTP ports (319 and 320) for that clock.
    ClockPorts,
}

impl Failure {
    pub fn message(&self) -> String {
        match self {
            Self::Unreachable => tr!("airplay-unreachable"),
            Self::Busy => tr!("airplay-busy"),
            Self::NeedsPairing => tr!("airplay-needs-pairing"),
            Self::PairingRefused => tr!("airplay-pairing-refused"),
            Self::NoClock => tr!("airplay-no-clock"),
            // Below 1024, so Linux keeps them for root (or a process with
            // CAP_NET_BIND_SERVICE). Elsewhere, something else has them.
            Self::ClockPorts if cfg!(target_os = "linux") => tr!("airplay-clock-ports-linux"),
            Self::ClockPorts => tr!("airplay-clock-ports"),
        }
        .to_owned()
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Failure {}

/// What to tell the listener about `e`, from the `Failure` in it if any.
pub fn describe(e: &anyhow::Error) -> String {
    // Through `.context(Failure::…)` too, which `chain()` holds wrapped.
    e.downcast_ref::<Failure>().map_or_else(|| tr!("airplay-failed").to_owned(), Failure::message)
}

/// A thread with a stop flag, which dropping it raises and then waits for the thread.
struct Worker {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    fn spawn(name: &str, f: impl FnOnce(&AtomicBool) + Send + 'static) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::Builder::new().name(name.into()).spawn({
            let stop = stop.clone();
            move || f(&stop)
        })?;
        Ok(Self { stop, thread: Some(thread) })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Seconds since 1900, as a 32.32 fixed-point number.
fn ntp_now() -> u64 {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = now.as_secs() + 2_208_988_800;
    let frac = (u64::from(now.subsec_nanos()) << 32) / 1_000_000_000;
    (secs << 32) | frac
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// The speaker's RTSP connection, which sets up the stream and controls it.
struct Rtsp {
    tcp: TcpStream,
    cseq: u32,
    url: String,
    session: Option<String>,
    client: String,
    /// Our `Active-Remote`, which AirPlay 2 speakers expect beside the `DACP-ID`.
    remote: String,
    /// What came in that isn't an answer yet.
    buf: Vec<u8>,
    /// What came in still encrypted (not a whole block yet).
    raw: Vec<u8>,
    /// Paired, everything both ways is encrypted.
    cipher: Option<pair::Cipher>,
    /// The pairing's shared secret, which the event channel's and the audio's keys come from.
    shared: Option<Vec<u8>>,
}

impl Rtsp {
    /// Connects to the speaker; requests go to `rtsp://<us>/<sid>`.
    fn open(speaker: &Speaker, sid: u32) -> anyhow::Result<Self> {
        let tcp = TcpStream::connect_timeout(&speaker.addr, TIMEOUT).map_err(|e| anyhow::Error::new(e).context(Failure::Unreachable))?;
        tcp.set_read_timeout(Some(TIMEOUT))?;
        tcp.set_write_timeout(Some(TIMEOUT))?;
        let _ = tcp.set_nodelay(true);
        let local = tcp.local_addr()?.ip();
        Ok(Self {
            tcp,
            cseq: 0,
            url: format!("rtsp://{local}/{sid}"),
            session: None,
            client: format!("{:016X}", random()),
            remote: (random() as u32).to_string(),
            buf: Vec::new(),
            raw: Vec::new(),
            cipher: None,
            shared: None,
        })
    }

    fn send(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        match &mut self.cipher {
            Some(c) => {
                let sealed = c.seal(bytes);
                self.tcp.write_all(&sealed)
            }
            None => self.tcp.write_all(bytes),
        }
    }

    /// Reads what's there onto `buf`. 0 when the speaker hung up.
    fn fill(&mut self) -> std::io::Result<usize> {
        let mut chunk = [0u8; 4096];
        let n = self.tcp.read(&mut chunk)?;
        match &mut self.cipher {
            Some(c) => {
                self.raw.extend_from_slice(&chunk[..n]);
                c.open(&mut self.raw, &mut self.buf).map_err(|()| std::io::Error::new(std::io::ErrorKind::InvalidData, "a block from the speaker didn't decrypt"))?;
            }
            None => self.buf.extend_from_slice(&chunk[..n]),
        }
        Ok(n)
    }

    /// Transient pairing (see `pair`): two POSTs, after which the connection's encrypted.
    fn pair(&mut self) -> anyhow::Result<()> {
        use pair::*;
        // 4: transient. (3, HomeKit's lasting pairing, gets as far as the proof and wants a
        // real PIN; and a speaker paired that way doesn't encrypt with these keys.)
        let hkp = [("X-Apple-HKP", "4".to_owned())];
        let m1 = tlv_encode(&[(METHOD, &[0]), (STATE, &[1]), (FLAGS, &[TRANSIENT])]);
        let res = self.request("POST", Some("/pair-setup"), &hkp, Some(("application/octet-stream", &m1)))?;
        let items = tlv_decode(&res.body);
        if let Some(e) = items.get(&ERROR) {
            return Err(anyhow!("pairing error {e:?}").context(Failure::PairingRefused));
        }
        let (Some(salt), Some(b_pub)) = (items.get(&SALT), items.get(&PUBLIC_KEY)) else {
            bail!("its pairing answer was missing a part");
        };
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).map_err(|e| anyhow!("no randomness: {e}"))?;
        let proofs = srp_client(salt, b_pub, &secret).ok_or_else(|| anyhow!("its pairing key was no good"))?;
        let m3 = tlv_encode(&[(STATE, &[3]), (PUBLIC_KEY, &proofs.a_pub), (PROOF, &proofs.m1)]);
        let res = self.request("POST", Some("/pair-setup"), &hkp, Some(("application/octet-stream", &m3)))?;
        let items = tlv_decode(&res.body);
        log::debug!("airplay: pairing M4: error {:?}, proof matches {}", items.get(&ERROR), items.get(&PROOF) == Some(&proofs.m2));
        if items.contains_key(&ERROR) || items.get(&PROOF) != Some(&proofs.m2) {
            bail!(Failure::PairingRefused);
        }
        self.cipher = Some(Cipher::new(&proofs.key));
        self.shared = Some(proofs.key);
        log::debug!("airplay: paired");
        Ok(())
    }

    /// The speaker's own volume, 0 to 1 (its -30 to 0 dB), if it says.
    fn volume(&mut self) -> Option<f32> {
        let res = self.request("GET_PARAMETER", None, &[], Some(("text/parameters", b"volume\r\n"))).ok()?;
        let db: f32 = String::from_utf8_lossy(&res.body).trim().strip_prefix("volume:")?.trim().parse().ok()?;
        Some(level(db))
    }

    /// Sets the speaker's own volume, 0 to 1.
    fn set_volume(&mut self, v: f32) -> anyhow::Result<()> {
        let body = format!("volume: {:.6}\r\n", decibels(v));
        self.request("SET_PARAMETER", None, &[], Some(("text/parameters", body.as_bytes())))?;
        Ok(())
    }

    /// Sends a request and waits for the answer, which has to be 200 OK.
    fn request(&mut self, method: &str, uri: Option<&str>, headers: &[(&str, String)], body: Option<(&str, &[u8])>) -> anyhow::Result<Response> {
        self.cseq += 1;
        // Apple TVs answer a sender that doesn't call itself "AirPlay/…" (or "iTunes/…") with 403.
        let mut req = format!(
            "{method} {} RTSP/1.0\r\nCSeq: {}\r\nUser-Agent: AirPlay/665.13.1 (ytm-rs/{})\r\nClient-Instance: {}\r\nDACP-ID: {}\r\nActive-Remote: {}\r\n",
            uri.unwrap_or(&self.url),
            self.cseq,
            env!("CARGO_PKG_VERSION"),
            self.client,
            self.client,
            self.remote
        );
        if let Some(s) = &self.session {
            req += &format!("Session: {s}\r\n");
        }
        for (k, v) in headers {
            req += &format!("{k}: {v}\r\n");
        }
        if let Some((kind, body)) = body {
            req += &format!("Content-Type: {kind}\r\nContent-Length: {}\r\n", body.len());
        }
        req += "\r\n";
        let mut out = req.into_bytes();
        if let Some((_, body)) = body {
            out.extend_from_slice(body);
        }
        self.send(&out).with_context(|| method.to_owned())?;
        let res = self.response().with_context(|| method.to_owned())?;
        log::debug!("airplay: {method} {} → {} {:?}", uri.unwrap_or(""), res.status, res.headers);
        match res.status {
            200 => Ok(res),
            401 | 403 | 470 if self.cipher.is_some() || method == "POST" => bail!(Failure::PairingRefused),
            401 | 403 | 470 => bail!(Failure::NeedsPairing),
            453 => bail!(Failure::Busy),
            s => bail!("{method} failed ({s})"),
        }
    }

    fn response(&mut self) -> anyhow::Result<Response> {
        let head_end = loop {
            if let Some(i) = self.buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            if self.buf.len() > MAX_MESSAGE {
                bail!("the speaker's answer is too long");
            }
            if self.fill()? == 0 {
                bail!("the speaker hung up");
            }
        };
        let head = String::from_utf8_lossy(&self.buf[..head_end]).into_owned();
        let mut lines = head.split("\r\n");
        let status = lines
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| anyhow!("not an RTSP answer"))?;
        let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned())).collect();
        let mut res = Response { status, headers, body: Vec::new() };
        let len: usize = res.header("Content-Length").and_then(|l| l.parse().ok()).unwrap_or(0);
        if head_end.saturating_add(len) > MAX_MESSAGE {
            bail!("the speaker's answer is too long");
        }
        while self.buf.len() < head_end + len {
            if self.fill()? == 0 {
                bail!("the speaker hung up");
            }
        }
        res.body = self.buf[head_end..head_end + len].to_vec();
        self.buf.drain(..head_end + len);
        Ok(res)
    }
}

/// A classic AirPlay (RAOP) session: ALAC over RTP, timed by NTP and sync packets.
pub struct Classic {
    rtsp: Rtsp,
    data: UdpSocket,
    control: UdpSocket,
    data_to: SocketAddr,
    control_to: SocketAddr,
    seq: u16,
    rtptime: u32,
    ssrc: u32,
    volume: Option<f32>,
    /// Answers the speaker's clock; stopped with the session (or a handshake that didn't
    /// get that far).
    _timing: Worker,
}

impl Classic {
    /// The handshake: blocks for as long as the speaker takes to answer (`TIMEOUT` at most
    /// for each step).
    fn connect(speaker: &Speaker) -> anyhow::Result<Self> {
        let sid = random() as u32;
        let mut rtsp = Rtsp::open(speaker, sid)?;
        let local = rtsp.tcp.local_addr()?.ip();
        let remote = speaker.addr.ip();
        // Paired first, so everything after is encrypted (and some speakers answer nothing else).
        if speaker.pair {
            rtsp.pair()?;
        }
        rtsp.request("OPTIONS", Some("*"), &[], None)?;
        // MFi-SAP (`et` 4), as PipeWire does it: a fixed Curve25519 key, its answer unused.
        if speaker.auth_setup && rtsp.cipher.is_none() {
            const AUTH_SETUP: [u8; 33] = [
                0x01, 0x59, 0x02, 0xed, 0xe9, 0x0d, 0x4e, 0xf2, 0xbd, 0x4c, 0xb6, 0x8a, 0x63, 0x30, 0x03, 0x82, 0x07, 0xa9, 0x4d, 0xbd, 0x50, 0xd8, 0xaa,
                0x46, 0x5b, 0x5d, 0x8c, 0x01, 0x2a, 0x0c, 0x7e, 0x1d, 0x4e,
            ];
            rtsp.request("POST", Some("/auth-setup"), &[], Some(("application/octet-stream", &AUTH_SETUP)))?;
        }

        let sdp = format!(
            "v=0\r\no=iTunes {sid} 0 IN IP4 {local}\r\ns=iTunes\r\nc=IN IP4 {remote}\r\nt=0 0\r\nm=audio 0 RTP/AVP 96\r\na=rtpmap:96 AppleLossless\r\na=fmtp:96 {FRAMES} 0 16 40 10 14 2 255 0 0 {RATE}\r\n"
        );
        rtsp.request("ANNOUNCE", None, &[], Some(("application/sdp", sdp.as_bytes())))?;

        let bind = || UdpSocket::bind(SocketAddr::new(if local.is_ipv4() { IpAddr::from([0, 0, 0, 0]) } else { IpAddr::from([0u16; 8]) }, 0));
        let (data, control, timing) = (bind()?, bind()?, bind()?);
        let transport = format!(
            "RTP/AVP/UDP;unicast;interleaved=0-1;mode=record;control_port={};timing_port={}",
            control.local_addr()?.port(),
            timing.local_addr()?.port()
        );
        // The speaker sets its clock by ours from here on, and some (Sonos) do before
        // they answer SETUP.
        let timing = Worker::spawn("airplay-timing", move |stop| answer_timing(timing, stop))?;
        let res = rtsp.request("SETUP", None, &[("Transport", transport)], None)?;
        let theirs = res.header("Transport").ok_or_else(|| anyhow!("SETUP gave no transport"))?;
        let port = |key: &str| transport_port(theirs, key);
        let data_to = SocketAddr::new(remote, port("server_port").ok_or_else(|| anyhow!("SETUP gave no server port"))?);
        let control_to = SocketAddr::new(remote, port("control_port").unwrap_or(data_to.port() + 1));
        rtsp.session = res.header("Session").map(|s| s.split(';').next().unwrap_or(s).trim().to_owned());

        let seq = random() as u16;
        let rtptime = random() as u32;
        let mut session = Self { rtsp, data, control, data_to, control_to, seq, rtptime, ssrc: random() as u32, volume: None, _timing: timing };
        session.rtsp.request("RECORD", None, &[("Range", "npt=0-".into()), ("RTP-Info", format!("seq={seq};rtptime={rtptime}"))], None)?;
        session.volume = session.rtsp.volume();
        Ok(session)
    }

    fn run(mut self, mut frames: impl Iterator<Item = Vec<u8>>, stop: &AtomicBool, ctl: &Receiver<Ctl>, lost: impl Fn()) {
        let _ = self.control.set_nonblocking(true);
        let _ = self.rtsp.tcp.set_nonblocking(true);
        let mut history: Vec<(u16, Vec<u8>)> = vec![(0, Vec::new()); HISTORY];
        // The first packet after starting or flushing is marked, as is the first sync.
        let mut first = true;
        let mut since_sync = u32::MAX;
        let mut start = Instant::now();
        let mut sent: u64 = 0;
        let mut checked = Instant::now();
        let mut buf = [0u8; 2048];
        let gone = loop {
            if stop.load(Ordering::Relaxed) {
                break false;
            }
            let mut failed = false;
            let mut volume = None;
            for c in ctl.try_iter() {
                match c {
                    Ctl::Flush => {
                        let info = format!("seq={};rtptime={}", self.seq, self.rtptime);
                        first = true;
                        since_sync = u32::MAX;
                        let _ = self.rtsp.tcp.set_nonblocking(false);
                        let flushed = self.rtsp.request("FLUSH", None, &[("RTP-Info", info)], None);
                        let _ = self.rtsp.tcp.set_nonblocking(true);
                        if let Err(e) = flushed {
                            log::warn!("airplay: {e:#}");
                            failed = true;
                            break;
                        }
                    }
                    // A drag sends one a frame: only the last is passed on.
                    Ctl::Volume(v) => volume = Some(v),
                    // Classic AirPlay can't: the engine flushes instead.
                    Ctl::Pause | Ctl::Resume => {}
                }
            }
            if failed {
                break true;
            }
            if let Some(v) = volume {
                let _ = self.rtsp.tcp.set_nonblocking(false);
                // A speaker with its volume fixed plays on all the same.
                if let Err(e) = self.rtsp.set_volume(v) {
                    log::warn!("airplay: the speaker didn't take the volume: {e:#}");
                }
                let _ = self.rtsp.tcp.set_nonblocking(true);
            }
            // Packets the speaker missed.
            while let Ok((n, from)) = self.control.recv_from(&mut buf) {
                log::trace!("airplay: control packet {:02x?}", &buf[..n.min(8)]);
                // The socket takes packets from anyone on the network: only the speaker's
                // are answered.
                if from.ip() == self.control_to.ip() && n >= 8 && buf[1] & 0x7f == 0x55 {
                    let seq = u16::from_be_bytes([buf[4], buf[5]]);
                    let count = u16::from_be_bytes([buf[6], buf[7]]);
                    for s in (0..count.min(HISTORY as u16)).map(|i| seq.wrapping_add(i)) {
                        let (at, packet) = &history[s as usize % HISTORY];
                        if *at == s && !packet.is_empty() {
                            let mut resend = vec![0x80, 0xd6, 0x00, 0x01];
                            resend.extend_from_slice(packet);
                            let _ = self.control.send_to(&resend, from);
                        }
                    }
                }
            }
            if since_sync >= RATE {
                let _ = self.control.send_to(&sync_packet(first, self.rtptime, ntp_now()), self.control_to);
                since_sync = 0;
            }
            let packet = rtp_packet(first, self.seq, self.rtptime, self.ssrc, &frames.next().unwrap_or_default());
            if let Err(e) = self.data.send_to(&packet, self.data_to) {
                log::warn!("airplay: sending failed: {e}");
                break true;
            }
            history[self.seq as usize % HISTORY] = (self.seq, packet);
            first = false;
            self.seq = self.seq.wrapping_add(1);
            self.rtptime = self.rtptime.wrapping_add(FRAMES as u32);
            since_sync = since_sync.saturating_add(FRAMES as u32);
            // The speaker hanging up (another device taking it over, or it turning off).
            if checked.elapsed() >= Duration::from_secs(1) {
                checked = Instant::now();
                match self.rtsp.fill() {
                    Ok(0) => {
                        log::info!("airplay: the speaker hung up");
                        break true;
                    }
                    // Nothing's asked of it now, so it's nothing to answer to; read all the
                    // same, so the encryption keeps count.
                    Ok(_) => self.rtsp.buf.clear(),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => {
                        log::info!("airplay: lost the speaker: {e}");
                        break true;
                    }
                }
            }
            // In real time: the music's own clock is the packets going out.
            sent += 1;
            let due = start + Duration::from_secs_f64(sent as f64 * FRAMES as f64 / RATE as f64);
            let now = Instant::now();
            if now > due + Duration::from_secs(1) {
                // Far behind (the computer slept): carry on from now, not in a burst.
                start = now;
                sent = 0;
            } else if due > now {
                std::thread::sleep(due - now);
            }
        };
        // Hands the mixer back (its bridge is in `frames`) before the goodbye, which the
        // engine doesn't wait for.
        drop(frames);
        if gone {
            lost();
        } else {
            let _ = self.rtsp.tcp.set_nonblocking(false);
            let _ = self.rtsp.tcp.set_read_timeout(Some(Duration::from_secs(1)));
            let _ = self.rtsp.request("TEARDOWN", None, &[], None);
        }
    }
}

/// A speaker that agreed to play: next, `play` gives it the music.
pub enum Session {
    Classic(Classic),
    Buffered(ap2::Buffered),
}

impl Session {
    /// The handshake: blocks for as long as the speaker takes to answer (`TIMEOUT` at most
    /// for each step, and up to `ap2::CLOCK_WAIT` for an AirPlay 2 speaker's clock).
    pub fn connect(speaker: &Speaker) -> anyhow::Result<Self> {
        // AirPlay 2 speakers that play buffered audio take classic AirPlay's handshake too,
        // and then play nothing (Sonos): they get their own.
        if speaker.pair && speaker.buffered {
            return Ok(Self::Buffered(ap2::Buffered::connect(speaker)?));
        }
        Ok(Self::Classic(Classic::connect(speaker)?))
    }

    /// The speaker's own volume when it was connected to, 0 to 1, if it said.
    pub fn volume(&self) -> Option<f32> {
        match self {
            Self::Classic(c) => c.volume,
            Self::Buffered(b) => b.volume,
        }
    }

    /// Streams `source` (any rate and channels: it's made 44.1 kHz stereo) until the
    /// returned `Stream` is dropped. `lost` is called once if the speaker goes away.
    pub fn play<S>(self, source: S, lost: impl Fn() + Send + 'static) -> anyhow::Result<Stream>
    where
        S: rodio::Source + Send + 'static,
    {
        let (ctl, ctls) = unbounded();
        let pauses = matches!(self, Self::Buffered(_));
        let worker = Worker::spawn("airplay", move |stop| {
            let frames = frames(source);
            match self {
                Self::Classic(c) => c.run(frames, stop, &ctls, lost),
                Self::Buffered(b) => b.run(frames, stop, &ctls, lost),
            }
        })?;
        Ok(Stream { ctl, pauses, worker })
    }
}

/// `source` made 44.1 kHz stereo, a packet's worth of ALAC at a time.
fn frames<S: rodio::Source>(source: S) -> impl Iterator<Item = Vec<u8>> {
    let mut source = rodio::source::UniformSourceIterator::new(source, std::num::NonZero::new(2).unwrap(), std::num::NonZero::new(RATE).unwrap());
    let mut pcm = vec![0i16; FRAMES * 2];
    std::iter::repeat_with(move || {
        for s in pcm.iter_mut() {
            *s = (source.next().unwrap_or(0.0).clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        }
        alac_frame(&pcm)
    })
}

/// What the engine asks of a speaker playing, carried out between packets.
enum Ctl {
    Flush,
    Pause,
    Resume,
    Volume(f32),
}

/// The speaker's volume, 0 to 1, from its -30 to 0 dB (-144 muted).
fn level(db: f32) -> f32 {
    ((db + 30.0) / 30.0).clamp(0.0, 1.0)
}

fn decibels(v: f32) -> f32 {
    if v <= 0.0 { -144.0 } else { v.min(1.0) * 30.0 - 30.0 }
}

/// A speaker playing. Dropping it stops the music there and lets the speaker go.
pub struct Stream {
    ctl: Sender<Ctl>,
    pauses: bool,
    worker: Worker,
}

impl Stream {
    /// Drops what the speaker has yet to play (up to `LATENCY` of it), so a pause, a seek
    /// or a new song is heard without waiting that out.
    pub fn flush(&self) {
        let _ = self.ctl.send(Ctl::Flush);
    }

    /// Whether it pauses itself (AirPlay 2): stopping at once and keeping what it has, so
    /// a pause needs no flush, and playing again picks up where it stopped.
    pub fn pauses(&self) -> bool {
        self.pauses
    }

    pub fn pause(&self) {
        let _ = self.ctl.send(Ctl::Pause);
    }

    pub fn resume(&self) {
        let _ = self.ctl.send(Ctl::Resume);
    }

    /// Sets the speaker's own volume, 0 to 1, which it changes at once.
    pub fn set_volume(&self, v: f32) {
        let _ = self.ctl.send(Ctl::Volume(v));
    }
}

impl Drop for Stream {
    /// Stops the music, without waiting for the goodbye to the speaker (seconds, if it's
    /// slow to answer): the engine has its mixer back as soon as the next packet's due.
    fn drop(&mut self) {
        self.worker.stop.store(true, Ordering::Relaxed);
        self.worker.thread.take();
    }
}

fn answer_timing(socket: UdpSocket, stop: &AtomicBool) {
    let _ = socket.set_read_timeout(Some(Duration::from_millis(250)));
    let mut buf = [0u8; 128];
    let mut asked = false;
    while !stop.load(Ordering::Relaxed) {
        if let Ok((n, from)) = socket.recv_from(&mut buf)
            && n >= 32
            && buf[1] & 0x7f == 0x52
        {
            if !std::mem::replace(&mut asked, true) {
                log::debug!("airplay: the speaker asks the time");
            }
            let _ = socket.send_to(&timing_reply(&buf[..32], ntp_now()), from);
        }
    }
}

/// The value of `key=` in a Transport header.
fn transport_port(transport: &str, key: &str) -> Option<u16> {
    transport.split(';').filter_map(|p| p.trim().split_once('=')).find(|(k, _)| *k == key).and_then(|(_, v)| v.parse().ok())
}

fn rtp_packet(first: bool, seq: u16, rtptime: u32, ssrc: u32, payload: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(12 + payload.len());
    p.extend_from_slice(&[0x80, if first { 0xe0 } else { 0x60 }]);
    p.extend_from_slice(&seq.to_be_bytes());
    p.extend_from_slice(&rtptime.to_be_bytes());
    p.extend_from_slice(&ssrc.to_be_bytes());
    p.extend_from_slice(payload);
    p
}

/// Tells the speaker the frame at `rtptime` plays `LATENCY` after `ntp`: it plays
/// `rtptime - LATENCY_FRAMES` then.
fn sync_packet(first: bool, rtptime: u32, ntp: u64) -> [u8; 20] {
    let mut p = [0u8; 20];
    p[..4].copy_from_slice(&[if first { 0x90 } else { 0x80 }, 0xd4, 0x00, 0x07]);
    p[4..8].copy_from_slice(&rtptime.wrapping_sub(LATENCY_FRAMES).to_be_bytes());
    p[8..16].copy_from_slice(&ntp.to_be_bytes());
    p[16..20].copy_from_slice(&rtptime.to_be_bytes());
    p
}

/// The answer to a timing request: its send time back, and ours (received and sent now).
fn timing_reply(request: &[u8], ntp: u64) -> [u8; 32] {
    let mut p = [0u8; 32];
    p[..4].copy_from_slice(&[0x80, 0xd3, 0x00, 0x07]);
    p[8..16].copy_from_slice(&request[24..32]);
    p[16..24].copy_from_slice(&ntp.to_be_bytes());
    p[24..32].copy_from_slice(&ntp.to_be_bytes());
    p
}

/// One packet's interleaved 16-bit stereo as an Apple Lossless frame stored uncompressed:
/// a channel pair element marked "escaped", the samples as they are, and an end tag.
fn alac_frame(pcm: &[i16]) -> Vec<u8> {
    let mut w = Bits { bytes: Vec::with_capacity(pcm.len() * 2 + 4), ..Bits::default() };
    w.put(1, 3); // a channel pair
    w.put(0, 4); // element instance
    w.put(0, 12); // unused
    w.put(0, 1); // a whole frame (the SDP's 352), so no sample count
    w.put(0, 2); // no bytes shifted out
    w.put(1, 1); // stored as is
    for s in pcm {
        w.put(*s as u16 as u32, 16);
    }
    w.put(7, 3); // the end
    w.finish()
}

/// Writes bits most significant first, a word at a time.
#[derive(Default)]
struct Bits {
    bytes: Vec<u8>,
    /// Bits not yet written out, in the low `held` bits.
    acc: u64,
    held: u8,
}

impl Bits {
    fn put(&mut self, value: u32, count: u8) {
        let mask = if count == 32 { u32::MAX } else { (1u32 << count) - 1 };
        self.acc = self.acc << count | u64::from(value & mask);
        self.held += count;
        while self.held >= 8 {
            self.held -= 8;
            self.bytes.push((self.acc >> self.held) as u8);
        }
    }

    /// What was written, the last byte padded with zeros.
    fn finish(mut self) -> Vec<u8> {
        if self.held > 0 {
            self.bytes.push((self.acc << (8 - self.held)) as u8);
        }
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_looks_at_the_network_until_its_allowed() {
        set_allowed(false);
        discover();
        assert!(!BROWSING.load(Ordering::Relaxed));
        assert!(DAEMON.lock().unwrap().is_none());
    }

    #[test]
    fn speakers_that_want_encryption_or_a_password_are_left_out() {
        assert!(usable(Some("0,3,5"), Some("0,1,2,3"), Some("false"), Some("0x4")));
        assert!(usable(None, None, None, None));
        assert!(!usable(Some("1"), Some("1"), None, None));
        assert!(!usable(Some("0"), Some("0,2"), None, None));
        assert!(!usable(Some("0"), Some("1"), Some("true"), None));
        // An Apple TV for its home only (one-time pairing), and one wanting a PIN.
        assert!(!usable(Some("0,3,5"), Some("0,1"), None, Some("0x644")));
        assert!(!usable(Some("0"), Some("1"), None, Some("0x8")));
    }

    #[test]
    fn the_slider_is_the_speakers_own_volume() {
        // Sonos shows -20 dB as 33 of 100: the scale is linear in dB, -30 to 0.
        assert_eq!(decibels(1.0), 0.0);
        assert!((decibels(1.0 / 3.0) + 20.0).abs() < 1e-4);
        assert_eq!(decibels(0.0), -144.0);
        assert_eq!(level(-144.0), 0.0);
        assert!((level(decibels(0.6)) - 0.6).abs() < 1e-6);
    }

    #[test]
    fn a_speakers_name_is_after_its_address() {
        assert_eq!(speaker_name("A1B2C3D4E5F6@Living Room"), "Living Room");
        assert_eq!(speaker_name("Kitchen"), "Kitchen");
    }

    #[test]
    fn the_transport_answer_gives_the_ports() {
        let t = "RTP/AVP/UDP;unicast;mode=record;server_port=6000;control_port=6001;timing_port=6002";
        assert_eq!(transport_port(t, "server_port"), Some(6000));
        assert_eq!(transport_port(t, "control_port"), Some(6001));
        assert_eq!(transport_port(t, "nope"), None);
    }

    #[test]
    fn an_uncompressed_alac_frame_has_its_header_samples_and_end_tag() {
        let frame = alac_frame(&[0x1234, -1]);
        // 23 header bits, 32 of samples, 3 of end tag: 58 bits, 8 bytes.
        assert_eq!(frame.len(), 8);
        // 001 0000 | 0000 0000 0000 | 0 00 1 | then the samples, one bit along.
        let mut bits = 0u64;
        for b in &frame {
            bits = bits << 8 | u64::from(*b);
        }
        let at = |from: u32, len: u32| (bits >> (64 - from - len)) & ((1 << len) - 1);
        assert_eq!(at(0, 3), 1);
        assert_eq!(at(3, 16), 0);
        assert_eq!(at(19, 4), 0b0001);
        assert_eq!(at(23, 16), 0x1234);
        assert_eq!(at(39, 16), 0xffff);
        assert_eq!(at(55, 3), 7);
        assert_eq!(at(58, 6), 0);
    }

    #[test]
    fn a_full_packet_is_the_size_receivers_expect() {
        // 23 + 352 × 2 × 16 + 3 bits.
        assert_eq!(alac_frame(&[0; FRAMES * 2]).len(), (23 + FRAMES * 32 + 3).div_ceil(8));
    }

    #[test]
    fn rtp_and_sync_headers() {
        let p = rtp_packet(true, 0x0102, 0x03040506, 0x0708090a, &[0xff]);
        assert_eq!(p, [0x80, 0xe0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 0xff]);
        assert_eq!(rtp_packet(false, 0, 0, 0, &[])[1], 0x60);
        let s = sync_packet(true, 100_000, 0x1122_3344_5566_7788);
        assert_eq!(&s[..4], &[0x90, 0xd4, 0, 7]);
        assert_eq!(u32::from_be_bytes(s[4..8].try_into().unwrap()), 100_000 - LATENCY_FRAMES);
        assert_eq!(u32::from_be_bytes(s[16..20].try_into().unwrap()), 100_000);
        assert_eq!(sync_packet(false, 0, 0)[0], 0x80);
    }

    #[test]
    fn a_timing_reply_hands_back_the_requests_send_time() {
        let mut req = [0u8; 32];
        req[..2].copy_from_slice(&[0x80, 0xd2]);
        req[24..32].copy_from_slice(&42u64.to_be_bytes());
        let r = timing_reply(&req, 7);
        assert_eq!(&r[..4], &[0x80, 0xd3, 0, 7]);
        assert_eq!(u64::from_be_bytes(r[8..16].try_into().unwrap()), 42);
        assert_eq!(u64::from_be_bytes(r[24..32].try_into().unwrap()), 7);
    }

    /// A pretend speaker on this computer: answers the handshake as shairport-sync does,
    /// and hands back the methods it was asked and the audio packets it got.
    fn fake_speaker() -> (Speaker, std::sync::mpsc::Receiver<String>, UdpSocket) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let audio = UdpSocket::bind("127.0.0.1:0").unwrap();
        let audio_port = audio.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut tcp, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                    match tcp.read(&mut chunk) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                    continue;
                };
                let head = String::from_utf8_lossy(&buf[..end]).into_owned();
                let len: usize = head.lines().find_map(|l| l.strip_prefix("Content-Length: ")).map_or(0, |v| v.parse().unwrap());
                while buf.len() < end + 4 + len {
                    let n = tcp.read(&mut chunk).unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                }
                buf.drain(..end + 4 + len);
                let method = head.split_whitespace().next().unwrap().to_owned();
                let cseq = head.lines().find_map(|l| l.strip_prefix("CSeq: ")).unwrap().to_owned();
                let extra = match method.as_str() {
                    "SETUP" => format!("Session: 1;timeout=60\r\nTransport: RTP/AVP/UDP;unicast;mode=record;server_port={audio_port};control_port={audio_port};timing_port=0\r\n"),
                    _ => String::new(),
                };
                // Noted before answering, so the sender never sees an answer before it's noted.
                let _ = tx.send(method.clone());
                tcp.write_all(format!("RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\nServer: AirTunes/366.0\r\n{extra}\r\n").as_bytes()).unwrap();
                if method == "TEARDOWN" {
                    return;
                }
            }
        });
        (Speaker { id: format!("{PREFIX}test"), name: "Test".into(), addr, pair: false, buffered: false, auth_setup: false }, rx, audio)
    }

    #[test]
    fn plays_on_a_speaker_and_lets_it_go() {
        let (speaker, methods, audio) = fake_speaker();
        let session = Session::connect(&speaker).unwrap();
        let said: Vec<String> = methods.try_iter().collect();
        assert_eq!(said, ["OPTIONS", "ANNOUNCE", "SETUP", "RECORD", "GET_PARAMETER"]);

        // A 48 kHz mono source, made 44.1 kHz stereo on the way.
        let source = rodio::buffer::SamplesBuffer::new(std::num::NonZero::new(1).unwrap(), std::num::NonZero::new(48_000).unwrap(), vec![0.5; 48_000]);
        let stream = session.play(source, || panic!("lost")).unwrap();
        audio.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut buf = [0u8; 4096];
        let mut packets = Vec::new();
        while packets.len() < 4 {
            let (n, _) = audio.recv_from(&mut buf).unwrap();
            // The sync packets come here too (the fake's control port is the audio one).
            if buf[1] & 0x7f == 0x60 {
                packets.push(buf[..n].to_vec());
            }
        }
        assert_eq!(packets[0][1], 0xe0, "the first packet is marked");
        assert_eq!(packets[1][1], 0x60);
        let seq = |p: &[u8]| u16::from_be_bytes([p[2], p[3]]);
        let time = |p: &[u8]| u32::from_be_bytes([p[4], p[5], p[6], p[7]]);
        assert_eq!(seq(&packets[1]), seq(&packets[0]).wrapping_add(1));
        assert_eq!(time(&packets[1]), time(&packets[0]).wrapping_add(FRAMES as u32));
        assert_eq!(packets[1].len(), 12 + (23 + FRAMES * 32 + 3).div_ceil(8));
        // Past the resampler's start, both channels at half scale: the sample after the
        // header is 23 bits in.
        let p = &packets[3][12..];
        let sample = ((u32::from(p[2]) & 1) << 15 | u32::from(p[3]) << 7 | u32::from(p[4]) >> 1) as u16 as i16;
        assert!((sample - i16::MAX / 2).abs() < 64, "{sample}");

        stream.flush();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !methods.try_iter().any(|m| m == "FLUSH") {
            assert!(Instant::now() < deadline, "no FLUSH");
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(stream);
        assert_eq!(methods.recv_timeout(Duration::from_secs(2)).unwrap(), "TEARDOWN");
    }

    #[test]
    fn a_speaker_that_wants_pairing_says_so() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut tcp, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = tcp.read(&mut buf);
            tcp.write_all(b"RTSP/1.0 403 Forbidden\r\nCSeq: 1\r\n\r\n").unwrap();
        });
        let e = Session::connect(&Speaker { id: String::new(), name: "HomePod".into(), addr, pair: false, buffered: false, auth_setup: false }).err().unwrap();
        assert!(matches!(e.downcast_ref::<Failure>(), Some(Failure::NeedsPairing)), "{e:#}");
        assert_eq!(describe(&e), Failure::NeedsPairing.message());
    }

    #[test]
    fn an_answer_too_long_to_hold_is_an_error() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut tcp, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = tcp.read(&mut buf);
            let _ = tcp.write_all(format!("RTSP/1.0 200 OK\r\nCSeq: 1\r\nContent-Length: {}\r\n\r\n", usize::MAX).as_bytes());
        });
        let e = Session::connect(&Speaker { id: String::new(), name: "Odd".into(), addr, pair: false, buffered: false, auth_setup: false }).err().unwrap();
        assert!(format!("{e:#}").contains("too long"), "{e:#}");
    }

    #[test]
    fn a_speaker_nobody_listens_on_is_unreachable() {
        // Attached as context (to the connection's error), unlike a bail!: found all the same.
        let addr = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let gone = Speaker { id: String::new(), name: "Gone".into(), addr, pair: false, buffered: false, auth_setup: false };
        let e = Session::connect(&gone).err().unwrap();
        assert!(matches!(e.downcast_ref::<Failure>(), Some(Failure::Unreachable)), "{e:#}");
        assert_eq!(describe(&e), Failure::Unreachable.message());
    }

    /// Dev check against a real speaker: YTM_AIRPLAY=<name> cargo test airplay_tone -- --ignored
    /// plays two seconds of a 440 Hz tone there.
    #[test]
    #[ignore]
    fn airplay_tone() {
        let _ = env_logger::builder().is_test(true).filter_module("ytm_rs::airplay", log::LevelFilter::Trace).try_init();
        let want = std::env::var("YTM_AIRPLAY").expect("YTM_AIRPLAY=<speaker name>");
        set_allowed(true);
        discover();
        let deadline = Instant::now() + Duration::from_secs(10);
        let speaker = loop {
            if let Some(s) = speakers().into_iter().find(|s| s.name.eq_ignore_ascii_case(&want)) {
                break s;
            }
            assert!(Instant::now() < deadline, "no speaker named {want:?}; found {:?}", speakers());
            std::thread::sleep(Duration::from_millis(200));
        };
        let session = Session::connect(&speaker).unwrap_or_else(|e| panic!("{e:#}"));
        let tone = rodio::source::SineWave::new(440.0);
        let stream = session.play(rodio::Source::amplify(tone, 0.3), || panic!("speaker lost")).unwrap();
        std::thread::sleep(Duration::from_secs(3));
        // An AirPlay 2 speaker pauses itself: a gap of 1.5 s, heard at once.
        stream.pause();
        std::thread::sleep(Duration::from_millis(1500));
        stream.resume();
        std::thread::sleep(Duration::from_secs(3));
        drop(stream);
        // The goodbye goes on after the drop, on the stream's own thread.
        std::thread::sleep(Duration::from_secs(2));
    }
}
