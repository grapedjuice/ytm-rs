//! Direct InnerTube `/player` call using the visionOS client.
//!
//! As of Oct 2026 this client returns plain (unciphered) stream URLs that need neither
//! signature deobfuscation nor a PO token, and googlevideo serves every byte range for
//! them; the web clients' streams need a PO token or get cut off after 1 MiB, so this
//! is the path for everyone except signed-in Premium accounts (see `webmusic.rs`).
//! Some networks get a YouTube experiment that cuts this client's file URLs off at
//! 1 MiB too (Nov 2026); its HLS segments still play there (`hls_audio`, `hls.rs`).
//! Mirrors yt-dlp's `visionos` client definition; update together if YouTube changes it.

use serde_json::{Value, json};

pub const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";

pub struct Stream {
    /// Explicit representation identity for exact-format renewal.
    pub itag: u64,
    pub url: String,
    pub size: u64,
    pub duration_ms: u64,
    /// dB above YouTube's -14 LUFS loudness target (positive = louder).
    pub loudness_db: Option<f32>,
    /// Opus in WebM; otherwise AAC in MP4.
    pub opus: bool,
}

/// A format's loudness relative to YouTube's target, falling back to the track-level
/// figure in `playerConfig.audioConfig`.
pub fn loudness(resp: &Value, format: &Value) -> Option<f32> {
    format["loudnessDb"].as_f64().map(|v| v as f32).or_else(|| {
        let a = &resp["playerConfig"]["audioConfig"];
        let track = a["trackAbsoluteLoudnessLkfs"].as_f64().or_else(|| a["perceptualLoudnessDb"].as_f64())?;
        Some((track - a["loudnessTargetLkfs"].as_f64().unwrap_or(-14.0)) as f32)
    })
}

/// The Android VR client's queue for an album's playlist (`OLAK5uy_…`). Unlike the web
/// clients, which list a track's music video when it has one, it lists the album's songs.
/// Needs no sign-in or PO token. Mirrors Morphe's "Play albums songs" patch
/// (github.com/MorpheApp/morphe-patches, `PlaylistRequest.java`); update together.
pub async fn album_queue(http: &reqwest::Client, playlist_id: &str) -> anyhow::Result<Value> {
    const VERSION: &str = "1.73.21";
    let body = json!({
        "context": {"client": {
            "clientName": "ANDROID_VR",
            "clientVersion": VERSION,
            "deviceMake": "Pico",
            "deviceModel": "A8110",
            "osName": "Android",
            "osVersion": "10",
            "androidSdkVersion": 29,
            // English, so the playlist's title says "Album - …".
            "hl": "en",
            "gl": "US",
        }},
        "playlistId": playlist_id,
        "contentCheckOk": true,
        "racyCheckOk": true,
    });
    Ok(http
        .post("https://youtubei.googleapis.com/youtubei/v1/next?prettyPrint=false")
        .header("User-Agent", format!("com.google.android.apps.youtube.vr.pico/{VERSION} (Linux; U; Android 10; A8110 Build/QKQ1) gzip"))
        .header("X-YouTube-Client-Name", "28")
        .header("X-YouTube-Client-Version", VERSION)
        .timeout(std::time::Duration::from_secs(8))
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

const LEGACY_OPUS_ITAGS: &[u64] = &[251, 140, 139];
const OPUS_ITAGS: &[u64] = &[251, 250, 249, 140, 139];
const AAC_ITAGS: &[u64] = &[140, 139];

/// Preserve the existing best-quality contract: 251, then 140/139; AAC skips Opus.
pub async fn audio_stream(http: &reqwest::Client, video_id: &str, visitor_data: &str, opus: bool) -> anyhow::Result<Stream> {
    let resp = player(http, video_id, visitor_data).await?;
    let stream = parse_audio_formats(&resp, if opus { LEGACY_OPUS_ITAGS } else { AAC_ITAGS })
        .into_iter().next().ok_or_else(|| anyhow::anyhow!("no audio stream in visionOS player response"))?;
    log::info!("visionOS: {video_id} itag {}", stream.itag);
    Ok(stream)
}

/// Direct candidates in quality order, including lower-bitrate Opus for recovery.
/// A returned URL is not proof of available bytes; the caller probes its target.
pub async fn audio_streams(http: &reqwest::Client, video_id: &str, visitor_data: &str, opus: bool) -> anyhow::Result<Vec<Stream>> {
    let resp = player(http, video_id, visitor_data).await?;
    let streams = parse_audio_formats(&resp, if opus { OPUS_ITAGS } else { AAC_ITAGS });
    anyhow::ensure!(!streams.is_empty(), "no audio stream in visionOS player response");
    Ok(streams)
}

/// The AAC stream as HLS segments: what plays where YouTube cuts the visionOS player's
/// progressive URLs off at 1 MiB without a PO token (see `hls.rs`).
pub struct HlsStream {
    /// Each segment's start (s) and URL.
    pub segments: Vec<(f64, String)>,
    pub duration_ms: u64,
    pub loudness_db: Option<f32>,
}

/// HLS audio: itag 234, the AAC-LC of itag 140. (233, HE-AAC, needs SBR, which
/// Symphonia's decoder doesn't do.)
pub async fn hls_audio(http: &reqwest::Client, video_id: &str, visitor_data: &str) -> anyhow::Result<HlsStream> {
    let resp = player(http, video_id, visitor_data).await?;
    let master_url = resp["streamingData"]["hlsManifestUrl"].as_str().ok_or_else(|| anyhow::anyhow!("no HLS manifest in visionOS player response"))?;
    let get = |url: String| async move {
        let r = http.get(url).header("User-Agent", UA).timeout(std::time::Duration::from_secs(8)).send().await.map_err(reqwest::Error::without_url)?.error_for_status().map_err(reqwest::Error::without_url)?;
        anyhow::Ok(r.text().await.map_err(reqwest::Error::without_url)?)
    };
    let master = get(master_url.to_owned()).await?;
    let media_url = hls_audio_playlist(&master, "234").ok_or_else(|| anyhow::anyhow!("no AAC audio (234) in the HLS manifest"))?;
    let (segments, listed) = hls_segments(&get(media_url).await?);
    anyhow::ensure!(!segments.is_empty(), "empty HLS audio playlist");
    let formats = resp["streamingData"]["adaptiveFormats"].as_array().map(Vec::as_slice).unwrap_or_default();
    let aac = formats.iter().find(|f| f["itag"] == 140);
    let duration_ms = aac.and_then(|f| f["approxDurationMs"].as_str()?.parse().ok()).unwrap_or((listed * 1000.0) as u64);
    log::info!("visionOS: {video_id} HLS itag 234 ({} segments)", segments.len());
    Ok(HlsStream { segments, duration_ms, loudness_db: loudness(&resp, aac.unwrap_or(&Value::Null)) })
}

/// The URI of an HLS master playlist's audio rendition in `group`.
fn hls_audio_playlist(master: &str, group: &str) -> Option<String> {
    let group = format!("GROUP-ID=\"{group}\"");
    master.lines().filter(|l| l.starts_with("#EXT-X-MEDIA:") && l.contains("TYPE=AUDIO") && l.contains(&group)).find_map(|l| {
        let uri = &l[l.find("URI=\"")? + 5..];
        Some(uri[..uri.find('"')?].to_owned())
    })
}

/// A media playlist's segments, each one's start (s, the `#EXTINF` durations before it)
/// and URL, and their length in all.
fn hls_segments(media: &str) -> (Vec<(f64, String)>, f64) {
    let (mut out, mut at, mut dur) = (Vec::new(), 0.0, 0.0);
    for line in media.lines().map(str::trim) {
        if let Some(d) = line.strip_prefix("#EXTINF:") {
            dur = d.split(',').next().and_then(|d| d.trim().parse().ok()).unwrap_or(0.0);
        } else if !line.is_empty() && !line.starts_with('#') {
            out.push((at, line.to_owned()));
            at += dur;
            dur = 0.0;
        }
    }
    (out, at)
}

/// A video's H.264 in fragmented MP4, the only video this app decodes.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoStream {
    pub url: String,
    pub width: u32,
    pub height: u32,
    /// Last byte of the header.
    pub init_end: u64,
    /// First and last byte of the fragment index.
    pub index: (u64, u64),
}

/// The best of `formats` at most `max_height` tall. Of two as tall, the lower frame rate:
/// a 60 fps video takes twice the decoding, which 720p already can't keep up with.
fn pick_video(formats: &[Value], max_height: u64) -> Option<VideoStream> {
    let range = |v: &Value| Some((v["start"].as_str()?.parse().ok()?, v["end"].as_str()?.parse().ok()?));
    formats
        .iter()
        .filter(|f| f["mimeType"].as_str().is_some_and(|m| m.starts_with("video/mp4") && m.contains("avc1")))
        .filter(|f| f["height"].as_u64().is_some_and(|h| h <= max_height))
        .max_by_key(|f| (f["height"].as_u64(), std::cmp::Reverse(f["fps"].as_u64())))
        .and_then(|f| {
            let (_, init_end) = range(&f["initRange"])?;
            Some(VideoStream { url: f["url"].as_str()?.to_owned(), width: f["width"].as_u64()? as u32, height: f["height"].as_u64()? as u32, init_end, index: range(&f["indexRange"])? })
        })
}

pub async fn video_stream(http: &reqwest::Client, video_id: &str, visitor_data: &str, max_height: u64) -> anyhow::Result<VideoStream> {
    let resp = player(http, video_id, visitor_data).await?;
    let formats = resp["streamingData"]["adaptiveFormats"].as_array().map(Vec::as_slice).unwrap_or_default();
    pick_video(formats, max_height).ok_or_else(|| anyhow::anyhow!("no H.264 video in visionOS player response"))
}

async fn player(http: &reqwest::Client, video_id: &str, visitor_data: &str) -> anyhow::Result<Value> {
    let body = json!({
        "context": {"client": {
            "clientName": "VISIONOS",
            "clientVersion": "1.02",
            "deviceMake": "Apple",
            "deviceModel": "RealityDevice17,1",
            "userAgent": UA,
            "osName": "visionOS",
            "osVersion": "26.5.23O471",
            "hl": "en",
            "gl": "US",
            "visitorData": visitor_data,
        }},
        "videoId": video_id,
        "contentCheckOk": true,
        "racyCheckOk": true,
    });
    let resp: Value = http
        .post("https://www.youtube.com/youtubei/v1/player?prettyPrint=false")
        .header("User-Agent", UA)
        .header("X-YouTube-Client-Name", "101")
        .header("X-YouTube-Client-Version", "1.02")
        .header("X-Goog-Visitor-Id", visitor_data)
        .header("Origin", "https://www.youtube.com")
        .timeout(std::time::Duration::from_secs(8))
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let status = &resp["playabilityStatus"];
    if status["status"] != "OK" {
        return Err(Refused {
            status: status["status"].as_str().unwrap_or("UNPLAYABLE").to_owned(),
            reason: status["reason"].as_str().unwrap_or("no reason given").to_owned(),
        }
        .into());
    }
    Ok(resp)
}

/// The player's `playabilityStatus` when it won't play the video: `status` is
/// LOGIN_REQUIRED, UNPLAYABLE, ERROR and so on, `reason` the words it gives.
#[derive(Debug)]
pub struct Refused {
    pub status: String,
    pub reason: String,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.status, self.reason)
    }
}

impl std::error::Error for Refused {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hls_audio_rendition_and_segments() {
        let master = "#EXTM3U\n\
            #EXT-X-MEDIA:URI=\"https://m/itag/233/index.m3u8\",TYPE=AUDIO,GROUP-ID=\"233\",NAME=\"Default\"\n\
            #EXT-X-MEDIA:URI=\"https://m/itag/234/index.m3u8\",TYPE=AUDIO,GROUP-ID=\"234\",NAME=\"Default\",DEFAULT=YES\n\
            #EXT-X-STREAM-INF:BANDWIDTH=149344,CODECS=\"avc1.4D400C,mp4a.40.2\",AUDIO=\"234\"\n\
            https://m/video.m3u8\n";
        assert_eq!(hls_audio_playlist(master, "234").as_deref(), Some("https://m/itag/234/index.m3u8"));
        assert_eq!(hls_audio_playlist(master, "140"), None);
        let media = "#EXTM3U\n#EXT-X-TARGETDURATION:7\n#EXTINF:7.0,\nhttps://s/0\n#EXTINF:7.0,\nhttps://s/1\n#EXTINF:5.04,\nhttps://s/2\n#EXT-X-ENDLIST\n";
        assert_eq!(hls_segments(media), (vec![(0.0, "https://s/0".to_owned()), (7.0, "https://s/1".to_owned()), (14.0, "https://s/2".to_owned())], 19.04));
    }

    #[test]
    fn video_is_the_tallest_h264_within_the_limit() {
        let format = |itag: u64, mime: &str, height: u64| json!({"itag": itag, "mimeType": mime, "height": height, "width": height * 16 / 9, "url": format!("u{itag}"), "initRange": {"start": "0", "end": "740"}, "indexRange": {"start": "741", "end": "2000"}});
        let formats = [format(137, "video/mp4; codecs=\"avc1.640028\"", 1080), format(136, "video/mp4; codecs=\"avc1.4D401F\"", 720), format(135, "video/mp4; codecs=\"avc1.4D401E\"", 480), format(247, "video/webm; codecs=\"vp9\"", 720), format(398, "video/mp4; codecs=\"av01.0.05M.08\"", 720)];
        let v = pick_video(&formats, 720).unwrap();
        assert_eq!((v.url.as_str(), v.height, v.init_end, v.index), ("u136", 720, 740, (741, 2000)));
        assert_eq!(pick_video(&formats, 480).unwrap().url, "u135");
        assert_eq!(pick_video(&formats, 100), None);
        assert_eq!(pick_video(&formats[3..], 1080), None);
        // The 30 fps one of the same height, whichever comes first.
        let at = |itag: u64, fps: u64| {
            let mut f = format(itag, "video/mp4; codecs=\"avc1.4D401F\"", 720);
            f["fps"] = json!(fps);
            f
        };
        assert_eq!(pick_video(&[at(298, 60), at(136, 30)], 720).unwrap().url, "u136");
        assert_eq!(pick_video(&[at(136, 30), at(298, 60)], 720).unwrap().url, "u136");
    }
}

/// Parsing never prints a stream URL or a format object containing signatures.
fn parse_audio_formats(resp: &Value, itags: &[u64]) -> Vec<Stream> {
    let formats = resp["streamingData"]["adaptiveFormats"].as_array().map(Vec::as_slice).unwrap_or_default();
    itags.iter().flat_map(|itag| {
        formats.iter().filter(move |f| f["itag"].as_u64() == Some(*itag)).filter_map(move |f| {
            let address = f["url"].as_str()?;
            let parsed = url::Url::parse(address).ok()?;
            if !matches!(parsed.scheme(), "https" | "http") || parsed.host_str().is_none() {
                return None;
            }
            let size = f["contentLength"].as_str().and_then(|n| n.parse::<u64>().ok())
                .or_else(|| f["contentLength"].as_u64()).filter(|n| *n > 0)?;
            Some(Stream {
                itag: *itag,
                url: address.to_owned(),
                size,
                duration_ms: f["approxDurationMs"].as_str().and_then(|d| d.parse().ok())
                    .or_else(|| f["approxDurationMs"].as_u64()).unwrap_or(0),
                loudness_db: loudness(resp, f),
                opus: matches!(*itag, 249..=251),
            })
        })
    }).collect()
}


#[cfg(test)]
mod audio_candidate_tests {
    use super::*;

    fn format(itag: u64) -> Value {
        json!({"itag": itag, "url": format!("https://media.invalid/audio?itag={itag}"),
            "contentLength": "4096", "approxDurationMs": "197508"})
    }

    fn response(formats: Vec<Value>) -> Value {
        json!({"streamingData": {"adaptiveFormats": formats}})
    }

    #[test]
    fn recovery_formats_are_quality_ordered_without_changing_legacy_selection() {
        let resp = response([139, 251, 249, 140, 250].into_iter().map(format).collect());
        let ids = |order| parse_audio_formats(&resp, order).into_iter().map(|s| s.itag).collect::<Vec<_>>();
        assert_eq!(ids(OPUS_ITAGS), [251, 250, 249, 140, 139]);
        assert_eq!(ids(LEGACY_OPUS_ITAGS), [251, 140, 139]);
        assert_eq!(ids(AAC_ITAGS), [140, 139]);
        // Lower Opus is recovery-only: the existing selector still prefers AAC.
        let resp = response([249, 140, 250].into_iter().map(format).collect());
        assert_eq!(parse_audio_formats(&resp, LEGACY_OPUS_ITAGS)[0].itag, 140);
    }

    #[test]
    fn every_opus_bitrate_has_the_opus_decoder_flag() {
        let resp = response([139, 140, 249, 250, 251].into_iter().map(format).collect());
        let streams = parse_audio_formats(&resp, OPUS_ITAGS);
        assert_eq!(streams.iter().map(|s| (s.itag, s.opus)).collect::<Vec<_>>(),
            [(251, true), (250, true), (249, true), (140, false), (139, false)]);
    }

    #[test]
    fn invalid_direct_urls_and_lengths_are_skipped_without_hiding_valid_duplicates() {
        let mut cipher_only = format(251);
        cipher_only.as_object_mut().unwrap().remove("url");
        cipher_only["signatureCipher"] = json!("not-a-direct-url");
        let mut invalid_url = format(250);
        invalid_url["url"] = json!("");
        let mut invalid_length = format(249);
        invalid_length["contentLength"] = json!("invalid");
        let mut zero_length = format(140);
        zero_length["contentLength"] = json!("0");
        let mut missing_length = format(139);
        missing_length.as_object_mut().unwrap().remove("contentLength");
        let mut unsupported_scheme = format(251);
        unsupported_scheme["url"] = json!("file:///audio");
        let resp = response(vec![cipher_only, invalid_url, invalid_length, zero_length, missing_length,
            unsupported_scheme, format(140)]);
        let streams = parse_audio_formats(&resp, OPUS_ITAGS);
        assert_eq!(streams.iter().map(|s| s.itag).collect::<Vec<_>>(), [140]);
        assert_eq!(streams[0].size, 4096);
    }

    #[test]
    fn all_direct_variants_keep_metadata_and_loudness() {
        let mut second = format(250);
        second["contentLength"] = json!(8192);
        second["approxDurationMs"] = json!(197481);
        let mut first = format(250);
        first["loudnessDb"] = json!(1.5);
        let mut resp = response(vec![second, first]);
        resp["playerConfig"] = json!({"audioConfig": {"trackAbsoluteLoudnessLkfs": -10.0, "loudnessTargetLkfs": -14.0}});
        let streams = parse_audio_formats(&resp, OPUS_ITAGS);
        assert_eq!(streams.len(), 2);
        assert_eq!((streams[0].size, streams[0].duration_ms), (8192, 197481));
        assert_eq!(streams[0].loudness_db, Some(4.0));
        assert_eq!(streams[1].loudness_db, Some(1.5));
    }
}
