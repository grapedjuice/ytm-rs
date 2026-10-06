//! Which rustypipe music endpoints currently work? cargo run --example apis
use rustypipe::client::RustyPipe;

macro_rules! check {
    ($name:expr, $e:expr, $f:expr) => {
        match $e.await {
            Ok(v) => println!("ok   {:<22} {}", $name, $f(&v)),
            Err(e) => println!("FAIL {:<22} {e}", $name),
        }
    };
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let rp = RustyPipe::builder().no_storage().build()?;
    let q = rp.query();
    check!("search_main", q.music_search_main("daft punk"), |r: &rustypipe::model::MusicSearchResult<_>| format!("{} items", r.items.items.len()));
    check!("search_tracks", q.music_search_tracks("daft punk"), |r: &rustypipe::model::MusicSearchResult<_>| format!("{} items", r.items.items.len()));
    check!("search_albums", q.music_search_albums("daft punk"), |r: &rustypipe::model::MusicSearchResult<_>| format!("{} items", r.items.items.len()));
    check!("search_artists", q.music_search_artists("daft punk"), |r: &rustypipe::model::MusicSearchResult<_>| format!("{} items", r.items.items.len()));
    check!("search_playlists", q.music_search_playlists("daft punk", false), |r: &rustypipe::model::MusicSearchResult<_>| format!("{} items", r.items.items.len()));
    check!("charts", q.music_charts(None), |r: &rustypipe::model::MusicCharts| format!("{} top", r.top_tracks.len()));
    check!("new_albums", q.music_new_albums(), |r: &Vec<_>| format!("{}", r.len()));
    check!("genres", q.music_genres(), |r: &Vec<_>| format!("{}", r.len()));
    check!("album", q.music_album("MPREb_Gj7bLw4FCbs"), |r: &rustypipe::model::MusicAlbum| format!("{} - {} tracks", r.name, r.tracks.len()));
    check!("artist", q.music_artist("UC_kRDKYrUlrbtrSiyu5Tflg", false), |r: &rustypipe::model::MusicArtist| format!("{} - {} tracks {} albums", r.name, r.tracks.len(), r.albums.len()));
    check!("playlist", q.music_playlist("RDCLAK5uy_kmPRjHDECIcuVwnKsx2Ng7fyNgFKWNJFs"), |r: &rustypipe::model::MusicPlaylist| format!("{} - {} tracks", r.name, r.tracks.items.len()));
    check!("details", q.music_details("4D7u5KF7SP8"), |r: &rustypipe::model::TrackDetails| format!("lyrics_id={:?}", r.lyrics_id.is_some()));
    check!("radio_track", q.music_radio_track("4D7u5KF7SP8"), |r: &rustypipe::model::paginator::Paginator<_>| format!("{} items", r.items.len()));
    check!("radio(RDAMVM)", q.music_radio("RDAMVM4D7u5KF7SP8"), |r: &rustypipe::model::paginator::Paginator<_>| format!("{} items", r.items.len()));
    if let Ok(d) = q.music_details("4D7u5KF7SP8").await {
        if let Some(id) = d.lyrics_id { check!("lyrics", q.music_lyrics(&id), |r: &rustypipe::model::Lyrics| format!("{} chars", r.body.len())); }
    }
    Ok(())
}
