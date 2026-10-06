//! Direct InnerTube `/player` call using the visionOS client.
//!
//! As of Oct 2026 this client returns plain (unciphered) stream URLs that need neither
//! signature deobfuscation nor a PO token, and googlevideo serves every byte range for
//! them. rustypipe's own player clients currently fail deobfuscation or get cut off
//! after 1 MiB, so this is the primary path and rustypipe is the fallback.
//! Mirrors yt-dlp's `visionos` client definition; update together if YouTube changes it.

use serde_json::{Value, json};

pub const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";

pub struct Stream {
    pub url: String,
    pub size: u64,
}

pub async fn audio_stream(http: &reqwest::Client, video_id: &str, visitor_data: &str) -> anyhow::Result<Stream> {
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
    // AAC-LC in MP4 (itag 140) decodes in pure Rust; HE-AAC (139) as a fallback.
    let formats = resp["streamingData"]["adaptiveFormats"].as_array().map(Vec::as_slice).unwrap_or_default();
    [140, 139]
        .iter()
        .find_map(|itag| {
            let f = formats.iter().find(|f| f["itag"] == *itag)?;
            Some(Stream {
                url: f["url"].as_str()?.to_owned(),
                size: f["contentLength"].as_str()?.parse().ok()?,
            })
        })
        .ok_or_else(|| anyhow::anyhow!("no AAC stream in visionOS player response"))
}
