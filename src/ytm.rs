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
    parse_album(&v, id).ok_or_else(|| anyhow::anyhow!("YouTube Music returned an empty album"))
}

fn parse_album(v: &Value, id: &str) -> Option<Collection> {
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
    let tracks = rows
        .filter_map(|i| list_item(&i["musicResponsiveListItemRenderer"]))
        .map(|mut t| {
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
    Some(Collection {
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
    })
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
    let (mut tracks, mut token) = panel_page(panel);
    while tracks.len() < limit {
        let Some(t) = token.take() else { break };
        let body = json!({ "continuation": t, "enablePersistentPlaylistPanel": true, "isAudioOnly": true });
        let v = api.call("next", body, auth).await?;
        let (more, next) = panel_page(&v["continuationContents"]["playlistPanelContinuation"]);
        if more.is_empty() {
            break;
        }
        tracks.extend(more);
        token = next;
    }
    Ok(tracks)
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

fn panel_page(panel: &Value) -> (Vec<Track>, Option<String>) {
    let tracks = arr(&panel["contents"])
        .iter()
        .filter_map(|i| {
            let wrapped = &i["playlistPanelVideoWrapperRenderer"]["primaryRenderer"]["playlistPanelVideoRenderer"];
            let r = if wrapped.is_null() { &i["playlistPanelVideoRenderer"] } else { wrapped };
            if is_video(r) { None } else { panel_item(r) }
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
    fn video_type(v: &Value) -> Option<&str> {
        match v {
            Value::Object(m) => m.get("musicVideoType").and_then(Value::as_str).or_else(|| m.values().find_map(video_type)),
            Value::Array(a) => a.iter().find_map(video_type),
            _ => None,
        }
    }
    // Only where the item itself plays: a playlist card can hold its first video's type too.
    let (two, row) = (&item["musicTwoRowItemRenderer"], &item["musicResponsiveListItemRenderer"]);
    let own = [&two["navigationEndpoint"], &row["overlay"], &row["flexColumns"][0], &item["navigationEndpoint"]];
    matches!(own.into_iter().find_map(video_type), Some("MUSIC_VIDEO_TYPE_OMV" | "MUSIC_VIDEO_TYPE_UGC" | "MUSIC_VIDEO_TYPE_OFFICIAL_SOURCE_MUSIC"))
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
    Some(Track { id, title, artists, album, duration, thumbs: thumbs(&r["thumbnail"]["musicThumbnailRenderer"]), track_nr, plays })
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
        let a = parse_album(&v, "MPREb_x").unwrap();
        assert_eq!(a.subtitle, "Album • The Weeknd • 2020");
        assert_eq!(a.save_id.as_deref(), Some("OLAK5uy_x"));
        let t = &a.tracks[0];
        assert_eq!(t.artists[0].name, "The Weeknd");
        assert_eq!(t.album.as_ref().and_then(|l| l.id.as_deref()), Some("MPREb_x"));
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
}


