use std::{collections::HashMap, io::Write, sync::Mutex, time::Duration};

use byteorder::{BigEndian, ByteOrder, WriteBytesExt};
use bytes::Bytes;
use thiserror::Error;
use tokio::sync::oneshot;

use crate::{
    Error, FileId, SpotifyId,
    key_policy::{self, Attempt, Lru},
    packet::PacketType,
    util::SeqGenerator,
};

#[derive(Debug, Hash, PartialEq, Eq, Copy, Clone)]
pub struct AudioKey(pub [u8; 16]);

#[derive(Debug, Error)]
pub enum AudioKeyError {
    /// Spotify negó la clave; lleva su código (0x0002 es una negativa pasajera, ver
    /// `key_policy::TRANSIENT_DENIAL`).
    #[error("audio key error {0:#06x}")]
    AesKey(u16),
    #[error("other end of channel disconnected")]
    Channel,
    #[error("unexpected packet type {0}")]
    Packet(u8),
    #[error("sequence {0} not pending")]
    Sequence(u32),
    #[error("audio key response timeout")]
    Timeout,
}

impl From<AudioKeyError> for Error {
    fn from(err: AudioKeyError) -> Self {
        match err {
            AudioKeyError::AesKey(_) => Error::unavailable(err),
            AudioKeyError::Channel => Error::aborted(err),
            AudioKeyError::Sequence(_) => Error::aborted(err),
            AudioKeyError::Packet(_) => Error::unimplemented(err),
            AudioKeyError::Timeout => Error::aborted(err),
        }
    }
}

impl AudioKeyError {
    /// El `AudioKeyError` que lleva dentro un `Error` del crate, si lo lleva.
    pub fn of(error: &Error) -> Option<&AudioKeyError> {
        error.error.downcast_ref::<AudioKeyError>()
    }
}

/// Cómo terminó una petición de clave fallida, para decidir si se reintenta (`key_policy`) y,
/// en el reproductor, si el fallo es pasajero.
pub fn classify(error: &Error) -> Attempt {
    match AudioKeyError::of(error) {
        Some(AudioKeyError::AesKey(code)) => Attempt::Denied(*code),
        Some(AudioKeyError::Timeout) => Attempt::Timeout,
        _ => Attempt::Other,
    }
}

const KEY_RESPONSE_TIMEOUT: Duration = Duration::from_millis(1500);

/// Claves ya concedidas, por (canción, fichero): solo en memoria y para todo el proceso, de modo
/// que repetir, volver atrás, recargar tras «Reiniciar» o retomar tras una reconexión (una sesión
/// nueva) no vuelvan a pedirla. Nunca se guardan en disco: son claves del DRM de Spotify.
static KEY_CACHE: Mutex<Lru<(SpotifyId, FileId), AudioKey>> =
    Mutex::new(Lru::new(key_policy::CACHE_CAP));

fn key_cache() -> std::sync::MutexGuard<'static, Lru<(SpotifyId, FileId), AudioKey>> {
    KEY_CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

/// La clave de este fichero si ya se concedió antes en este proceso.
pub fn cached_key(track: SpotifyId, file: FileId) -> Option<AudioKey> {
    key_cache().get(&(track, file))
}

/// Olvida la clave de un fichero (p. ej. si con ella el fichero no se pudo decodificar).
pub fn forget_key(track: SpotifyId, file: FileId) {
    key_cache().remove(&(track, file));
}

/// Olvida todas las claves (al cerrar sesión).
pub fn forget_cached_keys() {
    key_cache().clear();
}

component! {
    AudioKeyManager : AudioKeyManagerInner {
        sequence: SeqGenerator<u32> = SeqGenerator::new(0),
        pending: HashMap<u32, oneshot::Sender<Result<AudioKey, Error>>> = HashMap::new(),
    }
}

/// Quita de `pending` la petición de un intento al terminar, también si se abandona (el futuro se
/// suelta: otra canción, tiempo agotado). Sin esto, cada respuesta que no llegaba dejaba su
/// entrada para siempre.
struct PendingGuard<'a> {
    manager: &'a AudioKeyManager,
    seq: u32,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        let seq = self.seq;
        self.manager.lock(|inner| {
            inner.pending.remove(&seq);
        });
    }
}

impl AudioKeyManager {
    pub(crate) fn dispatch(&self, cmd: PacketType, mut data: Bytes) -> Result<(), Error> {
        let seq = BigEndian::read_u32(data.split_to(4).as_ref());

        let sender = self
            .lock(|inner| inner.pending.remove(&seq))
            .ok_or(AudioKeyError::Sequence(seq))?;

        match cmd {
            PacketType::AesKey => {
                let mut key = [0u8; 16];
                key.copy_from_slice(data.as_ref());
                sender
                    .send(Ok(AudioKey(key)))
                    .map_err(|_| AudioKeyError::Channel)?
            }
            PacketType::AesKeyError => {
                // Los dos primeros bytes son el código («error audio key 0 2» = 0x0002).
                let bytes = data.as_ref();
                let code = match bytes {
                    [hi, lo, ..] => u16::from_be_bytes([*hi, *lo]),
                    [lo] => u16::from(*lo),
                    [] => 0,
                };
                error!(
                    "error audio key {:x} {:x}",
                    bytes.first().copied().unwrap_or(0),
                    bytes.get(1).copied().unwrap_or(0)
                );
                crate::ttfs::count(crate::ttfs::Counter::KeyErrors);
                if code == key_policy::TRANSIENT_DENIAL {
                    key_policy::note_transient_denial();
                }
                sender
                    .send(Err(AudioKeyError::AesKey(code).into()))
                    .map_err(|_| AudioKeyError::Channel)?
            }
            _ => {
                trace!("Did not expect {cmd:?} AES key packet with data {data:#?}");
                return Err(AudioKeyError::Packet(cmd as u8).into());
            }
        }

        Ok(())
    }

    /// La clave de un fichero: de la caché si ya se concedió en este proceso; si no, se pide a
    /// Spotify con hasta 3 intentos (1 s entre ellos) cuando la niega con el código pasajero o
    /// no contesta a tiempo (`key_policy`). Cualquier otro código vuelve al momento.
    pub async fn request(&self, track: SpotifyId, file: FileId) -> Result<AudioKey, Error> {
        if let Some(key) = cached_key(track, file) {
            trace!("Audio key from the in-memory cache");
            return Ok(key);
        }
        let key = key_policy::with_retries(
            key_policy::ATTEMPTS,
            key_policy::RETRY_DELAY,
            classify,
            |_| self.request_once(track, file),
        )
        .await?;
        key_cache().put((track, file), key);
        Ok(key)
    }

    /// La clave por adelantado (precarga inteligente de Nanofy, solo para un fichero que ya está
    /// en la caché del disco: es el único caso en que la clave queda en el camino del primer
    /// sonido). Un solo intento, sin los reintentos de `request`: si Spotify la niega, su freno
    /// (`key_policy`) para toda precarga y la carga de verdad la pedirá con sus reintentos.
    pub async fn prefetch(&self, track: SpotifyId, file: FileId) -> Result<(), Error> {
        if cached_key(track, file).is_some() {
            return Ok(());
        }
        let key = self.request_once(track, file).await?;
        key_cache().put((track, file), key);
        Ok(())
    }

    /// Un solo intento: envía la petición y espera la respuesta como mucho 1,5 s.
    async fn request_once(&self, track: SpotifyId, file: FileId) -> Result<AudioKey, Error> {
        // Pruebas (`NANOFY_FAULT=key_err:N|key_timeout:N`): la clave se niega o no llega sin
        // pedirla de verdad, para no gastar el cupo de claves que Spotify vigila. Cada intento
        // gasta una, como si el servidor contestara.
        if crate::fault::key_error() {
            error!("error audio key 0 2 (NANOFY_FAULT)");
            crate::ttfs::count(crate::ttfs::Counter::KeyErrors);
            key_policy::note_transient_denial();
            return Err(AudioKeyError::AesKey(key_policy::TRANSIENT_DENIAL).into());
        }
        if crate::fault::key_timeout() {
            tokio::time::sleep(KEY_RESPONSE_TIMEOUT).await;
            error!("Audio key response timeout (NANOFY_FAULT)");
            crate::ttfs::count(crate::ttfs::Counter::KeyTimeouts);
            return Err(AudioKeyError::Timeout.into());
        }

        let (tx, rx) = oneshot::channel();

        let seq = self.lock(move |inner| {
            let seq = inner.sequence.get();
            inner.pending.insert(seq, tx);
            seq
        });
        let _pending = PendingGuard { manager: self, seq };

        self.send_key_request(seq, track, file)?;
        match tokio::time::timeout(KEY_RESPONSE_TIMEOUT, rx).await {
            Err(_) => {
                error!("Audio key response timeout");
                crate::ttfs::count(crate::ttfs::Counter::KeyTimeouts);
                Err(AudioKeyError::Timeout.into())
            }
            Ok(k) => k?,
        }
    }

    fn send_key_request(&self, seq: u32, track: SpotifyId, file: FileId) -> Result<(), Error> {
        let mut data: Vec<u8> = Vec::new();
        data.write_all(&file.0)?;
        data.write_all(&track.to_raw())?;
        data.write_u32::<BigEndian>(seq)?;
        data.write_u16::<BigEndian>(0x0000)?;

        self.session().send_packet(PacketType::RequestKey, data)
    }
}
