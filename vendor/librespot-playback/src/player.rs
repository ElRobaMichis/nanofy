use std::{
    collections::HashMap,
    fmt, fs,
    fs::File,
    future::Future,
    io::{self, Read, Seek, SeekFrom},
    mem,
    pin::Pin,
    process::exit,
    sync::Mutex,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    thread,
    time::{Duration, Instant},
};

#[cfg(feature = "passthrough-decoder")]
use crate::decoder::PassthroughDecoder;
use crate::{
    audio::{AudioDecrypt, AudioFetchParams, AudioFile, AudioFileError, StreamLoaderController},
    audio_backend::{Sink, out_queue},
    config::{AudioTuning, Bitrate, NormalisationMethod, NormalisationType, PlayerConfig},
    convert::Converter,
    core::{
        Error, FileId, Session, SpotifyId, SpotifyUri, audio_key,
        cdn_url::{self, CdnUrl},
        error::ErrorKind,
        fault,
        key_policy::{self, Attempt},
        spclient, ttfs,
        util::SeqGenerator,
    },
    crossfade::{self, Crossfader, IncomingStart, LimiterState, PacketSource, Ramp},
    decoder::{AudioDecoder, AudioPacket, AudioPacketPosition, DecoderError, SymphoniaDecoder},
    gain::{self, GainRamp},
    local_file::{LocalFileLookup, create_local_file_lookup},
    metadata::audio::{AudioFileFormat, AudioFiles, AudioItem},
    mixer::VolumeGetter,
};
use futures_util::{
    FutureExt, StreamExt, future::FusedFuture,
    stream::futures_unordered::FuturesUnordered,
};
use librespot_metadata::{audio::UniqueFields, track::Tracks};

use symphonia::core::io::MediaSource;
use symphonia::core::probe::Hint;
use tokio::sync::{mpsc, oneshot};

use crate::{NUM_CHANNELS, SAMPLE_RATE, SAMPLES_PER_SECOND};

/// Cómo empieza una canción que se carga (`Player::load_with_transition`).
pub use crate::crossfade::Transition;

const PRELOAD_NEXT_TRACK_BEFORE_END_DURATION_MS: u32 = 30000;
/// Lo que se guarda una canción preparada por adelantado (`Player::warm`) que nadie pide: luego se
/// suelta (su fichero a medio bajar y su decodificador).
const WARM_TTL: Duration = Duration::from_secs(90);
/// Precarga temprana: tras este tiempo sonando, sin esperar al final (ver `poll`).
const PRELOAD_NEXT_TRACK_AFTER_LISTENING_MS: u32 = 5000;
pub const DB_VOLTAGE_RATIO: f64 = 20.0;
pub const PCM_AT_0DBFS: f64 = 1.0;

// Spotify inserts a custom Ogg packet at the start with custom metadata values, that you would
// otherwise expect in Vorbis comments. This packet isn't well-formed and players may balk at it.
const SPOTIFY_OGG_HEADER_END: u64 = 0xa7;

const LOAD_HANDLES_POISON_MSG: &str = "load handles mutex should not be poisoned";

pub type PlayerResult = Result<(), Error>;

pub struct Player {
    commands: Option<mpsc::UnboundedSender<PlayerCommand>>,
    thread_handle: Option<thread::JoinHandle<()>>,
}

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum SinkStatus {
    Running,
    Closed,
    TemporarilyClosed,
}

pub type SinkEventCallback = Box<dyn Fn(SinkStatus) + Send>;

struct PlayerInternal {
    session: Session,
    config: PlayerConfig,
    commands: mpsc::UnboundedReceiver<PlayerCommand>,
    load_handles: Arc<Mutex<HashMap<thread::ThreadId, thread::JoinHandle<()>>>>,

    state: PlayerState,
    preload: PlayerPreload,
    /// Canción preparada por adelantado (`Player::warm`): la que se acaba de pulsar o se está
    /// pulsando. Va aparte de `preload` (la siguiente de la cola, para gapless), que así no se
    /// pierde; la carga de esa canción la toma de aquí.
    warm: PlayerPreload,
    /// Cuándo se pidió lo que hay en `warm` (ver `WARM_TTL`).
    warm_since: Instant,
    /// La última precarga que falló y cuándo (ver `PRELOAD_RETRY_AFTER`).
    preload_failed: Option<(SpotifyUri, Instant)>,
    sink: Box<dyn Sink>,
    sink_status: SinkStatus,
    /// Audio que la salida aún tenía en cola sin sonar cuando se pausó la canción (`Paused`). El
    /// decodificador va por delante de lo que se oye en esa cola: la posición de la pausa es la
    /// suya menos esto, y si la cola se pierde en la pausa (se suelta la salida), al reanudar se
    /// vuelve a decodificar desde ahí (`handle_play`). 0 si la cola se vació (buscar, cargar).
    paused_queue_ms: u32,
    sink_event_callback: Option<SinkEventCallback>,
    volume_getter: Box<dyn VolumeGetter + Send>,
    event_senders: Vec<mpsc::UnboundedSender<PlayerEvent>>,
    converter: Converter,

    normalisation_integrators: [f64; 2],
    normalisation_peaks: [f64; 2],
    normalisation_channel: usize,
    normalisation_knee_factor: f64,
    /// Paso suave de la ganancia tras un cambio de nivel en vivo (`SetAudioTuning`); su destino
    /// es el `normalisation_factor` de la canción que suena.
    gain_ramp: Option<GainRamp>,

    auto_normalise_as_album: bool,

    /// Fundido entre canciones (ver `crossfade.rs`). Con el fundido apagado nunca se activa, y
    /// mientras no está activo nada de lo que suena pasa por él.
    crossfader: Crossfader<OutgoingTrack, SpotifyUri>,
    /// `play_request_id` de la canción para la que ya se decidió si proponer un fundido: se
    /// propone una sola vez por canción (otra vez tras buscar, cambiar la repetición o los ajustes).
    crossfade_requested: Option<u64>,
    /// Lo propuesto a Spirc (`CrossfadeReady`), para empezar el fundido cuando llegue su carga.
    crossfade_plan: Option<FadePlan>,
    /// Cómo se decidió el fundido que acaba de empezar, para el registro al entrar la siguiente.
    crossfade_log: Option<FadeLog>,
    /// Marcos escritos en la salida: sitúa en el registro el principio y el final de cada fundido.
    frames_out: u64,
    /// La carga en curso se anunció con `Loading { crossfading: true }` (la interfaz no enseña
    /// «cargando» porque la anterior aún suena). Si la anterior se apaga del todo o la carga falla
    /// antes de que esta suene, se anuncia de verdad (`reveal_loading`): sin eso la barra seguiría
    /// en «sonando» en silencio, y un fallo sin nada detrás la dejaría así para siempre.
    loading_hidden: bool,
    /// Medida del tiempo hasta el primer sonido (`ttfs`) de la carga en curso, si es la de una
    /// orden de la interfaz; 0 si no (precarga, paso automático a la siguiente…).
    ttfs_seq: u64,
    /// La canción en pausa porque la red se cortó a media reproducción (`PlayerEvent::Stalled`).
    stalled: Option<StallState>,

    player_id: usize,
    play_request_id_generator: SeqGenerator<u64>,
    last_progress_update: Instant,

    local_file_lookup: Arc<LocalFileLookup>,
}

/// La canción que se va durante un fundido: su decodificador y, mientras él viva, el cargador de
/// su fichero (soltarlo antes cerraría la descarga) y su id para los registros.
struct OutgoingTrack {
    track_id: SpotifyUri,
    decoder: Decoder,
    _stream_loader_controller: StreamLoaderController,
}

impl PacketSource for OutgoingTrack {
    fn next_samples(&mut self) -> Option<Vec<f64>> {
        match self.decoder.next_packet() {
            Ok(Some((_, AudioPacket::Samples(samples)))) => Some(samples),
            Ok(Some((_, AudioPacket::Raw(_)))) | Ok(None) => None,
            Err(e) => {
                // Lo que quedaba ya iba apagándose: la saliente termina aquí, sin saltar de canción.
                warn!("[fundido] no se pudo seguir decodificando <{}>: {e}", self.track_id);
                None
            }
        }
    }
}

/// Fundido propuesto a Spirc y aún sin respuesta.
struct FadePlan {
    play_request_id: u64,
    next: SpotifyUri,
    fade_ms: u32,
}

/// Lo que se registra de un fundido al empezar a sonar la siguiente: lo pedido en los ajustes, lo
/// previsto al proponerlo y lo que de verdad le quedaba a la canción cuando llegó la carga.
struct FadeLog {
    requested_ms: u32,
    planned_ms: u32,
    remaining_ms: u32,
}

/// Una canción que se quedó sin datos a media reproducción porque la red se cortó. Está en pausa
/// (`PlayerState::Paused`) en lo último que se oyó, y se reanuda desde ahí al volver la red en vez
/// de saltar a la siguiente, que es lo que hacía librespot (y en cascada: la siguiente tampoco
/// llegaba a cargar sin red).
struct StallState {
    play_request_id: u64,
    /// Lo que se llegó a oír. Al reanudar se vuelve a buscar ahí: el lector del Ogg se quedó a
    /// medias de una página cuando falló la lectura y no se puede seguir sin volver a situarlo.
    position_ms: u32,
    /// Comprobación en curso de que vuelven a llegar datos (en un hilo aparte: la espera llega a
    /// `download_timeout` y el reproductor tiene que seguir atendiendo pausa y siguiente).
    /// `true` si llegaron.
    probe: Option<oneshot::Receiver<bool>>,
    /// Se pidió reanudar: suena en cuanto la comprobación diga que hay datos.
    resume: bool,
}

/// Comprueba en un hilo aparte si vuelven a llegar datos del fichero en el punto en que se cortó:
/// pide lo de los próximos segundos y espera a tener lo justo para seguir (como tras una búsqueda,
/// `preload_data_before_playback`). Sin red la espera acaba en `download_timeout`.
fn probe_stalled_stream(
    controller: StreamLoaderController,
    bytes_per_second: usize,
) -> oneshot::Receiver<bool> {
    let (tx, rx) = oneshot::channel();
    let params = AudioFetchParams::get();
    let request = (params.read_ahead_during_playback.as_secs_f32() * bytes_per_second as f32) as usize;
    let wait = (params.read_ahead_before_playback.as_secs_f32() * bytes_per_second as f32) as usize;
    let timeout = params.download_timeout;
    let spawned = thread::Builder::new()
        .name("nanofy-stall".into())
        .spawn(move || {
            // Corte simulado (`NANOFY_FAULT=cdn_stall_ms`): la descarga de verdad no se entera,
            // así que se espera aquí lo que le quede, como esperaría una lectura.
            if let Some(left) = fault::cdn_stall_left() {
                thread::sleep(left.min(timeout));
                if left > timeout {
                    let _ = tx.send(false);
                    return;
                }
            }
            let back = match controller.fetch_next_and_wait(request, wait) {
                Ok(()) => true,
                Err(e) => {
                    debug!("sigue sin llegar audio tras el corte: {e}");
                    false
                }
            };
            let _ = tx.send(back);
        });
    if let Err(e) = spawned {
        // Sin hilo, el canal se cierra y cuenta como «aún no hay datos»: se reintentará.
        warn!("no se pudo comprobar si vuelve la red: {e}");
    }
    rx
}

/// El limitador de la mezcla de dos canciones, con los tiempos de ataque y liberación de la
/// normalización y su estado a cero (uno nuevo en cada fundido).
fn mix_limiter(config: &PlayerConfig) -> LimiterState {
    LimiterState::new(
        crossfade::MIX_LIMIT_THRESHOLD_DBFS,
        crossfade::MIX_LIMIT_KNEE_DB,
        config.normalisation_attack_cf,
        config.normalisation_release_cf,
    )
}

/// Cómo terminó la petición de clave (ya con sus reintentos), para la marca «key» de `ttfs`.
/// `from_cache`: la clave ya estaba en la caché en memoria y no se pidió.
fn key_info<T>(key: &Result<T, Error>, from_cache: bool) -> String {
    match key {
        Ok(_) if from_cache => "cache".into(),
        Ok(_) => "ok".into(),
        Err(e) => match audio_key::classify(e) {
            Attempt::Timeout => "timeout".into(),
            Attempt::Denied(code) => format!("denegada {code:#06x}"),
            Attempt::Other => "error".into(),
        },
    }
}

/// Tras una precarga fallida, la misma canción no se vuelve a precargar hasta pasado esto: Spirc
/// la pide de nuevo con cada aviso de estado, y con Spotify frenando las claves cada intento
/// solo añadiría carga. Cuando le toque sonar se carga igualmente.
const PRELOAD_RETRY_AFTER: Duration = Duration::from_secs(60);

/// Por qué no se pudo cargar una canción (`PlayerEvent::LoadFailed`). Los fallos pasajeros
/// (Spotify frenando las claves o las peticiones, la red) no marcan la canción como no
/// disponible ni saltan a la siguiente: se queda en pausa y se reintenta. Los definitivos sí.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadFailure {
    /// No está disponible en este país, se retiró o no tiene ninguna alternativa (el motivo).
    NotAvailable(String),
    /// No hay ningún fichero en un formato que se pueda reproducir.
    NoFormat,
    /// Spotify negó la clave del fichero con este código (0x0002: negativa pasajera).
    KeyDenied(u16),
    /// La clave no llegó a tiempo, ni en los reintentos.
    KeyTimeout,
    /// El límite de peticiones de librespot o un 429 de Spotify.
    RateLimited,
    /// Un fallo de red: metadatos, resolución del fichero o descarga (qué falló).
    Network(String),
    /// El fichero no se pudo leer ni decodificar, o un archivo local que no se puede abrir.
    Decode(String),
}

impl LoadFailure {
    /// ¿Merece la pena reintentar la misma canción más tarde (en vez de marcarla y saltarla)?
    pub fn is_transient(&self) -> bool {
        match self {
            LoadFailure::KeyDenied(code) => *code == key_policy::TRANSIENT_DENIAL,
            LoadFailure::KeyTimeout | LoadFailure::RateLimited | LoadFailure::Network(_) => true,
            LoadFailure::NotAvailable(_) | LoadFailure::NoFormat | LoadFailure::Decode(_) => false,
        }
    }

    /// Un fallo de la clave (tras sus reintentos).
    pub fn from_key_error(e: &Error) -> Self {
        match audio_key::classify(e) {
            Attempt::Denied(code) => LoadFailure::KeyDenied(code),
            Attempt::Timeout => LoadFailure::KeyTimeout,
            // La sesión se cerró o el canal se cortó: es la conexión, no la canción.
            Attempt::Other => LoadFailure::Network(format!("clave: {e}")),
        }
    }

    /// Un fallo al pedir los metadatos de la canción. Un 404 es que no existe; el resto (límite,
    /// tiempo agotado, servidor caído…) puede ir bien en el siguiente intento.
    pub fn from_metadata_error(e: &Error) -> Self {
        match e.kind {
            ErrorKind::ResourceExhausted => LoadFailure::RateLimited,
            ErrorKind::NotFound => LoadFailure::NotAvailable(format!("metadatos: {e}")),
            // Spotify contestó, pero sin datos de esa entidad (SpClientError::ExpectedEntry("data")):
            // no existe o no se puede ver. Reintentarlo a los 15 y 60 s dejaba la canción en
            // «reintentando» sin decir que no existe.
            ErrorKind::FailedPrecondition if e.to_string().contains("expected an entry to exist in data") => {
                LoadFailure::NotAvailable(format!("metadatos: {e}"))
            }
            _ => LoadFailure::Network(format!("metadatos: {e}")),
        }
    }

    /// Un fallo al resolver o descargar el fichero de audio: aquí incluso un 404 suele ser un
    /// enlace de la CDN caducado, así que solo el límite se distingue del resto de la red.
    pub fn from_file_error(what: &str, e: &Error) -> Self {
        match e.kind {
            ErrorKind::ResourceExhausted => LoadFailure::RateLimited,
            _ => LoadFailure::Network(format!("{what}: {e}")),
        }
    }

    /// Lo que se anota en el registro y en `ttfs` (el último error de carga).
    pub fn describe(&self) -> String {
        match self {
            LoadFailure::NotAvailable(reason) => format!("no disponible: {reason}"),
            LoadFailure::NoFormat => "sin formato compatible".into(),
            LoadFailure::KeyDenied(code) => format!("clave: denegada ({code:#06x})"),
            LoadFailure::KeyTimeout => "clave: sin respuesta".into(),
            LoadFailure::RateLimited => "límite de peticiones".into(),
            LoadFailure::Network(what) | LoadFailure::Decode(what) => what.clone(),
        }
    }
}

impl fmt::Display for LoadFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.describe())
    }
}

/// La carga de una canción en su hilo (`PlayerInternal::load_track`).
type Loader = Pin<Box<dyn FusedFuture<Output = Result<PlayerLoadedTrackData, LoadFailure>> + Send>>;

/// Detalle de la marca «seek» de `ttfs`: en 0 ms la carga ya no busca (ver `load_remote_track`),
/// y el informe debe distinguirlo de una búsqueda que tardó poco.
fn seek_mark(position_ms: u32) -> String {
    if position_ms == 0 {
        "0 ms (sin búsqueda)".to_string()
    } else {
        format!("{position_ms} ms")
    }
}

fn is_episode(uri: &SpotifyUri) -> bool {
    matches!(uri, SpotifyUri::Episode { .. })
}

static PLAYER_COUNTER: AtomicUsize = AtomicUsize::new(0);

enum PlayerCommand {
    Load {
        track_id: SpotifyUri,
        play: bool,
        position_ms: u32,
        transition: Transition,
    },
    Preload {
        track_id: SpotifyUri,
    },
    /// Ver `Player::warm`.
    Warm {
        track_id: SpotifyUri,
        for_play: bool,
    },
    Play,
    Pause,
    Stop,
    /// Suelta el dispositivo de salida si no está sonando (ver `Player::release_sink`).
    ReleaseSink,
    Seek(u32),
    SetSession(Session),
    AddEventSender(mpsc::UnboundedSender<PlayerEvent>),
    SetSinkEventCallback(Option<SinkEventCallback>),
    EmitVolumeChangedEvent(u16),
    SetAutoNormaliseAsAlbum(bool),
    /// Calidad, gapless y normalización nuevas sin reiniciar el reproductor.
    SetAudioTuning(AudioTuning),
    /// Duración del fundido (0 lo apaga) y si se funden pistas seguidas de un álbum, en vivo.
    SetCrossfade {
        ms: u32,
        albums: bool,
    },
    EmitSessionDisconnectedEvent {
        connection_id: String,
        user_name: String,
    },
    EmitSessionConnectedEvent {
        connection_id: String,
        user_name: String,
    },
    EmitSessionClientChangedEvent {
        client_id: String,
        client_name: String,
        client_brand_name: String,
        client_model_name: String,
    },
    EmitFilterExplicitContentChangedEvent(bool),
    EmitShuffleChangedEvent(bool),
    EmitJamQueueEvent {
        current: String,
        context: String,
        next: Vec<String>,
    },
    EmitClusterSnapshotEvent(Vec<u8>),
    EmitRepeatChangedEvent {
        context: bool,
        track: bool,
    },
    EmitAutoPlayChangedEvent(bool),
    /// Spirc se detuvo tras varios fallos seguidos (ver `PlayerEvent::SkipCascade`).
    EmitSkipCascadeEvent(u32),
}

#[derive(Debug, Clone)]
pub enum PlayerEvent {
    // Play request id changed
    PlayRequestIdChanged {
        play_request_id: u64,
    },
    // Fired when the player is stopped (e.g. by issuing a "stop" command to the player).
    Stopped {
        play_request_id: u64,
        track_id: SpotifyUri,
    },
    // The player is delayed by loading a track.
    Loading {
        play_request_id: u64,
        track_id: SpotifyUri,
        position_ms: u32,
        /// Mientras carga, la canción anterior sigue sonando y apagándose (un fundido cuya
        /// siguiente no estaba precargada): la interfaz no debe enseñar «cargando».
        crossfading: bool,
    },
    // The player is preloading a track.
    Preloading {
        track_id: SpotifyUri,
    },
    // The player is playing a track.
    // This event is issued at the start of playback of whenever the position must be communicated
    // because it is out of sync. This includes:
    // start of a track
    // un-pausing
    // after a seek
    // after a buffer-underrun
    Playing {
        play_request_id: u64,
        track_id: SpotifyUri,
        position_ms: u32,
    },
    // The player entered a paused state.
    Paused {
        play_request_id: u64,
        track_id: SpotifyUri,
        position_ms: u32,
    },
    // The player thinks it's a good idea to issue a preload command for the next track now.
    // This event is intended for use within spirc.
    TimeToPreloadNextTrack {
        play_request_id: u64,
        track_id: SpotifyUri,
    },
    // The player reached the end of a track.
    // This event is intended for use within spirc. Spirc will respond by issuing another command.
    EndOfTrack {
        play_request_id: u64,
        track_id: SpotifyUri,
    },
    // The player was unable to load the requested track.
    // Nanofy: solo tras un fallo definitivo (justo después de `LoadFailed`); Spirc marca la
    // canción y salta a la siguiente.
    Unavailable {
        play_request_id: u64,
        track_id: SpotifyUri,
    },
    /// No se pudo cargar la canción que se pidió (nunca por una precarga, que falla en silencio).
    /// Con un fallo definitivo le sigue `Unavailable`; con uno pasajero (Spotify frenando las
    /// claves, límite de peticiones, red) no: Spirc la deja en pausa en su posición, sin marcarla
    /// ni saltar, y el siguiente «reproducir» la vuelve a cargar.
    LoadFailed {
        play_request_id: u64,
        track_id: SpotifyUri,
        reason: LoadFailure,
        transient: bool,
        /// Iba a sonar al terminar de cargar (no era una carga en pausa): solo entonces tiene
        /// sentido reintentarla sola.
        play: bool,
    },
    /// Spirc dejó de saltar canciones y se detuvo: `failed` seguidas no se pudieron reproducir en
    /// poco tiempo (ver `cascade.rs` en librespot-connect).
    SkipCascade {
        failed: u32,
    },
    /// Nanofy: la red se cortó a media canción (la CDN dejó de entregar datos y el fichero no
    /// estaba descargado entero). En vez de dar la canción por terminada y saltar a la siguiente,
    /// queda en pausa en `position_ms`, lo último que se oyó; el próximo «reproducir» la reanuda
    /// desde ahí en cuanto vuelven a llegar datos. Si al pedirlo aún no llegan, se repite este
    /// evento (sigue en pausa).
    Stalled {
        play_request_id: u64,
        track_id: SpotifyUri,
        position_ms: u32,
    },
    // The mixer volume was set to a new level.
    VolumeChanged {
        volume: u16,
    },
    PositionCorrection {
        play_request_id: u64,
        track_id: SpotifyUri,
        position_ms: u32,
    },
    /// Requires `PlayerConfig::position_update_interval` to be set to Some.
    /// Once set this event will be sent periodically while playing the track to inform about the
    /// current playback position
    PositionChanged {
        play_request_id: u64,
        track_id: SpotifyUri,
        position_ms: u32,
    },
    Seeked {
        play_request_id: u64,
        track_id: SpotifyUri,
        position_ms: u32,
    },
    TrackChanged {
        audio_item: Box<AudioItem>,
    },
    SessionConnected {
        connection_id: String,
        user_name: String,
    },
    SessionDisconnected {
        connection_id: String,
        user_name: String,
    },
    SessionClientChanged {
        client_id: String,
        client_name: String,
        client_brand_name: String,
        client_model_name: String,
    },
    ShuffleChanged {
        shuffle: bool,
    },
    /// Cola compartida de una Jam (la reenvía Spirc cuando cambia siendo participante) para que la
    /// interfaz la muestre tal cual la sesión, en vez de la cola local de la cuenta.
    JamQueue {
        current: String,
        context: String,
        next: Vec<String>,
    },
    /// Clúster de Connect (protobuf `Cluster` sin parsear) tal como lo devuelve Spotify al
    /// registrar el dispositivo: lo último que quedó sonando o en pausa en la cuenta, con posición.
    ClusterSnapshot {
        cluster: Vec<u8>,
    },
    RepeatChanged {
        context: bool,
        track: bool,
    },
    AutoPlayChanged {
        auto_play: bool,
    },
    FilterExplicitContentChanged {
        filter: bool,
    },
    /// Qué suena de verdad y con qué volumen: al empezar cada canción (justo después de
    /// `TrackChanged`) y cuando la normalización cambia en vivo (`SetAudioTuning`). Nanofy lo
    /// enseña en la etiqueta de calidad de la barra; Spirc lo ignora.
    AudioFormat {
        play_request_id: u64,
        /// La pista que suena, la misma que trae `TrackChanged`: si Spotify la sustituyó por una
        /// alternativa (otra edición del mismo tema), es la alternativa y no la pedida.
        track_id: SpotifyUri,
        source: AudioSource,
        /// Ganancia de la normalización que se aplica, en dB; `None` con la normalización
        /// apagada. En el nivel «Alto» el limitador puede bajar además los picos.
        normalisation_db: Option<f64>,
        /// La ganancia es la del álbum (un álbum en orden) y no la de la canción.
        album_gain: bool,
        /// La canción trae datos de volumen; sin ellos no se toca (ganancia 0 dB).
        gain_data: bool,
    },
    /// Se acerca el final natural de la canción y la siguiente ya está precargada: el reproductor
    /// propone fundirlas, con la antelación justa para que el fundido dure lo pedido aunque la
    /// respuesta tarde. Lo decide Spirc (sabe del contexto, la repetición y la Jam): si acepta,
    /// carga la siguiente con `Transition::Crossfade`; si no, la canción sigue hasta el final y la
    /// siguiente entra sin hueco, como siempre.
    CrossfadeReady {
        play_request_id: u64,
        track_id: SpotifyUri,
        next_track_id: SpotifyUri,
        /// Duración prevista (con los topes de 12 s y media canción).
        fade_ms: u32,
        /// La siguiente parece la pista que sigue en el mismo álbum. Con «fundir también los
        /// álbumes» apagado ni se propone; viaja por si Spirc quiere decidir con él.
        album_continuation: bool,
        /// «Fundir también canciones seguidas de un mismo álbum» al proponerlo. Viaja con la
        /// propuesta para que Spirc aplique la regla del álbum con el contexto sin una copia
        /// propia del ajuste: el reproductor es el único que lo guarda (`set_crossfade`), también
        /// a través de las reconexiones, que crean un Spirc nuevo.
        crossfade_albums: bool,
    },
    /// Empezó un fundido: `to` ya es la canción que suena (llegaron `TrackChanged` y `Playing`) y
    /// `from` se apaga durante `fade_ms`.
    CrossfadeStarted {
        from: SpotifyUri,
        to: SpotifyUri,
        fade_ms: u32,
    },
}

/// De dónde sale el audio de la canción cargada: el fichero elegido y por qué. Viaja con la
/// canción (cargada, sonando, en pausa) para poder repetirlo en `PlayerEvent::AudioFormat` sin
/// volver a mirar los metadatos.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioSource {
    /// Formato del fichero de Spotify; `None` en archivos locales.
    pub format: Option<AudioFileFormat>,
    /// Bits por muestra del original cuando el formato los guarda (FLAC, WAV de archivos
    /// locales); `None` en los comprimidos con pérdida, que no tienen una profundidad fija.
    pub bits: Option<u32>,
    /// Frecuencia del original. El decodificador solo acepta 44,1 kHz, pero se guarda la real
    /// para no dar por hecho nada en la interfaz.
    pub sample_rate: u32,
    /// Calidad pedida al cargarla (la de los ajustes en ese momento); `None` en archivos
    /// locales. Tras cambiar la calidad en vivo, la canción que ya sonaba sigue con la anterior.
    pub requested: Option<Bitrate>,
    /// Se eligió una copia ya guardada en la caché en otra calidad, aunque la pedida existía
    /// (`pick_audio_file`): no es que la canción no esté disponible en esa calidad.
    pub from_cache: bool,
}

impl PlayerEvent {
    pub fn get_play_request_id(&self) -> Option<u64> {
        use PlayerEvent::*;
        match self {
            Loading {
                play_request_id, ..
            }
            | Unavailable {
                play_request_id, ..
            }
            | LoadFailed {
                play_request_id, ..
            }
            | Playing {
                play_request_id, ..
            }
            | TimeToPreloadNextTrack {
                play_request_id, ..
            }
            | EndOfTrack {
                play_request_id, ..
            }
            | Paused {
                play_request_id, ..
            }
            | Stopped {
                play_request_id, ..
            }
            | PositionCorrection {
                play_request_id, ..
            }
            | Seeked {
                play_request_id, ..
            }
            | AudioFormat {
                play_request_id, ..
            }
            | CrossfadeReady {
                play_request_id, ..
            }
            | Stalled {
                play_request_id, ..
            } => Some(*play_request_id),
            _ => None,
        }
    }
}

pub type PlayerEventChannel = mpsc::UnboundedReceiver<PlayerEvent>;

#[inline]
pub fn db_to_ratio(db: f64) -> f64 {
    f64::powf(10.0, db / DB_VOLTAGE_RATIO)
}

#[inline]
pub fn ratio_to_db(ratio: f64) -> f64 {
    ratio.log10() * DB_VOLTAGE_RATIO
}

pub fn duration_to_coefficient(duration: Duration) -> f64 {
    f64::exp(-1.0 / (duration.as_secs_f64() * SAMPLES_PER_SECOND as f64))
}

pub fn coefficient_to_duration(coefficient: f64) -> Duration {
    Duration::from_secs_f64(-1.0 / f64::ln(coefficient) / SAMPLES_PER_SECOND as f64)
}

#[derive(Clone, Copy, Debug)]
pub struct NormalisationData {
    // Spotify provides these as `f32`, but audio metadata can contain up to `f64`.
    // Also, this negates the need for casting during sample processing.
    pub track_gain_db: f64,
    pub track_peak: f64,
    pub album_gain_db: f64,
    pub album_peak: f64,
}

impl Default for NormalisationData {
    fn default() -> Self {
        Self {
            track_gain_db: 0.0,
            track_peak: 1.0,
            album_gain_db: 0.0,
            album_peak: 1.0,
        }
    }
}

impl NormalisationData {
    fn parse_from_ogg<T: Read + Seek>(mut file: T) -> io::Result<NormalisationData> {
        const SPOTIFY_NORMALIZATION_HEADER_START_OFFSET: u64 = 144;
        const NORMALISATION_DATA_SIZE: usize = 16;

        let newpos = file.seek(SeekFrom::Start(SPOTIFY_NORMALIZATION_HEADER_START_OFFSET))?;
        if newpos != SPOTIFY_NORMALIZATION_HEADER_START_OFFSET {
            error!(
                "NormalisationData::parse_from_file seeking to {SPOTIFY_NORMALIZATION_HEADER_START_OFFSET} but position is now {newpos}"
            );

            error!("Falling back to default (non-track and non-album) normalisation data.");

            return Ok(NormalisationData::default());
        }

        let mut buf = [0u8; NORMALISATION_DATA_SIZE];

        file.read_exact(&mut buf)?;

        let track_gain_db = f32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as f64;
        let track_peak = f32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as f64;
        let album_gain_db = f32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]) as f64;
        let album_peak = f32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]) as f64;

        Ok(Self {
            track_gain_db,
            track_peak,
            album_gain_db,
            album_peak,
        })
    }

    /// Factor de normalización de una canción con `config` (con «Auto» ya resuelto a álbum o
    /// canción). Público para que Nanofy pruebe sus niveles de punta a punta.
    pub fn get_factor(config: &PlayerConfig, data: NormalisationData) -> f64 {
        if !config.normalisation {
            return 1.0;
        }

        let (gain_db, gain_peak) = if config.normalisation_type == NormalisationType::Album {
            (data.album_gain_db, data.album_peak)
        } else {
            (data.track_gain_db, data.track_peak)
        };

        // Sin datos de volumen no se sabe cuánto suena: se deja como está (ver `gain::no_data`).
        if gain::no_data(gain_db, gain_peak) {
            debug!("Sin datos de normalización: factor 1");
            return 1.0;
        }

        // As per the ReplayGain 1.0 & 2.0 (proposed) spec:
        // https://wiki.hydrogenaud.io/index.php?title=ReplayGain_1.0_specification#Clipping_prevention
        // https://wiki.hydrogenaud.io/index.php?title=ReplayGain_2.0_specification#Clipping_prevention
        let normalisation_factor = if config.normalisation_method == NormalisationMethod::Basic {
            // Como Spotify en «Normal» y «Bajo»: factor = min(ganancia + nivel, umbral / pico),
            // también por encima de 1 (antes se recortaba a 1.0 y los másteres tranquilos nunca
            // subían). El umbral deja el margen de 1 dB de Spotify; sin limitador.
            let factor = gain::basic_factor(
                gain_db,
                config.normalisation_pregain_db,
                config.normalisation_threshold_dbfs,
                gain_peak,
            );
            debug!("Normalización básica: {:.2} dB", ratio_to_db(factor));
            factor
        } else {
            // For Dynamic Normalisation it's up to the player to decide,
            // factor = ratio of (ReplayGain + PreGain).
            // We then let the dynamic limiter handle gain reduction.
            let factor = gain::dynamic_factor(gain_db, config.normalisation_pregain_db);
            let threshold_ratio = db_to_ratio(config.normalisation_threshold_dbfs);

            if factor > PCM_AT_0DBFS {
                let factor_db = gain_db + config.normalisation_pregain_db;
                let limiting_db = factor_db + config.normalisation_threshold_dbfs.abs();

                // Información y no aviso: en el nivel «Alto» es lo previsto (el limitador está
                // para eso) y llenaría el registro de Nanofy con cada canción tranquila.
                info!(
                    "This track may exceed dBFS by {factor_db:.2} dB and be subject to {limiting_db:.2} dB of dynamic limiting at its peak."
                );
            } else if factor > threshold_ratio {
                let limiting_db = gain_db
                    + config.normalisation_pregain_db
                    + config.normalisation_threshold_dbfs.abs();

                info!(
                    "This track may be subject to {limiting_db:.2} dB of dynamic limiting at its peak."
                );
            }

            factor
        };

        debug!("Normalisation Data: {data:?}");
        debug!(
            "Calculated Normalisation Factor for {:?}: {:.2}%",
            config.normalisation_type,
            normalisation_factor * 100.0
        );

        normalisation_factor
    }

    /// Lo que se enseña de la normalización de una canción (`PlayerEvent::AudioFormat`) con
    /// `config` (con «Auto» ya resuelto) y el `factor` que se le aplica: la ganancia en dB
    /// (`None` con la normalización apagada), si es la del álbum y si la canción trae datos de
    /// volumen. Público para que Nanofy lo pruebe con sus niveles.
    pub fn summary(
        config: &PlayerConfig,
        data: NormalisationData,
        factor: f64,
    ) -> (Option<f64>, bool, bool) {
        let album = config.normalisation_type == NormalisationType::Album;
        let (gain_db, peak) = if album {
            (data.album_gain_db, data.album_peak)
        } else {
            (data.track_gain_db, data.track_peak)
        };
        let gain_data = !gain::no_data(gain_db, peak);
        if !config.normalisation {
            return (None, false, gain_data);
        }
        // Un factor roto (0, NaN) no debería llegar aquí (`get_factor` lo evita), pero la
        // interfaz no debe enseñar «−inf dB».
        let db = if factor.is_finite() && factor > 0.0 { ratio_to_db(factor) } else { 0.0 };
        (Some(db), album, gain_data)
    }
}

impl Player {
    pub fn new<F>(
        config: PlayerConfig,
        session: Session,
        volume_getter: Box<dyn VolumeGetter + Send>,
        sink_builder: F,
    ) -> Arc<Self>
    where
        F: FnOnce() -> Box<dyn Sink> + Send + 'static,
    {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

        if config.normalisation {
            debug!("Normalisation Type: {:?}", config.normalisation_type);
            debug!(
                "Normalisation Pregain: {:.1} dB",
                config.normalisation_pregain_db
            );
            debug!(
                "Normalisation Threshold: {:.1} dBFS",
                config.normalisation_threshold_dbfs
            );
            debug!("Normalisation Method: {:?}", config.normalisation_method);

            if config.normalisation_method == NormalisationMethod::Dynamic {
                // as_millis() has rounding errors (truncates)
                debug!(
                    "Normalisation Attack: {:.0} ms",
                    coefficient_to_duration(config.normalisation_attack_cf).as_secs_f64() * 1000.
                );
                debug!(
                    "Normalisation Release: {:.0} ms",
                    coefficient_to_duration(config.normalisation_release_cf).as_secs_f64() * 1000.
                );
                debug!("Normalisation Knee: {} dB", config.normalisation_knee_db);
            }
        }

        let handle = thread::spawn(move || {
            let player_id = PLAYER_COUNTER.fetch_add(1, Ordering::AcqRel);
            debug!("new Player [{player_id}]");

            let converter = Converter::new(config.ditherer);
            let normalisation_knee_factor = 1.0 / (8.0 * config.normalisation_knee_db);

            // TODO: it would be neat if we could watch for added or modified files in the
            // specified directories, and dynamically update the lookup. Currently, a new player
            // must be created for any new local files to be playable.
            let local_file_lookup =
                create_local_file_lookup(config.local_file_directories.as_slice());
            let crossfader = Crossfader::new(mix_limiter(&config));

            let internal = PlayerInternal {
                session,
                config,
                commands: cmd_rx,
                load_handles: Arc::new(Mutex::new(HashMap::new())),

                state: PlayerState::Stopped,
                preload: PlayerPreload::None,
                warm: PlayerPreload::None,
                warm_since: Instant::now(),
                preload_failed: None,
                sink: sink_builder(),
                sink_status: SinkStatus::Closed,
                paused_queue_ms: 0,
                sink_event_callback: None,
                volume_getter,
                event_senders: vec![],
                converter,

                normalisation_peaks: [0.0; 2],
                normalisation_integrators: [0.0; 2],
                normalisation_channel: 0,
                normalisation_knee_factor,
                gain_ramp: None,

                auto_normalise_as_album: false,

                crossfader,
                crossfade_requested: None,
                crossfade_plan: None,
                crossfade_log: None,
                frames_out: 0,
                loading_hidden: false,
                ttfs_seq: 0,
                stalled: None,

                player_id,
                play_request_id_generator: SeqGenerator::new(0),
                last_progress_update: Instant::now(),

                local_file_lookup: Arc::new(local_file_lookup),
            };

            // While PlayerInternal is written as a future, it still contains blocking code.
            // It must be run by using block_on() in a dedicated thread.
            let runtime = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
            runtime.block_on(internal);

            debug!("PlayerInternal thread finished.");
        });

        Arc::new(Self {
            commands: Some(cmd_tx),
            thread_handle: Some(handle),
        })
    }

    pub fn is_invalid(&self) -> bool {
        if let Some(handle) = self.thread_handle.as_ref() {
            return handle.is_finished();
        }
        true
    }

    fn command(&self, cmd: PlayerCommand) {
        if let Some(commands) = self.commands.as_ref() {
            if let Err(e) = commands.send(cmd) {
                error!("Player Commands Error: {e}");
            }
        }
    }

    pub fn load(&self, track_id: SpotifyUri, start_playing: bool, position_ms: u32) {
        self.load_with_transition(track_id, start_playing, position_ms, Transition::Cut);
    }

    /// Como `load`, diciendo cómo debe empezar: con `Transition::Crossfade` (tras aceptar un
    /// `CrossfadeReady`) la canción que suena se apaga mientras entra esta; con
    /// `Transition::AutoSkip` (un salto que no pidió el usuario) un fundido en curso sigue. Si ya
    /// no es posible, es un corte normal.
    pub fn load_with_transition(
        &self,
        track_id: SpotifyUri,
        start_playing: bool,
        position_ms: u32,
        transition: Transition,
    ) {
        self.command(PlayerCommand::Load {
            track_id,
            play: start_playing,
            position_ms,
            transition,
        });
    }

    /// Fundido entre canciones en vivo: `ms` de duración (0 lo apaga; tope de 12 s) y si se funden
    /// también pistas seguidas de un mismo álbum. Un fundido que ya suena termina como iba.
    pub fn set_crossfade(&self, ms: u32, albums: bool) {
        self.command(PlayerCommand::SetCrossfade { ms, albums });
    }

    pub fn preload(&self, track_id: SpotifyUri) {
        self.command(PlayerCommand::Preload { track_id });
    }

    /// Prepara por adelantado la canción que casi seguro va a sonar enseguida (Nanofy): la que se
    /// acaba de pulsar, mientras Spirc resuelve el contexto, o la que se está pulsando (botón
    /// apretado). Metadatos, clave, primer trozo y decodificador, como una precarga, pero en su
    /// propio hueco: la siguiente de la cola sigue precargada para gapless. Cuando llega su carga
    /// la toma de ahí; si no llega, se suelta a los `WARM_TTL` o con otra carga. `for_play`: es
    /// la canción de una orden de reproducir ya dada (sus fases cuentan para su medida, `ttfs`).
    pub fn warm(&self, track_id: SpotifyUri, for_play: bool) {
        self.command(PlayerCommand::Warm { track_id, for_play });
    }

    pub fn play(&self) {
        self.command(PlayerCommand::Play)
    }

    pub fn pause(&self) {
        self.command(PlayerCommand::Pause)
    }

    pub fn stop(&self) {
        self.command(PlayerCommand::Stop)
    }

    /// Suelta el dispositivo de salida si no está sonando: tras un rato en pausa, para que
    /// Windows no lo dé por ocupado (unos auriculares Bluetooth multipunto no cambiarían al
    /// teléfono). Al volver a sonar se abre otra vez.
    pub fn release_sink(&self) {
        self.command(PlayerCommand::ReleaseSink)
    }

    pub fn seek(&self, position_ms: u32) {
        self.command(PlayerCommand::Seek(position_ms));
    }

    pub fn set_session(&self, session: Session) {
        self.command(PlayerCommand::SetSession(session));
    }

    pub fn get_player_event_channel(&self) -> PlayerEventChannel {
        let (event_sender, event_receiver) = mpsc::unbounded_channel();
        self.command(PlayerCommand::AddEventSender(event_sender));
        event_receiver
    }

    pub async fn await_end_of_track(&self) {
        let mut channel = self.get_player_event_channel();
        while let Some(event) = channel.recv().await {
            if matches!(
                event,
                PlayerEvent::EndOfTrack { .. } | PlayerEvent::Stopped { .. }
            ) {
                return;
            }
        }
    }

    pub fn set_sink_event_callback(&self, callback: Option<SinkEventCallback>) {
        self.command(PlayerCommand::SetSinkEventCallback(callback));
    }

    pub fn emit_volume_changed_event(&self, volume: u16) {
        self.command(PlayerCommand::EmitVolumeChangedEvent(volume));
    }

    pub fn set_auto_normalise_as_album(&self, setting: bool) {
        self.command(PlayerCommand::SetAutoNormaliseAsAlbum(setting));
    }

    /// Aplica ajustes de audio con el reproductor en marcha: el volumen de la normalización
    /// cambia al instante (con una rampa de 50 ms, sin chasquido) y la calidad, desde la próxima
    /// canción que se cargue, incluida la siguiente si ya estaba precargada.
    pub fn set_audio_tuning(&self, tuning: AudioTuning) {
        self.command(PlayerCommand::SetAudioTuning(tuning));
    }

    pub fn emit_filter_explicit_content_changed_event(&self, filter: bool) {
        self.command(PlayerCommand::EmitFilterExplicitContentChangedEvent(filter));
    }

    pub fn emit_session_connected_event(&self, connection_id: String, user_name: String) {
        self.command(PlayerCommand::EmitSessionConnectedEvent {
            connection_id,
            user_name,
        });
    }

    pub fn emit_session_disconnected_event(&self, connection_id: String, user_name: String) {
        self.command(PlayerCommand::EmitSessionDisconnectedEvent {
            connection_id,
            user_name,
        });
    }

    pub fn emit_session_client_changed_event(
        &self,
        client_id: String,
        client_name: String,
        client_brand_name: String,
        client_model_name: String,
    ) {
        self.command(PlayerCommand::EmitSessionClientChangedEvent {
            client_id,
            client_name,
            client_brand_name,
            client_model_name,
        });
    }

    pub fn emit_shuffle_changed_event(&self, shuffle: bool) {
        self.command(PlayerCommand::EmitShuffleChangedEvent(shuffle));
    }

    pub fn emit_cluster_snapshot_event(&self, cluster: Vec<u8>) {
        self.command(PlayerCommand::EmitClusterSnapshotEvent(cluster));
    }

    pub fn emit_jam_queue_event(&self, current: String, context: String, next: Vec<String>) {
        self.command(PlayerCommand::EmitJamQueueEvent {
            current,
            context,
            next,
        });
    }

    pub fn emit_repeat_changed_event(&self, context: bool, track: bool) {
        self.command(PlayerCommand::EmitRepeatChangedEvent { context, track });
    }

    pub fn emit_auto_play_changed_event(&self, auto_play: bool) {
        self.command(PlayerCommand::EmitAutoPlayChangedEvent(auto_play));
    }

    /// Spirc avisa de que se detuvo tras `failed` canciones seguidas que no se pudieron
    /// reproducir; llega a la interfaz como `PlayerEvent::SkipCascade`.
    pub fn emit_skip_cascade_event(&self, failed: u32) {
        self.command(PlayerCommand::EmitSkipCascadeEvent(failed));
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        debug!("Shutting down player thread ...");
        self.commands = None;
        if let Some(handle) = self.thread_handle.take() {
            if let Err(e) = handle.join() {
                error!("Player thread Error: {e:?}");
            }
        }
    }
}

struct PlayerLoadedTrackData {
    decoder: Decoder,
    normalisation_data: NormalisationData,
    stream_loader_controller: StreamLoaderController,
    audio_item: AudioItem,
    bytes_per_second: usize,
    duration_ms: u32,
    stream_position_ms: u32,
    is_explicit: bool,
    audio_source: AudioSource,
}

enum PlayerPreload {
    None,
    Loading {
        track_id: SpotifyUri,
        loader: Loader,
    },
    Ready {
        track_id: SpotifyUri,
        loaded_track: Box<PlayerLoadedTrackData>,
    },
}

impl PlayerPreload {
    /// La canción que se está cargando o ya está lista aquí.
    fn track_id(&self) -> Option<&SpotifyUri> {
        match self {
            PlayerPreload::None => None,
            PlayerPreload::Loading { track_id, .. } | PlayerPreload::Ready { track_id, .. } => {
                Some(track_id)
            }
        }
    }
}

type Decoder = Box<dyn AudioDecoder + Send>;

enum PlayerState {
    Stopped,
    Loading {
        track_id: SpotifyUri,
        play_request_id: u64,
        start_playback: bool,
        loader: Loader,
    },
    Paused {
        track_id: SpotifyUri,
        play_request_id: u64,
        decoder: Decoder,
        audio_item: AudioItem,
        normalisation_data: NormalisationData,
        normalisation_factor: f64,
        stream_loader_controller: StreamLoaderController,
        bytes_per_second: usize,
        duration_ms: u32,
        stream_position_ms: u32,
        suggested_to_preload_next_track: bool,
        is_explicit: bool,
        audio_source: AudioSource,
    },
    Playing {
        track_id: SpotifyUri,
        play_request_id: u64,
        decoder: Decoder,
        normalisation_data: NormalisationData,
        audio_item: AudioItem,
        normalisation_factor: f64,
        stream_loader_controller: StreamLoaderController,
        bytes_per_second: usize,
        duration_ms: u32,
        stream_position_ms: u32,
        reported_nominal_start_time: Option<Instant>,
        suggested_to_preload_next_track: bool,
        is_explicit: bool,
        audio_source: AudioSource,
    },
    EndOfTrack {
        track_id: SpotifyUri,
        play_request_id: u64,
        loaded_track: PlayerLoadedTrackData,
    },
    Invalid,
}

impl PlayerState {
    fn is_playing(&self) -> bool {
        use self::PlayerState::*;
        match *self {
            Stopped | EndOfTrack { .. } | Paused { .. } | Loading { .. } => false,
            Playing { .. } => true,
            Invalid => {
                error!("PlayerState::is_playing in invalid state");
                exit(1);
            }
        }
    }

    #[allow(dead_code)]
    fn is_stopped(&self) -> bool {
        use self::PlayerState::*;
        matches!(self, Stopped)
    }

    #[allow(dead_code)]
    fn is_loading(&self) -> bool {
        use self::PlayerState::*;
        matches!(self, Loading { .. })
    }

    fn decoder(&mut self) -> Option<&mut Decoder> {
        use self::PlayerState::*;
        match *self {
            Stopped | EndOfTrack { .. } | Loading { .. } => None,
            Paused {
                ref mut decoder, ..
            }
            | Playing {
                ref mut decoder, ..
            } => Some(decoder),
            Invalid => {
                error!("PlayerState::decoder in invalid state");
                exit(1);
            }
        }
    }

    fn playing_to_end_of_track(&mut self) {
        use self::PlayerState::*;
        let new_state = mem::replace(self, Invalid);
        match new_state {
            Playing {
                track_id,
                play_request_id,
                decoder,
                duration_ms,
                bytes_per_second,
                normalisation_data,
                stream_loader_controller,
                stream_position_ms,
                is_explicit,
                audio_item,
                audio_source,
                ..
            } => {
                *self = EndOfTrack {
                    track_id,
                    play_request_id,
                    loaded_track: PlayerLoadedTrackData {
                        decoder,
                        normalisation_data,
                        stream_loader_controller,
                        audio_item,
                        bytes_per_second,
                        duration_ms,
                        stream_position_ms,
                        is_explicit,
                        audio_source,
                    },
                };
            }
            _ => {
                error!("Called playing_to_end_of_track in non-playing state: {new_state:?}");
                exit(1);
            }
        }
    }

    fn paused_to_playing(&mut self) {
        use self::PlayerState::*;
        let new_state = mem::replace(self, Invalid);
        match new_state {
            Paused {
                track_id,
                play_request_id,
                decoder,
                audio_item,
                normalisation_data,
                normalisation_factor,
                stream_loader_controller,
                duration_ms,
                bytes_per_second,
                stream_position_ms,
                suggested_to_preload_next_track,
                is_explicit,
                audio_source,
            } => {
                *self = Playing {
                    track_id,
                    play_request_id,
                    decoder,
                    audio_item,
                    normalisation_data,
                    normalisation_factor,
                    stream_loader_controller,
                    duration_ms,
                    bytes_per_second,
                    stream_position_ms,
                    reported_nominal_start_time: Instant::now()
                        .checked_sub(Duration::from_millis(stream_position_ms as u64)),
                    suggested_to_preload_next_track,
                    is_explicit,
                    audio_source,
                };
            }
            _ => {
                error!("PlayerState::paused_to_playing in invalid state: {new_state:?}");
                exit(1);
            }
        }
    }

    fn playing_to_paused(&mut self) {
        use self::PlayerState::*;
        let new_state = mem::replace(self, Invalid);
        match new_state {
            Playing {
                track_id,
                play_request_id,
                decoder,
                audio_item,
                normalisation_data,
                normalisation_factor,
                stream_loader_controller,
                duration_ms,
                bytes_per_second,
                stream_position_ms,
                suggested_to_preload_next_track,
                is_explicit,
                audio_source,
                ..
            } => {
                *self = Paused {
                    track_id,
                    play_request_id,
                    decoder,
                    audio_item,
                    normalisation_data,
                    normalisation_factor,
                    stream_loader_controller,
                    duration_ms,
                    bytes_per_second,
                    stream_position_ms,
                    suggested_to_preload_next_track,
                    is_explicit,
                    audio_source,
                };
            }
            _ => {
                error!("PlayerState::playing_to_paused in invalid state: {new_state:?}");
                exit(1);
            }
        }
    }
}

/// Orden en que se buscan los formatos de una canción para cada calidad. Es el único: lo usan el
/// reproductor y las descargas de Nanofy, para que una descarga guarde justo el fichero que luego
/// se reproduce (antes, en 160 kbps, se descargaba el de 320 y sonaba el de 96, o nada sin red).
/// (Most) podcasts seem to support only 96 kbps Ogg Vorbis, so every order falls back to it.
/// Sin FLAC: sus claves están tras el DRM de Spotify y TRACK_V4 ni siquiera los lista.
pub fn format_order(bitrate: Bitrate) -> &'static [AudioFileFormat] {
    match bitrate {
        Bitrate::Bitrate96 => &[
            AudioFileFormat::OGG_VORBIS_96,
            AudioFileFormat::MP3_96,
            AudioFileFormat::OGG_VORBIS_160,
            AudioFileFormat::MP3_160,
            AudioFileFormat::MP3_256,
            AudioFileFormat::OGG_VORBIS_320,
            AudioFileFormat::MP3_320,
        ],
        Bitrate::Bitrate160 => &[
            AudioFileFormat::OGG_VORBIS_160,
            AudioFileFormat::MP3_160,
            AudioFileFormat::OGG_VORBIS_96,
            AudioFileFormat::MP3_96,
            AudioFileFormat::MP3_256,
            AudioFileFormat::OGG_VORBIS_320,
            AudioFileFormat::MP3_320,
        ],
        Bitrate::Bitrate320 => &[
            AudioFileFormat::OGG_VORBIS_320,
            AudioFileFormat::MP3_320,
            AudioFileFormat::MP3_256,
            AudioFileFormat::OGG_VORBIS_160,
            AudioFileFormat::MP3_160,
            AudioFileFormat::OGG_VORBIS_96,
            AudioFileFormat::MP3_96,
        ],
    }
}

/// Elige el fichero de audio de una canción: el primer formato de `order` que ya está en la
/// caché y, si ninguno lo está, el primero disponible. Un formato fuera de `order` (AAC, FLAC…)
/// nunca se elige, aunque esté en la caché.
pub fn pick_audio_file(
    order: &[AudioFileFormat],
    files: &HashMap<AudioFileFormat, FileId>,
    is_cached: impl Fn(FileId) -> bool,
) -> Option<(AudioFileFormat, FileId)> {
    let available = || {
        order
            .iter()
            .filter_map(|format| files.get(format).map(|&file_id| (*format, file_id)))
    };
    available()
        .find(|&(_, file_id)| is_cached(file_id))
        .or_else(|| available().next())
}

/// `format` (lo que eligió `pick_audio_file`) no es el primer formato disponible de `order`: se
/// eligió por estar ya en la caché, no porque la canción no exista en la calidad pedida.
pub fn picked_from_cache(
    order: &[AudioFileFormat],
    files: &HashMap<AudioFileFormat, FileId>,
    format: AudioFileFormat,
) -> bool {
    order.iter().find(|f| files.contains_key(f)) != Some(&format)
}

/// Lo que puede pedir a Spotify la precarga inteligente (`warm_track`), para que quien la manda
/// lo cuente contra su presupuesto antes de cada petición.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarmStep {
    /// Los metadatos de la canción (extended-metadata).
    Metadata,
    /// Dónde está su fichero en la CDN (storage-resolve).
    Storage,
    /// Su clave, solo si el fichero ya está en la caché del disco.
    Key,
}

/// Precarga inteligente (Nanofy): deja lista la canción sobre la que el usuario tiene el ratón,
/// sin bajar audio. Metadatos y ubicación del fichero en la CDN (los dos se guardan en memoria,
/// ver `spclient` y `CdnUrl`); si el fichero ya está en la caché del disco, la clave, que entonces
/// es lo único que queda en el camino del primer sonido. Elige el fichero igual que el
/// reproductor (`pick_audio_file`) para que sea el mismo que luego se carga. `allow` decide antes
/// de cada petición (presupuesto); lo que ya está en memoria no pide nada. Devuelve qué hizo.
pub async fn warm_track(
    session: &Session,
    uri: &SpotifyUri,
    bitrate: Bitrate,
    mut allow: impl FnMut(WarmStep) -> bool,
) -> Result<&'static str, Error> {
    if !matches!(uri, SpotifyUri::Track { .. } | SpotifyUri::Episode { .. }) {
        return Ok("no es una canción ni un episodio");
    }
    let track_id: SpotifyId = uri.try_into()?;
    if !session.spclient().playable_metadata_cached(uri) && !allow(WarmStep::Metadata) {
        return Ok("sin presupuesto");
    }
    let item = AudioItem::get_file(session, uri.clone()).await?;
    if item.availability.is_err() {
        return Ok("no disponible");
    }
    // Sin ficheros propios suena una alternativa (otra edición): eso ya no se prepara.
    let cache = session.cache();
    let is_cached = |file_id: FileId| {
        cache
            .and_then(|c| c.file_path(file_id))
            .is_some_and(|path| path.exists())
    };
    let Some((_, file_id)) = pick_audio_file(format_order(bitrate), &item.files, is_cached) else {
        return Ok("sin fichero propio");
    };
    if is_cached(file_id) {
        if audio_key::cached_key(track_id, file_id).is_some() {
            return Ok("lista (caché)");
        }
        if key_policy::keys_throttled() || !allow(WarmStep::Key) {
            return Ok("metadatos");
        }
        session.audio_key().prefetch(track_id, file_id).await?;
        return Ok("clave");
    }
    if CdnUrl::is_resolved(file_id) {
        return Ok("lista");
    }
    if !allow(WarmStep::Storage) {
        return Ok("metadatos");
    }
    CdnUrl::new(file_id).resolve_audio(session).await?;
    Ok("ubicación")
}

/// La CDN contestó, pero no con el trozo pedido (403 de una URL caducada, 404…): el problema son
/// las URL y no la red.
fn cdn_rejected(e: &Error) -> bool {
    matches!(
        e.error.downcast_ref::<AudioFileError>(),
        Some(AudioFileError::StatusCode(_))
    )
}

struct PlayerTrackLoader {
    session: Session,
    config: PlayerConfig,
    local_file_lookup: Arc<LocalFileLookup>,
}

impl PlayerTrackLoader {
    async fn find_available_alternative(
        &self,
        audio_item: AudioItem,
    ) -> Result<AudioItem, LoadFailure> {
        if let Err(e) = audio_item.availability {
            error!("Track is unavailable: {e}");
            Err(LoadFailure::NotAvailable(e.to_string()))
        } else if !audio_item.files.is_empty() {
            Ok(audio_item)
        } else if let Some(alternatives) = audio_item.alternatives {
            let Tracks(alternatives_vec) = alternatives; // required to make `into_iter` able to move

            let mut alternatives: FuturesUnordered<_> = alternatives_vec
                .into_iter()
                .map(|alt_id| AudioItem::get_file(&self.session, alt_id))
                .collect();

            // La primera alternativa disponible. Si ninguna lo está pero alguna ni siquiera se
            // pudo consultar, el fallo es de la red (o del límite), no de la canción.
            let mut network: Option<Error> = None;
            while let Some(alternative) = alternatives.next().await {
                match alternative {
                    Ok(alt) if alt.availability.is_ok() => return Ok(alt),
                    Ok(_) => {}
                    Err(e) => {
                        network.get_or_insert(e);
                    }
                }
            }
            Err(match network {
                Some(e) => LoadFailure::from_metadata_error(&e),
                None => LoadFailure::NotAvailable("ninguna alternativa disponible".into()),
            })
        } else {
            error!("Track should be available, but no alternatives found.");
            Err(LoadFailure::NotAvailable("sin ficheros ni alternativas".into()))
        }
    }

    fn stream_data_rate(&self, format: AudioFileFormat) -> Option<usize> {
        let kbps = match format {
            AudioFileFormat::OGG_VORBIS_96 => 12.,
            AudioFileFormat::OGG_VORBIS_160 => 20.,
            AudioFileFormat::OGG_VORBIS_320 => 40.,
            AudioFileFormat::MP3_256 => 32.,
            AudioFileFormat::MP3_320 => 40.,
            AudioFileFormat::MP3_160 => 20.,
            AudioFileFormat::MP3_96 => 12.,
            AudioFileFormat::MP3_160_ENC => 20.,
            AudioFileFormat::AAC_24 => 3.,
            AudioFileFormat::AAC_48 => 6.,
            AudioFileFormat::AAC_160 => 20.,
            AudioFileFormat::AAC_320 => 40.,
            AudioFileFormat::MP4_128 => 16.,
            AudioFileFormat::OTHER5 => 40.,
            AudioFileFormat::FLAC_FLAC => 112., // assume 900 kbit/s on average
            AudioFileFormat::XHE_AAC_12 => 1.5,
            AudioFileFormat::XHE_AAC_16 => 2.,
            AudioFileFormat::XHE_AAC_24 => 3.,
            AudioFileFormat::FLAC_FLAC_24BIT => 200.,
        };
        let data_rate: f32 = kbps * 1024.;
        Some(data_rate.ceil() as usize)
    }

    async fn load_track(
        &self,
        track_uri: SpotifyUri,
        position_ms: u32,
    ) -> Result<PlayerLoadedTrackData, LoadFailure> {
        match track_uri {
            SpotifyUri::Track { .. } | SpotifyUri::Episode { .. } => {
                self.load_remote_track(track_uri, position_ms).await
            }
            SpotifyUri::Local { .. } => self.load_local_track(track_uri, position_ms).await,
            _ => {
                error!("Cannot handle load of track with URI: <{track_uri}>",);
                Self::failed(
                    &track_uri.to_string(),
                    LoadFailure::NotAvailable("uri que no se puede reproducir".into()),
                )
            }
        }
    }

    /// Anota por qué falló una carga (el último error de `ttfs` y, si es la carga de una orden
    /// medida, su fallo) y devuelve el fallo, para salir con `return Self::failed(…)`.
    fn failed<T>(uri: &str, failure: LoadFailure) -> Result<T, LoadFailure> {
        ttfs::load_failed(uri.to_string(), failure.describe());
        Err(failure)
    }

    /// El decodificador no se pudo crear o no pudo ir a la posición pedida. Si antes falló una
    /// lectura del fichero (la CDN dejó de entregar datos) es la red; si no, el fichero.
    fn decode_failure(what: String, read_failed: &AtomicBool) -> LoadFailure {
        if read_failed.load(Ordering::Relaxed) {
            LoadFailure::Network(what)
        } else {
            LoadFailure::Decode(what)
        }
    }

    async fn load_remote_track(
        &self,
        track_uri: SpotifyUri,
        position_ms: u32,
    ) -> Result<PlayerLoadedTrackData, LoadFailure> {
        let uri = track_uri.to_string();
        let episode = is_episode(&track_uri);
        let track_id: SpotifyId = match (&track_uri).try_into() {
            Ok(id) => id,
            Err(_) => {
                warn!("<{track_uri}> could not be converted to a base62 ID");
                return Self::failed(&uri, LoadFailure::NotAvailable("uri no válida".into()));
            }
        };

        let audio_item = match AudioItem::get_file(&self.session, track_uri).await {
            Ok(audio) => match self.find_available_alternative(audio).await {
                Ok(audio) => audio,
                Err(failure) => {
                    warn!(
                        "spotify:track:<{}> is not available: {failure}",
                        track_id.to_base62().unwrap_or_default()
                    );
                    return Self::failed(&uri, failure);
                }
            },
            Err(e) => {
                error!("Unable to load audio item: {e:?}");
                return Self::failed(&uri, LoadFailure::from_metadata_error(&e));
            }
        };
        // De la caché de metadatos («hit»; «seed» si los trajo un lote de la lista de Nanofy) o
        // pedidos ahora («miss»; «check» si además se comprobó una semilla).
        ttfs::mark_thread("metadata", Some(spclient::metadata_source().into()));

        info!(
            "Loading <{}> with Spotify URI <{}>",
            audio_item.name, audio_item.uri
        );

        // Lo que ya está en la caché gana: tras cambiar la calidad, una canción descargada (o
        // escuchada) en la calidad anterior sigue sonando sin red, en vez de pedir otro fichero.
        let cache = self.session.cache();
        let is_cached = |file_id: FileId| {
            cache
                .and_then(|c| c.file_path(file_id))
                .is_some_and(|path| path.exists())
        };
        let order = format_order(self.config.bitrate);
        let (format, file_id) = match pick_audio_file(order, &audio_item.files, is_cached) {
            Some(t) => t,
            None => {
                warn!(
                    "<{}> is not available in any supported format",
                    audio_item.name
                );
                return Self::failed(&uri, LoadFailure::NoFormat);
            }
        };
        if order.first() != Some(&format) {
            // Caché en otra calidad o canción sin ese formato (los podcasts suelen ir a 96).
            info!("<{}> se reproduce en {format:?}", audio_item.name);
        }
        let audio_source = AudioSource {
            format: Some(format),
            // Ogg Vorbis y MP3 no guardan una profundidad de bits: se decodifican a coma flotante.
            bits: None,
            // El decodificador rechaza cualquier otra frecuencia (`SymphoniaDecoder::new`).
            sample_rate: SAMPLE_RATE,
            requested: Some(self.config.bitrate),
            // La interfaz no debe decir entonces que la canción no existe en la calidad pedida.
            from_cache: picked_from_cache(order, &audio_item.files, format),
        };

        let Some(bytes_per_second) = self.stream_data_rate(format) else {
            return Self::failed(&uri, LoadFailure::NoFormat);
        };

        // This is only a loop to be able to reload the file if an error occurred
        // while opening a cached file.
        loop {
            // La clave no depende del fichero: se pide a la vez que se abre (resolver la CDN y
            // bajar el primer trozo), en vez de después. Ahorra una ida y vuelta en cada canción.
            // Cada mitad marca su fase al terminar (`ttfs`); las dos van en este mismo hilo. La
            // petición de la clave ya reintenta sola una negativa pasajera o una respuesta que no
            // llega (`key_policy`), y la que ya se concedió en este proceso sale de la memoria.
            let key_from_cache = audio_key::cached_key(track_id, file_id).is_some();
            let (encrypted_file, key) = futures_util::join!(
                async {
                    // ¿Dónde está el fichero en la CDN? Quizá ya se sabía (`CdnUrl`): entonces, si
                    // la CDN rechaza esas URL (caducaron antes de lo que decían), se pregunta otra
                    // vez en vez de dar la canción por perdida.
                    let storage_cached = CdnUrl::is_resolved(file_id);
                    cdn_url::begin_open();
                    let mut file = AudioFile::open(&self.session, file_id, bytes_per_second).await;
                    cdn_url::end_open(file_id, file.is_ok());
                    if storage_cached && file.as_ref().err().is_some_and(cdn_rejected) {
                        warn!("CDN rejected the cached URLs of {file_id}; resolving them again");
                        cdn_url::begin_open();
                        file = AudioFile::open(&self.session, file_id, bytes_per_second).await;
                        cdn_url::end_open(file_id, file.is_ok());
                    }
                    let info = match &file {
                        Ok(f) if f.is_cached() => "cache",
                        Ok(_) => "cdn",
                        Err(_) => "error",
                    };
                    ttfs::mark_thread("cdn-head", Some(info.into()));
                    file
                },
                async {
                    let key = self.session.audio_key().request(track_id, file_id).await;
                    ttfs::mark_thread("key", Some(key_info(&key, key_from_cache)));
                    key
                }
            );

            let encrypted_file = match encrypted_file {
                Ok(encrypted_file) => encrypted_file,
                Err(e) => {
                    error!("Unable to load encrypted file: {e:?}");
                    return Self::failed(&uri, LoadFailure::from_file_error("fichero de audio", &e));
                }
            };

            let is_cached = encrypted_file.is_cached();

            let stream_loader_controller = match encrypted_file.get_stream_loader_controller() {
                Ok(controller) => controller,
                Err(e) => {
                    error!("Unable to get the stream loader controller: {e:?}");
                    return Self::failed(&uri, LoadFailure::from_file_error("fichero de audio", &e));
                }
            };

            // Not all audio files are encrypted. If we can't get a key, try loading the track
            // without decryption. If the file was encrypted after all, the decoder will fail
            // parsing and bail out, so we should be safe from outputting ear-piercing noise.
            // Nanofy: solo con episodios. Las canciones de Spotify siempre van cifradas: sin
            // clave el decodificador puede tomar el cifrado por otro formato y sacar basura o
            // ruido, así que una clave negada es definitiva para ese fichero (salvo la negativa
            // pasajera o la falta de respuesta, que se reintentan más tarde con toda la carga).
            // Un episodio en Ogg Vorbis tampoco: los alojados en Spotify van cifrados igual que
            // las canciones, y sin clave podrían sonar a ruido (antes ya era definitivo). El atajo
            // queda para los MP3 de podcasts externos, que no van cifrados.
            let (key, key_failure) = match key {
                Ok(key) => (Some(key), None),
                Err(e) if !episode || AudioFiles::is_ogg_vorbis(format) => {
                    error!("Unable to load key for <{}>: {e}", audio_item.name);
                    return Self::failed(&uri, LoadFailure::from_key_error(&e));
                }
                Err(e) => {
                    warn!("Unable to load key, continuing without decryption: {e}");
                    (None, Some(LoadFailure::from_key_error(&e)))
                }
            };

            // Pasa a `true` cuando el fichero ya suena (tras la búsqueda inicial): el corte de CDN
            // simulado no debe empezar con las lecturas de la carga.
            let streaming = Arc::new(AtomicBool::new(false));
            // Alguna lectura del fichero falló: un decodificador que no arranca es entonces la
            // red y no el fichero (ver `decode_failure`).
            let read_failed = Arc::new(AtomicBool::new(false));
            let mut decrypted_file = WatchedFile::new(
                AudioDecrypt::new(key, encrypted_file),
                stream_loader_controller.len() as u64,
                ttfs::thread_seq(),
                streaming.clone(),
                read_failed.clone(),
            );

            let is_ogg_vorbis = AudioFiles::is_ogg_vorbis(format);
            let (offset, mut normalisation_data) = if is_ogg_vorbis {
                // Spotify stores normalisation data in a custom Ogg packet instead of Vorbis comments.
                let normalisation_data =
                    NormalisationData::parse_from_ogg(&mut decrypted_file).ok();
                (SPOTIFY_OGG_HEADER_END, normalisation_data)
            } else {
                (0, None)
            };

            let audio_file = match Subfile::new(
                decrypted_file,
                offset,
                stream_loader_controller.len() as u64,
            ) {
                Ok(audio_file) => audio_file,
                Err(e) => {
                    error!("PlayerTrackLoader::load_track error opening subfile: {e}");
                    return Self::failed(
                        &uri,
                        Self::decode_failure(format!("fichero de audio: {e}"), &read_failed),
                    );
                }
            };

            let mut symphonia_decoder = |audio_file, format| {
                SymphoniaDecoder::new(audio_file, format).map(|mut decoder| {
                    // For formats other that Vorbis, we'll try getting normalisation data from
                    // ReplayGain metadata fields, if present.
                    if normalisation_data.is_none() {
                        normalisation_data = decoder.normalisation_data();
                    }
                    Box::new(decoder) as Decoder
                })
            };

            let mut hint = Hint::new();
            if let Some(mime_type) = AudioFiles::mime_type(format) {
                hint.mime_type(mime_type);
            }

            #[cfg(feature = "passthrough-decoder")]
            let decoder_type = if self.config.passthrough {
                PassthroughDecoder::new(audio_file, format).map(|x| Box::new(x) as Decoder)
            } else {
                symphonia_decoder(audio_file, hint)
            };

            #[cfg(not(feature = "passthrough-decoder"))]
            let decoder_type = { symphonia_decoder(audio_file, hint) };

            let normalisation_data = normalisation_data.unwrap_or_else(|| {
                warn!("Unable to get normalisation data, continuing with defaults.");
                NormalisationData::default()
            });

            let mut decoder = match decoder_type {
                Ok(decoder) => decoder,
                Err(e) if is_cached => {
                    warn!("Unable to read cached audio file: {e}. Trying to download it.");
                    // Por si lo que falló fue la clave guardada en memoria y no el fichero: el
                    // reintento la pide otra vez.
                    audio_key::forget_key(track_id, file_id);

                    match self.session.cache() {
                        Some(cache) => {
                            if cache.remove_file(file_id).is_err() {
                                error!("Error removing file from cache");
                                return Self::failed(
                                    &uri,
                                    LoadFailure::Decode(format!("caché dañada: {e}")),
                                );
                            }
                        }
                        None => {
                            error!("If the audio file is cached, a cache should exist");
                            return Self::failed(
                                &uri,
                                LoadFailure::Decode(format!("caché dañada: {e}")),
                            );
                        }
                    }

                    // Just try it again
                    ttfs::mark_thread("cache-retry", None);
                    continue;
                }
                Err(e) => {
                    error!("Unable to read audio file: {e}");
                    audio_key::forget_key(track_id, file_id);
                    // Un episodio que se intentó leer sin clave y no se puede decodificar iba
                    // cifrado: el fallo es el de la clave (quizá pasajero), no el del fichero.
                    let failure = key_failure.unwrap_or_else(|| {
                        Self::decode_failure(format!("decodificador: {e}"), &read_failed)
                    });
                    return Self::failed(&uri, failure);
                }
            };
            ttfs::mark_thread("decoder", None);

            let duration_ms = audio_item.duration_ms;
            // Don't try to seek past the track's duration.
            // If the position is invalid just start from
            // the beginning of the track.
            let position_ms = if position_ms > duration_ms {
                warn!(
                    "Invalid start position of {position_ms} ms exceeds track's duration of {duration_ms} ms, starting track from the beginning"
                );
                0
            } else {
                position_ms
            };

            // Desde el principio no se busca: el decodificador recién creado ya está al inicio
            // del audio. La normalización se lee del fichero antes de crearlo (y `Subfile` vuelve
            // a su inicio), el sondeo del final que hace el demultiplexor Ogg deja el cursor
            // donde estaba, y el decodificador «passthrough» del comentario original no se
            // compila. Buscar 0 ms en un Ogg que aún se descarga costaba, en cambio, una
            // bisección de todo el fichero: unas 6 peticiones de 64 KB a la CDN, una tras otra,
            // antes del primer sonido. librespot 0.4 tampoco buscaba en 0.
            let stream_position_ms = if position_ms == 0 {
                0
            } else {
                match decoder.seek(position_ms) {
                    Ok(new_position_ms) => new_position_ms,
                    Err(e) => {
                        error!(
                            "PlayerTrackLoader::load_track error seeking to starting position {position_ms}: {e}"
                        );
                        return Self::failed(
                            &uri,
                            Self::decode_failure(
                                format!("posición {position_ms} ms: {e}"),
                                &read_failed,
                            ),
                        );
                    }
                }
            };
            ttfs::mark_thread("seek", Some(seek_mark(position_ms)));

            // Ensure streaming mode now that we are ready to play from the requested position.
            stream_loader_controller.set_stream_mode();
            streaming.store(true, Ordering::Relaxed);

            let is_explicit = audio_item.is_explicit;

            info!("<{}> ({} ms) loaded", audio_item.name, duration_ms);

            return Ok(PlayerLoadedTrackData {
                decoder,
                normalisation_data,
                stream_loader_controller,
                audio_item,
                bytes_per_second,
                duration_ms,
                stream_position_ms,
                is_explicit,
                audio_source,
            });
        }
    }

    async fn load_local_track(
        &self,
        track_uri: SpotifyUri,
        position_ms: u32,
    ) -> Result<PlayerLoadedTrackData, LoadFailure> {
        info!("Loading local file with Spotify URI <{}>", track_uri);

        let SpotifyUri::Local { duration, .. } = track_uri else {
            error!("Unable to determine track duration for local file: not a local file URI");
            return Self::failed(
                &track_uri.to_string(),
                LoadFailure::NotAvailable("no es un archivo local".into()),
            );
        };

        let entry = self.local_file_lookup.get(&track_uri);

        let Some(path) = entry else {
            error!("Unable to find file path for local file <{track_uri}>");
            return Self::failed(
                &track_uri.to_string(),
                LoadFailure::NotAvailable("archivo local no encontrado".into()),
            );
        };

        let src = match File::open(path) {
            Ok(src) => src,
            Err(e) => {
                error!("Failed to open local file: {e}");
                return Self::failed(
                    &track_uri.to_string(),
                    LoadFailure::Decode(format!("fichero local: {e}")),
                );
            }
        };

        let mut hint = Hint::new();
        if let Some(file_extension) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(file_extension);
        }

        let decoder = match SymphoniaDecoder::new(src, hint) {
            Ok(decoder) => decoder,
            Err(e) => {
                error!("Error decoding local file: {e}");
                return Self::failed(
                    &track_uri.to_string(),
                    LoadFailure::Decode(format!("decodificador: {e}")),
                );
            }
        };
        ttfs::mark_thread("decoder", Some("local".into()));

        let mut decoder = Box::new(decoder);
        let normalisation_data = decoder.normalisation_data().unwrap_or_else(|| {
            warn!("Unable to get normalisation data, continuing with defaults.");
            NormalisationData::default()
        });

        let local_file_metadata = decoder.local_file_metadata().unwrap_or_default();
        let audio_source = AudioSource {
            format: None,
            bits: decoder.bits_per_sample(),
            sample_rate: decoder.sample_rate().unwrap_or(SAMPLE_RATE),
            requested: None,
            from_cache: false,
        };

        // Como en `load_remote_track`: desde el principio, el decodificador recién creado ya
        // está al inicio y buscar no aporta nada.
        let stream_position_ms = if position_ms == 0 {
            0
        } else {
            match decoder.seek(position_ms) {
                Ok(new_position_ms) => new_position_ms,
                Err(e) => {
                    error!(
                        "PlayerTrackLoader::load_local_track error seeking to starting position {position_ms}: {e}"
                    );
                    return Self::failed(
                        &track_uri.to_string(),
                        LoadFailure::Decode(format!("posición {position_ms} ms: {e}")),
                    );
                }
            }
        };
        ttfs::mark_thread("seek", Some(seek_mark(position_ms)));

        let file_size = match fs::metadata(path) {
            Ok(meta) => meta.len(),
            Err(e) => {
                return Self::failed(
                    &track_uri.to_string(),
                    LoadFailure::Decode(format!("fichero local: {e}")),
                );
            }
        };
        let bytes_per_second = (file_size / duration.as_secs()) as usize;

        let stream_loader_controller = StreamLoaderController::from_local_file(file_size);

        let name = local_file_metadata.name.unwrap_or_default();

        info!("Loaded <{name}> from path <{}>", path.display());

        Ok(PlayerLoadedTrackData {
            decoder,
            normalisation_data,
            stream_loader_controller,
            bytes_per_second,
            duration_ms: duration.as_millis() as u32,
            stream_position_ms,
            is_explicit: false,
            audio_source,
            audio_item: AudioItem {
                duration_ms: duration.as_millis() as u32,
                uri: track_uri.to_uri().unwrap_or_default(),
                track_id: track_uri,
                files: Default::default(),
                name,
                // We can't get a CoverImage.URL for the track image, applications will have to parse the file metadata themselves using unique_fields.path
                covers: vec![],
                language: local_file_metadata
                    .language
                    .map(|val| vec![val])
                    .unwrap_or_default(),
                is_explicit: false,
                availability: Ok(()),
                alternatives: None,
                unique_fields: UniqueFields::Local {
                    artists: local_file_metadata.artists,
                    album: local_file_metadata.album,
                    album_artists: local_file_metadata.album_artists,
                    number: local_file_metadata.number,
                    disc_number: local_file_metadata.disc_number,
                    path: path.to_path_buf(),
                },
            },
        })
    }
}

impl Future for PlayerInternal {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        // While this is written as a future, it still contains blocking code.
        // It must be run on its own thread.
        let passthrough = self.config.passthrough;

        loop {
            let mut all_futures_completed_or_not_ready = true;

            // process commands that were sent to us
            let cmd = match self.commands.poll_recv(cx) {
                Poll::Ready(None) => return Poll::Ready(()), // client has disconnected - shut down.
                Poll::Ready(Some(cmd)) => {
                    all_futures_completed_or_not_ready = false;
                    Some(cmd)
                }
                _ => None,
            };

            if let Some(cmd) = cmd {
                if let Err(e) = self.handle_command(cmd) {
                    error!("Error handling command: {e}");
                }
            }

            // Canción cortada por la red: ¿ya vuelven a llegar datos?
            if let Some(probe) = self.stalled.as_mut().and_then(|s| s.probe.as_mut()) {
                if let Poll::Ready(result) = probe.poll_unpin(cx) {
                    all_futures_completed_or_not_ready = false;
                    // Un hilo que no llegó a contestar cuenta como «aún no».
                    self.on_stall_probe(result.unwrap_or(false));
                }
            }

            // Handle loading of a new track to play
            if let PlayerState::Loading {
                ref mut loader,
                ref track_id,
                start_playback,
                play_request_id,
            } = self.state
            {
                // The loader may be terminated if we are trying to load the same track
                // as before, and that track failed to open before.
                let track_id = track_id.clone();

                if !loader.as_mut().is_terminated() {
                    match loader.as_mut().poll(cx) {
                        Poll::Ready(Ok(loaded_track)) => {
                            self.start_playback(
                                track_id,
                                play_request_id,
                                loaded_track,
                                start_playback,
                            );
                            if let PlayerState::Loading { .. } = self.state {
                                error!("The state wasn't changed by start_playback()");
                                exit(1);
                            }
                        }
                        Poll::Ready(Err(reason)) => {
                            let transient = reason.is_transient();
                            if transient {
                                // Sin «Skipping»: la canción no se salta, se reintenta.
                                warn!(
                                    "Unable to load track <{track_id:?}> for now ({reason}); it stays paused to retry"
                                );
                            } else {
                                error!(
                                    "Skipping to next track, unable to load track <{track_id:?}>: {reason}"
                                );
                                ttfs::count(ttfs::Counter::Unavailable);
                            }
                            // Si el hilo de carga no dijo por qué (o murió), al menos consta. Con
                            // el motivo: una carga por adelantado (`Player::warm`) que esta carga
                            // tomó no anota su fallo en la medida (`bind_thread_speculative`), y
                            // sin él la medida quedaba fallida sin decir por qué.
                            ttfs::fail(self.ttfs_seq, &reason.describe());
                            // La interfaz ve la secuencia de siempre (cargando → fallo), aunque la
                            // anterior aún termine de apagarse.
                            self.reveal_loading();
                            self.send_event(PlayerEvent::LoadFailed {
                                track_id: track_id.clone(),
                                play_request_id,
                                reason,
                                transient,
                                play: start_playback,
                            });
                            if transient {
                                // Nada que sonar hasta que se reintente: la salida se detiene (y
                                // se suelta al rato, como en pausa), salvo que la anterior aún se
                                // esté apagando en un fundido.
                                if !self.crossfader.has_outgoing() {
                                    self.ensure_sink_stopped(false);
                                }
                            } else {
                                // Solo un fallo definitivo marca la canción y salta (Spirc).
                                self.send_event(PlayerEvent::Unavailable {
                                    track_id,
                                    play_request_id,
                                })
                            }
                        }
                        Poll::Pending => (),
                    }
                }
            }

            // handle pending preload requests.
            if let PlayerPreload::Loading {
                ref mut loader,
                ref track_id,
            } = self.preload
            {
                let track_id = track_id.clone();
                match loader.as_mut().poll(cx) {
                    Poll::Ready(Ok(loaded_track)) => {
                        self.send_event(PlayerEvent::Preloading {
                            track_id: track_id.clone(),
                        });
                        self.preload = PlayerPreload::Ready {
                            track_id,
                            loaded_track: Box::new(loaded_track),
                        };
                    }
                    Poll::Ready(Err(reason)) => {
                        // Una precarga fallida no se anuncia: antes se marcaba la siguiente como
                        // no disponible en cuanto Spotify frenaba las claves, y luego se saltaba
                        // aunque pudiera sonar. Si de verdad no está, lo dirá su carga al llegarle
                        // el turno; mientras tanto no se vuelve a precargar enseguida.
                        info!("Unable to preload {track_id:?}: {reason}");
                        self.preload = PlayerPreload::None;
                        self.preload_failed = Some((track_id, Instant::now()));
                    }
                    Poll::Pending => (),
                }
            }

            // Canción preparada por adelantado (`Player::warm`).
            if let PlayerPreload::Loading {
                ref mut loader,
                ref track_id,
            } = self.warm
            {
                let track_id = track_id.clone();
                match loader.as_mut().poll(cx) {
                    Poll::Ready(Ok(loaded_track)) => {
                        debug!("Warmed track {track_id:?} is ready");
                        self.warm = PlayerPreload::Ready {
                            track_id,
                            loaded_track: Box::new(loaded_track),
                        };
                    }
                    Poll::Ready(Err(reason)) => {
                        // Era por adelantado: no se anuncia ni se marca nada. Si de verdad no
                        // puede sonar, lo dirá su carga.
                        debug!("Unable to warm {track_id:?}: {reason}");
                        self.warm = PlayerPreload::None;
                    }
                    Poll::Pending => (),
                }
            }
            self.expire_warm();

            // Sin salida de audio (no hay ningún dispositivo) la canción queda en pausa en vez de
            // seguir decodificando hacia ninguna parte.
            if self.state.is_playing() && !self.ensure_sink_running() {
                self.handle_pause();
            }

            if self.state.is_playing() {
                if let PlayerState::Playing {
                    ref track_id,
                    play_request_id,
                    ref mut decoder,
                    normalisation_factor,
                    ref mut stream_position_ms,
                    ref mut reported_nominal_start_time,
                    ref stream_loader_controller,
                    ..
                } = self.state
                {
                    let track_id = track_id.clone();
                    match decoder.next_packet() {
                        Ok(result) => {
                            if let Some((ref packet_position, ref packet)) = result {
                                let new_stream_position_ms = packet_position.position_ms;
                                let expected_position_ms = std::mem::replace(
                                    &mut *stream_position_ms,
                                    new_stream_position_ms,
                                );

                                if !passthrough {
                                    match packet.samples() {
                                        Ok(_) => {
                                            let new_stream_position = Duration::from_millis(
                                                new_stream_position_ms as u64,
                                            );

                                            let now = Instant::now();

                                            // Only notify if we're skipped some packets *or* we are behind.
                                            // If we're ahead it's probably due to a buffer of the backend
                                            // and we're actually in time.
                                            let notify_about_position =
                                                match *reported_nominal_start_time {
                                                    None => true,
                                                    Some(reported_nominal_start_time) => {
                                                        let mut notify = false;

                                                        if packet_position.skipped {
                                                            if let Some(ahead) = new_stream_position
                                                                .checked_sub(Duration::from_millis(
                                                                    expected_position_ms as u64,
                                                                ))
                                                            {
                                                                notify |=
                                                                    ahead >= Duration::from_secs(1)
                                                            }
                                                        }

                                                        if let Some(lag) = now
                                                            .checked_duration_since(
                                                                reported_nominal_start_time,
                                                            )
                                                        {
                                                            if let Some(lag) =
                                                                lag.checked_sub(new_stream_position)
                                                            {
                                                                notify |=
                                                                    lag >= Duration::from_secs(1)
                                                            }
                                                        }

                                                        notify
                                                    }
                                                };

                                            if notify_about_position {
                                                *reported_nominal_start_time =
                                                    now.checked_sub(new_stream_position);
                                                self.send_event(PlayerEvent::PositionCorrection {
                                                    play_request_id,
                                                    track_id: track_id.clone(),
                                                    position_ms: new_stream_position_ms,
                                                });
                                            }

                                            if let Some(interval) =
                                                self.config.position_update_interval
                                            {
                                                let last_progress_update_since_ms =
                                                    now.duration_since(self.last_progress_update);

                                                if last_progress_update_since_ms > interval {
                                                    self.last_progress_update = now;
                                                    self.send_event(PlayerEvent::PositionChanged {
                                                        play_request_id,
                                                        track_id,
                                                        position_ms: new_stream_position_ms,
                                                    });
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            error!(
                                                "Skipping to next track, unable to decode samples for track <{track_id:?}>: {e:?}"
                                            );
                                            self.send_event(PlayerEvent::EndOfTrack {
                                                track_id,
                                                play_request_id,
                                            })
                                        }
                                    }
                                }
                            }

                            self.handle_packet(result, normalisation_factor);
                        }
                        Err(e) => {
                            // Nanofy: si lo que falló es la lectura y al fichero aún le faltan
                            // datos, es la red (la CDN dejó de entregar a tiempo), no el final de
                            // la canción: se queda en pausa en su segundo y sigue al volver, en vez
                            // de saltar a la siguiente (que sin red tampoco cargaría). Un corte
                            // simulado cuenta aunque el fichero ya estuviera entero.
                            let network = e.is_io()
                                && (!stream_loader_controller.range_to_end_available()
                                    || fault::cdn_stalled());
                            if network {
                                self.enter_stall(track_id, play_request_id, &e);
                            } else {
                                error!(
                                    "Skipping to next track, unable to get next packet for track <{track_id:?}>: {e:?}"
                                );
                                self.send_event(PlayerEvent::EndOfTrack {
                                    track_id,
                                    play_request_id,
                                })
                            }
                        }
                    }
                } else {
                    error!("PlayerInternal poll: Invalid PlayerState");
                    exit(1);
                };
            }

            // La saliente de un fundido fuera de `Playing` (ahí la mezcla `handle_packet`): sola
            // mientras la siguiente carga, retenida en pausa y, en cualquier otro estado, cortada ya:
            // nunca se deja para mezclarla con lo que suene más tarde.
            if self.crossfader.has_outgoing() && !self.state.is_playing() {
                match self.state {
                    PlayerState::Loading {
                        start_playback: true,
                        ..
                    } => {
                        if self.ensure_sink_running() {
                            self.write_tail();
                            all_futures_completed_or_not_ready = false;
                        } else {
                            // Sin salida: la pausa quita la saliente (nadie la escucharía).
                            self.handle_pause();
                        }
                    }
                    PlayerState::Paused { .. } => (),
                    _ => self.cancel_crossfade("la reproducción se detuvo"),
                }
            }

            let is_playing = self.state.is_playing();
            if let PlayerState::Playing {
                ref track_id,
                play_request_id,
                duration_ms,
                stream_position_ms,
                ref mut stream_loader_controller,
                ref mut suggested_to_preload_next_track,
                ..
            }
            | PlayerState::Paused {
                ref track_id,
                play_request_id,
                duration_ms,
                stream_position_ms,
                ref mut stream_loader_controller,
                ref mut suggested_to_preload_next_track,
                ..
            } = self.state
            {
                let track_id = track_id.clone();

                // Además de a 30 s del final, la siguiente se precarga tras unos segundos
                // escuchando esta: así «siguiente» a mitad de canción suena al instante. No en
                // pausa (al abrir la app no se baja nada) ni al saltar canciones seguidas. Ni
                // mientras Spotify frena las claves (5 min tras una negativa pasajera): cada
                // precarga temprana es una clave más, así que solo queda la de cerca del final.
                let near_end = (duration_ms as i64 - stream_position_ms as i64)
                    < PRELOAD_NEXT_TRACK_BEFORE_END_DURATION_MS as i64;
                let listening = || {
                    is_playing
                        && stream_position_ms >= PRELOAD_NEXT_TRACK_AFTER_LISTENING_MS
                        && !key_policy::keys_throttled()
                };
                if (!*suggested_to_preload_next_track)
                    && (near_end || listening())
                    && stream_loader_controller.range_to_end_available()
                {
                    *suggested_to_preload_next_track = true;
                    self.send_event(PlayerEvent::TimeToPreloadNextTrack {
                        track_id,
                        play_request_id,
                    });
                }
            }

            self.maybe_offer_crossfade();

            if (!self.state.is_playing()) && all_futures_completed_or_not_ready {
                return Poll::Pending;
            }
        }
    }
}

impl PlayerInternal {
    /// Pone la salida a sonar. `false` si no se pudo (no hay ningún dispositivo de salida): quien
    /// llama decide qué hacer, normalmente dejar la canción en pausa. Antes pausaba aquí mismo, a
    /// mitad de quien llamaba, que luego seguía como si sonara (y en `poll` acababa en `exit`);
    /// no pasaba porque la salida nunca devolvía error, y ahora sí lo hace (ver `RodioSink::start`).
    fn ensure_sink_running(&mut self) -> bool {
        if self.sink_status != SinkStatus::Running {
            trace!("== Starting sink ==");
            if let Some(callback) = &mut self.sink_event_callback {
                callback(SinkStatus::Running);
            }
            match self.sink.start() {
                Ok(()) => self.sink_status = SinkStatus::Running,
                Err(e) => {
                    error!("{e}");
                    return false;
                }
            }
        }
        true
    }

    fn ensure_sink_stopped(&mut self, temporarily: bool) {
        match self.sink_status {
            SinkStatus::Running => {
                trace!("== Stopping sink ==");
                match self.sink.stop() {
                    Ok(()) => {
                        self.sink_status = if temporarily {
                            SinkStatus::TemporarilyClosed
                        } else {
                            SinkStatus::Closed
                        };
                        if let Some(callback) = &mut self.sink_event_callback {
                            callback(self.sink_status);
                        }
                    }
                    Err(e) => {
                        error!("{e}");
                        exit(1);
                    }
                }
            }
            SinkStatus::TemporarilyClosed => {
                if !temporarily {
                    self.sink_status = SinkStatus::Closed;
                    if let Some(callback) = &mut self.sink_event_callback {
                        callback(SinkStatus::Closed);
                    }
                }
            }
            SinkStatus::Closed => (),
        }
    }

    /// Como `ensure_sink_stopped`, pero callando la salida al instante (`Sink::pause_now`): lo
    /// que tenga en cola se queda sin sonar, para reanudarlo o tirarlo (`Sink::clear`). Antes una
    /// pausa esperaba a que se vaciara la cola: 0,3-0,5 s de música tras pulsar, hasta 2 s.
    fn ensure_sink_paused(&mut self, temporarily: bool) {
        match self.sink_status {
            SinkStatus::Running => {
                trace!("== Pausing sink ==");
                match self.sink.pause_now() {
                    Ok(()) => {
                        self.sink_status = if temporarily {
                            SinkStatus::TemporarilyClosed
                        } else {
                            SinkStatus::Closed
                        };
                        if let Some(callback) = &mut self.sink_event_callback {
                            callback(self.sink_status);
                        }
                    }
                    Err(e) => {
                        error!("{e}");
                        exit(1);
                    }
                }
            }
            SinkStatus::TemporarilyClosed => {
                if !temporarily {
                    self.sink_status = SinkStatus::Closed;
                    if let Some(callback) = &mut self.sink_event_callback {
                        callback(SinkStatus::Closed);
                    }
                }
            }
            SinkStatus::Closed => (),
        }
    }

    fn handle_player_stop(&mut self) {
        // Parada: la canción cortada por la red, si la había, ya no se reanuda.
        self.stalled = None;
        // Parar quita también el fundido: nada de la canción que se iba debe sonar al volver.
        self.clear_crossfade("parar");
        self.crossfade_plan = None;
        self.loading_hidden = false;
        self.paused_queue_ms = 0;
        // Al acabarse la lista (la última canción terminó sola) se deja sonar lo que queda en
        // cola, su final. Cualquier otra parada (otro dispositivo toma el control, cascada de
        // fallos, «siguiente» sin siguiente) es una orden: silencio ya.
        let natural_end = matches!(self.state, PlayerState::EndOfTrack { .. });
        match self.state {
            PlayerState::Playing {
                ref track_id,
                play_request_id,
                ..
            }
            | PlayerState::Paused {
                ref track_id,
                play_request_id,
                ..
            }
            | PlayerState::EndOfTrack {
                ref track_id,
                play_request_id,
                ..
            }
            | PlayerState::Loading {
                ref track_id,
                play_request_id,
                ..
            } => {
                let track_id = track_id.clone();

                if natural_end {
                    self.ensure_sink_stopped(false);
                } else {
                    self.ensure_sink_paused(false);
                    self.sink.clear();
                }
                self.send_event(PlayerEvent::Stopped {
                    track_id,
                    play_request_id,
                });
                self.state = PlayerState::Stopped;
            }
            PlayerState::Stopped => (),
            PlayerState::Invalid => {
                error!("PlayerInternal::handle_player_stop in invalid state");
                exit(1);
            }
        }
    }

    fn handle_play(&mut self) {
        match self.state {
            PlayerState::Paused {
                ref track_id,
                play_request_id,
                stream_position_ms,
                bytes_per_second,
                ref stream_loader_controller,
                ..
            } => {
                let track_id = track_id.clone();

                // Cortada por la red a media canción: antes de reanudar hay que ver si vuelven a
                // llegar datos, y eso puede tardar hasta `download_timeout`. Se comprueba en otro
                // hilo; mientras, sigue en pausa y atendiendo órdenes. Con datos suena desde lo
                // último que se oyó (`on_stall_probe`); sin ellos se vuelve a avisar del corte.
                if self
                    .stalled
                    .as_ref()
                    .is_some_and(|s| s.play_request_id == play_request_id)
                {
                    let controller = stream_loader_controller.clone();
                    ttfs::mark_seq(ttfs::pending_of(&["resume"]), "stall-probe", None);
                    if let Some(stall) = self.stalled.as_mut() {
                        stall.resume = true;
                        if stall.probe.is_none() {
                            stall.probe = Some(probe_stalled_stream(controller, bytes_per_second));
                        }
                    }
                    return;
                }

                // Una propuesta de fundido que cruzó con la pausa, Spirc la rechazó (no sonaba):
                // al reanudar cerca del final se vuelve a proponer.
                if self.crossfade_requested == Some(play_request_id) {
                    self.crossfade_requested = None;
                }
                let ttfs_seq = ttfs::pending_of(&["resume"]);
                ttfs::mark_seq(ttfs_seq, "player:play", None);
                // Lo que de verdad se oyó: el decodificador va por delante lo que quedó en la
                // cola de la salida al pausar, y ahí sigue (o desde ahí se vuelve a decodificar).
                let queued_at_pause = mem::take(&mut self.paused_queue_ms);
                let heard_ms = out_queue::heard_ms(stream_position_ms, queued_at_pause);
                // Si la cola ya no está (la salida se soltó tras 5 s en pausa), se rebobina antes
                // de abrirla; si se pierde al abrirla (cambió el dispositivo predeterminado),
                // justo después. Sin esto se saltaba lo que había en cola: 0,3-0,5 s.
                let resynced =
                    self.resync_lost_queue(stream_position_ms, queued_at_pause, ttfs_seq);
                self.state.paused_to_playing();
                self.send_event(PlayerEvent::Playing {
                    track_id,
                    play_request_id,
                    position_ms: heard_ms,
                });
                let was_running = self.sink_status == SinkStatus::Running;
                if !self.ensure_sink_running() {
                    // No hay ninguna salida de audio: de vuelta a la pausa, con su evento, en vez
                    // de «sonar» en silencio. Nanofy lo explica al ver la pausa sin salida. Al
                    // intentar abrirla se perdió lo que había en cola: el decodificador vuelve a
                    // lo que se oyó, para que la pausa diga ese punto y al reanudar no se salte.
                    if !resynced {
                        self.resync_lost_queue(stream_position_ms, queued_at_pause, ttfs_seq);
                    }
                    self.handle_pause();
                    return;
                }
                if !resynced {
                    self.resync_lost_queue(stream_position_ms, queued_at_pause, ttfs_seq);
                }
                if let PlayerState::Playing {
                    ref mut reported_nominal_start_time,
                    ..
                } = self.state
                {
                    *reported_nominal_start_time =
                        Instant::now().checked_sub(Duration::from_millis(heard_ms as u64));
                }
                let sink = if was_running { "running" } else { "start" };
                ttfs::mark_seq(ttfs_seq, "sink", Some(sink.into()));
                ttfs::arm_audible(ttfs_seq);
            }
            PlayerState::Loading {
                ref mut start_playback,
                ..
            } => {
                *start_playback = true;
            }
            _ => error!("Player::play called from invalid state: {:?}", self.state),
        }
    }

    fn handle_pause(&mut self) {
        // Una pausa se oye cuando la salida se calla: ya al instante (`ensure_sink_paused`), sin
        // esperar a que termine de sonar su cola.
        let ttfs_seq = ttfs::pending_of(&["pause"]);
        ttfs::mark_seq(ttfs_seq, "player:pause", None);
        self.handle_pause_inner();
        ttfs::finish(ttfs_seq, "silent", None);
    }

    fn handle_pause_inner(&mut self) {
        match self.state {
            PlayerState::Paused {
                ref track_id,
                play_request_id,
                ..
            } => {
                let track_id = track_id.clone();
                self.ensure_sink_paused(false);
                // Una canción cortada por la red que iba a reanudarse en cuanto llegaran datos:
                // ya no. Spirc la daba por sonando desde el «reproducir»; se le devuelve la pausa
                // con el punto bueno (el suyo habría seguido avanzando).
                if let Some(stall) = self
                    .stalled
                    .as_mut()
                    .filter(|s| s.play_request_id == play_request_id && s.resume)
                {
                    stall.resume = false;
                    let position_ms = stall.position_ms;
                    self.send_event(PlayerEvent::Paused {
                        track_id,
                        play_request_id,
                        position_ms,
                    });
                }
            }
            PlayerState::Playing {
                ref track_id,
                play_request_id,
                stream_position_ms,
                ..
            } => {
                let track_id = track_id.clone();

                self.state.playing_to_paused();

                self.ensure_sink_paused(false);
                // Lo que la salida aún tenía en cola no ha sonado: la posición de la pausa (la que
                // ven la interfaz y Spotify, y desde la que se reanuda) es la oída, no la del
                // decodificador, que va 0,3-0,5 s por delante.
                self.paused_queue_ms = self.sink.queued_ms().min(stream_position_ms);
                self.send_event(PlayerEvent::Paused {
                    track_id,
                    play_request_id,
                    position_ms: out_queue::heard_ms(stream_position_ms, self.paused_queue_ms),
                });
            }
            PlayerState::Loading {
                ref mut start_playback,
                ..
            } => {
                *start_playback = false;
                // La saliente de un fundido sonaba sola mientras esta cargaba. En pausa nadie la
                // escribiría: se calla la salida, se tira lo que tenía en cola y se quita, para
                // que no vuelva a sonar encima de la siguiente al reanudar.
                if self.crossfader.has_outgoing() {
                    self.ensure_sink_paused(false);
                    self.sink.clear();
                    self.clear_crossfade("pausa mientras cargaba la siguiente");
                }
                // Sin `reveal_loading`: en pausa la barra ya no enseña nada sonando, y si esta
                // carga falla luego, el fallo sí la anuncia antes de «no disponible».
            }
            _ => error!("Player::pause called from invalid state: {:?}", self.state),
        }
    }

    /// Al reanudar una canción pausada en `stream_position_ms` (la del decodificador) con
    /// `queued_at_pause_ms` aún en la cola de la salida: si esa cola se perdió (la salida se soltó
    /// o se reabrió), lleva el decodificador a lo que se oyó, para no saltarse ese trozo. Vale en
    /// pausa y sonando. `true` si hubo que hacerlo.
    fn resync_lost_queue(
        &mut self,
        stream_position_ms: u32,
        queued_at_pause_ms: u32,
        ttfs_seq: u64,
    ) -> bool {
        let Some(heard_ms) =
            out_queue::resume_from(stream_position_ms, queued_at_pause_ms, self.sink.queued_ms())
        else {
            return false;
        };
        // Lo poco que pudiera quedar en cola ya no encaja con lo que se va a decodificar.
        self.sink.clear();
        self.paused_queue_ms = 0;
        let Some(decoder) = self.state.decoder() else {
            return true;
        };
        // Desde el decodificador abierto: lo anterior a la posición ya está descargado, así que
        // no hace falta la red.
        match decoder.seek(heard_ms) {
            Ok(new_position_ms) => {
                if let PlayerState::Playing {
                    ref mut stream_position_ms,
                    ..
                }
                | PlayerState::Paused {
                    ref mut stream_position_ms,
                    ..
                } = self.state
                {
                    *stream_position_ms = new_position_ms;
                }
                debug!(
                    "la cola de la salida se perdió en la pausa: se sigue desde {new_position_ms} ms \
                     (el decodificador iba en {stream_position_ms} ms)"
                );
                ttfs::mark_seq(ttfs_seq, "resync", Some(format!("{new_position_ms} ms")));
            }
            Err(e) => {
                // Se reanuda desde donde iba el decodificador: se pierde ese trozo, como antes.
                warn!("no se pudo volver a {heard_ms} ms al reanudar: {e}");
            }
        }
        true
    }

    /// La red se cortó a media canción (ver `PlayerEvent::Stalled`): la lectura del fichero ya
    /// esperó `download_timeout` sin datos. La canción pasa a pausa en lo último que se oyó, sin
    /// saltar, y se avisa. Lo que quedara en la cola de la salida ya sonó durante esa espera.
    fn enter_stall(&mut self, track_id: SpotifyUri, play_request_id: u64, error: &DecoderError) {
        let PlayerState::Playing {
            stream_position_ms, ..
        } = self.state
        else {
            return;
        };
        let heard_ms = out_queue::heard_ms(stream_position_ms, self.sink.queued_ms());
        warn!(
            "Se cortó la red a media canción <{track_id}> ({error}): en pausa en {heard_ms} ms hasta que vuelva, sin saltar"
        );
        ttfs::count(ttfs::Counter::Stalls);
        self.state.playing_to_paused();
        self.ensure_sink_paused(false);
        // Al reanudar se vuelve a decodificar desde `heard_ms`: lo poco que quedara en cola
        // sobraría, y un fundido a medias no tiene ya con qué seguir.
        self.sink.clear();
        self.paused_queue_ms = 0;
        self.clear_crossfade("corte de la red");
        if let PlayerState::Paused {
            ref mut stream_position_ms,
            ..
        } = self.state
        {
            *stream_position_ms = heard_ms;
        }
        self.stalled = Some(StallState {
            play_request_id,
            position_ms: heard_ms,
            probe: None,
            resume: false,
        });
        self.send_event(PlayerEvent::Stalled {
            play_request_id,
            track_id,
            position_ms: heard_ms,
        });
    }

    /// Avisa otra vez del corte (al pedir reanudar aún no llegaban datos): sigue en pausa.
    fn emit_stalled(&mut self) {
        let Some(position_ms) = self.stalled.as_ref().map(|s| s.position_ms) else {
            return;
        };
        if let PlayerState::Paused {
            ref track_id,
            play_request_id,
            ..
        } = self.state
        {
            let track_id = track_id.clone();
            self.send_event(PlayerEvent::Stalled {
                play_request_id,
                track_id,
                position_ms,
            });
        }
    }

    /// Respuesta de la comprobación de la red tras un corte (`probe_stalled_stream`).
    fn on_stall_probe(&mut self, back: bool) {
        let Some(stall) = self.stalled.as_mut() else {
            return;
        };
        stall.probe = None;
        if !stall.resume {
            // Se pausó (o se buscó) mientras tanto: el próximo «reproducir» vuelve a comprobar.
            return;
        }
        if back {
            self.resume_after_stall();
        } else {
            stall.resume = false;
            info!("Sigue sin llegar audio tras el corte de la red; la canción sigue en pausa");
            ttfs::mark_seq(ttfs::pending_of(&["resume"]), "stall-probe", Some("sin red".into()));
            self.emit_stalled();
        }
    }

    /// Vuelven a llegar datos: el decodificador se sitúa otra vez en lo último que se oyó (el
    /// lector se quedó a medias de una página del Ogg cuando falló la lectura) y suena desde ahí.
    fn resume_after_stall(&mut self) {
        let Some(stall) = self.stalled.take() else {
            return;
        };
        let PlayerState::Paused {
            play_request_id,
            ref mut decoder,
            ref mut stream_position_ms,
            ref stream_loader_controller,
            ..
        } = self.state
        else {
            return;
        };
        if play_request_id != stall.play_request_id {
            return;
        }
        match decoder.seek(stall.position_ms) {
            Ok(new_position_ms) => {
                *stream_position_ms = new_position_ms;
                // Buscar pasa la descarga a acceso aleatorio; para seguir sonando, en continuo.
                stream_loader_controller.set_stream_mode();
                info!("Vuelve la red: se sigue desde {new_position_ms} ms");
            }
            Err(e) => {
                // La red volvió a fallar justo al buscar: sigue en pausa y se avisa otra vez.
                warn!("no se pudo volver a {} ms tras el corte: {e}", stall.position_ms);
                self.stalled = Some(StallState {
                    probe: None,
                    resume: false,
                    ..stall
                });
                self.emit_stalled();
                return;
            }
        }
        self.paused_queue_ms = 0;
        self.handle_play();
    }

    fn handle_packet(
        &mut self,
        packet: Option<(AudioPacketPosition, AudioPacket)>,
        normalisation_factor: f64,
    ) {
        // Una saliente solo vive mientras alguien la escribe (ver `Crossfader`): aquí, mezclada
        // bajo la canción que suena; fuera de `Playing`, `poll` la escribe sola o la corta.
        debug_assert!(
            !self.crossfader.has_outgoing()
                || matches!(
                    self.state,
                    PlayerState::Playing { .. }
                        | PlayerState::Loading {
                            start_playback: true,
                            ..
                        }
                )
        );
        match packet {
            Some((_, mut packet)) => {
                if !packet.is_empty() {
                    if let AudioPacket::Samples(ref mut data) = packet {
                        // Get the volume for the packet. In the case of hardware volume control
                        // this will always be 1.0 (no change).
                        let volume = self.volume_getter.attenuation_factor();
                        let channels = NUM_CHANNELS as usize;
                        // Rampa de un cambio de nivel en vivo, si va hacia el factor de esta
                        // canción (si no, es de otra y ya no vale).
                        let mut ramp = self
                            .gain_ramp
                            .take()
                            .filter(|r| r.target() == normalisation_factor);

                        if self.crossfader.is_active() {
                            // Durante un fundido: la ganancia propia de esta canción (la saliente
                            // lleva la suya), la mezcla y, después, la dinámica sobre la suma.
                            // Fuera de él, el camino de siempre, intacto.
                            let track_gain = if self.config.normalisation {
                                normalisation_factor
                            } else {
                                1.0
                            };
                            gain::apply_gain(data, channels, track_gain, 1.0, &mut ramp);
                            let had_outgoing = self.crossfader.has_outgoing();
                            let mixing = self.crossfader.process(data);
                            if had_outgoing && !self.crossfader.has_outgoing() {
                                self.log_fade_end();
                            }
                            self.finish_mix(data, mixing, volume);
                        } else {
                            // For the basic normalisation method, a normalisation factor of 1.0
                            // indicates that there is nothing to normalise (all samples should
                            // pass unaltered). For the dynamic method, there may still be peaks
                            // that we want to shave off.
                            //
                            // No matter the case we apply volume attenuation last if there is any.
                            match (self.config.normalisation, self.config.normalisation_method) {
                                (false, _) => {
                                    // Sin normalización la ganancia es 1; la rampa solo existe
                                    // justo después de apagarla, para llegar a 1 sin chasquido.
                                    gain::apply_gain(data, channels, 1.0, volume, &mut ramp);
                                }
                                (true, NormalisationMethod::Dynamic) => {
                                    let mut stage_gain = normalisation_factor;

                                    for (i, sample) in data.iter_mut().enumerate() {
                                        // This implementation assumes audio is stereo.

                                        // step 0: apply gain stage. Durante una rampa la ganancia
                                        // cambia por marco (igual en los dos canales) y el
                                        // limitador actúa ya sobre ella.
                                        if i % channels == 0 {
                                            if let Some(r) = ramp.as_mut() {
                                                stage_gain = r.next_gain();
                                            }
                                        }
                                        *sample *= stage_gain;

                                        // steps 1-8, con el volumen al final.
                                        self.limit_sample(sample, volume);
                                    }
                                    if ramp.is_some_and(|r| r.finished()) {
                                        ramp = None;
                                    }
                                }
                                (true, NormalisationMethod::Basic) => {
                                    // Se aplica con `!= 1.0` y no solo `< 1.0`: el nivel Normal
                                    // también sube los másteres tranquilos, como Spotify.
                                    gain::apply_gain(data, channels, normalisation_factor, volume, &mut ramp);
                                }
                            }
                        }
                        self.gain_ramp = ramp;
                    }

                    let frames = match &packet {
                        AudioPacket::Samples(s) => (s.len() / NUM_CHANNELS as usize) as u64,
                        AudioPacket::Raw(_) => 0,
                    };
                    if let Err(e) = self.sink.write(packet, &mut self.converter) {
                        error!("{e}");
                        self.handle_pause();
                    } else {
                        self.frames_out += frames;
                    }
                }
            }

            None => {
                self.state.playing_to_end_of_track();
                if let PlayerState::EndOfTrack {
                    ref track_id,
                    play_request_id,
                    ..
                } = self.state
                {
                    self.send_event(PlayerEvent::EndOfTrack {
                        track_id: track_id.clone(),
                        play_request_id,
                    })
                } else {
                    error!("PlayerInternal handle_packet: Invalid PlayerState");
                    exit(1);
                }
            }
        }
    }

    /// Pasos 1-8 del limitador del nivel «Alto» sobre una muestra que ya lleva su ganancia, y el
    /// volumen al final. Es el bucle de siempre separado del paso 0 (la ganancia), para pasar por
    /// él también la suma de un fundido: mismas operaciones en el mismo orden, mismo resultado.
    #[inline]
    fn limit_sample(&mut self, sample: &mut f64, volume: f64) {
        // Feedforward limiter in the log domain
        // After: Giannoulis, D., Massberg, M., & Reiss, J.D. (2012).
        // Digital Dynamic Range Compressor Design—A Tutorial and
        // Analysis. Journal of The Audio Engineering Society, 60,
        // 399-408.

        // zero-cost shorthands
        let threshold_db = self.config.normalisation_threshold_dbfs;
        let knee_db = self.config.normalisation_knee_db;
        let attack_cf = self.config.normalisation_attack_cf;
        let release_cf = self.config.normalisation_release_cf;

        // step 1-4: half-wave rectification and conversion into dB, and
        // gain computer with soft knee and subtractor
        let limiter_db = {
            // Add slight DC offset. Some samples are silence, which is
            // -inf dB and gets the limiter stuck. Adding a small
            // positive offset prevents this.
            *sample += f64::MIN_POSITIVE;

            let bias_db = ratio_to_db(sample.abs()) - threshold_db;
            let knee_boundary_db = bias_db * 2.0;
            if knee_boundary_db < -knee_db {
                0.0
            } else if knee_boundary_db.abs() <= knee_db {
                let term = knee_boundary_db + knee_db;
                term * term * self.normalisation_knee_factor
            } else {
                bias_db
            }
        };

        // track left/right channel
        let channel = self.normalisation_channel;
        self.normalisation_channel ^= 1;

        // step 5: smooth, decoupled peak detector for each channel
        // Use direct references to reduce repeated array indexing
        let integrator = &mut self.normalisation_integrators[channel];
        let peak = &mut self.normalisation_peaks[channel];

        *integrator = f64::max(
            limiter_db,
            release_cf * *integrator + (1.0 - release_cf) * limiter_db,
        );
        *peak = attack_cf * *peak + (1.0 - attack_cf) * *integrator;

        // steps 6-8: conversion into level and multiplication into gain
        // stage. Find maximum peak across both channels to couple the
        // gain and maintain stereo imaging.
        let max_peak = f64::max(self.normalisation_peaks[0], self.normalisation_peaks[1]);
        *sample *= db_to_ratio(-max_peak) * volume;
    }

    /// Dinámica y volumen de un bloque de un fundido, ya mezclado (`mixing`: sonaban las dos).
    /// Con el nivel «Alto» su limitador actúa sobre la suma, como lo haría sobre una canción; con
    /// los demás, y solo mientras suenan dos, el limitador de la mezcla. La protección final, solo
    /// mientras suenan dos: con una sola canción no hay suma que pueda pasarse.
    fn finish_mix(&mut self, data: &mut [f64], mixing: bool, volume: f64) {
        let own_limiter = self.config.normalisation
            && self.config.normalisation_method == NormalisationMethod::Dynamic;
        if own_limiter {
            for sample in data.iter_mut() {
                self.limit_sample(sample, 1.0);
            }
        }
        if mixing {
            self.crossfader.protect(data, own_limiter);
        } else {
            // Ya suena una sola: si el limitador de la mezcla seguía reduciendo, se suelta sin
            // escalón (ver `Crossfader::settle`).
            self.crossfader.settle(data);
        }
        if volume != 1.0 {
            for sample in data.iter_mut() {
                *sample *= volume;
            }
        }
    }

    /// Escribe muestras del fundido (la saliente sola o su suelta) en la salida.
    fn write_samples(&mut self, samples: Vec<f64>) {
        let frames = (samples.len() / NUM_CHANNELS as usize) as u64;
        match self.sink.write(AudioPacket::Samples(samples), &mut self.converter) {
            Ok(()) => self.frames_out += frames,
            Err(e) => {
                // Sin salida no hay dónde terminar de apagarla: se quita.
                error!("{e}");
                self.clear_crossfade("error de la salida");
            }
        }
    }

    /// Un bloque de la saliente sola mientras la siguiente carga: el fundido sigue sonando en vez
    /// de cortarse, y la siguiente, al llegar, entra con lo que le quede de rampa.
    fn write_tail(&mut self) {
        // Si la salida no pudo abrirse, la pausa que eso provoca ya quitó la saliente.
        if !self.crossfader.has_outgoing() {
            return;
        }
        if let Some(mut block) = self.crossfader.tail() {
            let volume = self.volume_getter.attenuation_factor();
            self.finish_mix(&mut block, true, volume);
            self.write_samples(block);
        }
        if !self.crossfader.has_outgoing() {
            self.log_fade_end();
            // La anterior ya no suena y la siguiente aún no: ahora sí está «cargando».
            self.reveal_loading();
        }
    }

    /// Anuncia la carga en curso que se anunció como parte de un fundido (`loading_hidden`), una
    /// sola vez. Spirc la trata igual que la primera (misma petición); la interfaz pasa a
    /// «cargando», y un fallo posterior la deja en «parado» como siempre.
    fn reveal_loading(&mut self) {
        if !mem::take(&mut self.loading_hidden) {
            return;
        }
        if let PlayerState::Loading {
            ref track_id,
            play_request_id,
            ..
        } = self.state
        {
            let track_id = track_id.clone();
            // Una carga de fundido o de salto automático siempre empieza al principio.
            self.send_event(PlayerEvent::Loading {
                track_id,
                play_request_id,
                position_ms: 0,
                crossfading: false,
            });
        }
    }

    fn log_fade_end(&self) {
        info!("[fundido] terminado muestra={}", self.frames_out);
    }

    /// Corta el fundido en curso (otra canción, búsqueda, cambio de estado). Con la salida en
    /// marcha, la suelta de 40 ms de la saliente se escribe aquí mismo, antes de seguir con lo que
    /// se pidió; si no, no se oiría y se quita. Nunca queda una cola de la canción anterior que
    /// mezclar con lo que suene después. La escritura son dos bloques: como mucho lo que tarda la
    /// salida en hacer sitio para ellos, igual que un paquete normal.
    fn cancel_crossfade(&mut self, reason: &str) {
        self.crossfade_log = None;
        if !self.crossfader.is_active() {
            return;
        }
        let had_outgoing = self.crossfader.has_outgoing();
        let blocks = self
            .crossfader
            .cancel(self.sink_status == SinkStatus::Running);
        let released = !blocks.is_empty();
        let volume = self.volume_getter.attenuation_factor();
        for mut block in blocks {
            self.finish_mix(&mut block, true, volume);
            self.write_samples(block);
        }
        if had_outgoing {
            info!(
                "[fundido] cortado ({reason}){} muestra={}",
                if released { ", suelta de 40 ms" } else { "" },
                self.frames_out
            );
        }
    }

    /// Quita el fundido sin escribir nada (parar, pausa mientras cargaba, error de la salida).
    fn clear_crossfade(&mut self, reason: &str) {
        self.crossfade_log = None;
        if self.crossfader.clear() {
            info!("[fundido] quitado ({reason})");
        }
    }

    /// Propone a Spirc fundir con la siguiente (`PlayerEvent::CrossfadeReady`) cuando el final
    /// natural de la canción está a la distancia del fundido más la antelación y la siguiente ya
    /// está precargada. Una vez por canción; Spirc decide. Con el fundido apagado no hace nada.
    fn maybe_offer_crossfade(&mut self) {
        let cfg_ms = self.config.crossfade_ms;
        if cfg_ms == 0 || self.config.passthrough || self.crossfader.is_fading() {
            return;
        }
        let PlayerState::Playing {
            ref track_id,
            play_request_id,
            duration_ms,
            stream_position_ms,
            ref stream_loader_controller,
            ref audio_item,
            ..
        } = self.state
        else {
            return;
        };
        if self.crossfade_requested == Some(play_request_id) {
            return;
        }
        let PlayerPreload::Ready {
            track_id: ref next,
            ref loaded_track,
        } = self.preload
        else {
            return;
        };
        // Repetir la canción o un pódcast a cualquiera de los dos lados: nunca.
        if next == track_id || is_episode(track_id) || is_episode(next) {
            return;
        }
        let remaining_ms = duration_ms.saturating_sub(stream_position_ms);
        let Some(fade_ms) =
            crossfade::plan_fade_ms(cfg_ms, remaining_ms, duration_ms, loaded_track.duration_ms)
        else {
            return;
        };
        // La saliente se decodifica junto con la entrante: si aún tuviera que esperar a la red,
        // pararía las dos. Se funde solo con el resto del fichero ya descargado (si no, la
        // canción acaba sin hueco, como siempre).
        if !stream_loader_controller.range_to_end_available() {
            return;
        }
        let album_continuation = crossfade::is_album_continuation(
            &audio_item.unique_fields,
            &loaded_track.audio_item.unique_fields,
        );
        let (track_id, next) = (track_id.clone(), next.clone());
        self.crossfade_requested = Some(play_request_id);
        if album_continuation && !self.config.crossfade_albums {
            debug!("[fundido] {track_id} → {next}: no se funde, sigue el mismo álbum");
            return;
        }
        debug!("[fundido] propuesto {track_id} → {next}: {fade_ms} ms (quedan {remaining_ms} ms)");
        self.crossfade_plan = Some(FadePlan {
            play_request_id,
            next: next.clone(),
            fade_ms,
        });
        self.send_event(PlayerEvent::CrossfadeReady {
            play_request_id,
            track_id,
            next_track_id: next,
            fade_ms,
            album_continuation,
            crossfade_albums: self.config.crossfade_albums,
        });
    }

    /// Empieza el fundido que Spirc aceptó hacia `next`: la canción que suena pasa a ser la
    /// saliente (con su decodificador, su fichero y su ganancia) y el estado queda en `Stopped`,
    /// así la carga sigue su camino de siempre (precarga lista → `start_playback`, o cargar). El
    /// fundido dura lo previsto o lo que le quede a la canción, si es menos. `false` si ya no se
    /// puede (la canción acabó o está en pausa, se buscó, se apagó el fundido, es otra canción):
    /// entonces la carga es un corte normal.
    fn begin_crossfade(&mut self, next: &SpotifyUri) -> bool {
        let plan = self.crossfade_plan.take();
        if self.config.crossfade_ms == 0 || self.config.passthrough || self.crossfader.is_fading() {
            return false;
        }
        let PlayerState::Playing {
            ref track_id,
            play_request_id,
            duration_ms,
            stream_position_ms,
            ..
        } = self.state
        else {
            return false;
        };
        let Some(plan) = plan.filter(|p| p.play_request_id == play_request_id && p.next == *next)
        else {
            return false;
        };
        if track_id == next {
            return false;
        }
        let remaining_ms = duration_ms.saturating_sub(stream_position_ms);
        let fade_ms = plan.fade_ms.min(remaining_ms);
        if fade_ms == 0 {
            return false;
        }
        let (from, decoder, stream_loader_controller, normalisation_factor) =
            match mem::replace(&mut self.state, PlayerState::Stopped) {
                PlayerState::Playing {
                    track_id,
                    decoder,
                    stream_loader_controller,
                    normalisation_factor,
                    ..
                } => (track_id, decoder, stream_loader_controller, normalisation_factor),
                other => {
                    self.state = other;
                    return false;
                }
            };
        // Cada canción suena a su propio nivel también mientras se funden.
        let gain = if self.config.normalisation {
            normalisation_factor
        } else {
            1.0
        };
        // Una rampa de nivel a medias era hacia el de esta canción; la siguiente calcula el suyo.
        self.gain_ramp = None;
        let limiter = mix_limiter(&self.config);
        self.crossfader.begin(
            OutgoingTrack {
                track_id: from,
                decoder,
                _stream_loader_controller: stream_loader_controller,
            },
            gain,
            crossfade::ms_to_frames(fade_ms),
            next.clone(),
            limiter,
        );
        self.crossfade_log = Some(FadeLog {
            requested_ms: self.config.crossfade_ms,
            planned_ms: plan.fade_ms,
            remaining_ms,
        });
        true
    }

    /// Salto automático (`Transition::AutoSkip`, un salto que no pidió el usuario) durante un
    /// fundido: la saliente sigue apagándose y espera a `next`; la canción que entraba (oculta, o
    /// que no se pudo cargar) se descarta. Con un corte, los segundos que le faltaban a la
    /// anterior se perderían en 40 ms. `false` si no hay fundido que conservar.
    fn continue_after_auto_skip(&mut self, next: &SpotifyUri) -> bool {
        let writing = matches!(
            self.state,
            PlayerState::Playing { .. }
                | PlayerState::Loading {
                    start_playback: true,
                    ..
                }
        );
        if !writing || !self.crossfader.auto_skip(next.clone()) {
            return false;
        }
        // La entrante descartada acababa de empezar su rampa: sonaba aún muy baja.
        self.state = PlayerState::Stopped;
        info!("[fundido] salto automático a {next}: la anterior sigue apagándose");
        true
    }

    /// El fundido empezó: la entrante ya suena (tras `TrackChanged` y `Playing`).
    fn announce_crossfade(&mut self, to: &SpotifyUri, ramp: Ramp, log: Option<FadeLog>) {
        let Some(from) = self
            .crossfader
            .outgoing()
            .map(|o| o.source().track_id.clone())
        else {
            return;
        };
        let fade_ms = u32::try_from(crossfade::frames_to_ms(ramp.total())).unwrap_or(u32::MAX);
        match log {
            Some(l) => info!(
                "[fundido] {from} → {to}: {fade_ms} ms (pedido {}, previsto {}, quedaban {}) muestra={}",
                l.requested_ms, l.planned_ms, l.remaining_ms, self.frames_out
            ),
            None => info!(
                "[fundido] {from} → {to}: {fade_ms} ms (tras un salto automático) muestra={}",
                self.frames_out
            ),
        }
        self.send_event(PlayerEvent::CrossfadeStarted {
            from,
            to: to.clone(),
            fade_ms,
        });
    }

    /// Ajustes del fundido en vivo. Lo ya decidido para la canción que suena se vuelve a decidir
    /// con los valores nuevos; un fundido que ya suena termina como iba.
    fn handle_set_crossfade(&mut self, ms: u32, albums: bool) {
        let ms = ms.min(crossfade::CROSSFADE_MAX_MS);
        if (ms, albums) != (self.config.crossfade_ms, self.config.crossfade_albums) {
            info!("[fundido] ajustes: {ms} ms, también en álbumes: {albums}");
        }
        self.config.crossfade_ms = ms;
        self.config.crossfade_albums = albums;
        self.crossfade_requested = None;
        if ms == 0 {
            // Apagado: lo que se hubiera propuesto ya no empieza.
            self.crossfade_plan = None;
        }
    }

    fn start_playback(
        &mut self,
        track_id: SpotifyUri,
        play_request_id: u64,
        loaded_track: PlayerLoadedTrackData,
        start_playback: bool,
    ) {
        let audio_item = Box::new(loaded_track.audio_item.clone());

        self.send_event(PlayerEvent::TrackChanged { audio_item });

        let position_ms = loaded_track.stream_position_ms;

        // La salida se pone a sonar antes de decidir nada: si no hay ningún dispositivo, la
        // canción se carga en pausa (Nanofy lo explica al ver la pausa sin salida). Antes se daba
        // por sonando sin salida: la barra decía «sonando» sin que se oyera nada.
        let was_running = self.sink_status == SinkStatus::Running;
        let start_playback = start_playback && {
            let ok = self.ensure_sink_running();
            if !ok {
                warn!("no hay salida de audio: la canción queda cargada en pausa");
            }
            ok
        };

        let normalisation_factor =
            NormalisationData::get_factor(&self.resolved_config(), loaded_track.normalisation_data);
        // Una rampa a medias era hacia el nivel de la canción anterior.
        self.gain_ramp = None;

        // Lo que se estuviera cargando ya está aquí.
        self.loading_hidden = false;

        // ¿Entra fundiéndose con la que se va? Solo la canción esperada y si va a sonar. Si no, la
        // saliente se corta aquí (en pausa nadie la escribiría y otra canción no debe heredarla).
        if !(start_playback && self.crossfader.expects(&track_id)) {
            self.cancel_crossfade("la canción nueva no entra fundiéndose");
        }
        let fade = self.crossfader.start_incoming(&track_id, start_playback);
        let fade_log = self.crossfade_log.take();
        let fading_to = matches!(fade, IncomingStart::Fading(_)).then(|| track_id.clone());

        // La medida de esta carga; o, si no es la de ninguna orden, la de un «reanudar» pulsado
        // mientras cargaba (`handle_play` en `Loading`). También si la de la carga ya no está
        // pendiente: reproducir, pausar y reanudar mientras carga deja la de «reproducir»
        // abandonada, y la del «reanudar» es la que se cumple al sonar.
        let ttfs_seq = if ttfs::is_pending(self.ttfs_seq) {
            self.ttfs_seq
        } else if start_playback {
            ttfs::pending_of(&["resume"])
        } else {
            0
        };

        if start_playback {
            self.paused_queue_ms = 0;
            let sink = if was_running { "running" } else { "start" };
            ttfs::mark_seq(ttfs_seq, "sink", Some(sink.into()));
            self.send_event(PlayerEvent::Playing {
                track_id: track_id.clone(),
                play_request_id,
                position_ms,
            });
            // Se oye con la próxima escritura en la salida, que ya es de esta canción.
            ttfs::mark_seq(ttfs_seq, "playing", None);
            ttfs::arm_audible(ttfs_seq);

            self.state = PlayerState::Playing {
                track_id,
                play_request_id,
                decoder: loaded_track.decoder,
                audio_item: loaded_track.audio_item,
                normalisation_data: loaded_track.normalisation_data,
                normalisation_factor,
                stream_loader_controller: loaded_track.stream_loader_controller,
                duration_ms: loaded_track.duration_ms,
                bytes_per_second: loaded_track.bytes_per_second,
                stream_position_ms: loaded_track.stream_position_ms,
                reported_nominal_start_time: Instant::now()
                    .checked_sub(Duration::from_millis(position_ms as u64)),
                suggested_to_preload_next_track: false,
                is_explicit: loaded_track.is_explicit,
                audio_source: loaded_track.audio_source,
            };
            if let (IncomingStart::Fading(ramp), Some(to)) = (fade, fading_to) {
                self.announce_crossfade(&to, ramp, fade_log);
            }
        } else {
            self.ensure_sink_stopped(false);
            // `paused_queue_ms` ya es 0 (la carga vació la cola o la dejó sonar hasta el final),
            // salvo con la misma canción recargada en pausa en su mismo punto: entonces su cola
            // sigue ahí y se reanuda tal cual. No se vuelve a medir: si la salida no pudo
            // vaciarse (dispositivo caído), lo que quede sería de la canción anterior.
            // Cargada en pausa: no va a sonar, pero la orden ya está cumplida.
            ttfs::finish(ttfs_seq, "paused", None);

            self.state = PlayerState::Paused {
                track_id: track_id.clone(),
                play_request_id,
                decoder: loaded_track.decoder,
                audio_item: loaded_track.audio_item,
                normalisation_data: loaded_track.normalisation_data,
                normalisation_factor,
                stream_loader_controller: loaded_track.stream_loader_controller,
                duration_ms: loaded_track.duration_ms,
                bytes_per_second: loaded_track.bytes_per_second,
                stream_position_ms: loaded_track.stream_position_ms,
                suggested_to_preload_next_track: false,
                is_explicit: loaded_track.is_explicit,
                audio_source: loaded_track.audio_source,
            };

            self.send_event(PlayerEvent::Paused {
                track_id,
                play_request_id,
                position_ms: out_queue::heard_ms(position_ms, self.paused_queue_ms),
            });
        }
        self.emit_audio_format();
    }

    /// Cuenta qué suena y con qué ganancia (`PlayerEvent::AudioFormat`) si hay una canción
    /// sonando o en pausa. Se llama al empezar cada una y tras un cambio de ajustes en vivo.
    fn emit_audio_format(&mut self) {
        let config = self.resolved_config();
        let event = match &self.state {
            PlayerState::Playing {
                play_request_id,
                audio_item,
                audio_source,
                normalisation_data,
                normalisation_factor,
                ..
            }
            | PlayerState::Paused {
                play_request_id,
                audio_item,
                audio_source,
                normalisation_data,
                normalisation_factor,
                ..
            } => {
                let (normalisation_db, album_gain, gain_data) =
                    NormalisationData::summary(&config, *normalisation_data, *normalisation_factor);
                PlayerEvent::AudioFormat {
                    play_request_id: *play_request_id,
                    track_id: audio_item.track_id.clone(),
                    source: *audio_source,
                    normalisation_db,
                    album_gain,
                    gain_data,
                }
            }
            _ => return,
        };
        self.send_event(event);
    }

    /// La configuración con la normalización «Auto» resuelta a álbum o canción según lo último que
    /// indicó Spirc (`set_auto_normalise_as_album`, justo antes de cada carga).
    fn resolved_config(&self) -> PlayerConfig {
        let mut config = self.config.clone();
        if config.normalisation_type == NormalisationType::Auto {
            if self.auto_normalise_as_album {
                config.normalisation_type = NormalisationType::Album;
            } else {
                config.normalisation_type = NormalisationType::Track;
            }
        };
        config
    }

    /// Ajustes de audio nuevos sin reiniciar nada. La calidad y gapless valen desde la próxima
    /// carga (el cargador copia `self.config` cada vez). La normalización cambia al instante en la
    /// canción que suena, con una rampa desde la ganancia que se oía hasta la nueva.
    fn handle_set_audio_tuning(&mut self, tuning: AudioTuning) {
        let old = self.config.tuning();
        if old == tuning {
            return;
        }
        let limiter_was_on =
            old.normalisation && old.normalisation_method == NormalisationMethod::Dynamic;
        let limiter_on =
            tuning.normalisation && tuning.normalisation_method == NormalisationMethod::Dynamic;
        // Reducción que el limitador aplica en este instante (1 si no actúa): si deja de actuar,
        // esa reducción también desaparece y la rampa tiene que salir de lo que de verdad sonaba
        // (`gain::ramp_start`).
        let limiter_gain = if limiter_was_on {
            db_to_ratio(-f64::max(self.normalisation_peaks[0], self.normalisation_peaks[1]))
        } else {
            1.0
        };

        self.config.set_tuning(&tuning);
        self.normalisation_knee_factor = 1.0 / (8.0 * self.config.normalisation_knee_db);
        if limiter_on && !limiter_was_on {
            // Lo que guardaba era de la última vez que actuó (otra canción, otro nivel).
            self.normalisation_integrators = [0.0; 2];
            self.normalisation_peaks = [0.0; 2];
        }

        let config = self.resolved_config();
        let previous_ramp = self.gain_ramp.take();
        match self.state {
            PlayerState::Playing {
                normalisation_data,
                ref mut normalisation_factor,
                ..
            } => {
                let new_factor = NormalisationData::get_factor(&config, normalisation_data);
                let old_factor = mem::replace(normalisation_factor, new_factor);
                let from = gain::ramp_start(
                    old_factor,
                    previous_ramp,
                    limiter_gain,
                    limiter_was_on && !limiter_on,
                );
                if from != new_factor {
                    self.gain_ramp = Some(GainRamp::with_duration(from, new_factor, SAMPLE_RATE));
                }
                info!(
                    "Normalización cambiada en vivo: {:.2} dB → {:.2} dB",
                    ratio_to_db(old_factor),
                    ratio_to_db(new_factor)
                );
            }
            PlayerState::Paused {
                normalisation_data,
                ref mut normalisation_factor,
                ..
            } => {
                // En pausa no suena nada que pueda chasquear: el nivel nuevo vale al reanudar.
                *normalisation_factor = NormalisationData::get_factor(&config, normalisation_data);
            }
            // Cargando, parado o al final: lo próximo que empiece ya calcula con lo nuevo.
            _ => {}
        }
        // La etiqueta de calidad enseña la ganancia nueva sin esperar a la próxima canción.
        self.emit_audio_format();

        if tuning.bitrate != old.bitrate {
            // La siguiente canción ya precargada (gapless) se pidió con la calidad anterior: se
            // vuelve a pedir con la nueva. Si su fichero ya está en la caché, la caché sigue
            // mandando (`pick_audio_file`), así que sin red no se pierde nada.
            let next = match &self.preload {
                PlayerPreload::Loading { track_id, .. } | PlayerPreload::Ready { track_id, .. } => {
                    Some(track_id.clone())
                }
                PlayerPreload::None => None,
            };
            if let Some(track_id) = next {
                self.preload = PlayerPreload::None;
                self.handle_command_preload(track_id);
            }
            // Lo preparado por adelantado también era de la calidad anterior.
            self.warm = PlayerPreload::None;
        }
    }

    fn handle_command_load(
        &mut self,
        track_id: SpotifyUri,
        play_request_id_option: Option<u64>,
        play: bool,
        position_ms: u32,
        transition: Transition,
    ) -> PlayerResult {
        let play_request_id =
            play_request_id_option.unwrap_or(self.play_request_id_generator.get());

        // Una carga sustituye a la canción cortada por la red, aunque sea ella misma: Nanofy la
        // vuelve a cargar así cuando el enlace de la CDN pudo caducar. Su decodificador no se
        // reutiliza: se quedó a medias de una lectura y su fichero apunta a ese enlace.
        let was_stalled = self.stalled.take().is_some();

        // ¿Es la carga de una orden medida (`ttfs`)? Una búsqueda durante la carga la repite con
        // el mismo `play_request_id`: sigue siendo la misma carga y conserva su medida.
        let claimed = ttfs::claim(|| track_id.to_string());
        if claimed != 0 || play_request_id_option.is_none() {
            self.ttfs_seq = claimed;
        }

        self.send_event(PlayerEvent::PlayRequestIdChanged { play_request_id });

        // ¿Sigue a una canción que terminó sola? Entonces lo que queda en la cola de la salida es
        // su final y debe sonar (sin hueco con gapless; sin gapless, antes de la pausa entre
        // canciones). También tras un salto automático desde una carga que falló (la siguiente
        // no estaba disponible): lo que queda en cola es aún el final de la que terminó. Se mira
        // antes de decidir el fundido, que cambia el estado.
        let after_natural_end = match self.state {
            PlayerState::EndOfTrack { .. } => true,
            PlayerState::Loading { .. } | PlayerState::Stopped => {
                transition == Transition::AutoSkip
            }
            _ => false,
        };
        // La misma canción en el mismo punto del decodificador (Spirc la vuelve a cargar tal
        // cual): nada que tirar, lo encolado continúa.
        let same_spot = match &self.state {
            PlayerState::Playing {
                track_id: current,
                stream_position_ms,
                ..
            }
            | PlayerState::Paused {
                track_id: current,
                stream_position_ms,
                ..
            } => !was_stalled && *current == track_id && *stream_position_ms == position_ms,
            _ => false,
        };

        // El fundido se decide antes de tocar la salida o el estado: empezarlo (Spirc aceptó el
        // propuesto), conservarlo tras un salto automático o, en cualquier otro caso, cortar el que
        // hubiera, soltándolo ya. Solo una canción que empieza a sonar desde el principio funde.
        let fresh_start = play && position_ms == 0;
        let crossfading = match transition {
            Transition::Crossfade => fresh_start && self.begin_crossfade(&track_id),
            Transition::AutoSkip => fresh_start && self.continue_after_auto_skip(&track_id),
            Transition::Cut => false,
        };
        if !crossfading {
            if after_natural_end || same_spot {
                self.cancel_crossfade("otra canción");
                // Sin gapless la salida se vacía entre canciones: el final de la anterior suena
                // entero antes de la pausa.
                if !self.config.gapless {
                    self.ensure_sink_stopped(play);
                    ttfs::mark_seq(self.ttfs_seq, "sink-drain", None);
                }
            } else {
                // Un corte pedido (otra canción, «siguiente», volver a buscar): lo que la salida
                // tenía en cola de la anterior (0,3-0,5 s) se tira en vez de sonar antes de la
                // nueva. Sin gapless, además, se calla al instante en vez de esperar a que se
                // vacíe, que bloqueaba este hilo en cada clic. Un fundido a medias se quita sin
                // su suelta de 40 ms: tras tirar la cola, esa suelta sería un trozo suelto de la
                // saliente (de 0,4 s más adelante) y se comería la rampa de entrada de la nueva.
                self.sink.clear();
                if !self.config.gapless {
                    self.ensure_sink_paused(play);
                }
                self.clear_crossfade("otra canción");
                ttfs::mark_seq(self.ttfs_seq, "sink-cut", None);
            }
        }
        // Lo que viene ahora sustituye a lo pausado (o lo reanuda en el mismo punto).
        if !same_spot {
            self.paused_queue_ms = 0;
        }

        if matches!(self.state, PlayerState::Invalid) {
            return Err(Error::internal(format!(
                "Player::handle_command_load called from invalid state: {:?}",
                self.state
            )));
        }

        // Now we check at different positions whether we already have a pre-loaded version
        // of this track somewhere. If so, use it and return.

        // Check if there's a matching loaded track in the EndOfTrack player state.
        // This is the case if we're repeating the same track again.
        if let PlayerState::EndOfTrack {
            track_id: previous_track_id,
            ..
        } = &self.state
        {
            if *previous_track_id == track_id {
                let mut loaded_track = match mem::replace(&mut self.state, PlayerState::Invalid) {
                    PlayerState::EndOfTrack { loaded_track, .. } => loaded_track,
                    _ => {
                        return Err(Error::internal(format!(
                            "PlayerInternal::handle_command_load repeating the same track: invalid state: {:?}",
                            self.state
                        )));
                    }
                };

                if position_ms != loaded_track.stream_position_ms {
                    // This may be blocking.
                    loaded_track.stream_position_ms = loaded_track.decoder.seek(position_ms)?;
                }
                ttfs::mark_seq(self.ttfs_seq, "reuse", Some("end-of-track".into()));
                self.preload = PlayerPreload::None;
                self.start_playback(track_id, play_request_id, loaded_track, play);
                if let PlayerState::Invalid = self.state {
                    return Err(Error::internal(format!(
                        "PlayerInternal::handle_command_load repeating the same track: start_playback() did not transition to valid player state: {:?}",
                        self.state
                    )));
                }
                return Ok(());
            }
        }

        // Check if we are already playing the track. If so, just do a seek and update our info.
        if let PlayerState::Playing {
            track_id: ref current_track_id,
            ref mut stream_position_ms,
            ref mut decoder,
            ..
        }
        | PlayerState::Paused {
            track_id: ref current_track_id,
            ref mut stream_position_ms,
            ref mut decoder,
            ..
        } = self.state
        {
            if *current_track_id == track_id && !was_stalled {
                // we can use the current decoder. Ensure it's at the correct position.
                if position_ms != *stream_position_ms {
                    // This may be blocking.
                    *stream_position_ms = decoder.seek(position_ms)?;
                }
                ttfs::mark_seq(self.ttfs_seq, "reuse", Some("same-track".into()));

                // Move the info from the current state into a PlayerLoadedTrackData so we can use
                // the usual code path to start playback.
                let old_state = mem::replace(&mut self.state, PlayerState::Invalid);

                if let PlayerState::Playing {
                    stream_position_ms,
                    decoder,
                    audio_item,
                    stream_loader_controller,
                    bytes_per_second,
                    duration_ms,
                    normalisation_data,
                    is_explicit,
                    audio_source,
                    ..
                }
                | PlayerState::Paused {
                    stream_position_ms,
                    decoder,
                    audio_item,
                    stream_loader_controller,
                    bytes_per_second,
                    duration_ms,
                    normalisation_data,
                    is_explicit,
                    audio_source,
                    ..
                } = old_state
                {
                    let loaded_track = PlayerLoadedTrackData {
                        decoder,
                        normalisation_data,
                        stream_loader_controller,
                        audio_item,
                        bytes_per_second,
                        duration_ms,
                        stream_position_ms,
                        is_explicit,
                        audio_source,
                    };

                    self.preload = PlayerPreload::None;
                    self.start_playback(track_id, play_request_id, loaded_track, play);

                    if let PlayerState::Invalid = self.state {
                        return Err(Error::internal(format!(
                            "PlayerInternal::handle_command_load already playing this track: start_playback() did not transition to valid player state: {:?}",
                            self.state
                        )));
                    }

                    return Ok(());
                } else {
                    return Err(Error::internal(format!(
                        "PlayerInternal::handle_command_load already playing this track: invalid state: {:?}",
                        self.state
                    )));
                }
            }
        }

        // Check if the requested track has been preloaded already. If so use the preloaded data.
        if let PlayerPreload::Ready {
            track_id: loaded_track_id,
            ..
        } = &self.preload
        {
            if track_id == *loaded_track_id {
                let preload = std::mem::replace(&mut self.preload, PlayerPreload::None);
                if let PlayerPreload::Ready {
                    track_id,
                    mut loaded_track,
                } = preload
                {
                    if position_ms != loaded_track.stream_position_ms {
                        // This may be blocking
                        loaded_track.stream_position_ms = loaded_track.decoder.seek(position_ms)?;
                    }
                    ttfs::mark_seq(self.ttfs_seq, "preload", Some("ready".into()));
                    self.start_playback(track_id, play_request_id, *loaded_track, play);
                    return Ok(());
                } else {
                    return Err(Error::internal(format!(
                        "PlayerInternal::handle_command_loading preloaded track: invalid state: {:?}",
                        self.state
                    )));
                }
            }
        }

        // La canción preparada por adelantado (`Player::warm`: se acaba de pulsar), ya lista.
        if matches!(&self.warm, PlayerPreload::Ready { track_id: warmed, .. } if *warmed == track_id)
        {
            if let PlayerPreload::Ready {
                track_id,
                mut loaded_track,
            } = mem::replace(&mut self.warm, PlayerPreload::None)
            {
                if position_ms != loaded_track.stream_position_ms {
                    // This may be blocking
                    loaded_track.stream_position_ms = loaded_track.decoder.seek(position_ms)?;
                }
                ttfs::mark_seq(self.ttfs_seq, "warm", Some("ready".into()));
                // Como en una carga nueva: la precarga era la siguiente de lo que sonaba.
                self.preload = PlayerPreload::None;
                self.start_playback(track_id, play_request_id, *loaded_track, play);
                return Ok(());
            }
        }

        let crossfading = self.crossfader.has_outgoing();
        self.loading_hidden = crossfading;
        self.send_event(PlayerEvent::Loading {
            track_id: track_id.clone(),
            play_request_id,
            position_ms,
            crossfading,
        });

        // Try to extract a pending loader from the preloading mechanism
        let loader = if let PlayerPreload::Loading {
            track_id: loaded_track_id,
            ..
        } = &self.preload
        {
            if (track_id == *loaded_track_id) && (position_ms == 0) {
                let mut preload = PlayerPreload::None;
                std::mem::swap(&mut preload, &mut self.preload);
                if let PlayerPreload::Loading { loader, .. } = preload {
                    // La precarga no estaba atada a ninguna medida: sus fases no se ven.
                    ttfs::mark_seq(self.ttfs_seq, "preload", Some("loading".into()));
                    Some(loader)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        // …o de la que se preparó por adelantado y aún carga (`Player::warm`).
        let loader = loader.or_else(|| self.take_warm_loader(&track_id, position_ms));

        self.preload = PlayerPreload::None;
        // Lo preparado por adelantado era para esta carga: si era otra canción (se apretó una y
        // se pidió otra) ya no se va a pedir.
        self.warm = PlayerPreload::None;

        // If we don't have a loader yet, create one from scratch.
        let loader =
            loader.unwrap_or_else(|| {
                Box::pin(self.load_track(track_id.clone(), position_ms, self.ttfs_seq, false))
            });

        // La carga va en su propio hilo: mientras tanto se abre aquí la salida (WASAPI tarda unos
        // 70 ms la primera vez y tras soltarla en pausa), en pausa y sin sonar, para que al llegar
        // la canción solo haya que darle a reproducir. Solo si va a sonar: restaurar la sesión al
        // abrir (en pausa) no toca el dispositivo. Si al final no suena, la salida se suelta a los
        // 5 s como en cualquier pausa (el fallo o la pausa lo avisan).
        if play && self.sink_status != SinkStatus::Running {
            if let Err(e) = self.sink.prepare() {
                debug!("no se pudo preparar la salida de audio: {e}");
            }
            ttfs::mark_seq(self.ttfs_seq, "sink-prepare", None);
        }

        // Set ourselves to a loading state.
        self.state = PlayerState::Loading {
            track_id,
            play_request_id,
            start_playback: play,
            loader,
        };

        Ok(())
    }

    /// El cargador de la canción preparada por adelantado (`Player::warm`), si es esta y aún
    /// carga desde el principio.
    fn take_warm_loader(&mut self, track_id: &SpotifyUri, position_ms: u32) -> Option<Loader> {
        if position_ms != 0
            || !matches!(&self.warm, PlayerPreload::Loading { track_id: warming, .. } if warming == track_id)
        {
            return None;
        }
        match mem::replace(&mut self.warm, PlayerPreload::None) {
            PlayerPreload::Loading { loader, .. } => {
                ttfs::mark_seq(self.ttfs_seq, "warm", Some("loading".into()));
                Some(loader)
            }
            _ => None,
        }
    }

    /// Ver `Player::warm`.
    fn handle_command_warm(&mut self, track_id: SpotifyUri, for_play: bool) {
        self.expire_warm();
        // Ya suena, ya carga o ya está preparada: nada que hacer.
        let current = match &self.state {
            PlayerState::Playing { track_id, .. }
            | PlayerState::Paused { track_id, .. }
            | PlayerState::EndOfTrack { track_id, .. } => Some(track_id),
            PlayerState::Loading {
                track_id, loader, ..
            } if !loader.is_terminated() => Some(track_id),
            _ => None,
        };
        if current == Some(&track_id)
            || self.preload.track_id() == Some(&track_id)
            || self.warm.track_id() == Some(&track_id)
        {
            return;
        }
        debug!("Warming track {track_id:?}");
        // La de una orden de reproducir: sus fases van a la medida de esa orden, sin que su fallo
        // la dé por fallida (ver `ttfs::bind_thread_speculative`).
        let seq = if for_play {
            ttfs::pending_of(&["play"])
        } else {
            0
        };
        // Lo preparado de otra canción ya no se va a pedir: se suelta, y su hilo se abandona en
        // su próxima espera (no pide más claves).
        let loader = self.load_track(track_id.clone(), 0, seq, true);
        self.warm = PlayerPreload::Loading {
            track_id,
            loader: Box::pin(loader),
        };
        self.warm_since = Instant::now();
    }

    /// Suelta lo preparado con `warm` si pasó `WARM_TTL` sin que nadie lo pidiera.
    fn expire_warm(&mut self) {
        if !matches!(self.warm, PlayerPreload::None) && self.warm_since.elapsed() >= WARM_TTL {
            debug!("Dropping the warmed track: nobody asked for it");
            self.warm = PlayerPreload::None;
        }
    }

    fn handle_command_preload(&mut self, track_id: SpotifyUri) {
        debug!("Preloading track");
        // La misma que se preparó por adelantado: pasa a ser la precarga, sin pedirla otra vez.
        if self.warm.track_id() == Some(&track_id) && self.preload.track_id() != Some(&track_id) {
            self.preload = mem::replace(&mut self.warm, PlayerPreload::None);
            return;
        }
        let mut preload_track = true;
        // check whether the track is already loaded somewhere or being loaded.
        if let PlayerPreload::Loading {
            track_id: currently_loading,
            ..
        }
        | PlayerPreload::Ready {
            track_id: currently_loading,
            ..
        } = &self.preload
        {
            if *currently_loading == track_id {
                // we're already preloading the requested track.
                preload_track = false;
            } else {
                // we're preloading something else - cancel it.
                self.preload = PlayerPreload::None;
            }
        }

        if let PlayerState::Playing {
            track_id: current_track_id,
            ..
        }
        | PlayerState::Paused {
            track_id: current_track_id,
            ..
        }
        | PlayerState::EndOfTrack {
            track_id: current_track_id,
            ..
        } = &self.state
        {
            if *current_track_id == track_id {
                // we already have the requested track loaded.
                preload_track = false;
            }
        }

        // La misma que acaba de fallar al precargarse: Spirc la vuelve a pedir con cada aviso de
        // estado y, si Spotify está frenando, cada intento es otra clave. Se carga al llegarle
        // el turno.
        if preload_track
            && self.preload_failed.as_ref().is_some_and(|(failed, at)| {
                *failed == track_id && at.elapsed() < PRELOAD_RETRY_AFTER
            })
        {
            debug!("Not preloading {track_id:?} again so soon after it failed");
            preload_track = false;
        }

        // schedule the preload of the current track if desired.
        if preload_track {
            let loader = self.load_track(track_id.clone(), 0, 0, false);
            self.preload = PlayerPreload::Loading {
                track_id,
                loader: Box::pin(loader),
            }
        }
    }

    fn handle_command_seek(&mut self, position_ms: u32) -> PlayerResult {
        // Medida de una búsqueda; también de un «anterior» pasados unos segundos de canción, que
        // Spirc convierte en volver al principio.
        let ttfs_seq = ttfs::pending_of(&["seek", "prev"]);
        ttfs::mark_seq(ttfs_seq, "player:seek", None);

        // Cortada por la red: buscar ahora esperaría a una red que no está. Se apunta el punto
        // nuevo y se buscará al reanudar, cuando vuelvan a llegar datos (la cola de la salida y
        // el fundido ya se quitaron al cortarse).
        if let PlayerState::Paused {
            ref track_id,
            play_request_id,
            ref mut stream_position_ms,
            ..
        } = self.state
        {
            if let Some(stall) = self
                .stalled
                .as_mut()
                .filter(|s| s.play_request_id == play_request_id)
            {
                stall.position_ms = position_ms;
                *stream_position_ms = position_ms;
                let track_id = track_id.clone();
                self.send_event(PlayerEvent::Seeked {
                    play_request_id,
                    track_id,
                    position_ms,
                });
                ttfs::finish(ttfs_seq, "paused", None);
                return Ok(());
            }
        }

        // Lo que la salida tenía en cola es de antes de la búsqueda: se tira ya, también en pausa.
        // Si no, sonando se oían 0,3-0,5 s más del punto viejo antes de saltar, y en pausa
        // sonaban al reanudar, antes de la posición nueva.
        let cleared = matches!(
            self.state,
            PlayerState::Playing { .. } | PlayerState::Paused { .. }
        );
        if cleared {
            self.sink.clear();
            self.paused_queue_ms = 0;
        }

        // Buscar corta el fundido, como en Spotify: la que suena sigue a su volumen desde la
        // posición pedida. Con la cola ya tirada, la saliente se quita sin su suelta de 40 ms:
        // sonaría como un trozo suelto tras el corte y se comería la rampa de entrada de la nueva
        // posición. Y si se vuelve atrás, al acercarse otra vez el final se puede volver a proponer.
        self.crossfade_requested = None;
        if cleared {
            self.clear_crossfade("búsqueda");
        } else {
            self.cancel_crossfade("búsqueda");
        }

        // When we are still loading, the user may immediately ask to
        // seek to another position yet the decoder won't be ready for
        // that. In this case just restart the loading process but
        // with the requested position.
        if let PlayerState::Loading {
            ref track_id,
            play_request_id,
            start_playback,
            ..
        } = self.state
        {
            return self.handle_command_load(
                track_id.clone(),
                Some(play_request_id),
                start_playback,
                position_ms,
                Transition::Cut,
            );
        }

        if let Some(decoder) = self.state.decoder() {
            match decoder.seek(position_ms) {
                Ok(new_position_ms) => {
                    if let PlayerState::Playing {
                        ref mut stream_position_ms,
                        ref track_id,
                        play_request_id,
                        ..
                    }
                    | PlayerState::Paused {
                        ref mut stream_position_ms,
                        ref track_id,
                        play_request_id,
                        ..
                    } = self.state
                    {
                        *stream_position_ms = new_position_ms;

                        self.send_event(PlayerEvent::Seeked {
                            play_request_id,
                            track_id: track_id.clone(),
                            position_ms: new_position_ms,
                        });
                    }
                    ttfs::mark_seq(ttfs_seq, "decoder:seek", Some(format!("{position_ms} ms")));
                }
                Err(e) => {
                    error!("PlayerInternal::handle_command_seek error: {e}");
                    ttfs::fail(ttfs_seq, "búsqueda fallida");
                }
            }
        } else {
            error!("Player::seek called from invalid state: {:?}", self.state);
        }

        // ensure we have a bit of a buffer of downloaded data
        self.preload_data_before_playback()?;
        // Espera bloqueante a tener datos por delante (hoy, los de la reproducción en curso).
        ttfs::mark_seq(ttfs_seq, "buffer", None);
        if self.state.is_playing() {
            ttfs::arm_audible(ttfs_seq);
        } else {
            ttfs::finish(ttfs_seq, "paused", None);
        }

        if let PlayerState::Playing {
            ref mut reported_nominal_start_time,
            ..
        } = self.state
        {
            *reported_nominal_start_time =
                Instant::now().checked_sub(Duration::from_millis(position_ms as u64));
        }

        Ok(())
    }

    fn handle_command(&mut self, cmd: PlayerCommand) -> PlayerResult {
        debug!("command={cmd:?}");
        match cmd {
            PlayerCommand::Load {
                track_id,
                play,
                position_ms,
                transition,
            } => self.handle_command_load(track_id, None, play, position_ms, transition)?,

            PlayerCommand::Preload { track_id } => self.handle_command_preload(track_id),

            PlayerCommand::Warm { track_id, for_play } => {
                self.handle_command_warm(track_id, for_play)
            }

            PlayerCommand::Seek(position_ms) => self.handle_command_seek(position_ms)?,

            PlayerCommand::Play => self.handle_play(),

            PlayerCommand::Pause => self.handle_pause(),

            PlayerCommand::Stop => self.handle_player_stop(),

            PlayerCommand::ReleaseSink => {
                // Una carga en curso que va a sonar acaba de preparar la salida (`prepare`):
                // soltarla ahora solo obligaría a abrirla otra vez dentro de un momento. Una que ya
                // falló (queda en `Loading` con el cargador terminado) no la retiene.
                let loading_to_play = matches!(
                    self.state,
                    PlayerState::Loading {
                        start_playback: true,
                        ref loader,
                        ..
                    } if !loader.is_terminated()
                );
                if self.sink_status != SinkStatus::Running && !loading_to_play {
                    // Lo que hubiera en cola se va con ella; si era de una pausa, al reanudar se
                    // vuelve a decodificar desde lo oído (`handle_play`).
                    self.sink.clear();
                    self.sink.release();
                }
            }

            PlayerCommand::SetSession(session) => self.session = session,

            PlayerCommand::AddEventSender(sender) => self.event_senders.push(sender),

            PlayerCommand::SetSinkEventCallback(callback) => self.sink_event_callback = callback,

            PlayerCommand::EmitVolumeChangedEvent(volume) => {
                self.send_event(PlayerEvent::VolumeChanged { volume })
            }

            PlayerCommand::EmitRepeatChangedEvent { context, track } => {
                // Con «repetir canción» Spirc no acepta fundidos: al cambiarla, lo ya decidido
                // para esta canción se vuelve a decidir.
                self.crossfade_requested = None;
                self.send_event(PlayerEvent::RepeatChanged { context, track })
            }

            PlayerCommand::EmitShuffleChangedEvent(shuffle) => {
                self.send_event(PlayerEvent::ShuffleChanged { shuffle })
            }

            PlayerCommand::EmitJamQueueEvent {
                current,
                context,
                next,
            } => self.send_event(PlayerEvent::JamQueue {
                current,
                context,
                next,
            }),

            PlayerCommand::EmitClusterSnapshotEvent(cluster) => {
                self.send_event(PlayerEvent::ClusterSnapshot { cluster })
            }

            PlayerCommand::EmitAutoPlayChangedEvent(auto_play) => {
                self.send_event(PlayerEvent::AutoPlayChanged { auto_play })
            }

            PlayerCommand::EmitSkipCascadeEvent(failed) => {
                self.send_event(PlayerEvent::SkipCascade { failed })
            }

            PlayerCommand::EmitSessionClientChangedEvent {
                client_id,
                client_name,
                client_brand_name,
                client_model_name,
            } => self.send_event(PlayerEvent::SessionClientChanged {
                client_id,
                client_name,
                client_brand_name,
                client_model_name,
            }),

            PlayerCommand::EmitSessionConnectedEvent {
                connection_id,
                user_name,
            } => self.send_event(PlayerEvent::SessionConnected {
                connection_id,
                user_name,
            }),

            PlayerCommand::EmitSessionDisconnectedEvent {
                connection_id,
                user_name,
            } => self.send_event(PlayerEvent::SessionDisconnected {
                connection_id,
                user_name,
            }),

            PlayerCommand::SetAutoNormaliseAsAlbum(setting) => {
                self.auto_normalise_as_album = setting
            }

            PlayerCommand::SetAudioTuning(tuning) => self.handle_set_audio_tuning(tuning),

            PlayerCommand::SetCrossfade { ms, albums } => self.handle_set_crossfade(ms, albums),

            PlayerCommand::EmitFilterExplicitContentChangedEvent(filter) => {
                self.send_event(PlayerEvent::FilterExplicitContentChanged { filter });

                if filter {
                    if let PlayerState::Playing {
                        ref track_id,
                        play_request_id,
                        is_explicit,
                        ..
                    }
                    | PlayerState::Paused {
                        ref track_id,
                        play_request_id,
                        is_explicit,
                        ..
                    } = self.state
                    {
                        let track_id = track_id.clone();

                        if is_explicit {
                            warn!(
                                "Currently loaded track is explicit, which client setting forbids -- skipping to next track."
                            );
                            self.send_event(PlayerEvent::EndOfTrack {
                                track_id,
                                play_request_id,
                            })
                        }
                    }
                }
            }
        };

        Ok(())
    }

    fn send_event(&mut self, event: PlayerEvent) {
        self.event_senders
            .retain(|sender| sender.send(event.clone()).is_ok());
    }

    /// `ttfs_seq`: medida (`ttfs`) a la que pertenece esta carga; 0 si a ninguna (precarga).
    /// `speculative`: carga por adelantado (`Player::warm`), ver `ttfs::bind_thread_speculative`.
    fn load_track(
        &mut self,
        spotify_uri: SpotifyUri,
        position_ms: u32,
        ttfs_seq: u64,
        speculative: bool,
    ) -> impl FusedFuture<Output = Result<PlayerLoadedTrackData, LoadFailure>> + Send + 'static {
        // This method creates a future that returns the loaded stream and associated info.
        // Ideally all work should be done using asynchronous code. However, seek() on the
        // audio stream is implemented in a blocking fashion. Thus, we can't turn it into future
        // easily. Instead we spawn a thread to do the work and return a one-shot channel as the
        // future to work with.

        let loader = PlayerTrackLoader {
            session: self.session.clone(),
            config: self.config.clone(),
            local_file_lookup: self.local_file_lookup.clone(),
        };

        let (result_tx, result_rx) = oneshot::channel();

        let load_handles_clone = self.load_handles.clone();
        let handle = tokio::runtime::Handle::current();

        let load_handle = thread::spawn(move || {
            let mut result_tx = result_tx;
            // Un hilo por carga: lo que marque (también la red, que no sabe de cargas) va a la
            // medida de esta carga y no a la de otra.
            if speculative {
                ttfs::bind_thread_speculative(ttfs_seq);
            } else {
                ttfs::bind_thread(ttfs_seq);
            }
            // Si ya nadie espera esta carga (otra canción, «siguiente» varias veces seguidas, otra
            // precarga), se abandona en su próxima espera en vez de terminarla: con los reintentos
            // de la clave, una carga abandonada podía pedir hasta tres claves más justo cuando
            // Spotify está frenando, y su resultado se tiraba igualmente.
            let result = handle.block_on(async {
                let load = std::pin::pin!(loader.load_track(spotify_uri, position_ms));
                let abandoned = std::pin::pin!(result_tx.closed());
                match futures_util::future::select(load, abandoned).await {
                    futures_util::future::Either::Left((result, _)) => Some(result),
                    futures_util::future::Either::Right(_) => None,
                }
            });
            match result {
                Some(result) => {
                    let _ = result_tx.send(result);
                }
                None => debug!("Track load abandoned: nobody is waiting for it any more"),
            }

            let mut load_handles = load_handles_clone.lock().expect(LOAD_HANDLES_POISON_MSG);
            load_handles.remove(&thread::current().id());
        });

        let mut load_handles = self.load_handles.lock().expect(LOAD_HANDLES_POISON_MSG);
        load_handles.insert(load_handle.thread().id(), load_handle);

        // Si el hilo terminó sin decir nada (murió), es como antes: un fallo definitivo.
        result_rx.map(|result| {
            result.unwrap_or_else(|_| Err(LoadFailure::Decode("la carga se interrumpió".into())))
        })
    }

    fn preload_data_before_playback(&mut self) -> PlayerResult {
        if let PlayerState::Playing {
            bytes_per_second,
            ref mut stream_loader_controller,
            ..
        } = self.state
        {
            let params = AudioFetchParams::get();
            // Request our read ahead range
            let request_data_length =
                (params.read_ahead_during_playback.as_secs_f32() * bytes_per_second as f32) as usize;

            // Se pide lo mismo que antes (5 s por delante), pero el hilo del reproductor solo
            // espera a tener `read_ahead_before_playback` (0,5 s, ver `main`): esperar los 5 s
            // enteros tras cada búsqueda bloqueaba Pausa/Siguiente y retrasaba el sonido sin que
            // hiciera falta, porque el resto sigue llegando mientras suena. Ese parámetro de
            // librespot no lo leía nadie.
            let wait_for_data_length =
                (params.read_ahead_before_playback.as_secs_f32() * bytes_per_second as f32) as usize;

            stream_loader_controller.fetch_next_and_wait(request_data_length, wait_for_data_length)
        } else {
            Ok(())
        }
    }
}

impl Drop for PlayerInternal {
    fn drop(&mut self) {
        debug!("drop PlayerInternal[{}]", self.player_id);
        // Lo preparado por adelantado ya no lo va a pedir nadie: soltarlo antes de esperar a los
        // hilos de carga deja que el suyo se abandone en su próxima espera, en vez de terminar
        // (hasta 10 s con una CDN que no contesta) mientras se cierra o se reconecta.
        self.warm = PlayerPreload::None;

        let handles: Vec<thread::JoinHandle<()>> = {
            // waiting for the thread while holding the mutex would result in a deadlock
            let mut load_handles = self.load_handles.lock().expect(LOAD_HANDLES_POISON_MSG);

            load_handles
                .drain()
                .map(|(_thread_id, handle)| handle)
                .collect()
        };

        for handle in handles {
            let _ = handle.join();
        }
    }
}

impl fmt::Debug for PlayerCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlayerCommand::Load {
                track_id,
                play,
                position_ms,
                transition,
            } => f
                .debug_tuple("Load")
                .field(&track_id)
                .field(&play)
                .field(&position_ms)
                .field(&transition)
                .finish(),
            PlayerCommand::Preload { track_id } => {
                f.debug_tuple("Preload").field(&track_id).finish()
            }
            PlayerCommand::Warm { track_id, for_play } => f
                .debug_tuple("Warm")
                .field(&track_id)
                .field(&for_play)
                .finish(),
            PlayerCommand::Play => f.debug_tuple("Play").finish(),
            PlayerCommand::Pause => f.debug_tuple("Pause").finish(),
            PlayerCommand::Stop => f.debug_tuple("Stop").finish(),
            PlayerCommand::ReleaseSink => f.debug_tuple("ReleaseSink").finish(),
            PlayerCommand::Seek(position) => f.debug_tuple("Seek").field(&position).finish(),
            PlayerCommand::SetSession(_) => f.debug_tuple("SetSession").finish(),
            PlayerCommand::AddEventSender(_) => f.debug_tuple("AddEventSender").finish(),
            PlayerCommand::SetSinkEventCallback(_) => {
                f.debug_tuple("SetSinkEventCallback").finish()
            }
            PlayerCommand::EmitVolumeChangedEvent(volume) => f
                .debug_tuple("EmitVolumeChangedEvent")
                .field(&volume)
                .finish(),
            PlayerCommand::SetAutoNormaliseAsAlbum(setting) => f
                .debug_tuple("SetAutoNormaliseAsAlbum")
                .field(&setting)
                .finish(),
            PlayerCommand::SetAudioTuning(tuning) => {
                f.debug_tuple("SetAudioTuning").field(&tuning).finish()
            }
            PlayerCommand::SetCrossfade { ms, albums } => f
                .debug_tuple("SetCrossfade")
                .field(&ms)
                .field(&albums)
                .finish(),
            PlayerCommand::EmitFilterExplicitContentChangedEvent(filter) => f
                .debug_tuple("EmitFilterExplicitContentChangedEvent")
                .field(&filter)
                .finish(),
            PlayerCommand::EmitSessionConnectedEvent {
                connection_id,
                user_name,
            } => f
                .debug_tuple("EmitSessionConnectedEvent")
                .field(&connection_id)
                .field(&user_name)
                .finish(),
            PlayerCommand::EmitSessionDisconnectedEvent {
                connection_id,
                user_name,
            } => f
                .debug_tuple("EmitSessionDisconnectedEvent")
                .field(&connection_id)
                .field(&user_name)
                .finish(),
            PlayerCommand::EmitSessionClientChangedEvent {
                client_id,
                client_name,
                client_brand_name,
                client_model_name,
            } => f
                .debug_tuple("EmitSessionClientChangedEvent")
                .field(&client_id)
                .field(&client_name)
                .field(&client_brand_name)
                .field(&client_model_name)
                .finish(),
            PlayerCommand::EmitShuffleChangedEvent(shuffle) => f
                .debug_tuple("EmitShuffleChangedEvent")
                .field(&shuffle)
                .finish(),
            PlayerCommand::EmitJamQueueEvent { next, .. } => f
                .debug_tuple("EmitJamQueueEvent")
                .field(&next.len())
                .finish(),
            PlayerCommand::EmitClusterSnapshotEvent(c) => f
                .debug_tuple("EmitClusterSnapshotEvent")
                .field(&c.len())
                .finish(),
            PlayerCommand::EmitRepeatChangedEvent { context, track } => f
                .debug_tuple("EmitRepeatChangedEvent")
                .field(&context)
                .field(&track)
                .finish(),
            PlayerCommand::EmitAutoPlayChangedEvent(auto_play) => f
                .debug_tuple("EmitAutoPlayChangedEvent")
                .field(&auto_play)
                .finish(),
            PlayerCommand::EmitSkipCascadeEvent(failed) => f
                .debug_tuple("EmitSkipCascadeEvent")
                .field(&failed)
                .finish(),
        }
    }
}

impl fmt::Debug for PlayerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use PlayerState::*;
        match self {
            Stopped => f.debug_struct("Stopped").finish(),
            Loading {
                track_id,
                play_request_id,
                ..
            } => f
                .debug_struct("Loading")
                .field("track_id", &track_id)
                .field("play_request_id", &play_request_id)
                .finish(),
            Paused {
                track_id,
                play_request_id,
                ..
            } => f
                .debug_struct("Paused")
                .field("track_id", &track_id)
                .field("play_request_id", &play_request_id)
                .finish(),
            Playing {
                track_id,
                play_request_id,
                ..
            } => f
                .debug_struct("Playing")
                .field("track_id", &track_id)
                .field("play_request_id", &play_request_id)
                .finish(),
            EndOfTrack {
                track_id,
                play_request_id,
                ..
            } => f
                .debug_struct("EndOfTrack")
                .field("track_id", &track_id)
                .field("play_request_id", &play_request_id)
                .finish(),
            Invalid => f.debug_struct("Invalid").finish(),
        }
    }
}

struct Subfile<T: Read + Seek> {
    stream: T,
    offset: u64,
    length: u64,
}

impl<T: Read + Seek> Subfile<T> {
    pub fn new(mut stream: T, offset: u64, length: u64) -> Result<Subfile<T>, io::Error> {
        let target = SeekFrom::Start(offset);
        stream.seek(target)?;

        Ok(Subfile {
            stream,
            offset,
            length,
        })
    }
}

impl<T: Read + Seek> Read for Subfile<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.read(buf)
    }
}

impl<T: Read + Seek> Seek for Subfile<T> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let pos = match pos {
            SeekFrom::Start(offset) => SeekFrom::Start(offset + self.offset),
            SeekFrom::End(offset) => {
                if (self.length as i64 - offset) < self.offset as i64 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "newpos would be < self.offset",
                    ));
                }
                pos
            }
            _ => pos,
        };

        let newpos = self.stream.seek(pos)?;
        Ok(newpos - self.offset)
    }
}

impl<R> MediaSource for Subfile<R>
where
    R: Read + Seek + Send + Sync,
{
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.length)
    }
}

/// Desde aquí hasta el final es «la cola»: el demultiplexor Ogg lee los últimos 65 307 bytes del
/// fichero para saber cuánto dura, y con el fichero en camino eso es una petición más a la CDN.
const TAIL_PROBE_BYTES: u64 = 70_000;
/// Una lectura más lenta que esto esperó a la red (o al disco): se anota en el registro.
const SLOW_READ: Duration = Duration::from_millis(5);

/// El fichero de audio tal como lo lee el decodificador, con dos añadidos:
/// - mientras la carga es la de una orden medida (`ttfs`), anota las lecturas que esperan a la red
///   (la cabecera, la cola que sondea el demultiplexor Ogg, cada paso de una búsqueda) y marca la
///   fase «cdn-tail» al llegar la cola. librespot-audio no está parcheado: esta es la única capa
///   propia por la que pasan esas peticiones;
/// - con `NANOFY_FAULT=cdn_stall_ms:N@P%`, simula un corte de la CDN al pasar por el P % (ver
///   `fault::cdn_stall`): la lectura espera, y falla con `TimedOut`, como la de verdad.
///
/// Sin medida pendiente ni fallo simulado, cada lectura solo suma su posición.
struct WatchedFile<T> {
    inner: T,
    /// Posición de lectura (la que devuelve `inner`).
    pos: u64,
    len: u64,
    /// Medida de la carga (0 = ninguna).
    ttfs: u64,
    tail_marked: bool,
    /// El fichero ya suena: solo entonces puede empezar el corte simulado.
    streaming: Arc<AtomicBool>,
    /// Hay un corte simulado pedido (se mira una vez, al abrir).
    stall: bool,
    /// Pasa a `true` en cuanto una lectura falla (la CDN dejó de entregar datos): un decodificador
    /// que no arranca o no llega a la posición pedida es entonces un fallo de red, pasajero, y
    /// no un fichero roto (ver `PlayerTrackLoader::decode_failure`).
    read_failed: Arc<AtomicBool>,
}

impl<T> WatchedFile<T> {
    fn new(
        inner: T,
        len: u64,
        ttfs: u64,
        streaming: Arc<AtomicBool>,
        read_failed: Arc<AtomicBool>,
    ) -> Self {
        Self {
            inner,
            pos: 0,
            len,
            ttfs,
            tail_marked: false,
            streaming,
            stall: fault::cdn_stall_planned(),
            read_failed,
        }
    }
}

impl<T: Read> Read for WatchedFile<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.stall {
            let timeout = AudioFetchParams::get().download_timeout;
            let can_start = self.streaming.load(Ordering::Relaxed);
            match fault::cdn_stall(self.pos, buf.len() as u64, self.len, can_start, timeout) {
                fault::Stall::No => {}
                fault::Stall::Wait(d) => thread::sleep(d),
                fault::Stall::TimedOut(d) => {
                    thread::sleep(d);
                    self.read_failed.store(true, Ordering::Relaxed);
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "NANOFY_FAULT: corte de la CDN simulado",
                    ));
                }
            }
        }
        let t0 = ttfs::is_pending(self.ttfs).then(Instant::now);
        let n = self.inner.read(buf).inspect_err(|_| {
            self.read_failed.store(true, Ordering::Relaxed);
        })?;
        if let Some(t0) = t0 {
            let took = t0.elapsed();
            if took >= SLOW_READ {
                debug!(
                    "[ttfs] #{} lectura de {n} B en el byte {} ({:.0} %): {} ms",
                    self.ttfs,
                    self.pos,
                    self.pos as f64 * 100.0 / self.len.max(1) as f64,
                    took.as_millis()
                );
            }
            if !self.tail_marked && self.pos.saturating_add(TAIL_PROBE_BYTES) >= self.len {
                self.tail_marked = true;
                ttfs::mark_seq(self.ttfs, "cdn-tail", Some(format!("{} ms", took.as_millis())));
            }
        }
        self.pos += n as u64;
        // Avance para el vigilante de «cargando» de Nanofy (`ttfs::activity`): llegan datos.
        ttfs::note_activity();
        Ok(n)
    }
}

impl<T: Seek> Seek for WatchedFile<T> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.pos = self.inner.seek(pos)?;
        Ok(self.pos)
    }
}
