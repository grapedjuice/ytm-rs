//! Network side: a small tokio runtime owning the RustyPipe client.
//!
//! The UI sends `Req`s and drains `Resp`s once per frame; every response wakes the UI.
//! Playback resolution also lives here: resolve stream → start progressive download →
//! build the decoder on a blocking thread → hand it to the audio engine.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crossbeam_channel::Sender;
use rustypipe::client::RustyPipe;
use rustypipe::model::{
    AlbumItem, ArtistItem, AudioCodec, MusicAlbum, MusicArtist, MusicPlaylist, MusicPlaylistItem,
    TrackItem,
};
use rustypipe::param::StreamFilter;
use tokio::runtime::Runtime;

use crate::audio::{AudioCmd, AudioHandle};
use crate::innertube;
use crate::stream::{self, Shared};


pub enum Req {
    Home,
    Search(String),
    Album(String),
    Artist(String),
    Playlist(String),
    Library,
    Lyrics(String),
    /// Radio seeded from a track; appended to the queue.
    Radio(String),
    Play { video_id: String, generation: u64 },
    SetCookie(String),
    Logout,
}

impl Req {
    fn name(&self) -> &'static str {
        match self {
            Req::Home => "Loading home",
            Req::Search(_) => "Search",
            Req::Album(_) => "Loading album",
            Req::Artist(_) => "Loading artist",
            Req::Playlist(_) => "Loading playlist",
            Req::Library => "Loading library",
            Req::Lyrics(_) => "Loading lyrics",
            Req::Radio(_) => "Loading radio",
            Req::Play { .. } => "Playback",
            Req::SetCookie(_) => "Sign in",
            Req::Logout => "Sign out",
        }
    }
}

pub struct Home {
    pub top_tracks: Vec<TrackItem>,
    pub trending: Vec<TrackItem>,
    pub new_albums: Vec<AlbumItem>,
    pub playlists: Vec<MusicPlaylistItem>,
}

pub struct SearchResults {
    pub query: String,
    pub tracks: Vec<TrackItem>,
    pub albums: Vec<AlbumItem>,
    pub artists: Vec<ArtistItem>,
    pub playlists: Vec<MusicPlaylistItem>,
}

pub struct Library {
    pub liked: Vec<TrackItem>,
    pub playlists: Vec<MusicPlaylistItem>,
    pub albums: Vec<AlbumItem>,
}

pub enum Resp {
    Home(Home),
    Search(SearchResults),
    Album(MusicAlbum),
    Artist(MusicArtist),
    Playlist(MusicPlaylist),
    Library(Library),
    Lyrics { video_id: String, text: Option<String> },
    Radio { seed: String, tracks: Vec<TrackItem> },
    /// Playback for `generation` failed before reaching the audio engine.
    PlayError { generation: u64, msg: String },
    LoggedIn(bool),
    Error(String),
}

#[derive(Clone)]
pub struct Backend {
    rt: Arc<Runtime>,
    rp: RustyPipe,
    http: reqwest::Client,
    tx: Sender<Resp>,
    audio: AudioHandle,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Backend {
    pub fn new(
        storage: PathBuf,
        tx: Sender<Resp>,
        audio: AudioHandle,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> anyhow::Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("net")
            .enable_all()
            .build()?;
        std::fs::create_dir_all(&storage)?;
        let rp = RustyPipe::builder().storage_dir(storage).timezone_local().no_reporter().build()?;
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .pool_idle_timeout(std::time::Duration::from_secs(30))
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36")
            .build()?;
        Ok(Self { rt: Arc::new(rt), rp, http, tx, audio, wake })
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    pub fn spawn<F: Future<Output = ()> + Send + 'static>(&self, f: F) {
        self.rt.spawn(f);
    }

    pub fn wake(&self) {
        (self.wake)()
    }

    pub fn request(&self, req: Req) {
        let this = self.clone();
        self.rt.spawn(async move {
            let resp = this.handle(req).await;
            if let Some(resp) = resp {
                let _ = this.tx.send(resp);
                this.wake();
            }
        });
    }

    async fn handle(&self, req: Req) -> Option<Resp> {
        let q = self.rp.query();
        let what = req.name();
        let res: anyhow::Result<Option<Resp>> = async {
            Ok(Some(match req {
                Req::Home => {
                    let (charts, new) = tokio::join!(q.music_charts(None), q.music_new_albums());
                    let charts = charts?;
                    Resp::Home(Home {
                        top_tracks: charts.top_tracks,
                        trending: charts.trending_tracks,
                        new_albums: new.unwrap_or_default(),
                        playlists: charts.playlists,
                    })
                }
                Req::Search(query) => {
                    let (t, al, ar, pl) = tokio::join!(
                        q.music_search_tracks(&query),
                        q.music_search_albums(&query),
                        q.music_search_artists(&query),
                        q.music_search_playlists(&query, false),
                    );
                    Resp::Search(SearchResults {
                        tracks: t?.items.items,
                        albums: al.map(|r| r.items.items).unwrap_or_default(),
                        artists: ar.map(|r| r.items.items).unwrap_or_default(),
                        playlists: pl.map(|r| r.items.items).unwrap_or_default(),
                        query,
                    })
                }
                Req::Album(id) => Resp::Album(q.music_album(&id).await?),
                Req::Artist(id) => Resp::Artist(q.music_artist(&id, false).await?),
                Req::Playlist(id) => {
                    let mut pl = q.music_playlist(&id).await?;
                    // Pull a few more pages so long playlists are usable without paging UI.
                    let _ = pl.tracks.extend_limit(&q, 500).await;
                    Resp::Playlist(pl)
                }
                Req::Library => {
                    let q = q.authenticated();
                    let (liked, pls, albums) = tokio::join!(
                        q.music_liked_tracks(),
                        q.music_saved_playlists(),
                        q.music_saved_albums()
                    );
                    let mut liked = liked?;
                    let _ = liked.tracks.extend_limit(&q, 1000).await;
                    Resp::Library(Library {
                        liked: liked.tracks.items,
                        playlists: pls.map(|p| p.items).unwrap_or_default(),
                        albums: albums.map(|p| p.items).unwrap_or_default(),
                    })
                }
                Req::Lyrics(video_id) => {
                    let details = q.music_details(&video_id).await?;
                    let text = match details.lyrics_id {
                        Some(id) => q.music_lyrics(&id).await.ok().map(|l| l.body),
                        None => None,
                    };
                    Resp::Lyrics { video_id, text }
                }
                Req::Radio(seed) => {
                    let mut radio = q.music_radio_track(&seed).await?;
                    let _ = radio.extend_limit(&q, 50).await;
                    Resp::Radio { seed, tracks: radio.items }
                }
                Req::Play { video_id, generation } => {
                    match self.start_playback(&video_id, generation).await {
                        Ok(()) => return Ok(None),
                        Err(e) => Resp::PlayError { generation, msg: format!("{e:#}") },
                    }
                }
                Req::SetCookie(cookie) => {
                    self.rp.user_auth_set_cookie(cookie).await?;
                    Resp::LoggedIn(true)
                }
                Req::Logout => {
                    self.rp.user_auth_remove_cookie().await?;
                    Resp::LoggedIn(false)
                }
            }))
        }
        .await;
        res.unwrap_or_else(|e| {
            log::warn!("{what} failed: {e:#}");
            Some(Resp::Error(format!("{what} failed: {e}")))
        })
    }

    async fn start_playback(&self, video_id: &str, generation: u64) -> anyhow::Result<()> {
        let t0 = std::time::Instant::now();
        let (url, size, ua) = match self.visionos_stream(video_id).await {
            Ok(s) => (s.url, s.size, innertube::UA.to_owned()),
            Err(e) => {
                log::warn!("visionOS player failed ({e:#}), falling back to rustypipe");
                let q = self.rp.query();
                let player = q.player(video_id).await?;
                // AAC in MP4 decodes in pure Rust (symphonia); Opus would need libopus.
                let filter = StreamFilter::new().no_video().audio_codecs([AudioCodec::Mp4a]);
                let s = player
                    .select_audio_stream(&filter)
                    .ok_or_else(|| anyhow::anyhow!("no AAC audio stream available"))?;
                (s.url.clone(), s.size, q.user_agent(player.client_type).into_owned())
            }
        };
        log::debug!("play {video_id} (gen {generation}): resolved {size} bytes in {:?}", t0.elapsed());
        if self.audio.status.wanted_gen.load(Ordering::Relaxed) != generation {
            return Ok(());
        }

        let shared = Shared::new(size);
        let refresh: stream::Refresh = {
            let (this, id) = (self.clone(), video_id.to_owned());
            Arc::new(move || {
                let (this, id) = (this.clone(), id.clone());
                Box::pin(async move { Ok(this.visionos_stream(&id).await?.url) })
            })
        };
        self.rt.spawn(stream::download(self.http.clone(), url, ua, shared.clone(), Some(refresh)));

        let audio = self.audio.clone();
        let tx = self.tx.clone();
        let wake = self.wake.clone();
        // Opening the decoder reads the container header, which blocks on the download.
        tokio::task::spawn_blocking(move || match stream::decoder(shared.clone(), false) {
            Ok(decoder) => {
                log::debug!("play gen {generation}: decoder ready in {:?}", t0.elapsed());
                audio.send(AudioCmd::Start { generation, decoder, shared })
            }
            Err(e) => {
                let _ = tx.send(Resp::PlayError { generation, msg: format!("decode: {e}") });
                wake();
            }
        });
        Ok(())
    }

    async fn visionos_stream(&self, video_id: &str) -> anyhow::Result<innertube::Stream> {
        let vd = self.rp.query().get_visitor_data(false).await?;
        match innertube::audio_stream(&self.http, video_id, &vd).await {
            Ok(s) => Ok(s),
            Err(e) => {
                // A stale/flagged visitor ID is the usual cause; retry once with a fresh one.
                log::info!("visionOS player: {e:#}; retrying with new visitor data");
                self.rp.query().remove_visitor_data(&vd);
                let vd = self.rp.query().get_visitor_data(true).await?;
                innertube::audio_stream(&self.http, video_id, &vd).await
            }
        }
    }

    pub fn is_logged_in(&self) -> bool {
        self.rp.query().auth_enabled(rustypipe::client::ClientType::Desktop)
    }
}
