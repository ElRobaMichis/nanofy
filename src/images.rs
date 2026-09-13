//! Carga de portadas: descarga en hilos aparte, caché en disco, reducción al tamaño en que
//! se va a mostrar y texturas con desalojo LRU para mantener la RAM acotada.
//!
//! Cada textura se guarda una sola vez (en RAM, sin copia en GPU) y al tamaño pedido:
//! una fila de 36 px no necesita una portada de 300 px.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use egui::load::SizedTexture;
use egui::{ColorImage, TextureHandle, TextureOptions};

use crate::bus::{Msg, UiTx};

/// Máximo de texturas residentes. Al superarlo se desalojan las menos usadas.
const MAX_TEXTURES: usize = 160;
/// Presupuesto de píxeles residentes (todas las texturas de portadas): 20 MB.
const MAX_TEXTURE_BYTES: usize = 16 * 1024 * 1024;
/// Texturas sin usar durante más de estos fotogramas se liberan (≈ 20 s a 1 fps en reposo).
const IDLE_FRAMES: u64 = 40;
const WORKERS: usize = 2;

enum Slot {
    Loading,
    Ready { tex: TextureHandle, used: u64, bytes: usize },
    Failed,
}

/// Tamaño pedido: lado máximo (miniaturas) o encaje exacto con recorte centrado (cabeceras
/// grandes, que así se dibujan 1:1 sin remuestrear cada fotograma).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Size {
    Max(u32),
    Fit(u32, u32),
}

pub struct Images {
    tx: mpsc::Sender<(String, Size)>,
    slots: HashMap<String, Slot>,
    frame: u64,
    /// Color dominante por URL (calculado al cargar miniaturas), para el fondo del reproductor.
    colors: HashMap<String, egui::Color32>,
}

impl Images {
    pub fn start(cache_dir: PathBuf, ui: UiTx) -> Self {
        let _ = std::fs::create_dir_all(&cache_dir);
        let (tx, rx) = mpsc::channel::<(String, Size)>();
        let rx = Arc::new(Mutex::new(rx));
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .http_status_as_error(false)
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .provider(ureq::tls::TlsProvider::NativeTls)
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            )
            .build();
        let agent = ureq::Agent::new_with_config(config);
        prune_cache(cache_dir.clone());

        for i in 0..WORKERS {
            let rx = rx.clone();
            let agent = agent.clone();
            let ui = ui.clone();
            let dir = cache_dir.clone();
            std::thread::Builder::new()
                .name(format!("nanofy-img-{i}"))
                .stack_size(512 * 1024)
                .spawn(move || loop {
                    let job = {
                        let guard = rx.lock().unwrap();
                        guard.recv()
                    };
                    match job {
                        Ok((url, size)) => {
                            let image = load(&agent, &dir, &url, size);
                            ui.send(Msg::Image {
                                key: key(&url, size),
                                image,
                            });
                        }
                        Err(_) => break,
                    }
                })
                .expect("no se pudo crear el hilo de imágenes");
        }

        Self {
            tx,
            slots: HashMap::new(),
            frame: 0,
            colors: HashMap::new(),
        }
    }

    /// Devuelve la textura (reducida a `max_side` px) si está lista; si no, la pide una vez.
    pub fn texture(&mut self, url: &str, max_side: u32) -> Option<SizedTexture> {
        self.texture_sized(url, Size::Max(max_side))
    }

    /// Textura recortada y escalada exactamente a `w`×`h` píxeles.
    pub fn texture_fit(&mut self, url: &str, w: u32, h: u32) -> Option<SizedTexture> {
        self.texture_sized(url, Size::Fit(w.max(1), h.max(1)))
    }

    fn texture_sized(&mut self, url: &str, size: Size) -> Option<SizedTexture> {
        let frame = self.frame;
        let k = key(url, size);
        match self.slots.get_mut(&k) {
            Some(Slot::Ready { tex, used, .. }) => {
                *used = frame;
                Some(SizedTexture::from_handle(tex))
            }
            Some(_) => None,
            None => {
                self.slots.insert(k, Slot::Loading);
                let _ = self.tx.send((url.to_string(), size));
                None
            }
        }
    }

    /// Color de fondo derivado de una portada ya cargada: oscuro para el tema oscuro y un
    /// tinte claro (mezclado con blanco) para el tema claro, para que el texto siga legible.
    pub fn color(&self, url: &str, dark: bool) -> Option<egui::Color32> {
        let raw = self.colors.get(url).copied()?;
        let (r, g, b) = (raw.r() as f64, raw.g() as f64, raw.b() as f64);
        let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        Some(if dark {
            let target = 52.0;
            let k = if lum > 1.0 { (target / lum).min(1.4) } else { 1.0 };
            let c = |v: f64| ((v * k).clamp(0.0, 255.0)) as u8;
            egui::Color32::from_rgb(c(r), c(g), c(b))
        } else {
            // Tinte: 78 % blanco + 22 % color, y nunca más oscuro que ~215 de luminancia.
            let mix = |v: f64| 255.0 * 0.78 + v * 0.22;
            let (mr, mg, mb) = (mix(r), mix(g), mix(b));
            let l2 = 0.2126 * mr + 0.7152 * mg + 0.0722 * mb;
            let k = if l2 < 215.0 { 215.0 / l2 } else { 1.0 };
            let c = |v: f64| ((v * k).clamp(0.0, 255.0)) as u8;
            egui::Color32::from_rgb(c(mr), c(mg), c(mb))
        })
    }

    pub fn loaded(&mut self, ctx: &egui::Context, key: &str, image: Option<ColorImage>) {
        let slot = match image {
            Some(img) => {
                let bytes = img.pixels.len() * 4;
                if img.size[0] <= 200 && img.size[1] <= 200 {
                    if let Some(url) = key.split_once('|').map(|(_, u)| u.to_string()) {
                        self.colors.entry(url).or_insert_with(|| dominant_color(&img));
                    }
                }
                Slot::Ready {
                    tex: ctx.load_texture(key, img, TextureOptions::LINEAR),
                    used: self.frame,
                    bytes,
                }
            }
            None => Slot::Failed,
        };
        self.slots.insert(key.to_string(), slot);
    }

    /// Llamar una vez por fotograma: desaloja texturas antiguas si hay demasiadas.
    pub fn end_frame(&mut self) {
        self.frame += 1;
        // Cada 30 fotogramas: libera las texturas que llevan tiempo sin verse (otras páginas).
        if self.frame % 30 == 0 {
            let cur = self.frame;
            self.slots.retain(|_, s| !matches!(s, Slot::Ready { used, .. } if cur.saturating_sub(*used) > IDLE_FRAMES));
        }
        let ready = self.resident();
        let bytes = self.resident_bytes();
        if ready <= MAX_TEXTURES && bytes <= MAX_TEXTURE_BYTES {
            return;
        }
        // Nunca se expulsa lo usado en el último fotograma: si se ve, se queda (evita que
        // las portadas parpadeen al recargarse cada fotograma cuando hay muchas visibles).
        let current = self.frame - 1;
        let mut by_age: Vec<(u64, String)> = self
            .slots
            .iter()
            .filter_map(|(k, s)| match s {
                Slot::Ready { used, .. } if *used < current => Some((*used, k.clone())),
                _ => None,
            })
            .collect();
        by_age.sort_unstable();
        // Se expulsa por antigüedad hasta bajar al 75 % de ambos límites.
        let target_n = MAX_TEXTURES * 3 / 4;
        let target_b = MAX_TEXTURE_BYTES * 3 / 4;
        let (mut n, mut b) = (ready, bytes);
        for (_, key) in by_age {
            if n <= target_n && b <= target_b {
                break;
            }
            if let Some(Slot::Ready { bytes: kb, .. }) = self.slots.remove(&key) {
                n -= 1;
                b = b.saturating_sub(kb);
            }
        }
    }

    pub fn resident(&self) -> usize {
        self.slots
            .values()
            .filter(|s| matches!(s, Slot::Ready { .. }))
            .count()
    }

    /// Bytes de píxeles residentes (aproximación de la RAM usada por portadas).
    pub fn resident_bytes(&self) -> usize {
        self.slots
            .values()
            .map(|s| match s {
                Slot::Ready { bytes, .. } => *bytes,
                _ => 0,
            })
            .sum()
    }

    pub fn clear(&mut self) {
        self.slots.clear();
    }
}

/// Mantiene la caché de portadas en disco por debajo de 100 MB (borra las más antiguas).
fn prune_cache(dir: PathBuf) {
    const MAX_BYTES: u64 = 100 * 1024 * 1024;
    std::thread::Builder::new()
        .name("nanofy-img-prune".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                return;
            };
            let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let md = e.metadata().ok()?;
                    if !md.is_file() {
                        return None;
                    }
                    Some((md.modified().ok()?, md.len(), e.path()))
                })
                .collect();
            let mut total: u64 = files.iter().map(|f| f.1).sum();
            if total <= MAX_BYTES {
                return;
            }
            files.sort_by_key(|f| f.0);
            for (_, len, path) in files {
                if total <= MAX_BYTES * 8 / 10 {
                    break;
                }
                if std::fs::remove_file(&path).is_ok() {
                    total = total.saturating_sub(len);
                }
            }
        })
        .ok();
}

fn key(url: &str, size: Size) -> String {
    match size {
        Size::Max(s) => format!("{s}|{url}"),
        Size::Fit(w, h) => format!("{w}x{h}|{url}"),
    }
}

fn cache_name(url: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn load(agent: &ureq::Agent, dir: &PathBuf, url: &str, size: Size) -> Option<ColorImage> {
    let file = dir.join(cache_name(url));
    let bytes = match std::fs::read(&file) {
        Ok(b) if !b.is_empty() => b,
        _ => {
            let mut resp = agent.get(url).call().ok()?;
            if resp.status().as_u16() != 200 {
                return None;
            }
            let b = resp.body_mut().read_to_vec().ok()?;
            let _ = std::fs::write(&file, &b);
            b
        }
    };
    let img = image::load_from_memory(&bytes).ok()?;
    let img = match size {
        Size::Max(max_side) => {
            let max_side = max_side.max(16);
            if img.width() > max_side || img.height() > max_side {
                img.thumbnail(max_side, max_side)
            } else {
                img
            }
        }
        Size::Fit(w, h) => {
            // Recorte centrado a la proporción pedida y escalado exacto.
            let (sw, sh) = (img.width() as f64, img.height() as f64);
            let target = w as f64 / h as f64;
            let (cw, ch) = if sw / sh > target { (sh * target, sh) } else { (sw, sw / target) };
            let (cx, cy) = (((sw - cw) / 2.0) as u32, ((sh - ch) / 2.0) as u32);
            img.crop_imm(cx, cy, cw.max(1.0) as u32, ch.max(1.0) as u32)
                .resize_exact(w, h, image::imageops::FilterType::Triangle)
        }
    };
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some(ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        rgba.as_raw(),
    ))
}

/// Media de los píxeles ponderada por saturación (los colores vivos mandan).
fn dominant_color(img: &ColorImage) -> egui::Color32 {
    let (mut r, mut g, mut b, mut wsum) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    let step = (img.pixels.len() / 4096).max(1);
    for px in img.pixels.iter().step_by(step) {
        let (pr, pg, pb) = (px.r() as f64, px.g() as f64, px.b() as f64);
        let max = pr.max(pg).max(pb);
        let min = pr.min(pg).min(pb);
        let sat = if max > 0.0 { (max - min) / max } else { 0.0 };
        let light = max / 255.0;
        let w = sat * sat * light + 0.02;
        r += pr * w;
        g += pg * w;
        b += pb * w;
        wsum += w;
    }
    if wsum <= 0.0 {
        return egui::Color32::from_rgb(128, 128, 128);
    }
    // Se guarda el color medio sin ajustar; `color()` lo adapta al tema.
    egui::Color32::from_rgb((r / wsum) as u8, (g / wsum) as u8, (b / wsum) as u8)
}
