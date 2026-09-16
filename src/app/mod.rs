//! Interfaz: estado de la aplicación, mensajes, acciones, atajos y composición de la ventana.
//!
//! egui solo repinta cuando hay entrada del usuario o cuando un hilo de fondo envía un
//! mensaje; mientras suena música se repinta 2 veces por segundo para mover el progreso
//! (y más a menudo si el panel de letras está abierto, para resaltar la línea actual).

mod artist;
pub mod control;
mod icons;
mod pages;
mod panels;
mod player_bar;
mod theme;
mod widgets;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use egui::{CornerRadius, Frame, Key, Margin, Modifiers};
use souvlaki::{MediaControlEvent, SeekDirection};

use crate::api::{Api, ApiResult, Req, Resp};
use crate::backend::{Backend, Cmd, Event};
use crate::bus::{Msg, UiTx};
use crate::cache::{PlayLog, Snapshot};
use crate::config::{vol_pct_to_raw, vol_raw_to_pct, Paths, Settings};
use crate::images::Images;
use crate::media::Media;
use crate::model::*;
use crate::shell::{NativeHandles, FPS_CAP_KEY, FRAME_MS_KEY, FRAME_PHASES_KEY};
use crate::update::{UpdateInfo, UpdateResult};
use crate::webauth::WebAuth;

pub use theme::GREEN;
pub const ERROR_RED: egui::Color32 = theme::RED;
pub const ROW_H: f32 = 56.0;
pub const SIDEBAR_W: f32 = 236.0;
pub const LIKED: &str = "liked";
pub const SEARCH_ID: &str = "nanofy_search_box";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Page {
    Home,
    Library,
    Show(String),
    Saves,
    Shows,
    Audiobooks,
    Folders,

    History,
    Search,
    Liked,
    Albums,
    Artists,
    Playlist(String),
    Album(String),
    Artist(String),
    User(String),
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PlayState {
    #[default]
    Stopped,
    Loading,
    Playing,
    Paused,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum Repeat {
    #[default]
    Off,
    Context,
    Track,
}

/// Una pestaña de contenido con su propio historial.
#[derive(Clone, Debug)]
pub struct Tab {
    pub history: Vec<Page>,
    pub idx: usize,
}

impl Tab {
    pub fn page(&self) -> &Page {
        &self.history[self.idx]
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ActiveTab {
    Home,
    Search,
    Tab(usize),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SideTab {
    Queue,
    Lyrics,
}

/// Clave de la caché de secciones del inicio (ver `home_sections`).
pub type HomeCacheKey = (usize, usize, usize, Vec<String>, usize, Vec<String>, usize, Vec<String>);

/// Diálogo «Añadir a una playlist».
#[derive(Default)]
pub struct AddDialog {
    pub uris: Vec<String>,
    pub query: String,
    pub selected: HashSet<String>,
    /// Nombre en edición de la playlist nueva (fila «+ Nueva playlist» abierta).
    pub new_name: Option<String>,
    pub focus: bool,
    pub open_folders: HashSet<String>,
    /// Playlist recién pedida: al crearse se marca seleccionada.
    pub pending_new: Option<String>,
}

#[derive(Default)]
pub struct PlayerState {
    pub now: Option<NowPlaying>,
    pub state: PlayState,
    pub position_ms: u32,
    pub position_at: Option<Instant>,
    pub volume: u16,
    pub shuffle: bool,
    pub repeat: Repeat,
    /// `Some` cuando la reproducción suena en otro dispositivo (se controla por Web API).
    pub remote: Option<Device>,
    pub liked: Option<bool>,
}

impl PlayerState {
    pub fn position(&self) -> u32 {
        let dur = self.now.as_ref().map(|n| n.duration_ms).unwrap_or(0);
        let p = match (self.state, self.position_at) {
            (PlayState::Playing, Some(t)) => self
                .position_ms
                .saturating_add(t.elapsed().as_millis() as u32),
            _ => self.position_ms,
        };
        if dur > 0 {
            p.min(dur)
        } else {
            p
        }
    }
}

#[derive(Default)]
pub struct TrackList {
    pub tracks: Vec<Track>,
    pub total: u32,
    pub loading: bool,
}

#[derive(Default)]
pub struct ArtistPage {
    pub artist: Option<Artist>,
    pub top: Vec<Track>,
    pub albums: Vec<AlbumRef>,
}

/// Lo que sonaba al cerrar: se guarda en `playback.json` y se restaura (en pausa, en el mismo
/// segundo) al volver a abrir, como hace Spotify.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct SavedPlayback {
    pub now: NowPlaying,
    pub position_ms: u32,
    pub target: PlayTarget,
    pub shuffle: bool,
    pub repeat: Repeat,
    #[serde(default)]
    pub queued: Vec<String>,
    /// Cola tal como se veía al cerrar: se muestra al instante al reabrir, sin esperar al servidor.
    #[serde(default)]
    pub queue: Vec<Track>,
    #[serde(default)]
    pub saved_at: u64,
}

#[derive(Clone)]
pub enum Auth {
    LoggedOut,
    LoggingIn,
    /// Hay credenciales guardadas y el backend está conectando: la interfaz se pinta ya como
    /// con sesión iniciada (con la instantánea) y la red llega en cuanto conecta.
    Connecting { username: String },
    LoggedIn { username: String },
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub enum PlayTarget {
    Context {
        uri: String,
        track_uri: Option<String>,
        index: Option<u32>,
        shuffle: bool,
    },
    Tracks {
        uris: Vec<String>,
        index: Option<u32>,
        shuffle: bool,
    },
}

pub enum Action {
    SaveAlbum(String, bool),
    Download(Vec<String>),
    /// Abre la radio de una canción en una pestaña nueva (sin reproducir).
    OpenRadio(String),
    DownloadEpisodes(Vec<String>),
    FollowShow(String, bool),
    SaveEpisode(String, bool),
    Go(Page),
    OpenPlaylist(Playlist),
    Play(PlayTarget),
    Like(String, bool),
    CopyText(String, &'static str),
    AddToQueue(String),
    AddToPlaylist { playlist_id: String, uri: String },
    RemoveFromPlaylist { playlist_id: String, uri: String },
    Follow { kind: &'static str, id: String, on: bool },
    FollowPlaylist(String, bool),
    OpenEditor(Option<Playlist>),
    PickPlaylistImage(String),
    OpenLink(String),
    Pin(String, bool),
    /// Abre la página en una pestaña nueva, sin cambiar de pestaña.
    OpenInTab(Page),
    CloseTab(usize),
    ActivateTab(usize),
}

/// Estado del diálogo de crear / editar playlist.
/// Diálogo de carpeta: nueva (`id` None) o renombrar. `playlist` se mete en la carpeta nueva.
pub struct FolderDialog {
    pub id: Option<String>,
    pub name: String,
    pub playlist: Option<String>,
    pub busy: bool,
}

pub struct PlaylistEditor {
    pub id: Option<String>,
    pub name: String,
    pub description: String,
    pub public: bool,
    pub collaborative: bool,
    pub image_path: Option<PathBuf>,
    pub busy: bool,
}

pub struct App {
    pub paths: Paths,
    pub settings: Settings,
    pub draft: Settings,
    pub backend: Backend,
    pub api: Api,
    pub images: Images,
    pub media: Media,
    pub hwnd: Option<*mut std::ffi::c_void>,
    rx: mpsc::Receiver<Msg>,
    ui_tx: UiTx,

    pub auth: Auth,
    pub user: Option<User>,
    pub device_id: String,
    pub status: Option<(String, Instant, bool)>,
    /// Versión nueva publicada en GitHub y si su aviso flotante está a la vista.
    pub update: Option<UpdateInfo>,
    pub update_banner: bool,
    /// Comprobación en curso y último resultado en texto (para Ajustes; `true` = error).
    pub update_busy: bool,
    pub update_note: Option<(String, bool)>,
    /// Próxima comprobación automática (al arrancar y cada 6 h).
    update_check_at: Option<Instant>,

    pub playlists: Vec<Playlist>,
    pub playlists_loaded: bool,
    pub playlist_meta: HashMap<String, Playlist>,
    pub lists: HashMap<String, TrackList>,
    pub saved_albums: Vec<Album>,
    pub followed_artists: Vec<Artist>,
    pub artists_loaded: bool,
    pub albums: HashMap<String, Album>,
    pub artists: HashMap<String, ArtistPage>,
    pub users: HashMap<String, UserProfile>,
    pub artist_views: HashMap<String, ArtistView>,
    pub artist_playlists: HashMap<String, Vec<Playlist>>,
    /// Radio pedida por bandera de arranque (se abre al iniciar sesión).
    pending_radio: Option<String>,
    /// Última orden de reproducción (para reconstruir la cola) y canciones añadidas a la cola desde Nanofy.
    pub last_play: Option<PlayTarget>,
    /// Página en la que se pulsó reproducir (para «Siguientes de: …» cuando no hay contexto).
    pub last_play_page: Option<Page>,
    playback_path: PathBuf,
    /// Reproducción guardada pendiente de cargar en el reproductor (tras conectar).
    restore_pending: Option<SavedPlayback>,
    /// Al abrir: pedir a Spotify la última sesión de la cuenta (una vez, si nada suena fuera).
    restore_wanted: bool,
    /// Se ha pedido la sesión a Spotify y aún no ha contestado.
    restore_awaiting: bool,
    /// La sesión restaurada llegó «reproduciendo»: se pausa en cuanto empiece (nunca se
    /// reproduce solo al abrir).
    pause_after_restore: bool,
    /// El usuario pulsó play mientras aún se decidía qué restaurar: se reanuda en cuanto cargue
    /// lo restaurado (desde su posición), en vez de empezar la pista desde cero.
    play_after_restore: bool,
    /// La barra muestra la última canción escuchada (de la instantánea) mientras llega la
    /// sesión real; si se pulsa play antes, se reproduce esa canción.
    now_placeholder: bool,
    /// Tras restaurar la sesión, la cola se pide con reintentos cortos hasta que Spotify la
    /// tenga lista (siguiente intento, intentos hechos).
    queue_retry: Option<(Instant, u8)>,
    /// Marca de tiempo pendiente: la primera pista tras restaurar la sesión.
    restore_mark: bool,
    /// Si Spotify aceptó la transferencia pero no llega ninguna pista (sesión sin contexto
    /// resoluble), a esta hora se usa la copia local.
    restore_fallback_at: Option<Instant>,
    /// Prueba (`--page loadctx:<uri>`): contexto que se carga en pausa al iniciar sesión.
    pending_loadctx: Option<String>,
    /// Contexto (id, pista actual) que se está restaurando: al llegar sus pistas se arma la cola.
    restore_ctx: Option<(String, String)>,
    /// Cuándo reintentar pedir las pistas del contexto (hasta que la sesión esté lista).
    restore_ctx_at: Option<Instant>,
    /// Playlists cuyas pistas se precargan en segundo plano (para que abrirlas sea instantáneo).
    prefetch_ids: std::collections::VecDeque<String>,
    prefetch_at: Option<Instant>,
    /// Ya se restauró (o descartó) la sesión con el estado del clúster en este arranque.
    cluster_restored: bool,
    /// Última actividad del servidor: `None` = aún no consultada; `Some(None)` = sin historial.
    server_last: Option<Option<ServerLast>>,
    /// Estado actual del reproductor en la cuenta (/me/player): fuente de verdad entre
    /// dispositivos. `Some(None)` = ya respondió sin nada; `None` = aún no ha respondido.
    server_now: Option<Option<crate::model::PlaybackState>>,
    /// Estado del clúster de Connect al conectar (lo último que quedó en cualquier dispositivo).
    server_cluster: Option<Option<crate::backend::ClusterInfo>>,
    /// Ya se decidió qué restaurar (local o servidor) en este arranque.
    restore_decided: bool,
    /// Se ha recibido al menos un estado del reproductor (o su 204) en este arranque.
    player_state_seen: bool,
    /// Si el sondeo remoto no responde, se restaura igualmente al vencer este plazo.
    restore_deadline: Option<Instant>,
    playback_saved_at: Instant,
    /// Cola de muestra (capturas): no se sobrescribe con la real.
    queue_frozen: bool,
    /// En una Jam (participante): la cola mostrada es la compartida de la sesión, no la de la
    /// cuenta; mientras esté activo se ignora la cola del Web API.
    jam_queue_active: bool,
    /// Última petición del historial a Spotify.
    pub recent_at: Instant,
    /// Historial fusionado (clave de invalidación, lista).
    pub history_cache: Option<((usize, u64, usize, String), Vec<Track>)>,
    /// Las filas muestran «Añadida por …» (playlists colaborativas).
    pub rows_added_by: bool,
    pub queued_local: Vec<String>,
    /// Tras vaciar la cola: (cuándo, posición a restaurar, canciones a volver a añadir).
    pending_queue: Option<(Instant, u32, Vec<String>)>,
    pub shows: HashMap<String, (Show, Vec<Episode>)>,
    pub play_log: PlayLog,
    play_log_path: PathBuf,
    play_log_dirty: bool,
    pub artist_search: String,
    pub artist_search_open: bool,
    pub artist_search_focus: bool,
    pub expanded_albums: HashSet<String>,
    /// Pistas descargadas a la caché de audio (ids), persistidas en downloads.json.
    pub downloaded: HashSet<String>,
    pub downloading: HashSet<String>,
    pending_downloads: Vec<String>,
    pending_probes: Vec<String>,
    /// Peticiones de prueba (`--page fx:…`) que se lanzan al iniciar sesión.
    pending_fx: Vec<Req>,
    pending_editor: Option<String>,
    /// Colaboradores conocidos por playlist (usuario, es_colaborador).
    pub members: HashMap<String, Vec<(String, bool)>>,
    pub folder_dialog: Option<FolderDialog>,
    /// Enlaces de invitación ya generados por playlist.
    pub invite_links: HashMap<String, String>,
    downloads_path: PathBuf,
    /// Selección múltiple de pistas (uris) y lista a la que pertenece.
    pub sel: HashSet<String>,
    pub sel_list: String,
    pub album_search: String,
    pub album_search_open: bool,
    pub album_search_focus: bool,
    /// Lista a la que pertenece la búsqueda interna (se reinicia al cambiar de página).
    pub list_search_id: String,
    pub followed_shows: HashSet<String>,
    pub saved_episodes: HashSet<String>,
    pub followed_shows_list: Vec<Show>,
    pub add_dialog: Option<AddDialog>,
    pub saved_episode_list: Vec<SavedEpisode>,
    pub audiobooks: Vec<Audiobook>,
    pub folders: Vec<Folder>,
    /// Página de podcast: orden (nuevos primero) y filtro (0 todos, 1 descargados, 2 guardados).
    pub show_sort_newest: bool,
    pub show_filter: u8,
    /// Feed de inicio (cacheado en home.json) y desplazamientos de sus estanterías.
    pub home_feed: Vec<HomeSection>,
    home_feed_path: PathBuf,
    pub home_scroll: HashMap<String, f32>,
    pub home_offsets: HashMap<String, (f32, f32)>,
    /// Sección que se está arrastrando en el panel de personalizar el inicio.
    pub home_drag: Option<String>,
    /// Secciones del inicio ya ordenadas (clave de invalidación, lista).
    pub home_cache: Option<(HomeCacheKey, Vec<HomeSection>)>,
    pub home_customize_once: bool,
    /// Instancia de prueba: no guarda ajustes al salir.
    pub ephemeral: bool,
    /// Botones de la miniatura de la barra de tareas (Windows): instalación diferida y estado.
    taskbar_at: Instant,
    taskbar_ready: bool,
    taskbar_playing: Option<bool>,
    ws_trimmed: bool,
    /// Próximo recorte del working set (tras cambiar de pista).
    ws_trim_at: Option<Instant>,
    /// Canciones ocultas (ids): atenuadas en las listas y saltadas al reproducir.
    pub hidden_tracks: HashSet<String>,
    hidden_path: PathBuf,
    last_skipped: Option<String>,
    /// Temporizador de apagado: instante de pausa o "al terminar la canción".
    pub sleep_at: Option<Instant>,
    /// Desde cuándo el reproductor está en «cargando» (vigilante de reproducción estancada).
    loading_since: Option<Instant>,
    stall_test: Option<Instant>,
    pub sleep_end_of_track: bool,
    pause_on_play: bool,
    pub miniplayer: bool,
    miniplayer_prev: Option<egui::Vec2>,
    pub fullscreen: bool,
    pub user_playlists: HashMap<String, Vec<Playlist>>,
    pub following: HashMap<String, bool>,
    pub recent: Vec<Track>,
    pub requested: HashSet<String>,

    pub search_query: String,
    pub search_result: Option<SearchResult>,
    pub search_loading: bool,
    pub focus_search: bool,

    /// Pestañas de contenido (Inicio y Buscar son fijas y no están aquí).
    pub tabs: Vec<Tab>,
    pub active: ActiveTab,

    pub player: PlayerState,
    pub devices: Vec<Device>,
    pub liked_set: HashSet<String>,
    pub seek_drag: Option<u32>,
    pub volume_drag: Option<u16>,
    pub prev_volume: u16,
    pub last_remote_poll: Instant,
    pub last_volume_sent: Instant,
    /// Último volumen enviado a Spotify y cuándo: los ecos que lleguen con otro valor
    /// durante un rato son respuestas atrasadas y se ignoran.
    volume_sent: Option<(u16, Instant)>,
    /// Volumen pendiente de enviar (los cambios se agrupan: como mucho uno cada 200 ms).
    volume_queued: Option<u16>,
    /// Acumulador de la rueda del ratón sobre el volumen (en puntos).
    volume_wheel: f32,
    pub selected: Option<(String, usize)>,

    pub side: Option<SideTab>,
    pub queue: Option<QueueResponse>,
    pub queue_at: Instant,
    pub lyrics: Option<Lyrics>,
    pub lyrics_for: Option<String>,
    pub lyrics_loading: bool,
    /// Última línea de letra a la que se desplazó el panel (pista, índice): el desplazamiento
    /// animado se pide una vez por cambio de línea, no en cada fotograma.
    pub lyrics_scrolled: Option<(String, usize)>,

    pub jam_open: bool,
    pub jam: Option<JamSession>,
    pub jam_link: String,
    pub jam_busy: bool,
    pub jam_at: Instant,
    pub jam_error: Option<String>,

    pub editor: Option<PlaylistEditor>,
    pub show_shortcuts: bool,

    pub sidebar_pins_open: bool,
    pub sidebar_playlists_open: bool,
    pub library_grid: bool,
    pub library_sort_name: bool,
    pub library_filter: String,
    pub home_filter: u8,
    pub artist_tab: u8,
    pub artist_grid: bool,
    pub search_filter: u8,

    #[cfg(not(windows))]
    pub sys: sysinfo::System,
    pub mem_mb: Option<f64>,
    pub mem_at: Instant,
    pub frame_ms: f32,
    /// Últimos fotogramas (ms) para medir coste medio y máximo.
    pub frame_hist: std::collections::VecDeque<f32>,
    /// Suma de fases [ui, teselado, raster, presentación] de los fotogramas de `frame_hist`.
    pub frame_phases: [f32; 4],
    pub web_busy: bool,
    media_dirty: bool,
    /// Instantánea de la biblioteca en disco.
    snapshot_path: PathBuf,
    snapshot_dirty: bool,
    snapshot_saved_at: Instant,
    snapshot_age: Option<u64>,
    /// Un `Req::Liked` completo está en curso: la primera página sustituye la lista cacheada.
    liked_refresh_pending: bool,
    /// Páginas de la recarga completa de Me gusta; sustituyen la lista solo al terminar.
    liked_reload: Vec<Track>,
    liked_reload_ids: HashSet<String>,
    media_ready: bool,
    shutdown_done: bool,
    /// Modo de diagnóstico (`--diag`): ejecuta pruebas y cierra.
    pub diag: bool,
    diag_started: Option<Instant>,
    diag_step: u8,

    pub actions: Vec<Action>,
}

impl App {
    pub fn new(
        ctx: &egui::Context,
        handles: NativeHandles,
        paths: Paths,
        settings: Settings,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let ui_tx = UiTx::new(tx, ctx.clone());
        crate::tmark("App::new");
        let backend = Backend::start(paths.clone(), settings.clone(), ui_tx.clone());
        crate::tmark("backend");
        let web = std::sync::Arc::new(WebAuth::load(paths.webauth_file()));
        let api = Api::start(
            backend.shared.clone(),
            backend.handle.clone(),
            web,
            ui_tx.clone(),
        );
        crate::tmark("api");
        let images = Images::start(paths.image_cache_dir(), ui_tx.clone());
        crate::tmark("imágenes");
        // Las teclas multimedia (SMTC/MPRIS) se registran en la primera reproducción.
        let media = Media::disabled();
        let snapshot_path = paths.state_dir.join("library.json");
        let playback_path = paths.state_dir.join("playback.json");
        let device_id0 = settings.device_id.clone();
        let snapshot = Snapshot::load(&snapshot_path);
        crate::tmark("instantánea");
        let downloads_path = paths.state_dir.join("downloads.json");
        let downloaded: HashSet<String> = std::fs::read_to_string(&downloads_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let hidden_path = paths.state_dir.join("hidden.json");
        let hidden_tracks: HashSet<String> = std::fs::read_to_string(&hidden_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let home_feed_path = paths.state_dir.join("home.json");
        let home_feed: Vec<HomeSection> = std::fs::read_to_string(&home_feed_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let play_log_path = paths.state_dir.join("plays.json");
        let play_log = PlayLog::load(&play_log_path);
        crate::tmark("home+plays");
        let volume = vol_pct_to_raw(settings.volume as f32);
        let side = settings.lyrics_open.then_some(SideTab::Lyrics);
        let settings_library_grid = settings.library_grid;
        // La consulta de versiones espera unos segundos para no competir con el arranque.
        let update_check_at = settings.update_check.then(|| Instant::now() + Duration::from_secs(5));
        crate::tmark("antes de fuentes");
        crate::fonts::install_system_fonts(ctx);
        crate::tmark("fuentes");

        let mut app = Self {
            draft: settings.clone(),
            paths,
            settings,
            backend,
            api,
            images,
            media,
            hwnd: handles.hwnd,
            rx,
            ui_tx,
            auth: Auth::LoggedOut,
            user: None,
            device_id: device_id0,
            status: None,
            update: None,
            update_banner: false,
            update_busy: false,
            update_note: None,
            update_check_at,
            playlists: Vec::new(),
            playlists_loaded: false,
            playlist_meta: HashMap::new(),
            lists: HashMap::new(),
            saved_albums: Vec::new(),
            followed_artists: Vec::new(),
            artists_loaded: false,
            albums: HashMap::new(),
            artists: HashMap::new(),
            users: HashMap::new(),
            artist_views: HashMap::new(),
            artist_playlists: HashMap::new(),
            pending_radio: None,
            last_play: None,
            last_play_page: None,
            playback_path,
            restore_pending: None,
            restore_wanted: true,
            restore_awaiting: false,
            pause_after_restore: false,
            play_after_restore: false,
            now_placeholder: false,
            queue_retry: None,
            restore_mark: false,
            restore_fallback_at: None,
            pending_loadctx: None,
            restore_ctx: None,
            restore_ctx_at: None,
            prefetch_ids: std::collections::VecDeque::new(),
            prefetch_at: None,
            cluster_restored: false,
            server_last: None,
            server_now: None,
            server_cluster: None,
            restore_decided: false,
            player_state_seen: false,
            restore_deadline: None,
            playback_saved_at: Instant::now(),
            queue_frozen: false,
            jam_queue_active: false,
            recent_at: Instant::now(),
            history_cache: None,
            rows_added_by: false,
            queued_local: Vec::new(),
            pending_queue: None,
            shows: HashMap::new(),
            play_log,
            play_log_path,
            play_log_dirty: false,
            artist_search: String::new(),
            artist_search_open: false,
            artist_search_focus: false,
            expanded_albums: HashSet::new(),
            downloaded,
            downloading: HashSet::new(),
            pending_downloads: Vec::new(),
            pending_probes: Vec::new(),
            pending_fx: Vec::new(),
            pending_editor: None,
            members: HashMap::new(),
            folder_dialog: None,
            invite_links: HashMap::new(),
            downloads_path,
            sel: HashSet::new(),
            sel_list: String::new(),
            album_search: String::new(),
            album_search_open: false,
            album_search_focus: false,
            list_search_id: String::new(),
            followed_shows: HashSet::new(),
            saved_episodes: HashSet::new(),
            followed_shows_list: Vec::new(),
            add_dialog: None,
            saved_episode_list: Vec::new(),
            audiobooks: Vec::new(),
            folders: Vec::new(),
            show_sort_newest: true,
            show_filter: 0,
            home_feed,
            home_feed_path,
            home_scroll: HashMap::new(),
            home_offsets: HashMap::new(),
            home_drag: None,
            home_cache: None,
            home_customize_once: false,
            ephemeral: false,
            taskbar_at: Instant::now(),
            taskbar_ready: false,
            taskbar_playing: None,
            ws_trimmed: false,
            ws_trim_at: None,
            hidden_tracks,
            hidden_path,
            last_skipped: None,
            sleep_at: None,
            loading_since: None,
            stall_test: None,
            sleep_end_of_track: false,
            pause_on_play: false,
            miniplayer: false,
            miniplayer_prev: None,
            fullscreen: false,
            user_playlists: HashMap::new(),
            following: HashMap::new(),
            recent: Vec::new(),
            requested: HashSet::new(),
            search_query: String::new(),
            search_result: None,
            search_loading: false,
            focus_search: false,
            tabs: Vec::new(),
            active: ActiveTab::Home,
            player: PlayerState {
                volume,
                ..Default::default()
            },
            devices: Vec::new(),
            liked_set: HashSet::new(),
            seek_drag: None,
            volume_drag: None,
            prev_volume: volume,
            last_remote_poll: Instant::now(),
            last_volume_sent: Instant::now(),
            volume_sent: None,
            volume_queued: None,
            volume_wheel: 0.0,
            selected: None,
            side,
            queue: None,
            queue_at: Instant::now() - Duration::from_secs(60),
            lyrics: None,
            lyrics_for: None,
            lyrics_loading: false,
            lyrics_scrolled: None,
            jam_open: false,
            jam: None,
            jam_link: String::new(),
            jam_busy: false,
            jam_at: Instant::now(),
            jam_error: None,
            editor: None,
            show_shortcuts: false,
            sidebar_pins_open: true,
            sidebar_playlists_open: true,
            library_grid: settings_library_grid,
            library_sort_name: false,
            library_filter: String::new(),
            home_filter: 0,
            artist_tab: 0,
            artist_grid: true,
            search_filter: 0,
            #[cfg(not(windows))]
            sys: sysinfo::System::new(),
            mem_mb: None,
            mem_at: Instant::now() - Duration::from_secs(10),
            frame_ms: 0.0,
            frame_hist: std::collections::VecDeque::with_capacity(240),
            frame_phases: [0.0; 4],
            web_busy: false,
            media_dirty: false,
            snapshot_path,
            snapshot_dirty: false,
            snapshot_saved_at: Instant::now(),
            snapshot_age: None,
            liked_refresh_pending: false,
            liked_reload: Vec::new(),
            liked_reload_ids: HashSet::new(),
            media_ready: false,
            shutdown_done: false,
            diag: false,
            diag_started: None,
            diag_step: 0,
            actions: Vec::new(),
        };
        app.apply_theme(ctx);
        if let Some(snap) = snapshot {
            app.apply_snapshot(snap);
        }
        app.load_saved_playback();
        if app.player.now.is_none() {
            // Sin copia local: la última escuchada según Spotify, en pausa, hasta que llegue la sesión.
            if let Some(t) = app.recent.first() {
                let np = NowPlaying::from_track(t);
                app.player.liked = np.id.as_ref().map(|id| app.liked_set.contains(id));
                app.player.now = Some(np);
                app.player.state = PlayState::Paused;
                app.player.position_ms = 0;
                app.player.position_at = None;
                app.now_placeholder = true;
            }
        }
        if app.api.web_configured() && !crate::config::no_session() {
            // El token propio de la Web API ya está en disco: el estado del reproductor y la
            // COLA se piden por HTTP de inmediato, sin esperar a que librespot conecte ni a que
            // este equipo sea el dispositivo activo. Así lo que suena en el móvil (pista, cola
            // y contexto) aparece en cuanto responde el servidor, no varios segundos después.
            app.api.send(Req::PlayerState);
            app.api.send(Req::Queue);
            // Historial del servidor: para saber si la cuenta hizo algo más reciente que la copia
            // local (p. ej. reproducir en el móvil o en Spotify de escritorio después de cerrar).
            app.api.send(Req::LastPlayback);
            crate::tmark("arranque: estado, cola e historial pedidos");
            app.last_remote_poll = Instant::now();
            app.queue_at = Instant::now();
            app.restore_deadline = Some(Instant::now() + Duration::from_secs(4));
        }
        // Con credenciales guardadas no se muestra la bienvenida: la sesión llegará sola.
        if let Some(username) = saved_username(&app.paths).filter(|_| !crate::config::no_session()) {
            log::info!("[t] credenciales guardadas ({username}): interfaz de sesión desde el primer fotograma");
            app.auth = Auth::Connecting { username };
        }
        app
    }

    /// Rellena la interfaz con la última instantánea antes de que llegue la red.
    fn apply_snapshot(&mut self, snap: Snapshot) {
        self.snapshot_age = Some(snap.age_secs());
        // Sin duplicados (una instantánea antigua podía tenerlos).
        let mut seen = HashSet::new();
        let liked: Vec<Track> = snap
            .liked
            .into_iter()
            .filter(|t| t.id.as_ref().map(|id| seen.insert(id.clone())).unwrap_or(true))
            .collect();
        let liked_total = snap.liked_total;
        self.user = snap.user;
        self.playlists = snap.playlists;
        self.playlists_loaded = !self.playlists.is_empty();
        self.recent = snap.recent;
        self.saved_albums = snap.saved_albums;
        if !self.saved_albums.is_empty() {
            self.requested.insert("albums".to_string());
        }
        self.following.clear();
        for a in &snap.followed_artists {
            self.following.insert(format!("artist:{}", a.id), true);
        }
        self.artists_loaded = !snap.followed_artists.is_empty();
        if self.artists_loaded {
            self.requested.insert("artists".to_string());
        }
        self.followed_artists = snap.followed_artists;
        if !liked.is_empty() {
            self.liked_set = liked.iter().filter_map(|t| t.id.clone()).collect();
            self.lists.insert(
                LIKED.to_string(),
                TrackList {
                    total: liked_total.max(liked.len() as u32),
                    tracks: liked,
                    loading: false,
                },
            );
            self.requested.insert(LIKED.to_string());
        }
    }

    fn snapshot(&self) -> Snapshot {
        let liked = self.lists.get(LIKED);
        Snapshot {
            saved_at: crate::cache::now_secs(),
            user: self.user.clone(),
            playlists: self.playlists.clone(),
            recent: self.recent.clone(),
            saved_albums: self.saved_albums.clone(),
            followed_artists: self.followed_artists.clone(),
            liked: liked.map(|l| l.tracks.clone()).unwrap_or_default(),
            liked_total: liked.map(|l| l.total).unwrap_or(0),
        }
    }

    fn save_play_log_if_needed(&mut self, force: bool) {
        if !self.play_log_dirty {
            return;
        }
        if !force && self.snapshot_saved_at.elapsed() < Duration::from_secs(5) {
            return;
        }
        self.play_log_dirty = false;
        self.play_log.save_async(self.play_log_path.clone());
    }

    fn save_snapshot_if_needed(&mut self, force: bool) {
        if !self.snapshot_dirty || !self.logged_in() {
            return;
        }
        if !force && self.snapshot_saved_at.elapsed() < Duration::from_secs(5) {
            return;
        }
        self.snapshot_dirty = false;
        self.snapshot_saved_at = Instant::now();
        self.snapshot().save_async(self.snapshot_path.clone());
    }

    /// Registra las teclas multimedia la primera vez que suena algo.
    fn ensure_media(&mut self) {
        if self.media_ready || !self.settings.media_keys {
            return;
        }
        self.media_ready = true;
        self.media = Media::new(self.hwnd, self.ui_tx.clone());
    }

    // ------------------------------------------------------------------ estado

    /// `--tab playlist:<id>` etc.: abre una pestaña en segundo plano.
    pub fn open_tab_from_flag(&mut self, spec: &str) {
        let page = match spec {
            "liked" => Page::Liked,
            "library" => Page::Library,
            "history" => Page::History,
            "saves" => Page::Saves,
            "shows" => Page::Shows,
            "audiobooks" => Page::Audiobooks,
            "folders" => Page::Folders,
            s if s.starts_with("playlist:") => Page::Playlist(s[9..].to_string()),
            s if s.starts_with("album:") => Page::Album(s[6..].to_string()),
            s if s.starts_with("artist:") => Page::Artist(s[7..].to_string()),
            _ => return,
        };
        self.open_tab(page);
    }

    /// Estado inicial pedido por línea de comandos (capturas, depuración).
    pub fn apply_start_flags(&mut self, page: Option<&str>, side: Option<&str>, jam: bool) {
        match page {
            Some("settings") => {
                self.draft = self.settings.clone();
                self.go(Page::Settings)
            }
            Some("library") => self.go(Page::Library),
            Some("history") => self.go(Page::History),
            Some(s) if s.starts_with("playlist:") => self.go(Page::Playlist(s[9..].to_string())),
            Some(s) if s.starts_with("album:") => {
                // album:<id>[:#n] (capturas): #n preselecciona la n-ésima pista.
                let parts: Vec<&str> = s[6..].split(':').collect();
                if let Some(mark) = parts.get(1) {
                    self.sel_list = parts[0].to_string();
                    self.sel.insert(mark.to_string());
                }
                self.go(Page::Album(parts[0].to_string()))
            }
            Some(s) if s.starts_with("artist:") => {
                let parts: Vec<&str> = s[7..].split(':').collect();
                if let Some(t) = parts.get(1).and_then(|t| t.parse::<u8>().ok()) {
                    self.artist_tab = t;
                }
                match parts.get(2).copied() {
                    Some("list") => self.artist_grid = false,
                    Some("grid") => self.artist_grid = true,
                    _ => {}
                }
                if let Some(aid) = parts.get(3) {
                    self.expanded_albums.insert(aid.to_string());
                }
                self.go(Page::Artist(parts[0].to_string()))
            }
            Some(s) if s.starts_with("show:") => self.go(Page::Show(s[5..].to_string())),
            Some(s) if s.starts_with("radio:") => self.pending_radio = Some(s[6..].to_string()),
            Some("home:customize") => self.home_customize_once = true,
            Some("editor:demo") => self.actions.push(Action::OpenEditor(None)),
            // Prueba de reconexión: simula una sesión estancada a los 6 s de arrancar.
            Some("stall") => self.stall_test = Some(Instant::now()),
            Some(s) if s.starts_with("editor:") => {
                // Captura: abre el editor de una playlist de la biblioteca en cuanto se cargue.
                self.pending_editor = Some(s[7..].to_string());
                self.go(Page::Folders)
            }
            Some("add:demo") => self.add_dialog = Some(AddDialog { uris: vec!["spotify:track:demo".into()], ..AddDialog::default() }),
            Some("bar:demo") => self.demo_playing(),
            Some("bar:demo:light") => {
                self.settings.theme = crate::config::Theme::Light;
                self.draft.theme = crate::config::Theme::Light;
                self.demo_playing();
            }
            Some("queue:demo") => {
                // Cola de muestra: una canción añadida y el resto siguiendo la playlist Me gusta.
                self.demo_playing();
                let tracks: Vec<Track> = self.lists.get(LIKED).map(|l| l.tracks.iter().take(9).cloned().collect()).unwrap_or_default();
                if tracks.len() > 2 {
                    self.queued_local = vec![tracks[1].uri.clone()];
                    self.last_play = Some(PlayTarget::Tracks { uris: tracks.iter().map(|t| t.uri.clone()).collect(), index: Some(0), shuffle: false });
                    self.last_play_page = Some(Page::Liked);
                    self.queue = Some(QueueResponse { currently_playing: Some(tracks[0].clone()), queue: tracks[1..].to_vec() });
                    self.side = Some(SideTab::Queue);
                    self.queue_frozen = true;
                }
            }
            Some("add:demo:playing") => {
                self.demo_playing();
                self.add_dialog = Some(AddDialog { uris: vec!["spotify:track:demo".into()], ..AddDialog::default() });
            }
            Some(s) if s.starts_with("probe:") => {
                self.pending_probes.push(s[6..].to_string());
                self.go(Page::History)
            }
            Some(s) if s.starts_with("loadctx:") => {
                // Prueba: deja la sesión de la cuenta en un contexto real (en pausa).
                self.pending_loadctx = Some(s[8..].to_string());
                self.go(Page::Home)
            }
            Some(s) if s.starts_with("fx:") => {
                // Pruebas de carpetas y permisos: fx:folder:<nombre>[:<pid>,…] | fx:rmfolder:<id>
                // | fx:renfolder:<id>:<nombre> | fx:mvin:<pid>:<fid> | fx:mvout:<pid>
                // | fx:invite:<pid> | fx:members:<pid> | fx:member:<pid>:<user>:on|off
                let parts: Vec<&str> = s[3..].splitn(4, ':').collect();
                let req = match parts.as_slice() {
                    ["folder", name] => Some(Req::FolderCreate { name: name.to_string(), playlists: Vec::new() }),
                    ["folder", name, pls] => Some(Req::FolderCreate { name: name.to_string(), playlists: pls.split(',').map(String::from).collect() }),
                    ["rmfolder", id] => Some(Req::FolderDelete(id.to_string())),
                    ["renfolder", id, name] => Some(Req::FolderRename { id: id.to_string(), name: name.to_string() }),
                    ["mvin", pid, fid] => Some(Req::FolderMove { playlist: pid.to_string(), folder: Some(fid.to_string()) }),
                    ["mvout", pid] => Some(Req::FolderMove { playlist: pid.to_string(), folder: None }),
                    ["invite", pid] => Some(Req::InviteLink(pid.to_string())),
                    ["members", pid] => Some(Req::Members(pid.to_string())),
                    ["member", pid, user, on] => Some(Req::SetMember { playlist: pid.to_string(), user: user.to_string(), contributor: *on == "on" }),
                    ["base", pid, on] => Some(Req::SetBase { playlist: pid.to_string(), contributor: *on == "on" }),
                    _ => None,
                };
                match req {
                    Some(r) => self.pending_fx.push(r),
                    None => log::warn!("fx: comando desconocido: {s}"),
                }
                self.go(Page::Folders)
            }
            Some(s) if s.starts_with("download:") => {
                // Prueba de descarga a la caché de audio (depuración): se lanza al iniciar sesión.
                self.pending_downloads.push(s[9..].to_string());
                self.go(Page::History)
            }
            Some("liked") => self.go(Page::Liked),
            Some("search") => self.go(Page::Search),
            Some("albums") => self.go(Page::Albums),
            Some("saves") => self.go(Page::Saves),
            Some("shows") => self.go(Page::Shows),
            Some("audiobooks") => self.go(Page::Audiobooks),
            Some("folders") => self.go(Page::Folders),
            Some("artists") => self.go(Page::Artists),
            _ => {}
        }
        match side {
            Some("queue") => self.side = Some(SideTab::Queue),
            Some("lyrics") => self.side = Some(SideTab::Lyrics),
            // `--side none`: capturas sin panel lateral y sin guardar ajustes al salir.
            Some("none") => {
                if page != Some("queue:demo") {
                    self.side = None;
                }
                self.ephemeral = true;
            }
            _ => {}
        }
        if jam {
            self.jam_open = true;
        }
    }

    /// Sesión operativa (la red y la reproducción están disponibles).
    pub fn logged_in(&self) -> bool {
        matches!(self.auth, Auth::LoggedIn { .. })
    }

    /// Sesión iniciada o a punto de estarlo: la interfaz se pinta como con sesión.
    pub fn signed_in(&self) -> bool {
        matches!(self.auth, Auth::LoggedIn { .. } | Auth::Connecting { .. })
    }

    /// Abre el diálogo «Añadir a una playlist» para una o varias canciones.
    pub fn open_add_dialog(&mut self, uris: Vec<String>) {
        if !self.logged_in() || uris.is_empty() {
            return;
        }
        self.request_once("rootlist", Req::Rootlist);
        self.add_dialog = Some(AddDialog { uris, ..AddDialog::default() });
    }

    /// Reproducción simulada para capturas (`--page bar:demo`): pista de la instantánea.
    fn demo_playing(&mut self) {
        if let Some(t) = self.lists.get(LIKED).and_then(|l| l.tracks.first()).cloned() {
            let mut np = NowPlaying::from_track(&t);
            np.duration_ms = t.duration_ms;
            self.player.now = Some(np);
            self.player.state = PlayState::Playing;
            self.player.position_ms = t.duration_ms / 3;
            self.player.position_at = Some(Instant::now());
            self.player.liked = Some(true);
        }
    }

    /// Oculta o vuelve a mostrar una canción (solo en Nanofy; Spotify no lo ofrece a apps externas).
    pub fn toggle_hidden(&mut self, id: &str) {
        if !self.hidden_tracks.remove(id) {
            self.hidden_tracks.insert(id.to_string());
            self.status("Canción oculta: se atenúa en las listas y se salta al reproducir");
        } else {
            self.status("Canción visible de nuevo");
        }
        if let Ok(t) = serde_json::to_string(&self.hidden_tracks) {
            let _ = std::fs::write(&self.hidden_path, t);
        }
    }

    /// De dónde sigue la reproducción (nombre y página): playlist, radio, álbum, artista, podcast
    /// o la página desde la que se pulsó reproducir (Me gusta, historial…).
    pub fn now_context(&self) -> Option<(String, Option<Page>)> {
        match &self.last_play {
            Some(PlayTarget::Context { uri, .. }) => {
                let mut parts = uri.splitn(3, ':');
                let (_, kind, id) = (parts.next()?, parts.next()?, parts.next()?);
                let id = id.to_string();
                Some(match kind {
                    "playlist" => {
                        let name = self
                            .playlist_meta
                            .get(&id)
                            .map(|p| p.name.clone())
                            .or_else(|| self.playlists.iter().find(|p| p.id == id).map(|p| p.name.clone()))
                            .unwrap_or_else(|| "Playlist".into());
                        (name, Some(Page::Playlist(id)))
                    }
                    "album" => (self.albums.get(&id).map(|a| a.name.clone()).unwrap_or_else(|| "Álbum".into()), Some(Page::Album(id))),
                    "artist" => (
                        self.artists.get(&id).and_then(|a| a.artist.as_ref()).map(|a| a.name.clone()).unwrap_or_else(|| "Artista".into()),
                        Some(Page::Artist(id)),
                    ),
                    "show" => (self.shows.get(&id).map(|s| s.0.name.clone()).unwrap_or_else(|| "Podcast".into()), Some(Page::Show(id))),
                    "station" => ("Radio de la canción".into(), None),
                    _ => ("Contexto".into(), None),
                })
            }
            Some(PlayTarget::Tracks { .. }) => self.last_play_page.clone().map(|p| (self.page_label(&p).1, Some(p))),
            None => None,
        }
    }

    /// Nombre visible de un usuario (pide el perfil la primera vez; mientras, el nombre de usuario).
    pub fn user_display(&mut self, username: &str) -> String {
        if let Some(u) = self.users.get(username) {
            return u.name().to_string();
        }
        self.request_once(&format!("user:{username}"), Req::User(username.to_string()));
        username.to_string()
    }

    pub fn my_id(&self) -> Option<&str> {
        self.user.as_ref().map(|u| u.id.as_str())
    }

    /// Submenú «Añadir a carpeta»: carpetas existentes, quitar de la actual y crear una nueva.
    pub fn folder_menu(&mut self, ui: &mut egui::Ui, pid: &str) {
        self.request_once("rootlist", Req::Rootlist);
        let current = self.folders.iter().find(|f| f.playlists.iter().any(|p| p == pid)).map(|f| f.id.clone());
        let label = match current.as_ref().and_then(|c| self.folders.iter().find(|f| &f.id == c)) {
            Some(f) => format!("Carpeta: {}", f.name),
            None => "Añadir a carpeta".to_string(),
        };
        let r = Self::menu_item(ui, Some(icons::Icon::Folder), &label, true);
        let folders: Vec<(String, String)> = self.folders.iter().map(|f| (f.id.clone(), f.name.clone())).collect();
        let pid = pid.to_string();
        egui::containers::menu::SubMenu::default().show(ui, &r, |ui| {
            for (fid, name) in &folders {
                if Some(fid) == current.as_ref() {
                    continue;
                }
                if Self::menu_item(ui, Some(icons::Icon::Folder), name, false).clicked() {
                    self.api.send(Req::FolderMove { playlist: pid.clone(), folder: Some(fid.clone()) });
                    ui.close();
                }
            }
            if current.is_some() && Self::menu_item(ui, Some(icons::Icon::Minus), "Quitar de la carpeta", false).clicked() {
                self.api.send(Req::FolderMove { playlist: pid.clone(), folder: None });
                ui.close();
            }
            if Self::menu_item(ui, Some(icons::Icon::Plus), "Nueva carpeta…", false).clicked() {
                self.folder_dialog = Some(FolderDialog { id: None, name: String::new(), playlist: Some(pid.clone()), busy: false });
                ui.close();
            }
        });
    }

    /// «Invitar colaboradores»: genera (o reutiliza) el enlace de invitación y lo copia.
    pub fn invite_menu_item(&mut self, ui: &mut egui::Ui, pid: &str) {
        if Self::menu_item(ui, Some(icons::Icon::People), "Invitar colaboradores", false).clicked() {
            self.request_invite(pid);
            ui.close();
        }
    }

    pub fn request_invite(&mut self, pid: &str) {
        match self.invite_links.get(pid).cloned() {
            Some(link) => self.actions.push(Action::CopyText(link, "Enlace de invitación")),
            None => self.api.send(Req::InviteLink(pid.to_string())),
        }
    }

    pub fn is_mine(&self, p: &Playlist) -> bool {
        match (self.my_id(), p.owner.id.as_deref()) {
            (Some(me), Some(owner)) => me == owner,
            _ => false,
        }
    }

    pub fn in_library(&self, playlist_id: &str) -> bool {
        self.playlists.iter().any(|p| p.id == playlist_id)
    }

    pub fn page(&self) -> &Page {
        match self.active {
            ActiveTab::Home => &Page::Home,
            ActiveTab::Search => &Page::Search,
            ActiveTab::Tab(i) => self.tabs.get(i).map(|t| t.page()).unwrap_or(&Page::Home),
        }
    }

    /// Navega: Inicio y Buscar son pestañas fijas; el resto de páginas viven en pestañas de
    /// contenido. Desde Inicio o Buscar se abre (o se reutiliza) una pestaña; dentro de una
    /// pestaña se avanza en su historial.
    pub fn go(&mut self, page: Page) {
        if *self.page() == page {
            return;
        }
        self.selected = None;
        match page {
            Page::Home => self.active = ActiveTab::Home,
            Page::Search => {
                self.active = ActiveTab::Search;
                self.focus_search = true;
            }
            page => {
                if let ActiveTab::Tab(i) = self.active {
                    if let Some(t) = self.tabs.get_mut(i) {
                        t.history.truncate(t.idx + 1);
                        t.history.push(page);
                        t.idx = t.history.len() - 1;
                        return;
                    }
                }
                if let Some(i) = self.tabs.iter().position(|t| *t.page() == page) {
                    self.active = ActiveTab::Tab(i);
                } else {
                    let i = self.open_tab(page);
                    self.active = ActiveTab::Tab(i);
                }
            }
        }
    }

    /// Abre una pestaña nueva (en segundo plano) y devuelve su índice.
    pub fn open_tab(&mut self, page: Page) -> usize {
        if let Some(i) = self.tabs.iter().position(|t| *t.page() == page) {
            return i;
        }
        const MAX_TABS: usize = 8;
        if self.tabs.len() >= MAX_TABS {
            let victim = (0..self.tabs.len()).find(|&i| self.active != ActiveTab::Tab(i)).unwrap_or(0);
            self.close_tab(victim);
        }
        self.tabs.push(Tab { history: vec![page], idx: 0 });
        self.tabs.len() - 1
    }

    pub fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        self.tabs.remove(i);
        self.active = match self.active {
            ActiveTab::Tab(a) if a == i => {
                if self.tabs.is_empty() {
                    ActiveTab::Home
                } else {
                    ActiveTab::Tab(i.min(self.tabs.len() - 1))
                }
            }
            ActiveTab::Tab(a) if a > i => ActiveTab::Tab(a - 1),
            other => other,
        };
    }

    pub fn can_back(&self) -> bool {
        matches!(self.active, ActiveTab::Tab(i) if self.tabs.get(i).map(|t| t.idx > 0).unwrap_or(false))
    }

    pub fn can_forward(&self) -> bool {
        matches!(self.active, ActiveTab::Tab(i) if self.tabs.get(i).map(|t| t.idx + 1 < t.history.len()).unwrap_or(false))
    }

    pub fn back(&mut self) {
        if let ActiveTab::Tab(i) = self.active {
            if let Some(t) = self.tabs.get_mut(i) {
                if t.idx > 0 {
                    t.idx -= 1;
                    self.selected = None;
                }
            }
        }
    }

    pub fn forward(&mut self) {
        if let ActiveTab::Tab(i) = self.active {
            if let Some(t) = self.tabs.get_mut(i) {
                if t.idx + 1 < t.history.len() {
                    t.idx += 1;
                    self.selected = None;
                }
            }
        }
    }

    /// Descarga pistas a la caché de audio (omite las ya descargadas o en curso).
    pub fn download(&mut self, ids: Vec<String>) {
        self.download_kind(ids, false)
    }

    pub fn download_kind(&mut self, ids: Vec<String>, episodes: bool) {
        if !self.logged_in() {
            return;
        }
        if self.settings.audio_cache_mb == 0 {
            self.status_err("Activa la caché de audio en Ajustes para descargar");
            return;
        }
        let ids: Vec<String> = ids
            .into_iter()
            .filter(|i| !self.downloaded.contains(i) && !self.downloading.contains(i))
            .collect();
        if ids.is_empty() {
            self.status("Ya estaban descargadas");
            return;
        }
        for i in &ids {
            self.downloading.insert(i.clone());
        }
        let n = ids.len();
        self.api.send(Req::Download { ids, episodes, quality: self.settings.quality });
        self.status(if n == 1 { "Descargando 1 canción…".to_string() } else { format!("Descargando {n} canciones…") });
    }

    pub fn status(&mut self, text: impl Into<String>) {
        self.status = Some((text.into(), Instant::now(), false));
    }

    // ------------------------------------------------------------ actualizaciones

    /// Consulta la última release en GitHub (en un hilo). `manual` = pulsado en Ajustes.
    pub fn check_updates(&mut self, manual: bool) {
        if self.update_busy {
            return;
        }
        self.update_busy = true;
        if manual {
            self.update_note = None;
        }
        crate::update::check(self.ui_tx.clone(), manual);
    }

    fn on_update(&mut self, result: UpdateResult, manual: bool) {
        self.update_busy = false;
        match result {
            UpdateResult::Available(info) => {
                // La versión omitida no vuelve a saltar sola, pero sí si el usuario la pide.
                let skipped = !manual && self.settings.update_skipped == info.version;
                self.update_note = Some((format!("Hay una versión nueva: {}", info.version), false));
                self.update_banner = !skipped;
                self.update = Some(info);
            }
            UpdateResult::UpToDate => {
                self.update = None;
                self.update_banner = false;
                self.update_note = Some((format!("Estás al día ({})", crate::update::current_version()), false));
            }
            UpdateResult::Failed(e) => {
                // La comprobación automática falla en silencio (sin red, límite de GitHub…).
                log::info!("[update] {e}");
                if manual {
                    self.update_note = Some((e, true));
                }
            }
        }
    }

    /// Activa o desactiva el aviso automático; se guarda al instante.
    pub fn set_update_check(&mut self, on: bool) {
        self.settings.update_check = on;
        self.draft.update_check = on;
        self.update_check_at = on.then(|| Instant::now() + Duration::from_secs(6 * 3600));
        if !self.ephemeral {
            self.settings.save(&self.paths);
        }
    }

    /// «Omitir esta versión»: no se vuelve a avisar de ella (sí de las siguientes).
    pub fn skip_update(&mut self) {
        if let Some(u) = &self.update {
            self.settings.update_skipped = u.version.clone();
            self.draft.update_skipped = u.version.clone();
            if !self.ephemeral {
                self.settings.save(&self.paths);
            }
        }
        self.update_banner = false;
    }

    /// Abre en el navegador el zip de esta plataforma (`download`) o la página de la release.
    pub fn open_update(&mut self, ctx: &egui::Context, download: bool) {
        let Some(u) = &self.update else {
            return;
        };
        let url = match (&u.asset_url, download) {
            (Some(asset), true) => asset.clone(),
            _ => u.page_url.clone(),
        };
        ctx.open_url(egui::OpenUrl::new_tab(url));
        self.status(if download { "Se ha abierto el navegador para descargar la versión nueva" } else { "Se han abierto las novedades en el navegador" });
    }

    pub fn status_err(&mut self, text: impl Into<String>) {
        let text = text.into();
        log::warn!("{text}");
        self.status = Some((text, Instant::now(), true));
    }

    pub fn request_once(&mut self, key: &str, req: Req) {
        if self.logged_in() && !self.requested.contains(key) {
            self.requested.insert(key.to_string());
            self.api.send(req);
        }
    }

    pub fn invalidate(&mut self, key: &str) {
        self.requested.remove(key);
    }

    pub fn login(&mut self) {
        self.auth = Auth::LoggingIn;
        self.backend.send(Cmd::Login);
    }

    pub fn apply_theme(&self, ctx: &egui::Context) {
        theme::apply(ctx, self.settings.theme, self.settings.zoom);
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(FPS_CAP_KEY), self.settings.fps_cap));
    }

    pub fn refresh_mem(&mut self) {
        if self.mem_at.elapsed() < Duration::from_secs(2) && self.mem_mb.is_some() {
            return;
        }
        self.mem_at = Instant::now();
        #[cfg(windows)]
        {
            // Consulta directa del proceso: sin tablas de procesos de sysinfo en memoria.
            use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
            use windows::Win32::System::Threading::GetCurrentProcess;
            let mut c = PROCESS_MEMORY_COUNTERS::default();
            let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32) };
            if ok.as_bool() {
                self.mem_mb = Some(c.WorkingSetSize as f64 / (1024.0 * 1024.0));
            }
        }
        #[cfg(not(windows))]
        if let Ok(pid) = sysinfo::get_current_pid() {
            self.sys.refresh_processes_specifics(
                sysinfo::ProcessesToUpdate::Some(&[pid]),
                true,
                sysinfo::ProcessRefreshKind::nothing().with_memory(),
            );
            self.mem_mb = self
                .sys
                .process(pid)
                .map(|p| p.memory() as f64 / (1024.0 * 1024.0));
        }
    }

    // --------------------------------------------------------------- mensajes

    fn drain(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Backend(e) => self.on_event(e),
                Msg::Api(r) => self.on_api(r),
                Msg::Image { key, image } => self.images.loaded(ctx, &key, image),
                Msg::Media(ev) => self.on_media(ev),
                Msg::Update { result, manual } => self.on_update(result, manual),
                Msg::Control(req) => {
                    let reply = self.control_exec(ctx, &req.cmd);
                    let _ = req.reply.send(reply);
                }
            }
        }
        if self.media_dirty {
            self.media_dirty = false;
            self.media.set_metadata(self.player.now.as_ref());
            self.media.set_playback(
                self.player.state == PlayState::Playing,
                self.player.state == PlayState::Stopped || self.player.now.is_none(),
                self.player.position(),
            );
        }
    }

    fn on_media(&mut self, ev: MediaControlEvent) {
        match ev {
            MediaControlEvent::Play => {
                if self.player.state != PlayState::Playing {
                    self.play_pause();
                }
            }
            MediaControlEvent::Pause | MediaControlEvent::Stop => {
                if self.player.state == PlayState::Playing {
                    self.play_pause();
                }
            }
            MediaControlEvent::Toggle => self.play_pause(),
            MediaControlEvent::Next => self.next(),
            MediaControlEvent::Previous => self.prev(),
            MediaControlEvent::Seek(dir) => self.seek_by(match dir {
                SeekDirection::Forward => 10_000,
                SeekDirection::Backward => -10_000,
            }),
            MediaControlEvent::SeekBy(dir, d) => {
                let ms = d.as_millis() as i64;
                self.seek_by(match dir {
                    SeekDirection::Forward => ms,
                    SeekDirection::Backward => -ms,
                })
            }
            MediaControlEvent::SetPosition(p) => self.seek(p.0.as_millis() as u32),
            MediaControlEvent::SetVolume(v) => {
                self.set_volume(vol_pct_to_raw((v.clamp(0.0, 1.0) * 100.0) as f32))
            }
            MediaControlEvent::OpenUri(uri) => self.actions.push(Action::OpenLink(uri)),
            MediaControlEvent::Raise | MediaControlEvent::Quit => {}
        }
    }

    fn on_event(&mut self, e: Event) {
        if self.diag {
            log::info!("[diag] evento {e:?}");
        } else {
            log::debug!("[evento] {e:?}");
        }
        match e {
            Event::Status(s) => self.status(s),
            Event::Error(s) => {
                if matches!(self.auth, Auth::LoggingIn | Auth::Connecting { .. }) {
                    self.auth = Auth::LoggedOut;
                }
                self.status_err(s);
            }
            Event::LoggedIn {
                username,
                device_id,
            } => {
                // Foto y nombre del perfil por el protocolo interno (no gasta cuota de la Web API).
                if !self.users.contains_key(&username) {
                    self.api.send(Req::User(username.clone()));
                }
                self.auth = Auth::LoggedIn { username };
                self.device_id = device_id;
                crate::tmark("sesión: conectado");
                if self.restore_pending.is_some() || self.restore_wanted {
                    // La decisión (copia local, clúster de Connect, recently-played) se toma en
                    // try_decide_restore; el clúster inicial llega justo después de conectar.
                    if self.restore_deadline.is_none() {
                        self.restore_deadline = Some(Instant::now() + Duration::from_millis(2500));
                    }
                    self.try_decide_restore(false);
                }
                self.refresh_from_network();
                if let Some(id) = self.pending_radio.take() {
                    self.api.send(Req::RadioPlaylist(id));
                }
                for e in std::mem::take(&mut self.pending_probes) {
                    self.api.send(Req::Probe(e));
                }
                for r in std::mem::take(&mut self.pending_fx) {
                    self.api.send(r);
                }
                if let Some(uri) = self.pending_loadctx.take() {
                    self.restore_wanted = false;
                    self.restore_pending = None;
                    self.backend.send(Cmd::LoadContext { uri, track_uri: None, index: Some(0), shuffle: false, resume: Some(0) });
                }
                if !self.pending_downloads.is_empty() {
                    let ids = std::mem::take(&mut self.pending_downloads);
                    self.download(ids);
                }
                self.status("Conectado a Spotify");
            }
            Event::LoggedOut => {
                self.auth = Auth::LoggedOut;
                self.user = None;
                self.playlists.clear();
                self.playlists_loaded = false;
                self.playlist_meta.clear();
                self.lists.clear();
                self.saved_albums.clear();
                self.followed_artists.clear();
                self.albums.clear();
                self.artists.clear();
                self.users.clear();
                self.user_playlists.clear();
                self.following.clear();
                self.recent.clear();
                self.requested.clear();
                self.search_result = None;
                self.liked_set.clear();
                self.devices.clear();
                self.queue = None;
                self.lyrics = None;
                self.lyrics_for = None;
                self.jam = None;
                self.jam_queue_active = false;
                let volume = self.player.volume;
                self.player = PlayerState {
                    volume,
                    ..Default::default()
                };
                self.media_dirty = true;
                self.status("Sesión cerrada");
            }
            Event::TrackChanged(np) => {
                self.ws_trim_at = Some(Instant::now() + Duration::from_secs(6));
                if self.now_placeholder || self.pause_after_restore || self.restore_mark {
                    self.restore_mark = false;
                    self.restore_fallback_at = None;
                    crate::tmark("sesión: pista cargada");
                    // La cola de la sesión restaurada: Spotify tarda unos segundos en reflejar
                    // el contexto cargado, así que se pide con reintentos cortos (tick).
                    self.queue_retry = Some((Instant::now() + Duration::from_millis(300), 0));
                }
                self.now_placeholder = false;
                if self.restore_pending.is_some() && self.cluster_restored {
                    // Ya suena lo de Spotify: la copia local sobra.
                    self.restore_pending = None;
                }
                self.player.remote = None;
                self.set_now_playing(np);
            }
            Event::Playing { position_ms } if self.pause_after_restore => {
                // Al abrir nunca suena solo: la sesión restaurada se deja en pausa.
                self.player.state = PlayState::Playing;
                self.player.position_ms = position_ms;
                self.play_pause();
            }
            Event::Playing { position_ms } if self.pause_on_play => {
                self.pause_on_play = false;
                self.player.state = PlayState::Playing;
                self.player.position_ms = position_ms;
                self.play_pause();
                self.status("Temporizador: reproducción pausada al terminar la canción");
            }
            Event::Playing { position_ms } => {
                self.player.remote = None;
                self.player.state = PlayState::Playing;
                self.player.position_ms = position_ms;
                self.player.position_at = Some(Instant::now());
                self.media_dirty = true;
            }
            Event::Paused { position_ms } => {
                self.pause_after_restore = false;
                self.player.remote = None;
                self.player.state = PlayState::Paused;
                self.player.position_ms = position_ms;
                self.player.position_at = None;
                self.media_dirty = true;
                if self.play_after_restore && !self.now_placeholder {
                    // Lo restaurado ya está cargado en pausa: se cumple el play pendiente.
                    self.play_after_restore = false;
                    self.backend.send(Cmd::Play);
                }
            }
            Event::Position(ms) => {
                self.player.position_ms = ms;
                self.player.position_at =
                    (self.player.state == PlayState::Playing).then(Instant::now);
                self.media_dirty = true;
            }
            Event::Stopped => {
                self.player.state = PlayState::Stopped;
                self.player.position_at = None;
                self.media_dirty = true;
            }
            Event::Loading => self.player.state = PlayState::Loading,
            Event::Unavailable => {
                self.status_err("Esta canción no está disponible (¿cuenta sin Premium?)");
                // Si no hay nada más que reproducir, librespot no manda «parado»: el botón se
                // quedaría en «cargando». Si sigue con otra pista, el evento Playing lo corrige.
                if self.player.state == PlayState::Loading {
                    self.player.state = PlayState::Stopped;
                    self.player.position_at = None;
                    self.media_dirty = true;
                }
            }
            Event::Volume(v) => {
                if self.volume_drag.is_none() && self.accept_volume_echo(v) {
                    self.player.volume = v;
                }
            }
            Event::ShutdownDone => self.shutdown_done = true,
            Event::Cluster(info) => {
                if self.server_cluster.is_none() {
                    crate::tmark("sesión: estado del clúster recibido");
                }
                self.server_cluster = Some(if info.track_uri.is_empty() { None } else { Some(info) });
                if !self.cluster_restored && (self.restore_wanted || self.restore_pending.is_some()) {
                    self.try_decide_restore(false);
                }
            }
            Event::SessionResumed(ok) => {
                self.restore_awaiting = false;
                if ok {
                    // Spotify tenía la sesión: manda sobre la copia local (que se conserva de
                    // respaldo hasta que llegue la pista).
                    self.restore_deadline = None;
                    self.pause_after_restore = true;
                    self.restore_mark = true;
                    self.restore_fallback_at = Some(Instant::now() + Duration::from_millis(3500));
                    self.status("Sesión restaurada desde Spotify");
                } else {
                    self.cluster_restored = true;
                    self.restore_deadline = None;
                    self.restore_local();
                }
            }
            Event::Reconnected => {
                self.status("Conexión con Spotify restablecida");
                self.loading_since = None;
                if self.player.state == PlayState::Loading {
                    self.player.state = PlayState::Stopped;
                }
                self.media_dirty = true;
            }
            Event::Shuffle(s) => self.player.shuffle = s,
            Event::Repeat { context, track } => {
                self.player.repeat = if track {
                    Repeat::Track
                } else if context {
                    Repeat::Context
                } else {
                    Repeat::Off
                }
            }
            Event::JamQueue { current, next, .. } => {
                // En una Jam, la cola mostrada es la de la sesión compartida (la envía el motor).
                // Se resuelve a pistas con metadatos y se muestra en lugar de la cola de la cuenta.
                self.jam_queue_active = true;
                self.api.send(Req::JamQueue { current, next });
            }
        }
    }

    /// Cambio de pista (local o remota): corazón, letras, cola y metadatos del sistema.
    fn set_now_playing(&mut self, np: NowPlaying) {
        if self.player.now.as_ref() == Some(&np) {
            return;
        }
        self.player.liked = np.id.as_ref().map(|id| self.liked_set.contains(id));
        // Canción oculta: se salta (una vez por pista, para no entrar en bucle).
        if let Some(id) = &np.id {
            if self.hidden_tracks.contains(id) && self.player.remote.is_none() && self.last_skipped.as_deref() != Some(id.as_str()) {
                self.last_skipped = Some(id.clone());
                self.next();
            }
        }
        if self.queued_local.first() == Some(&np.uri) {
            self.queued_local.remove(0);
        }
        if self.sleep_end_of_track {
            self.sleep_end_of_track = false;
            self.pause_on_play = true;
        }
        let played = track_from_now(&np);
        // Sin id de álbum (librespot no lo da): se pide para «Ver álbum» y el menú de la portada.
        if np.album_id.is_none() {
            if let Some(id) = np.id.clone() {
                self.request_once(&format!("trackinfo:{id}"), Req::TrackInfo(id));
            }
        }
        self.play_log.record(played.clone());
        // Historial en tiempo real: la canción que empieza va arriba (Spotify la registrará después).
        if self.recent.first().map(|r| r.uri != played.uri).unwrap_or(true) {
            self.recent.insert(0, played);
            self.recent.truncate(50);
            self.snapshot_dirty = true;
        }
        self.play_log_dirty = true;
        self.player.now = Some(np);
        self.ensure_media();
        self.media_dirty = true;
        self.queue_at = Instant::now() - Duration::from_secs(60);
        if self.side == Some(SideTab::Lyrics) {
            self.ensure_lyrics();
        }
    }

    pub fn ensure_lyrics(&mut self) {
        let Some(np) = self.player.now.clone() else {
            return;
        };
        let Some(id) = np.id.clone() else {
            return;
        };
        if Some(&id) == self.lyrics_for.as_ref() {
            return;
        }
        self.lyrics_for = Some(id.clone());
        self.lyrics = None;
        self.lyrics_loading = true;
        self.api.send(Req::Lyrics {
            id,
            name: np.name.clone(),
            artist: np.artists_str(),
            album: np.album.clone(),
            duration_ms: np.duration_ms,
        });
    }

    fn on_api(&mut self, r: ApiResult) {
        let resp = match r.result {
            Ok(resp) => resp,
            Err(e) => {
                match r.req {
                    Req::PlayerState | Req::Devices | Req::Queue => {
                        log::warn!("{e}")
                    }
                    // Precarga de playlist fallida (p. ej. límite de ritmo): se desmarca para que
                    // se recargue al abrirla, en vez de quedar bloqueada y salir vacía.
                    Req::PlaylistTracks(ref id) if !self.lists.contains_key(id) => {
                        self.requested.remove(&format!("pl:{id}"));
                        log::info!("precarga de playlist {id} falló ({e}); se reintentará al abrirla");
                    }
                    Req::FollowContains { .. } => log::warn!("{e}"),
                    Req::Search(_) => {
                        self.search_loading = false;
                        self.status_err(e);
                    }
                    Req::Playlists => {
                        self.playlists_loaded = true;
                        self.status_err(e);
                    }
                    Req::Liked if self.liked_refresh_pending => {
                        // La recarga completa falló: se conserva la lista cacheada.
                        self.liked_refresh_pending = false;
                        self.liked_reload.clear();
                        self.liked_reload_ids.clear();
                        self.requested.remove(LIKED);
                        if let Some(l) = self.lists.get_mut(LIKED) {
                            l.loading = false;
                        }
                        self.status_err(e);
                    }
                    _ if e.starts_with("Spotify ha agotado la cuota") => {
                        // Un único aviso; el resto de peticiones fallan en local sin ruido.
                        if !self.status.as_ref().map(|s| s.0.starts_with("Spotify ha agotado")).unwrap_or(false) {
                            self.status_err(e);
                        }
                    }
                    Req::Download { ref ids, .. } => {
                        for id in ids {
                            self.downloading.remove(id);
                        }
                        self.status_err(format!("Descarga: {e}"));
                    }
                    Req::Save(ref ids) | Req::Unsave(ref ids) => {
                        let was_save = matches!(r.req, Req::Save(_));
                        // Revertir el corazón optimista (el estado real es el contrario del que se pidió).
                        for id in ids.clone() {
                            self.set_liked(&id, !was_save);
                        }
                        self.status_err(e);
                    }
                    Req::WebConnect(_) => {
                        self.web_busy = false;
                        self.status_err(format!("No se pudo conectar la Web API: {e}"));
                    }
                    Req::Lyrics { .. } => {
                        self.lyrics_loading = false;
                    }
                    Req::JamCurrent | Req::JamJoin(_) | Req::JamLeave(_) | Req::JamEnd(_) => {
                        self.jam_busy = false;
                        self.jam_error = Some(format!("Jam: {e}"));
                    }
                    Req::CreatePlaylist { .. }
                    | Req::UpdatePlaylist { .. }
                    | Req::SetPlaylistImage { .. } => {
                        if let Some(ed) = self.editor.as_mut() {
                            ed.busy = false;
                        }
                        self.status_err(e);
                    }
                    _ => self.status_err(e),
                }
                return;
            }
        };
        match resp {
            Resp::Me(u) => {
                self.snapshot_dirty = true;
                if self.diag {
                    log::info!("[diag] me: id={} nombre={:?}", u.id, u.display_name);
                }
                // Perfil completo (foto) por el protocolo interno, para el avatar de la barra superior.
                if !self.users.contains_key(&u.id) {
                    self.api.send(Req::User(u.id.clone()));
                }
                self.user = Some(u)
            }
            Resp::Playlists(p) => {
                if self.diag {
                    log::info!("[diag] playlists: {}", p.len());
                    if let Some(first) = p.first() {
                        self.api.send(Req::PlaylistTracks(first.id.clone()));
                        self.api.send(Req::PlaylistMeta(first.id.clone()));
                    }
                }
                self.playlists = p;
                self.playlists_loaded = true;
                // Precarga en segundo plano: primero las fijadas, luego las propias. Así cambiar
                // entre playlists es instantáneo (ya están en memoria al abrirlas).
                let pinned = self.settings.pinned.clone();
                let mut order: Vec<String> = Vec::new();
                for id in &pinned {
                    if self.playlists.iter().any(|p| &p.id == id) && !order.contains(id) {
                        order.push(id.clone());
                    }
                }
                let owned: Vec<String> = self.playlists.iter().filter(|p| self.is_mine(p)).map(|p| p.id.clone()).collect();
                for id in owned {
                    if !order.contains(&id) {
                        order.push(id);
                    }
                }
                self.prefetch_ids = order.into_iter().take(30).collect();
                self.prefetch_at = Some(Instant::now() + Duration::from_secs(2));
                if let Some(id) = self.pending_editor.take() {
                    if let Some(pl) = self.playlists.iter().find(|p| p.id == id).cloned() {
                        self.actions.push(Action::OpenEditor(Some(pl)));
                    }
                }
                self.snapshot_dirty = true;
            }
            Resp::PlaylistMeta(mut p) => {
                if let Some(old) = self.playlist_meta.get(&p.id) {
                    if p.images.as_ref().map(|v| v.is_empty()).unwrap_or(true) {
                        p.images = old.images.clone();
                    }
                    if p.description.is_none() {
                        p.description = old.description.clone();
                    }
                    if p.owner.display_name.is_none() {
                        p.owner = old.owner.clone();
                    }
                }
                // Radios y mixes: la portada generada por Spotify solo llega en el feed de inicio.
                if p.images.as_ref().map(|v| v.is_empty()).unwrap_or(true) {
                    if let Some(it) = self.home_feed.iter().flat_map(|s| s.items.iter()).find(|it| it.uri == p.uri) {
                        p.images = it.image.as_ref().map(|u| vec![Image { url: u.clone(), width: Some(300), height: Some(300) }]);
                    }
                }
                self.playlist_meta.insert(p.id.clone(), p);
            }
            Resp::Tracks {
                key,
                tracks,
                total,
                done,
            } => {
                if self.diag {
                    log::info!("[diag] tracks key={key} +{} total={total} done={done}", tracks.len());
                }
                if key == "liked_recent" {
                    // Fusiona lo guardado recientemente con la lista de la instantánea.
                    let list = self.lists.entry(LIKED.to_string()).or_default();
                    let mut fresh: Vec<Track> = tracks
                        .into_iter()
                        .filter(|t| t.id.as_ref().map(|id| !self.liked_set.contains(id)).unwrap_or(false))
                        .collect();
                    for t in &fresh {
                        if let Some(id) = &t.id {
                            self.liked_set.insert(id.clone());
                        }
                    }
                    if !fresh.is_empty() {
                        fresh.extend(std::mem::take(&mut list.tracks));
                        list.tracks = fresh;
                    }
                    list.total = total.max(list.tracks.len() as u32);
                    list.loading = false;
                    if let Some(id) = self.player.now.as_ref().and_then(|n| n.id.clone()) {
                        self.player.liked = Some(self.liked_set.contains(&id));
                    }
                    self.snapshot_dirty = true;
                    return;
                }
                let mut tracks = tracks;
                if key == LIKED && self.liked_refresh_pending {
                    // Recarga completa: se acumula aparte y solo al terminar sustituye a la lista
                    // cacheada. Si la Web API falla a medias (cuota), la lista antigua sigue entera.
                    for t in tracks {
                        if t.id.as_ref().map(|id| self.liked_reload_ids.insert(id.clone())).unwrap_or(true) {
                            self.liked_reload.push(t);
                        }
                    }
                    if done {
                        self.liked_refresh_pending = false;
                        let fresh = std::mem::take(&mut self.liked_reload);
                        self.liked_set = std::mem::take(&mut self.liked_reload_ids);
                        let list = self.lists.entry(LIKED.to_string()).or_default();
                        list.tracks = fresh;
                        list.total = total.max(list.tracks.len() as u32);
                        list.loading = false;
                        if let Some(id) = self.player.now.as_ref().and_then(|n| n.id.clone()) {
                            self.player.liked = Some(self.liked_set.contains(&id));
                        }
                        self.snapshot_dirty = true;
                    } else if let Some(list) = self.lists.get_mut(LIKED) {
                        list.total = total;
                    }
                    return;
                }
                if key == LIKED {
                    // Nunca duplicar una pista ya presente (p. ej. si se pidió la lista dos veces).
                    tracks.retain(|t| match &t.id {
                        Some(id) => self.liked_set.insert(id.clone()),
                        None => true,
                    });
                    if let Some(id) = self.player.now.as_ref().and_then(|n| n.id.clone()) {
                        self.player.liked = Some(self.liked_set.contains(&id));
                    }
                    if done {
                        self.snapshot_dirty = true;
                    }
                }
                let list_key = key.clone();
                let list = self.lists.entry(key).or_default();
                if !list.loading {
                    // Primer lote de una carga nueva: sustituye a la lista anterior en vez de
                    // anexarse a ella (si no, cada recarga duplicaba las canciones).
                    list.tracks.clear();
                }
                list.tracks.extend(tracks);
                list.total = total;
                list.loading = !done;
                if self.diag {
                    log::info!("[diag] lista {} -> {} pistas", list_key, list.tracks.len());
                }
                if let Some((ctx, cur)) = self.restore_ctx.clone() {
                    if ctx == list_key && self.build_context_queue(&ctx, &cur) {
                        self.restore_ctx = None;
                    }
                }
            }
            Resp::SavedAlbums(a) => {
                self.saved_albums = a;
                self.snapshot_dirty = true;
            }
            Resp::FollowedArtists(a) => {
                self.following.retain(|k, _| !k.starts_with("artist:"));
                for ar in &a {
                    self.following.insert(format!("artist:{}", ar.id), true);
                }
                self.followed_artists = a;
                self.artists_loaded = true;
                self.snapshot_dirty = true;
            }
            Resp::Album(a) => {
                let id = a.id.clone();
                self.albums.insert(a.id.clone(), a);
                if let Some((ctx, cur)) = self.restore_ctx.clone() {
                    if ctx == id && self.build_context_queue(&ctx, &cur) {
                        self.restore_ctx = None;
                    }
                }
            }
            Resp::Artist(a) => {
                let id = a.id.clone();
                self.artists.entry(id).or_default().artist = Some(a);
            }
            Resp::ArtistTop(t) => {
                if let Req::ArtistTop(id) = r.req {
                    self.artists.entry(id).or_default().top = t;
                }
            }
            Resp::TrackInfo(t) => {
                let album_id = t.album.as_ref().and_then(|a| a.id.clone());
                if let Some(now) = self.player.now.as_mut() {
                    if now.id == t.id && now.album_id.is_none() {
                        now.album_id = album_id.clone();
                        if now.cover_url.is_none() {
                            now.cover_url = t.cover(300).map(|s| s.to_string());
                        }
                    }
                }
                if let Some(r) = self.recent.iter_mut().find(|r| r.id == t.id) {
                    if r.album.as_ref().map(|a| a.id.is_none()).unwrap_or(true) {
                        r.album = t.album.clone();
                    }
                }
            }
            Resp::RadioPlaylist { playlist_id } => {
                // Pestaña nueva y activa con la playlist de la radio (nombre y portada por librespot).
                let page = Page::Playlist(playlist_id);
                let before = self.tabs.len();
                self.open_tab(page.clone());
                if self.tabs.len() > before {
                    self.active = ActiveTab::Tab(self.tabs.len() - 1);
                } else if let Some(i) = self.tabs.iter().position(|t| t.page() == &page) {
                    self.active = ActiveTab::Tab(i);
                }
            }
            Resp::ArtistPlaylists { id, playlists } => {
                self.artist_playlists.insert(id, playlists);
            }
            Resp::ArtistAlbums(a) => {
                if let Req::ArtistAlbums(id) = r.req {
                    self.artists.entry(id).or_default().albums = a;
                }
            }
            Resp::Search(s) => {
                // Dos búsquedas seguidas pueden responder desordenadas: solo vale la de la
                // consulta actual (si el usuario ya ha vuelto a escribir, se ignora la vieja).
                let stale = matches!(&r.req, Req::Search(q) if q.trim() != self.search_query.trim() && !self.search_query.trim().is_empty());
                if !stale {
                    self.search_result = Some(s);
                    self.search_loading = false;
                }
            }
            Resp::Recent(t) => {
                if !self.play_log.seeded {
                    // Primera vez: el historial de Spotify sirve de semilla del registro local.
                    for tr in t.iter().rev() {
                        self.play_log.record(tr.clone());
                    }
                    self.play_log.seeded = true;
                    self.play_log_dirty = true;
                }
                self.recent = t;
                self.snapshot_dirty = true;
            }
            Resp::Devices(d) => {
                if self.diag {
                    log::info!("[diag] dispositivos: {:?}", d.iter().map(|x| (x.name.clone(), x.kind.clone(), x.is_active)).collect::<Vec<_>>());
                }
                self.devices = d
            }
            Resp::PlayerState(st) => {
                log::debug!("[estado] reproductor: {} ({} ms desde main)", st.as_ref().map(|s| s.item.as_ref().map(|t| t.name.clone()).unwrap_or_default()).unwrap_or_else(|| "nada".into()), crate::since_start_ms());
                self.on_player_state(st);
            }
            // Mientras la cola de la Jam está activa, se ignora la cola del Web API (de la cuenta).
            Resp::Queue(_) if self.queue_frozen || self.jam_queue_active => {}
            Resp::JamQueue(q) => {
                self.queue = Some(q);
            }
            Resp::Queue(q) => {
                log::debug!("[cola] respuesta: sonando {:?}, {} en cola ({} ms desde main)", q.currently_playing.as_ref().map(|t| t.name.clone()), q.queue.len(), crate::since_start_ms());
                if self.diag {
                    log::info!("[diag] cola: sonando {:?}, {} en cola", q.currently_playing.as_ref().map(|t| t.name.clone()), q.queue.len());
                }
                if self.queue.as_ref().map(|old| old.queue.is_empty()).unwrap_or(true) && !q.queue.is_empty() {
                    crate::tmark(&format!("cola: {} pistas ({} ms desde main)", q.queue.len(), crate::since_start_ms()));
                }
                if !q.queue.is_empty() {
                    self.queue_retry = None;
                    self.queue = Some(q);
                } else if self.queue.as_ref().map(|old| old.queue.is_empty()).unwrap_or(true) {
                    // Solo se acepta una cola vacía si no había ya una provisional con contenido.
                    self.queue = Some(q);
                }
            }
            Resp::Saved { ids, saved } => {
                for id in ids {
                    self.set_liked(&id, saved);
                }
                self.status(if saved {
                    "Guardada en Canciones que te gustan"
                } else {
                    "Quitada de Canciones que te gustan"
                });
            }
            Resp::AlbumSaved { id, saved } => {
                if saved {
                    if let Some(a) = self.albums.get(&id) {
                        if !self.saved_albums.iter().any(|x| x.id == id) {
                            let mut a = a.clone();
                            a.tracks = None;
                            self.saved_albums.insert(0, a);
                        }
                    }
                } else {
                    self.saved_albums.retain(|x| x.id != id);
                }
                self.snapshot_dirty = true;
                self.status(if saved { "Álbum guardado en tu biblioteca" } else { "Álbum quitado de tu biblioteca" });
            }
            Resp::HomeFeed(sections) => {
                if let Ok(t) = serde_json::to_string(&sections) {
                    let path = self.home_feed_path.clone();
                    std::thread::spawn(move || {
                        let _ = std::fs::write(path, t);
                    });
                }
                self.home_feed = sections;
            }
            Resp::SavedShows(list) => {
                self.followed_shows = list.iter().map(|s| s.id.clone()).collect();
                self.followed_shows_list = list;
            }
            Resp::SavedEpisodes(list) => {
                self.saved_episodes = list.iter().map(|e| e.episode.id.clone()).collect();
                self.saved_episode_list = list;
            }
            Resp::SavedAudiobooks(list) => self.audiobooks = list,
            Resp::LastPlayback(last) => {
                self.server_last = Some(last);
                self.try_decide_restore(false);
            }
            Resp::Folders(list) => {
                if self.diag {
                    for f in &list {
                        log::info!("[diag] carpeta {} '{}' {:?}", f.id, f.name, f.playlists);
                    }
                }
                self.folders = list
            }
            Resp::RootlistChanged => {
                self.invalidate("rootlist");
                self.api.send(Req::Rootlist);
                if let Some(d) = self.folder_dialog.as_ref() {
                    if d.busy {
                        self.folder_dialog = None;
                    }
                }
                if !matches!(r.req, Req::FolderMove { .. }) {
                    self.status(match r.req {
                        Req::FolderCreate { .. } => "Carpeta creada",
                        Req::FolderDelete(_) => "Carpeta eliminada",
                        _ => "Carpeta renombrada",
                    });
                }
            }
            Resp::InviteLink { playlist, link } => {
                self.invite_links.insert(playlist, link.clone());
                if matches!(r.req, Req::InviteLink(_)) {
                    log::info!("[invite] {link}");
                    self.actions.push(Action::CopyText(link, "Enlace de invitación"));
                }
            }
            Resp::Members { playlist, members } => {
                for (u, _) in &members {
                    let _ = self.user_display(u);
                }
                self.members.insert(playlist, members);
            }
            Resp::ShowFollowed { id, on } => {
                if on {
                    if let Some((s, _)) = self.shows.get(&id) {
                        if !self.followed_shows_list.iter().any(|x| x.id == id) {
                            self.followed_shows_list.insert(0, s.clone());
                        }
                    }
                    self.followed_shows.insert(id);
                } else {
                    self.followed_shows_list.retain(|x| x.id != id);
                    self.followed_shows.remove(&id);
                }
                self.status(if on { "Siguiendo el podcast" } else { "Has dejado de seguir el podcast" });
            }
            Resp::EpisodeSaved { id, on } => {
                if on {
                    self.saved_episodes.insert(id);
                    self.requested.remove("saved_episodes");
                } else {
                    self.saved_episode_list.retain(|e| e.episode.id != id);
                    self.saved_episodes.remove(&id);
                }
                self.status(if on { "Episodio guardado" } else { "Episodio quitado de guardados" });
            }
            Resp::Downloaded(id) => {
                self.downloading.remove(&id);
                self.downloaded.insert(id);
                if let Ok(t) = serde_json::to_string(&self.downloaded) {
                    let _ = std::fs::write(&self.downloads_path, t);
                }
                if self.downloading.is_empty() {
                    self.status("Descarga completada");
                }
            }
            Resp::PlaylistCreated(p) => {
                let id = p.id.clone();
                if let Some(d) = self.add_dialog.as_mut() {
                    if d.pending_new.as_deref() == Some(p.name.as_str()) {
                        d.pending_new = None;
                        d.selected.insert(id.clone());
                    }
                }
                if !self.playlists.iter().any(|x| x.id == id) {
                    self.playlists.insert(0, p.clone());
                }
                self.playlist_meta.insert(id.clone(), p);
                self.api.send(Req::Playlists);
                if let Some(ed) = self.editor.take() {
                    if let Some(path) = ed.image_path {
                        self.api.send(Req::SetPlaylistImage {
                            id: id.clone(),
                            path,
                        });
                    }
                }
                self.status("Playlist creada");
                self.go(Page::Playlist(id));
            }
            Resp::PlaylistChanged(id) => {
                // Recarga metadatos y pistas de esa playlist y la lista de la biblioteca.
                self.playlist_meta.remove(&id);
                self.lists.remove(&id);
                self.invalidate(&format!("plmeta:{id}"));
                self.invalidate(&format!("pl:{id}"));
                self.api.send(Req::Playlists);
                if let Some(ed) = self.editor.as_mut() {
                    ed.busy = false;
                }
                if matches!(r.req, Req::UpdatePlaylist { .. }) {
                    if let Some(ed) = self.editor.take() {
                        if let Some(path) = ed.image_path {
                            self.api.send(Req::SetPlaylistImage { id, path });
                            return;
                        }
                    }
                    self.status("Playlist actualizada");
                } else if matches!(r.req, Req::SetPlaylistImage { .. }) {
                    self.images.clear();
                    self.status("Imagen de la playlist actualizada");
                } else if matches!(r.req, Req::AddToPlaylist { .. }) {
                    self.status("Añadida a la playlist");
                } else if matches!(r.req, Req::RemoveFromPlaylist { .. }) {
                    self.status("Quitada de la playlist");
                } else if matches!(r.req, Req::FollowPlaylist(_)) {
                    self.status("Playlist añadida a tu biblioteca");
                } else if matches!(r.req, Req::UnfollowPlaylist(_)) {
                    self.status("Playlist quitada de tu biblioteca");
                }
            }
            Resp::User(u) => {
                if self.diag {
                    log::info!("[diag] user: id={} nombre={:?} imgs={}", u.id, u.display_name, u.images.len());
                }
                self.users.insert(u.id.clone(), u);
            }
            Resp::UserPlaylists { user_id, playlists } => {
                if self.diag {
                    log::info!("[diag] user playlists {user_id}: {}", playlists.len());
                }
                self.user_playlists.insert(user_id, playlists);
            }
            Resp::FollowContains {
                kind,
                ids,
                following,
            } => {
                for (id, f) in ids.into_iter().zip(following) {
                    self.following.insert(format!("{kind}:{id}"), f);
                }
            }
            Resp::Followed {
                kind,
                id,
                following,
            } => {
                self.following.insert(format!("{kind}:{id}"), following);
                if kind == "artist" {
                    self.invalidate("artists");
                    self.followed_artists.clear();
                }
                self.status(if following {
                    "Ahora sigues este perfil"
                } else {
                    "Has dejado de seguir este perfil"
                });
            }
            Resp::Lyrics(l) => {
                if self.diag {
                    log::info!(
                        "[diag] respuesta letras: {:?}",
                        l.as_ref().map(|l| (l.sync_type.clone(), l.lines.len(), l.lines.first().map(|x| x.words.clone())))
                    );
                }
                self.lyrics_loading = false;
                if let Req::Lyrics { id, .. } = &r.req {
                    if self.lyrics_for.as_deref() == Some(id.as_str()) {
                        self.lyrics = l;
                    }
                }
            }
            Resp::Jam(j) => {
                self.jam_busy = false;
                self.jam_error = None;
                match (&r.req, &j) {
                    (Req::JamJoin(_), Some(_)) => self.status("Te has unido al Jam"),
                    (Req::JamLeave(_), _) => self.status("Has salido del Jam"),
                    (Req::JamEnd(_), _) => self.status("Jam finalizado"),
                    _ => {}
                }
                // Al salir/terminar la Jam se deja de mostrar la cola compartida y se vuelve a la
                // cola normal de la cuenta.
                if j.is_none() && self.jam_queue_active {
                    self.jam_queue_active = false;
                    self.api.send(Req::Queue);
                }
                self.jam = j;
                self.jam_at = Instant::now();
            }
            Resp::ArtistView { id, view } => {
                self.artist_views.insert(id, view);
            }
            Resp::Show { show, episodes } => {
                self.shows.insert(show.id.clone(), (show, episodes));
            }
            Resp::WebConnected => {
                self.web_busy = false;
                self.status("Web API conectada con tu app. Cargando tu biblioteca…");
                self.reload_library();
            }
            Resp::WebDisconnected => {
                self.web_busy = false;
                self.status("Web API desconectada");
            }
            Resp::Done => {
                if matches!(r.req, Req::AddToQueue(_)) {
                    self.status("Añadida a la cola");
                    self.queue_at = Instant::now() - Duration::from_secs(60);
                }
            }
        }
    }

    /// Refresca en segundo plano lo que ya se muestra desde la instantánea. Me gusta y
    /// artistas seguidos son la fuente de verdad de los corazones y de «Siguiendo» (Spotify
    /// no permite consultarlo pista a pista), así que se cargan aquí.
    fn refresh_from_network(&mut self) {
        let have_snapshot = self.lists.contains_key(LIKED);
        self.requested.retain(|k| k == "albums" || k == "artists" || k == LIKED);
        // Primero el estado del reproductor: decide la restauración de la sesión y el worker
        // de la API es secuencial (detrás del inicio y las playlists tardaba segundos).
        self.api.send(Req::PlayerState);
        self.api.send(Req::Me);
        self.api.send(Req::HomeFeed);
        self.api.send(Req::Playlists);
        self.api.send(Req::Recent);
        self.api.send(Req::FollowedArtists);
        self.requested.insert("artists".to_string());
        let stale = self.snapshot_age.map(|a| a > 6 * 3600).unwrap_or(true);
        // Instantánea incompleta (p. ej. una recarga cortada por la cuota): se recarga entera.
        let incomplete = self
            .lists
            .get(LIKED)
            .map(|l| l.total as usize > l.tracks.len() + 5)
            .unwrap_or(true);
        if have_snapshot && !stale && !incomplete {
            // Solo lo añadido recientemente: dos páginas en vez de veinte.
            self.api.send(Req::LikedRecent);
        } else {
            self.liked_refresh_pending = true;
            self.requested.insert(LIKED.to_string());
            self.api.send(Req::Liked);
        }
    }

    /// Vuelve a pedir todo lo que depende de la Web API (tras conectar la app propia).
    pub fn reload_library(&mut self) {
        self.requested.clear();
        self.playlists_loaded = false;
        // Lo que ya se ve se conserva hasta que llegue lo nuevo: si la red falla (cuota, sin
        // conexión) la biblioteca no se queda vacía ni la instantánea se guarda a ceros.
        self.lists.retain(|k, _| k == LIKED);
        self.albums.clear();
        self.artists.clear();
        self.users.clear();
        self.user_playlists.clear();
        self.snapshot_age = None;
        self.refresh_from_network();
    }

    /// Busca el objeto `Track` completo de una canción por id entre las listas ya cargadas
    /// (excluyendo la de Me gusta). Sirve para insertarla al instante en «Canciones que te gustan»
    /// al darle like sin volver a pedirla a Spotify.
    fn find_loaded_track(&self, id: &str) -> Option<Track> {
        for (key, list) in &self.lists {
            if key == LIKED {
                continue;
            }
            if let Some(t) = list.tracks.iter().find(|t| t.id.as_deref() == Some(id)) {
                return Some(t.clone());
            }
        }
        None
    }

    pub fn set_liked(&mut self, id: &str, saved: bool) {
        if saved {
            let nuevo = self.liked_set.insert(id.to_string());
            // Al dar Me gusta, la canción aparece al instante al principio de «Canciones que te
            // gustan» (sin esperar a recargar desde Spotify). Se toma el objeto Track completo de
            // alguna lista ya cargada (la que se está viendo, la cola, el álbum…).
            if nuevo {
                if let Some(track) = self.find_loaded_track(id) {
                    if let Some(list) = self.lists.get_mut(LIKED) {
                        if !list.tracks.iter().any(|t| t.id.as_deref() == Some(id)) {
                            list.tracks.insert(0, track);
                            list.total = list.total.saturating_add(1);
                        }
                    }
                }
            }
        } else {
            self.liked_set.remove(id);
            // Al quitar el like, la canción desaparece al instante de «Canciones que te gustan»
            // (no se espera a recargar desde Spotify).
            if let Some(list) = self.lists.get_mut(LIKED) {
                let before = list.tracks.len();
                list.tracks.retain(|t| t.id.as_deref() != Some(id));
                if list.tracks.len() != before {
                    list.total = list.total.saturating_sub(1);
                }
            }
        }
        if self.player.now.as_ref().and_then(|n| n.id.as_deref()) == Some(id) {
            self.player.liked = Some(saved);
        }
    }

    fn on_player_state(&mut self, st: Option<PlaybackState>) {
        if (self.restore_wanted || self.restore_awaiting || self.restore_pending.is_some()) && !self.cluster_restored {
            // Si ahora mismo suena algo en otro dispositivo, no se restaura nada aquí.
            let elsewhere = st
                .as_ref()
                .map(|s| s.is_playing && s.device.as_ref().and_then(|d| d.id.as_deref()) != Some(self.device_id.as_str()))
                .unwrap_or(false);
            if elsewhere {
                self.restore_wanted = false;
                self.restore_awaiting = false;
                self.restore_pending = None;
                self.restore_deadline = None;
                self.cluster_restored = true;
                self.restore_decided = true;
            } else {
                self.server_now = Some(st.clone());
                self.player_state_seen = true;
                self.try_decide_restore(false);
            }
        }
        let Some(st) = st else {
            if self.player.remote.is_some() {
                self.player.state = PlayState::Stopped;
                self.player.position_at = None;
                self.media_dirty = true;
            }
            return;
        };
        let dev = st.device.clone().unwrap_or_default();
        if dev.id.as_deref() == Some(self.device_id.as_str()) {
            // Suena aquí: los eventos locales mandan.
            self.player.remote = None;
            return;
        }
        self.player.remote = Some(dev.clone());
        if let Some(t) = &st.item {
            self.set_now_playing(NowPlaying::from_track(t));
        }
        let new_state = if st.is_playing {
            PlayState::Playing
        } else {
            PlayState::Paused
        };
        if new_state != self.player.state {
            self.media_dirty = true;
        }
        self.player.state = new_state;
        self.player.position_ms = st.progress_ms.unwrap_or(0);
        self.player.position_at = st.is_playing.then(Instant::now);
        self.player.shuffle = st.shuffle_state;
        self.player.repeat = match st.repeat_state.as_str() {
            "track" => Repeat::Track,
            "context" => Repeat::Context,
            _ => Repeat::Off,
        };
        if let (Some(v), None) = (dev.volume_percent, self.volume_drag) {
            let raw = vol_pct_to_raw(v.min(100) as f32);
            if self.accept_volume_echo(raw) {
                self.player.volume = raw;
            }
        }
    }

    fn diag_tick(&mut self, ctx: &egui::Context) {
        if !self.diag || !self.logged_in() {
            return;
        }
        let started = *self.diag_started.get_or_insert_with(Instant::now);
        let t = started.elapsed().as_secs();
        ctx.request_repaint_after(Duration::from_millis(500));
        if self.diag_step == 0 {
            log::info!("[diag] inicio: dispositivos, cola, letras y reproducción de prueba");
            self.api.send(Req::Devices);
            self.api.send(Req::Queue);
            self.api.send(Req::LikedRecent);
            self.api.send(Req::Search("daft punk".to_string()));
            self.api.send(Req::Album("4m2880jivSbbyEGAKfITCa".to_string()));
            self.api.send(Req::Artist("4tZwfgrHOc3mvqYlEYSvVi".to_string()));
            self.api.send(Req::ArtistTop("4tZwfgrHOc3mvqYlEYSvVi".to_string()));
            self.api.send(Req::ArtistAlbums("4tZwfgrHOc3mvqYlEYSvVi".to_string()));
            self.set_volume(vol_pct_to_raw(35.0));
            self.play(PlayTarget::Tracks {
                uris: vec!["spotify:track:4uLU6hMCjMI75M1A2tKUQC".to_string()],
                index: Some(0),
                shuffle: false,
            });
            self.diag_step = 1;
        } else if self.diag_step == 1 && t >= 12 {
            log::info!(
                "[diag] estado tras 12 s: {:?}, posición {} ms, pista {:?}, remoto {:?}",
                self.player.state,
                self.player.position(),
                self.player.now.as_ref().map(|n| n.name.clone()),
                self.player.remote.as_ref().map(|d| d.name.clone())
            );
            log::info!("[diag] volumen -> 0 (t0)");
            self.set_volume(0);
            if self.player.state == PlayState::Playing {
                self.play_pause();
            }
            self.ensure_lyrics();
            self.diag_step = 2;
        } else if self.diag_step == 2 && t >= 15 {
            log::info!("[t] cierre solicitado");
            log::info!(
                "[diag] letras: {:?}",
                self.lyrics.as_ref().map(|l| (l.sync_type.clone(), l.lines.len()))
            );
            log::info!("[diag] fin");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            self.diag_step = 3;
        }
    }

    fn tick(&mut self, ctx: &egui::Context) {
        self.diag_tick(ctx);
        // Comprobación de versiones programada (la interfaz solo repinta cuando hace falta, así
        // que se pide un repintado para el instante previsto).
        if let Some(at) = self.update_check_at {
            let now = Instant::now();
            if now >= at {
                self.update_check_at = Some(now + Duration::from_secs(6 * 3600));
                self.check_updates(false);
            } else {
                ctx.request_repaint_after(at - now);
            }
        }
        self.flush_volume(ctx);
        self.poll_restore_queue(ctx);
        // Precarga suave de playlists (una cada 250 ms) para que abrirlas sea instantáneo.
        if let Some(at) = self.prefetch_at {
            if Instant::now() >= at && self.logged_in() {
                while let Some(id) = self.prefetch_ids.pop_front() {
                    if self.lists.contains_key(&id) || self.requested.contains(&format!("pl:{id}")) {
                        continue;
                    }
                    self.request_once(&format!("pl:{id}"), Req::PlaylistTracks(id.clone()));
                    break;
                }
                self.prefetch_at = if self.prefetch_ids.is_empty() {
                    None
                } else {
                    Some(Instant::now() + Duration::from_millis(500))
                };
                if self.prefetch_at.is_some() {
                    ctx.request_repaint_after(Duration::from_millis(500));
                }
            }
        }
        // Cola tras restaurar: reintentos cada ~1,2 s hasta que llegue con contenido (máx. 8).
        if let Some((at, n)) = self.queue_retry {
            if Instant::now() >= at {
                self.api.send(Req::Queue);
                self.queue_at = Instant::now();
                self.queue_retry = if n < 12 { Some((Instant::now() + Duration::from_millis(300), n + 1)) } else { None };
            }
            ctx.request_repaint_after(Duration::from_millis(300));
        }
        // Transferencia aceptada pero sin pista (sesión vacía o sin contexto): copia local.
        if let Some(t) = self.restore_fallback_at {
            if Instant::now() >= t {
                self.restore_fallback_at = None;
                self.restore_mark = false;
                self.pause_after_restore = false;
                if self.restore_pending.is_some() {
                    log::info!("[restore] la sesión de Spotify no trajo ninguna pista; se usa la copia local");
                    self.restore_local();
                }
            }
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        // Restauración de la sesión anterior si el sondeo remoto no llegó a responder.
        if let Some(t) = self.restore_deadline {
            if !self.restore_decided && !self.restore_awaiting && self.logged_in() && Instant::now() >= t {
                self.try_decide_restore(true);
            } else if self.restore_awaiting && self.logged_in() && Instant::now() >= t {
                // Se pidio la sesion al servidor pero no llego: copia local de respaldo.
                self.restore_deadline = None;
                self.restore_awaiting = false;
                self.cluster_restored = true;
                self.restore_local();
            }
            ctx.request_repaint_after(Duration::from_millis(300));
        }
        // Copia periódica de lo que suena (por si la app no llega a cerrarse limpiamente).
        if self.player.state == PlayState::Playing && self.playback_saved_at.elapsed() > Duration::from_secs(30) {
            self.save_playback();
        }
        if let Some(t) = self.stall_test {
            if t.elapsed() > Duration::from_secs(6) {
                self.stall_test = None;
                log::info!("[stall] simulando sesión estancada");
                self.backend.send(Cmd::Stalled);
            }
            ctx.request_repaint_after(Duration::from_secs(1));
        }
        // Vigilante: si «cargando» dura más de 20 s, la sesión con Spotify suele estar muerta
        // (equipo dormido, red caída, sesión caducada). Se pide al backend que reconecte.
        if self.player.state == PlayState::Loading && self.player.remote.is_none() {
            let since = *self.loading_since.get_or_insert_with(Instant::now);
            if since.elapsed() > Duration::from_secs(20) {
                self.loading_since = Some(Instant::now());
                self.backend.send(Cmd::Stalled);
            }
            ctx.request_repaint_after(Duration::from_secs(1));
        } else {
            self.loading_since = None;
        }
        #[cfg(windows)]
        {
            // Una sola vez, pasados 4 s: devuelve al sistema las páginas que solo se usaron al
            // arrancar (inicialización de DLLs, descompresión de fuentes…).
            if !self.ws_trimmed && self.taskbar_at.elapsed() > Duration::from_secs(4) {
                self.ws_trimmed = true;
                let atlas = ctx.fonts(|f| f.font_image_size());
                log::info!(
                    "[mem] atlas de fuentes {}x{} ({:.1} MB en f32), portadas {:.1} MB en {} texturas, me gusta {} pistas, feed {} secciones, historial local {} entradas",
                    atlas[0],
                    atlas[1],
                    (atlas[0] * atlas[1] * 4) as f64 / 1e6,
                    self.images.resident_bytes() as f64 / 1e6,
                    self.images.resident(),
                    self.lists.get(LIKED).map(|l| l.tracks.len()).unwrap_or(0),
                    self.home_feed.len(),
                    self.play_log.entries.len()
                );
                unsafe {
                    let _ = windows::Win32::System::ProcessStatus::K32EmptyWorkingSet(windows::Win32::System::Threading::GetCurrentProcess());
                }
            }
            if let Some(t) = self.ws_trim_at {
                if Instant::now() >= t {
                    self.ws_trim_at = None;
                    unsafe {
                        let _ = windows::Win32::System::ProcessStatus::K32EmptyWorkingSet(windows::Win32::System::Threading::GetCurrentProcess());
                    }
                } else {
                    ctx.request_repaint_after(t - Instant::now());
                }
            }
            if !self.taskbar_ready && self.taskbar_at.elapsed() > Duration::from_millis(1200) {
                self.taskbar_ready = true;
                if let Some(h) = self.hwnd {
                    if crate::taskbar::install(h, self.ui_tx.clone()) {
                        self.taskbar_playing = None;
                    }
                }
            }
            if self.taskbar_ready {
                let playing = self.player.state == PlayState::Playing;
                if self.taskbar_playing != Some(playing) {
                    self.taskbar_playing = Some(playing);
                    crate::taskbar::set_playing(playing);
                }
            }
        }
        if let Some((at, pos, adds)) = self.pending_queue.clone() {
            if Instant::now() >= at {
                self.pending_queue = None;
                self.seek(pos);
                for u in adds {
                    self.api.send(Req::AddToQueue(u));
                }
                self.api.send(Req::Queue);
                self.status("Cola actualizada");
            }
        }
        if let Some(t) = self.sleep_at {
            if Instant::now() >= t {
                self.sleep_at = None;
                if self.player.state == PlayState::Playing {
                    self.play_pause();
                }
                self.status("Temporizador: reproducción pausada");
            }
        }
        if self.logged_in() {
            // Sondeo del estado remoto: frecuente solo cuando controlamos otro dispositivo.
            let interval = if self.player.remote.is_some() {
                3
            } else if self.player.state == PlayState::Playing {
                90
            } else {
                45
            };
            if self.last_remote_poll.elapsed() > Duration::from_secs(interval) {
                self.last_remote_poll = Instant::now();
                self.api.send(Req::PlayerState);
            }
            ctx.request_repaint_after(Duration::from_secs(interval));

            // La cola se refresca a menudo solo cuando controlamos otro dispositivo; en local
            // cambia poco y cada consulta gasta presupuesto de la Web API.
            let queue_interval = if self.player.remote.is_some() { 6 } else { 20 };
            if self.side == Some(SideTab::Queue) && self.queue_at.elapsed() > Duration::from_secs(queue_interval)
            {
                self.queue_at = Instant::now();
                self.api.send(Req::Queue);
            }
            if self.jam_open && self.jam.is_some() && self.jam_at.elapsed() > Duration::from_secs(10)
            {
                self.jam_at = Instant::now();
                self.api.send(Req::JamCurrent);
            }
        }
        if self.player.state == PlayState::Playing {
            // Barra de progreso: una vez por segundo (el tiempo cambia por segundos). Letras
            // sincronizadas: justo cuando cambia la línea, no a intervalos fijos.
            let mut delay = Duration::from_millis(1000);
            if self.side == Some(SideTab::Lyrics) {
                if let Some(l) = self.lyrics.as_ref().filter(|l| l.synced()) {
                    let pos = self.player.position();
                    if let Some(next) = l.lines.iter().map(|x| x.start_ms).find(|&s| s > pos) {
                        delay = delay.min(Duration::from_millis((next - pos).max(30) as u64));
                    }
                }
            }
            ctx.request_repaint_after(delay);
        }
        if let Some((_, at, _)) = &self.status {
            if at.elapsed() > Duration::from_secs(8) {
                self.status = None;
            } else {
                ctx.request_repaint_after(Duration::from_secs(1));
            }
        }
        self.frame_ms = ctx
            .data(|d| d.get_temp::<f32>(egui::Id::new(FRAME_MS_KEY)))
            .unwrap_or(0.0);
        if self.frame_ms > 0.0 {
            if self.frame_hist.len() == 240 {
                self.frame_hist.pop_front();
            }
            self.frame_hist.push_back(self.frame_ms);
            if let Some(ph) = ctx.data(|d| d.get_temp::<[f32; 4]>(egui::Id::new(FRAME_PHASES_KEY))) {
                for k in 0..4 {
                    self.frame_phases[k] += ph[k];
                }
            }
        }
        self.save_snapshot_if_needed(false);
        self.save_play_log_if_needed(false);
    }

    /// Archivos soltados sobre la ventana: imagen de playlist (página propia o editor).
    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let files: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        let Some(path) = files.into_iter().find(|p| is_image_path(p)) else {
            return;
        };
        if let Some(ed) = self.editor.as_mut() {
            ed.image_path = Some(path);
            return;
        }
        if let Page::Playlist(id) = self.page().clone() {
            let mine = self
                .playlists
                .iter()
                .find(|p| p.id == id)
                .map(|p| self.is_mine(p))
                .unwrap_or(false);
            if mine {
                self.api.send(Req::SetPlaylistImage { id, path });
                self.status("Subiendo la imagen de la playlist…");
            } else {
                self.status_err("Solo puedes cambiar la imagen de tus propias playlists");
            }
        }
    }

    // --------------------------------------------------------------- acciones

    fn apply_actions(&mut self, ctx: &egui::Context) {
        let actions = std::mem::take(&mut self.actions);
        for a in actions {
            match a {
                Action::Go(p) => self.go(p),
                Action::OpenPlaylist(p) => {
                    let id = p.id.clone();
                    self.playlist_meta.insert(id.clone(), p);
                    self.go(Page::Playlist(id));
                }
                Action::Play(t) => self.play(t),
                Action::Like(id, on) => self.like(id, on),
                Action::SaveAlbum(id, on) => {
                    if self.logged_in() {
                        self.api.send(if on { Req::SaveAlbum(id) } else { Req::UnsaveAlbum(id) });
                    }
                }
                Action::OpenRadio(id) => {
                    if self.logged_in() {
                        self.status("Abriendo la radio…");
                        self.api.send(Req::RadioPlaylist(id));
                    }
                }
                Action::Download(ids) => self.download_kind(ids, false),
                Action::DownloadEpisodes(ids) => self.download_kind(ids, true),
                Action::FollowShow(id, on) => {
                    if self.logged_in() {
                        self.api.send(Req::FollowShow(id, on));
                    }
                }
                Action::SaveEpisode(id, on) => {
                    if self.logged_in() {
                        self.api.send(Req::SaveEpisode(id, on));
                    }
                }
                Action::CopyText(text, what) => {
                    ctx.copy_text(text);
                    self.status(format!("{what} copiado"));
                }
                Action::AddToQueue(uri) => {
                    if self.logged_in() {
                        if self.player.remote.is_none() {
                            self.queued_local.push(uri.clone());
                        }
                        self.api.send(Req::AddToQueue(uri));
                    }
                }
                Action::AddToPlaylist { playlist_id, uri } => {
                    self.api.send(Req::AddToPlaylist {
                        id: playlist_id,
                        uris: vec![uri],
                    });
                }
                Action::RemoveFromPlaylist { playlist_id, uri } if playlist_id == "queue" => self.queue_remove(&uri),
                Action::RemoveFromPlaylist { playlist_id, uri } => {
                    self.api.send(Req::RemoveFromPlaylist {
                        id: playlist_id,
                        uris: vec![uri],
                    });
                }
                Action::Follow { kind, id, on } => {
                    self.following.insert(format!("{kind}:{id}"), on);
                    self.api.send(if on {
                        Req::Follow { kind, id }
                    } else {
                        Req::Unfollow { kind, id }
                    });
                }
                Action::FollowPlaylist(id, on) => {
                    self.api.send(if on {
                        Req::FollowPlaylist(id)
                    } else {
                        Req::UnfollowPlaylist(id)
                    });
                }
                Action::OpenEditor(p) => {
                    if let Some(p) = &p {
                        self.request_once(&format!("members:{}", p.id), Req::Members(p.id.clone()));
                    }
                    self.editor = Some(match p {
                        Some(p) => PlaylistEditor {
                            id: Some(p.id.clone()),
                            name: p.name.clone(),
                            description: p.description.clone().unwrap_or_default(),
                            public: p.public.unwrap_or(true),
                            collaborative: p.collaborative.unwrap_or(false),
                            image_path: None,
                            busy: false,
                        },
                        None => PlaylistEditor {
                            id: None,
                            name: "Nueva playlist".to_string(),
                            description: String::new(),
                            public: true,
                            collaborative: false,
                            image_path: None,
                            busy: false,
                        },
                    });
                }
                Action::PickPlaylistImage(id) => {
                    if let Some(path) = pick_image_file() {
                        self.api.send(Req::SetPlaylistImage { id, path });
                        self.status("Subiendo la imagen de la playlist…");
                    }
                }
                Action::OpenLink(link) => self.open_link(&link),
                Action::OpenInTab(page) => {
                    self.open_tab(page);
                }
                Action::CloseTab(i) => self.close_tab(i),
                Action::ActivateTab(i) => {
                    if i < self.tabs.len() {
                        self.active = ActiveTab::Tab(i);
                        self.selected = None;
                    }
                }
                Action::Pin(id, on) => {
                    self.settings.pinned.retain(|x| x != &id);
                    if on {
                        self.settings.pinned.insert(0, id);
                    }
                    self.settings.save(&self.paths);
                }
            }
        }
    }

    /// Navega a un enlace/uri de Spotify pegado en la búsqueda o recibido del sistema.
    pub fn open_link(&mut self, link: &str) {
        let Some((kind, id)) = parse_spotify_link(link) else {
            return;
        };
        match kind.as_str() {
            "track" => {
                let shuffle = self.player.shuffle;
                self.play(PlayTarget::Tracks {
                    uris: vec![format!("spotify:track:{id}")],
                    index: Some(0),
                    shuffle,
                })
            }
            "album" => self.go(Page::Album(id)),
            "artist" => self.go(Page::Artist(id)),
            "playlist" => self.go(Page::Playlist(id)),
            "user" => self.go(Page::User(id)),
            "show" => self.go(Page::Show(id)),
            "episode" => {
                let shuffle = self.player.shuffle;
                self.play(PlayTarget::Tracks { uris: vec![format!("spotify:episode:{id}")], index: Some(0), shuffle })
            }
            "socialsession" => {
                self.jam_open = true;
                self.jam_link = link.to_string();
                self.jam_join(&id);
            }
            _ => self.status_err(format!("Tipo de enlace no soportado: {kind}")),
        }
    }

    pub fn jam_join(&mut self, token: &str) {
        if !self.logged_in() {
            self.status_err("Inicia sesión para unirte a un Jam");
            return;
        }
        self.jam_busy = true;
        self.jam_error = None;
        self.api.send(Req::JamJoin(token.to_string()));
    }

    /// Reconstruye la cola: recarga el contexto actual en la misma canción y posición (la cola
    /// queda vacía) y vuelve a añadir `keep`. Spotify no ofrece "quitar de la cola" a apps
    /// externas, así que solo se conocen las canciones añadidas desde Nanofy.
    fn queue_rebuild(&mut self, keep: Vec<String>) {
        if self.player.remote.is_some() {
            self.status_err("La cola solo se puede editar cuando la música suena en Nanofy");
            return;
        }
        let Some(now) = self.player.now.clone() else { return };
        let Some(mut target) = self.last_play.clone() else {
            self.status_err("No se puede reconstruir la cola: la reproducción no empezó desde Nanofy");
            return;
        };
        match &mut target {
            PlayTarget::Context { track_uri, index, shuffle, .. } => {
                *track_uri = Some(now.uri.clone());
                *index = None;
                *shuffle = self.player.shuffle;
            }
            PlayTarget::Tracks { uris, index, shuffle } => {
                *index = uris.iter().position(|u| u == &now.uri).map(|i| i as u32);
                *shuffle = self.player.shuffle;
            }
        }
        let pos = self.player.position();
        self.play(target);
        self.queued_local = keep.clone();
        self.pending_queue = Some((Instant::now() + Duration::from_millis(1500), pos, keep));
        self.queue = None;
    }

    /// Doble clic en la cola: si la pista pertenece al contexto en curso se salta a ella dentro
    /// del contexto (la radio/playlist sigue igual); si es una canción añadida a mano, se
    /// reproduce ella y lo que venía detrás. El origen mostrado en «Siguientes de» no cambia.
    pub fn play_from_queue(&mut self, uri: &str, rows: &[Track], i: usize) {
        let keep_play = self.last_play.clone();
        let keep_page = self.last_play_page.clone();
        let shuffle = self.player.shuffle;
        let target = match &keep_play {
            Some(PlayTarget::Context { uri: ctx, .. }) if !self.queued_local.iter().any(|u| u == uri) => {
                PlayTarget::Context { uri: ctx.clone(), track_uri: Some(uri.to_string()), index: None, shuffle }
            }
            Some(PlayTarget::Tracks { uris, .. }) if uris.iter().any(|u| u == uri) => {
                PlayTarget::Tracks { uris: uris.clone(), index: uris.iter().position(|u| u == uri).map(|p| p as u32), shuffle }
            }
            _ => PlayTarget::Tracks { uris: rows.iter().skip(i).map(|t| t.uri.clone()).collect(), index: Some(0), shuffle },
        };
        self.play(target);
        // Saltar dentro de la cola no cambia el origen: se restaura EXACTAMENTE el que había
        // (nunca la página actual, que sería p. ej. «Inicio»).
        if keep_play.is_some() {
            self.last_play = keep_play;
        }
        self.last_play_page = keep_page;
    }

    pub fn queue_remove(&mut self, uri: &str) {
        let Some(i) = self.queued_local.iter().position(|u| u == uri) else {
            self.status_err("Solo se pueden quitar las canciones añadidas a la cola desde Nanofy");
            return;
        };
        let mut keep = self.queued_local.clone();
        keep.remove(i);
        self.queue_rebuild(keep);
    }

    pub fn queue_clear(&mut self) {
        self.queue_rebuild(Vec::new());
    }

    /// Lee `playback.json` y deja la barra como estaba al cerrar (en pausa, mismo segundo).
    fn load_saved_playback(&mut self) {
        let Ok(text) = std::fs::read_to_string(&self.playback_path) else { return };
        let Ok(saved) = serde_json::from_str::<SavedPlayback>(&text) else { return };
        if saved.now.uri.is_empty() {
            return;
        }
        self.player.now = Some(saved.now.clone());
        self.player.liked = saved.now.id.as_ref().map(|id| self.liked_set.contains(id));
        self.player.state = PlayState::Paused;
        self.player.position_ms = saved.position_ms;
        self.player.position_at = None;
        self.player.shuffle = saved.shuffle;
        self.player.repeat = saved.repeat;
        self.last_play = Some(saved.target.clone());
        self.queued_local = saved.queued.clone();
        if !saved.queue.is_empty() {
            self.queue = Some(QueueResponse {
                currently_playing: Some(track_from_now(&saved.now)),
                queue: saved.queue.clone(),
            });
        }
        self.restore_pending = Some(saved);
        self.media_dirty = true;
    }

    /// Restaura al abrir: pide a Spotify la sesión de la cuenta (posición, cola y opciones
    /// incluidas). Si no la hay, se usa la copia local (`restore_local`).
    #[allow(dead_code)]
    fn restore_now(&mut self) {
        self.restore_wanted = false;
        self.restore_awaiting = false;
        self.restore_local();
    }

    /// Decide si restaurar la sesión del servidor (más reciente) o la copia local. Espera a
    /// tener el estado del reproductor y el historial del servidor, salvo que `force` (plazo).
    fn try_decide_restore(&mut self, force: bool) {
        if self.restore_decided || self.cluster_restored {
            return;
        }
        if !(self.restore_wanted || self.restore_pending.is_some()) {
            return;
        }
        // Se espera a /me/player (sesión activa exacta) y a recently-played (última sesión de
        // la cuenta, que sobrevive aunque el dispositivo se apague). El plazo de respaldo llama
        // con force=true si alguna no llega.
        // También al estado del clúster de Connect: es lo único que conserva la posición exacta
        // de lo que quedó en pausa en otro dispositivo (el teléfono) aunque ya esté cerrado.
        let web_pending = self.api.web_configured() && (self.server_now.is_none() || self.server_last.is_none());
        if !force && (web_pending || self.server_cluster.is_none()) {
            return;
        }
        self.restore_decided = true;
        self.restore_deadline = None;
        let local_at = self.restore_pending.as_ref().map(|s| s.saved_at).unwrap_or(0);
        let local_track = self.restore_pending.as_ref().map(|s| s.now.uri.clone());
        let local_ctx = self.restore_pending.as_ref().and_then(|s| match &s.target {
            PlayTarget::Context { uri, .. } => Some(uri.clone()),
            _ => None,
        });
        // 1) Sesión activa ahora mismo en la cuenta (/me/player): posición y cola exactas.
        let now = self.server_now.clone().flatten().filter(|s| s.item.is_some());
        if let Some(st) = now {
            let s_track = st.item.as_ref().map(|t| t.uri.clone());
            let s_ctx = st.context.as_ref().map(|c| c.uri.clone());
            if self.restore_pending.is_none() || s_track != local_track || s_ctx != local_ctx {
                log::info!("[restore] /me/player tiene una sesión distinta; se sincroniza (exacta)");
                self.restore_wanted = false;
                self.restore_from_server(st);
                return;
            }
        }
        // 2) Estado del clúster de Connect: lo último que quedó en pausa en cualquier
        //    dispositivo, con su posición exacta. Manda si es más reciente que la copia local.
        if let Some(Some(c)) = self.server_cluster.clone() {
            let elsewhere = !c.active_device_id.is_empty() && c.active_device_id != self.device_id;
            if elsewhere && c.is_playing && !c.is_paused {
                log::info!("[restore] suena en otro dispositivo; aquí no se carga nada");
                self.restore_wanted = false;
                self.restore_pending = None;
                self.cluster_restored = true;
                return;
            }
            let c_at = (c.timestamp_ms.max(0) / 1000) as u64;
            let newer = c_at > local_at.saturating_add(20);
            if self.restore_pending.is_none() || newer {
                log::info!("[restore] el clúster tiene la sesión más reciente ({c_at} > {local_at}): {} en {} ms", c.track_uri, c.position_ms);
                self.restore_wanted = false;
                self.restore_pending = None;
                self.cluster_restored = true;
                self.restore_from_cluster(c);
                return;
            }
        }
        // 3) Sin sesión activa: la última que registró la cuenta (recently-played) si es más
        //    reciente que la copia local y de otro contexto/pista (algo se oyó en otro sitio).
        let last = self.server_last.clone().flatten();
        if let Some(l) = last {
            let newer = l.played_at > local_at.saturating_add(20);
            let different = l.context_uri != local_ctx || Some(&l.track_uri) != local_track.as_ref();
            if self.restore_pending.is_none() || (newer && different) {
                log::info!("[restore] recently-played más reciente ({} > {}); se abre esa sesión", l.played_at, local_at);
                self.restore_wanted = false;
                self.restore_from_recent(l);
                return;
            }
        }
        if self.restore_pending.is_none() {
            // Sin copia local ni estado en el servidor: se pide la sesión por transferencia de
            // Connect (como antes), por si Spotify aún la conserva.
            if self.logged_in() && !self.restore_awaiting {
                self.restore_wanted = false;
                self.restore_awaiting = true;
                crate::tmark("sesión: pedida a Spotify");
                self.backend.send(Cmd::ResumeSession);
                self.restore_deadline = Some(Instant::now() + Duration::from_secs(5));
            }
            return;
        }
        log::info!("[restore] la copia local es la más reciente o la única; se restaura localmente");
        self.restore_wanted = false;
        self.restore_local();
    }

    /// Abre la última sesión que registró la cuenta (recently-played), en pausa al inicio de la
    /// pista. Sin posición ni cola exactas (la Web API no las da cuando el dispositivo se apagó),
    /// pero recupera el contexto y la pista correctos entre dispositivos.
    fn restore_from_recent(&mut self, last: crate::model::ServerLast) {
        let cmd = match &last.context_uri {
            Some(uri) if !uri.is_empty() && uri != "-" => Cmd::LoadContext {
                uri: uri.clone(),
                track_uri: Some(last.track_uri.clone()),
                index: None,
                shuffle: self.player.shuffle,
                resume: Some(0),
            },
            _ => Cmd::LoadTracks { uris: vec![last.track_uri.clone()], index: Some(0), shuffle: false, resume: Some(0) },
        };
        self.last_play = match &last.context_uri {
            Some(uri) if !uri.is_empty() && uri != "-" => Some(PlayTarget::Context { uri: uri.clone(), track_uri: Some(last.track_uri.clone()), index: None, shuffle: self.player.shuffle }),
            _ => Some(PlayTarget::Tracks { uris: vec![last.track_uri.clone()], index: Some(0), shuffle: false }),
        };
        self.now_placeholder = false;
        self.restore_mark = true;
        self.restore_pending = None;
        self.player.position_ms = 0;
        self.player.state = PlayState::Paused;
        // La canción se ve al instante (metadatos del historial), sin esperar a que cargue.
        if let Some(t) = &last.track {
            let np = NowPlaying::from_track(t);
            self.player.liked = np.id.as_ref().map(|id| self.liked_set.contains(id));
            self.player.now = Some(np);
        }
        self.backend.send(cmd);
        // La cola se arma con las pistas del propio contexto (la Web API no da cola de una
        // sesión en pausa); se piden y se construye al llegar.
        self.prepare_context_queue(last.context_uri.as_deref(), &last.track_uri);
        self.media_dirty = true;
        log::info!("[restore] recently-played: {} (ctx {:?})", last.track_uri, last.context_uri);
    }


    /// Prepara la cola desde las pistas del contexto (playlist/álbum): si ya están en memoria
    /// las usa; si no, las pide y se arma al llegar (Resp::Tracks / Resp::Album).
    fn prepare_context_queue(&mut self, context_uri: Option<&str>, current: &str) {
        let Some(uri) = context_uri else { return };
        let Some(id) = uri.rsplit(':').next().map(|s| s.to_string()) else { return };
        let is_album = uri.contains(":album:");
        self.restore_ctx = Some((id.clone(), current.to_string()));
        if self.build_context_queue(&id, current) {
            self.restore_ctx = None;
            return;
        }
        // La petición de pistas necesita la sesión de librespot; el tick la lanza y reintenta
        // en cuanto haya sesión.
        let _ = is_album;
        self.restore_ctx_at = Some(Instant::now());
    }

    /// Pide las pistas del contexto en restauración cuando la sesión está lista, y arma la cola.
    fn poll_restore_queue(&mut self, ctx_ui: &egui::Context) {
        let Some((id, cur)) = self.restore_ctx.clone() else { return };
        if self.build_context_queue(&id, &cur) {
            self.restore_ctx = None;
            self.restore_ctx_at = None;
            return;
        }
        if !self.logged_in() {
            ctx_ui.request_repaint_after(Duration::from_millis(300));
            return;
        }
        if let Some(at) = self.restore_ctx_at {
            if Instant::now() >= at {
                // Reintenta cada 1.2 s (la primera petición puede fallar si la sesión aún no está).
                self.restore_ctx_at = Some(Instant::now() + Duration::from_millis(1200));
                self.requested.remove(&format!("pl:{id}"));
                self.requested.remove(&format!("album:{id}"));
                if id.starts_with("37i9") || id.len() == 22 {
                    self.api.send_priority(Req::PlaylistTracks(id.clone()));
                }
            }
            ctx_ui.request_repaint_after(Duration::from_millis(400));
        }
    }

    /// Arma `self.queue` con las pistas del contexto tras la actual. Devuelve true si lo logró.
    fn build_context_queue(&mut self, ctx_id: &str, current: &str) -> bool {
        let tracks: Vec<Track> = match self.lists.get(ctx_id) {
            Some(l) if !l.tracks.is_empty() => l.tracks.clone(),
            _ => match self.albums.get(ctx_id).and_then(|a| a.tracks.as_ref()) {
                Some(p) if !p.items.is_empty() => p.items.clone(),
                _ => {
                    log::debug!("[cola-ctx] {ctx_id}: aún sin pistas");
                    return false;
                }
            },
        };
        log::debug!("[cola-ctx] {ctx_id}: {} pistas, actual={current}", tracks.len());
        let cur = tracks.iter().position(|t| t.uri == current).unwrap_or(0);
        let upcoming: Vec<Track> = tracks.iter().skip(cur + 1).take(80).cloned().collect();
        if upcoming.is_empty() {
            return false;
        }
        let n = upcoming.len();
        self.queue = Some(QueueResponse {
            currently_playing: tracks.get(cur).cloned(),
            queue: upcoming,
        });
        crate::tmark(&format!("cola del contexto: {n} pistas ({} ms)", crate::since_start_ms()));
        true
    }

    /// Restaura la sesión actual de la cuenta leída de /me/player (contexto, pista, posición,
    /// aleatorio y repetición), en pausa. La cola llega por Req::Queue.
    fn restore_from_server(&mut self, st: crate::model::PlaybackState) {
        let Some(item) = st.item.clone() else { self.restore_local(); return };
        let pos = st.progress_ms.unwrap_or(0);
        let ctx = st.context.as_ref().map(|c| c.uri.clone()).filter(|u| !u.is_empty());
        let shuffle = st.shuffle_state;
        let cmd = match &ctx {
            Some(uri) => Cmd::LoadContext {
                uri: uri.clone(),
                track_uri: Some(item.uri.clone()),
                index: None,
                shuffle,
                resume: Some(pos),
            },
            None => Cmd::LoadTracks { uris: vec![item.uri.clone()], index: Some(0), shuffle: false, resume: Some(pos) },
        };
        if let Some(uri) = &ctx {
            self.last_play = Some(PlayTarget::Context { uri: uri.clone(), track_uri: Some(item.uri.clone()), index: None, shuffle });
            self.ensure_context_meta(&uri.clone());
        } else {
            self.last_play = Some(PlayTarget::Tracks { uris: vec![item.uri.clone()], index: Some(0), shuffle: false });
        }
        // La barra muestra ya la pista de la cuenta.
        let np = NowPlaying::from_track(&item);
        self.player.liked = np.id.as_ref().map(|id| self.liked_set.contains(id));
        self.player.now = Some(np);
        self.player.position_ms = pos;
        self.player.state = PlayState::Paused;
        self.player.shuffle = shuffle;
        self.player.repeat = match st.repeat_state.as_str() {
            "track" => Repeat::Track,
            "context" => Repeat::Context,
            _ => Repeat::Off,
        };
        self.now_placeholder = false;
        self.restore_mark = true;
        self.restore_pending = None;
        self.backend.send(cmd);
        if self.player.repeat != Repeat::Off {
            self.backend.send(Cmd::Repeat { context: true, track: self.player.repeat == Repeat::Track });
        }
        // La cola: la exacta del servidor si la sesión sigue activa, y de respaldo la del contexto.
        self.api.send(Req::Queue);
        self.queue = None;
        self.queue_at = Instant::now();
        self.queue_retry = Some((Instant::now() + Duration::from_millis(400), 0));
        self.prepare_context_queue(ctx.as_deref(), &item.uri);
        self.media_dirty = true;
        log::info!("[restore] servidor: {} en {} ms (ctx {:?})", item.name, pos, ctx);
    }

        /// Carga en pausa la sesión de la cuenta tal como la tiene Spotify (clúster de Connect):
    /// contexto, pista, posición, aleatorio, repetición y cola manual.
    fn restore_from_cluster(&mut self, info: crate::backend::ClusterInfo) {
        let pos = info.position_ms;
        let cmd = if !info.context_uri.is_empty() && info.context_uri != "-" {
            Cmd::LoadContext {
                uri: info.context_uri.clone(),
                track_uri: Some(info.track_uri.clone()),
                index: None,
                shuffle: info.shuffle,
                resume: Some(pos),
            }
        } else {
            let mut uris = vec![info.track_uri.clone()];
            uris.extend(info.next.iter().filter(|u| !info.queue.contains(u)).cloned());
            Cmd::LoadTracks { uris, index: Some(0), shuffle: false, resume: Some(pos) }
        };
        if let Cmd::LoadContext { uri, track_uri, index, shuffle, .. } = &cmd {
            self.last_play = Some(PlayTarget::Context { uri: uri.clone(), track_uri: track_uri.clone(), index: *index, shuffle: *shuffle });
            self.ensure_context_meta(&uri.clone());
        }
        self.restore_mark = true;
        self.now_placeholder = false;
        self.backend.send(cmd);
        if info.repeat_context || info.repeat_track {
            self.backend.send(Cmd::Repeat { context: info.repeat_context, track: info.repeat_track });
        }
        self.player.shuffle = info.shuffle;
        self.player.repeat = if info.repeat_track {
            Repeat::Track
        } else if info.repeat_context {
            Repeat::Context
        } else {
            Repeat::Off
        };
        if !info.queue.is_empty() {
            self.queued_local = info.queue.clone();
            self.pending_queue = Some((Instant::now() + Duration::from_millis(2500), pos, info.queue));
        }
        log::info!("[restore] sesión del clúster: {} en {} ms ({})", info.track_uri, pos, info.context_uri);
    }

    /// Pide el nombre del contexto (álbum, playlist, artista) para que «Siguientes de: …» lo
    /// muestre aunque nunca se haya abierto su página en esta sesión.
    fn ensure_context_meta(&mut self, uri: &str) {
        let mut parts = uri.splitn(3, ':');
        let (Some(_), Some(kind), Some(id)) = (parts.next(), parts.next(), parts.next()) else { return };
        let id = id.to_string();
        let (key, req) = match kind {
            "album" if !self.albums.contains_key(&id) => (format!("album:{id}"), Req::Album(id)),
            "playlist" if !self.playlist_meta.contains_key(&id) && !self.playlists.iter().any(|p| p.id == id) => (format!("plmeta:{id}"), Req::PlaylistMeta(id)),
            "artist" if !self.artists.contains_key(&id) => (format!("artistmeta:{id}"), Req::Artist(id)),
            _ => return,
        };
        if self.requested.insert(key) {
            self.api.send(req);
        }
    }

    /// Carga en el reproductor (en pausa) lo guardado, con aleatorio, repetición y cola manual.
    fn restore_local(&mut self) {
        let Some(saved) = self.restore_pending.take() else { return };
        self.restore_deadline = None;
        self.now_placeholder = false;
        self.restore_mark = true;
        let pos = saved.position_ms;
        let cmd = match saved.target.clone() {
            // `saved.now` es la pista que sonaba al cerrar (puede no ser la que inició el
            // contexto): se resume esa, no la original de `target`.
            PlayTarget::Context { uri, shuffle, .. } => Cmd::LoadContext {
                uri,
                track_uri: Some(saved.now.uri.clone()),
                index: None,
                shuffle,
                resume: Some(pos),
            },
            PlayTarget::Tracks { uris, index, shuffle } => {
                let index = uris.iter().position(|u| u == &saved.now.uri).map(|i| i as u32).or(index);
                Cmd::LoadTracks { uris, index, shuffle, resume: Some(pos) }
            }
        };
        self.backend.send(cmd);
        let (context, track) = match saved.repeat {
            Repeat::Off => (false, false),
            Repeat::Context => (true, false),
            Repeat::Track => (true, true),
        };
        if saved.repeat != Repeat::Off {
            self.backend.send(Cmd::Repeat { context, track });
        }
        if !saved.queued.is_empty() {
            // La cola manual se vuelve a añadir cuando el dispositivo ya está activo.
            self.pending_queue = Some((Instant::now() + Duration::from_millis(2500), pos, saved.queued.clone()));
        }
        log::info!("[restore] {} en {} ms", saved.now.name, pos);
    }

    /// Guarda lo que suena aquí (pista, segundo, contexto, aleatorio, repetición y cola manual).
    fn save_playback(&mut self) {
        self.playback_saved_at = Instant::now();
        if self.ephemeral || self.restore_pending.is_some() || self.restore_wanted {
            return;
        }
        let (Some(now), Some(target)) = (self.player.now.clone(), self.last_play.clone()) else { return };
        if self.player.remote.is_some() || self.player.state == PlayState::Stopped {
            return;
        }
        let queue: Vec<Track> = self
            .queue
            .as_ref()
            .map(|q| q.queue.iter().take(80).cloned().collect())
            .unwrap_or_default();
        let saved = SavedPlayback {
            position_ms: self.player.position(),
            now,
            target,
            shuffle: self.player.shuffle,
            repeat: self.player.repeat,
            queued: self.queued_local.clone(),
            queue,
            saved_at: crate::cache::now_secs(),
        };
        if let Ok(text) = serde_json::to_string(&saved) {
            let tmp = self.playback_path.with_extension("tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, &self.playback_path);
            }
        }
    }

    pub fn play(&mut self, t: PlayTarget) {
        self.play_after_restore = false;
        if !self.signed_in() {
            self.status_err("Inicia sesión para reproducir música");
            return;
        }
        self.last_play = Some(t.clone());
        self.last_play_page = Some(self.page().clone());
        self.restore_wanted = false;
        self.restore_pending = None;
        self.restore_deadline = None;
        if let Some(dev) = self.player.remote.clone() {
            let req = match t {
                PlayTarget::Context {
                    uri,
                    track_uri,
                    index,
                    shuffle,
                } => {
                    if shuffle != self.player.shuffle {
                        self.api.send(Req::RemoteShuffle(shuffle));
                    }
                    Req::RemotePlay {
                        device_id: dev.id,
                        context_uri: Some(uri),
                        uris: None,
                        offset_uri: track_uri,
                        offset_index: index,
                    }
                }
                PlayTarget::Tracks {
                    uris,
                    index,
                    shuffle,
                } => {
                    if shuffle != self.player.shuffle {
                        self.api.send(Req::RemoteShuffle(shuffle));
                    }
                    // La Web API no admite listas enormes: ventana alrededor de la pista.
                    let start = index.unwrap_or(0) as usize;
                    let end = (start + 200).min(uris.len());
                    Req::RemotePlay {
                        device_id: dev.id,
                        context_uri: None,
                        uris: Some(uris[start..end].to_vec()),
                        offset_uri: None,
                        offset_index: None,
                    }
                }
            };
            self.api.send(req);
            self.last_remote_poll = Instant::now() - Duration::from_millis(2000);
        } else {
            match t {
                PlayTarget::Context {
                    uri,
                    track_uri,
                    index,
                    shuffle,
                } => self.backend.send(Cmd::LoadContext {
                    uri,
                    track_uri,
                    index,
                    shuffle,
                    resume: None,
                }),
                PlayTarget::Tracks {
                    uris,
                    index,
                    shuffle,
                } => self.backend.send(Cmd::LoadTracks {
                    uris,
                    index,
                    shuffle,
                    resume: None,
                }),
            }
            self.player.state = PlayState::Loading;
        }
    }

    pub fn like(&mut self, id: String, on: bool) {
        self.set_liked(&id, on);
        self.api.send(if on {
            Req::Save(vec![id])
        } else {
            Req::Unsave(vec![id])
        });
    }

    /// Miniplayer: solo la barra de reproducción, siempre visible.
    pub fn toggle_miniplayer(&mut self, ctx: &egui::Context) {
        self.miniplayer = !self.miniplayer;
        if self.miniplayer {
            self.miniplayer_prev = Some(ctx.content_rect().size());
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(820.0, 112.0)));
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop));
            self.fullscreen = false;
        } else {
            let size = self.miniplayer_prev.take().unwrap_or(egui::vec2(1280.0, 800.0));
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(egui::WindowLevel::Normal));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        }
    }

    pub fn toggle_fullscreen(&mut self, ctx: &egui::Context) {
        if self.miniplayer {
            self.toggle_miniplayer(ctx);
        }
        self.fullscreen = !self.fullscreen;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
    }

    pub fn play_pause(&mut self) {
        self.pause_after_restore = false;
        if self.now_placeholder && self.player.remote.is_none() {
            // Todavía se está decidiendo qué restaurar (copia local, clúster, servidor): el play
            // se aplica en cuanto cargue lo restaurado, desde su posición, no desde cero.
            if self.restore_pending.is_some() || self.restore_wanted || !self.restore_decided {
                self.play_after_restore = true;
                self.player.state = PlayState::Loading;
                return;
            }
            // Aún no hay nada cargado en el reproductor: se reproduce la canción mostrada.
            if let Some(uri) = self.player.now.as_ref().map(|n| n.uri.clone()) {
                self.now_placeholder = false;
                self.play(PlayTarget::Tracks { uris: vec![uri], index: Some(0), shuffle: false });
                return;
            }
        }
        if self.player.remote.is_some() {
            if self.player.state == PlayState::Playing {
                self.api.send(Req::RemotePause);
                self.player.state = PlayState::Paused;
                self.player.position_ms = self.player.position();
                self.player.position_at = None;
            } else {
                self.api.send(Req::RemoteResume);
                self.player.state = PlayState::Playing;
                self.player.position_at = Some(Instant::now());
            }
            self.media_dirty = true;
        } else {
            match self.player.state {
                PlayState::Playing => self.backend.send(Cmd::Pause),
                PlayState::Paused => self.backend.send(Cmd::Play),
                _ => self.backend.send(Cmd::PlayPause),
            }
        }
    }

    pub fn next(&mut self) {
        if self.player.remote.is_some() {
            self.api.send(Req::RemoteNext);
            self.last_remote_poll = Instant::now() - Duration::from_millis(2200);
        } else {
            self.backend.send(Cmd::Next);
        }
    }

    pub fn prev(&mut self) {
        if self.player.remote.is_some() {
            self.api.send(Req::RemotePrev);
            self.last_remote_poll = Instant::now() - Duration::from_millis(2200);
        } else {
            self.backend.send(Cmd::Prev);
        }
    }

    pub fn seek(&mut self, ms: u32) {
        self.player.position_ms = ms;
        self.player.position_at = (self.player.state == PlayState::Playing).then(Instant::now);
        if self.player.remote.is_some() {
            self.api.send(Req::RemoteSeek(ms));
        } else {
            self.backend.send(Cmd::Seek(ms));
        }
        self.media_dirty = true;
    }

    pub fn seek_by(&mut self, delta_ms: i64) {
        let dur = self.player.now.as_ref().map(|n| n.duration_ms).unwrap_or(0) as i64;
        let target = (self.player.position() as i64 + delta_ms).clamp(0, dur.max(0));
        self.seek(target as u32);
    }

    /// Cambia el volumen: se oye al instante y se sincroniza con Spotify como mucho cada
    /// 200 ms (el resto se agrupa y se envía el último valor desde `flush_volume`).
    pub fn set_volume(&mut self, raw: u16) {
        self.player.volume = raw;
        self.preview_volume(raw);
        if self.last_volume_sent.elapsed() >= Duration::from_millis(200) {
            self.send_volume(raw);
        } else {
            self.volume_queued = Some(raw);
        }
    }

    fn send_volume(&mut self, raw: u16) {
        self.last_volume_sent = Instant::now();
        self.volume_sent = Some((raw, Instant::now()));
        self.volume_queued = None;
        if self.player.remote.is_some() {
            let pct = vol_raw_to_pct(raw).round() as u8;
            self.api.send(Req::RemoteVolume(pct));
        } else {
            self.backend.send(Cmd::Volume(raw));
        }
    }

    /// Envía el volumen agrupado cuando toca (se llama cada fotograma).
    fn flush_volume(&mut self, ctx: &egui::Context) {
        if let Some(raw) = self.volume_queued {
            if self.last_volume_sent.elapsed() >= Duration::from_millis(200) {
                self.send_volume(raw);
            } else {
                ctx.request_repaint_after(Duration::from_millis(60));
            }
        }
    }

    /// ¿Aceptar un volumen que llega de fuera (eco de Spotify o sondeo del dispositivo)?
    /// Mientras haya un envío nuestro reciente, solo se acepta si coincide con lo enviado.
    fn accept_volume_echo(&mut self, v: u16) -> bool {
        if self.volume_queued.is_some() {
            return false;
        }
        match self.volume_sent {
            Some((sent, at)) => {
                if v == sent || at.elapsed() > Duration::from_millis(1500) {
                    self.volume_sent = None;
                    true
                } else {
                    false
                }
            }
            None => true,
        }
    }

    /// Rueda del ratón sobre el volumen: 2 % por muesca, con acumulación de restos.
    pub fn volume_wheel(&mut self, delta_points: f32) {
        // Una muesca de rueda son ~50 puntos en egui; los trackpads mandan valores menores.
        self.volume_wheel += delta_points;
        let step_points = 25.0;
        let steps = (self.volume_wheel / step_points).trunc();
        if steps != 0.0 {
            self.volume_wheel -= steps * step_points;
            let pct = (vol_raw_to_pct(self.player.volume) + steps).clamp(0.0, 100.0);
            self.set_volume(vol_pct_to_raw(pct));
        }
    }

    /// Volumen provisional durante el arrastre: audio inmediato sin tocar el estado remoto.
    pub fn preview_volume(&mut self, raw: u16) {
        if self.player.remote.is_none() {
            self.backend.send(Cmd::VolumePreview(raw));
        }
    }

    pub fn volume_by(&mut self, delta_pct: i32) {
        let pct = (vol_raw_to_pct(self.player.volume) + delta_pct as f32).clamp(0.0, 100.0);
        self.set_volume(vol_pct_to_raw(pct));
    }

    pub fn toggle_mute(&mut self) {
        if self.player.volume > 0 {
            self.prev_volume = self.player.volume;
            self.set_volume(0);
        } else {
            let v = if self.prev_volume == 0 {
                u16::MAX / 2
            } else {
                self.prev_volume
            };
            self.set_volume(v);
        }
    }

    pub fn toggle_shuffle(&mut self) {
        let on = !self.player.shuffle;
        self.player.shuffle = on;
        if self.player.remote.is_some() {
            self.api.send(Req::RemoteShuffle(on));
        } else {
            self.backend.send(Cmd::Shuffle(on));
        }
    }

    pub fn cycle_repeat(&mut self) {
        let next = match self.player.repeat {
            Repeat::Off => Repeat::Context,
            Repeat::Context => Repeat::Track,
            Repeat::Track => Repeat::Off,
        };
        self.player.repeat = next;
        if self.player.remote.is_some() {
            self.api.send(Req::RemoteRepeat(match next {
                Repeat::Off => "off",
                Repeat::Context => "context",
                Repeat::Track => "track",
            }));
        } else {
            self.backend.send(Cmd::Repeat {
                context: next != Repeat::Off,
                track: next == Repeat::Track,
            });
        }
    }

    pub fn select_device(&mut self, d: Device, is_self: bool) {
        if is_self {
            self.backend.send(Cmd::TransferHere);
            self.player.remote = None;
            self.status("Trayendo la reproducción a este equipo…");
        } else if let Some(id) = d.id.clone() {
            self.api.send(Req::Transfer { device_id: id });
            self.status(format!("Reproduciendo en {}", d.name));
            self.player.remote = Some(d);
            self.last_remote_poll = Instant::now() - Duration::from_millis(1500);
        }
    }

    pub fn toggle_side(&mut self, tab: SideTab) {
        if self.side == Some(tab) {
            self.side = None;
        } else {
            self.side = Some(tab);
            match tab {
                SideTab::Queue => self.queue_at = Instant::now() - Duration::from_secs(60),
                SideTab::Lyrics => self.ensure_lyrics(),
            }
        }
    }

    pub fn run_search(&mut self) {
        let q = self.search_query.trim().to_string();
        if q.is_empty() {
            return;
        }
        if !self.logged_in() {
            self.status_err("Inicia sesión para buscar");
            return;
        }
        if q.contains("open.spotify.com/") || q.starts_with("spotify:") {
            self.open_link(&q);
            self.search_query.clear();
            return;
        }
        self.search_loading = true;
        self.search_result = None;
        if self.search_filter == 8 {
            // Perfiles: Spotify no busca usuarios; se prueba el nombre de usuario tal cual.
            self.api.send(Req::User(q.clone()));
            self.search_loading = false;
        }
        self.api.send(Req::Search(q));
        self.go(Page::Search);
    }

    pub fn save_settings(&mut self, ctx: &egui::Context) {
        let restart = self.settings.playback_differs(&self.draft);
        let media_changed = self.settings.media_keys != self.draft.media_keys;
        self.draft.zoom = self.draft.zoom.clamp(0.7, 2.0);
        self.draft.fps_cap = self.draft.fps_cap.clamp(30, 480);
        self.settings = self.draft.clone();
        self.settings.save(&self.paths);
        self.apply_theme(ctx);
        if media_changed {
            self.media = if self.settings.media_keys {
                Media::new(self.hwnd, self.ui_tx.clone())
            } else {
                Media::disabled()
            };
            self.media_dirty = true;
        }
        if restart && self.logged_in() {
            self.backend.send(Cmd::Restart(self.settings.clone()));
        }
        self.status("Ajustes guardados");
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.egui_wants_keyboard_input();
        let mut acts: Vec<Shortcut> = Vec::new();
        ctx.input_mut(|i| {
            let cmd = Modifiers::COMMAND;
            let pairs = [
                (cmd, Key::Q, Shortcut::Quit),
                (cmd, Key::F, Shortcut::Search),
                (cmd, Key::ArrowRight, Shortcut::Next),
                (cmd, Key::ArrowLeft, Shortcut::Prev),
                (cmd, Key::ArrowUp, Shortcut::VolUp),
                (cmd, Key::ArrowDown, Shortcut::VolDown),
                (Modifiers::SHIFT, Key::ArrowRight, Shortcut::SeekFwd),
                (Modifiers::SHIFT, Key::ArrowLeft, Shortcut::SeekBack),
                (Modifiers::ALT, Key::ArrowLeft, Shortcut::Back),
                (Modifiers::ALT, Key::ArrowRight, Shortcut::Forward),
                (cmd, Key::H, Shortcut::Home),
                (cmd, Key::L, Shortcut::Liked),
                (cmd, Key::B, Shortcut::Sidebar),
                (cmd, Key::N, Shortcut::NewPlaylist),
                (cmd, Key::J, Shortcut::Jam),
                (cmd, Key::Comma, Shortcut::Settings),
                (cmd, Key::W, Shortcut::CloseTab),
                (cmd, Key::Slash, Shortcut::Help),
            ];
            for (m, k, s) in pairs {
                if i.consume_key(m, k) {
                    acts.push(s);
                }
            }
            if !typing {
                let plain = [
                    (Key::Space, Shortcut::PlayPause),
                    (Key::S, Shortcut::Shuffle),
                    (Key::R, Shortcut::Repeat),
                    (Key::M, Shortcut::Mute),
                    (Key::Q, Shortcut::Queue),
                    (Key::L, Shortcut::Lyrics),
                    (Key::Slash, Shortcut::Search),
                    (Key::Questionmark, Shortcut::Help),
                    (Key::Escape, Shortcut::Escape),
                ];
                for (k, s) in plain {
                    if i.consume_key(Modifiers::NONE, k) {
                        acts.push(s);
                    }
                }
                if i.consume_key(Modifiers::SHIFT, Key::Slash) {
                    acts.push(Shortcut::Help);
                }
            } else if i.consume_key(Modifiers::NONE, Key::Escape) {
                acts.push(Shortcut::Escape);
            }
        });
        for a in acts {
            self.run_shortcut(a, ctx);
        }
    }

    /// Ejecuta un atajo (teclado y modo de control comparten este camino).
    pub(crate) fn run_shortcut(&mut self, a: Shortcut, ctx: &egui::Context) {
        match a {
            Shortcut::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Shortcut::Search => self.focus_search = true,
            Shortcut::Next => self.next(),
            Shortcut::Prev => self.prev(),
            Shortcut::VolUp => self.volume_by(5),
            Shortcut::VolDown => self.volume_by(-5),
            Shortcut::SeekFwd => self.seek_by(10_000),
            Shortcut::SeekBack => self.seek_by(-10_000),
            Shortcut::Back => self.back(),
            Shortcut::Forward => self.forward(),
            Shortcut::Home => self.go(Page::Home),
            Shortcut::Liked => self.go(Page::Liked),
            Shortcut::Sidebar => self.settings.sidebar_visible = !self.settings.sidebar_visible,
            Shortcut::Settings => {
                self.draft = self.settings.clone();
                self.go(Page::Settings)
            }
            Shortcut::Help => self.show_shortcuts = !self.show_shortcuts,
            Shortcut::PlayPause => self.play_pause(),
            Shortcut::Shuffle => self.toggle_shuffle(),
            Shortcut::Repeat => self.cycle_repeat(),
            Shortcut::Mute => self.toggle_mute(),
            Shortcut::Queue => self.toggle_side(SideTab::Queue),
            Shortcut::Lyrics => self.toggle_side(SideTab::Lyrics),
            Shortcut::NewPlaylist => {
                if self.logged_in() {
                    self.actions.push(Action::OpenEditor(None));
                }
            }
            Shortcut::Jam => self.jam_open = !self.jam_open,
            Shortcut::CloseTab => {
                if let ActiveTab::Tab(i) = self.active {
                    self.close_tab(i);
                }
            }
            Shortcut::Escape => {
                self.show_shortcuts = false;
                self.jam_open = false;
                self.editor = None;
            }
        }
    }
}

pub(crate) enum Shortcut {
    Quit,
    Search,
    Next,
    Prev,
    VolUp,
    VolDown,
    SeekFwd,
    SeekBack,
    Back,
    Forward,
    Home,
    Liked,
    Sidebar,
    Settings,
    Help,
    PlayPause,
    Shuffle,
    Repeat,
    Mute,
    Queue,
    Lyrics,
    NewPlaylist,
    Jam,
    CloseTab,
    Escape,
}

impl crate::shell::UiApp for App {
    /// Ventana minimizada: mantiene al día pista, posición, letra y controles de medios sin
    /// pintar. Sin esto, los cambios de canción se acumulaban y la posición quedaba desfasada.
    fn pump(&mut self, ctx: &egui::Context) {
        self.drain(ctx);
        self.tick(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.drain(&ctx);
        self.tick(&ctx);
        self.shortcuts(&ctx);
        self.handle_dropped_files(&ctx);

        let p = theme::palette(&ctx);
        let bg = p.bg;
        let margin = 8.0;

        if self.miniplayer {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(bg).inner_margin(egui::Margin::same(6)))
                .show(ui, |ui| self.player_bar(ui));
            self.overlays(&ctx);
            self.apply_actions(&ctx);
            self.images.end_frame();
            return;
        }

        // Barra superior a todo el ancho.
        egui::Panel::top("topbar")
            .exact_size(52.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::new().fill(bg).inner_margin(Margin { left: 0, right: 0, top: 6, bottom: 0 }))
            .show(ui, |ui| self.top_bar(ui));

        // Reproductor flotante abajo.
        egui::Panel::bottom("player")
            .exact_size(96.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::new().fill(bg).inner_margin(Margin { left: 8, right: 8, top: 4, bottom: 8 }))
            .show(ui, |ui| self.player_bar(ui));

        if self.settings.sidebar_visible {
            egui::Panel::left("sidebar")
                .exact_size(SIDEBAR_W)
                .resizable(false)
                .show_separator_line(false)
                .frame(Frame::new().fill(bg).inner_margin(Margin { left: 10, right: 6, top: 4, bottom: 4 }))
                .show(ui, |ui| self.sidebar(ui));
        }

        if let Some(tab) = self.side {
            egui::Panel::right("side")
                .exact_size(340.0)
                .resizable(false)
                .show_separator_line(false)
                .frame(Frame::new().fill(bg).inner_margin(Margin { left: 0, right: 8, top: 0, bottom: 0 }))
                .show(ui, |ui| {
                    Frame::new()
                        .fill(p.card)
                        .corner_radius(CornerRadius::same(14))
                        .stroke(egui::Stroke::new(1.0, p.border))
                        .inner_margin(Margin::symmetric(14, 12))
                        .show(ui, |ui| {
                            ui.set_min_height(ui.available_height());
                            self.side_panel(ui, tab)
                        });
                });
        }

        let page_key = format!("{:?}", self.page());
        egui::CentralPanel::default()
            .frame(Frame::new().fill(bg).inner_margin(Margin {
                left: if self.settings.sidebar_visible { 0 } else { margin as i8 },
                right: margin as i8,
                top: 0,
                bottom: 0,
            }))
            .show(ui, |ui| {
                Frame::new()
                    .fill(p.card)
                    .corner_radius(CornerRadius::same(14))
                    .stroke(egui::Stroke::new(1.0, p.border))
                    .inner_margin(Margin { left: 22, right: 22, top: 18, bottom: 0 })
                    .show(ui, |ui| {
                        ui.set_min_size(ui.available_size());
                        egui::ScrollArea::vertical()
                            .id_salt(page_key)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                self.page_ui(ui);
                                ui.add_space(24.0);
                            });
                    });
            });

        self.overlays(&ctx);
        self.apply_actions(&ctx);
        self.images.end_frame();
    }

    fn on_exit(&mut self) {
        self.save_playback();
        if !self.ephemeral {
            self.settings.volume = vol_raw_to_pct(self.player.volume).round() as u8;
            self.settings.lyrics_open = self.side == Some(SideTab::Lyrics);
            self.settings.save(&self.paths);
        }
        // El backend empieza a desconectar (avisa a Spotify de la pausa y del dispositivo
        // inactivo) mientras aquí se escriben los ficheros: son pequeños y va en este hilo,
        // así el process::exit no los corta a medias.
        self.backend.send(Cmd::Shutdown);
        if self.snapshot_dirty && self.logged_in() {
            self.snapshot_dirty = false;
            self.snapshot().save_now(&self.snapshot_path);
        }
        if self.play_log_dirty {
            self.play_log_dirty = false;
            self.play_log.save_now(&self.play_log_path);
        }
        // Un margen corto para que salga el aviso de desconexión; si Spotify tarda más, se
        // cierra igual: la copia local de la reproducción ya está guardada y el servidor
        // detecta la desconexión por sí mismo.
        let t = Instant::now();
        while !self.shutdown_done && t.elapsed() < Duration::from_millis(30) {
            while let Ok(msg) = self.rx.try_recv() {
                if let Msg::Backend(Event::ShutdownDone) = msg {
                    self.shutdown_done = true;
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

fn is_image_path(p: &std::path::Path) -> bool {
    matches!(
        p.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("jpg" | "jpeg" | "png" | "webp" | "bmp")
    )
}

/// Diálogo nativo para elegir una imagen (bloquea la interfaz mientras está abierto).
pub fn pick_image_file() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Imagen de la playlist")
        .add_filter("Imágenes", &["jpg", "jpeg", "png", "webp", "bmp"])
        .pick_file()
}

pub fn fmt_thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push('.');
        }
        out.push(c);
    }
    out
}

pub fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
}

/// Pista a partir de lo que se está reproduciendo (para historial, menús y registro).
pub fn track_from_now(np: &NowPlaying) -> Track {
    Track {
        id: np.id.clone(),
        uri: np.uri.clone(),
        name: np.name.clone(),
        duration_ms: np.duration_ms,
        explicit: false,
        artists: np.artists.iter().map(|(n, i)| ArtistRef { id: i.clone(), name: n.clone(), uri: None }).collect(),
        album: Some(AlbumRef {
            id: np.album_id.clone(),
            name: np.album.clone(),
            uri: None,
            images: np.cover_url.iter().map(|u| Image { url: u.clone(), width: Some(300), height: Some(300) }).collect(),
            artists: Vec::new(),
            release_date: None,
            total_tracks: None,
            album_type: None,
        }),
        is_local: false,
        track_number: None,
        is_playable: None,
        kind: Some("track".into()),
        added_by: None,
        added_at: None,
    }
}

/// Nombre de usuario de las credenciales guardadas por librespot (si las hay).
fn saved_username(paths: &crate::config::Paths) -> Option<String> {
    let text = std::fs::read_to_string(paths.credentials_dir().join("credentials.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let u = v.get("username")?.as_str()?.trim();
    (!u.is_empty()).then(|| u.to_string())
}
