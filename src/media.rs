//! Integración con el sistema: teclas multimedia y "Reproduciendo ahora"
//! (SMTC en Windows, MPRIS en Linux, Now Playing en macOS) mediante `souvlaki`.

use std::ffi::c_void;
use std::time::Duration;

use souvlaki::{MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig};

use crate::bus::{Msg, UiTx};
use crate::model::NowPlaying;

pub struct Media {
    controls: Option<MediaControls>,
}

impl Media {
    pub fn new(hwnd: Option<*mut c_void>, ui: UiTx) -> Self {
        let config = PlatformConfig {
            display_name: "Nanofy",
            dbus_name: "nanofy",
            hwnd,
        };
        let controls = match MediaControls::new(config) {
            Ok(mut c) => {
                if let Err(e) = c.attach(move |ev| ui.send(Msg::Media(ev))) {
                    log::warn!("teclas multimedia no disponibles: {e:?}");
                    None
                } else {
                    Some(c)
                }
            }
            Err(e) => {
                log::warn!("teclas multimedia no disponibles: {e:?}");
                None
            }
        };
        Self { controls }
    }

    pub fn disabled() -> Self {
        Self { controls: None }
    }

    pub fn active(&self) -> bool {
        self.controls.is_some()
    }

    pub fn set_metadata(&mut self, np: Option<&NowPlaying>) {
        let Some(c) = self.controls.as_mut() else {
            return;
        };
        match np {
            Some(np) => {
                let artist = np.artists_str();
                let _ = c.set_metadata(MediaMetadata {
                    title: Some(&np.name),
                    album: Some(&np.album),
                    artist: Some(&artist),
                    cover_url: np.cover_url.as_deref(),
                    duration: Some(Duration::from_millis(np.duration_ms as u64)),
                });
            }
            None => {
                let _ = c.set_metadata(MediaMetadata::default());
            }
        }
    }

    pub fn set_playback(&mut self, playing: bool, stopped: bool, position_ms: u32) {
        let Some(c) = self.controls.as_mut() else {
            return;
        };
        let pos = Some(MediaPosition(Duration::from_millis(position_ms as u64)));
        let playback = if stopped {
            MediaPlayback::Stopped
        } else if playing {
            MediaPlayback::Playing { progress: pos }
        } else {
            MediaPlayback::Paused { progress: pos }
        };
        let _ = c.set_playback(playback);
    }
}
