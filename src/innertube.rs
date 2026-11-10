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

/// `opus`: Opus (itag 251, about 130-160 kbps) when the song has it, else AAC.
pub async fn audio_stream(http: &reqwest::Client, video_id: &str, visitor_data: &str, opus: bool) -> anyhow::Result<Stream> {
    let resp = player(http, video_id, visitor_data).await?;
    // Opus in WebM (251) when asked for; AAC-LC in MP4 (140), HE-AAC (139) as fallbacks.
    let formats = resp["streamingData"]["adaptiveFormats"].as_array().map(Vec::as_slice).unwrap_or_default();
    let itags: &[u64] = if opus { &[251, 140, 139] } else { &[140, 139] };
    itags
        .iter()
        .find_map(|itag| {
            let f = formats.iter().find(|f| f["itag"] == *itag)?;
            log::info!("visionOS: {video_id} itag {itag} ({} kbps)", f["averageBitrate"].as_u64().or(f["bitrate"].as_u64()).unwrap_or(0) / 1000);
            Some(Stream {
                url: f["url"].as_str()?.to_owned(),
                size: f["contentLength"].as_str()?.parse().ok()?,
                duration_ms: f["approxDurationMs"].as_str().and_then(|d| d.parse().ok()).unwrap_or(0),
                loudness_db: loudness(&resp, f),
                opus: *itag == 251,
            })
        })
        .ok_or_else(|| anyhow::anyhow!("no audio stream in visionOS player response"))
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
        let r = http.get(url).header("User-Agent", UA).timeout(std::time::Duration::from_secs(8)).send().await?.error_for_status()?;
        anyhow::Ok(r.text().await?)
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

/// The best of `formats` at most `max_height` tall.
fn pick_video(formats: &[Value], max_height: u64) -> Option<VideoStream> {
    let range = |v: &Value| Some((v["start"].as_str()?.parse().ok()?, v["end"].as_str()?.parse().ok()?));
    formats
        .iter()
        .filter(|f| f["mimeType"].as_str().is_some_and(|m| m.starts_with("video/mp4") && m.contains("avc1")))
        .filter(|f| f["height"].as_u64().is_some_and(|h| h <= max_height))
        .max_by_key(|f| f["height"].as_u64())
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
        anyhow::bail!(
            "{}: {}",
            status["status"].as_str().unwrap_or("UNPLAYABLE"),
            status["reason"].as_str().unwrap_or("no reason given")
        );
    }
    Ok(resp)
}

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
    }
}
