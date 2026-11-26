//! YouTube Music pages and actions, read from the InnerTube responses music.youtube.com
//! itself gets (see `ytmapi.rs` for the transport): home feed, search, albums,
//! playlists, artists, radio, charts, the library, and edits to it.

use std::collections::HashSet;

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

/// Whether podcasts are shown (Settings › Playback › Show podcasts). Off, the app is as
/// it was before it knew of them: no Podcasts chip on Home, and shows and episodes are
/// left out wherever YouTube Music lists them.
static PODCASTS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_podcasts(on: bool) {
    PODCASTS.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn podcasts() -> bool {
    PODCASTS.load(std::sync::atomic::Ordering::Relaxed)
}

/// A podcast show's page id is "MPSP" and its playlist's id. It's kept whole as the
/// playlist's id here, since the show's page is what lists its episodes; this is the
/// playlist id itself, for what YouTube takes that by (saving it, sharing it).
pub fn list_id(id: &str) -> &str {
    id.strip_prefix(SHOW_PREFIX).unwrap_or(id)
}

const SHOW_PREFIX: &str = "MPSP";

fn parse_home(v: &Value) -> HomePage {
    let sec = &v["contents"]["singleColumnBrowseResultsRenderer"]["tabs"][0]["tabRenderer"]["content"]["sectionListRenderer"];
    let chips = arr(&sec["header"]["chipCloudRenderer"]["chips"])
        .iter()
        .map(|c| &c["chipCloudChipRenderer"])
        .filter(|c| podcasts() || !podcasts_chip(c))
        .filter_map(|c| {
            let selected = c["isSelected"].as_bool().unwrap_or(false);
            // A selected chip's own endpoint re-selects it; deselecting returns to the default feed.
            let ep = if selected { &c["onDeselectedCommand"] } else { &c["navigationEndpoint"] };
            Some(Chip { title: text(&c["text"])?, params: ep["browseEndpoint"]["params"].as_str().map(str::to_owned), selected })
        })
        .collect();
    HomePage { chips, shelves: shelves(&sec["contents"]), continuation: continuation(sec) }
}

/// Home's Podcasts chip: by its title in English, and in any language by what it selects.
/// Its params are `ggNC` and then one 6-byte entry a chip, in base64 8 characters each:
/// `SgQIDBAD` is chip 12, Podcasts, selected.
fn podcasts_chip(c: &Value) -> bool {
    let params = c["navigationEndpoint"]["browseEndpoint"]["params"].as_str().unwrap_or_default();
    text(&c["text"]).as_deref() == Some("Podcasts") || params.strip_prefix("ggNC").is_some_and(|p| p.as_bytes().chunks(8).any(|e| e == b"SgQIDBAD"))
}

pub async fn home_more(api: &Api, token: &str, auth: bool) -> anyhow::Result<HomePage> {
    let v = api.call("browse", json!({ "continuation": token }), auth).await?;
    let sec = &v["continuationContents"]["sectionListContinuation"];
    Ok(HomePage { chips: vec![], shelves: shelves(&sec["contents"]), continuation: continuation(sec) })
}

// --------------------------------------------------------------- moods & genres

/// A group of Explore's Moods & genres ("Moods & moments", "Genres").
pub struct MoodGroup {
    pub title: String,
    pub moods: Vec<Mood>,
}

/// One of its categories: its browse params, and the colour of its stripe (ARGB).
pub struct Mood {
    pub title: String,
    pub params: String,
    pub color: u32,
}

/// A category's page: shelves of playlists.
pub struct MoodPage {
    pub params: String,
    pub title: String,
    pub shelves: Vec<Shelf>,
}

pub async fn moods(api: &Api, auth: bool) -> anyhow::Result<Vec<MoodGroup>> {
    Ok(parse_moods(&api.call("browse", json!({ "browseId": "FEmusic_moods_and_genres" }), auth).await?))
}

fn parse_moods(v: &Value) -> Vec<MoodGroup> {
    let sec = &v["contents"]["singleColumnBrowseResultsRenderer"]["tabs"][0]["tabRenderer"]["content"]["sectionListRenderer"];
    arr(&sec["contents"])
        .iter()
        .filter_map(|s| {
            let g = &s["gridRenderer"];
            let title = text(&g["header"]["gridHeaderRenderer"]["title"])?;
            let moods: Vec<Mood> = arr(&g["items"])
                .iter()
                .filter_map(|i| {
                    let b = &i["musicNavigationButtonRenderer"];
                    let params = b["clickCommand"]["browseEndpoint"]["params"].as_str()?.to_owned();
                    Some(Mood { title: text(&b["buttonText"])?, params, color: b["solid"]["leftStripeColor"].as_u64().unwrap_or(0) as u32 })
                })
                .collect();
            (!moods.is_empty()).then_some(MoodGroup { title, moods })
        })
        .collect()
}

pub async fn mood(api: &Api, params: &str, auth: bool) -> anyhow::Result<MoodPage> {
    let v = api.call("browse", json!({ "browseId": "FEmusic_moods_and_genres_category", "params": params }), auth).await?;
    Ok(parse_mood(&v, params))
}

fn parse_mood(v: &Value, params: &str) -> MoodPage {
    let sec = &v["contents"]["singleColumnBrowseResultsRenderer"]["tabs"][0]["tabRenderer"]["content"]["sectionListRenderer"];
    MoodPage { params: params.to_owned(), title: text(&v["header"]["musicHeaderRenderer"]["title"]).unwrap_or_default(), shelves: shelves(&sec["contents"]) }
}

// ----------------------------------------------------------------------- search

/// `search` filters, as the chips on music.youtube.com's results set them.
pub enum Filter {
    Songs,
    Albums,
    Artists,
    /// Playlists made by YouTube Music (not the community ones).
    Playlists,
    /// Playlists made by YouTube Music's listeners, which the site keeps on their own
    /// shelf.
    Community,
    Videos,
    /// Podcast shows.
    Podcasts,
    /// Podcast episodes.
    Episodes,
}

impl Filter {
    fn params(&self) -> &'static str {
        match self {
            Filter::Songs => "EgWKAQIIAWoQEAMQBBAJEAoQBRAREBAQFQ%3D%3D",
            Filter::Albums => "EgWKAQIYAWoQEAMQBBAJEAoQBRAREBAQFQ%3D%3D",
            Filter::Artists => "EgWKAQIgAWoQEAMQBBAJEAoQBRAREBAQFQ%3D%3D",
            Filter::Playlists => "EgeKAQQoADgBahIQAxAEEAkQDhAKEAUQERAQEBU%3D",
            Filter::Community => "EgeKAQQoAEABahIQAxAEEAkQDhAKEAUQERAQEBU%3D",
            Filter::Videos => "EgWKAQIQAWoQEAMQBBAJEAoQBRAREBAQFQ%3D%3D",
            Filter::Podcasts => "EgWKAQJQAWoQEAMQBBAJEAoQBRAREBAQFQ%3D%3D",
            Filter::Episodes => "EgWKAQJIAWoQEAMQBBAJEAoQBRAREBAQFQ%3D%3D",
        }
    }
}

async fn search_as(api: &Api, query: &str, filter: Filter, auth: bool) -> anyhow::Result<Vec<Value>> {
    Ok(search_rows(&api.call("search", json!({ "query": query, "params": filter.params() }), auth).await?))
}

fn search_rows(v: &Value) -> Vec<Value> {
    let sections = &v["contents"]["tabbedSearchResultsRenderer"]["tabs"][0]["tabRenderer"]["content"]["sectionListRenderer"]["contents"];
    arr(sections).iter().flat_map(|s| arr(&s["musicShelfRenderer"]["contents"]).iter().cloned()).collect()
}

/// `auth`: as the signed-in account, which adds the query to its search history.
pub async fn search_songs(api: &Api, query: &str, auth: bool) -> anyhow::Result<Vec<Track>> {
    Ok(search_as(api, query, Filter::Songs, auth).await?.iter().filter_map(|i| list_item(&i["musicResponsiveListItemRenderer"])).collect())
}

/// Like [`search_songs`], in English whatever Settings › Song info says: for matching
/// against titles and names that aren't translated (a video's, an imported playlist's), which "スターボーイ"
/// by "ザ・ウィークエンド" wouldn't.
pub async fn search_songs_in_english(api: &Api, query: &str, auth: bool) -> anyhow::Result<Vec<Track>> {
    let v = api.call_in_english("search", json!({ "query": query, "params": Filter::Songs.params() }), auth).await?;
    Ok(search_rows(&v).iter().filter_map(|i| list_item(&i["musicResponsiveListItemRenderer"])).collect())
}

/// Videos for a search: music videos and plain uploads, which song results leave out.
pub async fn search_videos(api: &Api, query: &str, auth: bool) -> anyhow::Result<Vec<Track>> {
    Ok(search_as(api, query, Filter::Videos, auth).await?.iter().filter_map(|i| list_item(&i["musicResponsiveListItemRenderer"])).collect())
}

/// A song's plays as YouTube Music counts them, which some lists (album pages) leave
/// out: from a song search for it, signed out so it stays out of the account's search
/// history. `None` when the song isn't among the results.
pub async fn song_plays(api: &Api, t: &Track) -> anyhow::Result<Option<u64>> {
    let query = format!("{} {}", t.artist_line(), t.title);
    Ok(search_songs(api, query.trim(), false).await?.into_iter().find(|s| s.id == t.id).and_then(|s| s.plays))
}

/// Song or video rows, searched as the signed-in account: signed-out searches miss
/// some major-label releases entirely (the Spotify transfer needs those).
pub async fn search_tracks_signed_in(api: &Api, query: &str, filter: Filter) -> anyhow::Result<Vec<Track>> {
    Ok(search_as(api, query, filter, true).await?.iter().filter_map(|i| list_item(&i["musicResponsiveListItemRenderer"])).collect())
}

/// Podcast episodes for a search.
pub async fn search_episodes(api: &Api, query: &str, auth: bool) -> anyhow::Result<Vec<Track>> {
    Ok(search_as(api, query, Filter::Episodes, auth).await?.iter().filter_map(|i| episode_row(&i["musicResponsiveListItemRenderer"])).collect())
}

pub async fn search_cards(api: &Api, query: &str, filter: Filter, auth: bool) -> anyhow::Result<Vec<Card>> {
    Ok(search_as(api, query, filter, auth).await?.iter().filter_map(|i| list_card(&i["musicResponsiveListItemRenderer"])).collect())
}

/// Where a search looks, as the tabs over music.youtube.com's results do: all of YouTube
/// Music, or only the signed-in account's library or its uploads.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Scope {
    #[default]
    Everywhere,
    Library,
    Uploads,
}

/// What a search of your library or your uploads found: rows that play (episodes apart,
/// with podcasts shown) and the pages it can open.
#[derive(Default)]
pub struct ScopedResults {
    pub tracks: Vec<Track>,
    pub episodes: Vec<Track>,
    pub cards: Vec<Card>,
}

/// Search only your library or only your uploads, as the account (so the query goes into
/// its search history, as on the website).
pub async fn search_in(api: &Api, query: &str, scope: Scope) -> anyhow::Result<ScopedResults> {
    let params = match scope {
        Scope::Everywhere => anyhow::bail!("not a search of your own music"),
        Scope::Library => "agIYBA%3D%3D",
        Scope::Uploads => "agIYAw%3D%3D",
    };
    let v = api.call_signed_in("search", json!({ "query": query, "params": params })).await?;
    Ok(parse_scoped(&v))
}

fn parse_scoped(v: &Value) -> ScopedResults {
    let mut out = ScopedResults::default();
    // Only the picked tab holds results; the others are links to their own search.
    let tabs = arr(&v["contents"]["tabbedSearchResultsRenderer"]["tabs"]);
    let Some(tab) = tabs.iter().map(|t| &t["tabRenderer"]).find(|t| t["selected"] == true) else { return out };
    let sections = arr(&tab["content"]["sectionListRenderer"]["contents"]);
    for r in sections.iter().flat_map(|s| arr(&s["musicShelfRenderer"]["contents"])).map(|i| &i["musicResponsiveListItemRenderer"]) {
        if !r["navigationEndpoint"].is_null() {
            out.cards.extend(list_card(r));
        } else if video_type(&[&r["overlay"], &r["flexColumns"][0]]) == Some("MUSIC_VIDEO_TYPE_PODCAST_EPISODE") {
            out.episodes.extend(episode_row(r).filter(|_| podcasts()));
        } else {
            out.tracks.extend(list_item(r));
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub text: String,
    /// A past search of the signed-in account: the token that removes it from the
    /// account's search history.
    pub forget: Option<String>,
}

/// Search terms for `input`, and the songs, videos, artists and albums it most likely
/// means. Signed in, the account's matching past searches come first, and an empty
/// `input` gives just those (its recent searches).
pub async fn suggestions(api: &Api, input: &str, auth: bool) -> anyhow::Result<(Vec<Suggestion>, Vec<Card>)> {
    let v = api.call("music/get_search_suggestions", json!({ "input": input }), auth).await?;
    Ok((parse_suggestions(&v), parse_suggested_hits(&v)))
}

/// The answer's second section: what the typed words most likely mean, as rows.
fn parse_suggested_hits(v: &Value) -> Vec<Card> {
    parse_suggested_hits_with(v, podcasts())
}

/// [`parse_suggested_hits`], with podcasts shown or not.
fn parse_suggested_hits_with(v: &Value, podcasts: bool) -> Vec<Card> {
    arr(&v["contents"][1]["searchSuggestionsSectionRenderer"]["contents"]).iter().filter_map(|i| suggested_hit(&i["musicResponsiveListItemRenderer"], podcasts)).collect()
}

/// An artist or album opens its page; a song, video or episode plays. Episodes only
/// with podcasts shown, by their show as search results have them.
fn suggested_hit(r: &Value, podcasts: bool) -> Option<Card> {
    if let Some(c) = list_card(r) {
        return Some(c);
    }
    let detail = &r["flexColumns"][1]["musicResponsiveListItemFlexColumnRenderer"]["text"];
    let subtitle = || runs(detail).filter_map(|r| r["text"].as_str()).collect::<String>();
    // An episode names its show where a song names its artists.
    if !show_links(detail).is_empty() {
        let t = episode_row(r).filter(|_| podcasts)?;
        return Some(Card { title: t.title.clone(), subtitle: subtitle(), thumbs: t.thumbs.clone(), round: false, links: t.artists.clone(), target: Target::Song(t) });
    }
    let mut t = list_item(r)?;
    // These rows lead with their kind ("Song • Daft Punk • 401M plays"), in the language
    // asked for: not an artist, whatever it's called.
    let mut lead = runs(detail);
    if let (Some(kind), Some(dot)) = (lead.next(), lead.next())
        && kind["navigationEndpoint"].is_null()
        && dot["text"].as_str().is_some_and(|s| s.trim() == "•")
    {
        let kind = kind["text"].as_str().unwrap_or_default();
        if t.artists.first().is_some_and(|a| a.id.is_none() && a.name == kind) {
            t.artists.remove(0);
        }
    }
    Some(Card { title: t.title.clone(), subtitle: subtitle(), thumbs: t.thumbs.clone(), round: false, links: t.artists.clone(), target: Target::Song(t) })
}

fn parse_suggestions(v: &Value) -> Vec<Suggestion> {
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
    if uploaded(id) {
        return upload_album(api, id).await;
    }
    let v = api.call("browse", json!({ "browseId": id }), false).await?;
    let (mut a, videos) = parse_album(&v, id).ok_or_else(|| anyhow::anyhow!("YouTube Music returned an empty album"))?;
    if !videos.is_empty() {
        // The song list it's matched against is in English whatever Settings › Song info
        // says ("Starboy" for "スターボーイ"): so are the titles it's matched by.
        let english = if api.locale().hl == crate::locale::ENGLISH.hl {
            None
        } else {
            let v = api.call_in_english("browse", json!({ "browseId": id }), false).await;
            v.ok().and_then(|v| parse_album(&v, id)).map(|(e, _)| e.tracks.into_iter().map(|t| t.title).collect::<Vec<_>>()).filter(|t| t.len() == a.tracks.len())
        };
        songs_for_videos(&mut a, &videos, english.as_deref()).await;
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
    counterparts(v, video_id).map(half).find(|r| video_type(&[&r["navigationEndpoint"]]) == Some("MUSIC_VIDEO_TYPE_ATV")).and_then(panel_item)
}

/// A song's music video, and where the song's start comes in it.
#[derive(Clone, Debug, PartialEq)]
pub struct MusicVideo {
    pub video: Track,
    /// The video's place at the song's start, in milliseconds: later for an intro before
    /// the song, earlier for a song that opens with what the video cut. 0 when not known.
    pub offset_ms: i64,
}

/// The music video of a song from YouTube's own pair, as [`paired_song`] the other way
/// round: Premium accounts only, `None` for others.
pub async fn paired_video(api: &Api, song_id: &str) -> anyhow::Result<Option<MusicVideo>> {
    let body = json!({"videoId": song_id, "isAudioOnly": true, "enablePersistentPlaylistPanel": true});
    let v = api.call_signed_in("next", body).await?;
    Ok(parse_paired_video(&v, song_id))
}

fn parse_paired_video(v: &Value, song_id: &str) -> Option<MusicVideo> {
    let c = counterparts(v, song_id).find(|c| music_video(&[&half(c)["navigationEndpoint"]]))?;
    let video = panel_item(half(c))?;
    // The pair lines the two up: this far into the song is that far into the video.
    let seg = &c["segmentMap"]["segment"][0];
    let ms = |key: &str| seg[key].as_str().and_then(|n| n.parse::<i64>().ok());
    let offset_ms = ms("counterpartVideoStartTimeMilliseconds").zip(ms("primaryVideoStartTimeMilliseconds")).map_or(0, |(video, song)| video - song);
    Some(MusicVideo { video, offset_ms })
}

/// The paired upload's ID if the pending unlike may touch it.
fn parse_other_half(v: &Value, video_id: &str, keep: &HashSet<String>) -> Option<String> {
    counterparts(v, video_id)
        .find_map(|c| half(c)["videoId"].as_str())
        .filter(|id| !keep.contains(*id))
        .map(str::to_owned)
}

/// The other version itself, in a pair from [`counterparts`].
fn half(counterpart: &Value) -> &Value {
    &counterpart["counterpartRenderer"]["playlistPanelVideoRenderer"]
}

/// What a `next` answer pairs with `video_id`: its other version, on Premium accounts,
/// with the `segmentMap` that lines the two up.
fn counterparts<'a>(v: &'a Value, video_id: &str) -> impl Iterator<Item = &'a Value> {
    let panel = &v["contents"]["singleColumnMusicWatchNextResultsRenderer"]["tabbedRenderer"]["watchNextTabbedResultsRenderer"]["tabs"][0]
        ["tabRenderer"]["content"]["musicQueueRenderer"]["content"]["playlistPanelRenderer"];
    let id = video_id.to_owned();
    arr(&panel["contents"])
        .iter()
        .map(|i| &i["playlistPanelVideoWrapperRenderer"])
        .filter(move |w| w["primaryRenderer"]["playlistPanelVideoRenderer"]["videoId"] == id.as_str())
        .flat_map(|w| arr(&w["counterpart"]))
}

/// The song version of a music video, by a signed-out search (a signed-in one would
/// land in the account's search history). Only a sure match counts: [`same_song`], and with
/// `clean` only one that isn't explicit (an album's clean edit is often among the hits).
pub async fn searched_song(api: &Api, video: &Track, clean: bool) -> anyhow::Result<Option<Track>> {
    let title = without_artist(&song_title(&video.title), video);
    let query = match video.artists.first() {
        Some(a) => format!("{title} {}", a.name),
        None => return Ok(None),
    };
    let hits = search_songs_in_english(api, &query, false).await?;
    Ok(hits.into_iter().take(5).find(|h| same_song(video, h) && !(clean && h.explicit)))
}

/// The song Shazam heard (its title, and its artists in one line), by a signed-out search in
/// English: Shazam's titles aren't translated. A sure match ([`heard_as`]) if there is one,
/// else the top hit (`Err`), if any.
pub async fn recognized_song(api: &Api, title: &str, artist: &str) -> anyhow::Result<Result<Track, Option<Track>>> {
    let mut hits = search_songs_in_english(api, &format!("{title} {artist}"), false).await?;
    Ok(match hits.iter().take(5).position(|h| heard_as(title, artist, h)) {
        Some(i) => Ok(hits.swap_remove(i)),
        None => Err(hits.into_iter().next()),
    })
}

/// Whether `hit` is the song Shazam calls `title` by `artist`: [`same_song`]'s checks on the
/// title, and the song's lead artist credited in Shazam's line ("Calvin Harris feat.
/// Rihanna"): a guest alone could be on another song of that name. Not its length check:
/// Shazam doesn't say how long the song is.
fn heard_as(title: &str, artist: &str, hit: &Track) -> bool {
    let base = base_title(title);
    let credit = Link { name: artist.to_owned(), id: None };
    !hit.music_video
        && !base.is_empty()
        && base == base_title(&hit.title)
        && variants(title) == variants(&hit.title)
        && qualifiers_agree(title, &hit.title)
        && hit.artists.first().is_some_and(|a| same_artist(a, &credit))
}

/// The music video of a song, by a signed-out videos search (signed in, it would land in
/// the account's search history). Only an official music video that's a sure match
/// counts: [`same_song`], the other way round.
pub async fn searched_video(api: &Api, song: &Track) -> anyhow::Result<Option<Track>> {
    let Some(artist) = song.artists.first().filter(|_| song.duration.is_some() && !song.has_video()) else { return Ok(None) };
    let hits = search_videos(api, &format!("{} {}", song.title, artist.name), false).await?;
    Ok(sure_video(song, hits))
}

/// Among the first few hits only: further down, a match is more likely another recording.
fn sure_video(song: &Track, hits: Vec<Track>) -> Option<Track> {
    hits.into_iter().take(5).find(|h| h.music_video && same_song(h, song))
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
        // A guest as other languages credit one too ("avec Justice", "con Bad Bunny").
        matches!(w.first(), Some(&("feat" | "ft" | "featuring" | "with" | "from" | "avec" | "con" | "mit" | "com")))
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
    let parts = [" | ", " & ", ", ", " x ", " feat. ", " ft. ", " and ", " with "].iter().fold(vec![lower.as_str()], |parts, sep| parts.iter().flat_map(|p| p.split(sep)).collect());
    // The whole credit too: "Simon & Garfunkel" is one artist.
    !name.is_empty() && std::iter::once(lower.as_str()).chain(parts).any(|p| words(p) == name)
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
/// `english`: the album's titles in English, row for row, when the page came in another language.
async fn songs_for_videos(a: &mut Collection, videos: &[usize], english: Option<&[String]>) {
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
    let swapped = swap_songs(a, videos, &songs, english);
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
/// per row), when the titles agree (`english`'s, when given). Returns how many changed.
fn swap_songs(a: &mut Collection, videos: &[usize], songs: &[AlbumSong], english: Option<&[String]>) -> usize {
    let mut swapped = 0;
    for &i in videos {
        let (Some(t), Some(s)) = (a.tracks.get_mut(i), songs.get(i)) else { continue };
        let title = english.and_then(|e| e.get(i)).unwrap_or(&t.title);
        if t.id != s.id && same_title(title, &s.title) {
            (t.id, t.duration) = (s.id.clone(), s.duration.or(t.duration));
            (t.music_video, t.video) = (false, false);
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
    let year = words.iter().rev().find(|w| is_year(w)).cloned();
    let artists = links(&h["straplineTextOne"]);
    let names = artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ");
    let subtitle = [Some(kind), (!names.is_empty()).then_some(names), year].into_iter().flatten().collect::<Vec<_>>().join(" • ");
    let thumbs = thumbs(&h["thumbnail"]["musicThumbnailRenderer"]);
    let link = Link { name: title.clone(), id: Some(id.to_owned()) };
    let rows = arr(&two["secondaryContents"]["sectionListRenderer"]["contents"]).iter().flat_map(|s| arr(&s["musicShelfRenderer"]["contents"]));
    let rows: Vec<(Track, bool)> = rows.filter_map(|i| Some((list_item(&i["musicResponsiveListItemRenderer"])?, is_video(i)))).collect();
    let videos = rows.iter().enumerate().filter(|(_, (_, video))| *video).map(|(i, _)| i).collect();
    let tracks = rows.into_iter().map(|(t, _)| album_song(t, &thumbs, &artists, &link)).collect();
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
        sorts: Vec::new(),
    };
    Some((a, videos))
}

/// An album's row, with what it leaves out because the header already says it.
fn album_song(mut t: Track, thumbs: &[Thumb], artists: &[Link], album: &Link) -> Track {
    if t.thumbs.is_empty() {
        t.thumbs = thumbs.to_vec();
    }
    if t.artists.is_empty() {
        t.artists = artists.to_vec();
    }
    t.album.get_or_insert_with(|| album.clone());
    t
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

/// The songs saved to the library, as Library › Songs lists them on the website: liked
/// ones, and the songs of albums and playlists saved there too. Loaded and kept like a
/// playlist, under this id.
pub const LIBRARY_SONGS: &str = "FEmusic_liked_videos";

/// A playlist's first page (about 100 songs), the next page's token, and how many
/// songs to stop at: `limit` (`None`: all), or fewer when a mix's header says so.
pub async fn playlist_start(api: &Api, id: &str, auth: bool, limit: Option<usize>) -> anyhow::Result<(Collection, Option<String>, usize)> {
    if id == LIBRARY_SONGS {
        let v = api.call_signed_in("browse", json!({ "browseId": LIBRARY_SONGS })).await?;
        let (c, token) = parse_library_songs(&v);
        return Ok((c, token, limit.unwrap_or(usize::MAX)));
    }
    // A podcast show's own page lists its episodes; a playlist's is "VL" and its id.
    let browse = if id.starts_with(SHOW_PREFIX) { id.to_owned() } else { format!("VL{id}") };
    let v = api.call("browse", json!({ "browseId": browse }), auth).await?;
    let Some((mut pl, token, count)) = parse_playlist(&v, id) else {
        // A session Google has revoked still gets answers, just signed-out ones.
        anyhow::ensure!(!(auth && api.account().is_some() && crate::ytmapi::logged_out(&v)), "Your YouTube sign-in has expired");
        anyhow::bail!("YouTube Music returned an empty playlist");
    };
    // Mixes (RD…) continue without end: stop at the length the header gives, or at 500.
    // Other lists end, and their header lags behind an edit: it would cut off a song just
    // added, or liked.
    let all = if id.starts_with("RD") { count.map_or(500, |c| c as usize) } else { usize::MAX };
    let limit = limit.map_or(all, |l| l.min(all));
    pl.tracks.truncate(limit);
    pl.set_ids.truncate(limit);
    Ok((pl, token, limit))
}

/// Library › Songs' first page (a "Shuffle all" row, then the songs) and the next page's
/// token; the later pages continue as a playlist's do. None saved yet: no songs.
fn parse_library_songs(v: &Value) -> (Collection, Option<String>) {
    let sections = arr(&v["contents"]["singleColumnBrowseResultsRenderer"]["tabs"][0]["tabRenderer"]["content"]["sectionListRenderer"]["contents"]);
    let shelf = sections.iter().map(|s| &s["musicShelfRenderer"]).find(|s| !s.is_null());
    let (tracks, set_ids, token) = shelf.map(|s| list_page(arr(&s["contents"]))).unwrap_or_default();
    let token = token.or_else(|| shelf.and_then(continuation));
    let c = Collection {
        id: LIBRARY_SONGS.to_owned(),
        is_album: false,
        save_id: None,
        title: String::new(),
        subtitle: String::new(),
        artists: Vec::new(),
        description: None,
        thumbs: Vec::new(),
        tracks,
        set_ids,
        count: None,
        owned: false,
        sorts: Vec::new(),
    };
    (c, token)
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
    let count = song_count(&h["secondSubtitle"]);
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
            sorts: sorts(shelf),
        },
        token,
        count,
    ))
}

/// Your own playlist's sort menu, with what each order sends to save it. Other lists'
/// menus (Liked Music's, others' playlists') can't be saved, so they're left out.
fn sorts(shelf: &Value) -> Vec<crate::backend::PlaylistSort> {
    let menu = &shelf["header"]["musicSideAlignedItemRenderer"]["startItems"][0]["sortFilterSubMenuRenderer"]["subMenuItems"];
    arr(menu)
        .iter()
        .filter_map(|item| {
            let actions = &item["serviceEndpoint"]["playlistEditEndpoint"]["actions"];
            Some(crate::backend::PlaylistSort {
                name: item["title"].as_str()?.to_owned(),
                on: item["selected"].as_bool() == Some(true),
                manual: actions[0]["playlistVideoOrder"] == 0,
                actions: actions.is_array().then(|| actions.clone())?,
            })
        })
        .collect()
}

/// Save one of a playlist's sort orders to it (see [`crate::backend::PlaylistSort`]).
pub async fn sort_playlist(api: &Api, id: &str, actions: Value) -> anyhow::Result<()> {
    let answer = write(api, "browse/edit_playlist", json!({"playlistId": id, "actions": actions})).await?;
    anyhow::ensure!(answer["status"].as_str().is_some_and(|s| s.contains("SUCCEEDED")), "YouTube Music didn't confirm the order");
    Ok(())
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
    let cont = &v["continuationContents"];
    // A podcast show's episodes continue as a plain shelf.
    let shelf = if cont["musicPlaylistShelfContinuation"].is_null() { &cont["musicShelfContinuation"] } else { &cont["musicPlaylistShelfContinuation"] };
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
            if r.is_null() {
                // A podcast show's page lists its episodes this way.
                return Some((episode(&i["musicMultiRowListItemRenderer"])?, None));
            }
            Some((list_item(r)?, r["playlistItemData"]["playlistSetVideoId"].as_str().map(str::to_owned)))
        })
        .unzip();
    (tracks, ids, item_token(items))
}

// ----------------------------------------------------------------------- artists

/// `auth`: ask as the account, so the page says whether it's subscribed.
pub async fn artist(api: &Api, id: &str, auth: bool) -> anyhow::Result<ArtistPage> {
    if uploaded(id) {
        return upload_artist(api, id).await;
    }
    let asked = std::time::Instant::now();
    let body = json!({ "browseId": id });
    // The page is anyone's to see: a session YouTube turns away gets it signed out, the
    // way a revoked one is answered anyway.
    let (v, fell_back) = match api.call("browse", body.clone(), auth).await {
        Err(e) if auth && api.account().is_some() && crate::ytmapi::session_refused(&e) => {
            log::warn!("artist {id} signed in: {e:#}; asking signed out");
            (api.call("browse", body, false).await?, true)
        }
        r => (r?, false),
    };
    let mut page = parse_artist(&v, id).ok_or_else(|| anyhow::anyhow!("YouTube Music returned an empty artist page"))?;
    page.asked = asked;
    if auth && api.account().is_some() && answered_signed_out(fell_back, &v) {
        unknown_subscription(&mut page);
    }
    Ok(page)
}

/// A signed-in ask answered as no one: by the fallback, or a session Google revoked.
fn answered_signed_out(fell_back: bool, v: &Value) -> bool {
    fell_back || crate::ytmapi::logged_out(v)
}

/// Whether the account subscribes isn't known: no pill, rather than a Subscribe that
/// would only fail.
fn unknown_subscription(page: &mut ArtistPage) {
    page.subscribed = false;
    page.channel_id = None;
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
        subscribed: sub["subscribed"].as_bool().unwrap_or(false),
        channel_id: sub["channelId"].as_str().map(str::to_owned),
        asked: std::time::Instant::now(),
        description: text(&h["description"]),
        top: Vec::new(),
        top_playlist: None,
        albums: Vec::new(),
        videos: Vec::new(),
        library: Vec::new(),
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
        let header = &carousel["header"]["musicCarouselShelfBasicHeaderRenderer"];
        // "From your library": its title leads to the artist's songs in your library ("MPLA"
        // and their channel), whatever language it's in.
        if header["title"]["runs"][0]["navigationEndpoint"]["browseEndpoint"]["browseId"].as_str().is_some_and(|b| b.starts_with("MPLA")) {
            page.library = arr(&carousel["contents"]).iter().filter_map(|i| two_row(&i["musicTwoRowItemRenderer"])).collect();
            continue;
        }
        let title = text(&header["title"]).unwrap_or_default();
        // Release cards list the year, singles their kind before it ("Single • 2026", in
        // the language asked for); spell out the kind and artist like elsewhere.
        let kind = if title.starts_with("Single") { "Single" } else { "Album" };
        for card in arr(&carousel["contents"]).iter().filter_map(|i| two_row(&i["musicTwoRowItemRenderer"])) {
            match card.target {
                Target::Album(_) => {
                    let (kind, rest) = card.subtitle.split_once(" • ").unwrap_or((kind, &card.subtitle));
                    let subtitle = [kind, me.name.as_str(), rest].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" • ");
                    let links = if card.links.is_empty() { vec![me.clone()] } else { card.links.clone() };
                    page.albums.push(Card { subtitle, links, ..card });
                }
                Target::Playlist(_) => page.playlists.push(card),
                Target::Artist(_) => page.similar.push(card),
                Target::Song(_) => page.videos.push(card),
            }
        }
    }
    Some(page)
}

// ------------------------------------------------------------- radio and explore

/// Songs to play after `video_id` (its radio), at least `limit` when there are enough.
/// A music video's radio is music videos only (a song's is songs): then those, rather
/// than nothing. Its song's radio is the one to ask for.
/// A song's first page of comments: its Comments tab (Up next, Lyrics, Comments, Related)
/// holds the token for it. `None`: the song has comments turned off (the tab is then just
/// a message) or has no such tab.
pub async fn comments(api: &Api, video_id: &str, auth: bool) -> anyhow::Result<Option<Value>> {
    let watch = api.call("next", json!({ "videoId": video_id }), auth).await?;
    let tabs = &watch["contents"]["singleColumnMusicWatchNextResultsRenderer"]["tabbedRenderer"]["watchNextTabbedResultsRenderer"]["tabs"];
    let mut tabs = tabs.as_array().into_iter().flatten();
    // By its title in English; in another language by its content: Up next holds the
    // queue, Lyrics and Related are pages of their own, Comments a list loaded later.
    let tab = tabs.clone().find(|t| t["tabRenderer"]["title"] == "Comments").or_else(|| tabs.find(|t| !t["tabRenderer"]["content"]["sectionListRenderer"].is_null()));
    let Some(token) = tab.and_then(|t| first_string(&t["tabRenderer"]["content"], "continuation")) else { return Ok(None) };
    api.call("next", json!({ "continuation": token }), auth).await.map(Some)
}

/// The lyrics YouTube Music shows on a song's Lyrics tab (licensed from Musixmatch or
/// LyricFind), as plain text: a signed-out client gets them without timing. `None`: the
/// song has no lyrics there. Signed out, so looking them up leaves no trace on the account.
pub async fn lyrics(api: &Api, video_id: &str) -> anyhow::Result<Option<String>> {
    let watch = api.call("next", json!({ "videoId": video_id }), false).await?;
    let tabs = &watch["contents"]["singleColumnMusicWatchNextResultsRenderer"]["tabbedRenderer"]["watchNextTabbedResultsRenderer"]["tabs"];
    // The Lyrics tab by its page (`MPLY…`), whatever language the tab's title is in.
    let Some(id) = arr(tabs).iter().filter_map(|t| t["tabRenderer"]["endpoint"]["browseEndpoint"]["browseId"].as_str()).find(|id| id.starts_with("MPLY")) else { return Ok(None) };
    let page = api.call("browse", json!({ "browseId": id }), false).await?;
    let text: String = arr(&page["contents"]["sectionListRenderer"]["contents"])
        .iter()
        .flat_map(|s| arr(&s["musicDescriptionShelfRenderer"]["description"]["runs"]))
        .filter_map(|r| r["text"].as_str())
        .collect();
    Ok(Some(text).filter(|t| !t.trim().is_empty()))
}

/// What to look a song's lyrics up by where only names match (LRCLib): its title without
/// what marks a music video, a leading "Artist - " or a closing "(feat. …)", and its first
/// artist.
pub fn lyrics_names(t: &Track) -> (String, String) {
    let title = without_artist(&song_title(&t.title), t);
    (without_guests(&title).to_owned(), t.artists.first().map(|a| a.name.clone()).unwrap_or_default())
}

/// How many songs a playlist header's second subtitle says it has: "[views •] 39 songs
/// [• duration]". In English by the word, in another language by where it stands.
fn song_count(subtitle: &Value) -> Option<u64> {
    let parts: Vec<&str> = runs(subtitle).filter_map(|r| r["text"].as_str()).filter(|t| t.trim() != "•").collect();
    let part = parts
        .iter()
        .find(|t| ["song", "track", "episode", "video"].iter().any(|w| t.contains(w)))
        .or(parts.get(if parts.len() == 3 { 1 } else { 0 }))?;
    // Some languages put words first ("عدد الأغاني: 125").
    leading_int(&part[part.find(|c: char| c.is_ascii_digit())?..])
}

/// The whole number `s` starts with, its digits grouped by commas, dots or spaces as a
/// language writes them ("1,046", "1.046", "1 046"); `None` when it doesn't start with one.
fn leading_int(s: &str) -> Option<u64> {
    let mut digits = String::new();
    let mut chars = s.trim_start().chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            digits.push(c);
        } else if digits.is_empty() || !matches!(c, ',' | '.' | ' ' | '\u{a0}' | '\u{202f}') || !chars.peek().is_some_and(char::is_ascii_digit) {
            break;
        }
    }
    digits.parse().ok()
}

/// A song or video by its id alone, as the queue lists it. `None`: there's no such thing.
pub async fn song(api: &Api, video_id: &str, auth: bool) -> anyhow::Result<Option<Track>> {
    let body = json!({"videoId": video_id, "isAudioOnly": true, "enablePersistentPlaylistPanel": true});
    let v = api.call("next", body, auth).await?;
    let panel = &v["contents"]["singleColumnMusicWatchNextResultsRenderer"]["tabbedRenderer"]["watchNextTabbedResultsRenderer"]["tabs"][0]
        ["tabRenderer"]["content"]["musicQueueRenderer"]["content"]["playlistPanelRenderer"];
    Ok(panel_page(panel).0.into_iter().map(|(t, _)| t).find(|t| t.id == video_id))
}

/// The page a music.youtube.com address stands for: an artist's `@handle`, an album's list.
pub async fn resolve(api: &Api, url: &str) -> anyhow::Result<Option<Target>> {
    let v = api.call("navigation/resolve_url", json!({ "url": url }), false).await?;
    Ok(v["endpoint"]["browseEndpoint"]["browseId"].as_str().and_then(crate::link::page))
}

/// The first string keyed `key` anywhere under `v`.
fn first_string(v: &Value, key: &str) -> Option<String> {
    match v {
        Value::Object(m) => m.get(key).and_then(Value::as_str).map(str::to_owned).or_else(|| m.values().find_map(|x| first_string(x, key))),
        Value::Array(a) => a.iter().find_map(|x| first_string(x, key)),
        _ => None,
    }
}

pub async fn radio(api: &Api, video_id: &str, auth: bool, limit: usize) -> anyhow::Result<Vec<Track>> {    let body = json!({
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

/// Charts for the country in Settings › Song info, or the global ones where YouTube Music
/// has none for it.
pub async fn charts(api: &Api) -> anyhow::Result<Charts> {
    let body = json!({ "browseId": "FEmusic_charts", "params": "sgYPRkVtdXNpY19leHBsb3Jl", "formData": { "selectedValues": [api.locale().gl] } });
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

/// The podcast shows saved to the library, with its own lists (New episodes, Episodes
/// for later) first, as YouTube Music has them.
pub async fn saved_podcasts(api: &Api) -> anyhow::Result<Vec<Card>> {
    let v = api.call_signed_in("browse", json!({ "browseId": "FEmusic_library_non_music_audio_list" })).await?;
    let sections = arr(&v["contents"]["singleColumnBrowseResultsRenderer"]["tabs"][0]["tabRenderer"]["content"]["sectionListRenderer"]["contents"]);
    let mut cards = grid_cards(sections);
    let mut token = sections.iter().find_map(|s| grid_next(&s["gridRenderer"]));
    for _ in 0..100 {
        let Some(t) = token.take() else { break };
        let Ok(v) = api.call_signed_in("browse", json!({ "continuation": t })).await else { break };
        let (more, next) = grid_continuation(&v);
        if more.is_empty() {
            break;
        }
        cards.extend(more);
        token = next;
    }
    Ok(cards)
}

pub async fn saved_albums(api: &Api) -> anyhow::Result<Vec<Card>> {
    saved(api, "4qmFsgIoEhRGRW11c2ljX2xpa2VkX2FsYnVtcxoQZ2dNR0tnUUlBQkFCb0FZQg%3D%3D").await
}

/// The artists the account subscribes to, as Library › Artists lists them.
pub async fn subscribed_artists(api: &Api) -> anyhow::Result<Vec<Card>> {
    let v = api.call_signed_in("browse", json!({ "browseId": "FEmusic_library_corpus_artists" })).await?;
    let sections = arr(&v["contents"]["singleColumnBrowseResultsRenderer"]["tabs"][0]["tabRenderer"]["content"]["sectionListRenderer"]["contents"]);
    // None subscribed: a message stands where the list would.
    let Some(shelf) = sections.iter().map(|s| &s["musicShelfRenderer"]).find(|s| !s.is_null()) else { return Ok(Vec::new()) };
    let mut cards = list_cards(arr(&shelf["contents"]));
    let mut token = continuation(shelf).or_else(|| item_token(arr(&shelf["contents"])));
    for _ in 0..100 {
        let Some(t) = token.take() else { break };
        // A later page failing keeps the pages already loaded.
        let v = match api.call_signed_in("browse", json!({ "continuation": t })).await {
            Ok(v) => v,
            Err(e) => {
                log::warn!("subscribed artists page: {e}");
                break;
            }
        };
        let (more, next) = shelf_continuation(&v);
        if more.is_empty() {
            break;
        }
        cards.extend(more);
        token = next;
    }
    Ok(cards)
}

/// A following page of a list shelf: its rows, and the token for the page after.
fn shelf_continuation(v: &Value) -> (Vec<Card>, Option<String>) {
    let appended = arr(&v["onResponseReceivedActions"][0]["appendContinuationItemsAction"]["continuationItems"]);
    if !appended.is_empty() {
        return (list_cards(appended), item_token(appended));
    }
    let more = &v["continuationContents"]["musicShelfContinuation"];
    (list_cards(arr(&more["contents"])), continuation(more))
}

fn list_cards(items: &[Value]) -> Vec<Card> {
    items.iter().filter_map(|i| list_card(&i["musicResponsiveListItemRenderer"])).collect()
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
    let v = api.call_signed_in("browse", json!({ "browseId": format!("{UPLOADS}tracks") })).await?;
    let Some(shelf) = upload_shelf(&v) else { return Ok(Vec::new()) };
    let mut tracks = upload_rows(shelf);
    tracks.extend(more_uploads(api, continuation(shelf)).await?);
    Ok(tracks)
}

/// One of your uploaded albums (opened from its songs, or a search of your uploads). Only
/// the account sees it, so it's asked for as the account.
async fn upload_album(api: &Api, id: &str) -> anyhow::Result<Collection> {
    let v = api.call_signed_in("browse", json!({ "browseId": id })).await?;
    let (mut a, token) = parse_upload_album(&v, id).ok_or_else(|| anyhow::anyhow!("YouTube Music returned an empty album"))?;
    let link = Link { name: a.title.clone(), id: Some(id.to_owned()) };
    for t in more_uploads(api, token).await? {
        let t = album_song(t, &a.thumbs, &a.artists, &link);
        a.tracks.push(t);
    }
    Ok(a)
}

/// One of your uploads' artists: the page lists your songs by them, as on the website.
async fn upload_artist(api: &Api, id: &str) -> anyhow::Result<ArtistPage> {
    let v = api.call_signed_in("browse", json!({ "browseId": id })).await?;
    let (mut page, token) = parse_upload_artist(&v, id).ok_or_else(|| anyhow::anyhow!("YouTube Music returned an empty artist page"))?;
    page.top.extend(more_uploads(api, token).await?);
    Ok(page)
}

/// The songs after `token` on a page of your uploads, up to 20 pages more.
async fn more_uploads(api: &Api, mut token: Option<String>) -> anyhow::Result<Vec<Track>> {
    let mut tracks = Vec::new();
    for _ in 0..20 {
        let Some(t) = token.take() else { break };
        let v = api.call_signed_in("browse", json!({ "continuation": t })).await?;
        let more = &v["continuationContents"]["musicShelfContinuation"];
        tracks.extend(upload_rows(more));
        token = continuation(more);
    }
    Ok(tracks)
}

/// The songs shelf of a page of your uploads: Library › Uploads, an uploaded artist's or
/// album's. In one column (as ytmusicapi reads them), or beside the header as other albums now are.
fn upload_shelf(v: &Value) -> Option<&Value> {
    let single = arr(&v["contents"]["singleColumnBrowseResultsRenderer"]["tabs"]).iter().flat_map(|t| arr(&t["tabRenderer"]["content"]["sectionListRenderer"]["contents"]));
    let beside = arr(&v["contents"]["twoColumnBrowseResultsRenderer"]["secondaryContents"]["sectionListRenderer"]["contents"]);
    single.chain(beside).map(|c| &c["musicShelfRenderer"]).find(|s| !s.is_null())
}

fn upload_rows(shelf: &Value) -> Vec<Track> {
    arr(&shelf["contents"]).iter().filter_map(|i| upload_row(&i["musicResponsiveListItemRenderer"])).collect()
}

/// An uploaded song's row. Some may name the song only in their menu (Play next, Add to
/// queue), which is where ytmusicapi reads it from.
fn upload_row(r: &Value) -> Option<Track> {
    let t = list_item(r).or_else(|| {
        let id = arr(&r["menu"]["menuRenderer"]["items"]).iter().find_map(|i| {
            let queued = &i["menuServiceItemRenderer"]["serviceEndpoint"]["queueAddEndpoint"]["queueTarget"]["videoId"];
            queued.as_str().or_else(|| i["menuNavigationItemRenderer"]["navigationEndpoint"]["watchEndpoint"]["videoId"].as_str())
        })?;
        let mut r = r.clone();
        r["playlistItemData"] = json!({ "videoId": id });
        list_item(&r)
    })?;
    // The menu's Delete asks first, then deletes it by this.
    let entity = arr(&r["menu"]["menuRenderer"]["items"]).iter().find_map(|i| {
        let confirm = &i["menuNavigationItemRenderer"]["navigationEndpoint"]["confirmDialogEndpoint"]["content"]["confirmDialogRenderer"];
        confirm["confirmButton"]["buttonRenderer"]["command"]["musicDeletePrivatelyOwnedEntityCommand"]["entityId"].as_str()
    });
    if let Some(e) = entity {
        UPLOAD_ENTITIES.lock().unwrap().insert(t.id.clone(), e.to_owned());
    }
    Some(t)
}

/// What deletes each uploaded song from the account (video ID → its entity), as the
/// uploads last listed them.
static UPLOAD_ENTITIES: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, String>>> = std::sync::LazyLock::new(Default::default);

/// What deletes `video_id` from the account's uploads, if it's one of them.
pub fn upload_entity(video_id: &str) -> Option<String> {
    UPLOAD_ENTITIES.lock().unwrap().get(video_id).cloned()
}

/// "2020", or with the mark some languages write after it ("2020年", "2020년").
fn is_year(s: &str) -> bool {
    s.get(..4).is_some_and(|y| y.bytes().all(|b| b.is_ascii_digit())) && matches!(&s[4..], "" | "年" | "년")
}

/// An uploaded album and the token for more of its songs. Its header is the one albums had
/// before 2024 ("Album • Artist • 2020" under the title), or the one they have now.
fn parse_upload_album(v: &Value, id: &str) -> Option<(Collection, Option<String>)> {
    let h = std::iter::once(&v["header"]["musicDetailHeaderRenderer"]).chain(header(&v["contents"]["twoColumnBrowseResultsRenderer"])).find(|h| !h.is_null())?;
    let title = text(&h["title"])?;
    let mut parts = links(&h["subtitle"]);
    let kind = match parts.first() {
        Some(p) if p.id.is_none() && !is_year(&p.name) => parts.remove(0).name,
        _ => "Album".to_owned(),
    };
    let year = parts.iter().position(|p| is_year(&p.name)).map(|i| parts.remove(i).name);
    let strapline = links(&h["straplineTextOne"]);
    let artists = if strapline.is_empty() { parts } else { strapline };
    let names = artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ");
    let subtitle = [Some(kind), (!names.is_empty()).then_some(names), year].into_iter().flatten().collect::<Vec<_>>().join(" • ");
    let thumb = &h["thumbnail"];
    let thumbs = thumbs(if thumb["croppedSquareThumbnailRenderer"].is_null() { &thumb["musicThumbnailRenderer"] } else { &thumb["croppedSquareThumbnailRenderer"] });
    let link = Link { name: title.clone(), id: Some(id.to_owned()) };
    let shelf = upload_shelf(v);
    let tracks = shelf.map(upload_rows).unwrap_or_default().into_iter().map(|t| album_song(t, &thumbs, &artists, &link)).collect();
    let a = Collection {
        id: id.to_owned(),
        is_album: true,
        // Not one to save to the library: it's in your uploads.
        save_id: None,
        title,
        subtitle,
        artists,
        description: text(&h["description"]).or_else(|| text(&h["description"]["musicDescriptionShelfRenderer"]["description"])),
        thumbs,
        tracks,
        set_ids: Vec::new(),
        count: None,
        owned: false,
        sorts: Vec::new(),
    };
    Some((a, shelf.and_then(continuation)))
}

/// An uploaded artist's page and the token for more of its songs: their name, and your
/// songs by them.
fn parse_upload_artist(v: &Value, id: &str) -> Option<(ArtistPage, Option<String>)> {
    let shelf = upload_shelf(v);
    let top = shelf.map(upload_rows).unwrap_or_default();
    // The header's title, or the name the songs give the artist.
    let name = v["header"]
        .as_object()
        .and_then(|h| h.values().find_map(|h| text(&h["title"])))
        .or_else(|| top.iter().flat_map(|t| &t.artists).find(|a| a.id.as_deref() == Some(id)).map(|a| a.name.clone()))
        .or_else(|| top.first()?.artists.first().map(|a| a.name.clone()))?;
    let page = ArtistPage {
        id: id.to_owned(),
        name,
        banner: Vec::new(),
        subscribers: None,
        subscribed: false,
        channel_id: None,
        asked: std::time::Instant::now(),
        description: None,
        top,
        top_playlist: None,
        albums: Vec::new(),
        videos: Vec::new(),
        library: Vec::new(),
        playlists: Vec::new(),
        similar: Vec::new(),
    };
    Some((page, shelf.and_then(continuation)))
}

// -------------------------------------------------------------------------- edits

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SongRating {
    Like,
    Dislike,
    Indifferent,
}

impl SongRating {
    fn from_status(status: &Value) -> Option<Self> {
        match status.as_str()? {
            "DISLIKE" => Some(Self::Dislike),
            "LIKE" => Some(Self::Like),
            "INDIFFERENT" => Some(Self::Indifferent),
            _ => None,
        }
    }
}

/// The signed-in account's current rating of the requested upload, if supplied.
pub async fn song_rating(api: &Api, video_id: &str) -> anyhow::Result<Option<SongRating>> {
    let body = json!({"videoId": video_id, "isAudioOnly": true});
    let v = api.call_signed_in("next", body).await?;
    Ok(parse_song_rating(&v, video_id))
}

fn parse_song_rating(v: &Value, video_id: &str) -> Option<SongRating> {
    let overlay = &v["playerOverlays"]["playerOverlayRenderer"];
    // The segmented player buttons use the current upload's rating entity.
    if v["currentVideoEndpoint"]["watchEndpoint"]["videoId"].as_str() == Some(video_id)
        && let Some(rating) = arr(&overlay["videoActionBar"]["videoActionBarViewModel"]["buttons"]).iter().find_map(|button| {
            let button = button.get("buttonViewModel").unwrap_or(button);
            let like = &button["segmentedLikeDislikeButtonViewModel"]["likeButtonViewModel"];
            let like = like.get("likeButtonViewModel").unwrap_or(like);
            let entity = &like["likeStatusEntity"];
            let updated = like["likeStatusEntityKey"].as_str().or_else(|| entity["key"].as_str()).and_then(|key| {
                arr(&v["frameworkUpdates"]["entityBatchUpdate"]["mutations"]).iter().rev().find_map(|mutation| {
                    let rating = &mutation["payload"]["likeStatusEntity"];
                    (rating["key"].as_str() == Some(key)).then(|| SongRating::from_status(&rating["likeStatus"])).flatten()
                })
            });
            updated.or_else(|| SongRating::from_status(&entity["likeStatus"]))
        })
    {
        return Some(rating);
    }

    // Older player overlays carry the target and rating directly.
    arr(&overlay["actions"]).iter().find_map(|a| {
        let button = &a["likeButtonRenderer"];
        (button["target"]["videoId"].as_str() == Some(video_id)).then(|| SongRating::from_status(&button["likeStatus"])).flatten()
    })
}

/// Like or un-like a song for the signed-in account.
pub async fn set_like(api: &Api, video_id: &str, like: bool) -> anyhow::Result<()> {
    let endpoint = if like { "like/like" } else { "like/removelike" };
    write(api, endpoint, json!({ "target": { "videoId": video_id } })).await?;
    Ok(())
}

/// After taking a song's like away, its other half's. A music video and its song are
/// liked separately, but with Premium, Liked Music lists a liked music video as its song,
/// so the song shown may not be the one holding the like. `epoch`: the account and channel
/// when the like was taken away; after a change the other half isn't theirs to touch.
/// Returns the other half when its like was taken away too.
pub async fn remove_other_half_like(api: &Api, video_id: &str, epoch: u64, keep: &HashSet<String>) -> anyhow::Result<Option<String>> {
    if api.epoch() != epoch {
        return Ok(None);
    }
    let body = json!({"videoId": video_id, "isAudioOnly": true, "enablePersistentPlaylistPanel": true});
    let v = api.call_signed_in("next", body).await?;
    let Some(other) = parse_other_half(&v, video_id, keep) else { return Ok(None) };
    tokio::time::sleep(crate::backend::LIKE_GAP).await;
    if api.epoch() != epoch {
        return Ok(None);
    }
    set_like(api, &other, false).await?;
    Ok(Some(other))
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
///
/// `sts` is the current player's signatureTimestamp. Without it YouTube answers
/// UNPLAYABLE ("The page needs to be reloaded") and leaves the stats URL out.
pub async fn report_play(api: &Api, video_id: &str, sts: Option<u32>) -> anyhow::Result<Watchtime> {
    let player = api.call_signed_in("player", report_play_body(video_id, sts)).await?;
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

fn report_play_body(video_id: &str, sts: Option<u32>) -> Value {
    let mut body = json!({"videoId": video_id, "contentCheckOk": true, "racyCheckOk": true});
    if let Some(sts) = sts {
        body["playbackContext"] = json!({"contentPlaybackContext": {"signatureTimestamp": sts}});
    }
    body
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

/// What an add does with songs the playlist has already.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Duplicates {
    /// Add nothing if any are there, and say so.
    Ask,
    /// Add only the songs it doesn't have.
    Skip,
    /// Add them all, a second time for some.
    Add,
}

/// Add songs to a playlist in one request: `Ok(false)` when [`Duplicates::Ask`] found
/// some there already, and none went in.
pub async fn add_videos(api: &Api, playlist_id: &str, video_ids: &[String], duplicates: Duplicates) -> anyhow::Result<bool> {
    // DEDUPE_OPTION_SKIP skips the check, not the songs: it's the one that adds twice.
    let option = match duplicates {
        Duplicates::Ask => "DEDUPE_OPTION_CHECK",
        Duplicates::Skip => "DEDUPE_OPTION_DROP_DUPLICATE",
        Duplicates::Add => "DEDUPE_OPTION_SKIP",
    };
    let actions: Vec<Value> = video_ids.iter().map(|v| json!({"action": "ACTION_ADD_VIDEO", "addedVideoId": v, "dedupeOption": option})).collect();
    let value = api.call_signed_in("browse/edit_playlist", json!({"playlistId": playlist_id, "actions": actions})).await?;
    if duplicates == Duplicates::Ask && asks_about_duplicates(&value) {
        return Ok(false);
    }
    anyhow::ensure!(!value["status"].as_str().is_some_and(|s| !s.contains("SUCCEEDED")), "YouTube Music rejected the change");
    Ok(true)
}

/// A refused add that offers to add the songs anyway (a toast for one, a dialog for
/// more): that offer is the same add with the check skipped, in any language.
fn asks_about_duplicates(answer: &Value) -> bool {
    answer["status"].as_str().is_some_and(|s| !s.contains("SUCCEEDED")) && answer["actions"].to_string().contains("DEDUPE_OPTION_SKIP")
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

/// Make an uploaded picture (`Session::upload_playlist_cover`'s id) a playlist's cover.
pub async fn set_playlist_cover(api: &Api, id: &str, blob: &str) -> anyhow::Result<()> {
    let image = json!({"imageKey": {"type": "PLAYLIST_IMAGE_TYPE_CUSTOM_THUMBNAIL", "name": "studio_square_thumbnail"}, "playlistScottyEncryptedBlobId": blob});
    write(api, "browse/edit_playlist", json!({"playlistId": id, "actions": [{"action": "ACTION_SET_CUSTOM_THUMBNAIL", "addedCustomThumbnail": image}]})).await?;
    Ok(())
}

/// Take a playlist's own picture off, back to YouTube Music's cover.
pub async fn remove_playlist_cover(api: &Api, id: &str) -> anyhow::Result<()> {
    let image = json!({"type": "PLAYLIST_IMAGE_TYPE_CUSTOM_THUMBNAIL", "name": "studio_square_thumbnail"});
    write(api, "browse/edit_playlist", json!({"playlistId": id, "actions": [{"action": "ACTION_REMOVE_CUSTOM_THUMBNAIL", "deletedCustomThumbnail": image}]})).await?;
    Ok(())
}

/// Take entries out of a playlist, `(videoId, setVideoId)` pairs, in one request.
pub async fn remove_videos(api: &Api, playlist_id: &str, items: &[(String, String)]) -> anyhow::Result<()> {
    let actions: Vec<Value> = items.iter().map(|(v, set)| json!({"action": "ACTION_REMOVE_VIDEO", "removedVideoId": v, "setVideoId": set})).collect();
    write(api, "browse/edit_playlist", json!({"playlistId": playlist_id, "actions": actions})).await?;
    Ok(())
}

/// Move a playlist's entry `set_id` to just before the entry `before`, or to the end.
pub async fn move_video(api: &Api, playlist_id: &str, set_id: &str, before: Option<&str>) -> anyhow::Result<()> {
    let answer = write(api, "browse/edit_playlist", json!({"playlistId": playlist_id, "actions": [move_action(set_id, before)]})).await?;
    // No reload follows, so nothing else would notice a move that didn't happen.
    anyhow::ensure!(answer["status"].as_str().is_some_and(|s| s.contains("SUCCEEDED")), "YouTube Music didn't confirm the move");
    Ok(())
}

fn move_action(set_id: &str, before: Option<&str>) -> Value {
    let mut action = json!({"action": "ACTION_MOVE_VIDEO_BEFORE", "setVideoId": set_id});
    if let Some(next) = before {
        action["movedSetVideoIdSuccessor"] = next.into();
    }
    action
}

/// Delete an uploaded song from the account (`Track::upload`).
pub async fn delete_upload(api: &Api, entity: &str) -> anyhow::Result<()> {
    write(api, "music/delete_privately_owned_entity", json!({"entityId": entity})).await?;
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

/// Subscribe to an artist's channel, or unsubscribe.
pub async fn set_subscribed(api: &Api, channel_id: &str, on: bool) -> anyhow::Result<()> {
    let endpoint = if on { "subscription/subscribe" } else { "subscription/unsubscribe" };
    write(api, endpoint, json!({"channelIds": [channel_id]})).await?;
    Ok(())
}

pub async fn save_collection(api: &Api, playlist_id: &str, save: bool) -> anyhow::Result<()> {
    let endpoint = if save { "like/like" } else { "like/removelike" };
    write(api, endpoint, json!({"target": {"playlistId": list_id(playlist_id)}})).await?;
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
    let own = page_type(&h["thumbnail"]["musicThumbnailRenderer"]["onTap"]) == Some("MUSIC_PAGE_TYPE_USER_CHANNEL");
    let items: Vec<&Value> = arr(&r["contents"]).iter().filter(|i| !is_video(i)).collect();
    let kind = if podcasts() && items.iter().any(|i| !i["musicMultiRowListItemRenderer"].is_null()) {
        let tracks: Vec<Track> = items.iter().filter_map(|i| episode(&i["musicMultiRowListItemRenderer"])).collect();
        let rows = r["numItemsPerColumn"].as_str().and_then(|n| n.parse().ok()).unwrap_or(3);
        ShelfKind::Songs { tracks, rows }
    } else if items.iter().any(|i| !i["musicResponsiveListItemRenderer"].is_null()) {
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
    (!empty).then_some(Shelf { title, strapline, strap_thumb, own, kind })
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
            video: has_video(&[nav]),
            explicit: explicit(&r["subtitleBadges"]),
            stands_for: None,
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
    // Your uploads' own artist and album pages, whatever page type their links carry; Library
    // › Uploads' own tabs open another way.
    if id.starts_with(UPLOADS) {
        return match upload_kind(&id)? {
            "MUSIC_PAGE_TYPE_ALBUM" => Some(Target::Album(id)),
            _ => Some(Target::Artist(id)),
        };
    }
    Some(match page_type(nav)? {
        "MUSIC_PAGE_TYPE_ALBUM" | "MUSIC_PAGE_TYPE_AUDIOBOOK" => Target::Album(id),
        "MUSIC_PAGE_TYPE_ARTIST" | "MUSIC_PAGE_TYPE_USER_CHANNEL" => Target::Artist(id),
        // An artist's songs in your library ("MPLA" and their channel): their own page opens.
        "MUSIC_PAGE_TYPE_LIBRARY_ARTIST" => Target::Artist(id.strip_prefix("MPLA").unwrap_or(&id).to_owned()),
        "MUSIC_PAGE_TYPE_PLAYLIST" => Target::Playlist(id.strip_prefix("VL").unwrap_or(&id).to_owned()),
        // A show opens as the playlist it is, by its own page (see `list_id`).
        "MUSIC_PAGE_TYPE_PODCAST_SHOW_DETAIL_PAGE" if podcasts() => Target::Playlist(id),
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
    // Search rows lead with the kind ("Artist • 239M monthly audience", "アーティスト • …"),
    // unlinked and in the language asked for; a card shows it already.
    let unlinked_first = runs(detail).next().is_some_and(|r| r["navigationEndpoint"].is_null());
    if matches!(target, Target::Artist(_)) && parts.len() >= 2 && unlinked_first {
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

/// A podcast episode as Home's Podcasts feed and a show's page list it: its show stands
/// where a song's artist does, linked to the show's page.
fn episode(r: &Value) -> Option<Track> {
    let id = r["onTap"]["watchEndpoint"]["videoId"].as_str()?.to_owned();
    let length = runs(&r["playbackProgress"]["musicPlaybackProgressRenderer"]["durationText"]).filter_map(|run| run["text"].as_str()).find_map(spoken_duration);
    Some(Track { id, title: text(&r["title"])?, artists: show_links(&r["secondTitle"]), duration: length, thumbs: thumbs(&r["thumbnail"]["musicThumbnailRenderer"]), ..Default::default() })
}

/// An episode in search results: "Episode • Dec 28, 2025 • <its show>" where a song has
/// its artists, which is what `list_item` would take the date for. Episodes show no plays.
fn episode_row(r: &Value) -> Option<Track> {
    let mut t = list_item(r)?;
    t.artists = show_links(&r["flexColumns"][1]["musicResponsiveListItemFlexColumnRenderer"]["text"]);
    t.album = None;
    t.plays = None;
    Some(t)
}

/// The podcast shows named in a line of text, each linked to its page.
fn show_links(v: &Value) -> Vec<Link> {
    runs(v)
        .filter(|run| page_type(run) == Some("MUSIC_PAGE_TYPE_PODCAST_SHOW_DETAIL_PAGE"))
        .map(|run| Link { name: run["text"].as_str().unwrap_or_default().to_owned(), id: browse_id(run) })
        .collect()
}

/// "3 hr 36 min", "49 min", "1 hr", "45 sec", and as some other languages write them
/// ("7 分 49 秒", "1 Std. 5 Min.") → seconds.
fn spoken_duration(s: &str) -> Option<u32> {
    let mut words = s.split_whitespace();
    let (mut total, mut any) = (0u32, false);
    while let (Some(n), Some(unit)) = (words.next(), words.next()) {
        let n: u32 = n.parse().ok()?;
        total += n * match unit.trim_end_matches('.') {
            "hr" | "hrs" | "hour" | "hours" | "h" | "Std" | "時間" | "시간" | "ч" => 3600,
            "min" | "mins" | "Min" | "分" | "분" | "мин" => 60,
            "sec" | "secs" | "s" | "Sek" | "秒" | "초" | "сек" => 1,
            _ => return None,
        };
        any = true;
    }
    any.then_some(total)
}

/// A song row (playlists, search, history, album tracks, Quick picks).
/// Music videos and other uploaded videos (not songs). Feeds, charts and radios drop them;
/// your own playlists, likes and history keep what you put there.
fn is_video(item: &Value) -> bool {
    // Only where the item itself plays: a playlist card can hold its first video's type too.
    let (two, row) = (&item["musicTwoRowItemRenderer"], &item["musicResponsiveListItemRenderer"]);
    let own = [&two["navigationEndpoint"], &row["overlay"], &row["flexColumns"][0], &item["navigationEndpoint"]];
    has_video(&own)
}

/// A video of any kind (an official music video, a channel's upload), not a song.
fn has_video(parts: &[&Value]) -> bool {
    matches!(video_type(parts), Some("MUSIC_VIDEO_TYPE_OMV" | "MUSIC_VIDEO_TYPE_UGC" | "MUSIC_VIDEO_TYPE_OFFICIAL_SOURCE_MUSIC"))
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

/// Marked explicit: an "E" among the item's badges (rows' `badges`, cards' `subtitleBadges`).
fn explicit(badges: &Value) -> bool {
    arr(badges).iter().any(|b| b["musicInlineBadgeRenderer"]["icon"]["iconType"] == "MUSIC_EXPLICIT_BADGE")
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
    let parts = [&r["overlay"], &r["flexColumns"][0]];
    let (music_video, video) = (music_video(&parts), has_video(&parts));
    for c in 1..cols.len() {
        let column = arr(&col(c)?["runs"]);
        for (i, run) in column.iter().enumerate() {
            let name = run["text"].as_str().unwrap_or_default();
            // Uploads link their artist and album to the account's own pages of them.
            let (kind, id) = (upload_page(run).or_else(|| page_type(run)), browse_id(run));
            if c == 1 && in_artists {
                match name.trim() {
                    "•" => in_artists = false,
                    _ if joins_names(column, i) => {}
                    "Song" | "Video" | "Episode" if i == 0 => {}
                    n if kind.is_none_or(|k| k == "MUSIC_PAGE_TYPE_ARTIST" || k == "MUSIC_PAGE_TYPE_USER_CHANNEL") && !looks_like_duration(n) && !n.ends_with(" plays") => {
                        artists.push(Link { name: n.to_owned(), id: id.clone() })
                    }
                    _ => {}
                }
                if in_artists {
                    continue;
                }
            }
            match kind {
                Some("MUSIC_PAGE_TYPE_ARTIST" | "MUSIC_PAGE_TYPE_USER_CHANNEL") if !artists.iter().any(|a| a.name == name) => {
                    artists.push(Link { name: name.to_owned(), id })
                }
                Some("MUSIC_PAGE_TYPE_ALBUM") => album = Some(Link { name: name.to_owned(), id }),
                // Search rows put the length in the text ("Artist • Album • 3:22").
                _ if duration.is_none() && looks_like_duration(name) => duration = parse_duration(name),
                _ if name.ends_with(" plays") => plays = count(name),
                // In another language ("再生回数 966万 回"): an unlinked count in a column
                // of its own, or after the artists on a song's row (a video's is its views).
                // Not a date ("Dec 28, 2025", "2025/12/28"), which holds more than one number.
                None if plays.is_none() && id.is_none() && (c >= 2 || !video) => plays = lone_count(name),
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
    let stands_for = like_target(&r["menu"]).filter(|v| *v != id);
    Some(Track { id, title, artists, album, duration, thumbs: thumbs(&r["thumbnail"]["musicThumbnailRenderer"]), track_nr, plays, music_video, video, explicit: explicit(&r["badges"]), stands_for })
}

/// The video a row's menu likes or unlikes. In Liked Music, a video liked on YouTube is
/// listed as the song YouTube Music matched it to, and its menu still points at the video.
fn like_target(menu: &Value) -> Option<String> {
    fn find(v: &Value) -> Option<&str> {
        match v {
            Value::Object(m) => m.get("likeEndpoint").and_then(|e| e["target"]["videoId"].as_str()).or_else(|| m.values().find_map(find)),
            Value::Array(a) => a.iter().find_map(find),
            _ => None,
        }
    }
    find(menu).map(str::to_owned)
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
        video: has_video(&[&r["navigationEndpoint"]]),
        explicit: explicit(&r["badges"]),
        stands_for: None,
    })
}

fn artist_links(v: &Value) -> Vec<Link> {
    runs(v)
        .filter(|run| matches!(upload_page(run).or_else(|| page_type(run)), Some("MUSIC_PAGE_TYPE_ARTIST" | "MUSIC_PAGE_TYPE_USER_CHANNEL")))
        .map(|run| Link { name: run["text"].as_str().unwrap_or_default().to_owned(), id: browse_id(run) })
        .collect()
}

/// Names in a header strapline, linked when they are.
fn links(v: &Value) -> Vec<Link> {
    let all = arr(&v["runs"]);
    (0..all.len())
        .filter(|&i| !joins_names(all, i))
        .filter_map(|i| Some(Link { name: all[i]["text"].as_str()?.trim().to_owned(), id: browse_id(&all[i]) }))
        .collect()
}

/// The run at `i` goes between names rather than being one, in any language: unlinked, and
/// no letters or digits (",", "&", "•", "、"), or a word between two linked names ("und", "et", "및").
fn joins_names(all: &[Value], i: usize) -> bool {
    let linked = |j: usize| all.get(j).is_some_and(|r| !r["navigationEndpoint"].is_null());
    let text = all[i]["text"].as_str().unwrap_or_default();
    !linked(i) && (!text.chars().any(char::is_alphanumeric) || i > 0 && linked(i - 1) && linked(i + 1))
}

fn looks_like_duration(s: &str) -> bool {
    s.contains(':') && s.split(':').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

fn parse_duration(s: &str) -> Option<u32> {
    s.split(':').try_fold(0u32, |acc, p| Some(acc * 60 + p.trim().parse::<u32>().ok()?))
}

/// "3.6B plays", "40.1M", "12K subscribers", and as other languages write them ("再生回数
/// 966万 回", "1,2 Mio. Aufrufe") → the number. A comma or dot before three digits groups
/// thousands, before fewer it's the decimal point. A fraction without a unit after it isn't
/// a count: `None`, rather than one off by the unit.
fn count(s: &str) -> Option<u64> {
    count_and_rest(s).map(|(n, _)| n)
}

/// [`count`], when `s` holds no other number: "2025年12月28日" and "28.12.2025" are dates.
fn lone_count(s: &str) -> Option<u64> {
    count_and_rest(s).filter(|(_, rest)| !rest.contains(|c: char| c.is_ascii_digit())).map(|(n, _)| n)
}

/// [`count`], and the text after its number.
fn count_and_rest(s: &str) -> Option<(u64, &str)> {
    let s = &s[s.find(|c: char| c.is_ascii_digit())?..];
    let b = s.as_bytes();
    let (mut num, mut fraction, mut i) = (String::new(), false, 0);
    while let Some(&c) = b.get(i) {
        let digits_after = b[i + 1..].iter().take_while(|d| d.is_ascii_digit()).count();
        match c {
            b'0'..=b'9' => num.push(c as char),
            b',' | b'.' if !fraction && digits_after == 3 => {}
            b',' | b'.' if !fraction && digits_after > 0 => {
                fraction = true;
                num.push('.');
            }
            _ => break,
        }
        i += 1;
    }
    let unit = s[i..].trim_start_matches([' ', '\u{a0}', '\u{202f}']);
    let mut words = unit.split(|c: char| c.is_whitespace() || c == '.').filter(|w| !w.is_empty());
    let word = words.next().unwrap_or_default();
    let mult = match unit.chars().next() {
        Some('万' | '萬' | '만') => 1e4,
        Some('億' | '亿' | '억') => 1e8,
        _ => match word {
            // Spanish billions: "3,4 mil M".
            "mil" if words.next() == Some("M") => 1e9,
            // Indonesian billions: "1,2 M pemutaran" / "1,2 M x ditonton" (M = miliar).
            "M" if matches!(words.next(), Some("pemutaran" | "ditonton" | "kali" | "x" | "pendengar" | "subscriber")) => 1e9,
            "K" | "k" | "Tsd" | "mil" | "mila" | "tys" | "тыс" | "rb" | "Rb" => 1e3,
            "M" | "Mio" | "mi" | "Mln" | "mln" | "млн" | "jt" | "Jt" => 1e6,
            "B" | "Mrd" | "Md" | "bi" | "Mld" | "mld" | "млрд" | "mlr" | "Mlr" => 1e9,
            _ if fraction => return None,
            _ => 1.0,
        },
    };
    Some(((num.parse::<f64>().ok()? * mult).round() as u64, &s[i..]))
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

/// The account's uploads pages: Library › Uploads and its tabs, and an uploaded artist's
/// or album's own page.
const UPLOADS: &str = "FEmusic_library_privately_owned_";

/// Which of your uploads' pages a browse id opens: an uploaded artist's
/// ("FEmusic_library_privately_owned_artist_detail…") or album's ("…_release_detail…"),
/// as the page type their links should carry. They don't always (an artist's is often
/// UNKNOWN).
fn upload_kind(id: &str) -> Option<&'static str> {
    let rest = id.strip_prefix(UPLOADS)?;
    if rest.starts_with("artist_detail") {
        Some("MUSIC_PAGE_TYPE_ARTIST")
    } else if rest.starts_with("release_detail") {
        Some("MUSIC_PAGE_TYPE_ALBUM")
    } else {
        None
    }
}

/// An uploaded artist's or album's page, which only the account that uploaded it can see.
pub fn uploaded(id: &str) -> bool {
    upload_kind(id).is_some()
}

/// An uploaded song's artist or album run, read as the artist or album it names.
fn upload_page(run: &Value) -> Option<&'static str> {
    upload_kind(run["navigationEndpoint"]["browseEndpoint"]["browseId"].as_str()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_add_offering_add_anyway_is_about_duplicates() {
        // Trimmed from YouTube Music's answers (November 2026): one song, then two.
        let add = |opt: &str| json!({"playlistEditEndpoint": {"actions": [{"action": "ACTION_ADD_VIDEO", "addedVideoId": "a", "dedupeOption": opt}]}});
        let toast = json!({"status": "STATUS_FAILED", "actions": [{"addToToastAction": {"item": {"notificationActionRenderer": {"actionButton": {"buttonRenderer": {"command": add("DEDUPE_OPTION_SKIP")}}}}}}]});
        let dialog = json!({"status": "STATUS_FAILED", "actions": [{"confirmDialogEndpoint": {"content": {"confirmDialogRenderer": {
            "cancelButton": {"buttonRenderer": {"command": add("DEDUPE_OPTION_DROP_DUPLICATE")}},
            "confirmButton": {"buttonRenderer": {"command": add("DEDUPE_OPTION_SKIP")}},
        }}}}]});
        assert!(asks_about_duplicates(&toast));
        assert!(asks_about_duplicates(&dialog));
        assert!(!asks_about_duplicates(&json!({"status": "STATUS_FAILED", "actions": []})));
        assert!(!asks_about_duplicates(&json!({"status": "STATUS_SUCCEEDED", "actions": [{"addToToastAction": {}}]})));
    }

    #[test]
    fn song_rating_comes_from_the_requested_player_button() {
        let mut response = json!({
            "playerOverlays": {"playerOverlayRenderer": {"actions": [
                {"likeButtonRenderer": {"target": {"videoId": "another"}, "likeStatus": "DISLIKE"}},
                {"likeButtonRenderer": {"target": {"videoId": "song"}, "likeStatus": "INDIFFERENT"}}
            ]}},
            "contents": {"likeEndpoint": {"target": {"videoId": "song"}, "status": "DISLIKE"}}
        });
        assert_eq!(parse_song_rating(&response, "song"), Some(SongRating::Indifferent));
        assert_eq!(parse_song_rating(&response, "another"), Some(SongRating::Dislike));
        assert_eq!(parse_song_rating(&response, "missing"), None);
        let status = &mut response["playerOverlays"]["playerOverlayRenderer"]["actions"][1]["likeButtonRenderer"]["likeStatus"];
        *status = json!("LIKE");
        assert_eq!(parse_song_rating(&response, "song"), Some(SongRating::Like));
    }

    #[test]
    fn song_rating_reads_the_signed_in_players_entity() {
        let mut response = json!({
            "currentVideoEndpoint": {"watchEndpoint": {"videoId": "song"}},
            "playerOverlays": {"playerOverlayRenderer": {"videoActionBar": {"videoActionBarViewModel": {"buttons": [
                {"buttonViewModel": {"segmentedLikeDislikeButtonViewModel": {
                    "likeButtonViewModel": {"likeButtonViewModel": {
                        "likeStatusEntityKey": "song-rating",
                        "likeStatusEntity": {"key": "song-rating", "likeStatus": "DISLIKE"}
                    }}
                }}}
            ]}}}},
            "frameworkUpdates": {"entityBatchUpdate": {"mutations": [
                {"payload": {"likeStatusEntity": {"key": "another-rating", "likeStatus": "LIKE"}}}
            ]}}
        });
        assert_eq!(parse_song_rating(&response, "song"), Some(SongRating::Dislike));
        assert_eq!(parse_song_rating(&response, "another"), None);
        response["frameworkUpdates"]["entityBatchUpdate"]["mutations"].as_array_mut().unwrap().push(
            json!({"payload": {"likeStatusEntity": {"key": "song-rating", "likeStatus": "INDIFFERENT"}}}),
        );
        assert_eq!(parse_song_rating(&response, "song"), Some(SongRating::Indifferent));
    }

    #[test]
    fn play_report_asks_with_the_signature_timestamp() {
        let body = report_play_body("gIhejm0k3dU", Some(20767));
        assert_eq!(body["videoId"], "gIhejm0k3dU");
        assert_eq!(body["playbackContext"]["contentPlaybackContext"]["signatureTimestamp"], 20767);
        // Without a prepared player it asks as before rather than not at all.
        let body = report_play_body("gIhejm0k3dU", None);
        assert!(body.get("playbackContext").is_none());
        assert_eq!(body["contentCheckOk"], true);
    }

    #[test]
    fn own_playlist_sorts_come_from_the_menu() {
        let item = |title: &str, on: bool, action: Value| json!({"title": title, "selected": on, "serviceEndpoint": {"playlistEditEndpoint": {"playlistId": "PL1", "actions": [action]}}});
        let shelf = json!({"header": {"musicSideAlignedItemRenderer": {"startItems": [{"sortFilterSubMenuRenderer": {"subMenuItems": [
            item("Manual ordering", false, json!({"action": "ACTION_SET_PLAYLIST_VIDEO_ORDER", "playlistVideoOrder": 0})),
            item("Newest first", true, json!({"action": "ACTION_SET_PLAYLIST_VIDEO_ORDER", "playlistVideoOrder": 1})),
            item("Title", false, json!({"action": "ACTION_SET_PLAYLIST_DYNAMIC_SORT_PREFERENCE", "playlistDynamicSortPreference": 1})),
        ]}}]}}});
        let s = sorts(&shelf);
        let got: Vec<(&str, bool, bool)> = s.iter().map(|s| (s.name.as_str(), s.on, s.manual)).collect();
        assert_eq!(got, [("Manual ordering", false, true), ("Newest first", true, false), ("Title", false, false)]);
        assert_eq!(s[1].actions[0]["playlistVideoOrder"], 1);
        // Liked Music's menu has nothing to send: no Sort of YouTube Music's there.
        let liked = json!({"header": {"musicSideAlignedItemRenderer": {"startItems": [{"sortFilterSubMenuRenderer": {"subMenuItems": [{"title": "Newest first", "selected": true}]}}]}}});
        assert!(sorts(&liked).is_empty());
    }

    #[test]
    fn moods_and_genres_are_read_with_their_categories() {
        let button = |name: &str, params: &str| json!({"musicNavigationButtonRenderer": {"buttonText": {"runs": [{"text": name}]}, "solid": {"leftStripeColor": 4288988671u64},
            "clickCommand": {"browseEndpoint": {"browseId": "FEmusic_moods_and_genres_category", "params": params}}}});
        let grid = |title: &str, items: Vec<Value>| json!({"gridRenderer": {"header": {"gridHeaderRenderer": {"title": {"runs": [{"text": title}]}}}, "items": items}});
        let v = json!({"contents": {"singleColumnBrowseResultsRenderer": {"tabs": [{"tabRenderer": {"content": {"sectionListRenderer": {"contents": [
            grid("Moods & moments", vec![button("Chill", "p1"), button("Focus", "p2")]),
            grid("Genres", vec![button("Rock", "p3")]),
            grid("Empty", vec![]),
        ]}}}}]}}});
        let groups = super::parse_moods(&v);
        assert_eq!(groups.iter().map(|g| g.title.as_str()).collect::<Vec<_>>(), ["Moods & moments", "Genres"]);
        assert_eq!((groups[0].moods[1].title.as_str(), groups[0].moods[1].params.as_str(), groups[0].moods[1].color), ("Focus", "p2", 0xFFA4_C5FF));
        let page = json!({"header": {"musicHeaderRenderer": {"title": {"runs": [{"text": "Chill"}]}}},
            "contents": {"singleColumnBrowseResultsRenderer": {"tabs": [{"tabRenderer": {"content": {"sectionListRenderer": {"contents": [
                {"musicCarouselShelfRenderer": {"header": {"musicCarouselShelfBasicHeaderRenderer": {"title": {"runs": [{"text": "Coffee shop blends"}]}}},
                    "contents": [{"musicTwoRowItemRenderer": {"title": {"runs": [{"text": "Mellow"}]}, "navigationEndpoint": {"browseEndpoint": {"browseId": "VLRDCLAK5uy_x", "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {"pageType": "MUSIC_PAGE_TYPE_PLAYLIST"}}}}}}]}}
            ]}}}}]}}});
        let m = super::parse_mood(&page, "p1");
        assert_eq!((m.title.as_str(), m.shelves.len(), m.shelves[0].title.as_str()), ("Chill", 1, "Coffee shop blends"));
    }

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
    fn library_songs_list_the_songs_saved_there() {
        // Shaped like a signed-in FEmusic_liked_videos answer (November 2026): a "Shuffle
        // all" row before the songs, and the next page's token on the shelf.
        let col = |runs: Value| json!({"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": runs}}});
        let browse = |id: &str, kind: &str| json!({"browseEndpoint": {"browseId": id, "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {"pageType": kind}}}});
        let watch = json!({"watchEndpoint": {"videoId": "JhulBGMA7G4", "playlistId": "MLCT", "watchEndpointMusicSupportedConfigs": {"watchEndpointMusicConfig": {"musicVideoType": "MUSIC_VIDEO_TYPE_ATV"}}}});
        let song = json!({"musicResponsiveListItemRenderer": {
            "flexColumns": [
                col(json!([{"text": "Harder, Better, Faster, Stronger", "navigationEndpoint": watch}])),
                col(json!([{"text": "Daft Punk", "navigationEndpoint": browse("UC_kRDKYrUlrbtrSiyu5Tflg", "MUSIC_PAGE_TYPE_ARTIST")}])),
                col(json!([{"text": "Discovery", "navigationEndpoint": browse("MPREb_GDfJeO4t1sl", "MUSIC_PAGE_TYPE_ALBUM")}])),
            ],
            "fixedColumns": [{"musicResponsiveListItemFixedColumnRenderer": {"text": {"runs": [{"text": "3:45"}]}}}],
        }});
        let shuffle = json!({"musicResponsiveListItemRenderer": {
            "flexColumns": [col(json!([{"text": "Shuffle all"}]))],
            "navigationEndpoint": {"watchPlaylistEndpoint": {"playlistId": "MLCT", "params": "wAEB8gECKAE%3D"}},
        }});
        let page = |shelf: Value| json!({"contents": {"singleColumnBrowseResultsRenderer": {"tabs": [{"tabRenderer": {"content": {"sectionListRenderer": {"contents": [shelf]}}}}]}}});
        let v = page(json!({"musicShelfRenderer": {"contents": [shuffle, song], "continuations": [{"nextContinuationData": {"continuation": "next-page"}}]}}));
        let (c, token) = parse_library_songs(&v);
        assert_eq!(c.id, LIBRARY_SONGS);
        assert!(!c.owned && !c.is_album);
        assert_eq!(c.tracks.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), ["Harder, Better, Faster, Stronger"]);
        let t = &c.tracks[0];
        assert_eq!((t.id.as_str(), t.artist_line().as_str(), t.duration), ("JhulBGMA7G4", "Daft Punk", Some(225)));
        assert_eq!(t.album.as_ref().map(|a| a.name.as_str()), Some("Discovery"));
        assert_eq!(c.set_ids.len(), c.tracks.len());
        assert_eq!(token.as_deref(), Some("next-page"));
        // The later pages continue as a playlist's do.
        let more = json!({"continuationContents": {"musicShelfContinuation": {"contents": [song]}}});
        let (tracks, _, next) = playlist_continuation(&more);
        assert_eq!((tracks.len(), next), (1, None));
        // Nothing saved yet: a message where the shelf would be, and no songs.
        let (c, token) = parse_library_songs(&page(json!({"itemSectionRenderer": {"contents": [{"messageRenderer": {}}]}})));
        assert!(c.tracks.is_empty() && token.is_none());
    }

    #[test]
    fn suggestions_offer_the_songs_artists_and_albums_they_mean() {
        // Trimmed from get_search_suggestions answers to "daft punk" and "random access
        // memories" (November 2026).
        let browse = |id: &str, kind: &str| json!({"browseEndpoint": {"browseId": id, "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {"pageType": kind}}}});
        let col = |runs: Value| json!({"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": runs}}});
        let thumb = json!({"musicThumbnailRenderer": {"thumbnail": {"thumbnails": [{"url": "https://lh3.googleusercontent.com/qLhu6P", "width": 60, "height": 60}]}}});
        let daft = browse("UCRr1xG_2WIDs18a6cIiCxeA", "MUSIC_PAGE_TYPE_ARTIST");
        let artist = json!({"musicResponsiveListItemRenderer": {"thumbnail": thumb, "navigationEndpoint": daft,
            "flexColumns": [col(json!([{"text": "Daft Punk"}])), col(json!([{"text": "65.2M monthly audience"}]))]}});
        let song = |kind: &str, id: &str, title: &str, count: &str| {
            let watch = json!({"watchEndpoint": {"videoId": id, "watchEndpointMusicSupportedConfigs": {"watchEndpointMusicConfig": {"musicVideoType": "MUSIC_VIDEO_TYPE_ATV"}}}});
            json!({"musicResponsiveListItemRenderer": {"thumbnail": thumb, "navigationEndpoint": watch, "playlistItemData": {"videoId": id},
                "flexColumns": [col(json!([{"text": title, "navigationEndpoint": watch}])),
                    col(json!([{"text": kind}, {"text": " • "}, {"text": "Daft Punk", "navigationEndpoint": daft}, {"text": " • "}, {"text": count}]))]}})
        };
        let album = json!({"musicResponsiveListItemRenderer": {"thumbnail": thumb, "navigationEndpoint": browse("MPREb_ram", "MUSIC_PAGE_TYPE_ALBUM"),
            "flexColumns": [col(json!([{"text": "Random Access Memories"}])),
                col(json!([{"text": "Album"}, {"text": " • "}, {"text": "Daft Punk", "navigationEndpoint": daft}, {"text": " • "}, {"text": "2013"}]))]}});
        let answer = |rows: Vec<Value>| json!({"contents": [
            {"searchSuggestionsSectionRenderer": {"contents": [{"searchSuggestionRenderer": {"suggestion": {"runs": [{"text": "daft punk"}]}}}]}},
            {"searchSuggestionsSectionRenderer": {"contents": rows}}
        ]});
        let hits = parse_suggested_hits(&answer(vec![artist, song("Song", "JhulBGMA7G4", "Harder, Better, Faster, Stronger", "401M plays"), album]));
        let shown: Vec<_> = hits.iter().map(|c| (c.title.as_str(), c.subtitle.as_str(), c.round)).collect();
        assert_eq!(
            shown,
            [("Daft Punk", "65.2M monthly audience", true), ("Harder, Better, Faster, Stronger", "Song • Daft Punk • 401M plays", false), ("Random Access Memories", "Album • Daft Punk • 2013", false)]
        );
        assert!(matches!(&hits[0].target, Target::Artist(id) if id == "UCRr1xG_2WIDs18a6cIiCxeA"));
        assert!(matches!(&hits[2].target, Target::Album(id) if id == "MPREb_ram"));
        let Target::Song(t) = &hits[1].target else { panic!("a song plays") };
        assert_eq!((t.id.as_str(), t.artist_line().as_str()), ("JhulBGMA7G4", "Daft Punk"));
        assert!(!hits[0].thumbs.is_empty() && !t.thumbs.is_empty());
        // In another language the kind is no artist either.
        let hits = parse_suggested_hits(&answer(vec![song("Canción", "wU26xVT_vBU", "One More Time", "860 M de reproducciones")]));
        let Target::Song(t) = &hits[0].target else { panic!("a song plays") };
        assert_eq!(t.artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["Daft Punk"]);
        // An episode plays by its show, and only with podcasts shown.
        let show = browse("MPSPPL1", "MUSIC_PAGE_TYPE_PODCAST_SHOW_DETAIL_PAGE");
        let watch = json!({"watchEndpoint": {"videoId": "xjZ31IKfu4Q"}});
        let episode = json!({"musicResponsiveListItemRenderer": {"thumbnail": thumb, "navigationEndpoint": watch, "playlistItemData": {"videoId": "xjZ31IKfu4Q"},
            "flexColumns": [col(json!([{"text": "The Buried Bodies Case", "navigationEndpoint": watch}])),
                col(json!([{"text": "Episode"}, {"text": " • "}, {"text": "Dec 28, 2025"}, {"text": " • "}, {"text": "Radiolab", "navigationEndpoint": show}]))]}});
        let rows = || answer(vec![episode.clone(), song("Song", "JhulBGMA7G4", "Harder, Better, Faster, Stronger", "401M plays")]);
        let off = parse_suggested_hits_with(&rows(), false);
        assert_eq!(off.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(), ["Harder, Better, Faster, Stronger"]);
        let on = parse_suggested_hits_with(&rows(), true);
        assert_eq!(on.len(), 2);
        let Target::Song(t) = &on[0].target else { panic!("an episode plays") };
        assert_eq!((t.id.as_str(), t.artist_line().as_str()), ("xjZ31IKfu4Q", "Radiolab"));
        // Recent searches, with nothing typed, have no second section.
        assert!(parse_suggested_hits(&json!({"contents": [{"searchSuggestionsSectionRenderer": {"contents": []}}]})).is_empty());
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

    /// A Liked Music row as YouTube Music sends it (November 2026) for a video liked on
    /// YouTube: the song it matched the video to, whose menu likes the video itself.
    fn liked_row(id: &str, liked: &str) -> Value {
        let watch = json!({"watchEndpoint": {"videoId": id, "watchEndpointMusicSupportedConfigs": {"watchEndpointMusicConfig": {"musicVideoType": "MUSIC_VIDEO_TYPE_ATV"}}}});
        let unlike = json!({"menuServiceItemRenderer": {"text": {"runs": [{"text": "Remove from playlist"}]}, "serviceEndpoint": {"likeEndpoint": {"status": "INDIFFERENT", "target": {"videoId": liked}}}}});
        json!({
            "playlistItemData": {"videoId": id, "playlistSetVideoId": "56B44F6D10557CC6"},
            "flexColumns": [
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Теплая ночь", "navigationEndpoint": watch}]}}},
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Гиксекот"}]}}}
            ],
            "overlay": {"musicItemThumbnailOverlayRenderer": {"content": {"musicPlayButtonRenderer": {"playNavigationEndpoint": watch}}}},
            "menu": {"menuRenderer": {"items": [unlike]}}
        })
    }

    #[test]
    fn a_liked_video_listed_as_its_song_knows_its_video() {
        let t = list_item(&liked_row("R5Wv-eZD1l8", "u6Pr452s_2s")).unwrap();
        assert_eq!((t.id.as_str(), t.stands_for.as_deref()), ("R5Wv-eZD1l8", Some("u6Pr452s_2s")));
        // Still the song: it plays as one, with the video a switch away.
        assert!(!t.has_video());
    }

    #[test]
    fn a_liked_song_stands_for_nothing_else() {
        assert_eq!(list_item(&liked_row("R5Wv-eZD1l8", "R5Wv-eZD1l8")).unwrap().stands_for, None);
    }

    // Library › Uploads row, shaped like a real answer (November 2026): the artist and album lead
    // to the account's uploads, the artist's page type UNKNOWN and the album's ALBUM.
    const UPLOAD_ROW: &str = r#"{"musicResponsiveListItemRenderer":{"flexColumns":[{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"A Sky Full Of Stars","navigationEndpoint":{"watchEndpoint":{"videoId":"FDpg6FxVQZU"}}}]}}},{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"Coldplay","navigationEndpoint":{"browseEndpoint":{"browseId":"FEmusic_library_privately_owned_artist_detaila_po_CICr2crg7OWpchIIY29sZHBsYXk","browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_UNKNOWN"}}}}}]}}},{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"Ghost Stories","navigationEndpoint":{"browseEndpoint":{"browseId":"FEmusic_library_privately_owned_release_detailb_po_CICr2crg7OWpchIOZ2hvc3Qgc3Rvcmllcw","browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_ALBUM"}}}}}]}}}],"fixedColumns":[{"musicResponsiveListItemFixedColumnRenderer":{"text":{"runs":[{"text":"4:28"}]}}}]}}"#;

    #[test]
    fn upload_row_links_its_artist_and_album_to_their_uploads_pages() {
        const ARTIST: &str = "FEmusic_library_privately_owned_artist_detaila_po_CICr2crg7OWpchIIY29sZHBsYXk";
        const ALBUM: &str = "FEmusic_library_privately_owned_release_detailb_po_CICr2crg7OWpchIOZ2hvc3Qgc3Rvcmllcw";
        let t = list_item(&row(UPLOAD_ROW)).unwrap();
        assert_eq!(names(&t), ["Coldplay"]);
        assert_eq!(t.artists[0].id.as_deref(), Some(ARTIST));
        assert_eq!(t.album.as_ref().map(|a| (a.name.as_str(), a.id.as_deref())), Some(("Ghost Stories", Some(ALBUM))));
        assert_eq!(t.duration, Some(268));
        // Whatever page type the run carries: none, or a plain artist's.
        let bare = UPLOAD_ROW.replace(r#","browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_UNKNOWN"}}"#, "");
        assert_eq!(list_item(&row(&bare)).unwrap().artists, t.artists);
        let plain = UPLOAD_ROW.replace("MUSIC_PAGE_TYPE_UNKNOWN", "MUSIC_PAGE_TYPE_ARTIST");
        assert_eq!(list_item(&row(&plain)).unwrap().artists, t.artists);
        // Those pages open in the app; Library › Uploads' own tabs aren't one of them.
        assert!(uploaded(ARTIST) && uploaded(ALBUM));
        assert!(!uploaded("FEmusic_library_privately_owned_tracks") && !uploaded("MPREb_54uxDZG0XOp"));
    }

    #[test]
    fn an_uploaded_song_knows_what_deletes_it() {
        // Its menu's Delete, as the uploads page has it (checked against a live answer).
        let mut row: Value = serde_json::from_str(UPLOAD_ROW).unwrap();
        row["musicResponsiveListItemRenderer"]["menu"] = json!({"menuRenderer": {"items": [
            {"menuNavigationItemRenderer": {"text": {"runs": [{"text": "Go to artist"}]}}},
            {"menuNavigationItemRenderer": {"icon": {"iconType": "DELETE"}, "navigationEndpoint": {"confirmDialogEndpoint": {"content": {"confirmDialogRenderer": {
                "confirmButton": {"buttonRenderer": {"command": {"musicDeletePrivatelyOwnedEntityCommand": {"entityId": "t_po_CL-T5LKR7Zrq4QEQ2fetwwY"}}}}
            }}}}}},
        ]}});
        row["musicResponsiveListItemRenderer"]["flexColumns"][0]["musicResponsiveListItemFlexColumnRenderer"]["text"]["runs"][0]["navigationEndpoint"]["watchEndpoint"]["videoId"] = json!("delete_test");
        let t = upload_row(&row["musicResponsiveListItemRenderer"]).unwrap();
        assert_eq!(t.id, "delete_test");
        assert_eq!(upload_entity("delete_test").as_deref(), Some("t_po_CL-T5LKR7Zrq4QEQ2fetwwY"));
        // Without one, nothing to delete it by.
        assert_eq!(upload_entity("not_uploaded"), None);
    }

    // An uploaded album's page as ytmusicapi reads one (not checked against a live answer):
    // the header albums had before 2024, then the songs in one column.
    fn upload_album_page() -> Value {
        let row: Value = serde_json::from_str(UPLOAD_ROW).unwrap();
        // A row that names its song only in its menu, and one with no song (Shuffle all).
        let menu_only = json!({"musicResponsiveListItemRenderer": {
            "flexColumns": [{"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Midnight City"}]}}}],
            "menu": {"menuRenderer": {"items": [{"menuServiceItemRenderer": {"serviceEndpoint": {"queueAddEndpoint": {"queueTarget": {"videoId": "dX3k_QDnzHE"}}}}}]}},
        }});
        let shuffle = json!({"musicResponsiveListItemRenderer": {"flexColumns": [{"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Shuffle all"}]}}}]}});
        json!({
            "header": {"musicDetailHeaderRenderer": {
                "title": {"runs": [{"text": "Ghost Stories"}]},
                "subtitle": {"runs": [{"text": "Album"}, {"text": " • "}, {"text": "Coldplay", "navigationEndpoint": {"browseEndpoint": {"browseId": "FEmusic_library_privately_owned_artist_detaila_po_CICr2crg7OWpchIIY29sZHBsYXk"}}}, {"text": " • "}, {"text": "2014"}]},
                "thumbnail": {"croppedSquareThumbnailRenderer": {"thumbnail": {"thumbnails": [{"url": "https://lh3.googleusercontent.com/gs=w226-h226", "width": 226, "height": 226}]}}},
            }},
            "contents": {"singleColumnBrowseResultsRenderer": {"tabs": [{"tabRenderer": {"content": {"sectionListRenderer": {"contents": [{"musicShelfRenderer": {
                "contents": [shuffle, row, menu_only],
                "continuations": [{"nextContinuationData": {"continuation": "more"}}],
            }}]}}}}]}},
        })
    }

    #[test]
    fn uploaded_album_page_lists_its_songs() {
        let id = "FEmusic_library_privately_owned_release_detailb_po_CICr2crg7OWpchIOZ2hvc3Qgc3Rvcmllcw";
        let (a, token) = parse_upload_album(&upload_album_page(), id).unwrap();
        assert_eq!((a.title.as_str(), a.subtitle.as_str(), a.is_album, a.save_id.as_deref()), ("Ghost Stories", "Album • Coldplay • 2014", true, None));
        assert_eq!(a.artists[0].id.as_deref(), Some("FEmusic_library_privately_owned_artist_detaila_po_CICr2crg7OWpchIIY29sZHBsYXk"));
        assert_eq!(a.thumbs.len(), 1);
        assert_eq!(a.tracks.iter().map(|t| (t.id.as_str(), t.title.as_str())).collect::<Vec<_>>(), [("FDpg6FxVQZU", "A Sky Full Of Stars"), ("dX3k_QDnzHE", "Midnight City")]);
        // What a row leaves out, the header says.
        let last = &a.tracks[1];
        assert_eq!((names(last), last.album.as_ref().and_then(|l| l.id.as_deref()), last.thumbs.len()), (vec!["Coldplay"], Some(id), 1));
        assert_eq!(token.as_deref(), Some("more"));
    }

    #[test]
    fn uploaded_album_page_reads_the_newer_header_too() {
        let v = upload_album_page();
        let shelf = v["contents"]["singleColumnBrowseResultsRenderer"]["tabs"][0]["tabRenderer"]["content"]["sectionListRenderer"]["contents"][0].clone();
        let v = json!({"contents": {"twoColumnBrowseResultsRenderer": {
            "tabs": [{"tabRenderer": {"content": {"sectionListRenderer": {"contents": [{"musicResponsiveHeaderRenderer": {
                "title": {"runs": [{"text": "Ghost Stories"}]},
                "subtitle": {"runs": [{"text": "Album"}, {"text": " • "}, {"text": "2014"}]},
                "straplineTextOne": {"runs": [{"text": "Coldplay"}]},
            }}]}}}}],
            "secondaryContents": {"sectionListRenderer": {"contents": [shelf]}},
        }}});
        let (a, _) = parse_upload_album(&v, "FEmusic_library_privately_owned_release_detailb").unwrap();
        assert_eq!((a.subtitle.as_str(), a.tracks.len()), ("Album • Coldplay • 2014", 2));
    }

    #[test]
    fn uploaded_artist_page_lists_all_your_songs_by_them() {
        let id = "FEmusic_library_privately_owned_artist_detaila_po_CICr2crg7OWpchIIY29sZHBsYXk";
        let mut v = upload_album_page();
        v["header"] = Value::Null;
        let (page, token) = parse_upload_artist(&v, id).unwrap();
        // No header: the name its songs give it.
        assert_eq!((page.id.as_str(), page.name.as_str(), page.top.len(), page.channel_id.as_deref()), (id, "Coldplay", 2, None));
        assert_eq!(token.as_deref(), Some("more"));
        v["header"] = json!({"musicHeaderRenderer": {"title": {"runs": [{"text": "Coldplay (uploads)"}]}}});
        assert_eq!(parse_upload_artist(&v, id).unwrap().0.name, "Coldplay (uploads)");
        assert!(parse_upload_artist(&json!({}), id).is_none());
    }

    // A search of your uploads and one of your library, trimmed from real answers (November
    // 2026). Only the picked tab holds results. Uploads come in one untitled shelf: your
    // uploads' artist and album (leading to pages of your own), then the songs.
    const UPLOADS_SEARCH: &str = r#"{"contents":{"tabbedSearchResultsRenderer":{"tabs":[{"tabRenderer":{"title":"YT Music","selected":false,"endpoint":{"searchEndpoint":{"params":"agIYAQ%3D%3D","query":"kaede"}}}},{"tabRenderer":{"title":"Library","selected":false,"endpoint":{"searchEndpoint":{"params":"agIYBA%3D%3D","query":"kaede"}}}},{"tabRenderer":{"title":"Uploads","selected":true,"content":{"sectionListRenderer":{"contents":[{"musicShelfRenderer":{"contents":[{"musicResponsiveListItemRenderer":{"flexColumns":[{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"Kaede"}]}}},{"musicResponsiveListItemFlexColumnRenderer":{"text":{}}}],"navigationEndpoint":{"browseEndpoint":{"browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_ARTIST"}},"browseId":"FEmusic_library_privately_owned_artist_detaila_po_CJD2uP34k9CPgQESBWthZWRl"}}}},{"musicResponsiveListItemRenderer":{"flexColumns":[{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"Stardust in Blue"}]}}},{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"Album"},{"text":" • "},{"navigationEndpoint":{"browseEndpoint":{"browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_UNKNOWN"}},"browseId":"FEmusic_library_privately_owned_artist_detaila_po_CJD2uP34k9CPgQESBWthZWRl"}},"text":"Kaede"}]}}}],"navigationEndpoint":{"browseEndpoint":{"browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_ALBUM"}},"browseId":"FEmusic_library_privately_owned_release_detailb_po_CJD2uP34k9CPgQESEHN0YXJkdXN0IGluIGJsdWUaBWthZWRlIg1odHRwIHVwbG9hZGVy"}}}},{"musicResponsiveListItemRenderer":{"flexColumns":[{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"navigationEndpoint":{"watchEndpoint":{"videoId":"pQlgsq6hpWE","watchEndpointMusicSupportedConfigs":{"watchEndpointMusicConfig":{"musicVideoType":"MUSIC_VIDEO_TYPE_PRIVATELY_OWNED_TRACK"}}}},"text":"さよならはハート仕掛け - Farewell Is A Heartwork"}]}}},{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"navigationEndpoint":{"browseEndpoint":{"browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_UNKNOWN"}},"browseId":"FEmusic_library_privately_owned_artist_detaila_po_CJD2uP34k9CPgQESBWthZWRl"}},"text":"Kaede"},{"text":" • "},{"navigationEndpoint":{"browseEndpoint":{"browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_ALBUM"}},"browseId":"FEmusic_library_privately_owned_release_detailb_po_CJD2uP34k9CPgQESEHN0YXJkdXN0IGluIGJsdWUaBWthZWRlIg1odHRwIHVwbG9hZGVy"}},"text":"Stardust in Blue"}]}}}],"playlistItemData":{"videoId":"pQlgsq6hpWE"}}}]}}]}}}}]}}}"#;
    // Library results come in titled shelves; its artists lead to "MPLA" and their channel.
    const LIBRARY_SEARCH: &str = r#"{"contents":{"tabbedSearchResultsRenderer":{"tabs":[{"tabRenderer":{"title":"YT Music","selected":false,"endpoint":{"searchEndpoint":{"params":"agIYAQ%3D%3D","query":"kaede"}}}},{"tabRenderer":{"title":"Library","selected":true,"content":{"sectionListRenderer":{"contents":[{"musicShelfRenderer":{"title":{"runs":[{"text":"Artists"}]},"contents":[{"musicResponsiveListItemRenderer":{"flexColumns":[{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"Kaede"}]}}},{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"3.8K subscribers"}]}}}],"navigationEndpoint":{"browseEndpoint":{"browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_LIBRARY_ARTIST"}},"browseId":"MPLAUCMJIUyOLyBdal5XpZW3-96w"}}}}]}},{"musicShelfRenderer":{"title":{"runs":[{"text":"Albums"}]},"contents":[{"musicResponsiveListItemRenderer":{"flexColumns":[{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"Stardust in Blue"}]}}},{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"Album"},{"text":" • "},{"navigationEndpoint":{"browseEndpoint":{"browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_ARTIST"}},"browseId":"UCMJIUyOLyBdal5XpZW3-96w"}},"text":"Kaede"},{"text":" & "},{"text":"Lamp"},{"text":" • "},{"text":"2020"}]}}}],"navigationEndpoint":{"browseEndpoint":{"browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_ALBUM"}},"browseId":"MPREb_54uxDZG0XOp"}}}}]}},{"musicShelfRenderer":{"title":{"runs":[{"text":"Songs"}]},"contents":[{"musicResponsiveListItemRenderer":{"flexColumns":[{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"navigationEndpoint":{"watchEndpoint":{"videoId":"G0BLtNsvMP8","watchEndpointMusicSupportedConfigs":{"watchEndpointMusicConfig":{"musicVideoType":"MUSIC_VIDEO_TYPE_ATV"}}}},"text":"When We Were Young"}]}}},{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"navigationEndpoint":{"browseEndpoint":{"browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_ARTIST"}},"browseId":"UCMJIUyOLyBdal5XpZW3-96w"}},"text":"Kaede"},{"text":" & "},{"text":"Lamp"},{"text":" • "},{"navigationEndpoint":{"browseEndpoint":{"browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_ALBUM"}},"browseId":"MPREb_54uxDZG0XOp"}},"text":"Stardust in Blue"}]}}}],"playlistItemData":{"videoId":"G0BLtNsvMP8"}}}]}}]}}}},{"tabRenderer":{"title":"Uploads","selected":false,"endpoint":{"searchEndpoint":{"params":"agIYAw%3D%3D","query":"kaede"}}}}]}}}"#;

    #[test]
    fn uploads_search_finds_your_songs_albums_and_artists() {
        let r = parse_scoped(&serde_json::from_str(UPLOADS_SEARCH).unwrap());
        let cards: Vec<_> = r.cards.iter().map(|c| (c.title.as_str(), c.subtitle.as_str(), &c.target, c.round)).collect();
        assert_eq!(
            cards,
            [
                ("Kaede", "Artist", &Target::Artist("FEmusic_library_privately_owned_artist_detaila_po_CJD2uP34k9CPgQESBWthZWRl".into()), true),
                (
                    "Stardust in Blue",
                    "Album • Kaede",
                    &Target::Album("FEmusic_library_privately_owned_release_detailb_po_CJD2uP34k9CPgQESEHN0YXJkdXN0IGluIGJsdWUaBWthZWRlIg1odHRwIHVwbG9hZGVy".into()),
                    false
                ),
            ]
        );
        // The album's artist leads to their uploads page, whatever its page type.
        assert_eq!(r.cards[1].links, [Link { name: "Kaede".into(), id: Some("FEmusic_library_privately_owned_artist_detaila_po_CJD2uP34k9CPgQESBWthZWRl".into()) }]);
        // Library › Uploads' own tabs aren't pages to open.
        assert_eq!(target(&json!({"browseEndpoint": {"browseId": "FEmusic_library_privately_owned_releases"}})), None);
        assert_eq!(r.tracks.len(), 1);
        let t = &r.tracks[0];
        assert_eq!((t.id.as_str(), t.title.as_str()), ("pQlgsq6hpWE", "さよならはハート仕掛け - Farewell Is A Heartwork"));
        assert_eq!(names(t), ["Kaede"]);
        assert_eq!(t.artists[0].id.as_deref(), Some("FEmusic_library_privately_owned_artist_detaila_po_CJD2uP34k9CPgQESBWthZWRl"));
        let album = "FEmusic_library_privately_owned_release_detailb_po_CJD2uP34k9CPgQESEHN0YXJkdXN0IGluIGJsdWUaBWthZWRlIg1odHRwIHVwbG9hZGVy";
        assert_eq!(t.album.as_ref().map(|a| (a.name.as_str(), a.id.as_deref())), Some(("Stardust in Blue", Some(album))));
        assert!(!t.video);
    }

    #[test]
    fn library_search_opens_its_artists_and_albums() {
        let r = parse_scoped(&serde_json::from_str(LIBRARY_SEARCH).unwrap());
        let cards: Vec<_> = r.cards.iter().map(|c| (c.title.as_str(), c.subtitle.as_str(), &c.target)).collect();
        assert_eq!(
            cards,
            [
                ("Kaede", "3.8K subscribers", &Target::Artist("UCMJIUyOLyBdal5XpZW3-96w".into())),
                ("Stardust in Blue", "Album • Kaede & Lamp • 2020", &Target::Album("MPREb_54uxDZG0XOp".into())),
            ]
        );
        assert_eq!(r.tracks.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), ["When We Were Young"]);
        assert_eq!(r.tracks[0].album.as_ref().and_then(|a| a.id.as_deref()), Some("MPREb_54uxDZG0XOp"));
    }

    #[test]
    fn a_search_without_a_picked_tab_finds_nothing() {
        let v = json!({"contents": {"tabbedSearchResultsRenderer": {"tabs": [{"tabRenderer": {"title": "Library"}}]}}});
        let r = parse_scoped(&v);
        assert!(r.tracks.is_empty() && r.cards.is_empty());
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
            swap_songs(&mut a, &videos, &songs, None);
        }
        a.tracks.into_iter().map(|t| t.id).collect()
    }

    #[test]
    fn album_music_videos_become_the_albums_songs() {
        let (mut a, videos) = parse_album(&album_page(&RICK_ROWS), "MPREb_x").unwrap();
        assert_eq!(videos, [0, 1]);
        let songs = album_songs(&vr_album("Album - Whenever You Need Somebody", &RICK_SONGS)).unwrap();
        assert_eq!(swap_songs(&mut a, &videos, &songs, None), 2);
        assert_eq!(a.tracks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["lYBUbBu4W08", "raBobo3GZYA", "d0t04zd918U"]);
        // The song's own length; the album's title.
        assert_eq!((a.tracks[1].title.as_str(), a.tracks[1].duration), ("Whenever You Need Somebody", Some(234)));
    }

    #[test]
    fn album_in_another_language_is_matched_by_its_english_titles() {
        let rows = [("34Na4j8AVgA", "スターボーイ（feat. ダフト・パンク）", "MUSIC_VIDEO_TYPE_OMV"), ("diW6jXhLE0E", "パーティー・モンスター", "MUSIC_VIDEO_TYPE_OMV")];
        let songs = album_songs(&vr_album("Album - Starboy", &[("aaaaaaaaaaa", "Starboy", "3:50"), ("bbbbbbbbbbb", "Party Monster", "4:09")])).unwrap();
        let (mut a, videos) = parse_album(&album_page(&rows), "MPREb_x").unwrap();
        assert_eq!(swap_songs(&mut a, &videos, &songs, None), 0);
        let english = ["Starboy (feat. Daft Punk)".to_owned(), "Party Monster".to_owned()];
        assert_eq!(swap_songs(&mut a, &videos, &songs, Some(&english)), 2);
        // The page's own titles stay.
        assert_eq!((a.tracks[0].id.as_str(), a.tracks[0].title.as_str()), ("aaaaaaaaaaa", "スターボーイ（feat. ダフト・パンク）"));
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

    #[test]
    fn a_guest_credited_in_another_language_is_harmless() {
        assert!(qualifiers_agree("Wake Me Up (avec Justice)", "Wake Me Up"));
        assert!(qualifiers_agree("Tití Me Preguntó (con Bad Bunny)", "Tití Me Preguntó"));
        assert!(!qualifiers_agree("Wake Me Up (Remix)", "Wake Me Up"));
        assert!(!qualifiers_agree("Song (Part 2)", "Song") && !qualifiers_agree("Song (Part 1)", "Song (Part 2)"));
    }

    /// Looks up the plays of a real song an album page lists without them. Run alone:
    /// `cargo test --bin ytm-rs -- --ignored ytm::tests::song_plays_live --exact --nocapture`
    #[tokio::test]
    #[ignore]
    async fn song_plays_live() {
        let api = crate::ytmapi::Api::new(&std::env::temp_dir().join("ytm-rs-song-plays-test"));
        let artist = |name: &str| vec![Link { name: name.into(), id: None }];
        let song = |id: &str, title: &str, by: &str| Track { id: id.into(), title: title.into(), artists: artist(by), ..Default::default() };
        for t in [song("L3XPdkm2F9Q", "独り言 - Hitori Goto", "Asagaya Romantics")] {
            let plays = song_plays(&api, &t).await.unwrap();
            println!("{} {}: {plays:?}", t.id, t.title);
            assert!(plays.is_some_and(|n| n > 0), "{} should have plays", t.title);
        }
    }

    /// What a song's id, an artist's handle and an album's list stand for, signed out. Run alone:
    /// `cargo test --bin ytm-rs -- --ignored ytm::tests::links_live --exact`
    #[tokio::test]
    #[ignore]
    async fn links_live() {
        let api = crate::ytmapi::Api::new(&std::env::temp_dir().join("ytm-rs-links-test"));
        assert_eq!(super::song(&api, "lYBUbBu4W08", false).await.unwrap().unwrap().title, "Never Gonna Give You Up");
        assert_eq!(super::song(&api, "zzzzzzzzzzz", false).await.unwrap(), None);
        assert_eq!(resolve(&api, "https://music.youtube.com/@RickAstleyYT").await.unwrap(), Some(Target::Artist("UCuAXFkgsw1L7xaCfnd5JJOw".into())));
        let album = "https://music.youtube.com/playlist?list=OLAK5uy_nmDUsWOMoEcz0SsVqUwir0oxu-k1oUyXE";
        assert_eq!(resolve(&api, album).await.unwrap(), Some(Target::Album("MPREb_dcYZhAh5urI".into())));
        assert_eq!(resolve(&api, "https://music.youtube.com/@doesnotexist123456789xyz").await.unwrap(), None);
    }

    /// A song's Lyrics tab, signed out. Run alone:
    /// `cargo test --bin ytm-rs -- --ignored ytm::tests::lyrics_live --exact`
    #[tokio::test]
    #[ignore]
    async fn lyrics_live() {
        let api = crate::ytmapi::Api::new(&std::env::temp_dir().join("ytm-rs-lyrics-test"));
        let text = lyrics(&api, "fJ9rUzIMcZQ").await.unwrap().expect("Bohemian Rhapsody has lyrics");
        assert!(text.starts_with("Is this the real life?"), "{text}");
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
    fn subscribed_artist_pages() {
        let artist: Value = serde_json::from_str(ARTIST_ROW).unwrap();
        let old = json!({"continuationContents": {"musicShelfContinuation": {"contents": [artist], "continuations": [{"nextContinuationData": {"continuation": "P2"}}]}}});
        let (cards, next) = shelf_continuation(&old);
        assert!(matches!(&cards[..], [Card { target: Target::Artist(id), .. }] if id == "UClYV6hHlupm_S_ObS1W-DYw"));
        assert_eq!(next.as_deref(), Some("P2"));

        let new = json!({"onResponseReceivedActions": [{"appendContinuationItemsAction": {"continuationItems": [
            artist,
            {"continuationItemRenderer": {"continuationEndpoint": {"continuationCommand": {"token": "P3"}}}}
        ]}}]});
        let (cards, next) = shelf_continuation(&new);
        assert_eq!(cards.len(), 1);
        assert_eq!(next.as_deref(), Some("P3"));

        assert_eq!(shelf_continuation(&json!({})), (Vec::new(), None));
    }

    #[test]
    fn artist_subscription() {
        let page = |button: Value| json!({"header": {"musicImmersiveHeaderRenderer": {"title": {"runs": [{"text": "Alex G"}]}, "subscriptionButton": button}}});
        let followed = parse_artist(&page(json!({"subscribeButtonRenderer": {"subscribed": true, "channelId": "UCx"}})), "UCa").unwrap();
        assert!(followed.subscribed);
        assert_eq!(followed.channel_id.as_deref(), Some("UCx"));
        // No button (signed out, or not a channel one can subscribe to): no pill.
        let none = parse_artist(&page(Value::Null), "UCa").unwrap();
        assert!(!none.subscribed);
        assert_eq!(none.channel_id, None);

        // Asked signed in, answered signed out: whether it's followed isn't known.
        let signed_out = json!({"responseContext": {"serviceTrackingParams": [{"params": [{"key": "logged_in", "value": "0"}]}]}});
        assert!(answered_signed_out(false, &signed_out));
        assert!(answered_signed_out(true, &json!({})));
        assert!(!answered_signed_out(false, &json!({})));
        let mut p = followed;
        unknown_subscription(&mut p);
        assert!(!p.subscribed);
        assert_eq!(p.channel_id, None);
    }

    #[test]
    fn artist_songs_from_your_library() {
        let song = |id: &str, title: &str| json!({"musicTwoRowItemRenderer": {
            "title": {"runs": [{"text": title}]},
            "subtitle": {"runs": [{"text": "Song"}, {"text": " • "}, {"text": "Ashe", "navigationEndpoint": {"browseEndpoint": {"browseId": "UCa",
                "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {"pageType": "MUSIC_PAGE_TYPE_ARTIST"}}}}}]},
            "navigationEndpoint": {"watchEndpoint": {"videoId": id, "watchEndpointMusicSupportedConfigs": {"watchEndpointMusicConfig": {"musicVideoType": "MUSIC_VIDEO_TYPE_ATV"}}}}
        }});
        let shelf = |title: Value, items: Vec<Value>| json!({"musicCarouselShelfRenderer": {"header": {"musicCarouselShelfBasicHeaderRenderer": {"title": title}}, "contents": items}});
        // Its title leads to the account's library page for the artist, in any language.
        let library = json!({"runs": [{"text": "Aus deiner Mediathek", "navigationEndpoint": {"browseEndpoint": {"browseId": "MPLAUCa", "params": "ggMCCAI%3D"}}}]});
        let v = json!({
            "header": {"musicImmersiveHeaderRenderer": {"title": {"runs": [{"text": "Ashe"}]}}},
            "contents": {"singleColumnBrowseResultsRenderer": {"tabs": [{"tabRenderer": {"content": {"sectionListRenderer": {"contents": [
                shelf(json!({"runs": [{"text": "Videos"}]}), vec![song("vvvvvvvvvvv", "A video")]),
                shelf(library, vec![song("aaaaaaaaaaa", "Moral of the Story"), song("bbbbbbbbbbb", "Till Forever Falls Apart")]),
            ]}}}}]}}
        });
        let page = parse_artist(&v, "UCa").unwrap();
        let ids: Vec<&str> = page.library.iter().filter_map(|c| if let Target::Song(t) = &c.target { Some(t.id.as_str()) } else { None }).collect();
        assert_eq!(ids, ["aaaaaaaaaaa", "bbbbbbbbbbb"]);
        assert_eq!(page.library[0].title, "Moral of the Story");
        assert_eq!(page.library[0].links.first().and_then(|l| l.id.as_deref()), Some("UCa"));
        // Video shelves land in `videos`, leaving library/albums/playlists/similar separate.
        assert_eq!(page.videos.len(), 1);
        assert_eq!(page.videos[0].title, "A video");
        assert!(page.albums.is_empty() && page.playlists.is_empty() && page.similar.is_empty());
        // Signed out there's no such shelf.
        let mut out = v.clone();
        out["contents"]["singleColumnBrowseResultsRenderer"]["tabs"][0]["tabRenderer"]["content"]["sectionListRenderer"]["contents"].as_array_mut().unwrap().pop();
        assert!(parse_artist(&out, "UCa").unwrap().library.is_empty());
    }

    #[test]
    fn counts_and_lengths() {
        assert_eq!(count("3.6B plays"), Some(3_600_000_000));
        assert_eq!(count("40.1M"), Some(40_100_000));
        assert_eq!(count("1,234 views"), Some(1_234));
        assert_eq!(parse_duration("1:02:03"), Some(3723));
        assert!(!looks_like_duration("Blinding Lights"));
    }

    // Settings › Song info: what's told apart without the English words.

    #[test]
    fn counts_in_other_languages() {
        assert_eq!(count("再生回数 966万 回"), Some(9_660_000));
        assert_eq!(count("月間視聴者数: 1.56億 人"), Some(156_000_000));
        assert_eq!(count("1968回視聴"), Some(1_968));
        assert_eq!(count("조회수 3.2만회"), Some(32_000));
        assert_eq!(count("1,2 Mio. Aufrufe"), Some(1_200_000));
        assert_eq!(count("12.345 Aufrufe"), Some(12_345));
        assert_eq!(count("3,4 Md lectures"), Some(3_400_000_000));
        assert_eq!(count("Tocou 13 mi vezes"), Some(13_000_000));
        assert_eq!(count("3,82 тыс."), Some(3_820));
        assert_eq!(count("2,5 mil M reproducciones"), Some(2_500_000_000));
        assert_eq!(count("12 mil reproducciones"), Some(12_000));
        assert_eq!(count("103 rb subscriber"), Some(103_000));
        assert_eq!(count("7,22\u{a0}jt"), Some(7_220_000));
        assert_eq!(count("1,2 jt kali ditonton"), Some(1_200_000));
        assert_eq!(count("1,2\u{a0}M pemutaran"), Some(1_200_000_000));
        assert_eq!(count("1,8\u{a0}M x ditonton"), Some(1_800_000_000));
        assert_eq!(count("2,5 mlr kali ditonton"), Some(2_500_000_000));
        // A fraction with no unit is no count.
        assert_eq!(count("1.5"), None);
    }

    #[test]
    fn years_and_song_counts_in_other_languages() {
        assert!(is_year("2020") && is_year("2024年") && is_year("2024년"));
        assert!(!is_year("20245") && !is_year("2020s") && !is_year("3:05") && !is_year("2024!"));
        let subtitle = |parts: &[&str]| json!({"runs": parts.iter().map(|t| json!({"text": t})).collect::<Vec<_>>()});
        assert_eq!(song_count(&subtitle(&["39 曲", " • ", "2 時間 28 分"])), Some(39));
        assert_eq!(song_count(&subtitle(&["1.2K views", " • ", "39 songs", " • ", "2+ hours"])), Some(39));
        assert_eq!(song_count(&subtitle(&["12 k vues", " • ", "1 234 titres", " • ", "3 heures"])), Some(1_234));
        assert_eq!(song_count(&subtitle(&["عدد الأغاني: 125", " • ", "أكثر من 6 ساعة"])), Some(125));
    }

    #[test]
    fn artists_joined_by_a_word_in_another_language() {
        let artist = |name: &str| json!({"text": name, "navigationEndpoint": {"browseEndpoint": {"browseId": format!("UC{name}"),
            "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {"pageType": "MUSIC_PAGE_TYPE_ARTIST"}}}}});
        let names = |v: Vec<Link>| v.into_iter().map(|l| l.name).collect::<Vec<_>>();
        for joiner in [" und ", " et ", " 및 ", "、", " & "] {
            let strap = json!({"runs": [artist("A"), {"text": joiner}, artist("B")]});
            assert_eq!(names(links(&strap)), ["A", "B"], "{joiner}");
        }
        // An unlinked artist is still one.
        assert_eq!(names(links(&json!({"runs": [artist("A"), {"text": ", "}, {"text": "B"}, {"text": " & "}, artist("C")]}))), ["A", "B", "C"]);
    }

    #[test]
    fn podcast_lengths_in_other_languages() {
        assert_eq!(spoken_duration("7 分 49 秒"), Some(469));
        assert_eq!(spoken_duration("1 Std. 5 Min."), Some(3_900));
        assert_eq!(spoken_duration("2 시간"), Some(7_200));
    }

    #[test]
    fn podcasts_chip_is_told_by_its_params() {
        let chip = |params: &str| json!({"navigationEndpoint": {"browseEndpoint": {"params": params}}});
        assert!(podcasts_chip(&chip("ggNCSgQIDBADSgQIBBABSgQICRAB")));
        assert!(!podcasts_chip(&chip("ggNCSgQIDBABSgQIBBADSgQICRAB")));
        assert!(!podcasts_chip(&chip("")));
        // In English, by its title too.
        assert!(podcasts_chip(&json!({"text": {"runs": [{"text": "Podcasts"}]}})));
    }

    #[test]
    fn japanese_song_row_has_its_plays() {
        let r = json!({
            "flexColumns": [
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "咲き誇れ", "navigationEndpoint": {"watchEndpoint": {"videoId": "aaaaaaaaaaa",
                    "watchEndpointMusicSupportedConfigs": {"watchEndpointMusicConfig": {"musicVideoType": "MUSIC_VIDEO_TYPE_ATV"}}}}}]}}},
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "LE SSERAFIM", "navigationEndpoint": {"browseEndpoint": {"browseId": "UCx",
                    "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {"pageType": "MUSIC_PAGE_TYPE_ARTIST"}}}}}, {"text": "、"}, {"text": "YOASOBI", "navigationEndpoint": {"browseEndpoint": {"browseId": "UCy",
                    "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {"pageType": "MUSIC_PAGE_TYPE_ARTIST"}}}}}]}}},
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "再生回数 966万 回"}]}}}
            ]
        });
        let t = list_item(&r).unwrap();
        assert_eq!(names(&t), ["LE SSERAFIM", "YOASOBI"]);
        assert_eq!(t.plays, Some(9_660_000));
    }

    #[test]
    fn japanese_artist_search_row_drops_its_kind() {
        let r = row(&ARTIST_ROW.replace("\"Artist\"", "\"アーティスト\"").replace("239M monthly audience", "月間視聴者数: 4260万 人"));
        assert_eq!(list_card(&r).unwrap().subtitle, "月間視聴者数: 4260万 人");
    }

    #[test]
    fn listen_again_is_told_by_the_accounts_channel() {
        let card = json!({"musicTwoRowItemRenderer": {"title": {"runs": [{"text": "Mellow"}]}, "navigationEndpoint": {"browseEndpoint": {"browseId": "VLRDCLAK5uy_x",
            "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {"pageType": "MUSIC_PAGE_TYPE_PLAYLIST"}}}}}});
        let shelf_with = |on_tap: Value| json!({"musicCarouselShelfRenderer": {"header": {"musicCarouselShelfBasicHeaderRenderer": {
            "title": {"runs": [{"text": "もう一度聴く"}]}, "thumbnail": {"musicThumbnailRenderer": {"onTap": on_tap}}}}, "contents": [card.clone()]}});
        let channel = json!({"browseEndpoint": {"browseId": "UCme", "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {"pageType": "MUSIC_PAGE_TYPE_USER_CHANNEL"}}}});
        assert!(shelf(&shelf_with(channel)).unwrap().own);
        assert!(!shelf(&shelf_with(Value::Null)).unwrap().own);
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
            duration: Some(secs),
            ..Default::default()
        }
    }

    fn video(title: &str, artists: &[&str], secs: u32) -> Track {
        Track { music_video: true, ..song(title, artists, secs) }
    }

    #[test]
    fn lyrics_are_looked_up_by_the_bare_title_and_first_artist() {
        let v = video("Dua Lipa - Levitating (feat. DaBaby) (Official Music Video)", &["Dua Lipa", "DaBaby"], 203);
        assert_eq!(lyrics_names(&v), ("Levitating".to_owned(), "Dua Lipa".to_owned()));
        // A song's own qualifiers stay: they're part of its name on LRCLib too.
        assert_eq!(lyrics_names(&song("Together Forever (2022 Remaster)", &["Rick Astley"], 200)).0, "Together Forever (2022 Remaster)");
    }

    #[test]
    fn a_recognized_song_is_found_by_a_sure_match_only() {
        // What Shazam answered for downloaded songs (November 2026).
        assert!(heard_as("Glue Song", "beabadoobee", &song("Glue Song", &["beabadoobee"], 135)));
        assert!(heard_as("Hit #2", "125 Rue Montmartre", &song("Hit #2", &["125 Rue Montmartre"], 200)));
        assert!(heard_as("This Is What You Came For (feat. Rihanna)", "Calvin Harris", &song("This Is What You Came For", &["Calvin Harris", "Rihanna"], 222)));
        assert!(heard_as("We Found Love", "Rihanna Feat. Calvin Harris", &song("We Found Love (feat. Calvin Harris)", &["Rihanna"], 215)));
        // An artist named with a separator, credited alone.
        assert!(heard_as("The Boxer", "Simon & Garfunkel", &song("The Boxer", &["Simon & Garfunkel"], 308)));
        for (hit, why) in [
            (song("Take On Me (Live)", &["a-ha"], 240), "a live recording"),
            (song("Take On Me", &["Weezer"], 244), "a cover"),
            (song("Take On Me (Acoustic Version)", &["a-ha"], 244), "an acoustic version"),
            (video("Take On Me", &["a-ha"], 244), "a music video"),
            (song("Take", &["a-ha"], 244), "a shorter title"),
            (song("Take On Me", &["Weezer", "a-ha"], 244), "another lead artist's"),
        ] {
            assert!(!heard_as("Take On Me", "a-ha", &hit), "{why} isn't the song");
        }
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
        // Both ways round, for taking a like away.
        let keep = HashSet::new();
        assert_eq!(parse_other_half(&v, "dQw4w9WgXcQ", &keep).as_deref(), Some("lYBUbBu4W08"));
        assert_eq!(parse_other_half(&v, "raBobo3GZYA", &keep).as_deref(), Some("BeyEGebJ1l4"));
        assert_eq!(parse_other_half(&v, "not-in-the-queue", &keep), None);
        assert_eq!(parse_other_half(&json!({}), "dQw4w9WgXcQ", &keep), None);
        // Unliking the remaining uploads must preserve a new rating on either version.
        let keep = HashSet::from(["lYBUbBu4W08".to_owned()]);
        assert_eq!(parse_other_half(&v, "dQw4w9WgXcQ", &keep), None);
        assert_eq!(parse_other_half(&v, "raBobo3GZYA", &keep).as_deref(), Some("BeyEGebJ1l4"));
        let keep = HashSet::from(["BeyEGebJ1l4".to_owned()]);
        assert_eq!(parse_other_half(&v, "raBobo3GZYA", &keep), None);
        let found = parse_paired_song(&v, "dQw4w9WgXcQ").unwrap();
        assert_eq!((found.id.as_str(), found.duration, found.music_video), ("lYBUbBu4W08", Some(214), false));
        // A song's counterpart is its video: no song version of a song.
        assert_eq!(parse_paired_song(&v, "raBobo3GZYA"), None);
        // Not in the queue (or a free account's queue, which has no pairs).
        assert_eq!(parse_paired_song(&v, "BeyEGebJ1l4"), None);
        // A song's music video, lined up with it: the song's start is 0.927 s into the video.
        let found = parse_paired_video(&v, "raBobo3GZYA").unwrap();
        assert_eq!((found.video.id.as_str(), found.video.music_video, found.offset_ms), ("BeyEGebJ1l4", true, 927));
        // A music video's counterpart is its song: no music video of a music video.
        assert_eq!(parse_paired_video(&v, "dQw4w9WgXcQ"), None);
        assert_eq!(parse_paired_video(&v, "not-in-the-queue"), None);
        // Without the map, the two start together.
        let mut bare = v.clone();
        let pair = &mut bare["contents"]["singleColumnMusicWatchNextResultsRenderer"]["tabbedRenderer"]["watchNextTabbedResultsRenderer"]["tabs"][0]["tabRenderer"]["content"]
            ["musicQueueRenderer"]["content"]["playlistPanelRenderer"]["contents"][0]["playlistPanelVideoWrapperRenderer"]["counterpart"][0];
        pair.as_object_mut().unwrap().remove("segmentMap");
        assert_eq!(parse_paired_video(&bare, "raBobo3GZYA").map(|m| m.offset_ms), Some(0));
    }

    #[test]
    fn the_music_video_of_a_song_is_found_by_a_sure_match_only() {
        let song_of = song("Take On Me", &["a-ha"], 226);
        let official = video("a-ha - Take On Me (Official Video) [4K]", &["a-ha"], 244);
        let not_official = Track { music_video: false, video: true, ..official.clone() };
        assert_eq!(sure_video(&song_of, vec![not_official.clone(), official.clone()]).map(|v| v.id), Some(official.id.clone()));
        // A fan's upload of the same song isn't its music video.
        assert_eq!(sure_video(&song_of, vec![not_official]), None);
        // Nor is a live video, another song's, or one past the first few hits.
        assert_eq!(sure_video(&song_of, vec![video("Take On Me (Live From MTV Unplugged)", &["a-ha"], 240)]), None);
        assert_eq!(sure_video(&song_of, vec![video("The Sun Always Shines on T.V.", &["a-ha"], 300)]), None);
        let others = (0..5).map(|i| video(&format!("Other {i}"), &["a-ha"], 244));
        assert_eq!(sure_video(&song_of, others.chain([official]).collect()), None);
        // Everyone on the video is on the song too.
        assert_eq!(sure_video(&song("Duet", &["Singer A"], 200), vec![video("Duet", &["Singer A", "Singer B"], 200)]), None);
        assert!(sure_video(&song("Duet", &["Singer A", "Singer B"], 200), vec![video("Duet", &["Singer A"], 200)]).is_some());
    }

    #[test]
    fn a_radio_is_its_songs_or_its_videos_when_it_has_only_those() {
        let t = |id: &str| Track { id: id.into(), title: id.into(), artists: Vec::new(), ..Default::default() };
        let ids = |items: &[(Track, bool)]| radio_tracks(items).map(|t| t.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&[(t("a"), false), (t("v"), true), (t("b"), false)]), ["a", "b"]);
        // A music video's radio: music videos only, kept rather than nothing.
        assert_eq!(ids(&[(t("v1"), true), (t("v2"), true)]), ["v1", "v2"]);
        assert!(ids(&[]).is_empty());
    }

    #[test]
    fn a_move_names_the_entry_after_it_except_at_the_end() {
        assert_eq!(move_action("a", Some("b")), json!({"action": "ACTION_MOVE_VIDEO_BEFORE", "setVideoId": "a", "movedSetVideoIdSuccessor": "b"}));
        assert_eq!(move_action("a", None), json!({"action": "ACTION_MOVE_VIDEO_BEFORE", "setVideoId": "a"}));
    }

    #[test]
    fn list_rows_say_whether_they_are_music_videos() {
        let row = |kind: &str| list_item(&album_row("x", "Song", kind)["musicResponsiveListItemRenderer"]).unwrap().music_video;
        assert!(row("MUSIC_VIDEO_TYPE_OMV"));
        assert!(!row("MUSIC_VIDEO_TYPE_ATV"));
        // User uploads and covers have no song version to look for.
        assert!(!row("MUSIC_VIDEO_TYPE_UGC"));
        // They're still videos to show.
        let video = |kind: &str| list_item(&album_row("x", "Song", kind)["musicResponsiveListItemRenderer"]).unwrap().video;
        assert!(video("MUSIC_VIDEO_TYPE_OMV") && video("MUSIC_VIDEO_TYPE_UGC") && !video("MUSIC_VIDEO_TYPE_ATV"));
    }

    #[test]
    fn explicit_songs_are_marked_in_rows_radio_and_cards() {
        let badge = |icon: &str| json!([{"musicInlineBadgeRenderer": {"icon": {"iconType": icon}}}]);
        let mut r = album_row("x", "Song", "MUSIC_VIDEO_TYPE_ATV")["musicResponsiveListItemRenderer"].clone();
        assert!(!list_item(&r).unwrap().explicit);
        r["badges"] = badge("MUSIC_EXPLICIT_BADGE");
        assert!(list_item(&r).unwrap().explicit);
        // Another badge isn't it.
        r["badges"] = badge("OFFLINE_PIN");
        assert!(!list_item(&r).unwrap().explicit);
        let mut p = serde_json::from_str::<Value>(PANEL_ITEM).unwrap()["playlistPanelVideoRenderer"].clone();
        assert!(!panel_item(&p).unwrap().explicit);
        p["badges"] = badge("MUSIC_EXPLICIT_BADGE");
        assert!(panel_item(&p).unwrap().explicit);
        let card = json!({"title": {"runs": [{"text": "Song"}]}, "navigationEndpoint": {"watchEndpoint": {"videoId": "x"}}, "subtitleBadges": badge("MUSIC_EXPLICIT_BADGE")});
        assert!(matches!(two_row(&card).unwrap().target, Target::Song(t) if t.explicit));
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
            let found = searched_song(&api, &v, false).await.unwrap();
            println!("{} -> {:?}", v.title, found.as_ref().map(|s| (&s.id, &s.title, s.artist_line(), s.duration)));
        }
    }

    /// A signed-out search for the music videos of a few songs. Run alone:
    /// `cargo test --bin ytm-rs -- --ignored ytm::tests::searched_video_live --exact --nocapture`
    #[tokio::test]
    #[ignore]
    async fn searched_video_live() {
        let api = crate::ytmapi::Api::new(&std::env::temp_dir().join("ytm-rs-album-songs-test"));
        for s in [
            song("Never Gonna Give You Up", &["Rick Astley"], 214),
            song("Take On Me", &["a-ha"], 226),
            song("bad guy", &["Billie Eilish"], 195),
            song("Bohemian Rhapsody", &["Queen"], 355),
        ] {
            let found = searched_video(&api, &s).await.unwrap();
            println!("{} -> {:?}", s.title, found.as_ref().map(|v| (&v.id, &v.title, v.artist_line(), v.duration)));
        }
    }
}

#[cfg(test)]
mod podcast_tests {
    use super::*;

    fn show_run(name: &str, id: &str) -> Value {
        json!({"text": name, "navigationEndpoint": {"browseEndpoint": {"browseId": id, "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {"pageType": "MUSIC_PAGE_TYPE_PODCAST_SHOW_DETAIL_PAGE"}}}}})
    }

    #[test]
    fn episodes_read_like_songs_by_their_show() {
        let r = json!({
            "title": {"runs": [{"text": "The Good Show"}]},
            "secondTitle": {"runs": [show_run("Radiolab", "MPSPPL1")]},
            "onTap": {"watchEndpoint": {"videoId": "gjhnOKvSeXA"}},
            "playbackProgress": {"musicPlaybackProgressRenderer": {"durationText": {"runs": [{"text": " • "}, {"text": "1 hr 3 min"}]}}},
            "thumbnail": {"musicThumbnailRenderer": {"thumbnail": {"thumbnails": [{"url": "u", "width": 120, "height": 120}]}}},
        });
        let t = episode(&r).unwrap();
        assert_eq!((t.id.as_str(), t.title.as_str(), t.duration), ("gjhnOKvSeXA", "The Good Show", Some(3780)));
        assert_eq!(t.artists, [Link { name: "Radiolab".into(), id: Some("MPSPPL1".into()) }]);
        assert_eq!(t.thumbs.len(), 1);
        // Nothing to play: not an episode.
        assert!(episode(&json!({"title": {"runs": [{"text": "x"}]}})).is_none());
    }

    #[test]
    fn a_search_episode_is_by_its_show_not_its_date() {
        let col = |runs: Value| json!({"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": runs}}});
        let r = json!({
            "playlistItemData": {"videoId": "xjZ31IKfu4Q"},
            "flexColumns": [col(json!([{"text": "The Buried Bodies Case"}])), col(json!([{"text": "2d ago"}, {"text": " • "}, show_run("Radiolab", "MPSPPL1")]))],
        });
        let t = episode_row(&r).unwrap();
        assert_eq!(t.title, "The Buried Bodies Case");
        assert_eq!(t.artists, [Link { name: "Radiolab".into(), id: Some("MPSPPL1".into()) }]);
    }

    #[test]
    fn a_search_episode_dated_has_no_plays() {
        let col = |runs: Value| json!({"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": runs}}});
        let r = json!({
            "playlistItemData": {"videoId": "xjZ31IKfu4Q"},
            "flexColumns": [col(json!([{"text": "The Buried Bodies Case"}])), col(json!([{"text": "Episode"}, {"text": " • "}, {"text": "Dec 28, 2025"}, {"text": " • "}, show_run("Radiolab", "MPSPPL1")]))],
        });
        assert_eq!(list_item(&r).unwrap().plays, None);
        let t = episode_row(&r).unwrap();
        assert_eq!((t.plays, t.artists.len()), (None, 1));
        // A date in any language holds more than one number; a count holds one.
        for date in ["Dec 28, 2025", "2025/12/28", "28.12.2025", "2025年12月28日", "28 déc. 2025"] {
            assert_eq!(lone_count(date), None, "{date}");
        }
        assert_eq!(lone_count("再生回数 966万 回"), Some(9_660_000));
        assert_eq!(lone_count("1,2 Mio. Aufrufe"), Some(1_200_000));
    }

    #[test]
    fn spoken_lengths() {
        assert_eq!(spoken_duration("3 hr 36 min"), Some(12_960));
        assert_eq!(spoken_duration("49 min"), Some(2_940));
        assert_eq!(spoken_duration("1 hr"), Some(3_600));
        assert_eq!(spoken_duration("45 sec"), Some(45));
        assert_eq!(spoken_duration(" • "), None);
        assert_eq!(spoken_duration("Aug 19"), None);
    }

    #[test]
    fn a_show_is_saved_and_shared_by_its_playlist_id() {
        assert_eq!(list_id("MPSPPLLHAUHF-RPhlA7"), "PLLHAUHF-RPhlA7");
        assert_eq!(list_id("PLabc"), "PLabc");
        assert_eq!(list_id("LM"), "LM");
    }

    /// Signed out: Home's Podcasts feed, a search for shows and episodes, and a show's
    /// page with its second page. Alone (it switches podcasts on for the process):
    /// `cargo test -- --ignored podcasts_live --nocapture`
    #[tokio::test]
    #[ignore]
    async fn podcasts_live() {
        set_podcasts(true);
        let api = crate::ytmapi::Api::new(&std::env::temp_dir().join("ytm-rs-podcasts-test"));
        let page = home(&api, None, false).await.unwrap();
        let chip = page.chips.iter().find(|c| c.title == "Podcasts").expect("no Podcasts chip on Home");
        let feed = home(&api, chip.params.as_deref(), false).await.unwrap();
        let episodes: usize = feed.shelves.iter().map(|s| if let ShelfKind::Songs { tracks, .. } = &s.kind { tracks.len() } else { 0 }).sum();
        println!("podcasts feed: {} shelves, {episodes} episodes", feed.shelves.len());
        assert!(episodes > 0, "no episodes in Home's Podcasts feed");
        let shows = search_cards(&api, "radiolab", Filter::Podcasts, false).await.unwrap();
        let eps = search_episodes(&api, "radiolab", false).await.unwrap();
        println!("search: {} shows, {} episodes; first {:?} / {:?} by {:?}", shows.len(), eps.len(), shows.first().map(|c| &c.title), eps.first().map(|t| &t.title), eps.first().map(Track::artist_line));
        assert!(!eps.is_empty() && eps.iter().all(|t| t.artists.iter().all(|a| a.id.as_deref().is_some_and(|id| id.starts_with(SHOW_PREFIX)))));
        let Some(Target::Playlist(id)) = shows.first().map(|c| c.target.clone()) else { panic!("no show in the search") };
        assert!(id.starts_with(SHOW_PREFIX));
        let show = playlist(&api, &id, false, Some(150)).await.unwrap();
        println!("show {:?} by {:?}: {} episodes, first {:?} {:?} s", show.title, show.subtitle, show.tracks.len(), show.tracks[0].title, show.tracks[0].duration);
        assert!(show.tracks.len() > 20, "the show's second page didn't load");
        assert!(show.tracks.iter().all(|t| t.duration.is_some()));
        set_podcasts(false);
        assert!(home(&api, None, false).await.unwrap().chips.iter().all(|c| c.title != "Podcasts"));
    }
}
