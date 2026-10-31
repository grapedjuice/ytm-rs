//! YouTube Music's own InnerTube API, as music.youtube.com calls it (the `WEB_REMIX`
//! client): request context, signed-in or anonymous headers, visitor data and the
//! current client version. Parsing lives in `ytm.rs`.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::webmusic::{self, Session};

const ORIGIN: &str = "https://music.youtube.com";
/// Used until the live version is read from music.youtube.com.
const FALLBACK_VERSION: &str = "1.20261006.10.00";
/// How long a scraped client version (and visitor ID) is trusted before re-reading it.
const REFRESH: Duration = Duration::from_secs(24 * 3600);
/// Accepts YouTube's cookie consent, so anonymous requests get content, not a consent page.
const CONSENT: &str = "SOCS=CAISAiAD";

/// Client version and visitor ID read from music.youtube.com, cached on disk.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
struct Web {
    version: String,
    visitor_data: String,
    /// Unix seconds when read.
    at: u64,
}

pub struct Api {
    file: PathBuf,
    web: tokio::sync::Mutex<Option<Web>>,
    account: RwLock<Option<Arc<Session>>>,
    /// IANA time zone and its current UTC offset in minutes.
    tz: (String, i32),
    /// Through a proxy, where it connects from was checked this run (a cached visitor
    /// ID may be from another address), and whether YouTube Music is offered there.
    checked: std::sync::atomic::AtomicBool,
    unavailable: std::sync::atomic::AtomicBool,
    /// When reading the page last failed (unix seconds): offline, every call would
    /// otherwise fetch it again first.
    failed_at: std::sync::atomic::AtomicU64,
    /// A signed-in request was answered as signed out (see `take_signed_out`).
    signed_out: std::sync::atomic::AtomicBool,
    /// YouTube refuses the odd signed-in request (403) and takes it when sent again a
    /// moment later; each request retries on its own (`retry_signed_in`). Only a run of
    /// refusals with nothing taken in between means it's limiting the account: then every
    /// signed-in request waits until this (unix ms).
    paused_until: std::sync::atomic::AtomicU64,
    /// Signed-in refusals since the last request YouTube took.
    refusals: std::sync::atomic::AtomicU64,
    /// When the session cookies were last rotated (unix ms).
    rotated_at: std::sync::atomic::AtomicU64,
}

/// music.youtube.com's "not available in your area" page, served to addresses in a
/// country without YouTube Music (most Tor exits): the API then answers with no songs.
#[derive(Debug)]
struct Unavailable;

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("YouTube Music isn't available where your proxy connects from. Try another proxy or server (Settings › Network)")
    }
}

impl std::error::Error for Unavailable {}

impl Api {
    pub fn new(data_dir: &Path) -> Self {
        let file = data_dir.join("ytmapi.json");
        let web = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice::<Web>(&b).ok());
        Self {
            file,
            web: tokio::sync::Mutex::new(web),
            account: RwLock::new(Session::load(data_dir).map(Arc::new)),
            tz: local_tz(),
            checked: Default::default(),
            unavailable: Default::default(),
            failed_at: Default::default(),
            signed_out: Default::default(),
            paused_until: Default::default(),
            refusals: Default::default(),
            rotated_at: Default::default(),
        }
    }

    pub fn account(&self) -> Option<Arc<Session>> {
        self.account.read().unwrap().clone()
    }

    pub fn set_account(&self, s: Option<Session>) {
        *self.account.write().unwrap() = s.map(Arc::new);
    }

    /// Whether a signed-in request came back signed out since the last call: Google
    /// may have revoked the session.
    pub fn take_signed_out(&self) -> bool {
        self.signed_out.swap(false, Relaxed)
    }

    /// Fetch fresh session cookies, as an open YouTube tab does. Fails with
    /// [`webmusic::Revoked`] when Google has signed the session out.
    pub async fn rotate(&self) -> anyhow::Result<()> {
        let Some(account) = self.account() else { return Ok(()) };
        // Not twice within a minute (the session check right after the 10-minute one):
        // Google answers that with 429, as Gemini-API notes, and the cookies are fresh.
        let now = now_ms();
        if now.saturating_sub(self.rotated_at.load(Relaxed)) < 60_000 {
            return Ok(());
        }
        self.take_in_replaced();
        let account = self.account().unwrap_or(account);
        let set = account.rotate(&crate::proxy::http()).await?;
        self.rotated_at.store(now, Relaxed);
        self.keep(&account, set.iter().map(String::as_str));
        Ok(())
    }

    /// Take in the cookies Google replaced while answering requests made outside the API
    /// (the Premium check, the web player).
    fn take_in_replaced(&self) {
        let Some(account) = self.account() else { return };
        let lines = account.take_replaced();
        if !lines.is_empty() {
            self.keep(&account, lines.iter().map(String::as_str));
        }
    }

    /// Fill in the signed-in browser's visitor ID for a session from before it was kept.
    pub async fn ensure_visitor(&self) {
        let Some(account) = self.account().filter(|a| a.visitor_data.is_none()) else { return };
        match self.session_from_cookie(&account.cookie).await {
            Ok(fresh) => {
                let mut slot = self.account.write().unwrap();
                if let Some(cur) = slot.as_ref().filter(|s| s.same_account(&account)) {
                    let mut next = (**cur).clone();
                    next.visitor_data = fresh.visitor_data;
                    let next = Arc::new(next);
                    *slot = Some(next.clone());
                    drop(slot);
                    if let Some(dir) = self.file.parent() {
                        next.save(dir);
                    }
                }
            }
            Err(e) => log::info!("couldn't read the account's visitor ID: {e:#}"),
        }
    }

    /// Wait out a pause YouTube's refusals started (see `paused_until`).
    async fn wait_if_paused(&self) {
        // Until it's over: a refusal meanwhile makes it longer.
        loop {
            let (until, now) = (self.paused_until.load(Relaxed), now_ms());
            if until <= now {
                return;
            }
            tokio::time::sleep(Duration::from_millis(until - now)).await;
        }
    }

    /// A signed-in request was refused. A few in a row are normal (each is sent again);
    /// a long run pauses them all, longer while it goes on (5 s, doubling up to 40 s).
    /// Pausing on every refusal made one stray 403 hold up the whole app for seconds.
    fn refused(&self, endpoint: &str) {
        const RUN: u64 = 6;
        let n = self.refusals.fetch_add(1, Relaxed) + 1;
        if n < RUN {
            return;
        }
        let ms = 5_000 << (n - RUN).min(3);
        let until = now_ms() + ms;
        if self.paused_until.fetch_max(until, Relaxed) < until {
            log::info!("{endpoint}: YouTube Music is limiting this account's requests; pausing them for {} s", ms / 1000);
        }
    }

    /// Whether signed-in requests are paused because YouTube is limiting the account.
    pub fn limited(&self) -> bool {
        self.paused_until.load(Relaxed) > now_ms()
    }

    /// Keep the cookies Google replaced while answering a request made as `used`.
    pub fn keep<'a>(&self, used: &Session, set_cookies: impl IntoIterator<Item = &'a str>) {
        let mut slot = self.account.write().unwrap();
        // Onto the current session: another request may have updated it meanwhile.
        let Some(new) = slot.as_ref().filter(|s| s.same_account(used)).and_then(|s| s.updated(set_cookies)) else { return };
        let new = Arc::new(new);
        *slot = Some(new.clone());
        drop(slot);
        if let Some(dir) = self.file.parent() {
            new.save(dir);
        }
    }

    /// The cached client version and visitor ID, read from music.youtube.com when
    /// missing, a day old, or `fresh` (a request was refused with the current ones).
    async fn web(&self, fresh: bool) -> Web {
        let mut slot = self.web.lock().await;
        let now = now_secs();
        // Tor changes exit every ten minutes or so, so check about as often there.
        let refresh = if crate::proxy::can_switch_exit() { 9 * 60 } else { REFRESH.as_secs() };
        let unchecked = crate::proxy::active().resolve().is_some() && !self.checked.load(Relaxed);
        let stale = unchecked || slot.as_ref().is_none_or(|w| fresh || now.saturating_sub(w.at) > refresh || w.visitor_data.is_empty());
        let backing_off = now.saturating_sub(self.failed_at.load(Relaxed)) < 30;
        if stale && !(backing_off && !unchecked) {
            let mut switched = false;
            loop {
                match self.read_web().await {
                    Ok(w) => {
                        let _ = std::fs::write(&self.file, serde_json::to_vec(&w).unwrap_or_default());
                        *slot = Some(w);
                        self.unavailable.store(false, Relaxed);
                    }
                    Err(e) if e.is::<Unavailable>() => {
                        // Once per check: a working exit that fails again right away won't be fixed by more.
                        if !std::mem::replace(&mut switched, true) && crate::proxy::new_circuit(offered_through).await {
                            continue;
                        }
                        log::warn!("{e}");
                        self.unavailable.store(true, Relaxed);
                    }
                    Err(e) => {
                        log::warn!("couldn't read music.youtube.com: {e:#}");
                        self.failed_at.store(now, Relaxed);
                    }
                }
                break;
            }
            self.checked.store(true, Relaxed);
        }
        slot.clone().unwrap_or_else(|| Web { version: FALLBACK_VERSION.into(), ..Default::default() })
    }

    async fn read_web(&self) -> anyhow::Result<Web> {
        let resp = crate::proxy::http()
            .get(ORIGIN)
            .header("User-Agent", webmusic::UA)
            .header("Cookie", CONSENT)
            .timeout(Duration::from_secs(20))
            .send()
            .await?
            .error_for_status()?;
        // The visitor ID also arrives as a cookie; the page's ytcfg has it too.
        let cookie = resp.headers().get_all("set-cookie").iter().find_map(|c| {
            let v = c.to_str().ok()?.strip_prefix("__Secure-YEC=")?.split(';').next()?;
            (!v.is_empty()).then(|| v.to_owned())
        });
        let html = resp.text().await?;
        // The full page carries its region; the "not available" one is a short stub.
        if !html.contains("\"INNERTUBE_CONTEXT_GL\"") && html.len() < 100_000 {
            anyhow::bail!(Unavailable);
        }
        let visitor_data = cookie
            .or_else(|| between(&html, "\"VISITOR_DATA\":\"", "\"").map(str::to_owned))
            .ok_or_else(|| anyhow::anyhow!("no visitor data on the page"))?;
        let version = between(&html, "\"INNERTUBE_CLIENT_VERSION\":\"", "\"")
            .or_else(|| between(&html, "\"INNERTUBE_CONTEXT_CLIENT_VERSION\":\"", "\""))
            .unwrap_or(FALLBACK_VERSION)
            .to_owned();
        log::info!("music.youtube.com client {version}");
        Ok(Web { version, visitor_data, at: now_secs() })
    }

    /// An anonymous visitor ID (the visionOS player needs one too).
    pub async fn visitor_data(&self, fresh: bool) -> anyhow::Result<String> {
        let w = self.web(fresh).await;
        anyhow::ensure!(!w.visitor_data.is_empty(), "couldn't get a YouTube visitor ID");
        Ok(w.visitor_data)
    }

    /// POST `body` to `/youtubei/v1/<endpoint>`. `auth`: as the signed-in account, when
    /// there is one; otherwise anonymously as the visitor.
    pub async fn call(&self, endpoint: &str, body: Value, auth: bool) -> anyhow::Result<Value> {
        let account = if auth { self.account() } else { None };
        match self.call_once(endpoint, &body, account.as_deref(), false).await {
            Err(e) if account.is_none() && is_retryable(&e) => self.retry_anonymous(endpoint, &body, e).await,
            Err(e) if account.is_some() && is_refused(&e) => self.retry_signed_in(endpoint, &body, account.as_deref(), e).await,
            r => r,
        }
    }

    /// Like `call`, but a signed-in account is required.
    pub async fn call_signed_in(&self, endpoint: &str, body: Value) -> anyhow::Result<Value> {
        let account = self.account().ok_or_else(|| anyhow::anyhow!("sign in first"))?;
        match self.call_once(endpoint, &body, Some(&account), false).await {
            Err(e) if is_refused(&e) => self.retry_signed_in(endpoint, &body, Some(&account), e).await,
            r => r,
        }
    }

    /// YouTube refuses the odd anonymous request (403) too, at random: the same one goes
    /// through a moment later, with the same visitor ID (a new one is refused as often).
    /// So send it again shortly, up to three times, and read music.youtube.com for a
    /// fresh visitor ID and client version only when the request itself looks stale
    /// (400) or for the last try. Reading the page (about 1 MB, one at a time) for every
    /// refusal held a whole transfer up.
    async fn retry_anonymous(&self, endpoint: &str, body: &Value, e: anyhow::Error) -> anyhow::Result<Value> {
        let mut last = e;
        for (i, wait) in [500, 1_500, 3_000].into_iter().enumerate() {
            let stale = last.downcast_ref::<ApiError>().is_some_and(|a| a.status == 400);
            let fresh = stale || i == 2;
            log::debug!("{endpoint}: {last:#}; trying again{}", if fresh { " with a fresh visitor ID" } else { "" });
            tokio::time::sleep(Duration::from_millis(wait + now_ms() % 300)).await;
            match self.call_once(endpoint, body, None, fresh).await {
                Err(e) if is_retryable(&e) => last = e,
                r => return r,
            }
        }
        Err(last)
    }

    /// YouTube refuses the odd signed-in request (403) and takes the same one a moment
    /// later. A refused request changed nothing, so it's safe to send again: up to three
    /// more times, 0.7, 2 and 4 s apart (a little jitter so parallel ones don't line up),
    /// as yt-dlp retries; longer only if they're all being refused (`refused`).
    async fn retry_signed_in(&self, endpoint: &str, body: &Value, account: Option<&Session>, e: anyhow::Error) -> anyhow::Result<Value> {
        let mut last = e;
        for wait in [700, 2_000, 4_000] {
            log::debug!("{endpoint}: {last:#}; trying again");
            tokio::time::sleep(Duration::from_millis(wait + now_ms() % 300)).await;
            match self.call_once(endpoint, body, account, false).await {
                Err(e) if is_refused(&e) => last = e,
                r => return r,
            }
        }
        Err(last)
    }

    async fn call_once(&self, endpoint: &str, body: &Value, account: Option<&Session>, fresh: bool) -> anyhow::Result<Value> {
        let mut web = self.web(fresh).await;
        if account.is_some() {
            self.wait_if_paused().await;
            self.take_in_replaced();
        }
        // Signed in, the account's own visitor ID, as the site sends.
        let current = account.and_then(|_| self.account());
        let account = current.as_deref().filter(|c| account.is_some_and(|a| a.same_account(c))).or(account);
        if let Some(v) = account.and_then(|a| a.visitor_data.clone()) {
            web.visitor_data = v;
        }
        anyhow::ensure!(!self.unavailable.load(Relaxed), Unavailable);
        let mut payload = body.clone();
        payload["context"] = json!({
            "client": {
                "clientName": "WEB_REMIX",
                "clientVersion": web.version,
                "platform": "DESKTOP",
                "originalUrl": ORIGIN,
                "visitorData": web.visitor_data,
                "hl": "en",
                "gl": "US",
                "timeZone": self.tz.0,
                "utcOffsetMinutes": self.tz.1,
            },
            "request": {"useSsl": true},
            "user": {"lockedSafetyMode": false},
        });
        let mut req = crate::proxy::http()
            .post(format!("{ORIGIN}/youtubei/v1/{endpoint}?prettyPrint=false"))
            .header("User-Agent", webmusic::UA)
            .header("Origin", ORIGIN)
            .header("X-Origin", ORIGIN)
            .header("Referer", ORIGIN)
            .header("X-YouTube-Client-Name", "67")
            .header("X-YouTube-Client-Version", &web.version)
            .timeout(Duration::from_secs(15));
        if !web.visitor_data.is_empty() {
            req = req.header("X-Goog-Visitor-Id", &web.visitor_data);
        }
        req = match account {
            Some(a) => a.sign(req, ORIGIN)?,
            None => req.header("Cookie", CONSENT),
        };
        let resp = req.json(&payload).send().await?;
        if let Some(a) = account {
            self.keep(a, resp.headers().get_all("set-cookie").iter().filter_map(|c| c.to_str().ok()));
        }
        let status = resp.status();
        let text = resp.text().await?;
        if status.is_success() {
            if account.is_some() {
                self.refusals.store(0, Relaxed);
            }
        } else {
            let msg = serde_json::from_str::<Value>(&text).ok().and_then(|v| v["error"]["message"].as_str().map(str::to_owned));
            let e = ApiError { status: status.as_u16(), msg: msg.unwrap_or_default() };
            if account.is_some() && is_throttled(&e) {
                self.refused(endpoint);
            }
            anyhow::bail!(e);
        }
        let v: Value = serde_json::from_str(&text)?;
        if account.is_some() && logged_out(&v) {
            log::warn!("{endpoint}: signed-in request answered as signed out");
            self.signed_out.store(true, Relaxed);
        }
        Ok(v)
    }

    /// GET `url` as the signed-in account, the way the web player sends its stats pings.
    pub async fn get_signed_in(&self, url: &str, query: &[(&str, &str)]) -> anyhow::Result<()> {
        let account = self.account().ok_or_else(|| anyhow::anyhow!("sign in first"))?;
        let req = crate::proxy::http()
            .get(url)
            .query(query)
            .header("User-Agent", webmusic::UA)
            .header("Origin", ORIGIN)
            .header("X-Origin", ORIGIN)
            .header("Referer", ORIGIN)
            .timeout(Duration::from_secs(15));
        let resp = account.sign(req, ORIGIN)?.send().await?;
        self.keep(&account, resp.headers().get_all("set-cookie").iter().filter_map(|c| c.to_str().ok()));
        resp.error_for_status()?;
        Ok(())
    }

    /// Check a signed-in cookie with YouTube and read which account/channel it is for.
    pub async fn session_from_cookie(&self, cookie: &str) -> anyhow::Result<Session> {
        let resp = crate::proxy::http()
            .get("https://www.youtube.com/results?search_query=")
            .header("User-Agent", webmusic::UA)
            .header("Cookie", cookie)
            .timeout(Duration::from_secs(15))
            .send()
            .await?;
        // Checking the current session: keep what Google replaced while answering.
        if let Some(a) = self.account().filter(|a| a.cookie == cookie) {
            self.keep(&a, resp.headers().get_all("set-cookie").iter().filter_map(|c| c.to_str().ok()));
        }
        let html = resp.error_for_status()?.text().await?;
        anyhow::ensure!(html.contains("\"USER_SESSION_ID\":\""), NotSignedIn);
        // `datasyncId` is "<page>||<user>" for a brand channel and "<user>||" for the
        // account's own channel; requests for a brand channel name it in X-Goog-PageId.
        let page_id = between(&html, "\"datasyncId\":\"", "\"").and_then(|d| {
            let (page, user) = d.split_once("||")?;
            (!user.is_empty()).then(|| page.to_owned())
        });
        let session_index = between(&html, "\"SESSION_INDEX\":\"", "\"").and_then(|s| s.parse().ok()).unwrap_or(0);
        let mut session = Session::new(cookie.to_owned(), session_index, page_id);
        session.visitor_data = between(&html, "\"VISITOR_DATA\":\"", "\"").filter(|v| !v.is_empty()).map(str::to_owned);
        Ok(session)
    }
}

/// youtube.com, loaded with the cookies, didn't show a signed-in page.
#[derive(Debug)]
pub struct NotSignedIn;

impl std::fmt::Display for NotSignedIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("YouTube didn't recognise a signed-in session in those cookies")
    }
}

impl std::error::Error for NotSignedIn {}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// Whether YouTube answered as signed out.
pub fn logged_out(v: &Value) -> bool {
    v["responseContext"]["serviceTrackingParams"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|s| s["params"].as_array().into_iter().flatten())
        .any(|p| p["key"] == "logged_in" && p["value"] == "0")
}

#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub msg: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.status, self.msg.is_empty()) {
            (401, _) => f.write_str("YouTube Music signed this session out; sign in again"),
            (_, true) => write!(f, "YouTube Music returned HTTP {}", self.status),
            (_, false) => write!(f, "YouTube Music returned HTTP {}: {}", self.status, self.msg),
        }
    }
}

impl std::error::Error for ApiError {}

fn is_retryable(e: &anyhow::Error) -> bool {
    match e.downcast_ref::<ApiError>() {
        Some(a) => a.status == 400 || a.status == 403 || a.status == 429 || a.status >= 500,
        None => e.downcast_ref::<reqwest::Error>().is_some_and(|r| r.is_timeout() || r.is_connect()),
    }
}

/// Whether music.youtube.com is offered through `http`: its "not available in your
/// area" stub is a couple of KB, so a few KB of the page tell.
pub async fn offered_through(http: &reqwest::Client) -> bool {
    let Ok(mut resp) = http.get(ORIGIN).header("User-Agent", webmusic::UA).header("Cookie", CONSENT).timeout(Duration::from_secs(15)).send().await else {
        return false;
    };
    let mut head = Vec::new();
    while head.len() < 16 * 1024 {
        match resp.chunk().await {
            Ok(Some(b)) => head.extend_from_slice(&b),
            Ok(None) => return false,
            Err(_) => return false,
        }
    }
    resp.status().is_success()
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The text between `start` and the next `end`.
fn is_refused(e: &anyhow::Error) -> bool {
    e.downcast_ref::<ApiError>().is_some_and(is_throttled)
}

/// A 403 while YouTube limits requests comes with no message; one that says the caller
/// lacks permission (someone else's playlist, say) won't change by waiting.
fn is_throttled(e: &ApiError) -> bool {
    e.status == 403 && !e.msg.to_ascii_lowercase().contains("permission")
}

pub fn between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let i = s.find(start)? + start.len();
    let j = s[i..].find(end)?;
    Some(&s[i..i + j])
}

/// The system's IANA time zone and current UTC offset, sent with every request the
/// way the web client does (YouTube Music uses them for "Today", history and charts).
fn local_tz() -> (String, i32) {
    let name = localzone::get_local_zone().unwrap_or_else(|| "UTC".into());
    (name, utc_offset_minutes())
}

#[cfg(windows)]
fn utc_offset_minutes() -> i32 {
    #[repr(C)]
    struct SystemTime([u16; 8]);
    #[repr(C)]
    struct TimeZoneInformation {
        bias: i32,
        standard_name: [u16; 32],
        standard_date: SystemTime,
        standard_bias: i32,
        daylight_name: [u16; 32],
        daylight_date: SystemTime,
        daylight_bias: i32,
    }
    unsafe extern "system" {
        fn GetTimeZoneInformation(info: *mut TimeZoneInformation) -> u32;
    }
    // SAFETY: a plain out-parameter struct with the documented layout.
    unsafe {
        let mut info: TimeZoneInformation = std::mem::zeroed();
        match GetTimeZoneInformation(&mut info) {
            1 => -(info.bias + info.standard_bias),
            2 => -(info.bias + info.daylight_bias),
            0 => -info.bias,
            _ => 0,
        }
    }
}

#[cfg(unix)]
fn utc_offset_minutes() -> i32 {
    // SAFETY: localtime_r fills the out-parameter from a valid time_t.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() { 0 } else { (tm.tm_gmtoff / 60) as i32 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stray_refusals_dont_pause_everything() {
        let api = Api::new(&std::env::temp_dir().join("ytm-rs-pause-test"));
        for _ in 0..5 {
            api.refused("search");
        }
        assert!(!api.limited(), "a few refusals only retry the requests refused");
        api.refused("search");
        assert!(api.limited(), "a long run pauses them all");
        let wait = api.paused_until.load(Relaxed) - now_ms();
        assert!((4_000..=5_000).contains(&wait), "{wait}");
        // Nothing past 40 s, however long it goes on.
        for _ in 0..10 {
            api.refused("search");
        }
        assert!(api.paused_until.load(Relaxed) - now_ms() <= 40_000);
        // Not permission errors.
        assert!(!is_throttled(&ApiError { status: 403, msg: "The caller does not have permission".into() }));
        assert!(is_throttled(&ApiError { status: 403, msg: String::new() }));
    }

    #[test]
    fn between_finds_ytcfg_values() {
        let html = r#"ytcfg.set({"INNERTUBE_CLIENT_VERSION":"1.20261006.10.00","VISITOR_DATA":"Cgt4eXo%3D"});"#;
        assert_eq!(between(html, "\"INNERTUBE_CLIENT_VERSION\":\"", "\""), Some("1.20261006.10.00"));
        assert_eq!(between(html, "\"VISITOR_DATA\":\"", "\""), Some("Cgt4eXo%3D"));
        assert_eq!(between(html, "\"MISSING\":\"", "\""), None);
    }
}
