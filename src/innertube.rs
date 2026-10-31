//! Direct InnerTube `/player` call using the visionOS client.
//!
//! As of Oct 2026 this client returns plain (unciphered) stream URLs that need neither
//! signature deobfuscation nor a PO token, and googlevideo serves every byte range for
//! them; the web clients' streams need a PO token or get cut off after 1 MiB, so this
//! is the path for everyone except signed-in Premium accounts (see `webmusic.rs`).
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

/// `opus`: Opus (itag 251, about 130-160 kbps) when the song has it, else AAC.
pub async fn audio_stream(http: &reqwest::Client, video_id: &str, visitor_data: &str, opus: bool) -> anyhow::Result<Stream> {
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
