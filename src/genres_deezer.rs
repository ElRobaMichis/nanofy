//! Géneros de «Canciones que te gustan» para su filtro y sus playlists dinámicas, de una sola
//! fuente: Deezer. Elegida el 17 oct 2026 con una muestra de 40 canciones del usuario (Deezer
//! 35/40 con género, iTunes 34/40, MusicBrainz 1/40): tiene un catálogo de géneros fijo y curado
//! (cada uno con su número), los da en el idioma que se le pide y localiza la canción por su ISRC,
//! el código de la grabación que Spotify da en sus metadatos, así que no la confunde con otra del
//! mismo nombre. Mezclar fuentes (como los chips de géneros de álbumes y canciones) da etiquetas que
//! no casan entre sí; aquí no se mezcla nada.
//!
//! Los géneros son los del álbum en Deezer (a veces varios: «Alternativo» e «Indie Pop»). Se busca
//! una vez por álbum de Spotify: el de Deezer por su UPC (una consulta con los géneros dentro) y, si
//! no, por el ISRC de sus canciones; sin ninguno, en el propio Deezer por título, artista y
//! duración. Varias consultas a la vez, al ritmo de su límite. Lo encontrado se guarda en disco;
//! sin resultado, se vuelve a probar pasados 30 días.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Lo que hace falta de cada canción para buscarla: id de Spotify, título, primer artista y
/// duración en ms.
pub type LikedSong = (String, String, String, u32);

/// Ritmo común de todas las consultas a Deezer: su límite es de 50 cada 5 s (medido el 17 oct
/// 2026: a 9,5 por segundo ninguna rechazada, a 12 una de cada seis). Si alguna otra consulta de
/// la app lo hace saltar, todos frenan (`Deezer::get`).
const PACE: Duration = Duration::from_millis(105);
/// Consultas a la vez: cada una tarda unos 350 ms, así que con una sola no se pasa de 3 por
/// segundo; con 5 se llega al ritmo de arriba.
const WORKERS: usize = 5;
/// Sin resultado, se vuelve a buscar pasado este tiempo.
const MISS_TTL: u64 = 30 * 24 * 3600;
/// Cada cuántas canciones se manda lo encontrado a la interfaz.
const BATCH: usize = 20;

/// Lo que dan los metadatos de Spotify de una canción para encontrarla en Deezer: el ISRC de la
/// grabación, su álbum de Spotify y el UPC (código de barras) de ese álbum.
#[derive(Clone, Debug, Default)]
pub struct Codes {
    pub isrc: Option<String>,
    pub album: Option<String>,
    pub upc: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Store {
    /// Géneros (números de Deezer) de cada canción, por id de Spotify.
    #[serde(default)]
    pub tracks: HashMap<String, Vec<u32>>,
    /// Géneros de cada álbum de Deezer ya consultado (muchas canciones comparten álbum).
    #[serde(default)]
    pub albums: HashMap<u64, Vec<u32>>,
    /// Géneros por álbum de Spotify: las canciones nuevas de un álbum ya visto no consultan nada.
    #[serde(default)]
    pub spotify_albums: HashMap<String, Vec<u32>>,
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

/// Géneros (número y nombre) de un álbum de Deezer en su respuesta.
fn parse_genres(v: &Value) -> Vec<(u32, String)> {
    v["genres"]["data"]
        .as_array()
        .map(|a| a.iter().filter_map(|g| Some((g["id"].as_u64()? as u32, g["name"].as_str()?.to_string()))).collect())
        .unwrap_or_default()
}

/// Formas de un UPC para pedirlo a Deezer. Spotify los da de 13 cifras y Deezer guarda muchos sin
/// los ceros de delante: con 948 canciones, de los que empiezan por cero 110 salieron sin ellos y
/// 41 tal cual, así que se prueba primero sin ceros.
pub fn upc_forms(upc: &str) -> Vec<String> {
    let short = upc.trim_start_matches('0');
    if short != upc && short.len() >= 11 {
        vec![short.to_string(), upc.to_string()]
    } else {
        vec![upc.to_string()]
    }
}

/// Cliente de Deezer que comparten los hilos: un solo ritmo para todos.
struct Deezer {
    agent: ureq::Agent,
    next: Mutex<Instant>,
    /// Para el registro: consultas hechas y álbumes hallados por la primera forma del UPC, por la
    /// segunda, por ISRC y buscando.
    stats: [std::sync::atomic::AtomicUsize; 5],
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
        Self { agent: ureq::Agent::new_with_config(config), next: Mutex::new(Instant::now()), stats: Default::default() }
    }

    fn count(&self, k: usize) {
        self.stats[k].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Espera su turno en el ritmo común.
    fn slot(&self) {
        let wait = {
            let mut next = self.next.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            let at = (*next).max(now);
            *next = at + PACE;
            at - now
        };
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }

    /// GET a su ritmo; los nombres de géneros, en español. Si Deezer avisa de su límite (código
    /// 4), frena a todos los hilos un par de segundos y repite. `None` si no hay respuesta válida.
    fn get(&self, url: &str) -> Option<Value> {
        for _ in 0..4 {
            self.slot();
            self.count(0);
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
            if v["error"]["code"].as_i64() == Some(4) {
                log::info!("[generos-deezer] límite de Deezer: se frena");
                let mut next = self.next.lock().unwrap_or_else(|e| e.into_inner());
                *next = (*next).max(Instant::now() + Duration::from_secs(2));
                continue;
            }
            return Some(v);
        }
        None
    }

    /// El álbum de Deezer por el UPC del de Spotify, con sus géneros (una sola consulta).
    fn album_by_upc(&self, upc: &str) -> Option<(u64, Vec<(u32, String)>)> {
        for (k, form) in upc_forms(upc).into_iter().enumerate() {
            if let Some(v) = self.get(&format!("https://api.deezer.com/album/upc:{form}")) {
                if let (None, Some(id)) = (v.get("error"), v["id"].as_u64()) {
                    self.count(1 + k);
                    return Some((id, parse_genres(&v)));
                }
            }
        }
        None
    }

    /// El álbum de Deezer de una canción: por su ISRC o, sin él, buscándola.
    fn album_of(&self, song: &LikedSong, isrc: Option<&str>) -> Option<u64> {
        if let Some(isrc) = isrc {
            if let Some(id) = self.get(&format!("https://api.deezer.com/track/isrc:{isrc}")).and_then(|v| v["album"]["id"].as_u64()) {
                self.count(3);
                return Some(id);
            }
        }
        let (_, name, artist, dur) = song;
        let q = crate::api::urlencode(&format!("{artist} {}", norm(name)));
        let v = self.get(&format!("https://api.deezer.com/search?q={q}&limit=10"))?;
        let id = v["data"].as_array()?.iter().find(|hit| same_song(name, artist, *dur, hit)).and_then(|hit| hit["album"]["id"].as_u64());
        if id.is_some() {
            self.count(4);
        }
        id
    }

    /// Géneros de un álbum de Deezer.
    fn album_genres(&self, album: u64) -> Option<Vec<(u32, String)>> {
        let v = self.get(&format!("https://api.deezer.com/album/{album}"))?;
        if v.get("error").is_some() {
            return None;
        }
        Some(parse_genres(&v))
    }
}

/// Canciones de un mismo álbum de Spotify (o una sola, si no se sabe su álbum): se buscan juntas.
struct Group {
    album: Option<String>,
    upc: Option<String>,
    songs: Vec<(LikedSong, Option<String>)>,
}

/// Géneros de un grupo: el álbum por su UPC y, si no, por el ISRC de hasta tres de sus canciones
/// (o buscando la primera). Devuelve los géneros con su nombre y el álbum de Deezer, si se halló.
fn lookup(dz: &Deezer, store: &Mutex<Store>, g: &Group) -> Option<(Vec<(u32, String)>, Option<u64>)> {
    if let Some((id, genres)) = g.upc.as_deref().and_then(|u| dz.album_by_upc(u)) {
        if !genres.is_empty() {
            return Some((genres, Some(id)));
        }
    }
    let with_isrc: Vec<&(LikedSong, Option<String>)> = g.songs.iter().filter(|(_, i)| i.is_some()).take(3).collect();
    let tries: Vec<&(LikedSong, Option<String>)> = if with_isrc.is_empty() { g.songs.iter().take(1).collect() } else { with_isrc };
    for (song, isrc) in tries {
        let Some(album) = dz.album_of(song, isrc.as_deref()) else { continue };
        let known = {
            let s = store.lock().unwrap_or_else(|e| e.into_inner());
            s.albums.get(&album).map(|ids| ids.iter().map(|id| (*id, s.names.get(id).cloned().unwrap_or_default())).collect::<Vec<_>>())
        };
        if let Some(genres) = known.or_else(|| dz.album_genres(album)) {
            if !genres.is_empty() {
                return Some((genres, Some(album)));
            }
        }
    }
    None
}

/// Lo que se manda a la interfaz: géneros de canciones, nombres de géneros y cuántas quedan.
pub struct Found {
    pub tracks: Vec<(String, Vec<u32>)>,
    pub names: Vec<(u32, String)>,
    pub pending: usize,
}

/// Busca los géneros de `songs` y va llamando a `emit` con lo encontrado (primero lo que ya se
/// conocía, luego cada `BATCH` canciones y al final, con `pending` 0). `codes` da los códigos de
/// un lote de ids (de los metadatos de Spotify). Las consultas van en `WORKERS` hilos a la vez.
pub fn run(store: &Mutex<Store>, path: &PathBuf, songs: Vec<LikedSong>, codes: impl Fn(&[String]) -> Option<HashMap<String, Codes>>, mut emit: impl FnMut(Found)) {
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
    let total = todo.len();
    let ids: Vec<String> = todo.iter().map(|t| t.0.clone()).collect();
    let Some(codes) = codes(&ids) else {
        // Sin los códigos (sin sesión), nada se apunta como no encontrado: se reintenta luego.
        emit(Found { tracks: Vec::new(), names: Vec::new(), pending: 0 });
        return;
    };
    let mut pending = total;
    let mut batch: Vec<(String, Vec<u32>)> = Vec::new();
    let mut new_names: Vec<(u32, String)> = Vec::new();
    let mut found = 0usize;

    // Por álbum de Spotify, en el orden de la lista (lo reciente primero). Las de un álbum ya
    // conocido salen sin consultar nada.
    let mut groups: Vec<Group> = Vec::new();
    let mut by_album: HashMap<String, usize> = HashMap::new();
    {
        let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
        for song in todo {
            let c = codes.get(&song.0).cloned().unwrap_or_default();
            if let Some(g) = c.album.as_ref().and_then(|a| s.spotify_albums.get(a).cloned()) {
                s.tracks.insert(song.0.clone(), g.clone());
                s.miss.remove(&song.0);
                batch.push((song.0, g));
                found += 1;
                pending -= 1;
                continue;
            }
            match c.album.as_ref().and_then(|a| by_album.get(a).copied()) {
                Some(k) => groups[k].songs.push((song, c.isrc)),
                None => {
                    if let Some(a) = &c.album {
                        by_album.insert(a.clone(), groups.len());
                    }
                    groups.push(Group { album: c.album, upc: c.upc, songs: vec![(song, c.isrc)] });
                }
            }
        }
    }
    if !batch.is_empty() {
        emit(Found { tracks: std::mem::take(&mut batch), names: Vec::new(), pending });
    }

    let dz = Deezer::new();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        for w in 0..WORKERS.min(groups.len()) {
            let (tx, dz, next, groups) = (tx.clone(), &dz, &next, &groups);
            let spawned = std::thread::Builder::new().name(format!("nanofy-generos-{w}")).stack_size(256 * 1024).spawn_scoped(scope, move || loop {
                let k = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let Some(g) = groups.get(k) else { break };
                if tx.send((k, lookup(dz, store, g))).is_err() {
                    break;
                }
            });
            if let Err(e) = spawned {
                log::warn!("[generos-deezer] hilo {w}: {e}");
            }
        }
        drop(tx);
        let mut since = 0usize;
        for (k, result) in rx {
            let g = &groups[k];
            {
                let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
                match &result {
                    Some((genres, dz_album)) => {
                        for (id, name) in genres {
                            if s.names.insert(*id, name.clone()).is_none() {
                                new_names.push((*id, name.clone()));
                            }
                        }
                        let ids: Vec<u32> = genres.iter().map(|(id, _)| *id).collect();
                        if let Some(a) = dz_album {
                            s.albums.insert(*a, ids.clone());
                        }
                        if let Some(a) = &g.album {
                            s.spotify_albums.insert(a.clone(), ids.clone());
                        }
                        for (song, _) in &g.songs {
                            s.tracks.insert(song.0.clone(), ids.clone());
                            s.miss.remove(&song.0);
                            batch.push((song.0.clone(), ids.clone()));
                        }
                        found += g.songs.len();
                    }
                    // Sin álbum o sin géneros en Deezer: se apunta para no repetir hasta dentro de un mes.
                    None => {
                        for (song, _) in &g.songs {
                            s.miss.insert(song.0.clone(), now);
                        }
                    }
                }
            }
            pending -= g.songs.len();
            since += g.songs.len();
            if pending == 0 || since >= BATCH {
                since = 0;
                store.lock().unwrap_or_else(|e| e.into_inner()).save(path);
                emit(Found { tracks: std::mem::take(&mut batch), names: std::mem::take(&mut new_names), pending });
            }
        }
    });
    if pending > 0 {
        // Algún hilo no arrancó: lo que queda se reintenta la próxima vez.
        store.lock().unwrap_or_else(|e| e.into_inner()).save(path);
        emit(Found { tracks: batch, names: new_names, pending: 0 });
    }
    let st: Vec<usize> = dz.stats.iter().map(|a| a.load(std::sync::atomic::Ordering::Relaxed)).collect();
    log::info!(
        "[generos-deezer] {found} de {total} con géneros en {} s: {} álbumes, {} consultas (álbum por UPC {} + {} con su otra forma, por ISRC {}, buscando {})",
        t0.elapsed().as_secs(),
        groups.len(),
        st[0],
        st[1],
        st[2],
        st[3],
        st[4]
    );
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
    fn upc_con_y_sin_ceros() {
        assert_eq!(upc_forms("0602445790098"), vec!["602445790098", "0602445790098"]);
        assert_eq!(upc_forms("196871234567"), vec!["196871234567"]);
        assert_eq!(upc_forms("00000"), vec!["00000"]);
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
