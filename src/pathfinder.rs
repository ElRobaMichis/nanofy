//! Búsqueda por «pathfinder», la API GraphQL de los clientes oficiales (la consulta persistida
//! searchDesktop en api-partner.spotify.com). Una sola petición con el token de login5 y el
//! client-token de la sesión de librespot: no gasta la cuota de la Web API ni se para con su
//! enfriamiento por QUOTA_EXCEEDED. Si falla, la búsqueda sigue por la Web API (api.rs,
//! `Req::Search`).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::backend::Shared;
use crate::model::*;

const URL: &str = "https://api-partner.spotify.com/pathfinder/v2/query";
/// Hash de la consulta persistida searchDesktop del reproductor web. Cambia cuando Spotify
/// cambia la consulta: entonces responde PersistedQueryNotFound y se busca el nuevo en el
/// propio reproductor web (`discover`).
const SEARCH_HASH: &str = "eef7cc54888d91bdd6802623477873caa3948ae173a0c34fd86827b267e94c03";
/// Versión del reproductor web que se anuncia (la que su paquete pone en estas llamadas). Si
/// Spotify deja de aceptarla, cambiarla aquí.
const APP_VERSION: &str = "896000000";
/// pathfinder es la API del reproductor web: se presenta como un navegador.
const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";
/// Plazo de una búsqueda. Responde en ~1 s; pasado esto conviene más la Web API que seguir
/// mirando «Buscando».
const TIMEOUT: Duration = Duration::from_secs(5);
/// Tras 401/403/404/5xx o una respuesta que no se entiende se deja de probar este tiempo: cada
/// intento fallido retrasaría la búsqueda por la Web API. También es el tope de un Retry-After.
const DOWN: Duration = Duration::from_secs(600);
/// Buscar el hash nuevo como mucho una vez al día, también entre sesiones (son ~4,5 MB del
/// reproductor web).
const DISCOVERY_EVERY: u64 = 86_400;
/// Limitador propio (token bucket): ráfaga y fichas por segundo. No toca el de la Web API: una
/// búsqueda por aquí no le quita ritmo a la biblioteca ni al revés.
const BURST: f64 = 5.0;
const REFILL: f64 = 1.0;
/// Bytes de la primera respuesta que se copian al registro (la entera son cientos de KB).
const LOG_FIRST: usize = 2 * 1024;
const WEB_PLAYER: &str = "https://open.spotify.com/";
const CDN: &str = "https://open.spotifycdn.com/cdn/build/web-player/";

pub enum PfErr {
    /// El hash ya no vale (PersistedQueryNotFound).
    UnknownHash,
    /// 401/403/404/5xx o una respuesta que no se entiende: se aparta un rato.
    Down(String),
    /// 429: se aparta lo que pida el Retry-After (en segundos).
    Limited(u64),
    /// Respondió bien pero sin nada: la Web API quizá encuentre algo.
    Empty(SearchResult),
    /// Fallo pasajero (red, sesión, otro 4xx como una consulta demasiado larga): solo esta
    /// búsqueda va por la Web API.
    Failed(String),
}

struct State {
    /// Hash con el que se busca: el incorporado o uno descubierto.
    hash: String,
    /// Hash descubierto y ya confirmado por una búsqueda buena, con cuándo se obtuvo: lo único
    /// que se guarda en disco.
    saved: Option<(String, u64)>,
    /// El hash fue rechazado y no hay otro: todo por la Web API (como `artist_albums_blocked`).
    blocked: bool,
    /// Apartado hasta entonces tras un fallo o un 429.
    down_until: Option<Instant>,
    /// `down_until` viene de un 429: ni sin Web API se le insiste antes de tiempo.
    limited: bool,
    tokens: f64,
    last: Instant,
    /// La primera respuesta ya está en el registro.
    logged: bool,
    discovering: bool,
    /// Último intento de descubrir el hash (segundos Unix), también de sesiones anteriores.
    tried_at: Option<u64>,
}

impl State {
    fn file_json(&self) -> String {
        json!({
            // Con otra versión de la app (otro hash incorporado) se descarta lo guardado: el
            // incorporado nuevo es más reciente que lo que se descubrió con la anterior.
            "builtin": SEARCH_HASH,
            "hash": self.saved.as_ref().map(|s| s.0.clone()),
            "fetched_at": self.saved.as_ref().map(|s| s.1),
            "tried_at": self.tried_at,
        })
        .to_string()
    }
}

/// Búsqueda por pathfinder con su estado, compartido con el hilo que descubre el hash nuevo.
#[derive(Clone)]
pub struct Pathfinder {
    agent: ureq::Agent,
    shared: Arc<Shared>,
    handle: tokio::runtime::Handle,
    state: Arc<Mutex<State>>,
    file: PathBuf,
}

impl Pathfinder {
    pub fn new(agent: ureq::Agent, shared: Arc<Shared>, handle: tokio::runtime::Handle, file: PathBuf) -> Self {
        let v: Value = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let now = crate::cache::now_secs();
        let saved = (v["builtin"].as_str() == Some(SEARCH_HASH))
            .then(|| v["hash"].as_str().filter(|h| is_hex(h, 64)))
            .flatten()
            .map(|h| (h.to_string(), v["fetched_at"].as_u64().unwrap_or(0)));
        // Una fecha futura (reloj adelantado) no bloquea el descubrimiento.
        let tried_at = v["tried_at"].as_u64().filter(|&t| t <= now);
        let hash = saved.as_ref().map_or(SEARCH_HASH.to_string(), |s| s.0.clone());
        if saved.is_some() {
            log::info!("pathfinder: hash de searchDesktop descubierto en una sesión anterior");
        }
        Self {
            agent,
            shared,
            handle,
            state: Arc::new(Mutex::new(State {
                hash,
                saved,
                blocked: false,
                down_until: None,
                limited: false,
                tokens: BURST,
                last: Instant::now(),
                logged: false,
                discovering: false,
                tried_at,
            })),
            file,
        }
    }

    /// Hash con el que probar ahora, o por qué no (y los segundos de un 429 pendiente).
    /// `web_cooled`: la Web API está sin cuota; entonces se insiste aunque esté apartado por un
    /// fallo, porque no queda otra vía, pero no antes de que pase un 429 ni con un hash
    /// rechazado.
    pub fn usable(&self, web_cooled: bool) -> Result<String, (String, Option<u64>)> {
        let st = self.state.lock().unwrap();
        if st.blocked {
            let why = if st.discovering { "buscando el hash nuevo de searchDesktop" } else { "hash de searchDesktop rechazado" };
            return Err((why.to_string(), None));
        }
        if let Some(until) = st.down_until {
            let left = until.saturating_duration_since(Instant::now());
            if !left.is_zero() && (st.limited || !web_cooled) {
                let secs = left.as_secs() + u64::from(left.subsec_nanos() > 0);
                let why = format!("pathfinder apartado {secs} s más");
                return Err((why, st.limited.then_some(secs)));
            }
        }
        Ok(st.hash.clone())
    }

    /// Respondió bien con `hash`: vuelve a estar disponible y, si era uno descubierto aún sin
    /// confirmar, se guarda en disco.
    pub fn ok(&self, hash: &str) {
        let save = {
            let mut st = self.state.lock().unwrap();
            st.down_until = None;
            st.limited = false;
            let confirm = hash != SEARCH_HASH && st.hash == hash && st.saved.as_ref().map(|s| s.0.as_str()) != Some(hash);
            if confirm {
                st.saved = Some((hash.to_string(), crate::cache::now_secs()));
            }
            confirm.then(|| st.file_json())
        };
        if let Some(text) = save {
            log::info!("pathfinder: hash de searchDesktop confirmado; se guarda");
            let _ = std::fs::write(&self.file, text);
        }
    }

    /// 401/403/404/5xx o una respuesta que no se entiende: la búsqueda va por la Web API un rato.
    pub fn down(&self, why: &str) {
        let mut st = self.state.lock().unwrap();
        st.down_until = Some(Instant::now() + DOWN);
        st.limited = false;
        log::warn!("pathfinder falla ({why}); la búsqueda va por la Web API los próximos {} min", DOWN.as_secs() / 60);
    }

    /// 429 de pathfinder: no se le insiste hasta que pase (como mucho `DOWN`).
    pub fn limited(&self, secs: u64) {
        let mut st = self.state.lock().unwrap();
        st.down_until = Some(Instant::now() + Duration::from_secs(secs).min(DOWN));
        st.limited = true;
        log::info!("pathfinder limita (Retry-After {secs} s); la búsqueda va por la Web API mientras tanto");
    }

    /// Spotify ya no conoce `hash` (PersistedQueryNotFound): todo por la Web API hasta tener
    /// otro. Si no se ha intentado en las últimas 24 h, se busca el nuevo en segundo plano y se
    /// comprueba con `q`, la búsqueda que falló.
    pub fn reject(&self, hash: &str, q: &str) {
        let start = {
            let mut st = self.state.lock().unwrap();
            // Otra búsqueda ya lo cambió por uno descubierto: este rechazo es del anterior.
            if st.hash != hash {
                return;
            }
            st.blocked = true;
            // El guardado ya no vale: el próximo arranque parte del incorporado.
            let save = st.saved.as_ref().is_some_and(|s| s.0 == hash);
            if save {
                st.saved = None;
            }
            let now = crate::cache::now_secs();
            let due = !st.discovering && st.tried_at.is_none_or(|t| now.saturating_sub(t) >= DISCOVERY_EVERY);
            if due {
                st.discovering = true;
                st.tried_at = Some(now);
            }
            (due || save).then(|| (due, st.file_json()))
        };
        match start {
            Some((due, text)) => {
                let _ = std::fs::write(&self.file, text);
                if due {
                    log::warn!("pathfinder: Spotify ya no conoce el hash de searchDesktop; se busca el nuevo en el reproductor web");
                    let pf = self.clone();
                    let (hash, q) = (hash.to_string(), q.to_string());
                    let spawned = std::thread::Builder::new()
                        .name("nanofy-pathfinder".into())
                        .stack_size(512 * 1024)
                        .spawn(move || pf.discover(&hash, &q));
                    if spawned.is_err() {
                        self.state.lock().unwrap().discovering = false;
                    }
                } else {
                    log::warn!("pathfinder: Spotify ya no conoce el hash de searchDesktop guardado; la búsqueda va por la Web API");
                }
            }
            None => log::warn!(
                "pathfinder: Spotify ya no conoce el hash de searchDesktop y hoy ya se intentó buscar el nuevo; la búsqueda va por la Web API"
            ),
        }
    }

    /// Busca `q` con `hash`. Bloquea el hilo que llama (el carril de búsqueda) hasta la
    /// respuesta o el plazo.
    pub fn search(&self, q: &str, hash: &str) -> Result<SearchResult, PfErr> {
        self.acquire();
        let session = self
            .shared
            .session
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| PfErr::Failed("no has iniciado sesión".to_string()))?;
        // Con plazo: si alguno caducó se pide de nuevo por el http_client de librespot, que no
        // tiene plazo propio, y una sesión colgada (reconectando) dejaría el carril de búsqueda
        // parado sin llegar nunca a la Web API.
        let token = self
            .handle
            .block_on(async { tokio::time::timeout(TIMEOUT, session.login5().auth_token()).await })
            .map_err(|_| PfErr::Failed("token: sin respuesta".to_string()))?
            .map_err(|e| PfErr::Failed(format!("token: {e}")))?
            .access_token;
        let client_token = self
            .handle
            .block_on(async { tokio::time::timeout(TIMEOUT, session.spclient().client_token()).await })
            .map_err(|_| PfErr::Failed("client-token: sin respuesta".to_string()))?
            .map_err(|e| PfErr::Failed(format!("client-token: {e}")))?;
        let body = json!({
            "operationName": "searchDesktop",
            "variables": {
                "searchTerm": q,
                "offset": 0,
                "limit": 10,
                "numberOfTopResults": 5,
                "includeAudiobooks": true,
                "includeArtistHasConcertsField": false,
                "includePreReleases": true,
                "includeLocalConcertsField": false,
                "includeAuthors": false,
            },
            "extensions": {"persistedQuery": {"version": 1, "sha256Hash": hash}},
        })
        .to_string();
        // El agente propio (y no el http_client de librespot): ese comparte el límite de
        // *.spotify.com con los metadatos y descarta el cuerpo de los errores, donde viene
        // PersistedQueryNotFound. ureq pide y descomprime gzip solo.
        let resp = self
            .agent
            .post(URL)
            .config()
            .timeout_global(Some(TIMEOUT))
            .build()
            .header("Authorization", &format!("Bearer {token}"))
            .header("client-token", &client_token)
            .header("app-platform", "WebPlayer")
            .header("spotify-app-version", APP_VERSION)
            .header("Content-Type", "application/json;charset=UTF-8")
            .header("Accept", "application/json")
            .header("Accept-Language", "es")
            .header("User-Agent", USER_AGENT)
            .send(body.as_bytes());
        let mut resp = resp.map_err(net_err)?;
        let status = resp.status().as_u16();
        let retry_after = resp
            .headers()
            .get("Retry-After")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(5)
            .clamp(1, DOWN.as_secs());
        let text = resp.body_mut().read_to_string().map_err(net_err)?;
        self.log_first(status, &text);
        let v: Option<Value> = serde_json::from_str(&text).ok();
        if unknown_hash(v.as_ref(), &text) {
            return Err(PfErr::UnknownHash);
        }
        match status {
            200..=299 => {}
            429 => return Err(PfErr::Limited(retry_after)),
            401 | 403 | 404 | 500..=599 => return Err(PfErr::Down(format!("HTTP {status}"))),
            _ => return Err(PfErr::Failed(format!("HTTP {status}: {}", snippet(&text)))),
        }
        let Some(v) = v else {
            return Err(PfErr::Down("respuesta que no es JSON".to_string()));
        };
        let errors = v["errors"].as_array().filter(|e| !e.is_empty()).map(|e| snippet(&Value::Array(e.clone()).to_string()));
        let Some((result, raw, mapped)) = map_search(&v) else {
            // Spotify no pudo con ESTA consulta (un texto larguísimo o raro): solo esta búsqueda
            // va por la Web API. Antes apartaba pathfinder 10 min y, con la Web API limitada, las
            // búsquedas siguientes fallaban todas.
            if query_failed(&v) {
                return Err(PfErr::Failed(format!("errores: {}", errors.unwrap_or_default())));
            }
            return Err(PfErr::Down(match errors {
                Some(e) => format!("errores: {e}"),
                None => "respuesta sin data.searchV2".to_string(),
            }));
        };
        // Errores parciales (un campo que no resolvió) con datos: se usa lo que llegó.
        if let Some(e) = errors {
            log::warn!("pathfinder respondió con errores parciales: {e}");
        }
        if mapped == 0 {
            // Elementos que no se entienden: el formato cambió, no es una búsqueda vacía.
            return Err(if raw > 0 {
                PfErr::Down(format!("{raw} resultados en un formato desconocido"))
            } else {
                PfErr::Empty(result)
            });
        }
        Ok(result)
    }

    /// Espera lo justo para no pasar del ritmo propio de pathfinder.
    fn acquire(&self) {
        let wait = {
            let mut st = self.state.lock().unwrap();
            let now = Instant::now();
            st.tokens = (st.tokens + now.duration_since(st.last).as_secs_f64() * REFILL).min(BURST);
            st.last = now;
            if st.tokens >= 1.0 {
                st.tokens -= 1.0;
                Duration::ZERO
            } else {
                let deficit = 1.0 - st.tokens;
                st.tokens = 0.0;
                Duration::from_secs_f64(deficit / REFILL)
            }
        };
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }

    /// La primera respuesta de la sesión, al registro (recortada): si Spotify cambia el
    /// formato, ahí se ve cómo es el nuevo. Las demás no, que son cientos de KB cada una.
    fn log_first(&self, status: u16, text: &str) {
        let first = !std::mem::replace(&mut self.state.lock().unwrap().logged, true);
        if first {
            let mut cut = text.len().min(LOG_FIRST);
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            log::info!("[pathfinder] primera respuesta (HTTP {status}, {} bytes): {}", text.len(), &text[..cut]);
        }
    }

    /// Hilo aparte: busca el hash nuevo en el reproductor web y lo comprueba con `q` antes de
    /// usarlo. Solo se guarda en disco si esa búsqueda responde bien.
    fn discover(&self, old: &str, q: &str) {
        let t0 = Instant::now();
        let found = match self.find_hash() {
            Ok(h) if h == old => {
                log::warn!("pathfinder: el reproductor web usa el mismo hash de searchDesktop; la búsqueda sigue por la Web API");
                None
            }
            Ok(h) => Some(h),
            Err(e) => {
                log::warn!("pathfinder: no se encontró el hash nuevo de searchDesktop ({e}); la búsqueda sigue por la Web API");
                None
            }
        };
        if let Some(h) = found {
            log::info!("pathfinder: hash nuevo de searchDesktop en {} ms; se comprueba", t0.elapsed().as_millis());
            // (se usa, queda confirmado y se guarda)
            let test = self.search(q, &h);
            let (install, confirmed) = match &test {
                Ok(_) | Err(PfErr::Empty(_)) => (true, true),
                Err(PfErr::UnknownHash) => {
                    log::warn!("pathfinder: Spotify tampoco conoce el hash nuevo; la búsqueda sigue por la Web API");
                    (false, false)
                }
                Err(PfErr::Down(e)) => {
                    log::warn!("pathfinder: el hash nuevo no da una búsqueda que se entienda ({e}); se descarta");
                    (false, false)
                }
                // Un fallo pasajero no dice nada del hash: se usa y lo confirma la próxima
                // búsqueda (`ok`), o la rechaza (`reject`) sin volver a descubrir hasta mañana.
                Err(PfErr::Limited(_)) | Err(PfErr::Failed(_)) => (true, false),
            };
            if install {
                let text = {
                    let mut st = self.state.lock().unwrap();
                    st.hash = h.clone();
                    st.blocked = false;
                    st.down_until = None;
                    st.limited = false;
                    if confirmed {
                        st.saved = Some((h, crate::cache::now_secs()));
                    }
                    confirmed.then(|| st.file_json())
                };
                if let Some(text) = text {
                    let _ = std::fs::write(&self.file, text);
                }
                // El 429 de la prueba sigue valiendo: no se le insiste antes de que pase.
                if let Err(PfErr::Limited(secs)) = test {
                    self.limited(secs);
                }
                log::info!("pathfinder: la búsqueda vuelve a ir por pathfinder (hash {})", if confirmed { "confirmado y guardado" } else { "sin confirmar" });
            }
        }
        self.state.lock().unwrap().discovering = false;
    }

    /// Hash actual de searchDesktop en el reproductor web: está en su paquete principal o en el
    /// trozo de la ruta de búsqueda, que se carga aparte.
    fn find_hash(&self) -> Result<String, String> {
        let html = self.fetch(WEB_PLAYER, 4 << 20)?;
        let scripts = script_urls(&html);
        if scripts.is_empty() {
            return Err("la página del reproductor web no enlaza su paquete".to_string());
        }
        let mut last_err = String::new();
        for url in scripts.iter().take(4) {
            let js = match self.fetch(url, 32 << 20) {
                Ok(js) => js,
                Err(e) => {
                    last_err = e;
                    continue;
                }
            };
            if let Some(h) = search_hash(&js) {
                return Ok(h);
            }
            for chunk in route_chunks(&js, "xpui-routes-search") {
                // De los candidatos (hay un mapa para JS y otro para CSS) uno da 404: normal.
                match self.fetch(&format!("{CDN}{chunk}"), 8 << 20) {
                    Ok(js) => {
                        if let Some(h) = search_hash(&js) {
                            return Ok(h);
                        }
                    }
                    Err(e) => log::debug!("pathfinder: {chunk}: {e}"),
                }
            }
        }
        Err(if last_err.is_empty() { "searchDesktop no aparece en el paquete".to_string() } else { last_err })
    }

    fn fetch(&self, url: &str, limit: u64) -> Result<String, String> {
        let mut resp = self
            .agent
            .get(url)
            .config()
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .header("User-Agent", USER_AGENT)
            .header("Accept-Language", "es")
            .call()
            .map_err(|e| format!("red: {e}"))?;
        let status = resp.status().as_u16();
        if status != 200 {
            return Err(format!("HTTP {status} en {url}"));
        }
        resp.body_mut()
            .with_config()
            .limit(limit)
            .lossy_utf8(true)
            .read_to_string()
            .map_err(|e| format!("red: {e}"))
    }
}

/// `true` si la respuesta dice que Spotify no conoce el hash de la consulta persistida. Se mira
/// en `errors` (y en el texto si no es JSON): una canción que se llame así no debe confundirlo.
fn unknown_hash(v: Option<&Value>, text: &str) -> bool {
    let is_pqnf = |s: &str| s.contains("PersistedQueryNotFound") || s.contains("PERSISTED_QUERY_NOT_FOUND");
    match v {
        Some(v) => v["errors"].as_array().is_some_and(|errs| {
            errs.iter().any(|e| {
                [&e["message"], &e["extensions"]["code"], &e["extensions"]["classification"]]
                    .iter()
                    .any(|f| f.as_str().is_some_and(is_pqnf))
            })
        }),
        None => is_pqnf(text),
    }
}

/// `true` si Spotify entendió la consulta pero falló al ejecutarla (`DataFetchingException`, por
/// ejemplo con 200 «a» seguidas): es cosa de esa búsqueda, no de que pathfinder haya cambiado
/// (eso da errores de validación o una respuesta sin `searchV2`).
fn query_failed(v: &Value) -> bool {
    v["errors"].as_array().is_some_and(|errs| {
        !errs.is_empty() && errs.iter().all(|e| e["extensions"]["classification"].as_str() == Some("DataFetchingException"))
    })
}

/// Un plazo agotado aparta pathfinder: si no contesta (un cortafuegos que lo bloquea, por
/// ejemplo), cada búsqueda esperaría `TIMEOUT` antes de ir a la Web API. Otro fallo de red puede
/// ser un corte momentáneo, que afectaría igual a la Web API: solo cuenta para esta búsqueda.
fn net_err(e: ureq::Error) -> PfErr {
    match e {
        ureq::Error::Timeout(_) => PfErr::Down(format!("no responde en {} s", TIMEOUT.as_secs())),
        e => PfErr::Failed(format!("red: {e}")),
    }
}

fn snippet(text: &str) -> String {
    text.chars().take(200).collect()
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// El hash de searchDesktop tal como lo lleva el paquete: `"searchDesktop","query","<64 hex>"`.
fn search_hash(js: &str) -> Option<String> {
    const NEEDLE: &str = "\"searchDesktop\",\"query\",\"";
    js.match_indices(NEEDLE).find_map(|(i, _)| {
        let rest = &js[i + NEEDLE.len()..];
        let h = rest.get(..64)?;
        (is_hex(h, 64) && rest[64..].starts_with('"')).then(|| h.to_string())
    })
}

/// Paquetes del reproductor web enlazados en su página, el principal (web-player.*.js) primero.
fn script_urls(html: &str) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    for (i, _) in html.match_indices(CDN) {
        let end = html[i..].find(['"', '\'', ' ', '>', '<']).map_or(html.len(), |e| i + e);
        let url = &html[i..end];
        if url.ends_with(".js") && !urls.iter().any(|u| u == url) {
            urls.push(url.to_string());
        }
    }
    urls.sort_by_key(|u| !u[CDN.len()..].starts_with("web-player."));
    urls
}

/// Nombres de archivo candidatos del trozo `name` según los mapas de trozos de webpack del
/// paquete: `<id>:"<name>"` da el número y `<id>:"<hash>"` sus hashes de contenido.
fn route_chunks(js: &str, name: &str) -> Vec<String> {
    let needle = format!(":\"{name}\"");
    let bytes = js.as_bytes();
    // Una clave del mapa empieza tras «{» o «,»: así 312:"…" no pasa por 12:"…".
    let key_start = |at: usize| at > 0 && matches!(bytes[at - 1], b'{' | b',');
    let mut out: Vec<String> = Vec::new();
    for (i, _) in js.match_indices(&needle) {
        let digits = bytes[..i].iter().rev().take_while(|b| b.is_ascii_digit()).count();
        if digits == 0 || !key_start(i - digits) {
            continue;
        }
        let id = &js[i - digits..i];
        let key = format!("{id}:\"");
        for (j, _) in js.match_indices(&key) {
            if !key_start(j) {
                continue;
            }
            let rest = &js[j + key.len()..];
            let Some(end) = rest.find('"') else { continue };
            let h = &rest[..end];
            let ok = (8..=20).contains(&h.len()) && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            let file = format!("{name}.{h}.js");
            if ok && !out.contains(&file) {
                out.push(file);
            }
        }
        if out.len() >= 4 {
            break;
        }
    }
    out.truncate(4);
    out
}

// ------------------------------------------------------------------ respuesta -> modelos

/// Respuesta de searchDesktop -> `SearchResult`, en el orden del servidor. `None` si no trae
/// data.searchV2. Devuelve también cuántos elementos traía y cuántos se entendieron.
fn map_search(v: &Value) -> Option<(SearchResult, usize, usize)> {
    let s = v.get("data")?.get("searchV2").filter(|s| s.is_object())?;
    let mut n = (0, 0);
    let tracks = section(s, "tracksV2", "Track", "track", track, &mut n);
    let albums = section(s, "albumsV2", "Album", "album", album, &mut n);
    let artists = section(s, "artists", "Artist", "artist", artist, &mut n);
    let playlists = section(s, "playlists", "Playlist", "playlist", playlist, &mut n);
    let shows = section(s, "podcasts", "Podcast", "show", show, &mut n);
    let episodes = section(s, "episodes", "Episode", "episode", episode, &mut n);
    let audiobooks = section(s, "audiobooks", "Audiobook", "audiobook", audiobook, &mut n);
    let result = SearchResult {
        tracks,
        albums: wrap(albums),
        artists: wrap(artists),
        playlists: wrap(playlists),
        shows: wrap(shows),
        episodes: wrap(episodes),
        audiobooks: wrap(audiobooks),
    };
    Some((result, n.0, n.1))
}

/// La Web API puede mandar huecos (null) en estas listas; SearchResult los admite.
fn wrap<T>(p: Option<Paging<T>>) -> Option<Paging<Option<T>>> {
    p.map(|p| Paging { items: p.items.into_iter().map(Some).collect(), next: p.next, total: p.total })
}

/// Una sección (`None` si no viene). Se salta lo que no es la entidad esperada (envoltorios
/// NotFound o RestrictedContent, capítulos entre los episodios…): sin uri del tipo correcto no
/// se podría abrir.
fn section<T>(
    s: &Value,
    key: &str,
    typename: &str,
    kind: &str,
    map: fn(&Value) -> Option<T>,
    n: &mut (usize, usize),
) -> Option<Paging<T>> {
    let sec = s.get(key).filter(|v| v.is_object())?;
    let marker = format!(":{kind}:");
    let mut items = Vec::new();
    for it in sec["items"].as_array().into_iter().flatten() {
        n.0 += 1;
        // Las pistas vienen como {item: {data}}; lo demás como {data}.
        let w = it.get("item").filter(|w| w.is_object()).unwrap_or(it);
        let d = &w["data"];
        if d["__typename"].as_str().is_some_and(|t| t != typename) {
            continue;
        }
        if !d["uri"].as_str().is_some_and(|u| u.starts_with("spotify:") && u.contains(&marker)) {
            continue;
        }
        if let Some(x) = map(d) {
            items.push(x);
        }
    }
    n.1 += items.len();
    let total = sec["totalCount"].as_u64().map_or(items.len() as u32, |t| t.min(u32::MAX as u64) as u32);
    Some(Paging { items, next: None, total })
}

/// Último tramo de la uri: el id con el que la interfaz abre cada cosa.
fn last_seg(uri: &str) -> String {
    uri.rsplit(':').next().unwrap_or(uri).to_string()
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn opt_text(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

/// Imágenes de `{sources: [{url, width, height}]}`; pick_image necesita los anchos.
fn images(v: &Value) -> Vec<Image> {
    v["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            Some(Image {
                url: opt_text(&s["url"])?,
                width: s["width"].as_u64().map(|w| w as u32),
                height: s["height"].as_u64().map(|h| h as u32),
            })
        })
        .collect()
}

fn artist_refs(v: &Value) -> Vec<ArtistRef> {
    v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| {
            let uri = opt_text(&a["uri"]);
            ArtistRef { id: uri.as_deref().map(last_seg), name: text(&a["profile"]["name"]), uri }
        })
        .collect()
}

fn explicit(d: &Value) -> bool {
    d["contentRating"]["label"].as_str() == Some("EXPLICIT")
}

fn duration_ms(d: &Value) -> u32 {
    d["duration"]["totalMilliseconds"].as_u64().map_or(0, |ms| ms.min(u32::MAX as u64) as u32)
}

/// AAAA-MM-DD de una fecha ISO (`releaseDate.isoString`).
fn iso_day(v: &Value) -> Option<String> {
    v["isoString"].as_str().filter(|s| !s.is_empty()).map(|s| s.chars().take(10).collect())
}

fn track(d: &Value) -> Option<Track> {
    let uri = d["uri"].as_str()?;
    let a = &d["albumOfTrack"];
    let album = a.is_object().then(|| {
        let auri = opt_text(&a["uri"]);
        AlbumRef {
            id: auri.as_deref().map(last_seg).or_else(|| opt_text(&a["id"])),
            name: text(&a["name"]),
            uri: auri,
            images: images(&a["coverArt"]),
            artists: Vec::new(),
            release_date: None,
            total_tracks: None,
            album_type: None,
        }
    });
    Some(Track {
        id: Some(last_seg(uri)),
        uri: uri.to_string(),
        name: text(&d["name"]),
        duration_ms: duration_ms(d),
        explicit: explicit(d),
        artists: artist_refs(&d["artists"]),
        album,
        is_local: false,
        track_number: None,
        is_playable: d["playability"]["playable"].as_bool(),
        kind: Some("track".to_string()),
        added_by: None,
        added_at: None,
    })
}

fn album(d: &Value) -> Option<AlbumRef> {
    let uri = d["uri"].as_str()?;
    // year_of() solo mira los 4 primeros caracteres: basta el año.
    let release_date = match &d["date"]["year"] {
        Value::Number(y) => Some(y.to_string()),
        Value::String(y) if !y.is_empty() => Some(y.clone()),
        _ => iso_day(&d["date"]),
    };
    // En minúsculas como la Web API, que llama «single» también a los EP.
    let album_type = d["type"].as_str().map(|t| match t.to_ascii_lowercase().as_str() {
        "ep" => "single".to_string(),
        t => t.to_string(),
    });
    Some(AlbumRef {
        id: Some(last_seg(uri)),
        name: text(&d["name"]),
        uri: Some(uri.to_string()),
        images: images(&d["coverArt"]),
        artists: artist_refs(&d["artists"]),
        release_date,
        total_tracks: None,
        album_type,
    })
}

fn artist(d: &Value) -> Option<Artist> {
    let uri = d["uri"].as_str()?;
    Some(Artist {
        id: last_seg(uri),
        name: text(&d["profile"]["name"]),
        uri: uri.to_string(),
        images: images(&d["visuals"]["avatarImage"]),
        genres: Vec::new(),
        followers: None,
    })
}

fn playlist(d: &Value) -> Option<Playlist> {
    let id = last_seg(d["uri"].as_str()?);
    let owner = &d["ownerV2"]["data"];
    // El id del propietario es su nombre de usuario (is_mine lo compara con el de la cuenta).
    let owner_id = opt_text(&owner["username"]).or_else(|| owner["uri"].as_str().map(last_seg));
    let imgs: Vec<Image> = d["images"]["items"].as_array().and_then(|a| a.first()).map(images).unwrap_or_default();
    Some(Playlist {
        // Las uris antiguas (spotify:user:x:playlist:id) también reproducen, pero la interfaz
        // compara con la forma corta.
        uri: format!("spotify:playlist:{id}"),
        id,
        name: text(&d["name"]),
        description: opt_text(&d["description"]),
        images: (!imgs.is_empty()).then_some(imgs),
        owner: Owner { display_name: opt_text(&owner["name"]), id: owner_id },
        tracks: None,
        public: None,
        collaborative: None,
        followers: None,
        snapshot_id: None,
    })
}

/// Editorial o autor: a veces `{name}`, a veces el texto tal cual.
fn name_of(v: &Value) -> String {
    v.as_str().map(str::to_string).unwrap_or_else(|| text(&v["name"]))
}

fn show(d: &Value) -> Option<Show> {
    let uri = d["uri"].as_str()?;
    Some(Show {
        id: last_seg(uri),
        name: text(&d["name"]),
        uri: uri.to_string(),
        images: images(&d["coverArt"]),
        publisher: name_of(&d["publisher"]),
        description: text(&d["description"]),
        total_episodes: None,
        media_type: d["mediaType"].as_str().map(|m| m.to_ascii_lowercase()),
        keywords: Vec::new(),
    })
}

fn episode(d: &Value) -> Option<Episode> {
    let uri = d["uri"].as_str()?;
    Some(Episode {
        id: last_seg(uri),
        name: text(&d["name"]),
        uri: uri.to_string(),
        images: images(&d["coverArt"]),
        duration_ms: duration_ms(d),
        release_date: iso_day(&d["releaseDate"]),
        description: text(&d["description"]),
        explicit: explicit(d),
    })
}

fn audiobook(d: &Value) -> Option<Audiobook> {
    let uri = d["uri"].as_str()?;
    // Los autores llegan como lista o como {items: [...]}, según la versión.
    let authors = d["authors"]
        .as_array()
        .or_else(|| d["authors"]["items"].as_array())
        .into_iter()
        .flatten()
        .map(|a| Author { name: name_of(a) })
        .filter(|a| !a.name.is_empty())
        .collect();
    Some(Audiobook {
        id: last_seg(uri),
        name: text(&d["name"]),
        uri: uri.to_string(),
        images: images(&d["coverArt"]),
        authors,
        publisher: name_of(&d["publisher"]),
        description: text(&d["description"]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Una respuesta real (consulta «cafe tacvba»). Está en qa/, que no se publica: sin ella la
    /// prueba se salta.
    #[test]
    fn mapea_la_muestra_real() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/qa/reports/pathfinder_sample.json");
        let Ok(raw) = std::fs::read_to_string(path) else {
            eprintln!("sin {path}: se omite");
            return;
        };
        let v: Value = serde_json::from_str(&raw).unwrap();
        assert!(!unknown_hash(Some(&v), &raw));
        let (r, total, mapped) = map_search(&v).expect("data.searchV2");
        assert!(total > 0);
        assert_eq!(total, mapped, "todo lo de la muestra es de un tipo conocido");

        let tracks = r.tracks.as_ref().unwrap();
        assert_eq!(tracks.items.len(), 10);
        let t = &tracks.items[0];
        assert_eq!(t.uri, "spotify:track:7s41ZGjQB5Ur8T0fQlk5uM");
        assert_eq!(t.id.as_deref(), Some("7s41ZGjQB5Ur8T0fQlk5uM"));
        assert_eq!(t.name, "Quiero Ver");
        assert_eq!(t.duration_ms, 205_573);
        assert_eq!(t.kind.as_deref(), Some("track"));
        assert_eq!(t.is_playable, Some(true));
        assert_eq!(t.artists[0].id.as_deref(), Some("09xj0S68Y1OU1vHMCZAIvz"));
        let al = t.album.as_ref().unwrap();
        assert_eq!(al.id.as_deref(), Some("4yLlkceQbuq64zdFnxdCYB"));
        assert!(t.cover(300).is_some());
        for t in &tracks.items {
            assert!(t.id.is_some() && !t.name.is_empty() && !t.artists.is_empty());
        }

        // Sin id la página no dibuja el álbum (pages.rs): todos deben traerlo.
        let albums: Vec<&AlbumRef> = r.albums.as_ref().unwrap().items.iter().flatten().collect();
        assert_eq!(albums.len(), 10);
        assert!(albums.iter().all(|a| a.id.is_some() && a.year().len() == 4 && !a.images.is_empty()));
        assert_eq!(albums[0].year(), "1996");
        assert_eq!(albums[0].album_type.as_deref(), Some("album"));
        assert_eq!(r.albums.as_ref().unwrap().total, 46);

        let artists: Vec<&Artist> = r.artists.as_ref().unwrap().items.iter().flatten().collect();
        assert_eq!(artists[0].id, "09xj0S68Y1OU1vHMCZAIvz");
        assert!(artists[0].cover(300).is_some());

        let playlists: Vec<&Playlist> = r.playlists.as_ref().unwrap().items.iter().flatten().collect();
        assert_eq!(playlists.len(), 10);
        assert_eq!(playlists[0].id, "37i9dQZF1DZ06evO01g8Cs");
        assert_eq!(playlists[0].owner.id.as_deref(), Some("spotify"));
        assert_eq!(playlists[0].owner_name(), "Spotify");
        assert!(playlists.iter().all(|p| !p.id.is_empty() && p.owner.id.is_some()));

        let shows: Vec<&Show> = r.shows.as_ref().unwrap().items.iter().flatten().collect();
        assert_eq!(shows.len(), 4);
        assert!(shows.iter().all(|s| !s.id.is_empty() && !s.publisher.is_empty()));

        let episodes: Vec<&Episode> = r.episodes.as_ref().unwrap().items.iter().flatten().collect();
        assert_eq!(episodes.len(), 10);
        assert_eq!(episodes[0].id, "3vG72upHSSmtM1whYOjvLi");
        assert_eq!(episodes[0].release_date.as_deref(), Some("2023-04-04"));
        assert_eq!(episodes[0].duration_ms, 2_519_903);

        assert!(r.audiobooks.as_ref().unwrap().items.is_empty());
    }

    #[test]
    fn salta_lo_que_no_es_la_entidad() {
        let v = json!({"data": {"searchV2": {
            "tracksV2": {"totalCount": 2, "items": [
                {"item": {"__typename": "NotFound"}},
                {"item": {"__typename": "TrackResponseWrapper", "data": {"__typename": "Track", "uri": "spotify:track:T1",
                    "name": "x", "contentRating": {"label": "EXPLICIT"}, "artists": {"items": [{"uri": "spotify:artist:A1", "profile": {"name": "A"}}]}}}}
            ]},
            "albumsV2": {"items": [{"__typename": "AlbumResponseWrapper", "data": {"__typename": "Album", "uri": "spotify:album:B1",
                "name": "b", "date": {"year": "2001"}, "type": "EP"}}]},
            "episodes": {"items": [{"__typename": "EpisodeOrChapterResponseWrapper", "data": {"__typename": "Chapter", "uri": "spotify:episode:C1"}}]},
            "playlists": {"items": [{"__typename": "PlaylistResponseWrapper", "data": {"__typename": "Playlist",
                "uri": "spotify:user:u:playlist:P1", "name": "p", "ownerV2": {"data": {"name": "Ü", "uri": "spotify:user:u"}}}}]},
            "audiobooks": {"items": [{"data": {"__typename": "Audiobook", "uri": "spotify:audiobook:L1", "name": "l",
                "authors": [{"name": "Autora"}], "coverArt": {"sources": [{"url": "https://i/x", "width": 300, "height": 300}]}}}]}
        }}});
        let (r, total, mapped) = map_search(&v).unwrap();
        assert_eq!((total, mapped), (6, 4));
        let t = &r.tracks.as_ref().unwrap().items;
        assert_eq!(t.len(), 1);
        assert!(t[0].explicit && t[0].album.is_none());
        assert_eq!(t[0].artists[0].id.as_deref(), Some("A1"));
        let a = r.albums.as_ref().unwrap().items[0].as_ref().unwrap();
        assert_eq!((a.year(), a.album_type.as_deref()), ("2001", Some("single")));
        assert!(r.episodes.as_ref().unwrap().items.is_empty());
        let p = r.playlists.as_ref().unwrap().items[0].as_ref().unwrap();
        assert_eq!((p.id.as_str(), p.uri.as_str(), p.owner.id.as_deref()), ("P1", "spotify:playlist:P1", Some("u")));
        let b = r.audiobooks.as_ref().unwrap().items[0].as_ref().unwrap();
        assert_eq!((b.id.as_str(), b.authors_str().as_str()), ("L1", "Autora"));
        assert!(r.shows.is_none() && r.artists.is_none());
    }

    #[test]
    fn errores_y_hash_desconocido() {
        assert!(map_search(&json!({"errors": [{"message": "x"}], "data": null})).is_none());
        let pq = json!({"errors": [{"message": "PersistedQueryNotFound", "extensions": {"code": "PERSISTED_QUERY_NOT_FOUND"}}]});
        assert!(unknown_hash(Some(&pq), ""));
        assert!(unknown_hash(None, "<html>PersistedQueryNotFound</html>"));
        // Una canción con ese nombre no lo es.
        let song = json!({"data": {"searchV2": {"tracksV2": {"items": [{"item": {"data": {"name": "PersistedQueryNotFound"}}}]}}}});
        assert!(!unknown_hash(Some(&song), &song.to_string()));
    }

    #[test]
    fn una_consulta_que_spotify_no_puede_ejecutar_no_aparta_pathfinder() {
        // Lo que devolvió con 200 «a» seguidas (7 oct 2026).
        let fallo = json!({"errors": [{"extensions": {"classification": "DataFetchingException", "service": "oxygen-search"},
            "locations": [{"column": 363, "line": 1}], "message": "Exception while fetching data (/searchV2)"}], "data": null});
        assert!(map_search(&fallo).is_none());
        assert!(query_failed(&fallo));
        // Un cambio de esquema (validación) o una respuesta sin errores sí lo apartan.
        let esquema = json!({"errors": [{"extensions": {"classification": "ValidationError"}, "message": "Field 'x' undefined"}], "data": null});
        assert!(!query_failed(&esquema));
        let mixto = json!({"errors": [{"extensions": {"classification": "DataFetchingException"}}, {"extensions": {"classification": "ValidationError"}}]});
        assert!(!query_failed(&mixto));
        assert!(!query_failed(&json!({"data": null})));
    }

    #[test]
    fn hash_en_el_reproductor_web() {
        let h = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let js = format!(r#"a=new o("searchUsers","query","{}",null),b=new o("searchDesktop","query","{h}",null)"#, "f".repeat(64));
        assert_eq!(search_hash(&js).as_deref(), Some(h));
        assert_eq!(search_hash(r#"("searchDesktop","query","abc")"#), None);

        let html = format!(r#"<script src="{CDN}vendor~web-player.1.js"></script><script src="{CDN}web-player.abc.js"></script><link href="{CDN}web-player.abc.css">"#);
        assert_eq!(script_urls(&html), vec![format!("{CDN}web-player.abc.js"), format!("{CDN}vendor~web-player.1.js")]);

        let bundle = r#"{12:"xpui-routes-search",312:"xpui-routes-album"}[e]+"."+{12:"0a1b2c3d",312:"ffffffff"}[e]+".js",{12:"99887766"}"#;
        assert_eq!(route_chunks(bundle, "xpui-routes-search"), vec!["xpui-routes-search.0a1b2c3d.js", "xpui-routes-search.99887766.js"]);
    }
}
