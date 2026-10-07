use std::{mem, path::PathBuf, str::FromStr, time::Duration};

pub use crate::dither::{DithererBuilder, TriangularDitherer, mk_ditherer};
use crate::{convert::i24, player::duration_to_coefficient};

#[derive(Clone, Copy, Debug, Hash, PartialOrd, Ord, PartialEq, Eq, Default)]
pub enum Bitrate {
    Bitrate96,
    #[default]
    Bitrate160,
    Bitrate320,
    // Sin variante «sin pérdida»: las claves de los FLAC de Spotify están tras su DRM y sus
    // metadatos (TRACK_V4) ni siquiera los listan, así que Nanofy nunca debe pedir una.
}

impl FromStr for Bitrate {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "96" => Ok(Self::Bitrate96),
            "160" => Ok(Self::Bitrate160),
            "320" => Ok(Self::Bitrate320),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialOrd, Ord, PartialEq, Eq, Default)]
pub enum AudioFormat {
    F64,
    F32,
    S32,
    S24,
    S24_3,
    #[default]
    S16,
}

impl FromStr for AudioFormat {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_ref() {
            "F64" => Ok(Self::F64),
            "F32" => Ok(Self::F32),
            "S32" => Ok(Self::S32),
            "S24" => Ok(Self::S24),
            "S24_3" => Ok(Self::S24_3),
            "S16" => Ok(Self::S16),
            _ => Err(()),
        }
    }
}

impl AudioFormat {
    // not used by all backends
    #[allow(dead_code)]
    pub fn size(&self) -> usize {
        match self {
            Self::F64 => mem::size_of::<f64>(),
            Self::F32 => mem::size_of::<f32>(),
            Self::S24_3 => mem::size_of::<i24>(),
            Self::S16 => mem::size_of::<i16>(),
            _ => mem::size_of::<i32>(), // S32 and S24 are both stored in i32
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NormalisationType {
    Album,
    Track,
    #[default]
    Auto,
}

impl FromStr for NormalisationType {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_ref() {
            "album" => Ok(Self::Album),
            "track" => Ok(Self::Track),
            "auto" => Ok(Self::Auto),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NormalisationMethod {
    Basic,
    #[default]
    Dynamic,
}

impl FromStr for NormalisationMethod {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_ref() {
            "basic" => Ok(Self::Basic),
            "dynamic" => Ok(Self::Dynamic),
            _ => Err(()),
        }
    }
}

#[derive(Clone)]
pub struct PlayerConfig {
    pub bitrate: Bitrate,
    pub gapless: bool,
    pub passthrough: bool,

    pub normalisation: bool,
    pub normalisation_type: NormalisationType,
    pub normalisation_method: NormalisationMethod,
    pub normalisation_pregain_db: f64,
    pub normalisation_threshold_dbfs: f64,
    pub normalisation_attack_cf: f64,
    pub normalisation_release_cf: f64,
    pub normalisation_knee_db: f64,

    /// Fundido entre canciones, en milisegundos; 0 lo apaga (por defecto, como en Spotify). Se
    /// cambia en vivo con `Player::set_crossfade`, sin reiniciar el reproductor.
    pub crossfade_ms: u32,
    /// Fundir también dos pistas seguidas de un mismo álbum, que Spotify deja sin fundir para
    /// respetar las transiciones del artista.
    pub crossfade_albums: bool,

    pub local_file_directories: Vec<PathBuf>,

    // pass function pointers so they can be lazily instantiated *after* spawning a thread
    // (thereby circumventing Send bounds that they might not satisfy)
    pub ditherer: Option<DithererBuilder>,
    /// Setting this will enable periodically sending events during playback informing about the playback position
    /// To consume the PlayerEvent::PositionChanged event, listen to events via `Player::get_player_event_channel()``
    pub position_update_interval: Option<Duration>,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            bitrate: Bitrate::default(),
            gapless: true,
            normalisation: false,
            normalisation_type: NormalisationType::default(),
            normalisation_method: NormalisationMethod::default(),
            normalisation_pregain_db: 0.0,
            normalisation_threshold_dbfs: -2.0,
            normalisation_attack_cf: duration_to_coefficient(Duration::from_millis(5)),
            normalisation_release_cf: duration_to_coefficient(Duration::from_millis(100)),
            normalisation_knee_db: 5.0,
            crossfade_ms: 0,
            crossfade_albums: false,
            passthrough: false,
            ditherer: Some(mk_ditherer::<TriangularDitherer>),
            position_update_interval: None,
            local_file_directories: Vec::new(),
        }
    }
}

/// La parte de `PlayerConfig` que se puede cambiar con el reproductor en marcha
/// (`Player::set_audio_tuning`), sin reiniciar la sesión: la calidad y gapless valen desde la
/// próxima canción que se cargue (el cargador copia la configuración en cada carga) y la
/// normalización, al instante.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioTuning {
    pub bitrate: Bitrate,
    pub gapless: bool,
    pub normalisation: bool,
    pub normalisation_type: NormalisationType,
    pub normalisation_method: NormalisationMethod,
    pub normalisation_pregain_db: f64,
    pub normalisation_threshold_dbfs: f64,
    pub normalisation_attack_cf: f64,
    pub normalisation_release_cf: f64,
    pub normalisation_knee_db: f64,
}

impl PlayerConfig {
    pub fn tuning(&self) -> AudioTuning {
        AudioTuning {
            bitrate: self.bitrate,
            gapless: self.gapless,
            normalisation: self.normalisation,
            normalisation_type: self.normalisation_type,
            normalisation_method: self.normalisation_method,
            normalisation_pregain_db: self.normalisation_pregain_db,
            normalisation_threshold_dbfs: self.normalisation_threshold_dbfs,
            normalisation_attack_cf: self.normalisation_attack_cf,
            normalisation_release_cf: self.normalisation_release_cf,
            normalisation_knee_db: self.normalisation_knee_db,
        }
    }

    pub fn set_tuning(&mut self, t: &AudioTuning) {
        self.bitrate = t.bitrate;
        self.gapless = t.gapless;
        self.normalisation = t.normalisation;
        self.normalisation_type = t.normalisation_type;
        self.normalisation_method = t.normalisation_method;
        self.normalisation_pregain_db = t.normalisation_pregain_db;
        self.normalisation_threshold_dbfs = t.normalisation_threshold_dbfs;
        self.normalisation_attack_cf = t.normalisation_attack_cf;
        self.normalisation_release_cf = t.normalisation_release_cf;
        self.normalisation_knee_db = t.normalisation_knee_db;
    }
}

// fields are intended for volume control range in dB
#[derive(Clone, Copy, Debug)]
pub enum VolumeCtrl {
    Cubic(f64),
    Fixed,
    Linear,
    Log(f64),
}

impl FromStr for VolumeCtrl {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_str_with_range(s, Self::DEFAULT_DB_RANGE)
    }
}

impl Default for VolumeCtrl {
    fn default() -> VolumeCtrl {
        VolumeCtrl::Log(Self::DEFAULT_DB_RANGE)
    }
}

impl VolumeCtrl {
    pub const MAX_VOLUME: u16 = u16::MAX;

    // Taken from: https://www.dr-lex.be/info-stuff/volumecontrols.html
    pub const DEFAULT_DB_RANGE: f64 = 60.0;

    pub fn from_str_with_range(s: &str, db_range: f64) -> Result<Self, <Self as FromStr>::Err> {
        use self::VolumeCtrl::*;
        match s.to_lowercase().as_ref() {
            "cubic" => Ok(Cubic(db_range)),
            "fixed" => Ok(Fixed),
            "linear" => Ok(Linear),
            "log" => Ok(Log(db_range)),
            _ => Err(()),
        }
    }
}
