//! Géneros de «Canciones que te gustan» para su filtro y sus playlists dinámicas, de una sola
//! fuente: Deezer. Elegida el 17 oct 2026 con una muestra de 40 canciones del usuario (Deezer
//! 35/40 con género, iTunes 34/40, MusicBrainz 1/40): tiene un catálogo de géneros fijo y curado
//! (cada uno con su número), los da en el idioma que se le pide y localiza la canción por su ISRC,
//! el código de la grabación que Spotify da en sus metadatos, así que no la confunde con otra del
//! mismo nombre. Mezclar fuentes (como los chips de géneros de álbumes y canciones) da etiquetas que
//! no casan entre sí; aquí no se mezcla nada.
//!
//! Los géneros son los del álbum en Deezer (a veces varios: «Alternativo» e «Indie Pop»). Sin ISRC
//! o sin coincidencia, se busca en el propio Deezer por título, artista y duración. Lo encontrado se
//! guarda en disco; sin resultado, se vuelve a probar pasados 30 días.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Lo que hace falta de cada canción para buscarla: id de Spotify, título, primer artista y
/// duración en ms.
pub type LikedSong = (String, String, String, u32);

/// Pausa entre consultas a Deezer: su límite es de 50 cada 5 s.
const PACE: Duration = Duration::from_millis(125);
/// Sin resultado, se vuelve a buscar pasado este tiempo.
const MISS_TTL: u64 = 30 * 24 * 3600;
/// Cada cuántas canciones se manda lo encontrado a la interfaz.
const BATCH: usize = 20;

#[derive(Default, Serialize, Deserialize)]
pub struct Store {
    /// Géneros (números de Deezer) de cada canción, por id de Spotify.
    #[serde(default)]
    pub tracks: HashMap<String, Vec<u32>>,
    /// Géneros de cada álbum de Deezer ya consultado (muchas canciones comparten álbum).
    #[serde(default)]
    pub albums: HashMap<u64, Vec<u32>>,
    /// Nombre (en español) de cada género.
    #[serde(default)]
    pub names: HashMap<u32, String>,
    /// Canciones sin resultado y cuándo se buscaron.
    #[serde(default)]
    pub miss: HashMap<String, u64>,
}

impl Store {
    pub fn load(path: &PathBuf) -> Self {
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    fn save(&self, path: &PathBuf) {
        if let Ok(t) = serde_json::to_string(self) {
            crate::cache::write_atomic(path, &t);
        }
    }

    /// ¿Hay que buscar esta canción? (ni conocida ni buscada sin éxito hace poco).
    fn wanted(&self, id: &str, now: u64) -> bool {
        !self.tracks.contains_key(id) && self.miss.get(id).is_none_or(|at| *at > now || now - at >= MISS_TTL)
    }
}

/// Título o nombre para comparar: minúsculas, sin lo que va entre paréntesis o corchetes ni lo
/// que va tras « - » (versiones, «feat.»), solo letras y números separados por un espacio.
pub fn norm(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0i32;
    let cut = s.find(" - ").map_or(s, |i| &s[..i]);
    for c in cut.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            _ if depth > 0 => {}
            c if c.is_alphanumeric() => out.extend(c.to_lowercase()),
            _ => {
                if !out.ends_with(' ') && !out.is_empty() {
                    out.push(' ');
                }
            }
        }
    }
    out.trim().to_string()
}

/// ¿Es este resultado de una búsqueda de Deezer la canción? Mismo título y artista (normalizados)
/// y duración a ±3 s si se conocen las dos.
pub fn same_song(name: &str, artist: &str, dur_ms: u32, hit: &Value) -> bool {
    let title = hit["title_short"].as_str().or(hit["title"].as_str()).unwrap_or("");
    let by = hit["artist"]["name"].as_str().unwrap_or("");
    let secs = hit["duration"].as_u64().unwrap_or(0);
    let dur_ok = dur_ms == 0 || secs == 0 || (secs as i64 * 1000 - dur_ms as i64).abs() <= 3000;
    !title.is_empty() && norm(title) == norm(name) && norm(by) == norm(artist) && dur_ok
}

struct Deezer {
    agent: ureq::Agent,
    last: Instant,
}

impl Deezer {
    fn new() -> Self {
        let tls = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(8)))
            .timeout_connect(Some(Duration::from_secs(4)))
            .http_status_as_error(false)
            .tls_config(tls)
            .build();
        Self { agent: ureq::Agent::new_with_config(config), last: Instant::now() - PACE }
    }

    /// GET con el ritmo de su límite; los nombres de géneros, en español. Si Deezer avisa de su
    /// límite (código 4), espera y repite. `None` si no hay respuesta válida.
    fn get(&mut self, url: &str) -> Option<Value> {
        for _ in 0..3 {
            let wait = PACE.saturating_sub(self.last.elapsed());
            if !wait.is_zero() {
                std::thread::sleep(wait);
            }
            self.last = Instant::now();
            let resp = self
                .agent
                .get(url)
                .header("User-Agent", "Nanofy (https://github.com/ElRobaMichis/nanofy)")
                .header("Accept", "application/json")
                .header("Accept-Language", "es")
                .call();
            let mut resp = match resp {
                Ok(r) if r.status().is_success() => r,
                Ok(r) => {
                    log::info!("[generos-deezer] {url}: HTTP {}", r.status());
                    return None;
                }
                Err(e) => {
                    log::info!("[generos-deezer] {url}: {e}");
                    return None;
                }
            };
            let v: Value = serde_json::from_str(&resp.body_mut().read_to_string().ok()?).ok()?;
            match v["error"]["code"].as_i64() {
                Some(4) => std::thread::sleep(Duration::from_secs(5)),
                Some(_) => return Some(v),
                None => return Some(v),
            }
        }
        None
    }

    /// El álbum de Deezer de una canción: por su ISRC y, si no, buscándola.
    fn album_of(&mut self, song: &LikedSong, isrc: Option<&str>) -> Option<u64> {
        if let Some(isrc) = isrc {
            if let Some(v) = self.get(&format!("https://api.deezer.com/track/isrc:{isrc}")) {
                if let Some(id) = v["album"]["id"].as_u64() {
                    return Some(id);
                }
            }
        }
        let (_, name, artist, dur) = song;
        let q = crate::api::urlencode(&format!("{artist} {}", norm(name)));
        let v = self.get(&format!("https://api.deezer.com/search?q={q}&limit=10"))?;
        v["data"].as_array()?.iter().find(|hit| same_song(name, artist, *dur, hit)).and_then(|hit| hit["album"]["id"].as_u64())
    }

    /// Géneros (número y nombre) de un álbum de Deezer.
    fn album_genres(&mut self, album: u64) -> Option<Vec<(u32, String)>> {
        let v = self.get(&format!("https://api.deezer.com/album/{album}"))?;
        if v.get("error").is_some() {
            return None;
        }
        Some(
            v["genres"]["data"]
                .as_array()
                .map(|a| a.iter().filter_map(|g| Some((g["id"].as_u64()? as u32, g["name"].as_str()?.to_string()))).collect())
                .unwrap_or_default(),
        )
    }
}

/// Lo que se manda a la interfaz: géneros de canciones, nombres de géneros y cuántas quedan.
pub struct Found {
    pub tracks: Vec<(String, Vec<u32>)>,
    pub names: Vec<(u32, String)>,
    pub pending: usize,
}

/// Busca los géneros de `songs` y va llamando a `emit` con lo encontrado (primero lo que ya se
/// conocía, luego cada `BATCH` canciones y al final, con `pending` 0). `isrcs` da el ISRC de un
/// lote de ids (de los metadatos de Spotify).
pub fn run(store: &Mutex<Store>, path: &PathBuf, songs: Vec<LikedSong>, isrcs: impl Fn(&[String]) -> HashMap<String, String>, mut emit: impl FnMut(Found)) {
    let now = crate::cache::now_secs();
    // Lo ya conocido, de una vez.
    let (known, todo): (Vec<(String, Vec<u32>)>, Vec<LikedSong>) = {
        let s = store.lock().unwrap_or_else(|e| e.into_inner());
        let known = songs.iter().filter_map(|t| s.tracks.get(&t.0).map(|g| (t.0.clone(), g.clone()))).collect();
        let todo = songs.into_iter().filter(|t| s.wanted(&t.0, now)).collect();
        (known, todo)
    };
    let names: Vec<(u32, String)> = store.lock().unwrap_or_else(|e| e.into_inner()).names.iter().map(|(k, v)| (*k, v.clone())).collect();
    emit(Found { tracks: known, names, pending: todo.len() });
    if todo.is_empty() {
        return;
    }
    log::info!("[generos-deezer] {} canciones por buscar", todo.len());
    let t0 = Instant::now();
    let ids: Vec<String> = todo.iter().map(|t| t.0.clone()).collect();
    let isrc = isrcs(&ids);
    let mut dz = Deezer::new();
    let mut batch: Vec<(String, Vec<u32>)> = Vec::new();
    let mut new_names: Vec<(u32, String)> = Vec::new();
    let mut found = 0usize;
    for (k, song) in todo.iter().enumerate() {
        let album = dz.album_of(song, isrc.get(&song.0).map(String::as_str));
        let genres = match album {
            Some(a) => {
                let cached = store.lock().unwrap_or_else(|e| e.into_inner()).albums.get(&a).cloned();
                match cached {
                    Some(g) => Some(g),
                    None => dz.album_genres(a).map(|gs| {
                        let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
                        for (id, name) in &gs {
                            if s.names.insert(*id, name.clone()).is_none() {
                                new_names.push((*id, name.clone()));
                            }
                        }
                        let ids: Vec<u32> = gs.into_iter().map(|(id, _)| id).collect();
                        s.albums.insert(a, ids.clone());
                        ids
                    }),
                }
            }
            None => None,
        };
        {
            let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
            match &genres {
                Some(g) if !g.is_empty() => {
                    s.tracks.insert(song.0.clone(), g.clone());
                    s.miss.remove(&song.0);
                }
                // Sin álbum o sin géneros en Deezer: se apunta para no repetir hasta dentro de un mes.
                _ => {
                    s.miss.insert(song.0.clone(), now);
                }
            }
        }
        if let Some(g) = genres.filter(|g| !g.is_empty()) {
            found += 1;
            batch.push((song.0.clone(), g));
        }
        let last = k + 1 == todo.len();
        if last || (k + 1) % BATCH == 0 {
            store.lock().unwrap_or_else(|e| e.into_inner()).save(path);
            emit(Found { tracks: std::mem::take(&mut batch), names: std::mem::take(&mut new_names), pending: todo.len() - k - 1 });
        }
    }
    log::info!("[generos-deezer] {found} de {} con géneros en {} s", todo.len(), t0.elapsed().as_secs());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titulos_normalizados() {
        assert_eq!(norm("Get Lucky (feat. Pharrell Williams and Nile Rodgers)"), "get lucky");
        assert_eq!(norm("Mrs Magic - Strings Version"), "mrs magic");
        assert_eq!(norm("Pensé en llamarte a vos"), "pensé en llamarte a vos");
        assert_eq!(norm("The Moon Cave [Remastered]"), "the moon cave");
        assert_eq!(norm("  ¿Hola?  Adiós!! "), "hola adiós");
    }

    #[test]
    fn misma_cancion_por_titulo_artista_y_duracion() {
        let hit = serde_json::json!({"title": "Get Lucky (Radio Edit)", "title_short": "Get Lucky", "duration": 248, "artist": {"name": "Daft Punk"}});
        assert!(same_song("Get Lucky", "Daft Punk", 247_000, &hit));
        assert!(same_song("Get Lucky", "daft punk", 0, &hit));
        assert!(!same_song("Get Lucky", "Daft Punk", 369_000, &hit));
        assert!(!same_song("Lose Yourself to Dance", "Daft Punk", 248_000, &hit));
        assert!(!same_song("Get Lucky", "Pharrell Williams", 248_000, &hit));
    }

    #[test]
    fn se_vuelve_a_buscar_al_mes() {
        let mut s = Store::default();
        s.tracks.insert("a".into(), vec![152]);
        s.miss.insert("b".into(), 1_000);
        assert!(!s.wanted("a", 2_000));
        assert!(!s.wanted("b", 2_000));
        assert!(s.wanted("b", 1_000 + MISS_TTL));
        assert!(s.wanted("c", 2_000));
        // Una fecha futura (reloj adelantado) caduca.
        assert!(s.wanted("b", 500));
    }
}
