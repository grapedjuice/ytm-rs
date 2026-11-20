//! Songs saved to disk: the audio as YouTube serves it (AAC in an .m4a, at the best
//! quality the account gets), tagged with title, artists, album and cover, in the
//! downloads folder. A downloaded song plays from its file, so it works offline.
//!
//! `downloads.json` in the data dir maps video ids to their files; a file that was
//! moved or deleted just stops counting as downloaded. A downloaded song's lyrics are
//! kept in the data dir's `lyrics` folder, every source's, so they show offline too.
//! The file's own lyrics tag, for other players, has the source the listener goes by
//! for that song: timed (LRC) when it has timing, unless the setting says otherwise.
//!
//! With `per_list` on, an album's or playlist's own download puts its songs in a folder
//! of their own under the downloads folder. A song is saved once: one downloaded already
//! (loose, or in another list's folder) stays where it is.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};

use serde::{Deserialize, Serialize};

use crate::lyrics::{LyricsSet, Provider};
use crate::model::Track;
use crate::stream::Shared;

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    path: PathBuf,
    duration_ms: u64,
    loudness_db: Option<f32>,
    /// A song version: the music video it was downloaded for, and the song, so that
    /// video plays it from this file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stands_for: Option<String>,
    /// More music videos this file is the song version of.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    videos: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    song: Option<Track>,
    /// The song as listed when it was downloaded, for Library › Downloads. Files from
    /// before it was kept are read from their tags instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    track: Option<Track>,
    /// The album or playlist whose folder it was saved in (`ListFolder::id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    list: Option<String>,
}

/// An album or playlist whose songs go in a folder of their own: its id, and the name
/// its folder takes (`list_name`).
#[derive(Clone, Debug, PartialEq)]
pub struct ListFolder {
    pub id: String,
    pub name: String,
}

struct State {
    index: HashMap<String, Entry>,
    /// Where the index is saved (the data dir's downloads.json).
    file: PathBuf,
    folder: PathBuf,
    /// Albums and playlists get a folder each (the setting).
    per_list: bool,
    /// What goes in a file's lyrics tag (the setting).
    file_lyrics: FileLyrics,
    /// Songs whose file wasn't there when last looked for. They stay in the index: a
    /// folder on a drive that's away has them back when it's back.
    gone: HashSet<String>,
    /// The songs saved this session, in the order they were: Library › Downloads takes
    /// on the new ones (`saved_since`) without looking at every file again.
    saved: Vec<String>,
}

static STATE: LazyLock<RwLock<State>> =
    LazyLock::new(|| RwLock::new(State { index: HashMap::new(), file: PathBuf::new(), folder: default_folder(), per_list: false, file_lyrics: FileLyrics::Synced, gone: HashSet::new(), saved: Vec::new() }));

/// Music\ytm-rs.
pub fn default_folder() -> PathBuf {
    let base = directories::UserDirs::new()
        .and_then(|u| u.audio_dir().map(Path::to_path_buf).or_else(|| Some(u.home_dir().join("Music"))))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("ytm-rs")
}

/// What a downloaded file's lyrics tag holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileLyrics {
    /// Timed lyrics as LRC, where the source has timing; plain words where it doesn't.
    Synced,
    Plain,
    Off,
}

impl FileLyrics {
    pub const ALL: [Self; 3] = [Self::Synced, Self::Plain, Self::Off];

    pub fn key(self) -> &'static str {
        match self {
            Self::Synced => "synced",
            Self::Plain => "plain",
            Self::Off => "off",
        }
    }

    pub fn from_key(key: &str) -> Self {
        Self::ALL.into_iter().find(|m| m.key() == key).unwrap_or(Self::Synced)
    }
}

/// Load the index; `folder` is the saved setting (empty for the default), `per_list` the
/// folder-per-list one, `file_lyrics` the lyrics-in-the-file one.
pub fn init(data_dir: &Path, folder: &str, per_list: bool, file_lyrics: FileLyrics) {
    let file = data_dir.join("downloads.json");
    let index = match std::fs::read(&file) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            // Kept aside: the next save would overwrite the only record of these songs.
            log::warn!("downloads index unreadable, starting a new one: {e}");
            let _ = std::fs::rename(&file, file.with_extension("json.bad"));
            HashMap::new()
        }),
        Err(_) => HashMap::new(),
    };
    let mut st = STATE.write().unwrap();
    st.index = index;
    st.file = file;
    st.folder = if folder.is_empty() { default_folder() } else { PathBuf::from(folder) };
    st.per_list = per_list;
    st.file_lyrics = file_lyrics;
    drop(st);
    // Which files are gone: disk work, off the UI thread.
    std::thread::spawn(|| {
        let entries: Vec<(String, PathBuf)> = STATE.read().unwrap().index.iter().map(|(id, e)| (id.clone(), e.path.clone())).collect();
        set_gone(entries.into_iter().filter(|(_, p)| !p.is_file()).collect());
    });
}

/// Remember these songs' files as gone (unless one's been saved again meanwhile).
fn set_gone(missing: Vec<(String, PathBuf)>) {
    let mut st = STATE.write().unwrap();
    let gone = missing.into_iter().filter(|(id, p)| st.index.get(id).is_some_and(|e| &e.path == p && !p.is_file())).map(|(id, _)| id).collect();
    st.gone = gone;
    LOOKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

static LOOKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many times the files have been looked for: what was worked out from an earlier look
/// is worked out again.
pub fn looks() -> u64 {
    LOOKS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Save the index whole or not at all: a crash mid-write doesn't lose it. Called after
/// a change, with `STATE` let go, so the file is written without holding anyone up. One
/// write at a time, each of the index as it is then: the last write has the last change.
fn write_index() {
    static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let (json, file) = {
        let st = STATE.read().unwrap();
        (serde_json::to_vec(&st.index), st.file.clone())
    };
    let Ok(json) = json else { return };
    let tmp = file.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, json).and_then(|_| std::fs::rename(&tmp, &file)) {
        log::warn!("couldn't save the downloads index: {e}");
    }
}

pub fn folder() -> PathBuf {
    STATE.read().unwrap().folder.clone()
}

pub fn set_folder(folder: PathBuf) {
    STATE.write().unwrap().folder = folder;
}

/// Whether an album's or playlist's own download puts it in a folder of its own.
pub fn per_list() -> bool {
    STATE.read().unwrap().per_list
}

pub fn set_per_list(on: bool) {
    STATE.write().unwrap().per_list = on;
}

pub fn file_lyrics() -> FileLyrics {
    STATE.read().unwrap().file_lyrics
}

/// Change what files' lyrics tags hold, and retag every downloaded song to match (in the
/// background; only files whose tag changes are written).
pub fn set_file_lyrics(m: FileLyrics) {
    let ids: Vec<String> = {
        let mut st = STATE.write().unwrap();
        if st.file_lyrics == m {
            return;
        }
        st.file_lyrics = m;
        st.index.keys().filter(|id| !st.gone.contains(*id)).cloned().collect()
    };
    std::thread::spawn(move || {
        for id in ids {
            retag(&id);
        }
    });
}

/// The listener's lyrics source order and per-song picks, which the files' lyrics follow.
struct Choice {
    order: Vec<(Provider, bool)>,
    pins: HashMap<String, Provider>,
}

static CHOICE: LazyLock<RwLock<Choice>> = LazyLock::new(|| RwLock::new(Choice { order: Provider::ALL.iter().map(|p| (*p, true)).collect(), pins: HashMap::new() }));

/// Take on the listener's source order and picks (at start, and when they change).
pub fn set_lyric_choice(order: Vec<(Provider, bool)>, pins: HashMap<String, Provider>) {
    *CHOICE.write().unwrap() = Choice { order, pins };
}

/// After the listener picks another source for a song: its file's lyrics follow, when
/// it's downloaded (in the background).
pub fn lyrics_picked(video_id: &str) {
    if path(video_id).is_none() {
        return;
    }
    let id = video_id.to_owned();
    std::thread::spawn(move || retag(&id));
}

/// Ask for a new folder (the system picker, on its own thread).
pub fn pick_folder(ctx: &egui::Context) {
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        if let Some(dir) = rfd::FileDialog::new().set_title("Download folder").set_directory(folder()).pick_folder() {
            set_folder(dir);
            ctx.request_repaint();
        }
    });
}

pub fn open_folder() {
    let dir = folder();
    let _ = std::fs::create_dir_all(&dir);
    open_dir(&dir);
}

fn open_dir(dir: &Path) {
    // Quoted: unquoted, explorer reads a comma in it as the end of the folder.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let dir: PathBuf = dir.components().collect();
        let _ = std::process::Command::new("explorer").raw_arg(format!("\"{}\"", dir.display())).spawn();
    }
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(dir).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
}

/// Open the song's folder in Explorer with the file selected.
pub fn show_file(path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", path.display())).spawn();
    }
    // macOS: Finder reveals it.
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    // Linux: the file manager's ShowItems selects it (Nautilus, Dolphin, Nemo, …);
    // without one, just open the folder.
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // dbus-send splits an array at commas: one in the name is escaped.
        let uri = url::Url::from_file_path(path).map(|u| u.to_string().replace(',', "%2C")).unwrap_or_default();
        let shown = std::process::Command::new("dbus-send")
            .args(["--session", "--print-reply", "--dest=org.freedesktop.FileManager1", "/org/freedesktop/FileManager1", "org.freedesktop.FileManager1.ShowItems"])
            .arg(format!("array:string:{uri}"))
            .arg("string:")
            .output()
            .is_ok_and(|o| o.status.success());
        if !shown && let Some(dir) = path.parent() {
            let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
        }
    }
}

/// Show an album's or playlist's downloads (`tracks`: its songs, when known): its own
/// folder when it has one, else the first of its songs, selected, in the folder that has
/// most of them. Disk work, on its own thread.
pub fn show_list(id: &str, tracks: &[Track]) {
    let (id, tracks) = (id.to_owned(), tracks.to_vec());
    std::thread::spawn(move || {
        let (own, files) = {
            let st = STATE.read().unwrap();
            let own: Vec<PathBuf> = st.index.values().filter(|e| e.list.as_deref() == Some(id.as_str())).map(|e| e.path.clone()).collect();
            let files: Vec<PathBuf> = tracks.iter().filter_map(|t| entry_for(&st, t)).map(|e| e.path.clone()).collect();
            (own, files)
        };
        let here = |v: Vec<PathBuf>| v.into_iter().filter(|p| p.is_file()).collect();
        match to_show(here(own), here(files), &folder()) {
            Some(Shown::Folder(dir)) => open_dir(&dir),
            Some(Shown::File(file)) => show_file(&file),
            // Its files went since the last look: the downloads folder.
            None => open_dir(&folder()),
        }
    });
}

#[derive(Debug, PartialEq)]
enum Shown {
    Folder(PathBuf),
    File(PathBuf),
}

/// What Show in folder shows of a list: the folder its songs were saved in (`own`), one
/// right under the downloads folder `root` before one elsewhere; else the first of its
/// songs' files (`files`, in its order) in the folder that has most of them.
fn to_show(own: Vec<PathBuf>, files: Vec<PathBuf>, root: &Path) -> Option<Shown> {
    let dirs = || own.iter().filter_map(|p| p.parent());
    if let Some(dir) = dirs().filter(|d| d.parent().is_some_and(|r| same_dir(r, root))).min().or_else(|| dirs().min()) {
        return Some(Shown::Folder(dir.to_path_buf()));
    }
    // Each file once (two rows can be one song), in the list's order.
    let mut seen = HashSet::new();
    let files: Vec<&PathBuf> = files.iter().filter(|f| seen.insert(*f)).collect();
    let mut count: HashMap<&Path, usize> = HashMap::new();
    for dir in files.iter().filter_map(|f| f.parent()) {
        *count.entry(dir).or_default() += 1;
    }
    // Most files, then the first folder by name: the same one every time.
    let (dir, _) = count.into_iter().max_by(|(a, x), (b, y)| x.cmp(y).then(b.cmp(a)))?;
    files.into_iter().find(|f| f.parent() == Some(dir)).map(|f| Shown::File(f.clone()))
}

/// The song's file, if it's downloaded and still there.
pub fn path(video_id: &str) -> Option<PathBuf> {
    STATE.read().unwrap().index.get(video_id).map(|e| e.path.clone()).filter(|p| p.is_file())
}

/// Whether all these songs are downloaded, as last looked (no look at the files now, so
/// it's quick enough for the UI thread; startup and Library › Downloads look).
/// A music video counts when its song version is.
pub fn all_listed(tracks: &[Track]) -> bool {
    let st = STATE.read().unwrap();
    let videos = song_versions(&st, tracks);
    tracks.iter().all(|t| listed_now(&st, &videos, t))
}

/// Whether any of these songs are downloaded, as last looked (see `all_listed`).
pub fn any_listed(tracks: &[Track]) -> bool {
    let st = STATE.read().unwrap();
    let videos = song_versions(&st, tracks);
    tracks.iter().any(|t| listed_now(&st, &videos, t))
}

/// Whether songs were saved in the album or playlist `id`'s own folder, as last looked.
pub fn has_list_folder(id: &str) -> bool {
    let st = STATE.read().unwrap();
    st.index.iter().any(|(song, e)| e.list.as_deref() == Some(id) && !st.gone.contains(song))
}

/// The music videos whose song versions are downloaded, when there are any among `tracks`.
fn song_versions<'a>(st: &'a State, tracks: &[Track]) -> HashSet<&'a str> {
    match tracks.iter().any(|t| t.music_video) {
        true => st.index.iter().filter(|(id, _)| !st.gone.contains(*id)).flat_map(|(_, e)| e.stands_for.iter().chain(&e.videos)).map(String::as_str).collect(),
        false => HashSet::new(),
    }
}

fn listed_now(st: &State, videos: &HashSet<&str>, t: &Track) -> bool {
    (st.index.contains_key(&t.id) && !st.gone.contains(&t.id)) || (t.music_video && videos.contains(t.id.as_str()))
}

/// The entry of a song's file, or of a music video's song version, unless it was gone
/// when last looked.
fn entry_for<'a>(st: &'a State, t: &Track) -> Option<&'a Entry> {
    let here = |(id, e): (&String, &'a Entry)| (!st.gone.contains(id)).then_some(e);
    let stands_for = |e: &Entry| e.stands_for.as_deref() == Some(t.id.as_str()) || e.videos.contains(&t.id);
    st.index.get_key_value(&t.id).and_then(here).or_else(|| match t.music_video {
        true => st.index.iter().filter(|(_, e)| stands_for(e)).find_map(here),
        false => None,
    })
}

/// The downloaded song version of the music video `video_id`, if its file is still there.
pub fn song_for(video_id: &str) -> Option<Track> {
    let st = STATE.read().unwrap();
    st.index
        .values()
        .find(|e| (e.stands_for.as_deref() == Some(video_id) || e.videos.iter().any(|v| v == video_id)) && e.path.is_file())
        .and_then(|e| e.song.clone().or_else(|| e.track.clone()))
}

/// The downloaded song `song_id` is the song version of the music video `video_id` too.
/// Writes the index: disk work.
pub fn link_video(song_id: &str, video_id: &str) {
    {
        let mut st = STATE.write().unwrap();
        let Some(e) = st.index.get_mut(song_id) else { return };
        if !add_video(&mut e.stands_for, &mut e.videos, video_id) {
            return;
        }
        if e.song.is_none() {
            e.song = e.track.clone();
        }
    }
    write_index();
}

/// The song's file if it's downloaded and still there; then, given `for_video`, it's
/// that music video's song version too. Disk work.
pub fn reuse(song_id: &str, for_video: Option<&str>) -> Option<PathBuf> {
    let path = path(song_id)?;
    if let Some(v) = for_video {
        link_video(song_id, v);
    }
    Some(path)
}

/// Make a song version stand for the music video `video_id` too: the first one it's
/// for, else one more. False when it did already.
fn add_video(first: &mut Option<String>, videos: &mut Vec<String>, video_id: &str) -> bool {
    if first.as_deref() == Some(video_id) || videos.iter().any(|v| v == video_id) {
        return false;
    }
    match first {
        None => *first = Some(video_id.to_owned()),
        Some(_) => videos.push(video_id.to_owned()),
    }
    true
}

/// Every downloaded song whose file is still there, newest first: Library › Downloads.
/// A song version is listed as the song (its file plays for the music video too).
pub fn all() -> Vec<Track> {
    let entries: Vec<(String, Entry)> = STATE.read().unwrap().index.iter().map(|(id, e)| (id.clone(), e.clone())).collect();
    let mut missing = Vec::new();
    let mut found: Vec<(std::time::SystemTime, Track)> = entries
        .into_iter()
        .filter_map(|(id, e)| {
            let Some(meta) = std::fs::metadata(&e.path).ok().filter(|m| m.is_file()) else {
                missing.push((id, e.path));
                return None;
            };
            let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            Some((modified, listed(id, e)))
        })
        .collect();
    set_gone(missing);
    found.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    found.into_iter().map(|(_, t)| t).collect()
}

/// How many songs have been saved this session. Read before `all`: `saved_since` from
/// it gives the ones saved after.
pub fn saves() -> usize {
    STATE.read().unwrap().saved.len()
}

/// The songs saved this session after the first `from`, newest first (each once), and
/// where to ask from next time. From the index alone, no look at the disk: quick enough
/// for the UI thread as each song of a run lands.
pub fn saved_since(from: usize) -> (usize, Vec<Track>) {
    let st = STATE.read().unwrap();
    let mut seen = HashSet::new();
    let tracks = st
        .saved
        .get(from..)
        .unwrap_or_default()
        .iter()
        .rev()
        .filter(|id| seen.insert(id.as_str()))
        .filter_map(|id| Some(listed(id.clone(), st.index.get(id)?.clone())))
        .collect();
    (st.saved.len(), tracks)
}

/// A downloaded song as Library › Downloads lists it: as the song (its file plays for the
/// music video too).
fn listed(id: String, e: Entry) -> Track {
    let mut t = e.track.or(e.song).unwrap_or_else(|| from_tags(&id, &e.path));
    t.id = id;
    (t.music_video, t.video) = (false, false);
    if e.duration_ms > 0 {
        t.duration = Some((e.duration_ms / 1000) as u32);
    }
    t
}

/// A song downloaded before the index kept its details: title, artists and album from
/// the file's tags, else the file's name.
fn from_tags(id: &str, path: &Path) -> Track {
    let tag = mp4ameta::Tag::read_from_path(path).ok();
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| id.to_owned());
    let link = |name: &str| crate::model::Link { name: name.to_owned(), id: None };
    Track {
        id: id.to_owned(),
        title: tag.as_ref().and_then(|t| t.title()).map(str::to_owned).unwrap_or(name),
        artists: tag.as_ref().map(|t| t.artists().map(link).collect()).unwrap_or_default(),
        album: tag.as_ref().and_then(|t| t.album()).map(link),
        track_nr: tag.as_ref().and_then(|t| t.track_number()),
        explicit: tag.as_ref().is_some_and(|t| t.advisory_rating() == Some(mp4ameta::AdvisoryRating::Explicit)),
        ..Default::default()
    }
}

/// A downloaded song that was saved without its plays (the list it came from had none).
pub fn lacks_plays(video_id: &str) -> bool {
    STATE.read().unwrap().index.get(video_id).and_then(|e| e.track.as_ref()).is_some_and(|t| t.plays.is_none())
}

/// Keep a downloaded song's plays, looked up after it was saved.
pub fn set_plays(video_id: &str, plays: u64) {
    {
        let mut st = STATE.write().unwrap();
        let Some(t) = st.index.get_mut(video_id).and_then(|e| e.track.as_mut()) else { return };
        t.plays = Some(plays);
    }
    write_index();
}

/// A downloaded song's length in seconds, as its file has it.
pub fn duration(video_id: &str) -> Option<u32> {
    let ms = STATE.read().unwrap().index.get(video_id).map(|e| e.duration_ms)?;
    (ms > 0).then_some((ms / 1000) as u32)
}

/// Where a song's lyrics are kept: `<data dir>/lyrics/<video id>.json`.
fn lyrics_file(video_id: &str) -> Option<PathBuf> {
    let id: String = video_id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    let dir = STATE.read().unwrap().file.parent()?.join("lyrics");
    (!id.is_empty()).then(|| dir.join(format!("{id}.json")))
}

/// The lyrics saved for a downloaded song, every source that had them.
pub fn saved_lyrics(video_id: &str) -> Option<LyricsSet> {
    path(video_id)?;
    let set: LyricsSet = serde_json::from_slice(&std::fs::read(lyrics_file(video_id)?).ok()?).ok()?;
    (!set.sources.is_empty()).then_some(set)
}

/// Keep a downloaded song's lyrics for offline, and bring the file's lyrics tag (for
/// other players) up to date. Nothing for a song that isn't downloaded, or lyrics that
/// are empty.
pub fn save_lyrics(video_id: &str, set: &LyricsSet) {
    let (Some(audio), Some(file)) = (path(video_id), lyrics_file(video_id)) else { return };
    if set.sources.is_empty() {
        return;
    }
    let saved = file.parent().map(std::fs::create_dir_all).transpose().and_then(|_| std::fs::write(&file, serde_json::to_vec(set).unwrap_or_default()));
    if let Err(e) = saved {
        log::warn!("couldn't save the lyrics of {video_id}: {e}");
        return;
    }
    tag_lyrics(&audio, video_id, set);
}

/// Bring a downloaded song's lyrics tag in line with its saved lyrics.
fn retag(video_id: &str) {
    if let (Some(audio), Some(set)) = (path(video_id), saved_lyrics(video_id)) {
        tag_lyrics(&audio, video_id, &set);
    }
}

/// Write the lyrics tag `video_id`'s file should have, when it has something else.
fn tag_lyrics(audio: &Path, video_id: &str, set: &LyricsSet) {
    // Only .m4a files are tagged (title, cover and all).
    if audio.extension().is_none_or(|e| e != "m4a") {
        return;
    }
    let want = {
        let c = CHOICE.read().unwrap();
        tag_text(set, crate::lyrics::choose(&c.order, &c.pins, video_id, set), file_lyrics())
    };
    // One write to a file at a time: a new pick and a play can both retag it.
    static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let written = mp4ameta::Tag::read_from_path(audio).and_then(|mut tag| {
        if tag.lyrics() == want.as_deref() {
            return Ok(());
        }
        match &want {
            Some(w) => tag.set_lyrics(w),
            None => tag.remove_lyrics(),
        }
        tag.write_to_path(audio)
    });
    if let Err(e) = written {
        log::warn!("couldn't tag the lyrics of {}: {e}", audio.display());
    }
}

/// What a file's lyrics tag holds: the `source`'s lyrics, as the setting says.
fn tag_text(set: &LyricsSet, source: Option<Provider>, mode: FileLyrics) -> Option<String> {
    let l = set.get(source?)?;
    let text = match mode {
        FileLyrics::Synced => crate::lyrics::to_lrc(l),
        FileLyrics::Plain => crate::lyrics::plain_words(l),
        FileLyrics::Off => return None,
    };
    (!text.trim().is_empty()).then_some(text)
}

/// A downloaded song's audio, as a complete stream buffer for playback.
pub fn open(video_id: &str) -> Option<Arc<Shared>> {
    let entry = STATE.read().unwrap().index.get(video_id).cloned()?;
    let data = std::fs::read(&entry.path).ok()?;
    log::info!("{video_id}: playing the downloaded file");
    Some(Shared::from_bytes(data, entry.duration_ms, entry.loudness_db))
}

/// Write a downloaded song into the folder (`list`'s own, given one) and remember it.
/// Returns the file. `stands_for`: the music video `t` is the song version of.
pub fn save(t: &Track, audio: &[u8], duration_ms: u64, loudness_db: Option<f32>, cover: Option<Vec<u8>>, stands_for: Option<String>, list: Option<&ListFolder>) -> anyhow::Result<PathBuf> {
    // One save at a time: two songs of the same name would otherwise both find it free,
    // and two lists of the same name the same folder.
    static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    // A copy: the disk work below doesn't hold up the rest of the app.
    let (mine, dirs, root) = {
        let st = STATE.read().unwrap();
        (st.index.get(&t.id).map(|e| (e.path.clone(), e.list.clone())), folders(&st.index), st.folder.clone())
    };
    // Saved again, a song of a list's folder goes back in it (it stays that list's).
    let kept = mine.as_ref().and_then(|(p, list)| Some((p.parent()?, list.as_ref()?))).filter(|(d, _)| d.parent().is_some_and(|r| same_dir(r, &root)));
    let (folder, list) = match (kept, list) {
        (Some((dir, id)), _) => (dir.to_path_buf(), Some(id.clone())),
        (None, Some(l)) => (list_dir(&dirs, &root, l), Some(l.id.clone())),
        (None, None) => (root, None),
    };
    std::fs::create_dir_all(&folder)?;
    let ext = if audio.get(4..8) == Some(b"ftyp") { "m4a" } else { "webm" };
    // A file already there for this song is replaced; another song's isn't.
    let mine = mine.map(|(p, _)| p);
    let stem = file_stem(t);
    let mut path = folder.join(format!("{stem}.{ext}"));
    let mut n = 2;
    while path.exists() && Some(&path) != mine.as_ref() {
        path = folder.join(format!("{stem} ({n}).{ext}"));
        n += 1;
    }
    let part = path.with_extension(format!("{ext}.part"));
    // YouTube's files say they're DASH segments ("dash" brand); as a file of its own,
    // call it audio, which some players and Explorer go by.
    let mut audio = audio.to_vec();
    if audio.get(8..12) == Some(b"dash") {
        audio[8..12].copy_from_slice(b"M4A ");
    }
    std::fs::write(&part, &audio)?;
    if ext == "m4a"
        && let Err(e) = tag(&part, t, cover)
    {
        // Untagged audio still plays; keep it.
        log::warn!("couldn't tag {}: {e:#}", path.display());
    }
    std::fs::rename(&part, &path)?;
    {
        let mut st = STATE.write().unwrap();
        // Saved again, it stays the song version of the videos it was already.
        let (mut first, mut videos) = st.index.get(&t.id).map(|e| (e.stands_for.clone(), e.videos.clone())).unwrap_or_default();
        if let Some(v) = stands_for {
            add_video(&mut first, &mut videos, &v);
        }
        let song = first.is_some().then(|| t.clone());
        let track = Some(t.clone());
        st.gone.remove(&t.id);
        st.index.insert(t.id.clone(), Entry { path: path.clone(), duration_ms, loudness_db, stands_for: first, videos, song, track, list });
        st.saved.push(t.id.clone());
    }
    write_index();
    Ok(path)
}

/// "Artists - Title", safe as a Windows file name.
fn file_stem(t: &Track) -> String {
    let artists = t.artist_line();
    let name = if artists.is_empty() { t.title.clone() } else { format!("{artists} - {}", t.title) };
    let clean = safe_name(&name, 150);
    if clean.is_empty() { t.id.clone() } else { clean }
}

/// The name an album's or playlist's folder takes: "Artist - Album" for an album with an
/// artist on all its songs (the first song's first such), else its title.
pub fn list_name(title: &str, album: bool, tracks: &[Track]) -> String {
    let on_all = |a: &str| tracks.iter().all(|t| t.artists.iter().any(|b| b.name.trim() == a));
    let artist = tracks.first().and_then(|t| t.artists.iter().map(|a| a.name.trim()).find(|a| !a.is_empty() && on_all(a)));
    match artist {
        Some(a) if album => format!("{a} - {title}"),
        _ => title.to_owned(),
    }
}

/// The folder for `list`'s songs, right under `root`: the one its songs were saved in
/// before (renamed since, it keeps it), else one by its name that's free, "Name (2)" and
/// on when it's taken (on disk, or by another list's songs).
fn list_dir(dirs: &HashSet<(PathBuf, Option<String>)>, root: &Path, list: &ListFolder) -> PathBuf {
    let before = dirs.iter().filter(|(d, l)| l.as_deref() == Some(list.id.as_str()) && d.parent().is_some_and(|r| same_dir(r, root))).map(|(d, _)| d).min();
    if let Some(dir) = before {
        return dir.to_path_buf();
    }
    // Short, so its songs' paths stay within Windows' 260 characters most programs need.
    let name = Some(safe_name(&list.name, 60)).filter(|n| !n.is_empty()).unwrap_or_else(|| safe_name(&list.id, 60));
    // An empty folder isn't (one left by a save that failed).
    let used = |dir: &Path| dir.exists() && !dir.read_dir().is_ok_and(|mut d| d.next().is_none());
    let taken = |dir: &Path| used(dir) || dirs.iter().any(|(d, _)| same_dir(d, dir));
    let mut dir = root.join(&name);
    let mut n = 2;
    while taken(&dir) {
        dir = root.join(format!("{name} ({n})"));
        n += 1;
    }
    dir
}

/// The folders songs are in, each with the list it's for (none: not a list's).
fn folders(index: &HashMap<String, Entry>) -> HashSet<(PathBuf, Option<String>)> {
    index.values().filter_map(|e| Some((e.path.parent()?.to_path_buf(), e.list.clone()))).collect()
}

/// The same folder: on Windows, whatever the case of its letters.
/// Part by part, so "D:/Music/" is "D:\Music" too.
fn same_dir(a: &Path, b: &Path) -> bool {
    match cfg!(windows) {
        true => {
            let parts = |p: &Path| p.components().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).collect::<Vec<_>>();
            parts(a) == parts(b)
        }
        false => a.components().eq(b.components()),
    }
}

/// `name` safe as a Windows file or folder name, of at most `max` characters (and 200
/// bytes, for Linux's 255 with room for " (2).webm.part"): none that Windows refuses, no dot or space at the end, and not a name it keeps for devices
/// (`CON`, `NUL`, `COM1`, …). Empty when nothing's left.
fn safe_name(name: &str, max: usize) -> String {
    let clean: String = name.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c }).collect();
    let mut bytes = 0;
    let clean: String = clean.trim().chars().take(max).take_while(|c| {
        bytes += c.len_utf8();
        bytes <= 200
    }).collect();
    let clean = clean.trim_end_matches(['.', ' ']);
    // Windows goes by the part before the first dot ("nul.m4a" is NUL too).
    let base = clean.split('.').next().unwrap_or_default();
    let device = base.trim_end().to_ascii_uppercase();
    let numbered = |p: &str| {
        device.strip_prefix(p).is_some_and(|n| {
            let mut c = n.chars();
            matches!((c.next(), c.next()), (Some('1'..='9' | '¹' | '²' | '³'), None))
        })
    };
    match matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL") || numbered("COM") || numbered("LPT") {
        true => format!("{base}_{}", &clean[base.len()..]),
        false => clean.to_owned(),
    }
}

fn tag(path: &Path, t: &Track, cover: Option<Vec<u8>>) -> anyhow::Result<()> {
    let mut tag = mp4ameta::Tag::read_from_path(path)?;
    tag.set_title(&t.title);
    for (i, a) in t.artists.iter().enumerate() {
        if i == 0 {
            tag.set_artist(&a.name);
        } else {
            tag.add_artist(&a.name);
        }
    }
    if let Some(a) = t.artists.first() {
        tag.set_album_artist(&a.name);
    }
    if let Some(album) = &t.album {
        tag.set_album(&album.name);
    }
    if let Some(n) = t.track_nr {
        tag.set_track_number(n);
    }
    // The "E" other players show too.
    if t.explicit {
        tag.set_advisory_rating(mp4ameta::AdvisoryRating::Explicit);
    }
    let img = cover.and_then(|c| match c.get(..4) {
        Some([0xff, 0xd8, ..]) => Some(mp4ameta::Img::jpeg(c)),
        Some([0x89, b'P', b'N', b'G']) => Some(mp4ameta::Img::png(c)),
        _ => None,
    });
    if let Some(img) = img {
        tag.set_artwork(img);
    }
    tag.write_to_path(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Link;

    #[test]
    fn file_names_are_safe_on_windows() {
        let track = |title: &str, artists: Vec<Link>| Track {
            id: "abc".into(),
            title: title.into(),
            artists,
            ..Default::default()
        };
        let t = track("What? / Why: \"Now\"...", vec![Link { name: "AC/DC".into(), id: None }]);
        assert_eq!(file_stem(&t), "AC_DC - What_ _ Why_ _Now_");
        assert_eq!(file_stem(&track("...", Vec::new())), "abc");
        assert_eq!(file_stem(&track("Con", Vec::new())), "Con_");
    }

    #[test]
    fn folder_names_are_safe_on_windows() {
        assert_eq!(safe_name("Drama: Act I?", 100), "Drama_ Act I_");
        assert_eq!(safe_name("  Mix.  ", 100), "Mix");
        assert_eq!(safe_name("nul", 100), "nul_");
        assert_eq!(safe_name("con. air", 100), "con_. air");
        assert_eq!(safe_name("COM3", 100), "COM3_");
        assert_eq!(safe_name("COM0", 100), "COM0");
        assert_eq!(safe_name("com¹", 100), "com¹_");
        // Long names in a script of wide letters stay within 200 bytes.
        assert_eq!(safe_name(&"漢".repeat(90), 100).len(), 198);
        assert_eq!(safe_name("Console", 100), "Console");
        // Cut short, it doesn't end in a space or a dot either.
        assert_eq!(safe_name("ab. cd", 4), "ab");
        assert_eq!(safe_name("???", 100), "___");
        assert_eq!(safe_name(" . ", 100), "");
    }

    #[test]
    fn albums_are_named_with_their_artist() {
        let by = |artist: &str| Track { artists: vec![Link { name: artist.into(), id: None }], ..Default::default() };
        assert_eq!(list_name("Greatest Hits", true, &[by("Queen"), by("Queen")]), "Queen - Greatest Hits");
        assert_eq!(list_name("Now 100", true, &[by("A"), by("B")]), "Now 100");
        // A guest first on one song: the album's artist all the same.
        let guest = Track { artists: vec![Link { name: "Guest".into(), id: None }, Link { name: "Queen".into(), id: None }], ..Default::default() };
        assert_eq!(list_name("Hits", true, &[by("Queen"), guest]), "Queen - Hits");
        assert_eq!(list_name("Mix", false, &[by("Queen")]), "Mix");
        assert_eq!(list_name("Empty", true, &[]), "Empty");
    }

    fn entry(path: PathBuf, list: Option<&str>) -> Entry {
        Entry { path, duration_ms: 0, loudness_db: None, stands_for: None, videos: Vec::new(), song: None, track: None, list: list.map(str::to_owned) }
    }

    #[test]
    fn each_list_gets_a_folder_of_its_own() {
        let root = std::env::temp_dir().join(format!("ytm-rs-list-dir-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let list = |id: &str, name: &str| ListFolder { id: id.into(), name: name.into() };
        let mut index = HashMap::new();

        // A new list: its name.
        assert_eq!(list_dir(&folders(&index), &root, &list("PL1", "Mix: 1")), root.join("Mix_ 1"));
        // A loose song is no list's folder.
        index.insert("loose".into(), entry(root.join("a.m4a"), None));
        assert_eq!(list_dir(&folders(&index), &root, &list("PL1", "Mix")), root.join("Mix"));
        // Its songs went there: so do the next, renamed since or not.
        index.insert("s1".into(), entry(root.join("Mix").join("s1.m4a"), Some("PL1")));
        assert_eq!(list_dir(&folders(&index), &root, &list("PL1", "Renamed")), root.join("Mix"));
        // Another list of the same name.
        assert_eq!(list_dir(&folders(&index), &root, &list("PL2", "Mix")), root.join("Mix (2)"));
        // A folder that's there already and isn't a list's.
        std::fs::create_dir_all(root.join("Mine")).unwrap();
        std::fs::write(root.join("Mine").join("x.txt"), "").unwrap();
        assert_eq!(list_dir(&folders(&index), &root, &list("PL3", "Mine")), root.join("Mine (2)"));
        // An empty one is free to take.
        std::fs::create_dir_all(root.join("Left")).unwrap();
        assert_eq!(list_dir(&folders(&index), &root, &list("PL5", "Left")), root.join("Left"));
        // A list's folder under a downloads folder that's no longer the one: a new one here.
        index.insert("s2".into(), entry(root.join("old").join("Jazz").join("s2.m4a"), Some("PL4")));
        assert_eq!(list_dir(&folders(&index), &root, &list("PL4", "Jazz")), root.join("Jazz"));
        // A folder by the same name but for case is the same one on Windows.
        #[cfg(windows)]
        {
            index.insert("s3".into(), entry(root.join("Rock").join("s3.m4a"), Some("PL6")));
            assert_eq!(list_dir(&folders(&index), &root, &list("PL7", "rock")), root.join("rock (2)"));
        }
        // No name left: its id.
        assert_eq!(list_dir(&folders(&index), &root, &list("PL5", "...")), root.join("PL5"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn show_in_folder_opens_a_lists_folder_else_selects_its_songs() {
        let root = PathBuf::from("music");
        let (mix, loose) = (root.join("Mix"), |n: &str| root.join(n));
        // Its own folder, one under the downloads folder first.
        let own = vec![root.join("old").join("Mix").join("a.m4a"), mix.join("b.m4a")];
        assert_eq!(to_show(own, vec![loose("c.m4a")], &root), Some(Shown::Folder(mix.clone())));
        assert_eq!(to_show(vec![PathBuf::from("elsewhere/Mix/a.m4a")], Vec::new(), &root), Some(Shown::Folder(PathBuf::from("elsewhere/Mix"))));
        // Loose songs: the list's first in the folder with most of them, each file once.
        let files = vec![mix.join("x.m4a"), mix.join("x.m4a"), mix.join("x.m4a"), loose("b.m4a"), loose("a.m4a")];
        assert_eq!(to_show(Vec::new(), files, &root), Some(Shown::File(loose("b.m4a"))));
        assert_eq!(to_show(Vec::new(), Vec::new(), &root), None);
    }

    #[test]
    fn a_song_version_stands_for_each_video_once() {
        let (mut first, mut videos) = (None, Vec::new());
        assert!(add_video(&mut first, &mut videos, "v1"));
        assert!(add_video(&mut first, &mut videos, "v2"));
        assert!(!add_video(&mut first, &mut videos, "v1"));
        assert!(!add_video(&mut first, &mut videos, "v2"));
        assert_eq!((first.as_deref(), videos), (Some("v1"), vec!["v2".to_owned()]));
    }

    /// The only test that sets up `STATE`: saving, linking videos and what Library ›
    /// Downloads takes on, in a folder of its own.
    #[test]
    fn saves_link_videos_and_are_taken_on() {
        let dir = std::env::temp_dir().join(format!("ytm-rs-downloads-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        init(&dir, &dir.join("music").display().to_string(), false, FileLyrics::Synced);
        let song = |id: &str, title: &str| Track { id: id.into(), title: title.into(), ..Default::default() };
        let from = saves();

        let a = save(&song("a", "Same"), b"not mp4", 61_000, None, None, Some("va".into()), None).unwrap();
        let b = save(&song("b", "Same"), b"not mp4", 0, None, None, None, None).unwrap();
        assert_ne!(a, b, "two songs of one name get a file each");
        assert!(a.is_file() && b.is_file());

        // Saved again, a song keeps its file and the videos it stood for.
        assert_eq!(save(&song("a", "Same"), b"not mp4", 61_000, None, None, Some("vb".into()), None).unwrap(), a);
        link_video("a", "vc");
        link_video("a", "va");
        assert_eq!(reuse("a", Some("vd")), Some(a.clone()));
        assert_eq!(reuse("missing", Some("ve")), None);
        {
            let st = STATE.read().unwrap();
            let e = &st.index["a"];
            assert_eq!(e.stands_for.as_deref(), Some("va"));
            assert_eq!(e.videos, ["vb", "vc", "vd"]);
            assert!(e.song.is_some());
        }
        assert_eq!(song_for("vc").map(|t| t.id), Some("a".to_owned()));

        // Written to disk with every change, read back whole.
        let written: HashMap<String, Entry> = serde_json::from_slice(&std::fs::read(dir.join("downloads.json")).unwrap()).unwrap();
        assert_eq!(written["a"].videos, ["vb", "vc", "vd"]);
        assert!(written.contains_key("b"));

        // Newest first, each once, without the music video flags; then nothing new.
        let (seen, new) = saved_since(from);
        assert_eq!(new.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(new[0].duration, Some(61));
        assert_eq!(saved_since(seen), (seen, Vec::new()));

        // Lists' songs go in their own folders, two lists of one name in one each; the
        // loose songs stay where they are.
        let music = dir.join("music");
        let mix = |id: &str| ListFolder { id: id.into(), name: "Mix".into() };
        let c = save(&song("c", "C"), b"not mp4", 0, None, None, None, Some(&mix("PL1"))).unwrap();
        let d = save(&song("d", "D"), b"not mp4", 0, None, None, None, Some(&mix("PL2"))).unwrap();
        let e = save(&song("e", "E"), b"not mp4", 0, None, None, None, Some(&mix("PL1"))).unwrap();
        assert_eq!((c.parent(), d.parent(), e.parent()), (Some(&*music.join("Mix")), Some(&*music.join("Mix (2)")), Some(&*music.join("Mix"))));
        assert!(has_list_folder("PL1") && has_list_folder("PL2") && !has_list_folder("PL3"));
        assert_eq!((path("a"), path("c")), (Some(a.clone()), Some(c.clone())));
        let written: HashMap<String, Entry> = serde_json::from_slice(&std::fs::read(dir.join("downloads.json")).unwrap()).unwrap();
        assert_eq!((written["c"].list.as_deref(), written["a"].list.as_deref()), (Some("PL1"), None));
        assert!(any_listed(&[song("x", "X"), song("c", "C")]) && !all_listed(&[song("x", "X"), song("c", "C")]));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_file_gets_the_chosen_sources_lyrics_as_set() {
        use crate::lyrics::{Line, Lyrics, Sync};
        let lyrics = |provider, text: &str| Lyrics { lines: vec![Line { start: 2000, dur: 1000, text: text.into(), ..Default::default() }], sync: Sync::Line, provider, language: None, songwriters: vec![] };
        let set = LyricsSet { sources: vec![lyrics(Provider::BLyricsRich, "first"), lyrics(Provider::LrclibSynced, "picked")] };
        let order: Vec<(Provider, bool)> = Provider::ALL.iter().map(|p| (*p, true)).collect();
        let pins = HashMap::from([("v".to_owned(), Provider::LrclibSynced)]);
        let chosen = crate::lyrics::choose(&order, &pins, "v", &set);
        assert_eq!(tag_text(&set, chosen, FileLyrics::Synced).as_deref(), Some("[00:02.00]picked
[00:03.00]"));
        assert_eq!(tag_text(&set, chosen, FileLyrics::Plain).as_deref(), Some("picked"));
        assert_eq!(tag_text(&set, chosen, FileLyrics::Off), None);
        // No pick: the first in order.
        let first = crate::lyrics::choose(&order, &HashMap::new(), "v", &set);
        assert_eq!(tag_text(&set, first, FileLyrics::Plain).as_deref(), Some("first"));
        assert_eq!(tag_text(&set, None, FileLyrics::Synced), None);
        assert_eq!(FileLyrics::from_key(""), FileLyrics::Synced);
        assert!(FileLyrics::ALL.iter().all(|m| FileLyrics::from_key(m.key()) == *m));
    }
}
