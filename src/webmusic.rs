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

/// The signed-in Google session: the youtube.com cookies the sign-in window captured,
/// plus which signed-in account and channel they are for.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub cookie: String,
    session_index: u32,
    /// Brand channel to act as (X-Goog-PageId), when it isn't the account's own.
    #[serde(default)]
    page_id: Option<String>,
    /// The signed-in browser's visitor ID: signed-in requests carry it, as the site's do,
    /// not an anonymous visitor's.
    #[serde(default)]
    pub visitor_data: Option<String>,
    /// Cookies Google replaced while answering a request made from here (the Premium
    /// check, the web player), for the API to take into the session.
    #[serde(skip)]
    replaced: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

/// Google has signed the session out (password change, "sign out everywhere", or a
/// copy it took for stolen).
#[derive(Debug)]
pub struct Revoked;

impl std::fmt::Display for Revoked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Google signed this session out")
    }
}

impl std::error::Error for Revoked {}

/// The credential the session is saved under (see [`crate::vault`]).
const VAULT: &str = "google session";
/// Where versions up to 0.2.12 kept the session, in plain text; moved into the
/// credential store on first launch.
const FILE: &str = "account.json";
/// Where versions up to 0.2.11 kept the session (rustypipe's cache).
const LEGACY: &str = "rustypipe_cache.json";

impl Session {
    pub fn new(cookie: String, session_index: u32, page_id: Option<String>) -> Self {
        Self { cookie, session_index, page_id, visitor_data: None, replaced: Default::default() }
    }

    /// `None` when signed out. A session saved by an older version is carried over.
    pub fn load(data_dir: &Path) -> Option<Self> {
        if let Some(s) = crate::vault::get(VAULT, data_dir).and_then(|b| serde_json::from_slice(&b).ok()) {
            // No plain copy may outlive the move (an interrupted one, or an older version run since).
            Self::forget_plain(data_dir);
            return Some(s);
        }
        if let Some(s) = std::fs::read(data_dir.join(FILE)).ok().and_then(|b| serde_json::from_slice::<Self>(&b).ok()) {
            // Only delete the plain copies once the credential store has it.
            if s.save(data_dir) {
                Self::forget_plain(data_dir);
                log::info!("signed-in session moved into the credential store");
            }
            return Some(s);
        }
        let cache: Value = serde_json::from_str(&std::fs::read_to_string(data_dir.join(LEGACY)).ok()?).ok()?;
        let auth = &cache["auth_cookie"];
        let index = auth["session_index"].as_u64().or_else(|| auth["session_index"].as_str()?.parse().ok()).unwrap_or(0);
        let s = Self::new(auth["cookie"].as_str()?.to_owned(), index as u32, auth["channel_syncid"].as_str().map(str::to_owned));
        log::info!("signed-in session carried over from the previous version");
        if s.save(data_dir) {
            Self::forget_plain(data_dir);
        }
        Some(s)
    }

    /// Save to the credential store; false when it refused.
    pub fn save(&self, data_dir: &Path) -> bool {
        match crate::vault::set(VAULT, data_dir, &serde_json::to_vec(self).unwrap_or_default()) {
            Ok(()) => true,
            Err(e) => {
                log::warn!("couldn't save the session: {e:#}");
                false
            }
        }
    }

    /// Sign out: forget the session, including an older version's copy of it.
    pub fn remove(data_dir: &Path) {
        crate::vault::delete(VAULT, data_dir);
        Self::forget_plain(data_dir);
    }

    /// Remove the plain-text copies older versions kept.
    fn forget_plain(data_dir: &Path) {
        let _ = std::fs::remove_file(data_dir.join(FILE));
        let legacy = data_dir.join(LEGACY);
        if let Some(mut cache) = std::fs::read(&legacy).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            && let Some(obj) = cache.as_object_mut().filter(|o| o.contains_key("auth_cookie"))
        {
            obj.remove("auth_cookie");
            let _ = std::fs::write(&legacy, serde_json::to_vec(&cache).unwrap_or_default());
        }
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

    /// Add the signed-in headers for a request to `origin`.
    pub fn sign(&self, req: reqwest::RequestBuilder, origin: &str) -> anyhow::Result<reqwest::RequestBuilder> {
        let mut req = req
            .header("Cookie", &self.cookie)
            .header("Authorization", self.authorization(origin)?)
            .header("X-Goog-AuthUser", self.session_index.to_string());
        if let Some(p) = &self.page_id {
            req = req.header("X-Goog-PageId", p);
        }
        Ok(req)
    }

    /// Google keeps a signed-in session alive by replacing its cookies as it's used
    /// (`__Secure-1PSIDTS`, `SIDCC`, …), and treats a copy that never takes the new
    /// ones as stolen. The session with these `Set-Cookie` headers applied, when they
    /// change anything. Only youtube.com-wide cookies count, and deletions are left out:
    /// a revoked session is noticed by the signed-out answers instead.
    pub fn updated<'a>(&self, set_cookies: impl IntoIterator<Item = &'a str>) -> Option<Self> {
        let year = 1970 + SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() / 31_556_952);
        let mut jar: Vec<(String, String)> = self
            .cookie
            .split(';')
            .filter_map(|kv| kv.trim().split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned())))
            .collect();
        let mut changed = false;
        for line in set_cookies {
            let mut parts = line.split(';');
            let Some((name, value)) = parts.next().and_then(|kv| kv.trim().split_once('=')) else { continue };
            let attr = |key: &str| parts.clone().find_map(|a| a.trim().split_once('=').filter(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.trim()));
            let wide = attr("domain").is_some_and(|d| d.trim_start_matches('.').eq_ignore_ascii_case("youtube.com"));
            let gone = value.is_empty()
                || attr("max-age").is_some_and(|a| a.parse::<i64>().is_ok_and(|a| a <= 0))
                || attr("expires").and_then(|e| e.split(|c: char| !c.is_ascii_digit()).find(|t| t.len() == 4)?.parse::<u64>().ok()).is_some_and(|y| y < year);
            if !wide || gone {
                continue;
            }
            match jar.iter_mut().find(|(k, _)| k == name) {
                Some((_, v)) if v == value => {}
                Some((_, v)) => {
                    *v = value.to_owned();
                    changed = true;
                }
                None => {
                    jar.push((name.to_owned(), value.to_owned()));
                    changed = true;
                }
            }
        }
        changed.then(|| Self { cookie: jar.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; "), ..self.clone() })
    }

    /// Ask Google for fresh session cookies, as an open YouTube tab does every few
    /// minutes: the `Set-Cookie` headers it answers with. Fails with [`Revoked`] when
    /// Google no longer knows the session.
    pub async fn rotate(&self, http: &reqwest::Client) -> anyhow::Result<Vec<String>> {
        const ACCOUNTS: &str = "https://accounts.youtube.com";
        let resp = http
            .post(format!("{ACCOUNTS}/RotateCookies"))
            .header("User-Agent", UA)
            .header("Origin", ACCOUNTS)
            .header("Content-Type", "application/json")
            .header("Cookie", &self.cookie)
            .body(r#"[000,"-0000000000000000000"]"#)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            anyhow::bail!(Revoked);
        }
        Ok(resp.error_for_status()?.headers().get_all("set-cookie").iter().filter_map(|c| c.to_str().ok().map(str::to_owned)).collect())
    }

    /// The `Set-Cookie` lines collected since the last call.
    pub fn take_replaced(&self) -> Vec<String> {
        std::mem::take(&mut *self.replaced.lock().unwrap())
    }

    /// Note a response's `Set-Cookie` lines for the API to take in.
    pub fn note_replaced(&self, headers: &reqwest::header::HeaderMap) {
        let lines: Vec<String> = headers.get_all("set-cookie").iter().filter_map(|c| c.to_str().ok().map(str::to_owned)).collect();
        if !lines.is_empty() {
            self.replaced.lock().unwrap().extend(lines);
        }
    }

    /// The same Google account and channel as `other`, whatever the cookies' age.
    pub fn same_account(&self, other: &Self) -> bool {
        self.session_index == other.session_index && self.page_id == other.page_id
    }

    async fn post(&self, http: &reqwest::Client, origin: &str, endpoint: &str, client: (&str, &str, &str), body: Value) -> anyhow::Result<Value> {
        let (number, version, ua) = client;
        let req = http
            .post(format!("{origin}/youtubei/v1/{endpoint}?prettyPrint=false"))
            .header("User-Agent", ua)
            .header("X-YouTube-Client-Name", number)
            .header("X-YouTube-Client-Version", version)
            .header("Origin", origin)
            .header("X-Origin", origin);
        let resp = self
            .sign(req, origin)?
            // The Premium check's browse page is about 1 MB, slow through Tor.
            .timeout(std::time::Duration::from_secs(if crate::proxy::active().resolve().is_some() { 30 } else { 10 }))
            .json(&body)
            .send()
            .await?;
        self.note_replaced(resp.headers());
        Ok(resp.error_for_status()?.json().await?)
    }

    /// Upload an audio file to the account's library, like "Upload music" on
    /// music.youtube.com: a resumable upload, started and then sent in one go.
    pub async fn upload(&self, http: &reqwest::Client, path: &Path) -> anyhow::Result<()> {
        const ORIGIN: &str = "https://music.youtube.com";
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let p = path.to_owned();
        let bytes = tokio::task::spawn_blocking(move || std::fs::read(p)).await??;
        anyhow::ensure!(bytes.len() <= 300 * 1024 * 1024, "{name} is larger than 300 MB");
        let req = |url: &str| -> anyhow::Result<reqwest::RequestBuilder> {
            let req = http
                .post(url)
                .header("User-Agent", UA)
                .header("Origin", ORIGIN)
                .header("X-Origin", ORIGIN)
                .header("Content-Type", "application/x-www-form-urlencoded;charset=utf-8");
            self.sign(req, ORIGIN)
        };
        let start = req(&format!("https://upload.youtube.com/upload/usermusic/http?authuser={}", self.session_index))?
            .header("X-Goog-Upload-Command", "start")
            .header("X-Goog-Upload-Header-Content-Length", bytes.len().to_string())
            .header("X-Goog-Upload-Protocol", "resumable")
            .body(format!("filename={name}"))
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await?
            .error_for_status()?;
        let url = start
            .headers()
            .get("X-Goog-Upload-URL")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| anyhow::anyhow!("YouTube Music didn't accept the upload"))?
            .to_owned();
        req(&url)?
            .header("X-Goog-Upload-Command", "upload, finalize")
            .header("X-Goog-Upload-Offset", "0")
            .body(bytes)
            .timeout(std::time::Duration::from_secs(600))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
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
        log::info!("web music: {video_id} itag {} ({} kbps)", f["itag"], f["bitrate"].as_u64().unwrap_or(0) / 1000);
        let size = f["contentLength"].as_str().and_then(|s| s.parse().ok()).ok_or_else(|| anyhow::anyhow!("unknown stream size"))?;
        let duration_ms = f["approxDurationMs"].as_str().and_then(|d| d.parse().ok()).unwrap_or(0);
        let loudness_db = crate::innertube::loudness(&resp, f);

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
        Ok(Stream { url: url.into(), size, duration_ms, loudness_db, opus: false })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_replaced_cookies_but_not_deletions() {
        let s = Session::new("SID=a; __Secure-1PSIDTS=old; SAPISID=k".into(), 0, None);
        let next = s
            .updated([
                "__Secure-1PSIDTS=new; Domain=.youtube.com; Path=/; Expires=Thu, 28-Oct-2027 02:22:00 GMT; Secure; HttpOnly",
                "SIDCC=c; expires=Thu, 28-Oct-2027 02:22:00 GMT; path=/; domain=.youtube.com",
                // Deletions, a host-only cookie and an unchanged one change nothing.
                "SID=gone; Domain=.youtube.com; Expires=Fri, 02-Feb-2024 02:22:00 GMT",
                "SAPISID=; Domain=.youtube.com",
                "SAPISID=x; Domain=.youtube.com; Max-Age=0",
                "YSC=h; Path=/",
                "SID=a; Domain=.youtube.com",
            ])
            .unwrap();
        assert_eq!(next.cookie, "SID=a; __Secure-1PSIDTS=new; SAPISID=k; SIDCC=c");
        assert!(next.updated(["SID=a; Domain=.youtube.com"]).is_none());
        assert!(next.same_account(&s));
    }

    #[test]
    fn carries_over_and_forgets_an_older_versions_session() {
        let dir = std::env::temp_dir().join(format!("ytm-rs-session-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = json!({
            "clients": {"desktop_music": {"data": {"version": "1.0"}}},
            "auth_cookie": {"cookie": "SAPISID=abc; SID=x", "session_index": "1", "channel_syncid": "brand"}
        });
        std::fs::write(dir.join(LEGACY), legacy.to_string()).unwrap();

        let s = Session::load(&dir).unwrap();
        assert_eq!((s.cookie.as_str(), s.session_index, s.page_id.as_deref()), ("SAPISID=abc; SID=x", 1, Some("brand")));
        // Kept in the credential store only.
        assert!(!dir.join(FILE).exists());
        assert!(crate::vault::get(VAULT, &dir).is_some());
        assert!(s.authorization("https://music.youtube.com").unwrap().starts_with("SAPISIDHASH "));

        // A plain account.json from 0.2.12 moves into the store too.
        std::fs::write(dir.join(FILE), serde_json::to_vec(&s).unwrap()).unwrap();
        crate::vault::delete(VAULT, &dir);
        assert_eq!(Session::load(&dir).unwrap().cookie, s.cookie);
        assert!(!dir.join(FILE).exists());

        Session::remove(&dir);
        assert!(Session::load(&dir).is_none());
        assert!(crate::vault::get(VAULT, &dir).is_none());
        let left: Value = serde_json::from_slice(&std::fs::read(dir.join(LEGACY)).unwrap()).unwrap();
        assert!(left.get("auth_cookie").is_none() && left.get("clients").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
