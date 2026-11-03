//! YouTube Music pages and actions, read from the InnerTube responses music.youtube.com
//! itself gets (see `ytmapi.rs` for the transport): home feed, search, albums,
//! playlists, artists, radio, charts, the library, and edits to it.

use serde_json::{Value, json};

use crate::backend::{ArtistPage, Collection};
use crate::model::{Card, Chip, Link, Shelf, ShelfKind, Target, Thumb, Track};
use crate::ytmapi::Api;

pub struct HomePage {
    pub chips: Vec<Chip>,
    pub shelves: Vec<Shelf>,
    pub continuation: Option<String>,
}

pub async fn home(api: &Api, params: Option<&str>, auth: bool) -> anyhow::Result<HomePage> {
    let mut body = json!({ "browseId": "FEmusic_home" });
    if let Some(p) = params {
        body["params"] = json!(p);
    }
    Ok(parse_home(&api.call("browse", body, auth).await?))
}

fn parse_home(v: &Value) -> HomePage {
    let sec = &v["contents"]["singleColumnBrowseResultsRenderer"]["tabs"][0]["tabRenderer"]["content"]["sectionListRenderer"];
    let chips = arr(&sec["header"]["chipCloudRenderer"]["chips"])
        .iter()
        .filter_map(|c| {
            let c = &c["chipCloudChipRenderer"];
            let selected = c["isSelected"].as_bool().unwrap_or(false);
            // A selected chip's own endpoint re-selects it; deselecting returns to the default feed.
            let ep = if selected { &c["onDeselectedCommand"] } else { &c["navigationEndpoint"] };
            Some(Chip { title: text(&c["text"])?, params: ep["browseEndpoint"]["params"].as_str().map(str::to_owned), selected })
        })
        .collect();
    HomePage { chips, shelves: shelves(&sec["contents"]), continuation: continuation(sec) }
}

pub async fn home_more(api: &Api, token: &str, auth: bool) -> anyhow::Result<HomePage> {
    let v = api.call("browse", json!({ "continuation": token }), auth).await?;
    let sec = &v["continuationContents"]["sectionListContinuation"];
    Ok(HomePage { chips: vec![], shelves: shelves(&sec["contents"]), continuation: continuation(sec) })
}

// ----------------------------------------------------------------------- search

/// `search` filters, as the chips on music.youtube.com's results set them.
pub enum Filter {
    Songs,
    Albums,
    Artists,
    /// Playlists made by YouTube Music (not the community ones).
    Playlists,
    Videos,
}

impl Filter {
    fn params(&self) -> &'static str {
        match self {
            Filter::Songs => "EgWKAQIIAWoQEAMQBBAJEAoQBRAREBAQFQ%3D%3D",
            Filter::Albums => "EgWKAQIYAWoQEAMQBBAJEAoQBRAREBAQFQ%3D%3D",
            Filter::Artists => "EgWKAQIgAWoQEAMQBBAJEAoQBRAREBAQFQ%3D%3D",
            Filter::Playlists => "EgeKAQQoADgBahIQAxAEEAkQDhAKEAUQERAQEBU%3D",
            Filter::Videos => "EgWKAQIQAWoQEAMQBBAJEAoQBRAREBAQFQ%3D%3D",
        }
    }
}

async fn search_as(api: &Api, query: &str, filter: Filter, auth: bool) -> anyhow::Result<Vec<Value>> {
    let v = api.call("search", json!({ "query": query, "params": filter.params() }), auth).await?;
    let sections = &v["contents"]["tabbedSearchResultsRenderer"]["tabs"][0]["tabRenderer"]["content"]["sectionListRenderer"]["contents"];
    Ok(arr(sections).iter().flat_map(|s| arr(&s["musicShelfRenderer"]["contents"]).iter().cloned()).collect())
}

/// `auth`: as the signed-in account, which adds the query to its search history.
pub async fn search_songs(api: &Api, query: &str, auth: bool) -> anyhow::Result<Vec<Track>> {
    Ok(search_as(api, query, Filter::Songs, auth).await?.iter().filter_map(|i| list_item(&i["musicResponsiveListItemRenderer"])).collect())
}

/// Song or video rows, searched as the signed-in account: signed-out searches miss
/// some major-label releases entirely (the Spotify transfer needs those).
pub async fn search_tracks_signed_in(api: &Api, query: &str, filter: Filter) -> anyhow::Result<Vec<Track>> {
    Ok(search_as(api, query, filter, true).await?.iter().filter_map(|i| list_item(&i["musicResponsiveListItemRenderer"])).collect())
}

pub async fn search_cards(api: &Api, query: &str, filter: Filter, auth: bool) -> anyhow::Result<Vec<Card>> {
    Ok(search_as(api, query, filter, auth).await?.iter().filter_map(|i| list_card(&i["musicResponsiveListItemRenderer"])).collect())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub text: String,
    /// A past search of the signed-in account: the token that removes it from the
    /// account's search history.
    pub forget: Option<String>,
}

/// Search terms for `input`. Signed in, the account's matching past searches come
/// first, and an empty `input` gives just those (its recent searches).
pub async fn suggestions(api: &Api, input: &str, auth: bool) -> anyhow::Result<Vec<Suggestion>> {
    let v = api.call("music/get_search_suggestions", json!({ "input": input }), auth).await?;
    Ok(parse_suggestions(&v))
}

fn parse_suggestions(v: &Value) -> Vec<Suggestion> {
    // A second section holds songs and artists, which the dropdown doesn't show.
    let items = &v["contents"][0]["searchSuggestionsSectionRenderer"]["contents"];
    arr(items)
        .iter()
        .filter_map(|i| {
            let past = &i["historySuggestionRenderer"];
            let r = if past.is_null() { &i["searchSuggestionRenderer"] } else { past };
            Some(Suggestion {
                text: text(&r["suggestion"])?,
                forget: past["serviceEndpoint"]["feedbackEndpoint"]["feedbackToken"].as_str().map(str::to_owned),
            })
        })
        .collect()
}

/// Remove a past search from the account's search history.
pub async fn forget_search(api: &Api, token: &str) -> anyhow::Result<()> {
    let v = api.call_signed_in("feedback", json!({ "feedbackTokens": [token] })).await?;
    anyhow::ensure!(v["feedbackResponses"][0]["isProcessed"] == true, "YouTube Music didn't remove it");
    Ok(())
}

// ------------------------------------------------------------- albums, playlists

pub async fn album(api: &Api, id: &str) -> anyhow::Result<Collection> {
    let v = api.call("browse", json!({ "browseId": id }), false).await?;
    let (mut a, videos) = parse_album(&v, id).ok_or_else(|| anyhow::anyhow!("YouTube Music returned an empty album"))?;
    if !videos.is_empty() {
        songs_for_videos(&mut a, &videos).await;
    }
    Ok(a)
}

// ------------------------------------------------------------- song versions

/// The song version of a music video from YouTube's own pair: for Premium accounts, the
/// queue wraps a music video together with its song (`counterpart`). Other accounts get
/// no pairs, so this is `None` for them.
pub async fn paired_song(api: &Api, video_id: &str) -> anyhow::Result<Option<Track>> {
    let body = json!({"videoId": video_id, "isAudioOnly": true, "enablePersistentPlaylistPanel": true});
    let v = api.call_signed_in("next", body).await?;
    Ok(parse_paired_song(&v, video_id))
}

fn parse_paired_song(v: &Value, video_id: &str) -> Option<Track> {
    let panel = &v["contents"]["singleColumnMusicWatchNextResultsRenderer"]["tabbedRenderer"]["watchNextTabbedResultsRenderer"]["tabs"][0]
        ["tabRenderer"]["content"]["musicQueueRenderer"]["content"]["playlistPanelRenderer"];
    arr(&panel["contents"])
        .iter()
        .map(|i| &i["playlistPanelVideoWrapperRenderer"])
        .filter(|w| w["primaryRenderer"]["playlistPanelVideoRenderer"]["videoId"] == video_id)
        .flat_map(|w| arr(&w["counterpart"]))
        .map(|c| &c["counterpartRenderer"]["playlistPanelVideoRenderer"])
        .find(|r| video_type(&[&r["navigationEndpoint"]]) == Some("MUSIC_VIDEO_TYPE_ATV"))
        .and_then(panel_item)
}

/// The song version of a music video, by a signed-out search (a signed-in one would
/// land in the account's search history). Only a sure match counts: [`same_song`].
pub async fn searched_song(api: &Api, video: &Track) -> anyhow::Result<Option<Track>> {
    let title = without_artist(&song_title(&video.title), video);
    let query = match video.artists.first() {
        Some(a) => format!("{title} {}", a.name),
        None => return Ok(None),
    };
    let hits = search_songs(api, &query, false).await?;
    Ok(hits.into_iter().take(5).find(|h| same_song(video, h)))
}

/// Whether `hit` (a song from a search) is the song of the music video `video`: the same
/// title once the video's own markings are off, the same kind of recording (no live,
/// remix, cover, clean… on one side only), everyone on the video credited on the song,
/// and a length that fits. A video can run longer (an intro, a skit), and often shorter:
/// many are cut down from the song.
fn same_song(video: &Track, hit: &Track) -> bool {
    let title = without_artist(&song_title(&video.title), video);
    let base = base_title(&title);
    let fits = match (video.duration, hit.duration) {
        (Some(v), Some(s)) => s + 30.max(v / 5) >= v && s <= v * 2,
        _ => false,
    };
    !hit.music_video
        && !base.is_empty()
        && base == base_title(&hit.title)
        // The whole title: "Song | Acoustic Version" says what the cut-off part is.
        && variants(&video.title) == variants(&hit.title)
        && qualifiers_agree(&title, &hit.title)
        && !video.artists.is_empty()
        && video.artists.iter().all(|a| hit.artists.iter().any(|b| same_artist(a, b)))
        && fits
}

/// Bracketed parts on one title only must be ones that don't change the recording: a
/// featured artist, where it's from, a remaster. "(Clean)" against "(Explicit)", "(A Deal
/// With God)" or "(7" Mix)" on one side only: not sure enough.
fn qualifiers_agree(a: &str, b: &str) -> bool {
    let harmless = |q: &String| {
        let w: Vec<&str> = q.split(' ').collect();
        matches!(w.first(), Some(&("feat" | "ft" | "featuring" | "with" | "from")))
            || w.iter().any(|w| w.starts_with("remaster"))
            || w.iter().all(|w| w.bytes().all(|c| c.is_ascii_digit()))
    };
    let (qa, qb) = (qualifiers(a), qualifiers(b));
    qa.iter().filter(|q| !qb.contains(q)).chain(qb.iter().filter(|q| !qa.contains(q))).all(harmless)
}

/// The words of each bracketed part of a title.
fn qualifiers(title: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut depth, mut part) = (0u32, String::new());
    for c in title.chars() {
        match c {
            '(' | '[' => {
                if depth == 0 {
                    part.clear();
                }
                depth += 1;
            }
            ')' | ']' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    out.push(words(&part));
                }
            }
            _ if depth > 0 => part.push(c),
            _ => {}
        }
    }
    out
}

/// A title without the "Artist - " some music videos lead with.
fn without_artist(title: &str, video: &Track) -> String {
    let Some((lead, rest)) = title.split_once(" - ") else { return title.to_owned() };
    let lead = words(lead);
    if !rest.trim().is_empty() && video.artists.iter().any(|a| words(&a.name) == lead) { rest.trim().to_owned() } else { title.to_owned() }
}

/// The same artist: the same artist page, or the same name, also as one of the names
/// in an unlinked credit ("Asha Bhosle | Bhupinder"). Not a name inside a longer one
/// ("Queen Tribute Band").
fn same_artist(a: &Link, b: &Link) -> bool {
    if a.id.is_some() && a.id == b.id {
        return true;
    }
    let name = words(&a.name);
    let lower = b.name.to_lowercase();
    !name.is_empty() && [" | ", " & ", ", ", " x ", " feat. ", " ft. ", " and ", " with "].iter().fold(vec![lower.as_str()], |parts, sep| parts.iter().flat_map(|p| p.split(sep)).collect()).into_iter().any(|p| words(p) == name)
}

/// A music video's title without what marks it as a video: "(Official Video)", "[4K]",
/// "| Lyric Video", "Official Music Video" at the end.
fn song_title(title: &str) -> String {
    const VIDEO_WORDS: &[&str] = &[
        "official", "video", "music", "lyric", "lyrics", "lyrical", "audio", "visualizer", "visualiser", "hd", "4k", "hq", "mv", "clip", "videoclip",
        "officiel", "full", "song", "with",
    ];
    const TRAILING: &[&str] = &["official music video", "official lyric video", "official video", "official audio", "official visualizer", "music video", "lyric video", "lyrical video", "video song", "full video"];
    let mut t = title.split(" | ").next().unwrap_or(title).to_owned();
    // Bracketed parts only about the video ("(Remix Official Video)" says more: it stays).
    let mut out = String::new();
    let mut rest = t.as_str();
    while let Some(open) = rest.find(['(', '[']) {
        let close = if rest[open..].starts_with('(') { ')' } else { ']' };
        let Some(len) = rest[open..].find(close) else { break };
        let inner = &rest[open + 1..open + len];
        out.push_str(&rest[..open]);
        if !words(inner).split(' ').all(|w| VIDEO_WORDS.contains(&w)) {
            out.push_str(&rest[open..=open + len]);
        }
        rest = &rest[open + len + 1..];
    }
    out.push_str(rest);
    t = out.trim().to_owned();
    loop {
        let ends_with = |p: &&str| t.len() >= p.len() && t.is_char_boundary(t.len() - p.len()) && t[t.len() - p.len()..].eq_ignore_ascii_case(p);
        let Some(cut) = TRAILING.iter().find(|p| ends_with(p)).map(|p| t.len() - p.len()) else { break };
        let shorter = t[..cut].trim_end_matches(|c: char| c.is_whitespace() || "-–|:\"'".contains(c)).to_owned();
        if shorter.is_empty() {
            break;
        }
        t = shorter;
    }
    t
}

/// The title's words without anything in brackets ("(feat. X)", "(2009 Remaster)").
fn base_title(title: &str) -> String {
    let mut out = String::new();
    let mut depth = 0u32;
    for c in title.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    words(&out)
}

/// Words that make a recording something other than the song itself.
fn variants(title: &str) -> Vec<String> {
    const VARIANTS: &[&str] = &[
        "live", "acoustic", "remix", "remixed", "cover", "instrumental", "karaoke", "sped", "slowed", "reverb", "nightcore", "8d", "demo",
        "unplugged", "orchestral", "extended", "version", "mashup", "rehearsal", "edit", "mix", "rework", "bootleg",
    ];
    let mut found: Vec<String> = words(title).split(' ').filter(|w| VARIANTS.contains(w)).map(str::to_owned).collect();
    found.sort();
    found.dedup();
    found
}

/// Lowercase words, punctuation dropped ("Don't" = "dont").
fn words(s: &str) -> String {
    let kept: String = s.to_lowercase().chars().filter(|c| c.is_alphanumeric() || c.is_whitespace()).collect();
    kept.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Some albums list music videos where the album has the songs too (all of Tame
/// Impala's Currents does): swap in the album's song at the same position. The Android
/// VR client lists an album's playlist as the album's songs, in album order, without a
/// sign-in; one request for the whole album.
async fn songs_for_videos(a: &mut Collection, videos: &[usize]) {
    let Some(list) = a.save_id.clone().filter(|l| l.starts_with("OLAK")) else { return };
    let songs = match crate::innertube::album_queue(&crate::proxy::http(), &list).await {
        Ok(q) => album_songs(&q),
        Err(e) => {
            log::debug!("album {}: Android VR song list failed: {e:#}", a.id);
            None
        }
    };
    // All or nothing on the count: a row the page leaves out would shift every pair after it.
    let Some(songs) = songs.filter(|s| s.len() == a.tracks.len()) else {
        log::info!("album {}: no song list that lines up with its {} rows; playing as listed", a.id, a.tracks.len());
        return;
    };
    let swapped = swap_songs(a, videos, &songs);
    log::info!("album {}: {swapped} of {} music videos swapped for the songs", a.id, videos.len());
}

/// A song of an album, as the Android VR client lists it.
#[derive(Debug, PartialEq)]
struct AlbumSong {
    id: String,
    title: String,
    duration: Option<u32>,
}

/// The songs of an album in album order, from [`crate::innertube::album_queue`]. `None`
/// unless every entry reads: they're matched to the page's rows by position.
fn album_songs(v: &Value) -> Option<Vec<AlbumSong>> {
    let list = &v["contents"]["singleColumnWatchNextResults"]["playlist"]["playlist"];
    // The same id also opens the album artist's Top Songs ("Song - …").
    if !list["title"].as_str()?.starts_with("Album") {
        return None;
    }
    arr(&list["contents"])
        .iter()
        .map(|e| {
            let r = &e["playlistPanelVideoRenderer"];
            Some(AlbumSong {
                id: r["videoId"].as_str()?.to_owned(),
                title: text(&r["title"])?,
                duration: text(&r["lengthText"]).and_then(|l| parse_duration(&l)),
            })
        })
        .collect()
}

/// Swaps each music video row (`videos`) for the song at its position in `songs` (one
/// per row), when the titles agree. Returns how many changed.
fn swap_songs(a: &mut Collection, videos: &[usize], songs: &[AlbumSong]) -> usize {
    let mut swapped = 0;
    for &i in videos {
        let (Some(t), Some(s)) = (a.tracks.get_mut(i), songs.get(i)) else { continue };
        if t.id != s.id && same_title(&t.title, &s.title) {
            (t.id, t.duration) = (s.id.clone(), s.duration.or(t.duration));
            t.music_video = false;
            swapped += 1;
        }
    }
    swapped
}

/// Titles equal but for case, punctuation ("Don't" = "Dont"), spacing and the guests: the
/// song list leaves out "(feat. …)" ("Levitating" for "Levitating (feat. DaBaby)").
fn same_title(a: &str, b: &str) -> bool {
    let (a, b) = (words(without_guests(a)), words(without_guests(b)));
    !a.is_empty() && a == b
}

/// The title without a closing "(feat. …)", "(ft. …)" or "[featuring …]".
fn without_guests(s: &str) -> &str {
    let s = s.trim_end();
    if let Some(open) = s.rfind(['(', '['])
        && s.ends_with([')', ']'])
    {
        let inner = s[open + 1..].to_lowercase();
        if ["feat", "ft."].iter().any(|p| inner.starts_with(p)) {
            return s[..open].trim_end();
        }
    }
    s
}

/// The album, and which of its rows are music videos rather than songs.
fn parse_album(v: &Value, id: &str) -> Option<(Collection, Vec<usize>)> {
    let two = &v["contents"]["twoColumnBrowseResultsRenderer"];
    let h = header(two)?;
    let title = text(&h["title"])?;
    let words: Vec<String> = runs(&h["subtitle"]).filter_map(|r| r["text"].as_str()).filter(|t| t.trim() != "•").map(str::to_owned).collect();
    let kind = words.first().cloned().unwrap_or_else(|| "Album".into());
    let year = words.iter().rev().find(|w| w.len() == 4 && w.bytes().all(|b| b.is_ascii_digit())).cloned();
    let artists = links(&h["straplineTextOne"]);
    let names = artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ");
    let subtitle = [Some(kind), (!names.is_empty()).then_some(names), year].into_iter().flatten().collect::<Vec<_>>().join(" • ");
    let thumbs = thumbs(&h["thumbnail"]["musicThumbnailRenderer"]);
    let link = Link { name: title.clone(), id: Some(id.to_owned()) };
    let rows = arr(&two["secondaryContents"]["sectionListRenderer"]["contents"]).iter().flat_map(|s| arr(&s["musicShelfRenderer"]["contents"]));
    let rows: Vec<(Track, bool)> = rows.filter_map(|i| Some((list_item(&i["musicResponsiveListItemRenderer"])?, is_video(i)))).collect();
    let videos = rows.iter().enumerate().filter(|(_, (_, video))| *video).map(|(i, _)| i).collect();
    let tracks = rows
        .into_iter()
        .map(|(mut t, _)| {
            // Album rows leave out what the header already says.
            if t.thumbs.is_empty() {
                t.thumbs = thumbs.clone();
            }
            if t.artists.is_empty() {
                t.artists = artists.clone();
            }
            t.album.get_or_insert_with(|| link.clone());
            t
        })
        .collect();
    // The album's playlist ID, which saving it to the library needs.
    let save_id = arr(&h["buttons"]).iter().find_map(|b| {
        let play = &b["musicPlayButtonRenderer"]["playNavigationEndpoint"];
        [&play["watchPlaylistEndpoint"], &play["watchEndpoint"], &b["downloadButtonRenderer"]["command"]["offlinePlaylistEndpoint"]]
            .iter()
            .find_map(|e| e["playlistId"].as_str())
            .map(str::to_owned)
    });
    let a = Collection {
        id: id.to_owned(),
        is_album: true,
        save_id,
        title,
        subtitle,
        artists,
        description: text(&h["description"]["musicDescriptionShelfRenderer"]["description"]),
        thumbs,
        tracks,
        set_ids: Vec::new(),
        count: None,
        owned: false,
    };
    Some((a, videos))
}

/// A playlist (including mixes and Liked Music), with up to `limit` tracks (`None`: all of them).
pub async fn playlist(api: &Api, id: &str, auth: bool, limit: Option<usize>) -> anyhow::Result<Collection> {
    let (mut pl, mut token, limit) = playlist_start(api, id, auth, limit).await?;
    while pl.tracks.len() < limit {
        let Some(t) = token.take() else { break };
        let (more, ids, next) = playlist_more(api, &t, auth).await?;
        if more.is_empty() {
            break;
        }
        pl.tracks.extend(more);
        pl.set_ids.extend(ids);
        token = next;
    }
    pl.tracks.truncate(limit);
    pl.set_ids.truncate(limit);
    Ok(pl)
}

/// A playlist's first page (about 100 songs), the next page's token, and how many
/// songs to stop at: `limit` (`None`: all), or fewer when the header says so.
pub async fn playlist_start(api: &Api, id: &str, auth: bool, limit: Option<usize>) -> anyhow::Result<(Collection, Option<String>, usize)> {
    let v = api.call("browse", json!({ "browseId": format!("VL{id}") }), auth).await?;
    let Some((mut pl, token, count)) = parse_playlist(&v, id) else {
        // A session Google has revoked still gets answers, just signed-out ones.
        anyhow::ensure!(!(auth && api.account().is_some() && crate::ytmapi::logged_out(&v)), "Your YouTube sign-in has expired");
        anyhow::bail!("YouTube Music returned an empty playlist");
    };
    // Mixes (RD…) continue without end: stop at the length the header gives, or at 500.
    let all = count.map_or(if id.starts_with("RD") { 500 } else { usize::MAX }, |c| c as usize);
    let limit = limit.map_or(all, |l| l.min(all));
    pl.tracks.truncate(limit);
    pl.set_ids.truncate(limit);
    Ok((pl, token, limit))
}

/// The playlist page after `token`: its songs, their entry ids, and the next token.
/// Each page names the next, so they can only be read one after another.
pub async fn playlist_more(api: &Api, token: &str, auth: bool) -> anyhow::Result<(Vec<Track>, Vec<Option<String>>, Option<String>)> {
    let v = api.call("browse", json!({ "continuation": token }), auth).await?;
    Ok(playlist_continuation(&v))
}

/// The page, the next page's token, and the song count from the header.
fn parse_playlist(v: &Value, id: &str) -> Option<(Collection, Option<String>, Option<u64>)> {
    let two = &v["contents"]["twoColumnBrowseResultsRenderer"];
    let h = header(two)?;
    let shelf = &two["secondaryContents"]["sectionListRenderer"]["contents"][0];
    let shelf = if shelf["musicPlaylistShelfRenderer"].is_null() { &shelf["musicShelfRenderer"] } else { &shelf["musicPlaylistShelfRenderer"] };
    let (tracks, set_ids, token) = list_page(arr(&shelf["contents"]));
    let token = token.or_else(|| continuation(shelf));
    // The owner: a channel avatar row on newer pages, a strapline on older ones.
    let face = &h["facepile"]["avatarStackViewModel"];
    let owner = face["text"]["content"].as_str().map(|name| Link {
        name: name.to_owned(),
        id: face["rendererContext"]["commandContext"]["onTap"]["innertubeCommand"]["browseEndpoint"]["browseId"].as_str().map(str::to_owned),
    });
    let owners: Vec<Link> = owner.into_iter().chain(links(&h["straplineTextOne"])).collect();
    let count = runs(&h["secondSubtitle"])
        .filter_map(|r| r["text"].as_str())
        .find(|t| ["song", "track", "episode", "video"].iter().any(|w| t.contains(w)))
        .and_then(|t| t.split_whitespace().next()?.replace(',', "").parse::<u64>().ok());
    let sub: Vec<String> = owners.first().map(|o| o.name.clone()).into_iter().collect();
    let count = count.or(Some(tracks.len() as u64));
    let thumb = &h["thumbnail"];
    let thumb = if thumb["musicThumbnailRenderer"].is_null() { &thumb["croppedSquareThumbnailRenderer"] } else { &thumb["musicThumbnailRenderer"] };
    Some((
        Collection {
            id: id.to_owned(),
            is_album: false,
            save_id: None,
            title: text(&h["title"])?,
            subtitle: sub.join(" • "),
            count,
            artists: owners.into_iter().filter(|o| o.id.is_some()).collect(),
            description: text(&h["description"]["musicDescriptionShelfRenderer"]["description"]),
            thumbs: thumbs(thumb),
            tracks,
            set_ids,
            owned: owned(two),
        },
        token,
        count,
    ))
}

/// The header of an album or playlist page; your own playlists wrap it in an editable one.
fn header(two: &Value) -> Option<&Value> {
    let c = &two["tabs"][0]["tabRenderer"]["content"]["sectionListRenderer"]["contents"][0];
    [&c["musicResponsiveHeaderRenderer"], &c["musicEditablePlaylistDetailHeaderRenderer"]["header"]["musicResponsiveHeaderRenderer"]]
        .into_iter()
        .find(|h| !h.is_null())
}

/// Your own playlists have the editable header.
fn owned(two: &Value) -> bool {
    !two["tabs"][0]["tabRenderer"]["content"]["sectionListRenderer"]["contents"][0]["musicEditablePlaylistDetailHeaderRenderer"].is_null()
}

fn playlist_continuation(v: &Value) -> (Vec<Track>, Vec<Option<String>>, Option<String>) {
    let items = &v["onResponseReceivedActions"][0]["appendContinuationItemsAction"]["continuationItems"];
    if !items.is_null() {
        return list_page(arr(items));
    }
    let shelf = &v["continuationContents"]["musicPlaylistShelfContinuation"];
    let (tracks, ids, token) = list_page(arr(&shelf["contents"]));
    (tracks, ids, token.or_else(|| continuation(shelf)))
}

/// Song rows, each one's playlist entry id (`setVideoId`, what removing it takes), and the
/// token for the next page when the list continues.
fn list_page(items: &[Value]) -> (Vec<Track>, Vec<Option<String>>, Option<String>) {
    let (tracks, ids) = items
        .iter()
        .filter_map(|i| {
            let r = &i["musicResponsiveListItemRenderer"];
            Some((list_item(r)?, r["playlistItemData"]["playlistSetVideoId"].as_str().map(str::to_owned)))
        })
        .unzip();
    (tracks, ids, item_token(items))
}

// ----------------------------------------------------------------------- artists

pub async fn artist(api: &Api, id: &str) -> anyhow::Result<ArtistPage> {
    let v = api.call("browse", json!({ "browseId": id }), false).await?;
    parse_artist(&v, id).ok_or_else(|| anyhow::anyhow!("YouTube Music returned an empty artist page"))
}

fn parse_artist(v: &Value, id: &str) -> Option<ArtistPage> {
    let h = ["musicImmersiveHeaderRenderer", "musicVisualHeaderRenderer"].iter().map(|k| &v["header"][*k]).find(|h| !h.is_null())?;
    let name = text(&h["title"])?;
    let sub = &h["subscriptionButton"]["subscribeButtonRenderer"];
    let subscribers = text(&sub["subscriberCountText"]).or_else(|| text(&sub["shortSubscriberCountText"])).and_then(|t| count(&t));
    let sections = arr(&v["contents"]["singleColumnBrowseResultsRenderer"]["tabs"][0]["tabRenderer"]["content"]["sectionListRenderer"]["contents"]);
    let me = Link { name: name.clone(), id: Some(id.to_owned()) };
    let mut page = ArtistPage {
        id: id.to_owned(),
        banner: thumbs(&h["thumbnail"]["musicThumbnailRenderer"]),
        subscribers,
        description: text(&h["description"]),
        top: Vec::new(),
        top_playlist: None,
        albums: Vec::new(),
        playlists: Vec::new(),
        similar: Vec::new(),
        name,
    };
    for s in sections {
        let shelf = &s["musicShelfRenderer"];
        if !shelf.is_null() && page.top.is_empty() {
            page.top = arr(&shelf["contents"]).iter().filter_map(|i| list_item(&i["musicResponsiveListItemRenderer"])).collect();
            let ep = if shelf["bottomEndpoint"].is_null() { &shelf["title"]["runs"][0]["navigationEndpoint"] } else { &shelf["bottomEndpoint"] };
            page.top_playlist = ep["browseEndpoint"]["browseId"].as_str().map(|b| b.strip_prefix("VL").unwrap_or(b).to_owned());
            continue;
        }
        if page.description.is_none() {
            page.description = text(&s["musicDescriptionShelfRenderer"]["description"]);
        }
        let carousel = &s["musicCarouselShelfRenderer"];
        let title = text(&carousel["header"]["musicCarouselShelfBasicHeaderRenderer"]["title"]).unwrap_or_default();
        // Release shelves list just the year; spell out the kind and artist like elsewhere.
        let kind = if title.starts_with("Single") { "Single" } else { "Album" };
        for card in arr(&carousel["contents"]).iter().filter_map(|i| two_row(&i["musicTwoRowItemRenderer"])) {
            match card.target {
                Target::Album(_) => {
                    let subtitle = [kind, me.name.as_str(), card.subtitle.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" • ");
                    let links = if card.links.is_empty() { vec![me.clone()] } else { card.links.clone() };
                    page.albums.push(Card { subtitle, links, ..card });
                }
                Target::Playlist(_) => page.playlists.push(card),
                Target::Artist(_) => page.similar.push(card),
                Target::Song(_) => {}
            }
        }
    }
    Some(page)
}

// ------------------------------------------------------------- radio and explore

/// Songs to play after `video_id` (its radio), at least `limit` when there are enough.
/// A music video's radio is music videos only (a song's is songs): then those, rather
/// than nothing. Its song's radio is the one to ask for.
pub async fn radio(api: &Api, video_id: &str, auth: bool, limit: usize) -> anyhow::Result<Vec<Track>> {
    let body = json!({
        "playlistId": format!("RDAMVM{video_id}"),
        "params": "wAEB8gECeAE%3D",
        "enablePersistentPlaylistPanel": true,
        "isAudioOnly": true,
        "tunerSettingValue": "AUTOMIX_SETTING_NORMAL",
    });
    let v = api.call("next", body, auth).await?;
    let panel = &v["contents"]["singleColumnMusicWatchNextResultsRenderer"]["tabbedRenderer"]["watchNextTabbedResultsRenderer"]["tabs"][0]
        ["tabRenderer"]["content"]["musicQueueRenderer"]["content"]["playlistPanelRenderer"];
    let (mut items, mut token) = panel_page(panel);
    while radio_tracks(&items).count() < limit {
        let Some(t) = token.take() else { break };
        let body = json!({ "continuation": t, "enablePersistentPlaylistPanel": true, "isAudioOnly": true });
        let v = api.call("next", body, auth).await?;
        let (more, next) = panel_page(&v["continuationContents"]["playlistPanelContinuation"]);
        if more.is_empty() {
            break;
        }
        items.extend(more);
        token = next;
    }
    Ok(radio_tracks(&items).cloned().collect())
}

/// A radio's songs, without its videos (`is_video`); all of it when it's videos only.
fn radio_tracks(items: &[(Track, bool)]) -> impl Iterator<Item = &Track> {
    let songs = items.iter().any(|(_, video)| !video);
    items.iter().filter(move |(_, video)| !songs || !video).map(|(t, _)| t)
}

/// The queue YouTube Music keeps for the account across devices: what the phone (or
/// the web player) last played, the way music.youtube.com picks it up when it opens.
/// Only Premium accounts have one; others get `None`.
pub async fn server_queue(api: &Api) -> anyhow::Result<Option<ServerQueue>> {
    let body = json!({"watchNextType": "WATCH_NEXT_TYPE_GET_QUEUE", "queueContextParams": "", "enablePersistentPlaylistPanel": true, "isAudioOnly": true});
    let v = api.call_signed_in("next", body).await?;
    Ok(parse_server_queue(&v))
}

pub struct ServerQueue {
    pub tracks: Vec<Track>,
    /// The song that was playing.
    pub index: usize,
    /// When it was last listened to, in unix seconds (0 if YouTube didn't say).
    pub watched_at: u64,
}

fn parse_server_queue(v: &Value) -> Option<ServerQueue> {
    let queue = &v["contents"]["singleColumnMusicWatchNextResultsRenderer"]["tabbedRenderer"]["watchNextTabbedResultsRenderer"]["tabs"][0]["tabRenderer"]["content"]["musicQueueRenderer"];
    let (mut tracks, mut index) = (Vec::new(), None);
    for i in arr(&queue["content"]["playlistPanelRenderer"]["contents"]) {
        let wrapped = &i["playlistPanelVideoWrapperRenderer"]["primaryRenderer"]["playlistPanelVideoRenderer"];
        let r = if wrapped.is_null() { &i["playlistPanelVideoRenderer"] } else { wrapped };
        let Some(t) = panel_item(r) else { continue };
        if r["selected"].as_bool() == Some(true) {
            index = Some(tracks.len());
        }
        tracks.push(t);
    }
    let watched_at = queue["musicQueueConfig"]["musicQueueRestorationInfo"]["lastWatchTimestamp"]["seconds"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
    Some(ServerQueue { index: index?, tracks, watched_at })
}

/// A radio page's tracks, each with whether it's a video (`is_video`).
fn panel_page(panel: &Value) -> (Vec<(Track, bool)>, Option<String>) {
    let tracks = arr(&panel["contents"])
        .iter()
        .filter_map(|i| {
            let wrapped = &i["playlistPanelVideoWrapperRenderer"]["primaryRenderer"]["playlistPanelVideoRenderer"];
            let r = if wrapped.is_null() { &i["playlistPanelVideoRenderer"] } else { wrapped };
            Some((panel_item(r)?, is_video(r)))
        })
        .collect();
    let token = panel["continuations"][0]["nextRadioContinuationData"]["continuation"].as_str().map(str::to_owned);
    (tracks, token)
}

pub struct Charts {
    /// Most played songs, when the chart page has a songs shelf.
    pub top: Vec<Track>,
    pub playlists: Vec<Card>,
    /// Channel IDs of the chart's top artists.
    pub artists: Vec<String>,
}

/// US charts.
pub async fn charts(api: &Api) -> anyhow::Result<Charts> {
    let body = json!({ "browseId": "FEmusic_charts", "params": "sgYPRkVtdXNpY19leHBsb3Jl", "formData": { "selectedValues": ["US"] } });
    Ok(parse_charts(&api.call("browse", body, false).await?))
}

fn parse_charts(v: &Value) -> Charts {
    let mut c = Charts { top: Vec::new(), playlists: Vec::new(), artists: Vec::new() };
    let sections = &v["contents"]["singleColumnBrowseResultsRenderer"]["tabs"][0]["tabRenderer"]["content"]["sectionListRenderer"]["contents"];
    for s in arr(sections) {
        for i in arr(&s["musicCarouselShelfRenderer"]["contents"]).iter().filter(|i| !is_video(i)) {
            if let Some(card) = two_row(&i["musicTwoRowItemRenderer"]) {
                if matches!(card.target, Target::Playlist(_)) {
                    c.playlists.push(card);
                }
                continue;
            }
            let r = &i["musicResponsiveListItemRenderer"];
            if let Some(Card { target: Target::Artist(id), .. }) = list_card(r) {
                c.artists.push(id);
            } else if c.top.len() < 100
                && let Some(t) = list_item(r)
            {
                c.top.push(t);
            }
        }
    }
    c
}

/// New albums and singles (personalised when signed in).
pub async fn new_albums(api: &Api, auth: bool) -> anyhow::Result<Vec<Card>> {
    let v = api.call("browse", json!({ "browseId": "FEmusic_new_releases_albums" }), auth).await?;
    let sections = &v["contents"]["singleColumnBrowseResultsRenderer"]["tabs"][0]["tabRenderer"]["content"]["sectionListRenderer"]["contents"];
    Ok(grid_cards(arr(sections)))
}

fn grid_cards(sections: &[Value]) -> Vec<Card> {
    sections
        .iter()
        .flat_map(|s| arr(&s["gridRenderer"]["items"]))
        .filter_map(|i| two_row(&i["musicTwoRowItemRenderer"]))
        .collect()
}

// ------------------------------------------------------------------------ library

/// Saved playlists or albums: a library page, opened with its fixed continuation token
/// (that's how music.youtube.com sorts it by recent activity), then every following page.
async fn saved(api: &Api, token: &str) -> anyhow::Result<Vec<Card>> {
    let v = api.call_signed_in("browse", json!({ "continuation": token })).await?;
    let sections = arr(&v["continuationContents"]["sectionListContinuation"]["contents"]);
    let mut cards = grid_cards(sections);
    let mut token = sections.iter().find_map(|s| grid_next(&s["gridRenderer"]));
    for _ in 0..100 {
        let Some(t) = token.take() else { break };
        // A later page failing keeps the pages already loaded.
        let v = match api.call_signed_in("browse", json!({ "continuation": t })).await {
            Ok(v) => v,
            Err(e) => {
                log::warn!("library page: {e}");
                break;
            }
        };
        let (more, next) = grid_continuation(&v);
        if more.is_empty() {
            break;
        }
        cards.extend(more);
        token = next;
    }
    Ok(cards)
}

/// The cards of a grid's next page, and the token for the page after it.
fn grid_continuation(v: &Value) -> (Vec<Card>, Option<String>) {
    let items = &v["onResponseReceivedActions"][0]["appendContinuationItemsAction"]["continuationItems"];
    if !items.is_null() {
        return (two_rows(arr(items)), item_token(arr(items)));
    }
    let grid = &v["continuationContents"]["gridContinuation"];
    (two_rows(arr(&grid["items"])), grid_next(grid))
}

/// A grid's next-page token: in its continuations, or as a trailing continuation item.
fn grid_next(grid: &Value) -> Option<String> {
    continuation(grid).or_else(|| item_token(arr(&grid["items"])))
}

fn two_rows(items: &[Value]) -> Vec<Card> {
    items.iter().filter_map(|i| two_row(&i["musicTwoRowItemRenderer"])).collect()
}

/// The token of a trailing "load more" item.
fn item_token(items: &[Value]) -> Option<String> {
    items.iter().find_map(|i| i["continuationItemRenderer"]["continuationEndpoint"]["continuationCommand"]["token"].as_str().map(str::to_owned))
}

pub async fn saved_playlists(api: &Api) -> anyhow::Result<Vec<Card>> {
    saved(api, "4qmFsgIrEhdGRW11c2ljX2xpa2VkX3BsYXlsaXN0cxoQZ2dNR0tnUUlBQkFCb0FZQg%3D%3D").await
}

pub async fn saved_albums(api: &Api) -> anyhow::Result<Vec<Card>> {
    saved(api, "4qmFsgIoEhRGRW11c2ljX2xpa2VkX2FsYnVtcxoQZ2dNR0tnUUlBQkFCb0FZQg%3D%3D").await
}

/// The signed-in account's name and photo, as its account menu shows them (for a brand
/// account, the channel's).
pub async fn account(api: &Api) -> anyhow::Result<(Option<String>, Option<String>)> {
    let v = api.call_signed_in("account/account_menu", json!({})).await?;
    let h = find_key(&v, "activeAccountHeaderRenderer").ok_or_else(|| anyhow::anyhow!("no account header"))?;
    let name = text(&h["accountName"]);
    let photo = arr(&h["accountPhoto"]["thumbnails"]).last().and_then(|t| t["url"].as_str()).map(str::to_owned);
    log::info!("account menu: name {}, photo {}", name.is_some(), photo.is_some());
    Ok((name, photo))
}

/// One of the signed-in Google account's channels: its own, or a brand channel it manages.
#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    pub name: String,
    pub handle: Option<String>,
    /// The Google account itself may have no YouTube channel ("No channel" in the
    /// switcher); it still has a library of its own.
    pub has_channel: bool,
    /// `None` for the Google account's own channel.
    pub page_id: Option<String>,
}

/// The channels the app can act as, the Google account's own first, from its account
/// switcher. Asked as the account's own channel, so a brand channel that no longer
/// works can't hide the list.
pub async fn channels(api: &Api) -> anyhow::Result<Vec<Channel>> {
    let v = api.call_as_google_account("account/accounts_list", json!({})).await?;
    let list = parse_channels(&v);
    log::info!("account switcher: {} channel(s)", list.len());
    anyhow::ensure!(!list.is_empty(), "the account switcher listed no channels");
    Ok(list)
}

fn parse_channels(v: &Value) -> Vec<Channel> {
    fn items<'a>(v: &'a Value, out: &mut Vec<&'a Value>) {
        match v {
            Value::Object(m) => m.iter().for_each(|(k, x)| if k == "accountItem" { out.push(x) } else { items(x, out) }),
            Value::Array(a) => a.iter().for_each(|x| items(x, out)),
            _ => {}
        }
    }
    let plain = |v: &Value| v["simpleText"].as_str().map(str::to_owned).or_else(|| text(v)).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    let mut found = Vec::new();
    items(v, &mut found);
    // (selected, the sign-in the item belongs to, the channel)
    let listed: Vec<(bool, Option<String>, Channel)> = found
        .into_iter()
        .filter_map(|i| {
            let tokens = arr(&i["serviceEndpoint"]["selectActiveIdentityEndpoint"]["supportedTokens"]);
            // "<page>||<manager>" for a brand channel, "<account>||" for an account's own.
            let datasync = tokens.iter().find_map(|t| t["datasyncIdToken"]["datasyncIdToken"].as_str()).and_then(|d| d.split_once("||"));
            let page_id = tokens.iter().find_map(|t| t["pageIdToken"]["pageId"].as_str()).map(str::to_owned);
            let login = datasync.map(|(a, b)| if b.is_empty() { a } else { b }).filter(|l| !l.is_empty()).map(str::to_owned);
            let handle = plain(&i["channelHandle"]).or_else(|| plain(&i["accountByline"]).filter(|b| b.starts_with('@')));
            let has_channel = i["hasChannel"].as_bool().or_else(|| tokens.iter().find_map(|t| t["accountStateToken"]["hasChannel"].as_bool())).unwrap_or(true);
            let channel = Channel { name: plain(&i["accountName"])?, handle, has_channel, page_id };
            Some((i["isSelected"].as_bool().unwrap_or(false), login, channel))
        })
        .collect();
    // Asked as the account's own channel, that's the selected item without a page. Other
    // items without one are other Google accounts signed in in the same browser, which
    // a page id can't act as; brand channels go with the account that manages them.
    // Nothing is guessed: without the selected one there's no list, and a brand channel
    // whose account isn't known is listed only when no other account is.
    let Some(own) = listed.iter().position(|(sel, _, c)| *sel && c.page_id.is_none()) else {
        return Vec::new();
    };
    let login = listed[own].1.clone();
    let alone = listed.iter().filter(|(_, _, c)| c.page_id.is_none()).count() == 1;
    let mut out = vec![listed[own].2.clone()];
    for (_, l, c) in listed {
        let theirs = match (&login, &l) {
            (Some(own), Some(l)) => own == l,
            _ => alone,
        };
        if c.page_id.is_some() && theirs && !out.iter().any(|o| o.page_id == c.page_id) {
            out.push(c);
        }
    }
    out
}

/// The first value under `key`, anywhere in `v`.
fn find_key<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(m) => m.get(key).or_else(|| m.values().find_map(|x| find_key(x, key))),
        Value::Array(a) => a.iter().find_map(|x| find_key(x, key)),
        _ => None,
    }
}

/// Recently played, newest first, each song once.
pub async fn history(api: &Api) -> anyhow::Result<Vec<Track>> {
    let v = api.call_signed_in("browse", json!({ "browseId": "FEmusic_history", "params": "oggECgIIAQ%3D%3D" })).await?;
    let sections = &v["contents"]["singleColumnBrowseResultsRenderer"]["tabs"][0]["tabRenderer"]["content"]["sectionListRenderer"]["contents"];
    let mut seen = std::collections::HashSet::new();
    Ok(arr(sections)
        .iter()
        .flat_map(|s| arr(&s["musicShelfRenderer"]["contents"]))
        .filter_map(|i| list_item(&i["musicResponsiveListItemRenderer"]))
        .filter(|t| seen.insert(t.id.clone()))
        .collect())
}

/// Songs the account uploaded (Library > Uploads on music.youtube.com).
pub async fn uploads(api: &Api) -> anyhow::Result<Vec<Track>> {
    let v = api.call_signed_in("browse", json!({ "browseId": "FEmusic_library_privately_owned_tracks" })).await?;
    let tabs = arr(&v["contents"]["singleColumnBrowseResultsRenderer"]["tabs"]);
    let shelf = tabs
        .iter()
        .flat_map(|t| arr(&t["tabRenderer"]["content"]["sectionListRenderer"]["contents"]))
        .map(|c| &c["musicShelfRenderer"])
        .find(|s| !s.is_null());
    let Some(shelf) = shelf else { return Ok(Vec::new()) };
    let mut tracks = upload_rows(shelf);
    let mut token = continuation(shelf);
    for _ in 0..20 {
        let Some(t) = token.take() else { break };
        let v = api.call_signed_in("browse", json!({ "continuation": t })).await?;
        let more = &v["continuationContents"]["musicShelfContinuation"];
        tracks.extend(upload_rows(more));
        token = continuation(more);
    }
    Ok(tracks)
}

fn upload_rows(shelf: &Value) -> Vec<Track> {
    arr(&shelf["contents"]).iter().filter_map(|i| list_item(&i["musicResponsiveListItemRenderer"])).collect()
}

// -------------------------------------------------------------------------- edits

/// Like or un-like a song for the signed-in account.
pub async fn set_like(api: &Api, video_id: &str, like: bool) -> anyhow::Result<()> {
    let endpoint = if like { "like/like" } else { "like/removelike" };
    write(api, endpoint, json!({ "target": { "videoId": video_id } })).await?;
    Ok(())
}

pub async fn dislike(api: &Api, video_id: &str) -> anyhow::Result<()> {
    write(api, "like/dislike", json!({"target": {"videoId": video_id}})).await?;
    Ok(())
}

/// Tell YouTube the account played a song, so it lands in watch history (and from
/// there in Recap, recommendations and "Listen again"), the way music.youtube.com does:
/// the signed-in player response carries a stats URL that the web player pings once
/// playback starts. Streams come from other clients, so this asks for the player
/// response only to get that URL. What it returns then reports how much of the song
/// was heard ([`report_watchtime`]), as the web player does while it plays.
pub async fn report_play(api: &Api, video_id: &str) -> anyhow::Result<Watchtime> {
    let player = api.call_signed_in("player", json!({"videoId": video_id, "contentCheckOk": true, "racyCheckOk": true})).await?;
    let tracking = &player["playbackTracking"];
    let url = tracking["videostatsPlaybackUrl"]["baseUrl"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("no playback tracking URL ({})", player["playabilityStatus"]["status"].as_str().unwrap_or("?")))?;
    let cpn = cpn();
    api.get_signed_in(url, &[("ver", "2"), ("c", "WEB_REMIX"), ("cpn", &cpn)]).await?;
    Ok(Watchtime {
        url: tracking["videostatsWatchtimeUrl"]["baseUrl"].as_str().map(str::to_owned),
        cpn,
        flush_at: arr(&tracking["videostatsScheduledFlushWalltimeSeconds"]).iter().filter_map(Value::as_u64).collect(),
        flush_every: tracking["videostatsDefaultFlushIntervalSeconds"].as_u64().filter(|&s| s > 0).unwrap_or(40),
    })
}

/// One play's listening-time reports: where they go and when the web player sends them.
pub struct Watchtime {
    url: Option<String>,
    cpn: String,
    /// Seconds after the start for the first reports (10, 20, 30), then one every `flush_every`.
    pub flush_at: Vec<u64>,
    pub flush_every: u64,
}

/// Report the stretch of the song played since the last report, `from`..`to` seconds
/// in, like the web player's periodic pings; `last` once the song ends or is left.
pub async fn report_watchtime(api: &Api, w: &Watchtime, from: f64, to: f64, elapsed: f64, last: bool) -> anyhow::Result<()> {
    let Some(url) = &w.url else { return Ok(()) };
    let (st, et, rt) = (format!("{from:.3}"), format!("{to:.3}"), format!("{elapsed:.3}"));
    let mut query = vec![("ver", "2"), ("c", "WEB_REMIX"), ("cpn", w.cpn.as_str()), ("cmt", et.as_str()), ("st", st.as_str()), ("et", et.as_str()), ("rt", rt.as_str()), ("state", if last { "paused" } else { "playing" })];
    if last {
        query.push(("final", "1"));
    }
    api.get_signed_in(url, &query).await
}

/// A client playback nonce: 16 characters from the web player's alphabet.
fn cpn() -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut x = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0x9e37_79b9_7f4a_7c15, |d| d.as_nanos() as u64) | 1;
    (0..16)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            ALPHABET[(x & 63) as usize] as char
        })
        .collect()
}

/// Check the response here so a rejected edit never looks successful in the UI.
async fn write(api: &Api, endpoint: &str, body: Value) -> anyhow::Result<Value> {
    let value = api.call_signed_in(endpoint, body).await?;
    if value["status"].as_str().is_some_and(|s| !s.contains("SUCCEEDED")) {
        anyhow::bail!("YouTube Music rejected the change");
    }
    Ok(value)
}

pub async fn create_playlist(api: &Api, title: &str, video_id: Option<&str>) -> anyhow::Result<String> {
    let mut body = json!({"title": title, "description": "", "privacyStatus": "PRIVATE"});
    if let Some(id) = video_id {
        body["videoIds"] = json!([id]);
    }
    let value = write(api, "playlist/create", body).await?;
    value["playlistId"].as_str().map(str::to_owned).ok_or_else(|| anyhow::anyhow!("YouTube Music did not return a playlist"))
}

/// Create a private playlist holding `video_ids` (a few hundred at most; add more with [`add_videos`]).
pub async fn create_playlist_with(api: &Api, title: &str, video_ids: &[String]) -> anyhow::Result<String> {
    // YouTube Music errors out on titles containing these.
    let title: String = title.chars().filter(|c| !matches!(c, '<' | '>')).collect();
    let value = write(api, "playlist/create", json!({"title": title, "description": "", "privacyStatus": "PRIVATE", "videoIds": video_ids})).await?;
    value["playlistId"].as_str().map(str::to_owned).ok_or_else(|| anyhow::anyhow!("YouTube Music did not return a playlist"))
}

/// Add songs to a playlist in one request, skipping any already in it.
pub async fn add_videos(api: &Api, playlist_id: &str, video_ids: &[String]) -> anyhow::Result<()> {
    let actions: Vec<Value> = video_ids.iter().map(|v| json!({"action": "ACTION_ADD_VIDEO", "addedVideoId": v, "dedupeOption": "DEDUPE_OPTION_SKIP"})).collect();
    write(api, "browse/edit_playlist", json!({"playlistId": playlist_id, "actions": actions})).await?;
    Ok(())
}

pub async fn edit_playlist(api: &Api, id: &str, title: &str, description: &str, privacy: Option<&str>) -> anyhow::Result<()> {
    let mut actions = vec![
        json!({"action": "ACTION_SET_PLAYLIST_NAME", "playlistName": title}),
        json!({"action": "ACTION_SET_PLAYLIST_DESCRIPTION", "playlistDescription": description}),
    ];
    if let Some(privacy) = privacy {
        actions.push(json!({"action": "ACTION_SET_PLAYLIST_PRIVACY", "playlistPrivacy": privacy}));
    }
    write(api, "browse/edit_playlist", json!({"playlistId": id, "actions": actions})).await?;
    Ok(())
}

/// Take entries out of a playlist, `(videoId, setVideoId)` pairs, in one request.
pub async fn remove_videos(api: &Api, playlist_id: &str, items: &[(String, String)]) -> anyhow::Result<()> {
    let actions: Vec<Value> = items.iter().map(|(v, set)| json!({"action": "ACTION_REMOVE_VIDEO", "removedVideoId": v, "setVideoId": set})).collect();
    write(api, "browse/edit_playlist", json!({"playlistId": playlist_id, "actions": actions})).await?;
    Ok(())
}

pub async fn delete_playlist(api: &Api, id: &str) -> anyhow::Result<()> {
    let response = write(api, "playlist/delete", json!({"playlistId": id})).await?;
    let deleted_id = arr(&response["command"]["commandExecutorCommand"]["commands"])
        .iter()
        .find_map(|command| command["handlePlaylistDeletionCommand"]["playlistId"].as_str());
    anyhow::ensure!(deleted_id == Some(id), "YouTube Music did not confirm playlist deletion");
    Ok(())
}

pub async fn save_collection(api: &Api, playlist_id: &str, save: bool) -> anyhow::Result<()> {
    let endpoint = if save { "like/like" } else { "like/removelike" };
    write(api, endpoint, json!({"target": {"playlistId": playlist_id}})).await?;
    Ok(())
}

/// A playlist's collaboration settings, as music.youtube.com's Collaborate panel shows
/// them. YouTube adds collaborators by invite link only: whoever opens the link while
/// "Allow new collaborators" is on can add songs.
#[derive(Clone)]
pub struct Collaboration {
    pub playlist_id: String,
    /// Private playlists can't be collaborative.
    pub private: bool,
    pub on: bool,
    pub allow_new: bool,
    pub invite_link: Option<String>,
    pub collaborators: Vec<String>,
    /// The panel's own edits for each switch (`browse/edit_playlist` bodies).
    turn_on: Option<Value>,
    turn_off: Option<Value>,
    allow: Option<Value>,
    revoke: Option<Value>,
}

pub enum CollabChange {
    Collaborate(bool),
    AllowNew(bool),
}

pub async fn collaboration(api: &Api, playlist_id: &str) -> anyhow::Result<Collaboration> {
    let page = api.call_signed_in("browse", json!({"browseId": format!("VL{playlist_id}")})).await?;
    let header = find_key(&page, "musicPlaylistEditHeaderRenderer").ok_or_else(|| anyhow::anyhow!("only your own playlists can be shared for collaboration"))?;
    let private = header["privacy"].as_str() == Some("PRIVATE");
    let Some(params) = header["collaborationSettingsCommand"]["showEngagementPanelEndpoint"]["globalConfiguration"]["params"].as_str() else {
        // A private playlist has no panel until it's made visible: collaboration is off,
        // and turning it on goes through the plain edits.
        anyhow::ensure!(private, "YouTube Music doesn't offer collaboration on this playlist");
        return Ok(Collaboration { playlist_id: playlist_id.to_owned(), private, on: false, allow_new: false, invite_link: None, collaborators: Vec::new(), turn_on: None, turn_off: None, allow: None, revoke: None });
    };
    let panel = api.call_signed_in("get_panel", json!({"panelId": "PAplaylist_collaborate", "params": params})).await?;
    parse_collaboration(playlist_id, private, &panel).ok_or_else(|| anyhow::anyhow!("couldn't read the playlist's collaboration settings"))
}

fn parse_collaboration(playlist_id: &str, private: bool, panel: &Value) -> Option<Collaboration> {
    let vm = find_key(panel, "playlistCollaborationViewModel")?;
    let values = &vm["playlistCollaborationFormSchema"]["initialValues"];
    let switch = |key: &str| &vm[key]["switchListItemViewModel"];
    // A switch's command: the edit itself, or inside a parallel command beside closing the dialog.
    let edit = |cmd: &Value| find_key(cmd, "playlistEditEndpoint").filter(|e| e["actions"].is_array()).cloned();
    let collaborate = switch("collaboratePlaylistCollaborationSetting");
    let allow_new = switch("allowNewCollaboratorsPlaylistCollaborationSetting");
    let link = find_key(&vm["copyLinkButton"], "copyLinkCommand").and_then(|c| c["shortUrl"].as_str()).filter(|u| !u.is_empty());
    // Each collaborator is a list item; its title is the name.
    let mut collaborators = Vec::new();
    collect_titles(&vm["playlistCollaborators"], &mut collaborators);
    Some(Collaboration {
        playlist_id: playlist_id.to_owned(),
        private,
        on: values["isCollaborationEnabled"].as_bool().or(collaborate["switchedOnByDefault"].as_bool()).unwrap_or(false),
        allow_new: values["isAllowNewCollaboratorsEnabled"].as_bool().or(allow_new["switchedOnByDefault"].as_bool()).unwrap_or(false),
        invite_link: link.map(str::to_owned),
        collaborators,
        turn_on: edit(&collaborate["switchOnCommand"]),
        turn_off: edit(&collaborate["switchOffCommand"]),
        allow: edit(&allow_new["switchOnCommand"]),
        revoke: edit(&allow_new["switchOffCommand"]),
    })
}

fn collect_titles(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            if let Some(t) = m.get("title").and_then(|t| t["content"].as_str()) {
                out.push(t.to_owned());
                return;
            }
            m.values().for_each(|x| collect_titles(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| collect_titles(x, out)),
        _ => {}
    }
}

/// Switch collaboration (or new collaborators) on or off with the panel's own edit.
/// Turning it on for a private playlist first makes it unlisted, as it must be.
pub async fn set_collaboration(api: &Api, c: &Collaboration, change: CollabChange) -> anyhow::Result<()> {
    let id = c.playlist_id.as_str();
    let fallback = |action: Value| json!({"playlistId": id, "actions": [action]});
    let body = match change {
        CollabChange::Collaborate(true) if c.private => {
            write(api, "browse/edit_playlist", fallback(json!({"action": "ACTION_SET_PLAYLIST_PRIVACY", "playlistPrivacy": "UNLISTED"}))).await?;
            // For several seconds after that, YouTube still sees it as private and
            // refuses the invite link ("Precondition check failed").
            let invite = fallback(json!({"action": "ACTION_CREATE_COLLABORATION_INVITE_LINK"}));
            for _ in 0..8 {
                match write(api, "browse/edit_playlist", invite.clone()).await {
                    Err(e) if e.downcast_ref::<crate::ytmapi::ApiError>().is_some_and(|a| a.status == 400) => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
                    r => return r.map(drop),
                }
            }
            invite
        }
        CollabChange::Collaborate(true) => c.turn_on.clone().unwrap_or_else(|| fallback(json!({"action": "ACTION_CREATE_COLLABORATION_INVITE_LINK"}))),
        CollabChange::Collaborate(false) => c.turn_off.clone().unwrap_or_else(|| fallback(json!({"action": "ACTION_SET_CLOSED_TO_CONTRIBUTIONS", "closedToContributions": true}))),
        CollabChange::AllowNew(true) => c.allow.clone().unwrap_or_else(|| fallback(json!({"action": "ACTION_CREATE_COLLABORATION_INVITE_LINK"}))),
        CollabChange::AllowNew(false) => c.revoke.clone().unwrap_or_else(|| fallback(json!({"action": "ACTION_REVOKE_COLLABORATION_TOKENS"}))),
    };
    write(api, "browse/edit_playlist", body).await?;
    Ok(())
}

// ------------------------------------------------------------------------ parsing

fn continuation(sec: &Value) -> Option<String> {
    sec["continuations"][0]["nextContinuationData"]["continuation"].as_str().map(str::to_owned)
}

fn shelves(contents: &Value) -> Vec<Shelf> {
    arr(contents).iter().filter_map(shelf).collect()
}

fn shelf(s: &Value) -> Option<Shelf> {
    let r = &s["musicCarouselShelfRenderer"];
    if r.is_null() {
        return None;
    }
    let h = &r["header"]["musicCarouselShelfBasicHeaderRenderer"];
    let title = text(&h["title"])?;
    let strapline = text(&h["strapline"]);
    let strap_thumb = thumbs(&h["thumbnail"]["musicThumbnailRenderer"]).into_iter().next().map(|t| t.url);
    let items: Vec<&Value> = arr(&r["contents"]).iter().filter(|i| !is_video(i)).collect();
    let kind = if items.iter().any(|i| !i["musicResponsiveListItemRenderer"].is_null()) {
        let tracks: Vec<Track> = items.iter().filter_map(|i| list_item(&i["musicResponsiveListItemRenderer"])).collect();
        let rows = r["numItemsPerColumn"].as_str().and_then(|n| n.parse().ok()).unwrap_or(4);
        ShelfKind::Songs { tracks, rows }
    } else {
        ShelfKind::Cards(items.iter().filter_map(|i| two_row(&i["musicTwoRowItemRenderer"])).collect())
    };
    let empty = match &kind {
        ShelfKind::Cards(c) => c.is_empty(),
        ShelfKind::Songs { tracks, .. } => tracks.is_empty(),
    };
    (!empty).then_some(Shelf { title, strapline, strap_thumb, kind })
}

/// A square/round card (album, playlist, artist, or a song shown as a card).
fn two_row(r: &Value) -> Option<Card> {
    let title = text(&r["title"])?;
    let subtitle = text(&r["subtitle"]).unwrap_or_default();
    let thumbs = thumbs(&r["thumbnailRenderer"]["musicThumbnailRenderer"]);
    let nav = &r["navigationEndpoint"];
    let target = if let Some(video_id) = nav["watchEndpoint"]["videoId"].as_str() {
        Target::Song(Track {
            id: video_id.to_owned(),
            title: title.clone(),
            artists: artist_links(&r["subtitle"]),
            album: None,
            duration: None,
            thumbs: thumbs.clone(),
            track_nr: None,
            plays: None,
            music_video: music_video(&[nav]),
        })
    } else if let Some(playlist) = nav["watchPlaylistEndpoint"]["playlistId"].as_str() {
        Target::Playlist(playlist.to_owned())
    } else {
        target(nav)?
    };
    let round = matches!(target, Target::Artist(_));
    Some(Card { title, subtitle, thumbs, target, round, links: artist_links(&r["subtitle"]) })
}

/// What a browse endpoint opens.
fn target(nav: &Value) -> Option<Target> {
    let id = nav["browseEndpoint"]["browseId"].as_str()?.to_owned();
    Some(match page_type(nav)? {
        "MUSIC_PAGE_TYPE_ALBUM" | "MUSIC_PAGE_TYPE_AUDIOBOOK" => Target::Album(id),
        "MUSIC_PAGE_TYPE_ARTIST" | "MUSIC_PAGE_TYPE_USER_CHANNEL" => Target::Artist(id),
        "MUSIC_PAGE_TYPE_PLAYLIST" => Target::Playlist(id.strip_prefix("VL").unwrap_or(&id).to_owned()),
        _ => return None,
    })
}

/// A list row that opens a page rather than playing (search results, chart artists).
fn list_card(r: &Value) -> Option<Card> {
    let target = target(&r["navigationEndpoint"])?;
    let col = |i: usize| &r["flexColumns"][i]["musicResponsiveListItemFlexColumnRenderer"]["text"];
    let title = text(col(0))?;
    let detail = col(1);
    let line: String = runs(detail).filter_map(|r| r["text"].as_str()).collect();
    let mut parts: Vec<&str> = line.split(" • ").filter(|p| !p.is_empty()).collect();
    // Search rows lead with the kind ("Artist • 239M monthly audience"); a card shows it already.
    if matches!(target, Target::Artist(_)) && parts.first() == Some(&"Artist") {
        parts.remove(0);
    }
    let subtitle = match &target {
        Target::Playlist(_) => std::iter::once("Playlist").chain(parts.iter().copied().filter(|p| *p != "Playlist")).collect::<Vec<_>>().join(" • "),
        Target::Artist(_) if parts.is_empty() => "Artist".into(),
        _ => parts.join(" • "),
    };
    let round = matches!(target, Target::Artist(_));
    Some(Card { title, subtitle, thumbs: thumbs(&r["thumbnail"]["musicThumbnailRenderer"]), target, round, links: artist_links(detail) })
}

/// A song row (playlists, search, history, album tracks, Quick picks).
/// Music videos and other uploaded videos (not songs). Feeds, charts and radios drop them;
/// your own playlists, likes and history keep what you put there.
fn is_video(item: &Value) -> bool {
    // Only where the item itself plays: a playlist card can hold its first video's type too.
    let (two, row) = (&item["musicTwoRowItemRenderer"], &item["musicResponsiveListItemRenderer"]);
    let own = [&two["navigationEndpoint"], &row["overlay"], &row["flexColumns"][0], &item["navigationEndpoint"]];
    matches!(video_type(&own), Some("MUSIC_VIDEO_TYPE_OMV" | "MUSIC_VIDEO_TYPE_UGC" | "MUSIC_VIDEO_TYPE_OFFICIAL_SOURCE_MUSIC"))
}

/// The first `musicVideoType` under these parts of an item.
fn video_type<'a>(parts: &[&'a Value]) -> Option<&'a str> {
    fn find(v: &Value) -> Option<&str> {
        match v {
            Value::Object(m) => m.get("musicVideoType").and_then(Value::as_str).or_else(|| m.values().find_map(find)),
            Value::Array(a) => a.iter().find_map(find),
            _ => None,
        }
    }
    parts.iter().find_map(|p| find(p))
}

/// An official music video (the label's upload of a song with its video), not the song
/// itself, a user upload or a cover.
fn music_video(parts: &[&Value]) -> bool {
    video_type(parts) == Some("MUSIC_VIDEO_TYPE_OMV")
}

fn list_item(r: &Value) -> Option<Track> {
    let cols = arr(&r["flexColumns"]);
    let col = |i: usize| cols.get(i).map(|c| &c["musicResponsiveListItemFlexColumnRenderer"]["text"]);
    let title = text(col(0)?)?;
    let id = r["playlistItemData"]["videoId"]
        .as_str()
        .or_else(|| runs(col(0)?).find_map(|run| run["navigationEndpoint"]["watchEndpoint"]["videoId"].as_str()))?
        .to_owned();
    // The second column starts with the artists ("A & B", linked or not), then on
    // search rows " • Album • 3:22"; the rest hold plays, album and so on.
    let mut artists = Vec::new();
    let mut album = None;
    let mut duration = None;
    let mut plays = None;
    let mut in_artists = true;
    for c in 1..cols.len() {
        for (i, run) in runs(col(c)?).enumerate() {
            let name = run["text"].as_str().unwrap_or_default();
            let kind = page_type(run);
            if c == 1 && in_artists {
                match name.trim() {
                    "•" => in_artists = false,
                    "" | "," | "&" => {}
                    "Song" | "Video" | "Episode" if i == 0 => {}
                    n if kind.is_none_or(|k| k == "MUSIC_PAGE_TYPE_ARTIST" || k == "MUSIC_PAGE_TYPE_USER_CHANNEL") && !looks_like_duration(n) && !n.ends_with(" plays") => {
                        artists.push(Link { name: n.to_owned(), id: browse_id(run) })
                    }
                    _ => {}
                }
                if in_artists {
                    continue;
                }
            }
            match kind {
                Some("MUSIC_PAGE_TYPE_ARTIST" | "MUSIC_PAGE_TYPE_USER_CHANNEL") if !artists.iter().any(|a| a.name == name) => {
                    artists.push(Link { name: name.to_owned(), id: browse_id(run) })
                }
                Some("MUSIC_PAGE_TYPE_ALBUM") => album = Some(Link { name: name.to_owned(), id: browse_id(run) }),
                // Search rows put the length in the text ("Artist • Album • 3:22").
                _ if duration.is_none() && looks_like_duration(name) => duration = parse_duration(name),
                _ if name.ends_with(" plays") => plays = count(name),
                _ => {}
            }
        }
        in_artists = false;
    }
    // Playlist rows carry the length ("3:45") in a fixed column.
    if let Some(d) = r["fixedColumns"][0]["musicResponsiveListItemFixedColumnRenderer"]["text"]["runs"][0]["text"].as_str().and_then(parse_duration) {
        duration = Some(d);
    }
    let track_nr = text(&r["index"]).and_then(|n| n.parse().ok());
    let music_video = music_video(&[&r["overlay"], &r["flexColumns"][0]]);
    Some(Track { id, title, artists, album, duration, thumbs: thumbs(&r["thumbnail"]["musicThumbnailRenderer"]), track_nr, plays, music_video })
}

/// A song in a radio / up-next panel.
fn panel_item(r: &Value) -> Option<Track> {
    let id = r["videoId"].as_str()?.to_owned();
    let title = text(&r["title"])?;
    let byline = &r["longBylineText"];
    let mut artists = artist_links(byline);
    if artists.is_empty()
        && let Some(name) = runs(byline).next().and_then(|r| r["text"].as_str())
    {
        artists.push(Link { name: name.to_owned(), id: None });
    }
    let album = runs(byline)
        .find(|run| page_type(run) == Some("MUSIC_PAGE_TYPE_ALBUM"))
        .map(|run| Link { name: run["text"].as_str().unwrap_or_default().to_owned(), id: browse_id(run) });
    Some(Track {
        id,
        title,
        artists,
        album,
        duration: text(&r["lengthText"]).as_deref().and_then(parse_duration),
        thumbs: thumbs(r),
        track_nr: None,
        plays: None,
        music_video: music_video(&[&r["navigationEndpoint"]]),
    })
}

fn artist_links(v: &Value) -> Vec<Link> {
    runs(v)
        .filter(|run| matches!(page_type(run), Some("MUSIC_PAGE_TYPE_ARTIST" | "MUSIC_PAGE_TYPE_USER_CHANNEL")))
        .map(|run| Link { name: run["text"].as_str().unwrap_or_default().to_owned(), id: browse_id(run) })
        .collect()
}

/// Names in a header strapline, linked when they are.
fn links(v: &Value) -> Vec<Link> {
    runs(v)
        .filter_map(|run| {
            let name = run["text"].as_str()?.trim();
            (!name.is_empty() && !matches!(name, "•" | "," | "&")).then(|| Link { name: name.to_owned(), id: browse_id(run) })
        })
        .collect()
}

fn looks_like_duration(s: &str) -> bool {
    s.contains(':') && s.split(':').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

fn parse_duration(s: &str) -> Option<u32> {
    s.split(':').try_fold(0u32, |acc, p| Some(acc * 60 + p.trim().parse::<u32>().ok()?))
}

/// "3.6B plays", "40.1M", "12K subscribers" → the number.
fn count(s: &str) -> Option<u64> {
    let n = s.split_whitespace().next()?.replace(',', "");
    let (num, mult) = match n.chars().last()? {
        'K' => (&n[..n.len() - 1], 1e3),
        'M' => (&n[..n.len() - 1], 1e6),
        'B' => (&n[..n.len() - 1], 1e9),
        _ => (n.as_str(), 1.0),
    };
    Some((num.parse::<f64>().ok()? * mult).round() as u64)
}

fn arr(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or_default()
}

fn runs(v: &Value) -> impl Iterator<Item = &Value> {
    arr(&v["runs"]).iter()
}

fn text(v: &Value) -> Option<String> {
    let s: String = runs(v).filter_map(|r| r["text"].as_str()).collect();
    (!s.is_empty()).then_some(s)
}

fn thumbs(r: &Value) -> Vec<Thumb> {
    arr(&r["thumbnail"]["thumbnails"])
        .iter()
        .filter_map(|t| {
            Some(Thumb {
                url: t["url"].as_str()?.to_owned(),
                width: t["width"].as_u64().unwrap_or(0) as u32,
                height: t["height"].as_u64().unwrap_or(0) as u32,
            })
        })
        .collect()
}

fn page_type(v: &Value) -> Option<&str> {
    let nav = if v["navigationEndpoint"].is_null() { v } else { &v["navigationEndpoint"] };
    nav["browseEndpoint"]["browseEndpointContextSupportedConfigs"]["browseEndpointContextMusicConfig"]["pageType"].as_str()
}

fn browse_id(run: &Value) -> Option<String> {
    run["navigationEndpoint"]["browseEndpoint"]["browseId"].as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `accountItem` as the account switcher lists it.
    fn account_item(name: &str, handle: Option<&str>, selected: bool, page: Option<&str>, datasync: &str) -> Value {
        let mut tokens = vec![json!({"accountStateToken": {"hasChannel": true}}), json!({"datasyncIdToken": {"datasyncIdToken": datasync}})];
        if let Some(p) = page {
            tokens.push(json!({"pageIdToken": {"pageId": p}}));
        }
        let mut item = json!({
            "accountName": {"runs": [{"text": name}]},
            "isSelected": selected,
            "serviceEndpoint": {"selectActiveIdentityEndpoint": {"supportedTokens": tokens}},
        });
        if let Some(h) = handle {
            item["channelHandle"] = json!({"simpleText": h});
        }
        json!({"accountItem": item})
    }

    fn switcher(sections: Vec<Vec<Value>>) -> Value {
        let sections: Vec<Value> = sections
            .into_iter()
            .map(|items| json!({"accountSectionListRenderer": {"contents": [{"accountItemSectionRenderer": {"contents": items}}]}}))
            .collect();
        json!({"actions": [{"getMultiPageMenuAction": {"menu": {"multiPageMenuRenderer": {"sections": sections}}}}]})
    }

    #[test]
    fn channels_are_the_accounts_own_then_its_brand_channels() {
        let v = switcher(vec![
            vec![
                account_item("Ada", Some("@ada"), true, None, "111||"),
                account_item("Ada Records", Some("@adarecords"), false, Some("900"), "900||111"),
                account_item("Ada Live", None, false, Some("901"), "901||111"),
            ],
            // Another Google account signed in in the same browser, and its brand channel.
            vec![account_item("Bob", Some("@bob"), false, None, "222||"), account_item("Bob's Band", None, false, Some("902"), "902||222")],
        ]);
        let list = parse_channels(&v);
        let ids: Vec<_> = list.iter().map(|c| (c.name.as_str(), c.page_id.as_deref())).collect();
        assert_eq!(ids, [("Ada", None), ("Ada Records", Some("900")), ("Ada Live", Some("901"))]);
        assert_eq!(list[0].handle.as_deref(), Some("@ada"));
        assert!(list.iter().all(|c| c.has_channel));
    }

    /// The signed-in account's channels, as Settings lists them.
    /// `YTM_DATA_DIR=<profile> cargo test --release -- --ignored channels_live --nocapture`
    #[tokio::test]
    #[ignore]
    async fn channels_live() {
        let api = Api::new(&std::path::PathBuf::from(std::env::var("YTM_DATA_DIR").unwrap()));
        let current = api.account().expect("no session in the vault").channel().map(str::to_owned);
        for c in channels(&api).await.unwrap() {
            let mark = if c.page_id == current { "*" } else { " " };
            println!("{mark} {} {:?} channel {} page {:?}", c.name, c.handle, c.has_channel, c.page_id);
        }
    }

    #[test]
    fn a_google_account_without_a_channel_is_listed_as_such() {
        let mut own = account_item("Ada", None, true, None, "111||");
        own["accountItem"]["hasChannel"] = json!(false);
        own["accountItem"]["accountByline"] = json!({"runs": [{"text": "No channel"}]});
        let v = switcher(vec![vec![own, account_item("Ada", Some("@ada5564"), false, Some("900"), "900||111")]]);
        let list = parse_channels(&v);
        assert_eq!(list.iter().map(|c| (c.has_channel, c.handle.as_deref())).collect::<Vec<_>>(), [(false, None), (true, Some("@ada5564"))]);
    }

    #[test]
    fn a_switcher_without_the_accounts_own_channel_lists_nothing() {
        assert!(parse_channels(&json!({"actions": []})).is_empty());
        let only_brand = switcher(vec![vec![account_item("Ada Records", None, true, Some("900"), "900||111")]]);
        assert!(parse_channels(&only_brand).is_empty());
        // None selected: which account is this one's isn't guessed.
        let unselected = switcher(vec![vec![account_item("Ada", None, false, None, "111||"), account_item("Ada Records", None, false, Some("900"), "900||111")]]);
        assert!(parse_channels(&unselected).is_empty());
    }

    #[test]
    fn brand_channels_of_an_unknown_account_are_left_out_beside_other_accounts() {
        let pages = |v: &Value| parse_channels(v).into_iter().map(|c| c.page_id).collect::<Vec<_>>();
        // No datasync id on a brand channel: whose it is is unknown.
        let mut brand = account_item("Bob's Band", None, false, Some("902"), "");
        brand["accountItem"]["serviceEndpoint"]["selectActiveIdentityEndpoint"]["supportedTokens"] = json!([{"pageIdToken": {"pageId": "902"}}]);
        let two_accounts = switcher(vec![vec![account_item("Ada", None, true, None, "111||"), account_item("Bob", None, false, None, "222||"), brand.clone()]]);
        assert_eq!(pages(&two_accounts), [None]);
        // With only this account in the switcher, it can only be this one's.
        let one_account = switcher(vec![vec![account_item("Ada", None, true, None, "111||"), brand]]);
        assert_eq!(pages(&one_account), [None, Some("902".to_owned())]);
    }

    #[test]
    fn past_searches_come_with_their_remove_token() {
        // Trimmed from a signed-in get_search_suggestions answer (October 2026).
        let v = json!({"contents": [
            {"searchSuggestionsSectionRenderer": {"contents": [
                {"historySuggestionRenderer": {"suggestion": {"runs": [{"text": "lo", "bold": true}, {"text": "fi beats"}]},
                    "serviceEndpoint": {"feedbackEndpoint": {"feedbackToken": "tok"}},
                    "navigationEndpoint": {"searchEndpoint": {"query": "lofi beats"}}}},
                {"searchSuggestionRenderer": {"suggestion": {"runs": [{"text": "lo"}, {"text": "ve songs"}]}}}
            ]}},
            {"searchSuggestionsSectionRenderer": {"contents": [{"musicResponsiveListItemRenderer": {}}]}}
        ]});
        assert_eq!(
            parse_suggestions(&v),
            [
                Suggestion { text: "lofi beats".into(), forget: Some("tok".into()) },
                Suggestion { text: "love songs".into(), forget: None },
            ]
        );
    }

    #[test]
    fn revoked_session_reads_as_logged_out() {
        // Trimmed from Liked Music as answered to a revoked session (October 2026).
        let v = json!({
            "contents": {"singleColumnBrowseResultsRenderer": {"tabs": [{"tabRenderer": {"content": {"sectionListRenderer": {"contents": [
                {"itemSectionRenderer": {"contents": [{"messageRenderer": {"text": {"runs": [{"text": "Looking for what you’ve liked?"}]}}}]}}
            ]}}}}]}},
            "responseContext": {"serviceTrackingParams": [{"service": "GFEEDBACK", "params": [{"key": "logged_in", "value": "0"}]}]},
        });
        assert!(parse_playlist(&v, "LM").is_none());
        assert!(crate::ytmapi::logged_out(&v));
        assert!(!crate::ytmapi::logged_out(&json!({"responseContext": {"serviceTrackingParams": [{"params": [{"key": "logged_in", "value": "1"}]}]}})));
    }

    // Trimmed from real music.youtube.com responses (October 2026).
    const SEARCH_ROW: &str = r#"{"musicResponsiveListItemRenderer":{"thumbnail":{"musicThumbnailRenderer":{"thumbnail":{"thumbnails":[{"url":"https://yt3.googleusercontent.com/JzRduI","width":60,"height":60}]}}},"flexColumns":[{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"Blinding Lights","navigationEndpoint":{"watchEndpoint":{"videoId":"_l3v7ZyJZUI"}}}]}}},{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"Emily Dawn","navigationEndpoint":{"browseEndpoint":{"browseId":"UCpTBon3nXs5Y4OuFVr2q6Ug","browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_ARTIST"}}}}},{"text":", "},{"text":"Vive","navigationEndpoint":{"browseEndpoint":{"browseId":"UCZOyQvwe70uoAMFSoUyWJWw","browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_ARTIST"}}}}},{"text":" & "},{"text":"Sandy Beach","navigationEndpoint":{"browseEndpoint":{"browseId":"UCgWx_rcbegYfQuvrkwqKdEw","browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_ARTIST"}}}}},{"text":" • "},{"text":"Blinding Lights","navigationEndpoint":{"browseEndpoint":{"browseId":"MPREb_91LK9pVGLhy","browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_ALBUM"}}}}},{"text":" • "},{"text":"2:52"}]}}},{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"2.8K plays"}]}}}],"playlistItemData":{"videoId":"_l3v7ZyJZUI"}}}"#;
    const PLAYLIST_ROW: &str = r#"{"musicResponsiveListItemRenderer":{"thumbnail":{"musicThumbnailRenderer":{"thumbnail":{"thumbnails":[{"url":"https://yt3.googleusercontent.com/Xl4q7A","width":60,"height":60}]}}},"flexColumns":[{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"Universe...","navigationEndpoint":{"watchEndpoint":{"videoId":"F9apex1lPEw","playlistId":"RDCLAK5uy_mPolD_J22gS1SKxufARWcTZd1UrAH_0ZI"}}}]}}},{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"R3HAB","navigationEndpoint":{"browseEndpoint":{"browseId":"UCjpBcfMn2g8afa3XIZmS3mw","browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_ARTIST"}}}}},{"text":" & "},{"text":"Jason Stephenson"}]}}},{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"259K plays"}]}}},{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"Dream inside a dream…","navigationEndpoint":{"browseEndpoint":{"browseId":"MPREb_t6uDCi8Ljsr","browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_ALBUM"}}}}}]}}}],"fixedColumns":[{"musicResponsiveListItemFixedColumnRenderer":{"text":{"runs":[{"text":"1:52"}]}}}],"playlistItemData":{"videoId":"F9apex1lPEw"}}}"#;
    const ALBUM_ROW: &str = r#"{"musicResponsiveListItemRenderer":{"flexColumns":[{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"Alone Again","navigationEndpoint":{"watchEndpoint":{"videoId":"JH398xAYpZA","playlistId":"OLAK5uy_l4UqNJCpAF3kNaV37LHdRc_A07MmVdiSU"}}}]}}},{"musicResponsiveListItemFlexColumnRenderer":{"text":{}}},{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"54M plays"}]}}}],"fixedColumns":[{"musicResponsiveListItemFixedColumnRenderer":{"text":{"runs":[{"text":"4:11"}]}}}],"playlistItemData":{"videoId":"JH398xAYpZA"},"index":{"runs":[{"text":"1"}]}}}"#;
    const QUICK_PICK: &str = r#"{"musicResponsiveListItemRenderer":{"thumbnail":{"musicThumbnailRenderer":{"thumbnail":{"thumbnails":[{"url":"https://yt3.googleusercontent.com/r6AM19","width":60,"height":60}]}}},"flexColumns":[{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"地味な色 - Sober Color","navigationEndpoint":{"watchEndpoint":{"videoId":"ccOWvt_7Xig","playlistId":"RDAMVMccOWvt_7Xig"}}}]}}},{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"mei ehara","navigationEndpoint":{"browseEndpoint":{"browseId":"UC6E1po8d95GkLmZD2azY9Ig","browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_ARTIST"}}}}},{"text":" • "},{"text":"37K plays"}]}}},{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"Sway","navigationEndpoint":{"browseEndpoint":{"browseId":"MPREb_ZVsoTQ0vbS4","browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_ALBUM"}}}}}]}}}],"playlistItemData":{"videoId":"ccOWvt_7Xig"}}}"#;
    const PANEL_ITEM: &str = r#"{"playlistPanelVideoRenderer":{"title":{"runs":[{"text":"Resonance"}]},"longBylineText":{"runs":[{"text":"Home","navigationEndpoint":{"browseEndpoint":{"browseId":"UCfWnoT0YX8RfjFaXmg2Eq5g","browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_ARTIST"}}}}},{"text":" • "},{"text":"Odyssey","navigationEndpoint":{"browseEndpoint":{"browseId":"MPREb_83fMW9gQPxu","browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_ALBUM"}}}}},{"text":" • "},{"text":"2014"}]},"thumbnail":{"thumbnails":[{"url":"https://yt3.googleusercontent.com/S6e6T4","width":60,"height":60}]},"lengthText":{"runs":[{"text":"3:33"}]},"navigationEndpoint":{"watchEndpoint":{"videoId":"aBhrEv5Z7uc","playlistId":"RDAMVMJ7p4bzqLvCw","index":3}},"videoId":"aBhrEv5Z7uc"}}"#;
    const ARTIST_ROW: &str = r#"{"musicResponsiveListItemRenderer":{"thumbnail":{"musicThumbnailRenderer":{"thumbnail":{"thumbnails":[{"url":"https://lh3.googleusercontent.com/U-SAmN","width":60,"height":60}]}}},"flexColumns":[{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"The Weeknd"}]}}},{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"Artist"},{"text":" • "},{"text":"239M monthly audience"}]}}}],"navigationEndpoint":{"browseEndpoint":{"browseId":"UClYV6hHlupm_S_ObS1W-DYw","browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_ARTIST"}}}}}}"#;

    fn row(s: &str) -> Value {
        serde_json::from_str::<Value>(s).unwrap()["musicResponsiveListItemRenderer"].clone()
    }

    fn names(t: &Track) -> Vec<&str> {
        t.artists.iter().map(|a| a.name.as_str()).collect()
    }

    #[test]
    fn search_row_has_artists_album_length_and_plays() {
        let t = list_item(&row(SEARCH_ROW)).unwrap();
        assert_eq!(names(&t), ["Emily Dawn", "Vive", "Sandy Beach"]);
        assert!(t.artists.iter().all(|a| a.id.is_some()));
        assert_eq!(t.album.as_ref().map(|a| a.name.as_str()), Some("Blinding Lights"));
        assert_eq!(t.duration, Some(172));
        assert_eq!(t.plays, Some(2_800));
    }

    #[test]
    fn playlist_row_keeps_unlinked_artists() {
        let t = list_item(&row(PLAYLIST_ROW)).unwrap();
        assert_eq!(names(&t), ["R3HAB", "Jason Stephenson"]);
        assert_eq!(t.artists[0].id.as_deref(), Some("UCjpBcfMn2g8afa3XIZmS3mw"));
        assert_eq!(t.artists[1].id, None);
        assert_eq!(t.album.as_ref().and_then(|a| a.id.as_deref()), Some("MPREb_t6uDCi8Ljsr"));
        assert_eq!((t.duration, t.plays), (Some(112), Some(259_000)));
    }

    #[test]
    fn album_row_has_number_and_no_artists() {
        let t = list_item(&row(ALBUM_ROW)).unwrap();
        assert_eq!((t.title.as_str(), t.track_nr, t.duration), ("Alone Again", Some(1), Some(251)));
        assert!(t.artists.is_empty());
    }

    #[test]
    fn quick_pick_artist_is_not_its_play_count() {
        let t = list_item(&row(QUICK_PICK)).unwrap();
        assert_eq!(names(&t), ["mei ehara"]);
        assert_eq!(t.plays, Some(37_000));
        assert_eq!(t.album.as_ref().map(|a| a.name.as_str()), Some("Sway"));
    }

    #[test]
    fn radio_item() {
        let v: Value = serde_json::from_str(PANEL_ITEM).unwrap();
        let t = panel_item(&v["playlistPanelVideoRenderer"]).unwrap();
        assert!(!t.id.is_empty() && !t.title.is_empty());
        assert!(!t.artists.is_empty() && t.album.is_some() && t.duration.is_some());
    }

    #[test]
    fn artist_search_row_becomes_a_round_card() {
        let c = list_card(&row(ARTIST_ROW)).unwrap();
        assert_eq!(c.target, Target::Artist("UClYV6hHlupm_S_ObS1W-DYw".into()));
        assert_eq!((c.title.as_str(), c.subtitle.as_str(), c.round), ("The Weeknd", "239M monthly audience", true));
    }

    #[test]
    fn album_header() {
        let v = json!({"contents": {"twoColumnBrowseResultsRenderer": {
            "tabs": [{"tabRenderer": {"content": {"sectionListRenderer": {"contents": [{"musicResponsiveHeaderRenderer": {
                "title": {"runs": [{"text": "After Hours"}]},
                "subtitle": {"runs": [{"text": "Album"}, {"text": " • "}, {"text": "2020"}]},
                "straplineTextOne": {"runs": [{"text": "The Weeknd", "navigationEndpoint": {"browseEndpoint": {"browseId": "UCw"}}}]},
                "buttons": [{"musicPlayButtonRenderer": {"playNavigationEndpoint": {"watchPlaylistEndpoint": {"playlistId": "OLAK5uy_x"}}}}]
            }}]}}}}],
            "secondaryContents": {"sectionListRenderer": {"contents": [{"musicShelfRenderer": {"contents": [serde_json::from_str::<Value>(ALBUM_ROW).unwrap()]}}]}}
        }}});
        let (a, videos) = parse_album(&v, "MPREb_x").unwrap();
        assert!(videos.is_empty());
        assert_eq!(a.subtitle, "Album • The Weeknd • 2020");
        assert_eq!(a.save_id.as_deref(), Some("OLAK5uy_x"));
        let t = &a.tracks[0];
        assert_eq!(t.artists[0].name, "The Weeknd");
        assert_eq!(t.album.as_ref().and_then(|l| l.id.as_deref()), Some("MPREb_x"));
    }

    /// An album row as the web album page lists it (trimmed); `kind` is its musicVideoType.
    fn album_row(id: &str, title: &str, kind: &str) -> Value {
        let watch = json!({"watchEndpoint": {"videoId": id, "watchEndpointMusicSupportedConfigs": {"watchEndpointMusicConfig": {"musicVideoType": kind}}}});
        json!({"musicResponsiveListItemRenderer": {
            "flexColumns": [{"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": title, "navigationEndpoint": watch}]}}}],
            "playlistItemData": {"videoId": id},
        }})
    }

    /// An album page (trimmed) with these rows (id, title, musicVideoType).
    fn album_page(rows: &[(&str, &str, &str)]) -> Value {
        let rows: Vec<Value> = rows.iter().map(|(id, title, kind)| album_row(id, title, kind)).collect();
        json!({"contents": {"twoColumnBrowseResultsRenderer": {
            "tabs": [{"tabRenderer": {"content": {"sectionListRenderer": {"contents": [{"musicResponsiveHeaderRenderer": {
                "title": {"runs": [{"text": "Whenever You Need Somebody"}]},
                "buttons": [{"musicPlayButtonRenderer": {"playNavigationEndpoint": {"watchPlaylistEndpoint": {"playlistId": "OLAK5uy_x"}}}}]
            }}]}}}}],
            "secondaryContents": {"sectionListRenderer": {"contents": [{"musicShelfRenderer": {"contents": rows}}]}}
        }}})
    }

    /// The Android VR client's album queue (trimmed from a real answer, November 2026).
    fn vr_album(title: &str, songs: &[(&str, &str, &str)]) -> Value {
        let contents: Vec<Value> = songs
            .iter()
            .map(|(id, t, len)| json!({"playlistPanelVideoRenderer": {"videoId": id, "title": {"runs": [{"text": t}]}, "lengthText": {"runs": [{"text": len}]}}}))
            .collect();
        json!({"contents": {"singleColumnWatchNextResults": {"playlist": {"playlist": {"title": title, "contents": contents}}}}})
    }

    const RICK_ROWS: [(&str, &str, &str); 3] = [
        ("dQw4w9WgXcQ", "Never Gonna Give You Up", "MUSIC_VIDEO_TYPE_OMV"),
        ("BeyEGebJ1l4", "Whenever You Need Somebody", "MUSIC_VIDEO_TYPE_OMV"),
        ("d0t04zd918U", "The Love Has Gone", "MUSIC_VIDEO_TYPE_ATV"),
    ];
    const RICK_SONGS: [(&str, &str, &str); 3] = [("lYBUbBu4W08", "Never Gonna Give You Up", "3:34"), ("raBobo3GZYA", "Whenever You Need Somebody", "3:54"), ("d0t04zd918U", "The Love Has Gone", "4:21")];

    /// The album's track ids after swapping in `vr`'s songs, the way `album` does.
    fn swapped(rows: &[(&str, &str, &str)], vr: &Value) -> Vec<String> {
        let (mut a, videos) = parse_album(&album_page(rows), "MPREb_x").unwrap();
        if let Some(songs) = album_songs(vr).filter(|s| s.len() == a.tracks.len()) {
            swap_songs(&mut a, &videos, &songs);
        }
        a.tracks.into_iter().map(|t| t.id).collect()
    }

    #[test]
    fn album_music_videos_become_the_albums_songs() {
        let (mut a, videos) = parse_album(&album_page(&RICK_ROWS), "MPREb_x").unwrap();
        assert_eq!(videos, [0, 1]);
        let songs = album_songs(&vr_album("Album - Whenever You Need Somebody", &RICK_SONGS)).unwrap();
        assert_eq!(swap_songs(&mut a, &videos, &songs), 2);
        assert_eq!(a.tracks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["lYBUbBu4W08", "raBobo3GZYA", "d0t04zd918U"]);
        // The song's own length; the album's title.
        assert_eq!((a.tracks[1].title.as_str(), a.tracks[1].duration), ("Whenever You Need Somebody", Some(234)));
    }

    #[test]
    fn album_songs_change_nothing_unless_they_line_up() {
        let vr = vr_album("Album - Whenever You Need Somebody", &RICK_SONGS);
        let listed = ["dQw4w9WgXcQ", "BeyEGebJ1l4", "d0t04zd918U"];
        // A row the page leaves out (or one more song) would shift every pair after it.
        assert_eq!(swapped(&RICK_ROWS[..2], &vr), listed[..2]);
        // A title that doesn't match keeps its music video; the others still change.
        let mut renamed = RICK_ROWS;
        renamed[1].1 = "Whenever You Need Somebody (Radio Edit)";
        assert_eq!(swapped(&renamed, &vr), ["lYBUbBu4W08", "BeyEGebJ1l4", "d0t04zd918U"]);
        // A song row stays as listed even when the song list says otherwise.
        let mut other = RICK_SONGS;
        other[2].0 = "elsewhere";
        assert_eq!(swapped(&RICK_ROWS, &vr_album("Album - Whenever You Need Somebody", &other))[2], "d0t04zd918U");
        // The same id opens the artist's Top Songs, which is no album.
        let top = vr_album("Song - Rick Astley", &RICK_SONGS);
        assert_eq!(album_songs(&top), None);
        assert_eq!(swapped(&RICK_ROWS, &top), listed);
        // An entry that doesn't read makes the positions unreliable.
        let mut broken = vr.clone();
        broken["contents"]["singleColumnWatchNextResults"]["playlist"]["playlist"]["contents"][1] = json!({"automixPreviewVideoRenderer": {}});
        assert_eq!(album_songs(&broken), None);
    }

    #[test]
    fn titles_match_but_for_case_and_punctuation() {
        assert!(same_title("The Less I Know The Better", "The Less I Know the Better"));
        assert!(same_title("Don't Say Goodbye", "dont say goodbye"));
        assert!(same_title("Für Elise", "für  elise"));
        assert!(same_title("Levitating (feat. DaBaby)", "Levitating"));
        assert!(same_title("Stan [Featuring Dido]", "stan"));
        assert!(same_title("LOYALTY. (ft. Rihanna)", "LOYALTY."));
        assert!(!same_title("Levitating (Remix)", "Levitating"));
        assert!(!same_title("Mitwa", "Tere Mere Honthon Pe - Mitwa"));
        assert!(!same_title("", ""));
    }

    /// Asks the Android VR client for a real album (no sign-in). Run alone:
    /// `cargo test --bin ytm-rs -- --ignored ytm::tests::album_songs_live --exact`
    #[tokio::test]
    #[ignore]
    async fn album_songs_live() {
        let api = crate::ytmapi::Api::new(&std::env::temp_dir().join("ytm-rs-album-songs-test"));
        let a = album(&api, "MPREb_dcYZhAh5urI").await.unwrap();
        println!("{:?}", a.tracks.iter().map(|t| (&t.id, &t.title, t.duration)).collect::<Vec<_>>());
        assert_eq!(a.tracks[0].id, "lYBUbBu4W08", "Never Gonna Give You Up should be the song, not the music video");
    }

    #[test]
    fn playlist_header_owner_count_and_next_page() {
        let v = json!({"contents": {"twoColumnBrowseResultsRenderer": {
            "tabs": [{"tabRenderer": {"content": {"sectionListRenderer": {"contents": [{"musicResponsiveHeaderRenderer": {
                "title": {"runs": [{"text": "Official Videos"}]},
                "secondSubtitle": {"runs": [{"text": "22M views"}, {"text": " • "}, {"text": "1,046 tracks"}]},
                "facepile": {"avatarStackViewModel": {"text": {"content": "The Weeknd"}, "rendererContext": {"commandContext": {"onTap": {"innertubeCommand": {"browseEndpoint": {"browseId": "UC0"}}}}}}}
            }}]}}}}],
            "secondaryContents": {"sectionListRenderer": {"contents": [{"musicPlaylistShelfRenderer": {"contents": [
                serde_json::from_str::<Value>(PLAYLIST_ROW).unwrap(),
                {"continuationItemRenderer": {"continuationEndpoint": {"continuationCommand": {"token": "NEXT"}}}}
            ]}}]}}
        }}});
        let (p, token, count) = parse_playlist(&v, "PLx").unwrap();
        assert_eq!(count, Some(1046));
        assert_eq!(p.byline(), "The Weeknd • 1046 songs");
        assert_eq!(p.artists[0].id.as_deref(), Some("UC0"));
        assert_eq!(p.tracks.len(), 1);
        assert_eq!(token.as_deref(), Some("NEXT"));
    }

    #[test]
    fn library_grid_pages() {
        let card = |id: &str| json!({"musicTwoRowItemRenderer": {"title": {"runs": [{"text": id}]}, "navigationEndpoint": {"browseEndpoint": {"browseId": id,
            "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {"pageType": "MUSIC_PAGE_TYPE_PLAYLIST"}}}}}});
        let first = json!({"gridRenderer": {"items": [card("VLa")], "continuations": [{"nextContinuationData": {"continuation": "P2"}}]}});
        assert_eq!(grid_next(&first["gridRenderer"]).as_deref(), Some("P2"));

        let old = json!({"continuationContents": {"gridContinuation": {"items": [card("VLb"), card("VLc")], "continuations": [{"nextContinuationData": {"continuation": "P3"}}]}}});
        let (cards, next) = grid_continuation(&old);
        assert_eq!(cards.len(), 2);
        assert_eq!(next.as_deref(), Some("P3"));

        let new = json!({"onResponseReceivedActions": [{"appendContinuationItemsAction": {"continuationItems": [
            card("VLd"),
            {"continuationItemRenderer": {"continuationEndpoint": {"continuationCommand": {"token": "P4"}}}}
        ]}}]});
        let (cards, next) = grid_continuation(&new);
        assert_eq!(cards.len(), 1);
        assert_eq!(next.as_deref(), Some("P4"));

        let last = json!({"continuationContents": {"gridContinuation": {"items": [card("VLe")]}}});
        assert_eq!(grid_continuation(&last).1, None);
    }

    #[test]
    fn counts_and_lengths() {
        assert_eq!(count("3.6B plays"), Some(3_600_000_000));
        assert_eq!(count("40.1M"), Some(40_100_000));
        assert_eq!(count("1,234 views"), Some(1_234));
        assert_eq!(parse_duration("1:02:03"), Some(3723));
        assert!(!looks_like_duration("Blinding Lights"));
    }

    #[test]
    fn server_queue_picks_the_selected_song() {
        let v: Value = serde_json::from_str(r#"{"contents":{"singleColumnMusicWatchNextResultsRenderer":{"tabbedRenderer":{"watchNextTabbedResultsRenderer":{"tabs":[{"tabRenderer":{"content":{"musicQueueRenderer":{
            "content":{"playlistPanelRenderer":{"contents":[
                {"playlistPanelVideoRenderer":{"title":{"runs":[{"text":"Before"}]},"videoId":"aaaaaaaaaaa","lengthText":{"runs":[{"text":"3:00"}]}}},
                {"playlistPanelVideoRenderer":{"title":{"runs":[{"text":"Spring Lovers"}]},"videoId":"Q-IaNDsZXDE","selected":true,"lengthText":{"runs":[{"text":"2:50"}]}}},
                {"automixPreviewVideoRenderer":{"content":{}}}]}},
            "musicQueueConfig":{"musicQueueRestorationInfo":{"lastWatchTimestamp":{"nanos":546492000,"seconds":"1793133616"}}}}}}}]}}}}}"#).unwrap();
        let q = parse_server_queue(&v).unwrap();
        assert_eq!(q.tracks.len(), 2);
        assert_eq!(q.index, 1);
        assert_eq!(q.tracks[1].id, "Q-IaNDsZXDE");
        assert_eq!(q.tracks[1].duration, Some(170));
        assert_eq!(q.watched_at, 1_793_133_616);
        // Signed out, or no queue: nothing playing anywhere.
        assert!(parse_server_queue(&json!({})).is_none());
    }

    #[test]
    fn collaboration_panel() {
        // Trimmed from a real Collaborate panel (get_panel), collaboration off.
        let edit = |action: &str| json!({"innertubeCommand": {"playlistEditEndpoint": {"actions": [{"action": action}], "playlistId": "PLx"}}});
        let v = json!({"content": {"engagementPanelSectionListRenderer": {"content": {"playlistCollaborationViewModel": {
            "allowNewCollaboratorsPlaylistCollaborationSetting": {"switchListItemViewModel": {"switchOffCommand": edit("ACTION_REVOKE_COLLABORATION_TOKENS"), "switchOnCommand": edit("ACTION_CREATE_COLLABORATION_INVITE_LINK"), "switchedOnByDefault": false}},
            "collaboratePlaylistCollaborationSetting": {"switchListItemViewModel": {
                "switchOffCommand": {"parallelCommand": {"commands": [{"dismissDialogCommand": {"hack": true}}, {"innertubeCommand": {"playlistEditEndpoint": {"actions": [{"action": "ACTION_SET_CLOSED_TO_CONTRIBUTIONS", "closedToContributions": true}], "playlistId": "PLx"}}}]}},
                "switchOnCommand": edit("ACTION_CREATE_COLLABORATION_INVITE_LINK"), "switchedOnByDefault": false}},
            "copyLinkButton": {"buttonViewModel": {"onTap": {"innertubeCommand": {"copyLinkCommand": {"id": "PLx", "shortUrl": ""}}}}},
            "playlistCollaborationFormSchema": {"initialValues": {"isAllowNewCollaboratorsEnabled": false, "isCollaborationEnabled": false}}}}}}});
        let c = parse_collaboration("PLx", false, &v).unwrap();
        assert!(!c.on && !c.allow_new);
        assert_eq!(c.invite_link, None);
        assert_eq!(c.turn_off.unwrap()["actions"][0]["closedToContributions"], true);
        assert_eq!(c.revoke.unwrap()["actions"][0]["action"], "ACTION_REVOKE_COLLABORATION_TOKENS");
        assert!(c.turn_on.is_some() && c.allow.is_some());
    }
    fn song(title: &str, artists: &[&str], secs: u32) -> Track {
        Track {
            id: title.into(),
            title: title.into(),
            artists: artists.iter().map(|a| Link { name: a.to_string(), id: None }).collect(),
            album: None,
            duration: Some(secs),
            thumbs: vec![],
            track_nr: None,
            plays: None,
            music_video: false,
        }
    }

    fn video(title: &str, artists: &[&str], secs: u32) -> Track {
        Track { music_video: true, ..song(title, artists, secs) }
    }

    #[test]
    fn a_music_video_title_loses_its_video_markings() {
        assert_eq!(song_title("Ghostbusters (Official HD Video)"), "Ghostbusters");
        assert_eq!(song_title("Take On Me (Official Video) [4K]"), "Take On Me");
        assert_eq!(song_title("Bad Guy | Official Lyric Video"), "Bad Guy");
        assert_eq!(song_title("\"Jaan-E-Zigar Jaaneman\" Lyrical Video"), "\"Jaan-E-Zigar Jaaneman");
        assert_eq!(song_title("Hothon Se Chhu Lo Tum (From \"Prem Geet\")"), "Hothon Se Chhu Lo Tum (From \"Prem Geet\")");
        assert_eq!(song_title("Together Forever (2022 Remaster)"), "Together Forever (2022 Remaster)");
        // Nothing left once the markings are off: keep the title.
        assert_eq!(song_title("Official Video"), "Official Video");
        assert_eq!(song_title("Ça Plane Pour Moi (Clip Officiel)"), "Ça Plane Pour Moi");
    }

    #[test]
    fn the_song_of_a_music_video_is_found_by_a_sure_match_only() {
        // Pairs YouTube Music itself makes for Premium (November 2026).
        assert!(same_song(&video("Never Gonna Give You Up", &["Rick Astley"], 214), &song("Never Gonna Give You Up", &["Rick Astley"], 214)));
        assert!(same_song(&video("Kisi Nazar Ko Tera Intezar", &["Asha Bhosle"], 452), &song("Kisi Nazar Ko Tera Intezar", &["Asha Bhosle", "Bhupinder Singh"], 404)));
        assert!(same_song(&video("Saagar Jaisi Aankhonwali (From \"Saagar\")", &["Kishore Kumar"], 270), &song("Saagar Jaisi Aankhonwali (From \"Saagar\")", &["Kishore Kumar", "R. D. Burman"], 304)));
        assert!(same_song(&video("Ghostbusters (Official HD Video)", &["Ray Parker Jr."], 246), &song("Ghostbusters", &["Ray Parker Jr."], 240)));
        // A remaster of the same recording is still the song.
        assert!(same_song(&video("Together Forever", &["Rick Astley"], 204), &song("Together Forever (2022 Remaster)", &["Rick Astley"], 206)));
        // YouTube pairs "Mitwa" with "Tere Mere Honthon Pe - Mitwa"; a search can't be that sure.
        assert!(!same_song(&video("Mitwa", &["Lata Mangeshkar"], 289), &song("Tere Mere Honthon Pe - Mitwa", &["Lata Mangeshkar"], 334)));

        let take_on_me = video("Take On Me (Official Video)", &["a-ha"], 244);
        for (hit, why) in [
            (song("Take On Me (Live)", &["a-ha"], 240), "a live recording"),
            (song("Take On Me - Live from MTV Unplugged", &["a-ha"], 240), "an unplugged one"),
            (song("Take On Me (Remix)", &["a-ha"], 244), "a remix"),
            (song("Take On Me", &["Weezer"], 244), "a cover"),
            (song("Take On Me (Acoustic Version)", &["a-ha"], 244), "an acoustic version"),
            (song("Take On Me", &["a-ha"], 500), "a much longer recording"),
            (song("Take On Me", &["a-ha"], 190), "a much shorter one"),
            (Track { duration: None, ..song("Take On Me", &["a-ha"], 0) }, "an unknown length"),
            (video("Take On Me", &["a-ha"], 244), "another music video"),
            (song("Take", &["a-ha"], 244), "a shorter title"),
        ] {
            assert!(!same_song(&take_on_me, &hit), "{why} isn't the song");
        }
        assert!(!same_song(&video("Love Story", &["Taylor Swift"], 236), &song("Love Story (Taylor's Version)", &["Taylor Swift"], 235)), "a re-recording isn't the song");
        // An edit named past a "|" is another cut of it.
        assert!(!same_song(&video("Take On Me | Radio Edit", &["a-ha"], 225), &song("Take On Me", &["a-ha"], 226)), "a radio edit isn't the song");
        assert!(!same_song(&video("Take On Me", &[], 244), &song("Take On Me", &["a-ha"], 244)), "no artist to compare");
        // Cut down for the video: the song runs longer.
        assert!(same_song(&video("Funky Town", &["Lipps Inc."], 237), &song("Funky Town", &["Lipps Inc."], 471)));
        // A much longer song by other singers is another recording.
        assert!(!same_song(&video("Ae Mere Humsafar", &["Alka Yagnik", "Udit Narayan"], 314), &song("Ae Mere Humsafar", &["Vinod Rathod", "Alka Yagnik"], 454)));
        // "Artist - Title" videos.
        assert!(same_song(&video("Michael Sembello - Maniac (From \"Flashdance\") (Official Audio)", &["Michael Sembello"], 245), &song("Maniac", &["Michael Sembello"], 245)));
        assert!(!same_song(&video("Daft Punk - Maniac", &["Michael Sembello"], 245), &song("Maniac", &["Michael Sembello"], 245)), "a lead that isn't the artist stays");
        // Clean and explicit, or a remix in the video's own markings, are other recordings.
        assert!(!same_song(&video("Song | Acoustic Version", &["A"], 200), &song("Song", &["A"], 200)));
        assert!(!same_song(&video("Song (Official Acoustic Video)", &["A"], 200), &song("Song", &["A"], 200)));
        assert!(!same_song(&video("Song (Foo Remix)", &["A"], 200), &song("Song (Bar Remix)", &["A"], 200)));
        assert!(!same_song(&video("Bad Guy (Remix Official Video)", &["Billie Eilish"], 207), &song("Bad Guy", &["Billie Eilish"], 195)));
        assert!(!same_song(&video("WAP (Clean)", &["Cardi B"], 187), &song("WAP (Explicit)", &["Cardi B"], 187)));
        assert!(!same_song(&video("Running Up That Hill", &["Kate Bush"], 296), &song("Running Up That Hill (A Deal With God)", &["Kate Bush"], 299)));
        // A tribute band, or the same song with other singers.
        assert!(!same_song(&video("Bohemian Rhapsody", &["Queen"], 355), &song("Bohemian Rhapsody", &["Queen Tribute Band"], 355)));
        assert!(!same_song(&video("Duet", &["Singer A", "Singer B"], 200), &song("Duet", &["Singer A", "Singer C"], 200)));
        // The same artist page counts whatever the name says.
        let linked = |name: &str| Link { name: name.into(), id: Some("UCx".into()) };
        assert!(same_song(&Track { artists: vec![linked("Ray Parker Jr.")], ..video("Ghostbusters", &[], 246) }, &Track { artists: vec![linked("Ray Parker, Jr.")], ..song("Ghostbusters", &[], 240) }));
        // An unlinked credit comes as one name.
        assert!(same_song(&video("Kisi Nazar Ko Tera Intezar", &["Asha Bhosle"], 452), &song("Kisi Nazar Ko Tera Intezar", &["Asha Bhosle | Bhupinder"], 405)));
        assert!(!same_song(&video("Take On Me", &["a"], 244), &song("Take On Me", &["a-ha"], 244)), "only whole names count");
    }

    #[test]
    fn premium_queues_pair_a_music_video_with_its_song() {
        // Trimmed from a Premium account's `next` answer (November 2026).
        let item = |id: &str, title: &str, kind: &str| {
            json!({"playlistPanelVideoRenderer": {"videoId": id, "title": {"runs": [{"text": title}]}, "lengthText": {"runs": [{"text": "3:34"}]},
                "longBylineText": {"runs": [{"text": "Rick Astley"}]},
                "navigationEndpoint": {"watchEndpoint": {"videoId": id, "watchEndpointMusicSupportedConfigs": {"watchEndpointMusicConfig": {"musicVideoType": kind}}}}}})
        };
        let wrap = |primary: Value, counterpart: Value| {
            json!({"playlistPanelVideoWrapperRenderer": {"primaryRenderer": primary, "counterpart": [{"counterpartRenderer": counterpart,
                "segmentMap": {"segment": [{"primaryVideoStartTimeMilliseconds": "0", "counterpartVideoStartTimeMilliseconds": "927", "durationMilliseconds": "212500"}]}}]}})
        };
        let v = json!({"contents": {"singleColumnMusicWatchNextResultsRenderer": {"tabbedRenderer": {"watchNextTabbedResultsRenderer": {"tabs": [{"tabRenderer": {"content": {"musicQueueRenderer": {"content": {"playlistPanelRenderer": {"contents": [
            wrap(item("raBobo3GZYA", "Whenever You Need Somebody", "MUSIC_VIDEO_TYPE_ATV"), item("BeyEGebJ1l4", "Whenever You Need Somebody", "MUSIC_VIDEO_TYPE_OMV")),
            wrap(item("dQw4w9WgXcQ", "Never Gonna Give You Up", "MUSIC_VIDEO_TYPE_OMV"), item("lYBUbBu4W08", "Never Gonna Give You Up", "MUSIC_VIDEO_TYPE_ATV")),
        ]}}}}}}]}}}}});
        let found = parse_paired_song(&v, "dQw4w9WgXcQ").unwrap();
        assert_eq!((found.id.as_str(), found.duration, found.music_video), ("lYBUbBu4W08", Some(214), false));
        // A song's counterpart is its video: no song version of a song.
        assert_eq!(parse_paired_song(&v, "raBobo3GZYA"), None);
        // Not in the queue (or a free account's queue, which has no pairs).
        assert_eq!(parse_paired_song(&v, "BeyEGebJ1l4"), None);
    }

    #[test]
    fn a_radio_is_its_songs_or_its_videos_when_it_has_only_those() {
        let t = |id: &str| Track { id: id.into(), title: id.into(), artists: Vec::new(), album: None, duration: None, thumbs: Vec::new(), track_nr: None, plays: None, music_video: false };
        let ids = |items: &[(Track, bool)]| radio_tracks(items).map(|t| t.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&[(t("a"), false), (t("v"), true), (t("b"), false)]), ["a", "b"]);
        // A music video's radio: music videos only, kept rather than nothing.
        assert_eq!(ids(&[(t("v1"), true), (t("v2"), true)]), ["v1", "v2"]);
        assert!(ids(&[]).is_empty());
    }

    #[test]
    fn list_rows_say_whether_they_are_music_videos() {
        let row = |kind: &str| list_item(&album_row("x", "Song", kind)["musicResponsiveListItemRenderer"]).unwrap().music_video;
        assert!(row("MUSIC_VIDEO_TYPE_OMV"));
        assert!(!row("MUSIC_VIDEO_TYPE_ATV"));
        // User uploads and covers have no song version to look for.
        assert!(!row("MUSIC_VIDEO_TYPE_UGC"));
    }

    /// A signed-out search for the songs of a few music videos. Run alone:
    /// `cargo test --bin ytm-rs -- --ignored ytm::tests::searched_song_live --exact --nocapture`
    #[tokio::test]
    #[ignore]
    async fn searched_song_live() {
        let api = crate::ytmapi::Api::new(&std::env::temp_dir().join("ytm-rs-album-songs-test"));
        for v in [
            video("Never Gonna Give You Up", &["Rick Astley"], 214),
            video("Take On Me (Official Video)", &["a-ha"], 244),
            video("Kisi Nazar Ko Tera Intezar", &["Asha Bhosle"], 452),
            video("Bad Guy", &["Billie Eilish"], 207),
        ] {
            let found = searched_song(&api, &v).await.unwrap();
            println!("{} -> {:?}", v.title, found.as_ref().map(|s| (&s.id, &s.title, s.artist_line(), s.duration)));
        }
    }
}


