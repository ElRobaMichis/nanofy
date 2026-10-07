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
use crate::pathfinder::{PfErr, Pathfinder};
use crate::webauth::{WebAuth, WebChain};
use librespot_core::meta_cache::SEED_ROWS;
use librespot_core::SpotifyUri;
use librespot_metadata::Metadata;

const BASE: &str = "https://api.spotify.com/v1";
const WORKERS: usize = 2;
/// Hilos del carril interno (`Api::send`): lo que se ve al abrir una página y sale del protocolo
/// interno de Spotify, no de la Web API.
const INTERNAL_WORKERS: usize = 3;
/// Elementos que se piden uno a uno como mucho por llamada (los que un lote de metadatos no
/// trajo, o un lote pequeño que falló), y cuántos a la vez. Sin tope, un lote fallido de
/// cientos agotaba el cupo de librespot (300 cada 30 s) y la canción que suena no cargaba.
const SINGLE_GET_MAX: usize = 20;
const SINGLE_GET_PARALLEL: usize = 4;
/// Sueltos como mucho en toda la carga de una playlist (lo que sus lotes no trajeron), sumando
/// todos los lotes: con 3 en vuelo y 20 por lote, una lista de miles podía pedir cientos.
const SINGLE_GET_LOAD_MAX: usize = 3 * SINGLE_GET_MAX;
/// Entidades por petición de extended-metadata.
const BATCH_MAX: usize = 500;
/// Primer lote de una playlist: pequeño, para que las primeras filas no esperen a uno de 500.
const PLAYLIST_FIRST_BATCH: usize = 100;
/// Canciones de una búsqueda cuyos metadatos se adelantan como mucho (`Req::WarmMeta`): las que
/// se ven sin bajar.
pub const WARM_META_MAX: usize = 10;
/// Lotes de una playlist en vuelo a la vez. No más: el otro hilo y la reproducción comparten
/// el cupo de librespot (300 peticiones cada 30 s).
const PLAYLIST_BATCH_PARALLEL: usize = 3;
/// Limitador de ritmo de la Web API (token bucket). Permite una rafaga inicial (el arranque
/// pide varias cosas a la vez) y luego un ritmo sostenido bajo el umbral de Spotify.
const RATE_BURST: f64 = 6.0;
/// Fichas por segundo que se reponen (ritmo sostenido). Conservador para no disparar el límite
/// por usuario del id compartido de primera parte.
const RATE_REFILL: f64 = 2.0;
/// Fichas que el carril de fondo deja siempre en el limitador: lo que ella pide (una búsqueda,
/// una página) encuentra al menos dos al momento aunque haya una recarga larga en curso.
const BG_RESERVE: f64 = 2.0;
/// Artistas por petición de miniaturas (nombre e imagen por extended-metadata).
const THUMBS_BATCH: usize = 100;
/// Un álbum o artista sin géneros en ninguna fuente no se vuelve a buscar hasta pasados estos
/// segundos (30 días): cada búsqueda son varias consultas externas en serie.
const GENRES_MISS_TTL: u64 = 30 * 86_400;
/// Una playlist se recarga reutilizando los metadatos de pistas de su copia en disco mientras
/// estos tengan menos de esto (7 días); pasado, se piden todos otra vez (nombres, portadas y
/// pistas retiradas cambian, aunque poco).
const LIST_META_TTL: u64 = 7 * 86_400;
/// Búsquedas de géneros en cola como mucho. Al pasar de ahí se descartan las más viejas (de
/// páginas que ya no se ven); se encargan otra vez la próxima vez que se pida ese álbum o ese
/// artista (tras reconectar o en otra sesión).
const ENRICH_GENRES_MAX: usize = 20;
/// Plazos de las llamadas de librespot (metadatos, spclient, login5). Sin ellos, una conexión
/// que se quedó colgada (tras suspender el equipo o cambiar de wifi) dejaba un hilo de la API
/// esperando para siempre, y detrás las búsquedas y páginas que no cargaban hasta reiniciar.
/// Al vencer, quien pidió conserva lo que tenía y su reintento hace el resto.
/// Una entidad suelta (artista, álbum, podcast, perfil, una pista) o un token.
const TIMEOUT_ITEM: Duration = Duration::from_secs(10);
/// Una lista entera (playlist4, rootlist): con miles de pistas tarda más.
const TIMEOUT_LIST: Duration = Duration::from_secs(20);
/// Lecturas de spclient, incluido cada lote de extended-metadata (hasta 500 entidades).
const TIMEOUT_SP_READ: Duration = Duration::from_secs(15);
/// Escrituras de spclient (cambios de playlist, permisos, Jam): con más margen, porque cortar
/// una que quizá ya se aplicó deja la duda, y al repetirla se podría añadir dos veces.
const TIMEOUT_SP_WRITE: Duration = Duration::from_secs(30);
/// Metadatos de una descarga (va en su propio hilo; abrir el audio no tiene plazo).
const TIMEOUT_DOWNLOAD_META: Duration = Duration::from_secs(60);
/// Segundos que una escritura del carril del reproductor (mandos remotos, cola, transferir)
/// espera como mucho ante un 429. Detrás van la pausa y el siguiente que ella pulse: mejor
/// fallar y que el sondeo del estado del reproductor lo reconcilie que dormir el hilo hasta
/// 35 s. No se aplazan a otro carril: una pausa aplazada podría llegar después de un reanudar.
const PLAYER_WRITE_WAIT: u64 = 3;

thread_local! {
    /// `true` en el hilo del carril de fondo (`Api::send_bg`): cede el limitador a lo de primer
    /// plano y nunca duerme un Retry-After.
    static BG: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// `true` en el hilo del carril del reproductor: sus escrituras esperan poco ante un 429.
    static PLAYER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn on_bg_lane() -> bool {
    BG.with(|b| b.get())
}

fn on_player_lane() -> bool {
    PLAYER.with(|p| p.get())
}

/// Desde cuándo carga aquí una canción (la interfaz está en «cargando»), en ms de
/// `loading_clock_ms`; 0 si no carga ninguna. Lo pone la interfaz con `set_playback_loading`.
static PLAYBACK_LOADING_SINCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Una carga frena los lotes de metadatos en segundo plano como mucho durante esto desde que
/// empezó. Más allá, o la carga está atascada por otra cosa (y la biblioteca no debe esperarla)
/// o el aviso es viejo (la interfaz no se pintó para quitarlo, p. ej. minimizada).
const PLAYBACK_YIELD_MAX_MS: u64 = 4_000;
/// Cada cuánto mira un lote en espera si la canción ya suena.
const PLAYBACK_YIELD_POLL: Duration = Duration::from_millis(40);

/// La interfaz avisa de que una canción carga aquí (`true`) o ya no (`false`). Se llama en cada
/// pasada de la interfaz: solo el paso de no cargar a cargar fija el momento de inicio.
pub fn set_playback_loading(loading: bool) {
    use std::sync::atomic::Ordering::Relaxed;
    if loading {
        let _ = PLAYBACK_LOADING_SINCE.compare_exchange(0, loading_clock_ms(), Relaxed, Relaxed);
    } else {
        PLAYBACK_LOADING_SINCE.store(0, Relaxed);
    }
}

/// Reloj de `PLAYBACK_LOADING_SINCE`: ms desde la primera consulta, más 1 para que nunca valga
/// 0 (que es «no carga»).
fn loading_clock_ms() -> u64 {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

/// ¿Debe esperar un lote en segundo plano? Solo si hay una carga y empezó hace menos de
/// PLAYBACK_YIELD_MAX_MS.
fn playback_gate(since_ms: u64, now_ms: u64) -> bool {
    since_ms != 0 && now_ms.saturating_sub(since_ms) < PLAYBACK_YIELD_MAX_MS
}

fn playback_loading() -> bool {
    playback_gate(PLAYBACK_LOADING_SINCE.load(std::sync::atomic::Ordering::Relaxed), loading_clock_ms())
}

/// Antes de pedir el siguiente lote de metadatos de una carga larga (una playlist de miles, un
/// álbum o artista grande): si una canción está cargando aquí, se espera a que suene. Sus
/// peticiones (metadatos, almacenamiento) van al mismo spclient y al mismo cupo de librespot
/// que estos lotes, y una respuesta de 500 entidades por delante retrasaba el primer sonido.
/// Los lotes ya en vuelo siguen; el primero de cada carga (las filas que se ven) no espera.
async fn yield_to_playback() {
    if !playback_loading() {
        return;
    }
    let t0 = Instant::now();
    while playback_loading() {
        tokio::time::sleep(PLAYBACK_YIELD_POLL).await;
    }
    log::debug!("metadatos por lotes: {} ms en espera de la canción que carga", t0.elapsed().as_millis());
}

/// `yield_to_playback` para los bucles que no van dentro de un `block_on` (hilos de la API).
fn yield_to_playback_blocking() {
    if !playback_loading() {
        return;
    }
    let t0 = Instant::now();
    while playback_loading() {
        std::thread::sleep(PLAYBACK_YIELD_POLL);
    }
    log::debug!("metadatos por lotes: {} ms en espera de la canción que carga", t0.elapsed().as_millis());
}

/// Trabajo del carril de enriquecimiento (letras y géneros de fuentes externas). No es un canal
/// porque hay que poder sustituir y descartar lo que espera: las letras de una canción ya
/// saltada o los géneros de páginas que ya no se ven.
#[derive(Default)]
struct EnrichQueue {
    /// Letras de la canción que suena. Van antes que cualquier género y solo cuenta la última:
    /// una petición nueva sustituye a la que aún espera.
    lyrics: Option<Req>,
    /// Géneros por buscar: (clave, artista, álbum). Se sirven de la más nueva a la más vieja,
    /// para que la página abierta los tenga antes que las que ya se dejaron atrás.
    genres: std::collections::VecDeque<(String, String, Option<String>)>,
    /// Claves en cola o en curso: abrir dos veces la misma página no las busca dos veces.
    queued: std::collections::HashSet<String>,
    /// La app se cierra: el hilo termina (como los otros carriles al soltar su canal).
    closed: bool,
}

type Enrich = Arc<(Mutex<EnrichQueue>, std::sync::Condvar)>;

/// Lo que la recarga de una playlist usa de su copia en disco (la escribe la interfaz:
/// `CachedList` en app/mod.rs; lo demás se ignora). Sin fecha de metadatos ni país (copias de
/// antes de guardarlos) no se reutiliza nada.
#[derive(serde::Deserialize)]
struct ListCopy {
    #[serde(default)]
    tracks: Vec<Track>,
    #[serde(default)]
    meta_at: u64,
    #[serde(default)]
    country: Option<String>,
}

/// ¿Va por el carril interno? Lo que abre una página y sale del protocolo interno de Spotify (sin
/// la Web API, o el álbum, que la prueba sin esperar). Nada que escriba; solo la lista de playlists
/// cae a la Web API, y únicamente si el rootlist no sirve.
fn interno(req: &Req) -> bool {
    matches!(
        req,
        Req::PlaylistTracks(_)
            | Req::Playlists
            | Req::Album(_)
            | Req::RadioPlaylist(_)
            | Req::HomeFeed
            | Req::ArtistView(_)
            | Req::User(_)
            | Req::TrackInfo(_)
            | Req::ArtistThumbs(_)
            | Req::JamQueue { .. }
    )
}

#[derive(Clone, Debug)]
pub enum Req {
    Me,
    /// Playlists de la biblioteca, del rootlist interno (spclient): nombres, orden, tamaño y
    /// portada subida sin gastar la cuota compartida de la Web API. Solo si el rootlist no
    /// sirve, de /me/playlists como antes.
    Playlists,
    /// Lo que el rootlist no trae del listado (mosaicos de portada, nombre visible del
    /// propietario, privacidad), de /me/playlists. Por el carril de fondo: ante un 429 falla al
    /// momento y la biblioteca sigue con lo que ya tenía.
    PlaylistsWeb,
    PlaylistMeta(String),
    PlaylistTracks(String),
    Liked,
    /// Las 100 canciones guardadas más recientes (para refrescar la instantánea).
    LikedRecent,
    SavedAlbums,
    FollowedArtists,
    Album(String),
    Artist(String),
    /// Nombre e imagen de varios artistas de una vez (tarjetas de información), por un lote de
    /// metadatos internos: sin cuota de la Web API ni búsqueda de géneros.
    ArtistThumbs(Vec<String>),
    /// Precarga inteligente: los metadatos de estas canciones (las primeras de una búsqueda) en un
    /// solo lote, sembrados en la caché del reproductor para que la que se pulse no los pida. Sin
    /// respuesta para la interfaz (`Resp::Done`) ni aviso si falla.
    WarmMeta(Vec<String>),
    /// Solo etiqueta los géneros que manda el carril de enriquecimiento (Resp::Genres); enviada
    /// a la API devuelve los ya conocidos, sin red.
    Genres { key: String },
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
    /// Sonda de las mezclas de Spotify (diagnóstico del modo de control, `mix_probe`): pide una
    /// vez todo lo que podría traer las transiciones de una playlist mezclada y escribe el informe
    /// en %TEMP%\nanofy_mixprobe_<id>.txt (ver `crate::mixprobe`). Responde `Resp::Done`.
    MixProbe(String),
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
        /// `None`: privacidad desconocida (el rootlist no la trae y la Web API no la dio); no se
        /// envía, para no volver pública una privada al cambiarle solo el nombre.
        public: Option<bool>,
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
    /// Conecta la biblioteca (Web API) con la identidad de primera parte: abre el navegador y
    /// espera en un hilo de la API. La interfaz usa `Api::begin_web_chain`, que no ocupa ninguno y
    /// se puede cancelar; sus resultados llegan con esta misma petición.
    WebConnect(String),
    WebDisconnect,
    WebConnectPersonal(String),
    WebDisconnectPersonal,
}

pub enum Resp {
    Me(User),
    /// Listado de la biblioteca. `rootlist`: salió del rootlist (sin nombre visible del
    /// propietario ni privacidad, y sin portada en las que no tienen una subida: se completan
    /// con el listado anterior y, en segundo plano, con `Req::PlaylistsWeb`). Si no, de la Web
    /// API, completo.
    Playlists { list: Vec<Playlist>, rootlist: bool },
    /// /me/playlists para completar el listado del rootlist (ver `Req::PlaylistsWeb`).
    PlaylistsWeb(Vec<Playlist>),
    PlaylistMeta(Playlist),
    /// Metadatos de una playlist sacados de playlist4 (librespot): nombre, portada, descripción,
    /// tamaño y si es colaborativa. No traen el nombre visible del propietario, la privacidad ni
    /// los seguidores, así que se mezclan campo a campo con lo que ya se sabía.
    PlaylistMetaPartial(Playlist),
    /// Página de pistas de una lista (`key` = id de playlist o "liked").
    Tracks {
        key: String,
        tracks: Vec<Track>,
        total: u32,
        done: bool,
    },
    /// Llega antes del último lote de una carga de playlist: lo que su copia en disco debe
    /// apuntar para que la carga siguiente reutilice sus pistas (ver `ListCopy`). `meta_at`
    /// (segundos Unix) es de cuándo son los metadatos más antiguos que trae (los reutilizados
    /// conservan su fecha) y `country`, para qué país se pidieron.
    PlaylistCopyInfo { id: String, meta_at: u64, country: String },
    SavedAlbums(Vec<Album>),
    FollowedArtists(Vec<Artist>),
    Album(Album),
    Artist(Artist),
    /// Artistas con nombre e imagen (y los géneros ya conocidos), sin seguidores: solo completan
    /// lo que falte, nunca sustituyen a un artista entero.
    ArtistThumbs(Vec<Artist>),
    /// Géneros encontrados después de responder (`key` = "album:<id>" o "artist:<id>").
    Genres { key: String, genres: Vec<String> },
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
    search_tx: mpsc::Sender<Req>,
    bg_tx: mpsc::Sender<Req>,
    internal_tx: mpsc::Sender<Req>,
    enrich: Enrich,
    web: Arc<WebAuth>,
    web_personal: Arc<WebAuth>,
    /// Para las autorizaciones de la biblioteca, que esperan en su propio hilo (`begin_web_chain`).
    ui: UiTx,
}

impl Drop for Api {
    fn drop(&mut self) {
        let (lock, cv) = &*self.enrich;
        if let Ok(mut q) = lock.lock() {
            q.closed = true;
        }
        cv.notify_all();
    }
}

impl Api {
    pub fn start(
        shared: Arc<Shared>,
        handle: tokio::runtime::Handle,
        web: Arc<WebAuth>,
        lists_dir: PathBuf,
        ui: UiTx,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<Req>();
        let rx = Arc::new(Mutex::new(rx));
        // Carril prioritario: estado del reproductor, cola y mandos remotos nunca esperan
        // detrás de la biblioteca (inicio, playlists, canciones que te gustan…).
        let (prio_tx, prio_rx) = mpsc::channel::<Req>();
        // Carril de búsqueda: lo que ella escribe no espera detrás de la precarga de playlists,
        // la recarga de «Canciones que te gustan» ni los géneros que ocupan los dos hilos comunes.
        let (search_tx, search_rx) = mpsc::channel::<Req>();
        // Carril de fondo: recargas largas que nadie está mirando (Me gusta completa, artistas
        // seguidos). Con su propio hilo no ocupan los dos comunes durante decenas de segundos.
        let (bg_tx, bg_rx) = mpsc::channel::<Req>();
        // Carril interno: lo que se ve al abrir una página y sale del protocolo interno de Spotify
        // (playlist4, rootlist, metadatos por lotes, radio, inicio, artista, perfil), no de la Web
        // API; un álbum, de ella solo si responde sin esperar (`interno`). En los hilos comunes
        // esperaban detrás de lecturas de la Web API dormidas en un Retry-After (hasta 25 s
        // cada una): con la cuota compartida limitando, abrir una playlist, un álbum o una radio
        // en frío tardaba 5-20 s en una petición de 0,3 s.
        let (internal_tx, internal_rx) = mpsc::channel::<Req>();
        let internal_rx = Arc::new(Mutex::new(internal_rx));
        // Carril de enriquecimiento: letras y géneros de fuentes externas (LRCLIB, iTunes, Deezer,
        // MusicBrainz), segundos de consultas en serie que antes ocupaban los dos hilos comunes
        // después de dibujar la página. Las letras van primero.
        let enrich: Enrich = Arc::new((Mutex::new(EnrichQueue::default()), std::sync::Condvar::new()));
        // Proveedor opcional de la app propia del usuario: lecturas con su propia cuota (rápidas,
        // sin compartir límite). Las escrituras siguen yendo por la identidad de primera parte.
        let web_personal = Arc::new(WebAuth::load_personal(web.state_dir().join("webapi_personal.json")));

        let tls = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        // Fuentes externas (letras, géneros): plazo corto. Con el de la Web API (20 s) una sola
        // que no contesta dejaba el carril de enriquecimiento parado y las letras detrás.
        let ext_config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(5)))
            .timeout_connect(Some(Duration::from_secs(3)))
            .http_status_as_error(false)
            .tls_config(tls.clone())
            .build();
        // Conectar y recibir la cabecera de la respuesta con plazo propio, más corto que el
        // total: una conexión que se cuelga tras suspender el equipo o cambiar de wifi falla en
        // segundos, en vez de ocupar el hilo los 20 s enteros.
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .timeout_connect(Some(Duration::from_secs(4)))
            .timeout_recv_response(Some(Duration::from_secs(10)))
            .http_status_as_error(false)
            .tls_config(tls)
            .build();
        let cooldown_file = web.state_dir().join("api_cooldown");
        let genres_file = web.state_dir().join("genres.json");
        let genres: std::collections::HashMap<String, Vec<String>> = std::fs::read_to_string(&genres_file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        // Sin géneros en ninguna fuente, con la fecha en que se buscaron: pasados 30 días se
        // vuelven a buscar (una fecha futura, de un reloj adelantado, también caduca).
        let genres_miss_file = web.state_dir().join("genres_miss.json");
        let now = crate::cache::now_secs();
        let genres_miss: std::collections::HashMap<String, Option<u64>> = std::fs::read_to_string(&genres_miss_file)
            .ok()
            .and_then(|t| serde_json::from_str::<std::collections::HashMap<String, u64>>(&t).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, at)| *at <= now && now - *at < GENRES_MISS_TTL)
            .map(|(k, at)| (k, Some(at)))
            .collect();
        let cooldown_until = std::fs::read_to_string(&cooldown_file)
            .ok()
            .and_then(|t| t.trim().parse::<u64>().ok())
            .filter(|&until| until > crate::cache::now_secs());
        let agent = ureq::Agent::new_with_config(config);
        let pf = Pathfinder::new(agent.clone(), shared.clone(), handle.clone(), web.state_dir().join("pathfinder.json"));
        let client = Arc::new(Client {
            agent,
            ext_agent: ureq::Agent::new_with_config(ext_config),
            pf,
            shared,
            handle,
            web: web.clone(),
            web_personal: web_personal.clone(),
            cooldown_until: Mutex::new(cooldown_until),
            personal_cooldown_until: Mutex::new(None),
            cooldown_file,
            genres: Mutex::new(genres),
            genres_file,
            genres_miss: Mutex::new(genres_miss),
            genres_miss_file,
            lists_dir,
            enrich: enrich.clone(),
            artist_albums_blocked: std::sync::atomic::AtomicBool::new(false),
            rate: Mutex::new((RATE_BURST, Instant::now())),
            read_backoff: Mutex::new([None, None]),
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
                    // Lo lee call(): las escrituras de este hilo esperan poco ante un 429.
                    PLAYER.with(|p| p.set(true));
                    while let Ok(req) = prio_rx.recv() {
                        run(req, &client, &ui);
                    }
                })
                .expect("no se pudo crear el hilo de la API");
        }
        {
            let client = client.clone();
            let ui = ui.clone();
            std::thread::Builder::new()
                .name("nanofy-api-search".into())
                .stack_size(512 * 1024)
                .spawn(move || loop {
                    let Ok(mut req) = search_rx.recv() else { break };
                    // Gana la última: una búsqueda que sigue en cola cuando llega otra ya no le
                    // interesa a nadie (la app solo acepta la respuesta de la consulta enviada
                    // la última), así que se descarta sin gastar cuota. Cualquier otra petición
                    // que acabe en este carril se ejecuta igualmente, en orden.
                    loop {
                        match search_rx.try_recv() {
                            Ok(newer @ Req::Search(_)) if matches!(req, Req::Search(_)) => {
                                log::info!("api {:?} descartada: hay una búsqueda más nueva", req);
                                req = newer;
                            }
                            Ok(next) => {
                                run(req, &client, &ui);
                                req = next;
                            }
                            Err(_) => break,
                        }
                    }
                    run(req, &client, &ui);
                })
                .expect("no se pudo crear el hilo de la API");
        }
        for i in 0..INTERNAL_WORKERS {
            let rx = internal_rx.clone();
            let client = client.clone();
            let ui = ui.clone();
            std::thread::Builder::new()
                .name(format!("nanofy-api-internal-{i}"))
                .stack_size(512 * 1024)
                .spawn(move || loop {
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
                .name("nanofy-api-bg".into())
                .stack_size(512 * 1024)
                .spawn(move || {
                    // Lo lee rate_acquire y call(): este hilo cede las fichas y falla rápido.
                    BG.with(|b| b.set(true));
                    while let Ok(req) = bg_rx.recv() {
                        run(req, &client, &ui);
                    }
                })
                .expect("no se pudo crear el hilo de la API");
        }
        {
            let client = client.clone();
            let ui = ui.clone();
            let enrich = enrich.clone();
            enum Job {
                Lyrics(Req),
                Genres(String, String, Option<String>),
            }
            std::thread::Builder::new()
                .name("nanofy-enrich".into())
                .stack_size(512 * 1024)
                .spawn(move || loop {
                    let job = {
                        let (lock, cv) = &*enrich;
                        let mut q = lock.lock().unwrap();
                        loop {
                            if q.closed {
                                return;
                            }
                            // Las letras antes que nada: las espera la canción que suena, y
                            // detrás de varias búsquedas de géneros tardarían más que antes.
                            if let Some(req) = q.lyrics.take() {
                                break Job::Lyrics(req);
                            }
                            if let Some((key, artist, album)) = q.genres.pop_back() {
                                break Job::Genres(key, artist, album);
                            }
                            q = cv.wait(q).unwrap();
                        }
                    };
                    match job {
                        Job::Lyrics(req) => run(req, &client, &ui),
                        Job::Genres(key, artist, album) => {
                            let t0 = Instant::now();
                            let lyrics_waiting = || {
                                let req = enrich.0.lock().unwrap().lyrics.take();
                                if let Some(req) = req {
                                    run(req, &client, &ui);
                                }
                            };
                            let genres = client.external_genres(&key, &artist, album.as_deref(), &lyrics_waiting);
                            // Después de guardarlos: quien la encargue otra vez ya los encuentra.
                            enrich.0.lock().unwrap().queued.remove(&key);
                            log::debug!("[generos] {key} en {} ms", t0.elapsed().as_millis());
                            // Sin géneros no hay nada que completar en la página.
                            if !genres.is_empty() {
                                ui.send(Msg::Api(ApiResult {
                                    req: Req::Genres { key: key.clone() },
                                    result: Ok(Resp::Genres { key, genres }),
                                }));
                            }
                        }
                    }
                })
                .expect("no se pudo crear el hilo de la API");
        }
        Self { tx, prio_tx, search_tx, bg_tx, internal_tx, enrich, web, web_personal, ui }
    }

    /// Prepara la autorización de la biblioteca (identidad de primera parte) y la deja esperando
    /// la vuelta del navegador en su propio hilo, no en los carriles de la API: puede tardar
    /// minutos, y antes ocupaba uno de los dos hilos comunes hasta que volvía (para siempre si se
    /// cerraba la pestaña). `open`: abrirla ya en el navegador; sin él, la URL se encadena al
    /// inicio de sesión (`Cmd::Login`). El resultado llega como el de `Req::WebConnect`, salvo si
    /// la cancela la interfaz (`WebChain::cancel`), que ya lo sabe.
    pub fn begin_web_chain(&self, open: bool) -> Result<WebChain, String> {
        let pending = self.web.prepare_connect()?;
        let chain = pending.handle();
        let web = self.web.clone();
        let ui = self.ui.clone();
        std::thread::Builder::new()
            .name("nanofy-webauth".into())
            .spawn(move || {
                let result = web.finish_connect(pending);
                if result.as_ref().is_err_and(|e| e == crate::webauth::CONNECT_CANCELLED) {
                    return;
                }
                ui.send(Msg::Api(ApiResult {
                    req: Req::WebConnect(crate::webauth::WEB_CLIENT_ID.to_string()),
                    result: result.map(|()| Resp::WebConnected),
                }));
            })
            .map_err(|e| format!("no se pudo crear el hilo de la autorización: {e}"))?;
        if open {
            crate::webauth::open_in_browser(&chain.url);
        }
        Ok(chain)
    }

    /// Envía por el carril prioritario (no espera detrás de la biblioteca). Para la
    /// restauración: las pistas del contexto deben llegar ya.
    pub fn send_priority(&self, req: Req) {
        let _ = self.prio_tx.send(req);
    }

    /// Envía por el carril de fondo, para lo que ya se ve desde la instantánea y solo se
    /// refresca. Un único hilo que lee de la Web API solo cuando sobran fichas (deja
    /// `BG_RESERVE`) y ante un 429 o el enfriamiento falla al momento en vez de esperar: quien
    /// llama debe conservar lo que ya tiene si la respuesta es un error.
    pub fn send_bg(&self, req: Req) {
        let _ = self.bg_tx.send(req);
    }

    pub fn send(&self, req: Req) {
        // Solo la búsqueda va a su carril «gana la última». Req::User se queda en el común
        // aunque la búsqueda de perfiles lo envíe junto a ella: descartarlo dejaría «user:x»
        // pedido para siempre y el perfil o el avatar no llegarían nunca.
        if matches!(req, Req::Search(_)) {
            let _ = self.search_tx.send(req);
            return;
        }
        // Las letras, a su hueco del carril de enriquecimiento: si aún espera la de una canción
        // ya saltada, la sustituye (la app solo acepta la de la que suena).
        if matches!(req, Req::Lyrics { .. }) {
            let (lock, cv) = &*self.enrich;
            if let Some(old) = lock.lock().unwrap().lyrics.replace(req) {
                log::info!("api {:?} descartada: hay unas letras más nuevas", old);
            }
            cv.notify_one();
            return;
        }
        if interno(&req) {
            let _ = self.internal_tx.send(req);
            return;
        }
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
    /// Para las fuentes externas (letras, géneros), con plazo corto.
    ext_agent: ureq::Agent,
    /// Búsqueda de los clientes oficiales (pathfinder), antes que la de la Web API.
    pf: Pathfinder,
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
    /// Claves ya consultadas sin resultado (no se repiten). Con fecha si todas las fuentes
    /// contestaron: se guardan en disco y valen 30 días. Sin fecha si alguna falló (sin red,
    /// límite): solo esta sesión, porque con red quizá sí los habría.
    genres_miss: Mutex<std::collections::HashMap<String, Option<u64>>>,
    genres_miss_file: PathBuf,
    /// Copias en disco de las playlists (las escribe la interfaz): al recargar una, sus pistas
    /// ya conocidas no se vuelven a pedir.
    lists_dir: PathBuf,
    /// Cola del carril de enriquecimiento (la comparte con Api, que le pasa las letras).
    enrich: Enrich,
    /// /artists/{id}/albums rechazado por Spotify (403/400) en esta sesión: se usa librespot
    /// directamente.
    artist_albums_blocked: std::sync::atomic::AtomicBool,
    /// Limitador de ritmo propio (token bucket) para la Web API: mantiene el ritmo por debajo
    /// del umbral de Spotify de forma proactiva, evitando los 429 en vez de reaccionar a ellos.
    /// (tokens disponibles, momento del último relleno).
    rate: Mutex<(f64, Instant)>,
    /// Hasta cuándo un token de lectura está limitado tras un 429 corto (Retry-After), por
    /// token: [app propia, primera parte]. Lo comparten todos los hilos, para que cada lectura
    /// no descubra el mismo 429 gastando otra llamada y esperando por su cuenta. Solo en
    /// memoria: el enfriamiento largo por cuota agotada es `cooldown_until`.
    /// (cuándo llegó el 429, hasta cuándo): el primero evita que una respuesta de una petición
    /// enviada ANTES del 429 (otro hilo, en vuelo) libere el token nada más limitarse.
    read_backoff: Mutex<[Option<(Instant, Instant)>; 2]>,
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

    /// Espera una llamada de librespot desde un hilo de la API, con plazo `d`. Ver
    /// `block_timeout`.
    fn block<T, E: std::fmt::Display>(
        &self,
        d: Duration,
        f: impl std::future::Future<Output = Result<T, E>>,
    ) -> Result<T, String> {
        block_timeout(&self.handle, d, f)
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

    /// Segundos que le quedan al enfriamiento por cuota agotada de la Web API, si lo hay.
    fn web_cooldown_left(&self) -> Option<u64> {
        let until = (*self.cooldown_until.lock().unwrap())?;
        let now = crate::cache::now_secs();
        (until > now).then(|| until - now)
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
            .block(TIMEOUT_ITEM, session.login5().auth_token())
            .map_err(|e| format!("token: {e}"))?;
        Ok((token.access_token, false))
    }

    /// Espera lo justo (si hace falta) para no superar el ritmo permitido de la Web API.
    /// Token bucket: se reponen `RATE_REFILL` fichas por segundo hasta un máximo de `RATE_BURST`;
    /// cada petición consume una. Así una ráfaga corta pasa al instante y el ritmo sostenido
    /// queda por debajo del umbral de Spotify, evitando los 429 antes de que ocurran.
    /// El carril de fondo solo toma una ficha si detrás quedan `BG_RESERVE`: una recarga de
    /// decenas de páginas no se come el ritmo de la búsqueda ni de las páginas que ella abre.
    fn rate_acquire(&self) {
        let bg = on_bg_lane();
        loop {
            let (sleep, got) = {
                let mut g = self.rate.lock().unwrap();
                let (ref mut tokens, ref mut last) = *g;
                let now = Instant::now();
                *tokens = (*tokens + last.elapsed().as_secs_f64() * RATE_REFILL).min(RATE_BURST);
                *last = now;
                if bg {
                    if *tokens >= 1.0 + BG_RESERVE {
                        *tokens -= 1.0;
                        (Duration::ZERO, true)
                    } else {
                        // No aparta nada (a diferencia del primer plano): vuelve a mirar cuando se
                        // haya repuesto lo que falta, por si entretanto lo gastó otra petición.
                        (Duration::from_secs_f64((1.0 + BG_RESERVE - *tokens) / RATE_REFILL), false)
                    }
                } else if *tokens >= 1.0 {
                    *tokens -= 1.0;
                    (Duration::ZERO, true)
                } else {
                    let deficit = 1.0 - *tokens;
                    *tokens = 0.0;
                    (Duration::from_secs_f64(deficit / RATE_REFILL), true)
                }
            };
            if sleep > Duration::ZERO {
                std::thread::sleep(sleep);
            }
            if got {
                return;
            }
        }
    }

    /// Hueco del token en `read_backoff`: la app propia o la de primera parte. El de login5 no
    /// tiene (ante un 429 ya se avisa con NO_APP_HINT).
    fn backoff_slot(own_app: bool, personal: bool) -> Option<usize> {
        if personal {
            Some(0)
        } else if own_app {
            Some(1)
        } else {
            None
        }
    }

    /// Separa los tokens libres de los que siguen limitados tras un 429 de lectura; devuelve
    /// los libres y lo que falta para que quede libre el primero de los otros.
    fn open_tokens(&self, tokens: &[(String, bool, bool)]) -> (Vec<(String, bool, bool)>, Option<Duration>) {
        let backoff = *self.read_backoff.lock().unwrap();
        let now = Instant::now();
        let mut open = Vec::new();
        let mut min_left: Option<Duration> = None;
        for t in tokens {
            let left = Self::backoff_slot(t.1, t.2)
                .and_then(|s| backoff[s])
                .map(|(_, until)| until.saturating_duration_since(now))
                .filter(|d| !d.is_zero());
            match left {
                Some(d) => min_left = Some(min_left.map_or(d, |m| m.min(d))),
                None => open.push(t.clone()),
            }
        }
        (open, min_left)
    }

    /// Ejecuta una petición y devuelve (código HTTP, cuerpo). Reintenta una vez ante 429.
    /// `wait_ok`=false (la búsqueda): una lectura limitada no espera el Retry-After; falla al
    /// momento con «reintenta en N s» y la interfaz la repite sola, sin dormir un hilo.
    fn call(
        &self,
        method: &str,
        url: &str,
        body: Option<(&str, &[u8])>,
        wait_ok: bool,
    ) -> Result<(u16, String), String> {
        // El carril de fondo nunca duerme un Retry-After ni espera la limitación compartida:
        // falla ya, quien lo pidió conserva su copia y se reintenta en otro arranque. Así no
        // gasta más cuota mientras Spotify limita ni retiene la espera de las demás lecturas.
        let wait_ok = wait_ok && !on_bg_lane();
        // Pruebas (`NANOFY_FAKE_429=web`): la cuota compartida, agotada por otros clientes.
        if fake_429_web() {
            log::debug!("[fallo] 429 simulado: {method} {url}");
            return Err(throttle_message(FAKE_429_SECS));
        }
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
                    .block(TIMEOUT_ITEM, session.login5().auth_token())
                    .map_err(|e| format!("token: {e}"))?;
                v.push((token.access_token, false, false));
            }
            v
        };
        // Lecturas con el último token disponible: un 429 corto (la identidad de primera
        // parte «en frío» pide 10-20 s) se espera una vez en vez de fallar; si no, la
        // búsqueda o la biblioteca no cargarían mientras la app propia esté sin cuota.
        let mut read_waited = false;
        // Lecturas: los tokens que acaban de recibir un 429 se saltan hasta que pase su
        // Retry-After. Se filtra DESPUÉS de construir la lista: si se saltaran al construirla y
        // no quedara ninguno, se usaría el token de login5, que ante un 429 diría «conecta tu
        // biblioteca» (NO_APP_HINT) en vez de «reintenta en N s». Las escrituras tienen su propio
        // presupuesto.
        // Si se saltó alguno, el último de la lista filtrada no es el último de verdad: un
        // QUOTA_EXCEEDED de la app propia no debe acabar en el enfriamiento largo de toda la API.
        let mut skipped = false;
        let tokens = if write {
            tokens
        } else {
            let (open, min_left) = self.open_tokens(&tokens);
            skipped = min_left.is_some();
            if open.is_empty() {
                // Ninguno libre (la lista nunca está vacía, así que hay una espera). Quien puede
                // esperar (biblioteca, playlists) espera solo lo que queda, una vez; la búsqueda
                // falla ya con los segundos para reintentarse sola.
                let left = min_left.unwrap_or_default();
                let secs = ceil_secs(left);
                if !wait_ok || secs > 25 {
                    return Err(throttle_message(secs));
                }
                log::info!("Spotify limita las lecturas; se esperan los {secs} s que quedan del Retry-After");
                std::thread::sleep(left);
                read_waited = true;
                let (open, min_left) = self.open_tokens(&tokens);
                if open.is_empty() {
                    return Err(throttle_message(ceil_secs(min_left.unwrap_or_default())));
                }
                skipped = min_left.is_some();
                open
            } else {
                open
            }
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
            // revierta. La app propia casi nunca llega aquí (tiene su propia cuota). En el carril
            // del reproductor, solo PLAYER_WRITE_WAIT: detrás esperan los demás mandos.
            let player_lane = on_player_lane();
            let mut write_budget = if player_lane { PLAYER_WRITE_WAIT } else { 35u64 };
            let mut advance = false;
            let slot = Self::backoff_slot(own_app, personal);
            for _ in 0..8 {
                let sent_at = Instant::now();
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
                if status != 429 {
                    // Respondió sin limitar: el token vuelve a estar libre para las lecturas. Solo
                    // si esta petición salió después del 429; una que ya iba en vuelo no dice nada.
                    if let Some(s) = slot {
                        let mut b = self.read_backoff.lock().unwrap();
                        if b[s].is_some_and(|(at, _)| at <= sent_at) {
                            b[s] = None;
                        }
                    }
                }
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
                        // La app propia era la última solo porque la de primera parte está en su
                        // espera corta: ya está apartada (personal_cooled), así que se repite con
                        // la de primera parte, que espera o falla con «reintenta en N s». Antes de
                        // la espera compartida se probaba esa directamente; no hay que bloquear
                        // toda la Web API horas ni guardarlo en disco por eso.
                        if personal && skipped {
                            return self.call(method, url, body, wait_ok);
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
                        // Las demás lecturas saltan este token hasta que pase el Retry-After, en
                        // vez de gastar otra llamada cada una para descubrir el mismo 429.
                        // Un 429 que llega tarde con un Retry-After menor no acorta uno ya puesto.
                        if let Some(s) = slot {
                            let now = Instant::now();
                            let mut b = self.read_backoff.lock().unwrap();
                            let until = (now + Duration::from_secs(retry_after)).max(b[s].map_or(now, |(_, u)| u));
                            b[s] = Some((now, until));
                        }
                        if is_last && !read_waited && retry_after <= 25 && wait_ok {
                            read_waited = true;
                            log::info!("Spotify limita las lecturas (Retry-After {retry_after} s); se espera una vez");
                            std::thread::sleep(Duration::from_secs(retry_after));
                            continue;
                        }
                        if !is_last {
                            fallback = Some((status, text));
                            advance = true;
                            break;
                        }
                        // La búsqueda no duerme el hilo hasta 25 s mientras ella mira «Buscando»:
                        // falla ya con los segundos y la interfaz la reintenta sola.
                        if !wait_ok {
                            return Err(throttle_message(retry_after));
                        }
                        return Err("Spotify está limitando las peticiones; inténtalo de nuevo en unos segundos.".to_string());
                    }
                    // ESCRITURA: reintenta respetando el Retry-After real hasta agotar el presupuesto.
                    // En el carril del reproductor, solo si el Retry-After cabe en lo que queda:
                    // dormir 3 s para repetir una petición que sigue limitada gasta otra llamada
                    // con el 429 seguro y retrasa igual los mandos que esperan detrás.
                    if write_budget > 0 && !(player_lane && retry_after > write_budget) {
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
        self.get_json_wait(url, true)
    }

    /// Como `get_json`, pero ante un 429 no espera: falla al momento con «reintenta en N s»
    /// (lo lee `retry_secs`). Para la búsqueda, que la interfaz reintenta sola.
    fn get_json_nowait<T: DeserializeOwned>(&self, url: &str) -> Result<T, String> {
        self.get_json_wait(url, false)
    }

    fn get_json_wait<T: DeserializeOwned>(&self, url: &str, wait_ok: bool) -> Result<T, String> {
        let (status, text) = self.call("GET", url, None, wait_ok)?;
        if !(200..300).contains(&status) {
            return Err(http_error(status, &text));
        }
        serde_json::from_str(&text).map_err(|e| format!("respuesta inesperada: {e}"))
    }

    /// Como `get_json`, pero 204 (sin contenido) devuelve `None`.
    fn get_json_opt<T: DeserializeOwned>(&self, url: &str) -> Result<Option<T>, String> {
        let (status, text) = self.call("GET", url, None, true)?;
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

    /// Búsqueda por la Web API /search (la de respaldo de pathfinder). Ante un 429 falla al
    /// momento con «reintenta en N s» en vez de esperar.
    fn search_web(&self, q: &str) -> Result<SearchResult, String> {
        let full = format!(
            "{BASE}/search?q={}&type=track,album,artist,playlist,show,episode,audiobook&limit=10&market=from_token",
            urlencode(q)
        );
        match self.get_json_nowait::<SearchResult>(&full) {
            Ok(r) => Ok(r),
            // Sin audiolibros solo si Spotify rechaza el tipo (400/403, de http_error) o
            // si lo que devuelve no se puede leer (un audiolibro con campos raros). Un
            // 429, el enfriamiento por cuota o un fallo de red se devuelven tal cual:
            // repetir gastaría otra llamada de cuota y fallaría igual.
            Err(e)
                if e.starts_with("HTTP 400")
                    || e.contains("(403)")
                    || e.starts_with("respuesta inesperada") =>
            {
                self.get_json_nowait(&format!(
                    "{BASE}/search?q={}&type=track,album,artist,playlist,show,episode&limit=10&market=from_token",
                    urlencode(q)
                ))
            }
            Err(e) => Err(e),
        }
    }

    fn send_json(&self, method: &str, url: &str, body: Option<Value>) -> Result<String, String> {
        let text = body.map(|b| b.to_string());
        let body = text
            .as_deref()
            .map(|t| ("application/json", t.as_bytes()));
        let (status, resp) = self.call(method, url, body, true)?;
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
                .ext_agent
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

    /// Una playlist por el protocolo interno (playlist4): pistas, atributos y propietario.
    fn playlist4(&self, id: &str) -> Result<librespot_metadata::Playlist, String> {
        let session = self.session()?;
        let uri = SpotifyUri::from_uri(&format!("spotify:playlist:{id}"))
            .map_err(|e| e.to_string())?;
        self.block(TIMEOUT_LIST, librespot_metadata::Playlist::get(&session, &uri))
            .map_err(|e| format!("playlist: {e}"))
    }

    /// (id de pista, usuario que la añadió, fecha) de cada pista de una playlist por librespot,
    /// y sus metadatos sacados de la misma descarga: así la página no espera a otra petición
    /// (de la Web API, que puede estar limitada) para tener nombre y portada.
    fn playlist_items(&self, id: &str) -> Result<(Vec<(String, Option<String>, Option<String>)>, Playlist), String> {
        let pl = self.playlist4(id)?;
        // Hoy Spotify manda la lista entera en una respuesta (se han visto 936 elementos), pero
        // el mensaje dice su largo total y si viene cortada. Si faltara algo, la carga lo
        // guardaría como la playlist completa: que al menos se vea en el registro. Cuentan todos
        // los elementos (episodios y locales también), antes de quedarse con las pistas.
        let got = pl.contents.items.len();
        let start = pl.contents.position.max(0) as usize;
        let want = pl.length.max(0) as usize;
        if pl.contents.is_truncated || start + got < want {
            log::warn!(
                "[playlist] {id}: llegó truncada ({got} elementos desde {start} de {want}, truncated={})",
                pl.contents.is_truncated
            );
        }
        let meta = meta_from_playlist4(&pl, id);
        let items = pl
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
            .collect();
        Ok((items, meta))
    }

    /// Detalles de pistas por id: metadatos internos por lotes de 500 (no gastan cuota de la Web
    /// API). Ya no se prueba antes /tracks: a las apps en modo desarrollo Spotify siempre lo
    /// rechaza (403), y averiguarlo costaba en cada arranque una lectura de la Web API por hilo
    /// (con un 429, hasta 25 s de espera) antes de la primera playlist.
    fn tracks_by_ids(&self, ids: &[String]) -> Result<Vec<Track>, String> {
        let mut out = Vec::with_capacity(ids.len());
        for (i, chunk) in ids.chunks(BATCH_MAX).enumerate() {
            if i > 0 {
                yield_to_playback_blocking();
            }
            // Las primeras filas (álbum, canciones de un artista) quedan sembradas para el
            // reproductor; las de lotes posteriores, no.
            match self.tracks_via_batch(chunk, SEED_ROWS.saturating_sub(i * BATCH_MAX)) {
                Ok(t) => out.extend(t),
                // Una a una solo si son pocas. Con un lote de cientos, el error sube: pedirlas
                // sueltas agotaba el cupo de librespot y aun así se perdían pistas; quien pidió
                // conserva lo que tenía y reintenta más tarde.
                Err(e) if chunk.len() <= SINGLE_GET_MAX => {
                    log::warn!("metadatos por lotes: {e}; se piden una a una");
                    out.extend(self.tracks_via_librespot(chunk)?);
                }
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    /// Como tracks_by_ids, pero una entrada por id pedido y en su orden (`None` si Spotify no la
    /// tiene): una pista puede volver con otro id (Spotify sustituye ediciones) y hay que saber
    /// de cuál de las pedidas es.
    fn tracks_aligned(&self, ids: &[String]) -> Result<Vec<Option<Track>>, String> {
        let kind = librespot_protocol::extension_kind::ExtensionKind::TRACK_V4;
        let uris: Vec<Option<SpotifyUri>> = ids.iter().map(|id| SpotifyUri::from_uri(&format!("spotify:track:{id}")).ok()).collect();
        let valid: Vec<SpotifyUri> = uris.iter().flatten().cloned().collect();
        let metas = match self.metadata_batch::<librespot_metadata::Track>(&valid, kind, SEED_ROWS) {
            Ok(m) => m,
            // Una a una solo si son pocas, como en tracks_by_ids.
            Err(e) if valid.len() <= SINGLE_GET_MAX => {
                log::warn!("{e}; se piden una a una");
                let session = self.session()?;
                let keys: Vec<String> = valid.iter().map(|u| u.to_uri().unwrap_or_default()).collect();
                let all: Vec<usize> = (0..keys.len()).collect();
                let got = self.handle.block_on(get_singles::<librespot_metadata::Track>(&session, &keys, &all));
                if got.iter().all(|(_, m)| m.is_none()) {
                    return Err(e);
                }
                let mut metas = vec![None; keys.len()];
                for (i, m) in got {
                    metas[i] = m;
                }
                metas
            }
            Err(e) => return Err(e),
        };
        let mut metas = metas.into_iter();
        Ok(uris.iter().map(|u| u.as_ref().and_then(|_| metas.next().flatten()).map(track_from_meta)).collect())
    }

    /// Copia en disco de una playlist, si sirve para reutilizar sus pistas al recargarla. No
    /// sirve si no la hay o no se puede leer, si sus metadatos tienen LIST_META_TTL o más (o no
    /// se sabe de cuándo son: copias de antes de guardarlo) o si son de otro país, que cambia
    /// qué ediciones están disponibles: entonces se piden todas, como antes.
    fn list_copy(&self, id: &str, country: &str) -> Option<ListCopy> {
        let text = std::fs::read_to_string(self.lists_dir.join(format!("{id}.json"))).ok()?;
        let copy = match serde_json::from_str::<ListCopy>(&text) {
            Ok(c) => c,
            Err(e) => {
                log::info!("playlist {id}: copia en disco ilegible ({e}); se piden todas sus pistas");
                return None;
            }
        };
        let now = crate::cache::now_secs();
        // Una fecha futura (el reloj iba adelantado al guardarla) tampoco se da por buena.
        if copy.meta_at == 0 || copy.meta_at > now || now - copy.meta_at >= LIST_META_TTL {
            log::info!("playlist {id}: metadatos de la copia sin fecha o de hace una semana o más; se piden todos");
            return None;
        }
        if copy.country.as_deref() != Some(country) {
            log::info!("playlist {id}: la copia es de otro país ({:?}, ahora {country}); se piden todas sus pistas", copy.country);
            return None;
        }
        Some(copy)
    }

    /// Recarga de una playlist con su copia en disco: solo se piden (en un lote) las pistas que
    /// la copia no tiene, y la lista se rehace en el orden de `items`, una fila por posición
    /// (las repetidas también), con quién y cuándo se añadió en esa posición. Antes cada
    /// recarga volvía a pedir todas: 2.000 pistas, 5 lotes aunque solo hubiera una nueva.
    /// `None` si faltan tantas que conviene la carga por lotes en paralelo, que además muestra
    /// las primeras filas antes.
    fn playlist_from_copy(
        &self,
        req: &Req,
        id: &str,
        items: &[(String, Option<String>, Option<String>)],
        copy: ListCopy,
        country: &str,
        t0: Instant,
        ui: &UiTx,
    ) -> Option<Result<Resp, String>> {
        // Por id de pista: una repetida es la misma entrada, clonada en cada posición.
        let mut known: std::collections::HashMap<String, Track> =
            copy.tracks.into_iter().filter_map(|t| Some((t.id.clone()?, t))).collect();
        let mut seen = std::collections::HashSet::new();
        let missing: Vec<String> = items
            .iter()
            .map(|(tid, _, _)| tid)
            .filter(|tid| !known.contains_key(*tid) && seen.insert(*tid))
            .cloned()
            .collect();
        if missing.len() > BATCH_MAX {
            log::info!("playlist {id}: {} pistas nuevas respecto a la copia; se cargan todas por lotes", missing.len());
            return None;
        }
        if !missing.is_empty() {
            let fetched = match self.tracks_aligned(&missing) {
                Ok(f) => f,
                // Falla la carga: quien pidió conserva lo que tenía y reintenta más tarde.
                Err(e) => return Some(Err(e)),
            };
            for (tid, t) in missing.iter().zip(fetched) {
                if let Some(t) = t {
                    known.insert(tid.clone(), t);
                }
            }
        }
        // Las que Spotify ya no tiene no llegan (como en la carga por lotes): se omiten.
        let tracks: Vec<Track> = items
            .iter()
            .filter_map(|(tid, by, at)| {
                let mut t = known.get(tid)?.clone();
                t.added_by = by.clone();
                t.added_at = at.clone();
                Some(t)
            })
            .collect();
        log::info!(
            "[t] PlaylistTracks {id} n={} completa en {} ms (de la copia; {} pedidas)",
            items.len(),
            t0.elapsed().as_millis(),
            missing.len()
        );
        // Lo reutilizado conserva su fecha: la copia nueva lleva la de la anterior, y pasado
        // LIST_META_TTL se piden todas otra vez.
        let info = Resp::PlaylistCopyInfo { id: id.to_string(), meta_at: copy.meta_at, country: country.to_string() };
        ui.send(Msg::Api(ApiResult { req: req.clone(), result: Ok(info) }));
        Some(Ok(Resp::Tracks { key: id.to_string(), tracks, total: items.len() as u32, done: true }))
    }

    /// Nombre e imagen de un artista por los metadatos internos.
    fn artist_via_librespot(&self, id: &str) -> Result<Artist, String> {
        let session = self.session()?;
        let uri = SpotifyUri::from_uri(&format!("spotify:artist:{id}")).map_err(|e| e.to_string())?;
        let a = self
            .block(TIMEOUT_ITEM, librespot_metadata::Artist::get(&session, &uri))
            .map_err(|e| format!("artista: {e}"))?;
        Ok(self.artist_from_meta(id, &a))
    }

    /// Artista de los metadatos internos, con los géneros ya conocidos (los externos los
    /// completa quien llama, sin retrasar la página). Sin seguidores: no vienen ahí.
    fn artist_from_meta(&self, id: &str, a: &librespot_metadata::Artist) -> Artist {
        let mut images: Vec<Image> = a.portrait_group.iter().map(image_from_meta).collect();
        if images.is_empty() {
            images = a.portraits.iter().map(image_from_meta).collect();
        }
        Artist {
            id: id.to_string(),
            name: a.name.clone(),
            uri: format!("spotify:artist:{id}"),
            images,
            genres: self.cached_genres(&format!("artist:{id}")).unwrap_or_default(),
            followers: None,
        }
    }

    /// Nombre e imagen de varios artistas por lotes de metadatos internos: una petición por
    /// cada THUMBS_BATCH en vez de una a la Web API (y una búsqueda de géneros) por artista.
    /// Lo que el lote no traiga se omite: la tarjeta deja la inicial.
    fn artist_thumbs(&self, ids: &[String]) -> Result<Vec<Artist>, String> {
        let wanted: Vec<(&String, SpotifyUri)> = ids
            .iter()
            .filter_map(|id| Some((id, SpotifyUri::from_uri(&format!("spotify:artist:{id}")).ok()?)))
            .collect();
        let mut out = Vec::with_capacity(wanted.len());
        for (i, chunk) in wanted.chunks(THUMBS_BATCH).enumerate() {
            if i > 0 {
                yield_to_playback_blocking();
            }
            let uris: Vec<SpotifyUri> = chunk.iter().map(|(_, u)| u.clone()).collect();
            let metas = self.metadata_batch::<librespot_metadata::Artist>(&uris, librespot_protocol::extension_kind::ExtensionKind::ARTIST_V4, 0)?;
            // Por posición, con el id pedido: la tarjeta lo busca por ese.
            for ((id, _), m) in chunk.iter().zip(metas) {
                if let Some(a) = m {
                    out.push(self.artist_from_meta(id, &a));
                }
            }
        }
        Ok(out)
    }

    /// GET sin autorización (fuentes públicas: iTunes, Deezer, MusicBrainz), con tiempo límite corto.
    fn plain_get_json(&self, url: &str) -> Option<Value> {
        let resp = self
            .ext_agent
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

    /// Géneros ya conocidos, sin red. `None`: aún no se han buscado.
    fn cached_genres(&self, key: &str) -> Option<Vec<String>> {
        if let Some(g) = self.genres.lock().unwrap().get(key) {
            return Some(g.clone());
        }
        self.genres_miss.lock().unwrap().contains_key(key).then(Vec::new)
    }

    /// Géneros sin retrasar la página ni ocupar el hilo: buscarlos cuesta varias consultas
    /// externas en serie (segundos), así que si no se conocen ya se devuelven vacíos al momento y
    /// se encargan al carril de enriquecimiento, que los manda aparte (Resp::Genres). Antes se
    /// buscaban aquí mismo y uno de los dos hilos comunes quedaba ocupado tras dibujar la página.
    fn genres_or_queue(&self, key: &str, artist: &str, album: Option<&str>) -> Vec<String> {
        if let Some(g) = self.cached_genres(key) {
            return g;
        }
        let (lock, cv) = &*self.enrich;
        let mut q = lock.lock().unwrap();
        if q.queued.insert(key.to_string()) {
            q.genres.push_back((key.to_string(), artist.to_string(), album.map(str::to_string)));
            if q.genres.len() > ENRICH_GENRES_MAX {
                if let Some((old, _, _)) = q.genres.pop_front() {
                    q.queued.remove(&old);
                }
            }
            cv.notify_one();
        }
        Vec::new()
    }

    /// Géneros cacheados o consultados a fuentes externas. `key` = "artist:<id>" o "album:<id>".
    /// Spotify dejó de exponer géneros (Web API y metadatos internos vienen vacíos), así que se
    /// combinan iTunes Search (género principal), Deezer (géneros de álbum) y MusicBrainz (etiquetas).
    /// Solo desde el carril de enriquecimiento (genres_or_queue); `between` se llama antes de
    /// cada consulta para atender ahí las letras que esperen.
    fn external_genres(&self, key: &str, artist: &str, album: Option<&str>, between: &dyn Fn()) -> Vec<String> {
        if let Some(g) = self.genres.lock().unwrap().get(key) {
            return g.clone();
        }
        if self.genres_miss.lock().unwrap().contains_key(key) {
            return Vec::new();
        }
        // Alguna fuente no contestó (sin red, límite, plazo vencido): un resultado vacío entonces
        // no dice que no tenga géneros.
        let failed = std::cell::Cell::new(false);
        // Deezer avisa de su límite con un 200 y un objeto «error».
        let get = |url: &str| {
            // Una búsqueda son hasta tres consultas de varios segundos: las letras de la canción
            // que suena no esperan a todas, solo a la que esté en curso.
            between();
            let v = self.plain_get_json(url);
            if v.as_ref().map_or(true, |v| v.get("error").is_some()) {
                failed.set(true);
            }
            v
        };
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
                if let Some(v) = get(&format!("https://api.deezer.com/search/album?q={q}&limit=1")) {
                    let hit = v["data"][0].clone();
                    let title = hit["title"].as_str().unwrap_or("");
                    if !title.is_empty() && (norm(title) == norm(album) || norm(title).starts_with(&norm(album)) || norm(album).starts_with(&norm(title))) {
                        if let Some(id) = hit["id"].as_i64() {
                            if let Some(a) = get(&format!("https://api.deezer.com/album/{id}")) {
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
                if let Some(v) = get(&format!("https://itunes.apple.com/search?term={q}&entity=album&limit=3")) {
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
                if let Some(v) = get(&format!("https://itunes.apple.com/search?term={q}&entity=musicArtist&limit=3")) {
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
                if let Some(v) = get(&format!("https://musicbrainz.org/ws/2/artist/?query={q}&fmt=json&limit=1")) {
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
            let text = {
                let mut miss = self.genres_miss.lock().unwrap();
                miss.insert(key.to_string(), (!failed.get()).then(crate::cache::now_secs));
                // Al disco solo las que tienen fecha; se escribe sin el cerrojo, que los hilos
                // comunes miran en cada álbum o artista.
                (!failed.get())
                    .then(|| {
                        let disk: std::collections::HashMap<&String, u64> = miss.iter().filter_map(|(k, at)| Some((k, (*at)?))).collect();
                        serde_json::to_string(&disk).ok()
                    })
                    .flatten()
            };
            if let Some(t) = text {
                crate::cache::write_atomic(&self.genres_miss_file, &t);
            }
        } else {
            // Igual: fuera del cerrojo, y atómico (un cierre a medio escribir los perdía todos).
            // Solo escribe este carril, así que no se pisan dos versiones.
            let text = {
                let mut map = self.genres.lock().unwrap();
                map.insert(key.to_string(), out.clone());
                serde_json::to_string(&*map).ok()
            };
            if let Some(t) = text {
                crate::cache::write_atomic(&self.genres_file, &t);
            }
        }
        out
    }

    /// Álbum completo (con pistas) por los metadatos internos.
    fn album_via_librespot(&self, id: &str) -> Result<Album, String> {
        let session = self.session()?;
        let uri = SpotifyUri::from_uri(&format!("spotify:album:{id}")).map_err(|e| e.to_string())?;
        let a = self
            .block(TIMEOUT_ITEM, librespot_metadata::Album::get(&session, &uri))
            .map_err(|e| format!("álbum: {e}"))?;
        let ids: Vec<String> = a.discs.iter().flat_map(|d| d.tracks.iter()).filter_map(|u| u.to_id().ok()).collect();
        let tracks = self.tracks_by_ids(&ids)?;
        let total = tracks.len() as u32;
        Ok(Album {
            // Los externos los completa quien llama, sin retrasar la página.
            genres: self.cached_genres(&format!("album:{id}")).unwrap_or_default(),
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
        Ok(meta_from_playlist4(&self.playlist4(id)?, id))
    }

    /// Álbumes de varios grupos (discos, sencillos…) a partir de uris internos, en una sola
    /// petición; cada grupo ordenado por fecha descendente.
    fn albums_from_uri_groups(&self, groups: &[Vec<SpotifyUri>]) -> Vec<Vec<AlbumRef>> {
        let all: Vec<SpotifyUri> = groups.iter().flatten().cloned().collect();
        let mut fetched = self
            .metadata_batch::<librespot_metadata::Album>(&all, librespot_protocol::extension_kind::ExtensionKind::ALBUM_V4, 0)
            .unwrap_or_default()
            .into_iter();
        groups
            .iter()
            .map(|g| {
                let mut out: Vec<AlbumRef> = fetched.by_ref().take(g.len()).flatten().map(album_ref_from_meta).collect();
                out.sort_by(|x, y| y.release_date.cmp(&x.release_date));
                out
            })
            .collect()
    }
}

fn album_ref_from_meta(a: librespot_metadata::Album) -> AlbumRef {
    AlbumRef {
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
    }
}

/// Ids de las 10 canciones populares de un artista en el país de la cuenta (o, si no hay lista
/// para él, en el primero que traiga).
fn top_track_ids(a: &librespot_metadata::Artist, country: &str) -> Vec<String> {
    a.top_tracks
        .iter()
        .find(|t| t.country == country)
        .or_else(|| a.top_tracks.first())
        .map(|t| t.tracks.iter().filter_map(|u| u.to_id().ok()).take(10).collect())
        .unwrap_or_default()
}

impl Client {
    /// Vista completa del artista: biografía, relacionados y discografía por grupos. Las
    /// populares salen antes, aparte (como Resp::ArtistTop), del mismo Artist::get: antes eran
    /// otra petición que lo descargaba otra vez y ocupaba un hilo, y así no esperan al lote de
    /// la discografía.
    fn artist_view_via_librespot(&self, id: &str, ui: &UiTx) -> Result<ArtistView, String> {
        let session = self.session()?;
        let uri = SpotifyUri::from_uri(&format!("spotify:artist:{id}")).map_err(|e| e.to_string())?;
        let a = self
            .block(TIMEOUT_ITEM, librespot_metadata::Artist::get(&session, &uri))
            .map_err(|e| format!("artista: {e}"))?;
        // Un fallo de las populares llega como el de Req::ArtistTop y no impide la vista.
        let top = self.tracks_by_ids(&top_track_ids(&a, &session.country()));
        ui.send(Msg::Api(ApiResult { req: Req::ArtistTop(id.to_string()), result: top.map(Resp::ArtistTop) }));
        let firsts = |groups: &librespot_metadata::artist::AlbumGroups, cap: usize| -> Vec<SpotifyUri> {
            groups.iter().filter_map(|g| g.first().cloned()).take(cap).collect()
        };
        let mut groups = self
            .albums_from_uri_groups(&[firsts(&a.albums, 40), firsts(&a.singles, 40), firsts(&a.compilations, 20), firsts(&a.appears_on_albums, 30)])
            .into_iter();
        let mut next = || groups.next().unwrap_or_default();
        let (albums, singles, compilations, appears_on) = (next(), next(), next(), next());
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
            .block(TIMEOUT_ITEM, librespot_metadata::Show::get(&session, &uri))
            .map_err(|e| format!("podcast: {e}"))?;
        let ep_uris: Vec<SpotifyUri> = s.episodes.iter().take(50).cloned().collect();
        let fetched = self
            .metadata_batch::<librespot_metadata::Episode>(&ep_uris, librespot_protocol::extension_kind::ExtensionKind::EPISODE_V4, SEED_ROWS)
            .unwrap_or_default();
        let episodes: Vec<Episode> = fetched
            .into_iter()
            .flatten()
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
            .block(TIMEOUT_ITEM, librespot_metadata::Artist::get(&session, &uri))
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
        let fetched = self
            .metadata_batch::<librespot_metadata::Album>(&uris, librespot_protocol::extension_kind::ExtensionKind::ALBUM_V4, 0)
            .unwrap_or_default();
        let mut out: Vec<AlbumRef> = fetched
            .into_iter()
            .flatten()
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

    /// Metadatos de muchas entidades en una sola petición (extended-metadata, lo que usa la app
    /// oficial), en el orden pedido. Pedirlas una a una gasta una petición por elemento: además
    /// de lento, agota el cupo de librespot (300 cada 30 s) y entonces tampoco se pueden cargar
    /// canciones para sonar. Lo que el lote no traiga (pocas: ediciones regionales, retiradas)
    /// se pide suelto, con tope. Envoltorio síncrono de batch_chunk para quien no carga una
    /// playlist: lotes uno tras otro, como antes.
    ///
    /// `seed`: las primeras filas que se siembran en la caché de metadatos del reproductor (solo
    /// canciones y episodios, ver `batch_chunk`).
    fn metadata_batch<M: librespot_metadata::Metadata + Clone>(
        &self,
        uris: &[SpotifyUri],
        kind: librespot_protocol::extension_kind::ExtensionKind,
        seed: usize,
    ) -> Result<Vec<Option<M>>, String> {
        let session = self.session()?;
        let keys: Vec<String> = uris.iter().map(|u| u.to_uri().unwrap_or_default()).collect();
        // El tope de sueltos es por llamada, no por lote.
        let singles = std::cell::Cell::new(SINGLE_GET_MAX);
        self.handle.block_on(async {
            let mut out = Vec::with_capacity(keys.len());
            for (i, chunk) in keys.chunks(BATCH_MAX).enumerate() {
                if i > 0 {
                    yield_to_playback().await;
                }
                let seed = seed.saturating_sub(i * BATCH_MAX);
                let first = batch_chunk::<M>(&session, chunk, kind, &singles, seed).await;
                out.extend(batch_retry(&session, chunk, kind, &singles, first, seed).await?);
            }
            Ok::<_, String>(out)
        })
    }

    /// `Req::WarmMeta`: un lote con las canciones de `uris` cuyos metadatos aún no tiene el
    /// reproductor (como mucho WARM_META_MAX), sembrado en su caché. Sin sueltos: lo que el lote no
    /// traiga se pedirá al reproducirlo, como siempre.
    fn warm_metadata(&self, uris: &[String]) -> Result<(), String> {
        // Si las semillas no coincidieron con lo del reproductor ya no se guardan: el lote sería
        // una petición tirada en cada búsqueda.
        if !librespot_core::spclient::SpClient::seeds_accepted() {
            return Ok(());
        }
        let kind = librespot_protocol::extension_kind::ExtensionKind::TRACK_V4;
        let session = self.session()?;
        let spclient = session.spclient();
        let mut seen = std::collections::HashSet::new();
        // Lo ya guardado no se vuelve a pedir, aunque sea una semilla aún sin comprobar: el lote
        // traería los mismos bytes (la comprobación la hace el reproductor al cargarla).
        let keys: Vec<String> = uris
            .iter()
            .filter(|u| u.starts_with("spotify:track:") && seen.insert(u.as_str()) && !spclient.metadata_held(kind, u))
            .take(WARM_META_MAX)
            .cloned()
            .collect();
        if keys.is_empty() {
            return Ok(());
        }
        let singles = std::cell::Cell::new(0);
        let n = keys.len();
        self.block(TIMEOUT_SP_READ, async {
            batch_chunk::<librespot_metadata::Track>(&session, &keys, kind, &singles, n).await.map_err(|(e, _)| e)
        })?;
        log::debug!("[precarga] metadatos de {n} canciones de la búsqueda");
        Ok(())
    }

    fn tracks_via_batch(&self, ids: &[String], seed: usize) -> Result<Vec<Track>, String> {
        let uris: Vec<SpotifyUri> = ids.iter().filter_map(|id| SpotifyUri::from_uri(&format!("spotify:track:{id}")).ok()).collect();
        let tracks = self.metadata_batch::<librespot_metadata::Track>(&uris, librespot_protocol::extension_kind::ExtensionKind::TRACK_V4, seed)?;
        Ok(tracks.into_iter().flatten().map(track_from_meta).collect())
    }

    /// Pistas una a una (solo para tandas pequeñas: tracks_by_ids no pasa más de SINGLE_GET_MAX),
    /// pocas a la vez y en el orden pedido.
    fn tracks_via_librespot(&self, ids: &[String]) -> Result<Vec<Track>, String> {
        use futures::StreamExt;
        let session = self.session()?;
        let session = &session;
        let uris: Vec<SpotifyUri> = ids
            .iter()
            .filter_map(|id| SpotifyUri::from_uri(&format!("spotify:track:{id}")).ok())
            .collect();
        // Plazo por pista, no para todas juntas: una que no contesta no se lleva por delante
        // las que sí llegaron. Tras la primera que no contesta, las que aún no salieron no se
        // piden (como en get_singles): la conexión está colgada.
        let stalled = &std::cell::Cell::new(false);
        let fetched: Vec<_> = self.handle.block_on(
            futures::stream::iter(uris.iter().map(|u| async move {
                if stalled.get() {
                    return None;
                }
                let r = tokio::time::timeout(TIMEOUT_ITEM, librespot_metadata::Track::get(session, u)).await;
                if r.is_err() {
                    stalled.set(true);
                }
                r.ok()
            }))
            .buffered(SINGLE_GET_PARALLEL)
            .collect(),
        );
        let mut out = Vec::with_capacity(fetched.len());
        let mut first_err = None;
        for r in fetched {
            match r {
                Some(Ok(t)) => out.push(track_from_meta(t)),
                Some(Err(e)) => {
                    first_err.get_or_insert(e.to_string());
                }
                None => {
                    first_err.get_or_insert(timeout_message(TIMEOUT_ITEM));
                }
            }
        }
        // Que no llegue ninguna es un fallo (sin red, el limitador), no una lista vacía: así
        // quien pidió conserva lo que tenía en vez de quedarse sin pistas.
        match first_err {
            Some(e) if out.is_empty() => Err(format!("pistas: {e}")),
            _ => Ok(out),
        }
    }

    /// Petición protobuf a un endpoint interno (spclient): cuerpo y respuesta en bytes. Con el
    /// plazo de su método (`sp_timeout`).
    fn spclient_pb(&self, method: http::Method, endpoint: &str, body: Option<&[u8]>) -> Result<Vec<u8>, String> {
        self.spclient_pb_with(method, endpoint, body, &[])
    }

    /// Como `spclient_pb`, con cabeceras de más (la sonda de mezclas pide la playlist con la
    /// lente `spotify-apply-lenses`). Los nombres van en minúsculas.
    fn spclient_pb_with(&self, method: http::Method, endpoint: &str, body: Option<&[u8]>, extra: &[(&'static str, &str)]) -> Result<Vec<u8>, String> {
        let session = self.session()?;
        let mut headers = pb_headers();
        for (name, value) in extra {
            headers.insert(http::HeaderName::from_static(name), http::HeaderValue::from_str(value).map_err(|e| e.to_string())?);
        }
        let bytes = self.block(sp_timeout(&method), session.spclient().request(&method, endpoint, Some(headers), body))?;
        Ok(bytes.to_vec())
    }

    /// Rootlist completo tal como llega: entradas en orden, sus metadatos (`decorate`: revisión,
    /// atributos, tamaño y propietario de cada playlist) y la revisión del propio rootlist.
    fn rootlist_content(&self) -> Result<librespot_protocol::playlist4_external::SelectedListContent, String> {
        use protobuf::Message;
        let session = self.session()?;
        let bytes = self
            .block(TIMEOUT_LIST, session.spclient().get_rootlist(0, Some(5000)))
            .map_err(|e| format!("rootlist: {e}"))?;
        let msg = librespot_protocol::playlist4_external::SelectedListContent::parse_from_bytes(&bytes)
            .map_err(|e| format!("rootlist: {e}"))?;
        // Más de 5000 entradas no caben en esta lectura: que quede en el registro, en vez de
        // perder en silencio las carpetas del final (y desplazar los índices de las ediciones).
        let got = msg.contents.items.len();
        if msg.contents.truncated() || msg.length().max(0) as usize > got {
            log::warn!("[rootlist] llegó truncado: {got} de {} entradas", msg.length());
        }
        Ok(msg)
    }

    /// Rootlist completo (uris en orden) y su revisión.
    fn rootlist_raw(&self) -> Result<(Vec<u8>, Vec<String>), String> {
        let msg = self.rootlist_content()?;
        let uris = msg.contents.items.iter().map(|i| i.uri().to_string()).collect();
        Ok((msg.revision().to_vec(), uris))
    }

    /// Las playlists de la biblioteca, del rootlist (ver `library_from_rootlist`): una lectura
    /// de spclient, sin la cuota compartida de la Web API, que otros clientes también gastan.
    fn library_via_rootlist(&self) -> Result<Vec<Playlist>, String> {
        let t0 = Instant::now();
        let msg = self.rootlist_content()?;
        let list = library_from_rootlist(&msg.contents)?;
        log::info!("[biblioteca] {} playlists del rootlist en {} ms", list.len(), t0.elapsed().as_millis());
        Ok(list)
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

    /// Pone (arriba del todo) o quita una playlist del rootlist: lo mismo que seguirla o dejar de
    /// seguirla, sin la Web API. Si ya está (o ya no está), no hace nada.
    fn rootlist_set_playlist(&self, id: &str, follow: bool) -> Result<(), String> {
        use librespot_protocol::playlist4_external::{op, Add, Item, Op, Rem};
        use protobuf::MessageField;
        let (rev, list) = self.rootlist_raw()?;
        let uri = format!("spotify:playlist:{id}");
        let at = list.iter().position(|u| u == &uri);
        let mut op = Op::new();
        match (follow, at) {
            (true, None) => {
                let mut it = Item::new();
                it.set_uri(uri);
                let mut add = Add::new();
                add.set_from_index(0);
                add.items.push(it);
                op.set_kind(op::Kind::ADD);
                op.add = MessageField::some(add);
            }
            (false, Some(idx)) => {
                let mut rem = Rem::new();
                rem.set_from_index(idx as i32);
                rem.set_length(1);
                op.set_kind(op::Kind::REM);
                op.rem = MessageField::some(rem);
            }
            _ => return Ok(()),
        }
        self.rootlist_changes(rev, vec![op])
    }

    /// Revisión actual de una playlist (para playlist4 «changes»).
    fn playlist_revision(&self, id: &str) -> Result<Vec<u8>, String> {
        use protobuf::Message;
        let session = self.session()?;
        let sid = librespot_core::SpotifyId::from_base62(id).map_err(|e| e.to_string())?;
        let bytes = self.block(TIMEOUT_LIST, session.spclient().get_playlist(&sid))?;
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

    /// Petición a un endpoint interno de Spotify a través de librespot (spclient), con el plazo
    /// de su método (`sp_timeout`).
    fn spclient(&self, method: http::Method, endpoint: &str) -> Result<String, String> {
        let session = self.session()?;
        let bytes = self.block(sp_timeout(&method), session.spclient().request(&method, endpoint, None, None))?;
        String::from_utf8(bytes.to_vec()).map_err(|e| e.to_string())
    }

    /// `Req::MixProbe`: la sonda de las mezclas de Spotify (ver `crate::mixprobe`). Cada parte va
    /// por su lado: un fallo se apunta en el informe y se sigue con la siguiente, porque lo que se
    /// busca es justo saber qué contesta Spotify a una sesión de librespot y qué no. Solo lee: no
    /// toca la playlist, ni la cola, ni lo que suena. Devuelve el informe y el veredicto.
    fn mix_probe(&self, id: &str) -> (String, String) {
        use crate::mixprobe::{self as mp, section};
        use protobuf::Message;
        use std::collections::HashMap;
        use std::fmt::Write as _;
        let t0 = Instant::now();
        let head = format!("Nanofy {} · sonda de mezclas de spotify:playlist:{id}", crate::update::current_version());
        let session = match self.session() {
            Ok(s) => s,
            Err(e) => {
                let verdict = format!("sin datos: {e}");
                return (format!("{head}\n\n{verdict}\nFIN\n"), verdict);
            }
        };
        let mut out: Vec<String> = Vec::new();
        let mut transitions = std::collections::BTreeSet::new();

        // (a) La playlist4 tal cual llega: sin lente y con las lentes «mix» y «auto», por si la
        // mezcla solo aparece al pedirla así (Spotify la guarda como una lente de la playlist).
        // Se compara el resumen, no los bytes: la respuesta lleva la hora y nunca sería igual.
        let endpoint = format!("/playlist/v2/playlist/{id}");
        let mut list = mp::ListFindings::default();
        let mut base: Option<Vec<String>> = None;
        for lens in [None, Some("mix"), Some("auto")] {
            out.push(section(&match lens {
                None => "(a) playlist4 sin cabecera (GET /playlist/v2/playlist/<id>)".to_string(),
                Some(l) => format!("(a) playlist4 con spotify-apply-lenses: {l}"),
            }));
            let extra: Vec<(&'static str, &str)> = lens.map(|l| vec![("spotify-apply-lenses", l)]).unwrap_or_default();
            let bytes = match self.spclient_pb_with(http::Method::GET, &endpoint, None, &extra) {
                Ok(b) => b,
                Err(e) => {
                    out.push(format!("error: {e}"));
                    continue;
                }
            };
            let msg = match librespot_protocol::playlist4_external::SelectedListContent::parse_from_bytes(&bytes) {
                Ok(m) => m,
                Err(e) => {
                    out.push(format!("no se pudo leer ({} bytes): {e}", bytes.len()));
                    continue;
                }
            };
            let mut lines = Vec::new();
            let found = mp::describe_list(&msg, &mut lines, &mut transitions);
            out.push(format!("{} bytes", bytes.len()));
            if lens.is_some() && base.as_ref() == Some(&lines) {
                out.push("lo mismo que sin cabecera".into());
                continue;
            }
            out.extend(lines.iter().cloned());
            // La primera que se pudo leer es la de referencia (normalmente la que va sin lente).
            if base.is_none() {
                list = found;
                base = Some(lines);
            }
        }

        // (b) context-resolve, el mismo que pide Spirc al reproducir la playlist: ahí van los
        // metadatos de cada pista (audio.fade_*, automix.*) si Spotify los manda.
        out.push(section("(b) context-resolve (GET /context-resolve/v1/spotify:playlist:<id>, como Spirc)"));
        let ctx = match self.block(TIMEOUT_SP_READ, session.spclient().get_context_raw(&format!("spotify:playlist:{id}"))) {
            Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(v) => {
                    out.push(format!("{} bytes", bytes.len()));
                    mp::describe_context(&v, &list.items, &mut out, &mut transitions)
                }
                Err(e) => {
                    out.push(format!("no es JSON ({} bytes): {e}", bytes.len()));
                    mp::ContextFindings::default()
                }
            },
            Err(e) => {
                out.push(format!("error: {e}"));
                mp::ContextFindings::default()
            }
        };

        // (c) extended-metadata con los tipos de las mezclas, por número: el estado de mezcla de
        // la playlist, el análisis de dos canciones y los datos de las transiciones encontradas.
        out.push(section("(c) extended-metadata (tipos de las mezclas, pedidos por número)"));
        let mut tracks = list.track_uris();
        if tracks.is_empty() {
            tracks = ctx.tracks.iter().filter(|u| u.starts_with("spotify:track:")).cloned().collect();
        }
        let sample: Vec<String> = tracks.iter().take(2).cloned().collect();
        let playlist_uri = vec![format!("spotify:playlist:{id}")];
        let mut with_data: Vec<String> = Vec::new();
        let mut note = |kind: i32, name: &str, n: usize| {
            if n > 0 {
                with_data.push(format!("{kind} {name} ×{n}"));
            }
        };
        let (k, name) = mp::KIND_MIX_STATE;
        note(k, name, self.mix_probe_kind(&session, &playlist_uri, k, name, mp::DETAIL, &mut out).len());
        if sample.is_empty() {
            out.push("la playlist no tiene canciones: no se piden los tipos de canción".into());
        } else {
            for &(k, name) in mp::TRACK_KINDS {
                note(k, name, self.mix_probe_kind(&session, &sample, k, name, mp::DETAIL, &mut out).len());
            }
        }
        let transition_uris: Vec<String> = transitions.iter().take(mp::TRANSITIONS_MAX).cloned().collect();
        out.push(format!("uris de transición encontradas en (a) y (b): {}", transitions.len()));
        let (td_kind, td_name) = mp::KIND_TRANSITION_DATA;
        let mut transition_data = 0;
        if transition_uris.is_empty() {
            // Sin uris no hay a quién pedirle la transición; con la de la playlist se ve al menos
            // si el tipo existe para esta sesión (y qué estado devuelve).
            out.push("ninguna: se prueba TRANSITION_DATA con la uri de la playlist".into());
            let n = self.mix_probe_kind(&session, &playlist_uri, td_kind, td_name, mp::DETAIL, &mut out).len();
            note(td_kind, td_name, n);
        } else {
            for &(k, name) in mp::TRANSITION_KINDS {
                let n = self.mix_probe_kind(&session, &transition_uris, k, name, mp::DETAIL, &mut out).len();
                note(k, name, n);
                if k == td_kind {
                    transition_data = n;
                }
            }
        }

        // (d) El BPM de cada canción, como lo enseña el editor de mezclas de Spotify: del tipo 222
        // en un lote y de cualquier metadato con pinta de tempo de (a) y (b).
        out.push(section("(d) BPM y tonalidad por canción (AUDIO_ATTRIBUTES_V2 y claves con pinta de tempo)"));
        let bpm_tracks: Vec<String> = tracks.iter().take(mp::BPM_MAX).cloned().collect();
        let (k, name) = mp::KIND_AUDIO_ATTRIBUTES;
        let attrs: HashMap<String, Vec<u8>> = if bpm_tracks.is_empty() {
            HashMap::new()
        } else {
            self.mix_probe_kind(&session, &bpm_tracks, k, name, 0, &mut out).into_iter().map(|(u, _, b)| (u, b)).collect()
        };
        let with_tempo_keys = bpm_tracks.iter().any(|u| list.tempo.contains_key(u) || ctx.tempo.contains_key(u));
        let with_bpm = bpm_tracks.iter().filter(|u| attrs.get(*u).and_then(|b| mp::bpm_guess(b)).is_some()).count();
        if attrs.is_empty() && !with_tempo_keys {
            out.push("ninguna canción trae BPM: ni AUDIO_ATTRIBUTES_V2 ni claves con pinta de tempo".into());
        } else {
            // Nombres para cotejar con lo que enseña el teléfono; sin sembrar la caché del
            // reproductor (es un diagnóstico, no una precarga).
            let ids: Vec<String> = bpm_tracks.iter().filter_map(|u| u.strip_prefix("spotify:track:")).map(str::to_string).collect();
            let names: HashMap<String, String> = match self.tracks_via_batch(&ids, 0) {
                Ok(found) => found.into_iter().map(|t| (t.uri.clone(), format!("{} — {}", t.artists_str(), t.name))).collect(),
                Err(e) => {
                    out.push(format!("sin los nombres de las canciones: {e}"));
                    HashMap::new()
                }
            };
            for (i, uri) in bpm_tracks.iter().enumerate() {
                let mut line = format!("  {:>3}. {} [{uri}]", i + 1, mp::clip(names.get(uri).map(String::as_str).unwrap_or("?")));
                match attrs.get(uri) {
                    Some(b) => {
                        if let Some(bpm) = mp::bpm_guess(b) {
                            let _ = write!(line, " · BPM probable {bpm:.1}");
                        }
                        let _ = write!(line, " · 222 {}", mp::compact(b, 160));
                    }
                    None => line.push_str(" · 222 sin datos"),
                }
                for t in list.tempo.get(uri).into_iter().chain(ctx.tempo.get(uri)).flatten() {
                    let _ = write!(line, " · {t}");
                }
                out.push(line);
            }
        }

        // (e) El análisis de audio por spclient (la Web API lo cerró a las apps nuevas): la otra
        // fuente posible de tempo, pulsos y secciones para las mezclas propias.
        out.push(section("(e) audio-analysis (GET /audio-attributes/v1/audio-analysis/<id>?format=json)"));
        let mut analysis_ok = 0;
        for uri in &sample {
            let tid = uri.trim_start_matches("spotify:track:");
            match self.spclient(http::Method::GET, &format!("/audio-attributes/v1/audio-analysis/{tid}?format=json")) {
                Ok(text) => match serde_json::from_str::<Value>(&text) {
                    Ok(v) => {
                        if v["track"]["tempo"].is_number() {
                            analysis_ok += 1;
                        }
                        out.push(format!("  {uri}: {} bytes", text.len()));
                        out.extend(mp::describe_analysis(&v).into_iter().map(|l| format!("    {l}")));
                    }
                    Err(e) => out.push(format!("  {uri}: no es JSON ({} bytes): {e}", text.len())),
                },
                Err(e) => out.push(format!("  {uri}: error: {e}")),
            }
        }
        if sample.is_empty() {
            out.push("sin canciones".into());
        }

        // Resumen arriba, con el veredicto del plan.
        let code = mp::decide(ctx.with_transition_keys > 0, transition_data > 0);
        let verdict = format!("{code}: {}", mp::explain(code));
        let flags = list.mix_flags();
        let n_tracks = ctx.tracks.len().max(list.items.len());
        let summary = [
            format!("veredicto: {verdict}"),
            format!("marca de mezcla de la playlist (atributos de formato): {}", if flags.is_empty() { "ninguna".to_string() } else { flags.join("; ") }),
            format!(
                "claves de mezcla del contexto: {}",
                if ctx.context_mix.is_empty() { "ninguna".to_string() } else { ctx.context_mix.join("; ") }
            ),
            format!("pistas con claves de transición en context-resolve: {} de {n_tracks}", ctx.with_transition_keys),
            format!("pistas con alguna clave de mezcla en context-resolve: {} de {n_tracks}", ctx.with_mix_keys),
            if ctx.uid_compared > 0 {
                format!("uid de context-resolve == item_id de playlist4: {} de {}", ctx.uid_equal, ctx.uid_compared)
            } else {
                "uid de context-resolve == item_id de playlist4: no se pudo comparar".to_string()
            },
            format!("uris de transición: {} · TRANSITION_DATA con datos: {transition_data}", transitions.len()),
            format!("extensiones con datos: {}", if with_data.is_empty() { "ninguna".to_string() } else { with_data.join(", ") }),
            format!("BPM (222): {with_bpm} de {} canciones · audio-analysis: {analysis_ok} de {}", bpm_tracks.len(), sample.len()),
            format!("duración de la sonda: {} ms", t0.elapsed().as_millis()),
        ];
        let mut report = String::new();
        let _ = writeln!(report, "{head}");
        let _ = writeln!(report, "Solo nombres de claves y valores de hasta {} caracteres; las cabeceras de las peticiones no se escriben.", mp::MAX_VALUE);
        let _ = writeln!(report, "{}", section("RESUMEN"));
        for l in summary.iter().chain(out.iter()) {
            let _ = writeln!(report, "{l}");
        }
        let _ = writeln!(report, "\nFIN");
        (report, verdict)
    }

    /// Una petición de extended-metadata de la sonda de mezclas: un tipo, por número, para estas
    /// entidades, con su resumen en el informe (`detail`: entidades con volcado). Devuelve las que
    /// traen datos.
    fn mix_probe_kind(
        &self,
        session: &librespot_core::Session,
        keys: &[String],
        kind: i32,
        name: &str,
        detail: usize,
        out: &mut Vec<String>,
    ) -> Vec<(String, i32, Vec<u8>)> {
        out.push(format!("-- {kind} {name} · {} entidades", keys.len()));
        let body = match build_batch_body_raw(session, keys, kind) {
            Ok(b) => b,
            Err(e) => {
                out.push(format!("   no se pudo armar la petición: {e}"));
                return Vec::new();
            }
        };
        let bytes = match self.block(TIMEOUT_SP_READ, fetch_batch(session, &body)) {
            Ok(b) => b,
            Err(e) => {
                out.push(format!("   error: {e}"));
                return Vec::new();
            }
        };
        match crate::mixprobe::describe_batch(&bytes, detail, out) {
            Ok(found) => found,
            Err(e) => {
                out.push(format!("   respuesta ilegible ({} bytes): {e}", bytes.len()));
                Vec::new()
            }
        }
    }

    fn exec(&self, req: &Req, ui: &UiTx) -> Result<Resp, String> {
        match req {
            Req::Me => Ok(Resp::Me(self.get_json(&format!("{BASE}/me"))?)),
            Req::Playlists => match self.library_via_rootlist() {
                Ok(list) => Ok(Resp::Playlists { list, rootlist: true }),
                // Sin rootlist (o con metadatos que no casan con sus entradas): la Web API, como
                // antes. Su error es el que se enseña; el del rootlist queda en el registro.
                Err(e) => {
                    log::warn!("[biblioteca] sin listado del rootlist ({e}); se pide a la Web API");
                    let list = self.all_pages::<Playlist>(&format!("{BASE}/me/playlists?limit=50"), 40)?;
                    Ok(Resp::Playlists { list, rootlist: false })
                }
            },
            Req::PlaylistsWeb => Ok(Resp::PlaylistsWeb(
                self.all_pages::<Playlist>(&format!("{BASE}/me/playlists?limit=50"), 40)?,
            )),
            Req::PlaylistMeta(id) => {
                // Las generadas por Spotify (radios, mixes) no están en la Web API para apps
                // externas: siempre daban 404 y luego se bajaba su playlist4. Directamente esta.
                if id.starts_with("37i9dQZ") {
                    return Ok(Resp::PlaylistMetaPartial(self.playlist_meta_via_librespot(id)?));
                }
                let url = format!("{BASE}/playlists/{id}?fields=id,name,uri,description,images,owner,tracks.total,public,collaborative,followers");
                // Solo completa lo que ya trae la carga de pistas (privacidad, seguidores, nombre
                // del propietario): ante un 429 no espera, falla al momento.
                match self.get_json_nowait::<Playlist>(&url) {
                    Ok(p) => Ok(Resp::PlaylistMeta(p)),
                    // Con Spotify limitando no se baja la playlist4 otra vez: la carga de pistas
                    // ya envía esos mismos metadatos y solo ocuparía un hilo.
                    Err(e)
                        if retry_secs(&e).is_some()
                            || e == NO_APP_HINT
                            || e.starts_with("Spotify ha agotado la cuota")
                            || e.starts_with("Spotify está limitando") =>
                    {
                        Err(e)
                    }
                    Err(e) => {
                        // La Web API no la sirve: nombre, portada y tamaño por los metadatos internos.
                        log::info!("/playlists/{id} no disponible ({e}); usando metadatos de librespot");
                        Ok(Resp::PlaylistMetaPartial(self.playlist_meta_via_librespot(id)?))
                    }
                }
            }
            Req::PlaylistTracks(id) => {
                // Spotify no permite /playlists/{id}/tracks a las apps en modo desarrollo:
                // los uris salen del protocolo interno (librespot) y los detalles por lotes.
                let t0 = Instant::now();
                // En el orden de la playlist, una entrada por posición: quién y cuándo la añadió
                // va por posición, no por id (por id, cada copia repetida se quedaba con la
                // fecha de la última).
                let (items, meta) = self.playlist_items(id)?;
                // Los metadatos salen de la misma playlist4, antes de la primera fila: la página
                // tiene nombre y portada sin esperar a /playlists/{id} (que ya no se pide para
                // las de la biblioteca ni las de Spotify), y la precarga y la restauración dejan
                // también el nombre del contexto.
                ui.send(Msg::Api(ApiResult { req: req.clone(), result: Ok(Resp::PlaylistMetaPartial(meta)) }));
                let total = items.len() as u32;
                if items.is_empty() {
                    return Ok(Resp::Tracks {
                        key: id.clone(),
                        tracks: Vec::new(),
                        total,
                        done: true,
                    });
                }
                let session = self.session()?;
                let country = session.country();
                // Recarga: con una copia en disco reciente solo se piden las pistas que no tiene.
                if let Some(copy) = self.list_copy(id, &country) {
                    if let Some(result) = self.playlist_from_copy(req, id, &items, copy, &country, t0, ui) {
                        return result;
                    }
                }
                // Todas pedidas ahora: la copia que se guarde lleva esta fecha.
                let meta_at = crate::cache::now_secs();
                let keys: Vec<String> = items.iter().map(|(i, _, _)| format!("spotify:track:{i}")).collect();
                // Un primer lote pequeño para que las primeras filas salgan tan pronto como
                // antes, y el resto de 500 en 500 con hasta 3 peticiones en vuelo: antes eran
                // lotes de 100 uno tras otro (2.000 pistas, 20 idas y vueltas en serie).
                // `buffered` entrega en orden aunque terminen desordenados.
                let first = keys.len().min(PLAYLIST_FIRST_BATCH);
                let chunks: Vec<&[String]> = std::iter::once(&keys[..first]).chain(keys[first..].chunks(BATCH_MAX)).collect();
                let last = chunks.len() - 1;
                let kind = librespot_protocol::extension_kind::ExtensionKind::TRACK_V4;
                // Tope de sueltos para toda la carga: con varios lotes en vuelo, el de cada lote
                // por sí solo permitiría cientos y agotaría el cupo de librespot.
                let singles = std::cell::Cell::new(SINGLE_GET_LOAD_MAX);
                let (session, singles, items) = (&session, &singles, &items);
                self.handle.block_on(async {
                    use futures::StreamExt;
                    // El reintento de un lote va dentro de su futuro, no aquí: mientras se espera
                    // aquí a otra cosa, los lotes en vuelo no avanzan, y su plazo (que corre igual)
                    // vencería sin que hubieran tenido ocasión de terminar.
                    // Cada lote, salvo el primero, cede antes el paso a una canción que esté
                    // cargando (`yield_to_playback`); la espera va fuera del plazo del lote.
                    // Cuántas filas de cada lote se siembran para el reproductor: solo las primeras
                    // SEED_ROWS de la lista (ver `seed_metadata`).
                    let offsets: Vec<usize> = chunks.iter().scan(0, |at, c| {
                        let start = *at;
                        *at += c.len();
                        Some(start)
                    }).collect();
                    let offsets = &offsets;
                    let mut batches = futures::stream::iter(chunks.iter().enumerate().map(|(i, &c)| async move {
                        if i > 0 {
                            yield_to_playback().await;
                        }
                        let seed = SEED_ROWS.saturating_sub(offsets[i]);
                        let first = batch_chunk::<librespot_metadata::Track>(session, c, kind, singles, seed).await;
                        batch_retry(session, c, kind, singles, first, seed).await
                    }))
                    .buffered(PLAYLIST_BATCH_PARALLEL);
                    let mut offset = 0;
                    for (ci, &chunk) in chunks.iter().enumerate() {
                        let Some(fetched) = batches.next().await else { break };
                        let metas = match fetched {
                            Ok(m) => m,
                            // Como en tracks_by_ids: una tanda pequeña (una playlist corta, o lo
                            // que sobra tras los lotes de 500) se pide una a una; solo falla si
                            // no llega ninguna.
                            Err(e) if chunk.len() <= SINGLE_GET_MAX => {
                                log::warn!("{e}; se piden una a una");
                                let all: Vec<usize> = (0..chunk.len()).collect();
                                let got = get_singles::<librespot_metadata::Track>(session, chunk, &all).await;
                                if got.iter().all(|(_, m)| m.is_none()) {
                                    return Err(e);
                                }
                                let mut metas = vec![None; chunk.len()];
                                for (i, m) in got {
                                    metas[i] = m;
                                }
                                metas
                            }
                            // Falla toda la carga: quien pidió conserva lo que tenía (la copia
                            // del disco) y reintenta más tarde, en vez de recibir una lista con
                            // cientos de huecos que acabaría guardada como buena.
                            Err(e) => return Err(e),
                        };
                        let tracks: Vec<Track> = metas
                            .into_iter()
                            .zip(&items[offset..])
                            .filter_map(|(m, (_, by, at))| {
                                let mut t = track_from_meta(m?);
                                t.added_by = by.clone();
                                t.added_at = at.clone();
                                Some(t)
                            })
                            .collect();
                        offset += chunk.len();
                        if ci == last {
                            log::info!("[t] PlaylistTracks {id} n={total} completa en {} ms", t0.elapsed().as_millis());
                            let info = Resp::PlaylistCopyInfo { id: id.clone(), meta_at, country: country.clone() };
                            ui.send(Msg::Api(ApiResult { req: req.clone(), result: Ok(info) }));
                            // El último lo envía run() con lo que devuelve: enviarlo aquí también
                            // duplicaría filas.
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
                })
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
                    // Como en stream_tracks: sin uri no se puede reproducir ni contar como nueva.
                    tracks.extend(
                        page.items
                            .into_iter()
                            .filter_map(|i| i.track)
                            .filter(|t| !t.uri.is_empty())
                            .map(slim_track),
                    );
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
            // /me/albums trae cada álbum con hasta 50 pistas: nada las usa (la página del álbum
            // pide las suyas) y engordaban la instantánea y cada guardado. Fuera ya aquí, como
            // hace Resp::AlbumSaved.
            Req::SavedAlbums => Ok(Resp::SavedAlbums(
                self.all_pages::<SavedAlbum>(&format!("{BASE}/me/albums?limit=50"), 20)?
                    .into_iter()
                    .map(|s| {
                        let mut a = s.album;
                        a.tracks = None;
                        a
                    })
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
                // Sin esperar un Retry-After: con Spotify limitando, los metadatos internos traen
                // el álbum en una petición (antes se esperaba hasta 25 s para luego caer a ellos).
                let mut album: Album = match self.get_json_nowait(&format!("{BASE}/albums/{id}")) {
                    Ok(a) => a,
                    Err(e) => {
                        log::info!("/albums/{id} no disponible ({e}); usando metadatos de librespot");
                        self.album_via_librespot(id)?
                    }
                };
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
                if album.genres.is_empty() {
                    let by = album.artists.first().map(|a| a.name.clone()).unwrap_or_default();
                    let name = album.name.clone();
                    album.genres = self.genres_or_queue(&format!("album:{id}"), &by, Some(&name));
                }
                Ok(Resp::Album(album))
            }
            Req::Artist(id) => {
                let mut a = match self.get_json::<Artist>(&format!("{BASE}/artists/{id}")) {
                    Ok(a) => a,
                    Err(e) => {
                        log::info!("/artists/{id} no disponible ({e}); usando metadatos de librespot");
                        self.artist_via_librespot(id)?
                    }
                };
                if a.genres.is_empty() {
                    let name = a.name.clone();
                    a.genres = self.genres_or_queue(&format!("artist:{id}"), &name, None);
                }
                Ok(Resp::Artist(a))
            }
            Req::ArtistThumbs(ids) => Ok(Resp::ArtistThumbs(self.artist_thumbs(ids)?)),
            Req::WarmMeta(uris) => {
                self.warm_metadata(uris)?;
                Ok(Resp::Done)
            }
            Req::Genres { key } => Ok(Resp::Genres {
                key: key.clone(),
                genres: self.cached_genres(key).unwrap_or_default(),
            }),
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
                // /artists/{id}/top-tracks está restringido: usamos los metadatos internos. La
                // página de artista ya no lo pide (salen con Req::ArtistView); queda para el
                // control y el diagnóstico.
                let session = self.session()?;
                let uri = SpotifyUri::from_uri(&format!("spotify:artist:{id}"))
                    .map_err(|e| e.to_string())?;
                let artist = self
                    .block(TIMEOUT_ITEM, librespot_metadata::Artist::get(&session, &uri))
                    .map_err(|e| format!("artista: {e}"))?;
                Ok(Resp::ArtistTop(self.tracks_by_ids(&top_track_ids(&artist, &session.country()))?))
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
                // Primero la búsqueda de los clientes oficiales: una petición de ~1 s que no gasta
                // cuota de la Web API ni se para con su enfriamiento. Si no responde bien, la de
                // la Web API, que falla rápido ante un 429.
                let t0 = Instant::now();
                // Segundos que pide un 429 de pathfinder, para reintentar solo si no hay Web API.
                let mut retry: Option<u64> = None;
                // pathfinder contestó, pero sin nada: se pregunta a la Web API y, si esa no
                // puede, vale esta respuesta vacía antes que un error.
                let mut empty: Option<SearchResult> = None;
                let why = match self.pf.usable(self.web_cooldown_left().is_some()) {
                    Err((why, secs)) => {
                        retry = secs;
                        why
                    }
                    Ok(hash) => match self.pf.search(q, &hash) {
                        Ok(r) => {
                            self.pf.ok(&hash);
                            log::info!("[t] búsqueda «{q}» por pathfinder en {} ms", t0.elapsed().as_millis());
                            return Ok(Resp::Search(r));
                        }
                        Err(PfErr::Empty(r)) => {
                            self.pf.ok(&hash);
                            empty = Some(r);
                            "pathfinder no encontró nada".to_string()
                        }
                        Err(PfErr::UnknownHash) => {
                            self.pf.reject(&hash, q);
                            "hash de searchDesktop rechazado".to_string()
                        }
                        Err(PfErr::Down(e)) => {
                            self.pf.down(&e);
                            e
                        }
                        Err(PfErr::Limited(secs)) => {
                            self.pf.limited(secs);
                            retry = Some(secs);
                            format!("pathfinder limita {secs} s")
                        }
                        Err(PfErr::Failed(e)) => e,
                    },
                };
                // Con la cuota de la Web API agotada la búsqueda es solo pathfinder: si limitó,
                // «reintenta en N s» para que la interfaz la repita sola; si no, su fallo.
                if let Some(left) = self.web_cooldown_left() {
                    if let Some(r) = empty {
                        return Ok(Resp::Search(r));
                    }
                    return Err(match retry {
                        Some(secs) => throttle_message(secs),
                        None => format!("La búsqueda no respondió ({why}). {}", cooldown_message(left)),
                    });
                }
                match self.search_web(q) {
                    Ok(r) => {
                        log::info!("[t] búsqueda «{q}» por la Web API en {} ms ({why})", t0.elapsed().as_millis());
                        Ok(Resp::Search(r))
                    }
                    Err(e) => match empty {
                        Some(r) => {
                            log::info!("búsqueda «{q}» sin resultados en pathfinder; la Web API falló ({e})");
                            Ok(Resp::Search(r))
                        }
                        None => Err(e),
                    },
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
            Req::MixProbe(id) => {
                let (report, verdict) = self.mix_probe(id);
                let path = crate::mixprobe::report_path(id);
                match crate::mixprobe::write_report(&path, &report) {
                    Ok(()) => log::info!("[mezcla] sonda de {id}: {verdict}; informe en {}", path.display()),
                    Err(e) => log::warn!("[mezcla] sonda de {id}: {verdict}; no se pudo escribir {}: {e}", path.display()),
                }
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
                // El rootlist entero, igual que lo leen las ediciones (rootlist_raw). Con el largo
                // por defecto de librespot (120 entradas), en una biblioteca grande las carpetas
                // del final faltaban o salían con la mitad de sus playlists.
                let (_, uris) = self.rootlist_raw()?;
                // Las carpetas son pares start-group/end-group alrededor de sus playlists.
                let mut folders: Vec<Folder> = Vec::new();
                let mut stack: Vec<Folder> = Vec::new();
                for uri in &uris {
                    if let Some(rest) = uri.strip_prefix("spotify:start-group:") {
                        let (id, name) = rest.split_once(':').unwrap_or((rest, ""));
                        let name = urldecode(name);
                        stack.push(Folder { id: id.to_string(), name, playlists: Vec::new() });
                    } else if uri.starts_with("spotify:end-group:") {
                        if let Some(f) = stack.pop() {
                            // Las subcarpetas cuentan como carpetas propias; la madre solo lista playlists.
                            folders.push(f);
                        }
                    } else if let Some(pid) = rootlist_playlist_id(uri) {
                        // Mismo criterio que el listado de la biblioteca (library_from_rootlist).
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
                let mut body = json!({
                    "name": name,
                    "description": description,
                });
                match public {
                    Some(public) => {
                        body["public"] = json!(public);
                        body["collaborative"] = json!(*collaborative && !public);
                    }
                    // Sin saber si es pública no se toca: colaborativa ya implica privada.
                    None => body["collaborative"] = json!(*collaborative),
                }
                self.send_json("PUT", &format!("{BASE}/playlists/{id}"), Some(body))?;
                Ok(Resp::PlaylistChanged(id.clone()))
            }
            Req::SetPlaylistImage { id, path } => {
                let jpeg = encode_cover(path)?;
                let b64 = base64::engine::general_purpose::STANDARD.encode(&jpeg);
                let (status, text) = self.call(
                    "PUT",
                    &format!("{BASE}/playlists/{id}/images"),
                    Some(("image/jpeg", b64.as_bytes())),
                    true,
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
            // Con la Web API limitada (o sin cuota), por el rootlist, como añadir y quitar canciones:
            // guardar o quitar una playlist de la biblioteca no se queda sin hacer.
            Req::FollowPlaylist(id) => {
                if let Err(e) = self.send_json("PUT", &format!("{BASE}/playlists/{id}/followers"), None) {
                    log::info!("FollowPlaylist por Web API falló ({e}); probando el rootlist");
                    self.rootlist_set_playlist(id, true)?;
                }
                Ok(Resp::PlaylistChanged(id.clone()))
            }
            Req::UnfollowPlaylist(id) => {
                if let Err(e) = self.send_json("DELETE", &format!("{BASE}/playlists/{id}/followers"), None) {
                    log::info!("UnfollowPlaylist por Web API falló ({e}); probando el rootlist");
                    self.rootlist_set_playlist(id, false)?;
                }
                Ok(Resp::PlaylistChanged(id.clone()))
            }
            Req::User(id) => {
                // /users/{id} está restringido: perfil por el endpoint interno de librespot,
                // que además trae las playlists públicas y si ya lo sigues.
                let session = self.session()?;
                let bytes = self
                    .block(TIMEOUT_ITEM, session.spclient().get_user_profile(id, Some(50), Some(0)))
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
                                    snapshot_id: None,
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
                    // Con plazo: una llamada colgada pararía el carril de enriquecimiento entero
                    // (los géneros esperan detrás). Si vence, se prueba LRCLIB.
                    let result = self.handle.block_on(async {
                        tokio::time::timeout(
                            Duration::from_secs(5),
                            session.spclient().request(&http::Method::GET, &endpoint, Some(headers), None),
                        )
                        .await
                    });
                    if let Ok(Ok(bytes)) = result {
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
                            // Sin respuesta en el plazo, la conexión está colgada: los demás
                            // esperarían otro plazo entero cada uno (hasta 2 min con el hilo ocupado).
                            let stalled = e == timeout_message(TIMEOUT_SP_WRITE);
                            last = e;
                            if stalled {
                                break;
                            }
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
                view: self.artist_view_via_librespot(id, ui)?,
            }),
            Req::Show(id) => {
                // Web API si está disponible; si no, metadatos internos.
                let web: Result<(Show, Vec<Episode>), String> = (|| {
                    let show: Show = self.get_json(&format!("{BASE}/shows/{id}?market=from_token"))?;
                    let first: Paging<Option<Episode>> = self.get_json(&format!("{BASE}/shows/{id}/episodes?limit=50&market=from_token"))?;
                    let mut episodes: Vec<Episode> = first.items.into_iter().flatten().collect();
                    // La página se muestra ya con los primeros 50 episodios; el resto (hasta 150
                    // más) y los temas llegan después en la misma respuesta completa.
                    ui.send(Msg::Api(ApiResult { req: req.clone(), result: Ok(Resp::Show { show: show.clone(), episodes: episodes.clone() }) }));
                    if let Some(next) = first.next {
                        episodes.extend(self.all_pages::<Option<Episode>>(&next, 3)?.into_iter().flatten());
                    }
                    Ok((show, episodes))
                })();
                match web {
                    Ok((mut show, episodes)) => {
                        // Los temas del podcast solo vienen por los metadatos internos.
                        if let Ok(session) = self.session() {
                            if let Ok(uri) = SpotifyUri::from_uri(&format!("spotify:show:{id}")) {
                                if let Ok(s) = self.block(TIMEOUT_ITEM, librespot_metadata::Show::get(&session, &uri)) {
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

/// Lectura de la Web API sin la biblioteca conectada (el token de login5 recibió un 429). No es
/// pasajero: hasta conectar la biblioteca seguirá igual, así que no promete reintentar. Se compara
/// por valor (`Req::PlaylistMeta`) y la barra lateral le añade «Conectar con Spotify».
pub const NO_APP_HINT: &str = "Conecta tu biblioteca con Spotify (no hace falta crear ninguna app).";

/// Metadatos de una playlist a partir de su playlist4 (la misma descarga que trae las pistas).
/// Sin nombre visible del propietario, privacidad ni seguidores (solo los da la Web API); el
/// id del propietario sí, para que «Tu playlist» y el menú de edición funcionen sin ella.
fn meta_from_playlist4(pl: &librespot_metadata::Playlist, id: &str) -> Playlist {
    let a = &pl.attributes;
    let images = playlist4_images(&a.picture, a.picture_sizes.iter().map(|p| (p.target_name.as_str(), p.url.as_str())));
    // Las playlists algorítmicas de Spotify comparten prefijo de id.
    let spotify_made = id.starts_with("37i9dQZ");
    // librespot guarda el usuario propietario (owner_username) en el uri de la playlist.
    let owner_user = match &pl.id {
        SpotifyUri::Playlist { user: Some(u), .. } if !u.is_empty() => Some(u.clone()),
        _ => None,
    };
    Playlist {
        id: id.to_string(),
        name: a.name.clone(),
        uri: format!("spotify:playlist:{id}"),
        description: Some(a.description.clone()).filter(|d| !d.is_empty()),
        images: Some(images),
        owner: if spotify_made {
            Owner { display_name: Some("Spotify".to_string()), id: Some("spotify".to_string()) }
        } else {
            Owner { display_name: None, id: owner_user }
        },
        tracks: Some(TracksRef { total: pl.length.max(0) as u32 }),
        public: None,
        collaborative: Some(a.is_collaborative),
        followers: None,
        snapshot_id: None,
    }
}

/// Portada según los atributos de playlist4: la subida por el usuario (`picture`, id de
/// imagen) o, en radios y mixes, la generada, que viene en variantes por tamaño (la mayor con
/// URL). Vacía si no hay ninguna: las playlists sin imagen subida no traen su mosaico.
fn playlist4_images<'a>(picture: &[u8], sizes: impl IntoIterator<Item = (&'a str, &'a str)>) -> Vec<Image> {
    if picture.is_empty() {
        sizes
            .into_iter()
            .filter(|(_, url)| !url.is_empty())
            .max_by_key(|(target, _)| match *target {
                "xlarge" => 4,
                "large" => 3,
                "default" => 2,
                _ => 1,
            })
            .map(|(_, url)| vec![Image { url: url.to_string(), width: Some(300), height: Some(300) }])
            .unwrap_or_default()
    } else {
        let hex: String = picture.iter().map(|b| format!("{b:02x}")).collect();
        vec![Image { url: format!("https://i.scdn.co/image/{hex}"), width: Some(300), height: Some(300) }]
    }
}

/// Id de la playlist de una entrada del rootlist: `spotify:playlist:<id>` o la forma antigua
/// `spotify:user:<usuario>:playlist:<id>`. `None` para las marcas de carpeta y lo demás.
fn rootlist_playlist_id(uri: &str) -> Option<&str> {
    let id = uri
        .strip_prefix("spotify:playlist:")
        .or_else(|| uri.strip_prefix("spotify:user:")?.split_once(":playlist:").map(|(_, id)| id))?;
    (!id.is_empty() && !id.contains(':')).then_some(id)
}

/// Marca de inicio o fin de carpeta en el rootlist.
fn rootlist_group(uri: &str) -> bool {
    uri.starts_with("spotify:start-group:") || uri.starts_with("spotify:end-group:")
}

/// Las playlists de la biblioteca a partir del rootlist (`decorate=revision,attributes,length,
/// owner`), en el orden del usuario (el de sus carpetas aplanadas): nombre, descripción,
/// portada subida, tamaño, id del propietario (no su nombre visible), si es colaborativa y la
/// revisión como snapshot_id. Privacidad y seguidores no vienen.
///
/// `meta_items` va por índice junto a `items`: o una entrada por cada una (también las marcas
/// de carpeta) o solo por las que no son marcas. Si las cuentas no casan con ninguna de las dos,
/// no se adivina (cada playlist podría llevarse el nombre de otra): error, y quien llama usa la
/// Web API. Lo mismo si ninguna trae nombre (no vinieron los atributos).
fn library_from_rootlist(contents: &librespot_protocol::playlist4_external::ListItems) -> Result<Vec<Playlist>, String> {
    use librespot_protocol::playlist4_external::{Item, MetaItem};
    let (items, metas) = (&contents.items, &contents.meta_items);
    let groups = items.iter().filter(|i| rootlist_group(i.uri())).count();
    let pairs: Vec<(&Item, &MetaItem)> = if metas.len() == items.len() {
        items.iter().zip(metas.iter()).filter(|(i, _)| !rootlist_group(i.uri())).collect()
    } else if metas.len() == items.len() - groups {
        items.iter().filter(|i| !rootlist_group(i.uri())).zip(metas.iter()).collect()
    } else {
        return Err(format!("rootlist: {} metadatos para {} entradas ({groups} de carpetas)", metas.len(), items.len()));
    };
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(pairs.len());
    for (item, meta) in pairs {
        let Some(id) = rootlist_playlist_id(item.uri()) else { continue };
        if !seen.insert(id) {
            continue;
        }
        let attrs = meta.attributes.as_ref();
        // Sin atributos y con un código de error propio (`decorate=…,status_code`: borrada por
        // su dueño, ya no accesible): Spotify no la enseña y la Web API tampoco la listaba. Sin
        // esto saldría una fila sin nombre que nada completa. Sin código se conserva: el nombre
        // puede llegar del listado anterior o de la Web API.
        if attrs.is_none_or(|a| a.name().is_empty()) && meta.has_status_code() && !matches!(meta.status_code(), 0 | 200) {
            log::debug!("[biblioteca] rootlist: {id} no disponible (código {})", meta.status_code());
            continue;
        }
        let owner = Some(meta.owner_username()).filter(|o| !o.is_empty()).map(str::to_string);
        // Las de Spotify (radios, Descubrimiento semanal…) se ven «De Spotify» sin preguntar.
        let display_name = (owner.as_deref() == Some("spotify")).then(|| "Spotify".to_string());
        let images = attrs
            .map(|a| playlist4_images(a.picture(), a.picture_size.iter().map(|p| (p.target_name(), p.url()))))
            .filter(|v| !v.is_empty());
        out.push(Playlist {
            id: id.to_string(),
            name: attrs.map(|a| a.name().to_string()).unwrap_or_default(),
            uri: format!("spotify:playlist:{id}"),
            description: attrs.map(|a| a.description()).filter(|d| !d.is_empty()).map(str::to_string),
            images,
            owner: Owner { display_name, id: owner },
            tracks: meta.has_length().then(|| TracksRef { total: meta.length().max(0) as u32 }),
            public: None,
            collaborative: attrs.map(|a| a.collaborative()),
            followers: None,
            // La revisión de la playlist en base64, la forma del snapshot_id de la Web API: sirve
            // igual para saber si su copia en disco sigue al día (ver App::list_unchanged).
            snapshot_id: Some(meta.revision())
                .filter(|r| !r.is_empty())
                .map(|r| base64::engine::general_purpose::STANDARD.encode(r)),
        });
    }
    if !out.is_empty() && out.iter().all(|p| p.name.is_empty()) {
        return Err(format!("rootlist: {} playlists sin atributos", out.len()));
    }
    Ok(out)
}

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
        // Solo lo que seguro no puede sonar: sin ficheros ni otra edición que los tenga, o aún sin
        // publicar. Es un aviso (la fila se ve apagada), nunca impide pulsarla: las reglas de país
        // y catálogo las aplica el reproductor, que es quien de verdad sabe.
        is_playable: ((t.files.is_empty() && t.alternatives.is_empty())
            || librespot_core::date::Date::now_utc() < t.earliest_live_timestamp)
            .then_some(false),
        kind: Some("track".to_string()),
        added_by: None,
        added_at: None,
    }
}

/// Lo que pide esperar el limitador propio de librespot («rate limited for at least another N
/// seconds»); cero si el error no lo dice (p. ej. un 429 de Spotify sin Retry-After).
fn limiter_wait(e: &librespot_core::Error) -> Duration {
    let secs = e
        .error
        .to_string()
        .strip_prefix("rate limited for at least another ")
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(0);
    Duration::from_secs(secs)
}

/// Espera una llamada de librespot desde un hilo que no es del runtime, con plazo `d`. Al vencer
/// se suelta el futuro, y con él la petición y los reintentos y esperas de librespot que llevara
/// dentro (hasta 10 intentos de spclient, las esperas por 429). Sin plazo, una conexión colgada
/// tras suspender el equipo o cambiar de wifi bloqueaba el hilo para siempre.
fn block_timeout<T, E: std::fmt::Display>(
    handle: &tokio::runtime::Handle,
    d: Duration,
    f: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, String> {
    // El plazo se crea dentro del runtime: tokio::time::timeout llamado fuera de él (en este
    // hilo, antes del block_on) entra en pánico por no haber temporizador.
    match handle.block_on(async move { tokio::time::timeout(d, f).await }) {
        Ok(r) => r.map_err(|e| e.to_string()),
        Err(_) => Err(timeout_message(d)),
    }
}

fn timeout_message(d: Duration) -> String {
    format!("Spotify no respondió en {} s", d.as_secs())
}

/// Plazo de una petición a spclient según su método: las escrituras con más margen (ver
/// TIMEOUT_SP_WRITE).
fn sp_timeout(method: &http::Method) -> Duration {
    if *method == http::Method::GET {
        TIMEOUT_SP_READ
    } else {
        TIMEOUT_SP_WRITE
    }
}

/// Cabeceras de las peticiones protobuf a spclient.
fn pb_headers() -> http::HeaderMap {
    let mut headers = http::HeaderMap::new();
    headers.insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("application/x-protobuf"));
    headers.insert(http::header::ACCEPT, http::HeaderValue::from_static("application/x-protobuf"));
    headers
}

/// Cuerpo de una petición extended-metadata: estas entidades (uris), todas del mismo tipo.
fn build_batch_body(
    session: &librespot_core::Session,
    keys: &[String],
    kind: librespot_protocol::extension_kind::ExtensionKind,
) -> Result<Vec<u8>, String> {
    use protobuf::Enum;
    build_batch_body_raw(session, keys, kind.value())
}

/// Como `build_batch_body`, con el tipo por número: el enum de librespot 0.8 acaba en la 195 y los
/// de las mezclas (217 en adelante) solo se pueden pedir así (la sonda de mezclas).
fn build_batch_body_raw(session: &librespot_core::Session, keys: &[String], kind: i32) -> Result<Vec<u8>, String> {
    use librespot_protocol::extended_metadata::{BatchedEntityRequest, EntityRequest, ExtensionQuery};
    use protobuf::{EnumOrUnknown, Message};
    let mut req = BatchedEntityRequest::new();
    let header = req.header.mut_or_insert_default();
    header.country = session.country();
    header.catalogue = "premium".to_string();
    for uri in keys {
        let mut q = ExtensionQuery::new();
        q.extension_kind = EnumOrUnknown::from_i32(kind);
        let mut e = EntityRequest::new();
        e.entity_uri = uri.clone();
        e.query.push(q);
        req.entity_request.push(e);
    }
    req.write_to_bytes().map_err(|e| e.to_string())
}

/// Lo que trae una respuesta de extended-metadata, por uri. Lo que no se puede leer se omite
/// (cuenta como que falta). `raw` recibe los bytes de cada entidad leída (los mismos que pediría
/// el reproductor), para sembrarlos en su caché.
fn parse_batch<M: librespot_metadata::Metadata>(
    bytes: &[u8],
    mut raw: impl FnMut(&str, Vec<u8>),
) -> Result<std::collections::HashMap<String, M>, String> {
    use librespot_protocol::extended_metadata::BatchedExtensionResponse;
    use protobuf::Message;
    let resp = BatchedExtensionResponse::parse_from_bytes(bytes).map_err(|e| e.to_string())?;
    let mut found = std::collections::HashMap::new();
    for data in resp.extended_metadata.into_iter().flat_map(|a| a.extension_data.into_iter()) {
        let Some(any) = data.extension_data.0 else { continue };
        let (Ok(msg), Ok(uri)) = (M::Message::parse_from_bytes(&any.value), SpotifyUri::from_uri(&data.entity_uri)) else { continue };
        if let Ok(m) = M::parse(&msg, &uri) {
            raw(&data.entity_uri, any.value);
            found.insert(data.entity_uri, m);
        }
    }
    Ok(found)
}

/// ¿Son metadatos que pide el reproductor al cargar (canciones y episodios)? Los demás (álbumes,
/// artistas) no se siembran.
fn seedable(kind: librespot_protocol::extension_kind::ExtensionKind) -> bool {
    use librespot_protocol::extension_kind::ExtensionKind;
    matches!(kind, ExtensionKind::TRACK_V4 | ExtensionKind::EPISODE_V4)
}

/// La petición de un lote, sin bloquear: se espera dentro de un `block_on` que ya está en
/// marcha. Llamar ahí a spclient_pb (que bloquea) sería un block_on anidado, y tokio aborta.
async fn fetch_batch(session: &librespot_core::Session, body: &[u8]) -> librespot_core::spclient::SpClientResult {
    session
        .spclient()
        .request(&http::Method::POST, "/extended-metadata/v0/extended-metadata", Some(pb_headers()), Some(body))
        .await
}

/// Error de un lote y, si merece otro intento, cuánto esperar antes.
type BatchErr = (String, Option<Duration>);

/// Un lote de metadatos (hasta 500), en el orden pedido: una petición y, para lo que no traiga
/// (pocas: ediciones regionales, retiradas), peticiones sueltas con tope por lote y del
/// presupuesto común `singles`; lo que quede sin pedir se omite, como lo que Spotify no tiene.
/// Si falla la petición del lote no se pide nada suelto: eso decide batch_retry.
///
/// `seed`: las primeras entidades del lote que se siembran en la caché de metadatos del
/// reproductor (`SpClient::seed_metadata`), así la canción en la que se hace clic ya no los pide.
async fn batch_chunk<M: librespot_metadata::Metadata + Clone>(
    session: &librespot_core::Session,
    keys: &[String],
    kind: librespot_protocol::extension_kind::ExtensionKind,
    singles: &std::cell::Cell<usize>,
    seed: usize,
) -> Result<Vec<Option<M>>, BatchErr> {
    use librespot_core::error::ErrorKind;
    let body = build_batch_body(session, keys, kind).map_err(|e| (e, None))?;
    // Plazo por lote, no para la carga entera: los lotes que ya llegaron se quedan. Al vencer
    // no se reintenta (ya pasó TIMEOUT_SP_READ con los reintentos de librespot dentro): falla
    // la carga y quien pidió conserva lo que tenía.
    let fetched = tokio::time::timeout(TIMEOUT_SP_READ, fetch_batch(session, &body))
        .await
        .map_err(|_| (timeout_message(TIMEOUT_SP_READ), None))?;
    let bytes = fetched.map_err(|e| {
        let retry = match e.kind {
            // Caídas y plazos vencidos ya los reintenta librespot (hasta 10 veces): repetirlos
            // aquí solo alargaría la espera.
            ErrorKind::Unavailable | ErrorKind::DeadlineExceeded => None,
            // Su limitador propio dice cuánto falta para poder pedir otra vez.
            ErrorKind::ResourceExhausted => Some(limiter_wait(&e)),
            _ => Some(Duration::ZERO),
        };
        (e.to_string(), retry)
    })?;
    let seed: std::collections::HashSet<&str> = if seedable(kind) {
        keys.iter().take(seed).map(String::as_str).collect()
    } else {
        Default::default()
    };
    let spclient = session.spclient();
    let mut found = parse_batch::<M>(&bytes, |uri, value| {
        if seed.contains(uri) {
            spclient.seed_metadata(kind, uri, value);
        }
    })
    .map_err(|e| (e, Some(Duration::ZERO)))?;
    // Una vez por uri: la misma canción dos veces en una playlist es una sola petición.
    let mut seen = std::collections::HashSet::new();
    let missing: Vec<usize> = (0..keys.len()).filter(|&i| !found.contains_key(&keys[i]) && seen.insert(&keys[i])).collect();
    if !missing.is_empty() {
        // Con tope y pocas a la vez: si faltan muchas, lo que pasa no es una edición regional
        // suelta, y cientos de peticiones solo agotarían el cupo de librespot.
        let n = missing.len().min(SINGLE_GET_MAX).min(singles.get());
        singles.set(singles.get() - n);
        log::info!("metadatos por lotes: {} de {} sin datos; se piden sueltos {n}", missing.len(), keys.len());
        // Puede volver con otro id (Spotify sustituye ediciones): cuenta como la pedida.
        for (i, m) in get_singles::<M>(session, keys, &missing[..n]).await {
            if let Some(m) = m {
                found.insert(keys[i].clone(), m);
            }
        }
    }
    // `get`, no `remove`: una playlist puede tener la misma canción dos veces.
    Ok(keys.iter().map(|k| found.get(k).cloned()).collect())
}

/// Un lote fallido se reintenta UNA vez, tras la espera que pida (en una playlist, los lotes ya
/// en vuelo siguen, pero no salen más de PLAYLIST_BATCH_PARALLEL); si vuelve a fallar, falla
/// todo. Antes se pedían sueltos sus cientos de elementos a la vez: se agotaba el cupo de
/// librespot, la canción que suena no cargaba y aun así se perdían pistas.
async fn batch_retry<M: librespot_metadata::Metadata + Clone>(
    session: &librespot_core::Session,
    keys: &[String],
    kind: librespot_protocol::extension_kind::ExtensionKind,
    singles: &std::cell::Cell<usize>,
    first: Result<Vec<Option<M>>, BatchErr>,
    seed: usize,
) -> Result<Vec<Option<M>>, String> {
    match first {
        Ok(v) => Ok(v),
        Err((e, Some(wait))) => {
            let wait = wait.clamp(Duration::from_millis(400), Duration::from_secs(10));
            log::warn!("metadatos por lotes: {e}; se reintenta en {} ms", wait.as_millis());
            tokio::time::sleep(wait).await;
            batch_chunk(session, keys, kind, singles, seed).await.map_err(|(e, _)| format!("metadatos por lotes: {e}"))
        }
        Err((e, None)) => Err(format!("metadatos por lotes: {e}")),
    }
}

/// Elementos sueltos de `keys` (por posición), pocos a la vez. El índice viaja con cada
/// resultado porque llegan en cualquier orden; el que falla vuelve como `None`. Plazo por
/// elemento (TIMEOUT_ITEM), no para todos juntos, para no perder los que sí llegaron.
async fn get_singles<M: librespot_metadata::Metadata>(
    session: &librespot_core::Session,
    keys: &[String],
    idx: &[usize],
) -> Vec<(usize, Option<M>)> {
    use futures::StreamExt;
    // Si uno no contesta en el plazo, la conexión está colgada: los que aún no salieron no se
    // piden (cada tanda esperaría otro plazo entero) y vuelven como `None`.
    let stalled = &std::cell::Cell::new(false);
    futures::stream::iter(idx.iter().map(|&i| async move {
        if stalled.get() {
            return (i, None);
        }
        let m = match SpotifyUri::from_uri(&keys[i]) {
            Ok(uri) => match tokio::time::timeout(TIMEOUT_ITEM, M::get(session, &uri)).await {
                Ok(r) => r.ok(),
                Err(_) => {
                    stalled.set(true);
                    None
                }
            },
            Err(_) => None,
        };
        (i, m)
    }))
    .buffer_unordered(SINGLE_GET_PARALLEL)
    .collect()
    .await
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

/// Retry-After del 429 simulado: el que da Spotify cuando la cuota compartida está saturada.
const FAKE_429_SECS: u64 = 30;

/// `NANOFY_FAKE_429=web` (pruebas): toda llamada a la Web API falla al momento como cuando la
/// cuota compartida del id de primera parte la han agotado otros clientes (429), sin salir a la
/// red. Comprueba que la biblioteca no depende de ella. Se lee una sola vez.
fn fake_429_web() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let on = fake_429_parse(std::env::var("NANOFY_FAKE_429").ok().as_deref());
        if on {
            log::warn!("[fallo] NANOFY_FAKE_429=web: la Web API responde 429 a todo");
        }
        on
    })
}

/// Lista separada por comas; de momento solo se entiende `web`.
fn fake_429_parse(v: Option<&str>) -> bool {
    v.is_some_and(|v| v.split(',').any(|p| p.trim().eq_ignore_ascii_case("web")))
}

fn cooldown_message(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let when = if h > 0 {
        format!("{h} h {m} min")
    } else {
        format!("{} min", m.max(1))
    };
    // Sin «tu app»: la cuota es la de la conexión de la biblioteca, nadie ha creado ninguna app.
    // El principio («Spotify ha agotado la cuota») lo buscan la interfaz y `Req::PlaylistMeta`.
    format!("Spotify ha agotado la cuota de la biblioteca; vuelve en {when}. La reproducción sigue funcionando.")
}

/// Lectura limitada con los segundos que faltan; `retry_secs` los vuelve a leer para que la
/// búsqueda se reintente sola.
fn throttle_message(secs: u64) -> String {
    format!("Spotify está limitando las peticiones (reintenta en {secs} s)")
}

/// Segundos de espera de un error de `throttle_message` (None si el error es de otro tipo).
pub fn retry_secs(e: &str) -> Option<u64> {
    let rest = e.split("(reintenta en ").nth(1)?;
    rest.split(" s)").next()?.trim().parse().ok()
}

/// Segundos enteros redondeando hacia arriba (mínimo 1): reintentar antes de que pase la espera
/// solo volvería a fallar.
fn ceil_secs(d: Duration) -> u64 {
    (d.as_secs() + u64::from(d.subsec_nanos() > 0)).max(1)
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
    // Metadatos con plazo largo (el hilo es solo de la descarga): uno colgado ya no deja sin
    // descargar el resto de la tanda. Abrir el audio no lo lleva: puede tardar de verdad.
    let files: librespot_metadata::audio::AudioFiles = if episode {
        let uri = SpotifyUri::from_uri(&format!("spotify:episode:{id}")).map_err(|e| e.to_string())?;
        let ep = block_timeout(handle, TIMEOUT_DOWNLOAD_META, librespot_metadata::Episode::get(session, &uri))
            .map_err(|e| format!("episodio: {e}"))?;
        ep.audio.clone()
    } else {
        let uri = SpotifyUri::from_uri(&format!("spotify:track:{id}")).map_err(|e| e.to_string())?;
        let mut track = block_timeout(handle, TIMEOUT_DOWNLOAD_META, librespot_metadata::Track::get(session, &uri))
            .map_err(|e| format!("pista: {e}"))?;
        if track.files.0.is_empty() {
            // Sin archivo en esta región: se prueba la primera alternativa.
            let alt = track.alternatives.first().cloned().ok_or("sin archivo de audio disponible")?;
            track = block_timeout(handle, TIMEOUT_DOWNLOAD_META, librespot_metadata::Track::get(session, &alt))
                .map_err(|e| format!("pista: {e}"))?;
        }
        track.files.clone()
    };
    let track = TrackFiles { files };
    // El mismo orden de formatos que el reproductor: lo descargado es lo que luego suena sin red.
    // Sin preferir lo que ya está en la caché (el reproductor sí): quien descarga en una calidad
    // la quiere, y el reproductor elige ese fichero porque va primero en su orden.
    let order = librespot_playback::player::format_order(quality.bitrate());
    let (_, file_id) = librespot_playback::player::pick_audio_file(order, &track.files, |_| false)
        .ok_or("sin formato de audio compatible")?;
    // Ritmo estimado (bytes/s) para el primer tramo; la descarga sigue entera de todos modos.
    let mut file = handle
        .block_on(librespot_audio::AudioFile::open(session, file_id, 40_000))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Lo que abre una página por el protocolo interno no espera detrás de la Web API limitada;
    /// las lecturas y escrituras de la Web API siguen en los hilos comunes.
    #[test]
    fn carril_interno() {
        for req in [
            Req::PlaylistTracks("p".into()),
            Req::Playlists,
            Req::Album("a".into()),
            Req::RadioPlaylist("t".into()),
            Req::HomeFeed,
            Req::ArtistView("a".into()),
            Req::User("u".into()),
            Req::TrackInfo("t".into()),
        ] {
            assert!(interno(&req), "{req:?}");
        }
        for req in [
            Req::Me,
            Req::Recent,
            Req::LikedRecent,
            Req::PlaylistMeta("p".into()),
            Req::PlaylistsWeb,
            Req::AddToPlaylist { id: "p".into(), uris: vec![] },
            Req::CreatePlaylist { user_id: "u".into(), name: "n".into(), description: String::new(), public: false, collaborative: false },
            Req::Search("q".into()),
            Req::Queue,
        ] {
            assert!(!interno(&req), "{req:?}");
        }
    }

    /// Un lote espera solo si hay una carga en curso que empezó hace menos de 4 s.
    #[test]
    fn espera_de_lotes_acotada() {
        assert!(!playback_gate(0, 0));
        assert!(!playback_gate(0, 123_456));
        assert!(playback_gate(1_000, 1_000));
        assert!(playback_gate(1_000, 1_000 + PLAYBACK_YIELD_MAX_MS - 1));
        // Pasado el tope, la carga ya no frena nada aunque la interfaz no lo haya quitado.
        assert!(!playback_gate(1_000, 1_000 + PLAYBACK_YIELD_MAX_MS));
        assert!(!playback_gate(1_000, 1_000 + 60_000));
        // Una lectura anterior al inicio (no debería darse) cuenta como recién empezada.
        assert!(playback_gate(5, 1));
    }

    /// El aviso de la interfaz: solo el paso a «cargando» fija el inicio, y los lotes en
    /// espera siguen en cuanto se quita. (Una sola prueba: el estado es global.)
    #[test]
    fn aviso_de_carga_y_espera_de_los_lotes() {
        use std::sync::atomic::Ordering::Relaxed;
        set_playback_loading(false);
        assert!(!playback_loading());
        // Sin carga, ceder no espera nada.
        let t0 = Instant::now();
        yield_to_playback_blocking();
        assert!(t0.elapsed() < Duration::from_millis(30));

        set_playback_loading(true);
        let since = PLAYBACK_LOADING_SINCE.load(Relaxed);
        assert_ne!(since, 0);
        assert!(playback_loading());
        std::thread::sleep(Duration::from_millis(5));
        // Las pasadas siguientes de la interfaz no mueven el inicio.
        set_playback_loading(true);
        assert_eq!(PLAYBACK_LOADING_SINCE.load(Relaxed), since);

        // Un lote espera hasta que la canción suena (aquí, 150 ms después).
        let clear = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(150));
            set_playback_loading(false);
        });
        let t1 = Instant::now();
        yield_to_playback_blocking();
        let waited = t1.elapsed();
        clear.join().unwrap();
        assert!(waited >= Duration::from_millis(100), "{waited:?}");
        assert!(waited < Duration::from_millis(1_500), "{waited:?}");
        assert_eq!(PLAYBACK_LOADING_SINCE.load(Relaxed), 0);
        assert!(!playback_loading());

        // La versión asíncrona hace lo mismo dentro de un runtime.
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        set_playback_loading(true);
        let clear = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(100));
            set_playback_loading(false);
        });
        let t2 = Instant::now();
        rt.block_on(yield_to_playback());
        let waited = t2.elapsed();
        clear.join().unwrap();
        assert!(waited >= Duration::from_millis(60), "{waited:?}");
        assert!(waited < Duration::from_millis(1_500), "{waited:?}");
    }

    /// El limitador propio de librespot (300 peticiones cada 30 s por dominio): agotado, una
    /// petición espera su turno (uno cada 100 ms) en vez de fallar al instante; si el turno
    /// llegaría después del plazo, falla ya, sin esperar, con el error que `limiter_wait` lee.
    #[test]
    fn limitador_espera_su_turno() {
        use librespot_core::error::ErrorKind;
        use librespot_core::http_client::{HttpClient, RATE_LIMIT_CALLS_PER_INTERVAL, RATE_LIMIT_QUEUE_WAIT};
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let client = HttpClient::new(None);
        let spclient: http::Uri = "https://spclient.wg.spotify.com/metadata/4/track/x".parse().unwrap();
        // Otro subdominio de spotify.com: comparte el presupuesto.
        let login5: http::Uri = "https://login5.spotify.com/v3/login".parse().unwrap();
        // La CDN es otro dominio, con su propio presupuesto.
        let cdn: http::Uri = "https://audio-fa.scdn.co/audio/x".parse().unwrap();
        rt.block_on(async {
            // La ráfaga entera pasa al momento.
            let t0 = Instant::now();
            for _ in 0..RATE_LIMIT_CALLS_PER_INTERVAL {
                client.acquire_rate_limit(&spclient, Duration::ZERO).await.unwrap();
            }
            assert!(t0.elapsed() < Duration::from_millis(500));

            // Agotado y con menos plazo que el próximo turno: falla sin esperar.
            let t1 = Instant::now();
            let e = client.acquire_rate_limit(&login5, Duration::from_millis(20)).await.unwrap_err();
            assert!(t1.elapsed() < Duration::from_millis(80), "{:?}", t1.elapsed());
            assert_eq!(e.kind, ErrorKind::ResourceExhausted);
            assert!(e.error.to_string().starts_with("rate limited for at least another "), "{e}");
            assert!(limiter_wait(&e) <= Duration::from_secs(1));

            // Con el plazo de `request`, espera su turno y pasa.
            let t2 = Instant::now();
            client.acquire_rate_limit(&spclient, RATE_LIMIT_QUEUE_WAIT).await.unwrap();
            let waited = t2.elapsed();
            assert!(waited >= Duration::from_millis(40), "{waited:?}");
            assert!(waited < Duration::from_millis(1_000), "{waited:?}");

            // Varias a la vez: cada una con su turno, una tras otra, y ninguna falla.
            let t3 = Instant::now();
            let all = futures::future::join_all(
                (0..3).map(|_| client.acquire_rate_limit(&spclient, RATE_LIMIT_QUEUE_WAIT)),
            )
            .await;
            assert!(all.iter().all(|r| r.is_ok()));
            let waited = t3.elapsed();
            assert!(waited >= Duration::from_millis(200), "{waited:?}");
            assert!(waited < Duration::from_millis(2_000), "{waited:?}");

            // La CDN no espera por lo gastado en spotify.com.
            let t4 = Instant::now();
            client.acquire_rate_limit(&cdn, Duration::ZERO).await.unwrap();
            assert!(t4.elapsed() < Duration::from_millis(50));
        });
    }

    mod rootlist {
        use super::super::*;
        use librespot_protocol::playlist4_external::{Item, ListAttributes, ListItems, MetaItem, PictureSize};
        use protobuf::MessageField;

        fn item(uri: &str) -> Item {
            let mut i = Item::new();
            i.set_uri(uri.to_string());
            i
        }

        /// Metadatos de una playlist como los trae `decorate=revision,attributes,length,owner`.
        fn meta(name: &str, length: i32, owner: &str) -> MetaItem {
            let mut a = ListAttributes::new();
            a.set_name(name.to_string());
            let mut m = MetaItem::new();
            m.attributes = MessageField::some(a);
            m.set_length(length);
            m.set_owner_username(owner.to_string());
            m.set_revision(vec![0, 0, 0, 2, 1, 2, 3]);
            m
        }

        /// Lo que viene en una marca de carpeta, si viene algo: nada.
        fn blank() -> MetaItem {
            MetaItem::new()
        }

        fn list(uris: &[&str], metas: Vec<MetaItem>) -> ListItems {
            let mut l = ListItems::new();
            l.items = uris.iter().map(|u| item(u)).collect();
            l.meta_items = metas;
            l
        }

        const URIS: [&str; 6] = [
            "spotify:playlist:aaa",
            "spotify:start-group:f1:Mis%20cosas",
            "spotify:playlist:bbb",
            "spotify:playlist:ccc",
            "spotify:end-group:f1",
            "spotify:playlist:ddd",
        ];

        fn names(v: &[Playlist]) -> Vec<(&str, &str)> {
            v.iter().map(|p| (p.id.as_str(), p.name.as_str())).collect()
        }

        /// Con un metadato por entrada (también las marcas de carpeta), por índice; el orden es
        /// el del rootlist, con las carpetas aplanadas.
        #[test]
        fn una_meta_por_entrada() {
            let metas = vec![meta("A", 3, "yo"), blank(), meta("B", 10, "otra"), meta("C", 0, "yo"), blank(), meta("D", 7, "spotify")];
            let out = library_from_rootlist(&list(&URIS, metas)).unwrap();
            assert_eq!(names(&out), [("aaa", "A"), ("bbb", "B"), ("ccc", "C"), ("ddd", "D")]);
            assert_eq!(out[1].tracks.as_ref().map(|t| t.total), Some(10));
            assert_eq!(out[2].tracks.as_ref().map(|t| t.total), Some(0));
            assert_eq!(out[0].uri, "spotify:playlist:aaa");
            // El propietario es su id de usuario; el nombre visible solo se sabe de Spotify.
            assert_eq!(out[0].owner.id.as_deref(), Some("yo"));
            assert_eq!(out[0].owner.display_name, None);
            assert_eq!(out[3].owner.display_name.as_deref(), Some("Spotify"));
            // La privacidad no viene en el rootlist: desconocida, no «pública».
            assert!(out.iter().all(|p| p.public.is_none()));
            assert_eq!(out[0].collaborative, Some(false));
        }

        /// Con metadatos solo de las playlists (sin las marcas), saltándose las marcas.
        #[test]
        fn metas_sin_las_marcas_de_carpeta() {
            let metas = vec![meta("A", 3, "yo"), meta("B", 10, "otra"), meta("C", 0, "yo"), meta("D", 7, "yo")];
            let out = library_from_rootlist(&list(&URIS, metas)).unwrap();
            assert_eq!(names(&out), [("aaa", "A"), ("bbb", "B"), ("ccc", "C"), ("ddd", "D")]);
            assert_eq!(out[1].tracks.as_ref().map(|t| t.total), Some(10));
        }

        /// Si las cuentas no casan no se adivina: cada playlist podría llevarse los datos de otra.
        #[test]
        fn cuentas_que_no_casan() {
            let metas = vec![meta("A", 3, "yo"), meta("B", 10, "otra")];
            assert!(library_from_rootlist(&list(&URIS, metas)).is_err());
            // Sin metadatos (no se atendió `decorate`): tampoco.
            assert!(library_from_rootlist(&list(&URIS, Vec::new())).is_err());
            // Ni con metadatos vacíos para todas.
            let metas = (0..URIS.len()).map(|_| blank()).collect();
            assert!(library_from_rootlist(&list(&URIS, metas)).is_err());
        }

        /// Biblioteca vacía, o solo con carpetas vacías: lista vacía, no un error.
        #[test]
        fn biblioteca_vacia() {
            assert!(library_from_rootlist(&list(&[], Vec::new())).unwrap().is_empty());
            let only_folder = ["spotify:start-group:f1:x", "spotify:end-group:f1"];
            assert!(library_from_rootlist(&list(&only_folder, Vec::new())).unwrap().is_empty());
        }

        /// Forma antigua del uri, repetidas y entradas que no son playlists.
        #[test]
        fn uris_antiguos_repetidas_y_otras() {
            let uris = ["spotify:user:pepe:playlist:aaa", "spotify:playlist:aaa", "spotify:artist:xyz", "spotify:playlist:bbb"];
            let metas = vec![meta("A", 1, "pepe"), meta("A bis", 1, "pepe"), blank(), meta("B", 2, "yo")];
            let out = library_from_rootlist(&list(&uris, metas)).unwrap();
            assert_eq!(names(&out), [("aaa", "A"), ("bbb", "B")]);
        }

        /// Las que ya no existen (sin atributos y con código de error) no salen como filas sin
        /// nombre; sin código, o con atributos, sí.
        #[test]
        fn no_disponibles_fuera() {
            let uris = ["spotify:playlist:a", "spotify:playlist:gone", "spotify:playlist:bare", "spotify:playlist:ok"];
            let mut gone = blank();
            gone.set_status_code(404);
            let mut ok = meta("OK", 2, "yo");
            ok.set_status_code(200);
            let out = library_from_rootlist(&list(&uris, vec![meta("A", 1, "yo"), gone, blank(), ok])).unwrap();
            assert_eq!(names(&out), [("a", "A"), ("bare", ""), ("ok", "OK")]);
        }

        /// Portada: la subida (id de imagen en hex), la generada por tamaños (la mayor) o ninguna.
        #[test]
        fn portadas() {
            let uris = ["spotify:playlist:a", "spotify:playlist:b", "spotify:playlist:c"];
            let mut with_pic = meta("A", 1, "yo");
            with_pic.attributes.mut_or_insert_default().set_picture(vec![0xab, 0x01, 0xff]);
            let mut with_sizes = meta("B", 1, "spotify");
            for (target, url) in [("default", "https://x/default"), ("xlarge", "https://x/xlarge"), ("large", "https://x/large"), ("small", "")] {
                let mut s = PictureSize::new();
                s.set_target_name(target.to_string());
                s.set_url(url.to_string());
                with_sizes.attributes.mut_or_insert_default().picture_size.push(s);
            }
            let out = library_from_rootlist(&list(&uris, vec![with_pic, with_sizes, meta("C", 1, "yo")])).unwrap();
            assert_eq!(out[0].cover(300), Some("https://i.scdn.co/image/ab01ff"));
            assert_eq!(out[1].cover(300), Some("https://x/xlarge"));
            // Sin imagen subida: sin portada (la pone el listado anterior, la Web API o el mosaico).
            assert!(out[2].images.is_none());
            // La mayor variante con URL, aunque la de más rango venga vacía.
            let sizes = [("xlarge", ""), ("large", "https://x/l"), ("default", "https://x/d")];
            assert_eq!(playlist4_images(&[], sizes)[0].url, "https://x/l");
            assert!(playlist4_images(&[], []).is_empty());
        }

        /// La revisión en base64, como el snapshot_id de la Web API; sin revisión, ninguno.
        #[test]
        fn revision_como_snapshot_id() {
            let uris = ["spotify:playlist:a", "spotify:playlist:b"];
            let mut no_rev = meta("B", 1, "yo");
            no_rev.clear_revision();
            let out = library_from_rootlist(&list(&uris, vec![meta("A", 1, "yo"), no_rev])).unwrap();
            assert_eq!(out[0].snapshot_id.as_deref(), Some("AAAAAgECAw=="));
            assert_eq!(out[1].snapshot_id, None);
        }

        #[test]
        fn ids_del_rootlist() {
            assert_eq!(rootlist_playlist_id("spotify:playlist:37i9dQZF1"), Some("37i9dQZF1"));
            assert_eq!(rootlist_playlist_id("spotify:user:pepe:playlist:abc"), Some("abc"));
            assert_eq!(rootlist_playlist_id("spotify:start-group:abc:Nombre"), None);
            assert_eq!(rootlist_playlist_id("spotify:end-group:abc"), None);
            assert_eq!(rootlist_playlist_id("spotify:playlist:"), None);
            assert_eq!(rootlist_playlist_id("spotify:user:pepe:collection"), None);
            assert!(rootlist_group("spotify:start-group:abc:x") && rootlist_group("spotify:end-group:abc"));
            assert!(!rootlist_group("spotify:playlist:abc"));
        }

        #[test]
        fn variable_del_429_simulado() {
            assert!(fake_429_parse(Some("web")));
            assert!(fake_429_parse(Some(" pathfinder , WEB ")));
            assert!(!fake_429_parse(Some("pathfinder")));
            assert!(!fake_429_parse(Some("")));
            assert!(!fake_429_parse(None));
        }
    }
}
