//! Shazam's unofficial match endpoint (the one its Android app uses).
//!
//! The request follows SongRec 0.8.2 (src/core/fingerprinting/communication.rs),
//! copyright marin-m and the SongRec contributors, https://github.com/marin-m/SongRec,
//! licensed under the GNU GPL v3.

use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};

use super::Problem;
use super::signature::Signature;

/// What Shazam heard.
#[derive(Clone, Debug)]
pub struct Heard {
    pub title: String,
    /// All of them, in one line ("Rihanna Feat. Calvin Harris").
    pub artist: String,
}

const USER_AGENT: &str = "Dalvik/2.1.0 (Linux; U; Android 6.0.1; SM-G920F Build/MMB29K)";

/// How long to leave Shazam alone after it says too many, when it doesn't say.
const COOL_DOWN: Duration = Duration::from_secs(60);

/// `None` when Shazam knows no song like it.
pub async fn identify(http: &reqwest::Client, sig: &Signature) -> Result<Option<Heard>, Problem> {
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    let body = json!({
        "geolocation": { "altitude": 300, "latitude": 45, "longitude": 2 },
        "signature": { "samplems": sig.duration_ms(), "timestamp": now, "uri": sig.to_uri() },
        "timestamp": now,
        "timezone": "Europe/Paris",
    });
    let url = format!(
        "https://amp.shazam.com/discovery/v5/en/US/android/-/tag/{}/{}?sync=true&webv3=true&sampling=true&connected=&shazamapiversion=v3&sharehub=true&video=v3",
        uuid::Uuid::new_v4().to_string().to_uppercase(),
        uuid::Uuid::new_v4()
    );
    let failed = |e: reqwest::Error| {
        log::warn!("recognize: Shazam: {e:#}");
        if e.is_connect() { Problem::Offline } else { Problem::NoAnswer }
    };
    let res = http
        .post(url)
        .header("User-Agent", USER_AGENT)
        .header("Content-Language", "en_US")
        .timeout(Duration::from_secs(8))
        .json(&body)
        .send()
        .await
        .map_err(failed)?;
    if res.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        // Within reason: it's untrusted.
        let wait = res.headers().get(reqwest::header::RETRY_AFTER).and_then(|v| v.to_str().ok()?.parse().ok()).map_or(COOL_DOWN, |s: u64| Duration::from_secs(s.min(3600)));
        log::warn!("recognize: Shazam says too many, for {wait:?}");
        return Err(Problem::RateLimited(Instant::now() + wait));
    }
    let v: Value = res.error_for_status().map_err(failed)?.json().await.map_err(failed)?;
    Ok(heard(&v))
}

fn heard(v: &Value) -> Option<Heard> {
    let t = &v["track"];
    Some(Heard { title: t["title"].as_str()?.to_owned(), artist: t["subtitle"].as_str().unwrap_or_default().to_owned() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_track_or_none() {
        let v = json!({"matches": [{}], "track": {"title": "Glue Song", "subtitle": "beabadoobee"}});
        let h = heard(&v).unwrap();
        assert_eq!((h.title.as_str(), h.artist.as_str()), ("Glue Song", "beabadoobee"));
        assert!(heard(&json!({"matches": [], "retryms": 6000})).is_none());
    }
}
