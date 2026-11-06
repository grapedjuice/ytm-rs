//! Music videos as moving pictures: YouTube's H.264 in fragmented MP4, decoded on the CPU
//! with OpenH264 and handed to the UI in time with the audio.
//!
//! The audio engine stays the clock: a thread here fetches one fragment at a time from
//! where the song is, decodes it, and keeps a few frames ready. The UI takes whichever frame
//! is due. Nothing runs unless the listener turned video on.

use std::collections::{BinaryHeap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use openh264::OpenH264API;
use openh264::decoder::{Decoder, DecoderConfig, Flush};
use openh264::formats::YUVSource;

use crate::audio::AudioStatus;
use crate::innertube::VideoStream;

/// Frames kept ready: under half a second, about 30 MB at 720p.
const READY: usize = 12;
/// Decoded this far behind the clock, a frame isn't worth converting: it would never show.
const LATE: f64 = 0.05;
/// A clock this far outside what's decoded is a seek.
const SEEK_BACK: f64 = 0.5;
const SEEK_AHEAD: f64 = 6.0;

// ---------------------------------------------------------------------------- MP4

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn be64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// The complete boxes in `b`: type, contents.
fn boxes(b: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
    let mut at = 0;
    std::iter::from_fn(move || {
        let size = be32(b, at)? as usize;
        let body = b.get(at + 8..at + size).filter(|_| size >= 8)?;
        let kind = &b[at + 4..at + 8];
        at += size;
        Some((kind, body))
    })
}

/// The first box along `path`.
fn find<'a>(b: &'a [u8], path: &[&str]) -> Option<&'a [u8]> {
    let (head, rest) = path.split_first()?;
    let body = boxes(b).find(|(kind, _)| *kind == head.as_bytes())?.1;
    if rest.is_empty() { Some(body) } else { find(body, rest) }
}

/// What the start of the file says about all of it.
#[derive(Debug, PartialEq)]
pub struct Track {
    /// The decoder's setup, as length-prefixed-free NAL units with start codes.
    params: Vec<u8>,
    /// Bytes in each NAL unit's length prefix.
    nal_len: usize,
    timescale: u32,
    default_duration: u32,
    /// The presentation time of the first sample, in timescale units.
    start: i64,
}

fn parse_track(moov: &[u8]) -> Option<Track> {
    let mdia = ["trak", "mdia"];
    let mdhd = find(moov, &[mdia[0], mdia[1], "mdhd"])?;
    let timescale = if mdhd[0] == 1 { be32(mdhd, 20)? } else { be32(mdhd, 12)? };
    let stsd = find(moov, &["trak", "mdia", "minf", "stbl", "stsd"])?;
    let at = stsd.windows(4).position(|w| w == b"avcC")? + 4;
    let c = stsd.get(at..)?;
    let nal_len = (*c.get(4)? & 3) as usize + 1;
    let mut params = Vec::new();
    let mut o = 6;
    let mut copy = |count: usize, o: &mut usize| -> Option<()> {
        for _ in 0..count {
            let n = u16::from_be_bytes([*c.get(*o)?, *c.get(*o + 1)?]) as usize;
            params.extend_from_slice(&[0, 0, 0, 1]);
            params.extend_from_slice(c.get(*o + 2..*o + 2 + n)?);
            *o += 2 + n;
        }
        Some(())
    };
    copy((*c.get(5)? & 31) as usize, &mut o)?;
    let pps = *c.get(o)? as usize;
    o += 1;
    copy(pps, &mut o)?;
    let default_duration = find(moov, &["mvex", "trex"]).and_then(|t| be32(t, 12)).unwrap_or(0);
    // With B-frames the first frame shows late; the edit list takes that back out.
    let start = find(moov, &["trak", "edts", "elst"]).and_then(|e| if e[0] == 1 { be64(e, 16).map(|v| v as i64) } else { be32(e, 12).map(|v| v as i32 as i64) }).filter(|v| *v > 0).unwrap_or(0);
    Some(Track { params, nal_len, timescale, default_duration, start })
}

/// One piece of the file, from its index.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fragment {
    offset: u64,
    size: u32,
    /// Seconds.
    start: f64,
}

/// The `sidx` box's fragments. `after`: the byte right after the box, where offsets count from.
fn parse_index(sidx: &[u8], after: u64) -> Option<Vec<Fragment>> {
    let v1 = *sidx.first()? == 1;
    let timescale = be32(sidx, 8)? as f64;
    let (mut time, first, mut at) = if v1 { (be64(sidx, 12)?, be64(sidx, 20)?, 28) } else { (be32(sidx, 12)? as u64, be32(sidx, 16)? as u64, 20) };
    let count = u16::from_be_bytes(sidx.get(at + 2..at + 4)?.try_into().ok()?) as usize;
    at += 4;
    let mut offset = after + first;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let size = be32(sidx, at)? & 0x7fff_ffff;
        out.push(Fragment { offset, size, start: time as f64 / timescale });
        offset += size as u64;
        time += be32(sidx, at + 4)? as u64;
        at += 12;
    }
    Some(out)
}

/// The fragment holding `secs`.
fn fragment_at(fragments: &[Fragment], secs: f64) -> usize {
    fragments.partition_point(|f| f.start <= secs).saturating_sub(1)
}

/// A frame's bytes in the fragment, and when it shows.
#[derive(Debug, PartialEq)]
struct Sample {
    at: usize,
    size: usize,
    /// Seconds.
    pts: f64,
}

/// The samples of a whole fragment (`moof` and `mdat` pairs), in decode order.
fn parse_fragment(b: &[u8], track: &Track) -> Vec<Sample> {
    let mut out = Vec::new();
    let mut start = 0;
    while let Some(size) = be32(b, start).map(|s| s as usize) {
        if size < 8 || start + size > b.len() {
            break;
        }
        if &b[start + 4..start + 8] == b"moof" {
            parse_moof(&b[start + 8..start + size], start, track, &mut out);
        }
        start += size;
    }
    out
}

fn parse_moof(moof: &[u8], moof_at: usize, track: &Track, out: &mut Vec<Sample>) -> Option<()> {
    let traf = find(moof, &["traf"])?;
    let tfhd = find(traf, &["tfhd"])?;
    let flags = be32(tfhd, 0)? & 0xff_ffff;
    let mut o = 8;
    if flags & 0x1 != 0 {
        o += 8;
    }
    if flags & 0x2 != 0 {
        o += 4;
    }
    let default_duration = if flags & 0x8 != 0 {
        o += 4;
        be32(tfhd, o - 4)?
    } else {
        track.default_duration
    };
    let default_size = if flags & 0x10 != 0 { be32(tfhd, o)? } else { 0 };
    let tfdt = find(traf, &["tfdt"])?;
    let mut time = if tfdt[0] == 1 { be64(tfdt, 4)? as i64 } else { be32(tfdt, 4)? as i64 };
    let trun = find(traf, &["trun"])?;
    let flags = be32(trun, 0)? & 0xff_ffff;
    let count = be32(trun, 4)? as usize;
    let mut o = 8;
    // Counted from the start of the `moof`, as YouTube's files say to.
    let mut at = moof_at;
    if flags & 0x1 != 0 {
        at = (moof_at as i64 + be32(trun, o)? as i32 as i64) as usize;
        o += 4;
    }
    if flags & 0x4 != 0 {
        o += 4;
    }
    let scale = track.timescale.max(1) as f64;
    for _ in 0..count {
        let duration = if flags & 0x100 != 0 {
            o += 4;
            be32(trun, o - 4)?
        } else {
            default_duration
        };
        let size = if flags & 0x200 != 0 {
            o += 4;
            be32(trun, o - 4)?
        } else {
            default_size
        } as usize;
        if flags & 0x400 != 0 {
            o += 4;
        }
        let offset = if flags & 0x800 != 0 {
            o += 4;
            be32(trun, o - 4)? as i32 as i64
        } else {
            0
        };
        out.push(Sample { at, size, pts: (time + offset - track.start) as f64 / scale });
        at += size;
        time += duration as i64;
    }
    Some(())
}

/// A sample's length-prefixed NAL units with start codes, as the decoder takes them.
fn annex_b(sample: &[u8], nal_len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(sample.len() + 16);
    let mut i = 0;
    while i + nal_len <= sample.len() {
        let len = sample[i..i + nal_len].iter().fold(0usize, |n, b| n << 8 | *b as usize);
        i += nal_len;
        let Some(nal) = sample.get(i..i + len) else { break };
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
        i += len;
    }
    out
}

// ---------------------------------------------------------------------------- player

pub struct Frame {
    pub pts: f64,
    pub image: egui::ColorImage,
}

/// The glow's size: stretched across the window it's soft on its own.
pub const GLOW_SIZE: [usize; 2] = [32, 18];

/// The frame's colours at [`GLOW_SIZE`], as averages of a few pixels from each block.
pub fn glow_colors(frame: &egui::ColorImage) -> Vec<[f32; 3]> {
    let ([w, h], [gw, gh]) = (frame.size, GLOW_SIZE);
    let mut out = Vec::with_capacity(gw * gh);
    if w == 0 || h == 0 {
        return vec![[0.0; 3]; gw * gh];
    }
    for gy in 0..gh {
        for gx in 0..gw {
            let mut sum = [0.0f32; 3];
            for (ox, oy) in [(1, 1), (3, 1), (1, 3), (3, 3)] {
                let (x, y) = ((gx * 4 + ox) * w / (gw * 4), (gy * 4 + oy) * h / (gh * 4));
                let c = frame.pixels[y * w + x];
                sum[0] += c.r() as f32 / 4.0;
                sum[1] += c.g() as f32 / 4.0;
                sum[2] += c.b() as f32 / 4.0;
            }
            out.push(sum);
        }
    }
    out
}

#[derive(Default)]
struct Shared {
    frames: Mutex<VecDeque<Frame>>,
    stop: AtomicBool,
    failed: AtomicBool,
}

/// A video being decoded along with the audio. Stops when dropped.
pub struct Player {
    shared: Arc<Shared>,
    status: Arc<AudioStatus>,
}

impl Player {
    pub fn start(stream: VideoStream, http: reqwest::Client, status: Arc<AudioStatus>) -> Self {
        let shared = Arc::new(Shared::default());
        let (s, clock) = (shared.clone(), status.clone());
        std::thread::Builder::new()
            .name("video".into())
            .spawn(move || {
                if let Err(e) = run(&stream, &http, &clock, &s) {
                    log::warn!("video: {e:#}");
                    s.failed.store(true, Ordering::Relaxed);
                }
            })
            .expect("a thread for the video");
        Self { shared, status }
    }

    /// The frame to show now, if a newer one than the last is due.
    pub fn take_due(&self) -> Option<Frame> {
        let now = self.status.position_smooth().as_secs_f64();
        let mut frames = self.shared.frames.lock().unwrap();
        let mut due = None;
        while frames.front().is_some_and(|f| f.pts <= now) {
            due = frames.pop_front();
        }
        due
    }

    /// It can't play: the picture stays.
    pub fn failed(&self) -> bool {
        self.shared.failed.load(Ordering::Relaxed)
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

fn new_decoder(track: &Track) -> anyhow::Result<Decoder> {
    // YouTube's H.264 has B-frames: flushing after every sample runs the decoder out of
    // picture buffers within a few frames.
    let config = DecoderConfig::new().flush_after_decode(Flush::NoFlush);
    let mut decoder = Decoder::with_api_config(OpenH264API::from_source(), config)?;
    for nal in openh264::nal_units(&track.params) {
        decoder.decode(nal)?;
    }
    Ok(decoder)
}

/// Times as sorted on the way out of the decoder, which returns frames in display order.
#[derive(PartialEq)]
struct Pts(f64);
impl Eq for Pts {}
impl PartialOrd for Pts {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Pts {
    // Smallest first out of the heap.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other.0.total_cmp(&self.0)
    }
}

fn to_image(yuv: &openh264::decoder::DecodedYUV) -> egui::ColorImage {
    let (w, h) = yuv.dimensions();
    let mut rgba = vec![0u8; w * h * 4];
    yuv.write_rgba8(&mut rgba);
    egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba)
}

fn run(stream: &VideoStream, http: &reqwest::Client, status: &AudioStatus, shared: &Shared) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let fetch = |from: u64, to: u64| -> anyhow::Result<Vec<u8>> {
        rt.block_on(async {
            let url = format!("{}&range={from}-{to}", stream.url);
            let mut last = None;
            for _ in 0..3 {
                let got = async { anyhow::Ok(http.get(&url).header("User-Agent", crate::innertube::UA).timeout(Duration::from_secs(30)).send().await?.error_for_status()?.bytes().await?.to_vec()) };
                match got.await {
                    Ok(b) => return Ok(b),
                    Err(e) => last = Some(e),
                }
            }
            Err(last.unwrap_or_else(|| anyhow::anyhow!("no answer")))
        })
    };
    let head = fetch(0, stream.init_end)?;
    let track = parse_track(find(&head, &["moov"]).ok_or_else(|| anyhow::anyhow!("no moov"))?).ok_or_else(|| anyhow::anyhow!("not H.264 in MP4"))?;
    let index = fetch(stream.index.0, stream.index.1)?;
    let fragments = find(&index, &["sidx"]).and_then(|s| parse_index(s, stream.index.1 + 1)).filter(|f| !f.is_empty()).ok_or_else(|| anyhow::anyhow!("no index"))?;
    log::info!("video: {}x{}, {} fragments", stream.width, stream.height, fragments.len());

    let mut decoder = new_decoder(&track)?;
    let mut next = fragment_at(&fragments, status.position_smooth().as_secs_f64());
    let mut pending: BinaryHeap<Pts> = BinaryHeap::new();
    // Where decoding has got to, for telling a seek from just playing on.
    let mut decoded_to = fragments[next].start;
    'play: while !shared.stop.load(Ordering::Relaxed) {
        let clock = status.position_smooth().as_secs_f64();
        if clock < decoded_to - seek_margin(&shared.frames) || clock > decoded_to + SEEK_AHEAD {
            next = fragment_at(&fragments, clock);
            decoded_to = fragments[next].start;
            decoder = new_decoder(&track)?;
            pending.clear();
            shared.frames.lock().unwrap().clear();
        }
        if shared.frames.lock().unwrap().len() >= READY || next >= fragments.len() {
            std::thread::sleep(Duration::from_millis(15));
            continue;
        }
        let f = fragments[next];
        let bytes = fetch(f.offset, f.offset + f.size as u64 - 1)?;
        next += 1;
        for sample in parse_fragment(&bytes, &track) {
            let Some(data) = bytes.get(sample.at..sample.at + sample.size) else { continue };
            pending.push(Pts(sample.pts));
            decoded_to = decoded_to.max(sample.pts);
            let Ok(Some(yuv)) = decoder.decode(&annex_b(data, track.nal_len)) else { continue };
            let Some(Pts(pts)) = pending.pop() else { continue };
            let clock = status.position_smooth().as_secs_f64();
            if pts < clock - LATE {
                continue;
            }
            let frame = Frame { pts, image: to_image(&yuv) };
            // Paused, or decoding ahead: wait with this frame for room.
            loop {
                if shared.stop.load(Ordering::Relaxed) {
                    break 'play;
                }
                let now = status.position_smooth().as_secs_f64();
                if now < pts - SEEK_AHEAD || now > pts + SEEK_AHEAD {
                    // A seek: the outer loop starts over from there.
                    continue 'play;
                }
                if shared.frames.lock().unwrap().len() < READY {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            shared.frames.lock().unwrap().push_back(frame);
        }
    }
    Ok(())
}

/// How far behind what's decoded the clock may be before it's a seek: decoding runs up to
/// a fragment ahead of what's shown, and the frames still waiting are ahead of it anyway.
fn seek_margin(frames: &Mutex<VecDeque<Frame>>) -> f64 {
    let frames = frames.lock().unwrap();
    let waiting = match (frames.front(), frames.back()) {
        (Some(a), Some(b)) => b.pts - a.pts,
        _ => 0.0,
    };
    waiting + SEEK_BACK + SEEK_AHEAD
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    fn u32s(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    fn track() -> Track {
        Track { params: vec![], nal_len: 4, timescale: 1000, default_duration: 40, start: 80 }
    }

    #[test]
    fn the_glow_follows_the_colours_where_they_are() {
        let (w, h) = (64, 36);
        let pixels = (0..w * h).map(|i| if i % w < w / 2 { egui::Color32::from_rgb(200, 0, 0) } else { egui::Color32::from_rgb(0, 0, 200) }).collect();
        let glow = glow_colors(&egui::ColorImage { size: [w, h], source_size: egui::vec2(w as f32, h as f32), pixels });
        assert_eq!(glow.len(), GLOW_SIZE[0] * GLOW_SIZE[1]);
        assert_eq!(glow[0], [200.0, 0.0, 0.0]);
        assert_eq!(glow[GLOW_SIZE[0] - 1], [0.0, 0.0, 200.0]);
    }

    #[test]
    fn nal_units_get_start_codes() {
        let sample = [0, 0, 0, 2, 9, 9, 0, 0, 0, 1, 7];
        assert_eq!(annex_b(&sample, 4), [0, 0, 0, 1, 9, 9, 0, 0, 0, 1, 7]);
        // Cut off: what's whole stays.
        assert_eq!(annex_b(&sample[..9], 4), [0, 0, 0, 1, 9, 9]);
    }

    #[test]
    fn the_index_lists_fragments_from_after_itself() {
        // Version 0: id, timescale 1000, first pts 0, first offset 10, reserved, 2 references.
        let mut sidx = u32s(&[0, 1, 1000, 0, 10]);
        sidx.extend_from_slice(&[0, 0, 0, 2]);
        sidx.extend(u32s(&[500, 5000, 0, 700, 4000, 0]));
        let f = parse_index(&sidx, 100).unwrap();
        assert_eq!(f, [Fragment { offset: 110, size: 500, start: 0.0 }, Fragment { offset: 610, size: 700, start: 5.0 }]);
        assert_eq!(fragment_at(&f, 0.0), 0);
        assert_eq!(fragment_at(&f, 4.9), 0);
        assert_eq!(fragment_at(&f, 5.0), 1);
        assert_eq!(fragment_at(&f, 99.0), 1);
    }

    #[test]
    fn samples_show_in_display_order_with_the_first_at_zero() {
        // tfhd: no defaults. tfdt v0: base 0. trun: data offset, size and composition offset per sample.
        let tfhd = boxed(b"tfhd", &u32s(&[0, 1]));
        let tfdt = boxed(b"tfdt", &u32s(&[0, 0]));
        let flags = 0x1 | 0x200 | 0x800;
        let trun = boxed(b"trun", &u32s(&[flags, 2, 500, 10, 80, 20, 120]));
        let traf = boxed(b"traf", &[tfhd, tfdt, trun].concat());
        let moof = boxed(b"moof", &traf);
        let samples = parse_fragment(&moof, &track());
        // First: decode time 0 + 80 - start 80. Second: decode time 40 + 120 - 80.
        assert_eq!(samples, [Sample { at: 500, size: 10, pts: 0.0 }, Sample { at: 510, size: 20, pts: 0.08 }]);
    }

    #[test]
    fn a_fragment_cut_short_has_no_made_up_samples() {
        let moof = boxed(b"moof", &[0; 40]);
        assert!(parse_fragment(&moof[..20], &track()).is_empty());
        assert!(parse_fragment(&moof, &track()).is_empty());
    }

    #[test]
    fn frames_leave_the_decoder_soonest_first() {
        let mut heap = BinaryHeap::new();
        for p in [0.2, 0.0, 0.1] {
            heap.push(Pts(p));
        }
        assert_eq!([heap.pop().unwrap().0, heap.pop().unwrap().0, heap.pop().unwrap().0], [0.0, 0.1, 0.2]);
    }
}
