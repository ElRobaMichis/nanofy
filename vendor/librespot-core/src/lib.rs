#[macro_use]
extern crate log;

use librespot_protocol as protocol;

#[macro_use]
mod component;

pub mod apresolve;
pub mod audio_key;
pub mod authentication;
pub mod cache;
pub mod cdn_url;
/// Caché de storage-resolve y orden de los servidores de la CDN (Nanofy).
pub mod cdn_policy;
pub mod channel;
pub mod config;
mod connection;
pub mod date;
#[allow(dead_code)]
pub mod dealer;
pub mod deserialize_with;
#[doc(hidden)]
pub mod diffie_hellman;
pub mod error;
/// Fallos simulados para las pruebas de Nanofy (`NANOFY_FAULT`); inerte sin la variable.
pub mod fault;
pub mod file_id;
pub mod http_client;
/// Reintentos, caché en memoria y freno de las claves de audio (Nanofy).
pub mod key_policy;
pub mod login5;
/// Caché en memoria de los metadatos que pide el reproductor, con lo que siembra Nanofy.
pub mod meta_cache;
pub mod mercury;
pub mod packet;
mod proxytunnel;
/// Plazo de cada intento de las peticiones a spclient (Nanofy).
pub mod request_policy;
pub mod session;
mod socket;
#[allow(dead_code)]
pub mod spclient;
pub mod spotify_id;
pub mod spotify_uri;
pub mod token;
/// Tiempo hasta el primer sonido por fases y contadores de fallos (instrumentación de Nanofy).
pub mod ttfs;
#[doc(hidden)]
pub mod util;
pub mod version;

pub use config::SessionConfig;
pub use error::Error;
pub use file_id::FileId;
pub use session::Session;
pub use spotify_id::SpotifyId;
pub use spotify_uri::SpotifyUri;
