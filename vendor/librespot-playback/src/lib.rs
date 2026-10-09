#[macro_use]
extern crate log;

use librespot_audio as audio;
use librespot_core as core;
use librespot_metadata as metadata;

pub mod audio_backend;
pub mod config;
pub mod convert;
pub mod crossfade;
pub mod decoder;
pub mod dither;
pub mod gain;
mod local_file;
pub mod mixer;
pub mod narration;
pub mod player;
mod symphonia_util;

pub const SAMPLE_RATE: u32 = 44100;
pub const NUM_CHANNELS: u8 = 2;
pub const SAMPLES_PER_SECOND: u32 = SAMPLE_RATE * NUM_CHANNELS as u32;
pub const PAGES_PER_MS: f64 = SAMPLE_RATE as f64 / 1000.0;
pub const MS_PER_PAGE: f64 = 1000.0 / SAMPLE_RATE as f64;

// El mezclador del fundido lleva su propia copia de la frecuencia y los canales (el binario lo
// compila también por separado para sus pruebas): si un día cambian aquí, que no compile.
const _: () = assert!(
    crossfade::SAMPLE_RATE == SAMPLE_RATE && crossfade::CHANNELS == NUM_CHANNELS as usize
);
