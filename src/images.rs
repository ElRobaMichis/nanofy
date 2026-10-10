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

/// Tamaño pedido: lado máximo (miniaturas), encaje exacto con recorte centrado (cabeceras
/// grandes, que así se dibujan 1:1 sin remuestrear cada fotograma) o cabecera de artista
/// compuesta a partir de su retrato (`hero_compose`).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Size {
    Max(u32),
    Fit(u32, u32),
    Hero(u32, u32),
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
    /// Color dominante por URL de portada, para el degradado de la página de esa portada.
    colors: HashMap<String, egui::Color32>,
    /// Color medio por URL de portada (la «pila» de las tarjetas de álbum).
    means: HashMap<String, egui::Color32>,
    /// Color medio de la franja de abajo de cada portada (el número de canciones de los mixes de
    /// Spotify en la biblioteca se tiñe con él).
    bottoms: HashMap<String, egui::Color32>,
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
            means: HashMap::new(),
            bottoms: HashMap::new(),
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

    /// Cabecera de artista de `w`×`h` píxeles compuesta a partir de su retrato (`hero_compose`).
    pub fn texture_hero(&mut self, url: &str, w: u32, h: u32) -> Option<SizedTexture> {
        self.texture_sized(url, Size::Hero(w.max(1), h.max(1)))
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

    /// `retry` solo cuenta sin imagen: el fallo fue pasajero y se reintentará.
    /// Color de arriba del degradado de una página (playlist, álbum) según su portada ya
    /// cargada: el tono dominante con la saturación moderada y oscurecido hasta una luminancia
    /// de ~40 (en la referencia de diseño, una portada azul da (30, 39, 87)).
    /// Color de la portada para el fondo del reproductor, adaptado al tema (ver `bar_color`).
    pub fn color(&self, url: &str, dark: bool) -> Option<egui::Color32> {
        self.colors.get(url).copied().map(|raw| bar_color(raw, dark))
    }

    /// Color de la «pila» de la tarjeta de esta portada (ver `stack_color`), si ya llegó.
    pub fn stack(&self, url: &str) -> Option<egui::Color32> {
        self.means.get(url).copied().map(stack_color)
    }

    /// Color de la franja de abajo de la portada, si ya llegó (ver `bottom_color`).
    pub fn bottom(&self, url: &str) -> Option<egui::Color32> {
        self.bottoms.get(url).copied()
    }

    pub fn tint(&self, url: &str) -> Option<egui::Color32> {
        let c = self.colors.get(url).copied()?;
        let (r, g, b) = (c.r() as f32, c.g() as f32, c.b() as f32);
        let lum = |r: f32, g: f32, b: f32| 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let l = lum(r, g, b);
        // Saturación como mucho ~0,65: un color puro saldría chillón en un fondo tan grande.
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let sat = if max > 0.0 { (max - min) / max } else { 0.0 };
        let k = if sat > 0.65 { 0.65 / sat } else { 1.0 };
        let (r, g, b) = (l + (r - l) * k, l + (g - l) * k, l + (b - l) * k);
        let target = 40.6;
        let l2 = lum(r, g, b).max(1.0);
        let f = (target / l2).min(255.0 / r.max(g).max(b).max(1.0));
        Some(egui::Color32::from_rgb((r * f).clamp(0.0, 255.0) as u8, (g * f).clamp(0.0, 255.0) as u8, (b * f).clamp(0.0, 255.0) as u8))
    }

    pub fn loaded(&mut self, ctx: &egui::Context, key: &str, image: Option<ColorImage>, retry: bool) {
        let slot = match image {
            Some(img) => {
                let bytes = img.pixels.len() * 4;
                if let Some(url) = key.split_once('|').map(|(_, u)| u.to_string()) {
                    self.means.entry(url.clone()).or_insert_with(|| mean_color(&img));
                    self.bottoms.entry(url.clone()).or_insert_with(|| bottom_color(&img));
                    self.colors.entry(url).or_insert_with(|| dominant_color(&img));
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
        Size::Hero(w, h) => format!("hero{w}x{h}|{url}"),
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
    if matches!(size, Size::Fit(..) | Size::Hero(..)) && stale() {
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
        Size::Fit(w, h) => fit_crop(&img, w, h),
        Size::Hero(w, h) => hero_compose(&img, w, h),
    };
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Loaded::Image(ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        rgba.as_raw(),
    ))
}

/// Recorte centrado a la proporción de `w`×`h` y escalado exacto.
fn fit_crop(img: &image::DynamicImage, w: u32, h: u32) -> image::DynamicImage {
    let (sw, sh) = (img.width() as f64, img.height() as f64);
    let target = w as f64 / h as f64;
    let (cw, ch) = if sw / sh > target { (sh * target, sh) } else { (sw, sw / target) };
    let (cx, cy) = (((sw - cw) / 2.0) as u32, ((sh - ch) / 2.0) as u32);
    img.crop_imm(cx, cy, cw.max(1.0) as u32, ch.max(1.0) as u32)
        .resize_exact(w, h, image::imageops::FilterType::Triangle)
}

/// Cabecera de artista de `w`×`h` a partir de un retrato, como la composición de la referencia:
/// la foto entera ajustada al alto y centrada; a los lados, sus bordes prolongados (el color medio
/// de cada fila en la franja del borde, suavizado en vertical) con un fundido sobre la foto para
/// que no se note la costura. Si la foto ya es más ancha que la cabecera, un recorte centrado.
fn hero_compose(img: &image::DynamicImage, w: u32, h: u32) -> image::DynamicImage {
    let scaled_w = ((img.width() as f64 * h as f64 / img.height().max(1) as f64).round() as u32).max(1);
    if scaled_w >= w {
        return fit_crop(img, w, h);
    }
    let photo = img.resize_exact(scaled_w, h, image::imageops::FilterType::Triangle).to_rgba8();
    let band = (scaled_w / 25).max(2);
    let edge = |from: u32| -> Vec<[f32; 3]> {
        let rows: Vec<[f32; 3]> = (0..h)
            .map(|y| {
                let mut acc = [0.0f32; 3];
                for x in from..(from + band).min(scaled_w) {
                    let p = photo.get_pixel(x, y);
                    for c in 0..3 {
                        acc[c] += p[c] as f32;
                    }
                }
                acc.map(|v| v / band as f32)
            })
            .collect();
        // Media móvil vertical: sin ella, cada fila del borde haría una raya.
        let r = (h / 24).max(1) as i64;
        (0..h as i64)
            .map(|y| {
                let (a, b) = ((y - r).max(0), (y + r).min(h as i64 - 1));
                let mut acc = [0.0f32; 3];
                for k in a..=b {
                    for c in 0..3 {
                        acc[c] += rows[k as usize][c];
                    }
                }
                acc.map(|v| v / (b - a + 1) as f32)
            })
            .collect()
    };
    let left = edge(0);
    let right = edge(scaled_w.saturating_sub(band));
    let x0 = (w - scaled_w) / 2;
    let feather = (scaled_w as f32 * 0.12).max(8.0);
    let mut out = image::RgbaImage::new(w, h);
    for y in 0..h {
        let (l, r) = (left[y as usize], right[y as usize]);
        for x in 0..w {
            let px = if x < x0 {
                l
            } else if x >= x0 + scaled_w {
                r
            } else {
                let dx = x - x0;
                let p = photo.get_pixel(dx, y);
                let p = [p[0] as f32, p[1] as f32, p[2] as f32];
                // Fundido de la foto con el color de su borde cerca de cada lado.
                let dl = dx as f32 / feather;
                let dr = (scaled_w - 1 - dx) as f32 / feather;
                let (t, side) = if dl < dr { (dl, l) } else { (dr, r) };
                let t = t.clamp(0.0, 1.0);
                let t = t * t * (3.0 - 2.0 * t);
                [side[0] + (p[0] - side[0]) * t, side[1] + (p[1] - side[1]) * t, side[2] + (p[2] - side[2]) * t]
            };
            out.put_pixel(x, y, image::Rgba([px[0].round() as u8, px[1].round() as u8, px[2].round() as u8, 255]));
        }
    }
    image::DynamicImage::ImageRgba8(out)
}

/// Color medio de la imagen (una muestra de ~4096 píxeles).
fn mean_color(img: &ColorImage) -> egui::Color32 {
    let step = (img.pixels.len() / 4096).max(1);
    let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
    for px in img.pixels.iter().step_by(step) {
        r += px.r() as u64;
        g += px.g() as u64;
        b += px.b() as u64;
        n += 1;
    }
    let n = n.max(1);
    egui::Color32::from_rgb((r / n) as u8, (g / n) as u8, (b / n) as u8)
}

/// Color medio del 3 % de abajo de la imagen: la franja de color de los mixes de Spotify.
fn bottom_color(img: &ColorImage) -> egui::Color32 {
    let [w, h] = img.size;
    let rows = (h * 3 / 100).max(1);
    let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
    for y in h.saturating_sub(rows)..h {
        for px in &img.pixels[y * w..(y + 1) * w] {
            r += px.r() as u64;
            g += px.g() as u64;
            b += px.b() as u64;
            n += 1;
        }
    }
    let n = n.max(1);
    egui::Color32::from_rgb((r / n) as u8, (g / n) as u8, (b / n) as u8)
}

/// Color de la «pila» de una tarjeta cuya portada tiene el color medio `mean`: el mismo tono con
/// la luminancia 68·(1 − e^(−L/60)), la curva medida en la referencia (portada blanca → gris 66;
/// oscuras → 20-35).
pub fn stack_color(mean: egui::Color32) -> egui::Color32 {
    let (r, g, b) = (mean.r() as f32, mean.g() as f32, mean.b() as f32);
    let l = (0.2126 * r + 0.7152 * g + 0.0722 * b).max(1.0);
    let target = 68.0 * (1.0 - (-l / 60.0).exp());
    let k = target / l;
    let c = |v: f32| (v * k).round().clamp(0.0, 255.0) as u8;
    egui::Color32::from_rgb(c(r), c(g), c(b))
}

/// Media de los píxeles ponderada por saturación (los colores vivos mandan); como mucho 4096
/// muestras por imagen.
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
    egui::Color32::from_rgb((r / wsum) as u8, (g / wsum) as u8, (b / wsum) as u8)
}

/// Fondo del reproductor a partir del color dominante de la portada. En oscuro: saturación como
/// mucho 0,55, luminancia llevada a ~52 y ningún canal por encima de 100. La luminancia apenas
/// cuenta el rojo, así que un rojo o un rosa intensos se quedaban con el canal rojo en 140-170:
/// el fondo salía chillón y los iconos grises casi no se veían. En claro, un tinte suave sobre
/// blanco (78 % blanco, nunca más oscuro que ~215 de luminancia).
pub fn bar_color(raw: egui::Color32, dark: bool) -> egui::Color32 {
    let (r, g, b) = (raw.r() as f64, raw.g() as f64, raw.b() as f64);
    let lum = |r: f64, g: f64, b: f64| 0.2126 * r + 0.7152 * g + 0.0722 * b;
    if dark {
        // Hacia el gris de su misma luminancia, lo justo para que la saturación (máx − mín) / máx
        // quede en 0,55: con mezcla k, vale k·(máx − mín) / (l + k·(máx − l)).
        let l = lum(r, g, b);
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let sat = if max > 0.0 { (max - min) / max } else { 0.0 };
        const SAT: f64 = 0.55;
        let den = (max - min) - SAT * (max - l);
        let k = if sat > SAT && den > 0.0 { (SAT * l / den).clamp(0.0, 1.0) } else { 1.0 };
        let (r, g, b) = (l + (r - l) * k, l + (g - l) * k, l + (b - l) * k);
        let l2 = lum(r, g, b);
        let f = if l2 > 1.0 { (52.0 / l2).min(1.4) } else { 1.0 };
        let f = f.min(100.0 / r.max(g).max(b).max(1.0));
        let c = |v: f64| (v * f).round().clamp(0.0, 255.0) as u8;
        egui::Color32::from_rgb(c(r), c(g), c(b))
    } else {
        let mix = |v: f64| 255.0 * 0.78 + v * 0.22;
        let (mr, mg, mb) = (mix(r), mix(g), mix(b));
        let l2 = lum(mr, mg, mb);
        let k = if l2 < 215.0 { 215.0 / l2 } else { 1.0 };
        let c = |v: f64| ((v * k).clamp(0.0, 255.0)) as u8;
        egui::Color32::from_rgb(c(mr), c(mg), c(mb))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Color32;

    fn sat(c: Color32) -> f64 {
        let (max, min) = (c.r().max(c.g()).max(c.b()) as f64, c.r().min(c.g()).min(c.b()) as f64);
        if max > 0.0 { (max - min) / max } else { 0.0 }
    }

    /// Un rosa intenso ya no sale chillón: ningún canal pasa de 100 y la saturación queda en
    /// ~0,55; un azul, un verde o un gris siguen como antes (en su tono y oscuros).
    #[test]
    fn fondo_del_reproductor_sin_colores_chillones() {
        let pink = bar_color(Color32::from_rgb(230, 40, 110), true);
        assert!(pink.r() <= 100 && pink.r() > pink.b() && pink.b() > pink.g(), "{pink:?}");
        assert!(sat(pink) <= 0.6, "{pink:?}");
        let red = bar_color(Color32::from_rgb(255, 0, 0), true);
        assert!(red.r() <= 100, "{red:?}");
        let blue = bar_color(Color32::from_rgb(30, 39, 87), true);
        assert!(blue.b() > blue.g() && blue.g() > blue.r() && blue.b() <= 100, "{blue:?}");
        let gray = bar_color(Color32::from_gray(80), true);
        assert!(gray.r() == gray.g() && gray.g() == gray.b() && (45..=60).contains(&gray.r()), "{gray:?}");
        // En claro, como siempre: un tinte claro.
        let light = bar_color(Color32::from_rgb(230, 40, 110), false);
        assert!(light.r() > 200 && light.g() > 180, "{light:?}");
    }
}
