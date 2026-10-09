//! Rutas de la aplicación y ajustes persistentes (un único `settings.json`).

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::hash::{BuildHasher, Hasher, RandomState};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub state_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl Paths {
    pub fn new() -> Self {
        let pd = ProjectDirs::from("", "", "nanofy")
            .expect("no se pudo determinar el directorio de usuario");
        let state_dir = pd
            .state_dir()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| pd.data_local_dir().to_path_buf());
        Self {
            config_dir: pd.config_dir().to_path_buf(),
            state_dir,
            cache_dir: pd.cache_dir().to_path_buf(),
        }
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join("settings.json")
    }
    pub fn credentials_dir(&self) -> PathBuf {
        self.state_dir.join("credentials")
    }
    pub fn volume_dir(&self) -> PathBuf {
        self.state_dir.clone()
    }
    pub fn audio_cache_dir(&self) -> PathBuf {
        self.cache_dir.join("audio")
    }
    pub fn image_cache_dir(&self) -> PathBuf {
        self.cache_dir.join("images")
    }
    pub fn webauth_file(&self) -> PathBuf {
        self.state_dir.join("webapi_token.json")
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Theme {
    #[default]
    System,
    Dark,
    Light,
}

/// Calidad de audio pedida a Spotify (Ogg Vorbis, como la app de escritorio de Spotify).
/// Los nombres en settings.json no cambian: los guardan versiones anteriores y el modo de control.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Quality {
    /// Ogg Vorbis 96 kbps
    Low,
    /// Ogg Vorbis 160 kbps
    Normal,
    /// Ogg Vorbis 320 kbps, lo máximo que Spotify entrega fuera de sus apps.
    /// Hasta 1.7 existía «Lossless» (FLAC), que nunca podía sonar: sus claves están tras el DRM
    /// de Spotify. Quien lo tenía guardado ya oía 320 kbps, así que se lee como `High`.
    #[default]
    #[serde(alias = "Lossless")]
    High,
}

impl Quality {
    /// Nombres de los niveles de Spotify: su «Alta» es 160 kbps y su «Muy alta», 320. Con los
    /// nombres de antes, comparar «Alta» con «Alta» era comparar 320 con 160 kbps.
    pub fn label(self) -> &'static str {
        match self {
            Quality::Low => "Normal · 96 kbps",
            Quality::Normal => "Alta · 160 kbps",
            Quality::High => "Muy alta · 320 kbps",
        }
    }

    /// kbps de esta calidad (los del formato Ogg Vorbis que se pide primero).
    pub fn kbps(self) -> u16 {
        match self {
            Quality::Low => 96,
            Quality::Normal => 160,
            Quality::High => 320,
        }
    }

    /// La única traducción a la calidad de librespot: la usan el reproductor y las descargas,
    /// así ambos buscan los formatos en el mismo orden (`player::format_order`).
    pub fn bitrate(self) -> librespot_playback::config::Bitrate {
        use librespot_playback::config::Bitrate;
        match self {
            Quality::Low => Bitrate::Bitrate96,
            Quality::Normal => Bitrate::Bitrate160,
            Quality::High => Bitrate::Bitrate320,
        }
    }
}

/// Nivel de volumen de la normalización, los tres de Spotify. Los nombres en settings.json son los
/// de la variante (como en `Quality`), así se pueden cambiar las etiquetas sin tocar archivos.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Loudness {
    /// −11 LUFS, con limitador.
    Loud,
    /// −14 LUFS, el predeterminado de Spotify.
    #[default]
    Normal,
    /// −19 LUFS.
    Quiet,
}

impl Loudness {
    pub fn label(self) -> &'static str {
        match self {
            Loudness::Loud => "Alto",
            Loudness::Normal => "Normal",
            Loudness::Quiet => "Bajo",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Loudness::Loud => "−11 LUFS, con limitador · para ambientes ruidosos",
            Loudness::Normal => "−14 LUFS · el predeterminado de Spotify",
            Loudness::Quiet => "−19 LUFS · para ambientes tranquilos",
        }
    }
}

/// Revisión de los ajustes de audio guardados. Sube cuando una versión nueva tiene que cambiar
/// una sola vez un valor que ya estaba guardado (ver `Settings::migrate`).
pub const AUDIO_REV: u8 = 1;

/// Duración del fundido entre canciones (segundos): los mismos topes que el mezclador del
/// reproductor, de donde se toman para que la interfaz no pueda pedir algo que él recortaría.
pub const CROSSFADE_SECS_MIN: u8 = (librespot_playback::crossfade::CROSSFADE_MIN_MS / 1000) as u8;
pub const CROSSFADE_SECS_MAX: u8 = (librespot_playback::crossfade::CROSSFADE_MAX_MS / 1000) as u8;
/// Elección propia (Spotify no tiene un valor recomendado): se nota sin tapar los finales.
pub const CROSSFADE_SECS_DEFAULT: u8 = 6;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Nombre con el que aparece este equipo en Spotify Connect.
    pub device_name: String,
    /// Identificador estable del dispositivo (se genera una vez).
    pub device_id: String,
    pub quality: Quality,
    /// Normalizar el volumen como Spotify (activada por defecto, como allí).
    pub normalisation: bool,
    /// Nivel de la normalización: Alto / Normal / Bajo.
    pub loudness: Loudness,
    /// Revisión de los ajustes de audio de este archivo. Los settings.json anteriores no la traen
    /// y se leen como 0 (por eso `default` de campo, que manda sobre el del contenedor): así
    /// `migrate` sabe que aún no se aplicó y lo hace una sola vez.
    #[serde(default)]
    pub audio_rev: u8,
    pub gapless: bool,
    /// Fundido entre canciones, como el de Spotify: la siguiente entra mientras la anterior se
    /// apaga, solo cuando una canción acaba sola. Apagado de fábrica, como allí. Se aplica al
    /// instante, sin «Guardar» ni reiniciar el reproductor, así que no cuenta en
    /// `restart_differs` ni en `audio_differs`.
    pub crossfade: bool,
    /// Duración del fundido en segundos (`CROSSFADE_SECS_MIN..=CROSSFADE_SECS_MAX`, se ajusta al
    /// leer).
    pub crossfade_secs: u8,
    /// Fundir también canciones seguidas de un mismo álbum. Spotify nunca lo hace, para respetar
    /// las transiciones del artista; aquí se puede elegir.
    pub crossfade_albums: bool,
    pub autoplay: bool,
    /// Precarga inteligente: prepara la canción sobre la que pasa el ratón (metadatos y ubicación
    /// del fichero) y la del botón de reproducir apretado, para que suene al instante. Activada de
    /// fábrica; los settings.json de antes no la traen y la toman del valor por defecto.
    pub smart_preload: bool,
    /// Límite de la caché de audio en disco (MB). 0 desactiva la caché.
    pub audio_cache_mb: u64,
    pub theme: Theme,
    /// Escala de la interfaz (1.0 = 100 %).
    pub zoom: f32,
    pub sidebar_visible: bool,
    /// Volumen 0..=100 (se guarda al salir).
    pub volume: u8,
    /// Límite de fotogramas por segundo durante animaciones.
    pub fps_cap: u32,
    /// Teclas multimedia e integración con el sistema (SMTC / MPRIS / Now Playing).
    pub media_keys: bool,
    /// Muestra el panel de letras al arrancar.
    pub lyrics_open: bool,
    /// Client ID de una app de desarrollador propia, opcional («Avanzado» en Ajustes): solo para
    /// que las lecturas de la Web API usen su cuota. Nanofy no la necesita.
    pub client_id: String,
    /// Ya se pidió una vez, sola, la autorización de la biblioteca tras iniciar sesión en el
    /// navegador (encadenada en la misma pestaña o en una segunda). Si la persona no la acepta, no
    /// se vuelve a abrir el navegador sin que lo pida: queda el aviso «Conecta tu biblioteca» del
    /// inicio. Los settings.json de antes no lo traen y lo leen como `false`.
    pub library_consent_asked: bool,
    /// Fijadas en la biblioteca, la última primero: ids de playlist y, para lo demás,
    /// `album:<id>`, `artist:<id>` y `folder:<id>`.
    pub pinned: Vec<String>,
    /// «Canciones que te gustan» se desfijó (de fábrica va fijada, como en Spotify).
    pub liked_unpinned: bool,
    /// Biblioteca en cuadrícula (true) o lista.
    pub library_grid: bool,
    /// Biblioteca de lo más antiguo a lo más reciente (el botón «Recientes»).
    pub library_oldest: bool,
    /// Biblioteca agrupada por tipo (playlists, carpetas, álbumes, artistas).
    pub library_grouped: bool,
    /// Secciones del inicio fijadas arriba y ocultas (ids de sección).
    pub home_pinned: Vec<String>,
    pub home_hidden: Vec<String>,
    /// Orden de las secciones del inicio, playlists añadidas como sección y filas recomendadas.
    pub home_order: Vec<String>,
    pub home_custom: Vec<String>,
    pub home_recs: bool,
    /// Consultar GitHub al arrancar (y cada pocas horas) y avisar si hay una versión nueva.
    pub update_check: bool,
    /// Descargar y preparar la versión nueva en segundo plano sin preguntar: solo queda pulsar
    /// «Reiniciar» (o se instala sola al abrir Nanofy la próxima vez). Sin esto, se avisa y se
    /// instala con «Instalar». Los settings.json de antes no lo traen: toman `UPDATE_AUTO_DEFAULT`.
    pub update_auto: bool,
    /// Versión que el usuario pidió omitir (no se vuelve a avisar de ella).
    pub update_skipped: String,
}

/// Valor de «Actualizar automáticamente» para instalaciones nuevas y para los ajustes guardados
/// por versiones que aún no lo tenían. En un solo sitio para poder cambiarlo.
pub const UPDATE_AUTO_DEFAULT: bool = true;

impl Default for Settings {
    fn default() -> Self {
        Self {
            device_name: "Nanofy".to_string(),
            device_id: random_hex(),
            quality: Quality::High,
            normalisation: true,
            loudness: Loudness::Normal,
            audio_rev: AUDIO_REV,
            gapless: true,
            crossfade: false,
            crossfade_secs: CROSSFADE_SECS_DEFAULT,
            crossfade_albums: false,
            autoplay: true,
            smart_preload: true,
            audio_cache_mb: 256,
            theme: Theme::System,
            zoom: 1.0,
            sidebar_visible: true,
            volume: 60,
            fps_cap: 144,
            media_keys: true,
            lyrics_open: false,
            // Client ID incluido al compilar (variable NANOFY_CLIENT_ID), para repartir Nanofy
            // sin que cada persona tenga que crear su app de desarrollador.
            client_id: option_env!("NANOFY_CLIENT_ID").unwrap_or("").to_string(),
            library_consent_asked: false,
            pinned: Vec::new(),
            liked_unpinned: false,
            library_grid: true,
            library_oldest: false,
            library_grouped: false,
            home_pinned: Vec::new(),
            home_hidden: Vec::new(),
            home_order: Vec::new(),
            home_custom: Vec::new(),
            home_recs: true,
            update_check: true,
            update_auto: UPDATE_AUTO_DEFAULT,
            update_skipped: String::new(),
        }
    }
}

impl Settings {
    pub fn load(paths: &Paths) -> Self {
        let file = paths.settings_file();
        let mut keep_file = false;
        let mut settings = match std::fs::read(&file) {
            Ok(bytes) => {
                // Sin `read_to_string`: un settings.json editado a mano y guardado en ANSI fallaría
                // entero y se perdería todo (device_id incluido); así solo se estropean las tildes.
                let text = String::from_utf8_lossy(&bytes);
                let not_utf8 = matches!(text, std::borrow::Cow::Owned(_));
                match Settings::parse_resilient(text.trim_start_matches('\u{feff}')) {
                    Some((settings, dropped)) => {
                        if !dropped.is_empty() {
                            log::warn!(
                                "settings.json: se descartan valores no válidos de {} (se usan los de por defecto); el resto se conserva",
                                dropped.join(", ")
                            );
                        }
                        if not_utf8 {
                            log::warn!("settings.json no está en UTF-8; se conserva todo salvo las letras que no se pueden leer");
                        }
                        if !dropped.is_empty() || not_utf8 {
                            Settings::backup(paths, &bytes);
                        }
                        settings
                    }
                    None => {
                        log::warn!("settings.json ilegible; se usan los valores por defecto (copia en settings.json.bak)");
                        Settings::backup(paths, &bytes);
                        Settings::default()
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default(),
            Err(e) => {
                // Existe pero no se puede leer (bloqueado por otro programa, permisos): no se
                // sobrescribe con los valores por defecto, que borraría el device_id y lo demás.
                log::warn!("no se pudo leer {}: {e}; se usan los valores por defecto sin guardarlos", file.display());
                keep_file = true;
                Settings::default()
            }
        };
        if settings.device_id.is_empty() {
            settings.device_id = random_hex();
        }
        settings.migrate();
        settings.zoom = settings.zoom.clamp(0.7, 2.0);
        settings.fps_cap = settings.fps_cap.clamp(30, 480);
        settings.crossfade_secs = settings.crossfade_secs.clamp(CROSSFADE_SECS_MIN, CROSSFADE_SECS_MAX);
        // Guardamos siempre para que el archivo exista y el device_id quede fijo.
        if !keep_file {
            settings.save(paths);
        }
        settings
    }

    /// Lee los ajustes sin que un solo campo malo los borre todos: con `#[serde(default)]` falta
    /// un campo y no pasa nada, pero un valor de otro tipo («zoom»: "x") o una variante que ya no
    /// existe hacía fallar el archivo entero, y se volvía a los valores por defecto con un
    /// device_id nuevo y sin playlists fijadas. Si la lectura completa falla, se parte de los
    /// valores por defecto y se copia cada clave que se lee por sí sola. Devuelve también las
    /// claves descartadas; `None` si el texto ni siquiera es un objeto JSON.
    fn parse_resilient(text: &str) -> Option<(Settings, Vec<String>)> {
        use serde_json::Value;
        if let Ok(settings) = serde_json::from_str::<Settings>(text) {
            return Some((settings, Vec::new()));
        }
        let Ok(Value::Object(file)) = serde_json::from_str::<Value>(text) else {
            return None;
        };
        // Se parte de un objeto vacío y no de `Settings::default()` serializado: así cada clave
        // que falta toma el mismo valor que en la lectura normal (incluidos los `#[serde(default
        // = …)]` de campo, p. ej. una marca de migración que en un archivo antiguo debe faltar).
        let mut merged = serde_json::Map::new();
        let mut dropped = Vec::new();
        for (key, value) in file {
            // Se prueba cada clave sobre lo ya aceptado: los campos son independientes, así que
            // lo que se lee por separado también se lee junto. Las claves desconocidas (de una
            // versión más nueva) pasan y serde las ignora, como en la lectura normal.
            let mut trial = merged.clone();
            trial.insert(key.clone(), value.clone());
            if serde_json::from_value::<Settings>(Value::Object(trial)).is_ok() {
                merged.insert(key, value);
            } else {
                dropped.push(key);
            }
        }
        let settings = serde_json::from_value::<Settings>(Value::Object(merged)).ok()?;
        Some((settings, dropped))
    }

    /// Copia del settings.json original antes de sobrescribirlo con valores descartados, por si
    /// había algo que recuperar a mano.
    fn backup(paths: &Paths, bytes: &[u8]) {
        let bak = paths.config_dir.join("settings.json.bak");
        if let Err(e) = std::fs::write(&bak, bytes) {
            log::warn!("no se pudo guardar {}: {e}", bak.display());
        }
    }

    pub fn save(&self, paths: &Paths) {
        let file = paths.settings_file();
        if let Err(e) = std::fs::create_dir_all(&paths.config_dir) {
            log::warn!("no se pudo crear {}: {e}", paths.config_dir.display());
            return;
        }
        match serde_json::to_string_pretty(self) {
            Ok(text) => {
                if let Err(e) = std::fs::write(&file, text) {
                    log::warn!("no se pudo guardar {}: {e}", file.display());
                }
            }
            Err(e) => log::warn!("no se pudieron serializar los ajustes: {e}"),
        }
    }

    /// Cambios de una sola vez en los ajustes guardados por versiones anteriores (el archivo se
    /// reescribe justo después, con la marca nueva). Devuelve si cambió algo.
    fn migrate(&mut self) -> bool {
        let mut changed = false;
        if self.audio_rev < 1 {
            // La normalización pasa a estar activada para todos, también para quien ya tenía
            // Nanofy, como en Spotify (Normal, −14 LUFS): antes venía apagada de fábrica y no se
            // distingue un «apagado» elegido del de fábrica. La marca evita volver a forzarla si
            // luego se apaga.
            if !self.normalisation {
                log::info!("ajustes: normalización de volumen activada como en Spotify (una sola vez)");
            }
            self.normalisation = true;
            self.audio_rev = 1;
            changed = true;
        }
        changed
    }

    /// Ajustes que obligan a reiniciar el reproductor de librespot (y con él la sesión).
    pub fn restart_differs(&self, other: &Settings) -> bool {
        self.device_name != other.device_name
            || self.autoplay != other.autoplay
            || self.audio_cache_mb != other.audio_cache_mb
    }

    /// Ajustes de audio que se aplican en vivo, sin reiniciar (`Cmd::AudioTuning`): la calidad y
    /// gapless desde la próxima canción que se cargue y el volumen al instante.
    pub fn audio_differs(&self, other: &Settings) -> bool {
        self.quality != other.quality
            || self.normalisation != other.normalisation
            || self.loudness != other.loudness
            || self.gapless != other.gapless
    }

    /// Ajustes del fundido distintos: van por su propia orden en vivo (`Cmd::Crossfade`), ni
    /// reinician ni pasan por `Cmd::AudioTuning`.
    pub fn crossfade_differs(&self, other: &Settings) -> bool {
        self.crossfade != other.crossfade
            || self.crossfade_secs != other.crossfade_secs
            || self.crossfade_albums != other.crossfade_albums
    }

    /// Duración del fundido elegida, en ms; 0 si está apagado. Con el tope aquí también: un
    /// valor parcheado sin pasar por `load` (modo de control) no llega fuera de rango.
    pub fn crossfade_ms(&self) -> u32 {
        if self.crossfade {
            self.crossfade_secs.clamp(CROSSFADE_SECS_MIN, CROSSFADE_SECS_MAX) as u32 * 1000
        } else {
            0
        }
    }

    /// Fundido en vigor (ms): el elegido, salvo con el temporizador «al terminar la canción»
    /// puesto. Ese temporizador pausa al cambiar de canción, y con un fundido el cambio llega al
    /// principio de la mezcla: pausaría a mitad, con la siguiente ya sonando encima. Mientras
    /// está puesto, la canción acaba entera y la siguiente entra sin hueco.
    pub fn crossfade_effective_ms(&self, sleep_end_of_track: bool) -> u32 {
        if sleep_end_of_track {
            0
        } else {
            self.crossfade_ms()
        }
    }
}

/// `NANOFY_NO_SESSION=1`: arranca sin conectar con Spotify (medidas de memoria «sin sesión»).
pub fn no_session() -> bool {
    std::env::var_os("NANOFY_NO_SESSION").is_some()
}

/// Rango del slider de volumen en decibelios: 0 % = silencio, 1 % ≈ -40 dB, 100 % = 0 dB.
/// El slider es lineal en dB (20·log10 de la amplitud), que es como percibe el oído.
pub const VOL_DB_RANGE: f32 = 40.0;

/// Porcentaje del slider (0..=100) → volumen lineal de librespot (0..=65535).
pub fn vol_pct_to_raw(pct: f32) -> u16 {
    let p = pct.clamp(0.0, 100.0);
    if p <= 0.0 {
        return 0;
    }
    let db = VOL_DB_RANGE * (p / 100.0 - 1.0);
    (10f32.powf(db / 20.0) * u16::MAX as f32).round().clamp(1.0, u16::MAX as f32) as u16
}

/// Volumen lineal de librespot → porcentaje del slider.
pub fn vol_raw_to_pct(raw: u16) -> f32 {
    if raw == 0 {
        return 0.0;
    }
    let amp = raw as f32 / u16::MAX as f32;
    let db = 20.0 * amp.log10();
    ((db / VOL_DB_RANGE + 1.0) * 100.0).clamp(0.0, 100.0)
}

/// Ganancia en dB de un volumen lineal (para mostrarla).
pub fn vol_raw_db(raw: u16) -> f32 {
    if raw == 0 {
        return f32::NEG_INFINITY;
    }
    20.0 * (raw as f32 / u16::MAX as f32).log10()
}

/// 40 caracteres hexadecimales pseudoaleatorios (formato que usa Spotify para device_id).
fn random_hex() -> String {
    let mut out = String::with_capacity(48);
    for _ in 0..3 {
        let mut h = RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out.truncate(40);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un settings.json de una versión anterior (sin `update_auto`) se lee entero: el campo nuevo
    /// toma su valor por defecto y lo demás, incluido el device_id, se conserva.
    #[test]
    fn ajustes_antiguos_sin_update_auto() {
        let old = r#"{"device_name":"Salón","device_id":"abc123","update_check":false,"update_skipped":"1.5.0"}"#;
        let s: Settings = serde_json::from_str(old).expect("settings.json de 1.6 válido");
        assert_eq!(s.update_auto, UPDATE_AUTO_DEFAULT);
        assert_eq!(s.device_name, "Salón");
        assert_eq!(s.device_id, "abc123");
        assert!(!s.update_check);
        assert_eq!(s.update_skipped, "1.5.0");
        let off: Settings = serde_json::from_str(r#"{"device_id":"abc123","update_auto":false}"#).unwrap();
        assert!(!off.update_auto);
        assert_eq!(Settings::default().update_auto, UPDATE_AUTO_DEFAULT);
    }

    /// La precarga inteligente viene activada, también para quien ya tenía un settings.json sin
    /// ella; quien la apaga la conserva apagada, y no pide reiniciar el reproductor.
    #[test]
    fn precarga_inteligente_activada_por_defecto() {
        assert!(Settings::default().smart_preload);
        let old: Settings = serde_json::from_str(r#"{"device_id":"abc123","gapless":false}"#).unwrap();
        assert!(old.smart_preload);
        assert_eq!(old.device_id, "abc123");
        assert!(!old.gapless);
        let off: Settings = serde_json::from_str(r#"{"device_id":"abc123","smart_preload":false}"#).unwrap();
        assert!(!off.smart_preload);
        let mut other = off.clone();
        other.smart_preload = true;
        assert!(!off.restart_differs(&other));
        assert!(!off.audio_differs(&other));
    }

    /// Quien guardó «Lossless» (hasta 1.7) ya oía 320 kbps: se lee como `High` sin tocar el resto,
    /// y los nombres guardados no cambian.
    #[test]
    fn calidad_lossless_se_lee_como_muy_alta() {
        let s: Settings = serde_json::from_str(r#"{"device_id":"abc123","quality":"Lossless"}"#).expect("Lossless se acepta");
        assert_eq!(s.quality, Quality::High);
        assert_eq!(s.device_id, "abc123");
        assert_eq!(serde_json::to_value(Quality::High).unwrap(), "High");
        assert_eq!(serde_json::to_value(Quality::Normal).unwrap(), "Normal");
        assert_eq!(serde_json::to_value(Quality::Low).unwrap(), "Low");
        assert_eq!(serde_json::to_value(&s).unwrap()["quality"], "High");
        for q in [Quality::Low, Quality::Normal, Quality::High] {
            let back: Quality = serde_json::from_value(serde_json::to_value(q).unwrap()).unwrap();
            assert_eq!(back, q);
        }
        assert_eq!(Quality::default(), Quality::High);
        // Nombres de los niveles de Spotify, con la tasa real.
        assert_eq!(Quality::Low.label(), "Normal · 96 kbps");
        assert_eq!(Quality::Normal.label(), "Alta · 160 kbps");
        assert_eq!(Quality::High.label(), "Muy alta · 320 kbps");
    }

    /// Un valor malo («zoom»: "x», una calidad que no existe) ya no devuelve todo a los valores por
    /// defecto: device_id, playlists fijadas y lo demás se conservan; solo esa clave se descarta.
    #[test]
    fn un_campo_malo_no_borra_los_ajustes() {
        let text = r#"{"device_id":"abc123","pinned":["p1","p2"],"zoom":"x","volume":40,
            "quality":"Ultra","home_order":["a"],"campo_futuro":{"n":1}}"#;
        assert!(serde_json::from_str::<Settings>(text).is_err(), "la lectura completa debe fallar");
        let (s, mut dropped) = Settings::parse_resilient(text).expect("es un objeto JSON");
        dropped.sort();
        assert_eq!(dropped, ["quality", "zoom"]);
        assert_eq!(s.device_id, "abc123");
        assert_eq!(s.pinned, ["p1", "p2"]);
        assert_eq!(s.volume, 40);
        assert_eq!(s.home_order, ["a"]);
        assert_eq!(s.zoom, Settings::default().zoom);
        assert_eq!(s.quality, Quality::High);
        // Lo que el archivo no trae toma el mismo valor que en una lectura normal.
        assert_eq!(s.update_auto, UPDATE_AUTO_DEFAULT);
        assert_eq!(s.gapless, Settings::default().gapless);

        // Un archivo correcto se lee igual que siempre, sin descartes.
        let (ok, dropped) = Settings::parse_resilient(r#"{"device_id":"abc123","quality":"Normal"}"#).unwrap();
        assert!(dropped.is_empty());
        assert_eq!(ok.quality, Quality::Normal);

        // Lo que ni siquiera es un objeto JSON (archivo cortado, vacío, una lista) no se recupera.
        for bad in [r#"{"device_id":"abc"#, "", "[1,2]", "null"] {
            assert!(Settings::parse_resilient(bad).is_none(), "{bad:?}");
        }
    }

    /// `Settings::load` de punta a punta en una carpeta temporal: un campo malo o un archivo en
    /// ANSI conservan el device_id y las playlists fijadas, el archivo queda reescrito y válido, y
    /// el original se guarda en settings.json.bak.
    #[test]
    fn load_resiste_un_campo_malo_y_un_archivo_ansi() {
        let dir = std::env::temp_dir().join(format!("nanofy-test-ajustes-{}", random_hex()));
        let paths = Paths { config_dir: dir.clone(), state_dir: dir.clone(), cache_dir: dir.clone() };
        std::fs::create_dir_all(&dir).unwrap();

        let original = "\u{feff}{\"device_id\":\"abc123\",\"pinned\":[\"p1\"],\"zoom\":\"x\",\"quality\":\"Lossless\"}";
        std::fs::write(paths.settings_file(), original).unwrap();
        let s = Settings::load(&paths);
        assert_eq!(s.device_id, "abc123");
        assert_eq!(s.pinned, ["p1"]);
        assert_eq!(s.quality, Quality::High);
        assert_eq!(s.zoom, 1.0);
        let saved: Settings = serde_json::from_str(&std::fs::read_to_string(paths.settings_file()).unwrap())
            .expect("el archivo reescrito se lee entero");
        assert_eq!(saved.device_id, "abc123");
        assert_eq!(std::fs::read_to_string(dir.join("settings.json.bak")).unwrap(), original);

        // «Salón» guardado en Windows-1252 (0xF3 no es UTF-8 válido).
        let ansi: &[u8] = b"{\"device_name\":\"Sal\xf3n\",\"device_id\":\"abc123\",\"pinned\":[\"p1\"]}";
        std::fs::write(paths.settings_file(), ansi).unwrap();
        let s = Settings::load(&paths);
        assert_eq!(s.device_id, "abc123");
        assert_eq!(s.pinned, ["p1"]);
        assert!(s.device_name.starts_with("Sal"));
        assert_eq!(std::fs::read(dir.join("settings.json.bak")).unwrap(), ansi);

        // Sin archivo: valores por defecto, y queda guardado con su device_id.
        std::fs::remove_file(paths.settings_file()).unwrap();
        let s = Settings::load(&paths);
        assert_eq!(s.device_id.len(), 40);
        let saved: Settings = serde_json::from_str(&std::fs::read_to_string(paths.settings_file()).unwrap()).unwrap();
        assert_eq!(saved.device_id, s.device_id);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Instalación nueva: normalización activada en Normal (−14 LUFS), como Spotify, y la marca de
    /// migración ya puesta. Los nombres guardados de los niveles son los de la variante.
    #[test]
    fn normalizacion_por_defecto_como_spotify() {
        let d = Settings::default();
        assert!(d.normalisation);
        assert_eq!(d.loudness, Loudness::Normal);
        assert_eq!(d.audio_rev, AUDIO_REV);
        let mut fresh = d.clone();
        assert!(!fresh.migrate(), "una instalación nueva no tiene nada que migrar");
        assert_eq!(fresh, d);

        for (l, name, label) in [(Loudness::Loud, "Loud", "Alto"), (Loudness::Normal, "Normal", "Normal"), (Loudness::Quiet, "Quiet", "Bajo")] {
            assert_eq!(serde_json::to_value(l).unwrap(), name);
            assert_eq!(serde_json::from_value::<Loudness>(serde_json::json!(name)).unwrap(), l);
            assert_eq!(l.label(), label);
        }
        assert!(Loudness::Loud.hint().starts_with("−11 LUFS"));
        assert!(Loudness::Normal.hint().starts_with("−14 LUFS"));
        assert!(Loudness::Quiet.hint().starts_with("−19 LUFS"));
    }

    /// Decisión del usuario: la normalización se activa también para quien ya tenía Nanofy, una
    /// sola vez. Un settings.json de 1.7 (normalisation: false, sin audio_rev) la activa y queda
    /// marcado; si luego se apaga, sigue apagada en los arranques siguientes.
    #[test]
    fn migracion_activa_la_normalizacion_una_sola_vez() {
        let old = r#"{"device_id":"abc123","normalisation":false,"pinned":["p1"],"quality":"Normal"}"#;
        let mut s: Settings = serde_json::from_str(old).expect("settings.json de 1.7 válido");
        assert_eq!(s.audio_rev, 0, "un archivo sin la marca se lee como revisión 0");
        assert!(!s.normalisation);
        assert!(s.migrate());
        assert!(s.normalisation);
        assert_eq!(s.audio_rev, 1);
        assert_eq!(s.loudness, Loudness::Normal);
        assert_eq!(s.device_id, "abc123");
        assert_eq!(s.pinned, ["p1"]);
        assert_eq!(s.quality, Quality::Normal);
        assert!(!s.migrate(), "la segunda vez no hace nada");

        // Apagada después de migrar: se respeta.
        let off = r#"{"device_id":"abc123","normalisation":false,"audio_rev":1,"loudness":"Quiet"}"#;
        let mut s: Settings = serde_json::from_str(off).unwrap();
        assert!(!s.migrate());
        assert!(!s.normalisation);
        assert_eq!(s.loudness, Loudness::Quiet);

        // La lectura resistente también deja la marca a 0 cuando falta (y descarta solo lo malo).
        let (s, dropped) = Settings::parse_resilient(r#"{"device_id":"abc123","normalisation":false,"zoom":"x","loudness":"Ultra"}"#).unwrap();
        let mut dropped = dropped;
        dropped.sort();
        assert_eq!(dropped, ["loudness", "zoom"]);
        assert_eq!(s.audio_rev, 0);
        assert_eq!(s.loudness, Loudness::Normal);
        assert_eq!(s.device_id, "abc123");
    }

    /// `Settings::load` de punta a punta: el primer arranque con un archivo de 1.7 activa la
    /// normalización y lo guarda con la marca; apagarla después dura.
    #[test]
    fn load_migra_y_respeta_el_apagado_posterior() {
        let dir = std::env::temp_dir().join(format!("nanofy-test-migracion-{}", random_hex()));
        let paths = Paths { config_dir: dir.clone(), state_dir: dir.clone(), cache_dir: dir.clone() };
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(paths.settings_file(), r#"{"device_id":"abc123","normalisation":false,"volume":35}"#).unwrap();
        let s = Settings::load(&paths);
        assert!(s.normalisation);
        assert_eq!(s.audio_rev, AUDIO_REV);
        assert_eq!(s.device_id, "abc123");
        assert_eq!(s.volume, 35);
        let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(paths.settings_file()).unwrap()).unwrap();
        assert_eq!(saved["normalisation"], true);
        assert_eq!(saved["audio_rev"], AUDIO_REV);
        assert_eq!(saved["loudness"], "Normal");

        // El usuario la apaga y guarda: el siguiente arranque no la vuelve a encender.
        let mut off = s.clone();
        off.normalisation = false;
        off.loudness = Loudness::Loud;
        off.save(&paths);
        let s = Settings::load(&paths);
        assert!(!s.normalisation);
        assert_eq!(s.loudness, Loudness::Loud);
        assert_eq!(s.device_id, "abc123");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Solo el nombre, autoplay y la caché reinician el reproductor; calidad, gapless y volumen
    /// se aplican en vivo; el fundido tiene su propia orden; lo demás (tema, zoom…) no toca la
    /// reproducción.
    #[test]
    fn ajustes_que_reinician_y_ajustes_en_vivo() {
        let base = Settings::default();
        let with = |f: &dyn Fn(&mut Settings)| {
            let mut s = base.clone();
            f(&mut s);
            (base.restart_differs(&s), base.audio_differs(&s))
        };
        assert_eq!(with(&|s| s.device_name = "Otro".into()), (true, false));
        assert_eq!(with(&|s| s.autoplay = !s.autoplay), (true, false));
        assert_eq!(with(&|s| s.audio_cache_mb = 1024), (true, false));
        assert_eq!(with(&|s| s.quality = Quality::Low), (false, true));
        assert_eq!(with(&|s| s.normalisation = false), (false, true));
        assert_eq!(with(&|s| s.loudness = Loudness::Quiet), (false, true));
        assert_eq!(with(&|s| s.gapless = false), (false, true));
        assert_eq!(with(&|s| s.theme = Theme::Dark), (false, false));
        assert_eq!(with(&|s| s.zoom = 1.3), (false, false));
        // El fundido va por su propia orden en vivo: ni reinicia ni es un ajuste de «Guardar».
        assert_eq!(with(&|s| s.crossfade = true), (false, false));
        assert_eq!(with(&|s| s.crossfade_secs = 3), (false, false));
        assert_eq!(with(&|s| s.crossfade_albums = true), (false, false));
        assert_eq!(with(&|_| {}), (false, false));
    }

    /// Fundido: apagado de fábrica (como Spotify) con 6 s, sin álbumes; los settings.json de antes
    /// lo leen así sin perder nada; la duración se ajusta a 1–12 s al leer y al pedirla, y el
    /// temporizador «al terminar la canción» lo suspende sin tocar lo elegido.
    #[test]
    fn fundido_por_defecto_topes_y_temporizador() {
        let d = Settings::default();
        assert!(!d.crossfade);
        assert_eq!(d.crossfade_secs, 6);
        assert!(!d.crossfade_albums);
        assert_eq!(d.crossfade_ms(), 0);
        assert_eq!((CROSSFADE_SECS_MIN, CROSSFADE_SECS_MAX), (1, 12));

        // settings.json de 1.7 (sin los campos): toma los valores de fábrica y conserva el resto.
        let old: Settings = serde_json::from_str(r#"{"device_id":"abc123","gapless":false,"pinned":["p1"]}"#).unwrap();
        assert!(!old.crossfade);
        assert_eq!(old.crossfade_secs, CROSSFADE_SECS_DEFAULT);
        assert_eq!(old.device_id, "abc123");
        assert!(!old.gapless);

        let on = |secs: u8| Settings { crossfade: true, crossfade_secs: secs, ..Settings::default() };
        assert_eq!(on(6).crossfade_ms(), 6000);
        assert_eq!(on(1).crossfade_ms(), 1000);
        assert_eq!(on(12).crossfade_ms(), 12_000);
        // Fuera de rango (parcheado a mano o por el modo de control): al tope, nunca 0 ni 255 s.
        assert_eq!(on(0).crossfade_ms(), 1000);
        assert_eq!(on(200).crossfade_ms(), 12_000);
        // Apagado manda sobre la duración guardada, que se conserva para cuando se encienda.
        assert_eq!(Settings { crossfade: false, ..on(3) }.crossfade_ms(), 0);

        // Temporizador «al terminar la canción»: 0 mientras está puesto, lo elegido al quitarlo.
        assert_eq!(on(6).crossfade_effective_ms(true), 0);
        assert_eq!(on(6).crossfade_effective_ms(false), 6000);
        assert_eq!(d.crossfade_effective_ms(false), 0);

        // Cualquiera de los tres campos cuenta como cambio del fundido; los demás, no.
        assert!(d.crossfade_differs(&on(6)));
        assert!(on(6).crossfade_differs(&on(7)));
        assert!(d.crossfade_differs(&Settings { crossfade_albums: true, ..d.clone() }));
        assert!(!d.crossfade_differs(&Settings { gapless: false, zoom: 1.4, ..d.clone() }));

        // Un valor que no cabe en u8 se descarta solo (no borra los ajustes) y vuelve a 6 s.
        let (s, dropped) = Settings::parse_resilient(r#"{"device_id":"abc123","crossfade":true,"crossfade_secs":300}"#).unwrap();
        assert_eq!(dropped, ["crossfade_secs"]);
        assert!(s.crossfade);
        assert_eq!(s.crossfade_secs, CROSSFADE_SECS_DEFAULT);
        assert_eq!(s.device_id, "abc123");
    }

    /// `Settings::load` ajusta la duración guardada fuera de rango y la deja así en el archivo.
    #[test]
    fn load_ajusta_la_duracion_del_fundido() {
        let dir = std::env::temp_dir().join(format!("nanofy-test-fundido-{}", random_hex()));
        let paths = Paths { config_dir: dir.clone(), state_dir: dir.clone(), cache_dir: dir.clone() };
        std::fs::create_dir_all(&dir).unwrap();
        for (saved, want) in [(0u8, 1u8), (3, 3), (40, 12)] {
            let text = format!(r#"{{"device_id":"abc123","crossfade":true,"crossfade_secs":{saved},"crossfade_albums":true}}"#);
            std::fs::write(paths.settings_file(), text).unwrap();
            let s = Settings::load(&paths);
            assert_eq!(s.crossfade_secs, want, "guardado {saved}");
            assert!(s.crossfade && s.crossfade_albums);
            assert_eq!(s.device_id, "abc123");
            let back: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(paths.settings_file()).unwrap()).unwrap();
            assert_eq!(back["crossfade_secs"], want);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Una sola traducción Quality → Bitrate y un solo orden de formatos para reproducir y para
    /// descargar; ninguno pide FLAC y todos acaban en 96 kbps (los podcasts suelen ir solo a 96).
    #[test]
    fn calidad_y_orden_de_formatos() {
        use librespot_metadata::audio::AudioFileFormat as F;
        use librespot_playback::{config::Bitrate, player::format_order};
        assert_eq!(Quality::Low.bitrate(), Bitrate::Bitrate96);
        assert_eq!(Quality::Normal.bitrate(), Bitrate::Bitrate160);
        assert_eq!(Quality::High.bitrate(), Bitrate::Bitrate320);
        for (q, first) in [(Quality::Low, F::OGG_VORBIS_96), (Quality::Normal, F::OGG_VORBIS_160), (Quality::High, F::OGG_VORBIS_320)] {
            let order = format_order(q.bitrate());
            assert_eq!(order.first(), Some(&first), "{q:?}");
            for ogg in [F::OGG_VORBIS_96, F::OGG_VORBIS_160, F::OGG_VORBIS_320] {
                assert!(order.contains(&ogg), "{q:?} sin {ogg:?}");
            }
            assert!(!order.iter().any(|f| matches!(f, F::FLAC_FLAC | F::FLAC_FLAC_24BIT)), "{q:?} pide FLAC");
        }
    }

    /// Lo que ya está en la caché gana: una canción descargada en otra calidad sigue sonando sin
    /// red tras cambiar la calidad. Entre varias en caché manda el orden; fuera del orden, nada.
    #[test]
    fn seleccion_de_formato_con_cache_primero() {
        use librespot_core::FileId;
        use librespot_metadata::audio::AudioFileFormat as F;
        use librespot_playback::player::{format_order, pick_audio_file};
        use std::collections::HashMap;
        let id = |b: u8| FileId([b; 20]);
        let files: HashMap<F, FileId> =
            [(F::OGG_VORBIS_96, id(1)), (F::OGG_VORBIS_160, id(2)), (F::OGG_VORBIS_320, id(3)), (F::FLAC_FLAC, id(9))].into();
        let high = format_order(Quality::High.bitrate());
        let normal = format_order(Quality::Normal.bitrate());
        let cached = |ids: &'static [u8]| move |f: FileId| ids.iter().any(|&b| f == id(b));

        // Nada en caché: el primero disponible del orden (como descarga el downloader).
        assert_eq!(pick_audio_file(high, &files, |_| false), Some((F::OGG_VORBIS_320, id(3))));
        assert_eq!(pick_audio_file(normal, &files, |_| false), Some((F::OGG_VORBIS_160, id(2))));
        // Descargada en «Alta» (160) y ahora en «Muy alta»: suena la de la caché.
        assert_eq!(pick_audio_file(high, &files, cached(&[2])), Some((F::OGG_VORBIS_160, id(2))));
        // Al revés: descargada en 320 y ahora en 160.
        assert_eq!(pick_audio_file(normal, &files, cached(&[3])), Some((F::OGG_VORBIS_320, id(3))));
        // Varias en caché: manda el orden de la calidad elegida.
        assert_eq!(pick_audio_file(high, &files, cached(&[1, 2, 3])), Some((F::OGG_VORBIS_320, id(3))));
        assert_eq!(pick_audio_file(normal, &files, cached(&[1, 3])), Some((F::OGG_VORBIS_96, id(1))));
        // Un FLAC en la caché nunca se elige.
        assert_eq!(pick_audio_file(high, &files, cached(&[9])), Some((F::OGG_VORBIS_320, id(3))));
        // Podcast solo en 96 kbps: suena en cualquier calidad.
        let podcast: HashMap<F, FileId> = [(F::OGG_VORBIS_96, id(1))].into();
        assert_eq!(pick_audio_file(high, &podcast, |_| false), Some((F::OGG_VORBIS_96, id(1))));
        // Para la etiqueta de calidad: ¿se eligió por la caché o porque no había otra?
        use librespot_playback::player::picked_from_cache;
        assert!(!picked_from_cache(high, &files, F::OGG_VORBIS_320));
        assert!(picked_from_cache(high, &files, F::OGG_VORBIS_160), "copia guardada en 160");
        assert!(picked_from_cache(normal, &files, F::OGG_VORBIS_320), "copia guardada en 320");
        let only_160: HashMap<F, FileId> = [(F::OGG_VORBIS_96, id(1)), (F::OGG_VORBIS_160, id(2))].into();
        assert!(!picked_from_cache(high, &only_160, F::OGG_VORBIS_160), "no existe en 320");
        assert!(!picked_from_cache(high, &podcast, F::OGG_VORBIS_96));
        // Sin formatos compatibles.
        let flac_only: HashMap<F, FileId> = [(F::FLAC_FLAC, id(9)), (F::AAC_320, id(8))].into();
        assert_eq!(pick_audio_file(high, &flac_only, |_| true), None);
        assert_eq!(pick_audio_file(high, &HashMap::new(), |_| true), None);
    }

    /// La marca de «ya se pidió la autorización de la biblioteca» no existe en los settings.json
    /// de antes: se lee como no pedida (sin tocar lo demás) y, una vez puesta, se conserva.
    #[test]
    fn la_marca_de_la_biblioteca_se_lee_de_archivos_viejos() {
        assert!(!Settings::default().library_consent_asked);
        let old: Settings = serde_json::from_str(r#"{"device_id":"abc123","client_id":"x","pinned":["p1"]}"#).unwrap();
        assert!(!old.library_consent_asked);
        assert_eq!(old.device_id, "abc123");
        assert_eq!(old.pinned, ["p1"]);
        let mut s = old.clone();
        s.library_consent_asked = true;
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert!(back.library_consent_asked);
        assert_eq!(back, s);
    }
}
