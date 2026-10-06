//! Carga de portadas: descarga en hilos aparte, caché en disco, reducción al tamaño en que
//! se va a mostrar y texturas con desalojo LRU para mantener la RAM acotada.
//!
//! Cada textura se guarda una sola vez (en RAM, sin copia en GPU) y al tamaño pedido:
//! una fila de 36 px no necesita una portada de 300 px.
//!
//! Los hilos sirven primero lo que está a la vista y no descargan lo que ya salió de ella.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use egui::load::SizedTexture;
use egui::{ColorImage, TextureHandle, TextureOptions};

use crate::bus::{Msg, UiTx};

/// Máximo de texturas residentes. Al superarlo se desalojan las menos usadas.
const MAX_TEXTURES: usize = 160;
/// Presupuesto de píxeles residentes (todas las texturas de portadas): 20 MB.
const MAX_TEXTURE_BYTES: usize = 16 * 1024 * 1024;
/// Texturas sin verse durante este tiempo se liberan (otras páginas). Por tiempo y no por
/// fotogramas: a 144 fps, 40 fotogramas eran 0,3 s y volver atrás en una lista dejaba
/// cuadros grises mientras se recargaban.
const IDLE: Duration = Duration::from_secs(15);
/// Cada cuánto se revisan las texturas sin uso y los fallos que toca reintentar.
const SWEEP_EVERY: Duration = Duration::from_secs(1);
/// Un fallo pasajero (red, plazo agotado, 5xx) se vuelve a pedir pasado este tiempo si la
/// portada sigue a la vista: antes, una portada que fallaba una vez quedaba en blanco toda la
/// sesión.
const RETRY_FAILED: Duration = Duration::from_secs(30);
/// Un encargo que lleva estos fotogramas sin pintarse salió de la vista y ya no se descarga.
/// En fotogramas y no en tiempo: sin repintados (app en reposo) nada caduca, y lo que se ve
/// se vuelve a pedir en cada fotograma.
const STALE_FRAMES: u64 = 5;
/// A la vista y esperando más que esto, un encargo pasa delante de los recién llegados.
const AGED: Duration = Duration::from_millis(300);
/// Las descargas van a la CDN de portadas (i.scdn.co) con un agente propio: sin cuota de la
/// Web API ni el limitador de librespot, y esperan a la red mucho más que a la CPU.
const WORKERS: usize = 4;

/// Último fotograma en que se pidió una portada que aún no ha llegado. La comparten el hueco
/// (`Slot::Loading`), que la refresca cada vez que se pinta, y el encargo en cola, que así
/// sabe si sigue a la vista.
pub type Wanted = Arc<AtomicU64>;

enum Slot {
    Loading { wanted: Wanted },
    /// `used` (fotograma) decide qué se expulsa al pasar los topes sin tocar lo pintado en el
    /// último fotograma; `seen` (hora) decide qué lleva tiempo sin verse.
    Ready { tex: TextureHandle, used: u64, seen: Instant, bytes: usize },
    /// `retry`: el fallo fue pasajero y se olvida pasado `RETRY_FAILED`.
    Failed { at: Instant, retry: bool },
}

/// Tamaño pedido: lado máximo (miniaturas) o encaje exacto con recorte centrado (cabeceras
/// grandes, que así se dibujan 1:1 sin remuestrear cada fotograma).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Size {
    Max(u32),
    Fit(u32, u32),
}

struct Job {
    key: String,
    url: String,
    size: Size,
    wanted: Wanted,
    /// Orden de llegada (creciente).
    pushed: u64,
    at: Instant,
}

/// Encargos de portadas por servir. No es un canal porque los hilos no toman el más antiguo
/// sino el que más interesa (lo que se ve ahora) y descartan lo que ya no se ve: con la cola
/// FIFO, tras desplazarse rápido por una lista larga las portadas donde se paraba esperaban
/// detrás de cientos de filas ya pasadas.
#[derive(Default)]
struct Pending {
    jobs: Vec<Job>,
    /// La app se cierra: los hilos terminan (como antes al soltar el canal).
    closed: bool,
}

impl Pending {
    /// Saca el encargo que más interesa. Lo que está a la vista (pintado en este fotograma o
    /// en el anterior) va antes que nada y, de eso, lo más nuevo, que es donde se paró la
    /// lista; pero lo que lleva `AGED` esperando a la vista pasa delante, del más antiguo al
    /// más nuevo, para que la portada del reproductor o una cabecera no se queden sin turno
    /// mientras la lista no para de moverse. Lo que ya no se ve va al final, lo visto más
    /// recientemente primero (es lo que reaparece al volver atrás). Recorre toda la cola, pero
    /// son unos pocos miles de encargos como mucho y una comparación por encargo.
    fn take(&mut self, cur: u64) -> Option<Job> {
        let now = Instant::now();
        let rank = |j: &Job| {
            let wanted = j.wanted.load(Ordering::Relaxed);
            if wanted + 1 >= cur {
                if now.saturating_duration_since(j.at) >= AGED {
                    (3, u64::MAX - j.pushed, 0)
                } else {
                    (2, j.pushed, 0)
                }
            } else {
                (1, wanted, j.pushed)
            }
        };
        let best = self.jobs.iter().enumerate().max_by_key(|(_, j)| rank(j))?.0;
        Some(self.jobs.swap_remove(best))
    }
}

type Queue = Arc<(Mutex<Pending>, Condvar)>;

/// Resultado de un encargo.
enum Loaded {
    Image(ColorImage),
    Failed { retry: bool },
    /// Sin copia en disco y fuera de la vista: no se descargó.
    Dropped,
}

pub struct Images {
    queue: Queue,
    /// `frame`, compartido con los hilos para saber qué encargos dejaron de verse.
    shared_frame: Arc<AtomicU64>,
    /// Contador de encargos, para ordenarlos por llegada.
    pushed: u64,
    slots: HashMap<String, Slot>,
    frame: u64,
    last_sweep: Instant,
    /// Para medir lo que tarda en llenarse lo que se ve: hora de la última portada pedida
    /// mientras alguna a la vista seguía sin llegar.
    waiting: Option<Instant>,
    /// Portadas a la vista sin textura en este fotograma.
    waiting_now: usize,
    /// Color dominante por URL (calculado al cargar miniaturas), para el fondo del reproductor.
    colors: HashMap<String, egui::Color32>,
}

impl Drop for Images {
    fn drop(&mut self) {
        let (lock, cv) = &*self.queue;
        if let Ok(mut q) = lock.lock() {
            q.closed = true;
        }
        cv.notify_all();
    }
}

impl Images {
    pub fn start(cache_dir: PathBuf, ui: UiTx) -> Self {
        let _ = std::fs::create_dir_all(&cache_dir);
        let queue: Queue = Arc::new((Mutex::new(Pending::default()), Condvar::new()));
        let shared_frame = Arc::new(AtomicU64::new(0));
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
            let queue = queue.clone();
            let frame = shared_frame.clone();
            let agent = agent.clone();
            let ui = ui.clone();
            let dir = cache_dir.clone();
            std::thread::Builder::new()
                .name(format!("nanofy-img-{i}"))
                .stack_size(512 * 1024)
                .spawn(move || loop {
                    let job = {
                        let (lock, cv) = &*queue;
                        let mut q = lock.lock().unwrap();
                        loop {
                            if q.closed {
                                return;
                            }
                            if let Some(job) = q.take(frame.load(Ordering::Relaxed)) {
                                break job;
                            }
                            q = cv.wait(q).unwrap();
                        }
                    };
                    // Se mira justo antes de ir a la red y no al sacarlo: si la fila volvió a
                    // la vista mientras tanto, se descarga.
                    let stale = || job.wanted.load(Ordering::Relaxed) + STALE_FRAMES < frame.load(Ordering::Relaxed);
                    match load(&agent, &dir, &job.url, job.size, stale) {
                        Loaded::Image(img) => ui.send(Msg::Image {
                            key: job.key,
                            image: Some(img),
                            retry: false,
                        }),
                        Loaded::Failed { retry } => ui.send(Msg::Image {
                            key: job.key,
                            image: None,
                            retry,
                        }),
                        // Nunca como `image: None`: eso deja el hueco en Failed y la portada
                        // no se volvería a pedir al verla otra vez.
                        Loaded::Dropped => ui.send(Msg::ImageDropped {
                            key: job.key,
                            wanted: job.wanted,
                        }),
                    }
                })
                .expect("no se pudo crear el hilo de imágenes");
        }

        Self {
            queue,
            shared_frame,
            pushed: 0,
            slots: HashMap::new(),
            frame: 0,
            last_sweep: Instant::now(),
            waiting: None,
            waiting_now: 0,
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
            Some(Slot::Ready { tex, used, seen, .. }) => {
                *used = frame;
                *seen = Instant::now();
                Some(SizedTexture::from_handle(tex))
            }
            Some(Slot::Loading { wanted }) => {
                // Sigue a la vista: su encargo no caduca y va delante de lo que ya no se ve.
                wanted.store(frame, Ordering::Relaxed);
                self.waiting_now += 1;
                self.waiting.get_or_insert_with(Instant::now);
                None
            }
            Some(Slot::Failed { .. }) => None,
            None => {
                let wanted: Wanted = Arc::new(AtomicU64::new(frame));
                self.slots.insert(k.clone(), Slot::Loading { wanted: wanted.clone() });
                self.pushed += 1;
                let (lock, cv) = &*self.queue;
                lock.lock().unwrap().jobs.push(Job {
                    key: k,
                    url: url.to_string(),
                    size,
                    wanted,
                    pushed: self.pushed,
                    at: Instant::now(),
                });
                cv.notify_one();
                self.waiting_now += 1;
                self.waiting = Some(Instant::now());
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

    /// `retry` solo cuenta sin imagen: el fallo fue pasajero y se reintentará.
    pub fn loaded(&mut self, ctx: &egui::Context, key: &str, image: Option<ColorImage>, retry: bool) {
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
                    seen: Instant::now(),
                    bytes,
                }
            }
            None => {
                // Un fallo tardío (de un encargo anterior a `clear()`) no tapa una textura buena.
                if matches!(self.slots.get(key), Some(Slot::Ready { .. })) {
                    return;
                }
                Slot::Failed { at: Instant::now(), retry }
            }
        };
        self.slots.insert(key.to_string(), slot);
    }

    /// Encargo descartado sin descargar porque salió de la vista: se quita su hueco para que
    /// la portada se pida de nuevo si vuelve a verse. Solo si es el mismo encargo: tras
    /// `clear()` la clave puede tener ya otro encargo o una textura, que no se tocan.
    pub fn dropped(&mut self, key: &str, wanted: &Wanted) {
        if matches!(self.slots.get(key), Some(Slot::Loading { wanted: w }) if Arc::ptr_eq(w, wanted)) {
            self.slots.remove(key);
        }
    }

    /// Llamar una vez por fotograma: desaloja texturas antiguas si hay demasiadas.
    pub fn end_frame(&mut self) {
        self.frame += 1;
        self.shared_frame.store(self.frame, Ordering::Relaxed);
        if self.waiting_now == 0 {
            if let Some(t0) = self.waiting.take() {
                // Solo las esperas que se notan: las copias de disco llegan antes.
                let ms = t0.elapsed().as_millis();
                if ms >= 100 {
                    log::info!("[img] visibles listas en {ms} ms");
                }
            }
        }
        self.waiting_now = 0;
        // Una vez por segundo: libera las texturas que llevan tiempo sin verse (otras páginas)
        // y olvida los fallos pasajeros viejos, para que se pidan otra vez si se ven.
        if self.last_sweep.elapsed() >= SWEEP_EVERY {
            let now = Instant::now();
            self.last_sweep = now;
            self.slots.retain(|_, s| match s {
                Slot::Ready { seen, .. } => now.saturating_duration_since(*seen) <= IDLE,
                Slot::Failed { at, retry: true } => now.saturating_duration_since(*at) < RETRY_FAILED,
                _ => true,
            });
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
        // Los encargos en cola se quedan sin hueco: lo que se vea se pedirá de nuevo. Los que
        // ya están en curso terminan y su respuesta se trata como cualquier otra tardía.
        self.queue.0.lock().unwrap().jobs.clear();
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

/// Escribe una portada en la caché a través de un fichero temporal: un cierre a medias o dos
/// hilos con la misma URL (tamaños distintos) nunca dejan un fichero truncado con el nombre
/// definitivo.
fn write_cache(file: &Path, bytes: &[u8]) {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = file.with_extension(format!("{}-{n}.tmp", std::process::id()));
    if std::fs::write(&tmp, bytes).is_err() || std::fs::rename(&tmp, file).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// `stale` dice si el encargo ya salió de la vista; se mira antes de ir a la red (y, en las
/// cabeceras, antes de leer el disco).
fn load(agent: &ureq::Agent, dir: &Path, url: &str, size: Size, stale: impl Fn() -> bool) -> Loaded {
    // Salvo las cabeceras (`Fit`): llevan el tamaño exacto en la clave, así que al redimensionar
    // la ventana se pide una por fotograma y las anteriores no vuelven a pedirse. Decodificar y
    // escalar cada una a hasta 1280 px cuesta decenas de ms y una textura de varios MB que solo
    // iría a desalojarse.
    if matches!(size, Size::Fit(..)) && stale() {
        return Loaded::Dropped;
    }
    let file = dir.join(cache_name(url));
    // Lo que está en disco se decodifica siempre, aunque ya no se vea: es barato y así está
    // lista si se vuelve atrás.
    let cached = match std::fs::read(&file) {
        Ok(b) if !b.is_empty() => match image::load_from_memory(&b) {
            Ok(img) => Some(img),
            Err(_) => {
                // Copia dañada (p. ej. de un cierre a medias): se borra y se descarga otra vez
                // en vez de fallar con ella en cada sesión.
                let _ = std::fs::remove_file(&file);
                None
            }
        },
        _ => None,
    };
    let img = match cached {
        Some(img) => img,
        None => {
            if stale() {
                return Loaded::Dropped;
            }
            let mut resp = match agent.get(url).call() {
                Ok(r) => r,
                // Una URL mal formada no se arregla reintentando; lo demás es red o plazo.
                Err(ureq::Error::BadUri(_) | ureq::Error::Http(_)) => return Loaded::Failed { retry: false },
                Err(_) => return Loaded::Failed { retry: true },
            };
            let status = resp.status().as_u16();
            if status != 200 {
                // 429 y 5xx de la CDN son pasajeros; un 404 o un 403 no cambian en la sesión.
                return Loaded::Failed { retry: status == 429 || status >= 500 };
            }
            let Ok(b) = resp.body_mut().read_to_vec() else {
                return Loaded::Failed { retry: true };
            };
            let Ok(img) = image::load_from_memory(&b) else {
                return Loaded::Failed { retry: false };
            };
            // Solo después de decodificarla: lo que no es una imagen no se guarda.
            write_cache(&file, &b);
            img
        }
    };
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
    Loaded::Image(ColorImage::from_rgba_unmultiplied(
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
