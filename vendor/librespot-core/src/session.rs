use std::{
    collections::HashMap,
    future::Future,
    io, mem,
    pin::Pin,
    sync::{
        Arc, OnceLock, RwLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::dealer::manager::DealerManager;
use crate::{
    Error,
    ap_link::{self, LinkLoss, LinkSlot, LinkState, LinkStatus, LinkWait, RenewFailure},
    apresolve::{ApResolver, SocketAddress},
    audio_key::AudioKeyManager,
    authentication::Credentials,
    cache::Cache,
    channel::ChannelManager,
    config::SessionConfig,
    connection::{self, AuthenticationError, Transport},
    http_client::HttpClient,
    login5::Login5Manager,
    mercury::MercuryManager,
    packet::PacketType,
    protocol::keyexchange::ErrorCode,
    spclient::SpClient,
    token::TokenProvider,
};
use byteorder::{BigEndian, ByteOrder};
use bytes::Bytes;
use futures_core::TryStream;
use futures_util::StreamExt;
use librespot_protocol::authentication::AuthenticationType;
use num_traits::FromPrimitive;
use pin_project_lite::pin_project;
use quick_xml::events::Event;
use thiserror::Error;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::{Duration as TokioDuration, Instant as TokioInstant, Sleep, sleep},
};
use tokio_stream::wrappers::UnboundedReceiverStream;
use uuid::Uuid;

const SESSION_DATA_POISON_MSG: &str = "session data rwlock should not be poisoned";

#[derive(Debug, Error)]
pub enum SessionError {
    #[error(transparent)]
    AuthenticationError(#[from] AuthenticationError),
    #[error("Cannot create session: {0}")]
    IoError(#[from] io::Error),
    #[error("Session is not connected")]
    NotConnected,
    #[error("packet {0} unknown")]
    Packet(u8),
}

impl From<SessionError> for Error {
    fn from(err: SessionError) -> Self {
        match err {
            SessionError::AuthenticationError(_) => Error::unauthenticated(err),
            SessionError::IoError(_) => Error::unavailable(err),
            SessionError::NotConnected => Error::unavailable(err),
            SessionError::Packet(_) => Error::unimplemented(err),
        }
    }
}

impl From<quick_xml::encoding::EncodingError> for Error {
    fn from(err: quick_xml::encoding::EncodingError) -> Self {
        Error::invalid_argument(err)
    }
}

pub type UserAttributes = HashMap<String, String>;

/// El punto de acceso rechazó el inicio de sesión porque la cuenta no es Premium.
pub fn is_premium_required(e: &Error) -> bool {
    let premium = |a: &AuthenticationError| {
        matches!(
            a,
            AuthenticationError::LoginFailed(ErrorCode::PremiumAccountRequired)
        )
    };
    if let Some(a) = e.error.downcast_ref::<AuthenticationError>() {
        return premium(a);
    }
    matches!(
        e.error.downcast_ref::<SessionError>(),
        Some(SessionError::AuthenticationError(a)) if premium(a)
    )
}

#[derive(Debug, Clone, Default)]
pub struct UserData {
    pub country: String,
    pub canonical_username: String,
    pub attributes: UserAttributes,
}

#[derive(Debug, Clone, Default)]
struct SessionData {
    session_id: String,
    client_id: String,
    client_name: String,
    client_brand_name: String,
    client_model_name: String,
    connection_id: String,
    auth_data: Vec<u8>,
    time_delta: i64,
    invalid: bool,
    user_data: UserData,
    /// Nanofy: autoplay cambiado en marcha (`Session::set_autoplay`); manda sobre la configuración.
    autoplay_live: Option<bool>,
}

/// El enlace con el punto de acceso en uso (Nanofy): su estado y, mientras está enlazado, la cola
/// de envío y las tareas que escriben y leen. Al renovarse se sustituye entero; Mercury y los
/// canales no se migran: Nanofy no los usa (todo va por spclient y el dealer).
struct Link {
    slot: LinkSlot,
    tx: Option<mpsc::UnboundedSender<(u8, Vec<u8>)>>,
    tasks: Vec<JoinHandle<()>>,
}

struct SessionInternal {
    config: SessionConfig,
    data: RwLock<SessionData>,

    http_client: HttpClient,
    /// Nunca se retiene a través de un `await` (ver `Session::lock_link`).
    link: std::sync::Mutex<Link>,
    /// El estado del enlace para quien lo vigila (Nanofy, para renovarlo; las claves, para esperar
    /// al nuevo). Un `watch`: el último estado nunca se pierde, aunque nadie escuche en ese momento.
    link_watch: watch::Sender<LinkStatus>,
    /// Una sola renovación a la vez.
    renew_lock: tokio::sync::Mutex<()>,
    born: Instant,
    /// Último paquete llegado del AP, en µs desde `born` más 1 (0: ninguno aún).
    rx_at_us: AtomicU64,

    apresolver: OnceLock<ApResolver>,
    audio_key: OnceLock<AudioKeyManager>,
    channel: OnceLock<ChannelManager>,
    mercury: OnceLock<MercuryManager>,
    dealer: OnceLock<DealerManager>,
    spclient: OnceLock<SpClient>,
    token_provider: OnceLock<TokenProvider>,
    login5: OnceLock<Login5Manager>,
    cache: Option<Arc<Cache>>,

    handle: tokio::runtime::Handle,
}

/// A shared reference to a Spotify session.
///
/// After instantiating, you need to login via [Session::connect].
/// You can either implement the whole playback logic yourself by using
/// this structs interface directly or hand it to a
/// `Player`.
///
/// *Note*: [Session] instances cannot yet be reused once invalidated.
///
/// Nanofy: perder el enlace con el punto de acceso ya no invalida la sesión. El enlace se
/// renueva dentro de la misma sesión ([Session::renew_link]) y solo [Session::shutdown], un
/// cierre deliberado, la deja inservible.
#[derive(Clone)]
pub struct Session(Arc<SessionInternal>);

impl Session {
    pub fn new(config: SessionConfig, cache: Option<Cache>) -> Self {
        let http_client = HttpClient::new(config.proxy.as_ref());

        debug!("new Session");

        let session_data = SessionData {
            client_id: config.client_id.clone(),
            // can be any guid, doesn't need to be simple
            session_id: Uuid::new_v4().as_simple().to_string(),
            ..SessionData::default()
        };

        Self(Arc::new(SessionInternal {
            config,
            data: RwLock::new(session_data),
            http_client,
            link: std::sync::Mutex::new(Link {
                slot: LinkSlot::new(Instant::now()),
                tx: None,
                tasks: Vec::new(),
            }),
            link_watch: watch::Sender::new(LinkStatus::initial()),
            renew_lock: tokio::sync::Mutex::new(()),
            born: Instant::now(),
            rx_at_us: AtomicU64::new(0),
            cache: cache.map(Arc::new),
            apresolver: OnceLock::new(),
            audio_key: OnceLock::new(),
            channel: OnceLock::new(),
            mercury: OnceLock::new(),
            dealer: OnceLock::new(),
            spclient: OnceLock::new(),
            token_provider: OnceLock::new(),
            login5: OnceLock::new(),
            handle: tokio::runtime::Handle::current(),
        }))
    }

    async fn connect_inner(
        &self,
        access_point: &SocketAddress,
        credentials: Credentials,
    ) -> Result<(Credentials, Transport), Error> {
        const MAX_RETRIES: u8 = 1;
        let mut transport = connection::connect_with_retry(
            &access_point.0,
            access_point.1,
            self.config().proxy.as_ref(),
            MAX_RETRIES,
        )
        .await?;
        let mut reusable_credentials = connection::authenticate(
            &mut transport,
            credentials.clone(),
            &self.config().device_id,
        )
        .await?;

        // Might be able to remove this once keymaster is replaced with login5.
        if credentials.auth_type == AuthenticationType::AUTHENTICATION_SPOTIFY_TOKEN {
            trace!(
                "Reconnect using stored credentials as token authed sessions cannot use keymaster."
            );
            transport = connection::connect_with_retry(
                &access_point.0,
                access_point.1,
                self.config().proxy.as_ref(),
                MAX_RETRIES,
            )
            .await?;
            reusable_credentials = connection::authenticate(
                &mut transport,
                reusable_credentials.clone(),
                &self.config().device_id,
            )
            .await?;
        }

        Ok((reusable_credentials, transport))
    }

    /// Abre un enlace con un punto de acceso y se autentica: prueba los AP que da el resolvedor
    /// (cada llamada saca el siguiente) hasta que uno acepta, y guarda las credenciales
    /// reutilizables que devuelve.
    async fn open_link(
        &self,
        credentials: Credentials,
        store_credentials: bool,
    ) -> Result<Transport, Error> {
        // There currently happen to be 6 APs but anything will do to avoid an infinite loop.
        const MAX_AP_TRIES: u8 = 6;
        let mut num_ap_tries = 0;
        let (reusable_credentials, transport) = loop {
            let ap = self.apresolver().resolve("accesspoint").await?;
            info!("Connecting to AP \"{}:{}\"", ap.0, ap.1);
            match self.connect_inner(&ap, credentials.clone()).await {
                Ok(ct) => break ct,
                Err(e) => {
                    num_ap_tries += 1;
                    if MAX_AP_TRIES == num_ap_tries {
                        error!("Tried too many access points");
                        return Err(e);
                    }
                    if let Some(AuthenticationError::LoginFailed(ErrorCode::TryAnotherAP)) =
                        e.error.downcast_ref::<AuthenticationError>()
                    {
                        warn!("Instructed to try another access point...");
                        continue;
                    } else if let Some(AuthenticationError::LoginFailed(..)) =
                        e.error.downcast_ref::<AuthenticationError>()
                    {
                        return Err(e);
                    } else {
                        warn!("Try another access point...");
                        continue;
                    }
                }
            }
        };

        let username = reusable_credentials
            .username
            .as_ref()
            .map_or("UNKNOWN", |s| s.as_str());
        info!("Authenticated as '{username}' !");
        self.set_username(username);
        self.set_auth_data(&reusable_credentials.auth_data);
        if let Some(cache) = self.cache() {
            if store_credentials {
                let cred_changed = cache
                    .credentials()
                    .map(|c| c != reusable_credentials)
                    .unwrap_or(true);
                if cred_changed {
                    cache.save_credentials(&reusable_credentials);
                }
            }
        }

        Ok(transport)
    }

    /// Pone en servicio un enlace ya autenticado, en lugar del que hubiera: devuelve su
    /// generación, o `None` si la sesión ya se cerró (el enlace se suelta sin usarse). Lo que iba
    /// por el anterior (su cola, sus tareas, las claves pendientes) se suelta aquí.
    fn attach_link(&self, transport: Transport) -> Option<u64> {
        // This channel serves as a buffer for packets and serializes access to the TcpStream, such
        // that `self.send_packet` can return immediately and needs no additional synchronization.
        let (tx, rx) = mpsc::unbounded_channel();
        let (sink, stream) = transport.split();
        let sender_task = UnboundedReceiverStream::new(rx).map(Ok).forward(sink);
        // Las tareas se lanzan con el cerrojo tomado: una pérdida que vean enseguida no puede
        // adelantarse a que este enlace quede apuntado como el actual.
        let attached = self.lock_link(|link| {
            // La sesión se cerró mientras se abría este enlace. Se mira con el cerrojo tomado:
            // `shutdown` marca la sesión antes de tomarlo para cerrar el enlace, así que o cierra
            // este ya puesto o aquí se ve la marca. Mirarlo antes, sin el cerrojo, dejaba un
            // hueco en el que el enlace nuevo quedaba en servicio, con sus tareas, en una sesión
            // ya cerrada que nadie iba a volver a cerrar.
            if self.is_invalid() {
                return None;
            }
            // Lo pendiente se pidió por el enlace anterior: su respuesta ya no llegará por este.
            // Se saca antes de poner el nuevo y con el cerrojo; después se llevaría también una
            // clave pedida ya por el nuevo.
            let pending = self.audio_key().take_pending();
            let generation = link.slot.attach(Instant::now());
            let weak = self.weak();
            let sender = self.0.handle.spawn(async move {
                if let Err(e) = sender_task.await {
                    error!("{e}");
                    if let Some(session) = weak.try_upgrade() {
                        session.link_lost(generation, LinkLoss::WriteError);
                    }
                }
            });
            let weak = self.weak();
            let dispatch = self.0.handle.spawn(async move {
                let _ = DispatchTask::new(weak, stream, generation).await;
            });
            let old_tx = link.tx.replace(tx);
            let old_tasks = mem::replace(&mut link.tasks, vec![sender, dispatch]);
            Some((generation, old_tx, old_tasks, pending))
        });
        // Con la sesión cerrada no se lanzó nada: soltar aquí el emisor y la lectura cierra la
        // conexión, sin publicar ni registrar nada.
        let (generation, old_tx, old_tasks, pending) = attached?;
        // Soltar las tareas del enlace anterior lo cierra (el sospechoso que seguía abierto).
        drop(old_tx);
        for task in old_tasks {
            task.abort();
        }
        AudioKeyManager::fail_link_lost(pending);

        // Pruebas (`NANOFY_FAULT=ap_drop:<ms>`): el enlace con el AP se corta de golpe ese tiempo
        // después, con lo mismo que un error de lectura (ver `DispatchTask::poll`). Va por enlace:
        // con `ap_drop_every` cada enlace renovado se vuelve a cortar.
        if let Some((after, every)) = crate::fault::ap_drop_after() {
            let weak = self.weak();
            self.0.handle.spawn(async move {
                tokio::time::sleep(after).await;
                if let Some(session) = weak.try_upgrade() {
                    // Solo si sigue siendo el enlace actual: el corte de uno ya sustituido no
                    // gastaría el de `ap_drop` (una vez por proceso) en nada.
                    if !session.is_invalid()
                        && session.link_gen() == generation
                        && crate::fault::ap_drop_claim(every)
                    {
                        error!("NANOFY_FAULT: el enlace con el AP se corta (ConnectionReset)");
                        error!("Connection to server closed.");
                        session.link_lost(generation, LinkLoss::Fault);
                    }
                }
            });
        }

        self.publish(None);
        if generation > 1 {
            info!("[ap] enlace {generation} conectado");
        }
        Some(generation)
    }

    pub async fn connect(
        &self,
        credentials: Credentials,
        store_credentials: bool,
    ) -> Result<(), Error> {
        // Una sola vez por sesión, como antes; los enlaces siguientes los abre `renew_link`.
        if self.link_gen() != 0 {
            return Err(SessionError::NotConnected.into());
        }
        let transport = self.open_link(credentials, store_credentials).await?;
        // Cerrada mientras se conectaba: el enlace no se usa.
        self.attach_link(transport).ok_or(SessionError::NotConnected)?;
        Ok(())
    }

    /// Abre otro enlace con el AP (el siguiente que dé el resolvedor) para sustituir al enlace
    /// `from_gen`, perdido o sospechoso, sin tocar la sesión: Spirc, el dealer, el registro del
    /// dispositivo y el reproductor no se enteran. Devuelve la generación del enlace nuevo.
    pub async fn renew_link(&self, from_gen: u64) -> Result<u64, Error> {
        let _single = self.0.renew_lock.lock().await;
        if self.is_invalid() {
            return Err(SessionError::NotConnected.into());
        }
        let (current, state) = self.lock_link(|l| (l.slot.generation(), l.slot.state()));
        if current > from_gen && state == LinkState::Up {
            // Otro ya lo renovó mientras se esperaba el turno.
            return Ok(current);
        }
        self.lock_link(|l| l.slot.renewing(Instant::now()));
        self.publish(None);
        // Si esta renovación se abandona a medias (la tarea se aborta al cerrar o reconectar) o
        // falla, el enlace no puede quedarse en `Renewing` para siempre: las claves esperarían
        // a un enlace que nadie va a abrir.
        let mut guard = RenewGuard(Some(self));

        let opened = if crate::fault::ap_renew_fail() {
            warn!("NANOFY_FAULT: la renovación del enlace con el AP falla (sin red)");
            Err(io::Error::new(io::ErrorKind::ConnectionRefused, "NANOFY_FAULT: sin red").into())
        } else {
            let credentials = Credentials {
                username: Some(self.username()),
                auth_type: AuthenticationType::AUTHENTICATION_STORED_SPOTIFY_CREDENTIALS,
                auth_data: self.auth_data(),
            };
            match tokio::time::timeout(ap_link::RENEW_TIMEOUT, self.open_link(credentials, true))
                .await
            {
                Ok(r) => r,
                Err(_) => Err(Error::deadline_exceeded(format!(
                    "el AP no completó la conexión en {} s",
                    ap_link::RENEW_TIMEOUT.as_secs()
                ))),
            }
        };
        let transport = opened?;
        // Si se cerró mientras se conectaba, el enlace nuevo no se usa: `attach_link` lo suelta
        // (lo mira con el cerrojo del enlace, sin hueco frente a `shutdown`).
        let generation = self.attach_link(transport).ok_or(SessionError::NotConnected)?;
        guard.0 = None;
        Ok(generation)
    }

    /// Por qué falló una renovación (`renew_link`), para decidir si insistir o reconectar de cero.
    pub fn renew_failure(e: &Error) -> RenewFailure {
        let auth = e.error.downcast_ref::<AuthenticationError>().or_else(|| {
            match e.error.downcast_ref::<SessionError>() {
                Some(SessionError::AuthenticationError(a)) => Some(a),
                _ => None,
            }
        });
        match auth {
            // «Prueba otro AP» solo llega aquí si se agotaron todos: es cosa del servidor, no de
            // las credenciales.
            Some(AuthenticationError::LoginFailed(ErrorCode::TryAnotherAP)) => {
                RenewFailure::Network
            }
            Some(AuthenticationError::LoginFailed(_)) => RenewFailure::Rejected,
            Some(AuthenticationError::Packet(_) | AuthenticationError::Transport) => {
                RenewFailure::Protocol
            }
            // TCP, DNS, el saludo cifrado, el plazo de la renovación…
            None => RenewFailure::Network,
        }
    }

    /// Acceso al enlace actual. El cerrojo no se retiene nunca a través de un `await`; si una
    /// tarea entró en pánico con él, se sigue igual (el estado del enlace no queda a medias).
    fn lock_link<R>(&self, f: impl FnOnce(&mut Link) -> R) -> R {
        let mut link = self.0.link.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut link)
    }

    /// El enlace `generation` se perdió (o es sospechoso) por `why`. Lo de un enlace ya
    /// sustituido, o lo que ya se sabía, no cambia nada.
    pub(crate) fn link_lost(&self, generation: u64, why: LinkLoss) {
        let fatal = why.fatal();
        let taken = self.lock_link(|l| {
            if !l.slot.lost(generation, why, Instant::now()) {
                return None;
            }
            Some(if fatal {
                // Las claves pendientes se sacan con el cerrojo, como en `attach_link`: si no, un
                // enlace nuevo puesto entre medias perdería las que ya se pidieron por él.
                let pending = self.audio_key().take_pending();
                (l.tx.take(), mem::take(&mut l.tasks), pending)
            } else {
                (None, Vec::new(), Vec::new())
            })
        });
        let Some((tx, tasks, pending)) = taken else { return };
        // Abortar desde dentro de la propia tarea de lectura es inocuo: vuelve justo después.
        drop(tx);
        for task in tasks {
            task.abort();
        }
        if fatal {
            // Las claves pendientes no van a llegar: que fallen ya y se reintenten por el nuevo.
            AudioKeyManager::fail_link_lost(pending);
            warn!("[ap] enlace {generation} perdido ({why:?})");
        } else {
            warn!("[ap] sospecha sobre el enlace {generation}: {why:?}");
        }
        self.publish(Some(why));
    }

    /// Publica el estado actual del enlace. Con el cerrojo tomado: dos avisos seguidos no pueden
    /// llegar en el orden contrario y dejar publicado uno viejo.
    fn publish(&self, why: Option<LinkLoss>) {
        self.lock_link(|l| {
            self.0.link_watch.send_replace(l.slot.status(why));
        });
    }

    /// Los cambios de estado del enlace (el actual se lee al momento con `borrow`).
    pub fn link_status(&self) -> watch::Receiver<LinkStatus> {
        self.0.link_watch.subscribe()
    }

    pub fn link_state(&self) -> LinkState {
        self.lock_link(|l| l.slot.state())
    }

    pub fn link_gen(&self) -> u64 {
        self.lock_link(|l| l.slot.generation())
    }

    /// Llegó un paquete del AP (cualquiera): está vivo.
    pub(crate) fn note_inbound(&self) {
        let us = self.0.born.elapsed().as_micros() as u64 + 1;
        self.0.rx_at_us.store(us, Ordering::Relaxed);
    }

    /// Cuándo llegó el último paquete del AP; `None` si aún ninguno.
    pub fn last_inbound(&self) -> Option<Instant> {
        match self.0.rx_at_us.load(Ordering::Relaxed) {
            0 => None,
            us => Some(self.0.born + Duration::from_micros(us - 1)),
        }
    }

    /// La generación de un enlace por el que se puede pedir algo ya, esperando como mucho `wait`
    /// a que lo haya (mientras se renueva). Con la sesión cerrada, al momento `Closed`.
    pub(crate) async fn usable_link(&self, wait: Duration) -> Result<u64, LinkWait> {
        // Primero se escucha y luego se mira: un cambio entre medias despierta igual.
        let mut rx = self.link_status();
        let deadline = TokioInstant::now() + wait;
        loop {
            if self.is_invalid() {
                return Err(LinkWait::Closed);
            }
            if let Some(g) = self.lock_link(|l| l.slot.usable().then(|| l.slot.generation())) {
                return Ok(g);
            }
            match tokio::time::timeout_at(deadline, rx.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) | Err(_) => return Err(LinkWait::TimedOut),
            }
        }
    }

    pub fn apresolver(&self) -> &ApResolver {
        self.0
            .apresolver
            .get_or_init(|| ApResolver::new(self.weak()))
    }

    pub fn audio_key(&self) -> &AudioKeyManager {
        self.0
            .audio_key
            .get_or_init(|| AudioKeyManager::new(self.weak()))
    }

    pub fn channel(&self) -> &ChannelManager {
        self.0
            .channel
            .get_or_init(|| ChannelManager::new(self.weak()))
    }

    pub fn http_client(&self) -> &HttpClient {
        &self.0.http_client
    }

    pub fn mercury(&self) -> &MercuryManager {
        self.0
            .mercury
            .get_or_init(|| MercuryManager::new(self.weak()))
    }

    pub fn dealer(&self) -> &DealerManager {
        self.0
            .dealer
            .get_or_init(|| DealerManager::new(self.weak()))
    }

    pub fn spclient(&self) -> &SpClient {
        self.0.spclient.get_or_init(|| SpClient::new(self.weak()))
    }

    pub fn token_provider(&self) -> &TokenProvider {
        self.0
            .token_provider
            .get_or_init(|| TokenProvider::new(self.weak()))
    }

    pub fn login5(&self) -> &Login5Manager {
        self.0
            .login5
            .get_or_init(|| Login5Manager::new(self.weak()))
    }

    pub fn time_delta(&self) -> i64 {
        self.0
            .data
            .read()
            .expect(SESSION_DATA_POISON_MSG)
            .time_delta
    }

    pub fn spawn<T>(&self, task: T)
    where
        T: Future + Send + 'static,
        T::Output: Send + 'static,
    {
        self.0.handle.spawn(task);
    }

    fn debug_info(&self) {
        debug!(
            "Session strong={} weak={}",
            Arc::strong_count(&self.0),
            Arc::weak_count(&self.0)
        );
    }

    /// ¿Permite la cuenta reproducir aquí? Solo con Premium: Spotify no sirve audio a apps
    /// externas para otras cuentas. Un tipo que aún no se conoce cuenta como permitido.
    ///
    /// Nanofy: antes, con otra cuenta, cerraba el proceso (`exit(1)`): la app desaparecía sin
    /// ningún mensaje. Ahora solo lo anota; el tipo queda en los atributos (`account_type`) y
    /// Nanofy, al verlo (`is_premium`), lo explica y deja seguir usando la biblioteca y la
    /// búsqueda sin reproducir.
    pub fn check_catalogue(attributes: &UserAttributes) -> bool {
        match attributes.get("type") {
            Some(account_type) if account_type != "premium" => {
                error!("librespot does not support {account_type:?} accounts.");
                info!("Please support Spotify and your artists and sign up for a premium account.");
                false
            }
            _ => true,
        }
    }

    /// Tipo de cuenta («premium», «free»…) según Spotify; `None` hasta que llega (justo después
    /// de iniciar sesión en el punto de acceso).
    pub fn account_type(&self) -> Option<String> {
        self.get_user_attribute("type")
    }

    /// La cuenta puede reproducir aquí (ver `check_catalogue`); mientras no se sabe, sí.
    pub fn is_premium(&self) -> bool {
        self.account_type().is_none_or(|t| t == "premium")
    }

    /// Por el enlace actual; sin enlace (perdido, aún sin renovar), `NotConnected`.
    pub fn send_packet(&self, cmd: PacketType, data: Vec<u8>) -> Result<(), Error> {
        let sent = self.lock_link(move |l| l.tx.as_ref().map(|tx| tx.send((cmd as u8, data))));
        match sent {
            Some(Ok(())) => Ok(()),
            _ => Err(SessionError::NotConnected.into()),
        }
    }

    pub fn cache(&self) -> Option<&Arc<Cache>> {
        self.0.cache.as_ref()
    }

    pub fn config(&self) -> &SessionConfig {
        &self.0.config
    }

    // This clones a fairly large struct, so use a specific getter or setter unless
    // you need more fields at once, in which case this can spare multiple `read`
    // locks.
    pub fn user_data(&self) -> UserData {
        self.0
            .data
            .read()
            .expect(SESSION_DATA_POISON_MSG)
            .user_data
            .clone()
    }

    pub fn session_id(&self) -> String {
        self.0
            .data
            .read()
            .expect(SESSION_DATA_POISON_MSG)
            .session_id
            .clone()
    }

    pub fn set_session_id(&self, session_id: &str) {
        session_id.clone_into(
            &mut self
                .0
                .data
                .write()
                .expect(SESSION_DATA_POISON_MSG)
                .session_id,
        );
    }

    pub fn device_id(&self) -> &str {
        &self.config().device_id
    }

    pub fn client_id(&self) -> String {
        self.0
            .data
            .read()
            .expect(SESSION_DATA_POISON_MSG)
            .client_id
            .clone()
    }

    pub fn set_client_id(&self, client_id: &str) {
        client_id.clone_into(
            &mut self
                .0
                .data
                .write()
                .expect(SESSION_DATA_POISON_MSG)
                .client_id,
        );
    }

    pub fn client_name(&self) -> String {
        self.0
            .data
            .read()
            .expect(SESSION_DATA_POISON_MSG)
            .client_name
            .clone()
    }

    pub fn set_client_name(&self, client_name: &str) {
        client_name.clone_into(
            &mut self
                .0
                .data
                .write()
                .expect(SESSION_DATA_POISON_MSG)
                .client_name,
        );
    }

    pub fn client_brand_name(&self) -> String {
        self.0
            .data
            .read()
            .expect(SESSION_DATA_POISON_MSG)
            .client_brand_name
            .clone()
    }

    pub fn set_client_brand_name(&self, client_brand_name: &str) {
        client_brand_name.clone_into(
            &mut self
                .0
                .data
                .write()
                .expect(SESSION_DATA_POISON_MSG)
                .client_brand_name,
        );
    }

    pub fn client_model_name(&self) -> String {
        self.0
            .data
            .read()
            .expect(SESSION_DATA_POISON_MSG)
            .client_model_name
            .clone()
    }

    pub fn set_client_model_name(&self, client_model_name: &str) {
        client_model_name.clone_into(
            &mut self
                .0
                .data
                .write()
                .expect(SESSION_DATA_POISON_MSG)
                .client_model_name,
        );
    }

    pub fn connection_id(&self) -> String {
        self.0
            .data
            .read()
            .expect(SESSION_DATA_POISON_MSG)
            .connection_id
            .clone()
    }

    pub fn set_connection_id(&self, connection_id: &str) {
        connection_id.clone_into(
            &mut self
                .0
                .data
                .write()
                .expect(SESSION_DATA_POISON_MSG)
                .connection_id,
        );
    }

    pub fn username(&self) -> String {
        self.0
            .data
            .read()
            .expect(SESSION_DATA_POISON_MSG)
            .user_data
            .canonical_username
            .clone()
    }

    pub fn set_username(&self, username: &str) {
        username.clone_into(
            &mut self
                .0
                .data
                .write()
                .expect(SESSION_DATA_POISON_MSG)
                .user_data
                .canonical_username,
        );
    }

    pub fn auth_data(&self) -> Vec<u8> {
        self.0
            .data
            .read()
            .expect(SESSION_DATA_POISON_MSG)
            .auth_data
            .clone()
    }

    pub fn set_auth_data(&self, auth_data: &[u8]) {
        auth_data.clone_into(
            &mut self
                .0
                .data
                .write()
                .expect(SESSION_DATA_POISON_MSG)
                .auth_data,
        );
    }

    pub fn country(&self) -> String {
        self.0
            .data
            .read()
            .expect(SESSION_DATA_POISON_MSG)
            .user_data
            .country
            .clone()
    }

    pub fn filter_explicit_content(&self) -> bool {
        match self.get_user_attribute("filter-explicit-content") {
            Some(value) => matches!(&*value, "1"),
            None => false,
        }
    }

    pub fn autoplay(&self) -> bool {
        if let Some(live) = self.0.data.read().expect(SESSION_DATA_POISON_MSG).autoplay_live {
            return live;
        }
        if let Some(overide) = self.config().autoplay {
            return overide;
        }

        match self.get_user_attribute("autoplay") {
            Some(value) => matches!(&*value, "1"),
            None => false,
        }
    }

    /// Nanofy: activa o desactiva el autoplay sin reiniciar la sesión (sustituye al de la
    /// configuración desde ahora).
    pub fn set_autoplay(&self, on: bool) {
        self.0.data.write().expect(SESSION_DATA_POISON_MSG).autoplay_live = Some(on);
    }

    pub fn set_user_attribute(&self, key: &str, value: &str) -> Option<String> {
        let mut dummy_attributes = UserAttributes::new();
        dummy_attributes.insert(key.to_owned(), value.to_owned());
        Self::check_catalogue(&dummy_attributes);

        self.0
            .data
            .write()
            .expect(SESSION_DATA_POISON_MSG)
            .user_data
            .attributes
            .insert(key.to_owned(), value.to_owned())
    }

    pub fn set_user_attributes(&self, attributes: UserAttributes) {
        Self::check_catalogue(&attributes);

        self.0
            .data
            .write()
            .expect(SESSION_DATA_POISON_MSG)
            .user_data
            .attributes
            .extend(attributes)
    }

    pub fn get_user_attribute(&self, key: &str) -> Option<String> {
        self.0
            .data
            .read()
            .expect(SESSION_DATA_POISON_MSG)
            .user_data
            .attributes
            .get(key)
            .cloned()
    }

    fn weak(&self) -> SessionWeak {
        SessionWeak(Arc::downgrade(&self.0))
    }

    pub fn shutdown(&self) {
        debug!("Shutdown: Invalidating session");
        self.0.data.write().expect(SESSION_DATA_POISON_MSG).invalid = true;
        // Nanofy: el enlace con el AP se cierra aquí, y quien espera uno para pedir una clave
        // (`usable_link`) se despierta y ve la sesión cerrada.
        let (tx, tasks) = self.lock_link(|l| {
            l.slot.close();
            (l.tx.take(), mem::take(&mut l.tasks))
        });
        drop(tx);
        for task in tasks {
            task.abort();
        }
        self.publish(None);
        self.mercury().shutdown();
        self.channel().shutdown();
        // Nanofy: también las claves de audio en espera (ver `AudioKeyManager::shutdown`).
        self.audio_key().shutdown();
    }

    pub fn is_invalid(&self) -> bool {
        self.0.data.read().expect(SESSION_DATA_POISON_MSG).invalid
    }
}

/// Una renovación en marcha (`Session::renew_link`): si no termina con el enlace nuevo en
/// servicio (falla, o se abandona a medias), el enlace vuelve a lo que había y se publica.
struct RenewGuard<'a>(Option<&'a Session>);

impl Drop for RenewGuard<'_> {
    fn drop(&mut self) {
        if let Some(session) = self.0 {
            session.lock_link(|l| l.slot.renew_failed(Instant::now()));
            session.publish(None);
        }
    }
}

#[derive(Clone)]
pub struct SessionWeak(Weak<SessionInternal>);

impl SessionWeak {
    fn try_upgrade(&self) -> Option<Session> {
        self.0.upgrade().map(Session)
    }

    pub(crate) fn upgrade(&self) -> Session {
        self.try_upgrade()
            .expect("session was dropped and so should have this component")
    }
}

impl Drop for SessionInternal {
    fn drop(&mut self) {
        debug!("drop Session");
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq)]
enum KeepAliveState {
    #[default]
    // Expecting a Ping from the server, either after startup or after a PongAck.
    ExpectingPing,

    // We need to send a Pong at the given time.
    PendingPong,

    // We just sent a Pong and wait for it be ACK'd.
    ExpectingPongAck,
}

const INITIAL_PING_TIMEOUT: TokioDuration = TokioDuration::from_secs(20);
const PING_TIMEOUT: TokioDuration = TokioDuration::from_secs(80); // 60s expected + 20s buffer
const PONG_DELAY: TokioDuration = TokioDuration::from_secs(60);
const PONG_ACK_TIMEOUT: TokioDuration = TokioDuration::from_secs(20);

impl KeepAliveState {
    fn debug(&self, sleep: &Sleep) {
        let delay = sleep
            .deadline()
            .checked_duration_since(TokioInstant::now())
            .map(|t| t.as_secs_f64())
            .unwrap_or(f64::INFINITY);

        trace!("keep-alive state: {self:?}, timeout in {delay:.1}");
    }
}

pin_project! {
    struct DispatchTask<S>
    where
        S: TryStream<Ok = (u8, Bytes)>
    {
        session: SessionWeak,
        keep_alive_state: KeepAliveState,
        #[pin]
        stream: S,
        #[pin]
        timeout: Sleep,
        // Nanofy: el enlace que lee; sus pérdidas llevan esta generación (`Session::link_lost`).
        generation: u64,
        // Para las pruebas (`NANOFY_FAULT=ap_silent`): qué enlace es y desde cuándo.
        fault_id: u64,
        started: std::time::Instant,
    }

    impl<S> PinnedDrop for DispatchTask<S>
    where
        S: TryStream<Ok = (u8, Bytes)>
    {
        fn drop(_this: Pin<&mut Self>) {
            debug!("drop Dispatch");
        }
    }
}

impl<S> DispatchTask<S>
where
    S: TryStream<Ok = (u8, Bytes)>,
{
    fn new(session: SessionWeak, stream: S, generation: u64) -> Self {
        Self {
            session,
            keep_alive_state: KeepAliveState::ExpectingPing,
            stream,
            timeout: sleep(INITIAL_PING_TIMEOUT),
            generation,
            fault_id: crate::fault::next_session_id(),
            started: std::time::Instant::now(),
        }
    }

    fn dispatch(
        mut self: Pin<&mut Self>,
        session: &Session,
        cmd: u8,
        data: Bytes,
    ) -> Result<(), Error> {
        use KeepAliveState::*;
        use PacketType::*;

        let packet_type = FromPrimitive::from_u8(cmd);
        let cmd = match packet_type {
            Some(cmd) => cmd,
            None => {
                trace!("Ignoring unknown packet {cmd:x}");
                return Err(SessionError::Packet(cmd).into());
            }
        };

        match packet_type {
            Some(Ping) => {
                trace!("Received Ping");
                if self.keep_alive_state != ExpectingPing {
                    warn!("Received unexpected Ping from server")
                }
                let mut this = self.as_mut().project();
                *this.keep_alive_state = PendingPong;
                this.timeout
                    .as_mut()
                    .reset(TokioInstant::now() + PONG_DELAY);
                this.keep_alive_state.debug(&this.timeout);

                let server_timestamp = BigEndian::read_u32(data.as_ref()) as i64;
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or(Duration::ZERO)
                    .as_secs() as i64;
                {
                    let mut data = session.0.data.write().expect(SESSION_DATA_POISON_MSG);
                    data.time_delta = server_timestamp.saturating_sub(timestamp);
                }

                session.debug_info();

                Ok(())
            }
            Some(PongAck) => {
                trace!("Received PongAck");
                if self.keep_alive_state != ExpectingPongAck {
                    warn!("Received unexpected PongAck from server")
                }
                let mut this = self.as_mut().project();
                *this.keep_alive_state = ExpectingPing;
                this.timeout
                    .as_mut()
                    .reset(TokioInstant::now() + PING_TIMEOUT);
                this.keep_alive_state.debug(&this.timeout);

                Ok(())
            }
            Some(CountryCode) => {
                let country = String::from_utf8(data.as_ref().to_owned())?;
                info!("Country: {country:?}");
                session
                    .0
                    .data
                    .write()
                    .expect(SESSION_DATA_POISON_MSG)
                    .user_data
                    .country = country;
                Ok(())
            }
            Some(StreamChunkRes) | Some(ChannelError) => session.channel().dispatch(cmd, data),
            Some(AesKey) | Some(AesKeyError) => session.audio_key().dispatch(cmd, data),
            Some(MercuryReq) | Some(MercurySub) | Some(MercuryUnsub) | Some(MercuryEvent) => {
                session.mercury().dispatch(cmd, data)
            }
            Some(ProductInfo) => {
                let data = std::str::from_utf8(&data)?;
                let mut reader = quick_xml::Reader::from_str(data);

                let mut buf = Vec::new();
                let mut current_element = String::new();
                let mut user_attributes: UserAttributes = HashMap::new();

                loop {
                    match reader.read_event_into(&mut buf) {
                        Ok(Event::Start(ref element)) => {
                            std::str::from_utf8(element)?.clone_into(&mut current_element)
                        }
                        Ok(Event::End(_)) => {
                            current_element = String::new();
                        }
                        Ok(Event::Text(ref value)) => {
                            if !current_element.is_empty() {
                                let _ = user_attributes.insert(
                                    current_element.clone(),
                                    value.xml_content()?.to_string(),
                                );
                            }
                        }
                        Ok(Event::Eof) => break,
                        Ok(_) => (),
                        Err(e) => warn!(
                            "Error parsing XML at position {}: {:?}",
                            reader.buffer_position(),
                            e
                        ),
                    }
                }

                trace!("Received product info: {user_attributes:#?}");
                Session::check_catalogue(&user_attributes);

                session
                    .0
                    .data
                    .write()
                    .expect(SESSION_DATA_POISON_MSG)
                    .user_data
                    .attributes = user_attributes;
                Ok(())
            }
            Some(SecretBlock)
            | Some(LegacyWelcome)
            | Some(UnknownDataAllZeros)
            | Some(LicenseVersion) => Ok(()),
            _ => {
                trace!("Ignoring {cmd:?} packet with data {data:#?}");
                Err(SessionError::Packet(cmd as u8).into())
            }
        }
    }
}

impl<S> Future for DispatchTask<S>
where
    S: TryStream<Ok = (u8, Bytes), Error = std::io::Error>,
    <S as TryStream>::Ok: std::fmt::Debug,
{
    type Output = Result<(), S::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        use KeepAliveState::*;

        let session = match self.session.try_upgrade() {
            Some(session) => session,
            None => return Poll::Ready(Ok(())),
        };

        // Process all messages that are immediately ready
        loop {
            match self.as_mut().project().stream.try_poll_next(cx) {
                Poll::Ready(Some(Ok((cmd, data)))) => {
                    // Pruebas (`NANOFY_FAULT=ap_silent`): lo que llega se pierde, como con un
                    // enlace muerto sin aviso (no hay Ping, ni respuestas de claves).
                    if crate::fault::ap_silent(self.fault_id, self.started) {
                        continue;
                    }
                    // Nanofy: el AP está vivo (cualquier paquete vale, también una clave negada;
                    // ver `ap_link::key_timeout_suspect`).
                    session.note_inbound();
                    let result = self.as_mut().dispatch(&session, cmd, data);
                    if let Err(e) = result {
                        debug!("could not dispatch command: {e}");
                    }
                }
                // Nanofy: se pierde el enlace, no la sesión; se renueva (`Session::renew_link`).
                Poll::Ready(None) => {
                    warn!("Connection to server closed.");
                    session.link_lost(self.generation, LinkLoss::Eof);
                    return Poll::Ready(Ok(()));
                }
                Poll::Ready(Some(Err(e))) => {
                    error!("Connection to server closed.");
                    session.link_lost(self.generation, LinkLoss::ReadError);
                    return Poll::Ready(Err(e));
                }
                Poll::Pending => break,
            }
        }

        // Handle the keep-alive sequence, returning an error when we haven't received a
        // Ping/PongAck for too long.
        //
        // The expected keepalive sequence is
        // - Server: Ping
        // - wait 60s
        // - Client: Pong
        // - Server: PongAck
        // - wait 60s
        // - repeat
        //
        // This means that we silently lost connection to Spotify servers if
        // - we don't receive Ping immediately after connecting,
        // - we don't receive a Ping 60s after the last PongAck or
        // - we don't receive a PongAck immediately after our Pong.
        //
        // Currently, we add a safety margin of 20s to these expected deadlines.
        let mut this = self.as_mut().project();
        if let Poll::Ready(()) = this.timeout.as_mut().poll(cx) {
            match this.keep_alive_state {
                ExpectingPing | ExpectingPongAck => {
                    // Nanofy: el enlace se da por perdido y se renueva (una vez por enlace: lo
                    // repetido no cambia nada).
                    session.link_lost(*this.generation, LinkLoss::KeepAlive);
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "session lost connection to server ({:?})",
                            this.keep_alive_state
                        ),
                    )));
                }
                PendingPong => {
                    trace!("Sending Pong");
                    // TODO: Ideally, this should flush the `Framed<TcpStream> as Sink`
                    // before starting the timeout.
                    let _ = session.send_packet(PacketType::Pong, vec![0, 0, 0, 0]);
                    *this.keep_alive_state = ExpectingPongAck;
                    this.timeout
                        .as_mut()
                        .reset(TokioInstant::now() + PONG_ACK_TIMEOUT);
                    this.keep_alive_state.debug(&this.timeout);
                }
            }
        }

        Poll::Pending
    }
}
