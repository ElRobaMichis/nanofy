//! Interfaz: estado de la aplicación, mensajes, acciones, atajos y composición de la ventana.
//!
//! egui solo repinta cuando hay entrada del usuario o cuando un hilo de fondo envía un
//! mensaje; mientras suena música se repinta 2 veces por segundo para mover el progreso
//! (y más a menudo si el panel de letras está abierto, para resaltar la línea actual).

mod artist;
pub mod control;
mod icon_masks;
mod icons;
mod library;
mod library_masks;
mod menu_masks;
mod add_panel;
mod pages;
mod panels;
mod player_bar;
mod player_menu;
mod lyrics_panel;
mod queue_panel;
mod theme;
mod warm;
mod watchdog;
mod widgets;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use egui::{Frame, Key, Margin, Modifiers};
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
use crate::update::{FailKind, MoveReason, Stage, UpdateInfo, UpdateResult};
use crate::webauth::WebAuth;

pub use theme::GREEN;
pub const ERROR_RED: egui::Color32 = theme::RED;
pub const ROW_H: f32 = 56.0;
pub const SIDEBAR_W: f32 = 272.0;
/// Alto del panel del reproductor: barra de 80 px, 5 por encima y 10 por debajo.
pub const PLAYER_PANEL_H: f32 = 95.0;
/// Alto de la barra superior.
pub const TOPBAR_H: f32 = 59.0;
/// Márgenes del contenido dentro de su panel (los de la referencia: el título a 36 px del borde
/// izquierdo, la portada de la derecha a 49 del derecho y 22 por arriba).
pub const CONTENT_PAD_LEFT: f32 = 36.0;
pub const CONTENT_PAD_RIGHT: f32 = 49.0;
pub const CONTENT_PAD_TOP: f32 = 22.0;
/// Filo del panel en Inicio y en la página de un artista (fondo de la ventana con un borde de
/// 1 px).
const ARTIST_PANEL_EDGE: egui::Color32 = egui::Color32::from_gray(26);
/// Márgenes de Inicio (referencia 9.png): chips y tarjetas a 25 px del borde izquierdo, 23 por la
/// derecha (caben 8 tarjetas) y 12,5 arriba.
const HOME_PAD_LEFT: f32 = 25.0;
const HOME_PAD_RIGHT: f32 = 23.0;
const HOME_PAD_TOP: f32 = 12.5;

/// Fondo del panel de contenido: sin `tint`, del color `card`; con él, el degradado de la
/// referencia (plano 26 px y de ahí lineal hasta `bottom` 8 px antes del borde de abajo).
fn paint_content_panel(painter: &egui::Painter, panel: egui::Rect, tint: Option<egui::Color32>, card: egui::Color32, bottom: egui::Color32) {
    let r = egui::CornerRadius::same(8);
    let Some(top) = tint else {
        painter.rect_filled(panel, r, card);
        return;
    };
    painter.rect_filled(panel, r, bottom);
    let flat_end = panel.min.y + 26.0;
    let grad_end = (panel.max.y - 8.0).max(flat_end + 1.0);
    painter.rect_filled(
        egui::Rect::from_min_max(panel.min, egui::pos2(panel.max.x, flat_end + 1.0)),
        egui::CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 },
        top,
    );
    let mut mesh = egui::epaint::Mesh::default();
    mesh.colored_vertex(egui::pos2(panel.min.x, flat_end), top);
    mesh.colored_vertex(egui::pos2(panel.max.x, flat_end), top);
    mesh.colored_vertex(egui::pos2(panel.max.x, grad_end), bottom);
    mesh.colored_vertex(egui::pos2(panel.min.x, grad_end), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}
pub const LIKED: &str = "liked";
pub const SEARCH_ID: &str = "nanofy_search_box";
/// La sesión del DJ de Spotify: una playlist especial que la Web API no da; sus canciones y las
/// frases del locutor salen de su propio servicio (ver `context_resolver` en librespot-connect).
pub const DJ_URI: &str = "spotify:playlist:37i9dQZF1EYkqdzj48dyYq";

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

/// Una pestaña de contenido con su propio historial. `hidden`: la vista principal, donde se
/// navega desde la barra lateral, Inicio o Buscar; no sale en la barra superior (solo hay una).
#[derive(Clone, Debug)]
pub struct Tab {
    pub history: Vec<Page>,
    pub idx: usize,
    pub hidden: bool,
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
pub type HomeCacheKey = (usize, usize, usize, Vec<String>, usize, Vec<String>, usize, Vec<String>, bool);

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
    /// Abierto desde el botón del reproductor: el centro x de ese botón (el panel sale encima de
    /// él) y su rectángulo (pulsarlo otra vez lo cierra). Sin ellos, en el centro de la ventana.
    pub bar_x: Option<f32>,
    pub button: Option<egui::Rect>,
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
    /// Formato, calidad y normalización de lo que suena en este equipo (`Event::AudioFormat`).
    /// Se borra al cambiar de canción y al pasar la reproducción a otro dispositivo.
    pub audio: Option<AudioInfo>,
    /// Fundidos entre canciones que han empezado en este equipo (`Event::Crossfade`) y el último,
    /// para el modo de control.
    pub transitions: u32,
    pub last_transition: Option<LastTransition>,
    /// El locutor del DJ está hablando antes (o después) de la canción: la barra lo enseña a él y
    /// la posición no avanza (`Event::Narration`).
    pub dj: Option<crate::backend::Narration>,
}

/// Un fundido que empezó: de qué canción a cuál (uris), cuánto dura y cuándo empezó.
#[derive(Debug, Clone)]
pub struct LastTransition {
    pub from: String,
    pub to: String,
    pub ms: u32,
    pub at: Instant,
}

impl PlayerState {
    /// Información de audio de la canción que muestra la barra, si suena aquí. Con la canción
    /// mostrada por adelantado (al hacer clic) o restaurada, la que hubiera es de otra pista.
    pub fn local_audio(&self) -> Option<&AudioInfo> {
        if self.remote.is_some() {
            return None;
        }
        let now = self.now.as_ref()?;
        self.audio.as_ref().filter(|a| a.uri == now.uri)
    }

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
    /// Versión de `tracks`: cambia con cada cambio de sus pistas (lote, sustitución, edición) y
    /// guía lo que las páginas guardan calculado (resumen, búsqueda interna). Las páginas sacan
    /// la lista de `lists` en cada fotograma y la vuelven a meter: deben devolverla tal cual.
    pub gen: u64,
}

impl TrackList {
    /// Lista nueva con versión propia.
    fn new(tracks: Vec<Track>, total: u32, gen: &mut u64) -> Self {
        let mut list = TrackList { tracks, total, loading: false, gen: 0 };
        list.touch(gen);
        list
    }

    /// Marca un cambio en sus pistas. El contador es de toda la app (`App::list_gen`), no de la
    /// lista: una borrada y vuelta a crear (cierre de sesión, recarga de la biblioteca) nunca
    /// repite una versión que ya tenga algo calculado guardado.
    fn touch(&mut self, gen: &mut u64) {
        *gen += 1;
        self.gen = *gen;
    }
}

/// Lo que las páginas de lista sacan de todas sus pistas. Se calcula una vez por versión de la
/// lista: en cada fotograma, con miles de pistas, costaba milisegundos al desplazarse.
pub struct ListSummary {
    pub total_ms: u64,
    /// Artistas más presentes.
    pub top: Vec<ArtistRef>,
    /// Quienes añadieron canciones, sin repetir y por orden de aparición. Sin el propietario
    /// delante: sus datos pueden llegar después que las pistas.
    pub added_by: Vec<String>,
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

/// Copia en disco de una playlist (o radio): se muestra al instante al volver a abrirla, también
/// tras reiniciar, mientras llega la versión fresca.
/// La API lee también `tracks`, `meta_at` y `country` (`ListCopy` en api.rs) para no volver a
/// pedir las pistas que ya tiene.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct CachedList {
    meta: Option<Playlist>,
    tracks: Vec<Track>,
    /// snapshot_id que tenía la playlist en el listado cuando se pidió esta versión (no al
    /// guardarla: pudo cambiar mientras llegaba). Sin él (radios, de otros) no se da por buena.
    #[serde(default)]
    snapshot_id: Option<String>,
    /// Cuándo se guardó (segundos Unix).
    #[serde(default)]
    saved_at: u64,
    /// De cuándo son los metadatos más antiguos de sus pistas y para qué país se pidieron.
    #[serde(default)]
    meta_at: u64,
    #[serde(default)]
    country: Option<String>,
}

/// Lo que se sabe de la copia en disco de una playlist (leída por warm_list/on_warmed o escrita
/// por save_list): su snapshot_id, cuándo se guardó y qué versión de la lista en memoria es ella
/// (`TrackList::gen`). Si la lista cambió después (edición, otra carga) ya no es la copia.
struct ListDisk {
    snapshot_id: Option<String>,
    saved_at: u64,
    gen: u64,
}

/// Lo que la API manda antes del último lote de una carga de playlist para su copia en disco
/// (ver `Resp::PlaylistCopyInfo`). Sin él (`Default`) la recarga siguiente las pide todas.
#[derive(Default)]
struct CopyInfo {
    meta_at: u64,
    country: Option<String>,
}

/// Copias en disco que se conservan por carpeta (playlists/radios y álbumes): unas pocas decenas
/// de KB cada una; las abiertas hace más tiempo se borran.
const CACHED_MAX: usize = 400;

/// Precarga de playlists al conectar: como mucho estas por tanda (cada una es la playlist4 y sus
/// lotes de metadatos, o solo lo nuevo respecto a su copia, a spclient, que limita a 300
/// peticiones cada 30 s y comparte con el audio).
const PREFETCH_MAX: usize = 10;
/// La que ya tiene una copia en disco más reciente que esto, sin snapshot_id con que compararla
/// (copias antiguas, o sin listado de esta sesión), no se precarga: al abrirla se ve esa copia
/// al instante y se refresca entonces. Con él decide `skip_unchanged`.
const PREFETCH_FRESH: Duration = Duration::from_secs(6 * 3600);
/// Si la respuesta de una precarga se pierde (sesión caída, cierre), tras esto sale la siguiente.
const PREFETCH_STALE: Duration = Duration::from_secs(60);
/// Una copia en disco con el mismo snapshot_id que el listado de esta sesión se da por buena sin
/// pedir nada si tiene menos de esto (24 h): pasado, se recarga igualmente (barata: la API
/// reutiliza sus pistas), por si algún cambio no hubiera movido el snapshot_id.
const LIST_UNCHANGED_SECS: u64 = 24 * 3600;
/// Tras reiniciar para actualizar, la copia local de la reproducción manda sin esperar a Spotify
/// si tiene menos de esto: es la que guardó la ventana anterior al cerrarse.
const UPDATE_RESTORE_FRESH_SECS: u64 = 120;
/// Lo que dura a la vista el aviso de después de actualizar (más mientras el ratón esté encima).
pub const UPDATED_TOAST_FOR: Duration = Duration::from_secs(8);

/// Aviso breve de después de actualizar: «Nanofy se actualizó a la {version}».
pub struct UpdatedToast {
    pub version: String,
    /// Desde cuándo cuenta: desde que se vio por primera vez (no desde que se creó la app, que
    /// puede ser antes de que la ventana se vea) y otra vez cada vez que el ratón pasa por encima.
    pub since: Option<Instant>,
}

/// Tiempo durante el que el listado de playlists de esta sesión decide si una copia en disco
/// sigue al día (ver `App::listing_fresh`).
const LISTING_TRUST: Duration = Duration::from_secs(10 * 60);
/// El listado del rootlist se completa con la Web API (mosaicos, nombre del propietario,
/// privacidad) como mucho una vez cada tanto (24 h) si no aparece ninguna playlist nueva: la
/// cuota de la Web API es compartida con otros clientes y la biblioteca no debe depender de ella.
const PLAYLISTS_WEB_SECS: u64 = 24 * 3600;
/// Una carga de playlist sin noticias (ni un lote) durante esto se da por perdida (un hilo
/// colgado, una respuesta que no llegará) y deja pasar otra de la misma playlist.
const PL_INFLIGHT_STALE: Duration = Duration::from_secs(60);
/// Me gusta se recarga entera como mucho cada tanto: en medio basta con lo reciente (1-2
/// páginas) y la cuenta de Spotify, que delata lo quitado en otro dispositivo.
const LIKED_FULL_SYNC_SECS: u64 = 7 * 24 * 3600;
/// Los artistas seguidos de la instantánea se dan por buenos durante este tiempo.
const ARTISTS_SYNC_SECS: u64 = 6 * 3600;
/// Espera antes de cada reintento automático de una playlist cuya carga falló a medias (sesión
/// caída, límite de librespot). Agotados, queda a la vista lo que llegó con «Reintentar».
const LIST_RETRY_DELAYS: [Duration; 3] = [Duration::from_secs(5), Duration::from_secs(20), Duration::from_secs(60)];
/// Cola del contexto restaurado: espera antes de volver a pedir sus pistas tras cada fallo (como
/// mucho estos reintentos) y plazo total tras el que se deja sin armar. Antes se pedía otra vez
/// cada 1,2 s sin límite, aunque la anterior siguiera en curso.
const RESTORE_CTX_RETRY: [Duration; 3] = [Duration::from_millis(1200), Duration::from_millis(2400), Duration::from_millis(4800)];
const RESTORE_CTX_DEADLINE: Duration = Duration::from_secs(60);

/// Resultados de búsqueda recientes que se guardan en memoria (los usados hace más tiempo salen).
const SEARCH_CACHE_MAX: usize = 50;
/// Un resultado más reciente que esto se muestra tal cual, sin pedir nada.
const SEARCH_FRESH: Duration = Duration::from_secs(30 * 60);
/// Hasta esto se muestra al instante y se refresca detrás; pasado, se busca como si no estuviera.
const SEARCH_KEEP: Duration = Duration::from_secs(24 * 3600);
/// Entre dos refrescos de la misma consulta: ir y volver entre búsquedas no gasta cuota.
const SEARCH_REFRESH_GAP: Duration = Duration::from_secs(10 * 60);

/// Un resultado de búsqueda guardado en `App::search_cache`.
struct CachedSearch {
    /// Cuándo respondió Spotify: decide si vale tal cual o se refresca.
    at: Instant,
    /// Última vez que se pidió o se mostró: con más de SEARCH_CACHE_MAX sale el menos usado.
    used: Instant,
    /// Último refresco en segundo plano enviado (si su respuesta no llega, `at` no cambia y sin
    /// esto cada vuelta a la consulta pediría otro).
    asked: Option<Instant>,
    result: SearchResult,
}

/// Clave de la caché de búsquedas: «Daft  Punk » y «daft punk» son la misma consulta.
fn search_key(q: &str) -> String {
    q.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Sin nada en ninguna categoría. No se guarda: puede ser el respaldo vacío de pathfinder con la
/// Web API caída, y guardarlo escondería durante media hora los resultados reales.
fn search_is_empty(s: &SearchResult) -> bool {
    s.tracks.as_ref().is_none_or(|p| p.items.is_empty())
        && s.albums.as_ref().is_none_or(|p| p.items.iter().all(Option::is_none))
        && s.artists.as_ref().is_none_or(|p| p.items.iter().all(Option::is_none))
        && s.playlists.as_ref().is_none_or(|p| p.items.iter().all(Option::is_none))
        && s.shows.as_ref().is_none_or(|p| p.items.iter().all(Option::is_none))
        && s.episodes.as_ref().is_none_or(|p| p.items.iter().all(Option::is_none))
        && s.audiobooks.as_ref().is_none_or(|p| p.items.iter().all(Option::is_none))
}

/// Una playlist que llega con bastantes menos pistas de las que tiene (lotes que fallaron) no se
/// guarda en disco ni sustituye a la copia que se ve. Unas pocas de menos sí se aceptan: las que
/// Spotify ya no tiene no llegan nunca.
fn list_short(len: usize, total: u32) -> bool {
    let total = total as usize;
    len + (total / 100).max(2) < total
}

/// Solo las playlists (id base62 de 22 caracteres) se guardan; «Me gusta» va en la instantánea.
fn is_playlist_key(key: &str) -> bool {
    key.len() == 22 && key.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// De dónde salen las pistas de un contexto restaurado para armar su cola.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CtxKind {
    /// Me gusta: su carga ya la lanza el arranque (Req::Liked / LikedRecent); no se pide nada.
    Liked,
    Album,
    /// Playlist, también radios, Daily Mix y las de Spotify (37i9).
    Playlist,
}

/// Lista de la que sale la cola de un contexto: (clave en `lists` o `albums`, tipo). Artistas,
/// podcasts, emisoras y demás no tienen aquí una lista con sus pistas (su cola la dan el
/// servidor o el clúster): antes su id se pedía como playlist y fallaba una y otra vez.
fn ctx_list_key(uri: &str) -> Option<(String, CtxKind)> {
    if uri.ends_with(":collection") || uri.contains("collection:tracks") {
        return Some((LIKED.to_string(), CtxKind::Liked));
    }
    // Una emisora sembrada en una playlist o un álbum («spotify:station:playlist:…») suena una
    // radio, no esa lista: su cola no sale de ella.
    if uri.contains(":station:") {
        return None;
    }
    let (kind, id) = if let Some((_, id)) = uri.split_once(":playlist:") {
        (CtxKind::Playlist, id)
    } else if let Some((_, id)) = uri.split_once(":album:") {
        (CtxKind::Album, id)
    } else {
        return None;
    };
    is_playlist_key(id).then(|| (id.to_string(), kind))
}

/// Abre la medida del tiempo hasta que suena una orden al reproductor de este equipo
/// (`librespot_core::ttfs`, que el modo de control enseña como `player.ttfs`). Se mide desde
/// aquí, el clic, porque eso es lo que espera quien escucha. Con `NANOFY_FAULT=limiter_exhaust`
/// es también cuando el presupuesto de peticiones de librespot aparece agotado.
fn ttfs_begin(kind: &'static str) {
    librespot_core::ttfs::begin(kind);
    if librespot_core::ttfs::is_play(kind) {
        librespot_core::fault::arm_limiter_drain();
    }
}

/// Fecha de hoy (AAAA-MM-DD, UTC), como la de `added_at` que da playlist4.
fn today_utc() -> String {
    // Días desde 1970 a fecha civil (algoritmo de Howard Hinnant, «civil_from_days»).
    let z = (crate::cache::now_secs() / 86400) as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Dónde se cortó la reproducción al perder la conexión con Spotify: se retoma ahí al volver.
#[derive(Clone, Copy)]
struct ResumePoint {
    pos: u32,
    playing: bool,
}

/// Una canción que no se pudo cargar por algo pasajero (Spotify frenando las claves, la red) y
/// que la app reintenta sola (`player_bar::LOAD_RETRY_DELAYS`).
#[derive(Clone, Debug)]
struct LoadRetry {
    uri: String,
    /// Fallos seguidos de esta canción hasta ahora.
    failures: u8,
    /// Cuándo reintentar; `None` si ya se reintentó (o se pidió a mano) y se espera el resultado.
    at: Option<Instant>,
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
    /// «Reintentar» de una playlist a medias. Va como acción porque la página tiene la lista
    /// fuera de `lists` mientras se dibuja, y el reintento necesita verla.
    RetryList(String),
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
    /// Se sabe si es pública (o ella tocó el interruptor). Si no (el listado del rootlist no lo
    /// trae y la Web API no contestó), al guardar no se envía: `public` es solo lo que se ve.
    pub public_known: bool,
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
    /// Hilo de lecturas y escrituras de disco (ver `crate::cache::Disk`).
    disk: crate::cache::Disk,

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
    /// Instalación de la versión nueva: disponible, descargando, preparando, lista o su error.
    pub update_stage: Stage,
    /// Para la preparación en curso (`Cancelar`, o porque empieza otra).
    update_cancel: Option<Arc<AtomicBool>>,
    /// Turno de la preparación en curso (`update::stage`): los mensajes de una anterior se
    /// ignoran.
    update_turn: u64,
    /// La preparación en curso la empezó la app sola («Actualizar automáticamente»), no el
    /// usuario con «Instalar»: no se enseña el aviso hasta que esté lista o haga falta él.
    update_silent: bool,
    /// Reintentos automáticos seguidos tras un fallo de red al preparar sola la versión nueva.
    update_retry: u8,
    /// «Reiniciar» pulsado durante una Jam: el aviso pide confirmarlo antes (se sale de ella).
    pub update_confirm_jam: bool,
    /// El estado lo puso `update_fake_stage` (capturas): «Reiniciar» no instala nada de verdad.
    update_fake: bool,
    /// Al cerrar, abrir este ejecutable: la versión nueva ya ocupa su sitio. La ruta se toma
    /// antes de renombrar nada (en Linux `current_exe` sigue al archivo apartado).
    pub restart_after_exit: Option<PathBuf>,
    /// Al cerrar para actualizar sonaba música: la versión nueva la retoma (`--resume-playing`).
    restart_resume: bool,
    /// Sustitución del ejecutable en marcha (en un hilo, con la ventana abierta).
    update_applying: bool,
    /// Esta ventana es la de una versión recién instalada (`--updated-from <versión anterior>`).
    pub updated_from: Option<String>,
    /// Abierta por «Reiniciar» mientras sonaba música (`--resume-playing`): al restaurar la copia
    /// local, que es de hace segundos, sigue sonando. Solo esa vez; cualquier otro arranque se
    /// restaura en pausa.
    resume_after_update: bool,
    /// Modo de control activo (`--control`): la app no se actualiza sola salvo contra un
    /// servidor de releases local (ver `update::auto_update_allowed`).
    control_mode: bool,
    /// Cuándo dar por buena la versión en uso (primer fotograma + 20 s) y si ya se hizo.
    health_at: Option<Instant>,
    health_marked: bool,
    /// Próxima comprobación automática (al arrancar y cada 6 h; 10 min si la release aún no
    /// trae zip para esta plataforma).
    update_check_at: Option<Instant>,
    /// Consultas seguidas cuya release más nueva aún no traía zip para esta plataforma.
    update_pending: u8,
    /// La versión nueva no llegó a arrancar y se volvió a esta (`--update-failed`, o falló al
    /// instalarla al abrir): el motivo, hasta que el usuario pulse «Reintentar», y si su aviso
    /// rojo sigue a la vista.
    update_failed: Option<String>,
    pub update_failed_banner: bool,
    /// «Reintentar» tras una actualización fallida: la consulta que va a llegar instala la
    /// versión nueva en cuanto la encuentre (la que no arrancaba no se prepara sola).
    update_install_after_check: bool,
    /// Diálogo «Novedades de Nanofy {v}» abierto, con las notas completas de esa versión.
    pub notes_dialog: Option<UpdateInfo>,
    /// Notas de la versión en uso, si esta ventana es la recién instalada y la anterior las
    /// guardó al prepararla («Ver novedades» del aviso de después de actualizar).
    current_notes: Option<UpdateInfo>,
    /// Aviso breve de después de actualizar («Nanofy se actualizó a la …»).
    pub updated_toast: Option<UpdatedToast>,

    pub playlists: Vec<Playlist>,
    pub playlists_loaded: bool,
    /// Cuándo llegó `playlists` si es el listado pedido en esta sesión (no el de la
    /// instantánea): solo sus snapshot_id, y mientras sea reciente (`listing_fresh`), sirven
    /// para dar por buena una copia en disco sin pedirla. Uno restaurado puede coincidir con una
    /// copia igual de vieja y esconder un cambio hecho en el móvil.
    playlists_fresh: Option<Instant>,
    /// Hay un listado pedido al conectar (arranque o reconexión) que aún no ha respondido: el
    /// inicio espera a él para decidir si sus secciones de playlists hace falta pedirlas.
    playlists_asked: bool,
    /// De dónde salió `playlists`: "instantánea", "rootlist" o "web" (vacío: aún nada). Para
    /// el modo de control.
    pub playlists_source: &'static str,
    /// Cuándo completó la Web API por última vez el listado del rootlist (segundos Unix) y qué
    /// playlists había entonces (ver `Snapshot::playlists_web_at` y `playlists_web_ids`).
    playlists_web_at: u64,
    playlists_web_ids: HashSet<String>,
    /// Hay un `Req::PlaylistsWeb` en el carril de fondo sin responder: no se encola otro.
    playlists_web_pending: bool,
    pub playlist_meta: HashMap<String, Playlist>,
    pub lists: HashMap<String, TrackList>,
    /// Última versión dada a una lista (ver `TrackList::gen`).
    list_gen: u64,
    /// Resumen de cada lista vista (clave → versión y pistas con que se calculó, resumen).
    page_cache: HashMap<String, (u64, usize, std::rc::Rc<ListSummary>)>,
    /// Último resultado de la búsqueda interna en una lista (clave, consulta, versión, pistas,
    /// coincidencias): mientras no cambien no se vuelve a filtrar ni a copiar en cada fotograma.
    filter_cache: Option<(String, String, u64, usize, std::rc::Rc<[Track]>)>,
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
    /// Copias en disco de las playlists abiertas (`<id>.json`).
    lists_dir: PathBuf,
    /// Playlists que se ven desde la copia en disco: la versión fresca se junta aparte
    /// (`list_fresh`) y la sustituye entera al terminar, sin parpadeos ni listas a medias.
    list_cached: HashSet<String>,
    list_fresh: HashMap<String, Vec<Track>>,
    /// Copias en disco que se están leyendo en el hilo del disco (id → número de la lectura):
    /// llegan con `Msg::Warmed` y solo se ponen si nada fresco se adelantó (ver `on_warmed`).
    warming: HashMap<String, u64>,
    warm_seq: u64,
    /// Playlists cuya carga falló o llegó con huecos: (cuándo toca el siguiente reintento
    /// automático, si queda alguno; reintentos ya lanzados). Mientras estén aquí la página no las
    /// vuelve a pedir por su cuenta (lo hace tick, con espera) y muestra «Reintentar». Sin hora,
    /// con una carga en vuelo, se está reintentando; sin hora y sin carga, solo queda el manual.
    list_retry: HashMap<String, (Option<Instant>, u8)>,
    /// Cargas de playlist en vuelo (id → última noticia: el envío o un lote). Todas salen por
    /// `load_playlist`, que nunca lanza una segunda de la misma: dos a la vez mezclaban sus lotes
    /// en la lista, y lo guardado en disco quedaba con pistas repetidas o cortado.
    pl_inflight: HashMap<String, Instant>,
    /// Playlists que se vuelven a cargar en cuanto termine la que está en vuelo: una edición o
    /// una recarga forzada llegó mientras corría y lo que esa trae puede ser de antes del cambio.
    pl_rerun: HashSet<String>,
    /// snapshot_id del listado al pedir cada carga de playlist: es el que guarda su copia.
    pl_snap: HashMap<String, Option<String>>,
    /// Lo que la API dijo de la carga en curso de cada playlist para su copia en disco.
    pl_copy_info: HashMap<String, CopyInfo>,
    /// Copias en disco ya leídas o escritas en esta sesión (ver `ListDisk`).
    list_disk: HashMap<String, ListDisk>,
    /// Playlists cuyo nombre, descripción o portada se acaban de editar y cuyos metadatos se
    /// piden otra vez a la Web API: si esta falla (límite), se toman de la playlist4.
    meta_after_edit: HashSet<String>,
    /// Radios ya resueltas (canción semilla → playlist); se carga del disco al primer uso.
    radios: Option<HashMap<String, String>>,
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
    /// Se perdió la conexión con Spotify: al volver, se carga lo que sonaba en este punto. Sin
    /// esto la sesión nueva empezaría vacía (o, antes, con la canción desde el principio).
    reconnect_resume: Option<ResumePoint>,
    /// Última orden de reproducción que aún no ha empezado a sonar: si la conexión cae antes,
    /// se repite al reconectar.
    pending_load: Option<Cmd>,
    /// La barra muestra ya la canción elegida, antes de que el reproductor la confirme.
    now_optimistic: bool,
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
    /// Contexto que se está restaurando (clave de su lista, tipo, pista actual): al llegar sus
    /// pistas se arma la cola.
    restore_ctx: Option<(String, CtxKind, String)>,
    /// Cuándo pedir sus pistas: la primera vez en cuanto haya sesión y después solo tras un
    /// fallo, con espera. `None`: ya pedidas (su respuesta despierta la interfaz) o nada que pedir.
    restore_ctx_at: Option<Instant>,
    /// Fallos de esa petición hasta ahora (cuál de RESTORE_CTX_RETRY toca).
    restore_ctx_fails: u8,
    /// Pasado esto se deja la cola sin armar: nunca se espera (ni se repinta) indefinidamente.
    restore_ctx_until: Option<Instant>,
    /// Playlists cuyas pistas se precargan en segundo plano (para que abrirlas sea instantáneo).
    prefetch_ids: std::collections::VecDeque<String>,
    prefetch_at: Option<Instant>,
    /// Precarga en vuelo (id, cuándo salió): solo una a la vez, para que la cola de la API no
    /// se llene de playlists enteras delante de lo que ella abre o busca.
    prefetch_inflight: Option<(String, Instant)>,
    /// La precarga espera la copia en disco de esta playlist (se lee en el hilo del disco) para
    /// decidir si pedirla.
    prefetch_warm: Option<String>,
    /// Medida para el registro: playlist abierta (id, último fotograma en que se dibujó, cuándo
    /// se abrió mientras no se haya visto aún su primera fila).
    pl_open_mark: Option<(String, u64, Option<Instant>)>,
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
    /// «Ver álbum» de una canción guardada sin su álbum: al llegar sus datos se abre el álbum.
    album_of_pending: Option<String>,
    /// Las filas muestran «Añadida por …» (playlists colaborativas).
    pub rows_added_by: bool,
    pub queued_local: Vec<String>,
    /// Tras vaciar la cola: (cuándo, posición a restaurar si hace falta, canciones a volver a
    /// añadir).
    pending_queue: Option<(Instant, Option<u32>, Vec<String>)>,
    pub shows: HashMap<String, (Show, Vec<Episode>)>,
    /// Compartido con el hilo del disco mientras se guarda (ver `PlayLog::save_async`): se
    /// modifica con `Arc::make_mut`.
    pub play_log: std::sync::Arc<PlayLog>,
    play_log_path: PathBuf,
    play_log_dirty: bool,
    /// Propio: con el de la instantánea, guardar una retrasaba el guardado del otro.
    play_log_saved_at: Instant,
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
    /// Panel «Personalizar inicio» abierto.
    pub home_customize_open: bool,
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
    /// Vigilante por etapas de la canción que está cargando aquí (`watchdog::LoadWatchdog`):
    /// lenta, reintento, reconexión y, a los 30 s, aviso. `None` si no carga nada.
    load_watch: Option<watchdog::LoadWatchdog>,
    /// La carga que se dejó de esperar a los 30 s, para que [Reintentar] la vuelva a pedir.
    stuck_load: Option<Cmd>,
    /// Aviso encima de la barra cuando una canción no se pudo reproducir (`playback_error_banner`).
    playback_error: Option<player_bar::PlaybackError>,
    /// Reintento automático de la canción que no se pudo cargar por algo pasajero.
    load_retry: Option<LoadRetry>,
    /// Canción cortada por la red a media reproducción, en pausa en su segundo, y sus reintentos.
    stall: Option<watchdog::StallRecovery>,
    /// Precarga inteligente: cuándo preparar la canción bajo el ratón o la del botón apretado.
    warm: warm::WarmTracker,
    /// Lo que las filas vieron en este fotograma (lo recoge `tick_warm` al final): la canción con
    /// el ratón encima y la del botón de reproducir apretado.
    warm_hover: Option<String>,
    warm_press: Option<String>,
    /// Falta la salida de audio: la canción (uri) que se pausó por eso y si sonaba, para seguir
    /// al volver un dispositivo.
    output_lost: Option<(Option<String>, bool)>,
    /// Reanudaciones automáticas al volver la salida, con su límite (`watchdog::OutputResumes`).
    output_resumes: watchdog::OutputResumes,
    /// La última pausa llegó con algo sonando o cargando (y no a mano desde la pausa): si fue por
    /// falta de salida, al volver un dispositivo se reanuda.
    paused_while_playing: bool,
    /// La cuenta no es Premium: Spotify no deja reproducir aquí (se explica en el aviso).
    not_premium: bool,
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
    /// Consulta enviada cuya respuesta se espera. Solo se acepta la de esta (no la del texto
    /// de la caja, que ella puede haber cambiado ya), y las respuestas viejas se ignoran.
    pub search_pending: Option<String>,
    /// Reintento automático de la consulta en vuelo tras un 429 corto: (cuándo, consulta). Solo
    /// se envía si esa consulta sigue siendo la pendiente.
    pub search_retry: Option<(Instant, String)>,
    /// Reintentos automáticos ya enviados de la consulta en vuelo (máximo 2).
    pub search_retries: u8,
    /// Consulta a la que pertenecen los resultados mostrados (siguen a la vista, atenuados,
    /// mientras llega la siguiente búsqueda).
    pub search_result_for: Option<String>,
    /// Resultados recientes por consulta (clave `search_key`): volver a una búsqueda reciente es
    /// instantáneo y no gasta cuota. Solo en memoria y se vacía al cerrar sesión, para que dos
    /// cuentas del mismo PC no vean las búsquedas de la otra.
    search_cache: HashMap<String, CachedSearch>,
    /// Consultas enviadas para refrescar un resultado de la caché que ya está a la vista: si
    /// fallan solo se anota (ni aviso rojo encima de resultados válidos ni reintento).
    search_refreshing: HashSet<String>,
    /// Consulta lanzada en Perfiles: ahí solo se pide el perfil, y su búsqueda general se pide al
    /// pasar a otra pestaña de resultados (antes se gastaba una /search que nadie miraba).
    pub search_pending_profile: Option<String>,
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
    /// Letras ya pedidas en esta sesión (también «sin letra»), por id de canción; las que tienen
    /// letra se guardan además en disco (`lyrics_from_disk`).
    lyrics_cache: HashMap<String, Option<Lyrics>>,
    /// Canciones cuya letra ya se pidió por adelantado.
    lyrics_prefetched: HashSet<String>,
    /// Panel de la letra: el botón de la letra en este fotograma (para ponerse encima), lo
    /// desplazada que está (px) y de qué canción (otra canción vuelve arriba).
    pub lyrics_button: Option<egui::Rect>,
    pub lyrics_offset: f32,
    pub lyrics_follow_track: String,
    /// Desplazamiento en curso al cambiar de renglón: desde, hasta y cuándo empezó (s).
    pub lyrics_anim: Option<(f32, f32, f64)>,
    /// Fotogramas pintados desde que se abrió (para medir la fluidez desde el modo de control).
    pub frames_painted: u64,

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
    pub library_filter: String,
    /// La lupa de la biblioteca abierta (con su campo para filtrar).
    pub library_search_open: bool,
    /// El menú de «Agrupar» de la biblioteca, abierto.
    pub library_group_open: bool,
    /// El panel de los tres puntos del reproductor, abierto, y la fila cuyo submenú se ve.
    pub player_more_open: bool,
    pub player_more_sub: Option<usize>,
    /// Abierto con el clic derecho en la portada: desde el puntero, no sobre los tres puntos.
    pub player_more_at: Option<egui::Pos2>,
    /// El menú de cristal de una canción de una lista (tres puntos de la fila o clic derecho).
    pub song_more: Option<player_menu::SongMenu>,
    /// El modo de control pide abrir el menú de la fila `.1` (de la lista `.0`, o de la primera
    /// que la tenga) con el submenú `.2`, como el botón de sus tres puntos.
    pub song_more_req: Option<(Option<String>, usize, Option<usize>)>,
    /// El panel de la cola en «Recientes» (si no, en «Cola»).
    pub queue_recent: bool,
    /// Arrastrando por su asa la canción `n` de las añadidas a la cola.
    pub queue_drag: Option<(bool, usize)>,
    /// Cuándo volver a pedir la cola (tras aleatorio, repetir o mover una canción, Spotify tarda
    /// un momento en tenerla al día).
    queue_refresh: Vec<Instant>,
    /// El modo de control pide abrir «Añadir a una playlist» como el botón del reproductor.
    pub add_from_bar: bool,
    /// Carpeta abierta dentro de la biblioteca (su id).
    pub library_folder: Option<String>,
    /// Última vez que sonó cada cosa de la biblioteca (clave de `library::library_key`), en
    /// segundos Unix, aquí y en la cuenta (recently-played). Ordena «Recientes». Va en su propio
    /// archivo y no en los ajustes: cambia con cada reproducción.
    pub library_recent: HashMap<String, u64>,
    library_recent_path: std::path::PathBuf,
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
    /// Autorización de la biblioteca preparada y esperando al navegador (encadenada al inicio de
    /// sesión o abierta con «Conectar con Spotify»): su URL para abrirla otra vez y su cancelación.
    pub web_chain: Option<crate::webauth::WebChain>,
    /// `web_chain` la armó `login` (encadenada al inicio de sesión): solo esa se suelta si el inicio
    /// de sesión se cancela o entra sin navegador; la de «Conectar con Spotify» sigue esperando.
    web_chain_login: bool,
    /// Cuándo se pulsó «Cancelar» en el inicio de sesión (el botón espera la respuesta del
    /// backend, normalmente al momento; pasados unos segundos sin ella se puede volver a pulsar).
    pub login_cancel_at: Option<Instant>,
    media_dirty: bool,
    /// Instantánea de la biblioteca en disco.
    snapshot_path: PathBuf,
    snapshot_dirty: bool,
    snapshot_saved_at: Instant,
    /// Ver `Snapshot::liked_synced_at`, `liked_server_total` y `artists_synced_at`.
    liked_synced_at: u64,
    liked_server_total: u64,
    artists_synced_at: u64,
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
            paths.cache_dir.join("lists"),
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
        let lists_dir = paths.cache_dir.join("lists");
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
        let library_recent_path = paths.state_dir.join("library_recent.json");
        let library_recent: HashMap<String, u64> = std::fs::read_to_string(&library_recent_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let play_log = std::sync::Arc::new(PlayLog::load(&play_log_path));
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
            disk: crate::cache::Disk::start(),
            auth: Auth::LoggedOut,
            user: None,
            device_id: device_id0,
            status: None,
            update: None,
            update_banner: false,
            update_busy: false,
            update_note: None,
            update_stage: Stage::Idle,
            update_cancel: None,
            update_turn: 0,
            update_silent: false,
            update_retry: 0,
            update_confirm_jam: false,
            update_fake: false,
            restart_after_exit: None,
            restart_resume: false,
            update_applying: false,
            updated_from: None,
            resume_after_update: false,
            control_mode: false,
            health_at: None,
            health_marked: false,
            update_check_at,
            update_pending: 0,
            update_failed: None,
            update_failed_banner: false,
            update_install_after_check: false,
            notes_dialog: None,
            current_notes: None,
            updated_toast: None,
            playlists: Vec::new(),
            playlists_loaded: false,
            playlists_fresh: None,
            playlists_asked: false,
            playlists_source: "",
            playlists_web_at: 0,
            playlists_web_ids: HashSet::new(),
            playlists_web_pending: false,
            playlist_meta: HashMap::new(),
            lists: HashMap::new(),
            list_gen: 0,
            page_cache: HashMap::new(),
            filter_cache: None,
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
            lists_dir,
            list_cached: HashSet::new(),
            list_fresh: HashMap::new(),
            warming: HashMap::new(),
            warm_seq: 0,
            list_retry: HashMap::new(),
            pl_inflight: HashMap::new(),
            pl_rerun: HashSet::new(),
            pl_snap: HashMap::new(),
            pl_copy_info: HashMap::new(),
            list_disk: HashMap::new(),
            meta_after_edit: HashSet::new(),
            radios: None,
            restore_pending: None,
            restore_wanted: true,
            restore_awaiting: false,
            pause_after_restore: false,
            play_after_restore: false,
            reconnect_resume: None,
            pending_load: None,
            now_optimistic: false,
            now_placeholder: false,
            queue_retry: None,
            restore_mark: false,
            restore_fallback_at: None,
            pending_loadctx: None,
            restore_ctx: None,
            restore_ctx_at: None,
            restore_ctx_fails: 0,
            restore_ctx_until: None,
            prefetch_ids: std::collections::VecDeque::new(),
            prefetch_at: None,
            prefetch_inflight: None,
            prefetch_warm: None,
            pl_open_mark: None,
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
            album_of_pending: None,
            rows_added_by: false,
            queued_local: Vec::new(),
            pending_queue: None,
            shows: HashMap::new(),
            play_log,
            play_log_path,
            play_log_dirty: false,
            play_log_saved_at: Instant::now(),
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
            home_customize_open: false,
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
            load_watch: None,
            stuck_load: None,
            playback_error: None,
            load_retry: None,
            stall: None,
            warm: warm::WarmTracker::default(),
            warm_hover: None,
            warm_press: None,
            output_lost: None,
            output_resumes: watchdog::OutputResumes::default(),
            paused_while_playing: false,
            not_premium: false,
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
            search_pending: None,
            search_retry: None,
            search_retries: 0,
            search_result_for: None,
            search_cache: HashMap::new(),
            search_refreshing: HashSet::new(),
            search_pending_profile: None,
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
            lyrics_cache: HashMap::new(),
            lyrics_prefetched: HashSet::new(),
            lyrics_for: None,
            lyrics_loading: false,
            lyrics_button: None,
            lyrics_offset: 0.0,
            lyrics_follow_track: String::new(),
            lyrics_anim: None,
            frames_painted: 0,
            jam_open: false,
            jam: None,
            jam_link: String::new(),
            jam_busy: false,
            jam_at: Instant::now(),
            jam_error: None,
            editor: None,
            show_shortcuts: false,
            sidebar_pins_open: false,
            sidebar_playlists_open: false,
            library_grid: settings_library_grid,
            library_filter: String::new(),
            library_search_open: false,
            library_group_open: false,
            player_more_open: false,
            player_more_sub: None,
            player_more_at: None,
            song_more: None,
            song_more_req: None,
            queue_recent: false,
            queue_drag: None,
            queue_refresh: Vec::new(),
            add_from_bar: false,
            library_folder: None,
            library_recent,
            library_recent_path,
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
            web_chain: None,
            web_chain_login: false,
            login_cancel_at: None,
            media_dirty: false,
            snapshot_path,
            snapshot_dirty: false,
            snapshot_saved_at: Instant::now(),
            liked_synced_at: 0,
            liked_server_total: 0,
            artists_synced_at: 0,
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
        self.liked_synced_at = snap.liked_synced_at;
        self.liked_server_total = snap.liked_server_total;
        self.artists_synced_at = snap.artists_synced_at;
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
        if self.playlists_loaded {
            self.playlists_source = "instantánea";
        }
        self.playlists_web_at = snap.playlists_web_at;
        self.playlists_web_ids = snap.playlists_web_ids.into_iter().collect();
        // Sus snapshot_id son de la sesión anterior: no dicen si una copia sigue al día.
        self.playlists_fresh = None;
        self.recent = snap.recent;
        self.saved_albums = snap.saved_albums;
        // Una instantánea de antes los traía con sus pistas: así el próximo guardado ya encoge.
        for a in &mut self.saved_albums {
            a.tracks = None;
        }
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
            // Lo marcado aquí sin fila: corazón encendido y, al llegar con lo reciente, entra en
            // la lista sin contarse otra vez como nuevo de fuera.
            self.liked_set.extend(snap.liked_extra);
            let total = liked_total.max(liked.len() as u32);
            self.lists.insert(LIKED.to_string(), TrackList::new(liked, total, &mut self.list_gen));
            self.requested.insert(LIKED.to_string());
        }
    }

    fn snapshot(&self) -> Snapshot {
        let liked = self.lists.get(LIKED);
        // Ver `Snapshot::liked_extra`: lo marcado aquí que aún no tiene fila.
        let liked_extra: Vec<String> = match liked {
            Some(l) => {
                let rows: HashSet<&str> = l.tracks.iter().filter_map(|t| t.id.as_deref()).collect();
                self.liked_set.iter().filter(|id| !rows.contains(id.as_str())).cloned().collect()
            }
            None => Vec::new(),
        };
        Snapshot {
            saved_at: crate::cache::now_secs(),
            user: self.user.clone(),
            playlists: self.playlists.clone(),
            recent: self.recent.clone(),
            saved_albums: self.saved_albums.clone(),
            followed_artists: self.followed_artists.clone(),
            liked: liked.map(|l| l.tracks.clone()).unwrap_or_default(),
            liked_total: liked.map(|l| l.total).unwrap_or(0),
            liked_synced_at: self.liked_synced_at,
            liked_server_total: self.liked_server_total,
            liked_extra,
            artists_synced_at: self.artists_synced_at,
            playlists_web_at: self.playlists_web_at,
            playlists_web_ids: self.playlists_web_ids.iter().cloned().collect(),
        }
    }

    fn save_play_log_if_needed(&mut self, force: bool) {
        if !self.play_log_dirty {
            return;
        }
        if !force && self.play_log_saved_at.elapsed() < Duration::from_secs(5) {
            return;
        }
        self.play_log_dirty = false;
        self.play_log_saved_at = Instant::now();
        self.play_log.clone().save_async(&self.disk, self.play_log_path.clone());
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
        // Aquí solo la copia de los datos; serializarla y escribirla, en el hilo del disco.
        self.snapshot().save_async(&self.disk, self.snapshot_path.clone());
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
                    "playlist" if uri == DJ_URI => ("DJ".into(), None),
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

    /// Id de usuario propio: el del perfil (/me) o, si aún no llegó (primer arranque con la Web
    /// API limitada), el nombre de usuario de la sesión, que es el mismo id (el del rootlist).
    /// Así «Tu playlist» y la edición no dependen de la cuota compartida.
    pub fn my_id(&self) -> Option<&str> {
        self.user.as_ref().map(|u| u.id.as_str()).or(match &self.auth {
            Auth::LoggedIn { username } | Auth::Connecting { username } => Some(username.as_str()).filter(|u| !u.is_empty()),
            _ => None,
        })
    }

    /// Nombre visible propio, del perfil de /me o del interno.
    fn my_display_name(&self) -> Option<String> {
        self.user
            .as_ref()
            .and_then(|u| u.display_name.clone())
            .or_else(|| self.my_id().and_then(|id| self.users.get(id)).and_then(|u| u.display_name.clone()))
            .filter(|n| !n.is_empty())
    }

    /// Una playlist de la biblioteca sin portada (ni subida ni de la Web API), o con la armada
    /// aquí, toma el mosaico de su lista ya en memoria (`mosaic_cover`), como en Spotify. Así
    /// las propias sin imagen no se quedan en gris aunque la Web API no conteste.
    fn fill_list_cover(&mut self, id: &str) {
        let Some(i) = self.playlists.iter().position(|p| p.id == id && p.own_cover()) else { return };
        let Some(img) = self.lists.get(id).filter(|l| !l.tracks.is_empty()).and_then(|l| mosaic_cover(&l.tracks)) else { return };
        let p = &mut self.playlists[i];
        if p.images.as_deref().and_then(|v| v.first()).map(|im| im.url.as_str()) != Some(img.url.as_str()) {
            p.images = Some(vec![img]);
            self.snapshot_dirty = true;
        }
    }

    /// Pide a la Web API, en segundo plano, lo que el listado del rootlist no trae, si hace
    /// falta: hay playlists que no estaban la última vez que se completó (seguidas o creadas en
    /// otro dispositivo) o eso fue hace más de `PLAYLISTS_WEB_SECS` (los mosaicos de portada
    /// cambian con las canciones). La cuota es compartida con otros clientes: en cada arranque
    /// no se gasta.
    fn enrich_listing_if_needed(&mut self) {
        if self.playlists_web_pending || self.playlists.is_empty() {
            return;
        }
        let stale = crate::cache::now_secs().saturating_sub(self.playlists_web_at) > PLAYLISTS_WEB_SECS;
        let new = self.playlists.iter().filter(|p| !self.playlists_web_ids.contains(&p.id)).count();
        if !stale && new == 0 {
            return;
        }
        log::info!("[biblioteca] se completa el listado con la Web API (nuevas: {new}, caducado: {stale})");
        self.playlists_web_pending = true;
        self.api.send_bg(Req::PlaylistsWeb);
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

    /// Tono del degradado de la página actual: el de la portada de la playlist o el álbum
    /// abiertos, cuando ya está cargada.
    fn page_tint(&self) -> Option<egui::Color32> {
        let url = match self.page() {
            Page::Playlist(id) => self
                .playlists
                .iter()
                .find(|pl| &pl.id == id)
                .and_then(|pl| pl.cover(300))
                .or_else(|| self.playlist_meta.get(id).and_then(|pl| pl.cover(300)))
                .map(str::to_string),
            Page::Album(id) => self.albums.get(id).and_then(|a| a.cover(300)).map(str::to_string),
            _ => None,
        }?;
        self.images.tint(&url)
    }

    pub fn page(&self) -> &Page {
        match self.active {
            ActiveTab::Home => &Page::Home,
            ActiveTab::Search => &Page::Search,
            ActiveTab::Tab(i) => self.tabs.get(i).map(|t| t.page()).unwrap_or(&Page::Home),
        }
    }

    /// Navega: Inicio y Buscar son pestañas fijas; el resto de páginas se abren en la vista
    /// principal (sin pestaña visible) o, dentro de una pestaña abierta con «Abrir en una nueva
    /// pestaña», en su historial, como en un navegador.
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
                self.go_main_push(page);
            }
        }
    }

    /// Navegación de la barra lateral (y de «Tu biblioteca», Ajustes y el perfil): siempre en la
    /// vista principal, sin abrir ni cambiar ninguna pestaña visible.
    pub fn go_main(&mut self, page: Page) {
        if *self.page() == page {
            return;
        }
        match page {
            Page::Home | Page::Search => self.go(page),
            page => {
                self.selected = None;
                self.go_main_push(page);
            }
        }
    }

    /// Lleva `page` a la vista principal (creándola si hace falta) y la activa.
    fn go_main_push(&mut self, page: Page) {
        const MAX_HISTORY: usize = 50;
        let i = match self.tabs.iter().position(|t| t.hidden) {
            Some(i) => {
                let t = &mut self.tabs[i];
                if *t.page() != page {
                    t.history.truncate(t.idx + 1);
                    t.history.push(page);
                    if t.history.len() > MAX_HISTORY {
                        t.history.remove(0);
                    }
                    t.idx = t.history.len() - 1;
                }
                i
            }
            None => {
                self.tabs.push(Tab { history: vec![page], idx: 0, hidden: true });
                self.tabs.len() - 1
            }
        };
        self.active = ActiveTab::Tab(i);
    }

    /// Abre una pestaña visible nueva (en segundo plano) y devuelve su índice; si ya hay una
    /// visible con esa página, la suya.
    pub fn open_tab(&mut self, page: Page) -> usize {
        if let Some(i) = self.tabs.iter().position(|t| !t.hidden && *t.page() == page) {
            return i;
        }
        const MAX_TABS: usize = 8;
        if self.tabs.iter().filter(|t| !t.hidden).count() >= MAX_TABS {
            let victim = (0..self.tabs.len()).find(|&i| !self.tabs[i].hidden && self.active != ActiveTab::Tab(i)).unwrap_or(0);
            self.close_tab(victim);
        }
        self.tabs.push(Tab { history: vec![page], idx: 0, hidden: false });
        self.tabs.len() - 1
    }

    pub fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        let hidden = self.tabs.remove(i).hidden;
        self.active = match self.active {
            // Cerrar la vista principal (Ctrl+W en una página de la barra lateral) vuelve a Inicio.
            ActiveTab::Tab(a) if a == i && hidden => ActiveTab::Home,
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
        // Dentro de una carpeta de la biblioteca, «atrás» sale de ella.
        if self.library_folder.is_some() && *self.page() == Page::Library {
            self.library_folder = None;
            return;
        }
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

    fn on_update(&mut self, result: UpdateResult, manual: bool, location: Option<(PathBuf, MoveReason)>, staged: Option<PathBuf>) {
        self.update_busy = false;
        // Tras «Reintentar» la consulta cuenta como pedida por el usuario aunque fuera la
        // automática que ya estaba en marcha.
        let install_after = std::mem::take(&mut self.update_install_after_check);
        self.on_update_result(result, manual || install_after, location, staged, install_after);
        // «Reintentar» del aviso rojo: si al final no hay nada que instalar, el porqué (al día,
        // aún publicándose, sin conexión) no puede quedarse solo en Ajustes.
        if install_after && !self.update_banner && !self.update_stage.busy() {
            match self.update_note.clone() {
                Some((text, true)) => {
                    self.update_failed = Some(text);
                    self.update_failed_banner = true;
                }
                Some((text, false)) => self.status(text),
                None => {}
            }
        }
    }

    fn on_update_result(&mut self, result: UpdateResult, manual: bool, location: Option<(PathBuf, MoveReason)>, staged: Option<PathBuf>, install_after: bool) {
        // Instalando o a punto de reiniciar: ya nada cambia la versión que se instala.
        if self.update_applying || self.restart_after_exit.is_some() {
            log::info!("[update] consulta ignorada: se está instalando la versión nueva");
            return;
        }
        // Con una preparación en marcha, una consulta que llega a la vez (la de 6 h o el botón de
        // Ajustes) no la cambia ni la quita: su aviso y Ajustes enseñan el progreso.
        if matches!(self.update_stage, Stage::Downloading { .. } | Stage::Preparing) {
            log::info!("[update] consulta ignorada: hay una instalación en marcha");
            // La consulta manual borró la nota y Ajustes enseña el progreso debajo de ella.
            if let (true, Some(u)) = (manual, &self.update) {
                self.update_note = Some((format!("Hay una versión nueva: {}", u.version), false));
            }
            return;
        }
        // Lista para «Reiniciar»: solo la cambia una versión aún más nueva (y no omitida, salvo
        // que el usuario la busque a mano). Cualquier otra respuesta (la misma versión, un fallo
        // de red) la deja como está.
        let replaces_ready = match (&self.update_stage, &result) {
            (Stage::Ready { version, .. }, UpdateResult::Available(info)) => {
                crate::update::is_newer(&info.version, version) && (manual || !self.update_is_skipped(&info.version))
            }
            _ => false,
        };
        if let Stage::Ready { version, .. } = &self.update_stage {
            if !replaces_ready {
                if manual {
                    self.update_note = Some((format!("Nanofy {version} está lista"), false));
                    self.update_banner = true;
                }
                return;
            }
            log::info!("[update] la {version} estaba lista, pero ya hay otra más nueva");
        }
        match result {
            UpdateResult::Available(info) => {
                self.update_pending = 0;
                // Una consulta nueva parte de cero: el error del intento anterior (o el «sin
                // descarga para tu sistema» de una release a medio publicar) ya no vale.
                let staging = self.offer_update(info, manual, location, staged);
                // Si la nueva no se prepara sola, la vieja no debe instalarse al abrir Nanofy.
                // Si se prepara, ella misma empieza quitando la marca de lista de la vieja.
                if replaces_ready && !staging {
                    crate::update::discard_staged_async();
                }
                // «Reintentar» tras una versión que no arrancaba: lo pidió el usuario, así que se
                // instala aunque sea esa misma (sola no se volvería a preparar).
                if install_after && !staging && self.update_stage == Stage::Available {
                    self.install_update();
                }
                // Con «Actualizar automáticamente» se empezó a preparar en segundo plano, sin
                // aviso: tras pulsar «Reintentar» (que cerró el aviso rojo) el usuario no vería
                // nada hasta que estuviera lista. La pidió él: a la vista y como si fuera
                // «Instalar» (apagar la opción ya no la descarta).
                if install_after && staging && self.update_silent && matches!(self.update_stage, Stage::Downloading { .. } | Stage::Preparing) {
                    self.update_silent = false;
                    self.update_banner = true;
                }
            }
            UpdateResult::Pending(info) => self.on_update_pending(info, manual),
            UpdateResult::UpToDate => {
                self.update_pending = 0;
                self.update_retry = 0;
                self.update = None;
                self.update_banner = false;
                self.update_stage = Stage::Idle;
                self.update_note = Some((format!("Estás al día ({})", crate::update::current_version()), false));
            }
            UpdateResult::Failed(e) => {
                // La comprobación automática falla en silencio (sin red, límite de GitHub…).
                log::info!("[update] {e}");
                if manual {
                    self.update_note = Some((e, true));
                }
                // Era el reintento de una preparación automática que se quedó sin red y sigue sin
                // ella: toca el intento siguiente o, agotados, el aviso (una vez: sin red, la
                // consulta de cada 6 h no debe volver a sacarlo).
                let retrying = self.update_silent && matches!(self.update_stage, Stage::Failed { kind: FailKind::Network, .. });
                if !manual && retrying && !self.schedule_auto_retry() && self.update_retry as usize == crate::update::AUTO_RETRY.len() {
                    self.update_retry += 1;
                    self.update_banner = self.update.is_some();
                }
            }
        }
    }

    /// La versión omitida, o una que ya se instaló aquí y no llegaba a arrancar (se volvió a
    /// esta): no salta sola ni se prepara sola.
    fn update_is_skipped(&self, version: &str) -> bool {
        self.settings.update_skipped == version || crate::update::is_bad_version(version)
    }

    /// Hay una versión nueva con zip para este sistema. Con «Actualizar automáticamente» se
    /// prepara sola en segundo plano, sin aviso hasta que esté lista para «Reiniciar»; si no, se
    /// avisa y se instala con «Instalar». La omitida (o la que no arrancaba) solo se ofrece si el
    /// usuario la busca a mano, y aun así instalarla es cosa suya. Devuelve si empezó a prepararla.
    fn offer_update(&mut self, info: UpdateInfo, manual: bool, location: Option<(PathBuf, MoveReason)>, staged: Option<PathBuf>) -> bool {
        let skipped = self.update_is_skipped(&info.version);
        self.update_note = Some((format!("Hay una versión nueva: {}", info.version), false));
        self.update = Some(info);
        self.update_stage = Stage::Available;
        if skipped && !manual {
            self.update_banner = false;
            return false;
        }
        // Desde esta carpeta no puede sustituirse (en ningún modo): el aviso dice qué hacer.
        if let Some((dir, reason)) = location {
            self.update_stage = Stage::NeedsMove { dir, reason };
            self.update_banner = true;
            return false;
        }
        // Ya preparada y probada en una sesión anterior (al abrir no se pudo sustituir el
        // ejecutable): queda lista para «Reiniciar» sin descargarla otra vez. No cuenta como
        // preparada sola: apagar «Actualizar automáticamente» no la descarta (quizá la pidió el
        // usuario con «Instalar»), y de todos modos se instala al abrir Nanofy.
        if let Some(staged) = staged {
            let version = self.update.as_ref().map(|u| u.version.clone()).unwrap_or_default();
            log::info!("[update] la {version} ya estaba preparada");
            self.update_note = Some((format!("Nanofy {version} está lista"), false));
            self.update_silent = false;
            self.update_fake = false;
            self.update_confirm_jam = false;
            self.update_retry = 0;
            self.remember_release_notes(&version);
            self.update_stage = Stage::Ready { version, staged };
            self.update_banner = true;
            return true;
        }
        if !skipped && self.auto_update_on() {
            self.start_stage(true);
            return true;
        }
        self.update_banner = true;
        false
    }

    /// Guarda la versión nueva y saca su aviso (salvo que sea la omitida y no la haya pedido el
    /// usuario: la omitida no vuelve a saltar sola). Tampoco salta sola una versión que ya se
    /// instaló aquí y no llegaba a arrancar (se volvió a esta): solo si la pide el usuario.
    fn show_update(&mut self, info: UpdateInfo, manual: bool) {
        let skipped = !manual && self.update_is_skipped(&info.version);
        self.update_note = Some((format!("Hay una versión nueva: {}", info.version), false));
        self.update_banner = !skipped;
        self.update = Some(info);
    }

    /// La release más nueva aún no trae zip para esta plataforma: CI lo está subiendo o falló esa
    /// compilación. Un aviso ahora solo podría mandar al navegador, así que se calla y se vuelve
    /// a mirar en 10 min; si tras varias consultas sigue sin zip, se avisa de que esa versión no
    /// tiene descarga para este sistema.
    fn on_update_pending(&mut self, info: UpdateInfo, manual: bool) {
        self.update_pending = self.update_pending.saturating_add(1);
        if self.update_pending < crate::update::PENDING_MAX {
            // Solo si el aviso automático está activo: si no, `tick` seguiría consultando cada 6 h.
            if self.settings.update_check {
                self.update_check_at = Some(Instant::now() + crate::update::PENDING_RECHECK);
            }
            if manual {
                self.update_note = Some((format!("Hay una versión nueva ({}), pero aún se está publicando; vuelve a mirar en unos minutos", info.version), false));
            }
            return;
        }
        self.update_stage = Stage::Failed { kind: FailKind::NoAsset, detail: crate::update::NO_ASSET_TEXT.to_string() };
        self.show_update(info, manual);
    }

    /// Activa o desactiva el aviso automático; se guarda al instante.
    pub fn set_update_check(&mut self, on: bool) {
        self.settings.update_check = on;
        self.draft.update_check = on;
        self.update_check_at = on.then(|| Instant::now() + Duration::from_secs(6 * 3600));
        if !self.ephemeral {
            self.settings.save(&self.paths);
        }
        // Sin consulta automática tampoco hay actualización automática (ver `auto_update_on`).
        if !on {
            self.stop_silent_update();
        }
    }

    /// Fundido entre canciones: se aplica al reproductor al instante, sin reiniciarlo, y fuera de
    /// «Guardar» (como el aviso de actualizaciones): cambia los ajustes y el borrador a la vez, así
    /// que lo que hubiera sin guardar en el borrador sigue igual. `persist` = false mientras se
    /// arrastra el deslizador: suena ya con el valor nuevo y el archivo se escribe al soltarlo.
    /// Autoplay (canciones parecidas al acabarse lo que suena), desde el interruptor de la cola:
    /// al instante, sin «Guardar» ni reiniciar el reproductor.
    pub fn set_autoplay(&mut self, on: bool) {
        self.settings.autoplay = on;
        self.draft.autoplay = on;
        if !self.ephemeral {
            self.settings.save(&self.paths);
        }
        self.backend.send(Cmd::Autoplay(on));
    }

    pub fn set_crossfade(&mut self, on: bool, secs: u8, albums: bool, persist: bool) {
        let secs = secs.clamp(crate::config::CROSSFADE_SECS_MIN, crate::config::CROSSFADE_SECS_MAX);
        let changed = (self.settings.crossfade, self.settings.crossfade_secs, self.settings.crossfade_albums) != (on, secs, albums);
        for s in [&mut self.settings, &mut self.draft] {
            s.crossfade = on;
            s.crossfade_secs = secs;
            s.crossfade_albums = albums;
        }
        if changed {
            self.sync_crossfade();
        }
        if persist && !self.ephemeral {
            self.settings.save(&self.paths);
        }
    }

    /// Manda al reproductor el fundido en vigor (`Settings::crossfade_effective_ms`: 0 mientras el
    /// temporizador «al terminar la canción» está puesto). El backend lo guarda aparte de los
    /// ajustes y los reproductores nuevos (reconexión, `Restart`) nacen con él.
    fn sync_crossfade(&self) {
        let ms = self.settings.crossfade_effective_ms(self.sleep_end_of_track);
        self.backend.send(Cmd::Crossfade { ms, albums: self.settings.crossfade_albums });
    }

    /// Pone o quita el temporizador «al terminar la canción» (todas las vías: menú, modo de
    /// control, al cumplirse, al elegir minutos). Mientras está puesto no hay fundido: pausa al
    /// cambiar de canción, y con un fundido ese cambio llega al principio de la mezcla, con la
    /// siguiente ya sonando encima de la que acaba.
    pub fn set_sleep_end_of_track(&mut self, on: bool) {
        if self.sleep_end_of_track != on {
            self.sleep_end_of_track = on;
            if self.settings.crossfade {
                self.sync_crossfade();
            }
        }
    }

    /// Esta copia puede prepararse sola las versiones nuevas: no es una compilación de
    /// desarrollo, ni una sesión de capturas, ni el modo de control de las pruebas (salvo contra
    /// un servidor de releases local). Ver `update::auto_update_allowed`.
    pub fn auto_update_allowed(&self) -> bool {
        let exe = crate::update::target_exe();
        crate::update::auto_update_allowed(&crate::update::AutoGuard {
            debug: cfg!(debug_assertions),
            ephemeral: self.ephemeral,
            control: self.control_mode,
            test_server: crate::update::test_server_configured(),
            exe: exe.as_deref(),
        })
    }

    /// «Actualizar automáticamente» está en vigor: activado, con la consulta automática activa
    /// (sin ella la opción ni se puede tocar), en un sistema donde la app se sustituye sola y en
    /// una copia que puede hacerlo (`auto_update_allowed`).
    pub fn auto_update_on(&self) -> bool {
        self.settings.update_check && self.settings.update_auto && crate::update::can_self_install() && self.auto_update_allowed()
    }

    /// Activa o desactiva «Actualizar automáticamente»; se guarda al instante.
    pub fn set_update_auto(&mut self, on: bool) {
        self.settings.update_auto = on;
        self.draft.update_auto = on;
        if !self.ephemeral {
            self.settings.save(&self.paths);
        }
        if !on {
            self.stop_silent_update();
            return;
        }
        // Había una versión nueva esperando a «Instalar»: se prepara ya.
        let waiting = self.update_stage == Stage::Available
            && self.update.as_ref().is_some_and(|u| u.asset.is_some() && !self.update_is_skipped(&u.version));
        if waiting && !self.update_applying && self.restart_after_exit.is_none() && self.auto_update_on() {
            self.start_stage(true);
        }
    }

    /// Se apagó la actualización automática: lo que la app empezó a preparar sola ya no debe
    /// instalarse sin que el usuario lo pida (lo preparado se instalaría al abrir Nanofy), así que
    /// se para o se descarta y queda el aviso de siempre, con «Instalar».
    fn stop_silent_update(&mut self) {
        if !self.update_silent || self.update_applying || self.restart_after_exit.is_some() {
            return;
        }
        match self.update_stage {
            Stage::Downloading { .. } | Stage::Preparing => {
                self.cancel_update();
                self.update_banner = true;
            }
            Stage::Ready { .. } => {
                crate::update::discard_staged_async();
                self.update_silent = false;
                self.update_confirm_jam = false;
                self.update_stage = Stage::Available;
                self.update_banner = true;
            }
            _ => {}
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

    /// Carpeta de las descargas de la actualización: la de estado, no junto al ejecutable, para
    /// que si este vive en una carpeta de OneDrive no se suba ni se bloquee a medias.
    fn update_work_dir(&self) -> PathBuf {
        self.paths.state_dir.join("update")
    }

    /// «Instalar» (y «Reintentar» tras un fallo, que empieza de cero): prepara la versión nueva
    /// en segundo plano con su aviso a la vista. Al terminar queda lista para «Reiniciar».
    pub fn install_update(&mut self) {
        let Some(u) = self.update.as_ref() else {
            return;
        };
        if self.update_applying || self.restart_after_exit.is_some() || self.update_stage.busy() {
            return;
        }
        if u.asset.is_none() {
            self.update_stage = Stage::Failed { kind: FailKind::NoAsset, detail: crate::update::NO_ASSET_TEXT.to_string() };
            return;
        }
        if !crate::update::can_self_install() {
            self.update_stage = Stage::Failed { kind: FailKind::NoAsset, detail: "En este sistema la actualización se instala a mano: descarga el zip".to_string() };
            return;
        }
        self.update_retry = 0;
        self.start_stage(false);
    }

    /// Empieza a preparar `self.update` en un hilo (descarga comprobada y autoprueba). `silent`:
    /// la empezó la app sola, sin aviso hasta que esté lista. Si había otra en marcha, se para:
    /// solo cuenta la última (sus mensajes llevan su turno).
    fn start_stage(&mut self, silent: bool) {
        let Some(u) = self.update.clone() else {
            return;
        };
        if let Some(old) = self.update_cancel.take() {
            old.store(true, Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.update_cancel = Some(cancel.clone());
        self.update_fake = false;
        self.update_silent = silent;
        self.update_confirm_jam = false;
        self.update_banner = !silent;
        self.update_stage = Stage::Downloading { done: 0, total: None };
        log::info!("[update] preparando la {}{}", u.version, if silent { " en segundo plano" } else { "" });
        self.update_turn = crate::update::stage(self.ui_tx.clone(), u, self.update_work_dir(), cancel);
    }

    /// «Cancelar» la descarga o la preparación: para el hilo, borra la descarga a medias y la
    /// versión nueva vuelve a estar solo disponible (se puede instalar otra vez).
    pub fn cancel_update(&mut self) {
        if !matches!(self.update_stage, Stage::Downloading { .. } | Stage::Preparing) {
            return;
        }
        if let Some(cancel) = self.update_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        // El hilo la borra en cuanto ve la cancelación, pero una descarga atascada puede tardar
        // en devolverle el control.
        if let Some(u) = &self.update {
            crate::update::remove_attempt_downloads(&self.update_work_dir(), &u.version, self.update_turn);
        }
        self.update_silent = false;
        self.update_stage = Stage::Available;
        log::info!("[update] preparación cancelada");
    }

    fn on_update_stage(&mut self, turn: u64, stage: Stage) {
        // De una preparación cancelada o sustituida por otra (sus últimos mensajes pueden llegar
        // después): ya no dice nada de la actual.
        if turn != self.update_turn || !matches!(self.update_stage, Stage::Downloading { .. } | Stage::Preparing) {
            // La de este turno terminó justo cuando se cancelaba («Cancelar», o se apagó
            // «Actualizar automáticamente») y su hilo ya no lo vio: lo que dejó listo no debe
            // instalarse al abrir Nanofy. Una de otro turno no: si hay otra después, es suya. Con
            // un estado de prueba (`update_fake_stage`, que la paró) ninguna real debe quedar.
            let shown_ready = matches!(self.update_stage, Stage::Ready { .. });
            let cancelled = turn != 0 && (self.update_fake || (turn == self.update_turn && !shown_ready));
            if cancelled && matches!(stage, Stage::Ready { .. }) && !self.update_applying && self.restart_after_exit.is_none() {
                log::info!("[update] la versión preparada llegó tras cancelar: se descarta");
                crate::update::discard_staged_async();
                return;
            }
            log::debug!("[update] estado de una preparación anterior ignorado: {}", stage.name());
            return;
        }
        match &stage {
            Stage::Ready { version, .. } => {
                self.update_cancel = None;
                self.update_retry = 0;
                self.update_note = Some((format!("Nanofy {version} está lista"), false));
                // Lista y probada, pero no se instala sin que el usuario lo diga («Reiniciar»), o
                // sola al abrir Nanofy la próxima vez. El aviso sale también si se preparó sola:
                // es lo único que queda por hacer.
                self.update_banner = true;
                self.remember_release_notes(version);
            }
            Stage::Failed { kind, .. } => {
                self.update_cancel = None;
                // Preparándola sola, un corte de red no merece aviso: se vuelve a intentar.
                if !(self.update_silent && *kind == FailKind::Network && self.schedule_auto_retry()) {
                    self.update_banner = true;
                }
            }
            Stage::NeedsMove { .. } => {
                self.update_cancel = None;
                self.update_banner = true;
            }
            Stage::Idle | Stage::Available | Stage::Downloading { .. } | Stage::Preparing => {}
        }
        self.update_stage = stage;
    }

    /// Tras quedarse sin red preparando sola la versión nueva, programa otra consulta (que vuelve
    /// a prepararla) en 5 min y luego en 30. Agotados, devuelve `false`: se avisa y queda la
    /// consulta normal de cada 6 h.
    fn schedule_auto_retry(&mut self) -> bool {
        let Some(wait) = crate::update::AUTO_RETRY.get(self.update_retry as usize).copied() else {
            return false;
        };
        if !self.settings.update_check {
            return false;
        }
        self.update_retry += 1;
        self.update_check_at = Some(Instant::now() + wait);
        log::info!("[update] sin red para la versión nueva; otro intento en {} min", wait.as_secs() / 60);
        true
    }

    /// «Reiniciar»: sustituye el ejecutable por la versión preparada y, si sale bien, cierra para
    /// abrir la nueva, que retoma la música si sonaba. La sustitución va en un hilo con la
    /// ventana aún abierta, nunca al cerrar: el vigilante de salida de 3 s podría cortarla entre
    /// los dos renombrados y dejar al usuario sin nanofy.exe. Si no se puede, la versión
    /// preparada se instala al abrir Nanofy la próxima vez. En una Jam se sale de ella al
    /// reiniciar: sin `confirm_jam`, el aviso pide confirmarlo primero.
    pub fn restart_to_update(&mut self, confirm_jam: bool) {
        if self.update_applying || self.restart_after_exit.is_some() {
            return;
        }
        if !matches!(self.update_stage, Stage::Ready { .. }) {
            return;
        }
        if self.jam.is_some() && !confirm_jam {
            self.update_confirm_jam = true;
            self.update_banner = true;
            return;
        }
        self.update_confirm_jam = false;
        if self.update_fake {
            // Un «lista» de mentira para capturas: lo que hubiera preparado de verdad junto al
            // ejecutable (la compilación con la que se trabaja) no debe instalarse por él.
            self.update_stage = Stage::Failed { kind: FailKind::Swap, detail: "Estado de prueba: no hay ninguna versión preparada".to_string() };
            return;
        }
        let Some(target) = crate::update::target_exe() else {
            self.update_stage = Stage::Failed { kind: FailKind::Swap, detail: "No se encuentra el ejecutable actual".to_string() };
            self.update_banner = true;
            return;
        };
        if let Stage::Ready { version, staged } = &self.update_stage {
            log::info!("[update] instalando la {version} ({} → {})", staged.display(), target.display());
        }
        self.update_applying = true;
        let ui = self.ui_tx.clone();
        let spawn = std::thread::Builder::new().name("nanofy-update-apply".to_string()).spawn(move || {
            let result = crate::update::apply_pending(&target).map(|()| target.clone()).map_err(|e| crate::update::swap_failure_text(&target, &e));
            ui.send(Msg::UpdateApplied(result));
        });
        if let Err(e) = spawn {
            self.update_applying = false;
            self.update_stage = Stage::Failed { kind: FailKind::Swap, detail: format!("No se pudo instalar la versión nueva ({e})") };
            self.update_banner = true;
        }
    }

    fn on_update_applied(&mut self, ctx: &egui::Context, result: Result<PathBuf, String>) {
        self.update_applying = false;
        match result {
            Ok(target) => {
                // Se decide ahora, con la ventana a punto de cerrarse: si suena aquí (no en otro
                // dispositivo por Connect), la versión nueva sigue en el mismo segundo.
                self.restart_resume = self.player.remote.is_none() && matches!(self.player.state, PlayState::Playing | PlayState::Loading);
                log::info!("[update] ejecutable sustituido; reiniciando ({})", if self.restart_resume { "la música seguirá" } else { "sin música que retomar" });
                self.restart_after_exit = Some(target);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(e) => {
                // El motivo real: el aviso ya ofrece «Reintentar» y «Descargar a mano».
                log::warn!("[update] {e}");
                self.update_stage = Stage::Failed { kind: FailKind::Swap, detail: e };
                self.update_banner = true;
            }
        }
    }

    /// `--updated-from <versión>` y `--resume-playing`: esta ventana es la versión recién
    /// instalada (por «Reiniciar» o al abrir) y, si sonaba música al reiniciar, la retoma.
    pub fn apply_update_flags(&mut self, updated_from: Option<String>, resume: bool) {
        if let Some(v) = &updated_from {
            log::info!("[update] actualizada desde la {v}{}", if resume { "; se retomará la música" } else { "" });
        }
        // Sin --updated-from no viene de un reinicio para actualizar: nunca suena sola.
        self.resume_after_update = resume && updated_from.is_some();
        if updated_from.is_some() {
            let version = crate::update::current_version();
            self.updated_toast = Some(UpdatedToast { version: version.clone(), since: None });
            // Las notas de esta versión las guardó la anterior al prepararla; para esta ya no hay
            // versión nueva que las traiga. En el hilo del disco: la ventana no espera por ellas.
            let (dir, ui) = (self.update_work_dir(), self.ui_tx.clone());
            self.disk.run(move || {
                if let Some(info) = crate::update::load_notes(&dir, &version) {
                    ui.send(Msg::ReleaseNotes(info));
                }
            });
        }
        self.updated_from = updated_from;
    }

    /// «Ver novedades»: el diálogo con las notas completas de `info` o, si la release no trae
    /// notas (las anteriores a 1.7 no las tienen), su página de GitHub.
    pub fn open_release_notes(&mut self, ctx: &egui::Context, info: Option<UpdateInfo>) {
        let Some(info) = info else {
            return;
        };
        if crate::update::note_lines(&info.notes).is_empty() {
            ctx.open_url(egui::OpenUrl::new_tab(info.page_url.clone()));
            self.status("Se han abierto las novedades en el navegador");
        } else {
            self.notes_dialog = Some(info);
        }
    }

    /// Notas de la versión en uso para «Ver novedades» del aviso de después de actualizar: las
    /// guardadas o, si no hay, solo la página de su release.
    pub fn current_release_notes(&self) -> UpdateInfo {
        let version = crate::update::current_version();
        self.current_notes.clone().filter(|n| n.version == version).unwrap_or_else(|| UpdateInfo {
            page_url: format!("{}/tag/v{version}", crate::update::RELEASES_URL),
            notes: String::new(),
            asset: None,
            version,
        })
    }

    /// La versión nueva quedó lista: sus notas se guardan para la ventana que se abrirá con ella.
    fn remember_release_notes(&self, version: &str) {
        if let Some(info) = self.update.clone().filter(|u| u.version == version) {
            let dir = self.update_work_dir();
            self.disk.run(move || crate::update::save_notes(&dir, &info));
        }
    }

    /// Página actual como valor de `--page` (lo que entiende `apply_start_flags`), para que la
    /// versión nueva se abra donde estaba el usuario. En el inicio no hace falta.
    fn restart_page(&self) -> Option<String> {
        let spec = control::page_spec(self.page());
        let plain = matches!(
            spec.as_str(),
            "library" | "search" | "liked" | "albums" | "artists" | "settings" | "history" | "saves" | "shows" | "audiobooks" | "folders"
        );
        let with_id = ["playlist:", "album:", "artist:", "show:"].iter().any(|p| spec.starts_with(p));
        (plain || with_id).then_some(spec)
    }

    /// Se abrió con `--update-failed`, o la versión nueva no arrancó al instalarla al abrir: se
    /// sigue con esta y se dice por qué.
    pub fn show_update_failed(&mut self, msg: &str) {
        log::warn!("[update] {msg}");
        self.update_note = Some((msg.to_string(), true));
        self.update_failed = Some(msg.to_string());
        self.update_failed_banner = true;
    }

    /// «Reintentar» del aviso rojo (o de Ajustes, con un error de consulta): vuelve a mirar y, si
    /// era una actualización que no arrancó, instala la versión nueva en cuanto llegue.
    pub fn retry_failed_update(&mut self) {
        self.update_failed_banner = false;
        if self.update_failed.take().is_some() {
            self.update_install_after_check = true;
            // Si ya había una consulta en marcha, su resultado no debe tomar el aviso viejo
            // como el error de este intento.
            self.update_note = None;
        }
        self.check_updates(true);
    }

    /// Abre en el navegador el zip de esta plataforma (`download`) o la página de la release.
    pub fn open_update(&mut self, ctx: &egui::Context, download: bool) {
        let Some(u) = &self.update else {
            return;
        };
        let url = match (&u.asset, download) {
            (Some(asset), true) => asset.url.clone(),
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

    /// Nombre e imagen de estos artistas en UNA petición (lote de metadatos internos), cada uno
    /// una vez por sesión con la misma clave que usaba la tarjeta. Antes era un Req::Artist por
    /// artista: una lectura de la Web API y segundos de búsqueda de géneros en los hilos comunes.
    pub fn request_artist_thumbs(&mut self, ids: Vec<String>) {
        if !self.logged_in() {
            return;
        }
        let ids: Vec<String> = ids.into_iter().filter(|id| self.requested.insert(format!("artistmeta:{id}"))).collect();
        if !ids.is_empty() {
            self.api.send(Req::ArtistThumbs(ids));
        }
    }

    pub fn invalidate(&mut self, key: &str) {
        self.requested.remove(key);
    }

    /// Pide las pistas de una playlist (una vez por sesión), salvo si espera un reintento tras
    /// una carga fallida: ese lo lanza tick con su espera, o ella con «Reintentar». Pedirla aquí
    /// en cada fotograma lo repetiría en bucle.
    pub fn request_list(&mut self, id: &str) {
        if !self.list_retry.contains_key(id) && !self.requested.contains(&format!("pl:{id}")) {
            self.load_playlist(id, false, false);
        }
    }

    /// Hay una carga de esa playlist en vuelo (con noticias recientes).
    fn pl_busy(&self, id: &str) -> bool {
        self.pl_inflight.get(id).is_some_and(|t| t.elapsed() < PL_INFLIGHT_STALE)
    }

    /// Única puerta para pedir las pistas de una playlist (página, inicio, precarga,
    /// restauración, reintentos, ediciones). Nunca lanza dos cargas de la misma a la vez: sus
    /// lotes se mezclaban y la lista (y su copia en disco) quedaba con pistas repetidas o cortada.
    /// Con una ya en vuelo no hace nada, salvo `force` (una edición, una recarga pedida): entonces
    /// se repite en cuanto termine esa, que puede traer la versión de antes del cambio.
    pub fn load_playlist(&mut self, id: &str, prio: bool, force: bool) {
        if !self.logged_in() {
            return;
        }
        if self.pl_busy(id) {
            if force {
                self.pl_rerun.insert(id.to_string());
            }
            return;
        }
        // La copia que deje esta carga lleva el snapshot_id del listado de ahora: lo que llegue
        // es al menos igual de nuevo. Si la anterior se dio por perdida y aún responde, su lista
        // (quizá de antes) se guardaría con este: mejor sin ninguno, que solo cuesta recargarla.
        let snap = self.playlists.iter().find(|p| p.id == id).and_then(|p| p.snapshot_id.clone());
        let lost = self.pl_inflight.remove(id);
        if let Some(t) = lost {
            log::info!("playlist {id}: la carga anterior lleva {} s sin noticias; se pide otra", t.elapsed().as_secs());
        }
        self.pl_snap.insert(id.to_string(), snap.filter(|_| lost.is_none()));
        self.pl_copy_info.remove(id);
        // Sin otra en vuelo, lo que quede aparte es de una carga que no terminó.
        self.list_fresh.remove(id);
        // Si ya se ve (en memoria o la copia del disco), lo nuevo se junta aparte y la sustituye
        // entera al terminar: tras una reconexión o una edición, la playlist abierta ya no se
        // vacía para volver de 100 en 100. Si no se ve nada, mejor que salga según llega.
        if self.lists.get(id).is_some_and(|l| !l.tracks.is_empty()) {
            self.list_cached.insert(id.to_string());
        }
        self.requested.insert(format!("pl:{id}"));
        self.pl_inflight.insert(id.to_string(), Instant::now());
        let req = Req::PlaylistTracks(id.to_string());
        if prio {
            self.api.send_priority(req);
        } else {
            self.api.send(req);
        }
    }

    /// Hay un reintento de esa playlist en vuelo.
    pub fn list_retry_busy(&self, id: &str) -> bool {
        self.list_retry.get(id).is_some_and(|r| r.0.is_none()) && self.pl_busy(id)
    }

    /// Vuelve a pedir una playlist que quedó a medias (reintento automático o «Reintentar»).
    pub fn retry_list(&mut self, id: &str) {
        if !self.logged_in() {
            return;
        }
        let sent = self.list_retry.get(id).map_or(0, |r| r.1);
        // Con otra carga en vuelo, una segunda mezclaría sus lotes: esa hace de reintento. Si
        // termina completa se quita de aquí; si falla, su error programa el siguiente.
        if self.pl_busy(id) {
            if let Some(r) = self.list_retry.get_mut(id) {
                r.0 = None;
            }
            return;
        }
        self.list_retry.insert(id.to_string(), (None, sent.saturating_add(1)));
        self.load_playlist(id, false, false);
    }

    /// Programa el siguiente reintento de una playlist cuya carga falló, con espera creciente.
    fn schedule_list_retry(&mut self, id: &str, e: &str) {
        // La restauración de la sesión ya la repite por su cuenta: otra carga a la vez mezclaría
        // sus lotes.
        if self.restoring_playlist(id) {
            self.list_retry.remove(id);
            return;
        }
        let mut sent = self.list_retry.get(id).map_or(0, |r| r.1);
        // Sin sesión (reconexión en curso) falla al momento y no cuenta como intento: la sesión
        // suele volver en segundos y no deben gastarse los reintentos esperándola.
        if e.contains("no has iniciado sesión") {
            sent = sent.saturating_sub(1);
        }
        match LIST_RETRY_DELAYS.get(sent as usize) {
            Some(wait) => {
                log::info!("playlist {id}: {e}; reintento {} en {} s", sent + 1, wait.as_secs());
                self.list_retry.insert(id.to_string(), (Some(Instant::now() + *wait), sent));
            }
            None => {
                log::info!("playlist {id}: {e}; sin más reintentos automáticos");
                self.list_retry.insert(id.to_string(), (None, sent));
            }
        }
    }

    /// Si una playlist aún no está en memoria, pide leer su última copia en disco para mostrarla
    /// en cuanto llegue (`on_warmed`, uno o dos fotogramas). La petición a Spotify sigue su
    /// curso y la sustituye al llegar.
    pub fn warm_list(&mut self, id: &str) {
        if self.lists.contains_key(id) || !is_playlist_key(id) {
            return;
        }
        // Aunque su carga ya esté pedida (p. ej. por la precarga) se muestra la copia: mientras
        // no llegue el primer lote no hay nada en `lists`, y con `list_cached` lo fresco se junta
        // aparte y la sustituye entera al terminar. Una sola lectura del disco por id: la página
        // llama aquí en cada fotograma y, sin copia, se releería un fichero que no existe.
        if !self.requested.insert(format!("warm:{id}")) {
            return;
        }
        // Aquí solo se mira si existe (barato): leer y parsear una copia de miles de pistas
        // costaba 5-20 ms del fotograma al abrirla. Sin copia no se espera nada: las pistas se
        // ven según llegan, como siempre.
        let path = self.lists_dir.join(format!("{id}.json"));
        if !path.is_file() {
            return;
        }
        self.warm_seq += 1;
        let seq = self.warm_seq;
        self.warming.insert(id.to_string(), seq);
        let (tx, id) = (self.ui_tx.clone(), id.to_string());
        self.disk.run(move || {
            let list = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| serde_json::from_str::<CachedList>(&t).ok())
                .map(Box::new);
            tx.send(Msg::Warmed { id, seq, list });
        });
    }

    /// Llegó la copia en disco pedida por `warm_list`. Se pone solo si sigue siendo la última
    /// lectura pedida de esa playlist y nada fresco se adelantó: ni un lote (que la quita de
    /// `warming`) ni otra copia a la vista. Lo que haya en `lists` tiene que ser el hueco vacío
    /// que deja la página mientras espera. Nunca se cambia una carga a medias por la copia: sus
    /// lotes siguientes irían aparte (`list_fresh`) y la lista acabaría mezclada.
    fn on_warmed(&mut self, id: String, seq: u64, list: Option<Box<CachedList>>) {
        if self.warming.get(&id) != Some(&seq) {
            return;
        }
        self.warming.remove(&id);
        // Las secciones del inicio la esperaban para decidir si pedirla: sin copia legible, su
        // clave de caché no cambia y no se volverían a mirar.
        if self.settings.home_custom.contains(&id) {
            self.home_cache = None;
        }
        let Some(cached) = list else {
            self.playlist_meta_fallback(&id);
            return;
        };
        let placeholder = self.lists.get(&id).map_or(true, |l| l.tracks.is_empty() && !l.loading);
        if !placeholder || self.list_cached.contains(&id) || self.list_fresh.contains_key(&id) {
            return;
        }
        let cached = *cached;
        if let Some(meta) = cached.meta {
            self.playlist_meta.entry(id.clone()).or_insert(meta);
        }
        let total = cached.tracks.len() as u32;
        let list = TrackList::new(cached.tracks, total, &mut self.list_gen);
        self.list_disk.insert(id.clone(), ListDisk { snapshot_id: cached.snapshot_id, saved_at: cached.saved_at, gen: list.gen });
        self.lists.insert(id.clone(), list);
        self.list_cached.insert(id.clone());
        self.fill_list_cover(&id);
        // Puede ser el contexto de la sesión restaurada: su cola se arma ya con la copia.
        self.restore_ctx_arrived(&id);
        // Después: si la cola salió de la copia, ya no hay carga de la restauración que lo traiga.
        self.playlist_meta_fallback(&id);
    }

    /// La copia en disco que iba a dar el nombre de una playlist de fuera de la biblioteca no lo
    /// dio (ilegible o guardada sin metadatos) y ninguna carga lo traerá: se piden a la Web API,
    /// como antes de esperar a la copia (ver ensure_context_meta).
    fn playlist_meta_fallback(&mut self, id: &str) {
        if self.playlist_meta.contains_key(id)
            || self.playlists.iter().any(|p| p.id == id)
            || self.pl_busy(id)
            || self.restoring_playlist(id)
        {
            return;
        }
        self.request_once(&format!("plmeta:{id}"), Req::PlaylistMeta(id.to_string()));
    }

    /// Muestra una playlist (su copia en disco si aún no está en memoria) y pide sus pistas si
    /// hace falta. Para la página, las secciones del inicio y la precarga: antes cada arranque
    /// volvía a bajar entera cada playlist abierta, precargada o del inicio aunque no cambiara.
    pub fn ensure_playlist(&mut self, id: &str) {
        self.warm_list(id);
        // Su copia se está leyendo: hasta tenerla no se sabe si sigue al día (`skip_unchanged`).
        // Al llegar despierta la interfaz y se decide entonces.
        if self.warming.contains_key(id) {
            return;
        }
        if self.requested.contains(&format!("pl:{id}")) || self.skip_unchanged(id) {
            return;
        }
        self.request_list(id);
    }

    /// Hay un listado de playlists de esta sesión y es reciente. Uno de hace horas ya no dice si
    /// una playlist que se abre por primera vez sigue igual (pudo editarse en el móvil
    /// mientras tanto): pasado LISTING_TRUST se pide como antes (y es barata: la API reutiliza
    /// las pistas que ya tiene su copia).
    fn listing_fresh(&self) -> bool {
        self.playlists_fresh.is_some_and(|t| t.elapsed() < LISTING_TRUST)
    }

    /// Ya no se espera el listado pedido al conectar: llegó (y es reciente) o falló. Hasta
    /// entonces, lo que solo se ve de paso (las secciones del inicio) se queda con su copia en
    /// disco en vez de pedirse sin saber si cambió.
    fn listing_settled(&self) -> bool {
        self.listing_fresh() || !self.playlists_asked
    }

    /// La lista que se ve de esa playlist es su copia en disco y el listado de esta sesión dice
    /// que no ha cambiado desde que se guardó (mismo snapshot_id, guardada hace menos de
    /// LIST_UNCHANGED_SECS). Nunca con una carga en vuelo o un reintento pendiente.
    fn list_unchanged(&self, id: &str) -> bool {
        if !self.listing_fresh() || self.pl_busy(id) || self.list_retry.contains_key(id) {
            return false;
        }
        let (Some(list), Some(disk)) = (self.lists.get(id), self.list_disk.get(id)) else { return false };
        let Some(snap) = disk.snapshot_id.as_deref() else { return false };
        let listed = self.playlists.iter().find(|p| p.id == id).and_then(|p| p.snapshot_id.as_deref());
        // Una fecha futura (reloj atrasado al guardarla) no cuenta como reciente.
        let age = crate::cache::now_secs().checked_sub(disk.saved_at);
        list.gen == disk.gen && listed == Some(snap) && age.is_some_and(|a| a < LIST_UNCHANGED_SECS)
    }

    /// Si la copia que se ve está al día (`list_unchanged`), la da por cargada sin pedir nada:
    /// ya no espera una versión fresca y la página no la vuelve a pedir en esta sesión (salvo
    /// reconexión, edición o «Reintentar»).
    fn skip_unchanged(&mut self, id: &str) -> bool {
        if !self.list_unchanged(id) {
            return false;
        }
        self.list_cached.remove(id);
        self.list_fresh.remove(id);
        if let Some(l) = self.lists.get_mut(id) {
            l.loading = false;
        }
        self.requested.insert(format!("pl:{id}"));
        log::info!("playlist {id}: sin cambios desde su copia en disco (snapshot_id); no se pide");
        true
    }

    /// Igual que `warm_list`, para un álbum: su última copia en disco mientras llega la fresca.
    pub fn warm_album(&mut self, id: &str) {
        if self.albums.contains_key(id) || !is_playlist_key(id) || !self.requested.insert(format!("warmalb:{id}")) {
            return;
        }
        let path = self.lists_dir.with_file_name("albums").join(format!("{id}.json"));
        if let Some(album) = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Album>(&t).ok()) {
            self.albums.insert(id.to_string(), album);
        }
    }

    fn save_album(&self, album: &Album) {
        if !is_playlist_key(&album.id) {
            return;
        }
        let dir = self.lists_dir.with_file_name("albums");
        let path = dir.join(format!("{}.json", album.id));
        if let Ok(text) = serde_json::to_string(album) {
            // Como save_list: en orden, sin dos escrituras del mismo fichero a la vez.
            self.disk.run(move || {
                crate::cache::write_atomic(&path, &text);
                crate::cache::prune_dir(&dir, CACHED_MAX);
            });
        }
    }

    /// Antigüedad de la copia en disco de una playlist (`None` si no hay copia). Una fecha en el
    /// futuro (reloj atrasado) cuenta como recién guardada.
    fn list_copy_age(&self, id: &str) -> Option<Duration> {
        let modified = std::fs::metadata(self.lists_dir.join(format!("{id}.json"))).ok()?.modified().ok()?;
        Some(modified.elapsed().unwrap_or_default())
    }

    /// Lo mismo para todas las copias, de una pasada por la carpeta: en Windows la fecha viene con
    /// la propia enumeración, sin abrir fichero a fichero cuando hay cientos de playlists.
    fn list_copy_ages(&self) -> HashMap<String, Duration> {
        let Ok(dir) = std::fs::read_dir(&self.lists_dir) else { return HashMap::new() };
        dir.flatten()
            .filter_map(|e| {
                let id = e.file_name().into_string().ok()?.strip_suffix(".json")?.to_string();
                let modified = e.metadata().ok()?.modified().ok()?;
                Some((id, modified.elapsed().unwrap_or_default()))
            })
            .collect()
    }

    /// Guarda los metadatos de una playlist (ya mezclados con los anteriores).
    fn store_playlist_meta(&mut self, mut p: Playlist) {
        // Radios y mixes: la portada generada por Spotify solo llega en el feed de inicio.
        if p.images.as_ref().map(|v| v.is_empty()).unwrap_or(true) {
            if let Some(it) = self.home_feed.iter().flat_map(|s| s.items.iter()).find(|it| it.uri == p.uri) {
                p.images = it.image.as_ref().map(|u| vec![Image { url: u.clone(), width: Some(300), height: Some(300) }]);
            }
        }
        self.playlist_meta.insert(p.id.clone(), p);
    }

    /// Guarda en disco (en segundo plano) la versión completa de una playlist, con el
    /// snapshot_id de cuando se pidió y lo que dijo la API de sus metadatos.
    fn save_list(&mut self, id: &str) {
        let Some(list) = self.lists.get(id) else { return };
        let (tracks, gen) = (list.tracks.clone(), list.gen);
        let snapshot_id = self.pl_snap.remove(id).flatten();
        let info = self.pl_copy_info.remove(id).unwrap_or_default();
        let saved_at = crate::cache::now_secs();
        self.list_disk.insert(id.to_string(), ListDisk { snapshot_id: snapshot_id.clone(), saved_at, gen });
        let cached = CachedList {
            meta: self.playlist_meta.get(id).cloned(),
            tracks,
            snapshot_id,
            saved_at,
            meta_at: info.meta_at,
            country: info.country,
        };
        let (dir, path) = (self.lists_dir.clone(), self.lists_dir.join(format!("{id}.json")));
        // En el hilo del disco y en orden con las lecturas de warm_list y el borrado de
        // forget_list: con un hilo por guardado, dos cargas seguidas (una edición mientras
        // llegaba) podían pisarse el mismo `.json.tmp`, o una vieja quedar encima de la última o
        // resucitar una copia ya olvidada.
        self.disk.run(move || {
            if let Ok(text) = serde_json::to_string(&cached) {
                crate::cache::write_atomic(&path, &text);
                crate::cache::prune_dir(&dir, CACHED_MAX);
            }
        });
    }

    /// Pestaña nueva y activa con la playlist de una radio.
    fn open_radio_tab(&mut self, playlist_id: String) {
        let page = Page::Playlist(playlist_id);
        let before = self.tabs.len();
        self.open_tab(page.clone());
        if self.tabs.len() > before {
            self.active = ActiveTab::Tab(self.tabs.len() - 1);
        } else if let Some(i) = self.tabs.iter().position(|t| !t.hidden && t.page() == &page) {
            self.active = ActiveTab::Tab(i);
        }
    }

    /// Radios ya resueltas (canción semilla → playlist), leídas del disco la primera vez:
    /// averiguar la playlist de una radio cuesta una ida y vuelta a Spotify.
    fn radios(&mut self) -> &HashMap<String, String> {
        if self.radios.is_none() {
            let map = std::fs::read_to_string(self.lists_dir.join("radios.json"))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default();
            self.radios = Some(map);
        }
        self.radios.as_ref().unwrap()
    }

    fn remember_radio(&mut self, seed: &str, playlist_id: &str) {
        self.radios();
        if let Some(map) = self.radios.as_mut() {
            if map.get(seed).map(String::as_str) != Some(playlist_id) {
                map.insert(seed.to_string(), playlist_id.to_string());
                self.save_radios();
            }
        }
    }

    fn forget_radio(&mut self, playlist_id: &str) {
        let Some(map) = self.radios.as_mut() else { return };
        let before = map.len();
        map.retain(|_, pid| pid != playlist_id);
        if map.len() != before {
            self.save_radios();
        }
    }

    fn save_radios(&self) {
        let Some(map) = self.radios.as_ref() else { return };
        let path = self.lists_dir.join("radios.json");
        if let Ok(text) = serde_json::to_string(map) {
            self.disk.run(move || crate::cache::write_atomic(&path, &text));
        }
    }

    /// Una edición propia (añadir o quitar canciones) que Spotify ya aceptó: se aplica al momento
    /// a la lista en memoria y se pide la fresca, que la sustituye entera al terminar y la guarda
    /// en disco. Si ya había una carga en vuelo (quizá de antes del cambio), la fresca sale detrás.
    fn apply_playlist_edit(&mut self, id: &str, uris: &[String], add: bool) {
        if !self.lists.contains_key(id) && !self.pl_busy(id) {
            // Ni a la vista ni cargándose: basta con olvidar la copia del disco, que ya no es la
            // de Spotify; se cargará entera al abrirla.
            self.forget_list(id);
            self.invalidate(&format!("pl:{id}"));
            self.invalidate(&format!("warm:{id}"));
            return;
        }
        // La copia que se esté leyendo es de antes del cambio: no se pone encima de él.
        self.warming.remove(id);
        if add {
            // Solo las que ya están en memoria (en otra lista o en Me gusta); el resto llega con
            // la recarga. Nunca en una carga a medias: sus lotes siguientes quedarían detrás.
            if self.lists.get(id).is_some_and(|l| !l.loading) {
                let me = self.my_id().map(str::to_string);
                let today = today_utc();
                let mut found: Vec<Track> = Vec::new();
                for uri in uris {
                    let Some(tid) = uri.strip_prefix("spotify:track:") else { continue };
                    let t = self.find_loaded_track(tid).or_else(|| {
                        self.lists.get(LIKED).and_then(|l| l.tracks.iter().find(|t| t.id.as_deref() == Some(tid)).cloned())
                    });
                    if let Some(mut t) = t {
                        t.added_at = Some(today.clone());
                        t.added_by = me.clone();
                        found.push(t);
                    }
                }
                if let Some(l) = self.lists.get_mut(id) {
                    l.total = l.total.saturating_add(found.len() as u32);
                    l.tracks.extend(found);
                    l.touch(&mut self.list_gen);
                }
            }
        } else {
            // Spotify quita todas las copias de cada canción; también de lo que ya llegó aparte.
            let gone: HashSet<&str> = uris.iter().map(String::as_str).collect();
            if let Some(l) = self.lists.get_mut(id) {
                let before = l.tracks.len();
                l.tracks.retain(|t| !gone.contains(t.uri.as_str()));
                l.total = l.total.saturating_sub((before - l.tracks.len()) as u32);
                l.touch(&mut self.list_gen);
            }
            if let Some(f) = self.list_fresh.get_mut(id) {
                f.retain(|t| !gone.contains(t.uri.as_str()));
            }
        }
        self.load_playlist(id, false, true);
    }

    /// Tras vaciar `lists` (cierre de sesión, recarga de la biblioteca): las marcas de «se ve la
    /// copia, lo fresco va aparte» ya no tienen copia detrás. Quedaban las de copias que nunca se
    /// recargaron (precarga, fallos) y on_warmed rechazaba por ellas la copia al volver a abrir la
    /// playlist: sin filas hasta el final de su carga. Solo siguen las de cargas en vuelo, que al
    /// terminar sustituyen la lista entera con lo que juntaron aparte.
    fn drop_list_staging(&mut self) {
        let inflight: HashSet<String> = self.list_cached.iter().filter(|id| self.pl_busy(id)).cloned().collect();
        self.list_cached.retain(|id| inflight.contains(id));
        self.list_fresh.retain(|id, _| inflight.contains(id));
    }

    /// Olvida la copia en disco (la playlist cambió: no debe verse la versión vieja).
    fn forget_list(&mut self, id: &str) {
        self.list_cached.remove(id);
        self.list_fresh.remove(id);
        // La que se esté leyendo ya no vale.
        self.warming.remove(id);
        self.list_retry.remove(id);
        self.pl_rerun.remove(id);
        self.list_disk.remove(id);
        // Detrás de un guardado aún encolado de esa playlist: borrarla antes lo dejaría escribirla
        // otra vez después.
        let path = self.lists_dir.join(format!("{id}.json"));
        self.disk.run(move || {
            let _ = std::fs::remove_file(path);
        });
    }

    pub fn login(&mut self) {
        // Un segundo clic (p. ej. en el avatar) no lanza otro inicio de sesión detrás del primero.
        if matches!(self.auth, Auth::LoggingIn) {
            return;
        }
        self.auth = Auth::LoggingIn;
        self.login_cancel_at = None;
        // Sin la biblioteca conectada, su autorización se prepara ya y la página de vuelta del
        // inicio de sesión salta a ella en la misma pestaña: un solo «Iniciar sesión con Spotify»
        // y nada que hacer en Ajustes. Una sola vez de forma automática (`library_consent_asked`).
        // Si quedó una encadenada de un intento anterior que falló al conectar (`Event::Error` no la
        // suelta), se reutiliza: sigue escuchando, y sin ella `web_busy` impediría preparar otra y
        // `LoggedIn` diría «un último paso en el navegador» sin que la pestaña hubiera saltado.
        let mut chain = if self.web_chain_login { self.web_chain.as_ref().map(|c| c.url.clone()) } else { None };
        if chain.is_none() && !self.api.web_configured() && !self.settings.library_consent_asked && !self.web_busy {
            match self.api.begin_web_chain(false) {
                Ok(c) => {
                    chain = Some(c.url.clone());
                    self.web_chain = Some(c);
                    self.web_chain_login = true;
                    self.web_busy = true;
                }
                // Sin cadena (puerto ocupado): al conectar se abre en una segunda pestaña.
                Err(e) => log::warn!("no se pudo preparar la autorización de la biblioteca: {e}"),
            }
        }
        self.backend.send(Cmd::Login { chain });
    }

    /// «Cancelar» mientras se espera al navegador: corta la espera del backend (como si se hubiera
    /// cancelado en Spotify), que contesta con `LoginAborted`. También la autorización encadenada.
    pub fn cancel_login(&mut self) {
        if !matches!(self.auth, Auth::LoggingIn) || self.login_cancelling() {
            return;
        }
        self.login_cancel_at = Some(Instant::now());
        self.cancel_login_chain();
        let ui = self.ui_tx.clone();
        let spawned = std::thread::Builder::new()
            .name("nanofy-login-cancel".into())
            .spawn(move || {
                // No había ninguna espera del navegador que cortar (el backend estaba conectando
                // con credenciales guardadas, o algo la retiene): la interfaz sale igualmente de
                // «iniciando sesión». Si la sesión llega después, `LoggedIn` manda.
                if !crate::backend::cancel_login_listener() {
                    ui.send(Msg::Backend(Event::LoginAborted { reason: "Inicio de sesión cancelado".to_string(), by_user: true }));
                }
            });
        if let Err(e) = spawned {
            log::warn!("no se pudo cancelar el inicio de sesión: {e}");
            self.login_cancel_at = None;
        }
    }

    /// «Cancelar» pulsado hace poco y aún sin respuesta del backend.
    pub fn login_cancelling(&self) -> bool {
        self.login_cancel_at.is_some_and(|t| t.elapsed() < Duration::from_secs(3))
    }

    /// «Conectar con Spotify» (aviso del inicio, Ajustes, barra lateral): abre en el navegador la
    /// autorización de la biblioteca. Nadie tiene que crear ninguna app.
    pub fn connect_library(&mut self) {
        if self.web_busy {
            return;
        }
        match self.api.begin_web_chain(true) {
            Ok(c) => {
                self.web_chain = Some(c);
                self.web_chain_login = false;
                self.web_busy = true;
                self.status("Se ha abierto el navegador: permite que Nanofy lea tu biblioteca…");
            }
            Err(e) => self.status_err(format!("No se pudo abrir la autorización de la biblioteca: {e}")),
        }
    }

    /// Suelta la autorización de la biblioteca que esperaba al navegador (si había una).
    pub fn cancel_web_chain(&mut self) {
        self.web_chain_login = false;
        if let Some(c) = self.web_chain.take() {
            c.cancel();
            self.web_busy = false;
        }
    }

    /// Suelta la autorización de la biblioteca solo si la encadenó el inicio de sesión.
    fn cancel_login_chain(&mut self) {
        if self.web_chain_login {
            self.cancel_web_chain();
        }
    }

    /// La autorización en curso terminó (llegó su resultado): ya no hay nada que cancelar.
    fn web_chain_done(&mut self) {
        self.web_busy = false;
        self.web_chain = None;
        self.web_chain_login = false;
    }

    /// Tras conectar la sesión: sigue con la autorización de la biblioteca si hace falta. `fresh`:
    /// se acaba de iniciar sesión en el navegador (ver `Event::LoggedIn`).
    fn after_login_library(&mut self, fresh: bool) {
        if self.api.web_configured() {
            return;
        }
        if !fresh {
            // Entró con credenciales guardadas (o es una reconexión): el navegador no pasó por la
            // página que encadena.
            self.cancel_login_chain();
            return;
        }
        if self.web_chain.is_some() {
            // La pestaña del inicio de sesión ya saltó (o salta) a la autorización de la biblioteca.
            // Desde aquí es una autorización como la de «Conectar con Spotify»: una reconexión
            // (`LoggedIn` sin `fresh`) no debe soltarla mientras se acepta en el navegador.
            self.web_chain_login = false;
            self.mark_library_consent_asked();
            self.status("Un último paso en el navegador: permite que Nanofy lea tu biblioteca (no tienes que crear nada).");
        } else if !self.settings.library_consent_asked && !self.web_busy {
            // No se pudo encadenar: una segunda pestaña, una sola vez, diciendo por qué se abre.
            self.mark_library_consent_asked();
            self.connect_library();
            if self.web_busy {
                self.status("Un último paso en otra pestaña del navegador: permite que Nanofy lea tu biblioteca (no tienes que crear nada).");
            }
        }
    }

    fn mark_library_consent_asked(&mut self) {
        if self.settings.library_consent_asked {
            return;
        }
        self.settings.library_consent_asked = true;
        self.draft.library_consent_asked = true;
        if !self.ephemeral {
            self.settings.save(&self.paths);
        }
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
        use crate::shell::ui_phase;
        let short = |s: String| s.chars().take(120).collect::<String>();
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Backend(e) => {
                    ui_phase("evento del reproductor", || short(format!("{e:?}")));
                    self.on_event(e)
                }
                Msg::Api(r) => {
                    ui_phase("respuesta de la API", || short(format!("{:?}", r.req)));
                    self.on_api(r)
                }
                Msg::Image { key, image, retry } => self.images.loaded(ctx, &key, image, retry),
                Msg::ImageDropped { key, wanted } => self.images.dropped(&key, &wanted),
                Msg::Warmed { id, seq, list } => {
                    ui_phase("copia de playlist leída", || id.clone());
                    self.on_warmed(id, seq, list)
                }
                Msg::Media(ev) => {
                    ui_phase("tecla multimedia", || format!("{ev:?}"));
                    self.on_media(ev)
                }
                Msg::Update { result, manual, location, staged } => self.on_update(result, manual, location, staged),
                Msg::UpdateStage { turn, stage } => self.on_update_stage(turn, stage),
                Msg::UpdateApplied(result) => self.on_update_applied(ctx, result),
                Msg::ReleaseNotes(info) => self.current_notes = Some(info),
                Msg::Control(req) => {
                    ui_phase("orden de control", || short(req.cmd.to_string()));
                    let reply = self.control_exec(ctx, &req.cmd);
                    let _ = req.reply.send(reply);
                }
            }
        }
        if self.media_dirty {
            ui_phase("controles multimedia de Windows", String::new);
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
        if matches!(
            e,
            Event::Playing { .. } | Event::Paused { .. } | Event::Stopped | Event::Unavailable | Event::LoadFailed { .. } | Event::Stalled { .. }
        ) {
            // Lo pedido ya llegó al reproductor: a partir de aquí, si la conexión cae, se retoma
            // desde donde suene, no desde donde se pidió.
            self.pending_load = None;
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
                fresh,
            } => {
                self.login_cancel_at = None;
                // Foto y nombre del perfil por el protocolo interno (no gasta cuota de la Web API).
                // Sale la primera, antes de la tanda de refresh_from_network; su clave se marca
                // después de esa (que vacía `requested`), para que Resp::Me no lo pida otra vez.
                let user_key = (!self.users.contains_key(&username)).then(|| format!("user:{username}"));
                if user_key.is_some() {
                    self.api.send(Req::User(username.clone()));
                }
                self.auth = Auth::LoggedIn { username };
                self.device_id = device_id;
                // Si la cuenta no es Premium, `NotPremium` llega justo detrás.
                self.not_premium = false;
                if self.playback_error.as_ref().is_some_and(|e| e.kind == player_bar::PlaybackErrorKind::NotPremium) {
                    self.playback_error = None;
                }
                crate::tmark("sesión: conectado");
                // Lo que casi seguro se va a restaurar se prepara ya en el reproductor, mientras
                // llega el estado de la cuenta (necesario antes de activar Connect, para no
                // quitarle la música a otro dispositivo): al decidir, suena al instante.
                if let Some(saved) = &self.restore_pending {
                    self.backend.send(Cmd::Preload(saved.now.uri.clone()));
                }
                // El aviso «Conectando…» duraba sus 8 s aunque la sesión llegase en 1 s.
                if self.status.as_ref().is_some_and(|(t, _, err)| !err && t.starts_with("Conectando")) {
                    self.status = None;
                }
                // Recién reiniciada para actualizar: la copia local la guardó la ventana anterior al
                // cerrarse hace unos segundos, así que manda sin esperar al estado de Spotify (que
                // aún diría lo de esa ventana). Solo aquí, ya conectada: antes, restaurar no puede
                // activar este dispositivo.
                let fresh_update = self.updated_from.is_some()
                    && self.restore_pending.as_ref().is_some_and(|s| crate::cache::now_secs().saturating_sub(s.saved_at) < UPDATE_RESTORE_FRESH_SECS);
                if !fresh_update && std::mem::take(&mut self.resume_after_update) {
                    // Sin esa copia reciente no se sabe qué sonaba: como en cualquier arranque,
                    // se restaura en pausa.
                    log::info!("[update] sin copia reciente de la reproducción: se restaura en pausa");
                }
                if self.restore_pending.is_some() || self.restore_wanted {
                    // La decisión (copia local, clúster de Connect, recently-played) se toma en
                    // try_decide_restore; el clúster inicial llega justo después de conectar.
                    if self.restore_deadline.is_none() {
                        self.restore_deadline = Some(Instant::now() + Duration::from_millis(2500));
                    }
                    self.try_decide_restore(fresh_update);
                }
                self.refresh_from_network(false);
                if let Some(k) = user_key {
                    self.requested.insert(k);
                }
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
                    self.backend.send(Cmd::LoadContext { uri, track_uri: None, index: Some(0), shuffle: false, resume: Some(0), first: None });
                }
                if !self.pending_downloads.is_empty() {
                    let ids = std::mem::take(&mut self.pending_downloads);
                    self.download(ids);
                }
                self.status("Conectado a Spotify");
                self.after_login_library(fresh);
            }
            Event::LoginAborted { reason, by_user } => {
                self.auth = Auth::LoggedOut;
                self.login_cancel_at = None;
                // La autorización encadenada ya no tiene por dónde llegar (la página de vuelta no
                // salta sin código).
                self.cancel_login_chain();
                if by_user {
                    self.status(reason);
                } else {
                    self.status_err(reason);
                }
            }
            Event::LoggedOut => {
                self.auth = Auth::LoggedOut;
                self.user = None;
                // Lo ya preparado era de esta cuenta (el backend también olvida sus cachés).
                self.warm.forget();
                self.playlists.clear();
                self.playlists_loaded = false;
                self.playlists_fresh = None;
                self.playlists_asked = false;
                self.playlists_source = "";
                self.playlists_web_at = 0;
                self.playlists_web_ids.clear();
                self.playlists_web_pending = false;
                self.playlist_meta.clear();
                self.lists.clear();
                self.list_disk.clear();
                self.warming.clear();
                self.drop_list_staging();
                self.pl_copy_info.clear();
                self.pl_snap.clear();
                self.page_cache.clear();
                self.filter_cache = None;
                self.saved_albums.clear();
                self.followed_artists.clear();
                self.albums.clear();
                self.artists.clear();
                self.users.clear();
                self.user_playlists.clear();
                self.following.clear();
                self.recent.clear();
                self.requested.clear();
                self.prefetch_inflight = None;
                // La precarga es de las playlists de esta cuenta: la rehace el listado de la próxima.
                self.prefetch_ids.clear();
                self.prefetch_at = None;
                self.prefetch_warm = None;
                self.list_retry.clear();
                self.clear_restore_ctx();
                // Las cargas en vuelo siguen contando hasta que respondan (si no, una de la
                // sesión siguiente podría mezclarse con sus lotes), pero no se repiten.
                self.pl_rerun.clear();
                self.meta_after_edit.clear();
                self.search_result = None;
                self.search_result_for = None;
                // La respuesta en vuelo ya no se aceptará: sin esto «Buscando» quedaría fijo.
                self.search_loading = false;
                self.search_pending = None;
                self.search_retry = None;
                self.search_retries = 0;
                self.search_cache.clear();
                self.search_refreshing.clear();
                self.search_pending_profile = None;
                self.liked_set.clear();
                // Una recarga completa a medias no debe impedir la de la próxima sesión
                // (request_liked_full no lanza otra mientras esta conste en curso).
                self.liked_refresh_pending = false;
                self.liked_reload.clear();
                self.liked_reload_ids.clear();
                // Sin lista no hay base: la próxima sesión recarga Me gusta y artistas enteros.
                self.liked_synced_at = 0;
                self.liked_server_total = 0;
                self.artists_synced_at = 0;
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
                self.reconnect_resume = None;
                self.pending_load = None;
                self.playback_error = None;
                self.load_retry = None;
                self.stall = None;
                self.stuck_load = None;
                self.output_lost = None;
                self.not_premium = false;
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
                // La de la canción anterior; la nueva llega justo detrás (`Event::AudioFormat`).
                self.player.audio = None;
                // Otra canción: el corte de red de la anterior ya no se reanuda.
                if self.stall.as_ref().is_some_and(|s| !s.is(&np.uri)) {
                    self.stall = None;
                }
                // Si el locutor del DJ la presenta, llega justo detrás (`Event::Narration`).
                self.player.dj = None;
                self.set_now_playing(np);
            }
            Event::Playing { position_ms } if self.pause_after_restore => {
                // Al abrir nunca suena solo: la sesión restaurada se deja en pausa.
                self.player.state = PlayState::Playing;
                self.player.position_ms = position_ms;
                self.playback_recovered();
                self.play_pause();
            }
            Event::Playing { position_ms } if self.pause_on_play => {
                self.pause_on_play = false;
                self.player.state = PlayState::Playing;
                self.player.position_ms = position_ms;
                self.playback_recovered();
                self.play_pause();
                self.status("Temporizador: reproducción pausada al terminar la canción");
            }
            Event::Playing { position_ms } => {
                self.player.remote = None;
                self.player.state = PlayState::Playing;
                self.player.position_ms = position_ms;
                // Mientras habla el locutor del DJ, la canción no avanza.
                self.player.position_at = self.player.dj.is_none().then(Instant::now);
                self.media_dirty = true;
                self.playback_recovered();
            }
            Event::Paused { position_ms } => {
                // Si esta pausa es por falta de salida (`NoAudioOutput` llega justo detrás), ¿iba a
                // sonar? Entonces se reanuda sola al volver un dispositivo. Nunca lo restaurado al
                // abrir, que no suena sin pedirlo.
                self.paused_while_playing =
                    matches!(self.player.state, PlayState::Playing | PlayState::Loading) && !self.pause_after_restore;
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
                self.player.dj = None;
                self.media_dirty = true;
                // Parada (fin de la lista, otro dispositivo): nada que reanudar.
                self.stall = None;
            }
            Event::Loading => self.player.state = PlayState::Loading,
            Event::Stalled { uri, position_ms } => self.on_stalled(uri, position_ms),
            Event::NoAudioOutput => {
                let uri = self.player.now.as_ref().map(|n| n.uri.clone());
                let resume = self.paused_while_playing || self.output_lost.as_ref().is_some_and(|(_, r)| *r);
                log::warn!("[reproducción] no hay salida de audio{}", if resume { "; se reanudará al volver" } else { "" });
                self.output_lost = Some((uri, resume));
                self.playback_error = Some(player_bar::PlaybackError::new(
                    player_bar::PlaybackErrorKind::NoOutput,
                    player_bar::NO_OUTPUT_TEXT.to_string(),
                ));
            }
            Event::AudioOutputBack => {
                let mut keep_banner = false;
                if let Some((uri, resume)) = self.output_lost.take() {
                    let same = uri.is_some() && self.player.now.as_ref().map(|n| n.uri.clone()) == uri;
                    let wanted = resume && same && self.player.state == PlayState::Paused && self.player.remote.is_none();
                    if wanted && !self.output_resumes.allow(Instant::now()) {
                        // El dispositivo está pero no se deja abrir: sin insistir más (ver
                        // `watchdog::OutputResumes`); el aviso se queda y reproducir lo reintenta.
                        log::warn!("[reproducción] vuelve la salida pero no suena; no se reanuda sola otra vez");
                        keep_banner = true;
                        self.output_lost = Some((uri, false));
                    } else if wanted {
                        log::info!("[reproducción] vuelve la salida de audio: se reanuda");
                        self.load_watch = None;
                        self.backend.send(Cmd::Play);
                        self.player.state = PlayState::Loading;
                        self.media_dirty = true;
                    }
                }
                if !keep_banner && self.playback_error.as_ref().is_some_and(|e| e.kind == player_bar::PlaybackErrorKind::NoOutput) {
                    self.playback_error = None;
                }
            }
            Event::NotPremium => {
                if matches!(self.auth, Auth::LoggingIn | Auth::Connecting { .. }) {
                    self.auth = Auth::LoggedOut;
                }
                self.not_premium = true;
                self.pending_load = None;
                self.load_retry = None;
                self.stall = None;
                self.stuck_load = None;
                if self.player.state == PlayState::Loading && self.player.remote.is_none() {
                    self.player.state = PlayState::Stopped;
                    self.media_dirty = true;
                }
                self.playback_error = Some(player_bar::PlaybackError::new(
                    player_bar::PlaybackErrorKind::NotPremium,
                    player_bar::NOT_PREMIUM_TEXT.to_string(),
                ));
            }
            Event::Unavailable => {
                // El aviso con el motivo ya lo puso `LoadFailed`, que llega justo antes (antes se
                // decía siempre «¿cuenta sin Premium?», y casi nunca era eso).
                // Si no hay nada más que reproducir, librespot no manda «parado»: el botón se
                // quedaría en «cargando». Si sigue con otra pista, el evento Playing lo corrige.
                if self.player.state == PlayState::Loading {
                    self.player.state = PlayState::Stopped;
                    self.player.position_at = None;
                    self.media_dirty = true;
                }
            }
            Event::LoadFailed { uri, reason, transient, play } => self.on_load_failed(uri, reason, transient, play),
            Event::SkipCascade { failed } => {
                log::warn!("[reproducción] {failed} canciones seguidas no se pudieron reproducir: se detuvo");
                self.load_retry = None;
                self.playback_error = Some(player_bar::PlaybackError::new(
                    player_bar::PlaybackErrorKind::Cascade,
                    player_bar::CASCADE_TEXT.to_string(),
                ));
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
            Event::Reconnecting => {
                // El audio se corta aquí: la barra se detiene en el punto que se retomará.
                let pos = self.player.position();
                self.reconnect_resume = match self.player.state {
                    _ if self.player.remote.is_some() || self.jam.is_some() || self.now_placeholder => None,
                    _ if self.player.now.is_none() => None,
                    // Parada tras agotar los reintentos de una carga fallida: Spirc la tenía en
                    // pausa para [Reintentar], pero el de la conexión nueva no tendrá nada. Se
                    // retoma en pausa para que [Reintentar] o reproducir sigan sirviendo.
                    PlayState::Stopped if self.failed_load_wanted_play() => Some(ResumePoint { pos, playing: false }),
                    PlayState::Stopped => None,
                    // En pausa a la espera del reintento automático de una carga fallida: iba a
                    // sonar, y la reconexión es el reintento (si se retomara en pausa y volviera a
                    // fallar, el aviso diría «reintentando…» sin reintento pendiente).
                    // Lo mismo con una canción cortada por la red, en pausa a la espera de reanudar.
                    s => Some(ResumePoint { pos, playing: s != PlayState::Paused || self.load_retry.is_some() || self.stall.is_some() }),
                };
                if self.player.remote.is_none() {
                    self.player.position_ms = pos;
                    self.player.position_at = None;
                    if self.player.state == PlayState::Playing {
                        self.player.state = PlayState::Loading;
                    }
                }
                self.media_dirty = true;
            }
            Event::Reconnected => {
                self.status("Conexión con Spotify restablecida");
                // El vigilante de «cargando» sigue contando: una reconexión no le da otros 30 s.
                // Las miniaturas de artistas que fallaron con la sesión caída siguen marcadas como
                // pedidas (para no repetirlas en cada fotograma): se pueden volver a pedir. Las
                // que ya tienen imagen no se piden; el resto, en un lote por tarjeta a la vista.
                self.requested.retain(|k| !k.starts_with("artistmeta:"));
                let resume = self.reconnect_resume.take();
                if let Some(cmd) = self.pending_load.clone() {
                    // Lo último que se pidió no llegó a sonar: se pide otra vez.
                    log::info!("[reconexión] se repite la carga pendiente");
                    self.backend.send(cmd);
                    self.player.state = PlayState::Loading;
                } else if let Some(point) = resume {
                    self.resume_after_reconnect(point);
                } else if self.player.state == PlayState::Loading {
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
            Event::AudioFormat(info) => self.player.audio = Some(info),
            Event::Crossfade { from, to, ms } => {
                // La barra ya enseña la nueva (llegaron `TrackChanged` y `Playing`); esto solo se
                // cuenta para el modo de control.
                self.player.transitions = self.player.transitions.saturating_add(1);
                self.player.last_transition = Some(LastTransition { from, to, ms, at: Instant::now() });
            }
            Event::Narration(speaking) => {
                let ended = speaking.is_none() && self.player.dj.is_some();
                if let Some(n) = &speaking {
                    log::info!("[dj] habla {} ({})", n.artist, n.title);
                }
                self.player.dj = speaking;
                if self.player.dj.is_some() {
                    self.player.position_at = None;
                } else if ended && self.player.state == PlayState::Playing {
                    // Termina de hablar: la canción empieza (o, tras la de salida, sigue la otra).
                    self.player.position_at = Some(Instant::now());
                }
                self.media_dirty = true;
            }
        }
    }

    /// Cambio de pista (local o remota): corazón, letras, cola y metadatos del sistema.
    fn set_now_playing(&mut self, np: NowPlaying) {
        // Si la barra ya la mostraba por adelantado (al hacer clic), la confirmación se procesa
        // entera igualmente: historial, «me gusta», letras…
        if !std::mem::take(&mut self.now_optimistic) && self.player.now.as_ref() == Some(&np) {
            return;
        }
        self.player.liked = np.id.as_ref().map(|id| self.liked_set.contains(id));
        // Canción oculta: se salta (una vez por pista, para no entrar en bucle). Es un salto que
        // no pidió el usuario: si la oculta entraba fundiéndose, la anterior sigue apagándose a
        // su ritmo en vez de cortarse en seco (con `Cmd::Next` normal perdería lo que le quedaba).
        if let Some(id) = &np.id {
            if self.hidden_tracks.contains(id) && self.player.remote.is_none() && self.last_skipped.as_deref() != Some(id.as_str()) {
                self.last_skipped = Some(id.clone());
                self.backend.send(Cmd::Next { auto: true });
            }
        }
        if self.queued_local.first() == Some(&np.uri) {
            self.queued_local.remove(0);
        }
        if self.sleep_end_of_track {
            // Cumplido: el fundido elegido vuelve (estaba suspendido mientras tanto).
            self.set_sleep_end_of_track(false);
            self.pause_on_play = true;
        }
        let played = track_from_now(&np);
        // Sin id de álbum (librespot no lo da): se pide para «Ver álbum» y el menú de la portada.
        if np.album_id.is_none() {
            if let Some(id) = np.id.clone() {
                self.request_once(&format!("trackinfo:{id}"), Req::TrackInfo(id));
            }
        }
        std::sync::Arc::make_mut(&mut self.play_log).record(played.clone());
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
        // Ya la tenemos (pedida antes, por adelantado o guardada en disco): al instante.
        if let Some(l) = self.lyrics_cache.get(&id).cloned().or_else(|| self.lyrics_from_disk(&id).map(Some)) {
            self.lyrics_cache.insert(id, l.clone());
            self.lyrics = l;
            self.lyrics_loading = false;
            return;
        }
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

    /// Dónde se guarda la letra de una canción.
    fn lyrics_file(&self, id: &str) -> PathBuf {
        self.paths.cache_dir.join("lyrics").join(format!("{id}.json"))
    }

    /// La letra guardada de una canción, si tiene menos de 30 días.
    fn lyrics_from_disk(&self, id: &str) -> Option<Lyrics> {
        let path = self.lyrics_file(id);
        let age = std::fs::metadata(&path).ok()?.modified().ok()?.elapsed().unwrap_or_default();
        if age > Duration::from_secs(30 * 24 * 3600) {
            return None;
        }
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
    }

    /// Guarda la letra (en segundo plano: no frena el fotograma).
    fn lyrics_to_disk(&self, l: &Lyrics) {
        let path = self.lyrics_file(&l.track_id);
        let Ok(json) = serde_json::to_string(l) else { return };
        std::thread::spawn(move || {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(path, json);
        });
    }

    /// Con el panel de la letra abierto y la de la que suena ya puesta: la de la canción
    /// siguiente, por adelantado (la siguiente del estado de Spotify), para que al cambiar
    /// aparezca al instante. Una vez por canción.
    pub(super) fn prefetch_next_lyrics(&mut self) {
        if self.lyrics_loading || self.lyrics.is_none() {
            return;
        }
        let now_uri = self.player.now.as_ref().map(|n| n.uri.clone()).unwrap_or_default();
        let Some(Some(info)) = &self.server_cluster else { return };
        let Some(next) = info.next.iter().find(|u| u.starts_with("spotify:track:") && **u != now_uri) else { return };
        let id = next.trim_start_matches("spotify:track:").to_string();
        if self.lyrics_cache.contains_key(&id) || self.lyrics_prefetched.contains(&id) {
            return;
        }
        self.lyrics_prefetched.insert(id.clone());
        if let Some(l) = self.lyrics_from_disk(&id) {
            self.lyrics_cache.insert(id, Some(l));
            return;
        }
        self.api.send(Req::LyricsPrefetch { id });
    }

    fn on_api(&mut self, r: ApiResult) {
        // Carga de playlist: con su último lote o su error deja de estar en vuelo; cada lote
        // intermedio cuenta como noticia (una de miles de pistas puede tardar en total más que
        // PL_INFLIGHT_STALE).
        let pl_end = match (&r.req, &r.result) {
            (Req::PlaylistTracks(id), Err(_) | Ok(Resp::Tracks { done: true, .. })) => {
                self.pl_inflight.remove(id);
                Some(id.clone())
            }
            (Req::PlaylistTracks(id), Ok(_)) => {
                if let Some(t) = self.pl_inflight.get_mut(id) {
                    *t = Instant::now();
                }
                None
            }
            _ => None,
        };
        // Llegó un lote fresco (aunque sea el último y vacío): la copia en disco que se esté
        // leyendo ya no se pone. Ver `on_warmed`.
        if let (Req::PlaylistTracks(id), Ok(Resp::Tracks { .. })) = (&r.req, &r.result) {
            self.warming.remove(id);
        }
        let gone = matches!(&r.result, Err(e) if e.contains("404"));
        // Falló lo que la restauración de la sesión espera para armar la cola: se decide antes de
        // repartir el error, para que, si se deja, sus brazos lo traten como uno cualquiera.
        if let Err(e) = &r.result {
            let restoring = match &r.req {
                Req::PlaylistTracks(id) => self.restoring_playlist(id),
                Req::Album(id) => self.restore_ctx.as_ref().is_some_and(|(k, kind, _)| *kind == CtxKind::Album && k == id),
                _ => false,
            };
            if restoring {
                self.restore_ctx_failed(e);
            }
        }
        self.dispatch_api(r);
        // La repetición pedida mientras corría sale después de repartir esta respuesta: antes,
        // su `list_cached` desviaría el último lote de esta y lo guardaría como la lista entera.
        // Si la playlist ya no existe, no se repite.
        if let Some(id) = pl_end {
            if self.pl_rerun.remove(&id) && !gone {
                log::info!("playlist {id}: cambió mientras se cargaba; se carga otra vez");
                self.load_playlist(&id, false, false);
            }
        }
    }

    fn dispatch_api(&mut self, r: ApiResult) {
        // La precarga en vuelo terminó (completa o con error): deja paso a la siguiente. Se mira
        // aquí, antes de repartir, porque varios brazos de error y de pistas salen con `return`.
        let prefetch_done = matches!(&r.req, Req::PlaylistTracks(id)
            if self.prefetch_inflight.as_ref().is_some_and(|(p, _)| p == id))
            && matches!(r.result, Err(_) | Ok(Resp::Tracks { done: true, .. }));
        if prefetch_done {
            self.prefetch_inflight = None;
        } else if let (Req::PlaylistTracks(id), Some((p, t))) = (&r.req, self.prefetch_inflight.as_mut()) {
            // Un lote intermedio: sigue avanzando, así que no cuenta como perdida. Una playlist de
            // miles de pistas con el limitador frenando puede tardar más de PREFETCH_STALE en total.
            if p == id {
                *t = Instant::now();
            }
        }
        let resp = match r.result {
            Ok(resp) => resp,
            Err(e) => {
                match r.req {
                    Req::PlayerState | Req::Devices | Req::Queue => {
                        log::warn!("{e}")
                    }
                    // «Ver álbum» de una canción sin su álbum: que no se quede en «Buscando».
                    Req::TrackInfo(ref id) if self.album_of_pending.as_deref() == Some(id.as_str()) => {
                        self.album_of_pending = None;
                        log::warn!("álbum de {id}: {e}");
                        self.status_err("No se pudo encontrar el álbum de esta canción");
                    }
                    // Se ve la copia del disco (o una carga a medias) y la fresca no llegó (sin
                    // red, límite de ritmo): se mantiene y se reintenta con espera creciente. Antes
                    // se desmarcaba sin más y la página abierta la pedía otra vez al fotograma
                    // siguiente, en bucle mientras durara el fallo. Si la playlist ya no existe
                    // (una radio caducada), se olvidan la copia y la radio.
                    Req::PlaylistTracks(ref id) if self.list_cached.contains(id) => {
                        self.list_fresh.remove(id);
                        // Lo que se ve puede ser una carga a medias de una anterior que se perdió
                        // (load_playlist la deja a la vista): que no quede en «Cargando».
                        if let Some(l) = self.lists.get_mut(id) {
                            l.loading = false;
                        }
                        if e.contains("404") {
                            self.requested.remove(&format!("pl:{id}"));
                            self.forget_list(id);
                            self.forget_radio(id);
                        } else if self.pl_rerun.contains(id) {
                            // Hay otra carga pedida (una edición, una recarga forzada) que sale en
                            // cuanto se reparta este error: esa hace de reintento.
                        } else {
                            self.schedule_list_retry(id, &e);
                            // Sin reintento programado (contexto en restauración, que la repite
                            // por su cuenta) sigue marcada, por lo mismo.
                            if self.list_retry.contains_key(id.as_str()) {
                                self.requested.remove(&format!("pl:{id}"));
                            }
                        }
                        log::info!("playlist {id}: {e}; se mantiene la copia guardada");
                    }
                    // Precarga de playlist fallida (p. ej. límite de ritmo): se desmarca para que
                    // se recargue al abrirla, en vez de quedar bloqueada y salir vacía.
                    Req::PlaylistTracks(ref id) if !self.lists.contains_key(id) => {
                        if e.contains("404") {
                            self.forget_radio(id);
                        }
                        self.requested.remove(&format!("pl:{id}"));
                        log::info!("precarga de playlist {id} falló ({e}); se reintentará al abrirla");
                    }
                    // Falló a medias con parte de la lista a la vista (sesión caída, límite de
                    // librespot): antes se quedaba en «Cargando X de Y» (repintando a 4 Hz) toda
                    // la sesión. Se conserva lo que llegó y se reintenta sola con espera; la
                    // carga nueva se junta aparte y sustituye entera a la parcial.
                    // También la que la página abrió vacía y falló en el primer lote (aún sin
                    // total): si no, quedaba vacía y marcada como pedida toda la sesión.
                    Req::PlaylistTracks(ref id)
                        if self
                            .lists
                            .get(id)
                            .is_some_and(|l| l.loading || l.tracks.is_empty() || l.tracks.len() < l.total as usize) =>
                    {
                        let short = match self.lists.get_mut(id) {
                            Some(l) => {
                                l.loading = false;
                                l.tracks.is_empty() || l.tracks.len() < l.total as usize
                            }
                            None => false,
                        };
                        self.list_fresh.remove(id);
                        if e.contains("404") {
                            // Ya no existe (una radio caducada): ni copia ni reintentos, y sigue
                            // marcada como pedida para que la página no la repita.
                            self.forget_list(id);
                            self.forget_radio(id);
                            self.status_err(e);
                        } else if self.pl_rerun.contains(id) {
                            // Otra carga pedida sale en cuanto se reparta este error y sustituye
                            // entera a lo que llegó: hace de reintento.
                            log::info!("playlist {id}: {e}; se repite ya");
                        } else if short {
                            // Sin nada a la vista (no hay copia ni llegó ningún lote), un aviso la
                            // primera vez: la página vacía no dice por qué. Los reintentos, callados.
                            let nothing = self.lists.get(id).is_none_or(|l| l.tracks.is_empty());
                            if nothing && !self.list_retry.contains_key(id.as_str()) {
                                self.status_err(format!("No se pudo cargar la playlist ({e}); se reintentará sola"));
                            }
                            self.schedule_list_retry(id, &e);
                            // Sin reintento programado (es el contexto en restauración, que la
                            // repite por su cuenta) sigue marcada: si no, la página abierta la
                            // pediría otra vez en el fotograma siguiente, en bucle.
                            if self.list_retry.contains_key(id.as_str()) {
                                self.requested.remove(&format!("pl:{id}"));
                            }
                        } else {
                            log::info!("playlist {id}: {e}; se mantiene lo que llegó");
                        }
                    }
                    // La página ya tiene nombre y portada por la carga de pistas: los de la Web API
                    // solo completaban (privacidad, seguidores), así que no merece un aviso. Sigue
                    // marcada como pedida: la página la pide en cada fotograma y, con Spotify
                    // limitando, fallaría al momento en bucle gastando el ritmo de la búsqueda.
                    // Se vuelve a pedir al reconectar o al editar la playlist.
                    // Tras editar nombre, descripción o portada sí hacen falta: sin ellos se vería
                    // la versión de antes. Salen de la playlist4 recargando sus pistas (sin vaciar
                    // lo que se ve); sin la lista en memoria, de la carga al abrirla.
                    Req::PlaylistMeta(ref id) if self.meta_after_edit.contains(id) => {
                        self.meta_after_edit.remove(id);
                        log::info!("metadatos de la playlist {id} tras editarla: {e}; se toman de la playlist4");
                        if self.lists.contains_key(id) {
                            self.load_playlist(id, false, true);
                        } else {
                            self.invalidate(&format!("pl:{id}"));
                        }
                    }
                    Req::PlaylistMeta(ref id) => log::info!("metadatos de la playlist {id} por la Web API: {e}"),
                    Req::FollowContains { .. } => log::warn!("{e}"),
                    // Miniaturas de las tarjetas: sin aviso, la tarjeta deja la inicial. Siguen
                    // marcadas como pedidas (si no, se pedirían en cada fotograma); se desmarcan
                    // al reconectar (Event::Reconnected) o al iniciar sesión.
                    Req::ArtistThumbs(_) => log::info!("miniaturas de artistas: {e}"),
                    // Precarga de los metadatos de una búsqueda: era por adelantado, sin aviso.
                    Req::WarmMeta(_) => log::debug!("[precarga] metadatos de la búsqueda: {e}"),
                    // El perfil propio que se pide al conectar deja su clave marcada (para que
                    // Resp::Me no lo repita): si falla, se desmarca una vez, para que Resp::Me o
                    // la página del perfil lo vuelvan a pedir en vez de quedar sin avatar ni
                    // perfil toda la sesión. Solo una: fallando en bucle se pediría sin parar.
                    Req::User(ref id)
                        if matches!(&self.auth, Auth::LoggedIn { username } if username == id)
                            && !self.requested.contains(&format!("userretry:{id}")) =>
                    {
                        self.requested.insert(format!("userretry:{id}"));
                        self.requested.remove(&format!("user:{id}"));
                        self.status_err(e);
                    }
                    // Refresco de un resultado de la caché que ya se ve: sigue a la vista, sin
                    // aviso ni reintento; se volverá a probar la próxima vez que lo busque.
                    Req::Search(ref q) if self.search_refreshing.contains(q.as_str()) => {
                        self.search_refreshing.remove(q.as_str());
                        if self.search_pending.as_deref() == Some(q.as_str()) {
                            self.search_loading = false;
                            self.search_pending = None;
                        }
                        log::info!("refresco de la búsqueda «{q}» falló ({e}); queda lo guardado");
                    }
                    // Solo el error de la consulta enviada la última quita «Buscando» y avisa; el
                    // de una anterior (ya sustituida) no debe tapar la que sigue en vuelo.
                    Req::Search(ref q) if self.search_pending.as_deref() == Some(q.as_str()) => {
                        match crate::api::retry_secs(&e) {
                            // Límite corto de Spotify (429): la búsqueda ya no duerme un hilo; se
                            // repite sola cuando pasa la espera (hasta 2 veces) y «Buscando»
                            // sigue a la vista en vez de un error que obligue a pulsar Intro.
                            Some(n) if n <= 30 && self.search_retries < 2 => {
                                log::info!("búsqueda «{q}» limitada por Spotify; se reintenta en {n} s");
                                self.search_retry = Some((Instant::now() + Duration::from_secs(n), q.clone()));
                                self.status(format!("Spotify limita; reintentando en {n} s"));
                            }
                            _ => {
                                self.search_loading = false;
                                self.search_pending = None;
                                self.status_err(e);
                            }
                        }
                    }
                    Req::Search(ref q) => log::info!("búsqueda antigua «{q}» falló ({e}); se ignora"),
                    Req::Playlists => {
                        self.playlists_loaded = true;
                        self.playlists_asked = false;
                        // Ni rootlist ni Web API. Con la copia a la vista, un límite pasajero de la
                        // cuota compartida no merece aviso: se ve la biblioteca de siempre.
                        if !self.playlists.is_empty() && crate::api::retry_secs(&e).is_some() {
                            log::info!("listado de playlists: {e}; se mantiene la copia");
                        } else {
                            self.status_err(e);
                        }
                    }
                    // Completar el listado del rootlist era opcional: se queda como estaba y se
                    // vuelve a intentar con el próximo listado.
                    Req::PlaylistsWeb => {
                        self.playlists_web_pending = false;
                        log::info!("[biblioteca] la Web API no completó el listado: {e}");
                    }
                    Req::Liked if self.liked_refresh_pending => {
                        // La recarga completa falló: se conserva la lista cacheada.
                        self.liked_refresh_pending = false;
                        self.liked_reload.clear();
                        self.liked_reload_ids.clear();
                        let have_list = self.lists.get(LIKED).is_some_and(|l| !l.tracks.is_empty());
                        if let Some(l) = self.lists.get_mut(LIKED) {
                            l.loading = false;
                        }
                        if have_list {
                            // Con la copia a la vista no se vuelve a pedir en esta sesión: al abrir
                            // la página saldría al momento y, con Spotify limitando, fallaría en
                            // bucle. La fecha de sincronización no cambia: el próximo arranque (o
                            // reconexión) lo reintenta en segundo plano.
                            log::info!("recarga completa de Me gusta fallida ({e}); se mantiene la copia");
                            // Un límite pasajero en una recarga de fondo no merece aviso: ella
                            // sigue viendo su lista y no ha pedido nada.
                            if crate::api::retry_secs(&e).is_none() {
                                self.status_err(e);
                            }
                        } else {
                            self.requested.remove(LIKED);
                            self.status_err(e);
                        }
                    }
                    // Igual con los artistas seguidos de la instantánea (carril de fondo).
                    Req::FollowedArtists if self.artists_loaded && crate::api::retry_secs(&e).is_some() => {
                        log::info!("artistas seguidos: {e}; se mantiene la copia");
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
                        self.web_chain_done();
                        // Caducada sin respuesta: vuelve el «Conectar con Spotify», sin más aviso.
                        if e == crate::webauth::CONNECT_EXPIRED {
                            log::info!("autorización de la biblioteca: {e}");
                        } else {
                            self.status_err(format!("No se pudo conectar la biblioteca: {e}"));
                        }
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
                // Perfil completo (foto) por el protocolo interno, para el avatar de la barra
                // superior. Con la misma clave que al conectar: si ya está en vuelo, no se repite.
                if !self.users.contains_key(&u.id) {
                    self.request_once(&format!("user:{}", u.id), Req::User(u.id.clone()));
                }
                // Las propias que llegaron del rootlist antes que el perfil: «De <tu nombre>».
                if let Some(name) = u.display_name.as_ref().filter(|n| !n.is_empty()) {
                    for p in self.playlists.iter_mut().filter(|p| p.owner.display_name.is_none() && p.owner.id.as_deref() == Some(u.id.as_str())) {
                        p.owner.display_name = Some(name.clone());
                        self.snapshot_dirty = true;
                    }
                }
                self.user = Some(u)
            }
            Resp::Playlists { list, rootlist } => {
                // Del rootlist: lo que no trae (nombre visible del propietario, privacidad, la
                // portada de las que no tienen una subida) sale del listado anterior.
                let p = if rootlist {
                    let me = self.my_id().map(str::to_string);
                    let my_name = self.my_display_name();
                    // Las recién seguidas no están en el listado anterior, pero su página ya
                    // trajo propietario, privacidad y portada.
                    let mut prev = self.playlists.clone();
                    let known: HashSet<&str> = self.playlists.iter().map(|p| p.id.as_str()).collect();
                    prev.extend(list.iter().filter(|p| !known.contains(p.id.as_str())).filter_map(|p| self.playlist_meta.get(&p.id).cloned()));
                    merge_rootlist_listing(&prev, list, me.as_deref(), my_name.as_deref())
                } else {
                    list
                };
                if self.diag {
                    log::info!("[diag] playlists: {} ({})", p.len(), if rootlist { "rootlist" } else { "web" });
                    if let Some(first) = p.first() {
                        self.load_playlist(&first.id, false, false);
                        self.api.send(Req::PlaylistMeta(first.id.clone()));
                    }
                }
                self.playlists = p;
                self.playlists_loaded = true;
                self.playlists_fresh = Some(Instant::now());
                self.playlists_asked = false;
                self.playlists_source = if rootlist { "rootlist" } else { "web" };
                if !rootlist {
                    // El de la Web API ya viene completo: cuenta como completado ahora.
                    self.playlists_web_at = crate::cache::now_secs();
                    self.playlists_web_ids = self.playlists.iter().map(|p| p.id.clone()).collect();
                }
                // Sin portada todavía: el mosaico de su lista, si ya está en memoria.
                let bare: Vec<String> =
                    self.playlists.iter().filter(|p| p.own_cover() && self.lists.contains_key(&p.id)).map(|p| p.id.clone()).collect();
                for id in bare {
                    self.fill_list_cover(&id);
                }
                if rootlist {
                    self.enrich_listing_if_needed();
                }
                // Precarga en segundo plano: primero las fijadas, luego las propias, de la copia en
                // disco más reciente (las que más abre) a las que no tienen. Así cambiar entre
                // playlists es instantáneo (ya están en memoria al abrirlas).
                let pinned = self.settings.pinned.clone();
                let mut order: Vec<String> = Vec::new();
                for id in &pinned {
                    if self.playlists.iter().any(|p| &p.id == id) && !order.contains(id) {
                        order.push(id.clone());
                    }
                }
                let ages = self.list_copy_ages();
                let mut owned: Vec<(String, Duration)> = self
                    .playlists
                    .iter()
                    .filter(|p| self.is_mine(p))
                    .map(|p| (p.id.clone(), ages.get(&p.id).copied().unwrap_or(Duration::MAX)))
                    .collect();
                owned.sort_by_key(|(_, age)| *age);
                for (id, _) in owned {
                    if !order.contains(&id) {
                        order.push(id);
                    }
                }
                self.prefetch_ids = order.into_iter().take(PREFETCH_MAX).collect();
                // No antes de 8 s desde el arranque: la ráfaga inicial (inicio, Me gusta, la sesión
                // a restaurar) va primero y la precarga no compite con ella.
                let launch = crate::START.get().copied().unwrap_or_else(Instant::now);
                self.prefetch_at = Some((Instant::now() + Duration::from_secs(2)).max(launch + Duration::from_secs(8)));
                if let Some(id) = self.pending_editor.take() {
                    if let Some(pl) = self.playlists.iter().find(|p| p.id == id).cloned() {
                        self.actions.push(Action::OpenEditor(Some(pl)));
                    }
                }
                self.snapshot_dirty = true;
            }
            Resp::PlaylistsWeb(web) => {
                self.playlists_web_pending = false;
                let total = web.len();
                let n = enrich_listing(&mut self.playlists, web);
                log::info!("[biblioteca] la Web API completó {n} de {} playlists ({total} en su listado)", self.playlists.len());
                self.playlists_web_at = crate::cache::now_secs();
                self.playlists_web_ids = self.playlists.iter().map(|p| p.id.clone()).collect();
                self.snapshot_dirty = true;
            }
            Resp::PlaylistMeta(mut p) => {
                self.meta_after_edit.remove(&p.id);
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
                self.store_playlist_meta(p);
            }
            Resp::PlaylistMetaPartial(p) => {
                if matches!(r.req, Req::PlaylistMeta(_)) {
                    self.meta_after_edit.remove(&p.id);
                }
                // De playlist4 (llega con cada carga de pistas, también de la precarga y la
                // restauración): manda en nombre, portada, descripción, tamaño y si es
                // colaborativa. Propietario, privacidad y seguidores no los trae: se conservan
                // los de la Web API o la biblioteca. Si no, sus propias playlists perderían
                // «Tu playlist», el menú de edición y la privacidad (la página prefiere esta
                // entrada a la de la biblioteca).
                let base = self
                    .playlist_meta
                    .get(&p.id)
                    .or_else(|| self.playlists.iter().find(|x| x.id == p.id))
                    .cloned();
                let merged = match base {
                    Some(mut m) => {
                        if !p.name.is_empty() {
                            m.name = p.name;
                        }
                        if p.images.as_ref().is_some_and(|v| !v.is_empty()) {
                            m.images = p.images;
                        }
                        // En las de usuarios playlist4 es la fuente de la descripción: vacía es que
                        // la borró. Conservar la anterior (de una copia en disco o de la biblioteca)
                        // la dejaría en el editor y guardar otro cambio la volvería a poner. En las
                        // de Spotify sí se conserva la que trajo el feed si playlist4 no trae.
                        if p.description.is_some() || !p.id.starts_with("37i9dQZ") {
                            m.description = p.description;
                        }
                        m.tracks = p.tracks.or(m.tracks);
                        m.collaborative = p.collaborative.or(m.collaborative);
                        m.owner.display_name = m.owner.display_name.or(p.owner.display_name);
                        m.owner.id = m.owner.id.or(p.owner.id);
                        m.public = m.public.or(p.public);
                        m.followers = m.followers.or(p.followers);
                        m
                    }
                    None => p,
                };
                self.store_playlist_meta(merged);
            }
            // Llega justo antes del último lote; save_list lo guarda con la copia.
            Resp::PlaylistCopyInfo { id, meta_at, country } => {
                self.pl_copy_info.insert(id, CopyInfo { meta_at, country: Some(country) });
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
                    // Fusiona lo guardado recientemente con la lista de la instantánea. Entran
                    // también las que se marcaron aquí sin tener la fila a mano (set_liked solo
                    // sabía el id): ahora que la recarga completa es semanal, si no, tardarían días.
                    let list = self.lists.entry(LIKED.to_string()).or_default();
                    let mut in_list: HashSet<String> = list.tracks.iter().filter_map(|t| t.id.clone()).collect();
                    let mut fresh: Vec<Track> = Vec::new();
                    // Solo las de otro dispositivo cuentan: las de aquí ya sumaron en set_liked.
                    let mut elsewhere: u64 = 0;
                    for t in tracks {
                        let Some(id) = t.id.clone() else { continue };
                        if !in_list.insert(id.clone()) {
                            continue;
                        }
                        if self.liked_set.insert(id) {
                            elsewhere += 1;
                        }
                        fresh.push(t);
                    }
                    if !fresh.is_empty() {
                        fresh.extend(std::mem::take(&mut list.tracks));
                        list.tracks = fresh;
                        list.touch(&mut self.list_gen);
                    }
                    // La cuenta de Spotify tal cual (sin max): max() escondía lo quitado fuera.
                    list.total = total;
                    list.loading = false;
                    // Si Spotify no dice lo esperado (la base más lo nuevo de fuera), se quitó algo
                    // en otro dispositivo o hay más nuevas de las que caben en dos páginas: toca
                    // reconciliar entera, en segundo plano. La base suma lo nuevo y no se iguala a
                    // la cuenta: así el descuadre sigue a la vista (y se reintenta) hasta que una
                    // recarga completa termine.
                    let expected = self.liked_server_total + elsewhere;
                    self.liked_server_total = expected;
                    if total as u64 != expected {
                        self.request_liked_full(&format!("Spotify dice {total} y se esperaban {expected}"));
                    }
                    if let Some(id) = self.player.now.as_ref().and_then(|n| n.id.clone()) {
                        self.player.liked = Some(self.liked_set.contains(&id));
                    }
                    self.snapshot_dirty = true;
                    // Lo guardado en otro dispositivo puede ser justo la pista restaurada.
                    self.restore_ctx_arrived(LIKED);
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
                        list.touch(&mut self.list_gen);
                        list.total = total.max(list.tracks.len() as u32);
                        list.loading = false;
                        // La cuenta cruda de Spotify, no la de filas: las no disponibles, las sin
                        // uri y el tope de páginas la dejan por encima, y comparar con las filas
                        // pedía otra recarga completa en cada arranque.
                        self.liked_server_total = total as u64;
                        self.liked_synced_at = crate::cache::now_secs();
                        if let Some(id) = self.player.now.as_ref().and_then(|n| n.id.clone()) {
                            self.player.liked = Some(self.liked_set.contains(&id));
                        }
                        self.snapshot_dirty = true;
                        // La recarga completa (sin instantánea, la única vía) puede traer la pista
                        // de una sesión restaurada desde Me gusta: se arma ya su cola.
                        self.restore_ctx_arrived(LIKED);
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
                let mut keep_shown = false;
                if self.list_cached.contains(&key) {
                    // Se está viendo la copia del disco: la fresca se junta aparte y la sustituye
                    // entera al final (sin encoger a 100 filas mientras llega el resto).
                    let fresh = self.list_fresh.entry(key.clone()).or_default();
                    fresh.extend(tracks);
                    if !done {
                        return;
                    }
                    tracks = self.list_fresh.remove(&key).unwrap_or_default();
                    self.list_cached.remove(&key);
                    if let Some(l) = self.lists.get_mut(&key) {
                        l.loading = false;
                        // Llegó con huecos (lotes que fallaron) y lo que se ve (la copia del
                        // disco) no es más corto: se queda esa en vez de una lista agujereada.
                        if is_playlist_key(&key) && list_short(tracks.len(), total) && l.tracks.len() >= tracks.len() {
                            log::info!("playlist {key}: llegaron {} de {total} pistas; se mantiene la que se ve", tracks.len());
                            keep_shown = true;
                        } else if self.pl_rerun.contains(&key) {
                            // Cambió mientras llegaba (una edición ya aplicada a lo que se ve):
                            // esta puede ser de antes y desharía el cambio un momento. Se queda la
                            // que se ve hasta la carga que sale ahora detrás.
                            keep_shown = true;
                        } else if is_playlist_key(&key) && tracks.len() > total as usize {
                            // Una sola carga nunca trae más filas que posiciones: se mezclaron dos
                            // (una dada por perdida que sí respondió). No se ve ni se guarda.
                            log::warn!("playlist {key}: llegaron {} filas para {total} posiciones; se descarta", tracks.len());
                            keep_shown = true;
                        }
                    }
                }
                let list_key = key.clone();
                if !keep_shown {
                    let list = self.lists.entry(key).or_default();
                    if !list.loading {
                        // Primer lote de una carga nueva: sustituye a la lista anterior en vez de
                        // anexarse a ella (si no, cada recarga duplicaba las canciones).
                        list.tracks.clear();
                    }
                    list.tracks.extend(tracks);
                    list.touch(&mut self.list_gen);
                    list.total = total;
                    list.loading = !done;
                    if self.diag {
                        log::info!("[diag] lista {} -> {} pistas", list_key, list.tracks.len());
                    }
                }
                if done && is_playlist_key(&list_key) {
                    let short = self.lists.get(&list_key).is_some_and(|l| list_short(l.tracks.len(), l.total));
                    if short {
                        // Con huecos no se guarda: en disco quedaría como buena y se vería así al
                        // abrirla. No se reintenta sola (las que faltan pueden no volver nunca),
                        // pero la página ofrece «Reintentar».
                        log::info!("playlist {list_key}: incompleta; no se guarda en disco");
                        self.list_retry.insert(list_key.clone(), (None, LIST_RETRY_DELAYS.len() as u8));
                        self.requested.remove(&format!("pl:{list_key}"));
                    } else {
                        self.list_retry.remove(&list_key);
                        // Con una repetición pendiente (una edición mientras llegaba) esta puede ser
                        // de antes del cambio: guarda la que sale detrás. Con más filas que
                        // posiciones se mezclaron dos cargas: en disco quedaría con repetidas.
                        let mixed = self.lists.get(&list_key).is_some_and(|l| l.tracks.len() > l.total as usize);
                        if mixed && !keep_shown {
                            log::warn!("playlist {list_key}: más filas que posiciones; no se guarda en disco");
                        }
                        if !keep_shown && !mixed && !self.pl_rerun.contains(&list_key) {
                            self.save_list(&list_key);
                            self.fill_list_cover(&list_key);
                        }
                    }
                }
                self.restore_ctx_arrived(&list_key);
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
                self.artists_synced_at = crate::cache::now_secs();
                self.snapshot_dirty = true;
            }
            Resp::Album(mut a) => {
                let id = a.id.clone();
                // Los géneros llegan aparte (Resp::Genres) y pueden adelantarse a una respuesta
                // que salió sin ellos: no se pierden.
                if a.genres.is_empty() {
                    if let Some(old) = self.albums.get(&id).filter(|o| !o.genres.is_empty()) {
                        a.genres = old.genres.clone();
                    }
                }
                self.save_album(&a);
                self.albums.insert(a.id.clone(), a);
                self.restore_ctx_arrived(&id);
            }
            Resp::Artist(mut a) => {
                let page = self.artists.entry(a.id.clone()).or_default();
                // Lo mismo con los géneros del artista, y con los seguidores: el de los metadatos
                // internos (respaldo de /artists, otra página que lo pide con Spotify limitando)
                // no los trae y no debe borrar los que ya llegaron.
                if let Some(old) = page.artist.as_ref() {
                    if a.genres.is_empty() && !old.genres.is_empty() {
                        a.genres = old.genres.clone();
                    }
                    if a.followers.as_ref().and_then(|f| f.total).is_none() && old.followers.is_some() {
                        a.followers = old.followers.clone();
                    }
                }
                page.artist = Some(a);
            }
            Resp::ArtistThumbs(list) => {
                // Solo completan lo que falta: una miniatura que llega tarde no debe pisar al
                // artista entero de su página (seguidores, géneros), que llega por Req::Artist.
                for a in list {
                    let page = self.artists.entry(a.id.clone()).or_default();
                    match page.artist.as_mut() {
                        None => page.artist = Some(a),
                        Some(old) => {
                            if old.images.is_empty() {
                                old.images = a.images;
                            }
                            if old.genres.is_empty() {
                                old.genres = a.genres;
                            }
                        }
                    }
                }
            }
            Resp::Genres { key, genres } => {
                // Del carril de enriquecimiento, cuando la página ya se ve. Lo que no esté en
                // memoria no importa: la próxima respuesta del álbum o artista ya los trae.
                if let Some(id) = key.strip_prefix("album:") {
                    if let Some(a) = self.albums.get_mut(id) {
                        a.genres = genres;
                    }
                    if let Some(a) = self.albums.get(id) {
                        self.save_album(a);
                    }
                } else if let Some(id) = key.strip_prefix("artist:") {
                    if let Some(a) = self.artists.get_mut(id).and_then(|p| p.artist.as_mut()) {
                        a.genres = genres;
                    }
                }
            }
            Resp::ArtistTop(t) => {
                if let Req::ArtistTop(id) = r.req {
                    self.artists.entry(id).or_default().top = t;
                }
            }
            Resp::TrackInfo(t) => {
                let album_id = t.album.as_ref().and_then(|a| a.id.clone());
                // La pista puede volver con otro id (Spotify sustituye ediciones): lo que cuenta
                // es la que se pidió.
                let asked = match &r.req {
                    Req::TrackInfo(id) => Some(id.clone()),
                    _ => t.id.clone(),
                };
                if let Some(now) = self.player.now.as_mut() {
                    if now.id == asked && now.album_id.is_none() {
                        now.album_id = album_id.clone();
                        if now.cover_url.is_none() {
                            now.cover_url = t.cover(300).map(|s| s.to_string());
                        }
                    }
                }
                if let Some(r) = self.recent.iter_mut().find(|r| r.id == asked) {
                    if r.album.as_ref().map(|a| a.id.is_none()).unwrap_or(true) {
                        r.album = t.album.clone();
                    }
                }
                // También en el registro local, del que sale el historial («Recientes» y la
                // página Historial): la canción se guardó al empezar, aún sin su álbum.
                if album_id.is_some() {
                    let stale = self
                        .play_log
                        .entries
                        .iter()
                        .any(|e| e.track.id == asked && e.track.album.as_ref().is_none_or(|a| a.id.is_none()));
                    if stale {
                        for e in std::sync::Arc::make_mut(&mut self.play_log).entries.iter_mut() {
                            if e.track.id == asked && e.track.album.as_ref().is_none_or(|a| a.id.is_none()) {
                                e.track.album = t.album.clone();
                            }
                        }
                        self.play_log_dirty = true;
                        self.history_cache = None;
                    }
                }
                if asked.is_some() && self.album_of_pending == asked {
                    self.album_of_pending = None;
                    match album_id {
                        Some(a) => self.go(Page::Album(a)),
                        None => self.status_err("No se encontró el álbum de esta canción"),
                    }
                }
            }
            Resp::RadioPlaylist { playlist_id } => {
                if let Req::RadioPlaylist(seed) = &r.req {
                    self.remember_radio(seed, &playlist_id);
                }
                self.open_radio_tab(playlist_id);
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
                // consulta enviada la última. Se compara con lo enviado, no con el texto de la
                // caja: si ella edita tras pulsar Intro, la respuesta llega igual y «Buscando»
                // no se queda fijo.
                if let Req::Search(q) = r.req {
                    let refresh = self.search_refreshing.remove(&q);
                    let current = self.search_pending.as_ref() == Some(&q);
                    if current {
                        self.search_loading = false;
                        self.search_pending = None;
                    }
                    // Un refresco que vuelve vacío (pathfinder sin nada y la Web API caída) no
                    // tapa lo guardado que ya se ve. Las respuestas que no se muestran también se
                    // guardan: valen para su consulta, y si ella vuelve a escribirla aparece al
                    // instante.
                    if current && !(refresh && search_is_empty(&s)) {
                        self.warm_search(&s);
                        self.cache_search(&q, s.clone());
                        self.search_result = Some(s);
                        self.search_result_for = Some(q);
                    } else {
                        self.cache_search(&q, s);
                    }
                }
            }
            Resp::RecentContexts(list) => self.merge_recent_contexts(list),
            Resp::Recent(t) => {
                if !self.play_log.seeded {
                    // Primera vez: el historial de Spotify sirve de semilla del registro local.
                    let plays = std::sync::Arc::make_mut(&mut self.play_log);
                    for tr in t.iter().rev() {
                        plays.record(tr.clone());
                    }
                    plays.seeded = true;
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
                // Llega completa de la Web API: el listado del rootlist que sale abajo no tiene
                // que completarla otra vez.
                self.playlists_web_ids.insert(id.clone());
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
                // La edición le da otro snapshot_id. Hasta que llegue el listado nuevo (se pide
                // abajo), el que se tiene ya no vale para dar por buena su copia en disco, ni
                // para guardarlo con la recarga que sale ahora.
                if let Some(p) = self.playlists.iter_mut().find(|p| p.id == id) {
                    p.snapshot_id = None;
                    // Lo editado ya está confirmado: a la biblioteca al momento. El listado del
                    // rootlist no trae la privacidad; sin esto quedaría la de antes del cambio.
                    if let Req::UpdatePlaylist { name, description, public, collaborative, .. } = &r.req {
                        p.name = name.clone();
                        p.description = Some(description.clone()).filter(|d| !d.is_empty());
                        match *public {
                            Some(public) => {
                                p.public = Some(public);
                                p.collaborative = Some(*collaborative && !public);
                            }
                            None => p.collaborative = Some(*collaborative),
                        }
                        self.snapshot_dirty = true;
                    }
                }
                match &r.req {
                    // Añadir o quitar: se aplica ya a la lista que se ve y la fresca la sustituye
                    // al llegar. Antes se borraba y la página abierta volvía de 100 en 100.
                    Req::AddToPlaylist { uris, .. } => self.apply_playlist_edit(&id, uris, true),
                    Req::RemoveFromPlaylist { uris, .. } => self.apply_playlist_edit(&id, uris, false),
                    // Nombre, descripción, privacidad, portada o seguirla: las pistas no cambian;
                    // solo se piden otra vez los metadatos.
                    other => {
                        self.invalidate(&format!("plmeta:{id}"));
                        // Seguirla o dejarla solo cambia la biblioteca (Req::Playlists, abajo): la
                        // página abierta los vuelve a pedir si le hacen falta, sin gastar una
                        // lectura de la Web API por cada clic desde un menú.
                        if matches!(other, Req::UpdatePlaylist { .. } | Req::SetPlaylistImage { .. }) {
                            self.playlist_meta.remove(&id);
                            self.meta_after_edit.insert(id.clone());
                            self.request_once(&format!("plmeta:{id}"), Req::PlaylistMeta(id.clone()));
                        }
                    }
                }
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
                // Playlists del listado del rootlist (solo trae el id del propietario) de este
                // usuario: ya tienen nombre que enseñar sin esperar a la Web API.
                if let Some(name) = u.display_name.as_ref().filter(|n| !n.is_empty()) {
                    let mut changed = false;
                    for p in self.playlists.iter_mut().filter(|p| p.owner.display_name.is_none() && p.owner.id.as_deref() == Some(u.id.as_str())) {
                        p.owner.display_name = Some(name.clone());
                        changed = true;
                    }
                    self.snapshot_dirty |= changed;
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
            // Mientras falta por responder una fuente mejor: se va enseñando (no se guarda).
            Resp::LyricsEarly(l) => {
                if let Req::Lyrics { id, .. } = &r.req {
                    if self.lyrics_for.as_deref() == Some(id.as_str()) && self.lyrics_loading {
                        self.lyrics = Some(l);
                        self.lyrics_loading = false;
                    }
                }
            }
            Resp::Lyrics(l) => {
                if self.diag {
                    log::info!(
                        "[diag] respuesta letras: {:?}",
                        l.as_ref().map(|l| (l.sync_type.clone(), l.lines.len(), l.lines.first().map(|x| x.words.clone())))
                    );
                }
                let id = match &r.req {
                    Req::Lyrics { id, .. } | Req::LyricsPrefetch { id } => id.clone(),
                    _ => String::new(),
                };
                if let Some(l) = &l {
                    self.lyrics_to_disk(l);
                }
                if self.lyrics_cache.len() > 300 {
                    self.lyrics_cache.clear();
                }
                self.lyrics_cache.insert(id.clone(), l.clone());
                // Solo la de la canción que suena cuenta: la de una ya saltada (o la de la
                // siguiente) no deja el panel en «sin letra» mientras llega la buena.
                if self.lyrics_for.as_deref() == Some(id.as_str()) {
                    self.lyrics = l;
                    self.lyrics_loading = false;
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
                let personal = matches!(r.req, Req::WebConnectPersonal(_));
                if personal {
                    self.web_busy = false;
                } else {
                    self.web_chain_done();
                }
                self.status(if personal {
                    "Tu app propia está conectada. Recargando la biblioteca…"
                } else {
                    "Biblioteca conectada con Spotify. Cargándola…"
                });
                // Si la autorización llegó antes que la sesión (encadenada al iniciar sesión), la
                // carga la hará `LoggedIn`, ya con el token nuevo.
                if self.logged_in() {
                    self.reload_library();
                }
            }
            Resp::WebDisconnected => {
                self.web_busy = false;
                self.status(if matches!(r.req, Req::WebDisconnectPersonal) {
                    "Tu app propia está desconectada"
                } else {
                    "Biblioteca desconectada de Spotify"
                });
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
    /// no permite consultarlo pista a pista), así que se cargan aquí. `force` (tras conectar la
    /// app propia) recarga además Me gusta y los artistas enteros aunque estén al día.
    fn refresh_from_network(&mut self, force: bool) {
        let have_snapshot = self.lists.contains_key(LIKED);
        self.requested.retain(|k| k == "albums" || k == "artists" || k == LIKED);
        // El listado que se tenga (de la instantánea, o de antes de una caída de la sesión) ya
        // no dice si las copias siguen al día: hasta que llegue el que se pide abajo, lo que
        // se abra se pide.
        self.playlists_fresh = None;
        self.playlists_asked = true;
        // La lista de playlists que se pide aquí vuelve a armar la precarga: una que quedara en
        // vuelo de la sesión anterior (quizá sin respuesta) no debe frenarla.
        self.prefetch_inflight = None;
        // Primero el estado del reproductor: decide la restauración de la sesión y el worker
        // de la API es secuencial (detrás del inicio y las playlists tardaba segundos).
        // Inicio, playlists, recientes y perfil siguen en primer plano: la portada y la barra
        // lateral los esperan.
        self.api.send(Req::PlayerState);
        self.api.send(Req::Me);
        self.api.send(Req::HomeFeed);
        self.api.send(Req::Playlists);
        self.api.send(Req::Recent);
        let now = crate::cache::now_secs();
        // Artistas seguidos: antes en cada arranque y cada reconexión; ahora solo sin copia o si
        // la copia ya tiene unas horas, y por el carril de fondo (la de la instantánea ya se ve).
        // Va antes que Me gusta: el carril es uno y en orden. Sin copia va en primer plano: el
        // de fondo falla al primer 429 (la identidad de primera parte en frío los da al arrancar)
        // y «artists» quedaría pedido sin respuesta, con «Siguiendo» vacío toda la sesión.
        if !self.artists_loaded {
            self.api.send(Req::FollowedArtists);
            self.requested.insert("artists".to_string());
        } else if force || now.saturating_sub(self.artists_synced_at) > ARTISTS_SYNC_SECS {
            self.api.send_bg(Req::FollowedArtists);
            self.requested.insert("artists".to_string());
        }
        // Me gusta: con copia, siempre lo reciente (1-2 páginas en primer plano); su respuesta
        // compara la cuenta de Spotify con la esperada y pide la recarga completa si no cuadra.
        // La completa sale ya solo sin copia, si la última tiene más de una semana o si la cuenta
        // no cuadraba desde la sesión anterior (una reconciliación pendiente o que falló). Ya no
        // cuenta la edad de la instantánea: cambia con cada guardado, no con cada sincronización.
        if have_snapshot {
            // Con la copia a la vista, la página no debe pedir la lista por su cuenta: esa carga
            // simple descarta lo que ya está en liked_set y dejaría la lista casi vacía.
            self.requested.insert(LIKED.to_string());
            self.api.send(Req::LikedRecent);
        }
        let synced_ago = now.saturating_sub(self.liked_synced_at);
        let mismatch = self
            .lists
            .get(LIKED)
            .is_some_and(|l| l.total as u64 != self.liked_server_total);
        let why = if !have_snapshot {
            Some("sin copia")
        } else if force {
            Some("recarga pedida")
        } else if synced_ago > LIKED_FULL_SYNC_SECS {
            Some("última sincronización completa hace más de una semana")
        } else if mismatch {
            Some("la cuenta no cuadraba con la última sincronización")
        } else {
            None
        };
        match why {
            Some(why) => self.request_liked_full(why),
            None => log::info!("Me gusta: solo lo reciente (sincronizada hace {} h)", synced_ago / 3600),
        }
    }

    /// Recarga completa de Me gusta por el carril de fondo. Se junta aparte y sustituye a la
    /// lista al terminar (ver `liked_refresh_pending`). Nunca dos a la vez: la segunda se
    /// mezclaría con la primera y, tras el `done` de esta, sus páginas vaciarían la lista.
    /// Sin lista a la vista (primer arranque) va en primer plano, como antes: ella la está
    /// esperando y el carril de fondo, que falla al primer 429, la dejaría sin corazones.
    fn request_liked_full(&mut self, why: &str) {
        if self.liked_refresh_pending {
            log::info!("Me gusta: {why}; ya hay una recarga completa en curso");
            return;
        }
        self.liked_refresh_pending = true;
        self.requested.insert(LIKED.to_string());
        if self.lists.get(LIKED).is_some_and(|l| !l.tracks.is_empty()) {
            log::info!("Me gusta: recarga completa en segundo plano ({why})");
            self.api.send_bg(Req::Liked);
        } else {
            log::info!("Me gusta: recarga completa ({why})");
            self.api.send(Req::Liked);
        }
    }

    /// Vuelve a pedir todo lo que depende de la Web API (tras conectar la app propia).
    pub fn reload_library(&mut self) {
        self.requested.clear();
        self.prefetch_inflight = None;
        // Las listas a medias se descartan abajo: se piden de nuevo al abrirlas.
        self.list_retry.clear();
        self.playlists_loaded = false;
        // Con la Web API recién conectada, el listado del rootlist que sale abajo se completa
        // con ella aunque se hubiera hecho hace poco (antes pudo fallar sin ella).
        self.playlists_web_at = 0;
        // Lo que ya se ve se conserva hasta que llegue lo nuevo: si la red falla (cuota, sin
        // conexión) la biblioteca no se queda vacía ni la instantánea se guarda a ceros.
        self.lists.retain(|k, _| k == LIKED);
        self.warming.clear();
        self.drop_list_staging();
        self.page_cache.retain(|k, _| k == LIKED);
        self.filter_cache = None;
        self.albums.clear();
        self.artists.clear();
        self.users.clear();
        self.user_playlists.clear();
        self.refresh_from_network(true);
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
                // En Spotify hay una más aunque aquí no se tenga la fila (entra con lo reciente
                // del próximo arranque): la cuenta y la base siguen al id, no a la fila, para que
                // LikedRecent no lo tome por un cambio hecho fuera y pida una recarga completa.
                self.liked_server_total += 1;
                let track = self.find_loaded_track(id);
                if let Some(list) = self.lists.get_mut(LIKED) {
                    list.total = list.total.saturating_add(1);
                    if let Some(track) = track {
                        if !list.tracks.iter().any(|t| t.id.as_deref() == Some(id)) {
                            list.tracks.insert(0, track);
                            list.touch(&mut self.list_gen);
                        }
                    }
                }
            }
        } else {
            let estaba = self.liked_set.remove(id);
            if estaba {
                self.liked_server_total = self.liked_server_total.saturating_sub(1);
            }
            // Al quitar el like, la canción desaparece al instante de «Canciones que te gustan»
            // (no se espera a recargar desde Spotify).
            if let Some(list) = self.lists.get_mut(LIKED) {
                let before = list.tracks.len();
                list.tracks.retain(|t| t.id.as_deref() != Some(id));
                if list.tracks.len() != before {
                    list.touch(&mut self.list_gen);
                }
                if estaba || list.tracks.len() != before {
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
        // Lo que suena allí no lo decodifica este equipo: `local_audio` ya no enseña nada con
        // `remote`. No se borra: un sondeo atrasado (justo tras traer la reproducción aquí) pone
        // `remote` un momento y, al volver a `None` sin otro TrackChanged, la etiqueta de la
        // canción que sigue sonando aquí se habría perdido hasta la siguiente.
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
            // Se marca como la consulta en vuelo; si no, su respuesta se ignoraría.
            self.search_pending = Some("daft punk".to_string());
            self.search_refreshing.clear();
            self.search_retry = None;
            self.search_retries = 0;
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
        // A los 20 s del primer fotograma la versión en uso funciona: si es una recién instalada
        // deja de estar a prueba, y se borran el ejecutable anterior y los restos de 1.4–1.6.
        // Antes no: el anterior es la vuelta atrás si la nueva no llega a arrancar. Con una
        // sustitución en marcha desde esta ventana se espera, porque el registro ya describe la
        // versión que se está instalando, no esta.
        if !self.health_marked {
            let now = Instant::now();
            let at = *self.health_at.get_or_insert(now + crate::update::HEALTHY_AFTER);
            if now < at {
                ctx.request_repaint_after(at - now);
            } else if !self.update_applying && self.restart_after_exit.is_none() {
                self.health_marked = true;
                crate::update::mark_healthy();
            }
        }
        self.flush_volume(ctx);
        self.poll_restore_queue(ctx);
        // Búsqueda limitada por Spotify (429 corto): se repite sola al pasar la espera, solo si
        // ella no ha lanzado otra consulta mientras tanto.
        if let Some(at) = self.search_retry.as_ref().map(|r| r.0) {
            let now = Instant::now();
            if now >= at {
                if let Some((_, q)) = self.search_retry.take() {
                    if self.search_pending.as_deref() == Some(q.as_str()) {
                        self.search_retries += 1;
                        log::info!("reintento {} de la búsqueda «{q}»", self.search_retries);
                        self.api.send(Req::Search(q));
                    }
                }
            } else {
                ctx.request_repaint_after(at - now);
            }
        }
        // Precarga suave de playlists para que abrirlas sea instantáneo: de una en una, la
        // siguiente cuando termina la anterior (o si su respuesta se perdió). Antes salía una cada
        // 500 ms sin esperar y llenaba la cola de la API delante de lo que ella abría o buscaba.
        if let Some(at) = self.prefetch_at {
            let now = Instant::now();
            let busy = self.prefetch_inflight.as_ref().map(|(_, t)| now.duration_since(*t)).filter(|d| *d < PREFETCH_STALE);
            if now < at {
                ctx.request_repaint_after(at - now);
            } else if let Some(waited) = busy {
                // Al terminar la respuesta repinta sola; esto es solo por si se pierde.
                ctx.request_repaint_after(PREFETCH_STALE - waited);
            } else if self.logged_in() {
                if let Some((lost, _)) = self.prefetch_inflight.take() {
                    log::info!("precarga de playlist {lost}: sin respuesta en {} s; se sigue con la siguiente", PREFETCH_STALE.as_secs());
                }
                while let Some(id) = self.prefetch_ids.pop_front() {
                    // Vuelve de esperar su copia del disco: estar en memoria (es esa copia) no la
                    // descarta, aún falta decidir si pedirla.
                    // Solo se suelta con la suya: si el listado rehízo la cola mientras se leía,
                    // otra delante no le quita la marca y, al llegarle el turno, aún se decide.
                    let warmed = self.prefetch_warm.as_deref() == Some(id.as_str());
                    if warmed {
                        self.prefetch_warm = None;
                    }
                    // La que ya está en memoria o pedida (página abierta, restauración de la
                    // sesión) no se pide otra vez: dos cargas de una misma playlist a la vez
                    // mezclan sus lotes.
                    let restoring = self.restoring_playlist(&id);
                    if (self.lists.contains_key(&id) && !warmed)
                        || self.requested.contains(&format!("pl:{id}"))
                        || self.pl_busy(&id)
                        || restoring
                    {
                        continue;
                    }
                    // Con el listado de esta sesión, la copia en disco dice si hace falta pedirla:
                    // se pone en memoria (al abrirla se ve al instante) y, si su snapshot_id es el
                    // del listado, no se pide nada. Antes cada arranque volvía a bajar entera cada
                    // playlist precargada. Una lectura del disco cada vez: varias seguidas, de
                    // listas de miles de pistas, ocuparían el hilo del disco delante de lo demás.
                    let read = warmed || (self.listing_fresh() && !self.requested.contains(&format!("warm:{id}")));
                    if read {
                        self.warm_list(&id);
                        if self.warming.contains_key(&id) {
                            // Se lee en el hilo del disco: su llegada despierta la interfaz y se
                            // sigue entonces por esta misma.
                            self.prefetch_warm = Some(id.clone());
                            self.prefetch_ids.push_front(id);
                            break;
                        }
                        if self.skip_unchanged(&id) {
                            ctx.request_repaint();
                            break;
                        }
                    }
                    // Copia reciente sin snapshot_id con que compararla (de antes de guardarlo, o
                    // sin listado de esta sesión): no se pide, y al abrirla se refresca entonces.
                    let comparable = self.listing_fresh() && self.list_disk.get(&id).is_some_and(|d| d.snapshot_id.is_some());
                    if !comparable && self.list_copy_age(&id).is_some_and(|age| age < PREFETCH_FRESH) {
                        if read {
                            ctx.request_repaint();
                            break;
                        }
                        continue;
                    }
                    log::info!("precarga de playlist {id}");
                    self.load_playlist(&id, false, false);
                    self.prefetch_inflight = Some((id, now));
                    break;
                }
                if self.prefetch_ids.is_empty() {
                    self.prefetch_at = None;
                }
            }
        }
        // Playlists que se quedaron a medias: el reintento que toque, solo con sesión. Con el
        // aviso de cuota a la vista Spotify está limitando: se espera a que se vaya (8 s).
        if !self.list_retry.is_empty() && self.logged_in() {
            let now = Instant::now();
            let mut due: Vec<String> = Vec::new();
            let mut next: Option<Instant> = None;
            for (id, (at, _)) in &self.list_retry {
                match *at {
                    Some(at) if at <= now => due.push(id.clone()),
                    Some(at) => next = Some(next.map_or(at, |n| n.min(at))),
                    None => {}
                }
            }
            let quota = self.status.as_ref().is_some_and(|s| s.0.starts_with("Spotify ha agotado"));
            if !due.is_empty() && quota {
                ctx.request_repaint_after(Duration::from_secs(1));
            } else {
                for id in due {
                    // La restauración de la sesión ya la pide por su cuenta.
                    if self.restoring_playlist(&id) {
                        self.list_retry.remove(&id);
                        continue;
                    }
                    log::info!("playlist {id}: reintento automático");
                    self.retry_list(&id);
                }
            }
            if let Some(at) = next {
                ctx.request_repaint_after(at - now);
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
                // Si la pista transferida aún está cargando (claves lentas, reintentos), sigue
                // valiendo que al abrir nunca suena sola: al llegar se pausa y, si falla, no se
                // reintenta sola (`on_load_failed`, que consume la marca).
                if self.player.state != PlayState::Loading {
                    self.pause_after_restore = false;
                }
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
        // Mientras una canción carga aquí, los lotes de metadatos en segundo plano (una playlist
        // de miles) esperan entre lote y lote para no ir por delante de ella en spclient.
        let loading_here = self.player.state == PlayState::Loading && self.player.remote.is_none();
        crate::api::set_playback_loading(loading_here);
        // Vigilante por etapas (`watchdog`): «cargando» nunca se queda así sin más. A los 2,5 s la
        // barra dice que va lenta; a los 8 s, si la carga no avanza, se comprueba Spirc y se pide
        // otra vez; a los 15 s, si sigue sin avanzar, se reconecta; a los 30 s se deja de esperar
        // con un aviso y [Reintentar]. Antes eran 20 s de «cargando» sin explicación y una
        // reconexión que se repetía cada 20 s si la canción no podía sonar.
        if loading_here {
            let now = Instant::now();
            let activity = librespot_core::ttfs::activity();
            let watch = self.load_watch.get_or_insert_with(|| watchdog::LoadWatchdog::new(now, activity));
            match watch.tick(now, activity) {
                watchdog::Stage::Wait => {}
                watchdog::Stage::Slow => self.prefetch_pending_context(),
                watchdog::Stage::Retry => {
                    log::warn!("[vigilante] 8 s cargando sin avanzar: se comprueba Spirc y se pide otra vez");
                    self.skip_stuck_context();
                    // En una Jam, volver a pedir la carga la repetiría para todos: solo el ping.
                    let again = self.pending_load.clone().filter(|_| self.jam.is_none()).map(Box::new);
                    self.backend.send(Cmd::RetryLoad(again));
                }
                watchdog::Stage::Reconnect => {
                    log::warn!("[vigilante] 15 s cargando sin avanzar: se reconecta");
                    // Las pistas del contexto pudieron llegar después de los 8 s: al volver, la
                    // carga que se repite ya no espera al contexto.
                    self.skip_stuck_context();
                    self.backend.send(Cmd::Stalled);
                }
                watchdog::Stage::GiveUp => self.give_up_loading(),
            }
            // A menudo: el avance se mide en ventanas de 2 s.
            ctx.request_repaint_after(Duration::from_millis(500));
        } else {
            self.load_watch = None;
        }
        self.tick_failed_load(ctx);
        self.tick_stall(ctx);
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
                    crate::shell::ui_phase("botones de la barra de tareas", String::new);
                    crate::taskbar::set_playing(playing);
                }
            }
        }
        if let Some((at, pos, adds)) = self.pending_queue.clone() {
            if Instant::now() >= at {
                self.pending_queue = None;
                if let Some(pos) = pos {
                    self.seek(pos);
                }
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
            // Puesta al día pedida tras aleatorio, repetir o mover una canción.
            if let Some(&at) = self.queue_refresh.first() {
                if Instant::now() >= at {
                    self.queue_refresh.remove(0);
                    if self.side == Some(SideTab::Queue) {
                        self.queue_at = Instant::now();
                        self.api.send(Req::Queue);
                    }
                } else {
                    ctx.request_repaint_after(at - Instant::now());
                }
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
                        match self.radios().get(&id).cloned() {
                            // Radio ya conocida: se abre al instante (con su copia en disco).
                            Some(pid) => self.open_radio_tab(pid),
                            None => {
                                self.status("Abriendo la radio…");
                                self.api.send(Req::RadioPlaylist(id));
                            }
                        }
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
                    let editor = match p {
                        Some(p) => {
                            // La privacidad, de donde se sepa (la copia que se pasa puede no traerla).
                            let public = p
                                .public
                                .or_else(|| self.playlists.iter().find(|x| x.id == p.id).and_then(|x| x.public))
                                .or_else(|| self.playlist_meta.get(&p.id).and_then(|x| x.public));
                            let collaborative = p.collaborative.unwrap_or(false);
                            PlaylistEditor {
                                id: Some(p.id.clone()),
                                name: p.name.clone(),
                                description: p.description.clone().unwrap_or_default(),
                                // Sin saberla, se ve como la deja la colaboración (colaborativa
                                // es privada) y no se envía al guardar.
                                public: public.unwrap_or(!collaborative),
                                public_known: public.is_some(),
                                collaborative,
                                image_path: None,
                                busy: false,
                            }
                        }
                        None => PlaylistEditor {
                            id: None,
                            name: "Nueva playlist".to_string(),
                            description: String::new(),
                            public: true,
                            public_known: true,
                            collaborative: false,
                            image_path: None,
                            busy: false,
                        },
                    };
                    self.editor = Some(editor);
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
                Action::RetryList(id) => self.retry_list(&id),
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
    pub(super) fn queue_rebuild(&mut self, keep: Vec<String>) {
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
        self.pending_queue = Some((Instant::now() + Duration::from_millis(1500), Some(pos), keep));
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

    /// «Ver álbum» de una canción guardada sin su álbum (el historial lo guardaba así al empezar
    /// a sonar): se piden los datos de la canción y, al llegar, se abre su álbum.
    pub fn open_album_of(&mut self, track_id: String) {
        self.album_of_pending = Some(track_id.clone());
        self.status("Buscando el álbum…");
        self.api.send(Req::TrackInfo(track_id));
    }

    pub fn queue_clear(&mut self) {
        self.queue_rebuild(Vec::new());
    }

    /// Quita lo que queda de la lista en curso: sigue la que suena, en su punto, y después solo
    /// lo añadido a la cola (y, con la reproducción automática, canciones parecidas).
    pub fn queue_drop_context(&mut self) {
        if self.player.remote.is_some() {
            self.status_err("La cola solo se puede editar cuando la música suena en Nanofy");
            return;
        }
        let Some(now) = self.player.now.clone() else { return };
        let keep = self.queued_local.clone();
        let pos = self.player.position();
        self.play(PlayTarget::Tracks { uris: vec![now.uri.clone()], index: Some(0), shuffle: false });
        self.last_play_page = None;
        self.queued_local = keep.clone();
        self.pending_queue = Some((Instant::now() + Duration::from_millis(1500), Some(pos), keep));
        self.queue = None;
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
        // Se decide en cuanto llega el estado del clúster de Connect (~1,8 s tras abrir): dice si
        // suena en otro dispositivo y conserva pista, posición exacta y cola de lo último que
        // quedó en pausa en cualquiera (el teléfono) aunque ya esté cerrado. Esperar además a la
        // Web API (/me/player, recently-played) retrasaba el «listo» hasta el plazo de 4 s, y casi
        // nunca cambiaba la decisión: solo se espera por ella si el clúster llega vacío. El plazo
        // de respaldo llama con force=true si nada llega.
        let web_pending = self.api.web_configured() && (self.server_now.is_none() || self.server_last.is_none());
        let cluster_has_session = matches!(self.server_cluster, Some(Some(_)));
        if !force && !cluster_has_session && (web_pending || self.server_cluster.is_none()) {
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
            // Si es la misma pista casi en el mismo punto, el clúster es el eco de la propia
            // sesión de Nanofy: la copia local (con su cola y origen) es la buena.
            let local_pos = self.restore_pending.as_ref().map(|s| s.position_ms).unwrap_or(0);
            let same = Some(&c.track_uri) == local_track.as_ref() && (c.position_ms as i64 - local_pos as i64).abs() < 5000;
            if self.restore_pending.is_none() || (newer && !same) {
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
                first: None,
            },
            _ => Cmd::LoadTracks { uris: vec![last.track_uri.clone()], index: Some(0), shuffle: false, resume: Some(0), first: None },
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


    /// Prepara la cola desde las pistas del contexto (playlist, álbum o Me gusta): de memoria o
    /// de la copia en disco, sin petición alguna; si no están (o la copia no tiene la pista
    /// actual), se piden una vez en cuanto haya sesión y se arma al llegar (Resp::Tracks /
    /// Resp::Album).
    fn prepare_context_queue(&mut self, context_uri: Option<&str>, current: &str) {
        self.abandon_restore_ctx();
        let Some(uri) = context_uri else { return };
        let Some((key, kind)) = ctx_list_key(uri) else {
            log::debug!("[cola-ctx] {uri}: contexto sin lista propia; la cola llega del servidor");
            return;
        };
        // Antes nunca se miraba el disco: la restauración esperaba siempre a la red. La copia de
        // una playlist se lee en el hilo del disco: al llegar (`on_warmed`) arma la cola, y el
        // tick no la pide mientras tanto.
        match kind {
            CtxKind::Playlist => self.warm_list(&key),
            CtxKind::Album => self.warm_album(&key),
            CtxKind::Liked => {}
        }
        if self.build_context_queue(&key, kind, current) {
            return;
        }
        // La petición necesita la sesión de librespot: el tick la lanza en cuanto la haya.
        let now = Instant::now();
        self.restore_ctx_at = (kind != CtxKind::Liked).then_some(now);
        self.restore_ctx_until = Some(now + RESTORE_CTX_DEADLINE);
        self.restore_ctx = Some((key, kind, current.to_string()));
    }

    /// Esa playlist es el contexto cuya cola se está restaurando: la restauración la pide y la
    /// reintenta por su cuenta.
    fn restoring_playlist(&self, id: &str) -> bool {
        self.restore_ctx.as_ref().is_some_and(|(k, kind, _)| *kind == CtxKind::Playlist && k == id)
    }

    fn clear_restore_ctx(&mut self) {
        self.restore_ctx = None;
        self.restore_ctx_at = None;
        self.restore_ctx_fails = 0;
        self.restore_ctx_until = None;
    }

    /// Se deja la restauración sin haber armado la cola (plazo agotado, otra reproducción, otra
    /// restauración). Mientras la llevaba, los fallos de esa playlist no programaban reintentos
    /// y la dejaban pedida (`pl:`): si nada más se ocupa de ella, se desmarca para que abrirla la
    /// pida otra vez en vez de quedarse a medias sin «Reintentar».
    fn abandon_restore_ctx(&mut self) {
        if let Some((key, kind, _)) = self.restore_ctx.take() {
            if kind == CtxKind::Playlist && !self.pl_busy(&key) && !self.list_retry.contains_key(&key) {
                self.invalidate(&format!("pl:{key}"));
            }
        }
        self.clear_restore_ctx();
    }

    /// Llegaron pistas de `key` (playlist, álbum o Me gusta): si es el contexto en restauración,
    /// se intenta armar su cola. Es lo que la arma: el tick ya no lo intenta en cada fotograma.
    fn restore_ctx_arrived(&mut self, key: &str) {
        let Some((k, kind, cur)) = self.restore_ctx.as_ref() else { return };
        if k != key {
            return;
        }
        let (kind, cur) = (*kind, cur.clone());
        if self.build_context_queue(key, kind, &cur) {
            self.clear_restore_ctx();
        }
    }

    /// Falló la carga de las pistas del contexto en restauración (la suya o la de quien la pidió
    /// antes, p. ej. la página abierta): se vuelve a pedir con espera creciente, o se deja si se
    /// agotan los reintentos o el contexto ya no existe. Se llama antes de repartir el error: si
    /// se deja, sus brazos la tratan como a cualquier otra (reintentos, «Reintentar»).
    fn restore_ctx_failed(&mut self, e: &str) {
        let Some((key, kind)) = self.restore_ctx.as_ref().map(|c| (c.0.clone(), c.1)) else { return };
        if e.contains("404") {
            log::info!("[restore] contexto {key}: ya no existe; la cola se deja sin armar");
            self.clear_restore_ctx();
            return;
        }
        // Ya hay un envío programado: este fallo es de otra carga, no de la última pedida.
        if self.restore_ctx_at.is_some() {
            // Salvo un álbum antes del primer envío: era la petición de «Siguientes de», la que
            // la restauración iba a compartir, y sigue marcada (su error no la desmarca); sin
            // desmarcarla, el envío programado no saldría.
            if kind == CtxKind::Album && self.restore_ctx_fails == 0 {
                self.invalidate(&format!("album:{key}"));
            }
            return;
        }
        // Sin sesión (reconexión en curso) una playlist falla al momento y no gasta intento: lo
        // acota el plazo. Un álbum sí lo gasta: con cero fallos no se desmarcaría para repetirlo.
        let counts = kind == CtxKind::Album || !e.contains("no has iniciado sesión");
        let fails = self.restore_ctx_fails + u8::from(counts);
        if fails as usize > RESTORE_CTX_RETRY.len() {
            log::info!("[restore] contexto {key}: {e}; sin más reintentos, la cola se deja sin armar");
            self.clear_restore_ctx();
            return;
        }
        let wait = RESTORE_CTX_RETRY[(fails as usize).saturating_sub(1)];
        log::info!("[restore] contexto {key}: {e}; se pide otra vez en {} ms", wait.as_millis());
        self.restore_ctx_fails = fails;
        self.restore_ctx_at = Some(Instant::now() + wait);
    }

    /// Pide las pistas del contexto en restauración: una vez en cuanto haya sesión y otra solo
    /// tras un fallo (ver `restore_ctx_failed`). Antes las pedía cada 1,2 s sin fin (una
    /// playlist de miles de pistas entera cada vez) y repintaba sin parar mientras tanto.
    fn poll_restore_queue(&mut self, ctx_ui: &egui::Context) {
        if self.restore_ctx.is_none() {
            return;
        }
        let now = Instant::now();
        if self.restore_ctx_until.is_some_and(|t| now >= t) {
            if let Some((key, _, _)) = self.restore_ctx.as_ref() {
                log::info!("[restore] cola del contexto {key}: sin armar tras {} s; se deja", RESTORE_CTX_DEADLINE.as_secs());
            }
            self.abandon_restore_ctx();
            return;
        }
        // Ya pedidas (o Me gusta, que no se pide): su respuesta, lote o error, despierta la
        // interfaz y arma la cola; no hace falta repintar mientras tanto.
        let Some(at) = self.restore_ctx_at else { return };
        if !self.logged_in() {
            // Sin sesión no hay a quién pedir: se mira otra vez en breve (hasta el plazo).
            ctx_ui.request_repaint_after(Duration::from_millis(300));
            return;
        }
        if now < at {
            ctx_ui.request_repaint_after(at - now);
            return;
        }
        // Su copia en disco se está leyendo: al llegar arma la cola (`on_warmed`) o, si no
        // basta, se pide entonces. Pedirla ya gastaría una carga entera que la copia evita.
        if self.restore_ctx.as_ref().is_some_and(|(k, kind, _)| *kind == CtxKind::Playlist && self.warming.contains_key(k)) {
            return;
        }
        self.restore_ctx_at = None;
        let Some((key, kind, cur)) = self.restore_ctx.clone() else { return };
        // Pudieron llegar por un camino que no avisa (la página abierta mostró la copia del disco).
        if self.build_context_queue(&key, kind, &cur) {
            self.clear_restore_ctx();
            return;
        }
        match kind {
            // Con una carga ya en vuelo (la de la página o la precarga) no sale otra: se espera a
            // esa, y su lote final o su error cuentan igual para la restauración.
            CtxKind::Playlist => self.load_playlist(&key, true, false),
            CtxKind::Album => {
                // La misma clave que la página y «Siguientes de»: una sola petición entre todos.
                // Tras un fallo sigue marcada (el error no la desmarca): se desmarca para repetirla.
                let k = format!("album:{key}");
                if self.restore_ctx_fails > 0 {
                    self.invalidate(&k);
                }
                self.request_once(&k, Req::Album(key));
            }
            CtxKind::Liked => {}
        }
    }

    /// Arma `self.queue` con las pistas del contexto tras la actual. Devuelve true si ya no hay
    /// nada que esperar: la cola armada, o la lista completa sin la pista actual (antes se armaba
    /// entonces desde la primera, una cola equivocada). Solo se clonan la actual y las 80
    /// siguientes (antes, la lista entera en cada intento, y el tick lo intentaba cada fotograma).
    fn build_context_queue(&mut self, key: &str, kind: CtxKind, current: &str) -> bool {
        // En una Jam (o con la cola de muestra) la cola que se ve no es la de este contexto.
        if self.jam_queue_active || self.queue_frozen {
            return true;
        }
        // Ya está la cola exacta de esta pista (la del servidor por Req::Queue, o la guardada al
        // cerrar): es mejor que la del contexto (trae el orden aleatorio y lo añadido a mano) y
        // no se pisa. Pasaba sobre todo con Me gusta, cuya recarga completa llega tarde.
        if self.queue.as_ref().is_some_and(|q| {
            !q.queue.is_empty() && q.currently_playing.as_ref().is_some_and(|t| t.uri == current)
        }) {
            return true;
        }
        let (now, upcoming) = {
            // (pistas, si siguen llegando a esta misma lista, si ya es la definitiva)
            let (tracks, more, settled): (&[Track], bool, bool) = match kind {
                // La respuesta de un álbum trae todas sus pistas.
                CtxKind::Album => match self.albums.get(key).and_then(|a| a.tracks.as_ref()) {
                    Some(p) => (p.items.as_slice(), false, true),
                    None => (&[], false, false),
                },
                // Una copia del disco o una carga a medias aún pueden cambiar: sin la pista actual
                // se espera a la fresca. Me gusta nunca se da por definitiva (lo guardado en otro
                // dispositivo llega aparte, en LikedRecent); la acota el plazo. Con una repetición
                // pendiente (cambió mientras llegaba) la que trae la pista puede ser la que sigue.
                CtxKind::Playlist | CtxKind::Liked => match self.lists.get(key) {
                    Some(l) => {
                        let settled = kind == CtxKind::Playlist
                            && !l.loading
                            && !self.list_cached.contains(key)
                            && !self.pl_rerun.contains(key)
                            && !list_short(l.tracks.len(), l.total);
                        (l.tracks.as_slice(), l.loading, settled)
                    }
                    None => (&[], false, false),
                },
            };
            if tracks.is_empty() {
                log::debug!("[cola-ctx] {key}: aún sin pistas");
                return false;
            }
            let Some(cur) = tracks.iter().position(|t| t.uri == current) else {
                if settled {
                    log::info!("[cola-ctx] {key}: la pista actual no está en el contexto; sin cola");
                }
                return settled;
            };
            let upcoming = &tracks[cur + 1..];
            // Una carga que sigue llegando trae más detrás: se esperan las 80 en vez de armar una
            // cola corta con lo llegado hasta ahora.
            if more && upcoming.len() < 80 {
                return false;
            }
            (tracks[cur].clone(), upcoming.iter().take(80).cloned().collect::<Vec<Track>>())
        };
        log::debug!("[cola-ctx] {key}: actual={current}, {} detrás", upcoming.len());
        if upcoming.is_empty() {
            // La última del contexto: no hay nada detrás y eso también es haberla armado (antes se
            // reintentaba sin fin). No pisa la cola exacta del servidor si ya llegó.
            if self.queue.is_none() {
                self.queue = Some(QueueResponse { currently_playing: Some(now), queue: Vec::new() });
            }
            return true;
        }
        let n = upcoming.len();
        self.queue = Some(QueueResponse {
            currently_playing: Some(now),
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
                first: None,
            },
            None => Cmd::LoadTracks { uris: vec![item.uri.clone()], index: Some(0), shuffle: false, resume: Some(pos), first: None },
        };
        if let Some(uri) = &ctx {
            self.last_play = Some(PlayTarget::Context { uri: uri.clone(), track_uri: Some(item.uri.clone()), index: None, shuffle });
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
        // Después de preparar la cola: si la restauración va a cargar la playlist del contexto,
        // esa carga ya trae su nombre y no se pide aparte (ver ensure_context_meta).
        if let Some(uri) = &ctx {
            self.ensure_context_meta(uri);
        }
        self.media_dirty = true;
        log::info!("[restore] servidor: {} en {} ms (ctx {:?})", item.name, pos, ctx);
    }

        /// Carga en pausa la sesión de la cuenta tal como la tiene Spotify (clúster de Connect):
    /// contexto, pista, posición, aleatorio, repetición y cola manual.
    fn restore_from_cluster(&mut self, info: crate::backend::ClusterInfo) {
        let pos = info.position_ms;
        // Solo contextos que librespot puede resolver; «spotify:web-api» (pistas sueltas puestas
        // desde la Web API o el móvil) y similares se restauran como lista de pistas.
        let ctx_kind = info.context_uri.split(':').nth(1).unwrap_or("");
        let ctx_ok = matches!(ctx_kind, "playlist" | "album" | "artist" | "show" | "station" | "collection" | "user");
        let cmd = if ctx_ok && !info.context_uri.is_empty() && info.context_uri != "-" {
            Cmd::LoadContext {
                uri: info.context_uri.clone(),
                track_uri: Some(info.track_uri.clone()),
                index: None,
                shuffle: info.shuffle,
                resume: Some(pos),
                first: None,
            }
        } else {
            let mut uris = vec![info.track_uri.clone()];
            uris.extend(info.next.iter().filter(|u| !info.queue.contains(u)).cloned());
            Cmd::LoadTracks { uris, index: Some(0), shuffle: false, resume: Some(pos), first: None }
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
            self.pending_queue = Some((Instant::now() + Duration::from_millis(2500), Some(pos), info.queue));
        }
        log::info!("[restore] sesión del clúster: {} en {} ms ({})", info.track_uri, pos, info.context_uri);
    }

    /// Pide el nombre del contexto (álbum, playlist, artista) para que «Siguientes de: …» lo
    /// muestre aunque nunca se haya abierto su página en esta sesión.
    fn ensure_context_meta(&mut self, uri: &str) {
        let mut parts = uri.splitn(3, ':');
        let (Some(_), Some(kind), Some(id)) = (parts.next(), parts.next(), parts.next()) else { return };
        let id = id.to_string();
        // La copia en disco del álbum ya trae el nombre (y la restauración la mira justo después
        // para la cola): con ella no se gasta una petición a la Web API solo para «Siguientes de».
        // Abrir su página lo pide fresco igualmente.
        if kind == "album" {
            self.warm_album(&id);
        }
        if kind == "playlist" && !self.playlist_meta.contains_key(&id) && !self.playlists.iter().any(|p| p.id == id) {
            // Su copia en disco trae el nombre (se lee en el hilo del disco), y la carga de sus
            // pistas (la de la restauración o una ya en vuelo) lo manda antes de la primera fila
            // (PlaylistMetaPartial). Pedirlo aparte gastaba una lectura de la Web API o, en las
            // de Spotify (37i9), otra descarga entera de la misma playlist4.
            self.warm_list(&id);
            if self.warming.contains_key(&id) || self.pl_busy(&id) || self.restoring_playlist(&id) {
                return;
            }
        }
        let (key, req) = match kind {
            "album" if !self.albums.contains_key(&id) => (format!("album:{id}"), Req::Album(id)),
            "playlist" if !self.playlist_meta.contains_key(&id) && !self.playlists.iter().any(|p| p.id == id) => (format!("plmeta:{id}"), Req::PlaylistMeta(id)),
            // Del artista basta el nombre: el lote de metadatos internos, sin la lectura de la Web
            // API ni la búsqueda de géneros de Req::Artist (eso lo pide su página al abrirla).
            "artist" if !self.artists.contains_key(&id) => (format!("artistmeta:{id}"), Req::ArtistThumbs(vec![id])),
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
                first: None,
            },
            PlayTarget::Tracks { uris, index, shuffle } => {
                let index = uris.iter().position(|u| u == &saved.now.uri).map(|i| i as u32).or(index);
                Cmd::LoadTracks { uris, index, shuffle, resume: Some(pos), first: None }
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
            self.pending_queue = Some((Instant::now() + Duration::from_millis(2500), Some(pos), saved.queued.clone()));
        }
        // «Reiniciar» para actualizar con música sonando: se carga en pausa en el mismo segundo y
        // el evento de pausa la reanuda, como tras una reconexión. Solo esta vez: la regla de que
        // al abrir nunca suena sola vale para cualquier otro arranque.
        if std::mem::take(&mut self.resume_after_update) {
            self.pause_after_restore = false;
            self.play_after_restore = true;
            self.player.state = PlayState::Loading;
            log::info!("[update] se retoma la música tras actualizar");
        }
        log::info!("[restore] {} en {} ms", saved.now.name, pos);
    }

    /// Tras reconectar, el reproductor de la sesión nueva está vacío: se vuelve a cargar la pista
    /// que sonaba, en el segundo en que se cortó, dentro de su contexto y con aleatorio,
    /// repetición y cola manual. Repetir la orden original la devolvería al principio (o a la
    /// canción con la que empezó el contexto).
    fn resume_after_reconnect(&mut self, point: ResumePoint) {
        let Some(now) = self.player.now.clone() else { return };
        let shuffle = self.player.shuffle;
        let cmd = match self.last_play.clone() {
            Some(PlayTarget::Context { uri, .. }) => Cmd::LoadContext {
                uri,
                track_uri: Some(now.uri.clone()),
                index: None,
                shuffle,
                resume: Some(point.pos),
                first: None,
            },
            Some(PlayTarget::Tracks { uris, .. }) if uris.contains(&now.uri) => {
                let index = uris.iter().position(|u| u == &now.uri).map(|i| i as u32);
                Cmd::LoadTracks { uris, index, shuffle, resume: Some(point.pos), first: None }
            }
            _ => Cmd::LoadTracks { uris: vec![now.uri.clone()], index: Some(0), shuffle: false, resume: Some(point.pos), first: None },
        };
        self.backend.send(cmd);
        let (context, track) = match self.player.repeat {
            Repeat::Off => (false, false),
            Repeat::Context => (true, false),
            Repeat::Track => (true, true),
        };
        if self.player.repeat != Repeat::Off {
            self.backend.send(Cmd::Repeat { context, track });
        }
        if !self.queued_local.is_empty() {
            // La cola manual se vuelve a añadir cuando el dispositivo ya está activo; sin
            // buscar la posición, que ya es la buena y seguirá avanzando.
            self.pending_queue = Some((Instant::now() + Duration::from_millis(2500), None, self.queued_local.clone()));
        }
        // Se carga en pausa en el segundo exacto; si sonaba, el evento de pausa la reanuda.
        self.pause_after_restore = false;
        self.play_after_restore = point.playing;
        self.player.state = if point.playing { PlayState::Loading } else { PlayState::Paused };
        log::info!(
            "[reconexión] se retoma {} en {} ms ({})",
            now.name,
            point.pos,
            if point.playing { "sonando" } else { "en pausa" }
        );
    }

    /// La canción pedida (`uri`) no se pudo cargar. Un fallo pasajero (Spotify frenando las
    /// claves o las peticiones, la red) ya no se trata como «no disponible»: Spirc la deja en
    /// pausa en su posición, sin marcarla ni saltar, y aquí se reintenta sola a los 15 s y a los
    /// 60 s; si tampoco, queda el aviso con [Reintentar] y [Saltar]. Uno definitivo se avisa y
    /// Spirc pasa a la siguiente (o se detiene si ya van varias seguidas).
    fn on_load_failed(&mut self, uri: String, reason: librespot_playback::player::LoadFailure, transient: bool, play: bool) {
        use player_bar::{PlaybackError, PlaybackErrorKind};
        let name = self.known_track_name(&uri);
        log::warn!(
            "[reproducción] no se pudo cargar {uri} ({reason}; {})",
            if transient { "pasajero" } else { "definitivo" }
        );
        // La carga de la restauración terminó, aunque sin sonar: ya no se está restaurando (antes
        // la marca solo la quitaba `TrackChanged`, que no llega nunca si la canción no carga). La
        // canción queda en pausa para «Reproducir» y la cola de su contexto se pide como al
        // cargar. Si la trajo la sesión de Spotify, la copia local de respaldo ya no hace falta:
        // sí trajo una pista.
        if std::mem::take(&mut self.restore_mark) {
            if self.restore_fallback_at.take().is_some() {
                self.restore_pending = None;
            }
            self.queue_retry = Some((Instant::now() + Duration::from_millis(300), 0));
        }
        // ¿Era la recarga (o un reintento) de una canción cortada por la red, y sigue sin red?
        let stalled_here = transient
            && matches!(reason, librespot_playback::player::LoadFailure::Network(_))
            && self.stall.as_ref().is_some_and(|s| s.is(&uri));
        if !stalled_here {
            // Otro motivo (o definitivo): ya no es un corte de red que esperar.
            self.stall = None;
        }
        if !transient {
            self.load_retry = None;
            self.playback_error = Some(PlaybackError::new(PlaybackErrorKind::Skipped, player_bar::skipped_text(&reason, name.as_deref())));
            return;
        }
        // Spirc la dejó en pausa, en su posición. Si la barra aún enseña la anterior (un
        // «siguiente» o el paso automático: `TrackChanged` llega al empezar a sonar), pasa a
        // enseñar esta, que es la que está en pausa y la que se reintenta.
        // La cortada por la red puede verse con otro id (relinking, `StallRecovery::alias`): es
        // la misma, no se cambia la barra.
        let same = |n: &NowPlaying| n.uri == uri || self.stall.as_ref().is_some_and(|s| s.is(&uri) && s.is(&n.uri));
        if self.player.now.as_ref().is_none_or(|n| !same(n)) {
            if let Some(track) = self.known_track(&uri).cloned() {
                self.player.now = Some(NowPlaying::from_track(&track));
                self.player.liked = track.id.as_ref().map(|id| self.liked_set.contains(id));
                self.player.audio = None;
                self.player.position_ms = 0;
                self.now_optimistic = true;
            }
        }
        // Spirc ya la tiene (en pausa): «reproducir» debe pedírsela a él, que la recarga en su
        // posición, y no empezar de cero la canción de la instantánea como si aún no hubiera nada.
        self.now_placeholder = false;
        self.player.state = PlayState::Paused;
        self.player.position_at = None;
        self.media_dirty = true;
        // Una carga en pausa (restaurar al abrir) no se reintenta sola: al abrir nunca suena
        // nada sin pedirlo. «Reproducir» la vuelve a cargar. Lo que iba a sonar (también lo que
        // se retomaba sonando tras una reconexión o una actualización) sí. El «reproducir
        // pendiente» de esa carga se consume aquí: si quedara puesto, la próxima pausa la
        // reanudaría sola.
        // Igual con «pausar al llegar» de la restauración: esa carga ya terminó, y si quedara
        // puesta la próxima canción que se pidiera se pausaría sola al empezar.
        let play_after_restore = std::mem::take(&mut self.play_after_restore);
        let pause_after_restore = std::mem::take(&mut self.pause_after_restore);
        let wants_play = (play && !pause_after_restore) || play_after_restore;
        if stalled_here {
            // Sigue sin red: se mantiene el aviso del corte y sus reintentos (cada uno vuelve a
            // cargar la canción en su segundo: Spirc la tiene como carga fallida).
            self.load_retry = None;
            let step = self.stall.as_mut().map(|s| s.on_failed_attempt(Instant::now()));
            self.apply_stall_step(step);
            return;
        }
        if !wants_play {
            self.load_retry = None;
            return;
        }
        let failures = match &self.load_retry {
            Some(r) if r.uri == uri => r.failures,
            _ => 0,
        }
        .saturating_add(1);
        match player_bar::load_retry_delay(failures) {
            Some(delay) => {
                log::info!("[reproducción] reintento automático en {} s (fallo {failures})", delay.as_secs());
                self.load_retry = Some(LoadRetry { uri, failures, at: Some(Instant::now() + delay) });
                self.playback_error = Some(PlaybackError::new(PlaybackErrorKind::Retrying, player_bar::retrying_text(&reason, name.as_deref())));
            }
            None => {
                // Sin más reintentos: parada, a la espera de [Reintentar] o [Saltar]. Spirc la
                // sigue teniendo en pausa, así que el botón de reproducir también la recarga.
                self.load_retry = None;
                self.player.state = PlayState::Stopped;
                self.playback_error = Some(PlaybackError::new(PlaybackErrorKind::Failed, player_bar::failed_text(name.as_deref())));
            }
        }
    }

    /// Algo suena: el aviso de un fallo (salvo el que solo informa de un salto, que se va solo) y
    /// el reintento pendiente sobran. Una canción cortada por la red vuelve a sonar: se recuerda
    /// un rato por si se corta otra vez enseguida (`watchdog::StallRecovery`).
    fn playback_recovered(&mut self) {
        self.load_retry = None;
        self.stuck_load = None;
        self.output_lost = None;
        if let Some(stall) = self.stall.as_mut() {
            stall.on_playing(Instant::now());
        }
        if self.playback_error.as_ref().is_some_and(|e| e.kind != player_bar::PlaybackErrorKind::Skipped) {
            self.playback_error = None;
        }
    }

    /// El usuario cambia de canción: lo pendiente de la anterior ya no aplica.
    fn forget_failed_load(&mut self) {
        self.load_retry = None;
        self.playback_error = None;
        self.stall = None;
        self.stuck_load = None;
        // Lo nuevo se vigila desde cero (sin heredar los segundos de la carga anterior).
        self.load_watch = None;
    }

    /// El usuario pausa: una canción cortada por la red ya no se reanuda sola, y el aviso que lo
    /// prometía sobra.
    fn forget_stall(&mut self) {
        if self.stall.take().is_some()
            && self.playback_error.as_ref().is_some_and(|e| e.kind == player_bar::PlaybackErrorKind::Stalled)
        {
            self.playback_error = None;
        }
    }

    /// La red se cortó a media canción (`Event::Stalled`): Spirc y el reproductor la tienen en
    /// pausa en `position_ms`, lo último que se oyó, sin saltar. Se reintenta sola (3, 6, 12 y
    /// luego cada 30 s); si tras dos intentos no llegan datos, o vuelve a sonar y se corta
    /// enseguida dos veces, se recarga entera en su segundo (el enlace del audio pudo caducar).
    fn on_stalled(&mut self, uri: String, position_ms: u32) {
        let now = Instant::now();
        log::warn!("[reproducción] se cortó la red en {uri} a {position_ms} ms; en pausa hasta que vuelva");
        self.player.state = PlayState::Paused;
        self.player.position_ms = position_ms;
        self.player.position_at = None;
        self.media_dirty = true;
        self.load_retry = None;
        let step = match self.stall.as_mut().filter(|s| s.is(&uri)) {
            Some(stall) => Some(stall.on_stalled(position_ms, now)),
            None => {
                // El corte es siempre de la que suena, que la barra puede enseñar con otro id
                // (relinking): se sigue con ese, y el pedido queda como alias. Antes no casaban
                // y el reintento automático se descartaba sin decir nada.
                let shown = self.player.now.as_ref().map(|n| n.uri.clone()).filter(|u| *u != uri && !self.now_optimistic);
                let stall = match shown {
                    Some(shown) => {
                        let mut s = watchdog::StallRecovery::new(shown, position_ms, now);
                        s.alias = Some(uri);
                        s
                    }
                    None => watchdog::StallRecovery::new(uri, position_ms, now),
                };
                let first = stall.due_in(now).map(watchdog::StallStep::Retry);
                self.stall = Some(stall);
                first
            }
        };
        self.apply_stall_step(step);
    }

    /// Lo que toca tras un corte o un intento fallido de reanudar (`watchdog::StallStep`).
    fn apply_stall_step(&mut self, step: Option<watchdog::StallStep>) {
        let Some(stall) = self.stall.as_ref() else { return };
        let name = self.known_track_name(&stall.uri);
        let pos = stall.position_ms;
        match step {
            Some(watchdog::StallStep::Retry(delay)) => {
                log::info!("[reproducción] se intentará seguir en {} s", delay.as_secs());
                self.player.state = PlayState::Paused;
                self.playback_error = Some(player_bar::PlaybackError::new(
                    player_bar::PlaybackErrorKind::Stalled,
                    player_bar::stalled_text(pos),
                ));
            }
            Some(watchdog::StallStep::Reload) => {
                // Con el enlace viejo los datos no volverían: Spirc la carga otra vez desde cero,
                // en su segundo y sonando (fichero y enlace nuevos). Sin pasar por la reconexión:
                // aquella vuelve a cargar el contexto (aleatorio nuevo) y a añadir la cola manual,
                // que en este Spirc sigue ahí y quedaría repetida.
                log::info!("[reproducción] tras el corte no llegan datos: se vuelve a cargar en {pos} ms");
                self.playback_error = Some(player_bar::PlaybackError::new(
                    player_bar::PlaybackErrorKind::Stalled,
                    player_bar::stalled_text(pos),
                ));
                self.load_watch = None;
                self.backend.send(Cmd::Reload);
                self.player.state = PlayState::Loading;
            }
            Some(watchdog::StallStep::GiveUp) | None => {
                // Demasiado sin red: se deja de intentar solo. Spirc la sigue teniendo en pausa,
                // así que [Reintentar] o reproducir siguen sirviendo.
                log::warn!("[reproducción] la red no vuelve: se deja de reintentar solo");
                self.stall = None;
                self.player.state = PlayState::Paused;
                self.playback_error = Some(player_bar::PlaybackError::new(
                    player_bar::PlaybackErrorKind::Failed,
                    player_bar::failed_text(name.as_deref()),
                ));
            }
        }
        self.media_dirty = true;
    }

    /// Reintento de una canción cortada por la red, cuando toca o con [Reintentar]: «reproducir»
    /// la reanuda desde su segundo si ya llegan datos; si no, llega otro `Stalled`.
    fn retry_stall_now(&mut self) {
        if let Some(stall) = self.stall.as_mut() {
            stall.at = None;
        }
        self.load_watch = None;
        self.backend.send(Cmd::Play);
        self.player.state = PlayState::Loading;
        self.media_dirty = true;
    }

    /// Reintentos de una canción cortada por la red y olvidarla cuando ya suena bien.
    fn tick_stall(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let Some(stall) = self.stall.as_ref() else { return };
        if stall.settled(now) {
            self.stall = None;
            return;
        }
        let Some(left) = stall.due_in(now) else { return };
        if !left.is_zero() {
            ctx.request_repaint_after(left);
            return;
        }
        let here = self.player.now.as_ref().is_some_and(|n| stall.is(&n.uri));
        let reconnecting = self.reconnect_resume.is_some() || !self.logged_in();
        let waiting = here && self.player.state == PlayState::Paused && self.player.remote.is_none();
        if reconnecting {
            // Reconectando: al volver se retoma sonando (`Reconnecting`); hasta entonces se espera.
            if let Some(stall) = self.stall.as_mut() {
                stall.at = Some(now + Duration::from_secs(5));
            }
            ctx.request_repaint_after(Duration::from_secs(5));
        } else if waiting {
            log::info!("[reproducción] se intenta seguir tras el corte de la red");
            self.retry_stall_now();
        } else if let Some(stall) = self.stall.as_mut() {
            // Ya no espera (se pidió a mano, suena otra cosa, otro dispositivo): no se insiste.
            stall.at = None;
        }
    }

    /// 30 s en «cargando» (`watchdog::GIVE_UP_AFTER`): se deja de esperar y se dice, con
    /// [Reintentar] (vuelve a pedir lo mismo) y [Saltar]. Si la carga termina más tarde, llega en
    /// pausa: nadie espera ya a que suene de golpe.
    fn give_up_loading(&mut self) {
        let name = self.player.now.as_ref().map(|n| n.name.clone());
        log::warn!("[vigilante] 30 s cargando: se deja de esperar ({})", name.as_deref().unwrap_or("?"));
        self.stuck_load = self.pending_load.take();
        // Lo que se retome al reconectar, en pausa; la reproducción pendiente tras restaurar, igual.
        if let Some(point) = self.reconnect_resume.as_mut() {
            point.playing = false;
        }
        self.play_after_restore = false;
        self.load_retry = None;
        self.stall = None;
        self.backend.send(Cmd::Pause);
        self.player.state = PlayState::Stopped;
        self.player.position_at = None;
        self.media_dirty = true;
        self.playback_error = Some(player_bar::PlaybackError::new(
            player_bar::PlaybackErrorKind::Stuck,
            player_bar::stuck_text(name.as_deref()),
        ));
    }

    /// [Reintentar] tras dejar de esperar una carga: se pide otra vez lo mismo (o, si mientras
    /// tanto llegó cargada en pausa o se está reconectando, que suene).
    fn retry_stuck_load(&mut self) {
        self.playback_error = None;
        self.load_watch = None;
        if let Some(point) = self.reconnect_resume.as_mut() {
            point.playing = true;
        } else if let Some(cmd) = self.stuck_load.take().filter(|_| self.player.state != PlayState::Paused) {
            self.pending_load = Some(cmd.clone());
            self.backend.send(cmd);
        } else {
            self.stuck_load = None;
            self.backend.send(Cmd::Play);
        }
        self.player.state = PlayState::Loading;
        self.media_dirty = true;
    }

    /// [Reintentar] del aviso, o el reintento automático: Spirc tiene la canción en pausa sin
    /// nada cargado y «reproducir» la vuelve a cargar en su posición.
    fn retry_failed_load(&mut self) {
        if let Some(r) = self.load_retry.as_mut() {
            // Los fallos siguen contando: si vuelve a fallar, toca el siguiente reintento.
            r.at = None;
        }
        self.load_watch = None;
        if self.playback_error.as_ref().is_some_and(|e| e.kind == player_bar::PlaybackErrorKind::Failed) {
            self.playback_error = None;
        }
        self.backend.send(Cmd::Play);
        self.player.state = PlayState::Loading;
        self.media_dirty = true;
    }

    /// [Saltar] del aviso: la siguiente, sonando. Spirc tiene la fallida en pausa, así que la
    /// siguiente cargaría en pausa: se pide reproducir detrás.
    fn skip_failed_load(&mut self) {
        self.forget_failed_load();
        self.next();
        self.backend.send(Cmd::Play);
        self.player.state = PlayState::Loading;
        self.media_dirty = true;
    }

    /// Lo pendiente de una canción que no se pudo cargar: el reintento automático cuando toca y
    /// quitar el aviso que solo informaba.
    fn tick_failed_load(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        if let Some(at) = self.load_retry.as_ref().and_then(|r| r.at) {
            if now < at {
                ctx.request_repaint_after(at - now);
            } else if self.reconnect_resume.is_some() || !self.logged_in() {
                // Reconectando: no hay reproductor al que pedírselo; se espera a que vuelva.
                if let Some(r) = self.load_retry.as_mut() {
                    r.at = Some(now + Duration::from_secs(5));
                }
                ctx.request_repaint_after(Duration::from_secs(5));
            } else if self.player.state == PlayState::Paused && self.player.remote.is_none() {
                log::info!("[reproducción] reintento automático de la canción que no se pudo cargar");
                self.retry_failed_load();
            } else if let Some(r) = self.load_retry.as_mut() {
                // Ya no está esperando (se pidió a mano o suena otra cosa): no se insiste.
                r.at = None;
            }
        }
        if self.playback_error.as_ref().is_some_and(|e| e.expired(now)) {
            self.playback_error = None;
        } else if let Some(left) = self.playback_error.as_ref().and_then(|e| e.remaining(now)) {
            ctx.request_repaint_after(left);
        }
    }

    /// Nombre de una canción que ya está en pantalla (la que suena, listas, álbumes, cola,
    /// búsqueda o historial), para los avisos.
    fn known_track_name(&self, uri: &str) -> Option<String> {
        if let Some(now) = self.player.now.as_ref().filter(|n| n.uri == uri) {
            return Some(now.name.clone());
        }
        self.known_track(uri).map(|t| t.name.clone())
    }

    /// Una canción que ya tenemos en pantalla (listas, álbumes, cola, búsqueda o historial).
    fn known_track(&self, uri: &str) -> Option<&Track> {
        let lists = self.lists.values().flat_map(|l| l.tracks.iter());
        let albums = self.albums.values().filter_map(|a| a.tracks.as_ref()).flat_map(|p| p.items.iter());
        let queue = self.queue.iter().flat_map(|q| q.queue.iter());
        let search = self.search_result.iter().filter_map(|r| r.tracks.as_ref()).flat_map(|p| p.items.iter());
        lists.chain(albums).chain(queue).chain(search).chain(self.recent.iter()).find(|x| x.uri == uri)
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
        // Se arma aquí (es lo de este instante); serializar y escribir, en el hilo del disco y en
        // orden: la del cierre no puede quedar debajo de una periódica anterior.
        let path = self.playback_path.clone();
        self.disk.run(move || {
            if let Ok(text) = serde_json::to_string(&saved) {
                crate::cache::write_atomic(&path, &text);
            }
        });
    }

    /// La carga de un contexto cuyas pistas ya tenemos (playlist, álbum, Me gusta) como lista de
    /// pistas sueltas, en la misma canción: Spirc no tiene que resolver el contexto en Spotify.
    /// `None` si no es un contexto, no tenemos sus pistas o la canción pedida no está entre ellas.
    /// Si lo que no llega es el contexto (context-resolve colgado: Spirc empezó la carga y nunca
    /// lo tuvo), la carga pendiente pasa a ser la lista de pistas sueltas, que no lo necesita: así
    /// se pide al reintentar y también tras reconectar. Solo si ya tenemos las pistas.
    fn skip_stuck_context(&mut self) {
        if self.jam.is_some() {
            return;
        }
        let suspect = librespot_core::ttfs::snapshot().is_some_and(|b| {
            b.outcome == librespot_core::ttfs::Outcome::Pending && watchdog::context_suspect(b.phases.iter().map(|p| p.name))
        });
        if !suspect {
            return;
        }
        if let Some(tracks) = self.pending_load.as_ref().and_then(|c| self.as_track_list(c)) {
            log::warn!("[vigilante] Spotify no devuelve el contexto: se pide como lista de pistas");
            self.pending_load = Some(tracks);
        }
    }

    /// La carga va lenta (2,5 s): si es de un álbum o una playlist cuyas pistas no tenemos (se
    /// pulsó desde una tarjeta, sin abrir su página), se piden ya por los metadatos, que no pasan
    /// por context-resolve. Así, si lo que no llega es el contexto, a los 8 s el vigilante puede
    /// pedir la carga como lista de pistas (`as_track_list`) en vez de repetir la misma espera.
    fn prefetch_pending_context(&mut self) {
        if self.jam.is_some() {
            return;
        }
        let Some(cmd) = self.pending_load.as_ref() else { return };
        let Cmd::LoadContext { uri, .. } = cmd else { return };
        if self.as_track_list(cmd).is_some() {
            return;
        }
        let Some((key, kind)) = ctx_list_key(uri) else { return };
        match kind {
            CtxKind::Playlist => self.load_playlist(&key, true, false),
            // La misma clave que la página: una sola petición entre todos.
            CtxKind::Album => self.request_once(&format!("album:{key}"), Req::Album(key)),
            CtxKind::Liked => {}
        }
    }

    fn as_track_list(&self, cmd: &Cmd) -> Option<Cmd> {
        let Cmd::LoadContext { uri, track_uri, index, shuffle, resume, first } = cmd else { return None };
        // La misma correspondencia contexto → lista que la cola del contexto (también «Me
        // gusta» como `spotify:user:…:collection`; emisoras y artistas no tienen lista propia).
        let (key, kind) = ctx_list_key(uri)?;
        let tracks = match kind {
            CtxKind::Album => self.albums.get(&key).and_then(|a| a.tracks.as_ref()).map(|p| &p.items),
            CtxKind::Playlist | CtxKind::Liked => self.lists.get(&key).map(|l| &l.tracks),
        }?;
        let uris: Vec<String> = tracks.iter().map(|t| t.uri.clone()).collect();
        let index = match (track_uri, index) {
            (Some(t), _) => Some(uris.iter().position(|u| u == t)? as u32),
            // Un índice fuera de lo que tenemos empezaría por otra canción: mejor no convertir.
            (None, Some(i)) if (*i as usize) >= uris.len() => return None,
            (None, Some(i)) => Some(*i),
            (None, None) => None,
        };
        (!uris.is_empty()).then_some(Cmd::LoadTracks { uris, index, shuffle: *shuffle, resume: *resume, first: first.clone() })
    }

    /// La canción con la que empezará `t`, si ya la tenemos en pantalla (lista, álbum, cola,
    /// búsqueda o historial). Con aleatorio no se sabe cuál será.
    fn target_track(&self, t: &PlayTarget) -> Option<Track> {
        let uri = match t {
            PlayTarget::Context { track_uri: Some(u), .. } => u.clone(),
            PlayTarget::Context { shuffle: true, .. } | PlayTarget::Tracks { shuffle: true, .. } => return None,
            PlayTarget::Context { uri, index, .. } => {
                let i = index.unwrap_or(0) as usize;
                let tracks = match uri.split(':').collect::<Vec<_>>()[..] {
                    ["spotify", "playlist", id] => self.lists.get(id).map(|l| &l.tracks),
                    ["spotify", "album", id] => self.albums.get(id).and_then(|a| a.tracks.as_ref()).map(|p| &p.items),
                    ["spotify", "collection", ..] => self.lists.get(LIKED).map(|l| &l.tracks),
                    _ => None,
                };
                return tracks.and_then(|v| v.get(i)).cloned();
            }
            PlayTarget::Tracks { uris, index, .. } => uris.get(index.unwrap_or(0) as usize)?.clone(),
        };
        self.known_track(&uri).cloned()
    }

    /// ¿Se puede precargar ahora? Con el ajuste puesto, una cuenta Premium conectada, sonando aquí
    /// (no en otro dispositivo ni en una Jam, donde la canción la decide otro) y sin una carga en
    /// curso, a la que no hay que quitarle red ni cupo.
    fn warm_allowed(&self) -> bool {
        self.settings.smart_preload
            && self.logged_in()
            && !self.not_premium
            && self.player.remote.is_none()
            && self.jam.is_none()
            && self.player.state != PlayState::Loading
    }

    /// Una fila de canción tiene el ratón encima en este fotograma (`press`: además, su botón de
    /// reproducir está apretado). Lo recoge `tick_warm`.
    pub fn report_row_hover(&mut self, uri: &str, press: bool) {
        if !warm::warmable(uri) {
            return;
        }
        self.warm_hover = Some(uri.to_string());
        if press {
            self.warm_press = Some(uri.to_string());
        }
    }

    /// Precarga inteligente, al final de cada fotograma: lo que vieron las filas (`report_row_hover`)
    /// decide si se prepara una canción (`warm::WarmTracker`).
    fn tick_warm(&mut self, ctx: &egui::Context) {
        let hover = self.warm_hover.take();
        let press = self.warm_press.take();
        let (hover, press) = if self.warm_allowed() { (hover, press) } else { (None, None) };
        let step = self.warm.frame(hover.as_deref(), press.as_deref(), Instant::now());
        if let Some(uri) = step.press {
            log::debug!("[precarga] botón apretado: {uri}");
            self.backend.send(Cmd::WarmHead(uri));
        }
        if let Some(uri) = step.hover {
            self.backend.send(Cmd::Warm(uri));
        }
        if let Some(wait) = step.wait {
            // Con el ratón quieto no llegan eventos: sin esto no se repintaría a los 150 ms.
            ctx.request_repaint_after(wait);
        }
    }

    /// Precarga de los metadatos de las primeras canciones de una búsqueda (un solo lote): la que
    /// se pulse ya no los pide.
    fn warm_search(&mut self, s: &SearchResult) {
        if !self.warm_allowed() {
            return;
        }
        let uris: Vec<String> = s
            .tracks
            .iter()
            .flat_map(|p| p.items.iter())
            .map(|t| t.uri.clone())
            .filter(|u| u.starts_with("spotify:track:"))
            .take(crate::api::WARM_META_MAX)
            .collect();
        if !uris.is_empty() {
            self.api.send_bg(Req::WarmMeta(uris));
        }
    }

    /// La canción con la que empezará `t` según Spirc, aunque no esté en pantalla: la pulsada, la
    /// de su posición en la lista o la primera. `None` si no se sabe (aleatorio sin una elegida).
    fn first_uri(&self, t: &PlayTarget) -> Option<String> {
        match t {
            PlayTarget::Context { track_uri: Some(u), .. } => Some(u.clone()),
            PlayTarget::Context { .. } => self.target_track(t).map(|track| track.uri),
            // Con aleatorio, Spirc empieza igualmente por la elegida (`index`).
            PlayTarget::Tracks { uris, index: Some(i), .. } => uris.get(*i as usize).cloned(),
            PlayTarget::Tracks { shuffle: true, .. } => None,
            PlayTarget::Tracks { uris, .. } => uris.first().cloned(),
        }
    }

    pub fn play(&mut self, t: PlayTarget) {
        self.play_after_restore = false;
        if !self.signed_in() {
            self.status_err("Inicia sesión para reproducir música");
            return;
        }
        if self.not_premium && self.player.remote.is_none() {
            self.show_not_premium();
            return;
        }
        self.last_play = Some(t.clone());
        self.last_play_page = Some(self.page().clone());
        self.note_library_play(&t);
        // Algo nuevo que reproducir: lo de la canción que no se pudo cargar ya no aplica.
        self.forget_failed_load();
        self.restore_wanted = false;
        self.restore_pending = None;
        self.restore_deadline = None;
        // Lo que suena ahora trae su propia cola: la del contexto restaurado ya no corresponde y,
        // si llegara después, la pisaría.
        self.abandon_restore_ctx();
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
            // Carga en paralelo: el reproductor empieza ya con esta canción mientras Spirc resuelve
            // el contexto (ver `Cmd::LoadContext::first`). En una Jam no: la canción la decide la
            // sesión compartida.
            let first = self.first_uri(&t).filter(|_| self.jam.is_none());
            if let Some(track) = self.target_track(&t) {
                // La barra muestra ya la canción elegida; el reproductor la confirma al cargarla.
                self.player.now = Some(NowPlaying::from_track(&track));
                self.player.liked = track.id.as_ref().map(|id| self.liked_set.contains(id));
                // La etiqueta de calidad era de la canción anterior.
                self.player.audio = None;
                self.player.position_ms = 0;
                self.player.position_at = None;
                self.now_optimistic = true;
                self.now_placeholder = false;
                self.media_dirty = true;
            }
            let cmd = match t {
                PlayTarget::Context {
                    uri,
                    track_uri,
                    index,
                    shuffle,
                } => Cmd::LoadContext {
                    uri,
                    track_uri,
                    index,
                    shuffle,
                    resume: None,
                    first,
                },
                PlayTarget::Tracks {
                    uris,
                    index,
                    shuffle,
                } => Cmd::LoadTracks {
                    uris,
                    index,
                    shuffle,
                    resume: None,
                    first,
                },
            };
            // Lo recién pedido manda sobre lo que sonaba si la conexión se cae antes de empezar.
            self.pending_load = Some(cmd.clone());
            self.reconnect_resume = None;
            ttfs_begin("play");
            self.backend.send(cmd);
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

    /// El aviso de cuenta sin Premium (al intentar reproducir aquí).
    fn show_not_premium(&mut self) {
        self.playback_error = Some(player_bar::PlaybackError::new(
            player_bar::PlaybackErrorKind::NotPremium,
            player_bar::NOT_PREMIUM_TEXT.to_string(),
        ));
    }

    /// ¿Hay aquí una sesión del DJ cargada (sonando o en pausa)?
    pub fn dj_active(&self) -> bool {
        self.player.remote.is_none()
            && self.player.state != PlayState::Stopped
            && matches!(&self.last_play, Some(PlayTarget::Context { uri, .. }) if uri == DJ_URI)
    }

    /// El botón del DJ, como en Spotify: sin el DJ, lo pone a sonar (el locutor se presenta); con
    /// el DJ ya aquí, salta al siguiente bloque de la sesión, que el locutor presenta («Elige tú»).
    pub fn dj_button(&mut self) {
        if self.dj_active() {
            if self.player.state == PlayState::Paused {
                self.backend.send(Cmd::Play);
            }
            self.backend.send(Cmd::DjJump);
        } else {
            self.actions.push(Action::Play(PlayTarget::Context {
                uri: DJ_URI.to_string(),
                track_uri: None,
                index: None,
                shuffle: false,
            }));
        }
    }

    pub fn play_pause(&mut self) {
        self.pause_after_restore = false;
        if self.not_premium && self.player.remote.is_none() && self.player.state != PlayState::Playing {
            self.show_not_premium();
            return;
        }
        if let Some(point) = self.reconnect_resume.as_mut() {
            // Reconectando: no hay reproductor al que mandar la orden; se decide cómo se
            // retomará (sonando o en pausa) y la barra lo refleja ya.
            point.playing = !point.playing;
            self.player.state = if point.playing { PlayState::Loading } else { PlayState::Paused };
            self.media_dirty = true;
            return;
        }
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
                PlayState::Playing => {
                    ttfs_begin("pause");
                    self.forget_stall();
                    self.backend.send(Cmd::Pause)
                }
                PlayState::Paused => {
                    ttfs_begin("resume");
                    self.load_watch = None;
                    // Reanudar a mano una canción cortada por la red es su reintento: el
                    // automático ya no hace falta (si aún no hay datos, llega otro corte).
                    if let Some(stall) = self.stall.as_mut() {
                        stall.at = None;
                    }
                    self.backend.send(Cmd::Play)
                }
                _ => {
                    // Cargando: lo normal es que esto la pause (también el intento de seguir
                    // tras un corte de red, que entonces ya no se reanuda solo).
                    self.forget_stall();
                    self.backend.send(Cmd::PlayPause)
                }
            }
        }
    }

    pub fn next(&mut self) {
        // Otra canción: el aviso y el reintento de la que no se pudo cargar ya no aplican.
        let resume = self.failed_load_wanted_play();
        self.forget_failed_load();
        if self.player.remote.is_some() {
            self.api.send(Req::RemoteNext);
            self.last_remote_poll = Instant::now() - Duration::from_millis(2200);
        } else {
            ttfs_begin("next");
            self.backend.send(Cmd::Next { auto: false });
            self.resume_after_failed_load(resume);
        }
    }

    pub fn prev(&mut self) {
        let resume = self.failed_load_wanted_play();
        self.forget_failed_load();
        if self.player.remote.is_some() {
            self.api.send(Req::RemotePrev);
            self.last_remote_poll = Instant::now() - Duration::from_millis(2200);
        } else {
            ttfs_begin("prev");
            self.backend.send(Cmd::Prev);
            self.resume_after_failed_load(resume);
        }
    }

    /// ¿Hay una canción que iba a sonar y no se pudo cargar por algo pasajero (en espera de
    /// reintento o ya sin reintentos)? Spirc la tiene en pausa aunque nadie la pausó.
    fn failed_load_wanted_play(&self) -> bool {
        self.load_retry.is_some()
            || self.stall.is_some()
            || self.playback_error.as_ref().is_some_and(|e| e.kind == player_bar::PlaybackErrorKind::Failed)
    }

    /// Tras «siguiente» o «anterior» desde una carga fallida que iba a sonar: Spirc cargaría la
    /// otra en pausa (la fallida quedó en pausa) y, con varios «siguiente» seguidos, la música
    /// acabaría parada sin que nadie la parase. Se pide reproducir detrás, como con [Saltar].
    fn resume_after_failed_load(&mut self, resume: bool) {
        if resume {
            self.backend.send(Cmd::Play);
            self.player.state = PlayState::Loading;
            self.media_dirty = true;
        }
    }

    pub fn seek(&mut self, ms: u32) {
        self.player.position_ms = ms;
        self.player.position_at = (self.player.state == PlayState::Playing).then(Instant::now);
        if self.player.remote.is_some() {
            self.api.send(Req::RemoteSeek(ms));
        } else if let Some(point) = self.reconnect_resume.as_mut() {
            // Reconectando: se retomará desde aquí.
            point.pos = ms;
        } else {
            ttfs_begin("seek");
            self.backend.send(Cmd::Seek(ms));
            // Cortada por la red: seguirá desde el punto nuevo (el reproductor lo apunta sin
            // esperar a la red), y el aviso lo dice.
            if let Some(stall) = self.stall.as_mut() {
                stall.position_ms = ms;
                if self.playback_error.as_ref().is_some_and(|e| e.kind == player_bar::PlaybackErrorKind::Stalled) {
                    self.playback_error = Some(player_bar::PlaybackError::new(
                        player_bar::PlaybackErrorKind::Stalled,
                        player_bar::stalled_text(ms),
                    ));
                }
            }
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
        // El orden de lo siguiente cambia: la cola abierta se pone al día sin cerrarla.
        self.queue_refresh_soon();
    }

    /// Pide la cola otra vez dentro de un momento y algo después (Spotify tarda en reflejar un
    /// cambio de orden), si el panel de la cola está abierto.
    pub fn queue_refresh_soon(&mut self) {
        let now = Instant::now();
        self.queue_refresh = vec![now + Duration::from_millis(700), now + Duration::from_millis(2500)];
    }

    /// Mueve la siguiente canción `uri` delante de `before` o, sin él, detrás de `after` (asa de
    /// la cola). Se ve al instante; Spotify la confirma en la siguiente consulta.
    pub fn queue_move(&mut self, uri: String, before: Option<String>, after: Option<String>) {
        if self.player.remote.is_some() {
            self.status_err("Para reordenar la cola, la música tiene que sonar en Nanofy");
            return;
        }
        let place = |list: &mut Vec<String>| {
            if let Some(from) = list.iter().position(|u| *u == uri) {
                let u = list.remove(from);
                let to = match (&before, &after) {
                    (Some(b), _) => list.iter().position(|x| x == b),
                    (None, Some(a)) => list.iter().position(|x| x == a).map(|i| i + 1),
                    _ => None,
                }
                .unwrap_or(from)
                .min(list.len());
                list.insert(to, u);
            }
        };
        if let Some(q) = self.queue.as_mut() {
            let mut uris: Vec<String> = q.queue.iter().map(|t| t.uri.clone()).collect();
            place(&mut uris);
            let mut rest = std::mem::take(&mut q.queue);
            for u in &uris {
                if let Some(i) = rest.iter().position(|t| &t.uri == u) {
                    q.queue.push(rest.remove(i));
                }
            }
        }
        place(&mut self.queued_local);
        self.backend.send(Cmd::MoveNext { uri, before, after });
        self.queue_refresh_soon();
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
        self.queue_refresh_soon();
    }

    pub fn select_device(&mut self, d: Device, is_self: bool) {
        if is_self {
            self.backend.send(Cmd::TransferHere);
            self.player.remote = None;
            self.status("Trayendo la reproducción a este equipo…");
        } else if let Some(id) = d.id.clone() {
            self.api.send(Req::Transfer { device_id: id });
            self.status(format!("Reproduciendo en {}", d.name));
            // `local_audio` ya oculta la etiqueta; si el traspaso falla y sigue sonando aquí, vuelve.
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
        self.search_for(q);
    }

    /// Busca `q` (ya sin espacios a los lados): lo escrito en la caja o la consulta que quedó
    /// pendiente en Perfiles al pasar a otra pestaña de resultados.
    fn search_for(&mut self, q: String) {
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
        if self.search_filter == 8 {
            // Perfiles: Spotify no busca usuarios; se prueba el nombre de usuario tal cual (por
            // spclient, sin cuota) si no se tiene ya. La búsqueda general no sale: aquí no se
            // ve, y se pide al pasar a otra pestaña (set_search_filter).
            if !self.users.contains_key(&q) {
                self.api.send(Req::User(q.clone()));
            }
            // Lo que siga en vuelo es de otra consulta y ya no debe sustituir a nada (si es de
            // esta, su respuesta valdrá al pasar a otra pestaña).
            if self.search_pending.as_deref() != Some(q.as_str()) {
                self.search_loading = false;
                self.search_pending = None;
                self.search_retry = None;
            }
            self.search_pending_profile = Some(q);
            self.go(Page::Search);
            return;
        }
        self.search_pending_profile = None;
        // Resultado reciente de esta consulta: a la vista al instante, sin «Buscando» ni petición.
        if let Some(c) = self.search_cache.get_mut(&search_key(&q)) {
            let age = c.at.elapsed();
            if age < SEARCH_KEEP {
                let now = Instant::now();
                c.used = now;
                // Si la de esta consulta ya está en vuelo (y no esperando un reintento tras un
                // 429), su respuesta sirve de refresco. Si no, pasada la media hora se pide otra
                // detrás, como mucho una cada SEARCH_REFRESH_GAP.
                let inflight = self.search_pending.as_deref() == Some(q.as_str()) && self.search_retry.is_none();
                let refresh = !inflight && age >= SEARCH_FRESH && c.asked.is_none_or(|t| t.elapsed() >= SEARCH_REFRESH_GAP);
                if refresh {
                    c.asked = Some(now);
                }
                self.search_result = Some(c.result.clone());
                self.search_result_for = Some(q.clone());
                self.search_loading = false;
                self.search_retry = None;
                self.search_retries = 0;
                log::info!("búsqueda «{q}» desde la caché (de hace {} min){}", age.as_secs() / 60, if refresh { "; se refresca detrás" } else { "" });
                if refresh {
                    // El carril de búsquedas descarta sin respuesta las que siguen en cola al
                    // llegar otra: un refresco anterior podría no contestar nunca y quedarse
                    // marcado (y callaría los errores de esa consulta para siempre).
                    self.search_refreshing.clear();
                }
                if inflight || refresh {
                    self.search_refreshing.insert(q.clone());
                    self.search_pending = Some(q.clone());
                } else {
                    // Lo que siga en vuelo es de otra consulta: al llegar se guarda, pero ya no
                    // sustituye a lo que se ve.
                    self.search_pending = None;
                }
                if refresh {
                    self.api.send(Req::Search(q));
                }
                self.go(Page::Search);
                return;
            }
        }
        // Los resultados anteriores no se borran: siguen a la vista, atenuados, hasta que
        // llega la respuesta de esta consulta.
        self.search_loading = true;
        // Esta vez la espera ella: si falla, se avisa y se reintenta como cualquier búsqueda. Los
        // refrescos de otras consultas también se desmarcan: el carril «gana la última» puede
        // descartarlos sin respuesta, y si contestan basta con guardarlos (no son la pendiente).
        self.search_refreshing.clear();
        // Intro de nuevo con la misma consulta aún en vuelo (o esperando su reintento tras un
        // 429): su respuesta se aceptará igual, así que repetirla solo gastaría cuota. Tras un
        // error pending ya está vacío y se reintenta.
        if self.search_pending.as_deref() == Some(q.as_str()) {
            self.go(Page::Search);
            return;
        }
        // Una consulta nueva anula el reintento programado de la anterior.
        self.search_retry = None;
        self.search_retries = 0;
        self.search_pending = Some(q.clone());
        self.api.send(Req::Search(q));
        self.go(Page::Search);
    }

    /// Guarda la respuesta de `q` en la caché de búsquedas (ver `search_cache`).
    fn cache_search(&mut self, q: &str, s: SearchResult) {
        // Una respuesta que llega tras cerrar sesión no entra en la caché de la sesión siguiente.
        if search_is_empty(&s) || !self.signed_in() {
            return;
        }
        let now = Instant::now();
        self.search_cache.insert(search_key(q), CachedSearch { at: now, used: now, asked: None, result: s });
        self.search_cache.retain(|_, c| c.at.elapsed() < SEARCH_KEEP);
        while self.search_cache.len() > SEARCH_CACHE_MAX {
            let Some(old) = self.search_cache.iter().min_by_key(|(_, c)| c.used).map(|(k, _)| k.clone()) else {
                break;
            };
            self.search_cache.remove(&old);
        }
    }

    /// Cambia la pestaña de resultados (las píldoras de la página y el modo de control).
    pub fn set_search_filter(&mut self, f: u8) {
        self.search_filter = f;
        if f == 8 {
            // Perfiles: el nombre de usuario tal cual, solo si no se tiene ya (antes se repetía
            // en cada clic).
            let q = self.search_query.trim();
            if !q.is_empty() && !self.users.contains_key(q) {
                self.api.send(Req::User(q.to_string()));
            }
        } else if self.logged_in() {
            // La consulta lanzada en Perfiles no buscó nada más: se busca ahora que hace falta
            // (o sale de la caché). Aún conectando se deja para el próximo clic.
            if let Some(q) = self.search_pending_profile.take() {
                self.search_for(q);
            }
        }
    }

    pub fn save_settings(&mut self, ctx: &egui::Context) {
        let restart = self.settings.restart_differs(&self.draft);
        let audio = self.settings.audio_differs(&self.draft);
        let quality_changed = self.settings.quality != self.draft.quality;
        let volume_changed = self.settings.normalisation != self.draft.normalisation
            || self.settings.loudness != self.draft.loudness;
        let media_changed = self.settings.media_keys != self.draft.media_keys;
        let autoplay_changed = self.settings.autoplay != self.draft.autoplay;
        self.draft.zoom = self.draft.zoom.clamp(0.7, 2.0);
        self.draft.fps_cap = self.draft.fps_cap.clamp(30, 480);
        self.draft.crossfade_secs = self.draft.crossfade_secs.clamp(crate::config::CROSSFADE_SECS_MIN, crate::config::CROSSFADE_SECS_MAX);
        // El interruptor y el deslizador ya lo aplican al instante (`set_crossfade`); esto cubre
        // el borrador cambiado por otra vía (el modo de control parchea ajustes y guarda).
        let crossfade_changed = self.settings.crossfade_differs(&self.draft);
        if quality_changed {
            // Lo preparado por la precarga inteligente era el fichero de la calidad anterior.
            self.warm.forget();
        }
        // Lo que se cambia desde la biblioteca (fijadas, vista, orden) no pasa por el borrador:
        // un borrador abierto antes de fijar algo no debe deshacerlo al guardar.
        self.draft.pinned = self.settings.pinned.clone();
        self.draft.liked_unpinned = self.settings.liked_unpinned;
        self.draft.library_grid = self.settings.library_grid;
        self.draft.library_oldest = self.settings.library_oldest;
        self.draft.library_grouped = self.settings.library_grouped;
        self.draft.library_kind = self.settings.library_kind;
        self.draft.lyrics_sync = self.settings.lyrics_sync;
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
            // El reinicio arranca con todos los ajustes nuevos, también los de audio.
            self.backend.send(Cmd::Restart(self.settings.clone()));
            self.status("Ajustes guardados");
        } else if audio || restart {
            // Calidad, gapless y volumen se aplican sin cortar la música. Sin sesión (o
            // reconectando) el backend solo guarda la copia, y la conexión siguiente ya la usa.
            self.backend.send(Cmd::AudioTuning(self.settings.clone()));
            self.status(match (quality_changed, volume_changed) {
                (true, true) => "Calidad aplicada desde la próxima canción que se cargue; el volumen, al instante",
                (true, false) => "Calidad aplicada desde la próxima canción que se cargue",
                (false, true) => "Volumen aplicado al instante",
                (false, false) => "Ajustes guardados",
            });
        } else {
            self.status("Ajustes guardados");
        }
        // Tras un `Restart` también: el backend ya crea el reproductor nuevo con el fundido en
        // vigor, pero así queda garantizado por el orden de las órdenes aunque eso cambie (con el
        // temporizador «al terminar la canción» puesto debe seguir a 0).
        if crossfade_changed || (restart && self.logged_in()) {
            self.sync_crossfade();
        }
        if autoplay_changed {
            self.backend.send(Cmd::Autoplay(self.settings.autoplay));
        }
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
            // Desde cualquier página abre Búsqueda con el cursor en la caja. go() ya enfoca al
            // cambiar de página, pero vuelve sin hacer nada si Búsqueda ya está abierta.
            Shortcut::Search => {
                self.focus_search = true;
                self.go(Page::Search);
            }
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
                self.notes_dialog = None;
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
        self.frames_painted += 1;
        let ctx = ui.ctx().clone();
        self.drain(&ctx);
        self.tick(&ctx);
        self.shortcuts(&ctx);
        self.handle_dropped_files(&ctx);

        let p = theme::palette(&ctx);
        let bg = p.bg;

        if self.miniplayer {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(bg).inner_margin(egui::Margin::same(6)))
                .show(ui, |ui| self.player_bar(ui));
            self.overlays(&ctx);
            self.apply_actions(&ctx);
            self.tick_warm(&ctx);
            self.images.end_frame();
            return;
        }

        // Barra superior a todo el ancho (59 px, como en la referencia de diseño).
        egui::Panel::top("topbar")
            .exact_size(TOPBAR_H)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::new().fill(bg).inner_margin(Margin::ZERO))
            .show(ui, |ui| self.top_bar(ui));

        // Biblioteca a la izquierda, de arriba abajo: el reproductor va solo bajo el contenido.
        if self.settings.sidebar_visible {
            egui::Panel::left("sidebar")
                .exact_size(SIDEBAR_W)
                .resizable(false)
                .show_separator_line(false)
                .frame(Frame::new().fill(bg).inner_margin(Margin::ZERO))
                .show(ui, |ui| self.sidebar(ui));
        }

        // Reproductor bajo el contenido: 5 px por encima, 6 a la derecha y 10 por debajo.
        egui::Panel::bottom("player")
            .exact_size(PLAYER_PANEL_H)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::new().fill(bg).inner_margin(Margin {
                left: if self.settings.sidebar_visible { 0 } else { 6 },
                right: 6,
                top: 5,
                bottom: 10,
            }))
            .show(ui, |ui| self.player_bar(ui));

        // La cola y la letra van en sus paneles de cristal sobre el contenido (abajo).
        let page_key = format!("{:?}", self.page());
        let tint = if p.dark { self.page_tint() } else { None };
        let mut content: Option<egui::Rect> = None;
        egui::CentralPanel::default()
            .frame(Frame::new().fill(bg).inner_margin(Margin {
                left: if self.settings.sidebar_visible { 0 } else { 6 },
                right: 6,
                top: 0,
                bottom: 0,
            }))
            .show(ui, |ui| {
                // Panel de contenido con esquinas de 8 px. En playlists y álbumes, degradado del
                // tono de la portada (plano los primeros 26 px) hasta el fondo de la ventana.
                let panel = ui.max_rect();
                content = Some(panel);
                // Inicio y la página de un artista van sobre el fondo de la ventana con un filo de
                // 1 px, como en la referencia; la del artista, de borde a borde (su cabecera ocupa
                // el panel entero).
                let page_now = self.page().clone();
                let library = page_now == Page::Library && self.signed_in();
                let black = library || matches!(page_now, Page::Artist(_) | Page::Home | Page::Search);
                if black && p.dark {
                    ui.painter().rect_filled(panel, egui::CornerRadius::same(8), bg);
                    ui.painter().rect_stroke(panel, egui::CornerRadius::same(8), egui::Stroke::new(1.0, ARTIST_PANEL_EDGE), egui::StrokeKind::Inside);
                } else {
                    paint_content_panel(ui.painter(), panel, tint, p.card, bg);
                }
                let (pad_l, pad_r, pad_t) = match page_now {
                    Page::Artist(_) => (0.0, 0.0, 0.0),
                    Page::Home | Page::Search => (HOME_PAD_LEFT, HOME_PAD_RIGHT, HOME_PAD_TOP),
                    _ => (CONTENT_PAD_LEFT, CONTENT_PAD_RIGHT, CONTENT_PAD_TOP),
                };
                if library {
                    // La biblioteca lleva su barra fija y desplaza solo lo de debajo.
                    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(panel));
                    c.set_clip_rect(panel.shrink(1.0));
                    self.library_panel(&mut c, panel);
                    return;
                }
                let inner = egui::Rect::from_min_max(
                    egui::pos2(panel.min.x + pad_l, panel.min.y + pad_t),
                    egui::pos2(panel.max.x - pad_r, panel.max.y),
                );
                let mut c = ui.new_child(egui::UiBuilder::new().max_rect(inner));
                c.set_clip_rect(panel.shrink(1.0));
                egui::ScrollArea::vertical()
                    .id_salt(page_key)
                    .auto_shrink([false, false])
                    .show(&mut c, |ui| {
                        self.page_ui(ui);
                        ui.add_space(24.0);
                    });
            });

        match (self.side, content) {
            (Some(SideTab::Queue), Some(c)) => self.queue_panel(&ctx, c),
            (Some(SideTab::Lyrics), Some(c)) => self.lyrics_float(&ctx, c),
            _ => {}
        }
        // Encima de la barra del reproductor (no en el miniplayer, que es solo la barra).
        self.playback_error_banner(&ctx);
        self.overlays(&ctx);
        self.apply_actions(&ctx);
        self.tick_warm(&ctx);
        self.images.end_frame();
    }

    fn on_exit(&mut self) {
        // Lo primero: desde aquí ninguna sustitución del ejecutable empieza a renombrar (si hay
        // una en curso, se espera a que termine el renombrado, milisegundos). El proceso termina
        // enseguida y no debe quedarse entre apartar el actual y poner el nuevo.
        crate::update::close_swap_gate();
        // Cerrar con normalidad tras pintar la ventana: si esta versión está a prueba, arranca.
        if !self.update_applying && self.restart_after_exit.is_none() && crate::FIRST_FRAME_MS.get().is_some() {
            crate::update::confirm_on_exit();
        }
        self.save_playback();
        if !self.ephemeral {
            self.settings.volume = vol_raw_to_pct(self.player.volume).round() as u8;
            self.settings.lyrics_open = self.side == Some(SideTab::Lyrics);
            self.settings.save(&self.paths);
        }
        // La versión nueva ya ocupa su sitio: se abre ya, antes de la despedida a Spotify y de
        // esperar al disco, para que el vigilante de salida de 3 s (shell.rs) no pueda cortar el
        // cierre antes de abrirla. Con --wait-pid espera a que este proceso termine antes de leer
        // nada (ajustes, la copia de la reproducción que save_playback acaba de encolar) o de
        // conectarse a Spotify. Mismos argumentos (p. ej. --control) para que arranque igual, en la
        // página en la que estaba el usuario y, si sonaba música, retomándola. Si no arranca, el
        // acceso directo del usuario ya abre la nueva la próxima vez. Con una versión lista y sin
        // «Reiniciar» no se hace nada aquí: se instala al abrir.
        if let Some(target) = self.restart_after_exit.take() {
            let args: Vec<String> = std::env::args().skip(1).collect();
            let args = crate::update::restart_args(&args, self.restart_page().as_deref());
            let mut extra = vec!["--updated-from".to_string(), crate::update::current_version()];
            if self.restart_resume {
                extra.push("--resume-playing".to_string());
            }
            if let Err(e) = crate::update::spawn_target(&target, &args, &extra) {
                log::error!("[update] no se pudo abrir {}: {e}", target.display());
            }
        }
        // El backend empieza a desconectar (avisa a Spotify de la pausa y del dispositivo
        // inactivo) mientras se escriben los ficheros. Van al hilo del disco detrás de lo que
        // ya tuviera encolado y se espera a que termine: así ninguna escritura periódica
        // anterior queda encima de la última, y el process::exit no las corta a medias.
        self.backend.send(Cmd::Shutdown);
        if self.snapshot_dirty && self.logged_in() {
            self.snapshot_dirty = false;
            self.snapshot().save_async(&self.disk, self.snapshot_path.clone());
        }
        if self.play_log_dirty {
            self.play_log_dirty = false;
            self.play_log.clone().save_async(&self.disk, self.play_log_path.clone());
        }
        self.disk.finish();
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
