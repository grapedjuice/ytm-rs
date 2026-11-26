use dbus::arg::{RefArg, Variant};
use dbus::blocking::Connection;
use dbus::channel::{MatchingReceiver, Sender};
use dbus::ffidisp::stdintf::org_freedesktop_dbus::PropertiesPropertiesChanged;
use dbus::message::SignalArgs;
use dbus::Path;
use std::collections::HashMap;
use std::convert::From;
use std::convert::TryInto;
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::super::Error;
use crate::{LoopStatus, MediaControlEvent, MediaMetadata, MediaPlayback, PlatformConfig};

/// A handle to OS media controls.
pub struct MediaControls {
    thread: Option<ServiceThreadHandle>,
    dbus_name: String,
    friendly_name: String,
    desktop_entry: Option<String>,
}

struct ServiceThreadHandle {
    event_channel: mpsc::Sender<InternalEvent>,
    thread: JoinHandle<Result<(), Error>>,
}

#[derive(Clone, PartialEq, Debug)]
enum InternalEvent {
    ChangeMetadata(OwnedMetadata),
    ChangePlayback(MediaPlayback),
    ChangeVolume(f64),
    ChangeShuffle(bool),
    ChangeLoopStatus(LoopStatus),
    Seeked(Duration),
    Kill,
}

#[derive(Debug)]
pub struct ServiceState {
    pub metadata: OwnedMetadata,
    pub metadata_dict: HashMap<String, Variant<Box<dyn RefArg>>>,
    pub playback_status: MediaPlayback,
    pub volume: f64,
    pub shuffle: bool,
    pub loop_status: LoopStatus,
}

impl ServiceState {
    pub fn set_metadata(&mut self, metadata: OwnedMetadata) {
        self.metadata_dict = create_metadata_dict(&metadata);
        self.metadata = metadata;
    }

    pub fn get_playback_status(&self) -> &'static str {
        match self.playback_status {
            MediaPlayback::Playing { .. } => "Playing",
            MediaPlayback::Paused { .. } => "Paused",
            MediaPlayback::Stopped => "Stopped",
        }
    }
}

pub fn create_metadata_dict(metadata: &OwnedMetadata) -> HashMap<String, Variant<Box<dyn RefArg>>> {
    let mut dict = HashMap::<String, Variant<Box<dyn RefArg>>>::new();

    let mut insert = |k: &str, v| dict.insert(k.to_string(), Variant(v));

    let OwnedMetadata {
        ref title,
        ref album,
        ref artist,
        ref cover_url,
        ref duration,
    } = metadata;

    // TODO: this is just a workaround to enable SetPosition.
    let path = Path::new("/").unwrap();

    // MPRIS
    insert("mpris:trackid", Box::new(path));

    if let Some(length) = duration {
        insert("mpris:length", Box::new(*length));
    }
    if let Some(cover_url) = cover_url {
        insert("mpris:artUrl", Box::new(cover_url.clone()));
    }

    // Xesam
    if let Some(title) = title {
        insert("xesam:title", Box::new(title.clone()));
    }
    if let Some(artist) = artist {
        insert("xesam:artist", Box::new(vec![artist.clone()]));
    }
    if let Some(album) = album {
        insert("xesam:album", Box::new(album.clone()));
    }

    dict
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct OwnedMetadata {
    pub title: Option<String>,
    pub album: Option<String>,
    pub artist: Option<String>,
    pub cover_url: Option<String>,
    pub duration: Option<i64>,
}

impl From<MediaMetadata<'_>> for OwnedMetadata {
    fn from(other: MediaMetadata) -> Self {
        OwnedMetadata {
            title: other.title.map(|s| s.to_string()),
            artist: other.artist.map(|s| s.to_string()),
            album: other.album.map(|s| s.to_string()),
            cover_url: other.cover_url.map(|s| s.to_string()),
            // TODO: This should probably not have an unwrap
            duration: other.duration.map(|d| d.as_micros().try_into().unwrap()),
        }
    }
}

impl MediaControls {
    /// Create media controls with the specified config.
    pub fn new(config: PlatformConfig) -> Result<Self, Error> {
        let PlatformConfig {
            dbus_name,
            display_name,
            desktop_entry,
            ..
        } = config;

        Ok(Self {
            thread: None,
            dbus_name: dbus_name.to_string(),
            friendly_name: display_name.to_string(),
            desktop_entry: desktop_entry.map(str::to_string),
        })
    }

    /// Attach the media control events to a handler.
    pub fn attach<F>(&mut self, event_handler: F) -> Result<(), Error>
    where
        F: Fn(MediaControlEvent) + Send + 'static,
    {
        self.detach()?;

        let dbus_name = self.dbus_name.clone();
        let friendly_name = self.friendly_name.clone();
        let desktop_entry = self.desktop_entry.clone();
        let (event_channel, rx) = mpsc::channel();

        // Check if the connection can be created BEFORE spawning the new thread
        let conn = Connection::new_session()?;
        let name = format!("org.mpris.MediaPlayer2.{}", dbus_name);
        conn.request_name(name, false, true, false)?;

        self.thread = Some(ServiceThreadHandle {
            event_channel,
            thread: thread::spawn(move || run_service(conn, friendly_name, desktop_entry, event_handler, rx)),
        });
        Ok(())
    }

    /// Detach the event handler.
    pub fn detach(&mut self) -> Result<(), Error> {
        if let Some(ServiceThreadHandle {
            event_channel,
            thread,
        }) = self.thread.take()
        {
            // We don't care about the result of this event, since we immedieately
            // check if the thread has panicked on the next line.
            event_channel.send(InternalEvent::Kill).ok();
            // One error in case the thread panics, and the other one in case the
            // thread has returned an error.
            thread.join().map_err(|_| Error::ThreadPanicked)??;
        }
        Ok(())
    }

    /// Set the current playback status.
    pub fn set_playback(&mut self, playback: MediaPlayback) -> Result<(), Error> {
        self.send_internal_event(InternalEvent::ChangePlayback(playback))
    }

    /// Set the metadata of the currently playing media item.
    pub fn set_metadata(&mut self, metadata: MediaMetadata) -> Result<(), Error> {
        self.send_internal_event(InternalEvent::ChangeMetadata(metadata.into()))
    }

    /// Set the volume level (0.0-1.0) (Only available on MPRIS)
    pub fn set_volume(&mut self, volume: f64) -> Result<(), Error> {
        self.send_internal_event(InternalEvent::ChangeVolume(volume))
    }

    /// ytm-rs: whether shuffle is on (MPRIS's `Shuffle`). (Only available on MPRIS)
    pub fn set_shuffle(&mut self, shuffle: bool) -> Result<(), Error> {
        self.send_internal_event(InternalEvent::ChangeShuffle(shuffle))
    }

    /// ytm-rs: what repeats (MPRIS's `LoopStatus`). (Only available on MPRIS)
    pub fn set_loop_status(&mut self, loop_status: LoopStatus) -> Result<(), Error> {
        self.send_internal_event(InternalEvent::ChangeLoopStatus(loop_status))
    }

    /// ytm-rs: the position jumped (a seek, a skip, a stall), for MPRIS's `Seeked`. Media
    /// widgets count on from the last position they had, so they'd stay wrong without
    /// it. Send it after `set_playback` with the new position. (Only available on MPRIS)
    pub fn seeked(&mut self, position: Duration) -> Result<(), Error> {
        self.send_internal_event(InternalEvent::Seeked(position))
    }

    fn send_internal_event(&mut self, event: InternalEvent) -> Result<(), Error> {
        let thread = &self.thread.as_ref().ok_or(Error::ThreadNotRunning)?;
        thread
            .event_channel
            .send(event)
            .map_err(|_| Error::ThreadPanicked)
    }
}

fn run_service<F>(
    conn: Connection,
    friendly_name: String,
    desktop_entry: Option<String>,
    event_handler: F,
    event_channel: mpsc::Receiver<InternalEvent>,
) -> Result<(), Error>
where
    F: Fn(MediaControlEvent) + Send + 'static,
{
    let state = Arc::new(Mutex::new(ServiceState {
        metadata: Default::default(),
        metadata_dict: create_metadata_dict(&Default::default()),
        playback_status: MediaPlayback::Stopped,
        volume: 1.0,
        shuffle: false,
        loop_status: LoopStatus::None,
    }));
    let event_handler = Arc::new(Mutex::new(event_handler));
    let seeked_signal = Arc::new(Mutex::new(None));

    let mut cr =
        super::interfaces::register_methods(&state, &event_handler, friendly_name, desktop_entry, seeked_signal.clone());

    conn.start_receive(
        dbus::message::MatchRule::new_method_call(),
        Box::new(move |msg, conn| {
            cr.handle_message(msg, conn).unwrap();
            true
        }),
    );

    // ytm-rs: this loop used to take one change, then wait up to a second for a method
    // call, so the OS heard about changes once a second, and a song change (several of
    // them) seconds late (#144). Now method calls are answered as they come, and all the
    // changes waiting go out together.
    loop {
        while conn.process(Duration::ZERO)? {}
        let first = match event_channel.recv_timeout(Duration::from_millis(20)) {
            Ok(event) => event,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };

        let mut changed_properties = HashMap::new();
        let mut seeked = None;

        for event in std::iter::once(first).chain(event_channel.try_iter()) {
            let mut state = state.lock().unwrap();
            match event {
                InternalEvent::Kill => return Ok(()),
                InternalEvent::ChangeMetadata(metadata) => {
                    state.set_metadata(metadata);
                    changed_properties.insert(
                        "Metadata".to_owned(),
                        Variant(state.metadata_dict.box_clone()),
                    );
                }
                InternalEvent::ChangePlayback(playback) => {
                    state.playback_status = playback;
                    changed_properties.insert(
                        "PlaybackStatus".to_owned(),
                        Variant(Box::new(state.get_playback_status().to_string())),
                    );
                }
                InternalEvent::ChangeVolume(volume) => {
                    state.volume = volume;
                    changed_properties.insert("Volume".to_owned(), Variant(Box::new(volume)));
                }
                // ytm-rs: shuffle and repeat, which media widgets can also set (see
                // interfaces.rs), go out as they change like the rest.
                InternalEvent::ChangeShuffle(shuffle) => {
                    state.shuffle = shuffle;
                    changed_properties.insert("Shuffle".to_owned(), Variant(Box::new(shuffle)));
                }
                InternalEvent::ChangeLoopStatus(loop_status) => {
                    state.loop_status = loop_status;
                    changed_properties.insert(
                        "LoopStatus".to_owned(),
                        Variant(Box::new(loop_status.as_str().to_string())),
                    );
                }
                InternalEvent::Seeked(position) => seeked = Some(position),
            }
        }

        let path = Path::new("/org/mpris/MediaPlayer2").unwrap();
        // ytm-rs: nothing but a `Seeked` in this batch: no empty PropertiesChanged.
        if !changed_properties.is_empty() {
            let properties_changed = PropertiesPropertiesChanged {
                interface_name: "org.mpris.MediaPlayer2.Player".to_owned(),
                changed_properties,
                invalidated_properties: Vec::new(),
            };
            conn.send(properties_changed.to_emit_message(&path)).ok();
        }
        // ytm-rs: after the new position, so a widget that reads Position on `Seeked` gets
        // it. In microseconds, as MPRIS has it.
        if let Some(position) = seeked {
            if let Some(make) = seeked_signal.lock().unwrap().as_ref() {
                let micros: i64 = position.as_micros().try_into().unwrap_or(i64::MAX);
                conn.send(make(&path, &(micros,))).ok();
            }
        }
    }

    Ok(())
}
