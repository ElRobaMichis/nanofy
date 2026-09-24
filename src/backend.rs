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
    config::{AudioFormat, Bitrate, PlayerConfig},
    config::VolumeCtrl,
    mixer::{self, MixerConfig, NoOpVolume},
    player::{Player, PlayerEvent},
};
use tokio::sync::mpsc;

use crate::bus::{Msg, UiTx};
use crate::config::{vol_pct_to_raw, Paths, Quality, Settings};
use crate::model::NowPlaying;

/// Tiempo en pausa tras el que se suelta el dispositivo de audio (ver `start`).
const RELEASE_AUDIO_AFTER_PAUSE: Duration = Duration::from_secs(5);

const REDIRECT_URI: &str = "http://127.0.0.1:8898/login";

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
    Login,
    /// La interfaz lleva demasiado tiempo en «cargando»: comprobar la sesión y reconectar si hace falta.
    Stalled,
    Logout,
    /// Reinicia el reproductor con nuevos ajustes de audio.
    Restart(Settings),
    PlayPause,
    /// Play y pausa explícitos: la interfaz conoce el estado real y evita que el conmutador de
    /// Spirc haga lo contrario cuando su estado interno se ha quedado desfasado.
    Play,
    Pause,
    Next,
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
    LoadContext {
        uri: String,
        track_uri: Option<String>,
        index: Option<u32>,
        shuffle: bool,
        /// `Some(ms)`: cargar en pausa en esa posición (restaurar la sesión anterior).
        resume: Option<u32>,
    },
    LoadTracks {
        uris: Vec<String>,
        index: Option<u32>,
        shuffle: bool,
        resume: Option<u32>,
    },
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum Event {
    Status(String),
    Error(String),
    LoggedIn { username: String, device_id: String },
    LoggedOut,
    TrackChanged(NowPlaying),
    Playing { position_ms: u32 },
    Paused { position_ms: u32 },
    Position(u32),
    Stopped,
    Loading,
    Unavailable,
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
        rt.spawn(run(rx, ui, shared.clone(), paths, settings));
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
    ui: UiTx,
    shared: Arc<Shared>,
    paths: Paths,
    mut settings: Settings,
) {
    let mut active: Option<Active> = None;
    // Último volumen pedido por la interfaz: Spirc ignora SetVolume mientras no es el
    // dispositivo activo, así que se reaplica justo después de activar.
    let mut last_volume: Option<u16> = None;
    // Vigilancia de la conexión: la tarea de Spirc avisa por aquí cuando termina (la sesión
    // caducó, se perdió la red, el equipo durmió…). Si no la paramos nosotros, se reconecta.
    let (dead_tx, mut dead_rx) = mpsc::unbounded_channel::<u64>();
    let mut generation: u64 = 0;
    let mut retry_at: Option<tokio::time::Instant> = None;
    let mut retry_delay = Duration::from_secs(2);

    // Inicio de sesión automático con las credenciales guardadas.
    if let Some(creds) = cached_credentials(&paths) {
        generation += 1;
        match start(&paths, &settings, creds, &ui, &shared, generation, dead_tx.clone()).await {
            Ok(a) => active = Some(a),
            Err(e) => {
                forget_cached_tokens(&paths);
                ui.error(format!("No se pudo conectar con Spotify: {e}"))
            }
        }
    }

    loop {
        enum Next {
            Cmd(Cmd),
            Dead(u64),
            Retry,
            Health,
        }
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
            _ = tokio::time::sleep(Duration::from_secs(30)) => Next::Health,
        };
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
                match start(&paths, &settings, creds, &ui, &shared, generation, dead_tx.clone()).await {
                    Ok(a) => {
                        retry_delay = Duration::from_secs(2);
                        ui.send(Msg::Backend(Event::Reconnected));
                        if let Some(v) = last_volume {
                            let _ = a.spirc.set_volume(v);
                        }
                        active = Some(a);
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
            Cmd::Login => {
                if active.is_some() {
                    continue;
                }
                retry_at = None;
                let creds = match cached_credentials(&paths) {
                    Some(c) => Ok(c),
                    None => {
                        ui.status("Se ha abierto el navegador para iniciar sesión en Spotify…");
                        tokio::task::spawn_blocking(oauth_login)
                            .await
                            .unwrap_or_else(|e| Err(e.to_string()))
                    }
                };
                match creds {
                    Ok(c) => match {
                        generation += 1;
                        start(&paths, &settings, c, &ui, &shared, generation, dead_tx.clone()).await
                    } {
                        Ok(a) => active = Some(a),
                        Err(e) => {
                            forget_cached_tokens(&paths);
                            ui.error(format!("No se pudo conectar con Spotify: {e}"))
                        }
                    },
                    Err(e) => ui.error(format!("Inicio de sesión cancelado: {e}")),
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
                ui.send(Msg::Backend(Event::LoggedOut));
            }
            Cmd::Restart(new_settings) => {
                settings = new_settings;
                if let Some(a) = active.take() {
                    a.stop(true).await;
                }
                if let Some(creds) = cached_credentials(&paths) {
                    generation += 1;
                    match start(&paths, &settings, creds, &ui, &shared, generation, dead_tx.clone()).await {
                        Ok(a) => {
                            ui.status("Ajustes de reproducción aplicados");
                            active = Some(a);
                        }
                        Err(e) => ui.error(format!("No se pudo reiniciar la reproducción: {e}")),
                    }
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
                let result = match other {
                    Cmd::PlayPause | Cmd::Play | Cmd::Pause | Cmd::Next | Cmd::Prev => {
                        // Tras mucho tiempo en pausa Spotify deja de tenernos como dispositivo
                        // activo y Spirc ignora estas órdenes: se reactiva antes (si ya lo
                        // estaba, la activación se ignora sin efecto).
                        let _ = a.spirc.activate();
                        match other {
                            Cmd::PlayPause => a.spirc.play_pause(),
                            Cmd::Play => a.spirc.play(),
                            Cmd::Pause => a.spirc.pause(),
                            Cmd::Next => a.spirc.next(),
                            _ => a.spirc.prev(),
                        }
                    }
                    Cmd::Seek(ms) => a.spirc.set_position_ms(ms),
                    Cmd::Volume(v) => {
                        last_volume = Some(v);
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
                    } => {
                        let _ = a.spirc.activate();
                        if let Some(v) = last_volume {
                            let _ = a.spirc.set_volume(v);
                        }
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
                            },
                        ))
                    }
                    Cmd::LoadTracks {
                        uris,
                        index,
                        shuffle,
                        resume,
                    } => {
                        let _ = a.spirc.activate();
                        if let Some(v) = last_volume {
                            let _ = a.spirc.set_volume(v);
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

fn cached_credentials(paths: &Paths) -> Option<Credentials> {
    if crate::config::no_session() {
        return None;
    }
    let cache = Cache::new(Some(paths.credentials_dir()), None, None, None).ok()?;
    cache.credentials()
}

fn oauth_login() -> Result<Credentials, String> {
    let client_id = SessionConfig::default().client_id;
    let client = librespot_oauth::OAuthClientBuilder::new(&client_id, REDIRECT_URI, SCOPES.to_vec())
        .open_in_browser()
        .with_custom_message("Listo. Ya puedes cerrar esta pestaña y volver a Nanofy.")
        .build()
        .map_err(|e| e.to_string())?;
    let token = client.get_access_token().map_err(|e| e.to_string())?;
    Ok(Credentials::with_access_token(token.access_token))
}

async fn start(
    paths: &Paths,
    settings: &Settings,
    creds: Credentials,
    ui: &UiTx,
    shared: &Arc<Shared>,
    generation: u64,
    dead_tx: mpsc::UnboundedSender<u64>,
) -> Result<Active, String> {
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

    let player_config = PlayerConfig {
        bitrate: match settings.quality {
            Quality::Low => Bitrate::Bitrate96,
            Quality::Normal => Bitrate::Bitrate160,
            Quality::High => Bitrate::Bitrate320,
            Quality::Lossless => Bitrate::Lossless,
        },
        gapless: settings.gapless,
        normalisation: settings.normalisation,
        ..PlayerConfig::default()
    };

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
            }) as Box<dyn Sink>
        },
    );

    let mut events = player.get_player_event_channel();
    let ui_events = ui.clone();
    let session_events = session.clone();
    // Débil: el reproductor no debe seguir vivo solo porque esta tarea lo recuerde.
    let player_weak = Arc::downgrade(&player);
    tokio::spawn(async move {
        while let Some(ev) = events.recv().await {
            if matches!(ev, PlayerEvent::Paused { .. } | PlayerEvent::Stopped { .. }) {
                // Tras un rato en pausa se suelta la salida de audio: con el flujo abierto,
                // Windows da el dispositivo por ocupado aunque solo suene silencio, y unos
                // auriculares Bluetooth multipunto no cambian al teléfono. Si en ese tiempo se
                // vuelve a reproducir, el reproductor ignora la orden.
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
            if let PlayerEvent::Unavailable { track_id, .. } = &ev {
                // Un fichero de caché a medias (cierre forzado, disco lleno) hace que la canción
                // falle al decodificar para siempre: se borra para que la próxima vez se descargue.
                tokio::spawn(purge_cached_audio(session_events.clone(), track_id.clone()));
            }
            if let Some(e) = map_event(ev) {
                ui_events.send(Msg::Backend(e));
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
        Ok(r) => r.map_err(|e| e.to_string())?,
        Err(_) => {
            session.shutdown();
            return Err("Spotify no respondió al conectar (25 s)".to_string());
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
    }));
    audio_backend::set_output_volume(initial_volume as f32 / u16::MAX as f32);
    ui.send(Msg::Backend(Event::Volume(initial_volume)));

    Ok(Active {
        spirc,
        session,
        player,
        generation,
        fast_stop: false,
    })
}

/// Sink que crea el sink real (y abre el dispositivo) en el primer `start()`.
struct LazySink {
    open: Option<Box<dyn FnOnce() -> Box<dyn Sink> + Send>>,
    inner: Option<Box<dyn Sink>>,
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
    fn release(&mut self) {
        if let Some(s) = self.inner.as_mut() {
            s.release();
        }
    }
    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
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
    info
}

/// Borra de la caché de audio los ficheros de una canción que no se pudo reproducir.
async fn purge_cached_audio(session: Session, uri: librespot_core::SpotifyUri) {
    use librespot_metadata::{Metadata, Track};
    let Some(cache) = session.cache().cloned() else { return };
    let Ok(track) = Track::get(&session, &uri).await else { return };
    let mut n = 0;
    for file in track.files.values() {
        if cache.file_path(*file).map(|p| p.exists()).unwrap_or(false) && cache.remove_file(*file).is_ok() {
            n += 1;
        }
    }
    if n > 0 {
        log::warn!("caché de audio: {n} fichero(s) de {uri} borrados por no poderse reproducir");
    }
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
        Loading { .. } => Event::Loading,
        Unavailable { .. } => Event::Unavailable,
        VolumeChanged { volume } => {
            audio_backend::set_output_volume(volume as f32 / u16::MAX as f32);
            Event::Volume(volume)
        }
        ShuffleChanged { shuffle } => Event::Shuffle(shuffle),
        RepeatChanged { context, track } => Event::Repeat { context, track },
        JamQueue { current, context, next } => Event::JamQueue { current, context, next },
        _ => return None,
    })
}
