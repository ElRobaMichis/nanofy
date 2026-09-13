#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod app;
mod backend;
mod bus;
mod cache;
mod config;
mod fonts;
mod images;
mod media;
#[cfg(windows)]
mod taskbar;
mod model;
mod raster;
mod shell;
mod webauth;

fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,nanofy=info"),
    )
    .format_timestamp_millis()
    .init();

    let t0 = std::time::Instant::now();
    let _ = START.set(t0);
    log::info!("[t] main");
    // Identidad estable en la barra de tareas: sin ella, al anclar el .exe suelto Windows no
    // asocia el botón con el acceso directo anclado y este se queda sin icono.
    #[cfg(windows)]
    set_app_user_model_id();
    let paths = config::Paths::new();
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
        for t in &extra_tabs {
            app.open_tab_from_flag(t);
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
