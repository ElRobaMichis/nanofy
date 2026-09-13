//! Ventana y bucle de eventos: winit + egui-winit + softbuffer. Todo el dibujo lo hace
//! `raster` en la CPU, así que no se crea ningún contexto OpenGL/Vulkan/DirectX y el
//! proceso no carga el driver gráfico (decenas o cientos de MB de RAM menos).
//!
//! Repintado bajo demanda: solo cuando hay entrada, cuando egui anima algo o cuando un hilo
//! de fondo pide un repintado. Durante animaciones se respeta el límite de FPS configurado.

use std::ffi::c_void;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{ViewportId, ViewportInfo};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Icon, Window, WindowAttributes, WindowId};

use crate::raster::Raster;

/// Clave en la memoria de egui con el límite de FPS pedido por la app.
pub const FPS_CAP_KEY: &str = "nanofy_fps_cap";
/// Clave con el coste del último fotograma en milisegundos (ui + rasterizado + presentación).
pub const FRAME_MS_KEY: &str = "nanofy_frame_ms";
/// [ui, teselado, rasterizado, presentación] del último fotograma, en ms.
pub const FRAME_PHASES_KEY: &str = "nanofy_frame_phases";
/// Fotogramas por segundo para animaciones cuando el usuario no está interactuando.
const IDLE_FPS: u32 = 24;
/// Tiempo tras la última entrada durante el que se aplica el límite alto de fps.
const INTERACTIVE_WINDOW: Duration = Duration::from_millis(400);

pub trait UiApp {
    fn ui(&mut self, ui: &mut egui::Ui);
    fn on_exit(&mut self);
    /// Procesa lógica en segundo plano (ventana minimizada) sin pintar. Por defecto no hace nada.
    fn pump(&mut self, _ctx: &egui::Context) {}
}

pub struct WindowConfig {
    pub title: String,
    pub size: (f32, f32),
    pub min_size: (f32, f32),
    /// RGBA de 32×32.
    pub icon_rgba: Option<Vec<u8>>,
}

#[derive(Debug)]
pub enum UserEvent {
    Repaint { when: Instant },
}

/// Recursos nativos que la app puede necesitar (p. ej. el HWND para las teclas multimedia).
#[derive(Clone, Copy, Default)]
pub struct NativeHandles {
    pub hwnd: Option<*mut c_void>,
}

type MakeApp<A> = Box<dyn FnOnce(&egui::Context, NativeHandles) -> A>;

struct Shell<A: UiApp> {
    ctx: egui::Context,
    config: WindowConfig,
    make_app: Option<MakeApp<A>>,
    app: Option<A>,
    window: Option<Arc<Window>>,
    egui_winit: Option<egui_winit::State>,
    sb_context: Option<softbuffer::Context<Arc<Window>>>,
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    surface_size: (u32, u32),
    raster: Raster,
    info: ViewportInfo,
    start: Instant,
    last_paint: Instant,
    /// Última entrada del usuario: las animaciones sin interacción (spinners, letras) se
    /// limitan a `IDLE_FPS`; el límite alto solo se usa mientras el usuario interactúa.
    last_input: Instant,
    next_repaint: Option<Instant>,
    exited: bool,
    frames: u64,
}

pub fn run<A: UiApp + 'static>(
    config: WindowConfig,
    make_app: impl FnOnce(&egui::Context, NativeHandles) -> A + 'static,
) -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    crate::tmark("bucle de eventos");
    let proxy: EventLoopProxy<UserEvent> = event_loop.create_proxy();
    let ctx = egui::Context::default();
    crate::tmark("contexto egui");
    {
        let proxy = Mutex::new(proxy);
        ctx.set_request_repaint_callback(move |info| {
            let when = Instant::now() + info.delay;
            let _ = proxy.lock().unwrap().send_event(UserEvent::Repaint { when });
        });
    }
    let mut shell = Shell {
        ctx,
        config,
        make_app: Some(Box::new(make_app)),
        app: None,
        window: None,
        egui_winit: None,
        sb_context: None,
        surface: None,
        surface_size: (0, 0),
        raster: Raster::default(),
        info: ViewportInfo::default(),
        start: Instant::now(),
        last_paint: Instant::now(),
        last_input: Instant::now(),
        next_repaint: None,
        exited: false,
        frames: 0,
    };
    event_loop.run_app(&mut shell)?;
    Ok(())
}

impl<A: UiApp> Shell<A> {
    fn create_window(&mut self, el: &ActiveEventLoop) {
        let mut attrs = WindowAttributes::default()
            .with_title(self.config.title.clone())
            .with_inner_size(LogicalSize::new(self.config.size.0, self.config.size.1))
            .with_min_inner_size(LogicalSize::new(
                self.config.min_size.0,
                self.config.min_size.1,
            ));
        if let Some(rgba) = self.config.icon_rgba.clone() {
            if let Ok(icon) = Icon::from_rgba(rgba, 32, 32) {
                attrs = attrs.with_window_icon(Some(icon));
            }
        }
        // La ventana se crea oculta: crearla visible cuesta ~18 ms más (DWM la presenta vacía).
        // Se pinta el primer fotograma y entonces se muestra ya con contenido.
        attrs = attrs.with_visible(false);
        let window = Arc::new(el.create_window(attrs).expect("no se pudo crear la ventana"));
        crate::tmark("ventana win32");

        let sb_context =
            softbuffer::Context::new(window.clone()).expect("no se pudo crear el contexto de dibujo");
        let surface = softbuffer::Surface::new(&sb_context, window.clone())
            .expect("no se pudo crear la superficie de dibujo");

        crate::tmark("softbuffer");
        let state = egui_winit::State::new(
            self.ctx.clone(),
            ViewportId::ROOT,
            &*window,
            Some(window.scale_factor() as f32),
            window.theme(),
            None,
        );
        egui_winit::update_viewport_info(&mut self.info, &self.ctx, &window, true);

        let handles = NativeHandles {
            hwnd: match window.window_handle().map(|h| h.as_raw()) {
                Ok(RawWindowHandle::Win32(h)) => Some(h.hwnd.get() as *mut c_void),
                _ => None,
            },
        };
        if let Some(make) = self.make_app.take() {
            self.app = Some(make(&self.ctx, handles));
        }

        self.window = Some(window.clone());
        self.egui_winit = Some(state);
        self.sb_context = Some(sb_context);
        self.surface = Some(surface);
        self.paint(el);
        window.set_visible(true);
        let since = crate::ms_since_process_creation().unwrap_or(0.0);
        let _ = crate::VISIBLE_MS.set(since);
        crate::tmark(&format!("ventana visible ({since:.0} ms desde la creación del proceso)"));
        window.request_redraw();
    }

    fn exit(&mut self, el: &ActiveEventLoop) {
        if self.exited {
            return;
        }
        self.exited = true;
        let t = Instant::now();
        // La ventana desaparece al instante; la despedida a Spotify sigue en segundo plano.
        if let Some(w) = &self.window {
            w.set_visible(false);
        }
        if let Some(app) = self.app.as_mut() {
            app.on_exit();
        }
        log::info!("[t] on_exit tardó {} ms", t.elapsed().as_millis());
        el.exit();
        // Los destructores (runtime de tokio, hilos de audio, SMTC) pueden tardar segundos o
        // bloquearse; todo lo persistente ya está guardado, así que terminamos aquí.
        std::process::exit(0);
    }

    /// Límite de fps vigente: el configurado si hay interacción reciente, si no `IDLE_FPS`.
    fn effective_fps(&self) -> u32 {
        let cap = self
            .ctx
            .data(|d| d.get_temp::<u32>(egui::Id::new(FPS_CAP_KEY)))
            .unwrap_or(144)
            .clamp(30, 480);
        if self.last_input.elapsed() < INTERACTIVE_WINDOW {
            cap
        } else {
            IDLE_FPS.min(cap)
        }
    }

    fn schedule(&mut self, when: Instant) {
        self.next_repaint = Some(self.next_repaint.map_or(when, |n| n.min(when)));
    }

    fn paint(&mut self, el: &ActiveEventLoop) {
        let (Some(window), Some(state), Some(app), Some(surface)) = (
            self.window.clone(),
            self.egui_winit.as_mut(),
            self.app.as_mut(),
            self.surface.as_mut(),
        ) else {
            return;
        };
        if window.is_minimized() == Some(true) {
            // Minimizada: no se rasteriza, pero SÍ se procesan los eventos del backend (cambios
            // de canción, posición, letra, controles de medios). Si no, al restaurar tras varias
            // canciones la posición quedaba desfasada.
            app.pump(&self.ctx);
            self.schedule(Instant::now() + Duration::from_millis(500));
            return;
        }
        let t0 = Instant::now();

        egui_winit::update_viewport_info(&mut self.info, &self.ctx, &window, false);
        let mut raw_input = state.take_egui_input(&window);
        raw_input.time = Some(self.start.elapsed().as_secs_f64());
        raw_input.viewports = std::iter::once((ViewportId::ROOT, self.info.clone())).collect();

        let mut full = self.ctx.run_ui(raw_input, |ui| app.ui(ui));
        let t_ui = t0.elapsed().as_secs_f32() * 1000.0;

        state.handle_platform_output(&window, full.platform_output);

        let mut actions = Vec::new();
        let mut repaint_delay = Duration::MAX;
        if let Some(vo) = full.viewport_output.get(&ViewportId::ROOT) {
            repaint_delay = vo.repaint_delay;
            egui_winit::process_viewport_commands(
                &self.ctx,
                &mut self.info,
                vo.commands.clone(),
                &window,
                &mut actions,
            );
        }
        if self.info.close_requested() {
            full.textures_delta.clear();
            self.exit(el);
            return;
        }
        self.info.events.clear();

        // Rasterizado
        self.raster.update_textures(&full.textures_delta);
        let ppp = full.pixels_per_point;
        let primitives = self.ctx.tessellate(full.shapes, ppp);
        let t_tess = t0.elapsed().as_secs_f32() * 1000.0;
        let mut t_raster = t_tess;

        let size = window.inner_size();
        if size.width > 0 && size.height > 0 {
            if self.surface_size != (size.width, size.height) {
                if let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                {
                    if surface.resize(w, h).is_ok() {
                        self.surface_size = (size.width, size.height);
                    }
                }
            }
            if self.surface_size == (size.width, size.height) {
                if let Ok(mut buffer) = surface.buffer_mut() {
                    let clear = self.ctx.global_style().visuals.panel_fill;
                    self.raster.paint(
                        &mut buffer,
                        size.width as usize,
                        size.height as usize,
                        ppp,
                        &primitives,
                        clear,
                    );
                    t_raster = t0.elapsed().as_secs_f32() * 1000.0;
                    let _ = buffer.present();
                }
            }
        }
        self.raster.free_textures(&full.textures_delta);
        full.textures_delta.clear();

        let frame_ms = t0.elapsed().as_secs_f32() * 1000.0;
        let repaint_delay_dbg = repaint_delay;
        if self.frames == 0 {
            let since = crate::ms_since_process_creation().unwrap_or(0.0);
            let _ = crate::FIRST_FRAME_MS.set(since);
            log::info!("[t] primer fotograma pintado ({frame_ms:.1} ms de trabajo; {since:.0} ms desde la creación del proceso)");
        }
        self.frames += 1;
        if self.frames % 25 == 0 {
            let causes: Vec<String> = self
                .ctx
                .repaint_causes()
                .iter()
                .map(|c| format!("{}:{}", c.file, c.line))
                .collect();
            log::debug!("[t] fotograma {} ({frame_ms:.1} ms, repaint_delay siguiente {:?}, causas {:?})", self.frames, repaint_delay_dbg, causes);
        }
        self.ctx.data_mut(|d| {
            d.insert_temp(egui::Id::new(FRAME_MS_KEY), frame_ms);
            d.insert_temp(egui::Id::new(FRAME_PHASES_KEY), [t_ui, t_tess - t_ui, t_raster - t_tess, frame_ms - t_raster]);
        });
        self.last_paint = Instant::now();

        // Siguiente repintado, respetando el límite de FPS durante animaciones.
        let min_interval = Duration::from_secs_f64(1.0 / self.effective_fps() as f64);
        if repaint_delay < Duration::from_secs(3600) {
            let when = (self.last_paint + repaint_delay).max(self.last_paint + min_interval);
            self.schedule(when);
        }
    }
}

impl<A: UiApp> ApplicationHandler<UserEvent> for Shell<A> {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_none() {
            self.create_window(el);
        }
    }

    fn new_events(&mut self, _el: &ActiveEventLoop, cause: StartCause) {
        if let StartCause::ResumeTimeReached { .. } = cause {
            self.next_repaint = None;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Repaint { when } => {
                let earliest =
                    self.last_paint + Duration::from_secs_f64(1.0 / self.effective_fps() as f64);
                let when = when.max(earliest);
                if when <= Instant::now() {
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                } else {
                    self.schedule(when);
                }
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(window) = self.window.clone() else {
            return;
        };
        if matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::KeyboardInput { .. }
                | WindowEvent::Touch(_)
                | WindowEvent::Resized(_)
        ) {
            self.last_input = Instant::now();
        }
        match &event {
            WindowEvent::CloseRequested => {
                self.exit(el);
                return;
            }
            WindowEvent::RedrawRequested => {
                self.paint(el);
                return;
            }
            WindowEvent::Resized(_) => {
                window.request_redraw();
            }
            _ => {}
        }
        if let Some(state) = self.egui_winit.as_mut() {
            let resp = state.on_window_event(&window, &event);
            if resp.repaint {
                window.request_redraw();
            }
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        match self.next_repaint {
            Some(when) if when <= Instant::now() => {
                self.next_repaint = None;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
                el.set_control_flow(ControlFlow::Wait);
            }
            Some(when) => el.set_control_flow(ControlFlow::WaitUntil(when)),
            None => el.set_control_flow(ControlFlow::Wait),
        }
    }

    fn exiting(&mut self, _el: &ActiveEventLoop) {
        if !self.exited {
            self.exited = true;
            if let Some(app) = self.app.as_mut() {
                app.on_exit();
            }
        }
    }
}
