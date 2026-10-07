#![warn(missing_docs)]
#![doc=include_str!("../README.md")]

#[macro_use]
extern crate log;

use librespot_core as core;
use librespot_playback as playback;
use librespot_protocol as protocol;

mod cascade;
mod context_resolver;
mod model;
mod shuffle_vec;
mod spirc;
/// Con qué canción del contexto empieza una carga (Nanofy).
mod start_index;
mod state;
mod state_queue;
mod state_sender;

pub use model::*;
pub use spirc::*;
pub use state::*;
