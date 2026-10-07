//! Time-synced lyrics from the Better Lyrics API (the backend of the better-lyrics
//! extension), which aggregates several providers and streams them as server-sent
//! events. We take the most precisely synced result:
//! syllable/word TTML > Musixmatch word-by-word > QQ (QRC) > line TTML > line LRC > plain.
//!
//! Access needs a JWT, obtained by exchanging a Cloudflare Turnstile token (see
//! turnstile.rs). JWTs last 24 h and are cached on disk.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

const API: &str = "https://lyrics.api.dacubeking.com/";

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Sync {
    None,
    Line,
    Word,
}

#[derive(Clone, Debug)]
pub struct Word {
    pub start: u32,
    pub end: u32,
    /// Includes trailing whitespace when the source had it.
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct Line {
    pub start: u32,
    pub end: u32,
    pub text: String,
    /// Empty for line-synced lyrics.
    pub words: Vec<Word>,
    /// Background vocals (shown smaller, under the main line).
    pub background: bool,
    /// Secondary singer in a duet (aligned to the other side).
    pub alt_singer: bool,
}

#[derive(Clone, Debug)]
pub struct Lyrics {
    pub lines: Vec<Line>,
    pub sync: Sync,
    pub source: &'static str,
}

impl Lyrics {
    pub fn plain(text: &str, source: &'static str) -> Self {
        let lines = text
            .lines()
            .map(|l| Line { start: 0, end: 0, text: l.trim().to_owned(), words: vec![], background: false, alt_singer: false })
            .collect();
        Self { lines, sync: Sync::None, source }
    }
}

// ------------------------------------------------------------------- API client

pub struct Client {
    http: reqwest::Client,
    jwt_file: PathBuf,
    webview_profile: PathBuf,
    jwt: tokio::sync::Mutex<Option<String>>,
}

pub struct Query<'a> {
    pub video_id: &'a str,
    pub song: &'a str,
    pub artist: &'a str,
    pub album: Option<&'a str>,
    pub duration: Option<u32>,
}

impl Client {
    pub fn new(http: reqwest::Client, data_dir: &Path) -> Self {
        let jwt_file = data_dir.join("lyrics_jwt");
        let cached = std::fs::read_to_string(&jwt_file).ok().filter(|t| jwt_seconds_left(t) > 3600);
        Self { http, jwt_file, webview_profile: data_dir.join("webview-lyrics"), jwt: tokio::sync::Mutex::new(cached) }
    }

    pub async fn fetch(&self, q: &Query<'_>) -> anyhow::Result<Option<Lyrics>> {
        let mut jwt = self.token(false).await?;
        for attempt in 0..2 {
            let mut form = vec![
                ("videoId", q.video_id.to_owned()),
                ("song", q.song.to_owned()),
                ("artist", q.artist.to_owned()),
                ("alwaysFetchMetadata", "false".to_owned()),
                ("token", jwt.clone()),
            ];
            if let Some(a) = q.album {
                form.push(("album", a.to_owned()));
            }
            if let Some(d) = q.duration {
                form.push(("duration", d.to_string()));
            }
            let resp = self
                .http
                .post(format!("{API}v2/lyrics"))
                .form(&form)
                .timeout(Duration::from_secs(20))
                .send()
                .await?;
            if resp.status() == reqwest::StatusCode::FORBIDDEN && attempt == 0 {
                jwt = self.token(true).await?;
                continue;
            }
            let body = resp.error_for_status()?.text().await?;
            return Ok(best_from_stream(&body));
        }
        unreachable!()
    }

    /// A valid JWT, from cache or by running the Turnstile challenge.
    async fn token(&self, force_new: bool) -> anyhow::Result<String> {
        let mut slot = self.jwt.lock().await;
        if let Some(t) = slot.as_ref().filter(|t| !force_new && jwt_seconds_left(t) > 60) {
            return Ok(t.clone());
        }
        let profile = self.webview_profile.clone();
        let turnstile = tokio::task::spawn_blocking(move || crate::turnstile::obtain(profile)).await??;
        let resp: Value = self
            .http
            .post(format!("{API}verify-turnstile"))
            .json(&serde_json::json!({ "token": turnstile }))
            .timeout(Duration::from_secs(15))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let jwt = resp["jwt"].as_str().ok_or_else(|| anyhow::anyhow!("no JWT in response"))?.to_owned();
        let _ = std::fs::write(&self.jwt_file, &jwt);
        log::info!("lyrics: new access token ({}h)", jwt_seconds_left(&jwt) / 3600);
        *slot = Some(jwt.clone());
        Ok(jwt)
    }
}

fn jwt_seconds_left(jwt: &str) -> i64 {
    use base64::Engine;
    let Some(payload) = jwt.split('.').nth(1) else { return 0 };
    let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')) else {
        return 0;
    };
    let exp = serde_json::from_slice::<Value>(&bytes).ok().and_then(|v| v["exp"].as_i64()).unwrap_or(0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    exp - now
}

// ------------------------------------------------------------------- provider selection

/// Pick the best lyrics out of a complete SSE response body.
pub fn best_from_stream(body: &str) -> Option<Lyrics> {
    let mut candidates: Vec<(u8, Lyrics)> = Vec::new();
    for block in body.split("\n\n") {
        let data: String = block
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim)
            .collect();
        let Ok(v) = serde_json::from_str::<Value>(&data) else { continue };
        let r = &v["results"];
        let s = |k: &str| r[k].as_str().filter(|s| !s.trim().is_empty());
        match v["provider"].as_str().unwrap_or_default() {
            "golyrics" | "binimum" => {
                let name = if v["provider"] == "golyrics" { "Better Lyrics" } else { "BiniLyrics" };
                if let Some(raw) = s("lyrics") {
                    let ttml = serde_json::from_str::<Value>(raw)
                        .ok()
                        .and_then(|j| j["ttml"].as_str().map(str::to_owned))
                        .unwrap_or_else(|| raw.to_owned());
                    if let Some(l) = parse_ttml(&ttml, name) {
                        let rank = if l.sync == Sync::Word { 100 } else { 60 };
                        candidates.push((rank + (name == "Better Lyrics") as u8, l));
                    }
                }
            }
            "musixmatch" => {
                if let Some(l) = s("wordByWord").and_then(|t| parse_lrc(t, "Musixmatch")) {
                    candidates.push((90, l));
                }
                if let Some(l) = s("synced").and_then(|t| parse_lrc(t, "Musixmatch")) {
                    candidates.push((55, l));
                }
            }
            "qq" => {
                let qrc = s("lyrics")
                    .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                    .and_then(|j| j["lyrics"].as_str().map(str::to_owned));
                if let Some(l) = qrc.as_deref().and_then(|t| parse_qrc(t, "QQ Music")) {
                    candidates.push((80, l));
                }
            }
            "lrclib" => {
                if let Some(l) = s("synced").and_then(|t| parse_lrc(t, "LRCLIB")) {
                    candidates.push((50, l));
                } else if let Some(t) = s("plain") {
                    candidates.push((10, Lyrics::plain(t, "LRCLIB")));
                }
            }
            "kugou" => {
                let lrc = s("lyrics")
                    .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                    .and_then(|j| j["lyrics"].as_str().map(str::to_owned));
                if let Some(l) = lrc.as_deref().and_then(|t| parse_lrc(t, "KuGou")) {
                    candidates.push((45, l));
                }
            }
            _ => {}
        }
    }
    candidates.into_iter().max_by_key(|(r, _)| *r).map(|(_, l)| l)
}

// ------------------------------------------------------------------- parsers

/// `mm:ss.xx` (LRC) → ms.
fn lrc_time(s: &str) -> Option<u32> {
    let (m, rest) = s.trim().split_once(':')?;
    let secs: f64 = rest.parse().ok()?;
    Some((m.parse::<f64>().ok()? * 60_000.0 + secs * 1000.0).round() as u32)
}

/// TTML clock values: `12.345`, `1:02.345`, `00:01:02.345`, or with an `s` suffix.
fn ttml_time(s: &str) -> Option<u32> {
    let s = s.trim().trim_end_matches('s');
    let mut total = 0.0;
    for part in s.split(':') {
        total = total * 60.0 + part.parse::<f64>().ok()?;
    }
    Some((total * 1000.0).round() as u32)
}

/// LRC with optional enhanced word timing (`<mm:ss.xx> word <mm:ss.xx>`).
pub fn parse_lrc(text: &str, source: &'static str) -> Option<Lyrics> {
    let mut lines = Vec::new();
    let mut any_words = false;
    for raw in text.lines() {
        // Leading [mm:ss.xx] tags (possibly several for repeated lines).
        let mut rest = raw.trim();
        let mut starts = Vec::new();
        while let Some(inner) = rest.strip_prefix('[') {
            let Some(close) = inner.find(']') else { break };
            match lrc_time(&inner[..close]) {
                Some(t) => starts.push(t),
                None => break, // metadata tag like [ar:...]
            }
            rest = &inner[close + 1..];
        }
        if starts.is_empty() {
            continue;
        }
        let (text, words) = if rest.contains('<') { parse_enhanced(rest) } else { (rest.trim().to_owned(), vec![]) };
        any_words |= !words.is_empty();
        for start in starts {
            let words = words.clone();
            lines.push(Line { start, end: 0, text: text.clone(), words, background: false, alt_singer: false });
        }
    }
    finish(lines, if any_words { Sync::Word } else { Sync::Line }, source)
}

fn parse_enhanced(s: &str) -> (String, Vec<Word>) {
    // Alternating timestamps and text segments.
    let mut segments: Vec<(u32, String)> = Vec::new();
    let mut rest = s;
    while let Some(open) = rest.find('<') {
        let Some(close) = rest[open..].find('>') else { break };
        if let Some(last) = segments.last_mut() {
            last.1.push_str(&rest[..open]);
        }
        match lrc_time(&rest[open + 1..open + close]) {
            Some(t) => segments.push((t, String::new())),
            None => {}
        }
        rest = &rest[open + close + 1..];
    }
    if let Some(last) = segments.last_mut() {
        last.1.push_str(rest);
    }
    let mut words: Vec<Word> = Vec::new();
    for i in 0..segments.len() {
        let (start, ref text) = segments[i];
        let end = segments.get(i + 1).map_or(start, |s| s.0);
        if text.trim().is_empty() {
            // Pure spacing segment: attach to the previous word.
            if let Some(w) = words.last_mut() {
                if !w.text.ends_with(' ') {
                    w.text.push(' ');
                }
            }
            continue;
        }
        words.push(Word { start, end, text: text.trim_start().to_owned() });
    }
    // Normalise single spaces between words.
    for w in &mut words {
        let trimmed = w.text.trim_end().len();
        if trimmed < w.text.len() {
            w.text.truncate(trimmed);
            w.text.push(' ');
        }
    }
    let text = words.iter().map(|w| w.text.as_str()).collect::<String>().trim().to_owned();
    (text, words)
}

/// QQ Music QRC: `[start,dur]word(start,dur)word(start,dur)` inside an XML attribute.
pub fn parse_qrc(xml: &str, source: &'static str) -> Option<Lyrics> {
    // Extract the attribute raw: XML parsers would fold its newlines into spaces.
    let start = xml.find("LyricContent=\"")? + "LyricContent=\"".len();
    let end = start + xml[start..].find("\"/>").or_else(|| xml[start..].find('"'))?;
    let content = unescape_xml(&xml[start..end]);
    let mut lines = Vec::new();
    for raw in content.lines() {
        let raw = raw.trim();
        let Some(inner) = raw.strip_prefix('[') else { continue };
        let Some(close) = inner.find(']') else { continue };
        let Some((ls, ld)) = inner[..close].split_once(',') else { continue }; // [ti:...] etc.
        let (Ok(ls), Ok(ld)) = (ls.parse::<u32>(), ld.parse::<u32>()) else { continue };
        let mut words = Vec::new();
        let mut rest = &inner[close + 1..];
        while let Some(open) = rest.find('(') {
            let Some(c) = rest[open..].find(')') else { break };
            let timing = &rest[open + 1..open + c];
            let word = &rest[..open];
            match timing.split_once(',').and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?))) {
                Some((ws, wd)) => {
                    if word.trim().is_empty() {
                        if let Some(w) = words.last_mut() {
                            let w: &mut Word = w;
                            if !w.text.ends_with(' ') {
                                w.text.push(' ');
                            }
                        }
                    } else {
                        words.push(Word { start: ws, end: ws + wd, text: word.to_owned() });
                    }
                }
                // A literal "(" inside the lyric text.
                None => {
                    if let Some(w) = words.last_mut() {
                        w.text.push_str(&rest[..open + 1]);
                    }
                    rest = &rest[open + 1..];
                    continue;
                }
            }
            rest = &rest[open + c + 1..];
        }
        let text = words.iter().map(|w| w.text.as_str()).collect::<String>().trim().to_owned();
        if text.is_empty() {
            continue;
        }
        lines.push(Line { start: ls, end: ls + ld, text, words, background: false, alt_singer: false });
    }
    // QRC usually opens with a "Title - Artist" credit line at 0; drop credit-like lines.
    if lines.first().is_some_and(|l| l.start == 0 && l.text.contains(" - ")) {
        lines.remove(0);
    }
    finish(lines, Sync::Word, source)
}

fn unescape_xml(s: &str) -> String {
    s.replace("&quot;", "\"").replace("&apos;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// Apple-style TTML (`itunes:timing` = None / Line / Word, spans per word or syllable).
pub fn parse_ttml(xml: &str, source: &'static str) -> Option<Lyrics> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    let timing = doc
        .root_element()
        .attributes()
        .find(|a| a.name() == "timing")
        .map(|a| a.value().to_ascii_lowercase())
        .unwrap_or_default();
    let first_agent = doc
        .descendants()
        .filter(|n| n.has_tag_name("p"))
        .find_map(|p| p.attributes().find(|a| a.name() == "agent").map(|a| a.value().to_owned()));
    let mut lines = Vec::new();
    for p in doc.descendants().filter(|n| n.has_tag_name("p")) {
        let attr = |n: roxmltree::Node, k: &str| n.attributes().find(|a| a.name() == k).map(|a| a.value().to_owned());
        let (Some(start), Some(end)) = (attr(p, "begin").and_then(|t| ttml_time(&t)), attr(p, "end").and_then(|t| ttml_time(&t)))
        else {
            continue;
        };
        let alt_singer = attr(p, "agent").is_some_and(|a| Some(&a) != first_agent.as_ref());
        let mut main = Vec::new();
        let mut bg = Vec::new();
        collect_spans(p, false, &mut main, &mut bg);
        let mut push = |words: Vec<Word>, background: bool| {
            let text = if words.is_empty() && !background {
                p.descendants().filter(|n| n.is_text()).filter_map(|n| n.text()).collect::<String>()
            } else {
                words.iter().map(|w| w.text.as_str()).collect()
            };
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if text.is_empty() {
                return;
            }
            let (s, e) = match (words.first(), words.last()) {
                (Some(f), Some(l)) if background => (f.start, l.end),
                _ => (start, end),
            };
            lines.push(Line { start: s, end: e, text, words, background, alt_singer });
        };
        push(main, false);
        if !bg.is_empty() {
            push(bg, true);
        }
    }
    lines.sort_by_key(|l| (l.start, l.background));
    let sync = if timing == "none" {
        Sync::None
    } else if lines.iter().any(|l| !l.words.is_empty()) {
        Sync::Word
    } else {
        Sync::Line
    };
    finish(lines, sync, source)
}

/// Timed spans → words. Untimed text between spans (spaces) attaches to the previous
/// word, so syllables of one word stay joined.
fn collect_spans(node: roxmltree::Node, in_bg: bool, main: &mut Vec<Word>, bg: &mut Vec<Word>) {
    for child in node.children() {
        if child.is_text() {
            let target = if in_bg { &mut *bg } else { &mut *main };
            if let (Some(w), Some(t)) = (target.last_mut(), child.text()) {
                if t.chars().any(char::is_whitespace) && !w.text.ends_with(' ') {
                    w.text.push(' ');
                }
            }
            continue;
        }
        if !child.has_tag_name("span") {
            continue;
        }
        let role_bg = child.attributes().any(|a| a.name() == "role" && a.value() == "x-bg");
        let begin = child.attributes().find(|a| a.name() == "begin").and_then(|a| ttml_time(a.value()));
        let end = child.attributes().find(|a| a.name() == "end").and_then(|a| ttml_time(a.value()));
        match (begin, end) {
            (Some(start), Some(end)) if !role_bg => {
                let text: String = child.descendants().filter(|n| n.is_text()).filter_map(|n| n.text()).collect();
                let target = if in_bg { &mut *bg } else { &mut *main };
                if !text.trim().is_empty() {
                    target.push(Word { start, end, text: text.trim().to_owned() });
                    if text.ends_with(char::is_whitespace) {
                        target.last_mut().unwrap().text.push(' ');
                    }
                }
            }
            _ => collect_spans(child, in_bg || role_bg, main, bg),
        }
    }
}

/// Fill missing line ends from the next line and drop empty instrumental gaps.
fn finish(mut lines: Vec<Line>, sync: Sync, source: &'static str) -> Option<Lyrics> {
    if sync != Sync::None {
        lines.sort_by_key(|l| l.start);
    }
    for i in 0..lines.len() {
        if lines[i].end <= lines[i].start {
            let next = lines[i + 1..].iter().find(|l| !l.background).map(|l| l.start);
            let from_words = lines[i].words.last().map(|w| w.end);
            lines[i].end = from_words.or(next).unwrap_or(lines[i].start + 5000);
        }
    }
    lines.retain(|l| !l.text.trim().is_empty());
    (!lines.is_empty()).then_some(Lyrics { lines, sync, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enhanced_lrc() {
        let l = parse_lrc("[00:31.69] <00:31.69> Like <00:35.63>   <00:35.64> the <00:35.69>\n[00:40.00] Next", "t").unwrap();
        assert_eq!(l.sync, Sync::Word);
        assert_eq!(l.lines[0].text, "Like the");
        assert_eq!(l.lines[0].words[0].start, 31690);
        assert_eq!(l.lines[0].words[1].start, 35640);
        assert_eq!(l.lines[1].start, 40000);
    }

    #[test]
    fn qrc() {
        let xml = "<Lyric_1 LyricContent=\"[ti:x]\n[0,6350]Song - Artist(0,198)\n[1000,2000]Get(1000,500) (1500,10)Lucky(1510,490)\n\"/>";
        let l = parse_qrc(xml, "t").unwrap();
        assert_eq!(l.lines.len(), 1);
        assert_eq!(l.lines[0].text, "Get Lucky");
        assert_eq!(l.lines[0].words[1].start, 1510);
    }

    #[test]
    fn ttml_words_and_background() {
        let xml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:itunes="http://music.apple.com/lyric-ttml-internal" xmlns:ttm="http://www.w3.org/ns/ttml#metadata" itunes:timing="Word"><body><div>
<p begin="1.0" end="3.0" ttm:agent="v1"><span begin="1.0" end="1.5">Hel</span><span begin="1.5" end="2.0">lo</span> <span begin="2.0" end="3.0">world</span><span ttm:role="x-bg"><span begin="2.5" end="3.5">(yeah)</span></span></p>
<p begin="0:04.000" end="0:05.000" ttm:agent="v2">Line two</p></div></body></tt>"#;
        let l = parse_ttml(xml, "t").unwrap();
        assert_eq!(l.sync, Sync::Word);
        assert_eq!(l.lines[0].text, "Hello world");
        assert!(l.lines[1].background && l.lines[1].text == "(yeah)");
        assert!(l.lines[2].alt_singer && l.lines[2].start == 4000);
    }
}
