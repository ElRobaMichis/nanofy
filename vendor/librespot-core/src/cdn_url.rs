use std::{
    cell::RefCell,
    ops::{Deref, DerefMut},
    sync::{Mutex, MutexGuard},
    time::Instant,
};

use protobuf::Message;
use thiserror::Error;
use time::Duration;
use url::Url;

use super::{
    Error, FileId, Session,
    cdn_policy::{self, HostPrefs, StorageCache},
    date::Date,
};

use librespot_protocol as protocol;
use protocol::storage_resolve::StorageResolveResponse as CdnUrlMessage;
use protocol::storage_resolve::storage_resolve_response::Result as StorageResolveResponse_Result;

#[derive(Debug, Clone)]
pub struct MaybeExpiringUrl(pub String, pub Option<Date>);

const CDN_URL_EXPIRY_MARGIN: Duration = Duration::seconds(5 * 60);

#[derive(Debug, Clone)]
pub struct MaybeExpiringUrls(pub Vec<MaybeExpiringUrl>);

impl Deref for MaybeExpiringUrls {
    type Target = Vec<MaybeExpiringUrl>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for MaybeExpiringUrls {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[derive(Debug, Error)]
pub enum CdnUrlError {
    #[error("all URLs expired")]
    Expired,
    #[error("resolved storage is not for CDN")]
    Storage,
    #[error("no URLs resolved")]
    Unresolved,
}

impl From<CdnUrlError> for Error {
    fn from(err: CdnUrlError) -> Self {
        match err {
            CdnUrlError::Expired => Error::deadline_exceeded(err),
            CdnUrlError::Storage | CdnUrlError::Unresolved => Error::unavailable(err),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CdnUrl {
    pub file_id: FileId,
    urls: MaybeExpiringUrls,
}

impl CdnUrl {
    pub fn new(file_id: FileId) -> Self {
        Self {
            file_id,
            urls: MaybeExpiringUrls(Vec::new()),
        }
    }

    /// Dónde está el fichero en la CDN. Lo ya resuelto en este proceso se reutiliza mientras sus
    /// URL no estén a punto de caducar (`cdn_policy`), y las URL vuelven ordenadas para probar
    /// primero el servidor que contestó la última vez y al final los que fallaron hace poco.
    pub async fn resolve_audio(&self, session: &Session) -> Result<Self, Error> {
        let file_id = self.file_id;
        let cached = storage().get(&file_id, Date::now_utc().as_timestamp_ms(), Instant::now());
        if let Some(urls) = cached {
            // Fase «storage» de la carga medida (si este es su hilo): sin petición.
            crate::ttfs::mark_thread("storage", Some("hit".into()));
            trace!("CDN storage of {file_id} from the in-memory cache");
            return Ok(Self {
                file_id,
                urls: ordered(urls),
            });
        }
        let response = session.spclient().get_audio_storage(&file_id).await?;
        crate::ttfs::mark_thread("storage", Some("miss".into()));
        let msg = CdnUrlMessage::parse_from_bytes(&response)?;
        let urls = MaybeExpiringUrls::try_from(msg)?;
        if !urls.is_empty() {
            storage().put(file_id, urls.clone(), urls.min_expiry_ms(), Instant::now());
        }

        let cdn_url = Self {
            file_id,
            urls: ordered(urls),
        };

        trace!("Resolved CDN storage: {cdn_url:#?}");

        Ok(cdn_url)
    }

    /// ¿Ya se sabe dónde está el fichero (y sus URL aún sirven)? Sin pedir nada.
    pub fn is_resolved(file_id: FileId) -> bool {
        storage().contains(&file_id, Date::now_utc().as_timestamp_ms(), Instant::now())
    }

    /// Olvida dónde estaba el fichero: todas sus URL fallaron (o una caducó antes de tiempo), así
    /// que la próxima carga vuelve a preguntar.
    pub fn forget(file_id: FileId) {
        storage().forget(&file_id);
    }

    /// Olvida todo lo resuelto (al cerrar sesión).
    pub fn forget_all() {
        storage().clear();
    }

    /// Ficheros con la ubicación guardada (para el modo de control).
    pub fn resolved_count() -> usize {
        storage().len()
    }

    #[deprecated = "This function only returns the first valid URL. Use try_get_urls instead, which allows for fallback logic."]
    pub fn try_get_url(&self) -> Result<&str, Error> {
        if self.urls.is_empty() {
            return Err(CdnUrlError::Unresolved.into());
        }

        let now = Date::now_utc();
        let url = self.urls.iter().find(|url| match url.1 {
            Some(expiry) => now < expiry,
            None => true,
        });

        if let Some(url) = url {
            Ok(&url.0)
        } else {
            Err(CdnUrlError::Expired.into())
        }
    }

    pub fn try_get_urls(&self) -> Result<Vec<&str>, Error> {
        if self.urls.is_empty() {
            return Err(CdnUrlError::Unresolved.into());
        }

        let now = Date::now_utc();
        let urls: Vec<&str> = self
            .urls
            .iter()
            .filter_map(|MaybeExpiringUrl(url, expiry)| match *expiry {
                Some(expiry) => {
                    if now < expiry {
                        Some(url.as_str())
                    } else {
                        None
                    }
                }
                None => Some(url.as_str()),
            })
            .collect();

        if urls.is_empty() {
            Err(CdnUrlError::Expired.into())
        } else {
            Ok(urls)
        }
    }
}

impl MaybeExpiringUrls {
    /// Caducidad (ms desde 1970, ya con el margen) de la URL que antes caduca; `None` si ninguna
    /// la trae.
    fn min_expiry_ms(&self) -> Option<i64> {
        self.iter()
            .filter_map(|MaybeExpiringUrl(_, expiry)| expiry.map(|e| e.as_timestamp_ms()))
            .min()
    }
}

/// Ubicaciones ya resueltas, por fichero (ver `CdnUrl::resolve_audio`).
static STORAGE: Mutex<StorageCache<FileId, MaybeExpiringUrls>> =
    Mutex::new(StorageCache::new(cdn_policy::CAP));
/// Qué servidores de la CDN contestaron y cuáles fallaron (ver `end_open`).
static HOSTS: Mutex<HostPrefs> = Mutex::new(HostPrefs::new());

fn storage() -> MutexGuard<'static, StorageCache<FileId, MaybeExpiringUrls>> {
    STORAGE.lock().unwrap_or_else(|e| e.into_inner())
}

fn hosts() -> MutexGuard<'static, HostPrefs> {
    HOSTS.lock().unwrap_or_else(|e| e.into_inner())
}

/// El servidor de la CDN que contestó la última vez (para el modo de control de Nanofy).
pub fn last_good_host() -> Option<String> {
    hosts().last_good().map(str::to_string)
}

/// Las URL en el orden en que conviene probarlas (`HostPrefs::order`).
fn ordered(mut urls: MaybeExpiringUrls) -> MaybeExpiringUrls {
    hosts().order(&mut urls.0, |u| u.0.as_str(), Instant::now());
    urls
}

thread_local! {
    /// Servidores de la CDN probados por este hilo desde `begin_open` (`None`: no se apunta). El
    /// reproductor abre cada fichero en un hilo propio y librespot-audio prueba las URL una tras
    /// otra en ese mismo hilo, así que lo apuntado aquí es justo lo que probó esa apertura; las
    /// descargas posteriores van en otros hilos y no se apuntan.
    static ATTEMPTS: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

/// Lo que se apunta como mucho por apertura (Spotify da 2-4 URL por fichero).
const ATTEMPTS_CAP: usize = 8;

/// Empieza a apuntar los servidores que prueba la apertura del fichero en este hilo.
pub fn begin_open() {
    ATTEMPTS.with(|a| *a.borrow_mut() = Some(Vec::new()));
}

/// spclient va a pedir un trozo a la CDN desde este hilo (ver `ATTEMPTS`).
pub(crate) fn note_attempt(host: Option<&str>) {
    ATTEMPTS.with(|a| {
        if let (Some(list), Some(host)) = (a.borrow_mut().as_mut(), host) {
            if list.len() < ATTEMPTS_CAP {
                list.push(host.to_string());
            }
        }
    });
}

/// Termina la apertura de `file_id` en este hilo: si se abrió (`ok`), el último servidor probado
/// es el bueno y los anteriores fallaron; si no, fallaron todos y se olvida dónde estaba el
/// fichero (quizá sus URL caducaron), para preguntar otra vez en la próxima carga.
pub fn end_open(file_id: FileId, ok: bool) {
    let attempted = ATTEMPTS.with(|a| a.borrow_mut().take()).unwrap_or_default();
    if !ok {
        CdnUrl::forget(file_id);
    }
    if attempted.is_empty() {
        return;
    }
    if !ok || attempted.len() > 1 {
        warn!(
            "CDN: {} de {} servidores fallaron al abrir {file_id} ({})",
            if ok { attempted.len() - 1 } else { attempted.len() },
            attempted.len(),
            attempted.join(", ")
        );
    }
    hosts().note(&attempted, ok, Instant::now());
}

impl TryFrom<CdnUrlMessage> for MaybeExpiringUrls {
    type Error = crate::Error;
    fn try_from(msg: CdnUrlMessage) -> Result<Self, Self::Error> {
        if !matches!(
            msg.result.enum_value_or_default(),
            StorageResolveResponse_Result::CDN
        ) {
            return Err(CdnUrlError::Storage.into());
        }

        let is_expiring = !msg.fileid.is_empty();

        let result = msg
            .cdnurl
            .iter()
            .map(|cdn_url| {
                let url = Url::parse(cdn_url)?;
                let mut expiry: Option<Date> = None;

                if is_expiring {
                    let mut expiry_str: Option<String> = None;
                    if let Some(token) = url
                        .query_pairs()
                        .into_iter()
                        .find(|(key, _value)| key == "verify")
                    {
                        // https://audio-cf.spotifycdn.com/audio/844ecdb297a87ebfee4399f28892ef85d9ba725f?verify=1750549951-4R3I2w2q7OfNkR%2FGH8qH7xtIKUPlDxywBuADY%2BsvMeU%3D
                        if let Some((expiry_str_candidate, _)) = token.1.split_once('-') {
                            expiry_str = Some(expiry_str_candidate.to_string());
                        }
                    } else if let Some(token) = url
                        .query_pairs()
                        .into_iter()
                        .find(|(key, _value)| key == "__token__")
                    {
                        //"https://audio-ak-spotify-com.akamaized.net/audio/4712bc9e47f7feb4ee3450ef2bb545e1d83c3d54?__token__=exp=1688165560~hmac=4e661527574fab5793adb99cf04e1c2ce12294c71fe1d39ffbfabdcfe8ce3b41",
                        if let Some(mut start) = token.1.find("exp=") {
                            start += 4;
                            if token.1.len() >= start {
                                let slice = &token.1[start..];
                                if let Some(end) = slice.find('~') {
                                    // this is the only valid invariant for akamaized.net
                                    expiry_str = Some(String::from(&slice[..end]));
                                } else {
                                    expiry_str = Some(String::from(slice));
                                }
                            }
                        }
                    } else if let Some(token) = url
                        .query_pairs()
                        .into_iter()
                        .find(|(key, _value)| key == "Expires")
                    {
                        //"https://audio-gm-off.spotifycdn.com/audio/4712bc9e47f7feb4ee3450ef2bb545e1d83c3d54?Expires=1688165560~FullPath~hmac=IIZA28qptl8cuGLq15-SjHKHtLoxzpy_6r_JpAU4MfM=",
                        if let Some(end) = token.1.find('~') {
                            // this is the only valid invariant for spotifycdn.com
                            let slice = &token.1[..end];
                            expiry_str = Some(String::from(&slice[..end]));
                        }
                    } else if let Some(query) = url.query() {
                        //"https://audio4-fa.scdn.co/audio/4712bc9e47f7feb4ee3450ef2bb545e1d83c3d54?1688165560_0GKSyXjLaTW1BksFOyI4J7Tf9tZDbBUNNPu9Mt4mhH4=",
                        let mut items = query.split('_');
                        if let Some(first) = items.next() {
                            // this is the only valid invariant for scdn.co
                            expiry_str = Some(String::from(first));
                        }
                    }

                    if let Some(exp_str) = expiry_str {
                        if let Ok(expiry_parsed) = exp_str.parse::<i64>() {
                            if let Ok(expiry_at) = Date::from_timestamp_ms(expiry_parsed * 1_000) {
                                let with_margin = expiry_at.saturating_sub(CDN_URL_EXPIRY_MARGIN);
                                expiry = Some(Date::from(with_margin));
                            }
                        } else {
                            warn!(
                                "Cannot parse CDN URL expiry timestamp '{exp_str}' from '{cdn_url}'"
                            );
                        }
                    } else {
                        warn!("Unknown CDN URL format: {cdn_url}");
                    }
                }
                Ok(MaybeExpiringUrl(cdn_url.to_owned(), expiry))
            })
            .collect::<Result<Vec<MaybeExpiringUrl>, Error>>()?;

        Ok(Self(result))
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_maybe_expiring_urls() {
        let timestamp = 1688165560;
        let mut msg = CdnUrlMessage::new();
        msg.result = StorageResolveResponse_Result::CDN.into();
        msg.cdnurl = vec![
            format!(
                "https://audio-cf.spotifycdn.com/audio/844ecdb297a87ebfee4399f28892ef85d9ba725f?verify={timestamp}-4R3I2w2q7OfNkR%2FGH8qH7xtIKUPlDxywBuADY%2BsvMeU%3D"
            ),
            format!(
                "https://audio-ak-spotify-com.akamaized.net/audio/foo?__token__=exp={timestamp}~hmac=4e661527574fab5793adb99cf04e1c2ce12294c71fe1d39ffbfabdcfe8ce3b41"
            ),
            format!(
                "https://audio-gm-off.spotifycdn.com/audio/foo?Expires={timestamp}~FullPath~hmac=IIZA28qptl8cuGLq15-SjHKHtLoxzpy_6r_JpAU4MfM="
            ),
            format!(
                "https://audio4-fa.scdn.co/audio/foo?{timestamp}_0GKSyXjLaTW1BksFOyI4J7Tf9tZDbBUNNPu9Mt4mhH4="
            ),
            "https://audio4-fa.scdn.co/foo?baz".to_string(),
        ];
        msg.fileid = vec![0];

        let urls = MaybeExpiringUrls::try_from(msg).expect("valid urls");
        assert_eq!(urls.len(), 5);
        assert!(urls[0].1.is_some());
        assert!(urls[1].1.is_some());
        assert!(urls[2].1.is_some());
        assert!(urls[3].1.is_some());
        assert!(urls[4].1.is_none());
        let timestamp_margin = Duration::seconds(timestamp) - CDN_URL_EXPIRY_MARGIN;
        assert_eq!(
            urls[0].1.unwrap().as_timestamp_ms() as i128,
            timestamp_margin.whole_milliseconds()
        );
    }
}
