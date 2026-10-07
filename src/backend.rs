//! Network side: a small tokio runtime owning the RustyPipe client.
//!
//! The UI sends `Req`s and drains `Resp`s once per frame; every response wakes the UI.
//! Playback resolution also lives here: resolve stream → start progressive download →
//! build the decoder on a blocking thread → hand it to the audio engine.
//! rustypipe types are converted to the app's own models (model.rs) at this boundary.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crossbeam_channel::Sender;
use rustypipe::client::RustyPipe;
use rustypipe::model::richtext::ToPlaintext;
use rustypipe::model::{AlbumItem, ArtistItem, AudioCodec, MusicPlaylistItem};
use rustypipe::param::StreamFilter;
use tokio::runtime::Runtime;

use crate::art::Art;
use crate::audio::{AudioCmd, AudioHandle};
use crate::lyrics::{self, Lyrics};
use crate::model::{self, Card, Target, Thumb, Track, sized};
use crate::stream::{self, Shared};
use crate::{innertube, ytm};

pub enum Req {
    /// Home feed, optionally filtered by a mood chip's params.
    Home(Option<String>),
    HomeMore(String),
    Explore,
    Search(String),
    Suggest(String),
    Album(String),
    Artist(String),
    Playlist(String),
    Library,
    History,
    Lyrics(Track),
    /// Process cover art for the background; `key` is echoed back.
    Art { key: String, url: String },
    /// Radio seeded from a track; appended to the queue.
    Radio(String),
    /// Fetch an album/playlist and play it from the start.
    PlayCollection { target: Target, shuffle: bool },
    Play { video_id: String, generation: u64 },
    /// Resolve and download a track ahead of time so skipping to it is instant.
    Prefetch(String),
    Like { video_id: String, like: bool },
    SetCookie(String),
    Logout,
}

impl Req {
    fn name(&self) -> &'static str {
        match self {
            Req::Home(_) | Req::HomeMore(_) => "Loading home",
            Req::Explore => "Loading explore",
            Req::Search(_) => "Search",
            Req::Suggest(_) => "Suggest",
            Req::Album(_) => "Loading album",
            Req::Artist(_) => "Loading artist",
            Req::Playlist(_) => "Loading playlist",
            Req::Library => "Loading library",
            Req::History => "Loading history",
            Req::Lyrics(_) => "Lyrics",
            Req::Art { .. } => "Art",
            Req::Radio(_) => "Loading radio",
            Req::PlayCollection { .. } => "Playback",
            Req::Play { .. } => "Playback",
            Req::Prefetch(_) => "Prefetch",
            Req::Like { .. } => "Like",
            Req::SetCookie(_) => "Sign in",
            Req::Logout => "Sign out",
        }
    }

    /// Background requests fail quietly (logged, no toast).
    fn quiet(&self) -> bool {
        matches!(self, Req::Suggest(_) | Req::Art { .. } | Req::Prefetch(_) | Req::HomeMore(_) | Req::Lyrics(_))
    }
}

pub struct Explore {
    pub new_albums: Vec<Card>,
    pub charts: Vec<Card>,
    pub top: Vec<Track>,
}

pub struct SearchResults {
    pub query: String,
    pub tracks: Vec<Track>,
    pub albums: Vec<Card>,
    pub artists: Vec<Card>,
    pub playlists: Vec<Card>,
}

/// An album or playlist page.
pub struct Collection {
    pub id: String,
    pub is_album: bool,
    pub title: String,
    pub subtitle: String,
    pub description: Option<String>,
    pub thumbs: Vec<Thumb>,
    pub tracks: Vec<Track>,
}

pub struct ArtistPage {
    pub id: String,
    pub name: String,
    pub banner: Vec<Thumb>,
    pub subscribers: Option<u64>,
    pub description: Option<String>,
    pub top: Vec<Track>,
    pub albums: Vec<Card>,
    pub playlists: Vec<Card>,
    pub similar: Vec<Card>,
}

pub struct Library {
    pub liked: Vec<Track>,
    pub playlists: Vec<Card>,
    pub albums: Vec<Card>,
}

pub enum Resp {
    Home(ytm::HomePage),
    HomeMore(ytm::HomePage),
    Explore(Explore),
    Search(SearchResults),
    Suggest { query: String, terms: Vec<String> },
    Collection(Collection),
    Artist(ArtistPage),
    Library(Library),
    History(Vec<Track>),
    Lyrics { video_id: String, lyrics: Option<Lyrics> },
    Art { key: String, art: Art },
    Radio { seed: String, tracks: Vec<Track> },
    PlayQueue { tracks: Vec<Track> },
    Liked { video_id: String, like: bool },
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
    data_dir: PathBuf,
    lyrics: Arc<lyrics::Client>,
    /// The upcoming track, resolved and downloading ahead of time.
    prefetched: Arc<std::sync::Mutex<Option<(String, Arc<Shared>)>>>,
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
        let data_dir = storage.clone();
        let rp = RustyPipe::builder().storage_dir(storage).timezone_local().no_reporter().build()?;
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .pool_idle_timeout(std::time::Duration::from_secs(30))
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36")
            .build()?;
        let lyrics = Arc::new(lyrics::Client::new(http.clone(), &data_dir));
        Ok(Self { rt: Arc::new(rt), rp, http, tx, audio, wake, prefetched: Default::default(), data_dir, lyrics })
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
        let (what, quiet) = (req.name(), req.quiet());
        let res: anyhow::Result<Option<Resp>> = async {
            Ok(Some(match req {
                Req::Home(params) => {
                    let q = if self.is_logged_in() { q.authenticated() } else { q };
                    let page = ytm::home(&q, params.as_deref()).await?;
                    Resp::Home(page)
                }
                Req::HomeMore(token) => {
                    let q = if self.is_logged_in() { q.authenticated() } else { q };
                    Resp::HomeMore(ytm::home_more(&q, &token).await?)
                }
                Req::Explore => {
                    let (charts, new) = tokio::join!(q.music_charts(None), q.music_new_albums());
                    let charts = charts?;
                    Resp::Explore(Explore {
                        new_albums: new.unwrap_or_default().iter().map(album_card).collect(),
                        charts: charts.playlists.iter().map(playlist_card).collect(),
                        top: model::tracks(if charts.top_tracks.is_empty() { &charts.trending_tracks } else { &charts.top_tracks }),
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
                        tracks: model::tracks(&t?.items.items),
                        albums: al.map(|r| r.items.items.iter().map(album_card).collect()).unwrap_or_default(),
                        artists: ar.map(|r| r.items.items.iter().map(artist_card).collect()).unwrap_or_default(),
                        playlists: pl.map(|r| r.items.items.iter().map(playlist_card).collect()).unwrap_or_default(),
                        query,
                    })
                }
                Req::Suggest(query) => {
                    let s = q.music_search_suggestion(&query).await?;
                    Resp::Suggest { query, terms: s.terms.into_iter().take(8).collect() }
                }
                Req::Album(id) => Resp::Collection(self.album(&id).await?),
                Req::Playlist(id) => Resp::Collection(self.playlist(&id).await?),
                Req::Artist(id) => {
                    let a = q.music_artist(&id, false).await?;
                    Resp::Artist(ArtistPage {
                        id: a.id,
                        name: a.name,
                        banner: model::thumbs(&a.header_image),
                        subscribers: a.subscriber_count,
                        description: a.description,
                        top: model::tracks(&a.tracks),
                        albums: a.albums.iter().map(album_card).collect(),
                        playlists: a.playlists.iter().map(playlist_card).collect(),
                        similar: a.similar_artists.iter().map(artist_card).collect(),
                    })
                }
                Req::Library => {
                    let q = q.authenticated();
                    let (liked, pls, albums) =
                        tokio::join!(q.music_liked_tracks(), q.music_saved_playlists(), q.music_saved_albums());
                    let mut liked = liked?;
                    let _ = liked.tracks.extend_limit(&q, 1000).await;
                    Resp::Library(Library {
                        liked: model::tracks(&liked.tracks.items),
                        playlists: pls.map(|p| p.items.iter().map(playlist_card).collect()).unwrap_or_default(),
                        albums: albums.map(|p| p.items.iter().map(album_card).collect()).unwrap_or_default(),
                    })
                }
                Req::History => {
                    let h = q.authenticated().music_history().await?;
                    let mut seen = std::collections::HashSet::new();
                    Resp::History(
                        h.items.iter().map(|i| Track::from(&i.item)).filter(|t| seen.insert(t.id.clone())).collect(),
                    )
                }
                Req::Lyrics(track) => {
                    let video_id = track.id.clone();
                    Resp::Lyrics { lyrics: self.lyrics_for(&track).await, video_id }
                }
                Req::Art { key, url } => {
                    let bytes = self.http.get(&url).send().await?.error_for_status()?.bytes().await?;
                    let art = tokio::task::spawn_blocking(move || crate::art::process(&bytes)).await??;
                    Resp::Art { key, art }
                }
                Req::Radio(seed) => {
                    let mut radio = q.music_radio_track(&seed).await?;
                    let _ = radio.extend_limit(&q, 50).await;
                    Resp::Radio { seed, tracks: model::tracks(&radio.items) }
                }
                Req::PlayCollection { target, shuffle } => {
                    let mut tracks = match target {
                        Target::Album(id) => self.album(&id).await?.tracks,
                        Target::Playlist(id) => self.playlist(&id).await?.tracks,
                        Target::Artist(id) => {
                            let a = q.music_artist(&id, false).await?;
                            model::tracks(&a.tracks)
                        }
                        Target::Song(t) => vec![t],
                    };
                    if shuffle {
                        crate::ui::shuffle(&mut tracks);
                    }
                    Resp::PlayQueue { tracks }
                }
                Req::Play { video_id, generation } => match self.start_playback(&video_id, generation).await {
                    Ok(()) => return Ok(None),
                    Err(e) => Resp::PlayError { generation, msg: format!("{e:#}") },
                },
                Req::Prefetch(video_id) => {
                    if self.prefetched.lock().unwrap().as_ref().is_some_and(|(id, _)| *id == video_id) {
                        return Ok(None);
                    }
                    let shared = self.open_stream(&video_id).await?;
                    if let Some((_, old)) = self.prefetched.lock().unwrap().replace((video_id, shared)) {
                        old.cancel();
                    }
                    return Ok(None);
                }
                Req::Like { video_id, like } => {
                    ytm::set_like(&q.authenticated(), &video_id, like).await?;
                    Resp::Liked { video_id, like }
                }
                Req::SetCookie(input) => {
                    self.set_cookie(&input).await?;
                    log::info!("signed in");
                    Resp::LoggedIn(true)
                }
                Req::Logout => {
                    let _ = self.rp.user_auth_remove_cookie().await;
                    crate::login::forget(&self.login_profile());
                    Resp::LoggedIn(false)
                }
            }))
        }
        .await;
        res.unwrap_or_else(|e| {
            log::warn!("{what} failed: {e:#}");
            (!quiet).then(|| Resp::Error(format!("{what} failed: {e}")))
        })
    }

    async fn album(&self, id: &str) -> anyhow::Result<Collection> {
        let a = self.rp.query().music_album(id).await?;
        let mut sub = vec![format!("{:?}", a.album_type).replace("Ep", "EP")];
        sub.push(a.artists.iter().map(|x| x.name.as_str()).collect::<Vec<_>>().join(", "));
        if let Some(y) = a.year {
            sub.push(y.to_string());
        }
        let mut tracks = model::tracks(&a.tracks);
        // Album track lists omit covers; reuse the album's so rows and the queue have art.
        for t in &mut tracks {
            if t.thumbs.is_empty() {
                t.thumbs = model::thumbs(&a.cover);
            }
            if t.album.is_none() {
                t.album = Some(model::Link { name: a.name.clone(), id: Some(a.id.clone()) });
            }
        }
        Ok(Collection {
            id: a.id,
            is_album: true,
            title: a.name,
            subtitle: sub.join(" • "),
            description: a.description.map(|d| d.to_plaintext()),
            thumbs: model::thumbs(&a.cover),
            tracks,
        })
    }

    async fn playlist(&self, id: &str) -> anyhow::Result<Collection> {
        let q = self.rp.query();
        let q = if self.is_logged_in() { q.authenticated() } else { q };
        let mut pl = q.music_playlist(id).await?;
        // Pull a few more pages so long playlists are usable without paging UI.
        let _ = pl.tracks.extend_limit(&q, 500).await;
        let mut sub = Vec::new();
        if let Some(c) = &pl.channel {
            sub.push(c.name.clone());
        }
        sub.push(format!("{} songs", pl.track_count.unwrap_or(pl.tracks.items.len() as u64)));
        Ok(Collection {
            id: pl.id,
            is_album: false,
            title: pl.name,
            subtitle: sub.join(" • "),
            description: pl.description.map(|d| d.to_plaintext()),
            thumbs: model::thumbs(&pl.thumbnail),
            tracks: model::tracks(&pl.tracks.items),
        })
    }

    async fn lyrics_for(&self, t: &Track) -> Option<Lyrics> {
        let artist = t.artist_line();
        let query = lyrics::Query {
            video_id: &t.id,
            song: &t.title,
            artist: &artist,
            album: t.album.as_ref().map(|a| a.name.as_str()),
            duration: t.duration.or_else(|| self.audio.status.duration().map(|d| d.as_secs() as u32)),
        };
        match self.lyrics.fetch(&query).await {
            Ok(Some(l)) => return Some(l),
            Ok(None) => {}
            Err(e) => log::warn!("better lyrics failed: {e:#}"),
        }
        // Fallback: YouTube Music's own (unsynced) lyrics.
        let q = self.rp.query();
        let id = q.music_details(&t.id).await.ok()?.lyrics_id?;
        let body = q.music_lyrics(&id).await.ok()?.body;
        Some(Lyrics::plain(&body, "YouTube Music"))
    }

    async fn set_cookie(&self, input: &str) -> anyhow::Result<()> {
        let input = input.trim();
        let res = if input.lines().any(|l| l.split('\t').count() >= 7) {
            // Netscape cookies.txt export from a browser extension.
            self.rp.user_auth_set_cookie_txt(input).await
        } else {
            // Tolerate a copied `cookie: ...` header line.
            let value = input.strip_prefix("cookie:").or_else(|| input.strip_prefix("Cookie:")).unwrap_or(input).trim();
            self.rp.user_auth_set_cookie(value).await
        };
        res.map_err(|e| {
            // Names only, never values: enough to see whether SAPISID etc. arrived.
            let names: Vec<&str> = input.split(';').filter_map(|kv| kv.trim().split('=').next()).take(40).collect();
            log::warn!("cookie sign-in rejected: {e:?}; cookie names: {names:?}");
            match e {
                rustypipe::error::Error::Auth(_) => {
                    anyhow::anyhow!("YouTube didn't recognise a signed-in session in those cookies")
                }
                e => e.into(),
            }
        })
    }

    async fn start_playback(&self, video_id: &str, generation: u64) -> anyhow::Result<()> {
        let t0 = std::time::Instant::now();
        let ready = {
            let mut slot = self.prefetched.lock().unwrap();
            match slot.take() {
                Some((id, s)) if id == video_id => Some(s),
                other => {
                    *slot = other;
                    None
                }
            }
        };
        let shared = match ready {
            Some(s) => {
                log::debug!("play {video_id} (gen {generation}): using prefetched stream");
                s
            }
            None => self.open_stream(video_id).await?,
        };
        log::debug!("play {video_id} (gen {generation}): stream open in {:?}", t0.elapsed());
        if self.audio.status.wanted_gen.load(Ordering::Relaxed) != generation {
            shared.cancel();
            return Ok(());
        }

        let audio = self.audio.clone();
        let tx = self.tx.clone();
        let wake = self.wake.clone();
        // Opening the decoder reads the container header, which blocks on the download.
        tokio::task::spawn_blocking(move || match stream::decoder(shared.clone(), false) {
            Ok(decoder) => {
                log::debug!("play gen {generation}: decoder ready in {:?}", t0.elapsed());
                let duration_ms = shared.duration_ms;
                audio.send(AudioCmd::Start { generation, decoder, shared, duration_ms })
            }
            Err(e) => {
                let _ = tx.send(Resp::PlayError { generation, msg: format!("decode: {e}") });
                wake();
            }
        });
        Ok(())
    }

    /// Resolve a stream URL and start downloading it into a new buffer.
    async fn open_stream(&self, video_id: &str) -> anyhow::Result<Arc<Shared>> {
        let (url, size, ua, duration_ms) = match self.visionos_stream(video_id).await {
            Ok(s) => (s.url, s.size, innertube::UA.to_owned(), s.duration_ms),
            Err(e) => {
                log::warn!("visionOS player failed ({e:#}), falling back to rustypipe");
                let q = self.rp.query();
                let player = q.player(video_id).await?;
                // AAC in MP4 decodes in pure Rust (symphonia); Opus would need libopus.
                let filter = StreamFilter::new().no_video().audio_codecs([AudioCodec::Mp4a]);
                let s = player
                    .select_audio_stream(&filter)
                    .ok_or_else(|| anyhow::anyhow!("no AAC audio stream available"))?;
                let dur = s.duration_ms.unwrap_or(0) as u64;
                (s.url.clone(), s.size, q.user_agent(player.client_type).into_owned(), dur)
            }
        };
        let shared = Shared::new(size, duration_ms);
        let refresh: stream::Refresh = {
            let (this, id) = (self.clone(), video_id.to_owned());
            Arc::new(move || {
                let (this, id) = (this.clone(), id.clone());
                Box::pin(async move { Ok(this.visionos_stream(&id).await?.url) })
            })
        };
        self.rt.spawn(stream::download(self.http.clone(), url, ua, shared.clone(), Some(refresh)));
        Ok(shared)
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

    /// Webview profile used by the sign-in window.
    pub fn login_profile(&self) -> PathBuf {
        self.data_dir.join("webview")
    }

    /// Report an error from outside the request flow (e.g. the sign-in window).
    pub fn report_error(&self, msg: String) {
        let _ = self.tx.send(Resp::Error(msg));
        self.wake();
    }

    pub fn is_logged_in(&self) -> bool {
        self.rp.query().auth_enabled(rustypipe::client::ClientType::Desktop)
    }
}

fn album_card(a: &AlbumItem) -> Card {
    let mut sub = vec![format!("{:?}", a.album_type).replace("Ep", "EP")];
    sub.push(a.artists.iter().map(|x| x.name.as_str()).collect::<Vec<_>>().join(", "));
    if let Some(y) = a.year {
        sub.push(y.to_string());
    }
    Card {
        title: a.name.clone(),
        subtitle: sub.join(" • "),
        thumbs: model::thumbs(&a.cover),
        target: Target::Album(a.id.clone()),
        round: false,
    }
}

fn playlist_card(p: &MusicPlaylistItem) -> Card {
    let mut sub = vec!["Playlist".to_owned()];
    if let Some(c) = &p.channel {
        sub.push(c.name.clone());
    }
    Card {
        title: p.name.clone(),
        subtitle: sub.join(" • "),
        thumbs: model::thumbs(&p.thumbnail),
        target: Target::Playlist(p.id.clone()),
        round: false,
    }
}

fn artist_card(a: &ArtistItem) -> Card {
    Card {
        title: a.name.clone(),
        subtitle: a.subscriber_count.map(|n| format!("{} subscribers", compact(n))).unwrap_or_else(|| "Artist".into()),
        thumbs: model::thumbs(&a.avatar),
        target: Target::Artist(a.id.clone()),
        round: true,
    }
}

pub fn compact(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}K", n as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{:.1}B", n as f64 / 1e9),
    }
}

/// Larger art URL for the background / now-playing view.
pub fn art_url(t: &Track) -> Option<String> {
    model::pick(&t.thumbs, 120).map(|u| sized(u, 544))
}
