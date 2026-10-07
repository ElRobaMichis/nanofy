use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

use bytes::Bytes;
use futures_util::{FutureExt, future::IntoStream};
use governor::{
    Quota, RateLimiter, clock::MonotonicClock, middleware::NoOpMiddleware,
    state::keyed::DefaultKeyedStateStore,
};
use http::{Uri, header::HeaderValue};
use http_body_util::{BodyExt, Full};
use hyper::{HeaderMap, Request, Response, StatusCode, body::Incoming, header::USER_AGENT};
use hyper_proxy2::{Intercept, Proxy, ProxyConnector};
use hyper_util::{
    client::legacy::{Client, ResponseFuture, connect::HttpConnector},
    rt::TokioExecutor,
};
use nonzero_ext::nonzero;
use thiserror::Error;
use url::Url;

#[cfg(all(feature = "__rustls", not(feature = "native-tls")))]
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
#[cfg(all(feature = "native-tls", not(feature = "__rustls")))]
use hyper_tls::HttpsConnector;

use crate::{
    Error,
    config::{OS, os_version},
    date::Date,
    version::{FALLBACK_USER_AGENT, VERSION_STRING, spotify_version},
};

// The 30 seconds interval is documented by Spotify, but the calls per interval
// is a guesstimate and probably subject to licensing (purchasing extra calls)
// and may change at any time.
pub const RATE_LIMIT_INTERVAL: Duration = Duration::from_secs(30);
pub const RATE_LIMIT_MAX_WAIT: Duration = Duration::from_secs(10);
pub const RATE_LIMIT_CALLS_PER_INTERVAL: u32 = 300;
/// Lo que `request` espera su turno en el limitador propio antes de rendirse con
/// `ResourceExhausted`. Antes fallaba al instante: tras cargar una biblioteca grande (que gasta
/// el mismo presupuesto de spotify.com), la canción pedida no cargaba, Spirc la daba por no
/// disponible y saltaba a la siguiente, que tampoco: decenas de saltos en milisegundos. Con el
/// cupo agotado entra una petición cada 100 ms (300 cada 30 s), así que 3 s dejan pasar la
/// reproducción en un momento sin colgar mucho rato el bucle de Spirc si el atasco es mayor.
pub const RATE_LIMIT_QUEUE_WAIT: Duration = Duration::from_secs(3);

#[derive(Debug, Error)]
pub enum HttpClientError {
    #[error("Response status code: {0}")]
    StatusCode(hyper::StatusCode),
}

impl From<HttpClientError> for Error {
    fn from(err: HttpClientError) -> Self {
        match err {
            HttpClientError::StatusCode(code) => {
                // not exhaustive, but what reasonably could be expected
                match code {
                    StatusCode::GATEWAY_TIMEOUT | StatusCode::REQUEST_TIMEOUT => {
                        Error::deadline_exceeded(err)
                    }
                    StatusCode::GONE
                    | StatusCode::NOT_FOUND
                    | StatusCode::MOVED_PERMANENTLY
                    | StatusCode::PERMANENT_REDIRECT
                    | StatusCode::TEMPORARY_REDIRECT => Error::not_found(err),
                    StatusCode::FORBIDDEN | StatusCode::PAYMENT_REQUIRED => {
                        Error::permission_denied(err)
                    }
                    StatusCode::NETWORK_AUTHENTICATION_REQUIRED
                    | StatusCode::PROXY_AUTHENTICATION_REQUIRED
                    | StatusCode::UNAUTHORIZED => Error::unauthenticated(err),
                    StatusCode::EXPECTATION_FAILED
                    | StatusCode::PRECONDITION_FAILED
                    | StatusCode::PRECONDITION_REQUIRED => Error::failed_precondition(err),
                    StatusCode::RANGE_NOT_SATISFIABLE => Error::out_of_range(err),
                    StatusCode::INTERNAL_SERVER_ERROR
                    | StatusCode::MISDIRECTED_REQUEST
                    | StatusCode::SERVICE_UNAVAILABLE
                    | StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS => Error::unavailable(err),
                    StatusCode::BAD_REQUEST
                    | StatusCode::HTTP_VERSION_NOT_SUPPORTED
                    | StatusCode::LENGTH_REQUIRED
                    | StatusCode::METHOD_NOT_ALLOWED
                    | StatusCode::NOT_ACCEPTABLE
                    | StatusCode::PAYLOAD_TOO_LARGE
                    | StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE
                    | StatusCode::UNSUPPORTED_MEDIA_TYPE
                    | StatusCode::URI_TOO_LONG => Error::invalid_argument(err),
                    StatusCode::TOO_MANY_REQUESTS => Error::resource_exhausted(err),
                    StatusCode::NOT_IMPLEMENTED => Error::unimplemented(err),
                    _ => Error::unknown(err),
                }
            }
        }
    }
}

type HyperClient = Client<ProxyConnector<HttpsConnector<HttpConnector>>, Full<bytes::Bytes>>;

pub struct HttpClient {
    user_agent: HeaderValue,
    proxy_url: Option<Url>,
    hyper_client: OnceLock<HyperClient>,

    rate_limiter:
        RateLimiter<String, DefaultKeyedStateStore<String>, MonotonicClock, NoOpMiddleware>,
}

impl HttpClient {
    pub fn new(proxy_url: Option<&Url>) -> Self {
        let zero_str = String::from("0");
        let os_version = os_version();

        let (spotify_platform, os_version) = match OS {
            "android" => ("Android", os_version),
            "ios" => ("iOS", os_version),
            "macos" => ("OSX", zero_str),
            "windows" => ("Win32", zero_str),
            _ => ("Linux", zero_str),
        };

        let user_agent_str = &format!(
            "Spotify/{} {}/{} ({})",
            spotify_version(),
            spotify_platform,
            os_version,
            VERSION_STRING
        );

        let user_agent = HeaderValue::from_str(user_agent_str).unwrap_or_else(|err| {
            error!("Invalid user agent <{user_agent_str}>: {err}");
            HeaderValue::from_static(FALLBACK_USER_AGENT)
        });

        let replenish_interval_ns =
            RATE_LIMIT_INTERVAL.as_nanos() / RATE_LIMIT_CALLS_PER_INTERVAL as u128;
        let quota = Quota::with_period(Duration::from_nanos(replenish_interval_ns as u64))
            .expect("replenish interval should be valid")
            .allow_burst(nonzero![RATE_LIMIT_CALLS_PER_INTERVAL]);
        let rate_limiter = RateLimiter::keyed(quota);

        Self {
            user_agent,
            proxy_url: proxy_url.cloned(),
            hyper_client: OnceLock::new(),
            rate_limiter,
        }
    }

    fn try_create_hyper_client(proxy_url: Option<&Url>) -> Result<HyperClient, Error> {
        // configuring TLS is expensive and should be done once per process

        #[cfg(all(feature = "__rustls", not(feature = "native-tls")))]
        let https_connector = {
            #[cfg(feature = "rustls-tls-native-roots")]
            let tls = HttpsConnectorBuilder::new().with_native_roots()?;
            #[cfg(feature = "rustls-tls-webpki-roots")]
            let tls = HttpsConnectorBuilder::new().with_webpki_roots();
            tls.https_or_http().enable_http1().enable_http2().build()
        };

        #[cfg(all(feature = "native-tls", not(feature = "__rustls")))]
        let https_connector = HttpsConnector::new();

        // When not using a proxy a dummy proxy is configured that will not intercept any traffic.
        // This prevents needing to carry the Client Connector generics through the whole project
        let proxy = match &proxy_url {
            Some(proxy_url) => Proxy::new(Intercept::All, proxy_url.to_string().parse()?),
            None => Proxy::new(Intercept::None, Uri::from_static("0.0.0.0")),
        };
        let proxy_connector = ProxyConnector::from_proxy(https_connector, proxy)?;

        // Conexiones ociosas durante 5 min en vez de 90 s: tras un rato en pausa, «reproducir»
        // o «siguiente» ya no paga otra vez TCP + TLS con spclient (y la CDN). Una que el equipo
        // dejara medio cerrada al suspenderse la corta el plazo de cada intento de spclient.
        let client = Client::builder(TokioExecutor::new())
            .http2_adaptive_window(true)
            .pool_idle_timeout(Duration::from_secs(300))
            .build(proxy_connector);
        Ok(client)
    }

    fn hyper_client(&self) -> &HyperClient {
        self.hyper_client
            .get_or_init(|| Self::try_create_hyper_client(self.proxy_url.as_ref()).unwrap())
    }

    pub async fn request(&self, req: Request<Bytes>) -> Result<Response<Incoming>, Error> {
        debug!("Requesting {}", req.uri());

        // Pruebas (`NANOFY_FAULT=spclient_hang:<endpoint>`): la petición no vuelve, como con un
        // socket medio cerrado tras suspender el equipo. Va dentro de esta función para que un
        // tiempo límite puesto alrededor de la petición también la corte.
        if let Some(hang) = crate::fault::spclient_hang(req.uri().path()) {
            warn!("NANOFY_FAULT: {} se queda colgada", req.uri().path());
            match hang {
                Some(delay) => tokio::time::sleep(delay).await,
                None => std::future::pending::<()>().await,
            }
        }

        // `Request` does not implement `Clone` because its `Body` may be a single-shot stream.
        // As correct as that may be technically, we now need all this boilerplate to clone it
        // ourselves, as any `Request` is moved in the loop.
        let (parts, body_as_bytes) = req.into_parts();
        let domain = Self::rate_limit_domain(&parts.uri);

        loop {
            // Un turno del limitador por intento, como antes (también al repetir tras un 429),
            // pero esperado: ver RATE_LIMIT_QUEUE_WAIT. Después se envía sin volver a mirarlo,
            // para no gastar dos turnos en una sola petición.
            self.wait_rate_limit(&domain, RATE_LIMIT_QUEUE_WAIT).await?;

            let mut req = Request::builder()
                .method(parts.method.clone())
                .uri(parts.uri.clone())
                .version(parts.version)
                .body(body_as_bytes.clone())?;
            *req.headers_mut() = parts.headers.clone();

            let response = self.send(req).await;

            if let Ok(response) = &response {
                let code = response.status();

                if code == StatusCode::TOO_MANY_REQUESTS {
                    crate::ttfs::count(crate::ttfs::Counter::Http429);
                    if let Some(duration) = Self::get_retry_after(response.headers()) {
                        warn!(
                            "Rate limited by service, retrying in {} seconds...",
                            duration.as_secs()
                        );
                        tokio::time::sleep(duration).await;
                        continue;
                    }
                }

                if !code.is_success() {
                    return Err(HttpClientError::StatusCode(code).into());
                }
            }

            let response = response?;
            return Ok(response);
        }
    }

    pub async fn request_body(&self, req: Request<Bytes>) -> Result<Bytes, Error> {
        let response = self.request(req).await?;
        Ok(response.into_body().collect().await?.to_bytes())
    }

    pub fn request_stream(&self, req: Request<Bytes>) -> Result<IntoStream<ResponseFuture>, Error> {
        Ok(self.request_fut(req)?.into_stream())
    }

    /// La petición tal cual, comprobando antes el limitador sin esperar: lo usan los flujos de la
    /// CDN (`request_stream`), que llevan su propio presupuesto por dominio y sus reintentos.
    pub fn request_fut(&self, req: Request<Bytes>) -> Result<ResponseFuture, Error> {
        let domain = Self::rate_limit_domain(req.uri());
        self.drain_limiter_for_fault(&domain);
        self.rate_limiter.check_key(&domain).map_err(|e| {
            crate::ttfs::count(crate::ttfs::Counter::RateLimited);
            Error::resource_exhausted(format!(
                "rate limited for at least another {} seconds",
                e.wait_time_from(Instant::now()).as_secs()
            ))
        })?;
        Ok(self.send(req))
    }

    /// Espera, como mucho `max_wait`, un turno del limitador propio para `uri` (por dominio, ver
    /// `rate_limit_domain`) y lo gasta. Si el turno llegaría más tarde, falla ya, sin esperar, con
    /// el `ResourceExhausted` de siempre («rate limited for at least another N seconds», que
    /// Nanofy lee para saber cuánto esperar). Es lo que hace `request` antes de cada intento;
    /// pública para las pruebas de Nanofy.
    pub async fn acquire_rate_limit(&self, uri: &Uri, max_wait: Duration) -> Result<(), Error> {
        let domain = Self::rate_limit_domain(uri);
        self.wait_rate_limit(&domain, max_wait).await
    }

    async fn wait_rate_limit(&self, domain: &String, max_wait: Duration) -> Result<(), Error> {
        self.drain_limiter_for_fault(domain);
        let deadline = Instant::now() + max_wait;
        let mut waiting_since: Option<Instant> = None;
        loop {
            match self.rate_limiter.check_key(domain) {
                Ok(()) => {
                    if let Some(t0) = waiting_since {
                        debug!(
                            "límite de peticiones a {domain}: turno tras esperar {} ms",
                            t0.elapsed().as_millis()
                        );
                    }
                    return Ok(());
                }
                Err(not_until) => {
                    let now = Instant::now();
                    let wait = not_until.wait_time_from(now);
                    if now + wait > deadline {
                        crate::ttfs::count(crate::ttfs::Counter::RateLimited);
                        return Err(Error::resource_exhausted(format!(
                            "rate limited for at least another {} seconds",
                            wait.as_secs()
                        )));
                    }
                    waiting_since.get_or_insert(now);
                    // Con varias esperando a la vez, el turno es de una sola: las demás vuelven
                    // a mirar y esperan el siguiente (al menos 1 ms, para no girar en vacío).
                    tokio::time::sleep(wait.max(Duration::from_millis(1))).await;
                }
            }
        }
    }

    /// Clave del limitador: el dominio sin subdominios.
    fn rate_limit_domain(uri: &Uri) -> String {
        match uri.host() {
            Some(host) => {
                // strip the prefix from *.domain.tld (assume rate limit is per domain, not subdomain)
                let mut parts = host
                    .split('.')
                    .map(|s| s.to_string())
                    .collect::<Vec<String>>();
                let n = parts.len().saturating_sub(2);
                parts.drain(n..).collect()
            }
            None => String::from(""),
        }
    }

    /// Pruebas (`NANOFY_FAULT=limiter_exhaust`): tras una orden de reproducir, el presupuesto
    /// de spotify.com aparece gastado, como cuando una biblioteca grande acaba de cargarse.
    fn drain_limiter_for_fault(&self, domain: &String) {
        if domain == "spotify.com" && crate::fault::take_limiter_drain() {
            let mut spent = 0;
            while spent < 2 * RATE_LIMIT_CALLS_PER_INTERVAL
                && self.rate_limiter.check_key(domain).is_ok()
            {
                spent += 1;
            }
            warn!("NANOFY_FAULT: presupuesto de peticiones a {domain} agotado ({spent} gastadas)");
        }
    }

    /// Envía la petición, que ya pasó por el limitador propio (`request_fut` o `wait_rate_limit`).
    fn send(&self, mut req: Request<Bytes>) -> ResponseFuture {
        let headers_mut = req.headers_mut();
        headers_mut.insert(USER_AGENT, self.user_agent.clone());

        // For rate limiting we cannot *just* depend on Spotify sending us HTTP/429
        // Retry-After headers. For example, when there is a service interruption
        // and HTTP/500 is returned, we don't want to DoS the Spotify infrastructure.
        // (De ahí el limitador propio, que se mira antes de llegar aquí.)
        self.hyper_client().request(req.map(Full::new))
    }

    pub fn get_retry_after(headers: &HeaderMap<HeaderValue>) -> Option<Duration> {
        let now = Date::now_utc().as_timestamp_ms();

        let mut retry_after_ms = None;
        if let Some(header_val) = headers.get("X-RateLimit-Next") {
            // *.akamaized.net (Akamai)
            if let Ok(date_str) = header_val.to_str() {
                if let Ok(target) = Date::from_iso8601(date_str) {
                    retry_after_ms = Some(target.as_timestamp_ms().saturating_sub(now))
                }
            }
        } else if let Some(header_val) = headers.get("Fastly-RateLimit-Reset") {
            // *.scdn.co (Fastly)
            if let Ok(timestamp) = header_val.to_str() {
                if let Ok(target) = timestamp.parse::<i64>() {
                    retry_after_ms = Some(target.saturating_sub(now))
                }
            }
        } else if let Some(header_val) = headers.get("Retry-After") {
            // Generic RFC compliant (including *.spotify.com)
            if let Ok(retry_after) = header_val.to_str() {
                if let Ok(duration) = retry_after.parse::<i64>() {
                    retry_after_ms = Some(duration * 1000)
                }
            }
        }

        if let Some(retry_after) = retry_after_ms {
            let duration = Duration::from_millis(retry_after as u64);
            if duration <= RATE_LIMIT_MAX_WAIT {
                return Some(duration);
            } else {
                debug!(
                    "Waiting {} seconds would exceed {} second limit",
                    duration.as_secs(),
                    RATE_LIMIT_MAX_WAIT.as_secs()
                );
            }
        }

        None
    }
}
