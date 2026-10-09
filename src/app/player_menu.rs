//! Menú «Más» del reproductor (los tres puntos), copiado de la referencia de diseño 13.webp
//! (cliente = referencia + (7, −1)): un panel de cristal de 287 px, con lo de detrás desenfocado
//! como en «Personalizar inicio», una fila cada 53,33 px con su icono (recortado de la
//! referencia, `menu_masks.rs`) y su texto, y una flecha en las que abren un submenú. Los
//! submenús salen a su izquierda con el mismo cristal.

use egui::{pos2, vec2, Color32, CornerRadius, Rect, Sense};

use super::library::{paint_mask, px};
use super::library_masks::LibMask;
use super::menu_masks as mm;
use super::theme;
use super::widgets::{galley_truncated, text_on_baseline};
use super::{Action, App, Page, SideTab};

/// Ancho del panel y de una fila.
const W: f32 = 287.0;
const ROW: f32 = 53.33;
/// Línea base de la primera fila desde arriba, y del final de la última línea base al borde.
const FIRST_BASE: f32 = 43.27;
const LAST_PAD: f32 = 32.9;
/// El borde derecho, a 40,5 px a la derecha del centro de los tres puntos; el de abajo, a 97,1
/// px del borde de abajo de la ventana (7 px por encima del reproductor).
const RIGHT_OF_DOTS: f32 = 40.5;
const FROM_BOTTOM: f32 = 97.1;
/// Texto: tinta desde 70 px; icono centrado en 38 y 6,7 px por encima de la línea base.
const TEXT_X: f32 = 70.0;
const FONT: f32 = 16.6;
const RADIUS: u8 = 5;
/// Submenús: ancho y margen del texto.
const SUB_W: f32 = 300.0;
const SUB_TEXT_X: f32 = 24.0;

/// Una fila del menú.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum What {
    Download,
    Timer,
    Hide,
    Share,
    Library,
    Queue,
    Radio,
    Album,
    Artist,
    Miniplayer,
    Fullscreen,
    Jam,
}

const ROWS: [What; 12] = [
    What::Download,
    What::Timer,
    What::Hide,
    What::Share,
    What::Library,
    What::Queue,
    What::Radio,
    What::Album,
    What::Artist,
    What::Miniplayer,
    What::Fullscreen,
    What::Jam,
];

impl What {
    fn mask(self) -> (&'static str, &'static LibMask) {
        match self {
            What::Download => ("menu-download", &mm::DOWNLOAD),
            What::Timer => ("menu-timer", &mm::TIMER),
            What::Hide => ("menu-hide", &mm::HIDE),
            What::Share => ("menu-share", &mm::SHARE),
            What::Library => ("menu-library", &mm::LIBRARY_ADD),
            What::Queue => ("menu-queue", &mm::QUEUE_ADD),
            What::Radio => ("menu-radio", &mm::RADIO),
            What::Album => ("menu-album", &mm::ALBUM),
            What::Artist => ("menu-artist", &mm::ARTIST),
            What::Miniplayer => ("menu-miniplayer", &mm::MINIPLAYER),
            What::Fullscreen => ("menu-fullscreen", &mm::FULLSCREEN),
            What::Jam => ("menu-jam", &mm::JAM),
        }
    }

    /// Las que abren un submenú (el temporizador también, aunque en la referencia no lleva
    /// flecha).
    fn submenu(self) -> bool {
        matches!(self, What::Timer | What::Library | What::Queue | What::Artist)
    }

    fn chevron(self) -> bool {
        matches!(self, What::Library | What::Queue | What::Artist)
    }
}

/// Lo que se elige en un submenú.
#[derive(Clone, Debug)]
enum Pick {
    Timer(u64),
    TimerEnd,
    TimerCancel,
    Like(bool),
    AddToPlaylist,
    QueueEnd,
    OpenQueue,
    Artist(String),
}

/// Colores: los de la referencia en oscuro; en claro, los de la paleta.
struct Ink {
    dark: bool,
    text: Color32,
    icon: Color32,
    off: Color32,
    glass: Color32,
    hover: Color32,
}

fn ink(ctx: &egui::Context) -> Ink {
    let p = theme::palette(ctx);
    if p.dark {
        Ink {
            dark: true,
            text: Color32::from_gray(229),
            icon: Color32::from_gray(143),
            off: Color32::from_gray(110),
            glass: Color32::from_rgba_unmultiplied(34, 34, 34, 191),
            hover: Color32::from_white_alpha(10),
        }
    } else {
        Ink {
            dark: false,
            text: p.text,
            icon: p.weak,
            off: p.faint,
            glass: Color32::from_rgba_unmultiplied(250, 250, 250, 225),
            hover: p.hover,
        }
    }
}

/// Desenfoque de lo de detrás: sigma ~56 px, medida en la referencia por cómo se difumina bajo
/// el panel el borde de una tarjeta (el doble que «Personalizar inicio»): lo de detrás queda en
/// manchas de color suaves, sin formas.
const BLUR_SIGMA: f32 = 56.0;

/// Cristal: lo de detrás desenfocado y el velo encima.
fn glass(painter: &egui::Painter, rect: Rect, ink: &Ink) {
    if ink.dark {
        painter.add(egui::Shape::Callback(egui::epaint::PaintCallback {
            rect,
            callback: std::sync::Arc::new(crate::raster::BackdropBlur { sigma: BLUR_SIGMA, corner: RADIUS as f32 }),
        }));
    }
    painter.rect_filled(rect, CornerRadius::same(RADIUS), ink.glass);
}

impl App {
    /// El panel, abierto con el botón de los tres puntos (`button`). Se cierra al elegir algo, al
    /// pulsar fuera o con Escape.
    pub(super) fn player_more_panel(&mut self, ctx: &egui::Context, button: Rect) {
        let ink = ink(ctx);
        let screen = ctx.content_rect();
        let track = self.player.now.as_ref().map(super::track_from_now);
        let n = ROWS.len();
        let natural = FIRST_BASE + ROW * (n as f32 - 1.0) + LAST_PAD;
        let bottom = screen.max.y - FROM_BOTTOM;
        let top = (bottom - natural).max(screen.min.y + 6.0);
        let right = button.center().x + RIGHT_OF_DOTS;
        let rect = Rect::from_min_max(pos2(right - W, top), pos2(right, bottom));
        // Sin sitio para todo: la fila base se aprieta un poco (ventanas muy bajas).
        let pitch = if rect.height() < natural { (rect.height() - FIRST_BASE - LAST_PAD) / (n as f32 - 1.0) } else { ROW };

        let id = track.as_ref().and_then(|t| t.id.clone());
        let is_episode = track.as_ref().is_some_and(|t| t.kind.as_deref() == Some("episode"));
        let downloaded = id.as_ref().is_some_and(|i| self.downloaded.contains(i));
        let hidden = id.as_ref().is_some_and(|i| self.hidden_tracks.contains(i));
        let album_id = track.as_ref().and_then(|t| t.album.as_ref().and_then(|a| a.id.clone()));
        let artists: Vec<(String, String)> = track
            .as_ref()
            .map(|t| t.artists.iter().filter_map(|a| a.id.clone().map(|id| (a.name.clone(), id))).collect())
            .unwrap_or_default();
        let label = |w: What| -> (String, bool) {
            match w {
                What::Download => (
                    if downloaded { "Descargada" } else if is_episode { "Descargar episodio" } else { "Descargar canción" }.to_string(),
                    id.is_some() && !downloaded,
                ),
                What::Timer => (
                    match (self.sleep_at, self.sleep_end_of_track) {
                        (Some(at), _) => format!("Temporizador ({} min)", (at.saturating_duration_since(std::time::Instant::now()).as_secs() + 59) / 60),
                        (None, true) => "Temporizador (al terminar)".to_string(),
                        _ => "Temporizador de apagado".to_string(),
                    },
                    true,
                ),
                What::Hide => ((if hidden { "Mostrar canción" } else { "Ocultar canción" }).to_string(), id.is_some()),
                What::Share => ("Compartir canción".to_string(), track.as_ref().is_some_and(|t| !t.uri.is_empty())),
                What::Library => ("Agregar a la biblioteca".to_string(), track.is_some()),
                What::Queue => ("Agregar a la cola".to_string(), track.is_some()),
                What::Radio => ("Ir a la radio de la canción".to_string(), id.is_some() && !is_episode),
                What::Album => ("Ver álbum".to_string(), album_id.is_some()),
                What::Artist => ("Ver artista".to_string(), !artists.is_empty()),
                What::Miniplayer => ((if self.miniplayer { "Salir del miniplayer" } else { "Miniplayer" }).to_string(), true),
                What::Fullscreen => ((if self.fullscreen { "Salir de pantalla completa" } else { "Pantalla completa" }).to_string(), true),
                What::Jam => ((if self.jam.is_some() { "Jam" } else { "Iniciar una Jam" }).to_string(), true),
            }
        };
        let rows: Vec<(What, String, bool)> = ROWS.iter().map(|&w| {
            let (l, on) = label(w);
            (w, l, on)
        }).collect();

        let mut chosen: Option<What> = None;
        let mut picked: Option<Pick> = None;
        let mut sub_rect: Option<Rect> = None;
        egui::Area::new(egui::Id::new("player_more_panel"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .show(ctx, |ui| {
                // Todo el panel recoge el puntero: lo de debajo no reacciona.
                ui.allocate_rect(rect, Sense::click());
                let painter = ui.painter().clone();
                glass(&painter, rect, &ink);
                let anchor_x = px(&painter, pos2(rect.min.x, 0.0)).0;
                for (k, (what, text, on)) in rows.iter().enumerate() {
                    let base = rect.min.y + FIRST_BASE + pitch * k as f32;
                    let c = base - 6.7;
                    let row = Rect::from_min_max(pos2(rect.min.x + 6.0, c - pitch / 2.0 + 1.0), pos2(rect.max.x - 6.0, c + pitch / 2.0 - 1.0));
                    let r = ui.interact(row, egui::Id::new(("player_more_row", k)), if *on { Sense::click() } else { Sense::hover() });
                    let sub_open = self.player_more_sub == Some(k);
                    if *on && (r.hovered() || sub_open) {
                        painter.rect_filled(row, CornerRadius::same(4), ink.hover);
                        if r.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                    }
                    // Al pasar por una fila con submenú se abre; por otra, se cierra.
                    if r.hovered() {
                        self.player_more_sub = (what.submenu() && *on).then_some(k);
                    }
                    let (name, m) = what.mask();
                    let a = (anchor_x, px(&painter, pos2(0.0, base)).1);
                    paint_mask(&painter, name, m, a, if *on { ink.icon } else { ink.off });
                    let max_w = if what.chevron() { 168.0 } else { W - TEXT_X - 18.0 };
                    let color = if *on { ink.text } else { ink.off };
                    let g = galley_truncated(&painter, text, theme::semilight(FONT), color, max_w);
                    let lead = g.rows.first().and_then(|r| r.row.glyphs.first()).map(|gl| gl.pos.x + gl.uv_rect.offset.x).unwrap_or(0.0);
                    text_on_baseline(&painter, pos2(rect.min.x + TEXT_X - lead, base), g, color);
                    if what.chevron() {
                        paint_mask(&painter, "menu-chevron", &mm::CHEVRON, a, if *on { ink.icon } else { ink.off });
                    }
                    if r.clicked() {
                        if what.submenu() {
                            self.player_more_sub = Some(k);
                        } else {
                            chosen = Some(*what);
                        }
                    }
                }
            });

        // Submenú a la izquierda, con su primera fila a la altura de la que lo abrió.
        if let Some(k) = self.player_more_sub {
            let what = ROWS[k];
            let items: Vec<(Pick, String)> = match what {
                What::Timer => {
                    let mut v: Vec<(Pick, String)> = [15u64, 30, 45, 60].iter().map(|&m| (Pick::Timer(m), format!("{m} minutos"))).collect();
                    v.push((Pick::TimerEnd, "Al terminar la canción".into()));
                    if self.sleep_at.is_some() || self.sleep_end_of_track {
                        v.push((Pick::TimerCancel, "Cancelar temporizador".into()));
                    }
                    v
                }
                What::Library => {
                    let liked = id.as_ref().is_some_and(|i| self.liked_set.contains(i));
                    let mut v = Vec::new();
                    if id.is_some() {
                        v.push((Pick::Like(!liked), if liked { "Quitar de Canciones que te gustan" } else { "Canciones que te gustan" }.to_string()));
                    }
                    v.push((Pick::AddToPlaylist, "Añadir a una playlist…".into()));
                    v
                }
                What::Queue => vec![(Pick::QueueEnd, "Al final de la cola".into()), (Pick::OpenQueue, "Abrir la cola".into())],
                What::Artist => artists.iter().map(|(name, aid)| (Pick::Artist(aid.clone()), name.clone())).collect(),
                _ => Vec::new(),
            };
            if !items.is_empty() {
                let base0 = rect.min.y + FIRST_BASE + pitch * k as f32;
                let h = FIRST_BASE + ROW * (items.len() as f32 - 1.0) + LAST_PAD;
                let top = (base0 - FIRST_BASE).min(screen.max.y - FROM_BOTTOM - h).max(screen.min.y + 6.0);
                let srect = Rect::from_min_size(pos2(rect.min.x - 6.0 - SUB_W, top), vec2(SUB_W, h));
                sub_rect = Some(srect);
                egui::Area::new(egui::Id::new("player_more_sub"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(srect.min)
                    .show(ctx, |ui| {
                        ui.allocate_rect(srect, Sense::click());
                        let painter = ui.painter().clone();
                        glass(&painter, srect, &ink);
                        for (j, (pick, text)) in items.iter().enumerate() {
                            let base = srect.min.y + FIRST_BASE + ROW * j as f32;
                            let c = base - 6.7;
                            let row = Rect::from_min_max(pos2(srect.min.x + 6.0, c - ROW / 2.0 + 1.0), pos2(srect.max.x - 6.0, c + ROW / 2.0 - 1.0));
                            let r = ui.interact(row, egui::Id::new(("player_more_sub_row", j)), Sense::click());
                            if r.hovered() {
                                painter.rect_filled(row, CornerRadius::same(4), ink.hover);
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            let g = galley_truncated(&painter, text, theme::semilight(FONT), ink.text, SUB_W - SUB_TEXT_X - 18.0);
                            text_on_baseline(&painter, pos2(srect.min.x + SUB_TEXT_X, base), g, ink.text);
                            if r.clicked() {
                                picked = Some(pick.clone());
                            }
                        }
                    });
            }
        }

        // Cerrar: al elegir, con Escape o al pulsar fuera (el botón ya lo hace al volver a pulsarlo).
        let pressed_outside = ctx.input(|i| {
            i.pointer.any_pressed()
                && i.pointer.interact_pos().is_some_and(|p| !rect.contains(p) && !button.contains(p) && !sub_rect.is_some_and(|s| s.contains(p)))
        });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) || pressed_outside {
            self.close_player_more();
        }
        if let Some(w) = chosen {
            self.close_player_more();
            let t = track.clone();
            match w {
                What::Download => {
                    if let Some(id) = id.clone() {
                        self.actions.push(if is_episode { Action::DownloadEpisodes(vec![id]) } else { Action::Download(vec![id]) });
                    }
                }
                What::Hide => {
                    if let Some(id) = &id {
                        self.toggle_hidden(id);
                    }
                }
                What::Share => {
                    if let Some(t) = &t {
                        self.actions.push(Action::CopyText(super::widgets::uri_to_link(&t.uri), "Enlace"));
                    }
                }
                What::Radio => {
                    if let Some(id) = id.clone() {
                        self.actions.push(Action::OpenRadio(id));
                    }
                }
                What::Album => {
                    if let Some(aid) = album_id.clone() {
                        self.actions.push(Action::Go(Page::Album(aid)));
                    }
                }
                What::Miniplayer => self.toggle_miniplayer(ctx),
                What::Fullscreen => self.toggle_fullscreen(ctx),
                What::Jam => self.jam_open = true,
                What::Timer | What::Library | What::Queue | What::Artist => {}
            }
        }
        if let Some(p) = picked {
            self.close_player_more();
            match p {
                Pick::Timer(m) => {
                    self.sleep_at = Some(std::time::Instant::now() + std::time::Duration::from_secs(m * 60));
                    self.set_sleep_end_of_track(false);
                    self.status(format!("Se pausará en {m} minutos"));
                }
                Pick::TimerEnd => {
                    self.set_sleep_end_of_track(true);
                    self.sleep_at = None;
                    self.status("Se pausará al terminar la canción");
                }
                Pick::TimerCancel => {
                    self.sleep_at = None;
                    self.set_sleep_end_of_track(false);
                }
                Pick::Like(on) => {
                    if let Some(id) = id.clone() {
                        self.actions.push(Action::Like(id, on));
                    }
                }
                Pick::AddToPlaylist => {
                    if let Some(t) = &track {
                        self.open_add_dialog(vec![t.uri.clone()]);
                    }
                }
                Pick::QueueEnd => {
                    if let Some(t) = &track {
                        self.actions.push(Action::AddToQueue(t.uri.clone()));
                    }
                }
                Pick::OpenQueue => {
                    if self.side != Some(SideTab::Queue) {
                        self.toggle_side(SideTab::Queue);
                    }
                }
                Pick::Artist(aid) => self.actions.push(Action::Go(Page::Artist(aid))),
            }
        }
    }

    pub(super) fn close_player_more(&mut self) {
        self.player_more_open = false;
        self.player_more_sub = None;
    }
}
