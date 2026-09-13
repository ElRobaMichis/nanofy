//! Token propio para la Web API con la app de desarrollador del usuario (OAuth 2.0 + PKCE).
//!
//! El token de librespot (client id del cliente oficial) vale para reproducir y para los
//! endpoints internos, pero Spotify limita globalmente ese client id en `api.spotify.com`
//! (responde 429 siempre). Con un Client ID propio, registrado gratis en
//! developer.spotify.com, cada usuario tiene su propia cuota.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

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
        let client = librespot_oauth::OAuthClientBuilder::new(&client_id, &self.redirect, SCOPES.to_vec())
            .open_in_browser()
            .with_custom_message("Listo. Ya puedes cerrar esta pestaña y volver a Nanofy.")
            .build()
            .map_err(|e| e.to_string())?;
        let token = client.get_access_token().map_err(|e| e.to_string())?;
        let stored = Stored {
            client_id: client_id.to_string(),
            refresh_token: token.refresh_token.clone(),
        };
        self.save(&stored);
        *self.stored.lock().unwrap() = Some(stored);
        let valid = token.expires_at.saturating_duration_since(Instant::now());
        self.cache_token(&token.access_token, valid);
        *self.live.lock().unwrap() = Some(Live {
            access_token: token.access_token,
            expires_at: token.expires_at - Duration::from_secs(30),
        });
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
