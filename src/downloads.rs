//! Songs saved to disk: the audio as YouTube serves it (AAC in an .m4a, at the best
//! quality the account gets), tagged with title, artists, album and cover, in the
//! downloads folder. A downloaded song plays from its file, so it works offline.
//!
//! `downloads.json` in the data dir maps video ids to their files; a file that was
//! moved or deleted just stops counting as downloaded. A downloaded song's lyrics are
//! kept in the data dir's `lyrics` folder, so they show offline too.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};

use serde::{Deserialize, Serialize};

use crate::lyrics::LyricsSet;
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
}

struct State {
    index: HashMap<String, Entry>,
    /// Where the index is saved (the data dir's downloads.json).
    file: PathBuf,
    folder: PathBuf,
    /// Songs whose file wasn't there when last looked for. They stay in the index: a
    /// folder on a drive that's away has them back when it's back.
    gone: HashSet<String>,
    /// The songs saved this session, in the order they were: Library › Downloads takes
    /// on the new ones (`saved_since`) without looking at every file again.
    saved: Vec<String>,
}

static STATE: LazyLock<RwLock<State>> =
    LazyLock::new(|| RwLock::new(State { index: HashMap::new(), file: PathBuf::new(), folder: default_folder(), gone: HashSet::new(), saved: Vec::new() }));

/// Music\ytm-rs.
pub fn default_folder() -> PathBuf {
    let base = directories::UserDirs::new()
        .and_then(|u| u.audio_dir().map(Path::to_path_buf).or_else(|| Some(u.home_dir().join("Music"))))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("ytm-rs")
}

/// Load the index; `folder` is the saved setting (empty for the default).
pub fn init(data_dir: &Path, folder: &str) {
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
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(dir).spawn();
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
        let uri = url::Url::from_file_path(path).map(|u| u.to_string()).unwrap_or_default();
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

/// The song's file, if it's downloaded and still there.
pub fn path(video_id: &str) -> Option<PathBuf> {
    STATE.read().unwrap().index.get(video_id).map(|e| e.path.clone()).filter(|p| p.is_file())
}

/// Whether all these songs are downloaded, as last looked (no look at the files now, so
/// it's quick enough for the UI thread; startup and Library › Downloads look).
/// A music video counts when its song version is.
pub fn all_listed(tracks: &[Track]) -> bool {
    let st = STATE.read().unwrap();
    let videos: std::collections::HashSet<&str> = match tracks.iter().any(|t| t.music_video) {
        true => st.index.iter().filter(|(id, _)| !st.gone.contains(*id)).flat_map(|(_, e)| e.stands_for.iter().chain(&e.videos)).map(String::as_str).collect(),
        false => Default::default(),
    };
    tracks.iter().all(|t| (st.index.contains_key(&t.id) && !st.gone.contains(&t.id)) || (t.music_video && videos.contains(t.id.as_str())))
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

/// Keep a downloaded song's lyrics for offline, and put their words in the file's
/// lyrics tag (for other players) when they changed. Nothing for a song that isn't
/// downloaded, or lyrics that are empty.
pub fn save_lyrics(video_id: &str, set: &LyricsSet) {
    let (Some(audio), Some(file)) = (path(video_id), lyrics_file(video_id)) else { return };
    if set.sources.is_empty() {
        return;
    }
    let words = plain_lyrics(set);
    let before = saved_lyrics(video_id).map(|s| plain_lyrics(&s));
    let saved = file.parent().map(std::fs::create_dir_all).transpose().and_then(|_| std::fs::write(&file, serde_json::to_vec(set).unwrap_or_default()));
    if let Err(e) = saved {
        log::warn!("couldn't save the lyrics of {video_id}: {e}");
        return;
    }
    if before.as_deref() != Some(words.as_str())
        && !words.is_empty()
        && audio.extension().is_some_and(|e| e == "m4a")
        && let Err(e) = mp4ameta::Tag::read_from_path(&audio).and_then(|mut tag| {
            tag.set_lyrics(&words);
            tag.write_to_path(&audio)
        })
    {
        log::warn!("couldn't tag the lyrics of {}: {e}", audio.display());
    }
}

/// The words of the first source's lyrics, a line each.
fn plain_lyrics(set: &LyricsSet) -> String {
    let Some(l) = set.sources.first() else { return String::new() };
    l.lines.iter().filter(|l| !l.instrumental).map(|l| l.text.trim()).collect::<Vec<_>>().join("
").trim().to_owned()
}

/// A downloaded song's audio, as a complete stream buffer for playback.
pub fn open(video_id: &str) -> Option<Arc<Shared>> {
    let entry = STATE.read().unwrap().index.get(video_id).cloned()?;
    let data = std::fs::read(&entry.path).ok()?;
    log::info!("{video_id}: playing the downloaded file");
    Some(Shared::from_bytes(data, entry.duration_ms, entry.loudness_db))
}

/// Write a downloaded song into the folder and remember it. Returns the file.
/// `stands_for`: the music video `t` is the song version of.
pub fn save(t: &Track, audio: &[u8], duration_ms: u64, loudness_db: Option<f32>, cover: Option<Vec<u8>>, stands_for: Option<String>) -> anyhow::Result<PathBuf> {
    // One save at a time: two songs of the same name would otherwise both find it free.
    static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let folder = folder();
    std::fs::create_dir_all(&folder)?;
    let ext = if audio.get(4..8) == Some(b"ftyp") { "m4a" } else { "webm" };
    // A file already there for this song is replaced; another song's isn't.
    let mine = STATE.read().unwrap().index.get(&t.id).map(|e| e.path.clone());
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
        st.index.insert(t.id.clone(), Entry { path: path.clone(), duration_ms, loudness_db, stands_for: first, videos, song, track });
        st.saved.push(t.id.clone());
    }
    write_index();
    Ok(path)
}

/// "Artists - Title", safe as a Windows file name.
fn file_stem(t: &Track) -> String {
    let artists = t.artist_line();
    let name = if artists.is_empty() { t.title.clone() } else { format!("{artists} - {}", t.title) };
    let clean: String = name.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c }).collect();
    let clean = clean.trim().trim_end_matches('.').trim();
    let clean: String = clean.chars().take(150).collect();
    if clean.is_empty() { t.id.clone() } else { clean }
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
        init(&dir, &dir.join("music").display().to_string());
        let song = |id: &str, title: &str| Track { id: id.into(), title: title.into(), ..Default::default() };
        let from = saves();

        let a = save(&song("a", "Same"), b"not mp4", 61_000, None, None, Some("va".into())).unwrap();
        let b = save(&song("b", "Same"), b"not mp4", 0, None, None, None).unwrap();
        assert_ne!(a, b, "two songs of one name get a file each");
        assert!(a.is_file() && b.is_file());

        // Saved again, a song keeps its file and the videos it stood for.
        assert_eq!(save(&song("a", "Same"), b"not mp4", 61_000, None, None, Some("vb".into())).unwrap(), a);
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

        let _ = std::fs::remove_dir_all(&dir);
    }
}
