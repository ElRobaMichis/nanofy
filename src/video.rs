//! Vídeo de los podcasts. Spotify lo sirve sin DRM (`encryption_infos` vacío en su manifiesto):
//! segmentos de MP4 fragmentado de 4 s, H.264 (perfil Main) en varias resoluciones. Aquí se
//! descargan los de una resolución, se separan sus muestras (cajas moof/trun/mdat), se decodifican
//! con OpenH264 y se dejan en una cola corta de fotogramas RGBA con su instante, que la interfaz
//! toma según la posición del audio (que sigue sonando por librespot, como siempre).

use std::collections::{BinaryHeap, VecDeque};
use std::cmp::Reverse;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Fotogramas decodificados por delante como mucho (a 854 × 480, ~1,6 MB cada uno).
const QUEUE: usize = 12;
/// Resolución que se pide: la mayor que no pase de esta altura (el marco de la página mide ~546).
const MAX_HEIGHT: u32 = 480;

/// Lo que hace falta del manifiesto de un vídeo para pedir sus segmentos.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct VideoManifest {
    pub base: String,
    pub init: String,
    pub segment: String,
    pub seg_len_ms: u64,
    pub end_ms: u64,
    pub profile: u32,
    pub width: u32,
    pub height: u32,
}

/// Manifiesto (`/manifests/v9/json/sources/…`) -> lo necesario, con la resolución elegida.
pub fn parse_manifest(v: &serde_json::Value) -> Result<VideoManifest, String> {
    let c = v["contents"].get(0).ok_or("manifiesto sin contenido")?;
    if c["encryption_infos"].as_array().is_some_and(|a| !a.is_empty()) {
        return Err("el vídeo va cifrado".into());
    }
    let profiles: Vec<&serde_json::Value> = c["profiles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["mime_type"].as_str() == Some("video/mp4") && p["file_type"].as_str() == Some("mp4"))
        .collect();
    let height = |p: &serde_json::Value| p["video_height"].as_u64().unwrap_or(0) as u32;
    let pick = profiles
        .iter()
        .filter(|p| height(p) <= MAX_HEIGHT)
        .max_by_key(|p| height(p))
        .or_else(|| profiles.iter().min_by_key(|p| height(p)))
        .ok_or("sin vídeo MP4 en el manifiesto")?;
    Ok(VideoManifest {
        base: v["base_urls"].get(0).and_then(|b| b.as_str()).ok_or("manifiesto sin servidor")?.to_string(),
        init: v["initialization_template"].as_str().ok_or("sin plantilla de inicio")?.to_string(),
        segment: v["segment_template"].as_str().ok_or("sin plantilla de segmentos")?.to_string(),
        seg_len_ms: c["segment_length"].as_u64().unwrap_or(4) * 1000,
        end_ms: c["end_time_millis"].as_u64().or(v["end_time_millis"].as_u64()).unwrap_or(0),
        profile: pick["id"].as_u64().unwrap_or(0) as u32,
        width: pick["video_width"].as_u64().unwrap_or(0) as u32,
        height: height(pick),
    })
}

// ------------------------------------------------------------------ MP4 fragmentado

/// Cajas hijas de `data`: (tipo, contenido).
fn boxes(data: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 8 <= data.len() {
        let mut size = u32::from_be_bytes(data[i..i + 4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = data[i + 4..i + 8].try_into().unwrap();
        let mut head = 8;
        if size == 1 && i + 16 <= data.len() {
            size = u64::from_be_bytes(data[i + 8..i + 16].try_into().unwrap()) as usize;
            head = 16;
        } else if size == 0 {
            size = data.len() - i;
        }
        if size < head || i + size > data.len() {
            break;
        }
        out.push((kind, &data[i + head..i + size]));
        i += size;
    }
    out
}

fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(data).into_iter().find(|(k, _)| k == kind).map(|(_, b)| b)
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4).map(|s| u32::from_be_bytes(s.try_into().unwrap()))
}

/// Lo del segmento de inicio: SPS y PPS (en Annex B), bytes de la longitud de cada NAL y escala
/// de tiempo de la pista.
#[derive(Clone, Debug, Default)]
pub struct Init {
    pub headers: Vec<u8>,
    pub nal_len: usize,
    pub timescale: u32,
}

pub fn parse_init(data: &[u8]) -> Result<Init, String> {
    let moov = child(data, b"moov").ok_or("inicio sin moov")?;
    let trak = child(moov, b"trak").ok_or("inicio sin trak")?;
    let mdia = child(trak, b"mdia").ok_or("inicio sin mdia")?;
    let mdhd = child(mdia, b"mdhd").ok_or("inicio sin mdhd")?;
    let timescale = if mdhd.first() == Some(&1) { be32(mdhd, 20) } else { be32(mdhd, 12) }.ok_or("mdhd corto")?;
    let stsd = child(child(child(mdia, b"minf").ok_or("sin minf")?, b"stbl").ok_or("sin stbl")?, b"stsd").ok_or("sin stsd")?;
    // stsd: versión/flags (4) y número de entradas (4); luego la entrada avc1: cabecera visual de 78
    // bytes y sus cajas (avcC entre ellas).
    let entries = boxes(stsd.get(8..).ok_or("stsd corto")?);
    let avc1 = entries.iter().find(|(k, _)| k == b"avc1" || k == b"avc3").ok_or("sin avc1")?.1;
    let avcc = child(avc1.get(78..).ok_or("avc1 corto")?, b"avcC").ok_or("sin avcC")?;
    let nal_len = (avcc.get(4).ok_or("avcC corto")? & 3) as usize + 1;
    let mut headers = Vec::new();
    let mut i = 5;
    let n_sps = (avcc.get(i).ok_or("avcC corto")? & 0x1f) as usize;
    i += 1;
    for _ in 0..n_sps {
        let len = u16::from_be_bytes(avcc.get(i..i + 2).ok_or("sps corto")?.try_into().unwrap()) as usize;
        headers.extend_from_slice(&[0, 0, 0, 1]);
        headers.extend_from_slice(avcc.get(i + 2..i + 2 + len).ok_or("sps corto")?);
        i += 2 + len;
    }
    let n_pps = *avcc.get(i).ok_or("avcC corto")? as usize;
    i += 1;
    for _ in 0..n_pps {
        let len = u16::from_be_bytes(avcc.get(i..i + 2).ok_or("pps corto")?.try_into().unwrap()) as usize;
        headers.extend_from_slice(&[0, 0, 0, 1]);
        headers.extend_from_slice(avcc.get(i + 2..i + 2 + len).ok_or("pps corto")?);
        i += 2 + len;
    }
    Ok(Init { headers, nal_len, timescale: timescale.max(1) })
}

/// Una muestra (un fotograma comprimido) con su instante de presentación en ms.
pub struct Sample {
    pub pts_ms: u64,
    pub data: Vec<u8>,
}

/// Las muestras de un segmento (moof + mdat), con sus NAL pasadas a Annex B.
pub fn parse_segment(data: &[u8], init: &Init) -> Result<Vec<Sample>, String> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    let all = boxes(data);
    for (k, moof) in &all {
        // Dónde empieza esta moof en `data` (los desplazamientos de trun son relativos a ella).
        let moof_start = moof.as_ptr() as usize - data.as_ptr() as usize - 8;
        if k != b"moof" {
            offset = moof_start;
            continue;
        }
        let traf = child(moof, b"traf").ok_or("moof sin traf")?;
        let tfhd = child(traf, b"tfhd").ok_or("traf sin tfhd")?;
        let tf_flags = be32(tfhd, 0).unwrap_or(0) & 0xff_ffff;
        let mut p = 8;
        let mut base = moof_start;
        if tf_flags & 0x01 != 0 {
            base = u64::from_be_bytes(tfhd.get(p..p + 8).ok_or("tfhd corto")?.try_into().unwrap()) as usize;
            p += 8;
        }
        if tf_flags & 0x02 != 0 {
            p += 4;
        }
        let def_dur = if tf_flags & 0x08 != 0 {
            let v = be32(tfhd, p).unwrap_or(0);
            p += 4;
            v
        } else {
            0
        };
        let def_size = if tf_flags & 0x10 != 0 { be32(tfhd, p).unwrap_or(0) } else { 0 };
        let tfdt = child(traf, b"tfdt");
        let mut dts: u64 = match tfdt {
            Some(t) if t.first() == Some(&1) => u64::from_be_bytes(t.get(4..12).ok_or("tfdt corto")?.try_into().unwrap()),
            Some(t) => be32(t, 4).unwrap_or(0) as u64,
            None => 0,
        };
        for (tk, trun) in boxes(traf) {
            if &tk != b"trun" {
                continue;
            }
            let flags = be32(trun, 0).unwrap_or(0) & 0xff_ffff;
            let version = trun[0];
            let count = be32(trun, 4).unwrap_or(0) as usize;
            let mut q = 8;
            let data_off = if flags & 0x01 != 0 {
                let v = be32(trun, q).unwrap_or(0) as i32;
                q += 4;
                v
            } else {
                0
            };
            if flags & 0x04 != 0 {
                q += 4;
            }
            let mut pos = (base as i64 + data_off as i64) as usize;
            for _ in 0..count {
                let dur = if flags & 0x100 != 0 {
                    let v = be32(trun, q).unwrap_or(0);
                    q += 4;
                    v
                } else {
                    def_dur
                };
                let size = if flags & 0x200 != 0 {
                    let v = be32(trun, q).unwrap_or(0);
                    q += 4;
                    v
                } else {
                    def_size
                } as usize;
                if flags & 0x400 != 0 {
                    q += 4;
                }
                let cto: i64 = if flags & 0x800 != 0 {
                    let v = be32(trun, q).unwrap_or(0);
                    q += 4;
                    if version == 0 { v as i64 } else { v as i32 as i64 }
                } else {
                    0
                };
                let raw = data.get(pos..pos + size).ok_or("muestra fuera del segmento")?;
                let pts = (dts as i64 + cto).max(0) as u64;
                out.push(Sample { pts_ms: pts * 1000 / init.timescale as u64, data: annex_b(raw, init.nal_len) });
                pos += size;
                dts += dur as u64;
            }
        }
        let _ = offset;
    }
    Ok(out)
}

/// NAL con su longitud delante (AVCC) -> con el prefijo 00 00 00 01 (Annex B).
fn annex_b(raw: &[u8], nal_len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + 16);
    let mut i = 0;
    while i + nal_len <= raw.len() {
        let mut n = 0usize;
        for k in 0..nal_len {
            n = (n << 8) | raw[i + k] as usize;
        }
        i += nal_len;
        if n == 0 || i + n > raw.len() {
            break;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&raw[i..i + n]);
        i += n;
    }
    out
}

// ------------------------------------------------------------------ reproductor

/// Un fotograma decodificado.
pub struct Frame {
    pub pts_ms: u64,
    pub size: [usize; 2],
    pub rgba: Vec<u8>,
}

#[derive(Default)]
struct Shared {
    frames: VecDeque<Frame>,
    /// Instante del audio que la interfaz va pidiendo.
    want_ms: u64,
    /// Sube al saltar a otro sitio: el hilo vacía la cola y empieza por el segmento de `want_ms`.
    generation: u64,
    stop: bool,
    error: Option<String>,
}

/// Un vídeo en curso: su hilo descarga y decodifica por delante de `want_ms`.
pub struct VideoPlayer {
    pub episode: String,
    shared: Arc<(Mutex<Shared>, Condvar)>,
}

impl Drop for VideoPlayer {
    fn drop(&mut self) {
        let (m, cv) = &*self.shared;
        m.lock().unwrap_or_else(|e| e.into_inner()).stop = true;
        cv.notify_all();
    }
}

impl VideoPlayer {
    pub fn start(episode: String, manifest: VideoManifest, start_ms: u64) -> Self {
        let shared = Arc::new((Mutex::new(Shared { want_ms: start_ms, ..Default::default() }), Condvar::new()));
        let s2 = shared.clone();
        let _ = std::thread::Builder::new().name("nanofy-video".into()).spawn(move || {
            if let Err(e) = worker(&s2, &manifest) {
                log::warn!("[vídeo] {e}");
                s2.0.lock().unwrap_or_else(|e| e.into_inner()).error = Some(e);
            }
        });
        Self { episode, shared }
    }

    pub fn error(&self) -> Option<String> {
        self.shared.0.lock().unwrap_or_else(|e| e.into_inner()).error.clone()
    }

    /// El fotograma para el instante `ms` del audio (el último que ya toca), si lo hay; los más
    /// viejos se descartan. Si `ms` cae lejos de lo que hay en la cola, el hilo salta allí.
    pub fn frame_at(&self, ms: u64) -> Option<Frame> {
        let (m, cv) = &*self.shared;
        let mut s = m.lock().unwrap_or_else(|e| e.into_inner());
        let jump = match (s.frames.front(), s.frames.back()) {
            (Some(a), Some(b)) => ms + 1500 < a.pts_ms || ms > b.pts_ms + 6000,
            _ => ms + 1500 < s.want_ms || ms > s.want_ms + 9000,
        };
        if jump {
            s.generation += 1;
            s.frames.clear();
        }
        s.want_ms = ms;
        let mut best = None;
        while s.frames.front().is_some_and(|f| f.pts_ms <= ms) {
            best = s.frames.pop_front();
        }
        cv.notify_all();
        best
    }
}

fn agent() -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .timeout_connect(Some(Duration::from_secs(6)))
            .tls_config(tls)
            .build(),
    )
}

fn fetch(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, String> {
    let mut r = agent.get(url).call().map_err(|e| format!("{e}"))?;
    r.body_mut().with_config().limit(64 << 20).read_to_vec().map_err(|e| e.to_string())
}

fn worker(shared: &Arc<(Mutex<Shared>, Condvar)>, m: &VideoManifest) -> Result<(), String> {
    use openh264::decoder::{DecodeOptions, Decoder, Flush};
    use openh264::formats::YUVSource;
    let (mtx, cv) = &**shared;
    let agent = agent();
    let fill = |t: &str, extra: &[(&str, String)]| {
        let mut s = format!("{}{}", m.base, t).replace("{{profile_id}}", &m.profile.to_string()).replace("{{file_type}}", "mp4");
        for (k, v) in extra {
            s = s.replace(k, v);
        }
        s
    };
    let init = parse_init(&fetch(&agent, &fill(&m.init, &[]))?)?;
    log::info!("[vídeo] {}×{} (perfil {}), segmentos de {} s", m.width, m.height, m.profile, m.seg_len_ms / 1000);
    let mut generation = u64::MAX;
    let mut seg = 0u64;
    let mut decoder = Decoder::new().map_err(|e| e.to_string())?;
    let mut pending: BinaryHeap<Reverse<u64>> = BinaryHeap::new();
    let opts = || DecodeOptions::new().flush_after_decode(Flush::NoFlush);
    loop {
        let (want, gen_now) = {
            let s = mtx.lock().unwrap_or_else(|e| e.into_inner());
            if s.stop {
                return Ok(());
            }
            (s.want_ms, s.generation)
        };
        if gen_now != generation {
            generation = gen_now;
            seg = want / m.seg_len_ms.max(1);
            decoder = Decoder::new().map_err(|e| e.to_string())?;
            pending.clear();
            let _ = decoder.decode_with_options(&init.headers, opts());
        }
        if m.end_ms > 0 && seg * m.seg_len_ms >= m.end_ms {
            // Al final: se espera a que salten o paren.
            let s = mtx.lock().unwrap_or_else(|e| e.into_inner());
            let _ = cv.wait_timeout(s, Duration::from_millis(300));
            continue;
        }
        let url = fill(&m.segment, &[("{{segment_timestamp}}", (seg * m.seg_len_ms / 1000).to_string())]);
        let data = match fetch(&agent, &url) {
            Ok(d) => d,
            Err(e) => {
                log::info!("[vídeo] segmento {seg}: {e}; se reintenta");
                std::thread::sleep(Duration::from_millis(800));
                continue;
            }
        };
        let samples = parse_segment(&data, &init)?;
        'samples: for smp in samples {
            pending.push(Reverse(smp.pts_ms));
            let out = match decoder.decode_with_options(&smp.data, opts()) {
                Ok(Some(yuv)) => {
                    let (w, h) = yuv.dimensions();
                    let mut rgba = vec![0u8; w * h * 4];
                    yuv.write_rgba8(&mut rgba);
                    Some(([w, h], rgba))
                }
                Ok(None) => None,
                Err(e) => {
                    log::debug!("[vídeo] muestra: {e}");
                    None
                }
            };
            let Some((size, rgba)) = out else { continue };
            let pts = pending.pop().map(|Reverse(p)| p).unwrap_or(smp.pts_ms);
            // Espera sitio en la cola (o un salto, o parar).
            let mut s = mtx.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if s.stop {
                    return Ok(());
                }
                if s.generation != generation {
                    break 'samples;
                }
                // Lo que ya pasó no se guarda.
                if pts + 200 < s.want_ms {
                    break;
                }
                if s.frames.len() < QUEUE {
                    s.frames.push_back(Frame { pts_ms: pts, size, rgba });
                    break;
                }
                s = cv.wait_timeout(s, Duration::from_millis(200)).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
            }
        }
        seg += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifiesto_sin_cifrar_y_resolucion() {
        let v = serde_json::json!({
            "base_urls": ["https://video-fa.scdn.co/segments/"],
            "initialization_template": "v1/x/profiles/{{profile_id}}/inits/{{file_type}}?a=1",
            "segment_template": "v1/x/profiles/{{profile_id}}/{{segment_timestamp}}.{{file_type}}?a=1",
            "contents": [{"segment_length": 4, "end_time_millis": 8148339, "encryption_infos": [], "profiles": [
                {"id": 15, "mime_type": "audio/mp4", "file_type": "mp4"},
                {"id": 11, "mime_type": "video/mp4", "file_type": "mp4", "video_height": 320, "video_width": 568},
                {"id": 10, "mime_type": "video/mp4", "file_type": "mp4", "video_height": 480, "video_width": 854},
                {"id": 9, "mime_type": "video/mp4", "file_type": "mp4", "video_height": 720, "video_width": 1280},
                {"id": 2, "mime_type": "video/mp2t", "file_type": "ts", "video_height": 480}
            ]}]
        });
        let m = parse_manifest(&v).unwrap();
        assert_eq!((m.profile, m.width, m.height, m.seg_len_ms, m.end_ms), (10, 854, 480, 4000, 8148339));
        let mut c = v.clone();
        c["contents"][0]["encryption_infos"] = serde_json::json!([{"key_system": "widevine"}]);
        assert!(parse_manifest(&c).is_err());
    }

    #[test]
    fn nal_a_annex_b() {
        let raw = [0, 0, 0, 2, 0x65, 0x88, 0, 0, 0, 1, 0x41];
        assert_eq!(annex_b(&raw, 4), vec![0, 0, 0, 1, 0x65, 0x88, 0, 0, 0, 1, 0x41]);
    }
}
