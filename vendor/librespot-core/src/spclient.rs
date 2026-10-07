use std::{
    cell::Cell,
    fmt::Write,
    sync::{LazyLock, Mutex, MutexGuard},
    time::{Duration, Instant, SystemTime},
};

use crate::config::{OS, os_version};
use crate::{
    Error, FileId, SpotifyId, SpotifyUri,
    apresolve::SocketAddress,
    config::SessionConfig,
    dealer::protocol::TransferOptions,
    error::ErrorKind,
    meta_cache::{self, MetaCache, MetaKey, SeedTrust},
    request_policy,
    protocol::{
        autoplay_context_request::AutoplayContextRequest,
        clienttoken_http::{
            ChallengeAnswer, ChallengeType, ClientTokenRequest, ClientTokenRequestType,
            ClientTokenResponse, ClientTokenResponseType,
        },
        connect::PutStateRequest,
        context::Context,
        extended_metadata::BatchedEntityRequest,
        extended_metadata::{BatchedExtensionResponse, EntityRequest, ExtensionQuery},
        extension_kind::ExtensionKind,
    },
    token::Token,
    util,
    version::spotify_semantic_version,
};
use bytes::Bytes;
use data_encoding::HEXUPPER_PERMISSIVE;
use futures_util::future::IntoStream;
use http::{Uri, header::HeaderValue};
use hyper::{
    HeaderMap, Method, Request,
    header::{ACCEPT, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HeaderName, RANGE},
};
use hyper_util::client::legacy::ResponseFuture;
use protobuf::{Enum, EnumOrUnknown, Message, MessageFull};
use rand::RngCore;
use serde::Serialize;
use sysinfo::System;
use thiserror::Error;

component! {
    SpClient : SpClientInner {
        accesspoint: Option<SocketAddress> = None,
        strategy: RequestStrategy = RequestStrategy::default(),
        client_token: Option<Token> = None,
    }
}

pub type SpClientResult = Result<Bytes, Error>;

#[allow(clippy::declare_interior_mutable_const)]
pub const CLIENT_TOKEN: HeaderName = HeaderName::from_static("client-token");
#[allow(clippy::declare_interior_mutable_const)]
const CONNECTION_ID: HeaderName = HeaderName::from_static("x-spotify-connection-id");

/// Metadatos ya pedidos o sembrados por Nanofy, de todo el proceso (ver `meta_cache`).
static META: LazyLock<Mutex<MetaCache<Bytes>>> =
    LazyLock::new(|| Mutex::new(MetaCache::new(meta_cache::CAP_BYTES, meta_cache::TTL)));
/// ¿Se pueden servir las semillas? (ver `SeedTrust`).
static SEED_TRUST: Mutex<SeedTrust> = Mutex::new(SeedTrust::Unverified);

fn meta_cache() -> MutexGuard<'static, MetaCache<Bytes>> {
    META.lock().unwrap_or_else(|e| e.into_inner())
}

/// Metadatos que se guardan: los que pide el reproductor al cargar (ver `get_metadata`).
fn cacheable(kind: ExtensionKind) -> bool {
    matches!(kind, ExtensionKind::TRACK_V4 | ExtensionKind::EPISODE_V4)
}

fn seed_trust() -> SeedTrust {
    *SEED_TRUST.lock().unwrap_or_else(|e| e.into_inner())
}

thread_local! {
    /// De dónde salieron los últimos metadatos que pidió este hilo («hit», «seed», «check» o
    /// «miss»): el reproductor lo pone en su marca de `ttfs` (cada carga va en su propio hilo).
    static META_SOURCE: Cell<&'static str> = const { Cell::new("miss") };
}

fn set_metadata_source(source: &'static str) {
    META_SOURCE.with(|s| s.set(source));
}

/// De dónde salieron los últimos metadatos que pidió este hilo (ver `META_SOURCE`).
pub fn metadata_source() -> &'static str {
    META_SOURCE.with(|s| s.get())
}

/// Estado de la caché de metadatos para el modo de control: entradas, bytes, semillas y si las
/// semillas se usan.
pub fn metadata_cache_stats() -> (usize, usize, usize, &'static str) {
    let cache = meta_cache();
    (cache.len(), cache.bytes(), cache.seeded(), seed_trust().name())
}

/// Olvida los metadatos guardados (al cerrar sesión: otra cuenta puede ser de otro país).
pub fn forget_cached_metadata() {
    let mut cache = meta_cache();
    *cache = MetaCache::new(meta_cache::CAP_BYTES, meta_cache::TTL);
}

const NO_METRICS_AND_SALT: RequestOptions = RequestOptions {
    metrics: false,
    salt: false,
    base_url: None,
    fast: false,
};

/// Los metadatos de una canción que pide el reproductor al cargarla: van por la misma ruta que
/// los lotes de 500 de Nanofy, pero son una respuesta pequeña en el camino del primer sonido.
const ONE_ITEM_METADATA: RequestOptions = RequestOptions {
    metrics: true,
    salt: true,
    base_url: None,
    fast: true,
};

#[derive(Debug, Error)]
pub enum SpClientError {
    #[error("missing attribute {0}")]
    Attribute(String),
    #[error("expected data but received none")]
    NoData,
    #[error("expected an entry to exist in {0}")]
    ExpectedEntry(&'static str),
}

impl From<SpClientError> for Error {
    fn from(err: SpClientError) -> Self {
        Self::failed_precondition(err)
    }
}

#[derive(Copy, Clone, Debug)]
pub enum RequestStrategy {
    TryTimes(usize),
    Infinitely,
}

impl Default for RequestStrategy {
    fn default() -> Self {
        RequestStrategy::TryTimes(10)
    }
}

pub struct RequestOptions {
    metrics: bool,
    salt: bool,
    base_url: Option<&'static str>,
    /// Plazo corto (`request_policy::FAST`) aunque la ruta sea la de algo que puede ser grande.
    fast: bool,
}

impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            metrics: true,
            salt: true,
            base_url: None,
            fast: false,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct TransferRequest {
    pub transfer_options: TransferOptions,
}

impl SpClient {
    pub fn set_strategy(&self, strategy: RequestStrategy) {
        self.lock(|inner| inner.strategy = strategy)
    }

    pub async fn flush_accesspoint(&self) {
        self.lock(|inner| inner.accesspoint = None)
    }

    pub async fn get_accesspoint(&self) -> Result<SocketAddress, Error> {
        // Memoize the current access point.
        let ap = self.lock(|inner| inner.accesspoint.clone());
        let tuple = match ap {
            Some(tuple) => tuple,
            None => {
                let tuple = self.session().apresolver().resolve("spclient").await?;
                self.lock(|inner| inner.accesspoint = Some(tuple.clone()));
                info!(
                    "Resolved \"{}:{}\" as spclient access point",
                    tuple.0, tuple.1
                );
                tuple
            }
        };
        Ok(tuple)
    }

    pub async fn base_url(&self) -> Result<String, Error> {
        let ap = self.get_accesspoint().await?;
        Ok(format!("https://{}:{}", ap.0, ap.1))
    }

    async fn client_token_request<M: Message>(&self, message: &M) -> Result<Bytes, Error> {
        let body = message.write_to_bytes()?;

        let request = Request::builder()
            .method(&Method::POST)
            .uri("https://clienttoken.spotify.com/v1/clienttoken")
            .header(ACCEPT, HeaderValue::from_static("application/x-protobuf"))
            .body(body.into())?;

        self.session().http_client().request_body(request).await
    }

    pub async fn client_token(&self) -> Result<String, Error> {
        let client_token = self.lock(|inner| {
            if let Some(token) = &inner.client_token {
                if token.is_expired() {
                    inner.client_token = None;
                }
            }
            inner.client_token.clone()
        });

        if let Some(client_token) = client_token {
            return Ok(client_token.access_token);
        }

        // El de la sesión anterior, si sigue vigente: ahorra una ida y vuelta al abrir.
        if let Some((access_token, expires)) = self.session().cache().and_then(|c| c.token("client")) {
            let now = SystemTime::now();
            self.lock(|inner| {
                inner.client_token = Some(Token {
                    access_token: access_token.clone(),
                    expires_in: expires.duration_since(now).unwrap_or_default(),
                    token_type: "client-token".to_string(),
                    scopes: vec![],
                    timestamp: now,
                })
            });
            return Ok(access_token);
        }

        debug!("Client token unavailable or expired, requesting new token.");

        let mut request = ClientTokenRequest::new();
        request.request_type = ClientTokenRequestType::REQUEST_CLIENT_DATA_REQUEST.into();

        let client_data = request.mut_client_data();

        client_data.client_version = spotify_semantic_version();

        // Current state of affairs: keymaster ID works on all tested platforms, but may be phased out,
        // so it seems a good idea to mimick the real clients. `self.session().client_id()` returns the
        // ID of the client that last connected, but requesting a client token with this ID only works
        // on macOS and Windows. On Android and iOS we can send a platform-specific client ID and are
        // then presented with a hash cash challenge. On Linux, we have to pass the old keymaster ID.
        // We delegate most of this logic to `SessionConfig`.
        let os = OS;
        let client_id = match os {
            "macos" | "windows" => self.session().client_id(),
            os => SessionConfig::default_for_os(os).client_id,
        };
        client_data.client_id = client_id;

        let connectivity_data = client_data.mut_connectivity_sdk_data();
        connectivity_data.device_id = self.session().device_id().to_string();

        let platform_data = connectivity_data
            .platform_specific_data
            .mut_or_insert_default();

        let os_version = os_version();
        let kernel_version = System::kernel_version().unwrap_or_else(|| String::from("0"));

        match os {
            "windows" => {
                let os_version = os_version.parse::<f32>().unwrap_or(10.) as i32;
                let kernel_version = kernel_version.parse::<i32>().unwrap_or(21370);

                let (pe, image_file) = match std::env::consts::ARCH {
                    "arm" => (448, 452),
                    "aarch64" => (43620, 452),
                    "x86_64" => (34404, 34404),
                    _ => (332, 332), // x86
                };

                let windows_data = platform_data.mut_desktop_windows();
                windows_data.os_version = os_version;
                windows_data.os_build = kernel_version;
                windows_data.platform_id = 2;
                windows_data.unknown_value_6 = 9;
                windows_data.image_file_machine = image_file;
                windows_data.pe_machine = pe;
                windows_data.unknown_value_10 = true;
            }
            "ios" => {
                let ios_data = platform_data.mut_ios();
                ios_data.user_interface_idiom = 0;
                ios_data.target_iphone_simulator = false;
                ios_data.hw_machine = "iPhone14,5".to_string();
                ios_data.system_version = os_version;
            }
            "android" => {
                let android_data = platform_data.mut_android();
                android_data.android_version = os_version;
                android_data.api_version = 31;
                "Pixel".clone_into(&mut android_data.device_name);
                "GF5KQ".clone_into(&mut android_data.model_str);
                "Google".clone_into(&mut android_data.vendor);
            }
            "macos" => {
                let macos_data = platform_data.mut_desktop_macos();
                macos_data.system_version = os_version;
                macos_data.hw_model = "iMac21,1".to_string();
                macos_data.compiled_cpu_type = std::env::consts::ARCH.to_string();
            }
            _ => {
                let linux_data = platform_data.mut_desktop_linux();
                linux_data.system_name = "Linux".to_string();
                linux_data.system_release = kernel_version;
                linux_data.system_version = os_version;
                linux_data.hardware = std::env::consts::ARCH.to_string();
            }
        }

        let mut response = self.client_token_request(&request).await?;
        let mut count = 0;
        const MAX_TRIES: u8 = 3;

        let token_response = loop {
            count += 1;

            let message = ClientTokenResponse::parse_from_bytes(&response)?;

            match ClientTokenResponseType::from_i32(message.response_type.value()) {
                // depending on the platform, you're either given a token immediately
                // or are presented a hash cash challenge to solve first
                Some(ClientTokenResponseType::RESPONSE_GRANTED_TOKEN_RESPONSE) => {
                    debug!("Received a granted token");
                    break message;
                }
                Some(ClientTokenResponseType::RESPONSE_CHALLENGES_RESPONSE) => {
                    debug!("Received a hash cash challenge, solving...");

                    let challenges = message.challenges().clone();
                    let state = challenges.state;
                    if let Some(challenge) = challenges.challenges.first() {
                        let hash_cash_challenge = challenge.evaluate_hashcash_parameters();

                        let ctx = vec![];
                        let prefix = HEXUPPER_PERMISSIVE
                            .decode(hash_cash_challenge.prefix.as_bytes())
                            .map_err(|e| {
                                Error::failed_precondition(format!(
                                    "Unable to decode hash cash challenge: {e}"
                                ))
                            })?;
                        let length = hash_cash_challenge.length;

                        let mut suffix = [0u8; 0x10];
                        let answer = util::solve_hash_cash(&ctx, &prefix, length, &mut suffix);

                        match answer {
                            Ok(_) => {
                                // the suffix must be in uppercase
                                let suffix = HEXUPPER_PERMISSIVE.encode(&suffix);

                                let mut answer_message = ClientTokenRequest::new();
                                answer_message.request_type =
                                    ClientTokenRequestType::REQUEST_CHALLENGE_ANSWERS_REQUEST
                                        .into();

                                let challenge_answers = answer_message.mut_challenge_answers();

                                let mut challenge_answer = ChallengeAnswer::new();
                                challenge_answer.mut_hash_cash().suffix = suffix;
                                challenge_answer.ChallengeType =
                                    ChallengeType::CHALLENGE_HASH_CASH.into();

                                challenge_answers.state = state.to_string();
                                challenge_answers.answers.push(challenge_answer);

                                trace!("Answering hash cash challenge");
                                match self.client_token_request(&answer_message).await {
                                    Ok(token) => {
                                        response = token;
                                        continue;
                                    }
                                    Err(e) => {
                                        trace!("Answer not accepted {count}/{MAX_TRIES}: {e}");
                                    }
                                }
                            }
                            Err(e) => trace!(
                                "Unable to solve hash cash challenge {count}/{MAX_TRIES}: {e}"
                            ),
                        }

                        if count < MAX_TRIES {
                            response = self.client_token_request(&request).await?;
                        } else {
                            return Err(Error::failed_precondition(format!(
                                "Unable to solve any of {MAX_TRIES} hash cash challenges"
                            )));
                        }
                    } else {
                        return Err(Error::failed_precondition("No challenges found"));
                    }
                }

                Some(unknown) => {
                    return Err(Error::unimplemented(format!(
                        "Unknown client token response type: {unknown:?}"
                    )));
                }
                None => return Err(Error::failed_precondition("No client token response type")),
            }
        };

        let granted_token = token_response.granted_token();
        let access_token = granted_token.token.to_owned();

        self.lock(|inner| {
            let client_token = Token {
                access_token: access_token.clone(),
                expires_in: Duration::from_secs(
                    granted_token
                        .refresh_after_seconds
                        .try_into()
                        .unwrap_or(7200),
                ),
                token_type: "client-token".to_string(),
                scopes: granted_token
                    .domains
                    .iter()
                    .map(|d| d.domain.clone())
                    .collect(),
                timestamp: SystemTime::now(),
            };

            inner.client_token = Some(client_token);
        });
        if let Some(cache) = self.session().cache() {
            let secs = granted_token.refresh_after_seconds.try_into().unwrap_or(7200);
            cache.save_token("client", &access_token, SystemTime::now() + Duration::from_secs(secs));
        }

        trace!("Got client token: {granted_token:?}");

        Ok(access_token)
    }

    pub async fn request_with_protobuf<M: Message + MessageFull>(
        &self,
        method: &Method,
        endpoint: &str,
        headers: Option<HeaderMap>,
        message: &M,
    ) -> SpClientResult {
        self.request_with_protobuf_and_options(
            method,
            endpoint,
            headers,
            message,
            &Default::default(),
        )
        .await
    }

    pub async fn request_with_protobuf_and_options<M: Message + MessageFull>(
        &self,
        method: &Method,
        endpoint: &str,
        headers: Option<HeaderMap>,
        message: &M,
        options: &RequestOptions,
    ) -> SpClientResult {
        let body = message.write_to_bytes()?;

        let mut headers = headers.unwrap_or_default();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/x-protobuf"),
        );

        self.request_with_options(method, endpoint, Some(headers), Some(&body), options)
            .await
    }

    pub async fn request_as_json(
        &self,
        method: &Method,
        endpoint: &str,
        headers: Option<HeaderMap>,
        body: Option<&str>,
    ) -> SpClientResult {
        let mut headers = headers.unwrap_or_default();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        self.request(method, endpoint, Some(headers), body.map(|s| s.as_bytes()))
            .await
    }

    pub async fn request(
        &self,
        method: &Method,
        endpoint: &str,
        headers: Option<HeaderMap>,
        body: Option<&[u8]>,
    ) -> SpClientResult {
        self.request_with_options(method, endpoint, headers, body, &Default::default())
            .await
    }

    pub async fn request_with_options(
        &self,
        method: &Method,
        endpoint: &str,
        headers: Option<HeaderMap>,
        body: Option<&[u8]>,
        options: &RequestOptions,
    ) -> SpClientResult {
        let mut tries: usize = 0;
        let mut last_response;

        let body = body.unwrap_or_default();

        // Plazo de cada intento e intentos como mucho (ver `request_policy`): sin ellos una
        // petición que no vuelve dejaba esperando para siempre a Spirc o a la carga de la canción.
        let policy = if options.fast {
            request_policy::fast_policy(method.as_str(), endpoint)
        } else {
            request_policy::attempt_policy(method.as_str(), endpoint)
        };
        let max_tries = match self.lock(|inner| inner.strategy) {
            RequestStrategy::TryTimes(n) => Some(n.min(policy.max_tries)),
            RequestStrategy::Infinitely => None,
        };

        loop {
            tries += 1;

            // Todo el intento va dentro del plazo, también los tokens y el punto de acceso: un
            // socket medio cerrado puede colgar cualquiera de esas peticiones. Un error al montar
            // la petición sale tal cual (`Err` de fuera), como antes con `?`.
            let attempt = async {
                // Reconnection logic: retrieve the endpoint every iteration, so we can try
                // another access point when we are experiencing network issues (see below).
                let mut url = match options.base_url {
                    Some(base_url) => base_url.to_string(),
                    None => self.base_url().await?,
                };
                url.push_str(endpoint);

                // Add metrics. There is also an optional `partner` key with a value like
                // `vodafone-uk` but we've yet to discover how we can find that value.
                // For the sake of documentation you could also do "product=free" but
                // we only support premium anyway.
                if options.metrics && !url.contains("product=0") {
                    let _ = write!(
                        url,
                        "{}product=0&country={}",
                        util::get_next_query_separator(&url),
                        self.session().country()
                    );
                }

                // Defeat caches. Spotify-generated URLs already contain this.
                if options.salt && !url.contains("salt=") {
                    let _ = write!(
                        url,
                        "{}salt={}",
                        util::get_next_query_separator(&url),
                        rand::rng().next_u32()
                    );
                }

                let mut request = Request::builder()
                    .method(method)
                    .uri(url)
                    .header(CONTENT_LENGTH, body.len())
                    .body(Bytes::copy_from_slice(body))?;

                // Reconnection logic: keep getting (cached) tokens because they might have expired.
                let token = self.session().login5().auth_token().await?;

                let headers_mut = request.headers_mut();
                if let Some(ref headers) = headers {
                    for (name, value) in headers {
                        headers_mut.insert(name, value.clone());
                    }
                }

                headers_mut.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("{} {}", token.token_type, token.access_token,))?,
                );

                match self.client_token().await {
                    Ok(client_token) => {
                        let _ =
                            headers_mut.insert(CLIENT_TOKEN, HeaderValue::from_str(&client_token)?);
                    }
                    Err(e) => {
                        // currently these endpoints seem to work fine without it
                        warn!("Unable to get client token: {e} Trying to continue without...")
                    }
                }

                Ok::<_, Error>(self.session().http_client().request_body(request).await)
            };

            let timed_out;
            last_response = match tokio::time::timeout(policy.timeout, attempt).await {
                Ok(Ok(response)) => {
                    timed_out = false;
                    response
                }
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    timed_out = true;
                    warn!(
                        "spclient: {method} {endpoint} sin respuesta en {} s (intento {tries})",
                        policy.timeout.as_secs()
                    );
                    Err(Error::deadline_exceeded(format!(
                        "{endpoint}: sin respuesta en {} s",
                        policy.timeout.as_secs()
                    )))
                }
            };

            if last_response.is_ok() {
                return last_response;
            }

            // Una escritura que no es idempotente quizá ya se aplicó aunque la respuesta no
            // llegara a tiempo: no se repite (se añadiría dos veces, se saltarían dos canciones).
            if timed_out && !policy.retry_on_timeout {
                break;
            }

            // Break before the reconnection logic below, so that the current access point
            // is retained when max_tries == 1. Leave it up to the caller when to flush.
            if max_tries.is_some_and(|max| tries >= max) {
                break;
            }

            // Reconnection logic: drop the current access point if we are experiencing issues.
            // This will cause the next call to base_url() to resolve a new one.
            if let Err(ref network_error) = last_response {
                match network_error.kind {
                    ErrorKind::Unavailable | ErrorKind::DeadlineExceeded => {
                        // Keep trying the current access point three times before dropping it
                        // (y antes del último intento, para que no vaya también al que falla).
                        if request_policy::flush_accesspoint_after(tries, max_tries) {
                            self.flush_accesspoint().await
                        }
                    }
                    _ => break, // if we can't build the request now, then we won't ever
                }
            }

            debug!("Error was: {last_response:?}");
        }

        last_response
    }

    pub async fn put_connect_state_request(&self, state: &PutStateRequest) -> SpClientResult {
        let endpoint = format!("/connect-state/v1/devices/{}", self.session().device_id());

        let mut headers = HeaderMap::new();
        headers.insert(CONNECTION_ID, self.session().connection_id().parse()?);

        self.request_with_protobuf(&Method::PUT, &endpoint, Some(headers), state)
            .await
    }

    pub async fn delete_connect_state_request(&self) -> SpClientResult {
        let endpoint = format!("/connect-state/v1/devices/{}", self.session().device_id());
        self.request(&Method::DELETE, &endpoint, None, None).await
    }

    pub async fn put_connect_state_inactive(&self, notify: bool) -> SpClientResult {
        let endpoint = format!(
            "/connect-state/v1/devices/{}/inactive?notify={notify}",
            self.session().device_id()
        );

        let mut headers = HeaderMap::new();
        headers.insert(CONNECTION_ID, self.session().connection_id().parse()?);

        self.request(&Method::PUT, &endpoint, Some(headers), None)
            .await
    }

    pub async fn get_extended_metadata(
        &self,
        request: BatchedEntityRequest,
    ) -> Result<BatchedExtensionResponse, Error> {
        self.extended_metadata_with(request, &Default::default())
            .await
    }

    async fn extended_metadata_with(
        &self,
        request: BatchedEntityRequest,
        options: &RequestOptions,
    ) -> Result<BatchedExtensionResponse, Error> {
        let res = self
            .request_with_protobuf_and_options(
                &Method::POST,
                "/extended-metadata/v0/extended-metadata",
                None,
                &request,
                options,
            )
            .await?;
        Ok(BatchedExtensionResponse::parse_from_bytes(&res)?)
    }

    /// Metadatos de una entidad, de la caché si ya se tienen (ver `meta_cache`). Es lo que pide
    /// el reproductor al cargar una canción. Solo se guardan los de canciones y episodios (los que
    /// están en el camino del primer sonido): álbumes, artistas y demás se piden como siempre.
    pub async fn get_metadata(&self, kind: ExtensionKind, id: &SpotifyUri) -> SpClientResult {
        if !cacheable(kind) {
            return self.fetch_metadata(kind, id).await;
        }
        let key = MetaKey::new(self.session().country(), kind.value(), id.to_uri()?);
        let hit = meta_cache().get(&key, Instant::now());
        let seed = match meta_cache::lookup(hit, seed_trust()) {
            meta_cache::Lookup::Serve { value, seeded } => {
                set_metadata_source(if seeded { "seed" } else { "hit" });
                return Ok(value);
            }
            meta_cache::Lookup::Check(seed) => Some(seed),
            meta_cache::Lookup::Fetch => None,
        };
        let fetched = self.fetch_metadata(kind, id).await?;
        if let Some(seed) = seed {
            // La comprobación única: ¿los bytes del lote de Nanofy son los mismos que los de esta
            // petición? Decide para el resto del proceso.
            let mut trust = SEED_TRUST.lock().unwrap_or_else(|e| e.into_inner());
            let before = *trust;
            *trust = before.after_check(&seed, &fetched);
            if *trust != before {
                if trust.serves_seeds() {
                    info!("metadatos sembrados por Nanofy: coinciden con los del reproductor; se usan");
                } else {
                    warn!("metadatos sembrados por Nanofy: no coinciden con los del reproductor; se dejan de usar");
                    drop(trust);
                    meta_cache().drop_seeded();
                }
            }
            set_metadata_source("check");
        } else {
            set_metadata_source("miss");
        }
        meta_cache().put(key, fetched.clone(), false, Instant::now());
        Ok(fetched)
    }

    /// Siembra en la caché lo que trajo un lote de extended-metadata de Nanofy (`any.value` de
    /// cada entidad), para que el reproductor no lo vuelva a pedir. Mientras las semillas no se
    /// hayan comprobado se guardan sin servirse; si no coincidieron, ni se guardan.
    pub fn seed_metadata(&self, kind: ExtensionKind, uri: &str, value: impl Into<Bytes>) {
        if !cacheable(kind) || !seed_trust().accepts_seeds() {
            return;
        }
        let key = MetaKey::new(self.session().country(), kind.value(), uri);
        meta_cache().put(key, value.into(), true, Instant::now());
    }

    /// ¿Ya están en la caché (y se servirían sin pedir nada) los metadatos de `uri`?
    pub fn metadata_cached(&self, kind: ExtensionKind, uri: &str) -> bool {
        let key = MetaKey::new(self.session().country(), kind.value(), uri);
        // Una semilla aún sin comprobar se volvería a pedir: no cuenta como guardada.
        let serves_seeds = seed_trust().serves_seeds();
        meta_cache()
            .peek(&key, Instant::now())
            .is_some_and(|seeded| !seeded || serves_seeds)
    }

    /// ¿Hay ya algo guardado para `uri`, aunque sea una semilla sin comprobar? (Volver a sembrarla
    /// no cambiaría nada: los bytes del lote serían los mismos.)
    pub fn metadata_held(&self, kind: ExtensionKind, uri: &str) -> bool {
        let key = MetaKey::new(self.session().country(), kind.value(), uri);
        meta_cache().contains(&key, Instant::now())
    }

    /// ¿Sirve de algo sembrar? No si la comprobación única dijo que las semillas no coinciden
    /// (`seed_metadata` ya no guarda nada): pedir un lote solo para sembrar sería una petición
    /// tirada.
    pub fn seeds_accepted() -> bool {
        seed_trust().accepts_seeds()
    }

    /// Como `metadata_cached`, con el tipo que pide el reproductor para `uri` (TRACK_V4 para una
    /// canción, EPISODE_V4 para un episodio).
    pub fn playable_metadata_cached(&self, uri: &SpotifyUri) -> bool {
        let kind = match uri {
            SpotifyUri::Track { .. } => ExtensionKind::TRACK_V4,
            SpotifyUri::Episode { .. } => ExtensionKind::EPISODE_V4,
            _ => return false,
        };
        uri.to_uri()
            .is_ok_and(|u| self.metadata_cached(kind, &u))
    }

    async fn fetch_metadata(&self, kind: ExtensionKind, id: &SpotifyUri) -> SpClientResult {
        let req = BatchedEntityRequest {
            entity_request: vec![EntityRequest {
                entity_uri: id.to_uri()?,
                query: vec![ExtensionQuery {
                    extension_kind: EnumOrUnknown::new(kind),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        // Una sola entidad (la canción que se va a cargar): plazo corto, no el de los lotes.
        let mut res = self.extended_metadata_with(req, &ONE_ITEM_METADATA).await?;
        let mut extended_metadata = res
            .extended_metadata
            .pop()
            .ok_or(SpClientError::ExpectedEntry("extended_metadata"))?;

        let mut data = extended_metadata
            .extension_data
            .pop()
            .ok_or(SpClientError::ExpectedEntry("extension_data"))?;

        match data.extension_data.take() {
            None => Err(SpClientError::ExpectedEntry("data").into()),
            Some(data) => Ok(Bytes::from(data.value)),
        }
    }

    pub async fn get_track_metadata(&self, track_uri: &SpotifyUri) -> SpClientResult {
        self.get_metadata(ExtensionKind::TRACK_V4, track_uri).await
    }

    pub async fn get_episode_metadata(&self, episode_uri: &SpotifyUri) -> SpClientResult {
        self.get_metadata(ExtensionKind::EPISODE_V4, episode_uri)
            .await
    }

    pub async fn get_album_metadata(&self, album_uri: &SpotifyUri) -> SpClientResult {
        self.get_metadata(ExtensionKind::ALBUM_V4, album_uri).await
    }

    pub async fn get_artist_metadata(&self, artist_uri: &SpotifyUri) -> SpClientResult {
        self.get_metadata(ExtensionKind::ARTIST_V4, artist_uri)
            .await
    }

    pub async fn get_show_metadata(&self, show_uri: &SpotifyUri) -> SpClientResult {
        self.get_metadata(ExtensionKind::SHOW_V4, show_uri).await
    }

    pub async fn get_lyrics(&self, track_id: &SpotifyId) -> SpClientResult {
        let endpoint = format!("/color-lyrics/v2/track/{}", track_id.to_base62()?);

        self.request_as_json(&Method::GET, &endpoint, None, None)
            .await
    }

    pub async fn get_lyrics_for_image(
        &self,
        track_id: &SpotifyId,
        image_id: &FileId,
    ) -> SpClientResult {
        let endpoint = format!(
            "/color-lyrics/v2/track/{}/image/spotify:image:{}",
            track_id.to_base62()?,
            image_id
        );

        self.request_as_json(&Method::GET, &endpoint, None, None)
            .await
    }

    pub async fn get_playlist(&self, playlist_id: &SpotifyId) -> SpClientResult {
        let endpoint = format!("/playlist/v2/playlist/{}", playlist_id.to_base62()?);

        self.request(&Method::GET, &endpoint, None, None).await
    }

    pub async fn get_user_profile(
        &self,
        username: &str,
        playlist_limit: Option<u32>,
        artist_limit: Option<u32>,
    ) -> SpClientResult {
        let mut endpoint = format!("/user-profile-view/v3/profile/{username}");

        if playlist_limit.is_some() || artist_limit.is_some() {
            let _ = write!(endpoint, "?");

            if let Some(limit) = playlist_limit {
                let _ = write!(endpoint, "playlist_limit={limit}");
                if artist_limit.is_some() {
                    let _ = write!(endpoint, "&");
                }
            }

            if let Some(limit) = artist_limit {
                let _ = write!(endpoint, "artist_limit={limit}");
            }
        }

        self.request_as_json(&Method::GET, &endpoint, None, None)
            .await
    }

    pub async fn get_user_followers(&self, username: &str) -> SpClientResult {
        let endpoint = format!("/user-profile-view/v3/profile/{username}/followers");

        self.request_as_json(&Method::GET, &endpoint, None, None)
            .await
    }

    pub async fn get_user_following(&self, username: &str) -> SpClientResult {
        let endpoint = format!("/user-profile-view/v3/profile/{username}/following");

        self.request_as_json(&Method::GET, &endpoint, None, None)
            .await
    }

    pub async fn get_radio_for_track(&self, track_uri: &SpotifyUri) -> SpClientResult {
        let endpoint = format!(
            "/inspiredby-mix/v2/seed_to_playlist/{}?response-format=json",
            track_uri.to_uri()?
        );

        self.request_as_json(&Method::GET, &endpoint, None, None)
            .await
    }

    // Known working scopes: stations, tracks
    // For others see: https://gist.github.com/roderickvd/62df5b74d2179a12de6817a37bb474f9
    //
    // Seen-in-the-wild but unimplemented query parameters:
    // - image_style=gradient_overlay
    // - excludeClusters=true
    // - language=en
    // - count_tracks=0
    // - market=from_token
    pub async fn get_apollo_station(
        &self,
        scope: &str,
        context_uri: &str,
        count: Option<usize>,
        previous_tracks: Vec<SpotifyId>,
        autoplay: bool,
    ) -> SpClientResult {
        let mut endpoint = format!("/radio-apollo/v3/{scope}/{context_uri}?autoplay={autoplay}");

        // Spotify has a default of 50
        if let Some(count) = count {
            let _ = write!(endpoint, "&count={count}");
        }

        let previous_track_str = previous_tracks
            .iter()
            .map(|track| track.to_base62())
            .collect::<Result<Vec<_>, _>>()?
            .join(",");
        // better than checking `previous_tracks.len() > 0` because the `filter_map` could still return 0 items
        if !previous_track_str.is_empty() {
            let _ = write!(endpoint, "&prev_tracks={previous_track_str}");
        }

        self.request_as_json(&Method::GET, &endpoint, None, None)
            .await
    }

    pub async fn get_next_page(&self, next_page_uri: &str) -> SpClientResult {
        let endpoint = next_page_uri.trim_start_matches("hm:/");
        self.request_as_json(&Method::GET, endpoint, None, None)
            .await
    }

    // TODO: Seen-in-the-wild but unimplemented endpoints
    // - /presence-view/v1/buddylist

    pub async fn get_audio_storage(&self, file_id: &FileId) -> SpClientResult {
        let endpoint = format!(
            "/storage-resolve/files/audio/interactive/{}",
            file_id.to_base16()?
        );
        self.request(&Method::GET, &endpoint, None, None).await
    }

    pub fn stream_from_cdn<U>(
        &self,
        cdn_url: U,
        offset: usize,
        length: usize,
    ) -> Result<IntoStream<ResponseFuture>, Error>
    where
        U: TryInto<Uri>,
        <U as TryInto<Uri>>::Error: Into<http::Error>,
    {
        let req = Request::builder()
            .method(&Method::GET)
            .uri(cdn_url)
            .header(
                RANGE,
                HeaderValue::from_str(&format!("bytes={}-{}", offset, offset + length - 1))?,
            )
            .body(Bytes::new())?;

        let uri = req.uri().clone();
        let stream = self.session().http_client().request_stream(req)?;
        // Qué servidor de la CDN prueba la apertura de un fichero (ver `cdn_url::end_open`). Solo
        // si la petición sale: si la frenó el limitador propio, el servidor no tiene la culpa y no
        // debe pasar al final de la lista como fallido.
        crate::cdn_url::note_attempt(uri.host());

        Ok(stream)
    }

    pub async fn request_url(&self, url: &str) -> SpClientResult {
        let request = Request::builder()
            .method(&Method::GET)
            .uri(url)
            .body(Bytes::new())?;

        self.session().http_client().request_body(request).await
    }

    // Audio preview in 96 kbps MP3, unencrypted
    pub async fn get_audio_preview(&self, preview_id: &FileId) -> SpClientResult {
        const ATTRIBUTE: &str = "audio-preview-url-template";
        let template = self
            .session()
            .get_user_attribute(ATTRIBUTE)
            .ok_or_else(|| SpClientError::Attribute(ATTRIBUTE.to_string()))?;

        let mut url = template.replace("{id}", &preview_id.to_base16()?);
        let separator = match url.find('?') {
            Some(_) => "&",
            None => "?",
        };
        let _ = write!(url, "{}cid={}", separator, self.session().client_id());

        self.request_url(&url).await
    }

    // The first 128 kB of a track, unencrypted
    pub async fn get_head_file(&self, file_id: &FileId) -> SpClientResult {
        const ATTRIBUTE: &str = "head-files-url";
        let template = self
            .session()
            .get_user_attribute(ATTRIBUTE)
            .ok_or_else(|| SpClientError::Attribute(ATTRIBUTE.to_string()))?;

        let url = template.replace("{file_id}", &file_id.to_base16()?);

        self.request_url(&url).await
    }

    pub async fn get_image(&self, image_id: &FileId) -> SpClientResult {
        const ATTRIBUTE: &str = "image-url";
        let template = self
            .session()
            .get_user_attribute(ATTRIBUTE)
            .ok_or_else(|| SpClientError::Attribute(ATTRIBUTE.to_string()))?;
        let url = template.replace("{file_id}", &image_id.to_base16()?);

        self.request_url(&url).await
    }

    /// Request the context for an uri
    ///
    /// All [SpotifyId] uris are supported in addition to the following special uris:
    /// - liked songs:
    ///   - all: `spotify:user:<user_id>:collection`
    ///   - of artist: `spotify:user:<user_id>:collection:artist:<artist_id>`
    /// - search: `spotify:search:<search+query>` (whitespaces are replaced with `+`)
    ///
    /// ## Query params found in the wild:
    /// - include_video=true
    ///
    /// ## Known results of uri types:
    /// - uris of type `track`
    ///   - returns a single page with a single track
    ///   - when requesting a single track with a query in the request, the returned track uri
    ///     **will** contain the query
    /// - uris of type `artist`
    ///   - returns 2 pages with tracks: 10 most popular tracks and latest/popular album
    ///   - remaining pages are artist albums sorted by popularity (only provided as page_url)
    /// - uris of type `search`
    ///   - is massively influenced by the provided query
    ///   - the query result shown by the search expects no query at all
    ///   - uri looks like `spotify:search:never+gonna`
    pub async fn get_context(&self, uri: &str) -> Result<Context, Error> {
        let res = self.get_context_raw(uri).await?;
        let ctx_json = String::from_utf8(res.to_vec())?;
        if ctx_json.is_empty() {
            Err(SpClientError::NoData)?
        }

        let ctx = protobuf_json_mapping::parse_from_str::<Context>(&ctx_json);

        if ctx.is_err() {
            trace!("failed parsing context: {ctx_json}")
        }

        Ok(ctx?)
    }

    /// La misma petición que `get_context`, sin interpretarla: el JSON tal cual llega. La sonda de
    /// mezclas de Nanofy lo lee entero (claves que el modelo `Context` no recoge) y debe ver
    /// exactamente lo que ve Spirc, sin parámetros de más en la URL.
    pub async fn get_context_raw(&self, uri: &str) -> SpClientResult {
        let uri = format!("/context-resolve/v1/{uri}");
        self.request_with_options(&Method::GET, &uri, None, None, &NO_METRICS_AND_SALT)
            .await
    }

    pub async fn get_autoplay_context(
        &self,
        context_request: &AutoplayContextRequest,
    ) -> Result<Context, Error> {
        let res = self
            .request_with_protobuf_and_options(
                &Method::POST,
                "/context-resolve/v1/autoplay",
                None,
                context_request,
                &NO_METRICS_AND_SALT,
            )
            .await?;

        let ctx_json = String::from_utf8(res.to_vec())?;
        if ctx_json.is_empty() {
            Err(SpClientError::NoData)?
        }

        let ctx = protobuf_json_mapping::parse_from_str::<Context>(&ctx_json);

        if ctx.is_err() {
            trace!("failed parsing context: {ctx_json}")
        }

        Ok(ctx?)
    }

    pub async fn get_rootlist(&self, from: usize, length: Option<usize>) -> SpClientResult {
        let length = length.unwrap_or(120);
        let user = self.session().username();
        let endpoint = format!(
            "/playlist/v2/user/{user}/rootlist?decorate=revision,attributes,length,owner,capabilities,status_code&from={from}&length={length}"
        );

        self.request(&Method::GET, &endpoint, None, None).await
    }

    /// Triggers the transfers of the playback from one device to another
    ///
    /// Using the same `device_id` for `from_device_id` and `to_device_id`, initiates the transfer
    /// from the currently active device.
    pub async fn transfer(
        &self,
        from_device_id: &str,
        to_device_id: &str,
        transfer_request: Option<&TransferRequest>,
    ) -> SpClientResult {
        let body = transfer_request.map(serde_json::to_string).transpose()?;

        let endpoint =
            format!("/connect-state/v1/connect/transfer/from/{from_device_id}/to/{to_device_id}");
        self.request_with_options(
            &Method::POST,
            &endpoint,
            None,
            body.as_deref().map(|s| s.as_bytes()),
            &NO_METRICS_AND_SALT,
        )
        .await
    }

    /// Envía un comando de reproducción al dispositivo virtual de una Jam (social session), para
    /// que Spotify lo aplique y lo propague a todos los participantes. `command_json` es el objeto
    /// `{"command": {"endpoint": "...", ...}}`. Así un participante puede controlar la cola
    /// compartida (siguiente, anterior, reproducir, añadir a la cola).
    pub async fn jam_command(
        &self,
        from_device_id: &str,
        session_id: &str,
        command_json: &str,
    ) -> SpClientResult {
        let endpoint = format!(
            "/connect-state/v1/player/command/from/{from_device_id}/to/social-connect-{session_id}"
        );
        self.request_as_json(&Method::POST, &endpoint, None, Some(command_json))
            .await
    }
}
