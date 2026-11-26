//! Spotify the way open.spotify.com's web player reads it (from spot2ytm, after SpotAPI's
//! `PublicPlaylist`): a TOTP-gated access token, a client token, and persisted GraphQL
//! queries.
//!
//! Anonymously it reads public playlists. With the `sp_dc` cookie the Spotify sign-in
//! window captures, it reads as that user: their library, Liked Songs and private
//! playlists, without a developer app or Premium.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::Engine;
use futures_util::{StreamExt, TryStreamExt, stream};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha1::Sha1;

use crate::ytmapi::between;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";
const SECRETS_URL: &str = "https://code.thetadev.de/ThetaDev/spotify-secrets/raw/branch/main/secrets/secretDict.json";
/// Used when the secrets list can't be fetched (SpotAPI's fallback, version 61).
const FALLBACK_SECRET: (u32, &[u8]) = (
    61,
    &[44, 55, 47, 42, 70, 40, 34, 114, 76, 74, 50, 111, 120, 97, 75, 76, 94, 102, 43, 69, 49, 120, 118, 80, 64, 78],
);
/// The largest playlist page the pathfinder API serves.
const PAGE: usize = 343;
/// Liked Songs and the library come in smaller pages.
const LIBRARY_PAGE: usize = 50;
const CDN: &str = "https://open.spotifycdn.com/cdn/build/web-player/";
/// The persisted queries this reads, found in the web player's bundle.
const OPERATIONS: [&str; 4] = ["fetchPlaylist", "fetchLibraryTracks", "libraryV3", "profileAttributes"];
/// Where the query hashes are cached, so a new bundle (several MB) is read once per web-player release.
const HASH_CACHE: &str = "spotify_queries.json";

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Track {
    pub name: String,
    pub artists: Vec<String>,
    /// A file from someone's computer, not a Spotify song: just a title and an artist.
    #[serde(default)]
    pub local: bool,
    /// Album art, about 64 px.
    #[serde(default)]
    pub art: Option<String>,
    /// Length in seconds, when Spotify gives one.
    #[serde(default)]
    pub duration: Option<u32>,
}

impl Track {
    pub fn artist_line(&self) -> String {
        self.artists.join(", ")
    }
}

pub struct Playlist {
    pub name: String,
    pub tracks: Vec<Track>,
    /// Removed songs, podcast episodes and other items with no title or artist.
    pub skipped: usize,
}

/// What to transfer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Source {
    Playlist(String),
    /// The signed-in user's Liked Songs.
    Liked,
}

impl Source {
    /// An open.spotify.com playlist link, a `spotify:playlist:` URI, or a bare id.
    pub fn parse(link: &str) -> Result<Self> {
        let link = link.trim();
        if link.ends_with("/collection/tracks") {
            return Ok(Self::Liked);
        }
        let id = if let Some(rest) = link.strip_prefix("spotify:playlist:") {
            rest
        } else if let Some((_, rest)) = link.split_once("://") {
            let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
            ensure!(matches!(host, "open.spotify.com" | "www.open.spotify.com"), crate::tr!("transfer-error-not-spotify-link"));
            let path = path.split(['?', '#']).next().unwrap_or("");
            let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty() && !p.starts_with("intl-")).collect();
            match parts[..] {
                ["playlist", id] => id,
                _ => bail!(crate::tr!("transfer-error-not-playlist-link")),
            }
        } else {
            link
        };
        ensure!(id.len() == 22 && id.bytes().all(|b| b.is_ascii_alphanumeric()), crate::tr!("transfer-error-not-spotify-playlist"));
        Ok(Self::Playlist(id.to_owned()))
    }

    /// The key saved progress is filed under.
    pub fn key(&self) -> &str {
        match self {
            Self::Playlist(id) => id,
            Self::Liked => "liked",
        }
    }
}

/// A playlist in the user's library, as the source list shows it.
#[derive(Clone, Debug)]
pub struct Entry {
    pub source: Source,
    pub name: String,
    pub owner: String,
    pub count: Option<usize>,
    pub art: Option<String>,
}

/// The signed-in user: their name and library.
pub struct Account {
    pub name: String,
    pub library: Vec<Entry>,
}

pub struct Client {
    http: reqwest::Client,
    access_token: String,
    client_token: String,
    client_version: String,
    hashes: Hashes,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Hashes {
    /// The bundle they were read from.
    bundle: String,
    ops: std::collections::BTreeMap<String, String>,
}

impl Client {
    /// `sp_dc` signs in as the user it belongs to; `None` is anonymous.
    pub async fn connect(sp_dc: Option<&str>, data_dir: &Path) -> Result<Self> {
        let http = crate::proxy::apply(reqwest::Client::builder())
            .user_agent(UA)
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(20))
            .build()?;

        let resp = http.get("https://open.spotify.com").send().await?.error_for_status()?;
        let device_id = resp
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok()?.strip_prefix("sp_t=")?.split(';').next().map(str::to_owned))
            .next()
            .unwrap_or_default();
        let html = resp.text().await?;

        let config = between(&html, r#"<script id="appServerConfig" type="text/plain">"#, "</script>")
            .context("open.spotify.com is missing appServerConfig")?;
        let config: Value = serde_json::from_slice(&base64::engine::general_purpose::STANDARD.decode(config.trim())?)?;
        let client_version = config["clientVersion"].as_str().context("appServerConfig has no clientVersion")?.to_owned();
        let bundle = script_srcs(&html)
            .find(|s| s.contains("web-player/web-player") && s.ends_with(".js"))
            .context("open.spotify.com has no web-player bundle")?
            .to_owned();

        // The access token, client token and query hashes don't depend on each other.
        let (token, hashes) = tokio::try_join!(access_token(&http, sp_dc, &device_id), query_hashes(&http, &bundle, data_dir))?;
        let (access_token, client_id) = token;
        let client_token = client_token(&http, &client_version, &client_id, &device_id).await?;
        Ok(Self { http, access_token, client_token, client_version, hashes })
    }

    fn hash(&self, operation: &str) -> Result<&str> {
        self.hashes.ops.get(operation).map(String::as_str).ok_or_else(|| anyhow!("couldn't find Spotify's {operation} query"))
    }

    /// One persisted GraphQL query, retried on server errors and rate limits.
    async fn query(&self, operation: &str, variables: Value) -> Result<Value> {
        let extensions = json!({"persistedQuery": {"version": 1, "sha256Hash": self.hash(operation)?}});
        let mut tries = 0;
        loop {
            let resp = self
                .http
                .post("https://api-partner.spotify.com/pathfinder/v1/query")
                .query(&[("operationName", operation), ("variables", &variables.to_string()), ("extensions", &extensions.to_string())])
                .bearer_auth(&self.access_token)
                .header("Client-Token", &self.client_token)
                .header("Spotify-App-Version", &self.client_version)
                .header("Accept-Language", "en")
                .header("Content-Length", "0")
                .send()
                .await;
            tries += 1;
            match resp.and_then(|r| r.error_for_status()) {
                Ok(r) => return Ok(r.json().await?),
                Err(e) if tries < 3 && e.status().is_none_or(|s| s.is_server_error() || s.as_u16() == 429) => {
                    tokio::time::sleep(std::time::Duration::from_millis(500 * tries)).await;
                }
                Err(e) => return Err(e).with_context(|| format!("Spotify's {operation} query failed")),
            }
        }
    }

    /// The signed-in user's name and their playlists, Liked Songs first.
    pub async fn account(&self) -> Result<Account> {
        let (profile, library) = tokio::join!(self.query("profileAttributes", json!({})), self.library());
        let name = profile.ok().and_then(|p| p["data"]["me"]["profile"]["name"].as_str().map(str::to_owned)).unwrap_or_default();
        // The Web API lists the same playlists if the library query ever changes shape.
        let mut library = match library {
            Ok(l) if !l.is_empty() => l,
            other => {
                if let Err(e) = &other {
                    log::warn!("spotify libraryV3: {e:#}; trying the Web API");
                }
                match self.web_api_playlists().await {
                    Ok(l) => l,
                    Err(e) => {
                        log::warn!("spotify /me/playlists: {e:#}");
                        other?
                    }
                }
            }
        };
        if !library.iter().any(|e| e.source == Source::Liked) {
            let count = self
                .query("fetchLibraryTracks", json!({"offset": 0, "limit": 1}))
                .await
                .ok()
                .and_then(|b| b["data"]["me"]["library"]["tracks"]["totalCount"].as_u64())
                .map(|c| c as usize);
            library.insert(0, Entry { source: Source::Liked, name: crate::tr!("transfer-liked-songs").to_owned(), owner: String::new(), count, art: None });
        }
        Ok(Account { name, library })
    }

    async fn web_api_playlists(&self) -> Result<Vec<Entry>> {
        let mut out = Vec::new();
        let mut url = Some("https://api.spotify.com/v1/me/playlists?limit=50".to_owned());
        while let Some(u) = url.take() {
            let page: Value = self.http.get(&u).bearer_auth(&self.access_token).send().await?.error_for_status()?.json().await?;
            for p in page["items"].as_array().into_iter().flatten() {
                let (Some(id), Some(name)) = (p["id"].as_str(), p["name"].as_str()) else { continue };
                out.push(Entry {
                    source: Source::Playlist(id.to_owned()),
                    name: name.trim().to_owned(),
                    owner: p["owner"]["display_name"].as_str().unwrap_or_default().to_owned(),
                    count: p["tracks"]["total"].as_u64().or(p["items"]["total"].as_u64()).map(|c| c as usize),
                    art: p["images"].as_array().and_then(|i| i.last()).and_then(|i| i["url"].as_str()).map(str::to_owned),
                });
            }
            url = page["next"].as_str().map(str::to_owned);
        }
        Ok(out)
    }

    async fn library(&self) -> Result<Vec<Entry>> {
        let page = |offset: usize| async move {
            let variables = json!({
                "filters": ["Playlists"], "order": null, "textFilter": null, "features": ["LIKED_SONGS"],
                "limit": LIBRARY_PAGE, "offset": offset, "flatten": true, "folderUri": null,
                "includeFoldersWhenFlattening": true,
            });
            let body = self.query("libraryV3", variables).await?;
            let lib = body["data"]["me"]["libraryV3"].clone();
            ensure!(lib.is_object(), crate::tr!("transfer-error-library-sign-in"));
            Ok::<_, anyhow::Error>(lib)
        };
        let first = page(0).await?;
        let total = first["totalCount"].as_u64().unwrap_or(0) as usize;
        let mut items: Vec<Value> = first["items"].as_array().cloned().unwrap_or_default();
        let rest: Vec<Value> = stream::iter((LIBRARY_PAGE..total).step_by(LIBRARY_PAGE)).map(page).buffered(4).try_collect().await?;
        for mut p in rest {
            if let Value::Array(more) = p["items"].take() {
                items.extend(more);
            }
        }
        Ok(items.iter().filter_map(|i| library_entry(&i["item"])).collect())
    }

    async fn page(&self, id: &str, offset: usize, limit: usize) -> Result<Value> {
        let variables = json!({"uri": format!("spotify:playlist:{id}"), "offset": offset, "limit": limit, "enableWatchFeedEntrypoint": false});
        let body = self.query("fetchPlaylist", variables).await?;
        let playlist = &body["data"]["playlistV2"];
        if playlist.is_null() || playlist["__typename"] == "NotFound" {
            bail!(crate::tr!("transfer-error-playlist-not-found"));
        }
        Ok(playlist.clone())
    }

    /// A playlist's name, owner, size and cover, from its first page.
    pub async fn playlist_info(&self, id: &str) -> Result<Entry> {
        let p = self.page(id, 0, 1).await?;
        let art = p["images"]["items"].as_array().and_then(|i| i.first()).and_then(|i| art(&i["sources"]));
        Ok(Entry {
            source: Source::Playlist(id.to_owned()),
            name: p["name"].as_str().unwrap_or_default().trim().to_owned(),
            owner: p["ownerV2"]["data"]["name"].as_str().unwrap_or_default().to_owned(),
            count: p["content"]["totalCount"].as_u64().map(|c| c as usize),
            art,
        })
    }

    /// The signed-in user's Liked Songs: the first page for the total, then the rest at once.
    pub async fn liked(&self, on_progress: impl Fn(usize, usize)) -> Result<Playlist> {
        let page = |offset: usize| async move {
            let body = self.query("fetchLibraryTracks", json!({"offset": offset, "limit": LIBRARY_PAGE})).await?;
            let tracks = &body["data"]["me"]["library"]["tracks"];
            ensure!(tracks.is_object(), crate::tr!("transfer-error-liked-sign-in"));
            Ok::<_, anyhow::Error>((offset, tracks.clone()))
        };
        let (_, first) = page(0).await?;
        let mut items: Vec<Value> = first["items"].as_array().cloned().unwrap_or_default();
        let total = (first["totalCount"].as_u64().unwrap_or(0) as usize).max(items.len());
        let done = std::sync::atomic::AtomicUsize::new(items.len());
        on_progress(items.len(), total);

        let mut pages: Vec<(usize, Value)> = stream::iter((LIBRARY_PAGE..total).step_by(LIBRARY_PAGE))
            .map(page)
            .buffer_unordered(6)
            .inspect_ok(|(_, p)| {
                let n = p["items"].as_array().map_or(0, Vec::len);
                on_progress(done.fetch_add(n, std::sync::atomic::Ordering::Relaxed) + n, total)
            })
            .try_collect()
            .await?;
        pages.sort_by_key(|(offset, _)| *offset);
        for (_, mut p) in pages {
            if let Value::Array(page) = p["items"].take() {
                items.extend(page);
            }
        }
        to_playlist(crate::tr!("transfer-liked-songs").to_owned(), &items)
    }

    /// The whole playlist: one page for the name and total, then every other page at once.
    pub async fn playlist(&self, id: &str, on_progress: impl Fn(usize, usize)) -> Result<Playlist> {
        let first = self.page(id, 0, PAGE).await?;
        let name = first["name"].as_str().map(str::trim).filter(|s| !s.is_empty()).context("Spotify returned no playlist name")?.to_owned();
        let total = first["content"]["totalCount"].as_u64().unwrap_or(0) as usize;
        let mut items: Vec<Value> = first["content"]["items"].as_array().cloned().unwrap_or_default();
        let total = total.max(items.len());
        let done = std::sync::atomic::AtomicUsize::new(items.len());
        on_progress(items.len(), total);

        let offsets: Vec<usize> = (PAGE..total).step_by(PAGE).collect();
        let mut pages: Vec<(usize, Vec<Value>)> = stream::iter(offsets)
            .map(|offset| async move {
                let page = self.page(id, offset, PAGE).await?;
                Ok::<_, anyhow::Error>((offset, page["content"]["items"].as_array().cloned().unwrap_or_default()))
            })
            .buffer_unordered(6)
            .inspect_ok(|(_, page)| on_progress(done.fetch_add(page.len(), std::sync::atomic::Ordering::Relaxed) + page.len(), total))
            .try_collect()
            .await?;
        pages.sort_by_key(|(offset, _)| *offset);
        items.extend(pages.into_iter().flat_map(|(_, page)| page));
        to_playlist(name, &items)
    }
}

fn library_entry(item: &Value) -> Option<Entry> {
    let data = if item["data"].is_object() { &item["data"] } else { item };
    match data["__typename"].as_str()? {
        "PseudoPlaylist" if data["uri"].as_str().is_some_and(|u| u.ends_with(":collection")) || data["name"] == "Liked Songs" => Some(Entry {
            source: Source::Liked,
            name: crate::tr!("transfer-liked-songs").to_owned(),
            owner: String::new(),
            count: data["count"].as_u64().map(|c| c as usize),
            art: None,
        }),
        "Playlist" => {
            let id = data["uri"].as_str()?.strip_prefix("spotify:playlist:")?.to_owned();
            let name = data["name"].as_str()?.trim().to_owned();
            let owner = data["ownerV2"]["data"]["name"].as_str().unwrap_or_default().to_owned();
            let art = data["images"]["items"].as_array().and_then(|i| i.first()).and_then(|i| art(&i["sources"]));
            Some(Entry { source: Source::Playlist(id), name, owner, count: None, art })
        }
        _ => None,
    }
}

/// The smallest image at least 60 px wide (Spotify lists 64, 300 and 640).
fn art(sources: &Value) -> Option<String> {
    let list = sources.as_array()?;
    let width = |s: &Value| s["width"].as_u64().unwrap_or(0);
    list.iter()
        .filter(|s| width(s) >= 60)
        .min_by_key(|s| width(s))
        .or_else(|| list.first())
        .and_then(|s| s["url"].as_str())
        .map(str::to_owned)
}

fn to_playlist(name: String, items: &[Value]) -> Result<Playlist> {
    let mut tracks = Vec::with_capacity(items.len());
    let mut skipped = 0;
    for item in items {
        match parse_track(item) {
            Some(t) => tracks.push(t),
            None => skipped += 1,
        }
    }
    ensure!(!tracks.is_empty(), crate::tr!("transfer-error-no-songs"));
    Ok(Playlist { name, tracks, skipped })
}

fn parse_track(item: &Value) -> Option<Track> {
    let mut track = ["itemV2", "item", "track"].iter().map(|k| &item[k]).find(|v| v.is_object()).unwrap_or(item);
    if track["data"].is_object() {
        track = &track["data"];
    }
    let name = track["name"].as_str()?.trim();
    if track["__typename"] == "LocalTrack" {
        let artists = split_artists(track["artistName"].as_str().unwrap_or_default());
        return (!name.is_empty()).then(|| Track { name: name.to_owned(), artists, local: true, art: None, duration: None });
    }
    let artists = track["artists"]["items"].as_array().or(track["artists"].as_array())?;
    let artists: Vec<String> = artists
        .iter()
        .filter_map(|a| a["profile"]["name"].as_str().or(a["name"].as_str()))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let art = art(&track["albumOfTrack"]["coverArt"]["sources"]);
    let duration = [&track["trackDuration"]["totalMilliseconds"], &track["duration"]["totalMilliseconds"], &track["duration_ms"]]
        .into_iter()
        .find_map(Value::as_u64)
        .map(|ms| ((ms + 500) / 1000) as u32);
    (!name.is_empty() && !artists.is_empty()).then(|| Track { name: name.to_owned(), artists, local: false, art, duration })
}

/// A local file's artist field, which lists every artist in one string.
fn split_artists(s: &str) -> Vec<String> {
    s.split([',', ';']).map(str::trim).filter(|a| !a.is_empty()).map(str::to_owned).collect()
}

async fn access_token(http: &reqwest::Client, sp_dc: Option<&str>, device_id: &str) -> Result<(String, String)> {
    let (version, secret) = totp_secret(http).await;
    let totp = totp(&secret, SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs());
    let version = version.to_string();
    let mut cookie = format!("sp_t={device_id}");
    if let Some(sp_dc) = sp_dc {
        cookie.push_str(&format!("; sp_dc={sp_dc}"));
    }
    let resp: Value = http
        .get("https://open.spotify.com/api/token")
        .query(&[("reason", "init"), ("productType", "web-player"), ("totp", &totp), ("totpServer", &totp), ("totpVer", &version)])
        .header("Cookie", cookie)
        .send()
        .await?
        .error_for_status()
        .context("Spotify refused an access token")?
        .json()
        .await?;
    if sp_dc.is_some() && resp["isAnonymous"] != false {
        bail!(SignedOut);
    }
    let token = resp["accessToken"].as_str().context("Spotify returned no access token")?;
    let client_id = resp["clientId"].as_str().context("Spotify returned no client id")?;
    Ok((token.to_owned(), client_id.to_owned()))
}

/// The saved Spotify sign-in has expired or was signed out elsewhere.
#[derive(Debug)]
pub struct SignedOut;

impl std::fmt::Display for SignedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(crate::tr!("transfer-error-expired"))
    }
}

impl std::error::Error for SignedOut {}

/// The newest web-player TOTP secret, falling back to a known one.
async fn totp_secret(http: &reqwest::Client) -> (u32, Vec<u8>) {
    let fetched = async {
        let secrets: serde_json::Map<String, Value> =
            http.get(SECRETS_URL).timeout(std::time::Duration::from_secs(5)).send().await?.error_for_status()?.json().await?;
        let (version, list) = secrets
            .iter()
            .filter_map(|(k, v)| Some((k.parse::<u32>().ok()?, v.as_array()?)))
            .max_by_key(|(k, _)| *k)
            .ok_or_else(|| anyhow!("empty secrets list"))?;
        let bytes = list.iter().map(|n| n.as_u64().map(|n| n as u8)).collect::<Option<Vec<u8>>>().context("bad secret")?;
        Ok::<_, anyhow::Error>((version, bytes))
    };
    fetched.await.unwrap_or_else(|_: anyhow::Error| (FALLBACK_SECRET.0, FALLBACK_SECRET.1.to_vec()))
}

/// RFC 6238 TOTP (SHA-1, 30 s, 6 digits) over Spotify's obfuscated secret.
fn totp(secret: &[u8], unix: u64) -> String {
    // The key is the decimal digits of each XOR-ed byte, joined, as ASCII.
    let key: String = secret.iter().enumerate().map(|(i, b)| (b ^ ((i % 33) as u8 + 9)).to_string()).collect();
    let mut mac = Hmac::<Sha1>::new_from_slice(key.as_bytes()).expect("HMAC takes any key length");
    mac.update(&(unix / 30).to_be_bytes());
    let hash = mac.finalize().into_bytes();
    let offset = (hash[19] & 0xf) as usize;
    let code = u32::from_be_bytes(hash[offset..offset + 4].try_into().unwrap()) & 0x7fff_ffff;
    format!("{:06}", code % 1_000_000)
}

async fn client_token(http: &reqwest::Client, client_version: &str, client_id: &str, device_id: &str) -> Result<String> {
    let body = json!({"client_data": {
        "client_version": client_version,
        "client_id": client_id,
        "js_sdk_data": {"device_brand": "unknown", "device_model": "unknown", "os": "windows", "os_version": "NT 10.0", "device_id": device_id, "device_type": "computer"},
    }});
    let resp: Value = http
        .post("https://clienttoken.spotify.com/v1/clienttoken")
        .header("Accept", "application/json")
        .json(&body)
        .send()
        .await?
        .error_for_status()
        .context("Spotify refused a client token")?
        .json()
        .await?;
    ensure!(resp["response_type"] == "RESPONSE_GRANTED_TOKEN_RESPONSE", "Spotify refused a client token: {}", resp["response_type"]);
    Ok(resp["granted_token"]["token"].as_str().context("no client token")?.to_owned())
}

/// The persisted-query hashes, from the cache when the bundle hasn't changed. They live in
/// the main bundle; older builds kept them in lazy chunks, so those are searched as a fallback.
async fn query_hashes(http: &reqwest::Client, bundle: &str, data_dir: &Path) -> Result<Hashes> {
    let cache = data_dir.join(HASH_CACHE);
    if let Some(h) = std::fs::read(&cache).ok().and_then(|b| serde_json::from_slice::<Hashes>(&b).ok())
        && h.bundle == bundle
        && h.ops.contains_key("fetchPlaylist")
    {
        return Ok(h);
    }
    let js = http.get(bundle).send().await?.error_for_status()?.text().await?;
    let mut ops = std::collections::BTreeMap::new();
    let scan = |ops: &mut std::collections::BTreeMap<String, String>, text: &str| {
        for op in OPERATIONS {
            if !ops.contains_key(op)
                && let Some(h) = find_hash(text, op)
            {
                ops.insert(op.to_owned(), h);
            }
        }
    };
    scan(&mut ops, &js);
    if ops.len() < OPERATIONS.len() {
        let mut chunks = stream::iter(chunk_urls(&js)).map(|url| async move { http.get(url).send().await.ok()?.text().await.ok() }).buffer_unordered(8);
        while let Some(chunk) = chunks.next().await {
            if let Some(chunk) = chunk {
                scan(&mut ops, &chunk);
            }
            if ops.len() == OPERATIONS.len() {
                break;
            }
        }
    }
    ensure!(ops.contains_key("fetchPlaylist"), "couldn't find Spotify's fetchPlaylist query");
    let hashes = Hashes { bundle: bundle.to_owned(), ops };
    let _ = std::fs::write(&cache, serde_json::to_vec(&hashes).unwrap_or_default());
    Ok(hashes)
}

fn find_hash(js: &str, operation: &str) -> Option<String> {
    let hash = between(js, &format!(r#""{operation}","query",""#), "\"")?;
    (hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hash.to_owned())
}

/// Lazy chunk file names, from the bundle's `{id:"name"}` and `{id:"hash"}` maps.
fn chunk_urls(js: &str) -> Vec<String> {
    let maps: Vec<Vec<(&str, &str)>> = js.match_indices('{').filter_map(|(i, _)| parse_id_map(&js[i..])).collect();
    let is_hash = |v: &str| v.len() == 8 && v.bytes().all(|b| b.is_ascii_hexdigit());
    let mut urls = Vec::new();
    for names in maps.iter().filter(|m| m.iter().any(|(_, v)| !is_hash(v))) {
        for hashes in maps.iter().filter(|m| m.iter().all(|(_, v)| is_hash(v))) {
            for (id, name) in names {
                if let Some((_, hash)) = hashes.iter().find(|(k, _)| k == id) {
                    urls.push(format!("{CDN}{name}.{hash}.js"));
                }
            }
        }
    }
    urls.sort();
    urls.dedup();
    urls
}

/// Parse a `{12:"a",34:"b"}` literal at the start of `s`.
fn parse_id_map(s: &str) -> Option<Vec<(&str, &str)>> {
    let body = &s[1..s.find('}')?];
    let mut out = Vec::new();
    for entry in body.split(',') {
        let (id, value) = entry.split_once(':')?;
        let value = value.strip_prefix('"')?.strip_suffix('"')?;
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) || value.is_empty() || value.contains('"') {
            return None;
        }
        out.push((id, value));
    }
    (out.len() > 1).then_some(out)
}

fn script_srcs(html: &str) -> impl Iterator<Item = &str> {
    html.split("<script").skip(1).filter_map(|tag| between(tag.split('>').next()?, "src=\"", "\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources() {
        let id = Source::Playlist("37i9dQZF1DXcBWIGoYBM5M".into());
        for link in [
            "https://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5M",
            "https://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5M?si=abc",
            "https://open.spotify.com/intl-de/playlist/37i9dQZF1DXcBWIGoYBM5M",
            "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M",
            "37i9dQZF1DXcBWIGoYBM5M",
        ] {
            assert!(Source::parse(link).unwrap() == id, "{link}");
        }
        assert!(Source::parse("https://open.spotify.com/collection/tracks").unwrap() == Source::Liked);
        assert!(Source::parse("https://evil.com/playlist/37i9dQZF1DXcBWIGoYBM5M").is_err());
        assert!(Source::parse("https://open.spotify.com/album/37i9dQZF1DXcBWIGoYBM5M").is_err());
    }

    #[test]
    fn totp_matches_reference() {
        // Computed independently with Python's hmac, the way pyotp derives it.
        assert_eq!(totp(FALLBACK_SECRET.1, 1_699_999_980), "371599");
        assert_eq!(totp(FALLBACK_SECRET.1, 1_700_000_009), "371599");
    }

    #[test]
    fn id_maps() {
        let js = r#"x={1:"a",2:"b"};y={1:"0123abcd",2:"deadbeef"};"#;
        assert_eq!(chunk_urls(js), vec![format!("{CDN}a.0123abcd.js"), format!("{CDN}b.deadbeef.js")]);
    }

    /// Live: reads a public playlist anonymously (`cargo test -- --ignored spotify_live`).
    #[tokio::test]
    #[ignore]
    async fn spotify_live() {
        let dir = std::env::temp_dir().join("ytm-rs-spotify-test");
        std::fs::create_dir_all(&dir).unwrap();
        let c = Client::connect(None, &dir).await.unwrap();
        let p = c.playlist("37i9dQZF1DXcBWIGoYBM5M", |_, _| {}).await.unwrap();
        println!("{}: {} songs, first {} by {:?} art {:?}", p.name, p.tracks.len(), p.tracks[0].name, p.tracks[0].artists, p.tracks[0].art);
        assert!(p.tracks.len() > 20);
        assert!(p.tracks[0].art.is_some());
        assert!(p.tracks.iter().all(|t| t.duration.is_some()), "every song has a length");
    }
}

#[cfg(test)]
mod library_live {
    /// `cargo test --release -- --ignored library_live --nocapture` (uses the saved Spotify sign-in)
    #[tokio::test]
    #[ignore]
    async fn library_live() {
        let dir = directories::ProjectDirs::from("", "", "ytm-rs").unwrap().data_dir().to_path_buf();
        let cookie = String::from_utf8(crate::vault::get("spotify sp_dc", &dir).expect("signed in")).unwrap();
        let c = super::Client::connect(Some(&cookie), &dir).await.unwrap();
        let body = c
            .query("libraryV3", serde_json::json!({"filters": ["Playlists"], "order": null, "textFilter": null, "features": ["LIKED_SONGS"], "limit": 50, "offset": 0, "flatten": true, "folderUri": null, "includeFoldersWhenFlattening": true}))
            .await
            .unwrap();
        let lib = &body["data"]["me"]["libraryV3"];
        println!("typename {} total {} keys {:?}", lib["__typename"], lib["totalCount"], lib.as_object().map(|o| o.keys().collect::<Vec<_>>()));
        for it in lib["items"].as_array().into_iter().flatten().take(6) {
            let s = it.to_string();
            println!("ITEM {}", &s[..s.len().min(600)]);
        }
        let acc = c.account().await.unwrap();
        println!("parsed {} entries: {:?}", acc.library.len(), acc.library.iter().take(8).map(|e| &e.name).collect::<Vec<_>>());
    }
}
