//! Links to YouTube Music and YouTube, pasted into the app.

use crate::model::Target;

#[derive(Clone, Debug, PartialEq)]
pub enum Link {
    /// A song or video, perhaps as one of a playlist's.
    Song { id: String, list: Option<String> },
    Page(Target),
    /// Only YouTube Music knows what this address stands for (`ytm::resolve`).
    Resolve(String),
    /// A jam's code.
    Jam(String),
}

/// The first link in `text`.
pub fn parse(text: &str) -> Option<Link> {
    text.split_whitespace().find_map(parse_one)
}

/// The first link pasted among `events`.
pub fn pasted(events: &[egui::Event]) -> Option<Link> {
    events.iter().find_map(|e| if let egui::Event::Paste(text) = e { parse(text) } else { None })
}

fn parse_one(word: &str) -> Option<Link> {
    let word = word.trim_matches(['<', '>', '(', ')', '"', '\'', ',', '.', '!', ';']);
    if let Some(code) = crate::deeplink::parse(word) {
        return Some(Link::Jam(code));
    }
    let rest = match word.split_once("://") {
        Some((scheme, rest)) => (scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http")).then_some(rest)?,
        None => word,
    };
    let rest = rest.split('#').next()?;
    let (address, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (host, path) = address.split_once('/')?;
    let host = host.to_ascii_lowercase();
    let host = host.strip_prefix("www.").or(host.strip_prefix("m.")).unwrap_or(&host);
    let mut path = path.split('/');
    let (first, second) = (path.next()?, path.next().unwrap_or(""));
    let param = |name: &str| query.split('&').find_map(|p| p.strip_prefix(name)?.strip_prefix('='));
    let list = param("list").filter(|id| is_id(id));
    match (host, first) {
        ("youtu.be", id) => song(id, list),
        ("youtube.com" | "music.youtube.com", "watch") if param("v").is_some() => song(param("v")?, list),
        ("youtube.com" | "music.youtube.com", "watch" | "playlist") => list.map(playlist),
        ("youtube.com" | "music.youtube.com", "shorts" | "live" | "embed") => song(second, None),
        ("youtube.com" | "music.youtube.com", "browse" | "channel") => page(second).map(Link::Page),
        ("youtube.com" | "music.youtube.com", handle) if handle.len() > 1 && handle.starts_with('@') => Some(Link::Resolve(format!("https://music.youtube.com/{handle}"))),
        _ => None,
    }
}

/// The page of a browse id.
pub fn page(id: &str) -> Option<Target> {
    if !is_id(id) {
        return None;
    }
    let owned = id.to_owned();
    match id {
        _ if id.starts_with("MPRE") => Some(Target::Album(owned)),
        _ if id.starts_with("UC") => Some(Target::Artist(owned)),
        _ if id.starts_with("MPSP") => Some(Target::Playlist(owned)),
        _ => id.strip_prefix("VL").map(|list| Target::Playlist(list.to_owned())),
    }
}

/// What a playlist's id is a link to.
pub fn playlist(id: &str) -> Link {
    // An album's own list.
    if id.starts_with("OLAK5uy_") {
        return Link::Resolve(format!("https://music.youtube.com/playlist?list={id}"));
    }
    Link::Page(Target::Playlist(id.strip_prefix("VL").unwrap_or(id).to_owned()))
}

fn song(id: &str, list: Option<&str>) -> Option<Link> {
    // Radios and mixes (other RD lists) and Liked Music are whoever's copied the link.
    let list = list.filter(|l| ["PL", "OLAK5uy_", "RDCLAK5uy_"].iter().any(|kind| l.starts_with(kind)));
    (id.len() == 11 && is_id(id)).then(|| Link::Song { id: id.to_owned(), list: list.map(str::to_owned) })
}

fn is_id(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song_link(id: &str, list: Option<&str>) -> Option<Link> {
        Some(Link::Song { id: id.into(), list: list.map(str::to_owned) })
    }

    #[test]
    fn songs_and_videos() {
        for url in [
            "https://music.youtube.com/watch?v=lYBUbBu4W08",
            "https://music.youtube.com/watch?si=abc&v=lYBUbBu4W08&feature=share#x",
            "http://www.youtube.com/watch?v=lYBUbBu4W08&t=30s",
            "HTTPS://M.YouTube.com/watch?v=lYBUbBu4W08",
            "youtu.be/lYBUbBu4W08?si=abc",
            "https://www.youtube.com/shorts/lYBUbBu4W08",
            "https://youtube.com/live/lYBUbBu4W08?feature=share",
            "  <https://youtu.be/lYBUbBu4W08>\n",
            "listen to this: https://youtu.be/lYBUbBu4W08, it's good",
        ] {
            assert_eq!(parse(url), song_link("lYBUbBu4W08", None), "{url}");
        }
    }

    #[test]
    fn a_song_keeps_the_playlist_its_link_names_but_not_a_radio() {
        assert_eq!(parse("https://music.youtube.com/watch?v=lYBUbBu4W08&list=PLabc_-1"), song_link("lYBUbBu4W08", Some("PLabc_-1")));
        assert_eq!(parse("https://youtu.be/lYBUbBu4W08?list=OLAK5uy_abc"), song_link("lYBUbBu4W08", Some("OLAK5uy_abc")));
        for list in ["RDAMVMlYBUbBu4W08", "RDMM", "LM", "PL%2F..", ""] {
            assert_eq!(parse(&format!("https://music.youtube.com/watch?v=lYBUbBu4W08&list={list}")), song_link("lYBUbBu4W08", None), "{list}");
        }
    }

    #[test]
    fn pages() {
        let page = |t| Some(Link::Page(t));
        assert_eq!(parse("https://music.youtube.com/playlist?list=PLabc&si=x"), page(Target::Playlist("PLabc".into())));
        assert_eq!(parse("https://music.youtube.com/playlist?list=VLPLabc"), page(Target::Playlist("PLabc".into())));
        assert_eq!(parse("https://www.youtube.com/watch?list=PLabc"), page(Target::Playlist("PLabc".into())));
        assert_eq!(parse("https://music.youtube.com/browse/VLPLabc"), page(Target::Playlist("PLabc".into())));
        assert_eq!(parse("https://music.youtube.com/browse/MPREb_abc"), page(Target::Album("MPREb_abc".into())));
        assert_eq!(parse("https://music.youtube.com/channel/UCabc"), page(Target::Artist("UCabc".into())));
    }

    #[test]
    fn addresses_to_resolve() {
        let resolve = |s: &str| Some(Link::Resolve(s.into()));
        assert_eq!(parse("https://www.youtube.com/@RickAstleyYT/videos"), resolve("https://music.youtube.com/@RickAstleyYT"));
        assert_eq!(parse("https://music.youtube.com/playlist?list=OLAK5uy_abc"), resolve("https://music.youtube.com/playlist?list=OLAK5uy_abc"));
    }

    #[test]
    fn jams() {
        assert_eq!(parse("ytm-rs://jam/ABC234"), Some(Link::Jam("ABC234".into())));
        assert_eq!(parse("https://grapedjuice.github.io/ytm-rs/jam/?c=abc234"), Some(Link::Jam("ABC234".into())));
    }

    #[test]
    fn a_link_among_what_was_pasted() {
        let paste = |t: &str| egui::Event::Paste(t.into());
        assert_eq!(pasted(&[paste("some words"), egui::Event::Copy, paste("see youtu.be/lYBUbBu4W08")]), song_link("lYBUbBu4W08", None));
        assert_eq!(pasted(&[paste("some words"), egui::Event::Text("youtu.be/lYBUbBu4W08".into())]), None);
        assert_eq!(parse("listen to this: https://youtu.be/lYBUbBu4W08!"), song_link("lYBUbBu4W08", None));
    }

    #[test]
    fn anything_else_is_a_search() {
        for text in [
            "",
            "never gonna give you up",
            "youtube.com",
            "https://music.youtube.com/",
            "https://music.youtube.com/watch?v=tooshort",
            "https://music.youtube.com/watch?v=lYBUbBu4W0%2F",
            "https://music.youtube.com/browse/FEmusic_home",
            "https://music.youtube.com/channel/UC../x",
            "https://music.youtube.com/@",
            "https://youtube.com.evil.example/watch?v=lYBUbBu4W08",
            "https://evil.example/music.youtube.com/watch?v=lYBUbBu4W08",
            "https://user@evil.example/watch?v=lYBUbBu4W08",
            "ftp://music.youtube.com/watch?v=lYBUbBu4W08",
            "file:///music.youtube.com/watch?v=lYBUbBu4W08",
        ] {
            assert_eq!(parse(text), None, "{text}");
        }
    }
}
