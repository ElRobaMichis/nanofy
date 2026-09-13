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

/// Calidad de audio pedida a Spotify.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Quality {
    /// Ogg Vorbis 96 kbps
    Low,
    /// Ogg Vorbis 160 kbps
    Normal,
    /// Ogg Vorbis 320 kbps
    #[default]
    High,
    /// FLAC sin pérdida si la cuenta y la canción lo permiten; si no, 320 kbps.
    Lossless,
}

impl Quality {
    pub fn label(self) -> &'static str {
        match self {
            Quality::Low => "Baja (96 kbps)",
            Quality::Normal => "Normal (160 kbps)",
            Quality::High => "Alta (320 kbps)",
            Quality::Lossless => "Sin pérdida (FLAC, experimental)",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Nombre con el que aparece este equipo en Spotify Connect.
    pub device_name: String,
    /// Identificador estable del dispositivo (se genera una vez).
    pub device_id: String,
    pub quality: Quality,
    pub normalisation: bool,
    pub gapless: bool,
    pub autoplay: bool,
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
    /// Client ID de tu app de desarrollador de Spotify (para la Web API).
    pub client_id: String,
    /// Playlists fijadas en la biblioteca (ids).
    pub pinned: Vec<String>,
    /// Biblioteca en cuadrícula (true) o lista.
    pub library_grid: bool,
    /// Secciones del inicio fijadas arriba y ocultas (ids de sección).
    pub home_pinned: Vec<String>,
    pub home_hidden: Vec<String>,
    /// Orden de las secciones del inicio, playlists añadidas como sección y filas recomendadas.
    pub home_order: Vec<String>,
    pub home_custom: Vec<String>,
    pub home_recs: bool,
    /// Consultar GitHub al arrancar (y cada pocas horas) y avisar si hay una versión nueva.
    pub update_check: bool,
    /// Versión que el usuario pidió omitir (no se vuelve a avisar de ella).
    pub update_skipped: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            device_name: "Nanofy".to_string(),
            device_id: random_hex(),
            quality: Quality::High,
            normalisation: false,
            gapless: true,
            autoplay: true,
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
            pinned: Vec::new(),
            library_grid: true,
            home_pinned: Vec::new(),
            home_hidden: Vec::new(),
            home_order: Vec::new(),
            home_custom: Vec::new(),
            home_recs: true,
            update_check: true,
            update_skipped: String::new(),
        }
    }
}

impl Settings {
    pub fn load(paths: &Paths) -> Self {
        let file = paths.settings_file();
        let mut settings = match std::fs::read_to_string(&file) {
            Ok(text) => serde_json::from_str::<Settings>(text.trim_start_matches('\u{feff}')).unwrap_or_else(|e| {
                log::warn!("settings.json inválido ({e}); se usan los valores por defecto");
                Settings::default()
            }),
            Err(_) => Settings::default(),
        };
        if settings.device_id.is_empty() {
            settings.device_id = random_hex();
        }
        settings.zoom = settings.zoom.clamp(0.7, 2.0);
        settings.fps_cap = settings.fps_cap.clamp(30, 480);
        // Guardamos siempre para que el archivo exista y el device_id quede fijo.
        settings.save(paths);
        settings
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

    /// Ajustes que obligan a reiniciar el reproductor de librespot.
    pub fn playback_differs(&self, other: &Settings) -> bool {
        self.device_name != other.device_name
            || self.quality != other.quality
            || self.normalisation != other.normalisation
            || self.gapless != other.gapless
            || self.autoplay != other.autoplay
            || self.audio_cache_mb != other.audio_cache_mb
    }
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
