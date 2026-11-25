//! App-side data types, parsed from YouTube Music's responses in `ytm.rs`.

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Link {
    pub name: String,
    /// Browse id (artist channel / album) when the name is clickable.
    pub id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artists: Vec<Link>,
    pub album: Option<Link>,
    /// Seconds; unknown for some home-feed items until playback reports it.
    pub duration: Option<u32>,
    pub thumbs: Vec<Thumb>,
    pub track_nr: Option<u16>,
    /// Play count, as YouTube Music shows it on artist pages and in search.
    pub plays: Option<u64>,
    /// An official music video (not the song), which plays its song version instead
    /// when one is found and that's switched on.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub music_video: bool,
    /// A video to show, not a song: an official music video or any other upload (a
    /// channel's own video of a song, a live performance).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub video: bool,
    /// Marked explicit (the "E" badge), which can be hidden.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub explicit: bool,
    /// The video this song stands in for: a video liked on YouTube shows in Liked Music
    /// as the song YouTube Music matched it to, and the like is the video's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stands_for: Option<String>,
}

impl Track {
    pub fn artist_line(&self) -> String {
        self.artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
    }

    /// Whether there's a video to show. Saved before `video` was, an official music video
    /// only says `music_video`.
    pub fn has_video(&self) -> bool {
        self.video || self.music_video
    }

    pub fn first_artist_id(&self) -> Option<&str> {
        self.artists.iter().find_map(|a| a.id.as_deref())
    }
}

/// Smallest thumbnail at least `min_px` wide (falls back to the largest).
pub fn pick_thumb(thumbs: &[Thumb], min_px: u32) -> Option<&Thumb> {
    thumbs.iter().filter(|t| t.width >= min_px).min_by_key(|t| t.width).or_else(|| thumbs.iter().max_by_key(|t| t.width))
}

/// The square region of a `w × h` image that [`Thumb::square_uv`] shows, as
/// `(x, y, side)` in pixels: `None` when the image already is that square (or close
/// enough that nothing shows). Image work that doesn't run through a texture's UV —
/// the backdrop's colours — crops with this first, so what it reads matches the
/// sleeve the sleeve crops in flight.
pub fn square_crop_px(w: u32, h: u32) -> Option<(u32, u32, u32)> {
    let ratio = if h > 0 { w as f32 / h as f32 } else { 1.0 };
    let side = if ratio > 1.6 {
        // A 16:9 picture: its middle square, the sides off.
        h
    } else if ratio > 1.2 {
        // 16:9 picture inside a 4:3 frame: content is the middle 75% vertically.
        (h as f32 * 0.75) as u32
    } else {
        return None;
    };
    Some(((w - side) / 2, (h - side) / 2, side.max(1)))
}

pub fn pick(thumbs: &[Thumb], min_px: u32) -> Option<&str> {
    pick_thumb(thumbs, min_px).map(|t| t.url.as_str())
}

/// A YouTube video thumbnail's largest size (1280×720), for one listed smaller. Not every
/// video has one that big: it fails to load, and the one listed stays.
pub fn ytimg_max(url: &str) -> Option<String> {
    let id = url.split_once("i.ytimg.com/vi/")?.1.split('/').next().filter(|id| !id.is_empty())?;
    Some(format!("https://i.ytimg.com/vi/{id}/maxresdefault.jpg"))
}

/// Ask Google's image CDN for a specific size (`=w226-h226…` / `=s120` suffixes), so
/// thumbnails are sharp at the size drawn without downloading huge originals.
pub fn sized(url: &str, px: u32) -> String {
    if (url.contains("googleusercontent.com") || url.contains("ggpht.com"))
        && let Some(i) = url.rfind('=')
    {
        // A square one that asked to be cropped to fill (`-p`, `-c`; artist photos are
        // often portrait) keeps that, or it comes back its own shape and is drawn
        // stretched into the square.
        let crop = square_crop(&url[i + 1..]);
        return format!("{}=w{px}-h{px}{}-l90-rj", &url[..i], if crop { "-p" } else { "" });
    }
    url.to_owned()
}

/// Whether image options like `w120-h120-p-l90-rj` or `s120-c-k-no` ask for a square crop.
fn square_crop(opts: &str) -> bool {
    let parts: Vec<&str> = opts.split('-').collect();
    if !parts.iter().any(|p| *p == "p" || *p == "c") {
        return false;
    }
    let num = |prefix: char| parts.iter().find_map(|p| p.strip_prefix(prefix)?.parse::<u32>().ok());
    parts.iter().any(|p| p.strip_prefix('s').is_some_and(|n| n.parse::<u32>().is_ok())) || matches!((num('w'), num('h')), (Some(w), Some(h)) if w == h)
}

#[cfg(test)]
mod sized_tests {
    use super::{sized, ytimg_max};

    #[test]
    fn a_video_thumbnail_has_a_biggest_size() {
        let listed = "https://i.ytimg.com/vi/3JBKp0YbSEc/hq720.jpg?sqp=-oaymwEKCNUGEN8DIABIWg&rs=AMzJL3mWm";
        assert_eq!(ytimg_max(listed).as_deref(), Some("https://i.ytimg.com/vi/3JBKp0YbSEc/maxresdefault.jpg"));
        assert_eq!(ytimg_max("https://lh3.googleusercontent.com/abc=w544-h544"), None);
    }

    #[test]
    fn square_crops_stay_cropped() {
        let base = "https://yt3.ggpht.com/abc";
        assert_eq!(sized(&format!("{base}=w120-h120-p-l90-rj"), 544), format!("{base}=w544-h544-p-l90-rj"));
        assert_eq!(sized(&format!("{base}=s88-c-k-c0x00ffffff-no-rj"), 544), format!("{base}=w544-h544-p-l90-rj"));
        // Album art doesn't ask, and a wide banner isn't squared.
        assert_eq!(sized("https://lh3.googleusercontent.com/x=w120-h120-l90-rj", 544), "https://lh3.googleusercontent.com/x=w544-h544-l90-rj");
        assert_eq!(sized("https://lh3.googleusercontent.com/x=w1440-h600-p-l90-rj", 544), "https://lh3.googleusercontent.com/x=w544-h544-l90-rj");
    }
}

/// What a card or shelf item opens.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Target {
    Song(Track),
    Album(String),
    Playlist(String),
    Artist(String),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Card {
    pub title: String,
    pub subtitle: String,
    pub thumbs: Vec<Thumb>,
    pub target: Target,
    /// Artists are shown as circles.
    pub round: bool,
    /// Artists named in the subtitle; clicking one opens their page.
    pub links: Vec<Link>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ShelfKind {
    /// Square/round cards in one horizontal row.
    Cards(Vec<Card>),
    /// Song rows laid out in columns of `rows` (Quick picks).
    Songs { tracks: Vec<Track>, rows: usize },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Shelf {
    pub title: String,
    /// Small line above the title ("GRAPE JUICE" on Listen again).
    pub strapline: Option<String>,
    pub strap_thumb: Option<String>,
    /// Listen again: its thumbnail is the account's own channel. Told by that, not the
    /// title, which comes in the language asked for.
    #[serde(default)]
    pub own: bool,
    pub kind: ShelfKind,
}

/// Which of Home's shelves is Quick picks: by its title in English (`hl`, Settings › Song
/// info), and in another language ("Schnellauswahl", "曲のミックス") as the first shelf of song rows.
pub fn quick_picks(shelves: &[Shelf], hl: &str) -> Option<usize> {
    let rows = |s: &Shelf| matches!(s.kind, ShelfKind::Songs { .. });
    if hl == crate::locale::ENGLISH.hl {
        return shelves.iter().position(|s| s.title == "Quick picks");
    }
    shelves.iter().position(rows)
}

#[cfg(test)]
mod quick_picks_tests {
    use super::{quick_picks, Shelf, ShelfKind};

    fn shelf(title: &str, songs: bool) -> Shelf {
        let kind = if songs { ShelfKind::Songs { tracks: vec![], rows: 4 } } else { ShelfKind::Cards(vec![]) };
        Shelf { title: title.into(), strapline: None, strap_thumb: None, own: false, kind }
    }

    #[test]
    fn quick_picks_by_title_or_else_the_first_song_rows() {
        assert_eq!(quick_picks(&[shelf("Listen again", false), shelf("Covers", true), shelf("Quick picks", true)], "en"), Some(2));
        // In English, song rows by another name aren't it.
        assert_eq!(quick_picks(&[shelf("Listen again", false), shelf("Covers", true)], "en"), None);
        assert_eq!(quick_picks(&[shelf("もう一度聴く", false), shelf("曲のミックス", true), shelf("カバー", true)], "ja"), Some(1));
        assert_eq!(quick_picks(&[shelf("Mixed for you", false)], "ja"), None);
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Chip {
    pub title: String,
    /// Browse params selecting this mood; `None` clears the filter.
    pub params: Option<String>,
    pub selected: bool,
}
