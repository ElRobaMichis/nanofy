#![recursion_limit = "512"]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod app;
mod backend;
mod bus;
mod cache;
mod config;
mod control;
mod fonts;
mod images;
mod lyrics_sources;
mod media;
mod mixprobe;
#[cfg(windows)]
mod taskbar;
mod model;
mod pathfinder;
mod raster;
mod shell;
mod update;
mod webauth;

// Las pruebas del remuestreador de la salida de audio (en el librespot-playback parcheado) se
// compilan también aquí: `cargo test` del binario no ejecuta las de las dependencias.
#[cfg(test)]
#[allow(dead_code)]
#[path = "../vendor/librespot-playback/src/audio_backend/resample.rs"]
mod resample_tests;
// Igual con la ganancia de la normalización (factores de Spotify y rampa de los cambios en vivo).
#[cfg(test)]
#[allow(dead_code)]
#[path = "../vendor/librespot-playback/src/gain.rs"]
mod gain_tests;
// Y con las primitivas del fundido entre canciones (rampas, mezcla de la saliente, limitador).
#[cfg(test)]
#[allow(dead_code)]
#[path = "../vendor/librespot-playback/src/crossfade.rs"]
mod crossfade_tests;
// Y con la instrumentación del tiempo hasta el primer sonido y los fallos simulados
// (`NANOFY_FAULT`), en el librespot-core parcheado.
#[cfg(test)]
#[allow(dead_code)]
#[path = "../vendor/librespot-core/src/ttfs.rs"]
mod ttfs_tests;
#[cfg(test)]
#[allow(dead_code)]
#[path = "../vendor/librespot-core/src/fault.rs"]
mod fault_tests;
// Y con los reintentos, la caché y el freno de las claves de audio, y el cortacircuitos de
// saltos automáticos de Spirc.
#[cfg(test)]
#[allow(dead_code)]
#[path = "../vendor/librespot-core/src/key_policy.rs"]
mod key_policy_tests;
#[cfg(test)]
#[allow(dead_code)]
#[path = "../vendor/librespot-connect/src/cascade.rs"]
mod cascade_tests;
// Y con la pausa instantánea (cola de la salida, rampa tras vaciarla, rebobinado al reanudar),
// los plazos de spclient y la cola de los avisos de estado de Spirc.
#[cfg(test)]
#[allow(dead_code)]
#[path = "../vendor/librespot-playback/src/audio_backend/out_queue.rs"]
mod out_queue_tests;
#[cfg(test)]
#[allow(dead_code)]
#[path = "../vendor/librespot-core/src/request_policy.rs"]
mod request_policy_tests;
#[cfg(test)]
#[allow(dead_code)]
#[path = "../vendor/librespot-connect/src/state_queue.rs"]
mod state_queue_tests;
// Y con la caché de metadatos (semillas incluidas), la de storage-resolve con el orden de los
// servidores de la CDN, y la canción con la que empieza una carga (fila pulsada, más páginas).
#[cfg(test)]
#[allow(dead_code)]
#[path = "../vendor/librespot-core/src/meta_cache.rs"]
mod meta_cache_tests;
#[cfg(test)]
#[allow(dead_code)]
#[path = "../vendor/librespot-core/src/cdn_policy.rs"]
mod cdn_policy_tests;
#[cfg(test)]
#[allow(dead_code)]
#[path = "../vendor/librespot-connect/src/start_index.rs"]
mod start_index_tests;

/// Copia del registro en `<state_dir>/nanofy.log`, además de la salida de error habitual. La
/// versión publicada corre sin consola: sin esto no queda rastro de los fallos que solo aparecen
/// tras horas de uso. Al arrancar, el registro anterior pasa a `nanofy.log.1`, de modo que cerrar
/// y abrir la aplicación —lo primero que uno hace cuando algo falla— no borre lo que hay que ver.
struct LogSink {
    file: Option<std::fs::File>,
    escrito: u64,
}

impl LogSink {
    /// Tope por sesión; a partir de ahí el registro sigue solo en la salida de error.
    const MAX: u64 = 4 << 20;

    fn new(paths: &config::Paths) -> Self {
        let file = (|| {
            std::fs::create_dir_all(&paths.state_dir).ok()?;
            let ruta = paths.state_dir.join("nanofy.log");
            let _ = std::fs::rename(&ruta, paths.state_dir.join("nanofy.log.1"));
            std::fs::File::create(&ruta).ok()
        })();
        Self { file, escrito: 0 }
    }
}

impl std::io::Write for LogSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stderr().write_all(buf);
        if let Some(f) = &mut self.file {
            if self.escrito < Self::MAX {
                self.escrito += buf.len() as u64;
                let _ = f.write_all(buf);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(f) = &mut self.file {
            let _ = f.flush();
        }
        std::io::stderr().flush()
    }
}

fn main() {
    // `--self-test`: la actualización arranca así la versión nueva antes de instalarla, para
    // saber que Windows o el antivirus la dejan abrir. Va lo primero: sin registro (no rota
    // nanofy.log), sin leer ajustes y sin ventana.
    if std::env::args_os().nth(1).is_some_and(|a| a == "--self-test") {
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = writeln!(out, "nanofy {}", env!("CARGO_PKG_VERSION"));
        let _ = out.flush();
        std::process::exit(0);
    }
    let paths = config::Paths::new();
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,nanofy=info"),
    )
    .format_timestamp_millis()
    .target(env_logger::Target::Pipe(Box::new(LogSink::new(&paths))))
    .init();

    let t0 = std::time::Instant::now();
    let _ = START.set(t0);
    log::info!("[t] main {}", env!("CARGO_PKG_VERSION"));
    // Antes de la primera sesión: es un OnceLock, y el primer `get` (al abrir un fichero de
    // audio) fijaría los valores por defecto para siempre. No va en `Backend::start`, que se
    // repite en cada reconexión.
    if librespot_audio::AudioFetchParams::set(audio_fetch_params()).is_err() {
        log::warn!("parámetros de descarga del audio ya fijados; se usan los de librespot");
    }
    // Actualizaciones, antes que nada más (ajustes, ventana): esperar a la ventana que se cerró
    // para actualizar, volver a la versión anterior si esta no llega a arrancar e instalar la que
    // quedó preparada. Si con eso ya se abrió otra versión, esta termina aquí.
    let update_notice = match update::on_launch() {
        update::Launch::Exit => std::process::exit(0),
        update::Launch::Continue { notice } => notice,
    };
    // Identidad estable en la barra de tareas: sin ella, al anclar el .exe suelto Windows no
    // asocia el botón con el acceso directo anclado y este se queda sin icono.
    #[cfg(windows)]
    set_app_user_model_id();
    let settings = config::Settings::load(&paths);
    tmark("ajustes cargados");
    // `--diag`: prueba automática de letras, dispositivos, cola y reproducción (10 s), y sale.
    let diag = std::env::args().any(|a| a == "--diag");
    // `--page settings|liked|search`, `--side queue|lyrics`, `--jam`: estado inicial de la ventana.
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| -> Option<String> {
        args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
    };
    let start_page = flag("--page");
    let start_side = flag("--side");
    let start_jam = args.iter().any(|a| a == "--jam");
    // `--update-failed <texto>`: la versión nueva no arrancaba y se volvió a esta.
    let update_failed = update_notice.or_else(|| flag("--update-failed"));
    // `--updated-from <versión>`: la abrió la anterior al instalarse esta; `--resume-playing`:
    // sonaba música al pulsar «Reiniciar» y debe seguir sonando.
    let updated_from = flag("--updated-from");
    let resume_playing = args.iter().any(|a| a == "--resume-playing");
    // `--control <puerto>`: modo de control local para las pruebas automatizadas (carpeta qa/).
    let control_port = flag("--control").and_then(|p| p.parse::<u16>().ok());
    // `--tab <página>` (repetible): abre pestañas adicionales en segundo plano.
    let extra_tabs: Vec<String> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| *a == "--tab")
        .filter_map(|(i, _)| args.get(i + 1).cloned())
        .collect();

    let config = shell::WindowConfig {
        title: "Nanofy".to_string(),
        size: (1120.0, 720.0),
        min_size: (760.0, 500.0),
        icon_rgba: Some(icon_rgba()),
    };
    tmark("icono generado");

    if let Err(e) = shell::run(config, move |ctx, handles| {
        log::info!("[t] ventana creada a los {} ms", t0.elapsed().as_millis());
        let mut app = app::App::new(ctx, handles, paths, settings);
        log::info!("[t] app creada a los {} ms", t0.elapsed().as_millis());
        app.diag = diag;
        app.apply_start_flags(start_page.as_deref(), start_side.as_deref(), start_jam);
        app.apply_update_flags(updated_from.clone(), resume_playing);
        if let Some(msg) = &update_failed {
            app.show_update_failed(msg);
        }
        for t in &extra_tabs {
            app.open_tab_from_flag(t);
        }
        if let Some(port) = control_port {
            app.control_start(port);
        }
        app
    }) {
        log::error!("error fatal: {e}");
        std::process::exit(1);
    }
}

/// Fija el AppUserModelID del proceso para que el anclaje a la barra de tareas conserve el icono.
#[cfg(windows)]
fn set_app_user_model_id() {
    use windows::core::w;
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
    unsafe {
        if let Err(e) = SetCurrentProcessExplicitAppUserModelID(w!("Nanofy.Desktop")) {
            log::warn!("no se pudo fijar el AppUserModelID: {e}");
        }
    }
}

/// Resonancia: aros verde salvia. RGBA integrado para evitar decodificación al iniciar.
fn icon_rgba() -> Vec<u8> {
    const ICON: &[u8; 32 * 32 * 4] = include_bytes!("../assets/nanofy-32.rgba");
    ICON.to_vec()
}

/// Milisegundos desde que Windows creó el proceso (incluye cargar el ejecutable y sus DLL).
#[cfg(windows)]
pub fn ms_since_process_creation() -> Option<f64> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe {
        GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u).ok()?;
        let now = GetSystemTimeAsFileTime();
        let to_u64 = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
        Some((to_u64(now).saturating_sub(to_u64(c))) as f64 / 10_000.0)
    }
}
#[cfg(not(windows))]
pub fn ms_since_process_creation() -> Option<f64> {
    None
}

/// Marcas de arranque medidas desde la creación del proceso (para `--control` y el bench).
pub static FIRST_FRAME_MS: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
pub static VISIBLE_MS: std::sync::OnceLock<f64> = std::sync::OnceLock::new();

/// Parámetros de descarga del audio: los de librespot salvo dos.
/// - `read_ahead_before_playback`, que librespot define pero no usa y el reproductor parcheado
///   usa como lo que espera tras una búsqueda antes de seguir (0,5 s de audio; por delante se
///   siguen pidiendo 5 s).
/// - El plazo de descarga baja de 8 a 5 s. Es lo que espera una lectura sin datos antes de darse
///   por cortada. Con 8 s no se podía bajar mientras un corte a media canción acababa en un salto
///   a la siguiente; ahora la canción queda en pausa en su segundo y sigue al volver la red, así
///   que antes se avisa y antes se reintenta (y menos tiempo pasa el reproductor bloqueado).
fn audio_fetch_params() -> librespot_audio::AudioFetchParams {
    librespot_audio::AudioFetchParams {
        read_ahead_before_playback: std::time::Duration::from_millis(500),
        download_timeout: std::time::Duration::from_secs(5),
        ..Default::default()
    }
}

/// Instante de arranque para las marcas de tiempo `[t]`.
pub static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Marca de tiempo relativa al arranque (solo se imprime con `nanofy=info`).
pub fn since_start_ms() -> u64 {
    START.get().map(|s| s.elapsed().as_millis() as u64).unwrap_or(0)
}

pub fn tmark(label: &str) {
    if log::log_enabled!(log::Level::Info) {
        if let Some(s) = START.get() {
            log::info!("[t] {label}: {:.1} ms", s.elapsed().as_secs_f64() * 1000.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    #[test]
    fn parametros_de_descarga_solo_cambian_la_espera_tras_buscar_y_el_plazo() {
        let p = super::audio_fetch_params();
        let d = librespot_audio::AudioFetchParams::default();
        assert_eq!(p.read_ahead_before_playback, Duration::from_millis(500));
        // Menos de lo que se pide por delante: si no, esperar «antes» no ahorraría nada.
        assert!(p.read_ahead_before_playback < p.read_ahead_during_playback);
        // Un corte a media canción ya no salta de canción: el plazo baja de los 8 s de librespot
        // a 5 s, y sigue por encima de lo que se espera tras buscar.
        assert_eq!(d.download_timeout, Duration::from_secs(8));
        assert_eq!(p.download_timeout, Duration::from_secs(5));
        assert!(p.download_timeout > p.read_ahead_before_playback);
        // El resto, como en librespot: el tamaño mínimo de cada petición sigue en 64 KB (el
        // decodificador lo necesita potencia de 2 y > 32 KB).
        assert_eq!(p.minimum_download_size, d.minimum_download_size);
        assert_eq!(p.minimum_throughput, d.minimum_throughput);
        assert_eq!(p.initial_ping_time_estimate, d.initial_ping_time_estimate);
        assert_eq!(p.maximum_assumed_ping_time, d.maximum_assumed_ping_time);
        assert_eq!(p.read_ahead_during_playback, d.read_ahead_during_playback);
        assert_eq!(p.prefetch_threshold_factor, d.prefetch_threshold_factor);
    }
}
