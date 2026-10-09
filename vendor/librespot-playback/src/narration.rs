//! Nanofy: las frases del locutor del DJ de Spotify.
//!
//! Una canción de una sesión del DJ trae en sus metadatos hasta tres textos (SSML) con su voz y su
//! motor de síntesis: `narration.intro` (al llegar a ella en orden), `narration.jump` (al saltar a
//! ella, con el botón del DJ) y `narration.outro` (al terminar). Spirc dice al reproductor qué
//! frases tiene cada canción (`Player::narrate`) al cargarla o precargarla; el reproductor las
//! pide en cuanto lo sabe (al servicio de voz de Spotify, que devuelve un MP3), las decodifica aquí
//! y las hace sonar antes (o después) de la canción por el mismo camino que ella: volumen,
//! normalización y fundido con la anterior.

use std::{
    collections::{HashMap, VecDeque},
    io::{Cursor, ErrorKind},
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use futures_util::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use symphonia::core::{
    audio::SampleBuffer, codecs::DecoderOptions, errors::Error as SymphoniaError,
    formats::FormatOptions, io::MediaSourceStream, meta::MetadataOptions, probe::Hint,
};
use tokio::sync::oneshot;

use crate::{
    NUM_CHANNELS, SAMPLE_RATE,
    core::{Session, SpotifyUri},
    player::db_to_ratio,
};

pub const INTRO: &str = "narration.intro";
pub const JUMP: &str = "narration.jump";
pub const OUTRO: &str = "narration.outro";

/// Nivel de referencia de Spotify (LUFS) y el que declara el DJ para sus frases.
const LOUDNESS_TARGET: f64 = -14.0;
const DEFAULT_LOUDNESS: f64 = -16.0;
const DEFAULT_TRUE_PEAK: f64 = -3.0;
/// Lo más que una canción espera a su frase: el locutor es un adorno, la música no.
const WAIT_FOR_CLIP: Duration = Duration::from_secs(6);
/// Marcos por escritura de la frase (≈ 46 ms) y de silencio mientras llega (20 ms).
const CHUNK_FRAMES: usize = 2048;
const SILENCE_FRAMES: usize = 882;
/// Canciones de las que se guardan frases: la que suena, la siguiente y alguna anterior.
const MAX_SLOTS: usize = 4;

/// Una frase: lo que hace falta para sintetizarla y lo que se enseña mientras suena.
#[derive(Debug, Clone, PartialEq)]
pub struct NarrationRequest {
    pub ssml: String,
    /// Enum `TtsVoice` del cliente de Spotify (`VOICE35` = 35).
    pub voice: i32,
    /// Enum `TtsProvider` del cliente de Spotify.
    pub provider: i32,
    pub loudness: f64,
    pub true_peak: f64,
    /// «Up next», «DJ Livi» y su imagen: lo que la app de Spotify enseña mientras habla.
    pub title: String,
    pub artist: String,
    pub image: String,
}

impl NarrationRequest {
    /// La frase `prefix` (`INTRO`, `JUMP` u `OUTRO`) de los metadatos de una canción, si tiene
    /// texto: el resto de sus metadatos viene aunque no haya nada que decir.
    pub fn from_metadata(metadata: &HashMap<String, String>, prefix: &str) -> Option<Self> {
        let get = |key: &str| {
            metadata
                .get(&format!("{prefix}.{key}"))
                .map(String::as_str)
                .unwrap_or_default()
        };
        let ssml = get("ssml");
        if ssml.trim().is_empty() {
            return None;
        }
        let number = |key: &str, default: f64| {
            get(key)
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .unwrap_or(default)
        };
        Some(Self {
            ssml: ssml.to_string(),
            voice: voice_number(get("voice")),
            provider: provider_number(get("tts_provider")),
            loudness: number("loudness", DEFAULT_LOUDNESS),
            true_peak: number("true_peak", DEFAULT_TRUE_PEAK),
            title: get("title").to_string(),
            artist: get("artist").to_string(),
            image: get("image").to_string(),
        })
    }

    /// Factor de nivel de la frase: lo que le falta hasta el nivel de referencia de Spotify más
    /// el ajuste del usuario, sin que su pico pase de 0 dBFS. Sin normalización, tal cual llega.
    pub fn gain(&self, normalisation: bool, pregain_db: f64) -> f64 {
        if !normalisation {
            return 1.0;
        }
        let factor = db_to_ratio(LOUDNESS_TARGET - self.loudness + pregain_db);
        let peak = db_to_ratio(self.true_peak);
        if peak > 0.0 && factor * peak > 1.0 {
            1.0 / peak
        } else {
            factor
        }
    }
}

/// `VOICE35` → 35. Sin número, la primera voz.
fn voice_number(name: &str) -> i32 {
    name.trim()
        .strip_prefix("VOICE")
        .and_then(|n| n.parse().ok())
        .unwrap_or(1)
}

/// El motor de síntesis por su nombre; uno desconocido, el que usa el DJ (`SONANTIC_FAST`).
fn provider_number(name: &str) -> i32 {
    match name.trim() {
        "CLOUD_TTS" => 1,
        "READSPEAKER" => 2,
        "POLLY" => 3,
        "WELL_SAID" => 4,
        "SONANTIC_DEPRECATED" => 5,
        "OPENAI" => 7,
        "SONANTIC_LARGE" => 8,
        "ELEVEN_LABS_DEPRECATED" => 9,
        "ADS_PUBLIC" => 10,
        _ => 6,
    }
}

/// El audio de una frase cuando llegue: estéreo intercalado a 44,1 kHz. Compartido, para que la
/// misma frase pedida al precargar sirva al sonar.
type ClipAudio = Shared<BoxFuture<'static, Result<Arc<Vec<f64>>, String>>>;

#[derive(Clone)]
struct Clip {
    request: NarrationRequest,
    audio: ClipAudio,
}

impl Clip {
    /// Empieza a pedir la frase en el runtime del reproductor (síntesis, descarga y decodificación).
    fn fetch(session: &Session, request: NarrationRequest) -> Self {
        let (tx, rx) = oneshot::channel();
        let session = session.clone();
        let req = request.clone();
        tokio::runtime::Handle::current().spawn(async move {
            let started = Instant::now();
            let result = async {
                let mp3 = session
                    .spclient()
                    .narration_audio(&req.ssml, req.voice, req.provider, SAMPLE_RATE as i32)
                    .await
                    .map_err(|e| e.to_string())?;
                let bytes = mp3.len();
                let samples = tokio::task::spawn_blocking(move || decode(mp3))
                    .await
                    .map_err(|e| e.to_string())??;
                info!(
                    "[dj] frase lista en {} ms ({bytes} bytes, {:.1} s)",
                    started.elapsed().as_millis(),
                    samples.len() as f64 / (SAMPLE_RATE as f64 * NUM_CHANNELS as f64)
                );
                Ok(Arc::new(samples))
            }
            .await;
            let _ = tx.send(result);
        });
        let audio = rx
            .map(|r| r.unwrap_or_else(|_| Err("la petición se interrumpió".to_string())))
            .boxed()
            .shared();
        Self { request, audio }
    }
}

/// MP3 → estéreo intercalado a 44,1 kHz. Las frases llegan en mono (se duplica) y, si llegaran a
/// otra frecuencia, se interpola.
fn decode(mp3: Vec<u8>) -> Result<Vec<f64>, String> {
    let mss = MediaSourceStream::new(Box::new(Cursor::new(mp3)), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("formato: {e}"))?;
    let mut format = probed.format;
    let track = format.default_track().ok_or("sin pista de audio")?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("decodificador: {e}"))?;
    let mut rate = track.codec_params.sample_rate.unwrap_or(SAMPLE_RATE);

    let mut out = Vec::new();
    let mut buffer: Option<(SampleBuffer<f64>, u64)> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(e)) if e.kind() == ErrorKind::UnexpectedEof => break,
            Err(e) if !out.is_empty() => {
                debug!("[dj] fin de la frase: {e}");
                break;
            }
            Err(e) => return Err(format!("lectura: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("decodificación: {e}")),
        };
        let spec = *decoded.spec();
        rate = spec.rate;
        let channels = spec.channels.count().max(1);
        let frames = decoded.capacity() as u64;
        if buffer.as_ref().is_none_or(|(_, cap)| *cap < frames) {
            buffer = Some((SampleBuffer::new(frames, spec), frames));
        }
        let (buf, _) = buffer.as_mut().expect("creado arriba");
        buf.copy_interleaved_ref(decoded);
        for frame in buf.samples().chunks(channels) {
            let left = frame[0];
            let right = if channels > 1 { frame[1] } else { left };
            out.push(left);
            out.push(right);
        }
    }
    if out.is_empty() {
        return Err("frase vacía".to_string());
    }
    if rate != SAMPLE_RATE && rate > 0 {
        out = resample(&out, rate);
    }
    Ok(out)
}

/// Estéreo intercalado de `from` Hz a 44,1 kHz, interpolando entre marcos (de sobra para voz).
fn resample(stereo: &[f64], from: u32) -> Vec<f64> {
    let frames = stereo.len() / 2;
    let out_frames = (frames as u64 * SAMPLE_RATE as u64 / from as u64) as usize;
    let step = from as f64 / SAMPLE_RATE as f64;
    let mut out = Vec::with_capacity(out_frames * 2);
    for i in 0..out_frames {
        let pos = i as f64 * step;
        let j = (pos.floor() as usize).min(frames - 1);
        let k = (j + 1).min(frames - 1);
        let t = pos - j as f64;
        for c in 0..2 {
            out.push(stereo[j * 2 + c] * (1.0 - t) + stereo[k * 2 + c] * t);
        }
    }
    out
}

/// La frase que suena (o espera a llegar) delante o detrás de una canción.
pub(crate) struct Speaking {
    pub play_request_id: u64,
    pub outro: bool,
    pub request: NarrationRequest,
    audio: ClipAudio,
    samples: Option<Arc<Vec<f64>>>,
    pos: usize,
    deadline: Instant,
}

/// Lo siguiente que debe sonar de la frase en curso.
pub(crate) enum Step {
    /// Un bloque de la frase.
    Chunk(Vec<f64>),
    /// Aún no ha llegado: un poco de silencio mientras la canción la espera.
    Wait(Vec<f64>),
    /// Terminó (o no llegó a tiempo): sigue la canción, o se acaba si era la de salida.
    Done { outro: bool },
}

/// Las frases conocidas por canción y la que suena.
#[derive(Default)]
pub(crate) struct Narrations {
    slots: VecDeque<(SpotifyUri, Option<Clip>, Option<Clip>)>,
    pub speaking: Option<Speaking>,
}

impl Narrations {
    /// Frases de `track_id` (Spirc, al cargarla o precargarla). Una frase que ya se estaba
    /// pidiendo con el mismo texto se aprovecha; sin ninguna, la canción se olvida.
    pub fn set(
        &mut self,
        session: &Session,
        track_id: SpotifyUri,
        intro: Option<NarrationRequest>,
        outro: Option<NarrationRequest>,
    ) {
        let clip = |request: Option<NarrationRequest>| {
            let request = request?;
            let known = self
                .slots
                .iter()
                .flat_map(|(_, a, b)| [a, b])
                .flatten()
                .find(|c| c.request == request)
                .cloned();
            Some(known.unwrap_or_else(|| Clip::fetch(session, request)))
        };
        let intro = clip(intro);
        let outro = clip(outro);
        self.slots.retain(|(id, ..)| *id != track_id);
        if intro.is_some() || outro.is_some() {
            self.slots.push_back((track_id, intro, outro));
            while self.slots.len() > MAX_SLOTS {
                self.slots.pop_front();
            }
        }
    }

    /// Empieza la frase de entrada de `track_id`, si tiene: la canción empieza a sonar desde el
    /// principio. Lo que sonara antes de otra canción se deja.
    pub fn begin_intro(&mut self, track_id: &SpotifyUri, play_request_id: u64) -> Option<&NarrationRequest> {
        self.speaking = None;
        let clip = self
            .slots
            .iter()
            .find(|(id, ..)| id == track_id)
            .and_then(|(_, intro, _)| intro.clone())?;
        self.speaking = Some(Speaking::new(play_request_id, false, clip));
        self.speaking.as_ref().map(|s| &s.request)
    }

    /// Empieza la frase de salida de `track_id`, si tiene: la canción acaba de terminar.
    pub fn begin_outro(&mut self, track_id: &SpotifyUri, play_request_id: u64) -> Option<&NarrationRequest> {
        let clip = self
            .slots
            .iter()
            .find(|(id, ..)| id == track_id)
            .and_then(|(_, _, outro)| outro.clone())?;
        self.speaking = Some(Speaking::new(play_request_id, true, clip));
        self.speaking.as_ref().map(|s| &s.request)
    }

    /// ¿Tiene `track_id` frase de salida? Entonces no se funde con la siguiente: se la comería.
    pub fn has_outro(&self, track_id: &SpotifyUri) -> bool {
        self.slots
            .iter()
            .any(|(id, _, outro)| id == track_id && outro.is_some())
    }

    /// Lo siguiente de la frase en curso; al terminar, deja de haberla.
    pub fn step(&mut self, cx: &mut Context<'_>) -> Step {
        let Some(s) = self.speaking.as_mut() else {
            return Step::Done { outro: false };
        };
        let outro = s.outro;
        if s.samples.is_none() {
            match s.audio.poll_unpin(cx) {
                Poll::Ready(Ok(samples)) => s.samples = Some(samples),
                Poll::Ready(Err(e)) => {
                    warn!("[dj] la frase del locutor no se pudo preparar: {e}");
                    self.speaking = None;
                    return Step::Done { outro };
                }
                Poll::Pending if Instant::now() >= s.deadline => {
                    warn!(
                        "[dj] la frase del locutor no llegó en {} s: sigue la música",
                        WAIT_FOR_CLIP.as_secs()
                    );
                    self.speaking = None;
                    return Step::Done { outro };
                }
                Poll::Pending => {
                    return Step::Wait(vec![0.0; SILENCE_FRAMES * NUM_CHANNELS as usize]);
                }
            }
        }
        let samples = s.samples.as_ref().expect("comprobado arriba");
        let end = (s.pos + CHUNK_FRAMES * NUM_CHANNELS as usize).min(samples.len());
        if s.pos >= end {
            self.speaking = None;
            return Step::Done { outro };
        }
        let chunk = samples[s.pos..end].to_vec();
        s.pos = end;
        Step::Chunk(chunk)
    }
}

impl Speaking {
    fn new(play_request_id: u64, outro: bool, clip: Clip) -> Self {
        Self {
            play_request_id,
            outro,
            request: clip.request,
            audio: clip.audio,
            samples: None,
            pos: 0,
            deadline: Instant::now() + WAIT_FOR_CLIP,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frase_de_los_metadatos() {
        let mut md = HashMap::new();
        md.insert("narration.intro.ssml".to_string(), "<speak>hola</speak>".to_string());
        md.insert("narration.intro.voice".to_string(), "VOICE35".to_string());
        md.insert("narration.intro.tts_provider".to_string(), "SONANTIC_FAST".to_string());
        md.insert("narration.intro.loudness".to_string(), "-16.0".to_string());
        md.insert("narration.intro.artist".to_string(), "DJ Livi".to_string());
        md.insert("narration.outro.voice".to_string(), "VOICE35".to_string());
        let intro = NarrationRequest::from_metadata(&md, INTRO).expect("tiene texto");
        assert_eq!(intro.voice, 35);
        assert_eq!(intro.provider, 6);
        assert_eq!(intro.true_peak, DEFAULT_TRUE_PEAK);
        assert_eq!(intro.artist, "DJ Livi");
        // Sin texto no hay frase aunque haya el resto.
        assert!(NarrationRequest::from_metadata(&md, OUTRO).is_none());
    }

    #[test]
    fn nivel_de_la_frase() {
        let mut md = HashMap::new();
        md.insert("narration.intro.ssml".to_string(), "x".to_string());
        md.insert("narration.intro.loudness".to_string(), "-16.0".to_string());
        md.insert("narration.intro.true_peak".to_string(), "-3.0".to_string());
        let r = NarrationRequest::from_metadata(&md, INTRO).unwrap();
        assert_eq!(r.gain(false, 0.0), 1.0);
        // −16 LUFS hasta −14: +2 dB.
        assert!((r.gain(true, 0.0) - db_to_ratio(2.0)).abs() < 1e-9);
        // Con +6 dB de ajuste el pico (−3 dBTP) pasaría de 0 dBFS: se queda en +3 dB.
        assert!((r.gain(true, 6.0) - db_to_ratio(3.0)).abs() < 1e-9);
    }

    #[test]
    fn remuestreo_de_mono_a_44100() {
        let stereo: Vec<f64> = (0..2400).flat_map(|i| [i as f64, i as f64]).collect();
        let out = resample(&stereo, 24000);
        assert_eq!(out.len() / 2, 2400 * 44100 / 24000);
        assert_eq!(out[0], 0.0);
        assert!((out[out.len() - 2] - 2399.0).abs() < 1.0);
    }
}
