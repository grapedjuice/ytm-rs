//! App-side data types. rustypipe's models are `#[non_exhaustive]`, so items parsed
//! from raw InnerTube responses (the home feed) can't be built as rustypipe types.

use rustypipe::model::{ArtistId, Thumbnail, TrackItem};

#[derive(Clone, Debug, PartialEq)]
pub struct Thumb {
    pub url: String,
    pub width: u32,
    pub height: u32,
}

impl Thumb {
    /// UV rect that shows a centred square of this image, trimming the letterboxing of
    /// 4:3 video thumbnails and the sides of 16:9 ones.
    pub fn square_uv(&self) -> egui::Rect {
        use egui::{pos2, Rect};
        let ratio = if self.height > 0 { self.width as f32 / self.height as f32 } else { 1.0 };
        if ratio > 1.6 {
            let w = 1.0 / ratio;
            Rect::from_min_max(pos2(0.5 - w / 2.0, 0.0), pos2(0.5 + w / 2.0, 1.0))
        } else if ratio > 1.2 {
            // 16:9 picture inside a 4:3 frame: content is the middle 75% vertically.
            let w = 0.75 / ratio;
            Rect::from_min_max(pos2(0.5 - w / 2.0, 0.125), pos2(0.5 + w / 2.0, 0.875))
        } else {
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub name: String,
    /// Browse id (artist channel / album) when the name is clickable.
    pub id: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artists: Vec<Link>,
    pub album: Option<Link>,
    /// Seconds; unknown for some home-feed items until playback reports it.
    pub duration: Option<u32>,
    pub thumbs: Vec<Thumb>,
    pub track_nr: Option<u16>,
}

impl Track {
    pub fn artist_line(&self) -> String {
        self.artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
    }

    pub fn first_artist_id(&self) -> Option<&str> {
        self.artists.iter().find_map(|a| a.id.as_deref())
    }
}

pub fn thumbs(t: &[Thumbnail]) -> Vec<Thumb> {
    t.iter().map(|t| Thumb { url: t.url.clone(), width: t.width, height: t.height }).collect()
}

pub fn links(a: &[ArtistId]) -> Vec<Link> {
    a.iter().map(|a| Link { name: a.name.clone(), id: a.id.clone() }).collect()
}

impl From<&TrackItem> for Track {
    fn from(t: &TrackItem) -> Self {
        let mut artists = links(&t.artists);
        if let (Some(first), Some(id)) = (artists.first_mut(), &t.artist_id) {
            first.id.get_or_insert_with(|| id.clone());
        }
        Track {
            id: t.id.clone(),
            title: t.name.clone(),
            artists,
            album: t.album.as_ref().map(|a| Link { name: a.name.clone(), id: Some(a.id.clone()) }),
            duration: t.duration,
            thumbs: thumbs(&t.cover),
            track_nr: t.track_nr,
        }
    }
}

pub fn tracks(items: &[TrackItem]) -> Vec<Track> {
    items.iter().map(Track::from).collect()
}

/// Smallest thumbnail at least `min_px` wide (falls back to the largest).
pub fn pick_thumb(thumbs: &[Thumb], min_px: u32) -> Option<&Thumb> {
    thumbs.iter().filter(|t| t.width >= min_px).min_by_key(|t| t.width).or_else(|| thumbs.iter().max_by_key(|t| t.width))
}

pub fn pick(thumbs: &[Thumb], min_px: u32) -> Option<&str> {
    pick_thumb(thumbs, min_px).map(|t| t.url.as_str())
}

/// Ask Google's image CDN for a specific size (`=w226-h226…` / `=s120` suffixes), so
/// thumbnails are sharp at the size drawn without downloading huge originals.
pub fn sized(url: &str, px: u32) -> String {
    if url.contains("googleusercontent.com") || url.contains("ggpht.com") {
        if let Some(i) = url.rfind('=') {
            return format!("{}=w{px}-h{px}-l90-rj", &url[..i]);
        }
    }
    url.to_owned()
}

/// What a card or shelf item opens.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Song(Track),
    Album(String),
    Playlist(String),
    Artist(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub title: String,
    pub subtitle: String,
    pub thumbs: Vec<Thumb>,
    pub target: Target,
    /// Artists are shown as circles.
    pub round: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ShelfKind {
    /// Square/round cards in one horizontal row.
    Cards(Vec<Card>),
    /// Song rows laid out in columns of `rows` (Quick picks).
    Songs { tracks: Vec<Track>, rows: usize },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Shelf {
    pub title: String,
    /// Small line above the title ("GRAPE JUICE" on Listen again).
    pub strapline: Option<String>,
    pub strap_thumb: Option<String>,
    pub kind: ShelfKind,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chip {
    pub title: String,
    /// Browse params selecting this mood; `None` clears the filter.
    pub params: Option<String>,
    pub selected: bool,
}
