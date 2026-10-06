//! Instantánea de la biblioteca en disco: al arrancar, la interfaz se llena al instante con
//! los datos de la última sesión y la red solo se usa para refrescarlos en segundo plano.

use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::model::*;

type DiskJob = Box<dyn FnOnce() + Send>;

/// Un solo hilo («nanofy-disk») para lo que se lee y escribe fuera de la interfaz: la
/// instantánea, el registro de reproducciones, lo que suena y las copias de playlists, álbumes
/// y radios (leerlas al abrirlas, guardarlas y borrarlas). Va en orden: antes cada guardado tenía su propio hilo, y dos
/// escrituras del mismo fichero (mismo `.json.tmp`) podían pisarse, o una vieja quedar encima
/// de la última.
pub struct Disk {
    tx: Option<mpsc::Sender<DiskJob>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Disk {
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel::<DiskJob>();
        let thread = std::thread::Builder::new()
            .name("nanofy-disk".into())
            .spawn(move || {
                for job in rx {
                    job();
                }
            })
            .ok();
        Self { tx: thread.is_some().then_some(tx), thread }
    }

    /// Encola un trabajo. Sin hilo (no arrancó o ya se cerró) se hace aquí mismo: mejor un
    /// tirón que perder lo que había que guardar.
    pub fn run(&self, job: impl FnOnce() + Send + 'static) {
        let job: DiskJob = Box::new(job);
        match &self.tx {
            Some(tx) => {
                if let Err(e) = tx.send(job) {
                    (e.0)();
                }
            }
            None => job(),
        }
    }

    /// Espera a que termine todo lo encolado y cierra el hilo. Al salir: lo último guardado es
    /// lo que queda en disco y el proceso no lo corta a medias.
    pub fn finish(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

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
    /// Cuándo terminó la última recarga completa de Me gusta (0 = nunca, p. ej. una instantánea
    /// de una versión anterior). `saved_at` no sirve: cambia con cada guardado.
    pub liked_synced_at: u64,
    /// Cuenta cruda de Spotify (`total`, con las no disponibles) en esa recarga, ajustada con los
    /// Me gusta dados y quitados aquí: la base con la que lo reciente detecta lo quitado fuera.
    pub liked_server_total: u64,
    /// Ids con Me gusta dado aquí sin fila en la lista (no había objeto Track a mano). Ya suman
    /// en `liked_server_total`: sin guardarlos, el próximo arranque los tomaría por nuevos de
    /// otro dispositivo, la cuenta no cuadraría y saldría una recarga completa.
    pub liked_extra: Vec<String>,
    /// Cuándo llegaron por última vez los artistas seguidos.
    pub artists_synced_at: u64,
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

    /// Escribe ahora mismo (fichero temporal + renombrado).
    pub fn save_now(&self, path: &Path) {
        if let Ok(text) = serde_json::to_string(self) {
            write_atomic(path, &text);
        }
    }

    /// Serializa y escribe en el hilo del disco. Serializar aquí (miles de Me gusta) era un
    /// tirón del fotograma en cada cambio de canción.
    pub fn save_async(self, disk: &Disk, path: PathBuf) {
        disk.run(move || self.save_now(&path));
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

    /// Serializa y escribe en el hilo del disco. Va compartido (Arc): el registro de la interfaz
    /// solo se copia si se apunta otra canción mientras se escribe, no en cada guardado.
    pub fn save_async(self: Arc<Self>, disk: &Disk, path: PathBuf) {
        disk.run(move || self.save_now(&path));
    }
}
