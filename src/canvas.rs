//! Animated covers: the short looping clips Apple Music shows in place of some albums'
//! covers. Found by searching Apple Music's catalogue for the song, played by
//! [`crate::video::Loop`]. Nothing is asked unless the listener turned them on.
//!
//! Dev hook: `YTM_CANVAS=<HLS URL>` gives every song that clip, without asking Apple.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::model::Track;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";
const DAY: u64 = 86_400;

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// ---------------------------------------------------------------------------- matching

/// Letters and digits of every script, lowercase, without accents, single-spaced.
fn normalize(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    use unicode_normalization::char::is_combining_mark;
    let plain: String = s.nfd().filter(|c| !is_combining_mark(*c)).flat_map(char::to_lowercase).map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
    plain.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The artists credited in one line ("A, B & C feat. D").
fn split_artists(line: &str) -> Vec<String> {
    let mut s = format!(" {} ", line.to_lowercase());
    for sep in [",", "&", " × ", " x ", " feat. ", " feat ", " ft. ", " ft ", " featuring ", " with "] {
        s = s.replace(sep, "\u{1}");
    }
    s.split('\u{1}').map(normalize).filter(|a| !a.is_empty()).collect()
}

/// The title without "(feat. …)", "[Live]", " - Remastered 2011" and the like.
fn clean_title(title: &str) -> String {
    let title = title.split(" - ").next().unwrap_or(title);
    let mut out = String::new();
    let mut depth = 0;
    for c in title.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let out = out.trim();
    if out.is_empty() { title.to_owned() } else { out.to_owned() }
}

/// The same song, give or take what [`clean_title`] leaves out. Not a prefix: "Love" isn't
/// "Love Is Lonely".
fn same_title(a: &str, b: &str) -> bool {
    let (a, b) = (normalize(&clean_title(a)), normalize(&clean_title(b)));
    !a.is_empty() && a == b
}

/// Every artist asked for is credited on the hit. A wrong clip is worse than none.
fn same_artists(wanted: &[String], credited: &str) -> bool {
    let credited = split_artists(credited);
    !wanted.is_empty() && wanted.iter().all(|w| credited.contains(w))
}

fn compilation(album: &str) -> bool {
    let a = album.to_lowercase();
    ["playlist", "essentials", "dj mix", "mixed", "apple music", "today's hits", "set list"].iter().any(|w| a.contains(w))
}

// ---------------------------------------------------------------------------- Apple Music

struct Token {
    jwt: Option<String>,
    expires: u64,
    retry_after: u64,
}

static TOKEN: Mutex<Token> = Mutex::new(Token { jwt: None, expires: 0, retry_after: 0 });

/// The JWT in `jwt`'s payload: when it expires (s), if it reads.
fn jwt_expiry(jwt: &str) -> Option<u64> {
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(jwt.split('.').nth(1)?.trim_end_matches('=')).ok()?;
    serde_json::from_slice::<serde_json::Value>(&payload).ok()?["exp"].as_u64()
}

/// The token Apple Music's web player sends, from its own script.
async fn apple_token(http: &reqwest::Client) -> Option<String> {
    let now = now_secs();
    {
        let t = TOKEN.lock().unwrap();
        if let Some(j) = t.jwt.as_ref().filter(|_| now + 60 < t.expires) {
            return Some(j.clone());
        }
        if now < t.retry_after {
            return None;
        }
    }
    let found = async {
        let html = http.get("https://music.apple.com/us/browse").header("User-Agent", UA).send().await.ok()?.text().await.ok()?;
        let mut scripts: Vec<&str> = html.match_indices("/assets/index").filter_map(|(at, _)| html[at..].split(['"', '\'']).next()).filter(|p| p.ends_with(".js")).collect();
        scripts.dedup();
        for path in scripts {
            let Ok(js) = async { http.get(format!("https://music.apple.com{path}")).header("User-Agent", UA).send().await?.text().await }.await else { continue };
            let pick = js.match_indices("eyJ").find_map(|(at, _)| {
                let jwt: String = js[at..].chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')).collect();
                (jwt.split('.').count() == 3).then_some(())?;
                jwt_expiry(&jwt).filter(|e| *e > now).map(|e| (jwt, e))
            });
            if pick.is_some() {
                return pick;
            }
        }
        None
    }
    .await;
    let mut t = TOKEN.lock().unwrap();
    match found {
        Some((jwt, expires)) => {
            *t = Token { jwt: Some(jwt.clone()), expires, retry_after: 0 };
            Some(jwt)
        }
        None => {
            t.retry_after = now + 30 * 60;
            None
        }
    }
}

/// The square clip in an `editorialVideo`, else the tall one.
fn motion(video: &serde_json::Value) -> Option<String> {
    let link = |k: &str| ["video", "videoUrl", "hlsUrl", "url"].iter().find_map(|f| video[k][f].as_str()).filter(|u| !u.is_empty()).map(str::to_owned);
    ["motionDetailSquare", "motionSquareVideo1x1", "motionDetailTall", "motionTallVideo3x4"].iter().find_map(|k| link(k))
}

/// The album's id in a song's web address: `…/album/<name>/<id>?i=<song>`.
fn album_id(url: &str) -> Option<String> {
    let id = url.split("/album/").nth(1)?.split('?').next()?.rsplit('/').next()?;
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).then(|| id.to_owned())
}

async fn apple(http: &reqwest::Client, track: &Track) -> Option<String> {
    let token = apple_token(http).await?;
    let title = clean_title(&track.title);
    let artist = track.artists.first()?.name.clone();
    let wanted = split_artists(&artist);
    let album = track.album.as_ref().map(|a| a.name.as_str()).filter(|a| !a.is_empty() && !title.to_lowercase().contains(&a.to_lowercase()));
    let term = [Some(artist.as_str()), Some(title.as_str()), album].into_iter().flatten().collect::<Vec<_>>().join(" ");
    let get = |url: String| {
        let token = token.clone();
        async move {
            let r = http.get(url).header("User-Agent", UA).header("Authorization", format!("Bearer {token}")).header("Origin", "https://music.apple.com").header("Referer", "https://music.apple.com/").timeout(Duration::from_secs(15)).send().await.ok()?;
            r.error_for_status().ok()?.json::<serde_json::Value>().await.ok()
        }
    };
    let mut url = url::Url::parse("https://amp-api.music.apple.com/v1/catalog/us/search").ok()?;
    url.query_pairs_mut().append_pair("term", &term).append_pair("types", "songs").append_pair("limit", "10").append_pair("extend", "editorialVideo").append_pair("include", "albums");
    let found = get(url.into()).await?;
    for hit in found["results"]["songs"]["data"].as_array()? {
        let a = &hit["attributes"];
        if compilation(a["albumName"].as_str().unwrap_or("")) || !same_artists(&wanted, a["artistName"].as_str().unwrap_or("")) || !same_title(a["name"].as_str().unwrap_or(""), &title) {
            continue;
        }
        if let Some(clip) = motion(&a["editorialVideo"]) {
            return Some(clip);
        }
        // The song's album has the clip, more often than the song itself. Search results
        // seldom carry the album's link: its id is also in the song's own web address.
        let Some(id) = hit["relationships"]["albums"]["data"][0]["id"].as_str().map(str::to_owned).or_else(|| album_id(a["url"].as_str()?)) else { continue };
        let Some(album) = get(format!("https://amp-api.music.apple.com/v1/catalog/us/albums/{id}?extend=editorialVideo")).await else { continue };
        if let Some(clip) = motion(&album["data"][0]["attributes"]["editorialVideo"]) {
            return Some(clip);
        }
    }
    None
}

// ---------------------------------------------------------------------------- what was found

#[derive(Serialize, Deserialize, Clone)]
struct Entry {
    clip: Option<String>,
    /// When it was looked up (s).
    at: u64,
}

impl Entry {
    /// Found clips are trusted for a month (links do move), misses for a few days.
    fn fresh(&self, now: u64) -> bool {
        now.saturating_sub(self.at) < if self.clip.is_some() { 30 * DAY } else { 3 * DAY }
    }
}

const MAX_ENTRIES: usize = 3000;

struct Index {
    path: PathBuf,
    songs: HashMap<String, Entry>,
}

static INDEX: Mutex<Option<Index>> = Mutex::new(None);

fn with_index<R>(data_dir: &Path, f: impl FnOnce(&mut Index) -> R) -> R {
    let mut g = INDEX.lock().unwrap();
    let index = g.get_or_insert_with(|| {
        let path = data_dir.join("animated_covers.json");
        let songs = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Index { path, songs }
    });
    f(index)
}

/// The clip standing in for `track`'s cover, if it has one: what was found before, else
/// Apple Music's.
pub async fn find(http: &reqwest::Client, data_dir: &Path, track: &Track) -> Option<String> {
    if let Ok(url) = std::env::var("YTM_CANVAS") {
        return Some(url);
    }
    let now = now_secs();
    if let Some(e) = with_index(data_dir, |i| i.songs.get(&track.id).filter(|e| e.fresh(now)).cloned()) {
        return e.clip;
    }
    let clip = apple(http, track).await;
    log::info!("animated cover of {}: {}", track.id, if clip.is_some() { "found" } else { "none" });
    let snapshot = with_index(data_dir, |i| {
        // Offline or a hiccup: the clip already known stays.
        if clip.is_some() || i.songs.get(&track.id).is_none_or(|e| e.clip.is_none()) {
            i.songs.insert(track.id.clone(), Entry { clip: clip.clone(), at: now });
        }
        if i.songs.len() > MAX_ENTRIES {
            let mut by_age: Vec<(u64, String)> = i.songs.iter().map(|(k, e)| (e.at, k.clone())).collect();
            by_age.sort();
            for (_, k) in by_age.into_iter().take(i.songs.len() - MAX_ENTRIES) {
                i.songs.remove(&k);
            }
        }
        serde_json::to_vec(&i.songs).ok().map(|b| (i.path.clone(), b))
    });
    if let Some((path, bytes)) = snapshot {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
    clip.or_else(|| with_index(data_dir, |i| i.songs.get(&track.id).and_then(|e| e.clip.clone())))
}

// ---------------------------------------------------------------------------- the clip

/// A clip's H.264 in fragmented MP4: the init segment and the media segments.
pub struct Clip {
    pub init: Vec<u8>,
    pub segments: Vec<Vec<u8>>,
}

/// One rendition in a master playlist.
#[derive(Debug, PartialEq)]
struct Variant {
    uri: String,
    side: u32,
    bandwidth: u64,
    h264: bool,
}

/// `KEY=value` (or `KEY="value"`) from an attribute list.
fn attr<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let mut rest = line;
    while let Some(at) = rest.find(key) {
        let before = rest[..at].chars().last();
        let after = &rest[at + key.len()..];
        if matches!(before, None | Some(',' | ':')) && let Some(v) = after.strip_prefix('=') {
            return Some(match v.strip_prefix('"') {
                Some(q) => q.split('"').next().unwrap_or(""),
                None => v.split(',').next().unwrap_or(""),
            });
        }
        rest = after;
    }
    None
}

fn variants(master: &str) -> Vec<Variant> {
    let mut out = Vec::new();
    let mut lines = master.lines().map(str::trim);
    while let Some(line) = lines.next() {
        let Some(info) = line.strip_prefix("#EXT-X-STREAM-INF:") else { continue };
        let Some(uri) = lines.find(|l| !l.is_empty() && !l.starts_with('#')) else { break };
        let side = attr(info, "RESOLUTION").and_then(|r| r.split_once('x')).and_then(|(w, h)| Some(w.parse::<u32>().ok()?.min(h.parse().ok()?))).unwrap_or(0);
        let h264 = attr(info, "CODECS").is_none_or(|c| c.contains("avc1"));
        out.push(Variant { uri: uri.to_owned(), side, bandwidth: attr(info, "BANDWIDTH").and_then(|b| b.parse().ok()).unwrap_or(0), h264 });
    }
    out
}

/// The H.264 rendition for a cover `side` px across: the smallest at least that big,
/// else the biggest, at the best quality that size comes in. Decoding cost goes mostly
/// with size, and the cover shows no more.
fn pick(variants: &[Variant], side: u32) -> Option<&Variant> {
    let avc = variants.iter().filter(|v| v.h264);
    let fits = avc.clone().filter(|v| v.side >= side).min_by_key(|v| (v.side, std::cmp::Reverse(v.bandwidth)));
    fits.or_else(|| avc.max_by_key(|v| (v.side, v.bandwidth)))
}

/// A media playlist's init segment and media files. Apple's give every segment as a byte
/// range of one file: each file is listed once, whole, which holds those segments in order.
fn media(playlist: &str) -> Option<(String, Vec<String>)> {
    let init = playlist.lines().find_map(|l| l.trim().strip_prefix("#EXT-X-MAP:")).and_then(|m| attr(m, "URI"))?.to_owned();
    let mut files: Vec<String> = Vec::new();
    for line in playlist.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if !files.iter().any(|f| f == line) {
            files.push(line.to_owned());
        }
    }
    (!files.is_empty()).then_some((init, files))
}

/// Bytes of an MP4 up to the end of its `moov`, the part a decoder sets up from.
fn head_len(file: &[u8]) -> usize {
    let mut at = 0;
    while let Some(size) = file.get(at..at + 8).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize) {
        if size < 8 || at + size > file.len() {
            break;
        }
        at += size;
        if &file[at - size + 4..at - size + 8] == b"moov" {
            return at;
        }
    }
    file.len()
}

/// The last clip loaded: an album's songs often share one, and it needn't be fetched again.
static LAST: Mutex<Option<(String, u32, Arc<Clip>)>> = Mutex::new(None);

/// Fetches the clip at `url` (an HLS playlist), in the rendition for `side` px.
pub async fn load(http: &reqwest::Client, url: &str, side: u32) -> anyhow::Result<Arc<Clip>> {
    if let Some((_, _, clip)) = LAST.lock().unwrap().as_ref().filter(|(u, s, _)| u == url && *s == side) {
        return Ok(clip.clone());
    }
    let base = url::Url::parse(url)?;
    let text = |u: url::Url| async move { anyhow::Ok(http.get(u).header("User-Agent", UA).timeout(Duration::from_secs(20)).send().await?.error_for_status()?.text().await?) };
    let bytes = |u: url::Url| async move { anyhow::Ok(http.get(u).header("User-Agent", UA).timeout(Duration::from_secs(30)).send().await?.error_for_status()?.bytes().await?.to_vec()) };
    let mut playlist_url = base.clone();
    let mut playlist = text(base.clone()).await?;
    let all = variants(&playlist);
    if !all.is_empty() {
        let v = pick(&all, side).ok_or_else(|| anyhow::anyhow!("no H.264 rendition"))?;
        log::info!("animated cover: {}px rendition of {} for {side}px", v.side, all.len());
        playlist_url = base.join(&v.uri)?;
        playlist = text(playlist_url.clone()).await?;
    }
    let (init, files) = media(&playlist).ok_or_else(|| anyhow::anyhow!("not fragmented MP4"))?;
    let mut out = Vec::with_capacity(files.len());
    for f in &files {
        out.push(bytes(playlist_url.join(f)?).await?);
    }
    // In one of those files, the init segment is at its start: only that much of it again.
    let init = match files.iter().position(|f| *f == init) {
        Some(i) => out[i][..head_len(&out[i])].to_vec(),
        None => bytes(playlist_url.join(&init)?).await?,
    };
    log::info!("animated cover: {} file(s), {} KB", files.len(), out.iter().map(Vec::len).sum::<usize>() / 1024);
    let clip = Arc::new(Clip { init, segments: out });
    *LAST.lock().unwrap() = Some((url.to_owned(), side, clip.clone()));
    Ok(clip)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cargo test --release -- --ignored animated_covers_live --nocapture`: asks Apple Music
    /// for well-known songs' clips and lists what each found, how long it took, and the
    /// renditions on offer.
    #[tokio::test]
    #[ignore]
    async fn animated_covers_live() {
        let http = reqwest::Client::new();
        let dir = std::env::temp_dir().join(format!("ytm-canvas-live-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let songs = [
            ("Espresso", "Sabrina Carpenter", "Short n' Sweet"),
            ("BIRDS OF A FEATHER", "Billie Eilish", "HIT ME HARD AND SOFT"),
            ("Blinding Lights", "The Weeknd", "After Hours"),
            ("APT.", "ROSÉ", "rosie"),
            ("Not Like Us", "Kendrick Lamar", "Not Like Us"),
            ("Anti-Hero", "Taylor Swift", "Midnights"),
            ("Flowers", "Miley Cyrus", "Endless Summer Vacation"),
            ("good 4 u", "Olivia Rodrigo", "SOUR"),
            ("Levitating", "Dua Lipa", "Future Nostalgia"),
            ("Die With A Smile", "Lady Gaga", "Die With A Smile"),
            ("Bohemian Rhapsody", "Queen", "A Night at the Opera"),
            ("Stairway to Heaven", "Led Zeppelin", "Led Zeppelin IV"),
            ("HUMBLE.", "Kendrick Lamar", "DAMN."),
            ("Cruel Summer", "Taylor Swift", "Lover"),
            ("Paint The Town Red", "Doja Cat", "Scarlet"),
            ("Vampire", "Olivia Rodrigo", "GUTS"),
        ];
        let t0 = std::time::Instant::now();
        println!("token: {}", if apple_token(&http).await.is_some() { "ok" } else { "NONE" });
        println!("token took {} ms", t0.elapsed().as_millis());
        let mut found = 0;
        for (i, (title, artist, album)) in songs.iter().enumerate() {
            let track = Track {
                id: format!("live{i}"),
                title: (*title).into(),
                artists: vec![crate::model::Link { name: (*artist).into(), id: None }],
                album: Some(crate::model::Link { name: (*album).into(), id: None }),
                ..Default::default()
            };
            let t = std::time::Instant::now();
            let clip = find(&http, &dir, &track).await;
            let ms = t.elapsed().as_millis();
            let cached = std::time::Instant::now();
            assert_eq!(find(&http, &dir, &track).await, clip, "the second ask comes from the index");
            let again = cached.elapsed().as_micros();
            print!("{title:22} {artist:18} {ms:>5} ms (again {again} µs)  ");
            let Some(url) = clip else {
                println!("none");
                continue;
            };
            found += 1;
            let master = http.get(&url).header("User-Agent", UA).send().await.unwrap().text().await.unwrap();
            let v = variants(&master);
            let list: Vec<String> = v.iter().map(|v| format!("{}{}@{}k", v.side, if v.h264 { "" } else { "(hevc)" }, v.bandwidth / 1000)).collect();
            println!("{} renditions: {}", v.len(), list.join(" "));
        }
        println!("{found} of {} have a clip", songs.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn titles_and_artists_match_across_scripts_and_credits() {
        assert!(same_title("Espresso", "espresso"));
        assert!(same_title("Bohemian Rhapsody (Remastered 2011)", "Bohemian Rhapsody"));
        assert!(same_title("Café", "Cafe"));
        assert!(same_title("Stairway to Heaven - Remaster", "Stairway to Heaven"));
        assert!(!same_title("Love Is Lonely", "Love"));
        // Non-Latin titles keep their letters, so they can't come out empty and match anything.
        assert!(same_title("Звезда", "звезда"));
        assert!(!same_title("Звезда", "Ночь"));
        assert_eq!(clean_title("Die With A Smile (feat. Bruno Mars)"), "Die With A Smile");
        assert!(same_artists(&split_artists("ROSÉ"), "ROSÉ & Bruno Mars"));
        assert!(same_artists(&split_artists("Lady Gaga"), "Lady Gaga, Bruno Mars"));
        assert!(!same_artists(&split_artists("Bruno"), "Bruno Mars"));
        assert!(!same_artists(&[], "Anyone"));
    }

    #[test]
    fn the_rendition_is_the_smallest_h264_that_fills_the_cover() {
        let master = "#EXTM3U\n\
            #EXT-X-STREAM-INF:BANDWIDTH=900000,CODECS=\"hvc1.2.4.L123.B0\",RESOLUTION=1080x1080\nhevc.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=4000000,CODECS=\"avc1.640028\",RESOLUTION=1080x1080\nh1080.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=1500000,CODECS=\"avc1.64001f\",RESOLUTION=720x720\nh720.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=600000,CODECS=\"avc1.64001e\",RESOLUTION=480x480\nh480.m3u8\n";
        let v = variants(master);
        assert_eq!(v.len(), 4);
        assert!(!v[0].h264);
        assert_eq!(pick(&v, 600).unwrap().uri, "h720.m3u8");
        assert_eq!(pick(&v, 400).unwrap().uri, "h480.m3u8");
        // Apple has several bitrates of one size: the best of them.
        let ladder = variants("#EXT-X-STREAM-INF:BANDWIDTH=937000,CODECS=\"avc1.640020\",RESOLUTION=486x486
lo.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=1819000,CODECS=\"avc1.640020\",RESOLUTION=486x486
hi.m3u8
");
        assert_eq!(pick(&ladder, 470).unwrap().uri, "hi.m3u8");
        // Bigger than any: the biggest H.264, not the HEVC.
        assert_eq!(pick(&v, 2000).unwrap().uri, "h1080.m3u8");
    }

    #[test]
    fn a_media_playlist_gives_its_init_and_segments() {
        let p = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:4.0,\nseg0.m4s\n#EXTINF:4.0,\nseg1.m4s\n#EXT-X-ENDLIST\n";
        assert_eq!(media(p), Some(("init.mp4".to_owned(), vec!["seg0.m4s".to_owned(), "seg1.m4s".to_owned()])));
        assert_eq!(media("#EXTM3U\n#EXTINF:4.0,\nseg0.ts\n"), None);
        // Byte ranges of one file: the file once.
        let ranged = "#EXTM3U\n#EXT-X-MAP:URI=\"clip.mp4\",BYTERANGE=\"900@0\"\n#EXT-X-BYTERANGE:5000@900\n#EXTINF:4.0,\nclip.mp4\n#EXT-X-BYTERANGE:5000@5900\n#EXTINF:4.0,\nclip.mp4\n";
        assert_eq!(media(ranged), Some(("clip.mp4".to_owned(), vec!["clip.mp4".to_owned()])));
        let file = [&[0, 0, 0, 12][..], b"ftyp", &[1, 2, 3, 4], &[0, 0, 0, 9], b"moov", &[5], &[0, 0, 0, 8], b"moof"].concat();
        assert_eq!(head_len(&file), 21);
        assert_eq!(head_len(&file[..12]), 12);
        assert_eq!(attr("BANDWIDTH=1,AVERAGE-BANDWIDTH=2", "BANDWIDTH"), Some("1"));
        assert_eq!(attr("AVERAGE-BANDWIDTH=2", "BANDWIDTH"), None);
        assert_eq!(album_id("https://music.apple.com/us/album/espresso/1739079974?i=1739079976").as_deref(), Some("1739079974"));
        assert_eq!(album_id("https://music.apple.com/us/song/espresso/1739079976"), None);
    }
}
