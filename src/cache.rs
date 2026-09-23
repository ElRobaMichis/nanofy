//! Instantánea de la biblioteca en disco: al arrancar, la interfaz se llena al instante con
//! los datos de la última sesión y la red solo se usa para refrescarlos en segundo plano.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::model::*;

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub struct Snapshot {
    pub saved_at: u64,
    pub user: Option<User>,
    pub playlists: Vec<Playlist>,
    pub recent: Vec<Track>,
    pub saved_albums: Vec<Album>,
    pub followed_artists: Vec<Artist>,
    pub liked: Vec<Track>,
    pub liked_total: u32,
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Escritura atómica: fichero temporal y renombrado, así nunca queda un JSON a medias.
pub fn write_atomic(path: &Path, text: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Deja en `dir` como mucho `keep` copias `.json` (borra las escritas hace más tiempo, es decir,
/// las que no se han abierto recientemente). `radios.json` no se toca.
pub fn prune_dir(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(SystemTime, std::path::PathBuf)> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json") && p.file_name().is_some_and(|n| n != "radios.json"))
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
        .collect();
    if files.len() <= keep {
        return;
    }
    files.sort();
    for (_, p) in &files[..files.len() - keep] {
        let _ = std::fs::remove_file(p);
    }
}

impl Snapshot {
    pub fn load(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn age_secs(&self) -> u64 {
        now_secs().saturating_sub(self.saved_at)
    }

    /// Escribe ahora mismo (fichero temporal + renombrado). Para el cierre.
    pub fn save_now(&self, path: &Path) {
        if let Ok(text) = serde_json::to_string(self) {
            write_atomic(path, &text);
        }
    }

    /// Serializa en este hilo (rápido) y escribe en otro para no tocar el fotograma.
    pub fn save_async(&self, path: PathBuf) {
        let Ok(text) = serde_json::to_string(self) else {
            return;
        };
        std::thread::Builder::new()
            .name("nanofy-snapshot".into())
            .spawn(move || {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let tmp = path.with_extension("json.tmp");
                if std::fs::write(&tmp, text).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
            })
            .ok();
    }
}

/// Registro local de reproducciones: cuántas veces ha sonado cada canción en Nanofy.
/// Spotify no expone "tus más escuchadas" por artista, así que se construye aquí.
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct PlayEntry {
    pub track: Track,
    pub count: u32,
    pub last: u64,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct PlayLog {
    pub entries: Vec<PlayEntry>,
    pub seeded: bool,
}

impl PlayLog {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn record(&mut self, track: Track) {
        let Some(id) = track.id.clone() else {
            return;
        };
        let now = now_secs();
        if let Some(e) = self.entries.iter_mut().find(|e| e.track.id.as_deref() == Some(id.as_str())) {
            e.count += 1;
            e.last = now;
            e.track = track;
        } else {
            self.entries.push(PlayEntry { track, count: 1, last: now });
        }
        if self.entries.len() > 5000 {
            self.entries.sort_by(|a, b| b.last.cmp(&a.last));
            self.entries.truncate(4000);
        }
    }

    pub fn save_now(&self, path: &Path) {
        if let Ok(text) = serde_json::to_string(self) {
            write_atomic(path, &text);
        }
    }

    pub fn save_async(&self, path: PathBuf) {
        let Ok(text) = serde_json::to_string(self) else {
            return;
        };
        std::thread::Builder::new()
            .name("nanofy-playlog".into())
            .spawn(move || {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let tmp = path.with_extension("json.tmp");
                if std::fs::write(&tmp, text).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
            })
            .ok();
    }
}
