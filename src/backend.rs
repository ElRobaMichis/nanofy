//! Reproducción: sesión de librespot, dispositivo Spotify Connect y reproductor local.
//!
//! Corre en un runtime de tokio con dos hilos. La interfaz envía `Cmd` y recibe `Event`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use librespot_connect::{
    ConnectConfig, LoadContextOptions, LoadRequest, LoadRequestOptions, Options as ContextOptions,
    PlayingTrack, Spirc,
};
use librespot_core::{
    authentication::Credentials,
    cache::Cache,
    config::{DeviceType, SessionConfig},
    Session,
};
use librespot_metadata::audio::UniqueFields;
use librespot_playback::{
    audio_backend::{self, Sink, SinkResult},
    convert::Converter,
    decoder::AudioPacket,
    config::{AudioFormat, AudioTuning, NormalisationMethod, NormalisationType, PlayerConfig},
    config::VolumeCtrl,
    mixer::{self, MixerConfig, NoOpVolume},
    player::{duration_to_coefficient, AudioSource, LoadFailure, Player, PlayerEvent, WarmStep},
};
use tokio::sync::mpsc;

use crate::bus::{Msg, UiTx};
use crate::config::{vol_pct_to_raw, Loudness, Paths, Settings};
use crate::model::{AudioInfo, NowPlaying};

/// Tiempo en pausa tras el que se suelta el dispositivo de audio (ver `start`).
const RELEASE_AUDIO_AFTER_PAUSE: Duration = Duration::from_secs(5);

/// Cada cuánto se mira la salud de la conexión (sesión invalidada, equipo que despierta).
const HEALTH_EVERY: Duration = Duration::from_secs(30);
/// Entre dos vistazos de salud pasó al menos esto: el equipo estuvo dormido (ver `slept`).
const SLEEP_GAP: Duration = Duration::from_secs(45);
/// Lo que se espera a que Spirc conteste un ping antes de darlo por atascado (`Cmd::RetryLoad`).
const SPIRC_PING_TIMEOUT: Duration = Duration::from_secs(1);
/// Cada cuánto se mira si vuelve a haber salida de audio mientras falta.
const OUTPUT_PROBE_EVERY: Duration = Duration::from_millis(1500);
/// Lo que se espera a saber el tipo de cuenta tras conectar, si aún no había llegado.
const ACCOUNT_TYPE_WAIT: Duration = Duration::from_secs(10);

/// Presupuesto de la precarga inteligente en spclient (metadatos y storage-resolve): ráfaga y una
/// ficha más cada tanto. Va aparte del limitador de librespot (300 cada 30 s, compartido con la
/// reproducción y las listas), así que pasar el ratón por una lista no puede gastar lo que necesita
/// la canción que se pulsa.
const WARM_SPCLIENT_BURST: f64 = 8.0;
const WARM_SPCLIENT_EVERY: Duration = Duration::from_secs(2);
/// Lo mismo para las claves por adelantado (solo de ficheros ya en la caché del disco): Spotify
/// vigila cuántas se piden y las niega en ráfagas, así que muy pocas: dos de golpe y luego una
/// cada 10 s (en dos minutos pasando el ratón, 14 como mucho).
const WARM_KEY_BURST: f64 = 2.0;
const WARM_KEY_EVERY: Duration = Duration::from_secs(10);
/// Tras un «demasiadas peticiones» (de librespot o de Spotify) o una clave negada, la precarga
/// calla este tiempo.
const WARM_BACKOFF: Duration = Duration::from_secs(60);
/// Plazo de una precarga entera: lo que no llegue en este tiempo ya no sirve.
const WARM_TIMEOUT: Duration = Duration::from_secs(8);

const REDIRECT_URI: &str = "http://127.0.0.1:8898/login";
/// Puerto de `REDIRECT_URI`, donde espera librespot-oauth mientras se inicia sesión en el navegador.
const LOGIN_PORT: u16 = 8898;
/// Un inicio de sesión que no se completa en el navegador en este tiempo se cancela solo. Antes
/// la espera de librespot-oauth no acababa nunca si se cerraba la pestaña, y con ella el bucle de
/// órdenes del reproductor quedaba parado hasta reiniciar Nanofy.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Motivo de un inicio de sesión cancelado (en Spotify, con «Cancelar» o por `LOGIN_TIMEOUT`).
const LOGIN_CANCELLED: &str = "Inicio de sesión cancelado";

const SCOPES: &[&str] = &[
    "app-remote-control",
    "playlist-modify",
    "playlist-modify-private",
    "playlist-modify-public",
    "playlist-read",
    "playlist-read-collaborative",
    "playlist-read-private",
    "streaming",
    "user-follow-modify",
    "user-follow-read",
    "user-library-modify",
    "user-library-read",
    "user-modify",
    "user-modify-playback-state",
    "user-modify-private",
    "user-personalized",
    "user-read-currently-playing",
    "user-read-email",
    "user-read-play-history",
    "user-read-playback-position",
    "user-read-playback-state",
    "user-read-private",
    "user-read-recently-played",
    "user-top-read",
];

#[derive(Clone, Debug)]
pub enum Cmd {
    /// Iniciar sesión (en el navegador si no hay credenciales guardadas). `chain`: URL de la
    /// autorización de la biblioteca ya preparada (`WebAuth::prepare_connect`); la página de vuelta
    /// del inicio de sesión salta a ella, en la misma pestaña, si Spotify devolvió el código.
    Login { chain: Option<String> },
    /// La interfaz lleva demasiado tiempo en «cargando»: comprobar la sesión y reconectar si hace falta.
    Stalled,
    /// La carga lleva un rato sin avanzar (vigilante de la interfaz, a los 8 s): si Spirc no
    /// contesta a un ping en 1 s, su bucle está atascado y se reconecta (como `Stalled`); si
    /// contesta, se vuelve a pedir la carga pendiente, si la hay.
    RetryLoad(Option<Box<Cmd>>),
    Logout,
    /// Reinicia el reproductor (y la sesión) con ajustes que no se pueden cambiar en marcha:
    /// nombre del dispositivo, autoplay, caché de audio.
    Restart(Settings),
    /// Ajustes nuevos sin reiniciar: el reproductor aplica la normalización al instante y la
    /// calidad desde la próxima canción. También sin sesión, para que conectar use los nuevos.
    AudioTuning(Settings),
    /// Fundido entre canciones en vigor (`ms` = 0 lo apaga), al instante y sin reiniciar. Es el
    /// valor efectivo, no el ajuste: 0 mientras el temporizador «al terminar la canción» está
    /// puesto aunque el fundido esté elegido. Por eso no se guarda en la copia de los ajustes,
    /// sino aparte, y es lo que usan los reproductores nuevos (reconexión, `Restart`).
    Crossfade {
        ms: u32,
        albums: bool,
    },
    PlayPause,
    /// Play y pausa explícitos: la interfaz conoce el estado real y evita que el conmutador de
    /// Spirc haga lo contrario cuando su estado interno se ha quedado desfasado.
    Play,
    Pause,
    /// Vuelve a cargar desde cero la canción en pausa, en su punto, y la pone a sonar (una
    /// canción cortada por la red cuyo enlace al audio pudo caducar; ver `Spirc::reload`).
    Reload,
    /// Siguiente canción. `auto`: un salto que no pidió el usuario (la canción que entra está
    /// oculta). Durante un fundido, la anterior sigue apagándose a su ritmo en vez de cortarse en
    /// 40 ms como con un «siguiente» a mano.
    Next {
        auto: bool,
    },
    Prev,
    Seek(u32),
    /// Volumen definitivo: salida de audio + estado de Spotify Connect.
    Volume(u16),
    /// Volumen provisional mientras se arrastra el slider: solo la salida de audio.
    VolumePreview(u16),
    Shuffle(bool),
    Repeat {
        context: bool,
        track: bool,
    },
    /// Trae a este equipo la reproducción que suena en otro dispositivo.
    TransferHere,
    /// Prepara una canción en el reproductor (metadatos, clave y primer trozo de audio) sin
    /// tocar Spotify Connect: al abrir, la que casi seguro se va a restaurar.
    Preload(String),
    /// Al abrir: trae en pausa la última sesión de la cuenta guardada en Spotify (contexto,
    /// canción, posición, aleatorio, repetición y cola), como hace la app oficial.
    #[allow(dead_code)]
    ResumeSession,
    /// `index` junto a `track_uri`: la fila pulsada en la lista sin filtrar, que Spirc usa solo si
    /// en esa posición del contexto está justo esa canción (ver `LoadRequestOptions::fallback_index`).
    LoadContext {
        uri: String,
        track_uri: Option<String>,
        index: Option<u32>,
        shuffle: bool,
        /// `Some(ms)`: cargar en pausa en esa posición (restaurar la sesión anterior).
        resume: Option<u32>,
        /// La canción con la que casi seguro empieza: el reproductor la carga ya, mientras Spirc
        /// resuelve el contexto (ver `Player::warm`). `None` si no se sabe (aleatorio, Jam).
        first: Option<String>,
    },
    LoadTracks {
        uris: Vec<String>,
        index: Option<u32>,
        shuffle: bool,
        resume: Option<u32>,
        /// Como en `LoadContext`.
        first: Option<String>,
    },
    /// Precarga inteligente: el ratón lleva un rato sobre esta canción. Metadatos y ubicación del
    /// fichero (y su clave si ya está en la caché del disco), sin bajar audio, con presupuesto
    /// propio (`WarmBudget`). Sin sesión, sin Premium o con Spotify frenando las claves, nada.
    Warm(String),
    /// Se está pulsando el botón de reproducir de esta canción: el reproductor la prepara entera
    /// (primer trozo y decodificador) en su hueco aparte (`Player::warm`). Como `Warm`, sin aviso
    /// si no se puede.
    WarmHead(String),
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum Event {
    Status(String),
    Error(String),
    /// Sesión conectada. `fresh`: se acaba de iniciar sesión en el navegador (no con credenciales
    /// guardadas ni al reconectar): la interfaz sigue entonces con la autorización de la biblioteca.
    LoggedIn { username: String, device_id: String, fresh: bool },
    LoggedOut,
    /// El inicio de sesión en el navegador no se completó: `by_user` si se canceló (en Spotify, con
    /// «Cancelar» o porque pasó `LOGIN_TIMEOUT`), si no, falló. No hay sesión.
    LoginAborted { reason: String, by_user: bool },
    TrackChanged(NowPlaying),
    Playing { position_ms: u32 },
    Paused { position_ms: u32 },
    Position(u32),
    Stopped,
    Loading,
    /// Fallo definitivo de la canción que se pidió (le precede `LoadFailed`): Spirc la marca y
    /// pasa a la siguiente.
    Unavailable,
    /// La canción que se pidió (`uri`) no se pudo cargar. `transient`: Spotify frenando las
    /// claves o las peticiones, o la red; no se salta: queda en pausa y «reproducir» la vuelve a
    /// cargar. `play`: iba a sonar (no era una carga en pausa, como la de restaurar al abrir).
    LoadFailed {
        uri: String,
        reason: LoadFailure,
        transient: bool,
        play: bool,
    },
    /// Spirc se detuvo tras `failed` canciones seguidas que no se pudieron reproducir.
    SkipCascade { failed: u32 },
    /// La red se cortó a media canción: en pausa en `position_ms` (lo último que se oyó), sin
    /// saltar; «reproducir» la reanuda desde ahí en cuanto vuelven a llegar datos. También llega
    /// cuando se pidió reanudar y aún no llegaban.
    Stalled { uri: String, position_ms: u32 },
    /// No hay ningún dispositivo de salida de audio: la canción quedó en pausa.
    NoAudioOutput,
    /// Vuelve a haber un dispositivo de salida tras `NoAudioOutput`.
    AudioOutputBack,
    /// La cuenta no es Premium: Spotify no deja reproducir en apps externas. Se puede seguir
    /// usando la biblioteca y la búsqueda; las órdenes de reproducir se ignoran.
    NotPremium,
    Volume(u16),
    Shuffle(bool),
    Repeat { context: bool, track: bool },
    ShutdownDone,
    /// Se suelta la conexión con Spotify para reconectar: el audio se corta aquí, y la interfaz
    /// guarda este punto para retomarlo tal cual.
    Reconnecting,
    /// La sesión con Spotify se perdió y se ha restablecido. El reproductor es nuevo y está
    /// vacío: la interfaz vuelve a cargar lo que sonaba.
    Reconnected,
    /// Resultado de `Cmd::ResumeSession`: Spotify tenía (o no) una sesión que restaurar.
    SessionResumed(bool),
    /// Estado del clúster de Spotify Connect (llega por el dealer al conectar y con cada cambio):
    /// la sesión de la cuenta aunque ningún dispositivo esté activo.
    Cluster(ClusterInfo),
    /// Cola compartida de una Jam (pista actual, contexto y siguientes, por uri) cuando somos
    /// participante, para mostrarla en la interfaz en lugar de la cola local de la cuenta.
    JamQueue {
        current: String,
        context: String,
        next: Vec<String>,
    },
    /// Formato, calidad y normalización de lo que suena aquí (al empezar cada canción y al
    /// cambiar la normalización en vivo).
    AudioFormat(AudioInfo),
    /// Empezó un fundido: `to` ya es la canción que suena (llegaron `TrackChanged` y `Playing`) y
    /// `from` se apaga durante `ms`.
    Crossfade { from: String, to: String, ms: u32 },
}

/// Resumen del `PlayerState` del clúster.
#[derive(Debug, Clone, Default)]
pub struct ClusterInfo {
    pub active_device_id: String,
    /// Última actualización del estado (ms desde 1970): permite saber si es más reciente que
    /// la copia local aunque el dispositivo que lo dejó ya esté apagado.
    pub timestamp_ms: i64,
    pub context_uri: String,
    pub track_uri: String,
    pub position_ms: u32,
    pub is_playing: bool,
    pub is_paused: bool,
    pub shuffle: bool,
    pub repeat_context: bool,
    pub repeat_track: bool,
    /// Canciones añadidas a mano a la cola.
    pub queue: Vec<String>,
    /// Siguientes pistas (incluida la cola manual), en orden.
    pub next: Vec<String>,
}

/// Estado compartido con la capa de Web API (para obtener tokens).
#[derive(Default)]
pub struct Shared {
    pub session: Mutex<Option<Session>>,
}

pub struct Backend {
    tx: mpsc::UnboundedSender<Cmd>,
    pub handle: tokio::runtime::Handle,
    pub shared: Arc<Shared>,
    _rt: tokio::runtime::Runtime,
}

/// Por qué no se pudo conectar (`start`).
#[derive(Debug)]
enum StartError {
    /// Spotify rechazó el inicio de sesión porque la cuenta no es Premium.
    NotPremium,
    Other(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::NotPremium => f.write_str("la cuenta no es Premium"),
            StartError::Other(e) => f.write_str(e),
        }
    }
}

impl From<String> for StartError {
    fn from(e: String) -> Self {
        StartError::Other(e)
    }
}

impl From<&str> for StartError {
    fn from(e: &str) -> Self {
        StartError::Other(e.to_string())
    }
}

/// Cuenta un fallo al conectar: sin Premium no es un error que reintentar sino algo que explicar
/// (`Event::NotPremium`); el resto, con `what` delante.
fn report_start_error(ui: &UiTx, what: &str, e: StartError) {
    match e {
        StartError::NotPremium => {
            log::warn!("Spotify rechaza la cuenta: no es Premium");
            ui.send(Msg::Backend(Event::NotPremium));
        }
        StartError::Other(e) => ui.error(format!("{what}: {e}")),
    }
}

/// Órdenes que harían sonar algo aquí. Con una cuenta sin Premium se ignoran (Spotify no daría
/// el audio y acabarían en errores de clave o saltos); pausar, el volumen y lo demás siguen.
fn is_playback(cmd: &Cmd) -> bool {
    matches!(
        cmd,
        Cmd::PlayPause
            | Cmd::Play
            | Cmd::Reload
            | Cmd::Next { .. }
            | Cmd::Prev
            | Cmd::Seek(_)
            | Cmd::TransferHere
            | Cmd::Preload(_)
            | Cmd::ResumeSession
            | Cmd::LoadContext { .. }
            | Cmd::LoadTracks { .. }
    )
}

/// ¿Durmió el equipo mientras el bucle esperaba el vistazo de salud (despierto, como mucho
/// `HEALTH_EVERY`)? `mono` es lo que pasó según el reloj monótono y `wall`, según la hora del
/// sistema (`None` si fue hacia atrás). Se miran los dos: en Windows no está garantizado que el
/// monótono cuente el tiempo dormido, y la hora del sistema sí lo cuenta pero puede saltar sola
/// (sincronización); basta con que uno lo vea.
fn slept(mono: Duration, wall: Option<Duration>) -> bool {
    mono >= SLEEP_GAP || wall.is_some_and(|w| w >= SLEEP_GAP)
}

/// Un cubo de fichas: `burst` de golpe y una más cada `every`.
#[derive(Debug, Clone)]
struct Bucket {
    tokens: f64,
    burst: f64,
    every: Duration,
    last: std::time::Instant,
}

impl Bucket {
    fn new(burst: f64, every: Duration, now: std::time::Instant) -> Self {
        Self {
            tokens: burst,
            burst,
            every,
            last: now,
        }
    }

    fn take(&mut self, now: std::time::Instant) -> bool {
        let gained = now.saturating_duration_since(self.last).as_secs_f64() / self.every.as_secs_f64();
        self.tokens = (self.tokens + gained).min(self.burst);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Presupuesto de la precarga inteligente (ver `WARM_SPCLIENT_BURST`). Lo que ya está en memoria
/// no gasta fichas: solo cada petición que de verdad sale.
#[derive(Debug, Clone)]
struct WarmBudget {
    spclient: Bucket,
    keys: Bucket,
    quiet_until: Option<std::time::Instant>,
}

impl WarmBudget {
    fn new(now: std::time::Instant) -> Self {
        Self {
            spclient: Bucket::new(WARM_SPCLIENT_BURST, WARM_SPCLIENT_EVERY, now),
            keys: Bucket::new(WARM_KEY_BURST, WARM_KEY_EVERY, now),
            quiet_until: None,
        }
    }

    /// ¿Calla por un «demasiadas peticiones» o una clave negada reciente?
    fn quiet(&self, now: std::time::Instant) -> bool {
        self.quiet_until.is_some_and(|t| now < t)
    }

    /// ¿Puede salir esta petición? Si sí, gasta su ficha.
    fn allow(&mut self, step: WarmStep, now: std::time::Instant) -> bool {
        if self.quiet(now) {
            return false;
        }
        match step {
            WarmStep::Metadata | WarmStep::Storage => self.spclient.take(now),
            WarmStep::Key => self.keys.take(now),
        }
    }

    /// Spotify (o librespot) pidió frenar: nada de precarga durante `WARM_BACKOFF`.
    fn back_off(&mut self, now: std::time::Instant) {
        self.quiet_until = Some(now + WARM_BACKOFF);
    }
}

/// ¿Es un error que pide frenar la precarga? El limitador de librespot o un 429 de Spotify
/// (ResourceExhausted), o una clave negada.
fn warm_should_back_off(e: &librespot_core::Error) -> bool {
    use librespot_core::error::ErrorKind;
    matches!(e.kind, ErrorKind::ResourceExhausted) || librespot_core::audio_key::AudioKeyError::of(e).is_some()
}

/// Una precarga inteligente pendiente: la última pedida (las anteriores ya no interesan).
struct WarmJob {
    uri: String,
    session: Session,
    bitrate: librespot_playback::config::Bitrate,
}

/// La precarga pendiente y el aviso al trabajador. Solo una en marcha y, detrás, solo la última:
/// pasar el ratón deprisa por veinte filas no encola veinte.
#[derive(Default)]
struct WarmSlot {
    job: Mutex<Option<WarmJob>>,
    ready: tokio::sync::Notify,
}

impl WarmSlot {
    fn push(&self, job: WarmJob) {
        *self.job.lock().unwrap() = Some(job);
        self.ready.notify_one();
    }

    fn clear(&self) {
        *self.job.lock().unwrap() = None;
    }
}

/// Atiende las precargas inteligentes de una en una (`Cmd::Warm`).
async fn warm_worker(slot: Arc<WarmSlot>) {
    let mut budget = WarmBudget::new(std::time::Instant::now());
    loop {
        slot.ready.notified().await;
        // Se saca del hueco: la sesión no queda retenida ahí tras una reconexión.
        let Some(job) = slot.job.lock().unwrap().take() else { continue };
        let now = std::time::Instant::now();
        if budget.quiet(now) || librespot_core::key_policy::keys_throttled() {
            continue;
        }
        let Ok(uri) = librespot_core::SpotifyUri::from_uri(&job.uri) else { continue };
        let t0 = std::time::Instant::now();
        let warm = librespot_playback::player::warm_track(&job.session, &uri, job.bitrate, |step| {
            budget.allow(step, std::time::Instant::now())
        });
        let result = tokio::time::timeout(WARM_TIMEOUT, warm).await;
        match result {
            Ok(Ok(what)) => log::debug!("[precarga] {}: {what} ({} ms)", job.uri, t0.elapsed().as_millis()),
            Ok(Err(e)) => {
                if warm_should_back_off(&e) {
                    log::info!("[precarga] Spotify pide frenar ({e}); sin precarga {} s", WARM_BACKOFF.as_secs());
                    budget.back_off(std::time::Instant::now());
                } else {
                    log::debug!("[precarga] {}: {e}", job.uri);
                }
            }
            Err(_) => log::debug!("[precarga] {}: sin respuesta en {} s", job.uri, WARM_TIMEOUT.as_secs()),
        }
    }
}

/// Hay un vigilante de la salida de audio en marcha (uno basta: avisa al volver y termina).
static OUTPUT_WATCH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Falta la salida de audio: se avisa a la interfaz y se mira cada poco si vuelve a haber un
/// dispositivo, para que reanude lo que sonaba. El vigilante no depende de la sesión: si se
/// reconecta mientras tanto, el aviso de vuelta sigue llegando.
fn watch_output(ui: &UiTx) {
    use std::sync::atomic::Ordering;
    ui.send(Msg::Backend(Event::NoAudioOutput));
    if OUTPUT_WATCH.swap(true, Ordering::SeqCst) {
        return;
    }
    let ui = ui.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(OUTPUT_PROBE_EVERY).await;
            // cpal pregunta al sistema (unos ms, con COM en Windows): fuera de los hilos de tokio.
            let back = tokio::task::spawn_blocking(audio_backend::probe_output).await.unwrap_or(false);
            if back {
                log::info!("vuelve a haber salida de audio");
                ui.send(Msg::Backend(Event::AudioOutputBack));
                break;
            }
        }
        OUTPUT_WATCH.store(false, Ordering::SeqCst);
    });
}

/// ¿Atiende Spirc? Su respuesta llega cuando le toca el turno al ping (ver `Spirc::ping`).
async fn spirc_alive(spirc: &Spirc) -> bool {
    match spirc.ping() {
        Ok(rx) => matches!(tokio::time::timeout(SPIRC_PING_TIMEOUT, rx).await, Ok(Ok(()))),
        Err(_) => false,
    }
}

impl Backend {
    pub fn start(paths: Paths, settings: Settings, ui: UiTx) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            // Dos hilos: si una llamada de librespot bloquea uno (visto al reconectar tras un
            // estancamiento), los temporizadores y el bucle de órdenes siguen vivos en el otro.
            .worker_threads(2)
            // Pilas pequeñas y pocos hilos de bloqueo (DNS, archivos): menos RAM comprometida.
            .thread_stack_size(512 * 1024)
            .max_blocking_threads(2)
            .thread_keep_alive(Duration::from_secs(3))
            .thread_name("nanofy-io")
            .enable_all()
            .build()
            .expect("no se pudo crear el runtime de tokio");
        let (tx, rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared::default());
        let handle = rt.handle().clone();
        rt.spawn(run(rx, tx.clone(), ui, shared.clone(), paths, settings));
        Self {
            tx,
            handle,
            shared,
            _rt: rt,
        }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }
}

struct Active {
    spirc: Spirc,
    session: Session,
    player: Arc<Player>,
    /// Identifica esta conexión: los avisos de «tarea terminada» de conexiones viejas se ignoran.
    generation: u64,
    /// Cierre de la app: no esperar a Spotify más que unas decenas de ms.
    fast_stop: bool,
}

impl Active {
    /// `keep_session`: dejar la sesión de la cuenta en Spotify (pausada, con posición) para que
    /// se restaure al volver a abrir o desde otro dispositivo. `Spirc::shutdown` borra el estado
    /// del dispositivo en el servidor (y con él la sesión), así que al cerrar la app solo se
    /// desconecta; el borrado se reserva para cerrar sesión.
    async fn stop(self, keep_session: bool) {
        if keep_session {
            let _ = self.spirc.disconnect(true);
            // Tiempo para que Spirc envíe la posición y el estado «inactivo» (una petición).
            // Al cerrar la app el límite lo pone on_exit (unos 40 ms); al cerrar sesión o
            // reconectar hay más margen.
            tokio::time::sleep(Duration::from_millis(if self.fast_stop { 20 } else { 350 })).await;
        } else {
            let _ = self.spirc.shutdown();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.session.shutdown();
    }
}

/// Volumen de la interfaz frente al que ya tiene Spirc. Antes de cada carga se activa el
/// dispositivo y se le pasa el volumen (mientras no estaba activo, Spirc lo ignoró); si es el
/// mismo que ya se le dio tras activar, no se repite. Va por conexión (`generation`): un Spirc
/// nuevo no tiene nada aplicado. Un volumen pedido fuera de una carga no cuenta como aplicado,
/// porque pudo llegar con el dispositivo inactivo; así se vuelve a pasar en la carga siguiente.
#[derive(Default)]
struct VolumeSync {
    /// Último volumen pedido por la interfaz.
    wanted: Option<u16>,
    /// Conexión y valor que se le dio a Spirc justo después de activar.
    applied: Option<(u64, u16)>,
}

impl VolumeSync {
    /// Volumen que hay que pasarle a Spirc tras activar, si no lo tiene ya.
    fn pending(&self, generation: u64) -> Option<u16> {
        let v = self.wanted?;
        (self.applied != Some((generation, v))).then_some(v)
    }

    fn applied(&mut self, generation: u64, v: u16) {
        self.applied = Some((generation, v));
    }
}

/// Olvida los tokens guardados entre sesiones (librespot los reutiliza al abrir para conectar
/// antes). Si una conexión falla o se cae, el siguiente intento los pide nuevos: así un token
/// revocado antes de caducar no puede dejar la app sin conectar.
fn forget_cached_tokens(paths: &Paths) {
    let _ = std::fs::remove_file(paths.credentials_dir().join("tokens.json"));
}

/// Suelta la conexión actual para volver a conectar. Se avisa antes de cerrarla: es cuando el
/// audio se corta, y la interfaz fija ahí el punto que retomará.
async fn drop_for_reconnect(active: &mut Option<Active>, shared: &Shared, ui: &UiTx, paths: &Paths) {
    forget_cached_tokens(paths);
    if let Some(a) = active.take() {
        ui.send(Msg::Backend(Event::Reconnecting));
        a.stop(true).await;
    }
    *shared.session.lock().unwrap() = None;
}

async fn run(
    mut rx: mpsc::UnboundedReceiver<Cmd>,
    // Para volver a encolar una orden (la carga que `Cmd::RetryLoad` repite), detrás de las demás.
    self_tx: mpsc::UnboundedSender<Cmd>,
    ui: UiTx,
    shared: Arc<Shared>,
    paths: Paths,
    mut settings: Settings,
) {
    let mut active: Option<Active> = None;
    // Último volumen pedido por la interfaz: Spirc ignora SetVolume mientras no es el
    // dispositivo activo, así que se reaplica justo después de activar (solo si cambió).
    let mut volume = VolumeSync::default();
    // Vigilancia de la conexión: la tarea de Spirc avisa por aquí cuando termina (la sesión
    // caducó, se perdió la red, el equipo durmió…). Si no la paramos nosotros, se reconecta.
    let (dead_tx, mut dead_rx) = mpsc::unbounded_channel::<u64>();
    let mut generation: u64 = 0;
    let mut retry_at: Option<tokio::time::Instant> = None;
    let mut retry_delay = Duration::from_secs(2);
    // Fundido en vigor (ms, también en álbumes). Empieza como en los ajustes y luego manda la
    // interfaz (`Cmd::Crossfade`), que lo anula con el temporizador «al terminar la canción».
    // Va aparte de `settings` para que un `Restart` o una reconexión no lo devuelvan al ajuste
    // guardado mientras esa anulación sigue en pie.
    let mut crossfade: (u32, bool) = (settings.crossfade_ms(), settings.crossfade_albums);
    // Precarga inteligente (`Cmd::Warm`): un trabajador para todas las conexiones.
    let warm = Arc::new(WarmSlot::default());
    tokio::spawn(warm_worker(warm.clone()));

    // Inicio de sesión automático con las credenciales guardadas.
    if let Some(creds) = cached_credentials(&paths) {
        generation += 1;
        match start(&paths, &settings, crossfade, creds, &ui, &shared, generation, dead_tx.clone(), false).await {
            Ok(a) => active = Some(a),
            Err(e) => {
                forget_cached_tokens(&paths);
                report_start_error(&ui, "No se pudo conectar con Spotify", e)
            }
        }
    }

    // Vistazo de salud a intervalos fijos (antes, un temporizador que cualquier orden reiniciaba:
    // con uso continuo no llegaba nunca). Si la espera que acaba en un vistazo duró mucho más que
    // el intervalo, el equipo estuvo dormido.
    let mut health = tokio::time::interval(HEALTH_EVERY);
    health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    health.reset();

    loop {
        enum Next {
            Cmd(Cmd),
            Dead(u64),
            Retry,
            Health,
        }
        // Desde aquí el bucle solo espera. Lo que tarden las órdenes (una conexión puede llevar
        // 25 s) no cuenta como tiempo dormido: solo lo esperado hasta el vistazo siguiente.
        let idle_from = (std::time::Instant::now(), std::time::SystemTime::now());
        let retry = async {
            match retry_at {
                Some(t) => tokio::time::sleep_until(t).await,
                None => std::future::pending::<()>().await,
            }
        };
        let next = tokio::select! {
            c = rx.recv() => match c {
                Some(c) => Next::Cmd(c),
                None => break,
            },
            d = dead_rx.recv() => match d {
                Some(g) => Next::Dead(g),
                None => continue,
            },
            _ = retry => Next::Retry,
            _ = health.tick() => Next::Health,
        };
        // ¿Durmió el equipo mientras se esperaba? Con el vistazo de salud cada 30 s, despierto
        // nunca se espera 45 s. Se mira con lo que haya despertado al bucle, no solo con el
        // vistazo: al volver de la suspensión puede llegar antes otra cosa y el vistazo, ya
        // atrasado, saldría enseguida en la vuelta siguiente sin ver nada.
        let mono = idle_from.0.elapsed();
        let wall = std::time::SystemTime::now().duration_since(idle_from.1).ok();
        if active.is_some() && slept(mono, wall) {
            // Tras suspender, la conexión con Spotify suele quedar medio cerrada: parece viva,
            // pero lo primero que se pida se queda colgado (la canción en «cargando»). Se
            // reconecta ya, antes de que haga falta; la interfaz retoma lo que hubiera en su punto,
            // sonando o en pausa, como en cualquier reconexión.
            log::warn!(
                "el equipo estuvo dormido ({} s esperando, {} s de reloj); se reconecta",
                mono.as_secs(),
                wall.map_or(0, |w| w.as_secs())
            );
            drop_for_reconnect(&mut active, &shared, &ui, &paths).await;
            retry_at = Some(tokio::time::Instant::now());
        }
        let cmd = match next {
            Next::Cmd(c) => c,
            Next::Dead(g) => {
                // Solo cuenta si es la conexión actual; las paradas voluntarias ya vaciaron `active`.
                if active.as_ref().map(|a| a.generation) == Some(g) {
                    log::warn!("la conexión con Spotify terminó sola; reconectando");
                    ui.status("Se perdió la conexión con Spotify; reconectando…");
                    drop_for_reconnect(&mut active, &shared, &ui, &paths).await;
                    retry_at = Some(tokio::time::Instant::now());
                }
                continue;
            }
            Next::Health => {
                if active.as_ref().map(|a| a.session.is_invalid()).unwrap_or(false) {
                    log::warn!("la sesión de Spotify está invalidada; reconectando");
                    ui.status("Se perdió la conexión con Spotify; reconectando…");
                    drop_for_reconnect(&mut active, &shared, &ui, &paths).await;
                    retry_at = Some(tokio::time::Instant::now());
                }
                continue;
            }
            Next::Retry => {
                retry_at = None;
                if active.is_some() {
                    continue;
                }
                let Some(creds) = cached_credentials(&paths) else { continue };
                generation += 1;
                match start(&paths, &settings, crossfade, creds, &ui, &shared, generation, dead_tx.clone(), false).await {
                    Ok(a) => {
                        retry_delay = Duration::from_secs(2);
                        ui.send(Msg::Backend(Event::Reconnected));
                        if let Some(v) = volume.wanted {
                            let _ = a.spirc.set_volume(v);
                        }
                        active = Some(a);
                    }
                    Err(StartError::NotPremium) => {
                        // No se va a arreglar reintentando: se explica y se deja de insistir.
                        forget_cached_tokens(&paths);
                        report_start_error(&ui, "", StartError::NotPremium);
                    }
                    Err(e) => {
                        forget_cached_tokens(&paths);
                        log::warn!("reconexión fallida: {e}; reintento en {retry_delay:?}");
                        ui.status(format!("Sin conexión con Spotify; reintentando en {} s…", retry_delay.as_secs()));
                        retry_at = Some(tokio::time::Instant::now() + retry_delay);
                        retry_delay = (retry_delay * 2).min(Duration::from_secs(30));
                    }
                }
                continue;
            }
        };
        match cmd {
            Cmd::Stalled => {
                // La interfaz lleva demasiado en «cargando»: si la sesión está caída (o
                // parece viva pero no responde), se reconecta y se repite la última orden.
                if let Some(a) = active.as_ref() {
                    log::warn!("reproducción estancada (sesión inválida: {}); reconectando", a.session.is_invalid());
                    ui.status("Spotify no responde; reconectando…");
                    drop_for_reconnect(&mut active, &shared, &ui, &paths).await;
                }
                retry_at = Some(tokio::time::Instant::now());
            }
            Cmd::RetryLoad(load) => {
                // Reconectando: al volver, la interfaz repite la carga pendiente por su cuenta.
                let Some(a) = active.as_ref() else { continue };
                if a.session.is_invalid() || !spirc_alive(&a.spirc).await {
                    log::warn!("la carga no avanza y Spirc no contesta en {SPIRC_PING_TIMEOUT:?}; reconectando");
                    ui.status("Spotify no responde; reconectando…");
                    drop_for_reconnect(&mut active, &shared, &ui, &paths).await;
                    retry_at = Some(tokio::time::Instant::now());
                } else if let Some(cmd) = load {
                    // Spirc atiende: lo atascado es la carga (una petición que no vuelve). Se pide
                    // otra vez por el camino de siempre, detrás de lo que ya esperaba.
                    log::info!("la carga no avanza; se vuelve a pedir");
                    let _ = self_tx.send(*cmd);
                } else {
                    log::info!("la carga no avanza, pero Spirc contesta; se sigue esperando");
                }
            }
            Cmd::Login { chain } => {
                if active.is_some() {
                    continue;
                }
                retry_at = None;
                let (creds, fresh) = match cached_credentials(&paths) {
                    Some(c) => (Ok(c), false),
                    None => {
                        ui.status("Se ha abierto el navegador para iniciar sesión en Spotify…");
                        (browser_login(chain).await, true)
                    }
                };
                match creds {
                    Ok(c) => match {
                        generation += 1;
                        start(&paths, &settings, crossfade, c, &ui, &shared, generation, dead_tx.clone(), fresh).await
                    } {
                        Ok(a) => active = Some(a),
                        Err(e) => {
                            forget_cached_tokens(&paths);
                            report_start_error(&ui, "No se pudo conectar con Spotify", e)
                        }
                    },
                    Err(e) => {
                        let by_user = e.starts_with(LOGIN_CANCELLED);
                        let reason = if by_user { e } else { format!("No se pudo iniciar sesión: {e}") };
                        log::warn!("{reason}");
                        ui.send(Msg::Backend(Event::LoginAborted { reason, by_user }));
                    }
                }
            }
            Cmd::Logout => {
                retry_at = None;
                if let Some(a) = active.take() {
                    a.stop(false).await;
                }
                *shared.session.lock().unwrap() = None;
                let _ = std::fs::remove_file(paths.credentials_dir().join("credentials.json"));
                forget_cached_tokens(&paths);
                // Las claves de audio concedidas a esta cuenta (solo en memoria) se olvidan con ella,
                // y también sus metadatos y ubicaciones de ficheros (otra cuenta puede ser de otro
                // país).
                librespot_core::audio_key::forget_cached_keys();
                librespot_core::spclient::forget_cached_metadata();
                librespot_core::cdn_url::CdnUrl::forget_all();
                warm.clear();
                ui.send(Msg::Backend(Event::LoggedOut));
            }
            Cmd::Restart(new_settings) => {
                settings = new_settings;
                if let Some(a) = active.take() {
                    a.stop(true).await;
                }
                if let Some(creds) = cached_credentials(&paths) {
                    generation += 1;
                    match start(&paths, &settings, crossfade, creds, &ui, &shared, generation, dead_tx.clone(), false).await {
                        Ok(a) => {
                            ui.status("Ajustes de reproducción aplicados");
                            active = Some(a);
                        }
                        Err(e) => report_start_error(&ui, "No se pudo reiniciar la reproducción", e),
                    }
                }
            }
            Cmd::AudioTuning(new_settings) => {
                // La copia de aquí es la que usan las reconexiones y `Restart`.
                settings = new_settings;
                if let Some(a) = active.as_ref() {
                    log::info!(
                        "ajustes de audio en vivo: calidad {:?}, gapless {}, normalización {} ({:?})",
                        settings.quality,
                        settings.gapless,
                        if settings.normalisation { "activada" } else { "desactivada" },
                        settings.loudness
                    );
                    a.player.set_audio_tuning(tuning(&settings));
                }
            }
            Cmd::Crossfade { ms, albums } => {
                // También sin sesión: la conexión siguiente crea el reproductor con este valor.
                crossfade = (ms, albums);
                if let Some(a) = active.as_ref() {
                    a.player.set_crossfade(ms, albums);
                }
            }
            Cmd::Warm(uri) => {
                // Sin aviso si no se puede: es por adelantado, nadie la ha pedido aún.
                let Some(a) = active.as_ref() else { continue };
                if a.session.is_invalid() || !a.session.is_premium() || librespot_core::key_policy::keys_throttled() {
                    continue;
                }
                warm.push(WarmJob {
                    uri,
                    session: a.session.clone(),
                    bitrate: settings.quality.bitrate(),
                });
            }
            Cmd::WarmHead(uri) => {
                let Some(a) = active.as_ref() else { continue };
                if a.session.is_invalid() || !a.session.is_premium() || librespot_core::key_policy::keys_throttled() {
                    continue;
                }
                if let Ok(uri) = librespot_core::SpotifyUri::from_uri(&uri) {
                    log::debug!("[precarga] se prepara entera {uri}");
                    a.player.warm(uri, false);
                }
            }
            Cmd::Shutdown => {
                if let Some(mut a) = active.take() {
                    a.fast_stop = true;
                    a.stop(true).await;
                }
                ui.send(Msg::Backend(Event::ShutdownDone));
                break;
            }
            other => {
                log::debug!("[cmd] {other:?}");
                // Una orden que no llega a ejecutarse aquí no se guarda: al reconectar, la interfaz
                // repite la carga que aún no sonaba o retoma lo que sonaba en su punto exacto.
                let Some(a) = active.as_ref() else {
                    if retry_at.is_some() {
                        ui.status("Reconectando con Spotify…");
                    } else {
                        ui.status("Inicia sesión para reproducir música");
                    }
                    continue;
                };
                if a.session.is_invalid() {
                    // La sesión murió sin que la tarea avisara todavía: reconectar ya.
                    ui.status("Se perdió la conexión con Spotify; reconectando…");
                    drop_for_reconnect(&mut active, &shared, &ui, &paths).await;
                    retry_at = Some(tokio::time::Instant::now());
                    continue;
                }
                if is_playback(&other) && !a.session.is_premium() {
                    // Sin Premium Spotify no da el audio: en vez de cargar para fallar, se explica.
                    log::info!("[cmd] ignorada (cuenta sin Premium): {other:?}");
                    ui.send(Msg::Backend(Event::NotPremium));
                    continue;
                }
                let result = match other {
                    Cmd::PlayPause | Cmd::Play | Cmd::Pause | Cmd::Reload | Cmd::Next { .. } | Cmd::Prev => {
                        // Tras mucho tiempo en pausa Spotify deja de tenernos como dispositivo
                        // activo y Spirc ignora estas órdenes: se reactiva antes (si ya lo
                        // estaba, la activación se ignora sin efecto).
                        let _ = a.spirc.activate();
                        match other {
                            Cmd::PlayPause => a.spirc.play_pause(),
                            Cmd::Play => a.spirc.play(),
                            Cmd::Pause => a.spirc.pause(),
                            Cmd::Reload => a.spirc.reload(),
                            Cmd::Next { auto: false } => a.spirc.next(),
                            Cmd::Next { auto: true } => a.spirc.auto_next(),
                            _ => a.spirc.prev(),
                        }
                    }
                    Cmd::Seek(ms) => a.spirc.set_position_ms(ms),
                    Cmd::Volume(v) => {
                        volume.wanted = Some(v);
                        // Se oye al instante; Spirc solo sincroniza el estado con Spotify.
                        audio_backend::set_output_volume(v as f32 / u16::MAX as f32);
                        a.spirc.set_volume(v)
                    }
                    Cmd::VolumePreview(v) => {
                        audio_backend::set_output_volume(v as f32 / u16::MAX as f32);
                        Ok(())
                    }
                    Cmd::Shuffle(on) => a.spirc.shuffle(on),
                    Cmd::Repeat { context, track } => a
                        .spirc
                        .repeat(context)
                        .and_then(|_| a.spirc.repeat_track(track)),
                    Cmd::TransferHere => a.spirc.transfer(None),
                    Cmd::Preload(uri) => {
                        if let Ok(uri) = librespot_core::SpotifyUri::from_uri(&uri) {
                            a.player.preload(uri);
                        }
                        Ok(())
                    }
                    Cmd::ResumeSession => {
                        use librespot_core::dealer::protocol::TransferOptions;
                        use librespot_core::spclient::TransferRequest;
                        let dev = a.session.device_id().to_string();
                        // Variantes de opciones (se prueban en orden hasta que el servidor acepte).
                        // `restore_paused: "restore"` es lo que acepta el servidor (otras opciones dan 500).
                        let variants: Vec<(&str, Option<TransferRequest>)> = vec![
                            ("restore", Some(TransferRequest { transfer_options: TransferOptions { restore_paused: Some("restore".into()), restore_position: None, restore_track: None, retain_session: None } })),
                            ("sin opciones", None),
                        ];
                        // Hasta que Spirc registra el dispositivo (justo después de recibir el
                        // connection id del dealer) el servidor responde 404 a la transferencia.
                        // Se espera a tener connection id (máx. 3 s) y un margen para el registro.
                        let t0 = std::time::Instant::now();
                        while a.session.connection_id().is_empty() && t0.elapsed() < Duration::from_secs(3) {
                            tokio::time::sleep(Duration::from_millis(40)).await;
                        }
                        crate::tmark("sesión: dealer conectado");
                        tokio::time::sleep(Duration::from_millis(120)).await;
                        let mut ok = false;
                        'tries: for attempt in 0..6u32 {
                            if attempt > 0 {
                                tokio::time::sleep(Duration::from_millis(300)).await;
                            }
                            for (name, req) in &variants {
                                match a.session.spclient().transfer(&dev, &dev, req.as_ref()).await {
                                    Ok(b) => {
                                        crate::tmark("sesión: transferida");
                                        log::info!("[restore] transferencia '{name}' aceptada al intento {} ({} bytes)", attempt + 1, b.len());
                                        ok = true;
                                        break 'tries;
                                    }
                                    Err(e) => log::debug!("[restore] transferencia '{name}' (intento {}): {e}", attempt + 1),
                                }
                            }
                        }
                        if !ok {
                            log::info!("[restore] Spotify no tenía sesión que restaurar");
                        }
                        ui.send(Msg::Backend(Event::SessionResumed(ok)));
                        Ok(())
                    }
                    Cmd::LoadContext {
                        uri,
                        track_uri,
                        index,
                        shuffle,
                        resume,
                        first,
                    } => {
                        warm_first(a, first.as_deref(), resume);
                        let _ = a.spirc.activate();
                        if let Some(v) = volume.pending(a.generation) {
                            if a.spirc.set_volume(v).is_ok() {
                                volume.applied(a.generation, v);
                            }
                        }
                        // Con la canción, la fila pulsada va aparte: Spirc la usa solo si ahí está
                        // justo esa canción (una repetida suena donde se pulsó; con la lista
                        // filtrada la fila no es la posición en el contexto y se busca por uri).
                        let fallback_index = track_uri.as_ref().and(index);
                        let playing_track = track_uri
                            .map(PlayingTrack::Uri)
                            .or(index.map(PlayingTrack::Index));
                        a.spirc.load(LoadRequest::from_context_uri(
                            uri,
                            LoadRequestOptions {
                                start_playing: resume.is_none(),
                                seek_to: resume.unwrap_or(0),
                                context_options: Some(LoadContextOptions::Options(ContextOptions {
                                    shuffle,
                                    repeat: false,
                                    repeat_track: false,
                                })),
                                playing_track,
                                fallback_index,
                            },
                        ))
                    }
                    Cmd::LoadTracks {
                        uris,
                        index,
                        shuffle,
                        resume,
                        first,
                    } => {
                        warm_first(a, first.as_deref(), resume);
                        let _ = a.spirc.activate();
                        if let Some(v) = volume.pending(a.generation) {
                            if a.spirc.set_volume(v).is_ok() {
                                volume.applied(a.generation, v);
                            }
                        }
                        a.spirc.load(LoadRequest::from_tracks(
                            uris,
                            LoadRequestOptions {
                                start_playing: resume.is_none(),
                                seek_to: resume.unwrap_or(0),
                                context_options: Some(LoadContextOptions::Options(ContextOptions {
                                    shuffle,
                                    repeat: false,
                                    repeat_track: false,
                                })),
                                playing_track: index.map(PlayingTrack::Index),
                                fallback_index: None,
                            },
                        ))
                    }
                    _ => Ok(()),
                };
                if let Err(e) = result {
                    ui.error(format!("Error de reproducción: {e}"));
                }
            }
        }
    }
}

/// Carga en paralelo: el reproductor empieza ya con la canción pulsada (metadatos, clave, CDN),
/// mientras Spirc resuelve el contexto (context-resolve, una ida y vuelta más que antes iba
/// delante). Cuando Spirc la pide, el reproductor la toma de su hueco aparte (`Player::warm`). Solo
/// al reproducir desde el principio: restaurar en una posición carga en pausa por su camino. Con
/// Spotify frenando las claves no se adelanta nada (la carga de Spirc la pedirá igual). Si al
/// final Spirc empieza por otra (una canción sustituida por otra edición), lo preparado se suelta.
fn warm_first(a: &Active, first: Option<&str>, resume: Option<u32>) {
    if resume.unwrap_or(0) != 0 || librespot_core::key_policy::keys_throttled() {
        return;
    }
    if let Some(uri) = first.and_then(|u| librespot_core::SpotifyUri::from_uri(u).ok()) {
        a.player.warm(uri, true);
    }
}

fn cached_credentials(paths: &Paths) -> Option<Credentials> {
    if crate::config::no_session() {
        return None;
    }
    let cache = Cache::new(Some(paths.credentials_dir()), None, None, None).ok()?;
    cache.credentials()
}

/// Inicio de sesión en el navegador con plazo (`LOGIN_TIMEOUT`). La espera de librespot-oauth es
/// bloqueante y no tiene plazo: al cumplirse, se le corta como lo haría «Cancelar».
async fn browser_login(chain: Option<String>) -> Result<Credentials, String> {
    let mut job = tokio::task::spawn_blocking(move || oauth_login(chain.as_deref()));
    match tokio::time::timeout(LOGIN_TIMEOUT, &mut job).await {
        Ok(r) => r.unwrap_or_else(|e| Err(e.to_string())),
        Err(_) => {
            log::warn!("inicio de sesión sin completar en {} min: se cancela", LOGIN_TIMEOUT.as_secs() / 60);
            std::thread::spawn(cancel_login_listener);
            match tokio::time::timeout(Duration::from_secs(5), job).await {
                // Llegó justo a tiempo: vale.
                Ok(Ok(Ok(c))) => return Ok(c),
                Ok(_) => {}
                // No se pudo cortar (el puerto no era suyo): se deja ese hilo y el bucle sigue.
                Err(_) => log::warn!("la espera del inicio de sesión no se pudo cortar"),
            }
            Err(format!("{LOGIN_CANCELLED}: pasaron {} minutos sin completarlo en el navegador", LOGIN_TIMEOUT.as_secs() / 60))
        }
    }
}

/// `chain`: ver `Cmd::Login`.
fn oauth_login(chain: Option<&str>) -> Result<Credentials, String> {
    let client_id = SessionConfig::default().client_id;
    let client = librespot_oauth::OAuthClientBuilder::new(&client_id, REDIRECT_URI, SCOPES.to_vec())
        .open_in_browser()
        .with_custom_message(&crate::webauth::login_page(chain))
        .build()
        .map_err(|e| e.to_string())?;
    let token = client.get_access_token().map_err(login_error)?;
    Ok(Credentials::with_access_token(token.access_token))
}

/// Texto de un fallo del inicio de sesión. Volver sin código (`error=access_denied`: «Cancelar»
/// en Spotify o en Nanofy) es una cancelación, no un error; el resto se explica tal cual.
fn login_error(e: librespot_oauth::OAuthError) -> String {
    match e {
        librespot_oauth::OAuthError::AuthCodeNotFound { uri } if uri.contains("error=") => LOGIN_CANCELLED.to_string(),
        e => e.to_string(),
    }
}

/// Corta un inicio de sesión que espera en el navegador: le hace al servidor local de
/// librespot-oauth la misma visita que haría Spotify al cancelar (`?error=access_denied`), su
/// espera termina con error y el bucle de órdenes queda libre. Bloqueante (unos 2 s como mucho,
/// reintentando por si aún no escuchaba): para un hilo aparte. Devuelve si llegó a entregarla.
pub fn cancel_login_listener() -> bool {
    cancel_listener_at(LOGIN_PORT)
}

fn cancel_listener_at(port: u16) -> bool {
    use std::io::{Read, Write};
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < until {
        if let Ok(mut s) = std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(300)) {
            let _ = s.set_write_timeout(Some(Duration::from_secs(1)));
            let _ = s.set_read_timeout(Some(Duration::from_secs(1)));
            if s
                .write_all(b"GET /login?error=access_denied HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
                .is_ok()
            {
                // Se lee su respuesta (la página) para no cortarle la escritura.
                let mut sink = [0u8; 4096];
                while matches!(s.read(&mut sink), Ok(n) if n > 0) {}
                log::info!("inicio de sesión: espera del navegador cortada");
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    log::warn!("inicio de sesión: no había ninguna espera del navegador que cortar");
    false
}

#[allow(clippy::too_many_arguments)]
async fn start(
    paths: &Paths,
    settings: &Settings,
    crossfade: (u32, bool),
    creds: Credentials,
    ui: &UiTx,
    shared: &Arc<Shared>,
    generation: u64,
    dead_tx: mpsc::UnboundedSender<u64>,
    fresh: bool,
) -> Result<Active, StartError> {
    ui.status("Conectando con Spotify…");

    let audio_dir = (settings.audio_cache_mb > 0).then(|| paths.audio_cache_dir());
    let cache = Cache::new(
        Some(paths.credentials_dir()),
        Some(paths.volume_dir()),
        audio_dir,
        Some(settings.audio_cache_mb.max(1) * 1024 * 1024),
    )
    .map_err(|e| e.to_string())?;

    let initial_volume = cache
        .volume()
        .unwrap_or_else(|| vol_pct_to_raw(settings.volume as f32));

    let session_config = SessionConfig {
        device_id: settings.device_id.clone(),
        autoplay: Some(settings.autoplay),
        ..SessionConfig::default()
    };
    let session = Session::new(session_config, Some(cache));

    let mut player_config = PlayerConfig {
        // La salida es F32: el tramado solo actúa al convertir a enteros, así que no hacía nada
        // (salvo anunciar en el registro un «Converting with ditherer» que no era cierto).
        ditherer: None,
        ..PlayerConfig::default()
    };
    player_config.set_tuning(&tuning(settings));
    // El fundido en vigor, no el del ajuste (ver `Cmd::Crossfade`).
    (player_config.crossfade_ms, player_config.crossfade_albums) = crossfade;

    // Volumen lineal en el mezclador: la curva perceptual (dB) la aplica la interfaz.
    let mixer = (mixer::find(None).ok_or("no hay mezclador de volumen disponible")?)(
        MixerConfig {
            volume_ctrl: VolumeCtrl::Linear,
            ..MixerConfig::default()
        },
    )
    .map_err(|e| e.to_string())?;
    let sink = audio_backend::find(None).ok_or("no hay salida de audio disponible")?;

    // El dispositivo de audio se abre en la primera reproducción, no al arrancar: sin
    // hilos ni búferes de WASAPI/ALSA/CoreAudio hasta que hacen falta.
    let player = Player::new(
        player_config,
        session.clone(),
        Box::new(NoOpVolume),
        move || {
            Box::new(LazySink {
                open: Some(Box::new(move || sink(None, AudioFormat::F32))),
                inner: None,
                tap: PcmTap::from_env(),
            }) as Box<dyn Sink>
        },
    );

    let mut events = player.get_player_event_channel();
    let ui_events = ui.clone();
    // Débil: el reproductor no debe seguir vivo solo porque esta tarea lo recuerde.
    let player_weak = Arc::downgrade(&player);
    tokio::spawn(async move {
        while let Some(ev) = events.recv().await {
            if matches!(
                ev,
                PlayerEvent::Paused { .. }
                    | PlayerEvent::Stopped { .. }
                    | PlayerEvent::Stalled { .. }
                    | PlayerEvent::LoadFailed { transient: true, .. }
            ) {
                // Tras un rato en pausa se suelta la salida de audio: con el flujo abierto,
                // Windows da el dispositivo por ocupado aunque solo suene silencio, y unos
                // auriculares Bluetooth multipunto no cambian al teléfono. Si en ese tiempo se
                // vuelve a reproducir, el reproductor ignora la orden. Una carga que falló por
                // algo pasajero también deja la canción en pausa a la espera de reintentarla.
                let player = player_weak.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(RELEASE_AUDIO_AFTER_PAUSE).await;
                    if let Some(p) = player.upgrade() {
                        p.release_sink();
                    }
                });
            }
            if let PlayerEvent::ClusterSnapshot { cluster } = &ev {
                use protobuf::Message as _;
                match librespot_protocol::connect::Cluster::parse_from_bytes(cluster) {
                    Ok(c) => ui_events.send(Msg::Backend(Event::Cluster(cluster_info(&c, "inicial")))),
                    Err(e) => log::warn!("clúster inicial no legible: {e}"),
                }
                continue;
            }
            // Una pausa sin salida de audio no la pidió nadie: no hay ningún dispositivo (la
            // salida lo intentó 1,5 s). Se avisa (después de la pausa) y se vigila su vuelta.
            let no_output = matches!(ev, PlayerEvent::Paused { .. }) && !audio_backend::output_ok();
            // Una canción no disponible ya no borra su audio de la caché: casi siempre es una
            // clave denegada o el límite de peticiones, no un fichero dañado, y se perdía audio
            // bueno; además pedía sus metadatos otra vez, una petición más justo cuando Spotify
            // ya estaba frenando. Un fichero de caché que no se puede leer lo borra el propio
            // reproductor, que lo vuelve a descargar en la misma carga.
            if let Some(e) = map_event(ev) {
                ui_events.send(Msg::Backend(e));
            }
            if no_output {
                watch_output(&ui_events);
            }
        }
    });

    let connect = ConnectConfig {
        name: settings.device_name.clone(),
        device_type: DeviceType::Computer,
        initial_volume,
        ..ConnectConfig::default()
    };

    // Escucha propia del clúster: Spirc recibe lo mismo, pero no lo expone. El primer estado
    // llega justo tras registrar el dispositivo y trae la sesión de la cuenta (contexto, pista,
    // posición, opciones y cola) aunque no haya ningún dispositivo activo.
    {
        use futures_util::StreamExt;
        use librespot_core::dealer::protocol::Message as DealerMessage;
        use librespot_protocol::connect::ClusterUpdate;
        let mut clusters = session
            .dealer()
            .listen_for("hm://connect-state/v1/cluster", DealerMessage::from_raw::<ClusterUpdate>)
            .map_err(|e| e.to_string())?;
        let ui_c = ui.clone();
        tokio::spawn(async move {
            while let Some(r) = clusters.next().await {
                let cu = match r {
                    Ok(cu) => cu,
                    Err(e) => {
                        log::debug!("clúster: {e}");
                        continue;
                    }
                };
                let info = cluster_info(&cu.cluster, "actualización");
                ui_c.send(Msg::Backend(Event::Cluster(info)));
            }
        });
    }
    // Spirc::new (parche propio) pide el token de cliente y el de acceso mientras conecta al
    // punto de acceso; esperar aquí antes a esos mismos datos solo retrasaba la conexión.
    let (spirc, task) = match tokio::time::timeout(
        Duration::from_secs(25),
        Spirc::new(connect, session.clone(), creds, player.clone(), mixer),
    )
    .await
    {
        // Una cuenta sin Premium la rechaza el propio inicio de sesión: no es un fallo de red que
        // reintentar (antes: «No se pudo conectar con Spotify: Premium account required», en
        // inglés y reintentando sin fin al reconectar).
        Ok(Err(e)) if librespot_core::session::is_premium_required(&e) => {
            session.shutdown();
            return Err(StartError::NotPremium);
        }
        Ok(r) => r.map_err(|e| e.to_string())?,
        Err(_) => {
            session.shutdown();
            return Err("Spotify no respondió al conectar (25 s)".into());
        }
    };
    tokio::spawn(async move {
        task.await;
        let _ = dead_tx.send(generation);
    });

    *shared.session.lock().unwrap() = Some(session.clone());
    ui.send(Msg::Backend(Event::LoggedIn {
        username: session.username(),
        device_id: session.device_id().to_string(),
        fresh,
    }));
    audio_backend::set_output_volume(initial_volume as f32 / u16::MAX as f32);
    ui.send(Msg::Backend(Event::Volume(initial_volume)));

    // Otra cuenta que no es Premium sí puede entrar: Spotify lo dice justo después (tipo de cuenta)
    // y librespot cerraba entonces la aplicación sin ningún mensaje. Ahora se explica y se deja la
    // sesión abierta para la biblioteca y la búsqueda; las órdenes de reproducir no pasan (ver
    // `is_playback`). Si el tipo aún no ha llegado, se espera un poco sin bloquear la conexión.
    if !session.is_premium() {
        log::warn!("la cuenta es {:?}: sin reproducción", session.account_type());
        ui.send(Msg::Backend(Event::NotPremium));
    } else if session.account_type().is_none() {
        let session = session.clone();
        let ui = ui.clone();
        let player = Arc::downgrade(&player);
        tokio::spawn(async move {
            let t0 = std::time::Instant::now();
            while t0.elapsed() < ACCOUNT_TYPE_WAIT && !session.is_invalid() {
                if let Some(t) = session.account_type() {
                    if !session.is_premium() {
                        log::warn!("la cuenta es {t:?}: sin reproducción");
                        // Lo que hubiera empezado a cargar (restaurar al abrir) no va a sonar.
                        if let Some(p) = player.upgrade() {
                            p.stop();
                        }
                        ui.send(Msg::Backend(Event::NotPremium));
                    }
                    return;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        });
    }

    Ok(Active {
        spirc,
        session,
        player,
        generation,
        fast_stop: false,
    })
}

/// Calidad y normalización del reproductor a partir de los ajustes; la usan el arranque y los
/// cambios en vivo (`Cmd::AudioTuning`), así ambos configuran exactamente lo mismo.
///
/// La normalización sigue el modelo de Spotify. Sus metadatos traen la ganancia de cada canción
/// (y de su álbum) para −14 LUFS y el pico:
/// - Normal (−14 LUFS): esa ganancia, también hacia arriba, pero solo hasta dejar el pico a
///   −1 dBFS; sin limitador.
/// - Bajo (−19 LUFS): lo mismo con −5 dB.
/// - Alto (−11 LUFS): +3 dB sin tope y un limitador a −1 dBFS (ataque 5 ms, liberación 100 ms,
///   rodilla de 1 dB), el único nivel que lo usa.
///
/// «Auto» usa la ganancia del álbum cuando Spirc lo indica (un álbum en orden) y la de la canción
/// en lo demás.
fn tuning(s: &Settings) -> AudioTuning {
    let (method, pregain_db) = match s.loudness {
        Loudness::Loud => (NormalisationMethod::Dynamic, 3.0),
        Loudness::Normal => (NormalisationMethod::Basic, 0.0),
        Loudness::Quiet => (NormalisationMethod::Basic, -5.0),
    };
    AudioTuning {
        // Una sola traducción Quality → Bitrate, compartida con las descargas.
        bitrate: s.quality.bitrate(),
        gapless: s.gapless,
        normalisation: s.normalisation,
        normalisation_type: NormalisationType::Auto,
        normalisation_method: method,
        normalisation_pregain_db: pregain_db,
        normalisation_threshold_dbfs: -1.0,
        normalisation_attack_cf: duration_to_coefficient(Duration::from_millis(5)),
        normalisation_release_cf: duration_to_coefficient(Duration::from_millis(100)),
        normalisation_knee_db: 1.0,
    }
}

/// Sink que crea el sink real (y abre el dispositivo) en el primer `start()`.
struct LazySink {
    open: Option<Box<dyn FnOnce() -> Box<dyn Sink> + Send>>,
    inner: Option<Box<dyn Sink>>,
    /// Copia de lo que sale, para las pruebas (`NANOFY_PCM_TAP`).
    tap: Option<PcmTap>,
}

/// Copia en un archivo de todo lo que el reproductor manda a la salida, si `NANOFY_PCM_TAP=ruta`:
/// muestras f32 little-endian intercaladas, 44,1 kHz estéreo. Es la mezcla tal cual, antes del
/// volumen y del remuestreo de la salida y sin recortar, para que las pruebas vean si un fundido
/// pasa de 1,0 o deja huecos sin depender del dispositivo. Sin la variable no cuesta nada.
struct PcmTap {
    file: std::fs::File,
    buf: Vec<u8>,
}

impl PcmTap {
    fn from_env() -> Option<Self> {
        let path = std::env::var_os("NANOFY_PCM_TAP")?;
        // Se añade al final: tras reconectar o reiniciar el reproductor (sink nuevo) la grabación
        // sigue en el mismo archivo.
        match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            Ok(file) => {
                log::info!("[pcm] copia de la salida en {}", std::path::Path::new(&path).display());
                Some(Self { file, buf: Vec::new() })
            }
            Err(e) => {
                log::warn!("[pcm] no se pudo abrir {}: {e}", std::path::Path::new(&path).display());
                None
            }
        }
    }

    /// Escribe un paquete; `false` si el archivo falló (se deja de copiar, sin tocar el audio).
    fn write(&mut self, samples: &[f64]) -> bool {
        use std::io::Write as _;
        pcm_tap_bytes(samples, &mut self.buf);
        match self.file.write_all(&self.buf) {
            Ok(()) => true,
            Err(e) => {
                log::warn!("[pcm] se deja de copiar la salida: {e}");
                false
            }
        }
    }
}

/// Muestras f64 → f32 little-endian en `out` (que se vacía antes). Sin recortar: un pico de la
/// mezcla por encima de 1,0 debe verse en la copia.
fn pcm_tap_bytes(samples: &[f64], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(samples.len() * 4);
    for &s in samples {
        out.extend_from_slice(&(s as f32).to_le_bytes());
    }
}

impl LazySink {
    fn get(&mut self) -> &mut Box<dyn Sink> {
        if self.inner.is_none() {
            let open = self.open.take().expect("sink ya abierto");
            self.inner = Some(open());
        }
        self.inner.as_mut().unwrap()
    }
}

impl Sink for LazySink {
    fn start(&mut self) -> SinkResult<()> {
        self.get().start()
    }
    fn stop(&mut self) -> SinkResult<()> {
        match self.inner.as_mut() {
            Some(s) => s.stop(),
            None => Ok(()),
        }
    }
    // Los tres de la pausa instantánea y la salida preparada se pasan tal cual: sin ellos valdrían
    // los de por defecto del trait (pausar esperando a la cola, no vaciarla, no preparar nada).
    fn pause_now(&mut self) -> SinkResult<()> {
        match self.inner.as_mut() {
            Some(s) => s.pause_now(),
            None => Ok(()),
        }
    }
    fn clear(&mut self) {
        if let Some(s) = self.inner.as_mut() {
            s.clear();
        }
    }
    fn prepare(&mut self) -> SinkResult<()> {
        // También crea el sink real si aún no existía: es justo lo que se quiere adelantar.
        self.get().prepare()
    }
    fn queued_ms(&self) -> u32 {
        self.inner.as_ref().map_or(0, |s| s.queued_ms())
    }
    fn release(&mut self) {
        if let Some(s) = self.inner.as_mut() {
            s.release();
        }
    }
    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        if let (Some(tap), AudioPacket::Samples(samples)) = (self.tap.as_mut(), &packet) {
            if !tap.write(samples) {
                self.tap = None;
            }
        }
        self.get().write(packet, converter)
    }
}

/// Resume el clúster de Connect (estado de la cuenta) en lo que la interfaz necesita.
fn cluster_info(c: &librespot_protocol::connect::Cluster, origen: &str) -> ClusterInfo {
    let ps = &c.player_state;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let mut pos = ps.position_as_of_timestamp;
    if ps.is_playing && !ps.is_paused && ps.timestamp > 0 {
        pos += now_ms - ps.timestamp;
    }
    let info = ClusterInfo {
        active_device_id: c.active_device_id.clone(),
        timestamp_ms: ps.timestamp,
        context_uri: ps.context_uri.clone(),
        track_uri: ps.track.uri.clone(),
        position_ms: pos.clamp(0, u32::MAX as i64) as u32,
        is_playing: ps.is_playing,
        is_paused: ps.is_paused,
        shuffle: ps.options.shuffling_context,
        repeat_context: ps.options.repeating_context,
        repeat_track: ps.options.repeating_track,
        queue: ps.next_tracks.iter().filter(|t| t.provider == "queue").map(|t| t.uri.clone()).collect(),
        next: ps.next_tracks.iter().map(|t| t.uri.clone()).take(60).collect(),
    };
    log::info!(
        "[clúster {origen}] activo=<{}> ctx={} pista={} pos={} ms ts={} playing={} paused={} sig={}",
        info.active_device_id,
        info.context_uri,
        info.track_uri,
        info.position_ms,
        info.timestamp_ms,
        info.is_playing,
        info.is_paused,
        info.next.len()
    );
    log_mix_keys(ps, origen);
    info
}

/// Las claves de mezcla que trae el clúster (fundidos y transiciones de Spotify: `audio.*`,
/// `automix.*`, `media.*`…), del contexto, de la pista que suena y de las dos siguientes, con el
/// uid de cada una. Solo al depurar (`RUST_LOG=nanofy=debug`): sirve para ver si Spotify manda las
/// transiciones de una playlist mezclada a un dispositivo Connect de terceros cuando la reproduce
/// el teléfono (plan 1.7, paso 24); no cambia nada de lo que se hace con el clúster.
fn log_mix_keys(ps: &librespot_protocol::player::PlayerState, origen: &str) {
    if !log::log_enabled!(log::Level::Debug) {
        return;
    }
    let ctx = crate::mixprobe::mix_pairs(&ps.context_metadata);
    let tracks: Vec<String> = std::iter::once(&*ps.track)
        .chain(ps.next_tracks.iter().take(2))
        .filter_map(|t| {
            let keys = crate::mixprobe::mix_pairs(&t.metadata);
            (!keys.is_empty()).then(|| format!("{} uid={} [{}]", t.uri, t.uid, keys.join("; ")))
        })
        .collect();
    if ctx.is_empty() && tracks.is_empty() {
        return;
    }
    log::debug!("[mezcla] clúster {origen}: contexto [{}] · pistas {}", ctx.join("; "), tracks.join(" · "));
}

fn map_event(ev: PlayerEvent) -> Option<Event> {
    use PlayerEvent::*;
    Some(match ev {
        TrackChanged { audio_item } => {
            let item = *audio_item;
            let (artists, album) = match item.unique_fields {
                UniqueFields::Track { artists, album, .. } => (
                    artists
                        .iter()
                        .map(|a| (a.name.clone(), a.id.to_id().ok()))
                        .collect(),
                    album,
                ),
                UniqueFields::Episode { show_name, .. } => (vec![(show_name, None)], String::new()),
                UniqueFields::Local { artists, album, .. } => (
                    artists.map(|a| vec![(a, None)]).unwrap_or_default(),
                    album.unwrap_or_default(),
                ),
            };
            // Portada: la mayor de hasta 320 px; si no hay, la más pequeña.
            let cover_url = item
                .covers
                .iter()
                .filter(|c| c.width <= 320)
                .max_by_key(|c| c.width)
                .or_else(|| item.covers.iter().min_by_key(|c| c.width))
                .map(|c| c.url.clone());
            Event::TrackChanged(NowPlaying {
                uri: item.uri.clone(),
                id: item.track_id.to_id().ok(),
                name: item.name,
                artists,
                album,
                album_id: None,
                cover_url,
                duration_ms: item.duration_ms,
            })
        }
        Playing { position_ms, .. } => Event::Playing { position_ms },
        Paused { position_ms, .. } => Event::Paused { position_ms },
        Seeked { position_ms, .. } | PositionCorrection { position_ms, .. } => {
            Event::Position(position_ms)
        }
        Stopped { .. } => Event::Stopped,
        // Durante un fundido la canción anterior sigue sonando mientras carga la siguiente: la
        // barra no debe enseñar «cargando» (al entrar llegan `TrackChanged` y `Playing`).
        Loading { crossfading: true, .. } => return None,
        Loading { .. } => Event::Loading,
        Unavailable { .. } => Event::Unavailable,
        LoadFailed { track_id, reason, transient, play, .. } => Event::LoadFailed {
            uri: track_id.to_uri().unwrap_or_else(|_| track_id.to_string()),
            reason,
            transient,
            play,
        },
        SkipCascade { failed } => Event::SkipCascade { failed },
        Stalled { track_id, position_ms, .. } => Event::Stalled {
            uri: track_id.to_uri().unwrap_or_else(|_| track_id.to_string()),
            position_ms,
        },
        VolumeChanged { volume } => {
            audio_backend::set_output_volume(volume as f32 / u16::MAX as f32);
            Event::Volume(volume)
        }
        ShuffleChanged { shuffle } => Event::Shuffle(shuffle),
        RepeatChanged { context, track } => Event::Repeat { context, track },
        JamQueue { current, context, next } => Event::JamQueue { current, context, next },
        AudioFormat { track_id, source, normalisation_db, album_gain, gain_data, .. } => {
            Event::AudioFormat(audio_info(&track_id, source, normalisation_db, album_gain, gain_data))
        }
        CrossfadeStarted { from, to, fade_ms } => Event::Crossfade {
            from: from.to_string(),
            to: to.to_string(),
            ms: fade_ms,
        },
        _ => return None,
    })
}

/// Cómo sale el audio hacia el dispositivo (la última salida abierta). Se lee de unos atómicos
/// del reproductor: se puede llamar en cada fotograma sin esperar a su hilo.
pub fn audio_output() -> crate::model::AudioOutput {
    let (rate, channels, sinc) = audio_backend::output_info();
    crate::model::AudioOutput { rate, channels, sinc }
}

/// Códec legible y kbps de un formato de Spotify. Sin comodín: un formato nuevo en librespot
/// obliga a decidir aquí cómo se enseña.
fn format_info(f: librespot_metadata::audio::AudioFileFormat) -> (&'static str, Option<u16>) {
    use librespot_metadata::audio::AudioFileFormat::*;
    match f {
        OGG_VORBIS_96 => ("Ogg Vorbis", Some(96)),
        OGG_VORBIS_160 => ("Ogg Vorbis", Some(160)),
        OGG_VORBIS_320 => ("Ogg Vorbis", Some(320)),
        MP3_96 => ("MP3", Some(96)),
        MP3_160 | MP3_160_ENC => ("MP3", Some(160)),
        MP3_256 => ("MP3", Some(256)),
        MP3_320 => ("MP3", Some(320)),
        AAC_24 => ("AAC", Some(24)),
        AAC_48 => ("AAC", Some(48)),
        MP4_128 => ("AAC", Some(128)),
        AAC_160 => ("AAC", Some(160)),
        AAC_320 => ("AAC", Some(320)),
        XHE_AAC_12 => ("xHE-AAC", Some(12)),
        XHE_AAC_16 => ("xHE-AAC", Some(16)),
        XHE_AAC_24 => ("xHE-AAC", Some(24)),
        FLAC_FLAC | FLAC_FLAC_24BIT => ("FLAC", None),
        OTHER5 => ("Desconocido", None),
    }
}

/// Lo que cuenta el reproductor de la canción que suena, en los tipos de la interfaz.
fn audio_info(
    track_id: &librespot_core::SpotifyUri,
    source: AudioSource,
    normalisation_db: Option<f64>,
    album_gain: bool,
    gain_data: bool,
) -> AudioInfo {
    use librespot_playback::config::Bitrate;
    let (codec, kbps) = match source.format {
        Some(f) => format_info(f),
        None => ("Archivo local", None),
    };
    AudioInfo {
        // La misma uri que trae TrackChanged (`NowPlaying::uri`).
        uri: track_id.to_uri().unwrap_or_default(),
        format: source.format.map(|f| format!("{f:?}")),
        codec: codec.to_string(),
        kbps,
        bits: source.bits,
        sample_rate: source.sample_rate,
        requested_kbps: source.requested.map(|b| match b {
            Bitrate::Bitrate96 => 96,
            Bitrate::Bitrate160 => 160,
            Bitrate::Bitrate320 => 320,
        }),
        from_cache: source.from_cache,
        episode: matches!(track_id, librespot_core::SpotifyUri::Episode { .. }),
        gain_db: normalisation_db.map(|db| db as f32),
        album_gain,
        gain_data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Quality;
    use librespot_playback::config::Bitrate;
    use librespot_playback::player::NormalisationData;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 0.001
    }

    /// Factor que aplicaría el reproductor con estos ajustes, por canción o por álbum.
    fn factor(s: &Settings, data: NormalisationData, album: bool) -> f64 {
        let mut config = PlayerConfig::default();
        config.set_tuning(&tuning(s));
        // «Auto» lo resuelve el reproductor según Spirc; aquí se elige a mano.
        config.normalisation_type = if album { NormalisationType::Album } else { NormalisationType::Track };
        NormalisationData::get_factor(&config, data)
    }

    fn track(gain_db: f64, peak: f64) -> NormalisationData {
        NormalisationData { track_gain_db: gain_db, track_peak: peak, album_gain_db: 0.0, album_peak: 1.0 }
    }

    fn with_level(l: Loudness) -> Settings {
        Settings { loudness: l, ..Settings::default() }
    }

    /// Los niveles de Spotify traducidos a librespot: Normal y Bajo sin limitador (Bajo, −5 dB),
    /// Alto con +3 dB y limitador a −1 dBFS; todos con «Auto» (álbum cuando toca).
    #[test]
    fn niveles_como_spotify() {
        let n = tuning(&with_level(Loudness::Normal));
        assert_eq!(n.normalisation_method, NormalisationMethod::Basic);
        assert_eq!(n.normalisation_pregain_db, 0.0);
        assert_eq!(n.normalisation_threshold_dbfs, -1.0);
        assert_eq!(n.normalisation_type, NormalisationType::Auto);
        let q = tuning(&with_level(Loudness::Quiet));
        assert_eq!(q.normalisation_method, NormalisationMethod::Basic);
        assert_eq!(q.normalisation_pregain_db, -5.0);
        let l = tuning(&with_level(Loudness::Loud));
        assert_eq!(l.normalisation_method, NormalisationMethod::Dynamic);
        assert_eq!(l.normalisation_pregain_db, 3.0);
        assert_eq!(l.normalisation_threshold_dbfs, -1.0);
        assert_eq!(l.normalisation_knee_db, 1.0);
        assert_eq!(l.normalisation_attack_cf, duration_to_coefficient(Duration::from_millis(5)));
        assert_eq!(l.normalisation_release_cf, duration_to_coefficient(Duration::from_millis(100)));

        // Calidad, gapless y el interruptor salen tal cual de los ajustes.
        let s = Settings { quality: Quality::Low, gapless: false, normalisation: false, ..Settings::default() };
        let t = tuning(&s);
        assert_eq!(t.bitrate, Bitrate::Bitrate96);
        assert!(!t.gapless);
        assert!(!t.normalisation);
        // Instalación nueva: activada, en Normal y a 320 kbps.
        let d = tuning(&Settings::default());
        assert!(d.normalisation);
        assert_eq!(d, n);
        assert_eq!(d.bitrate, Bitrate::Bitrate320);
    }

    /// Los valores del plan, de punta a punta (ajustes → configuración → factor del reproductor).
    #[test]
    fn factores_de_punta_a_punta() {
        let normal = with_level(Loudness::Normal);
        assert!(close(factor(&normal, track(6.0, 0.5), false), 1.782));
        assert!(close(factor(&normal, track(-6.0, 1.0), false), 0.501));
        assert!(close(factor(&normal, track(3.0, 1.0), false), 0.891));
        assert!(close(factor(&with_level(Loudness::Loud), track(-6.0, 1.0), false), 0.708));
        assert!(close(factor(&with_level(Loudness::Quiet), track(-6.0, 1.0), false), 0.282));
        // Alto: +3 dB sin tope aunque el pico pase (el limitador lo baja en tiempo real).
        assert!(close(factor(&with_level(Loudness::Loud), track(3.0, 1.0), false), 1.995));
        // Apagada: el audio pasa intacto.
        let off = Settings { normalisation: false, ..Settings::default() };
        assert_eq!(factor(&off, track(-8.0, 1.0), false), 1.0);
        // Álbum en orden: manda la ganancia del álbum, no la de la canción.
        let data = NormalisationData { track_gain_db: 4.0, track_peak: 0.5, album_gain_db: -2.0, album_peak: 1.0 };
        assert!(close(factor(&normal, data, true), 0.794));
        assert!(close(factor(&normal, data, false), 1.585));
        // Sin datos de volumen (pódcast en MP3, archivo local sin ReplayGain): no se toca en
        // ningún nivel, ni por canción ni por álbum.
        for l in [Loudness::Loud, Loudness::Normal, Loudness::Quiet] {
            for album in [false, true] {
                assert_eq!(factor(&with_level(l), NormalisationData::default(), album), 1.0, "{l:?} álbum {album}");
            }
        }
    }

    /// Lo que se cuenta de cada formato: códec y kbps; el FLAC no tiene kbps fijos.
    #[test]
    fn formatos_de_spotify_legibles() {
        use librespot_metadata::audio::AudioFileFormat as F;
        assert_eq!(format_info(F::OGG_VORBIS_320), ("Ogg Vorbis", Some(320)));
        assert_eq!(format_info(F::OGG_VORBIS_160), ("Ogg Vorbis", Some(160)));
        assert_eq!(format_info(F::OGG_VORBIS_96), ("Ogg Vorbis", Some(96)));
        assert_eq!(format_info(F::MP3_256), ("MP3", Some(256)));
        assert_eq!(format_info(F::MP3_160_ENC), ("MP3", Some(160)));
        assert_eq!(format_info(F::AAC_320), ("AAC", Some(320)));
        assert_eq!(format_info(F::FLAC_FLAC_24BIT), ("FLAC", None));
    }

    /// Del evento del reproductor a lo que enseña la interfaz: la uri es la de TrackChanged, la
    /// calidad pedida en kbps, los pódcasts marcados y la ganancia en dB.
    #[test]
    fn evento_de_audio_a_la_interfaz() {
        use librespot_core::SpotifyUri;
        use librespot_metadata::audio::AudioFileFormat as F;
        let source = AudioSource {
            format: Some(F::OGG_VORBIS_160),
            bits: None,
            sample_rate: 44_100,
            requested: Some(Bitrate::Bitrate320),
            from_cache: true,
        };
        let track = SpotifyUri::from_uri("spotify:track:4uLU6hMCjMI75M1A2tKUQC").unwrap();
        let a = audio_info(&track, source, Some(-5.25), true, true);
        assert_eq!(a.uri, "spotify:track:4uLU6hMCjMI75M1A2tKUQC");
        assert_eq!(a.format.as_deref(), Some("OGG_VORBIS_160"));
        assert_eq!(a.codec, "Ogg Vorbis");
        assert_eq!((a.kbps, a.requested_kbps), (Some(160), Some(320)));
        assert!(a.from_cache && !a.episode && a.album_gain && a.gain_data);
        assert!(a.below_requested());
        assert_eq!(a.gain_db, Some(-5.25));
        assert_eq!(a.sample_rate, 44_100);

        let episode = SpotifyUri::from_uri("spotify:episode:512ojhOuo1ktJprKbVcKyQ").unwrap();
        let podcast = AudioSource { format: Some(F::OGG_VORBIS_96), from_cache: false, ..source };
        let p = audio_info(&episode, podcast, None, false, false);
        assert!(p.episode);
        assert!(!p.below_requested(), "un pódcast a 96 kbps no es un aviso");
        assert_eq!(p.gain_db, None);

        let local = AudioSource { format: None, bits: Some(24), requested: None, from_cache: false, sample_rate: 44_100 };
        let l = audio_info(&track, local, Some(0.0), false, false);
        assert_eq!((l.codec.as_str(), l.kbps, l.bits, l.format), ("Archivo local", None, Some(24), None));
    }

    /// La ganancia que se enseña es la que se aplica, en cada nivel: por canción o por álbum,
    /// «sin datos» cuando la canción no trae volumen y nada con la normalización apagada.
    #[test]
    fn normalizacion_que_se_ensena() {
        let shown = |s: &Settings, data: NormalisationData, album: bool| {
            let mut config = PlayerConfig::default();
            config.set_tuning(&tuning(s));
            config.normalisation_type = if album { NormalisationType::Album } else { NormalisationType::Track };
            let factor = NormalisationData::get_factor(&config, data);
            NormalisationData::summary(&config, data, factor)
        };
        let normal = with_level(Loudness::Normal);
        let (db, album, data) = shown(&normal, track(-6.0, 1.0), false);
        assert!(close(db.unwrap(), -6.0) && !album && data);
        // Normal no deja pasar el pico de −1 dBFS: +6 dB con pico 0,5 se queda en +5,02.
        let (db, _, _) = shown(&normal, track(6.0, 0.5), false);
        assert!(close(db.unwrap(), 5.021), "{db:?}");
        // Alto: +3 sobre la ganancia; Bajo: −5.
        assert!(close(shown(&with_level(Loudness::Loud), track(-6.0, 1.0), false).0.unwrap(), -3.0));
        assert!(close(shown(&with_level(Loudness::Quiet), track(-6.0, 1.0), false).0.unwrap(), -11.0));
        // Álbum en orden: la del álbum.
        let both = NormalisationData { track_gain_db: 4.0, track_peak: 0.5, album_gain_db: -2.0, album_peak: 1.0 };
        let (db, album, data) = shown(&normal, both, true);
        assert!(close(db.unwrap(), -2.0) && album && data);
        // Sin datos de volumen: 0 dB y marcado.
        assert_eq!(shown(&normal, NormalisationData::default(), false), (Some(0.0), false, false));
        // Apagada: sin ganancia ni álbum.
        let off = Settings { normalisation: false, ..Settings::default() };
        assert_eq!(shown(&off, track(-8.0, 1.0), false), (None, false, true));
    }

    /// La configuración que se cambia en vivo vuelve a leerse igual (nada se pierde por el camino).
    #[test]
    fn tuning_ida_y_vuelta() {
        for l in [Loudness::Loud, Loudness::Normal, Loudness::Quiet] {
            let t = tuning(&with_level(l));
            let mut config = PlayerConfig::default();
            config.set_tuning(&t);
            assert_eq!(config.tuning(), t);
        }
    }

    /// La copia de la salida para las pruebas: f32 little-endian intercaladas, en orden y sin
    /// recortar (un pico de 1,2 se ve como 1,2), añadidas al archivo paquete a paquete.
    #[test]
    fn copia_pcm_de_la_salida() {
        let mut buf = vec![0xAA; 3];
        pcm_tap_bytes(&[0.5, -1.0, 1.25, 0.0], &mut buf);
        assert_eq!(buf.len(), 16, "el búfer se vacía antes");
        let back: Vec<f32> = buf.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        assert_eq!(back, [0.5, -1.0, 1.25, 0.0]);
        pcm_tap_bytes(&[], &mut buf);
        assert!(buf.is_empty());

        let path = std::env::temp_dir().join(format!("nanofy-test-pcm-{}.f32", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&path).unwrap();
        let mut tap = PcmTap { file, buf: Vec::new() };
        assert!(tap.write(&[0.25, -0.25]));
        assert!(tap.write(&[1.5, -2.0]));
        drop(tap);
        let bytes = std::fs::read(&path).unwrap();
        let got: Vec<f32> = bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        assert_eq!(got, [0.25, -0.25, 1.5, -2.0]);
        let _ = std::fs::remove_file(&path);
    }

    /// El fundido llega a la interfaz con las uris de las dos canciones y su duración; la carga
    /// de la siguiente durante un fundido no enseña «cargando».
    #[test]
    fn eventos_del_fundido_a_la_interfaz() {
        use librespot_core::SpotifyUri;
        let a = SpotifyUri::from_uri("spotify:track:4uLU6hMCjMI75M1A2tKUQC").unwrap();
        let b = SpotifyUri::from_uri("spotify:track:0DiWol3AO6WpXZgp0goxAV").unwrap();
        match map_event(PlayerEvent::CrossfadeStarted { from: a.clone(), to: b.clone(), fade_ms: 5980 }) {
            Some(Event::Crossfade { from, to, ms }) => {
                assert_eq!(from, "spotify:track:4uLU6hMCjMI75M1A2tKUQC");
                assert_eq!(to, "spotify:track:0DiWol3AO6WpXZgp0goxAV");
                assert_eq!(ms, 5980);
            }
            other => panic!("{other:?}"),
        }
        let loading = |crossfading| PlayerEvent::Loading { play_request_id: 1, track_id: b.clone(), position_ms: 0, crossfading };
        assert!(map_event(loading(true)).is_none());
        assert!(matches!(map_event(loading(false)), Some(Event::Loading)));
        // La propuesta a Spirc es interna: la interfaz no la ve.
        let ready = PlayerEvent::CrossfadeReady {
            play_request_id: 1,
            track_id: a,
            next_track_id: b,
            fade_ms: 6000,
            album_continuation: false,
            crossfade_albums: false,
        };
        assert!(map_event(ready).is_none());
    }

    /// Clave negada dos veces con el código pasajero (0x0002) y concedida al tercer intento: la
    /// canción carga. Es la lógica de reintentos de `AudioKeyManager::request` con sus errores de
    /// verdad, sin sesión: el «servidor» de prueba contesta en orden.
    #[tokio::test]
    async fn clave_negada_dos_veces_y_concedida_al_tercer_intento() {
        use librespot_core::audio_key::{classify, AudioKey, AudioKeyError};
        use librespot_core::key_policy::{with_retries, ATTEMPTS};
        use librespot_core::Error;

        // Contesta, en orden, lo de `respuestas` (código de negativa o `None` = sin respuesta) y
        // después concede. Devuelve el resultado y los intentos hechos.
        async fn pide(respuestas: &[Option<u16>]) -> (Result<AudioKey, Error>, usize) {
            let mut intentos = 0;
            let r = with_retries(ATTEMPTS, Duration::from_millis(1), classify, |n| {
                intentos += 1;
                let r = match respuestas.get(n as usize) {
                    Some(Some(code)) => Err(AudioKeyError::AesKey(*code).into()),
                    Some(None) => Err(AudioKeyError::Timeout.into()),
                    None => Ok(AudioKey([9; 16])),
                };
                async move { r }
            })
            .await;
            (r, intentos)
        }

        let (r, intentos) = pide(&[Some(2), Some(2)]).await;
        assert_eq!(r.unwrap(), AudioKey([9; 16]));
        assert_eq!(intentos, 3);

        // Sin respuesta también se reintenta; el tercer fallo seguido ya no.
        let (r, intentos) = pide(&[None, Some(2), None]).await;
        let e = r.unwrap_err();
        assert_eq!(intentos, 3);
        assert_eq!(LoadFailure::from_key_error(&e), LoadFailure::KeyTimeout);

        // Una negativa definitiva vuelve al momento, sin esperar.
        let (r, intentos) = pide(&[Some(1), Some(2)]).await;
        assert_eq!(intentos, 1);
        let e = r.unwrap_err();
        assert!(matches!(AudioKeyError::of(&e), Some(AudioKeyError::AesKey(1))));
        let fallo = LoadFailure::from_key_error(&e);
        assert_eq!(fallo, LoadFailure::KeyDenied(1));
        assert!(!fallo.is_transient(), "otro código es definitivo para ese fichero");

        // Agotados los intentos con el código 2: un fallo pasajero, que no salta la canción.
        let (r, _) = pide(&[Some(2); 3]).await;
        let fallo = LoadFailure::from_key_error(&r.unwrap_err());
        assert_eq!(fallo, LoadFailure::KeyDenied(2));
        assert!(fallo.is_transient());
    }

    /// Qué fallos de carga son pasajeros (la canción queda en pausa y se reintenta) y cuáles
    /// definitivos (se marca y se salta).
    #[test]
    fn fallos_de_carga_tipados() {
        use librespot_core::Error;
        assert!(LoadFailure::KeyDenied(2).is_transient());
        assert!(LoadFailure::KeyTimeout.is_transient());
        assert!(LoadFailure::RateLimited.is_transient());
        assert!(LoadFailure::Network("cdn".into()).is_transient());
        assert!(!LoadFailure::KeyDenied(1).is_transient());
        assert!(!LoadFailure::NotAvailable("país".into()).is_transient());
        assert!(!LoadFailure::NoFormat.is_transient());
        assert!(!LoadFailure::Decode("roto".into()).is_transient());

        // Metadatos: el límite (o un 429) es pasajero; un 404, que la canción no existe.
        let limite = Error::resource_exhausted("rate limited");
        assert_eq!(LoadFailure::from_metadata_error(&limite), LoadFailure::RateLimited);
        assert!(matches!(LoadFailure::from_metadata_error(&Error::not_found("404")), LoadFailure::NotAvailable(_)));
        assert!(matches!(LoadFailure::from_metadata_error(&Error::unavailable("503")), LoadFailure::Network(_)));
        assert!(matches!(LoadFailure::from_metadata_error(&Error::deadline_exceeded("t")), LoadFailure::Network(_)));
        // Spotify contestó sin datos de esa canción (no existe): definitivo, no «reintentando».
        let sin_datos: Error = librespot_core::spclient::SpClientError::ExpectedEntry("data").into();
        let fallo = LoadFailure::from_metadata_error(&sin_datos);
        assert!(matches!(fallo, LoadFailure::NotAvailable(_)), "{fallo:?}");
        assert!(!fallo.is_transient());
        // Una respuesta vacía de otra forma puede ser un fallo del servidor: se reintenta.
        let vacia: Error = librespot_core::spclient::SpClientError::NoData.into();
        assert!(matches!(LoadFailure::from_metadata_error(&vacia), LoadFailure::Network(_)));
        // El fichero: un 404 de la CDN suele ser un enlace caducado, no la canción.
        assert!(matches!(LoadFailure::from_file_error("fichero", &Error::not_found("404")), LoadFailure::Network(_)));
        assert_eq!(LoadFailure::from_file_error("fichero", &limite), LoadFailure::RateLimited);
        // Un error que no es de la clave (la sesión se cerró) es la conexión.
        assert!(matches!(LoadFailure::from_key_error(&Error::aborted("canal")), LoadFailure::Network(_)));

        assert_eq!(LoadFailure::KeyDenied(2).describe(), "clave: denegada (0x0002)");
        assert_eq!(LoadFailure::KeyTimeout.to_string(), "clave: sin respuesta");
    }

    /// Los fallos de carga y la cascada detenida llegan a la interfaz con la uri, el motivo y si
    /// la canción iba a sonar.
    #[test]
    fn fallos_de_carga_a_la_interfaz() {
        use librespot_core::SpotifyUri;
        let a = SpotifyUri::from_uri("spotify:track:4uLU6hMCjMI75M1A2tKUQC").unwrap();
        let ev = PlayerEvent::LoadFailed {
            play_request_id: 3,
            track_id: a.clone(),
            reason: LoadFailure::KeyDenied(2),
            transient: true,
            play: true,
        };
        assert_eq!(ev.get_play_request_id(), Some(3));
        match map_event(ev) {
            Some(Event::LoadFailed { uri, reason, transient, play }) => {
                assert_eq!(uri, "spotify:track:4uLU6hMCjMI75M1A2tKUQC");
                assert_eq!(reason, LoadFailure::KeyDenied(2));
                assert!(transient);
                assert!(play);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            map_event(PlayerEvent::Unavailable { play_request_id: 3, track_id: a }),
            Some(Event::Unavailable)
        ));
        assert!(matches!(
            map_event(PlayerEvent::SkipCascade { failed: 5 }),
            Some(Event::SkipCascade { failed: 5 })
        ));
    }

    /// Un corte de la red a media canción llega a la interfaz como pausa en su segundo, sin salto.
    #[test]
    fn corte_de_red_a_la_interfaz() {
        use librespot_core::SpotifyUri;
        let a = SpotifyUri::from_uri("spotify:track:4uLU6hMCjMI75M1A2tKUQC").unwrap();
        let ev = PlayerEvent::Stalled { play_request_id: 9, track_id: a, position_ms: 133_400 };
        // Spirc lo filtra por petición, como la pausa.
        assert_eq!(ev.get_play_request_id(), Some(9));
        match map_event(ev) {
            Some(Event::Stalled { uri, position_ms }) => {
                assert_eq!(uri, "spotify:track:4uLU6hMCjMI75M1A2tKUQC");
                assert_eq!(position_ms, 133_400);
            }
            other => panic!("{other:?}"),
        }
    }

    /// Una cuenta sin Premium ya no cierra la aplicación (antes `exit(1)` sin ningún mensaje):
    /// se anota y se sabe por el tipo de cuenta. Si esto saliera del proceso, la prueba moriría.
    #[test]
    fn cuenta_sin_premium_no_cierra_la_app() {
        use librespot_core::session::UserAttributes;
        let attrs = |t: &str| UserAttributes::from([("type".to_string(), t.to_string())]);
        assert!(!librespot_core::Session::check_catalogue(&attrs("free")));
        assert!(!librespot_core::Session::check_catalogue(&attrs("open")));
        assert!(librespot_core::Session::check_catalogue(&attrs("premium")));
        // Sin tipo todavía (o sin ese atributo): no se bloquea nada.
        assert!(librespot_core::Session::check_catalogue(&UserAttributes::new()));
    }

    /// Sin Premium solo se ignoran las órdenes que harían sonar algo aquí.
    #[test]
    fn ordenes_que_reproducen() {
        for c in [
            Cmd::Play,
            Cmd::Reload,
            Cmd::PlayPause,
            Cmd::Next { auto: false },
            Cmd::Prev,
            Cmd::Seek(1000),
            Cmd::TransferHere,
            Cmd::Preload("spotify:track:x".into()),
            Cmd::ResumeSession,
            Cmd::LoadTracks { uris: vec![], index: None, shuffle: false, resume: None, first: None },
            Cmd::LoadContext { uri: "spotify:album:x".into(), track_uri: None, index: None, shuffle: false, resume: Some(0), first: None },
        ] {
            assert!(is_playback(&c), "{c:?}");
        }
        for c in [Cmd::Pause, Cmd::Volume(1), Cmd::Shuffle(true), Cmd::Stalled, Cmd::RetryLoad(None), Cmd::Logout] {
            assert!(!is_playback(&c), "{c:?}");
        }
    }

    /// La precarga inteligente: ráfaga, una ficha más cada tanto (sin pasar de la ráfaga) y
    /// silencio tras un «demasiadas peticiones».
    #[test]
    fn presupuesto_de_la_precarga() {
        let t0 = std::time::Instant::now();
        let mut b = WarmBudget::new(t0);
        // Ráfaga de 8 en spclient (metadatos y ubicación comparten cubo) y de 2 claves.
        for i in 0..8 {
            let step = if i % 2 == 0 { WarmStep::Metadata } else { WarmStep::Storage };
            assert!(b.allow(step, t0), "{i}");
        }
        assert!(!b.allow(WarmStep::Metadata, t0));
        for _ in 0..2 {
            assert!(b.allow(WarmStep::Key, t0));
        }
        assert!(!b.allow(WarmStep::Key, t0));
        // Una ficha cada 2 s en spclient y cada 10 s en claves.
        let t2 = t0 + WARM_SPCLIENT_EVERY;
        assert!(b.allow(WarmStep::Storage, t2));
        assert!(!b.allow(WarmStep::Storage, t2));
        assert!(!b.allow(WarmStep::Key, t2));
        assert!(b.allow(WarmStep::Key, t0 + WARM_KEY_EVERY + Duration::from_millis(1)));
        // Mucho tiempo después no hay más que la ráfaga.
        let late = t0 + Duration::from_secs(3600);
        for _ in 0..8 {
            assert!(b.allow(WarmStep::Metadata, late));
        }
        assert!(!b.allow(WarmStep::Metadata, late));
        // Tras frenar, nada durante un minuto aunque haya fichas.
        let mut b = WarmBudget::new(t0);
        b.back_off(t0);
        assert!(b.quiet(t0));
        assert!(!b.allow(WarmStep::Metadata, t0 + WARM_BACKOFF - Duration::from_secs(1)));
        assert!(b.allow(WarmStep::Metadata, t0 + WARM_BACKOFF));
    }

    /// Dos minutos pasando el ratón por 40 filas (una cada 0,5 s, cada una con metadatos y
    /// ubicación por pedir): la precarga no pasa de su ráfaga más una ficha cada 2 s, muy lejos del
    /// límite de librespot (300 cada 30 s).
    #[test]
    fn pasar_el_raton_no_gasta_el_cupo() {
        let t0 = std::time::Instant::now();
        let mut b = WarmBudget::new(t0);
        let mut sent = 0;
        for i in 0..240u64 {
            let now = t0 + Duration::from_millis(500 * i);
            for step in [WarmStep::Metadata, WarmStep::Storage] {
                if b.allow(step, now) {
                    sent += 1;
                }
            }
        }
        // 8 de ráfaga + 120 s / 2 s = 68 como mucho en dos minutos.
        assert!(sent <= 68, "{sent}");
        // En cualquier ventana de 30 s, muy por debajo de las 300 del limitador.
        assert!(sent < 100);
        // Y las claves por adelantado (filas con el fichero ya en el disco): 2 + 120 s / 10 s.
        let mut keys = 0;
        for i in 0..240u64 {
            if b.allow(WarmStep::Key, t0 + Duration::from_millis(500 * i)) {
                keys += 1;
            }
        }
        assert!(keys <= 14, "{keys}");
    }

    /// El equipo durmió si la espera hasta el vistazo de salud (cada 30 s) duró 45 s o más en
    /// cualquiera de los dos relojes; un vistazo puntual o con algo de retraso no lo es.
    #[test]
    fn deteccion_de_suspension() {
        let s = Duration::from_secs;
        assert!(!slept(s(30), Some(s(30))));
        assert!(!slept(s(44), Some(s(31))));
        // Windows sin contar el tiempo dormido en el reloj monótono: lo ve la hora del sistema.
        assert!(slept(s(30), Some(s(600))));
        // La hora del sistema fue hacia atrás (sincronización): manda el monótono.
        assert!(slept(s(120), None));
        assert!(!slept(s(30), None));
        assert!(slept(SLEEP_GAP, Some(s(30))));
    }

    /// El volumen se pasa a Spirc tras activar solo si cambió desde la última vez en esta
    /// conexión; uno pedido fuera de una carga (quizá ignorado) se vuelve a pasar, y una
    /// conexión nueva lo recibe siempre.
    #[test]
    fn volumen_tras_activar_solo_si_cambio() {
        let mut v = VolumeSync::default();
        // Sin volumen pedido no se envía nada.
        assert_eq!(v.pending(1), None);

        v.wanted = Some(30_000);
        assert_eq!(v.pending(1), Some(30_000));
        v.applied(1, 30_000);
        // Misma conexión y mismo valor: no se repite en cada carga.
        assert_eq!(v.pending(1), None);
        assert_eq!(v.pending(1), None);

        // La interfaz lo cambia (`Cmd::Volume`): la carga siguiente lo pasa otra vez, por si
        // llegó con el dispositivo inactivo.
        v.wanted = Some(40_000);
        assert_eq!(v.pending(1), Some(40_000));
        v.applied(1, 40_000);
        assert_eq!(v.pending(1), None);

        // Volver al valor de antes también es un cambio.
        v.wanted = Some(30_000);
        assert_eq!(v.pending(1), Some(30_000));
        v.applied(1, 30_000);

        // Reconexión o reinicio: Spirc nuevo, sin nada aplicado.
        assert_eq!(v.pending(2), Some(30_000));
        v.applied(2, 30_000);
        assert_eq!(v.pending(2), None);

        // Silencio (0) es un volumen como otro.
        v.wanted = Some(0);
        assert_eq!(v.pending(2), Some(0));
    }

    #[test]
    fn cancelar_el_inicio_de_sesion_corta_la_espera_de_librespot_oauth() {
        assert!(REDIRECT_URI.contains(&format!(":{LOGIN_PORT}/")), "LOGIN_PORT debe ser el puerto de REDIRECT_URI");
        // Un servidor como el de librespot-oauth (en otro puerto: el de verdad podría estar
        // esperando un inicio de sesión de la app abierta): atiende la primera conexión, lee la
        // línea de la petición y contesta.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            use std::io::{BufRead, Write};
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            std::io::BufReader::new(&stream).read_line(&mut line).unwrap();
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok");
            line
        });
        assert!(cancel_listener_at(port));
        let line = server.join().unwrap();
        assert_eq!(line.trim_end(), "GET /login?error=access_denied HTTP/1.1");
        // librespot-oauth no encuentra el código en esa URL: eso es una cancelación, no un error.
        let uri = format!("http://localhost{}", line.split_whitespace().nth(1).unwrap());
        assert_eq!(login_error(librespot_oauth::OAuthError::AuthCodeNotFound { uri }), LOGIN_CANCELLED);
        // Igual si se cancela en la propia página de Spotify (trae también el state).
        let uri = "http://localhost/login?error=access_denied&state=x".to_string();
        assert_eq!(login_error(librespot_oauth::OAuthError::AuthCodeNotFound { uri }), LOGIN_CANCELLED);
        // Lo demás es un fallo de verdad, con su texto.
        let uri = "http://localhost/login".to_string();
        assert_ne!(login_error(librespot_oauth::OAuthError::AuthCodeNotFound { uri }), LOGIN_CANCELLED);
        assert_ne!(login_error(librespot_oauth::OAuthError::AuthCodeListenerTerminated), LOGIN_CANCELLED);
    }
}
