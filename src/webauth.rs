//! Token de la Web API (biblioteca, búsqueda, Me gusta, perfiles) con OAuth 2.0 + PKCE.
//!
//! El token de librespot (client id del cliente oficial) vale para reproducir y para los
//! endpoints internos, pero Spotify limita globalmente ese client id en `api.spotify.com`
//! (responde 429 siempre). Por eso la biblioteca usa una segunda autorización con una identidad
//! de primera parte (`WEB_CLIENT_ID`): nadie tiene que crear ninguna app, solo aceptar en la web
//! de Spotify. Tras el primer inicio de sesión se encadena sola en la misma pestaña
//! (`prepare_connect` + `login_page`), así que basta con un «Iniciar sesión con Spotify».
//! Quien ya tenga una app de desarrollador puede añadirla además para las lecturas
//! (`load_personal`, en «Avanzado» de Ajustes); no hace falta para nada.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use oauth2::basic::BasicClient;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, CsrfToken, EndpointNotSet, EndpointSet, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, Scope, TokenResponse, TokenUrl,
};
use serde::{Deserialize, Serialize};

/// Identidad de primera parte de Spotify (el mismo tipo de client id que usan clientes como
/// fastpotify o librespot) con su redirección de loopback registrada. Una app de desarrollador
/// propia queda limitada por Spotify en «modo desarrollo» (403 al dar Me gusta, seguir, etc.);
/// esta identidad no tiene esa restricción, así que todas las escrituras funcionan.
pub const WEB_CLIENT_ID: &str = "d420a117a32841c2b3474932e49fb54b";
pub const REDIRECT_URI: &str = "http://127.0.0.1:8989/login";

pub const SCOPES: &[&str] = &[
    "playlist-modify-private",
    "playlist-modify-public",
    "playlist-read-collaborative",
    "playlist-read-private",
    "ugc-image-upload",
    "user-follow-modify",
    "user-follow-read",
    "user-library-modify",
    "user-library-read",
    "user-modify-playback-state",
    "user-read-playback-position",
    "user-read-playback-state",
    "user-read-private",
    "user-read-recently-played",
    "user-top-read",
];

#[derive(Serialize, Deserialize, Clone, Default)]
struct Stored {
    client_id: String,
    refresh_token: String,
}

struct Live {
    access_token: String,
    expires_at: Instant,
}

/// Token de acceso persistido junto al refresh token (expiración en segundos Unix).
#[derive(Serialize, Deserialize, Clone, Default)]
struct CachedToken {
    access_token: String,
    expires_unix: u64,
}

pub const PERSONAL_REDIRECT_URI: &str = "http://127.0.0.1:8899/callback";

const AUTH_URL: &str = "https://accounts.spotify.com/authorize";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";

/// Plazo de una autorización preparada (`prepare_connect`): lo que puede durar el inicio de
/// sesión (que se corta solo a los 5 min) más otro tanto para aceptar la de la biblioteca.
/// Pasado, se suelta el puerto y la interfaz vuelve a ofrecer «Conectar con Spotify».
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Cada cuánto mira el hilo que espera la vuelta del navegador si se canceló o caducó. Con una
/// espera bloqueante solo se podría cortar conectándose al puerto, y eso podría cortar por error
/// otra autorización que ya escuchase en él.
const ACCEPT_POLL: Duration = Duration::from_millis(100);

/// Fin de una autorización preparada que canceló la propia interfaz: ella ya lo sabe, no se avisa.
pub const CONNECT_CANCELLED: &str = "autorización cancelada";
/// La autorización preparada caducó sin que el navegador volviera (`CONNECT_TIMEOUT`).
pub const CONNECT_EXPIRED: &str = "la autorización caducó sin respuesta del navegador";

const PAGE_DONE: &str = "Listo. Ya puedes cerrar esta pestaña y volver a Nanofy.";
const PAGE_LOGIN_CANCELLED: &str = "Inicio de sesión cancelado. Ya puedes cerrar esta pestaña.";
const PAGE_DENIED: &str = "No se ha conectado tu biblioteca. Puedes hacerlo cuando quieras desde Nanofy con «Conectar con Spotify».";
const PAGE_STALE: &str = "Este enlace ya no vale. Vuelve a Nanofy y pulsa «Conectar con Spotify».";

type SpotifyClient = BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

/// Autorización de primera parte preparada: el puerto de vuelta ya escucha y la URL (con PKCE)
/// ya existe, así que se puede encadenar al inicio de sesión o abrir en el navegador ANTES de
/// esperar la respuesta (`WebAuth::finish_connect`, en su propio hilo).
pub struct PendingConnect {
    listener: TcpListener,
    /// Ruta de la redirección («/login»): otras peticiones al puerto (el favicon) no cuentan.
    path: String,
    client: SpotifyClient,
    client_id: String,
    verifier: PkceCodeVerifier,
    state: String,
    url: String,
    cancel: Arc<AtomicBool>,
    deadline: Instant,
}

impl PendingConnect {
    /// Lo que guarda la interfaz de esta autorización mientras espera.
    pub fn handle(&self) -> WebChain {
        WebChain {
            url: self.url.clone(),
            cancel: self.cancel.clone(),
        }
    }
}

/// Autorización de la biblioteca en curso, vista desde la interfaz: la URL (para abrirla otra
/// vez si se cerró la pestaña) y el interruptor para cancelarla.
pub struct WebChain {
    pub url: String,
    cancel: Arc<AtomicBool>,
}

impl WebChain {
    /// El hilo que espera la suelta en menos de `ACCEPT_POLL` y termina sin avisar.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Abre una URL en el navegador predeterminado sin esperar a que arranque.
pub fn open_in_browser(url: &str) {
    if let Err(e) = open::that_detached(url) {
        log::warn!("no se pudo abrir el navegador: {e}");
    }
}

/// Lo que trajo una petición al puerto de vuelta.
#[derive(Debug, PartialEq)]
enum Callback {
    /// Spotify devolvió el código de esta autorización.
    Code(String),
    /// La persona no aceptó (o Spotify devolvió otro error) en esta autorización.
    Denied(String),
    /// La ruta es la de vuelta pero no es de esta autorización (una pestaña vieja): se avisa en
    /// el navegador y se sigue esperando.
    Stale,
    /// Otra cosa (favicon, una conexión muda): se sigue esperando.
    Ignore,
}

/// Interpreta la primera línea de una petición HTTP al puerto de vuelta. Sin el `state` de esta
/// autorización no se acepta nada: así una pestaña vieja o una página cualquiera que apunte al
/// puerto no pueden colar un código ni cancelarla.
fn parse_callback(request_line: &str, path: &str, state: &str) -> Callback {
    let mut parts = request_line.split_whitespace();
    if parts.next() != Some("GET") {
        return Callback::Ignore;
    }
    let Some(target) = parts.next().filter(|t| t.starts_with('/')) else {
        return Callback::Ignore;
    };
    let Ok(url) = oauth2::url::Url::parse(&format!("http://127.0.0.1{target}")) else {
        return Callback::Ignore;
    };
    if url.path() != path {
        return Callback::Ignore;
    }
    let (mut code, mut error, mut got_state) = (None, None, None);
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            "state" => got_state = Some(v.into_owned()),
            _ => {}
        }
    }
    if got_state.as_deref() != Some(state) {
        return Callback::Stale;
    }
    match (code.filter(|c| !c.is_empty()), error) {
        (Some(c), _) => Callback::Code(c),
        (None, Some(e)) => Callback::Denied(e),
        (None, None) => Callback::Stale,
    }
}

/// Escapa un texto para meterlo en un atributo HTML entre comillas dobles.
fn html_attr(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

const PAGE_STYLE: &str = "body{font-family:system-ui,sans-serif;background:#121212;color:#fff;margin:0;\
min-height:100vh;display:flex;align-items:center;justify-content:center;text-align:center;padding:0 24px}\
a{color:#1ed760}";

/// Página mínima con un solo mensaje (vuelta de la autorización de la biblioteca).
fn simple_page(text: &str) -> String {
    format!(
        "<!doctype html><html lang=\"es\"><head><meta charset=\"utf-8\"><title>Nanofy</title>\
         <style>{PAGE_STYLE}</style></head><body><p>{text}</p></body></html>"
    )
}

/// Página que muestra el inicio de sesión de librespot (127.0.0.1:8898) al volver de Spotify.
/// librespot-oauth la envía tal cual, sin Content-Type: el navegador la reconoce como HTML por el
/// `<!doctype html>` del principio, y por eso también el `<meta charset>` (sin él, «…» y las
/// tildes saldrían mal). La misma página vale para el éxito y para la cancelación, así que el
/// mensaje lo decide el script según haya `code` en la URL.
///
/// Con `next` (la autorización de la biblioteca ya preparada, `prepare_connect`), si Spotify
/// devolvió un código la misma pestaña salta a ella: un solo «Iniciar sesión con Spotify» deja
/// conectadas la reproducción y la biblioteca. Si se canceló, NO salta (una redirección `meta`
/// incondicional llevaría a la segunda autorización tras cancelar la primera); el enlace queda
/// por si el salto no ocurre.
pub fn login_page(next: Option<&str>) -> String {
    let script_cancel = format!("document.getElementById('m').textContent='{PAGE_LOGIN_CANCELLED}'");
    match next {
        Some(url) => {
            let href = html_attr(url);
            format!(
                "<!doctype html><html lang=\"es\"><head><meta charset=\"utf-8\"><title>Nanofy</title>\
                 <style>{PAGE_STYLE}</style></head><body><div><p id=\"m\">Un momento…</p>\
                 <p id=\"l\"><a id=\"a\" href=\"{href}\">Continuar: permite que Nanofy lea tu biblioteca</a></p></div>\
                 <script>if(/[?&]code=/.test(location.search)){{location.replace(document.getElementById('a').href)}}\
                 else{{{script_cancel};document.getElementById('l').remove()}}</script></body></html>"
            )
        }
        None => format!(
            "<!doctype html><html lang=\"es\"><head><meta charset=\"utf-8\"><title>Nanofy</title>\
             <style>{PAGE_STYLE}</style></head><body><p id=\"m\">{PAGE_DONE}</p>\
             <script>if(!/[?&]code=/.test(location.search)){{{script_cancel}}}</script></body></html>"
        ),
    }
}

/// Atiende una conexión al puerto de vuelta: lee la petición (con tope y plazo), contesta con
/// una página y devuelve lo que traía.
fn serve_callback(stream: TcpStream, path: &str, state: &str) -> Callback {
    // La conexión aceptada no hereda en todos los sistemas el modo sin bloqueo del puerto: se fija
    // en bloqueante y con plazo, para que una conexión muda (la pre-conexión de un navegador) no
    // retenga el hilo.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
    let mut reader = BufReader::new((&stream).take(16 * 1024));
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return Callback::Ignore;
    }
    // Las cabeceras se leen antes de contestar: cerrar con datos sin leer puede acabar en un
    // reset de la conexión, y el navegador mostraría un error en vez de la página.
    let mut header = String::new();
    loop {
        header.clear();
        match reader.read_line(&mut header) {
            Ok(n) if n > 0 && !header.trim().is_empty() => {}
            _ => break,
        }
    }
    let result = parse_callback(&line, path, state);
    let (status, body) = match &result {
        Callback::Code(_) => ("200 OK", simple_page(PAGE_DONE)),
        Callback::Denied(_) => ("200 OK", simple_page(PAGE_DENIED)),
        Callback::Stale => ("400 Bad Request", simple_page(PAGE_STALE)),
        Callback::Ignore => ("404 Not Found", String::new()),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = (&stream).write_all(response.as_bytes());
    let _ = (&stream).flush();
    result
}

/// Espera en el puerto de vuelta el código de esta autorización, mirando cada `ACCEPT_POLL` si
/// se canceló o caducó.
fn wait_for_code(p: &PendingConnect) -> Result<String, String> {
    loop {
        if p.cancel.load(Ordering::Relaxed) {
            return Err(CONNECT_CANCELLED.to_string());
        }
        if Instant::now() >= p.deadline {
            return Err(CONNECT_EXPIRED.to_string());
        }
        match p.listener.accept() {
            Ok((stream, _)) => match serve_callback(stream, &p.path, &p.state) {
                Callback::Code(code) => return Ok(code),
                Callback::Denied(e) if e == "access_denied" => return Err("la cancelaste en el navegador".to_string()),
                Callback::Denied(e) => return Err(format!("Spotify respondió «{e}»")),
                Callback::Stale | Callback::Ignore => {}
            },
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(ACCEPT_POLL),
            Err(e) => {
                log::warn!("autorización de la biblioteca: {e}");
                std::thread::sleep(ACCEPT_POLL);
            }
        }
    }
}

pub struct WebAuth {
    file: PathBuf,
    token_file: PathBuf,
    stored: Mutex<Option<Stored>>,
    live: Mutex<Option<Live>>,
    /// Client id fijo (proveedor de primera parte) o `None` (app propia del usuario: el id sale
    /// del grant guardado o del que se pasa al conectar).
    fixed_client_id: Option<String>,
    redirect: String,
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl WebAuth {
    /// Proveedor de PRIMERA PARTE (para escrituras y como respaldo de lectura).
    pub fn load(file: PathBuf) -> Self {
        Self::load_inner(file, "webapi_access.json", Some(WEB_CLIENT_ID.to_string()), REDIRECT_URI)
    }

    /// Proveedor de la app PROPIA del usuario (lecturas rápidas con su propia cuota). Opcional.
    pub fn load_personal(file: PathBuf) -> Self {
        Self::load_inner(file, "webapi_personal_access.json", None, PERSONAL_REDIRECT_URI)
    }

    fn load_inner(file: PathBuf, token_name: &str, fixed_client_id: Option<String>, redirect: &str) -> Self {
        let stored = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| serde_json::from_str::<Stored>(&t).ok())
            .filter(|s| !s.refresh_token.is_empty())
            // Para el proveedor de primera parte, un grant con otro client id (p. ej. una app de
            // desarrollador vieja) se descarta para forzar la reconexión correcta.
            .filter(|s| fixed_client_id.as_ref().map(|c| &s.client_id == c).unwrap_or(true));
        let token_file = file
            .parent()
            .map(|p| p.join(token_name))
            .unwrap_or_else(|| PathBuf::from(token_name));
        // Si el token guardado sigue vigente (con 60 s de margen), se reutiliza sin red.
        let live = std::fs::read_to_string(&token_file)
            .ok()
            .and_then(|t| serde_json::from_str::<CachedToken>(&t).ok())
            .filter(|c| !c.access_token.is_empty() && c.expires_unix > now_unix() + 60)
            .map(|c| Live {
                access_token: c.access_token,
                expires_at: Instant::now() + Duration::from_secs(c.expires_unix - now_unix() - 30),
            });
        Self {
            file,
            token_file,
            stored: Mutex::new(stored),
            live: Mutex::new(live),
            fixed_client_id,
            redirect: redirect.to_string(),
        }
    }

    /// Guarda el token de acceso en disco para reutilizarlo en el próximo arranque.
    fn cache_token(&self, access_token: &str, valid_for: Duration) {
        let cached = CachedToken {
            access_token: access_token.to_string(),
            expires_unix: now_unix() + valid_for.as_secs(),
        };
        if let Ok(text) = serde_json::to_string(&cached) {
            let _ = std::fs::write(&self.token_file, text);
        }
    }

    pub fn state_dir(&self) -> PathBuf {
        self.file.parent().map(|p| p.to_path_buf()).unwrap_or_default()
    }

    pub fn configured(&self) -> bool {
        self.stored.lock().unwrap().is_some()
    }

    fn save(&self, stored: &Stored) {
        if let Some(dir) = self.file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(stored) {
            if let Err(e) = std::fs::write(&self.file, text) {
                log::warn!("no se pudo guardar el token de la Web API: {e}");
            }
        }
    }

    /// Flujo OAuth completo en el navegador (bloqueante). Guarda el refresh token.
    /// El proveedor de primera parte ignora `client_id`; el personal usa el que se le pasa.
    pub fn connect(&self, client_id: &str) -> Result<(), String> {
        let client_id = match &self.fixed_client_id {
            Some(fixed) => fixed.clone(),
            None => {
                let c = client_id.trim();
                if c.len() < 16 {
                    return Err("Client ID no válido".to_string());
                }
                c.to_string()
            }
        };
        // Página HTML con su juego de caracteres: el texto suelto, sin Content-Type, salía con las
        // tildes rotas en algunos navegadores.
        let client = librespot_oauth::OAuthClientBuilder::new(&client_id, &self.redirect, SCOPES.to_vec())
            .open_in_browser()
            .with_custom_message(&simple_page(PAGE_DONE))
            .build()
            .map_err(|e| e.to_string())?;
        let token = client.get_access_token().map_err(|e| e.to_string())?;
        let valid = token.expires_at.saturating_duration_since(Instant::now());
        self.store_grant(&client_id, token.refresh_token, token.access_token, valid);
        Ok(())
    }

    /// Guarda un grant recién concedido: el refresh token en disco y el token de acceso en memoria
    /// y en disco (para el próximo arranque).
    fn store_grant(&self, client_id: &str, refresh_token: String, access_token: String, valid_for: Duration) {
        let stored = Stored {
            client_id: client_id.to_string(),
            refresh_token,
        };
        self.save(&stored);
        *self.stored.lock().unwrap() = Some(stored);
        self.cache_token(&access_token, valid_for);
        *self.live.lock().unwrap() = Some(Live {
            access_token,
            expires_at: Instant::now() + valid_for.saturating_sub(Duration::from_secs(30)),
        });
    }

    /// Prepara la autorización de primera parte sin abrir nada: escucha ya en el puerto de vuelta
    /// (127.0.0.1:8989), para que la redirección de Spotify nunca llegue antes que el servidor, y
    /// genera la URL con PKCE. Es otro puerto que el del inicio de sesión de librespot (8898), así
    /// que las dos esperas conviven y la página de vuelta del inicio de sesión puede saltar a esta
    /// (`login_page`). Solo para la identidad de primera parte: la app propia va por `connect`.
    pub fn prepare_connect(&self) -> Result<PendingConnect, String> {
        let client_id = self
            .fixed_client_id
            .clone()
            .ok_or_else(|| "solo para la identidad de primera parte".to_string())?;
        let redirect = oauth2::url::Url::parse(&self.redirect).map_err(|e| e.to_string())?;
        let addr = redirect
            .socket_addrs(|| None)
            .ok()
            .and_then(|a| a.into_iter().next())
            .ok_or_else(|| format!("redirección sin dirección: {}", self.redirect))?;
        let listener = TcpListener::bind(addr).map_err(|e| format!("no se pudo escuchar en {addr}: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let client = BasicClient::new(ClientId::new(client_id.clone()))
            .set_auth_uri(AuthUrl::new(AUTH_URL.to_string()).map_err(|e| e.to_string())?)
            .set_token_uri(TokenUrl::new(TOKEN_URL.to_string()).map_err(|e| e.to_string())?)
            .set_redirect_uri(RedirectUrl::new(self.redirect.clone()).map_err(|e| e.to_string())?);
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, state) = client
            .authorize_url(CsrfToken::new_random)
            .add_scopes(SCOPES.iter().map(|s| Scope::new(s.to_string())))
            .set_pkce_challenge(challenge)
            .url();
        Ok(PendingConnect {
            listener,
            path: redirect.path().to_string(),
            client,
            client_id,
            verifier,
            state: state.secret().clone(),
            url: url.to_string(),
            cancel: Arc::new(AtomicBool::new(false)),
            deadline: Instant::now() + CONNECT_TIMEOUT,
        })
    }

    /// Espera la vuelta del navegador de una autorización preparada y guarda el grant. Bloquea
    /// (minutos, mientras la persona está en el navegador): va en su propio hilo, nunca en los
    /// carriles de la API. Termina sola al cancelarla (`CONNECT_CANCELLED`) o al caducar
    /// (`CONNECT_EXPIRED`), y en todos los casos suelta el puerto.
    pub fn finish_connect(&self, p: PendingConnect) -> Result<(), String> {
        let code = wait_for_code(&p)?;
        let PendingConnect { listener, client, client_id, verifier, .. } = p;
        drop(listener);
        // Sin redirecciones: el intercambio del código solo habla con accounts.spotify.com.
        let http = oauth2::reqwest::blocking::Client::builder()
            .redirect(oauth2::reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| e.to_string())?;
        let token = client
            .exchange_code(AuthorizationCode::new(code))
            .set_pkce_verifier(verifier)
            .request(&http)
            .map_err(|e| format!("Spotify no aceptó la autorización ({e})"))?;
        let refresh = token.refresh_token().map(|t| t.secret().to_string()).unwrap_or_default();
        let valid = token.expires_in().unwrap_or(Duration::from_secs(3600));
        self.store_grant(&client_id, refresh, token.access_token().secret().to_string(), valid);
        log::info!("biblioteca autorizada en el navegador");
        Ok(())
    }

    pub fn disconnect(&self) {
        *self.stored.lock().unwrap() = None;
        *self.live.lock().unwrap() = None;
        let _ = std::fs::remove_file(&self.token_file);
        let _ = std::fs::remove_file(&self.file);
    }

    /// Token de acceso vigente, renovándolo si hace falta. `None` si no hay app configurada.
    pub fn access_token(&self) -> Result<Option<String>, String> {
        let Some(stored) = self.stored.lock().unwrap().clone() else {
            return Ok(None);
        };
        {
            let live = self.live.lock().unwrap();
            if let Some(l) = live.as_ref() {
                if Instant::now() < l.expires_at {
                    return Ok(Some(l.access_token.clone()));
                }
            }
        }
        let client = librespot_oauth::OAuthClientBuilder::new(
            &stored.client_id,
            &self.redirect,
            SCOPES.to_vec(),
        )
        .build()
        .map_err(|e| e.to_string())?;
        let token = client
            .refresh_token(&stored.refresh_token)
            .map_err(|e| format!("no se pudo renovar el token de la Web API: {e}"))?;
        if !token.refresh_token.is_empty() && token.refresh_token != stored.refresh_token {
            let updated = Stored {
                client_id: stored.client_id.clone(),
                refresh_token: token.refresh_token.clone(),
            };
            self.save(&updated);
            *self.stored.lock().unwrap() = Some(updated);
        }
        let access = token.access_token.clone();
        let valid = token.expires_at.saturating_duration_since(Instant::now());
        self.cache_token(&access, valid);
        *self.live.lock().unwrap() = Some(Live {
            access_token: token.access_token,
            expires_at: token.expires_at - Duration::from_secs(30),
        });
        Ok(Some(access))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Proveedor de primera parte en un puerto libre cualquiera (`:0`), con un archivo que no
    /// existe: estas pruebas nunca llegan a guardar nada ni a hablar con Spotify.
    fn test_auth() -> WebAuth {
        let file = std::env::temp_dir()
            .join(format!("nanofy-test-webauth-{}", std::process::id()))
            .join("webapi.json");
        WebAuth::load_inner(file, "webapi_test_access.json", Some(WEB_CLIENT_ID.to_string()), "http://127.0.0.1:0/login")
    }

    fn query(url: &str, key: &str) -> Option<String> {
        oauth2::url::Url::parse(url)
            .ok()?
            .query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    }

    /// Una petición como la del navegador al puerto de vuelta; devuelve la respuesta entera.
    fn get(port: u16, target: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(s, "GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nUser-Agent: prueba\r\n\r\n").unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    }

    #[test]
    fn la_vuelta_solo_vale_con_el_state_de_esta_autorizacion() {
        let st = "abc123";
        assert_eq!(parse_callback("GET /login?code=XYZ&state=abc123 HTTP/1.1", "/login", st), Callback::Code("XYZ".into()));
        assert_eq!(
            parse_callback("GET /login?error=access_denied&state=abc123 HTTP/1.1", "/login", st),
            Callback::Denied("access_denied".into())
        );
        // Otra autorización (pestaña vieja), sin state o sin nada: no se acepta ni se cancela.
        assert_eq!(parse_callback("GET /login?code=XYZ&state=otro HTTP/1.1", "/login", st), Callback::Stale);
        assert_eq!(parse_callback("GET /login?code=XYZ HTTP/1.1", "/login", st), Callback::Stale);
        assert_eq!(parse_callback("GET /login?error=access_denied HTTP/1.1", "/login", st), Callback::Stale);
        assert_eq!(parse_callback("GET /login?state=abc123&code= HTTP/1.1", "/login", st), Callback::Stale);
        // Lo que no es la ruta de vuelta se ignora.
        assert_eq!(parse_callback("GET /favicon.ico HTTP/1.1", "/login", st), Callback::Ignore);
        assert_eq!(parse_callback("POST /login?code=XYZ&state=abc123 HTTP/1.1", "/login", st), Callback::Ignore);
        assert_eq!(parse_callback("", "/login", st), Callback::Ignore);
        assert_eq!(parse_callback("GET * HTTP/1.1", "/login", st), Callback::Ignore);
        // El código llega decodificado.
        assert_eq!(parse_callback("GET /login?state=abc123&code=a%2Bb HTTP/1.1", "/login", st), Callback::Code("a+b".into()));
    }

    #[test]
    fn la_pagina_del_inicio_de_sesion_encadena_solo_si_hubo_codigo() {
        let next = "https://accounts.spotify.com/authorize?response_type=code&client_id=x&state=s\"<>";
        let page = login_page(Some(next));
        // HTML reconocible sin Content-Type y con su juego de caracteres.
        assert!(page.starts_with("<!doctype html>"));
        assert!(page.contains("<meta charset=\"utf-8\">"));
        // La URL va escapada en el atributo: ni un `&` suelto ni comillas que lo cierren.
        assert!(page.contains("authorize?response_type=code&amp;client_id=x&amp;state=s&quot;&lt;&gt;"));
        assert!(!page.contains("code&client_id"));
        // El salto depende de que la URL de vuelta traiga el código; sin él, se avisa.
        assert!(page.contains("if(/[?&]code=/.test(location.search)){location.replace("));
        assert!(page.contains(PAGE_LOGIN_CANCELLED));
        assert!(!page.contains("http-equiv=\"refresh\""), "una redirección incondicional saltaría también al cancelar");

        let plain = login_page(None);
        assert!(plain.starts_with("<!doctype html>"));
        assert!(plain.contains(PAGE_DONE));
        assert!(plain.contains(PAGE_LOGIN_CANCELLED));
        assert!(!plain.contains("location.replace"));
    }

    #[test]
    fn la_autorizacion_preparada_lleva_pkce_y_escucha_ya() {
        let auth = test_auth();
        let p = auth.prepare_connect().unwrap();
        let port = p.listener.local_addr().unwrap().port();
        assert_ne!(port, 0);
        let url = p.handle().url;
        assert!(url.starts_with(AUTH_URL));
        assert_eq!(query(&url, "client_id").as_deref(), Some(WEB_CLIENT_ID));
        assert_eq!(query(&url, "response_type").as_deref(), Some("code"));
        assert_eq!(query(&url, "code_challenge_method").as_deref(), Some("S256"));
        assert_eq!(query(&url, "redirect_uri").as_deref(), Some("http://127.0.0.1:0/login"));
        assert_eq!(query(&url, "state").as_deref(), Some(p.state.as_str()));
        assert!(query(&url, "code_challenge").is_some_and(|c| c.len() >= 43));
        assert!(query(&url, "scope").is_some_and(|s| s.contains("user-library-read")));
        // Ya escucha: la vuelta del navegador no puede llegar antes que el servidor.
        assert!(TcpStream::connect(("127.0.0.1", port)).is_ok());
        // La app propia del usuario no usa este camino.
        let personal = WebAuth::load_personal(std::env::temp_dir().join("nanofy-test-webauth-no-existe.json"));
        assert!(personal.prepare_connect().is_err());
    }

    #[test]
    fn rechazar_en_el_navegador_termina_la_espera_con_un_error_claro() {
        let auth = Arc::new(test_auth());
        let p = auth.prepare_connect().unwrap();
        let port = p.listener.local_addr().unwrap().port();
        let state = p.state.clone();
        let waiter = {
            let auth = auth.clone();
            std::thread::spawn(move || auth.finish_connect(p))
        };
        // Primero una pestaña vieja y el favicon: se contestan y se sigue esperando.
        let stale = get(port, "/login?code=VIEJO&state=otro");
        assert!(stale.starts_with("HTTP/1.1 400"));
        assert!(stale.contains("text/html; charset=utf-8"));
        assert!(get(port, "/favicon.ico").starts_with("HTTP/1.1 404"));
        assert!(!waiter.is_finished());
        let denied = get(port, &format!("/login?error=access_denied&state={state}"));
        assert!(denied.starts_with("HTTP/1.1 200"));
        assert!(denied.contains(PAGE_DENIED));
        let r = waiter.join().unwrap();
        assert_eq!(r, Err("la cancelaste en el navegador".to_string()));
        assert!(!auth.configured());
        // El puerto queda libre.
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
    }

    #[test]
    fn cancelar_o_caducar_suelta_el_puerto_enseguida() {
        let auth = Arc::new(test_auth());
        let p = auth.prepare_connect().unwrap();
        let port = p.listener.local_addr().unwrap().port();
        let chain = p.handle();
        let waiter = {
            let auth = auth.clone();
            std::thread::spawn(move || auth.finish_connect(p))
        };
        std::thread::sleep(Duration::from_millis(150));
        let t0 = Instant::now();
        chain.cancel();
        assert_eq!(waiter.join().unwrap(), Err(CONNECT_CANCELLED.to_string()));
        assert!(t0.elapsed() < Duration::from_secs(1), "tardó {:?}", t0.elapsed());
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err());

        let mut p = auth.prepare_connect().unwrap();
        p.deadline = Instant::now();
        assert_eq!(auth.finish_connect(p), Err(CONNECT_EXPIRED.to_string()));
    }
}
