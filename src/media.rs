//! OS media integration (Windows SMTC / MPRIS / macOS Now Playing) via souvlaki.

use crossbeam_channel::Sender;
use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, PlatformConfig};

pub struct Media {
    controls: MediaControls,
}

impl Media {
    pub fn new(
        hwnd: Option<*mut std::ffi::c_void>,
        tx: Sender<MediaControlEvent>,
        wake: impl Fn() + Send + 'static,
    ) -> Option<Self> {
        let config = PlatformConfig { display_name: "YT Music", dbus_name: "ytm_rs", hwnd };
        let mut controls = MediaControls::new(config)
            .map_err(|e| log::warn!("media controls unavailable: {e:?}"))
            .ok()?;
        controls
            .attach(move |ev| {
                let _ = tx.send(ev);
                wake();
            })
            .map_err(|e| log::warn!("media controls attach failed: {e:?}"))
            .ok()?;
        Some(Self { controls })
    }

    pub fn set_track(&mut self, title: &str, artist: &str, album: Option<&str>, cover: Option<&str>, secs: Option<u32>) {
        let _ = self.controls.set_metadata(MediaMetadata {
            title: Some(title),
            artist: Some(artist),
            album,
            cover_url: cover,
            duration: secs.map(|s| std::time::Duration::from_secs(s as u64)),
        });
    }

    pub fn set_playing(&mut self, playing: bool) {
        let _ = self.controls.set_playback(if playing {
            MediaPlayback::Playing { progress: None }
        } else {
            MediaPlayback::Paused { progress: None }
        });
    }
}
