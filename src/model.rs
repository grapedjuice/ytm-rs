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

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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
}

impl Track {
    pub fn artist_line(&self) -> String {
        self.artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
    }

    pub fn first_artist_id(&self) -> Option<&str> {
        self.artists.iter().find_map(|a| a.id.as_deref())
    }
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
            // A square one that asked to be cropped to fill (`-p`, `-c`; artist photos are
            // often portrait) keeps that, or it comes back its own shape and is drawn
            // stretched into the square.
            let crop = square_crop(&url[i + 1..]);
            return format!("{}=w{px}-h{px}{}-l90-rj", &url[..i], if crop { "-p" } else { "" });
        }
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
    use super::sized;

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
