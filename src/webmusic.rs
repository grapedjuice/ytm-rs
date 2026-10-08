//! Signed-in `/player` call as the YouTube Music web client, for songs the anonymous
//! visionOS client refuses: Music Premium only and age-restricted.
//!
//! Its stream URLs carry challenges that [`crate::jsc`] solves. Premium accounts need
//! no PO token for them, so this path is used only when the account has Premium;
//! otherwise googlevideo cuts the stream off after about 1 MiB.
//! Mirrors yt-dlp's `web_music` client; update together if YouTube changes it.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::Url;
use serde_json::{Value, json};
use sha1::{Digest, Sha1};

use crate::innertube::Stream;
use crate::jsc::Solver;

pub const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";
const VERSION: &str = "1.20260707.12.00";

/// The signed-in session as stored by rustypipe.
pub struct Session {
    pub cookie: String,
    session_index: u32,
}

impl Session {
    /// `None` when signed out.
    pub fn load(data_dir: &Path) -> Option<Self> {
        let cache: Value = serde_json::from_str(&std::fs::read_to_string(data_dir.join("rustypipe_cache.json")).ok()?).ok()?;
        let auth = &cache["auth_cookie"];
        Some(Self {
            cookie: auth["cookie"].as_str()?.to_owned(),
            session_index: auth["session_index"].as_u64().unwrap_or(0) as u32,
        })
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.cookie.split(';').find_map(|kv| kv.trim().strip_prefix(name)?.strip_prefix('='))
    }

    /// `SAPISIDHASH` authorization, as the web clients send it.
    fn authorization(&self, origin: &str) -> anyhow::Result<String> {
        let ts = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let parts: Vec<String> = [("SAPISIDHASH", "SAPISID"), ("SAPISID1PHASH", "__Secure-1PAPISID"), ("SAPISID3PHASH", "__Secure-3PAPISID")]
            .iter()
            .filter_map(|(scheme, cookie)| {
                let sid = self.get(cookie)?;
                let hash = Sha1::digest(format!("{ts} {sid} {origin}"));
                let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
                Some(format!("{scheme} {ts}_{hex}"))
            })
            .collect();
        anyhow::ensure!(!parts.is_empty(), "signed-in session has no SAPISID cookie");
        Ok(parts.join(" "))
    }

    async fn post(&self, http: &reqwest::Client, origin: &str, endpoint: &str, client: (&str, &str, &str), body: Value) -> anyhow::Result<Value> {
        let (number, version, ua) = client;
        Ok(http
            .post(format!("{origin}/youtubei/v1/{endpoint}?prettyPrint=false"))
            .header("User-Agent", ua)
            .header("X-YouTube-Client-Name", number)
            .header("X-YouTube-Client-Version", version)
            .header("Origin", origin)
            .header("X-Origin", origin)
            .header("Cookie", &self.cookie)
            .header("Authorization", self.authorization(origin)?)
            .header("X-Goog-AuthUser", self.session_index.to_string())
            .timeout(std::time::Duration::from_secs(10))
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    /// Whether the account has YouTube Premium, read from the YouTube top bar logo
    /// the way yt-dlp does.
    pub async fn is_premium(&self, http: &reqwest::Client) -> anyhow::Result<bool> {
        const WEB: &str = "2.20260708.00.00";
        let body = json!({"context": {"client": {"clientName": "WEB", "clientVersion": WEB, "hl": "en", "gl": "US"}}, "browseId": "FEwhat_to_watch"});
        let resp = self.post(http, "https://www.youtube.com", "browse", ("1", WEB, UA), body).await?;
        let logo = &resp["topbar"]["desktopTopbarRenderer"]["logo"]["topbarLogoRenderer"];
        Ok(logo["iconImage"]["iconType"] == "YOUTUBE_PREMIUM_LOGO"
            || logo["tooltipText"].to_string().to_lowercase().contains("premium"))
    }

    pub async fn audio_stream(&self, http: &reqwest::Client, solver: &Solver, video_id: &str) -> anyhow::Result<Stream> {
        let player = solver.player(http).await?;
        let body = json!({
            "context": {"client": {"clientName": "WEB_REMIX", "clientVersion": VERSION, "hl": "en", "gl": "US"}},
            "videoId": video_id,
            "playbackContext": {"contentPlaybackContext": {"html5Preference": "HTML5_PREF_WANTS", "signatureTimestamp": player.sts}},
            "contentCheckOk": true,
            "racyCheckOk": true,
        });
        let resp = self.post(http, "https://music.youtube.com", "player", ("67", VERSION, UA), body).await?;
        let status = &resp["playabilityStatus"];
        if status["status"] != "OK" {
            anyhow::bail!("{}: {}", status["status"].as_str().unwrap_or("UNPLAYABLE"), status["reason"].as_str().unwrap_or("no reason given"));
        }
        // 141 is Premium's 256 kbps AAC-LC; 140 is the usual 128 kbps.
        let formats = resp["streamingData"]["adaptiveFormats"].as_array().map(Vec::as_slice).unwrap_or_default();
        let f = [141, 140, 139]
            .iter()
            .find_map(|itag| formats.iter().find(|f| f["itag"] == *itag))
            .ok_or_else(|| anyhow::anyhow!("no AAC stream in web player response"))?;
        let size = f["contentLength"].as_str().and_then(|s| s.parse().ok()).ok_or_else(|| anyhow::anyhow!("unknown stream size"))?;
        let duration_ms = f["approxDurationMs"].as_str().and_then(|d| d.parse().ok()).unwrap_or(0);

        // Either a plain URL, or `signatureCipher` = s=<challenge>&sp=<param>&url=<url>.
        let (raw, cipher) = match f["url"].as_str() {
            Some(u) => (u.to_owned(), None),
            None => {
                let c = f["signatureCipher"].as_str().ok_or_else(|| anyhow::anyhow!("format has no URL"))?;
                let q: Vec<(String, String)> = Url::parse(&format!("http://x/?{c}"))?.query_pairs().into_owned().collect();
                let get = |k: &str| q.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
                let url = get("url").ok_or_else(|| anyhow::anyhow!("cipher has no url"))?;
                let s = get("s").ok_or_else(|| anyhow::anyhow!("cipher has no signature"))?;
                (url, Some((s, get("sp").unwrap_or_else(|| "signature".into()))))
            }
        };
        let mut url = Url::parse(&raw)?;
        let mut pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        let n = pairs.iter().find(|(k, _)| k == "n").map(|(_, v)| v.clone());
        let (n_out, sig_out) = solver.solve(&player, n, cipher.as_ref().map(|(s, _)| s.clone())).await?;
        if let Some(n) = n_out {
            pairs.iter_mut().filter(|(k, _)| k == "n").for_each(|(_, v)| *v = n.clone());
        }
        if let (Some(sig), Some((_, sp))) = (sig_out, cipher) {
            pairs.push((sp, sig));
        }
        url.query_pairs_mut().clear().extend_pairs(pairs);
        Ok(Stream { url: url.into(), size, duration_ms })
    }
}
