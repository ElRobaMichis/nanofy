//! Cliente de la Web API de Spotify y de algunos endpoints internos (letras, Jam) vía
//! `spclient` de librespot. Peticiones bloqueantes con `ureq` en un pequeño grupo de hilos;
//! el token sale de la sesión de librespot (login5), así que no hace falta registrar una app.

use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::backend::Shared;
use crate::bus::{Msg, UiTx};
use crate::model::*;
use crate::webauth::WebAuth;
use librespot_core::SpotifyUri;
use librespot_metadata::Metadata;

#[derive(serde::Deserialize)]
struct TracksResponse {
    #[serde(default)]
    tracks: Vec<Option<Track>>,
}

const BASE: &str = "https://api.spotify.com/v1";
const WORKERS: usize = 2;
/// Limitador de ritmo de la Web API (token bucket). Permite una rafaga inicial (el arranque
/// pide varias cosas a la vez) y luego un ritmo sostenido bajo el umbral de Spotify.
const RATE_BURST: f64 = 6.0;
/// Fichas por segundo que se reponen (ritmo sostenido). Conservador para no disparar el límite
/// por usuario del id compartido de primera parte.
const RATE_REFILL: f64 = 2.0;

#[derive(Clone, Debug)]
pub enum Req {
    Me,
    Playlists,
    PlaylistMeta(String),
    PlaylistTracks(String),
    Liked,
    /// Las 100 canciones guardadas más recientes (para refrescar la instantánea).
    LikedRecent,
    SavedAlbums,
    FollowedArtists,
    Album(String),
    Artist(String),
    ArtistTop(String),
    /// Pista completa (álbum, portada) por los metadatos internos.
    TrackInfo(String),
    /// Radio de una canción: Spotify la genera como playlist; devuelve su id.
    RadioPlaylist(String),
    /// Playlists creadas por el propio artista (búsqueda filtrada por propietario).
    ArtistPlaylists { id: String, name: String },
    ArtistAlbums(String),
    Search(String),
    Recent,
    Devices,
    PlayerState,
    /// Última actividad de la cuenta (historial), para comparar con la copia local.
    LastPlayback,
    Queue,
    AddToQueue(String),
    /// Resuelve la cola compartida de una Jam (pista actual + siguientes, por uri) a pistas con
    /// metadatos, para mostrarla en la interfaz.
    JamQueue {
        current: String,
        next: Vec<String>,
    },
    Transfer {
        device_id: String,
    },
    RemotePlay {
        device_id: Option<String>,
        context_uri: Option<String>,
        uris: Option<Vec<String>>,
        offset_uri: Option<String>,
        offset_index: Option<u32>,
    },
    RemotePause,
    RemoteResume,
    RemoteNext,
    RemotePrev,
    RemoteSeek(u32),
    RemoteVolume(u8),
    RemoteShuffle(bool),
    RemoteRepeat(&'static str),
    Save(Vec<String>),
    Unsave(Vec<String>),
    SaveAlbum(String),
    UnsaveAlbum(String),
    /// Descarga pistas a la caché de audio de librespot (reproducción sin red después).
    Download { ids: Vec<String>, episodes: bool, quality: crate::config::Quality },
    /// Podcasts seguidos y episodios guardados (ids).
    SavedShows,
    SavedEpisodes,
    SavedAudiobooks,
    /// Carpetas de playlists (rootlist interno por spclient).
    Rootlist,
    /// Sonda de depuración: GET a un endpoint interno (spclient) y volcado a disco.
    Probe(String),
    /// Carpetas (rootlist interno, playlist4 «changes»): crear, borrar, renombrar y mover playlists.
    FolderCreate { name: String, playlists: Vec<String> },
    FolderDelete(String),
    FolderRename { id: String, name: String },
    /// Mueve una playlist dentro de una carpeta (`Some(id)`) o la saca (`None`).
    FolderMove { playlist: String, folder: Option<String> },
    /// Enlace de invitación para colaborar (servicio playlist-permission, funciona en públicas).
    InviteLink(String),
    /// Colaboradores de una playlist (usuario → nivel).
    Members(String),
    /// Nivel de un miembro: `None` lo quita.
    SetMember { playlist: String, user: String, contributor: bool },
    /// Permiso base de la playlist: `contributor` = cualquiera con el enlace puede editar.
    SetBase { playlist: String, contributor: bool },
    /// Página de inicio personalizada (Daily Mix, Discover Weekly, artistas favoritos…).
    HomeFeed,
    FollowShow(String, bool),
    SaveEpisode(String, bool),
    // Playlists
    CreatePlaylist {
        user_id: String,
        name: String,
        description: String,
        public: bool,
        collaborative: bool,
    },
    UpdatePlaylist {
        id: String,
        name: String,
        description: String,
        public: bool,
        collaborative: bool,
    },
    SetPlaylistImage {
        id: String,
        path: PathBuf,
    },
    AddToPlaylist {
        id: String,
        uris: Vec<String>,
    },
    RemoveFromPlaylist {
        id: String,
        uris: Vec<String>,
    },
    FollowPlaylist(String),
    UnfollowPlaylist(String),
    // Perfiles y seguimiento
    User(String),
    UserPlaylists(String),
    FollowContains {
        kind: &'static str,
        ids: Vec<String>,
    },
    Follow {
        kind: &'static str,
        id: String,
    },
    Unfollow {
        kind: &'static str,
        id: String,
    },
    // Letras: endpoint interno de Spotify vía librespot y, si no hay, LRCLIB (abierto).
    Lyrics {
        id: String,
        name: String,
        artist: String,
        album: String,
        duration_ms: u32,
    },
    JamCurrent,
    JamJoin(String),
    JamLeave(String),
    JamEnd(String),
    /// Vista completa de artista por los metadatos internos: biografía, relacionados, discografía por grupos.
    ArtistView(String),
    /// Podcast / audiolibro con sus episodios.
    Show(String),
    /// Conecta la Web API con la app de desarrollador del usuario (abre el navegador).
    WebConnect(String),
    WebDisconnect,
    WebConnectPersonal(String),
    WebDisconnectPersonal,
}

pub enum Resp {
    Me(User),
    Playlists(Vec<Playlist>),
    PlaylistMeta(Playlist),
    /// Página de pistas de una lista (`key` = id de playlist o "liked").
    Tracks {
        key: String,
        tracks: Vec<Track>,
        total: u32,
        done: bool,
    },
    SavedAlbums(Vec<Album>),
    FollowedArtists(Vec<Artist>),
    Album(Album),
    Artist(Artist),
    ArtistTop(Vec<Track>),
    TrackInfo(Track),
    RadioPlaylist { playlist_id: String },
    ArtistPlaylists { id: String, playlists: Vec<Playlist> },
    ArtistAlbums(Vec<AlbumRef>),
    Search(SearchResult),
    Recent(Vec<Track>),
    Devices(Vec<Device>),
    PlayerState(Option<PlaybackState>),
    Queue(QueueResponse),
    JamQueue(QueueResponse),
    Saved {
        ids: Vec<String>,
        saved: bool,
    },
    PlaylistCreated(Playlist),
    AlbumSaved { id: String, saved: bool },
    SavedShows(Vec<Show>),
    SavedAudiobooks(Vec<Audiobook>),
    Folders(Vec<Folder>),
    LastPlayback(Option<ServerLast>),
    /// El rootlist cambió (carpetas): hay que recargarlo.
    RootlistChanged,
    InviteLink { playlist: String, link: String },
    Members { playlist: String, members: Vec<(String, bool)> },
    HomeFeed(Vec<HomeSection>),
    SavedEpisodes(Vec<SavedEpisode>),
    ShowFollowed { id: String, on: bool },
    EpisodeSaved { id: String, on: bool },
    Downloaded(String),
    PlaylistChanged(String),
    User(UserProfile),
    UserPlaylists {
        user_id: String,
        playlists: Vec<Playlist>,
    },
    FollowContains {
        kind: &'static str,
        ids: Vec<String>,
        following: Vec<bool>,
    },
    Followed {
        kind: &'static str,
        id: String,
        following: bool,
    },
    Lyrics(Option<Lyrics>),
    Jam(Option<JamSession>),
    WebConnected,
    WebDisconnected,
    ArtistView { id: String, view: ArtistView },
    Show { show: Show, episodes: Vec<Episode> },
    Done,
}

pub struct ApiResult {
    pub req: Req,
    pub result: Result<Resp, String>,
}

pub struct Api {
    tx: mpsc::Sender<Req>,
    prio_tx: mpsc::Sender<Req>,
    web: Arc<WebAuth>,
    web_personal: Arc<WebAuth>,
}

impl Api {
    pub fn start(
        shared: Arc<Shared>,
        handle: tokio::runtime::Handle,
        web: Arc<WebAuth>,
        ui: UiTx,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<Req>();
        let rx = Arc::new(Mutex::new(rx));
        // Carril prioritario: estado del reproductor, cola y mandos remotos nunca esperan
        // detrás de la biblioteca (inicio, playlists, canciones que te gustan…).
        let (prio_tx, prio_rx) = mpsc::channel::<Req>();
        // Proveedor opcional de la app propia del usuario: lecturas con su propia cuota (rápidas,
        // sin compartir límite). Las escrituras siguen yendo por la identidad de primera parte.
        let web_personal = Arc::new(WebAuth::load_personal(web.state_dir().join("webapi_personal.json")));

        let tls = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .http_status_as_error(false)
            .tls_config(tls)
            .build();
        let cooldown_file = web.state_dir().join("api_cooldown");
        let genres_file = web.state_dir().join("genres.json");
        let genres: std::collections::HashMap<String, Vec<String>> = std::fs::read_to_string(&genres_file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let cooldown_until = std::fs::read_to_string(&cooldown_file)
            .ok()
            .and_then(|t| t.trim().parse::<u64>().ok())
            .filter(|&until| until > crate::cache::now_secs());
        let client = Arc::new(Client {
            agent: ureq::Agent::new_with_config(config),
            shared,
            handle,
            web: web.clone(),
            web_personal: web_personal.clone(),
            cooldown_until: Mutex::new(cooldown_until),
            personal_cooldown_until: Mutex::new(None),
            cooldown_file,
            genres: Mutex::new(genres),
            genres_file,
            genres_miss: Mutex::new(std::collections::HashSet::new()),
            tracks_blocked: std::sync::atomic::AtomicBool::new(false),
            artist_albums_blocked: std::sync::atomic::AtomicBool::new(false),
            rate: Mutex::new((RATE_BURST, Instant::now())),
        });

        fn run(req: Req, client: &Client, ui: &UiTx) {
            let t0 = std::time::Instant::now();
            let result = client.exec(&req, ui);
            match &result {
                Ok(_) => log::debug!("api {:?} ok en {} ms", req, t0.elapsed().as_millis()),
                Err(e) => log::warn!("api {:?} error: {e}", req),
            }
            ui.send(Msg::Api(ApiResult { req, result }));
        }
        for i in 0..WORKERS {
            let rx = rx.clone();
            let client = client.clone();
            let ui = ui.clone();
            std::thread::Builder::new()
                .name(format!("nanofy-api-{i}"))
                .stack_size(512 * 1024)
                .spawn(move || loop {
                    // Solo la espera está serializada; cada petición se ejecuta en paralelo.
                    let req = {
                        let guard = rx.lock().unwrap();
                        guard.recv()
                    };
                    match req {
                        Ok(req) => run(req, &client, &ui),
                        Err(_) => break,
                    }
                })
                .expect("no se pudo crear el hilo de la API");
        }
        {
            let client = client.clone();
            let ui = ui.clone();
            std::thread::Builder::new()
                .name("nanofy-api-player".into())
                .stack_size(512 * 1024)
                .spawn(move || {
                    while let Ok(req) = prio_rx.recv() {
                        run(req, &client, &ui);
                    }
                })
                .expect("no se pudo crear el hilo de la API");
        }
        Self { tx, prio_tx, web, web_personal }
    }

    /// Envía por el carril prioritario (no espera detrás de la biblioteca). Para la
    /// restauración: las pistas del contexto deben llegar ya.
    pub fn send_priority(&self, req: Req) {
        let _ = self.prio_tx.send(req);
    }

    pub fn send(&self, req: Req) {
        let player = matches!(
            req,
            Req::PlayerState
                | Req::Queue
                | Req::Devices
                | Req::AddToQueue(_)
                | Req::Transfer { .. }
                | Req::RemotePlay { .. }
                | Req::RemotePause
                | Req::RemoteResume
                | Req::RemoteNext
                | Req::RemotePrev
                | Req::RemoteSeek(_)
                | Req::RemoteVolume(_)
                | Req::RemoteShuffle(_)
                | Req::RemoteRepeat(_)
        );
        let _ = if player { self.prio_tx.send(req) } else { self.tx.send(req) };
    }

    /// `true` si la Web API usa la app de desarrollador del usuario.
    pub fn web_configured(&self) -> bool {
        self.web.configured()
    }

    /// `true` si el usuario ha conectado además su propia app para lecturas rápidas.
    pub fn personal_configured(&self) -> bool {
        self.web_personal.configured()
    }
}

struct Client {
    agent: ureq::Agent,
    shared: Arc<Shared>,
    handle: tokio::runtime::Handle,
    web: Arc<WebAuth>,
    web_personal: Arc<WebAuth>,
    /// Hasta cuándo Spotify ha bloqueado la cuota de la app (429 con Retry-After largo).
    /// Se persiste en disco para no gastar más cuota al reiniciar.
    cooldown_until: Mutex<Option<u64>>,
    /// La app PROPIA del usuario agotó su cuota (QUOTA_EXCEEDED): hasta cuándo no se usa; el
    /// resto de tokens sigue funcionando.
    personal_cooldown_until: Mutex<Option<u64>>,
    cooldown_file: PathBuf,
    /// Géneros por "artist:<id>" / "album:<id>" (fuentes externas; se guardan en disco).
    genres: Mutex<std::collections::HashMap<String, Vec<String>>>,
    genres_file: PathBuf,
    /// Claves ya consultadas sin resultado en esta sesión (no se repiten).
    genres_miss: Mutex<std::collections::HashSet<String>>,
    /// Endpoints que Spotify ha rechazado (403/400) en esta sesión: se usa librespot directamente.
    tracks_blocked: std::sync::atomic::AtomicBool,
    artist_albums_blocked: std::sync::atomic::AtomicBool,
    /// Limitador de ritmo propio (token bucket) para la Web API: mantiene el ritmo por debajo
    /// del umbral de Spotify de forma proactiva, evitando los 429 en vez de reaccionar a ellos.
    /// (tokens disponibles, momento del último relleno).
    rate: Mutex<(f64, Instant)>,
}

impl Client {
    fn session(&self) -> Result<librespot_core::Session, String> {
        self.shared
            .session
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "no has iniciado sesión".to_string())
    }

    /// Token para la Web API. `write`=escritura (Me gusta, seguir) -> SIEMPRE la identidad de
    /// primera parte (la app propia del usuario no puede escribir en modo desarrollo). Las
    /// lecturas prefieren la app propia (cuota propia, rápida) y si no, la de primera parte.
    /// Devuelve también si el token es de una app propia/primera parte (`true`) o de librespot.
    fn personal_cooled(&self) -> bool {
        self.personal_cooldown_until
            .lock()
            .unwrap()
            .map(|until| until > crate::cache::now_secs())
            .unwrap_or(false)
    }

    fn token(&self, write: bool) -> Result<(String, bool), String> {
        if !write && !self.personal_cooled() {
            if let Some(t) = self.web_personal.access_token()? {
                return Ok((t, true));
            }
        }
        if let Some(t) = self.web.access_token()? {
            return Ok((t, true));
        }
        let session = self.session()?;
        let token = self
            .handle
            .block_on(session.login5().auth_token())
            .map_err(|e| format!("token: {e}"))?;
        Ok((token.access_token, false))
    }

    /// Espera lo justo (si hace falta) para no superar el ritmo permitido de la Web API.
    /// Token bucket: se reponen `RATE_REFILL` fichas por segundo hasta un máximo de `RATE_BURST`;
    /// cada petición consume una. Así una ráfaga corta pasa al instante y el ritmo sostenido
    /// queda por debajo del umbral de Spotify, evitando los 429 antes de que ocurran.
    fn rate_acquire(&self) {
        let sleep = {
            let mut g = self.rate.lock().unwrap();
            let (ref mut tokens, ref mut last) = *g;
            let now = Instant::now();
            *tokens = (*tokens + last.elapsed().as_secs_f64() * RATE_REFILL).min(RATE_BURST);
            *last = now;
            if *tokens >= 1.0 {
                *tokens -= 1.0;
                Duration::ZERO
            } else {
                let deficit = 1.0 - *tokens;
                *tokens = 0.0;
                Duration::from_secs_f64(deficit / RATE_REFILL)
            }
        };
        if sleep > Duration::ZERO {
            std::thread::sleep(sleep);
        }
    }

    /// Ejecuta una petición y devuelve (código HTTP, cuerpo). Reintenta una vez ante 429.
    fn call(
        &self,
        method: &str,
        url: &str,
        body: Option<(&str, &[u8])>,
    ) -> Result<(u16, String), String> {
        if let Some(until) = *self.cooldown_until.lock().unwrap() {
            let now = crate::cache::now_secs();
            if until > now {
                return Err(cooldown_message(until - now));
            }
        }
        let write = method != "GET";
        // Las escrituras (Me gusta, seguir) no esperan en el limitador: son escasas y deben
        // sentirse instantáneas. El limitador espacia solo las lecturas.
        if !write {
            self.rate_acquire();
        }
        // Tokens a intentar. En ESCRITURAS: primero la app PROPIA del usuario (cuota propia,
        // instantánea, y sí puede escribir en /me/library aunque esté en modo desarrollo); si un
        // endpoint concreto la bloquea (403 de modo desarrollo, p. ej. /me/tracks o /me/following),
        // se cae a la identidad de PRIMERA PARTE. En LECTURAS, token() ya elige (propia -> 1a parte).
        // (token, es app con cuota propia, es la app PROPIA del usuario)
        let tokens: Vec<(String, bool, bool)> = {
            let mut v = Vec::new();
            if !self.personal_cooled() {
                if let Some(t) = self.web_personal.access_token()? {
                    v.push((t, true, true));
                }
            }
            if let Some(t) = self.web.access_token()? {
                v.push((t, true, false));
            }
            if v.is_empty() {
                let session = self.session()?;
                let token = self
                    .handle
                    .block_on(session.login5().auth_token())
                    .map_err(|e| format!("token: {e}"))?;
                v.push((token.access_token, false, false));
            }
            v
        };
        let last_i = tokens.len() - 1;
        // Guarda el resultado de un token que falló con 403/429 por si ningún otro token funciona.
        let mut fallback: Option<(u16, String)> = None;
        for (ti, (token, own_app, personal)) in tokens.iter().enumerate() {
            let own_app = *own_app;
            let personal = *personal;
            let is_last = ti == last_i;
            let bearer = format!("Bearer {token}");
            // Presupuesto total de espera para ESCRITURAS ante 429 (el id compartido de primera
            // parte está limitado por cuenta y en frío devuelve Retry-After de hasta ~21 s). La
            // interfaz es optimista, así que esperar en segundo plano evita que el Me gusta se
            // revierta. La app propia casi nunca llega aquí (tiene su propia cuota).
            let mut write_budget = 35u64;
            let mut advance = false;
            for _ in 0..8 {
                let resp = match method {
                    "GET" => self.agent.get(url).header("Authorization", &bearer).call(),
                    "DELETE" => match body {
                        Some((ct, bytes)) => self
                            .agent
                            .delete(url)
                            .header("Authorization", &bearer)
                            .header("Content-Type", ct)
                            .force_send_body()
                            .send(bytes),
                        None => self.agent.delete(url).header("Authorization", &bearer).call(),
                    },
                    _ => {
                        let rb = if method == "PUT" {
                            self.agent.put(url)
                        } else {
                            self.agent.post(url)
                        };
                        let rb = rb.header("Authorization", &bearer);
                        match body {
                            Some((ct, bytes)) => rb.header("Content-Type", ct).send(bytes),
                            None => rb.header("Content-Type", "application/json").send_empty(),
                        }
                    }
                };
                let mut resp = resp.map_err(|e| format!("red: {e}"))?;
                let status = resp.status().as_u16();
                if status == 429 {
                    let retry_after = resp
                        .headers()
                        .get("Retry-After")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(2)
                        .max(1);
                    let text = resp.body_mut().read_to_string().unwrap_or_default();
                    // Cuota mensual real agotada (reason QUOTA_EXCEEDED): si hay otro token que
                    // probar, se prueba; si es el último, enfriamiento largo.
                    if text.contains("QUOTA_EXCEEDED") {
                        // La cuota de ESTE token se ha agotado: se respeta el Retry-After real
                        // (no una hora fija). Si es la app propia y hay otro token, se sigue
                        // con la identidad de primera parte tanto en lecturas como en escrituras.
                        let wait = retry_after.clamp(60, 6 * 3600);
                        if personal {
                            *self.personal_cooldown_until.lock().unwrap() = Some(crate::cache::now_secs() + wait);
                            log::warn!("la app propia agotó su cuota (Retry-After {wait} s); se usa la identidad de primera parte");
                        }
                        if !is_last {
                            fallback = Some((status, text));
                            advance = true;
                            break;
                        }
                        let until = crate::cache::now_secs() + wait;
                        *self.cooldown_until.lock().unwrap() = Some(until);
                        let _ = std::fs::write(&self.cooldown_file, until.to_string());
                        return Err(cooldown_message(wait));
                    }
                    // Las LECTURAS no bloquean la interfaz: fallan rápido (el llamador usa librespot
                    // o la caché). Un token que no es de app propia (login5) sin cuota -> avisar.
                    if !write {
                        if !own_app {
                            return Err(NO_APP_HINT.to_string());
                        }
                        return Err("Spotify está limitando las peticiones; inténtalo de nuevo en unos segundos.".to_string());
                    }
                    // ESCRITURA: reintenta respetando el Retry-After real hasta agotar el presupuesto.
                    if write_budget > 0 {
                        let wait = retry_after.min(write_budget).min(22);
                        write_budget = write_budget.saturating_sub(wait);
                        std::thread::sleep(Duration::from_secs(wait));
                        continue;
                    }
                    // Sin presupuesto: probar el siguiente token si lo hay.
                    if !is_last {
                        fallback = Some((status, text));
                        advance = true;
                        break;
                    }
                    return Err("Spotify está limitando las peticiones; inténtalo de nuevo en unos segundos.".to_string());
                }
                // 403 en ESCRITURA con la app propia (modo desarrollo bloquea este endpoint): se
                // cae al token de primera parte, que sí puede.
                if status == 403 && write && !is_last {
                    let text = resp.body_mut().read_to_string().unwrap_or_default();
                    fallback = Some((status, text));
                    advance = true;
                    break;
                }
                let text = resp.body_mut().read_to_string().unwrap_or_default();
                return Ok((status, text));
            }
            let _ = advance;
        }
        if let Some(r) = fallback {
            return Ok(r);
        }
        Err("Spotify está limitando las peticiones; espera unos segundos".to_string())
    }

    fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T, String> {
        let (status, text) = self.call("GET", url, None)?;
        if !(200..300).contains(&status) {
            return Err(http_error(status, &text));
        }
        serde_json::from_str(&text).map_err(|e| format!("respuesta inesperada: {e}"))
    }

    /// Como `get_json`, pero 204 (sin contenido) devuelve `None`.
    fn get_json_opt<T: DeserializeOwned>(&self, url: &str) -> Result<Option<T>, String> {
        let (status, text) = self.call("GET", url, None)?;
        if status == 204 || (status == 200 && text.trim().is_empty()) {
            return Ok(None);
        }
        if !(200..300).contains(&status) {
            return Err(http_error(status, &text));
        }
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| format!("respuesta inesperada: {e}"))
    }

    fn send_json(&self, method: &str, url: &str, body: Option<Value>) -> Result<String, String> {
        let text = body.map(|b| b.to_string());
        let body = text
            .as_deref()
            .map(|t| ("application/json", t.as_bytes()));
        let (status, resp) = self.call(method, url, body)?;
        if (200..300).contains(&status) {
            Ok(resp)
        } else {
            Err(http_error(status, &resp))
        }
    }

    /// Guarda o quita de la biblioteca (Me gusta, álbumes, artistas, podcasts, episodios) por el
    /// endpoint unificado `/me/library`, que el token de primera parte sí permite escribir. Acepta
    /// cualquier tipo de uri; `PUT` guarda/sigue y `DELETE` quita/deja de seguir.
    fn library_write(&self, add: bool, uris: &[String]) -> Result<(), String> {
        let method = if add { "PUT" } else { "DELETE" };
        let url = format!("{BASE}/me/library?uris={}", uris.join(","));
        self.send_json(method, &url, None).map(|_| ())
    }

    fn all_pages<T: DeserializeOwned>(&self, first: &str, max_pages: usize) -> Result<Vec<T>, String> {
        let mut out = Vec::new();
        let mut url = first.to_string();
        for _ in 0..max_pages {
            let page: Paging<T> = self.get_json(&url)?;
            out.extend(page.items);
            match page.next {
                Some(next) => url = next,
                None => break,
            }
        }
        Ok(out)
    }

    /// Descarga una lista de pistas página a página y envía cada página según llega.
    fn stream_tracks<I: DeserializeOwned>(
        &self,
        req: &Req,
        key: &str,
        first: &str,
        pick: impl Fn(I) -> Option<Track>,
        ui: &UiTx,
    ) -> Result<Resp, String> {
        let mut url = first.to_string();
        let mut pages = 0;
        loop {
            let page: Paging<I> = self.get_json(&url)?;
            let tracks: Vec<Track> = page
                .items
                .into_iter()
                .filter_map(&pick)
                .filter(|t| !t.uri.is_empty())
                .map(slim_track)
                .collect();
            pages += 1;
            match page.next {
                Some(next) if pages < 200 => {
                    ui.send(Msg::Api(ApiResult {
                        req: req.clone(),
                        result: Ok(Resp::Tracks {
                            key: key.to_string(),
                            tracks,
                            total: page.total,
                            done: false,
                        }),
                    }));
                    url = next;
                }
                _ => {
                    return Ok(Resp::Tracks {
                        key: key.to_string(),
                        tracks,
                        total: page.total,
                        done: true,
                    })
                }
            }
        }
    }

    /// Letras desde LRCLIB (https://lrclib.net), sin autenticación.
    fn lrclib(&self, id: &str, name: &str, artist: &str, album: &str, duration_ms: u32) -> Option<Lyrics> {
        let first_artist = artist.split(',').next().unwrap_or(artist).trim();
        let mut url = format!(
            "https://lrclib.net/api/get?track_name={}&artist_name={}&duration={}",
            urlencode(name),
            urlencode(first_artist),
            duration_ms / 1000
        );
        if !album.is_empty() {
            url.push_str("&album_name=");
            url.push_str(&urlencode(album));
        }
        let fetch = |url: &str| -> Option<Value> {
            let mut resp = match self
                .agent
                .get(url)
                .header("User-Agent", "Nanofy/0.1 (cliente nativo de Spotify)")
                .call()
            {
                Ok(r) => r,
                Err(e) => {
                    log::warn!("lrclib {url}: {e}");
                    return None;
                }
            };
            let status = resp.status().as_u16();
            if status != 200 {
                log::info!("lrclib {url}: HTTP {status}");
                return None;
            }
            match resp.body_mut().read_json::<Value>() {
                Ok(v) => Some(v),
                Err(e) => {
                    log::warn!("lrclib {url}: respuesta inesperada: {e}");
                    None
                }
            }
        };
        // Una entrada con menos de 4 líneas suele ser vandalismo o un marcador; en ese caso
        // buscamos otra versión de la misma canción.
        let usable = |v: &Value| -> bool {
            v.get("syncedLyrics")
                .and_then(|s| s.as_str())
                .map(|s| s.lines().count() >= 4)
                .unwrap_or(false)
                || v.get("plainLyrics")
                    .and_then(|s| s.as_str())
                    .map(|s| s.lines().count() >= 4)
                    .unwrap_or(false)
        };
        let exact = fetch(&url).filter(usable);
        let v = exact.or_else(|| {
            let url = format!(
                "https://lrclib.net/api/search?track_name={}&artist_name={}",
                urlencode(name),
                urlencode(first_artist)
            );
            fetch(&url).and_then(|v| {
                v.as_array().and_then(|a| {
                    a.iter()
                        .find(|c| usable(c) && c.get("syncedLyrics").and_then(|s| s.as_str()).is_some())
                        .or_else(|| a.iter().find(|c| usable(c)))
                        .cloned()
                })
            })
        })?;
        let synced = v.get("syncedLyrics").and_then(|s| s.as_str()).unwrap_or("");
        let plain = v.get("plainLyrics").and_then(|s| s.as_str()).unwrap_or("");
        if !synced.is_empty() {
            return Some(Lyrics::from_lrc(id, synced, "LRCLIB"));
        }
        if !plain.is_empty() {
            return Some(Lyrics::from_plain(id, plain, "LRCLIB"));
        }
        None
    }

    /// Uris de las pistas de una playlist por el protocolo interno (playlist4).
    /// (id de pista, usuario que la añadió) de una playlist, por librespot.
    fn playlist_items(&self, id: &str) -> Result<Vec<(String, Option<String>, Option<String>)>, String> {
        let session = self.session()?;
        let uri = SpotifyUri::from_uri(&format!("spotify:playlist:{id}"))
            .map_err(|e| e.to_string())?;
        let pl = self
            .handle
            .block_on(librespot_metadata::Playlist::get(&session, &uri))
            .map_err(|e| format!("playlist: {e}"))?;
        Ok(pl
            .contents
            .items
            .iter()
            .filter_map(|it| match &it.id {
                SpotifyUri::Track { .. } => it.id.to_id().ok().map(|tid| {
                    let by = it.attributes.added_by.trim().to_string();
                    let d = it.attributes.timestamp.as_utc();
                    let date = (d.unix_timestamp() > 0).then(|| format!("{:04}-{:02}-{:02}", d.year(), d.month() as u8, d.day()));
                    (tid, (!by.is_empty()).then_some(by), date)
                }),
                _ => None,
            })
            .collect())
    }

    /// Detalles de pistas por id: Web API por lotes de 50 y, si falla, metadatos de librespot.
    fn tracks_by_ids(&self, ids: &[String]) -> Result<Vec<Track>, String> {
        let mut out = Vec::with_capacity(ids.len());
        use std::sync::atomic::Ordering;
        for chunk in ids.chunks(50) {
            if self.tracks_blocked.load(Ordering::Relaxed) {
                out.extend(self.tracks_via_librespot(chunk)?);
                continue;
            }
            let url = format!("{BASE}/tracks?ids={}", chunk.join(","));
            match self.get_json::<TracksResponse>(&url) {
                Ok(r) => out.extend(r.tracks.into_iter().flatten().map(slim_track)),
                Err(e) => {
                    log::info!("/tracks no disponible ({e}); usando metadatos de librespot");
                    self.tracks_blocked.store(true, Ordering::Relaxed);
                    out.extend(self.tracks_via_librespot(chunk)?);
                }
            }
        }
        Ok(out)
    }

    /// Nombre e imagen de un artista por los metadatos internos.
    fn artist_via_librespot(&self, id: &str) -> Result<Artist, String> {
        let session = self.session()?;
        let uri = SpotifyUri::from_uri(&format!("spotify:artist:{id}")).map_err(|e| e.to_string())?;
        let a = self
            .handle
            .block_on(librespot_metadata::Artist::get(&session, &uri))
            .map_err(|e| format!("artista: {e}"))?;
        let mut images: Vec<Image> = a.portrait_group.iter().map(image_from_meta).collect();
        if images.is_empty() {
            images = a.portraits.iter().map(image_from_meta).collect();
        }
        Ok(Artist {
            id: id.to_string(),
            name: a.name.clone(),
            uri: format!("spotify:artist:{id}"),
            images,
            genres: self.external_genres(&format!("artist:{id}"), &a.name, None),
            followers: None,
        })
    }

    /// GET sin autorización (fuentes públicas: iTunes, Deezer, MusicBrainz), con tiempo límite corto.
    fn plain_get_json(&self, url: &str) -> Option<Value> {
        let resp = self
            .agent
            .get(url)
            .header("User-Agent", "Nanofy/1.0.0 (https://github.com/nanofy)")
            .header("Accept", "application/json")
            .call();
        let mut resp = match resp {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                log::info!("[generos] {url}: HTTP {}", r.status());
                return None;
            }
            Err(e) => {
                log::info!("[generos] {url}: {e}");
                return None;
            }
        };
        let text = resp.body_mut().read_to_string().ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Géneros cacheados o consultados a fuentes externas. `key` = "artist:<id>" o "album:<id>".
    /// Spotify dejó de exponer géneros (Web API y metadatos internos vienen vacíos), así que se
    /// combinan iTunes Search (género principal), Deezer (géneros de álbum) y MusicBrainz (etiquetas).
    fn external_genres(&self, key: &str, artist: &str, album: Option<&str>) -> Vec<String> {
        if let Some(g) = self.genres.lock().unwrap().get(key) {
            return g.clone();
        }
        if self.genres_miss.lock().unwrap().contains(key) {
            return Vec::new();
        }
        let norm = |s: &str| s.trim().to_lowercase();
        let mut out: Vec<String> = Vec::new();
        let push = |g: &str, out: &mut Vec<String>| {
            let g = g.trim().to_lowercase();
            if !g.is_empty() && g != "música" && g != "music" && g != "worldwide" && !out.contains(&g) {
                out.push(g);
            }
        };
        match album {
            Some(album) => {
                // Deezer: búsqueda del álbum y géneros del álbum (varios).
                let q = urlencode(&format!("artist:\"{artist}\" album:\"{album}\""));
                if let Some(v) = self.plain_get_json(&format!("https://api.deezer.com/search/album?q={q}&limit=1")) {
                    let hit = v["data"][0].clone();
                    let title = hit["title"].as_str().unwrap_or("");
                    if !title.is_empty() && (norm(title) == norm(album) || norm(title).starts_with(&norm(album)) || norm(album).starts_with(&norm(title))) {
                        if let Some(id) = hit["id"].as_i64() {
                            if let Some(a) = self.plain_get_json(&format!("https://api.deezer.com/album/{id}")) {
                                if let Some(gs) = a["genres"]["data"].as_array() {
                                    for g in gs {
                                        if let Some(n) = g["name"].as_str() {
                                            push(n, &mut out);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                // iTunes: género principal del álbum (si el título coincide).
                let q = urlencode(&format!("{artist} {album}"));
                if let Some(v) = self.plain_get_json(&format!("https://itunes.apple.com/search?term={q}&entity=album&limit=3")) {
                    if let Some(items) = v["results"].as_array() {
                        for it in items {
                            let name = it["collectionName"].as_str().unwrap_or("");
                            let by = it["artistName"].as_str().unwrap_or("");
                            if norm(by) == norm(artist) && (norm(name) == norm(album) || norm(name).starts_with(&norm(album))) {
                                if let Some(g) = it["primaryGenreName"].as_str() {
                                    push(g, &mut out);
                                }
                                break;
                            }
                        }
                    }
                }
            }
            None => {
                // iTunes: género principal del artista.
                let q = urlencode(artist);
                if let Some(v) = self.plain_get_json(&format!("https://itunes.apple.com/search?term={q}&entity=musicArtist&limit=3")) {
                    if let Some(items) = v["results"].as_array() {
                        for it in items {
                            if norm(it["artistName"].as_str().unwrap_or("")) == norm(artist) {
                                if let Some(g) = it["primaryGenreName"].as_str() {
                                    push(g, &mut out);
                                }
                                break;
                            }
                        }
                    }
                }
                // MusicBrainz: etiquetas de la comunidad (las más votadas), si el servidor responde.
                let q = urlencode(&format!("artist:\"{artist}\""));
                if let Some(v) = self.plain_get_json(&format!("https://musicbrainz.org/ws/2/artist/?query={q}&fmt=json&limit=1")) {
                    let hit = v["artists"][0].clone();
                    if hit["score"].as_i64().unwrap_or(0) >= 90 && norm(hit["name"].as_str().unwrap_or("")) == norm(artist) {
                        if let Some(tags) = hit["tags"].as_array() {
                            let mut tags: Vec<(i64, String)> = tags
                                .iter()
                                .filter_map(|t| Some((t["count"].as_i64().unwrap_or(0), t["name"].as_str()?.to_string())))
                                .filter(|(c, _)| *c > 0)
                                .collect();
                            tags.sort_by(|a, b| b.0.cmp(&a.0));
                            for (_, t) in tags.into_iter().take(4) {
                                push(&t, &mut out);
                            }
                        }
                    }
                }
            }
        }
        log::info!("[generos] {key} ({artist}{}): {out:?}", album.map(|a| format!(" — {a}")).unwrap_or_default());
        if out.is_empty() {
            self.genres_miss.lock().unwrap().insert(key.to_string());
        } else {
            let mut map = self.genres.lock().unwrap();
            map.insert(key.to_string(), out.clone());
            if let Ok(t) = serde_json::to_string(&*map) {
                let _ = std::fs::write(&self.genres_file, t);
            }
        }
        out
    }

    /// Álbum completo (con pistas) por los metadatos internos.
    fn album_via_librespot(&self, id: &str) -> Result<Album, String> {
        let session = self.session()?;
        let uri = SpotifyUri::from_uri(&format!("spotify:album:{id}")).map_err(|e| e.to_string())?;
        let a = self
            .handle
            .block_on(librespot_metadata::Album::get(&session, &uri))
            .map_err(|e| format!("álbum: {e}"))?;
        let ids: Vec<String> = a.discs.iter().flat_map(|d| d.tracks.iter()).filter_map(|u| u.to_id().ok()).collect();
        let tracks = self.tracks_via_librespot(&ids)?;
        let total = tracks.len() as u32;
        Ok(Album {
            genres: self.external_genres(&format!("album:{id}"), &a.artists.first().map(|x| x.name.clone()).unwrap_or_default(), Some(&a.name)),
            id: id.to_string(),
            name: a.name.clone(),
            uri: format!("spotify:album:{id}"),
            images: a.covers.iter().map(image_from_meta).collect(),
            artists: a
                .artists
                .iter()
                .map(|ar| ArtistRef {
                    id: ar.id.to_id().ok(),
                    name: ar.name.clone(),
                    uri: ar.id.to_uri().ok(),
                })
                .collect(),
            release_date: Some(format!("{:04}", a.date.as_utc().year())),
            total_tracks: Some(total),
            album_type: Some(
                match a.album_type {
                    librespot_metadata::album::AlbumType::SINGLE => "single",
                    librespot_metadata::album::AlbumType::COMPILATION => "compilation",
                    _ => "album",
                }
                .to_string(),
            ),
            tracks: Some(Paging { items: tracks, next: None, total }),
        })
    }

    /// Metadatos de una playlist por librespot (nombre, descripción, portada, tamaño).
    fn playlist_meta_via_librespot(&self, id: &str) -> Result<Playlist, String> {
        let session = self.session()?;
        let uri = SpotifyUri::from_uri(&format!("spotify:playlist:{id}")).map_err(|e| e.to_string())?;
        let pl = self
            .handle
            .block_on(librespot_metadata::Playlist::get(&session, &uri))
            .map_err(|e| format!("playlist: {e}"))?;
        let a = &pl.attributes;
        let images = if a.picture.is_empty() {
            // Radios y mixes: la portada generada viene en las variantes por tamaño.
            a.picture_sizes
                .iter()
                .max_by_key(|p| match p.target_name.as_str() {
                    "xlarge" => 4,
                    "large" => 3,
                    "default" => 2,
                    _ => 1,
                })
                .filter(|p| !p.url.is_empty())
                .map(|p| vec![Image { url: p.url.clone(), width: Some(300), height: Some(300) }])
                .unwrap_or_default()
        } else {
            let hex: String = a.picture.iter().map(|b| format!("{b:02x}")).collect();
            vec![Image { url: format!("https://i.scdn.co/image/{hex}"), width: Some(300), height: Some(300) }]
        };
        // Las playlists algorítmicas de Spotify comparten prefijo de id.
        let spotify_made = id.starts_with("37i9dQZ");
        Ok(Playlist {
            id: id.to_string(),
            name: a.name.clone(),
            uri: format!("spotify:playlist:{id}"),
            description: Some(a.description.clone()).filter(|d| !d.is_empty()),
            images: Some(images),
            owner: Owner {
                display_name: spotify_made.then(|| "Spotify".to_string()),
                id: spotify_made.then(|| "spotify".to_string()),
            },
            tracks: Some(TracksRef { total: pl.length.max(0) as u32 }),
            public: None,
            collaborative: Some(a.is_collaborative),
            followers: None,
        })
    }

    /// Álbumes a partir de uris internos (en paralelo), ordenados por fecha descendente.
    fn albums_from_uris(&self, session: &librespot_core::Session, uris: &[SpotifyUri]) -> Vec<AlbumRef> {
        let fetched = self.handle.block_on(async {
            futures::future::join_all(uris.iter().map(|u| librespot_metadata::Album::get(session, u))).await
        });
        let mut out: Vec<AlbumRef> = fetched
            .into_iter()
            .filter_map(|r| r.ok())
            .map(|a| AlbumRef {
                id: a.id.to_id().ok(),
                name: a.name.clone(),
                uri: a.id.to_uri().ok(),
                images: a.covers.iter().map(image_from_meta).collect(),
                artists: a
                    .artists
                    .iter()
                    .map(|ar| ArtistRef { id: ar.id.to_id().ok(), name: ar.name.clone(), uri: ar.id.to_uri().ok() })
                    .collect(),
                release_date: Some(format!("{:04}-{:02}-{:02}", a.date.as_utc().year(), a.date.as_utc().month() as u8, a.date.as_utc().day())),
                total_tracks: Some(a.discs.iter().map(|d| d.tracks.len() as u32).sum()),
                album_type: Some(
                    match a.album_type {
                        librespot_metadata::album::AlbumType::SINGLE => "single",
                        librespot_metadata::album::AlbumType::COMPILATION => "compilation",
                        _ => "album",
                    }
                    .to_string(),
                ),
            })
            .collect();
        out.sort_by(|x, y| y.release_date.cmp(&x.release_date));
        out
    }

    /// Vista completa del artista: biografía, relacionados y discografía por grupos.
    fn artist_view_via_librespot(&self, id: &str) -> Result<ArtistView, String> {
        let session = self.session()?;
        let uri = SpotifyUri::from_uri(&format!("spotify:artist:{id}")).map_err(|e| e.to_string())?;
        let a = self
            .handle
            .block_on(librespot_metadata::Artist::get(&session, &uri))
            .map_err(|e| format!("artista: {e}"))?;
        let firsts = |groups: &librespot_metadata::artist::AlbumGroups, cap: usize| -> Vec<SpotifyUri> {
            groups.iter().filter_map(|g| g.first().cloned()).take(cap).collect()
        };
        let albums = self.albums_from_uris(&session, &firsts(&a.albums, 40));
        let singles = self.albums_from_uris(&session, &firsts(&a.singles, 40));
        let compilations = self.albums_from_uris(&session, &firsts(&a.compilations, 20));
        let appears_on = self.albums_from_uris(&session, &firsts(&a.appears_on_albums, 30));
        let latest = albums.iter().chain(singles.iter()).max_by(|x, y| x.release_date.cmp(&y.release_date)).cloned();
        let mut header: Vec<Image> = a.portrait_group.iter().map(image_from_meta).collect();
        if header.is_empty() {
            header = a.portraits.iter().map(image_from_meta).collect();
        }
        let related = a
            .related
            .iter()
            .filter_map(|r| {
                let rid = r.id.to_id().ok()?;
                let mut images: Vec<Image> = r.portrait_group.iter().map(image_from_meta).collect();
                if images.is_empty() {
                    images = r.portraits.iter().map(image_from_meta).collect();
                }
                Some(Artist { id: rid.clone(), name: r.name.clone(), uri: format!("spotify:artist:{rid}"), images, genres: Vec::new(), followers: None })
            })
            .collect();
        Ok(ArtistView {
            header: pick_image(&header, 1000).map(|s| s.to_string()),
            biography: a.biographies.first().map(|b| crate::app::strip_html(&b.text)).unwrap_or_default(),
            related,
            albums,
            singles,
            compilations,
            appears_on,
            latest,
        })
    }

    /// Podcast con episodios por los metadatos internos.
    fn show_via_librespot(&self, id: &str) -> Result<(Show, Vec<Episode>), String> {
        let session = self.session()?;
        let uri = SpotifyUri::from_uri(&format!("spotify:show:{id}")).map_err(|e| e.to_string())?;
        let s = self
            .handle
            .block_on(librespot_metadata::Show::get(&session, &uri))
            .map_err(|e| format!("podcast: {e}"))?;
        let ep_uris: Vec<SpotifyUri> = s.episodes.iter().take(50).cloned().collect();
        let fetched = self.handle.block_on(async {
            futures::future::join_all(ep_uris.iter().map(|u| librespot_metadata::Episode::get(&session, u))).await
        });
        let episodes: Vec<Episode> = fetched
            .into_iter()
            .filter_map(|r| r.ok())
            .map(|e| Episode {
                id: e.id.to_id().unwrap_or_default(),
                name: e.name.clone(),
                uri: e.id.to_uri().unwrap_or_default(),
                images: e.covers.iter().map(image_from_meta).collect(),
                duration_ms: e.duration.max(0) as u32,
                release_date: Some(format!("{:04}-{:02}-{:02}", e.publish_time.as_utc().year(), e.publish_time.as_utc().month() as u8, e.publish_time.as_utc().day())),
                description: e.description.clone(),
                explicit: e.is_explicit,
            })
            .collect();
        let show = Show {
            id: id.to_string(),
            name: s.name.clone(),
            uri: format!("spotify:show:{id}"),
            images: s.covers.iter().map(image_from_meta).collect(),
            publisher: s.publisher.clone(),
            description: s.description.clone(),
            total_episodes: Some(s.episodes.len() as u32),
            media_type: None,
            keywords: s.keywords.clone(),
        };
        Ok((show, episodes))
    }

    /// Discografía (álbumes y sencillos) por los metadatos internos.
    fn artist_albums_via_librespot(&self, id: &str) -> Result<Vec<AlbumRef>, String> {
        let session = self.session()?;
        let uri = SpotifyUri::from_uri(&format!("spotify:artist:{id}")).map_err(|e| e.to_string())?;
        let artist = self
            .handle
            .block_on(librespot_metadata::Artist::get(&session, &uri))
            .map_err(|e| format!("artista: {e}"))?;
        // Cada grupo agrupa versiones del mismo álbum: nos quedamos con la primera.
        let mut uris: Vec<SpotifyUri> = Vec::new();
        for group in artist.albums.iter().chain(artist.singles.iter()) {
            if let Some(first) = group.first() {
                uris.push(first.clone());
            }
            if uris.len() >= 60 {
                break;
            }
        }
        let fetched = self.handle.block_on(async {
            futures::future::join_all(
                uris.iter()
                    .map(|u| librespot_metadata::Album::get(&session, u)),
            )
            .await
        });
        let mut out: Vec<AlbumRef> = fetched
            .into_iter()
            .filter_map(|r| r.ok())
            .map(|a| AlbumRef {
                id: a.id.to_id().ok(),
                name: a.name.clone(),
                uri: a.id.to_uri().ok(),
                images: a.covers.iter().map(image_from_meta).collect(),
                artists: a
                    .artists
                    .iter()
                    .map(|ar| ArtistRef {
                        id: ar.id.to_id().ok(),
                        name: ar.name.clone(),
                        uri: ar.id.to_uri().ok(),
                    })
                    .collect(),
                release_date: Some(format!("{:04}", a.date.as_utc().year())),
                total_tracks: Some(a.discs.iter().map(|d| d.tracks.len() as u32).sum()),
                album_type: Some(
                    match a.album_type {
                        librespot_metadata::album::AlbumType::SINGLE => "single",
                        librespot_metadata::album::AlbumType::COMPILATION => "compilation",
                        _ => "album",
                    }
                    .to_string(),
                ),
            })
            .collect();
        out.sort_by(|x, y| y.release_date.cmp(&x.release_date));
        Ok(out)
    }

    fn tracks_via_librespot(&self, ids: &[String]) -> Result<Vec<Track>, String> {
        let session = self.session()?;
        let uris: Vec<SpotifyUri> = ids
            .iter()
            .filter_map(|id| SpotifyUri::from_uri(&format!("spotify:track:{id}")).ok())
            .collect();
        let fetched = self.handle.block_on(async {
            futures::future::join_all(
                uris.iter()
                    .map(|u| librespot_metadata::Track::get(&session, u)),
            )
            .await
        });
        Ok(fetched
            .into_iter()
            .filter_map(|r| r.ok())
            .map(track_from_meta)
            .collect())
    }

    /// Petición protobuf a un endpoint interno (spclient): cuerpo y respuesta en bytes.
    fn spclient_pb(&self, method: http::Method, endpoint: &str, body: Option<&[u8]>) -> Result<Vec<u8>, String> {
        let session = self.session()?;
        let mut headers = http::HeaderMap::new();
        headers.insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("application/x-protobuf"));
        headers.insert(http::header::ACCEPT, http::HeaderValue::from_static("application/x-protobuf"));
        let bytes = self
            .handle
            .block_on(session.spclient().request(&method, endpoint, Some(headers), body))
            .map_err(|e| format!("{e}"))?;
        Ok(bytes.to_vec())
    }

    /// Rootlist completo (uris en orden) y su revisión.
    fn rootlist_raw(&self) -> Result<(Vec<u8>, Vec<String>), String> {
        use protobuf::Message;
        let session = self.session()?;
        let bytes = self
            .handle
            .block_on(session.spclient().get_rootlist(0, Some(5000)))
            .map_err(|e| format!("rootlist: {e}"))?;
        let msg = librespot_protocol::playlist4_external::SelectedListContent::parse_from_bytes(&bytes)
            .map_err(|e| format!("rootlist: {e}"))?;
        let uris = msg.contents.items.iter().map(|i| i.uri().to_string()).collect();
        Ok((msg.revision().to_vec(), uris))
    }

    /// Aplica operaciones al rootlist (playlist4 «changes»). Los índices de cada op se calculan
    /// sobre `list`, que el llamador mantiene sincronizada con lo que hará el servidor.
    fn rootlist_changes(&self, revision: Vec<u8>, ops: Vec<librespot_protocol::playlist4_external::Op>) -> Result<(), String> {
        use librespot_protocol::playlist4_external::{ChangeInfo, Delta, ListChanges};
        use protobuf::{Message, MessageField};
        let session = self.session()?;
        let user = session.username();
        let mut info = ChangeInfo::new();
        info.set_user(user.clone());
        info.set_timestamp(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0));
        let mut delta = Delta::new();
        delta.ops = ops;
        delta.info = MessageField::some(info);
        let mut changes = ListChanges::new();
        changes.set_base_revision(revision);
        changes.deltas.push(delta);
        changes.set_want_resulting_revisions(true);
        changes.set_want_sync_result(true);
        let body = changes.write_to_bytes().map_err(|e| e.to_string())?;
        let endpoint = format!("/playlist/v2/user/{user}/rootlist/changes");
        let out = self.spclient_pb(http::Method::POST, &endpoint, Some(&body))?;
        log::info!("[rootlist] changes: {} bytes de respuesta", out.len());
        Ok(())
    }

    /// Revisión actual de una playlist (para playlist4 «changes»).
    fn playlist_revision(&self, id: &str) -> Result<Vec<u8>, String> {
        use protobuf::Message;
        let session = self.session()?;
        let sid = librespot_core::SpotifyId::from_base62(id).map_err(|e| e.to_string())?;
        let bytes = self.handle.block_on(session.spclient().get_playlist(&sid)).map_err(|e| format!("{e}"))?;
        let msg = librespot_protocol::playlist4_external::SelectedListContent::parse_from_bytes(&bytes).map_err(|e| e.to_string())?;
        Ok(msg.revision().to_vec())
    }

    /// Añade pistas a una playlist por el protocolo interno (funciona para el dueño y para
    /// colaboradores, sin depender de que la Web API autorice la app).
    fn playlist_add_internal(&self, id: &str, uris: &[String]) -> Result<(), String> {
        use librespot_protocol::playlist4_external::{op, Add, ChangeInfo, Delta, Item, ListChanges, Op};
        use protobuf::{Message, MessageField};
        let session = self.session()?;
        let user = session.username();
        let rev = self.playlist_revision(id)?;
        let mut add = Add::new();
        add.set_add_last(true);
        for u in uris {
            let mut it = Item::new();
            it.set_uri(u.clone());
            add.items.push(it);
        }
        let mut op = Op::new();
        op.set_kind(op::Kind::ADD);
        op.add = MessageField::some(add);
        let mut info = ChangeInfo::new();
        info.set_user(user.clone());
        info.set_timestamp(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0));
        let mut delta = Delta::new();
        delta.ops.push(op);
        delta.info = MessageField::some(info);
        let mut changes = ListChanges::new();
        changes.set_base_revision(rev);
        changes.deltas.push(delta);
        changes.set_want_resulting_revisions(true);
        let body = changes.write_to_bytes().map_err(|e| e.to_string())?;
        let out = self.spclient_pb(http::Method::POST, &format!("/playlist/v2/playlist/{id}/changes"), Some(&body))?;
        log::info!("[playlist] {id}: {} pistas añadidas por protocolo interno ({} bytes)", uris.len(), out.len());
        Ok(())
    }

    /// Quita pistas de una playlist por el protocolo interno (Rem por uri, colaboradores incluidos).
    fn playlist_remove_internal(&self, id: &str, uris: &[String]) -> Result<(), String> {
        use librespot_protocol::playlist4_external::{op, ChangeInfo, Delta, Item, ListChanges, Op, Rem};
        use protobuf::{Message, MessageField};
        let session = self.session()?;
        let user = session.username();
        let rev = self.playlist_revision(id)?;
        let mut rem = Rem::new();
        rem.set_items_as_key(true);
        for u in uris {
            let mut it = Item::new();
            it.set_uri(u.clone());
            rem.items.push(it);
        }
        let mut op = Op::new();
        op.set_kind(op::Kind::REM);
        op.rem = MessageField::some(rem);
        let mut info = ChangeInfo::new();
        info.set_user(user.clone());
        info.set_timestamp(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0));
        let mut delta = Delta::new();
        delta.ops.push(op);
        delta.info = MessageField::some(info);
        let mut changes = ListChanges::new();
        changes.set_base_revision(rev);
        changes.deltas.push(delta);
        let body = changes.write_to_bytes().map_err(|e| e.to_string())?;
        let out = self.spclient_pb(http::Method::POST, &format!("/playlist/v2/playlist/{id}/changes"), Some(&body))?;
        log::info!("[playlist] {id}: {} pistas quitadas por protocolo interno ({} bytes)", uris.len(), out.len());
        Ok(())
    }

    /// Nivel de colaboración: enlace de invitación con el token del servicio de permisos.
    fn invite_link(&self, playlist: &str) -> Result<String, String> {
        use librespot_protocol::playlist_permission::{Permission, PermissionGrant, PermissionGrantOptions, PermissionLevel};
        use protobuf::{Message, MessageField};
        let mut perm = Permission::new();
        perm.set_permission_level(PermissionLevel::CONTRIBUTOR);
        let mut opts = PermissionGrantOptions::new();
        opts.permission = MessageField::some(perm);
        let body = opts.write_to_bytes().map_err(|e| e.to_string())?;
        // Ruta observada en el reproductor web: POST …/permission-grant (con guion).
        let out = self.spclient_pb(http::Method::POST, &format!("/playlist-permission/v1/playlist/{playlist}/permission-grant"), Some(&body))?;
        let grant = PermissionGrant::parse_from_bytes(&out).map_err(|e| format!("invitación: {e}"))?;
        let token = grant.token().to_string();
        if token.is_empty() {
            return Err("Spotify no devolvió un enlace de invitación".into());
        }
        Ok(format!("https://open.spotify.com/playlist/{playlist}?pi={token}"))
    }

    /// Petición a un endpoint interno de Spotify a través de librespot (spclient).
    fn spclient(&self, method: http::Method, endpoint: &str) -> Result<String, String> {
        let session = self.session()?;
        let bytes = self
            .handle
            .block_on(session.spclient().request(&method, endpoint, None, None))
            .map_err(|e| format!("{e}"))?;
        String::from_utf8(bytes.to_vec()).map_err(|e| e.to_string())
    }

    fn exec(&self, req: &Req, ui: &UiTx) -> Result<Resp, String> {
        match req {
            Req::Me => Ok(Resp::Me(self.get_json(&format!("{BASE}/me"))?)),
            Req::Playlists => Ok(Resp::Playlists(
                self.all_pages::<Playlist>(&format!("{BASE}/me/playlists?limit=50"), 40)?,
            )),
            Req::PlaylistMeta(id) => {
                let url = format!("{BASE}/playlists/{id}?fields=id,name,uri,description,images,owner,tracks.total,public,collaborative,followers");
                match self.get_json::<Playlist>(&url) {
                    Ok(p) => Ok(Resp::PlaylistMeta(p)),
                    Err(e) => {
                        // Las playlists generadas por Spotify (radios, mixes) no están en la Web API
                        // para apps externas: nombre, portada y tamaño por los metadatos internos.
                        log::info!("/playlists/{id} no disponible ({e}); usando metadatos de librespot");
                        Ok(Resp::PlaylistMeta(self.playlist_meta_via_librespot(id)?))
                    }
                }
            }
            Req::PlaylistTracks(id) => {
                // Spotify no permite /playlists/{id}/tracks a las apps en modo desarrollo:
                // los uris salen del protocolo interno (librespot) y los detalles por lotes.
                let items = self.playlist_items(id)?;
                let added: std::collections::HashMap<String, (Option<String>, Option<String>)> =
                    items.iter().map(|(i, by, at)| (i.clone(), (by.clone(), at.clone()))).collect();
                let ids: Vec<String> = items.into_iter().map(|(i, _, _)| i).collect();
                let total = ids.len() as u32;
                if ids.is_empty() {
                    return Ok(Resp::Tracks {
                        key: id.clone(),
                        tracks: Vec::new(),
                        total,
                        done: true,
                    });
                }
                let chunks: Vec<&[String]> = ids.chunks(100).collect();
                let last = chunks.len() - 1;
                for (i, chunk) in chunks.iter().enumerate() {
                    let mut tracks = self.tracks_by_ids(chunk)?;
                    for t in &mut tracks {
                        if let Some((by, at)) = t.id.as_ref().and_then(|tid| added.get(tid)) {
                            t.added_by = by.clone();
                            t.added_at = at.clone();
                        }
                    }
                    if i == last {
                        return Ok(Resp::Tracks {
                            key: id.clone(),
                            tracks,
                            total,
                            done: true,
                        });
                    }
                    ui.send(Msg::Api(ApiResult {
                        req: req.clone(),
                        result: Ok(Resp::Tracks {
                            key: id.clone(),
                            tracks,
                            total,
                            done: false,
                        }),
                    }));
                }
                unreachable!()
            }
            Req::Liked => self.stream_tracks::<SavedTrack>(
                req,
                "liked",
                &format!("{BASE}/me/tracks?limit=50"),
                |i| i.track,
                ui,
            ),
            Req::LikedRecent => {
                let mut tracks = Vec::new();
                let mut url = format!("{BASE}/me/tracks?limit=50");
                let mut total = 0;
                for _ in 0..2 {
                    let page: Paging<SavedTrack> = self.get_json(&url)?;
                    total = page.total;
                    tracks.extend(page.items.into_iter().filter_map(|i| i.track).map(slim_track));
                    match page.next {
                        Some(n) => url = n,
                        None => break,
                    }
                }
                Ok(Resp::Tracks {
                    key: "liked_recent".to_string(),
                    tracks,
                    total,
                    done: true,
                })
            }
            Req::SavedAlbums => Ok(Resp::SavedAlbums(
                self.all_pages::<SavedAlbum>(&format!("{BASE}/me/albums?limit=50"), 20)?
                    .into_iter()
                    .map(|s| s.album)
                    .collect(),
            )),
            Req::FollowedArtists => {
                let mut out = Vec::new();
                let mut after: Option<String> = None;
                for _ in 0..20 {
                    let mut url = format!("{BASE}/me/following?type=artist&limit=50");
                    if let Some(a) = &after {
                        url.push_str("&after=");
                        url.push_str(a);
                    }
                    let page: FollowedArtists = self.get_json(&url)?;
                    let n = page.artists.items.len();
                    out.extend(page.artists.items);
                    after = page.artists.cursors.and_then(|c| c.after);
                    if after.is_none() || n == 0 {
                        break;
                    }
                }
                Ok(Resp::FollowedArtists(out))
            }
            Req::Album(id) => {
                let mut album: Album = match self.get_json(&format!("{BASE}/albums/{id}")) {
                    Ok(a) => a,
                    Err(e) => {
                        log::info!("/albums/{id} no disponible ({e}); usando metadatos de librespot");
                        return Ok(Resp::Album(self.album_via_librespot(id)?));
                    }
                };
                if album.genres.is_empty() {
                    let by = album.artists.first().map(|a| a.name.clone()).unwrap_or_default();
                    album.genres = self.external_genres(&format!("album:{id}"), &by, Some(&album.name));
                }
                if let Some(p) = album.tracks.as_mut() {
                    let mut next = p.next.take();
                    let mut n = 0;
                    while let Some(url) = next {
                        if n >= 10 {
                            break;
                        }
                        let page: Paging<Track> = self.get_json(&url)?;
                        p.items.extend(page.items);
                        next = page.next;
                        n += 1;
                    }
                }
                Ok(Resp::Album(album))
            }
            Req::Artist(id) => match self.get_json::<Artist>(&format!("{BASE}/artists/{id}")) {
                Ok(mut a) => {
                    if a.genres.is_empty() {
                        a.genres = self.external_genres(&format!("artist:{id}"), &a.name, None);
                    }
                    Ok(Resp::Artist(a))
                }
                Err(e) => {
                    log::info!("/artists/{id} no disponible ({e}); usando metadatos de librespot");
                    Ok(Resp::Artist(self.artist_via_librespot(id)?))
                }
            },
            Req::ArtistPlaylists { id, name } => {
                let r: SearchResult = self.get_json(&format!("{BASE}/search?q={}&type=playlist&limit=50", urlencode(name)))?;
                let want = name.trim().to_lowercase();
                let playlists: Vec<Playlist> = r
                    .playlists
                    .map(|pg| pg.items.into_iter().flatten().collect::<Vec<Playlist>>())
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|pl| pl.owner_name().trim().to_lowercase() == want)
                    .collect();
                Ok(Resp::ArtistPlaylists { id: id.clone(), playlists })
            }
            Req::RadioPlaylist(id) => {
                let text = self.spclient(
                    http::Method::GET,
                    &format!("/inspiredby-mix/v2/seed_to_playlist/spotify:track:{id}?response-format=json"),
                )?;
                let v: Value = serde_json::from_str(&text).map_err(|e| format!("radio: {e}"))?;
                let uri = v["mediaItems"][0]["uri"].as_str().unwrap_or("");
                match uri.strip_prefix("spotify:playlist:") {
                    Some(pid) => Ok(Resp::RadioPlaylist { playlist_id: pid.to_string() }),
                    None => Err("Spotify no devolvió una radio para esta canción".into()),
                }
            }
            Req::TrackInfo(id) => {
                let mut v = self.tracks_by_ids(std::slice::from_ref(id))?;
                v.pop().map(Resp::TrackInfo).ok_or_else(|| "pista no encontrada".to_string())
            }
            Req::ArtistTop(id) => {
                // /artists/{id}/top-tracks está restringido: usamos los metadatos internos.
                let session = self.session()?;
                let uri = SpotifyUri::from_uri(&format!("spotify:artist:{id}"))
                    .map_err(|e| e.to_string())?;
                let artist = self
                    .handle
                    .block_on(librespot_metadata::Artist::get(&session, &uri))
                    .map_err(|e| format!("artista: {e}"))?;
                let country = session.country();
                let top = artist
                    .top_tracks
                    .iter()
                    .find(|t| t.country == country)
                    .or_else(|| artist.top_tracks.first());
                let ids: Vec<String> = top
                    .map(|t| t.tracks.iter().filter_map(|u| u.to_id().ok()).take(10).collect())
                    .unwrap_or_default();
                Ok(Resp::ArtistTop(self.tracks_by_ids(&ids)?))
            }
            Req::ArtistAlbums(id) => {
                use std::sync::atomic::Ordering;
                if !self.artist_albums_blocked.load(Ordering::Relaxed) {
                    match self.all_pages::<AlbumRef>(
                        &format!("{BASE}/artists/{id}/albums?include_groups=album,single"),
                        6,
                    ) {
                        Ok(a) => return Ok(Resp::ArtistAlbums(a)),
                        Err(e) => {
                            log::info!("/artists/{id}/albums no disponible ({e}); usando librespot");
                            self.artist_albums_blocked.store(true, Ordering::Relaxed);
                        }
                    }
                }
                Ok(Resp::ArtistAlbums(self.artist_albums_via_librespot(id)?))
            }
            Req::Search(q) => {
                let full = format!(
                    "{BASE}/search?q={}&type=track,album,artist,playlist,show,episode,audiobook&limit=10&market=from_token",
                    urlencode(q)
                );
                match self.get_json::<SearchResult>(&full) {
                    Ok(r) => Ok(Resp::Search(r)),
                    Err(_) => Ok(Resp::Search(self.get_json(&format!(
                        "{BASE}/search?q={}&type=track,album,artist,playlist,show,episode&limit=10&market=from_token",
                        urlencode(q)
                    ))?)),
                }
            }
            Req::LastPlayback => {
                let page: Paging<PlayHistory> = self.get_json(&format!("{BASE}/me/player/recently-played?limit=1"))?;
                let last = page.items.into_iter().next().and_then(|h| {
                    let track = h.track?;
                    let slim = slim_track(track.clone());
                    Some(ServerLast {
                        played_at: h.played_at.as_deref().map(rfc3339_to_unix).unwrap_or(0),
                        context_uri: h.context.map(|c| c.uri).filter(|u| !u.is_empty()),
                        track_uri: track.uri,
                        track: Some(slim),
                    })
                });
                Ok(Resp::LastPlayback(last))
            }
            Req::Recent => Ok(Resp::Recent(
                self.get_json::<Paging<PlayHistory>>(&format!(
                    "{BASE}/me/player/recently-played?limit=30"
                ))?
                .items
                .into_iter()
                .filter_map(|h| h.track)
                .map(slim_track)
                .collect(),
            )),
            Req::Devices => Ok(Resp::Devices(
                self.get_json::<Devices>(&format!("{BASE}/me/player/devices"))?
                    .devices,
            )),
            Req::PlayerState => Ok(Resp::PlayerState(self.get_json_opt(&format!(
                "{BASE}/me/player?additional_types=track,episode"
            ))?)),
            Req::Queue => {
                let q: Option<QueueResponse> = self.get_json_opt(&format!("{BASE}/me/player/queue"))?;
                Ok(Resp::Queue(q.unwrap_or_default()))
            }
            Req::JamQueue { current, next } => {
                // Resuelve a pistas con metadatos la cola compartida de la Jam (uris de canción).
                let to_id = |u: &str| u.rsplit(':').next().unwrap_or(u).to_string();
                let mut ids: Vec<String> = Vec::new();
                if !current.is_empty() {
                    ids.push(to_id(current));
                }
                ids.extend(next.iter().filter(|u| u.contains(":track:")).map(|u| to_id(u)));
                let tracks = self.tracks_by_ids(&ids)?;
                let mut it = tracks.into_iter();
                let currently_playing = if current.is_empty() { None } else { it.next() };
                Ok(Resp::JamQueue(QueueResponse {
                    currently_playing,
                    queue: it.collect(),
                }))
            }
            Req::AddToQueue(uri) => {
                self.send_json(
                    "POST",
                    &format!("{BASE}/me/player/queue?uri={}", urlencode(uri)),
                    None,
                )?;
                Ok(Resp::Done)
            }
            Req::Transfer { device_id } => {
                self.send_json(
                    "PUT",
                    &format!("{BASE}/me/player"),
                    Some(json!({ "device_ids": [device_id], "play": true })),
                )?;
                Ok(Resp::Done)
            }
            Req::RemotePlay {
                device_id,
                context_uri,
                uris,
                offset_uri,
                offset_index,
            } => {
                let mut url = format!("{BASE}/me/player/play");
                if let Some(d) = device_id {
                    url.push_str("?device_id=");
                    url.push_str(d);
                }
                let mut body = serde_json::Map::new();
                if let Some(c) = context_uri {
                    body.insert("context_uri".into(), json!(c));
                }
                if let Some(u) = uris {
                    body.insert("uris".into(), json!(u));
                }
                if let Some(o) = offset_uri {
                    body.insert("offset".into(), json!({ "uri": o }));
                } else if let Some(i) = offset_index {
                    body.insert("offset".into(), json!({ "position": i }));
                }
                let body = (!body.is_empty()).then_some(Value::Object(body));
                self.send_json("PUT", &url, body)?;
                Ok(Resp::Done)
            }
            Req::RemotePause => {
                self.send_json("PUT", &format!("{BASE}/me/player/pause"), None)?;
                Ok(Resp::Done)
            }
            Req::RemoteResume => {
                self.send_json("PUT", &format!("{BASE}/me/player/play"), None)?;
                Ok(Resp::Done)
            }
            Req::RemoteNext => {
                self.send_json("POST", &format!("{BASE}/me/player/next"), None)?;
                Ok(Resp::Done)
            }
            Req::RemotePrev => {
                self.send_json("POST", &format!("{BASE}/me/player/previous"), None)?;
                Ok(Resp::Done)
            }
            Req::RemoteSeek(ms) => {
                self.send_json("PUT", &format!("{BASE}/me/player/seek?position_ms={ms}"), None)?;
                Ok(Resp::Done)
            }
            Req::RemoteVolume(p) => {
                self.send_json(
                    "PUT",
                    &format!("{BASE}/me/player/volume?volume_percent={p}"),
                    None,
                )?;
                Ok(Resp::Done)
            }
            Req::RemoteShuffle(on) => {
                self.send_json("PUT", &format!("{BASE}/me/player/shuffle?state={on}"), None)?;
                Ok(Resp::Done)
            }
            Req::RemoteRepeat(state) => {
                self.send_json("PUT", &format!("{BASE}/me/player/repeat?state={state}"), None)?;
                Ok(Resp::Done)
            }
            Req::Save(ids) => {
                let uris: Vec<String> = ids.iter().map(|i| format!("spotify:track:{i}")).collect();
                self.library_write(true, &uris).map_err(like_error)?;
                Ok(Resp::Saved { ids: ids.clone(), saved: true })
            }
            Req::Unsave(ids) => {
                let uris: Vec<String> = ids.iter().map(|i| format!("spotify:track:{i}")).collect();
                self.library_write(false, &uris).map_err(like_error)?;
                Ok(Resp::Saved {
                    ids: ids.clone(),
                    saved: false,
                })
            }
            Req::SaveAlbum(id) => {
                self.library_write(true, &[format!("spotify:album:{id}")])?;
                Ok(Resp::AlbumSaved { id: id.clone(), saved: true })
            }
            Req::UnsaveAlbum(id) => {
                self.library_write(false, &[format!("spotify:album:{id}")])?;
                Ok(Resp::AlbumSaved { id: id.clone(), saved: false })
            }
            Req::HomeFeed => {
                let text = self.spclient(http::Method::GET, "/homeview/v1/home?platform=web&locale=es")?;
                let v: Value = serde_json::from_str(&text).map_err(|e| format!("inicio: {e}"))?;
                Ok(Resp::HomeFeed(parse_home(&v)))
            }
            Req::FolderCreate { name, playlists } => {
                use librespot_protocol::playlist4_external::{op, Add, Item, Mov, Op};
                use protobuf::MessageField;
                let (rev, mut list) = self.rootlist_raw()?;
                let fid = {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    std::time::SystemTime::now().hash(&mut h);
                    name.hash(&mut h);
                    format!("{:016x}", h.finish())
                };
                let start = format!("spotify:start-group:{fid}:{}", urlencode_path(name));
                let end = format!("spotify:end-group:{fid}");
                let mut ops = Vec::new();
                // La carpeta nueva va arriba del todo: start-group y end-group en 0 y 1.
                let mut add = Add::new();
                add.set_from_index(0);
                for u in [&start, &end] {
                    let mut it = Item::new();
                    it.set_uri(u.clone());
                    add.items.push(it);
                }
                let mut op = Op::new();
                op.set_kind(op::Kind::ADD);
                op.add = MessageField::some(add);
                ops.push(op);
                list.insert(0, start);
                list.insert(1, end.clone());
                // Las playlists elegidas se mueven dentro (justo antes del end-group).
                for pid in playlists {
                    let uri = format!("spotify:playlist:{pid}");
                    let Some(from) = list.iter().position(|u| u == &uri) else { continue };
                    let to = list.iter().position(|u| u == &end).unwrap_or(1);
                    if from == to - 1 {
                        continue;
                    }
                    let mut mv = Mov::new();
                    mv.set_from_index(from as i32);
                    mv.set_length(1);
                    mv.set_to_index(to as i32);
                    let mut op = Op::new();
                    op.set_kind(op::Kind::MOV);
                    op.mov = MessageField::some(mv);
                    ops.push(op);
                    let u = list.remove(from);
                    let to = if from < to { to - 1 } else { to };
                    list.insert(to, u);
                }
                self.rootlist_changes(rev, ops)?;
                Ok(Resp::RootlistChanged)
            }
            Req::FolderDelete(fid) => {
                use librespot_protocol::playlist4_external::{op, Op, Rem};
                use protobuf::MessageField;
                let (rev, mut list) = self.rootlist_raw()?;
                let start_p = format!("spotify:start-group:{fid}:");
                let end = format!("spotify:end-group:{fid}");
                let mut ops = Vec::new();
                // Se quitan las dos marcas; las playlists de dentro siguen en la biblioteca.
                for pred in [Box::new(|u: &String| u.starts_with(&start_p)) as Box<dyn Fn(&String) -> bool>, Box::new(|u: &String| u == &end)] {
                    let Some(idx) = list.iter().position(|u| pred(u)) else { continue };
                    let mut rem = Rem::new();
                    rem.set_from_index(idx as i32);
                    rem.set_length(1);
                    let mut op = Op::new();
                    op.set_kind(op::Kind::REM);
                    op.rem = MessageField::some(rem);
                    ops.push(op);
                    list.remove(idx);
                }
                if ops.is_empty() {
                    return Err("carpeta no encontrada".into());
                }
                self.rootlist_changes(rev, ops)?;
                Ok(Resp::RootlistChanged)
            }
            Req::FolderRename { id, name } => {
                use librespot_protocol::playlist4_external::{op, Add, Item, Op, Rem};
                use protobuf::MessageField;
                let (rev, list) = self.rootlist_raw()?;
                let start_p = format!("spotify:start-group:{id}:");
                let Some(idx) = list.iter().position(|u| u.starts_with(&start_p)) else {
                    return Err("carpeta no encontrada".into());
                };
                // UPDATE_ITEM_URIS no está admitido en el rootlist: se quita la marca de inicio y se
                // vuelve a añadir con el nombre nuevo en el mismo sitio (mismo id → misma carpeta).
                let mut rem = Rem::new();
                rem.set_from_index(idx as i32);
                rem.set_length(1);
                let mut op1 = Op::new();
                op1.set_kind(op::Kind::REM);
                op1.rem = MessageField::some(rem);
                let mut it = Item::new();
                it.set_uri(format!("spotify:start-group:{id}:{}", urlencode_path(name)));
                let mut add = Add::new();
                add.set_from_index(idx as i32);
                add.items.push(it);
                let mut op2 = Op::new();
                op2.set_kind(op::Kind::ADD);
                op2.add = MessageField::some(add);
                self.rootlist_changes(rev, vec![op1, op2])?;
                Ok(Resp::RootlistChanged)
            }
            Req::FolderMove { playlist, folder } => {
                use librespot_protocol::playlist4_external::{op, Mov, Op};
                use protobuf::MessageField;
                let (rev, list) = self.rootlist_raw()?;
                let uri = format!("spotify:playlist:{playlist}");
                let Some(from) = list.iter().position(|u| u == &uri) else {
                    return Err("la playlist no está en tu biblioteca".into());
                };
                let to = match folder {
                    Some(fid) => {
                        let end = format!("spotify:end-group:{fid}");
                        list.iter().position(|u| u == &end).ok_or("carpeta no encontrada")?
                    }
                    None => 0,
                };
                if from == to || (to > 0 && from == to - 1) {
                    return Ok(Resp::RootlistChanged);
                }
                let mut mv = Mov::new();
                mv.set_from_index(from as i32);
                mv.set_length(1);
                mv.set_to_index(to as i32);
                let mut op = Op::new();
                op.set_kind(op::Kind::MOV);
                op.mov = MessageField::some(mv);
                self.rootlist_changes(rev, vec![op])?;
                Ok(Resp::RootlistChanged)
            }
            Req::InviteLink(pid) => Ok(Resp::InviteLink { playlist: pid.clone(), link: self.invite_link(pid)? }),
            Req::SetBase { playlist, contributor } => {
                use librespot_protocol::playlist_permission::{PermissionLevel, SetPermissionLevelRequest};
                use protobuf::Message;
                let mut r = SetPermissionLevelRequest::new();
                r.set_permission_level(if *contributor { PermissionLevel::CONTRIBUTOR } else { PermissionLevel::VIEWER });
                let body = r.write_to_bytes().map_err(|e| e.to_string())?;
                let out = self.spclient_pb(http::Method::PUT, &format!("/playlist-permission/v1/playlist/{playlist}/permission/base"), Some(&body))?;
                log::info!("[base] {playlist} contributor={contributor}: {} bytes", out.len());
                Ok(Resp::Done)
            }
            Req::Members(pid) => {
                use librespot_protocol::playlist_permission::{GetMemberPermissionsResponse, PermissionLevel};
                use protobuf::Message;
                let out = self.spclient_pb(http::Method::GET, &format!("/playlist-permission/v1/playlist/{pid}/permission/members"), None)?;
                let resp = GetMemberPermissionsResponse::parse_from_bytes(&out).map_err(|e| format!("miembros: {e}"))?;
                let mut members: Vec<(String, bool)> = resp
                    .member_permissions
                    .iter()
                    .map(|(u, p)| (u.clone(), p.permission_level() == PermissionLevel::CONTRIBUTOR))
                    .collect();
                members.sort();
                log::info!("[members] {pid}: {members:?}");
                Ok(Resp::Members { playlist: pid.clone(), members })
            }
            Req::SetMember { playlist, user, contributor } => {
                use librespot_protocol::playlist_permission::{PermissionLevel, SetPermissionLevelRequest};
                use protobuf::Message;
                let endpoint = format!("/playlist-permission/v1/playlist/{playlist}/permission/member/{user}");
                if *contributor {
                    let mut r = SetPermissionLevelRequest::new();
                    r.set_permission_level(PermissionLevel::CONTRIBUTOR);
                    let body = r.write_to_bytes().map_err(|e| e.to_string())?;
                    self.spclient_pb(http::Method::PUT, &endpoint, Some(&body))?;
                } else {
                    self.spclient_pb(http::Method::DELETE, &endpoint, None)?;
                }
                Ok(Resp::Done)
            }
            Req::Probe(endpoint) => {
                let text = self.spclient(http::Method::GET, endpoint)?;
                let name = endpoint.trim_start_matches('/').replace(['/', '?', '&', '='], "_");
                let _ = std::fs::write(std::env::temp_dir().join(format!("nanofy_probe_{name}.json")), &text);
                log::info!("[probe] {endpoint}: {} bytes: {}", text.len(), text.chars().take(300).collect::<String>());
                Ok(Resp::Done)
            }
            Req::SavedShows => {
                let items: Vec<Value> = self.all_pages(&format!("{BASE}/me/shows?limit=50"), 10)?;
                Ok(Resp::SavedShows(
                    items.into_iter().filter_map(|v| serde_json::from_value::<Show>(v["show"].clone()).ok()).filter(|s| !s.id.is_empty()).collect(),
                ))
            }
            Req::SavedEpisodes => {
                let items: Vec<Value> = self.all_pages(&format!("{BASE}/me/episodes?limit=50"), 10)?;
                let list = items
                    .into_iter()
                    .filter_map(|v| serde_json::from_value::<EpisodeWithShow>(v["episode"].clone()).ok())
                    .filter(|e| !e.episode.id.is_empty())
                    .map(|e| SavedEpisode {
                        show_name: e.show.as_ref().map(|s| s.name.clone()).unwrap_or_default(),
                        show_id: e.show.as_ref().map(|s| s.id.clone()).unwrap_or_default(),
                        episode: e.episode,
                    })
                    .collect();
                Ok(Resp::SavedEpisodes(list))
            }
            Req::SavedAudiobooks => {
                let items: Vec<Option<Audiobook>> = self.all_pages(&format!("{BASE}/me/audiobooks?limit=50"), 10)?;
                Ok(Resp::SavedAudiobooks(items.into_iter().flatten().filter(|a| !a.id.is_empty()).collect()))
            }
            Req::Rootlist => {
                use protobuf::Message;
                let session = self.session()?;
                let bytes = self
                    .handle
                    .block_on(session.spclient().get_rootlist(0, None))
                    .map_err(|e| format!("carpetas: {e}"))?;
                let msg = librespot_protocol::playlist4_external::SelectedListContent::parse_from_bytes(&bytes)
                    .map_err(|e| format!("carpetas: {e}"))?;
                // Las carpetas son pares start-group/end-group alrededor de sus playlists.
                let mut folders: Vec<Folder> = Vec::new();
                let mut stack: Vec<Folder> = Vec::new();
                for item in &msg.contents.items {
                    let uri = item.uri();
                    if let Some(rest) = uri.strip_prefix("spotify:start-group:") {
                        let (id, name) = rest.split_once(':').unwrap_or((rest, ""));
                        let name = urldecode(name);
                        stack.push(Folder { id: id.to_string(), name, playlists: Vec::new() });
                    } else if uri.starts_with("spotify:end-group:") {
                        if let Some(f) = stack.pop() {
                            // Las subcarpetas cuentan como carpetas propias; la madre solo lista playlists.
                            folders.push(f);
                        }
                    } else if let Some(pid) = uri.strip_prefix("spotify:playlist:") {
                        if let Some(f) = stack.last_mut() {
                            f.playlists.push(pid.to_string());
                        }
                    }
                }
                folders.extend(stack);
                Ok(Resp::Folders(folders))
            }
            Req::FollowShow(id, on) => {
                self.library_write(*on, &[format!("spotify:show:{id}")])?;
                Ok(Resp::ShowFollowed { id: id.clone(), on: *on })
            }
            Req::SaveEpisode(id, on) => {
                self.library_write(*on, &[format!("spotify:episode:{id}")])?;
                Ok(Resp::EpisodeSaved { id: id.clone(), on: *on })
            }
            Req::Download { ids, episodes, quality } => {
                let session = self.session()?;
                let cache_ok = session
                    .cache()
                    .map(|c| c.file_path(librespot_core::FileId::from_raw(&[0u8; 20])).is_some())
                    .unwrap_or(false);
                if !cache_ok {
                    return Err("la caché de audio está desactivada (Ajustes → Caché de audio)".into());
                }
                // Hilo propio: una descarga tarda segundos y no debe bloquear las peticiones de la interfaz.
                let handle = self.handle.clone();
                let ui = ui.clone();
                let ids = ids.clone();
                let q = *quality;
                let ep = *episodes;
                std::thread::Builder::new()
                    .name("nanofy-download".into())
                    .stack_size(1024 * 1024)
                    .spawn(move || {
                        for id in ids {
                            let result = download_track(&session, &handle, &id, ep, q).map(|_| Resp::Downloaded(id.clone()));
                            ui.send(Msg::Api(ApiResult { req: Req::Download { ids: vec![id], episodes: ep, quality: q }, result }));
                        }
                    })
                    .map_err(|e| e.to_string())?;
                Ok(Resp::Done)
            }
            Req::CreatePlaylist {
                user_id,
                name,
                description,
                public,
                collaborative,
            } => {
                let text = self.send_json(
                    "POST",
                    &format!("{BASE}/users/{user_id}/playlists"),
                    Some(json!({
                        "name": name,
                        "description": description,
                        "public": public,
                        "collaborative": *collaborative && !public,
                    })),
                )?;
                let p: Playlist =
                    serde_json::from_str(&text).map_err(|e| format!("respuesta inesperada: {e}"))?;
                Ok(Resp::PlaylistCreated(p))
            }
            Req::UpdatePlaylist {
                id,
                name,
                description,
                public,
                collaborative,
            } => {
                self.send_json(
                    "PUT",
                    &format!("{BASE}/playlists/{id}"),
                    Some(json!({
                        "name": name,
                        "description": description,
                        "public": public,
                        "collaborative": *collaborative && !public,
                    })),
                )?;
                Ok(Resp::PlaylistChanged(id.clone()))
            }
            Req::SetPlaylistImage { id, path } => {
                let jpeg = encode_cover(path)?;
                let b64 = base64::engine::general_purpose::STANDARD.encode(&jpeg);
                let (status, text) = self.call(
                    "PUT",
                    &format!("{BASE}/playlists/{id}/images"),
                    Some(("image/jpeg", b64.as_bytes())),
                )?;
                if !(200..300).contains(&status) {
                    return Err(http_error(status, &text));
                }
                Ok(Resp::PlaylistChanged(id.clone()))
            }
            Req::AddToPlaylist { id, uris } => {
                let web = (|| -> Result<(), String> {
                    for chunk in uris.chunks(100) {
                        self.send_json("POST", &format!("{BASE}/playlists/{id}/tracks"), Some(json!({ "uris": chunk })))?;
                    }
                    Ok(())
                })();
                if let Err(e) = web {
                    log::info!("AddToPlaylist por Web API falló ({e}); probando protocolo interno");
                    for chunk in uris.chunks(100) {
                        self.playlist_add_internal(id, chunk)?;
                    }
                }
                Ok(Resp::PlaylistChanged(id.clone()))
            }
            Req::RemoveFromPlaylist { id, uris } => {
                let web = (|| -> Result<(), String> {
                    for chunk in uris.chunks(100) {
                        let tracks: Vec<Value> = chunk.iter().map(|u| json!({ "uri": u })).collect();
                        self.send_json("DELETE", &format!("{BASE}/playlists/{id}/tracks"), Some(json!({ "tracks": tracks })))?;
                    }
                    Ok(())
                })();
                if let Err(e) = web {
                    log::info!("RemoveFromPlaylist por Web API falló ({e}); probando protocolo interno");
                    for chunk in uris.chunks(100) {
                        self.playlist_remove_internal(id, chunk)?;
                    }
                }
                Ok(Resp::PlaylistChanged(id.clone()))
            }
            Req::FollowPlaylist(id) => {
                self.send_json("PUT", &format!("{BASE}/playlists/{id}/followers"), None)?;
                Ok(Resp::PlaylistChanged(id.clone()))
            }
            Req::UnfollowPlaylist(id) => {
                self.send_json("DELETE", &format!("{BASE}/playlists/{id}/followers"), None)?;
                Ok(Resp::PlaylistChanged(id.clone()))
            }
            Req::User(id) => {
                // /users/{id} está restringido: perfil por el endpoint interno de librespot,
                // que además trae las playlists públicas y si ya lo sigues.
                let session = self.session()?;
                let bytes = self
                    .handle
                    .block_on(session.spclient().get_user_profile(id, Some(50), Some(0)))
                    .map_err(|e| format!("perfil: {e}"))?;
                let v: Value = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("perfil: respuesta inesperada: {e}"))?;
                let img = |v: &Value| -> Option<Vec<Image>> {
                    v.as_str().filter(|s| !s.is_empty()).map(|u| {
                        vec![Image {
                            url: u.to_string(),
                            width: Some(300),
                            height: Some(300),
                        }]
                    })
                };
                let profile = UserProfile {
                    id: id.clone(),
                    display_name: v["name"].as_str().map(|s| s.to_string()),
                    images: img(&v["image_url"]).unwrap_or_default(),
                    followers: v["followers_count"].as_u64().map(|n| Followers { total: Some(n) }),
                };
                let playlists: Vec<Playlist> = v["public_playlists"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|p| {
                                let uri = p["uri"].as_str()?;
                                let pid = uri.rsplit(':').next()?.to_string();
                                Some(Playlist {
                                    id: pid,
                                    name: p["name"].as_str().unwrap_or("").to_string(),
                                    uri: uri.to_string(),
                                    description: None,
                                    images: img(&p["image_url"]),
                                    owner: Owner {
                                        display_name: p["owner_name"].as_str().map(|s| s.to_string()),
                                        id: p["owner_uri"]
                                            .as_str()
                                            .and_then(|u| u.rsplit(':').next())
                                            .map(|s| s.to_string()),
                                    },
                                    tracks: None,
                                    public: Some(true),
                                    collaborative: None,
                                    followers: None,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                ui.send(Msg::Api(ApiResult {
                    req: Req::UserPlaylists(id.clone()),
                    result: Ok(Resp::UserPlaylists {
                        user_id: id.clone(),
                        playlists,
                    }),
                }));
                if let Some(f) = v["is_following"].as_bool() {
                    ui.send(Msg::Api(ApiResult {
                        req: Req::FollowContains {
                            kind: "user",
                            ids: vec![id.clone()],
                        },
                        result: Ok(Resp::FollowContains {
                            kind: "user",
                            ids: vec![id.clone()],
                            following: vec![f],
                        }),
                    }));
                }
                Ok(Resp::User(profile))
            }
            Req::UserPlaylists(id) => Ok(Resp::UserPlaylists {
                user_id: id.clone(),
                playlists: self.all_pages::<Playlist>(
                    &format!("{BASE}/users/{}/playlists?limit=50", urlencode(id)),
                    10,
                )?,
            }),
            Req::FollowContains { kind, ids } => {
                let following: Vec<bool> = self.get_json(&format!(
                    "{BASE}/me/following/contains?type={kind}&ids={}",
                    ids.iter().map(|s| urlencode(s)).collect::<Vec<_>>().join(",")
                ))?;
                Ok(Resp::FollowContains {
                    kind,
                    ids: ids.clone(),
                    following,
                })
            }
            Req::Follow { kind, id } => {
                self.library_write(true, &[format!("spotify:{kind}:{id}")])?;
                Ok(Resp::Followed {
                    kind,
                    id: id.clone(),
                    following: true,
                })
            }
            Req::Unfollow { kind, id } => {
                self.library_write(false, &[format!("spotify:{kind}:{id}")])?;
                Ok(Resp::Followed {
                    kind,
                    id: id.clone(),
                    following: false,
                })
            }
            Req::Lyrics {
                id,
                name,
                artist,
                album,
                duration_ms,
            } => {
                // 1) Endpoint interno de Spotify (solo funciona para algunos tokens/cuentas).
                if let Ok(session) = self.session() {
                    let mut headers = http::HeaderMap::new();
                    headers.insert("app-platform", "WebPlayer".parse().unwrap());
                    headers.insert("accept", "application/json".parse().unwrap());
                    let endpoint = format!(
                        "/color-lyrics/v2/track/{id}?format=json&vocalRemoval=false&market=from_token"
                    );
                    let result = self.handle.block_on(session.spclient().request(
                        &http::Method::GET,
                        &endpoint,
                        Some(headers),
                        None,
                    ));
                    if let Ok(bytes) = result {
                        if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
                            if let Some(l) = Lyrics::from_json(id, &v) {
                                return Ok(Resp::Lyrics(Some(l)));
                            }
                        }
                    }
                }
                // 2) LRCLIB: base de datos abierta de letras sincronizadas.
                Ok(Resp::Lyrics(self.lrclib(id, name, artist, album, *duration_ms)))
            }
            Req::JamCurrent => {
                let session = self.session()?;
                let endpoint = format!(
                    "/social-connect/v2/sessions/current_or_new?local_device_id={}&type=REMOTE",
                    session.device_id()
                );
                let text = self.spclient(http::Method::GET, &endpoint)?;
                let v: Value = serde_json::from_str(&text).map_err(|e| format!("Jam: {e}"))?;
                Ok(Resp::Jam(JamSession::from_json(&v)))
            }
            Req::JamJoin(token) => {
                let session = self.session()?;
                let endpoint = format!(
                    "/social-connect/v2/sessions/join/{}?playback_control=listen_and_control&join_type=frictionless_join&local_device_id={}",
                    urlencode(token),
                    session.device_id()
                );
                let text = self.spclient(http::Method::POST, &endpoint)?;
                let v: Value = serde_json::from_str(&text).map_err(|e| format!("Jam: {e}"))?;
                Ok(Resp::Jam(JamSession::from_json(&v)))
            }
            Req::JamLeave(session_id) => {
                // Salir de una Jam (participante). La ruta v3 `/leave` con DELETE devuelve 405, así
                // que se prueban los endpoints conocidos en orden hasta que uno responda 2xx. Salir
                // es idempotente y benigno, por lo que probar varios es seguro. Ninguno de estos
                // borra la sesión del anfitrión (eso es JamEnd, ruta distinta sin sufijo).
                let me = self.session()?.username();
                let candidates = [
                    (http::Method::DELETE, format!("/social-connect/v3/sessions/{session_id}/members/{me}")),
                    (http::Method::DELETE, format!("/social-connect/v2/sessions/{session_id}/members/{me}")),
                    (http::Method::POST, format!("/social-connect/v3/sessions/{session_id}/leave")),
                    (http::Method::POST, format!("/social-connect/v2/sessions/{session_id}/leave")),
                ];
                let mut last = String::new();
                for (method, ep) in candidates {
                    match self.spclient(method.clone(), &ep) {
                        Ok(_) => {
                            log::info!("[jam] salida correcta con {method} {ep}");
                            return Ok(Resp::Jam(None));
                        }
                        Err(e) => {
                            log::info!("[jam] salida fallida con {method} {ep}: {e}");
                            last = e;
                        }
                    }
                }
                Err(format!("Jam: no se pudo salir ({last})"))
            }
            Req::JamEnd(session_id) => {
                let _ = self.spclient(
                    http::Method::DELETE,
                    &format!("/social-connect/v3/sessions/{session_id}"),
                )?;
                Ok(Resp::Jam(None))
            }
            Req::ArtistView(id) => Ok(Resp::ArtistView {
                id: id.clone(),
                view: self.artist_view_via_librespot(id)?,
            }),
            Req::Show(id) => {
                // Web API si está disponible; si no, metadatos internos.
                let web: Result<(Show, Vec<Episode>), String> = (|| {
                    let show: Show = self.get_json(&format!("{BASE}/shows/{id}?market=from_token"))?;
                    let eps = self.all_pages::<Option<Episode>>(&format!("{BASE}/shows/{id}/episodes?limit=50&market=from_token"), 4)?;
                    Ok((show, eps.into_iter().flatten().collect()))
                })();
                match web {
                    Ok((mut show, episodes)) => {
                        // Los temas del podcast solo vienen por los metadatos internos.
                        if let Ok(session) = self.session() {
                            if let Ok(uri) = SpotifyUri::from_uri(&format!("spotify:show:{id}")) {
                                if let Ok(s) = self.handle.block_on(librespot_metadata::Show::get(&session, &uri)) {
                                    show.keywords = s.keywords.clone();
                                }
                            }
                        }
                        Ok(Resp::Show { show, episodes })
                    }
                    Err(e) => {
                        log::info!("/shows/{id} no disponible ({e}); usando metadatos de librespot");
                        let (show, episodes) = self.show_via_librespot(id)?;
                        Ok(Resp::Show { show, episodes })
                    }
                }
            }
            Req::WebConnect(client_id) => {
                self.web.connect(client_id)?;
                Ok(Resp::WebConnected)
            }
            Req::WebDisconnect => {
                self.web.disconnect();
                Ok(Resp::WebDisconnected)
            }
            Req::WebConnectPersonal(client_id) => {
                self.web_personal.connect(client_id)?;
                Ok(Resp::WebConnected)
            }
            Req::WebDisconnectPersonal => {
                self.web_personal.disconnect();
                Ok(Resp::WebDisconnected)
            }
        }
    }
}

pub const NO_APP_HINT: &str = "Spotify limita la Web API del cliente compartido (429). Configura tu Client ID en Ajustes.";

fn image_from_meta(im: &librespot_metadata::image::Image) -> Image {
    Image {
        url: format!("https://i.scdn.co/image/{}", im.id),
        width: Some(im.width.max(0) as u32),
        height: Some(im.height.max(0) as u32),
    }
}

fn track_from_meta(t: librespot_metadata::Track) -> Track {
    Track {
        id: t.id.to_id().ok(),
        uri: t.id.to_uri().unwrap_or_default(),
        name: t.name,
        duration_ms: t.duration.max(0) as u32,
        explicit: t.is_explicit,
        artists: t
            .artists
            .iter()
            .map(|a| ArtistRef {
                id: a.id.to_id().ok(),
                name: a.name.clone(),
                uri: a.id.to_uri().ok(),
            })
            .collect(),
        album: Some(AlbumRef {
            id: t.album.id.to_id().ok(),
            name: t.album.name.clone(),
            uri: t.album.id.to_uri().ok(),
            images: t.album.covers.iter().map(image_from_meta).collect(),
            artists: Vec::new(),
            release_date: None,
            total_tracks: None,
            album_type: None,
        }),
        is_local: false,
        track_number: Some(t.number.max(0) as u32),
        is_playable: None,
        kind: Some("track".to_string()),
        added_by: None,
        added_at: None,
    }
}

/// Deja solo las imágenes útiles (≤ 300 px) para no retener URLs que nunca se usan.
/// Convierte una marca RFC3339 («2026-09-06T19:30:50.089Z») a segundos Unix (UTC).
fn rfc3339_to_unix(s: &str) -> u64 {
    let b = s.as_bytes();
    let num = |a: usize, b2: usize| s.get(a..b2).and_then(|x| x.parse::<i64>().ok());
    let (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(se)) =
        (num(0, 4), num(5, 7), num(8, 10), num(11, 13), num(14, 16), num(17, 19))
    else {
        return 0;
    };
    let _ = b;
    // Días desde 1970 (algoritmo de Howard Hinnant, «days_from_civil»).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    (days * 86400 + h * 3600 + mi * 60 + se).max(0) as u64
}

fn slim_track(mut t: Track) -> Track {
    if let Some(album) = t.album.as_mut() {
        if album.images.len() > 2 {
            album.images.retain(|i| i.width.unwrap_or(0) <= 300);
        }
    }
    t
}

/// Lee una imagen, la reduce y la codifica como JPEG de menos de 256 KB (límite de Spotify).
fn encode_cover(path: &PathBuf) -> Result<Vec<u8>, String> {
    let img = image::open(path).map_err(|e| format!("no se pudo abrir la imagen: {e}"))?;
    let mut side = 640u32;
    let mut quality = 88u8;
    for _ in 0..6 {
        let small = img.thumbnail(side, side).to_rgb8();
        let mut out = Vec::new();
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
        enc.encode_image(&small)
            .map_err(|e| format!("no se pudo codificar la imagen: {e}"))?;
        if out.len() < 190 * 1024 {
            return Ok(out);
        }
        side = (side * 3 / 4).max(160);
        quality = quality.saturating_sub(10).max(50);
    }
    Err("la imagen es demasiado grande incluso reducida".to_string())
}

fn cooldown_message(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let when = if h > 0 {
        format!("{h} h {m} min")
    } else {
        format!("{} min", m.max(1))
    };
    format!("Spotify ha agotado la cuota de tu app; la Web API vuelve en {when}. La reproducción sigue funcionando.")
}

fn http_error(status: u16, text: &str) -> String {
    let msg = serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| text.chars().take(120).collect());
    match status {
        401 => "Spotify rechazó la sesión (401). Cierra sesión y vuelve a entrar.".to_string(),
        403 => format!("Spotify no lo permite (403): {msg}"),
        404 => format!("No encontrado (404): {msg}"),
        _ => format!("HTTP {status}: {msg}"),
    }
}

/// Codificación de nombre de carpeta en el rootlist (espacio → %20, como el cliente oficial).
fn urlencode_path(s: &str) -> String {
    urlencode(s).replace('+', "%20")
}

pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Descarga el audio de una pista a la caché de librespot (el reproductor la usará sin red).
fn download_track(
    session: &librespot_core::Session,
    handle: &tokio::runtime::Handle,
    id: &str,
    episode: bool,
    quality: crate::config::Quality,
) -> Result<(), String> {
    use crate::config::Quality;
    use librespot_metadata::audio::AudioFileFormat as F;
    let files: librespot_metadata::audio::AudioFiles = if episode {
        let uri = SpotifyUri::from_uri(&format!("spotify:episode:{id}")).map_err(|e| e.to_string())?;
        let ep = handle
            .block_on(librespot_metadata::Episode::get(session, &uri))
            .map_err(|e| format!("episodio: {e}"))?;
        ep.audio.clone()
    } else {
        let uri = SpotifyUri::from_uri(&format!("spotify:track:{id}")).map_err(|e| e.to_string())?;
        let mut track = handle
            .block_on(librespot_metadata::Track::get(session, &uri))
            .map_err(|e| format!("pista: {e}"))?;
        if track.files.0.is_empty() {
            // Sin archivo en esta región: se prueba la primera alternativa.
            let alt = track.alternatives.first().cloned().ok_or("sin archivo de audio disponible")?;
            track = handle
                .block_on(librespot_metadata::Track::get(session, &alt))
                .map_err(|e| format!("pista: {e}"))?;
        }
        track.files.clone()
    };
    let track = TrackFiles { files };
    let order: &[F] = match quality {
        Quality::Lossless => &[F::FLAC_FLAC_24BIT, F::FLAC_FLAC, F::OGG_VORBIS_320, F::OGG_VORBIS_160, F::OGG_VORBIS_96],
        Quality::High => &[F::OGG_VORBIS_320, F::OGG_VORBIS_160, F::OGG_VORBIS_96],
        Quality::Normal => &[F::OGG_VORBIS_160, F::OGG_VORBIS_320, F::OGG_VORBIS_96],
        Quality::Low => &[F::OGG_VORBIS_96, F::OGG_VORBIS_160, F::OGG_VORBIS_320],
    };
    let (fmt, file_id) = order
        .iter()
        .find_map(|f| track.files.0.get(f).map(|fid| (*f, *fid)))
        .ok_or("sin formato de audio compatible")?;
    let bps = if matches!(fmt, F::FLAC_FLAC | F::FLAC_FLAC_24BIT) { 200_000 } else { 40_000 };
    let mut file = handle
        .block_on(librespot_audio::AudioFile::open(session, file_id, bps))
        .map_err(|e| format!("audio: {e}"))?;
    if matches!(file, librespot_audio::AudioFile::Cached(_)) {
        return Ok(());
    }
    // Leer hasta el final fuerza la descarga completa; librespot la guarda en la caché al terminar.
    std::io::copy(&mut file, &mut std::io::sink()).map_err(|e| format!("descarga: {e}"))?;
    Ok(())
}

/// Archivos de audio de una pista o episodio (misma selección de formato para ambos).
struct TrackFiles {
    files: librespot_metadata::audio::AudioFiles,
}

/// Convierte la respuesta de homeview (lista plana de cabeceras y tarjetas) en estanterías.
fn parse_home(v: &Value) -> Vec<HomeSection> {
    let mut out: Vec<HomeSection> = Vec::new();
    let Some(body) = v["body"].as_array() else {
        return out;
    };
    for el in body {
        let comp = el["component"]["id"].as_str().unwrap_or("");
        let id = el["id"].as_str().unwrap_or("").to_string();
        match comp {
            "glue:sectionHeader" => {
                let title = el["text"]["title"].as_str().unwrap_or("").trim().to_string();
                out.push(HomeSection { id: id.trim_end_matches("-header").to_string(), title, items: Vec::new() });
            }
            "glue2:card" => {
                let section_id = el["metadata"]["sectionId"].as_str().unwrap_or("").to_string();
                let uri = el["target"]["uri"]
                    .as_str()
                    .or_else(|| el["metadata"]["uri"].as_str())
                    .unwrap_or("")
                    .to_string();
                if uri.is_empty() {
                    continue;
                }
                let item = HomeItem {
                    uri,
                    title: el["text"]["title"].as_str().unwrap_or("").to_string(),
                    subtitle: el["text"]["subtitle"].as_str().unwrap_or("").to_string(),
                    image: el["images"]["main"]["uri"].as_str().map(|s| s.to_string()),
                    context: None,
                };
                match out.last_mut().filter(|s| s.id == section_id || section_id.is_empty()) {
                    Some(sec) => sec.items.push(item),
                    None => {
                        // Tarjetas sin cabecera (escuchado recientemente).
                        if let Some(sec) = out.iter_mut().find(|s| s.id == section_id) {
                            sec.items.push(item);
                        } else {
                            out.push(HomeSection { id: section_id, title: "Escuchado recientemente".into(), items: vec![item] });
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out.retain(|s| !s.items.is_empty() && !matches!(s.title.to_lowercase().as_str(), "atajos" | "shortcuts"));
    out
}

/// Decodifica "%20" y "+" de los nombres de carpeta del rootlist.
fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() + 0 && i + 2 <= bytes.len() - 1 => {
                if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Mensaje claro cuando la Web API rechaza escribir en la biblioteca (cuenta no autorizada en
/// la app de desarrollo). El corazón y guardar necesitan que la cuenta esté en «User Management».
fn like_error(e: String) -> String {
    if e.contains("403") || e.contains("Forbidden") || e.contains("404") {
        "Spotify no permite modificar «Me gusta» desde apps de terceros en modo desarrollo.          Alternativa: añade la canción a una playlist (eso sí funciona)."
            .to_string()
    } else {
        e
    }
}
