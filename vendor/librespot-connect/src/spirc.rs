use crate::{
    LoadContextOptions, LoadRequestOptions, PlayContext,
    cascade::{self, SkipBreaker},
    context_resolver::{ContextAction, ContextResolver, ResolveContext},
    core::{
        Error, Session, SpotifyUri,
        authentication::Credentials,
        dealer::{
            manager::{BoxedStream, BoxedStreamResult, Reply, RequestReply},
            protocol::{Command, FallbackWrapper, Message, Request},
        },
        session::UserAttributes,
        spclient::TransferRequest,
    },
    model::{LoadRequest, PlayingTrack, SpircPlayStatus},
    start_index::{self, Wanted},
    playback::{
        crossfade::{self, FadeOffer},
        mixer::Mixer,
        player::{Player, PlayerEvent, PlayerEventChannel, Transition},
    },
    protocol::{
        connect::{Cluster, ClusterUpdate, LogoutCommand, PutStateReason, SetVolumeCommand},
        context::Context,
        explicit_content_pubsub::UserAttributesUpdate,
        playlist4_external::PlaylistModificationInfo,
        social_connect_v2::{SessionUpdate, SessionUpdateReason},
        transfer_state::TransferState,
        user_attributes::UserAttributesMutation,
    },
    state::{
        context::{ContextType, ResetContext},
        provider::IsProvider,
        {ConnectConfig, ConnectState},
    },
    state_sender::StateSender,
};
use futures_util::StreamExt;
use librespot_protocol::context_page::ContextPage;
use protobuf::MessageField;
use std::{
    future::Future,
    sync::Arc,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot},
    time::sleep,
};

#[derive(Debug, Error)]
enum SpircError {
    #[error("response payload empty")]
    NoData,
    #[error("{0} had no uri")]
    NoUri(&'static str),
    #[error("message pushed for another URI")]
    InvalidUri(String),
    #[error("failed to put connect state for new device")]
    FailedDealerSetup,
    #[error("unknown endpoint: {0:#?}")]
    UnknownEndpoint(serde_json::Value),
}

impl From<SpircError> for Error {
    fn from(err: SpircError) -> Self {
        use SpircError::*;
        match err {
            NoData | NoUri(_) => Error::unavailable(err),
            InvalidUri(_) | FailedDealerSetup => Error::aborted(err),
            UnknownEndpoint(_) => Error::unimplemented(err),
        }
    }
}

struct SpircTask {
    player: Arc<Player>,
    mixer: Arc<dyn Mixer>,

    /// the state management object
    connect_state: ConnectState,
    connect_established: bool,
    /// El dealer ya se lanzó en `Spirc::new`, en paralelo a la conexión con el punto de acceso.
    dealer_started: bool,

    play_request_id: Option<u64>,
    play_status: SpircPlayStatus,

    connection_id_update: BoxedStreamResult<String>,
    connect_state_update: BoxedStreamResult<ClusterUpdate>,
    connect_state_volume_update: BoxedStreamResult<SetVolumeCommand>,
    connect_state_logout_request: BoxedStreamResult<LogoutCommand>,
    playlist_update: BoxedStreamResult<PlaylistModificationInfo>,
    session_update: BoxedStreamResult<FallbackWrapper<SessionUpdate>>,
    connect_state_command: BoxedStream<RequestReply>,
    user_attributes_update: BoxedStreamResult<UserAttributesUpdate>,
    user_attributes_mutation: BoxedStreamResult<UserAttributesMutation>,

    commands: Option<mpsc::UnboundedReceiver<SpircCommand>>,
    player_events: Option<PlayerEventChannel>,

    context_resolver: ContextResolver,

    shutdown: bool,
    session: Session,

    /// is set when transferring, and used after resolving the contexts to finish the transfer
    pub transfer_state: Option<TransferState>,

    /// when set to true, it will update the volume after [VOLUME_UPDATE_DELAY],
    /// when no other future resolves, otherwise resets the delay
    update_volume: bool,

    /// when set to true, it will update the volume after [UPDATE_STATE_DELAY],
    /// when no other future resolves, otherwise resets the delay
    update_state: bool,

    /// `true` cuando somos participante (no anfitrión) de una Jam. En ese modo el anfitrión manda
    /// la reproducción, así que no rellenamos con autoplay ni avanzamos con la cola local: seguimos
    /// solo lo que la sesión (el anfitrión) nos envía.
    jam_participant: bool,

    /// id de la Jam actual (para enviar comandos al dispositivo virtual `social-connect-<id>`).
    jam_session_id: Option<String>,

    /// Última cola de la Jam reenviada a la interfaz (current + siguientes), para no repetir.
    jam_last_queue: Vec<String>,

    /// Fundido entre canciones: `play_request_id` de la última canción cuyo fundido se aceptó. Sus
    /// eventos aún pueden llegar antes que la carga de la siguiente, que ya va de camino, y se
    /// ignoran: su `EndOfTrack` no debe volver a avanzar (se saltarían dos canciones).
    crossfade_from_prid: Option<u64>,
    /// Cómo empieza la próxima canción que cargue `load_track`: un corte, salvo que quien avanza
    /// pida otra cosa (fundido aceptado, salto automático). La carga lo consume.
    next_load_transition: Transition,
    /// La canción con la que se aceptó fundir, mientras se avanza hacia ella: si la que acaba
    /// cargando es otra, entra con un corte.
    crossfade_to: Option<SpotifyUri>,

    /// La canción actual no se pudo cargar por un fallo pasajero (Spotify frenando las claves, la
    /// red): está en pausa pero el reproductor no tiene nada cargado, así que «reproducir» la
    /// vuelve a cargar en su posición en vez de reanudar.
    load_failed: bool,
    /// Detiene la cascada de saltos cuando varias canciones seguidas fallan de verdad.
    skip_breaker: SkipBreaker,
    /// Envía el estado a Spotify en segundo plano (`notify`): ninguna orden espera a un PUT.
    state_sender: StateSender,

    spirc_id: usize,
}

static SPIRC_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
enum SpircCommand {
    Play,
    PlayPause,
    Pause,
    Prev,
    Next,
    /// Como `Next`, pero sin que lo haya pedido el usuario (ver `Spirc::auto_next`).
    AutoNext,
    VolumeUp,
    VolumeDown,
    Shutdown,
    Shuffle(bool),
    Repeat(bool),
    RepeatTrack(bool),
    Disconnect { pause: bool },
    SetPosition(u32),
    SetVolume(u16),
    Activate,
    Transfer(Option<TransferRequest>),
    Load(LoadRequest),
    /// Nanofy: ¿atiende el bucle? Se contesta en cuanto llega su turno, sin tocar nada (ver
    /// `Spirc::ping`).
    Ping(oneshot::Sender<()>),
    /// Nanofy: vuelve a cargar la canción en pausa, en su punto y sonando (ver `Spirc::reload`).
    Reload,
}

impl SpircCommand {
    /// Fase `ttfs` (tiempo hasta el primer sonido) en que Spirc empieza a atender la orden. Va de
    /// una en una y cada una espera su `notify` (un PUT a connect-state): la distancia entre la
    /// orden de la interfaz y esta marca es lo que la orden esperó su turno.
    fn ttfs_phase(&self) -> &'static str {
        match self {
            SpircCommand::Activate => "spirc:activate",
            SpircCommand::SetVolume(_) => "spirc:volume",
            SpircCommand::Load(_) => "spirc:load",
            SpircCommand::Next | SpircCommand::AutoNext => "spirc:next",
            SpircCommand::Prev => "spirc:prev",
            SpircCommand::Play | SpircCommand::PlayPause | SpircCommand::Reload => "spirc:play",
            SpircCommand::Pause => "spirc:pause",
            SpircCommand::SetPosition(_) => "spirc:seek",
            _ => "spirc",
        }
    }
}

const CONTEXT_FETCH_THRESHOLD: usize = 2;

// delay to update volume after a certain amount of time, instead on each update request
const VOLUME_UPDATE_DELAY: Duration = Duration::from_millis(500);
// to reduce updates to remote, we group some request by waiting for a set amount of time
const UPDATE_STATE_DELAY: Duration = Duration::from_millis(200);

/// The spotify connect handle
pub struct Spirc {
    commands: mpsc::UnboundedSender<SpircCommand>,
}

impl Spirc {
    /// Initializes a new spotify connect device
    ///
    /// The returned tuple consists out of a handle to the [`Spirc`] that
    /// can control the local connect device when active. And a [`Future`]
    /// which represents the [`Spirc`] event loop that processes the whole
    /// connect device logic.
    pub async fn new(
        config: ConnectConfig,
        session: Session,
        credentials: Credentials,
        player: Arc<Player>,
        mixer: Arc<dyn Mixer>,
    ) -> Result<(Spirc, impl Future<Output = ()>), Error> {
        fn extract_connection_id(msg: Message) -> Result<String, Error> {
            let connection_id = msg
                .headers
                .get("Spotify-Connection-Id")
                .ok_or_else(|| SpircError::InvalidUri(msg.uri.clone()))?;
            Ok(connection_id.to_owned())
        }

        let spirc_id = SPIRC_COUNTER.fetch_add(1, Ordering::AcqRel);
        debug!("new Spirc[{spirc_id}]");

        let connect_state = ConnectState::new(config, &session);

        let connection_id_update = session
            .dealer()
            .listen_for("hm://pusher/v1/connections/", extract_connection_id)?;

        let connect_state_update = session
            .dealer()
            .listen_for("hm://connect-state/v1/cluster", Message::from_raw)?;

        let connect_state_volume_update = session
            .dealer()
            .listen_for("hm://connect-state/v1/connect/volume", Message::from_raw)?;

        let connect_state_logout_request = session
            .dealer()
            .listen_for("hm://connect-state/v1/connect/logout", Message::from_raw)?;

        let playlist_update = session
            .dealer()
            .listen_for("hm://playlist/v2/playlist/", Message::from_raw)?;

        let session_update = session
            .dealer()
            .listen_for("social-connect/v2/session_update", Message::try_from_json)?;

        let user_attributes_update = session
            .dealer()
            .listen_for("spotify:user:attributes:update", Message::from_raw)?;

        // can be trigger by toggling autoplay in a desktop client
        let user_attributes_mutation = session
            .dealer()
            .listen_for("spotify:user:attributes:mutated", Message::from_raw)?;

        let connect_state_command = session
            .dealer()
            .handle_for("hm://connect-state/v1/player/command")?;

        // El token de cliente y el de acceso (login5) se piden mientras se conecta al punto de
        // acceso, no uno tras otro (~0,4 s menos al abrir). login5 solo necesita el usuario y las
        // credenciales guardadas, que ya se tienen antes de conectar; si así no funciona, se pide
        // como siempre al terminar de conectar.
        let early_login5 = credentials.auth_type
            == librespot_protocol::authentication::AuthenticationType::AUTHENTICATION_STORED_SPOTIFY_CREDENTIALS
            && !credentials.auth_data.is_empty()
            && credentials.username.as_deref().is_some_and(|u| !u.is_empty());
        if early_login5 {
            session.set_username(credentials.username.as_deref().unwrap_or_default());
            session.set_auth_data(&credentials.auth_data);
        }
        let tokens = async {
            // pre-acquire client_token, preventing multiple request while running
            session.spclient().client_token().await?;
            if early_login5 {
                match session.login5().auth_token().await {
                    // Con el token de acceso ya en la mano, el dealer (por donde llega el estado de
                    // la cuenta) tampoco tiene que esperar a la conexión con el punto de acceso: lo
                    // que reciba antes de que arranque la tarea queda en cola en los oyentes.
                    Ok(_) => match session.dealer().start().await {
                        Ok(()) => return Ok::<bool, Error>(true),
                        Err(e) => return Err(e),
                    },
                    Err(e) => debug!("login5 before connecting failed ({e}); retrying after connecting"),
                }
            }
            Ok::<bool, Error>(false)
        };
        // Connect *after* all message listeners are registered
        let (tokens, connected) = tokio::join!(tokens, session.connect(credentials, true));
        let dealer_started = tokens?;
        connected?;

        // pre-acquire access_token (we need to be authenticated to retrieve a token); si ya se
        // obtuvo antes de conectar, sale de la caché al instante.
        let _ = session.login5().auth_token().await?;

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

        let player_events = player.get_player_event_channel();
        let state_sender = StateSender::new(session.clone());

        let mut task = SpircTask {
            player,
            mixer,

            connect_state,
            dealer_started,
            connect_established: false,

            play_request_id: None,
            play_status: SpircPlayStatus::Stopped,

            connection_id_update,
            connect_state_update,
            connect_state_volume_update,
            connect_state_logout_request,
            playlist_update,
            session_update,
            connect_state_command,
            user_attributes_update,
            user_attributes_mutation,
            commands: Some(cmd_rx),
            player_events: Some(player_events),

            context_resolver: ContextResolver::new(session.clone()),

            shutdown: false,
            session,

            transfer_state: None,
            update_volume: false,
            update_state: false,

            jam_participant: false,
            jam_session_id: None,
            jam_last_queue: Vec::new(),

            crossfade_from_prid: None,
            next_load_transition: Transition::Cut,
            crossfade_to: None,

            load_failed: false,
            skip_breaker: SkipBreaker::new(),
            state_sender,

            spirc_id,
        };

        let spirc = Spirc { commands: cmd_tx };

        let initial_volume = task.connect_state.device_info().volume;
        task.connect_state.set_volume(0);

        match initial_volume.try_into() {
            Ok(volume) => {
                task.set_volume(volume);
                // we don't want to update the volume initially,
                // we just want to set the mixer to the correct volume
                task.update_volume = false;
            }
            Err(why) => error!("failed to update initial volume: {why}"),
        };

        Ok((spirc, task.run()))
    }

    /// Safely shutdowns the spirc.
    ///
    /// This pauses the playback, disconnects the connect device and
    /// bring the future initially returned to an end.
    pub fn shutdown(&self) -> Result<(), Error> {
        Ok(self.commands.send(SpircCommand::Shutdown)?)
    }

    /// Resumes the playback
    ///
    /// Does nothing if we are not the active device, or it isn't paused.
    pub fn play(&self) -> Result<(), Error> {
        Ok(self.commands.send(SpircCommand::Play)?)
    }

    /// Resumes or pauses the playback
    ///
    /// Does nothing if we are not the active device.
    pub fn play_pause(&self) -> Result<(), Error> {
        Ok(self.commands.send(SpircCommand::PlayPause)?)
    }

    /// Pauses the playback
    ///
    /// Does nothing if we are not the active device, or if it isn't playing.
    pub fn pause(&self) -> Result<(), Error> {
        Ok(self.commands.send(SpircCommand::Pause)?)
    }

    /// Seeks to the beginning or skips to the previous track.
    ///
    /// Seeks to the beginning when the current track position
    /// is greater than 3 seconds.
    ///
    /// Does nothing if we are not the active device.
    pub fn prev(&self) -> Result<(), Error> {
        Ok(self.commands.send(SpircCommand::Prev)?)
    }

    /// Skips to the next track.
    ///
    /// Does nothing if we are not the active device.
    pub fn next(&self) -> Result<(), Error> {
        Ok(self.commands.send(SpircCommand::Next)?)
    }

    /// Skips to the next track without the user asking for it (e.g. a hidden track that just
    /// started).
    ///
    /// Igual que [Spirc::next], salvo durante un fundido entre canciones: la canción anterior
    /// sigue apagándose a su ritmo y la nueva entra con lo que le quede de rampa. Con un salto
    /// normal los segundos que le faltaban a la anterior se perderían en 40 ms.
    ///
    /// Does nothing if we are not the active device.
    pub fn auto_next(&self) -> Result<(), Error> {
        Ok(self.commands.send(SpircCommand::AutoNext)?)
    }

    /// Increases the volume by configured steps of [ConnectConfig].
    ///
    /// Does nothing if we are not the active device.
    pub fn volume_up(&self) -> Result<(), Error> {
        Ok(self.commands.send(SpircCommand::VolumeUp)?)
    }

    /// Decreases the volume by configured steps of [ConnectConfig].
    ///
    /// Does nothing if we are not the active device.
    pub fn volume_down(&self) -> Result<(), Error> {
        Ok(self.commands.send(SpircCommand::VolumeDown)?)
    }

    /// Shuffles the playback according to the value.
    ///
    /// If true shuffles/reshuffles the playback. Otherwise, does
    /// nothing (if not shuffled) or unshuffles the playback while
    /// resuming at the position of the current track.
    ///
    /// Does nothing if we are not the active device.
    pub fn shuffle(&self, shuffle: bool) -> Result<(), Error> {
        Ok(self.commands.send(SpircCommand::Shuffle(shuffle))?)
    }

    /// Repeats the playback context according to the value.
    ///
    /// Does nothing if we are not the active device.
    pub fn repeat(&self, repeat: bool) -> Result<(), Error> {
        Ok(self.commands.send(SpircCommand::Repeat(repeat))?)
    }

    /// Repeats the current track if true.
    ///
    /// Does nothing if we are not the active device.
    ///
    /// Skipping to the next track disables the repeating.
    pub fn repeat_track(&self, repeat: bool) -> Result<(), Error> {
        Ok(self.commands.send(SpircCommand::RepeatTrack(repeat))?)
    }

    /// Update the volume to the given value.
    ///
    /// Does nothing if we are not the active device.
    pub fn set_volume(&self, volume: u16) -> Result<(), Error> {
        Ok(self.commands.send(SpircCommand::SetVolume(volume))?)
    }

    /// Updates the position to the given value.
    ///
    /// Does nothing if we are not the active device.
    ///
    /// If value is greater than the track duration,
    /// the update is ignored.
    pub fn set_position_ms(&self, position_ms: u32) -> Result<(), Error> {
        Ok(self.commands.send(SpircCommand::SetPosition(position_ms))?)
    }

    /// Load a new context and replace the current.
    ///
    /// Does nothing if we are not the active device.
    ///
    /// Does not overwrite the queue.
    pub fn load(&self, command: LoadRequest) -> Result<(), Error> {
        Ok(self.commands.send(SpircCommand::Load(command))?)
    }

    /// Disconnects the current device and pauses the playback according the value.
    ///
    /// Does nothing if we are not the active device.
    pub fn disconnect(&self, pause: bool) -> Result<(), Error> {
        Ok(self.commands.send(SpircCommand::Disconnect { pause })?)
    }

    /// Nanofy: comprueba que el bucle de Spirc sigue atendiendo órdenes. La respuesta llega
    /// cuando le toca el turno a esta, así que si tarda es que una orden anterior está atascada
    /// (una petición colgada tras suspender el equipo, por ejemplo) y lo que haya detrás no se
    /// atenderá: entonces conviene reconectar en vez de repetir la carga.
    pub fn ping(&self) -> Result<oneshot::Receiver<()>, Error> {
        let (tx, rx) = oneshot::channel();
        self.commands.send(SpircCommand::Ping(tx))?;
        Ok(rx)
    }

    /// Nanofy: vuelve a cargar desde cero la canción en pausa, en el punto en que está, y la pone
    /// a sonar. Es lo que hace «reproducir» tras una carga fallida, para cuando el reproductor sí
    /// tiene la canción pero su fichero ya no sirve: una canción cortada por la red cuyo enlace al
    /// audio pudo caducar. No toca el contexto, la cola ni el aleatorio. Sin canción en pausa, nada.
    pub fn reload(&self) -> Result<(), Error> {
        Ok(self.commands.send(SpircCommand::Reload)?)
    }

    /// Acquires the control as active connect device.
    ///
    /// Does not [Spirc::transfer] the playback. Does nothing if we are not the active device.
    pub fn activate(&self) -> Result<(), Error> {
        Ok(self.commands.send(SpircCommand::Activate)?)
    }

    /// Acquires the control as active connect device over the transfer flow.
    ///
    /// Does nothing if we are not the active device.
    pub fn transfer(&self, transfer_request: Option<TransferRequest>) -> Result<(), Error> {
        Ok(self
            .commands
            .send(SpircCommand::Transfer(transfer_request))?)
    }
}

impl SpircTask {
    async fn run(mut self) {
        // simplify unwrapping of received item or parsed result
        macro_rules! unwrap {
            ( $next:expr, |$some:ident| $use_some:expr ) => {
                match $next {
                    Some($some) => $use_some,
                    None => {
                        error!("{} selected, but none received", stringify!($next));
                        break;
                    }
                }
            };
            ( $next:expr, match |$ok:ident| $use_ok:expr ) => {
                unwrap!($next, |$ok| match $ok {
                    Ok($ok) => $use_ok,
                    Err(why) => error!("could not parse {}: {}", stringify!($ok), why),
                })
            };
        }

        if !self.dealer_started {
            if let Err(why) = self.session.dealer().start().await {
                error!("starting dealer failed: {why}");
                return;
            }
        }

        while !self.session.is_invalid() && !self.shutdown {
            let commands = self.commands.as_mut();
            let player_events = self.player_events.as_mut();

            // when state and volume update have a higher priority than context resolving
            // because of that the context resolving has to wait, so that the other tasks can finish
            let allow_context_resolving = !self.update_state && !self.update_volume;

            tokio::select! {
                // startup of the dealer requires a connection_id, which is retrieved at the very beginning
                connection_id_update = self.connection_id_update.next() => unwrap! {
                    connection_id_update,
                    match |connection_id| if let Err(why) = self.handle_connection_id_update(connection_id).await {
                        error!("failed handling connection id update: {why}");
                        break;
                    }
                },
                // main dealer update of any remote device updates
                cluster_update = self.connect_state_update.next() => unwrap! {
                    cluster_update,
                    match |cluster_update| if let Err(e) = self.handle_cluster_update(cluster_update).await {
                        error!("could not dispatch connect state update: {e}");
                    }
                },
                // main dealer request handling (dealer expects an answer)
                request = self.connect_state_command.next() => unwrap! {
                    request,
                    |request| if let Err(e) = self.handle_connect_state_request(request).await {
                        error!("couldn't handle connect state command: {e}");
                    }
                },
                // volume request handling is send separately (it's more like a fire forget)
                volume_update = self.connect_state_volume_update.next() => unwrap! {
                    volume_update,
                    match |volume_update| match volume_update.volume.try_into() {
                        Ok(volume) => self.set_volume(volume),
                        Err(why) => error!("can't update volume, failed to parse i32 to u16: {why}")
                    }
                },
                logout_request = self.connect_state_logout_request.next() => unwrap! {
                    logout_request,
                    |logout_request| {
                        error!("received logout request, currently not supported: {logout_request:#?}");
                        // todo: call logout handling
                    }
                },
                playlist_update = self.playlist_update.next() => unwrap! {
                    playlist_update,
                    match |playlist_update| if let Err(why) = self.handle_playlist_modification(playlist_update) {
                        error!("failed to handle playlist modification: {why}")
                    }
                },
                user_attributes_update = self.user_attributes_update.next() => unwrap! {
                    user_attributes_update,
                    match |attributes| self.handle_user_attributes_update(attributes)
                },
                user_attributes_mutation = self.user_attributes_mutation.next() => unwrap! {
                    user_attributes_mutation,
                    match |attributes| self.handle_user_attributes_mutation(attributes)
                },
                session_update = self.session_update.next() => unwrap! {
                    session_update,
                    match |session_update| self.handle_session_update(session_update)
                },
                cmd = async { commands?.recv().await }, if commands.is_some() && self.connect_established => if let Some(cmd) = cmd {
                    if let Err(e) = self.handle_command(cmd).await {
                        debug!("could not dispatch command: {e}");
                    }
                },
                event = async { player_events?.recv().await }, if player_events.is_some() => if let Some(event) = event {
                    if let Err(e) = self.handle_player_event(event) {
                        error!("could not dispatch player event: {e}");
                    }
                },
                _ = async { sleep(UPDATE_STATE_DELAY).await }, if self.update_state => {
                    self.update_state = false;

                    if let Err(why) = self.notify() {
                        error!("state update: {why}")
                    }
                },
                _ = async { sleep(VOLUME_UPDATE_DELAY).await }, if self.update_volume => {
                    self.update_volume = false;

                    info!("delayed volume update for all devices: volume is now {}", self.connect_state.device_info().volume);
                    // Los dos avisos salen en este orden y ninguno sustituye al otro (el del
                    // volumen lleva su propio motivo), igual que cuando se esperaba a cada uno.
                    self.state_sender.send(
                        self.connect_state
                            .state_request_with_reason(PutStateReason::VOLUME_CHANGED),
                    );

                    // for some reason the web-player does need two separate updates, so that the
                    // position of the current track is retained, other clients also send a state
                    // update before they send the volume update
                    if let Err(why) = self.notify() {
                        error!("error updating connect state for volume update: {why}")
                    }
                },
                // context resolver handling, the idea/reason behind it the following:
                //
                // when we request a context that has multiple pages (for example an artist)
                // resolving all pages at once can take around ~1-30sec, when we resolve
                // everything at once that would block our main loop for that time
                //
                // to circumvent this behavior, we request each context separately here and
                // finish after we received our last item of a type
                next_context = async {
                    self.context_resolver.get_next_context(|| {
                        // Sending local file URIs to this endpoint results in a Bad Request status.
                        // It's likely appropriate to filter them out anyway; Spotify's backend
                        // has no knowledge about these tracks and so can't do anything with them.
                        self.connect_state.recent_track_uris()
                            .into_iter()
                            .filter(|t| !t.starts_with("spotify:local"))
                            .collect::<Vec<_>>()
                    }).await
                }, if allow_context_resolving && self.context_resolver.has_next() => {
                    let update_state = self.handle_next_context(next_context);
                    if update_state {
                        if let Err(why) = self.notify() {
                            error!("update after context resolving failed: {why}")
                        }
                    }
                },
                else => break
            }
        }

        if !self.shutdown && self.connect_state.is_active() {
            warn!("unexpected shutdown");
            if let Err(why) = self.handle_disconnect().await {
                error!("error during disconnecting: {why}")
            }
        }

        // Lo que aún esperaba en la cola tiene que llegar antes del borrado, no después.
        self.flush_state().await;
        // this should clear the active session id, leaving an empty state
        if let Err(why) = self.session.spclient().delete_connect_state_request().await {
            error!("error during connect state deletion: {why}")
        };

        self.session.dealer().close().await;
    }

    fn handle_next_context(&mut self, next_context: Result<Context, Error>) -> bool {
        let next_context = match next_context {
            Err(why) => {
                self.context_resolver.mark_next_unavailable();
                self.context_resolver.remove_used_and_invalid();
                error!("{why}");
                return false;
            }
            Ok(ctx) => ctx,
        };

        debug!("handling next context {:?}", next_context.uri);

        match self
            .context_resolver
            .apply_next_context(&mut self.connect_state, next_context)
        {
            Ok(remaining) => {
                if let Some(remaining) = remaining {
                    self.context_resolver.add_list(remaining)
                }
            }
            Err(why) => {
                error!("{why}")
            }
        }

        let update_state = if self
            .context_resolver
            .try_finish(&mut self.connect_state, &mut self.transfer_state)
        {
            self.add_autoplay_resolving_when_required();
            true
        } else {
            false
        };

        self.context_resolver.remove_used_and_invalid();
        update_state
    }

    // todo: is the time_delta still necessary?
    fn now_ms(&self) -> i64 {
        let dur = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|err| err.duration());

        dur.as_millis() as i64 + 1000 * self.session.time_delta()
    }

    async fn handle_command(&mut self, cmd: SpircCommand) -> Result<(), Error> {
        trace!("Received SpircCommand::{cmd:?}");
        // Antes que nada y sin marca de `ttfs`: el ping no es parte de ninguna orden, y su marca
        // haría pasar por avance de la carga lo que solo es el vigilante preguntando.
        let cmd = match cmd {
            SpircCommand::Ping(tx) => {
                let _ = tx.send(());
                return Ok(());
            }
            cmd => cmd,
        };
        crate::core::ttfs::mark(cmd.ttfs_phase(), None);
        match cmd {
            // Ya contestado arriba; aquí solo para que la lista esté completa.
            SpircCommand::Ping(tx) => {
                let _ = tx.send(());
                return Ok(());
            }
            SpircCommand::Shutdown => {
                trace!("Received SpircCommand::Shutdown");
                self.handle_pause();
                self.handle_disconnect().await?;
                self.shutdown = true;
                if let Some(rx) = self.commands.as_mut() {
                    rx.close()
                }
            }
            SpircCommand::Transfer(request) if !self.connect_state.is_active() => {
                let device_id = self.session.device_id();
                self.session
                    .spclient()
                    .transfer(device_id, device_id, request.as_ref())
                    .await?;
                return Ok(());
            }
            SpircCommand::Activate if !self.connect_state.is_active() => {
                trace!("Received SpircCommand::{cmd:?}");
                self.handle_activate();
                return self.notify();
            }
            // Una orden ignorada no cambia nada que contar a Spotify: sin `notify`. Antes caía al
            // `notify` del final, un PUT de connect-state (~120 ms) que, como la app activa antes
            // de cada reproducir/siguiente/pausa, retrasaba en serie la orden de verdad.
            SpircCommand::Transfer(..) | SpircCommand::Activate => {
                debug!("SpircCommand::{cmd:?} will be ignored while already active");
                return Ok(());
            }
            _ if !self.connect_state.is_active() => {
                warn!("SpircCommand::{cmd:?} will be ignored while Not Active");
                return Ok(());
            }
            SpircCommand::Disconnect { pause } => {
                if pause {
                    self.handle_pause()
                }
                return self.handle_disconnect().await;
            }
            SpircCommand::Play => self.handle_play(),
            SpircCommand::Reload => self.handle_reload()?,
            SpircCommand::PlayPause => self.handle_play_pause(),
            SpircCommand::Pause => self.handle_pause(),
            SpircCommand::Prev => {
                if self.jam_participant {
                    self.jam_send(r#"{"command":{"endpoint":"skip_prev"}}"#.to_string())
                        .await;
                }
                self.handle_prev()?
            }
            SpircCommand::Next => {
                if self.jam_participant {
                    self.jam_send(r#"{"command":{"endpoint":"skip_next"}}"#.to_string())
                        .await;
                }
                self.handle_next(None)?
            }
            SpircCommand::AutoNext => {
                if self.jam_participant {
                    self.jam_send(r#"{"command":{"endpoint":"skip_next"}}"#.to_string())
                        .await;
                }
                self.handle_next_with(Transition::AutoSkip)?
            }
            SpircCommand::VolumeUp => self.handle_volume_up(),
            SpircCommand::VolumeDown => self.handle_volume_down(),
            SpircCommand::Shuffle(shuffle) => self.handle_shuffle(shuffle)?,
            SpircCommand::Repeat(repeat) => self.handle_repeat_context(repeat)?,
            SpircCommand::RepeatTrack(repeat) => self.handle_repeat_track(repeat),
            SpircCommand::SetPosition(position) => self.handle_seek(position),
            SpircCommand::SetVolume(volume) => {
                // `set_volume` ya programa, si el valor cambió, el aviso retrasado del volumen
                // (VOLUME_UPDATE_DELAY), que agrupa los de un arrastre y envía el estado. Avisar
                // también aquí era un PUT más por cada cambio, o por nada si el valor era el mismo.
                self.set_volume(volume);
                return Ok(());
            }
            SpircCommand::Load(command) => {
                if self.jam_participant {
                    if let Some(json) = jam_play_json(&command) {
                        self.jam_send(json).await;
                    }
                }
                self.handle_load(command, None, None).await?
            }
        };

        // El aviso a Spotify sale aparte (`StateSender`): la orden siguiente (una pausa tras un
        // «siguiente») ya no espera a este PUT, ni a uno colgado.
        self.notify()
    }

    /// Envía un comando de reproducción a la Jam actual (si somos participante) para que se aplique
    /// a la cola compartida y lo escuchen todos. Solo registra el resultado; la reproducción local
    /// sigue su curso y la sesión nos devolverá el estado resultante.
    /// Reenvía a la interfaz la cola compartida de la Jam (pista actual, contexto y siguientes)
    /// cuando cambia, para que la app la muestre en lugar de la cola local de la cuenta.
    fn emit_jam_queue_if_changed(&mut self) {
        if !self.jam_participant {
            return;
        }
        let current = self.connect_state.current_track(|t| t.uri.clone());
        let context = self.connect_state.context_uri().clone();
        let next: Vec<String> = self
            .connect_state
            .next_tracks()
            .iter()
            .map(|t| t.uri.clone())
            .take(50)
            .collect();
        let sig: Vec<String> = std::iter::once(current.clone()).chain(next.iter().cloned()).collect();
        if self.jam_last_queue == sig {
            return;
        }
        self.jam_last_queue = sig;
        self.player.emit_jam_queue_event(current, context, next);
    }

    async fn jam_send(&mut self, command_json: String) {
        let Some(session_id) = self.jam_session_id.clone() else {
            return;
        };
        let from = self.session.device_id().to_string();
        match self
            .session
            .spclient()
            .jam_command(&from, &session_id, &command_json)
            .await
        {
            Ok(resp) => info!(
                "[jam-out] enviado ({} bytes): {}",
                resp.len(),
                command_json.chars().take(90).collect::<String>()
            ),
            Err(why) => warn!(
                "[jam-out] error: {why} :: {}",
                command_json.chars().take(90).collect::<String>()
            ),
        }
    }

    fn handle_player_event(&mut self, event: PlayerEvent) -> Result<(), Error> {
        if let PlayerEvent::TrackChanged { audio_item } = event {
            self.connect_state.update_duration(audio_item.duration_ms);
            self.update_state = true;
            return Ok(());
        }

        // update play_request_id
        if let PlayerEvent::PlayRequestIdChanged { play_request_id } = event {
            self.play_request_id = Some(play_request_id);
            return Ok(());
        }

        let is_current_track = matches! {
            (event.get_play_request_id(), self.play_request_id),
            (Some(event_id), Some(current_id)) if event_id == current_id
        };

        // we only process events if the play_request_id matches. If it doesn't, it is
        // an event that belongs to a previous track and only arrives now due to a race
        // condition. In this case we have updated the state already and don't want to
        // mess with it.
        if !is_current_track {
            return Ok(());
        }

        // Fundido aceptado: Spirc ya avanzó a la siguiente, cuya carga va de camino, pero hasta que
        // llegue su `PlayRequestIdChanged` los eventos de la anterior (que sigue sonando mientras
        // el reproductor no atiende la carga) aún pasan el filtro de arriba. Son viejos: un
        // `EndOfTrack` (acabó antes de atender la carga, la siguiente entrará sin hueco) avanzaría
        // otra vez y se saltarían dos; un `PositionCorrection` daría por sonando la siguiente con
        // la posición de la anterior.
        if self.crossfade_from_prid.is_some()
            && event.get_play_request_id() == self.crossfade_from_prid
        {
            if matches!(event, PlayerEvent::EndOfTrack { .. }) {
                debug!("[fundido] acabó con el fundido ya aceptado: la siguiente ya va de camino");
            }
            return Ok(());
        }

        if let PlayerEvent::Playing { .. } = event {
            // Algo suena: lo de antes ya no es una cascada de fallos ni una carga fallida.
            self.skip_breaker.reset();
            self.load_failed = false;
        }

        match event {
            PlayerEvent::EndOfTrack { .. } => {
                let next_track = self
                    .connect_state
                    .repeat_track()
                    .then(|| self.connect_state.current_track(|t| t.uri.clone()));

                self.handle_next(next_track)?
            }
            PlayerEvent::Loading { .. } => match self.play_status {
                SpircPlayStatus::LoadingPlay { position_ms } => {
                    self.connect_state
                        .update_position(position_ms, self.now_ms());
                    trace!("==> LoadingPlay");
                }
                SpircPlayStatus::LoadingPause { position_ms } => {
                    self.connect_state
                        .update_position(position_ms, self.now_ms());
                    trace!("==> LoadingPause");
                }
                _ => {
                    self.connect_state.update_position(0, self.now_ms());
                    trace!("==> Loading");
                }
            },
            PlayerEvent::Seeked { position_ms, .. } => {
                trace!("==> Seeked");
                self.connect_state
                    .update_position(position_ms, self.now_ms())
            }
            PlayerEvent::Playing { position_ms, .. }
            | PlayerEvent::PositionCorrection { position_ms, .. } => {
                trace!("==> Playing");
                let new_nominal_start_time = self.now_ms() - position_ms as i64;
                match self.play_status {
                    SpircPlayStatus::Playing {
                        ref mut nominal_start_time,
                        ..
                    } => {
                        if (*nominal_start_time - new_nominal_start_time).abs() > 100 {
                            *nominal_start_time = new_nominal_start_time;
                            self.connect_state
                                .update_position(position_ms, self.now_ms());
                        } else {
                            return Ok(());
                        }
                    }
                    SpircPlayStatus::LoadingPlay { .. } | SpircPlayStatus::LoadingPause { .. } => {
                        self.connect_state
                            .update_position(position_ms, self.now_ms());
                        self.play_status = SpircPlayStatus::Playing {
                            nominal_start_time: new_nominal_start_time,
                            preloading_of_next_track_triggered: false,
                        };
                    }
                    _ => return Ok(()),
                }
            }
            // Un corte de la red a media canción (Nanofy): el reproductor la dejó en pausa en el
            // segundo que se oyó, sin saltar. Para Spotify y para «reproducir» es una pausa: al
            // reanudar, el reproductor vuelve a ese punto en cuanto hay datos.
            PlayerEvent::Paused {
                position_ms: new_position_ms,
                ..
            }
            | PlayerEvent::Stalled {
                position_ms: new_position_ms,
                ..
            } => {
                trace!("==> Paused");
                match self.play_status {
                    SpircPlayStatus::Paused { .. } | SpircPlayStatus::Playing { .. } => {
                        self.connect_state
                            .update_position(new_position_ms, self.now_ms());
                        self.play_status = SpircPlayStatus::Paused {
                            position_ms: new_position_ms,
                            preloading_of_next_track_triggered: false,
                        };
                    }
                    SpircPlayStatus::LoadingPlay { .. } | SpircPlayStatus::LoadingPause { .. } => {
                        self.connect_state
                            .update_position(new_position_ms, self.now_ms());
                        self.play_status = SpircPlayStatus::Paused {
                            position_ms: new_position_ms,
                            preloading_of_next_track_triggered: false,
                        };
                    }
                    _ => return Ok(()),
                }
            }
            PlayerEvent::Stopped { .. } => {
                trace!("==> Stopped");
                match self.play_status {
                    SpircPlayStatus::Stopped => return Ok(()),
                    _ => self.play_status = SpircPlayStatus::Stopped,
                }
            }
            PlayerEvent::TimeToPreloadNextTrack { .. } => {
                self.handle_preload_next_track();
                return Ok(());
            }
            PlayerEvent::LoadFailed {
                track_id,
                reason,
                transient: true,
                ..
            } => {
                // Un fallo pasajero (Spotify frenando las claves o las peticiones, la red): ni se
                // marca como no disponible ni se salta, que era lo que encadenaba una canción tras
                // otra. Queda en pausa en su posición; el reproductor no tiene nada cargado, así
                // que el próximo «reproducir» (la interfaz lo reintenta sola) la carga otra vez.
                let position_ms = self.position();
                warn!(
                    "<{track_id}> could not be loaded for now ({reason}); paused at {position_ms} ms to retry"
                );
                self.load_failed = true;
                self.connect_state
                    .update_position(position_ms, self.now_ms());
                self.play_status = SpircPlayStatus::Paused {
                    position_ms,
                    preloading_of_next_track_triggered: false,
                };
            }
            // Un fallo definitivo: lo atiende el `Unavailable` que llega justo detrás.
            PlayerEvent::LoadFailed { .. } => return Ok(()),
            PlayerEvent::Unavailable { track_id, .. } => {
                let current = self.connect_state.current_track(|t| &t.uri) == &track_id.to_uri()?;
                if current && self.skip_breaker.trip(Instant::now()) {
                    // Varias seguidas en poco tiempo: no es esta canción, es la lista (o la
                    // cuenta). Se detiene en vez de recorrerla entera marcándolo todo, y sin
                    // precargar la siguiente.
                    warn!(
                        "{} tracks in a row could not be played; stopping instead of skipping",
                        cascade::MAX_FAILURES
                    );
                    self.connect_state.mark_unavailable(&track_id)?;
                    self.handle_stop();
                    self.player
                        .emit_skip_cascade_event(cascade::MAX_FAILURES as u32);
                } else {
                    self.handle_unavailable(&track_id)?;
                    if current {
                        // Un salto que nadie pidió: si la que no se pudo cargar entraba
                        // fundiéndose, la anterior sigue apagándose a su ritmo en vez de cortarse.
                        // Se cuenta: una ráfaga de estos es la cascada de «Skipping to next track».
                        debug!(
                            "auto-skip ({} of {} before stopping)",
                            self.skip_breaker.count(Instant::now()),
                            cascade::MAX_FAILURES
                        );
                        crate::core::ttfs::count(crate::core::ttfs::Counter::AutoSkips);
                        self.handle_next_with(Transition::AutoSkip)?
                    }
                }
            }
            PlayerEvent::CrossfadeReady {
                play_request_id,
                track_id,
                next_track_id,
                fade_ms,
                album_continuation,
                crossfade_albums,
            } => {
                let accepted = self.handle_crossfade_ready(
                    play_request_id,
                    track_id,
                    next_track_id,
                    fade_ms,
                    album_continuation,
                    crossfade_albums,
                )?;
                if !accepted {
                    return Ok(());
                }
            }
            _ => return Ok(()),
        }

        self.update_state = true;
        Ok(())
    }

    async fn handle_connection_id_update(&mut self, connection_id: String) -> Result<(), Error> {
        trace!("Received connection ID update: {connection_id:?}");
        self.session.set_connection_id(&connection_id);

        let mut cluster_raw: Option<Vec<u8>> = None;
        // Por la misma cola que el resto de avisos, para no adelantar ni quedar detrás de uno que
        // aún esté de camino; se espera porque hace falta el clúster que devuelve.
        let new_device = self
            .connect_state
            .state_request_with_reason(PutStateReason::NEW_DEVICE);
        let cluster = match self.state_sender.send_and_wait(new_device).await {
            Ok(res) => {
                cluster_raw = Some(res.to_vec());
                Cluster::parse_from_bytes(&res).ok()
            }
            Err(why) => {
                error!("{why:?}");
                None
            }
        }
        .ok_or(SpircError::FailedDealerSetup)?;
        // Nanofy: el clúster inicial trae lo último que quedó en la cuenta (p. ej. en pausa en
        // el móvil) con su posición; se reenvía a la interfaz para restaurarlo.
        if let Some(raw) = cluster_raw.take() {
            self.player.emit_cluster_snapshot_event(raw);
        }

        debug!(
            "successfully put connect state for {} with connection-id {connection_id}",
            self.session.device_id()
        );

        self.connect_established = true;

        let same_session = cluster.player_state.session_id == self.session.session_id()
            || cluster.player_state.session_id.is_empty();
        if !cluster.active_device_id.is_empty() || !same_session {
            info!(
                "active device is <{}> with session <{}>",
                cluster.active_device_id, cluster.player_state.session_id
            );
            return Ok(());
        } else if cluster.transfer_data.is_empty() {
            debug!("got empty transfer state, do nothing");
            return Ok(());
        } else {
            info!(
                "trying to take over control automatically, session_id: {}",
                cluster.player_state.session_id
            )
        }

        use protobuf::Message;

        match TransferState::parse_from_bytes(&cluster.transfer_data) {
            Ok(transfer_state) => self.handle_transfer(transfer_state)?,
            Err(why) => error!("failed to take over control: {why}"),
        }

        Ok(())
    }

    fn handle_user_attributes_update(&mut self, update: UserAttributesUpdate) {
        trace!("Received attributes update: {update:#?}");
        let attributes: UserAttributes = update
            .pairs
            .iter()
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect();
        self.session.set_user_attributes(attributes)
    }

    fn handle_user_attributes_mutation(&mut self, mutation: UserAttributesMutation) {
        for attribute in mutation.fields.iter() {
            let key = &attribute.name;

            if key == "autoplay" && self.session.config().autoplay.is_some() {
                trace!("Autoplay override active. Ignoring mutation.");
                continue;
            }

            if let Some(old_value) = self.session.user_data().attributes.get(key) {
                let new_value = match old_value.as_ref() {
                    "0" => "1",
                    "1" => "0",
                    _ => old_value,
                };
                self.session.set_user_attribute(key, new_value);

                trace!("Received attribute mutation, {key} was {old_value} is now {new_value}");

                if key == "filter-explicit-content" && new_value == "1" {
                    self.player
                        .emit_filter_explicit_content_changed_event(matches!(new_value, "1"));
                }

                if key == "autoplay" && old_value != new_value {
                    self.player
                        .emit_auto_play_changed_event(matches!(new_value, "1"));

                    self.add_autoplay_resolving_when_required()
                }
            } else {
                trace!("Received attribute mutation for {key} but key was not found!");
            }
        }
    }

    async fn handle_cluster_update(
        &mut self,
        mut cluster_update: ClusterUpdate,
    ) -> Result<(), Error> {
        let reason = cluster_update.update_reason.enum_value();

        let device_ids = cluster_update.devices_that_changed.join(", ");
        debug!(
            "cluster update: {reason:?} from {device_ids}, active device: {}",
            cluster_update.cluster.active_device_id
        );

        if let Some(cluster) = cluster_update.cluster.take() {
            let became_inactive = self.connect_state.is_active()
                && cluster.active_device_id != self.session.device_id();
            if became_inactive {
                info!("device became inactive");
                self.handle_disconnect().await?;
                self.handle_stop();
            } else if self.connect_state.is_active() {
                // fixme: workaround fix, because of missing information why it behaves like it does
                //  background: when another device sends a connect-state update, some player's position de-syncs
                //  tried: providing session_id, playback_id, track-metadata "track_player"
                self.update_state = true;
            }
        } else if self.connect_state.is_active() {
            self.flush_state().await;
            self.connect_state.became_inactive(&self.session).await?;
        }

        Ok(())
    }

    async fn handle_connect_state_request(
        &mut self,
        (request, sender): RequestReply,
    ) -> Result<(), Error> {
        self.connect_state.set_last_command(request.clone());

        debug!(
            "handling: '{}' from {}",
            request.command, request.sent_by_device_id
        );

        let response = match self.handle_request(request).await {
            Ok(_) => Reply::Success,
            Err(why) => {
                error!("failed to handle request: {why}");
                Reply::Failure
            }
        };

        sender.send(response).map_err(Into::into)
    }

    async fn handle_request(&mut self, request: Request) -> Result<(), Error> {
        use Command::*;

        match request.command {
            // errors and unknown commands
            Transfer(transfer) if transfer.data.is_none() => {
                warn!("transfer endpoint didn't contain any data to transfer");
                Err(SpircError::NoData)?
            }
            Unknown(unknown) => Err(SpircError::UnknownEndpoint(unknown))?,
            // implicit update of the connect_state
            UpdateContext(update_context) => {
                if matches!(update_context.context.uri, Some(ref uri) if uri != self.connect_state.context_uri())
                {
                    debug!(
                        "ignoring context update for <{:?}>, because it isn't the current context <{}>",
                        update_context.context.uri,
                        self.connect_state.context_uri()
                    )
                } else {
                    self.context_resolver.add(ResolveContext::from_context(
                        update_context.context,
                        ContextType::Default,
                        ContextAction::Replace,
                    ))
                }
                return Ok(());
            }
            // modification and update of the connect_state
            Transfer(transfer) => {
                self.handle_transfer(transfer.data.expect("by condition checked"))?;
                return self.notify();
            }
            Play(mut play) => {
                if !self.connect_state.is_active() {
                    self.handle_activate()
                }

                let context = match play.context.uri {
                    Some(s) => PlayContext::Uri(s),
                    None if !play.context.pages.is_empty() => PlayContext::Tracks(
                        play.context
                            .pages
                            .iter()
                            .cloned()
                            .flat_map(|p| p.tracks)
                            .flat_map(|t| t.uri)
                            .collect(),
                    ),
                    None => Err(SpircError::NoUri("context"))?,
                };

                let context_options = play
                    .options
                    .player_options_override
                    .map(Into::into)
                    .map(LoadContextOptions::Options);

                let fallback_index = play
                    .options
                    .skip_to
                    .as_ref()
                    .and_then(|s| s.track_index)
                    .map(|i| i as usize);

                self.handle_load(
                    LoadRequest {
                        context,
                        options: LoadRequestOptions {
                            start_playing: true,
                            seek_to: play.options.seek_to.unwrap_or_default(),
                            playing_track: play.options.skip_to.and_then(|s| s.try_into().ok()),
                            context_options,
                            fallback_index: None,
                        },
                    },
                    play.context.pages.pop(),
                    fallback_index,
                )
                .await?;

                self.connect_state.set_origin(play.play_origin)
            }
            Pause(_) => self.handle_pause(),
            SeekTo(seek_to) => {
                // for some reason the position is stored in value, not in position
                trace!("seek to {seek_to:?}");
                self.handle_seek(seek_to.value)
            }
            SetShufflingContext(shuffle) => self.handle_shuffle(shuffle.value)?,
            SetRepeatingContext(repeat_context) => {
                self.handle_repeat_context(repeat_context.value)?
            }
            SetRepeatingTrack(repeat_track) => self.handle_repeat_track(repeat_track.value),
            AddToQueue(add_to_queue) => self.connect_state.add_to_queue(add_to_queue.track, true),
            SetQueue(set_queue) => self.connect_state.handle_set_queue(set_queue),
            SetOptions(set_options) => {
                // Los `set_options` de una Jam llegan como latido cada ~0.5 s con los mismos
                // valores. Empujar el estado a Spotify en cada uno satura el límite de peticiones
                // (429) y Spotify acaba expulsando el dispositivo de la sesión (la sincronización
                // se corta tras unas canciones). Por eso solo se aplica y se reporta el estado
                // cuando algo cambia de verdad.
                let mut changed = false;
                if let Some(repeat_context) = set_options.repeating_context {
                    if repeat_context != self.connect_state.repeat_context() {
                        self.handle_repeat_context(repeat_context)?;
                        changed = true;
                    }
                }

                if let Some(repeat_track) = set_options.repeating_track {
                    if repeat_track != self.connect_state.repeat_track() {
                        self.handle_repeat_track(repeat_track);
                        changed = true;
                    }
                }

                if let Some(shuffle) = set_options.shuffling_context {
                    if shuffle != self.connect_state.shuffling_context() {
                        self.handle_shuffle(shuffle)?;
                        changed = true;
                    }
                }

                if changed {
                    self.update_state = true;
                }
                return Ok(());
            }
            SkipNext(skip_next) => self.handle_next(skip_next.track.map(|t| t.uri))?,
            SkipPrev(_) => self.handle_prev()?,
            Resume(_) if matches!(self.play_status, SpircPlayStatus::Stopped) => {
                self.load_track(true, 0)?
            }
            Resume(_) => self.handle_play(),
        }

        self.update_state = true;
        Ok(())
    }

    fn handle_transfer(&mut self, mut transfer: TransferState) -> Result<(), Error> {
        // Anti-bucle de Jam: la sesión puede reenviar `transfer` para la misma canción muchas veces
        // por segundo. Si ya estamos activos y reproduciendo esa misma pista, no se recarga (evita
        // la tormenta de recargas que corta el audio y dispara el 429 que expulsa el dispositivo).
        if self.connect_state.is_active() {
            if let Ok(incoming) = self.connect_state.current_track_from_transfer(&transfer) {
                let current_uri: String = self.connect_state.current_track(|t| t.uri.clone());
                if !current_uri.is_empty() && incoming.uri == current_uri {
                    debug!(
                        "ignorando transfer repetido para la pista actual <{}>",
                        incoming.uri
                    );
                    return Ok(());
                }
            }
        }

        let mut ctx_uri = match transfer.current_session.context.uri {
            None => Err(SpircError::NoUri("transfer context"))?,
            // can apparently happen when a state is transferred and was started with "uris" via the api
            Some(ref uri) if uri == "-" || uri.is_empty() => None,
            Some(ref uri) => Some(uri.clone()),
        };

        self.connect_state.reset_context(
            ctx_uri
                .as_deref()
                .map(ResetContext::WhenDifferent)
                .unwrap_or(ResetContext::Completely),
        );

        match self.connect_state.current_track_from_transfer(&transfer) {
            Err(why) => warn!("didn't find initial track: {why}"),
            Ok(track) => {
                debug!("found initial track <{}>", track.uri);
                self.connect_state.set_track(track)
            }
        };

        let autoplay = self.connect_state.current_track(|t| t.is_autoplay());
        if autoplay {
            ctx_uri = ctx_uri.map(|c| c.replace("station:", ""));
        }

        let fallback = self.connect_state.current_track(|t| &t.uri).clone();
        let load_from_context_uri = ctx_uri.is_some();

        match ctx_uri {
            Some(ref uri) => {
                self.context_resolver.add(ResolveContext::from_uri(
                    uri.clone(),
                    &fallback,
                    ContextType::Default,
                    ContextAction::Replace,
                ));
            }
            None => {
                let all_tracks = transfer
                    .current_session
                    .context
                    .pages
                    .iter()
                    .cloned()
                    .flat_map(|p| p.tracks)
                    .collect::<Vec<_>>();

                if !all_tracks.is_empty() {
                    self.load_context_from_tracks(all_tracks)?;
                } else {
                    warn!(
                        "tried to transfer with an invalid state, using fallback as ctx_uri ({fallback})"
                    );
                    ctx_uri = Some(fallback.clone())
                }
            }
        };

        self.handle_activate();

        let timestamp = self.now_ms();
        let state = &mut self.connect_state;
        state.handle_initial_transfer(&mut transfer, ctx_uri.clone());

        // adjust active context, so resolve knows for which context it should set up the state
        state.active_context = if autoplay {
            ContextType::Autoplay
        } else {
            ContextType::Default
        };

        // update position if the track continued playing
        let transfer_timestamp = transfer.playback.timestamp.unwrap_or_default();
        let position = match transfer.playback.position_as_of_timestamp {
            Some(position) if transfer.playback.is_paused.unwrap_or_default() => position.into(),
            // update position if the track continued playing
            Some(position) if position > 0 => {
                let time_since_position_update = timestamp - transfer_timestamp;
                i64::from(position) + time_since_position_update
            }
            _ => 0,
        };

        let is_playing = !transfer.playback.is_paused();

        if self.connect_state.current_track(|t| t.is_autoplay()) || autoplay {
            if let Some(ctx_uri) = ctx_uri {
                debug!("currently in autoplay context, async resolving autoplay for {ctx_uri}");
                self.context_resolver.add(ResolveContext::from_uri(
                    ctx_uri,
                    fallback,
                    ContextType::Autoplay,
                    ContextAction::Replace,
                ))
            } else {
                warn!("couldn't resolve autoplay context without a context uri");
            }
        }

        if load_from_context_uri {
            self.transfer_state = Some(transfer);
        } else {
            match self.connect_state.get_context(ContextType::Default) {
                Err(why) => {
                    warn!("continuing transfer in an unknown state. {why}");
                    self.transfer_state = Some(transfer);
                }
                Ok(ctx) => {
                    let idx = ConnectState::find_index_in_context(ctx, |pt| {
                        self.connect_state.current_track(|t| pt.uri == t.uri)
                    })?;
                    self.connect_state.reset_playback_to_position(Some(idx))?;
                }
            }
        }

        self.load_track(is_playing, position.try_into()?)
    }

    async fn handle_disconnect(&mut self) -> Result<(), Error> {
        self.context_resolver.clear();

        self.play_status = SpircPlayStatus::Stopped {};
        self.connect_state
            .update_position_in_relation(self.now_ms());
        self.notify()?;
        // El último estado (posición incluida, para retomarla) y todo lo anterior, antes de
        // pasar a inactivo: llegando después lo desharía.
        self.flush_state().await;

        self.connect_state.became_inactive(&self.session).await?;

        self.player
            .emit_session_disconnected_event(self.session.connection_id(), self.session.username());

        Ok(())
    }

    fn handle_stop(&mut self) {
        // Parada: ya no hay nada que recargar al pulsar «reproducir».
        self.load_failed = false;
        self.player.stop();
        self.connect_state.update_position(0, self.now_ms());
        self.connect_state.clear_next_tracks();

        if let Err(why) = self.connect_state.reset_playback_to_position(None) {
            warn!("failed filling up next_track during stopping: {why}")
        }
    }

    fn handle_activate(&mut self) {
        self.connect_state.set_active(true);
        self.player
            .emit_session_connected_event(self.session.connection_id(), self.session.username());
        self.player.emit_session_client_changed_event(
            self.session.client_id(),
            self.session.client_name(),
            self.session.client_brand_name(),
            self.session.client_model_name(),
        );

        self.player
            .emit_volume_changed_event(self.connect_state.device_info().volume as u16);

        self.player
            .emit_auto_play_changed_event(self.session.autoplay());

        self.player
            .emit_filter_explicit_content_changed_event(self.session.filter_explicit_content());

        self.player
            .emit_shuffle_changed_event(self.connect_state.shuffling_context());

        self.player.emit_repeat_changed_event(
            self.connect_state.repeat_context(),
            self.connect_state.repeat_track(),
        );
    }

    async fn handle_load(
        &mut self,
        cmd: LoadRequest,
        page: Option<ContextPage>,
        fallback_index: Option<usize>,
    ) -> Result<(), Error> {
        // Algo nuevo que reproducir: los fallos de lo anterior ya no cuentan para la cascada.
        self.skip_breaker.reset();
        self.connect_state
            .reset_context(if let PlayContext::Uri(ref uri) = cmd.context {
                ResetContext::WhenDifferent(uri)
            } else {
                ResetContext::Completely
            });

        self.connect_state.reset_options();

        let autoplay = matches!(cmd.context_options, Some(LoadContextOptions::Autoplay));
        match cmd.context {
            PlayContext::Uri(uri) => {
                self.load_context_from_uri(uri, page.as_ref(), autoplay)
                    .await?
            }
            PlayContext::Tracks(tracks) => self.load_context_from_tracks(tracks)?,
        }
        // Contexto resuelto (context-resolve, o la lista de pistas tal cual): aún no se ha pedido
        // nada al reproductor.
        crate::core::ttfs::mark("spirc:context", None);

        let cmd_options = cmd.options;

        self.connect_state.set_active_context(ContextType::Default);

        // for play commands with skip by uid, the context of the command contains
        // tracks with uri and uid, so we merge the new context with the resolved/existing context
        self.connect_state.merge_context(page);

        // load here, so that we clear the queue only after we definitely retrieved a new context
        self.connect_state.clear_next_tracks();
        self.connect_state.clear_restrictions();

        debug!("play track <{:?}>", cmd_options.playing_track);

        // Siempre un índice que existe en el contexto: antes, uno fuera de rango (o una canción
        // que no estaba en la primera página de una lista enorme) hacía fallar la carga entera o
        // empezaba por la primera canción (ver `start_index`).
        let index = match cmd_options.playing_track {
            None => None,
            Some(ref playing_track) => Some(
                self.start_index(playing_track, cmd_options.fallback_index, fallback_index)
                    .await,
            ),
        };

        if let Some(LoadContextOptions::Options(ref options)) = cmd_options.context_options {
            debug!(
                "loading with shuffle: <{}>, repeat track: <{}> context: <{}>",
                options.shuffle, options.repeat, options.repeat_track
            );

            self.connect_state.set_shuffle(options.shuffle);
            self.connect_state.set_repeat_context(options.repeat);
            self.connect_state.set_repeat_track(options.repeat_track);
        }

        if matches!(cmd_options.context_options, Some(LoadContextOptions::Options(ref o)) if o.shuffle)
        {
            if let Some(index) = index {
                self.connect_state.set_current_track(index)?;
            } else {
                self.connect_state.set_current_track_random()?;
            }

            if self.context_resolver.has_next() {
                self.connect_state.update_queue_revision()
            } else {
                self.connect_state.shuffle_new()?;
                self.add_autoplay_resolving_when_required();
            }
        } else {
            self.connect_state
                .set_current_track(index.unwrap_or_default())?;
            self.connect_state.reset_playback_to_position(index)?;
            self.add_autoplay_resolving_when_required();
        }

        if self.connect_state.current_track(MessageField::is_some) {
            self.load_track(cmd_options.start_playing, cmd_options.seek_to)?;
        } else {
            info!("No active track, stopping");
            self.handle_stop()
        }

        Ok(())
    }

    /// Posición del contexto en la que empieza una carga (ver `start_index`). `hint` es la fila
    /// que pulsó el usuario en Nanofy y `spotify_fallback`, el índice de una orden de otro
    /// dispositivo. Si la canción pedida aún no está en lo que se tiene del contexto (una lista
    /// enorme cuya página no ha llegado), se traen más páginas, pocas y con tiempo tasado; si ni
    /// así aparece, se empieza por el índice de Spotify si existe o por la primera, sin hacer
    /// fallar la carga.
    async fn start_index(
        &mut self,
        playing_track: &PlayingTrack,
        hint: Option<u32>,
        spotify_fallback: Option<usize>,
    ) -> usize {
        let wanted = match playing_track {
            PlayingTrack::Index(i) => Wanted::Index(*i as usize),
            PlayingTrack::Uri(uri) => Wanted::Uri(uri),
            PlayingTrack::Uid(uid) => Wanted::Uid(uid),
        };
        let hint = hint.map(|i| i as usize);
        let deadline = tokio::time::Instant::now() + start_index::EXTRA_PAGES_BUDGET;
        let mut pages = 0;
        loop {
            let found = self
                .connect_state
                .get_context(ContextType::Default)
                .ok()
                .and_then(|ctx| {
                    start_index::locate(
                        ctx.tracks.as_slice(),
                        |t| t.uri.as_str(),
                        |t| t.uid.as_str(),
                        wanted,
                        hint,
                    )
                });
            if let Some(i) = found {
                if pages > 0 {
                    info!("found {playing_track:?} at {i} after {pages} more context pages");
                    crate::core::ttfs::mark("spirc:pages", Some(pages.to_string()));
                }
                return i;
            }
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if pages >= start_index::MAX_EXTRA_PAGES
                || left.is_zero()
                || !self.context_resolver.next_is_default_page()
            {
                break;
            }
            pages += 1;
            match tokio::time::timeout(left, self.context_resolver.get_next_context(Vec::new))
                .await
            {
                Ok(next) => {
                    self.handle_next_context(next);
                }
                // Lo que falta se sigue trayendo en segundo plano, como siempre.
                Err(_) => break,
            }
        }
        let len = self
            .connect_state
            .get_context(ContextType::Default)
            .map(|ctx| ctx.tracks.len())
            .unwrap_or(0);
        let i = start_index::give_up(len, spotify_fallback);
        warn!(
            "Failed to find {playing_track:?} in the context ({len} tracks, {pages} more pages); starting at {i}"
        );
        i
    }

    async fn load_context_from_uri(
        &mut self,
        context_uri: String,
        page: Option<&ContextPage>,
        autoplay: bool,
    ) -> Result<(), Error> {
        if !self.connect_state.is_active() {
            self.handle_activate();
        }

        let update_context = if autoplay {
            ContextType::Autoplay
        } else {
            ContextType::Default
        };

        self.connect_state.set_active_context(update_context);

        let fallback = match page {
            // check that the uri is valid or the page has a valid uri that can be used
            Some(page) => match ConnectState::find_valid_uri(Some(&context_uri), Some(page)) {
                Some(ctx_uri) => ctx_uri,
                None => return Err(SpircError::InvalidUri(context_uri).into()),
            },
            // when there is no page, the uri should be valid
            None => &context_uri,
        };

        let current_context_uri = self.connect_state.context_uri();

        if current_context_uri == &context_uri && fallback == context_uri {
            debug!("context <{current_context_uri}> didn't change, no resolving required")
        } else {
            debug!("resolving context for load command");
            self.context_resolver.clear();
            self.context_resolver.add(ResolveContext::from_uri(
                &context_uri,
                fallback,
                update_context,
                ContextAction::Replace,
            ));
            let context = self.context_resolver.get_next_context(Vec::new).await;
            self.handle_next_context(context);
        }

        Ok(())
    }

    fn load_context_from_tracks(&mut self, tracks: impl Into<ContextPage>) -> Result<(), Error> {
        const WEB_API_URI: &str = "spotify:web-api";
        let ctx = Context {
            // by providing values for uri/url the player in the official client's isn't frozen
            uri: Some(WEB_API_URI.into()),
            url: Some(format!("context://{WEB_API_URI}")),
            pages: vec![tracks.into()],
            ..Default::default()
        };

        let _ = self
            .connect_state
            .update_context(ctx, ContextType::Default)?;

        Ok(())
    }

    fn handle_play(&mut self) {
        match self.play_status {
            // La carga falló por algo pasajero y el reproductor no tiene nada que reanudar: se
            // vuelve a cargar, sonando, en el punto en que quedó.
            SpircPlayStatus::Paused { position_ms, .. } if self.load_failed => {
                info!("Reloading the track that failed to load, at {position_ms} ms");
                if let Err(e) = self.load_track(true, position_ms) {
                    warn!("could not reload the track that failed to load: {e}");
                    return;
                }
            }
            SpircPlayStatus::Paused {
                position_ms,
                preloading_of_next_track_triggered,
            } => {
                self.player.play();
                self.connect_state
                    .update_position(position_ms, self.now_ms());
                self.play_status = SpircPlayStatus::Playing {
                    nominal_start_time: self.now_ms() - position_ms as i64,
                    preloading_of_next_track_triggered,
                };
            }
            SpircPlayStatus::LoadingPause { position_ms } => {
                self.player.play();
                self.play_status = SpircPlayStatus::LoadingPlay { position_ms };
            }
            _ => return,
        }

        // Synchronize the volume from the mixer. This is useful on
        // systems that can switch sources from and back to librespot.
        let current_volume = self.mixer.volume();
        self.set_volume(current_volume);
    }

    /// Ver `Spirc::reload`. La posición es la de la pausa: la que el reproductor dio al cortarse.
    fn handle_reload(&mut self) -> Result<(), Error> {
        if let SpircPlayStatus::Paused { position_ms, .. } = self.play_status {
            info!("Reloading the paused track from scratch, at {position_ms} ms");
            self.load_track(true, position_ms)?;
        }
        Ok(())
    }

    fn handle_play_pause(&mut self) {
        match self.play_status {
            SpircPlayStatus::Paused { .. } | SpircPlayStatus::LoadingPause { .. } => {
                self.handle_play()
            }
            SpircPlayStatus::Playing { .. } | SpircPlayStatus::LoadingPlay { .. } => {
                self.handle_pause()
            }
            _ => (),
        }
    }

    fn handle_pause(&mut self) {
        match self.play_status {
            SpircPlayStatus::Playing {
                nominal_start_time,
                preloading_of_next_track_triggered,
            } => {
                self.player.pause();
                let position_ms = (self.now_ms() - nominal_start_time) as u32;
                self.connect_state
                    .update_position(position_ms, self.now_ms());
                self.play_status = SpircPlayStatus::Paused {
                    position_ms,
                    preloading_of_next_track_triggered,
                };
            }
            SpircPlayStatus::LoadingPlay { position_ms } => {
                self.player.pause();
                self.play_status = SpircPlayStatus::LoadingPause { position_ms };
            }
            _ => (),
        }
    }

    fn handle_seek(&mut self, position_ms: u32) {
        let duration = self.connect_state.player().duration;
        if i64::from(position_ms) > duration {
            warn!("tried to seek to {position_ms}ms of {duration}ms");
            return;
        }

        self.connect_state
            .update_position(position_ms, self.now_ms());
        if self.load_failed {
            // Sin nada cargado no hay dónde buscar (el reproductor volvería a cargarla y a
            // sonar estando en pausa): se apunta la posición para la próxima carga.
            if let SpircPlayStatus::Paused {
                position_ms: ref mut position,
                ..
            } = self.play_status
            {
                *position = position_ms;
            }
            return;
        }
        self.player.seek(position_ms);
        let now = self.now_ms();
        match self.play_status {
            SpircPlayStatus::Stopped => (),
            SpircPlayStatus::LoadingPause {
                position_ms: ref mut position,
            }
            | SpircPlayStatus::LoadingPlay {
                position_ms: ref mut position,
            }
            | SpircPlayStatus::Paused {
                position_ms: ref mut position,
                ..
            } => *position = position_ms,
            SpircPlayStatus::Playing {
                ref mut nominal_start_time,
                ..
            } => *nominal_start_time = now - position_ms as i64,
        };
    }

    fn handle_shuffle(&mut self, shuffle: bool) -> Result<(), Error> {
        self.player.emit_shuffle_changed_event(shuffle);
        self.connect_state.handle_shuffle(shuffle)
    }

    fn handle_repeat_context(&mut self, repeat: bool) -> Result<(), Error> {
        self.player
            .emit_repeat_changed_event(repeat, self.connect_state.repeat_track());
        self.connect_state.handle_set_repeat_context(repeat)
    }

    fn handle_repeat_track(&mut self, repeat: bool) {
        self.player
            .emit_repeat_changed_event(self.connect_state.repeat_context(), repeat);
        self.connect_state.set_repeat_track(repeat);
    }

    fn handle_preload_next_track(&mut self) {
        // Requests the player thread to preload the next track
        match self.play_status {
            SpircPlayStatus::Paused {
                ref mut preloading_of_next_track_triggered,
                ..
            }
            | SpircPlayStatus::Playing {
                ref mut preloading_of_next_track_triggered,
                ..
            } => {
                *preloading_of_next_track_triggered = true;
            }
            _ => (),
        }

        if let Some(track_id) = self.connect_state.preview_next_track() {
            self.player.preload(track_id);
        }
    }

    // Mark unavailable tracks so we can skip them later
    fn handle_unavailable(&mut self, track_id: &SpotifyUri) -> Result<(), Error> {
        self.connect_state.mark_unavailable(track_id)?;
        self.handle_preload_next_track();

        Ok(())
    }

    fn add_autoplay_resolving_when_required(&mut self) {
        let require_load_new = !self
            .connect_state
            .has_next_tracks(Some(CONTEXT_FETCH_THRESHOLD))
            && self.session.autoplay()
            // En una Jam como participante no rellenamos con autoplay: la reproducción la manda el
            // anfitrión, así que no metemos canciones de nuestra biblioteca en la cola compartida.
            && !self.jam_participant
            && !self.connect_state.context_uri().is_empty();

        if !require_load_new {
            return;
        }

        let current_context = self.connect_state.context_uri();
        let fallback = self.connect_state.current_track(|t| &t.uri);

        let has_tracks = self
            .connect_state
            .get_context(ContextType::Autoplay)
            .map(|c| !c.tracks.is_empty())
            .unwrap_or_default();

        let resolve = ResolveContext::from_uri(
            current_context,
            fallback,
            ContextType::Autoplay,
            if has_tracks {
                ContextAction::Append
            } else {
                ContextAction::Replace
            },
        );

        self.context_resolver.add(resolve);
    }

    fn handle_next(&mut self, track_uri: Option<String>) -> Result<(), Error> {
        let continue_playing = self.connect_state.is_playing();

        let current_uri = self.connect_state.current_track(|t| &t.uri);
        let mut has_next_track =
            matches!(track_uri, Some(ref track_uri) if current_uri == track_uri);

        if !has_next_track {
            has_next_track = loop {
                let index = self.connect_state.next_track()?;

                let current_uri = self.connect_state.current_track(|t| &t.uri);
                if matches!(track_uri, Some(ref track_uri) if current_uri != track_uri) {
                    continue;
                } else {
                    break index.is_some();
                }
            };
        };

        if has_next_track {
            self.add_autoplay_resolving_when_required();
            self.load_track(continue_playing, 0)
        } else {
            info!("Not playing next track because there are no more tracks left in queue.");
            self.handle_stop();
            Ok(())
        }
    }

    /// `handle_next` diciendo cómo debe empezar la siguiente (fundido aceptado o salto
    /// automático). Pase lo que pase (sin siguiente, error), lo próximo vuelve a ser un corte:
    /// la transición no puede quedarse esperando a una carga que llegue más tarde por otro motivo.
    fn handle_next_with(&mut self, transition: Transition) -> Result<(), Error> {
        self.next_load_transition = transition;
        let result = self.handle_next(None);
        self.next_load_transition = Transition::Cut;
        result
    }

    /// El reproductor propone fundir la canción que suena con la siguiente
    /// (`PlayerEvent::CrossfadeReady`). Decide Spirc, que sabe lo que el reproductor no: el
    /// contexto (un álbum en orden), la repetición, la Jam y cómo está la cola ahora. Al aceptar
    /// avanza como al final de la canción, pero la siguiente se carga con
    /// `Transition::Crossfade`; al rechazar no hace nada: la canción suena hasta el final y la
    /// siguiente entra sin hueco, como siempre. `true` si aceptó (el estado cambió).
    fn handle_crossfade_ready(
        &mut self,
        play_request_id: u64,
        track_id: SpotifyUri,
        next_track_id: SpotifyUri,
        fade_ms: u32,
        album_continuation: bool,
        crossfade_albums: bool,
    ) -> Result<bool, Error> {
        let current_matches = SpotifyUri::from_uri(self.connect_state.current_track(|t| &t.uri))
            .is_ok_and(|current| current == track_id);
        // La siguiente de verdad es la que sacará `next_track`: ni una marca de fin de contexto ni
        // una ya no disponible (las salta), que la vista previa sí devolvería.
        let (next_usable, next_from_context) = self
            .connect_state
            .next_tracks()
            .first()
            .map_or((false, false), |t| (!t.is_unavailable(), t.is_context()));
        let next_matches =
            next_usable && self.connect_state.preview_next_track().as_ref() == Some(&next_track_id);

        // Lo que le queda según la cuenta de Spirc: la propuesta salió antes de una búsqueda
        // hacia atrás que Spirc ya atendió si ahora queda mucho más.
        let duration = self.connect_state.player().duration;
        let remaining_ms = if duration > 0 {
            let left = duration - i64::from(self.position());
            Some(u32::try_from(left.max(0)).unwrap_or(u32::MAX))
        } else {
            None
        };

        let offer = FadeOffer {
            playing: matches!(self.play_status, SpircPlayStatus::Playing { .. }),
            repeat_track: self.connect_state.repeat_track(),
            jam_participant: self.jam_participant,
            already_accepted: self.crossfade_from_prid == Some(play_request_id),
            queue_matches: current_matches && next_matches,
            remaining_ms,
            fade_ms,
            album_in_order: crossfade::is_album_in_order(
                self.connect_state.context_uri(),
                self.connect_state.shuffling_context(),
                self.connect_state.current_track(|t| t.is_context()),
                next_from_context,
            ),
            album_continuation,
            crossfade_albums,
        };
        if let Err(why) = crossfade::decide_offer(&offer) {
            info!(
                "[fundido] {track_id} → {next_track_id}: no se funde ({}); sin hueco al acabar",
                why.reason()
            );
            return Ok(false);
        }

        info!(
            "[fundido] aceptado {track_id} → {next_track_id}: {fade_ms} ms (quedan {} ms según Spirc)",
            remaining_ms.map_or_else(|| "?".to_string(), |ms| ms.to_string())
        );
        self.crossfade_from_prid = Some(play_request_id);
        self.crossfade_to = Some(next_track_id);
        let result = self.handle_next_with(Transition::Crossfade);
        self.crossfade_to = None;
        if result.is_err() {
            // Sin carga en camino, el final de la canción tiene que avanzar como siempre.
            self.crossfade_from_prid = None;
        }
        result.map(|()| true)
    }

    fn handle_prev(&mut self) -> Result<(), Error> {
        // Previous behaves differently based on the position
        // Under 3s it goes to the previous song (starts playing)
        // Over 3s it seeks to zero (retains previous play status)
        if self.position() < 3000 {
            let repeat_context = self.connect_state.repeat_context();
            match self.connect_state.prev_track()? {
                None if repeat_context => self.connect_state.reset_playback_to_position(None)?,
                None => {
                    self.connect_state.reset_playback_to_position(None)?;
                    self.handle_stop()
                }
                Some(_) => self.load_track(self.connect_state.is_playing(), 0)?,
            }
        } else {
            self.handle_seek(0);
        }

        Ok(())
    }

    fn handle_volume_up(&mut self) {
        let volume = (self.connect_state.device_info().volume as u16)
            .saturating_add(self.connect_state.volume_step_size);

        self.set_volume(volume);
    }

    fn handle_volume_down(&mut self) {
        let volume = (self.connect_state.device_info().volume as u16)
            .saturating_sub(self.connect_state.volume_step_size);

        self.set_volume(volume);
    }

    fn handle_playlist_modification(
        &mut self,
        playlist_modification_info: PlaylistModificationInfo,
    ) -> Result<(), Error> {
        let uri = playlist_modification_info
            .uri
            .ok_or(SpircError::NoUri("playlist modification"))?;
        let uri = String::from_utf8(uri)?;

        if self.connect_state.context_uri() != &uri {
            debug!(
                "ignoring playlist modification update for playlist <{uri}>, because it isn't the current context"
            );
            return Ok(());
        }

        debug!("playlist modification for current context: {uri}");
        self.context_resolver.add(ResolveContext::from_uri(
            uri,
            self.connect_state.current_track(|t| &t.uri),
            ContextType::Default,
            ContextAction::Replace,
        ));

        Ok(())
    }

    fn handle_session_update(&mut self, session_update: FallbackWrapper<SessionUpdate>) {
        // we know that this enum value isn't present in our current proto definitions, by that
        // the json parsing fails because the enum isn't known as proto representation
        const WBC: &str = "WIFI_BROADCAST_CHANGED";

        let mut session_update = match session_update {
            FallbackWrapper::Inner(update) => update,
            FallbackWrapper::Fallback(value) => {
                let fallback_inner = value.to_string();
                if fallback_inner.contains(WBC) {
                    log::debug!("Received SessionUpdate::{WBC}");
                } else {
                    log::warn!("SessionUpdate couldn't be parse correctly: {value:?}");
                }
                return;
            }
        };

        let reason = session_update.reason.enum_value();

        let mut session = match session_update.session.take() {
            None => return,
            Some(session) => session,
        };

        let active_device = session.host_active_device_id.take();
        if matches!(active_device, Some(ref device) if device == self.session.device_id()) {
            info!(
                "session update: <{:?}> for self, current session_id {}, new session_id {}",
                reason,
                self.session.session_id(),
                session.session_id
            );

            if self.session.session_id() != session.session_id {
                self.session.set_session_id(&session.session_id);
                self.connect_state.set_session_id(session.session_id.clone());
            }
        } else {
            debug!("session update: <{reason:?}> from active session host: <{active_device:?}>");
        }

        // Jam: somos PARTICIPANTE (el anfitrión manda la reproducción) cuando la sesión sigue
        // activa y su dispositivo activo es otro, no el nuestro. Al salir, ser expulsados o
        // terminar la sesión, volvemos al modo normal (autoplay y cola local otra vez).
        match reason {
            Ok(SessionUpdateReason::YOU_LEFT)
            | Ok(SessionUpdateReason::SESSION_DELETED)
            | Ok(SessionUpdateReason::YOU_WERE_KICKED) => {
                self.jam_participant = false;
                self.jam_session_id = None;
                self.jam_last_queue.clear();
            }
            _ => {
                self.jam_participant =
                    matches!(active_device, Some(ref d) if d != self.session.device_id());
                if !session.session_id.is_empty() {
                    self.jam_session_id = Some(session.session_id.clone());
                }
            }
        }
        debug!(
            "jam_participant = {} session = {:?}",
            self.jam_participant, self.jam_session_id
        );

        // this seems to be used for jams or handling the current session_id
        //
        // handling this event was intended to keep the playback when other clients (primarily
        // mobile) connects, otherwise they would steel the current playback when there was no
        // session_id provided on the initial PutStateReason::NEW_DEVICE state update
        //
        // by generating an initial session_id from the get-go we prevent that behavior and
        // currently don't need to handle this event, might still be useful for later "jam" support
    }

    fn position(&mut self) -> u32 {
        match self.play_status {
            SpircPlayStatus::Stopped => 0,
            SpircPlayStatus::LoadingPlay { position_ms }
            | SpircPlayStatus::LoadingPause { position_ms }
            | SpircPlayStatus::Paused { position_ms, .. } => position_ms,
            SpircPlayStatus::Playing {
                nominal_start_time, ..
            } => (self.now_ms() - nominal_start_time) as u32,
        }
    }

    fn load_track(&mut self, start_playing: bool, position_ms: u32) -> Result<(), Error> {
        // Cada carga consume la transición pedida (un corte si nadie pidió otra), también cuando
        // al final no carga nada: no puede pasar a una carga posterior.
        let transition = std::mem::take(&mut self.next_load_transition);
        // Una carga nueva (otra canción, o la misma otra vez) deja atrás la que falló.
        self.load_failed = false;

        if self.connect_state.current_track(MessageField::is_none) {
            debug!("current track is none, stopping playback");
            self.handle_stop();
            return Ok(());
        }

        let current_uri = self.connect_state.current_track(|t| &t.uri);
        let id = SpotifyUri::from_uri(current_uri)?;
        // Solo se funde con la canción aceptada, desde el principio y sonando. Si la que toca es
        // otra (la cola cambió entre la decisión y el avance), entra con un corte.
        let transition = match transition {
            Transition::Crossfade
                if !start_playing
                    || position_ms != 0
                    || self.crossfade_to.as_ref() != Some(&id) =>
            {
                info!("[fundido] {id} no es la carga aceptada (otra, en pausa o a mitad): corte");
                Transition::Cut
            }
            other => other,
        };
        // Ganancia de álbum como Spotify: un álbum escuchado en orden se normaliza entero, así
        // las intros y las baladas no suben respecto al resto. En aleatorio, o con una canción de
        // la cola o de autoplay (proveedor distinto de «context»), por canción. Va antes de
        // `load`: el reproductor atiende las órdenes en orden y calcula el factor al empezar.
        let album = self.connect_state.context_uri().starts_with("spotify:album:")
            && !self.connect_state.shuffling_context()
            && self.connect_state.current_track(|t| t.is_context());
        self.player.set_auto_normalise_as_album(album);
        self.player
            .load_with_transition(id, start_playing, position_ms, transition);

        self.connect_state
            .update_position(position_ms, self.now_ms());
        if start_playing {
            self.play_status = SpircPlayStatus::LoadingPlay { position_ms };
        } else {
            self.play_status = SpircPlayStatus::LoadingPause { position_ms };
        }
        self.connect_state.set_status(&self.play_status);

        Ok(())
    }

    /// Cuenta a Spotify el estado actual. Se copia aquí mismo y lo envía `StateSender` en
    /// segundo plano: quien avisa no espera al PUT (~120 ms, o hasta su plazo si no contesta), y
    /// si se acumulan varios mientras otro va de camino solo sale el último.
    fn notify(&mut self) -> Result<(), Error> {
        self.emit_jam_queue_if_changed();
        // Si ya se precargó la siguiente y cambió (cola, aleatorio…), se precarga la nueva. El
        // reproductor ignora la petición si ya tiene esa misma.
        if let SpircPlayStatus::Playing { preloading_of_next_track_triggered: true, .. }
        | SpircPlayStatus::Paused { preloading_of_next_track_triggered: true, .. } = self.play_status
        {
            self.handle_preload_next_track();
        }
        self.connect_state.set_status(&self.play_status);

        if self.connect_state.is_playing() {
            self.connect_state
                .update_position_in_relation(self.now_ms());
        }

        self.connect_state.set_now(self.now_ms() as u64);

        self.state_sender.send(self.connect_state.state_request());
        Ok(())
    }

    /// Espera a que Spotify haya recibido todo lo avisado hasta ahora. Con la sesión ya perdida
    /// no: nada de eso llegaría, y el fin de esta tarea (que pide reconectar) no debe esperarlo.
    async fn flush_state(&mut self) {
        if !self.session.is_invalid() {
            self.state_sender.flush().await;
        }
    }

    fn set_volume(&mut self, volume: u16) {
        debug!("SpircTask::set_volume({volume})");

        let old_volume = self.connect_state.device_info().volume;
        let new_volume = volume as u32;
        if old_volume != new_volume || self.mixer.volume() != volume {
            self.update_volume = true;

            self.connect_state.set_volume(new_volume);
            self.mixer.set_volume(volume);
            if let Some(cache) = self.session.cache() {
                cache.save_volume(volume)
            }
            if self.connect_state.is_active() {
                self.player.emit_volume_changed_event(volume);
            }
        }
    }
}

impl Drop for SpircTask {
    fn drop(&mut self) {
        debug!("drop Spirc[{}]", self.spirc_id);
    }
}

/// Construye el JSON del comando `play` para enviarlo a una Jam cuando el participante reproduce
/// un contexto (álbum, playlist…). Devuelve `None` para listas de pistas sueltas (aún no
/// soportadas por esta vía). Las uris de Spotify no llevan caracteres que rompan el JSON.
fn jam_play_json(command: &LoadRequest) -> Option<String> {
    let ctx = match &command.context {
        PlayContext::Uri(uri) => uri.clone(),
        PlayContext::Tracks(_) => return None,
    };
    let seek = command.options.seek_to;
    let skip_to = match &command.options.playing_track {
        Some(PlayingTrack::Uri(uri)) => format!(r#","skip_to":{{"track_uri":"{uri}"}}"#),
        _ => String::new(),
    };
    Some(format!(
        r#"{{"command":{{"endpoint":"play","context":{{"uri":"{ctx}"}},"options":{{"seek_to":{seek}{skip_to}}},"play_origin":{{"feature_identifier":"harmony"}}}}}}"#
    ))
}
