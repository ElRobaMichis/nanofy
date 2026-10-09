//! Menú «Más» del reproductor (los tres puntos), copiado de la referencia de diseño 13.webp
//! (cliente = referencia + (7, −1)): un panel de cristal de 287 px, con lo de detrás desenfocado
//! como en «Personalizar inicio», una fila cada 53,33 px con su icono (recortado de la
//! referencia, `menu_masks.rs`) y su texto, y una flecha en las que abren un submenú. Los
//! submenús salen a su izquierda con el mismo cristal.
//!
//! El mismo panel es el menú de una canción de una lista (los tres puntos de la fila o el clic
//! derecho): las filas de la canción, sin las del reproductor, junto al botón o al puntero.

use egui::{pos2, vec2, Color32, CornerRadius, Pos2, Rect, Sense};

use super::icons::{self, Icon};
use super::library::{paint_mask, px};
use super::library_masks::LibMask;
use super::menu_masks as mm;
use super::theme;
use super::widgets::{galley_truncated, text_on_baseline};
use super::{Action, App, Page, SideTab};
use crate::model::Track;

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
/// Separación del panel a los bordes de la ventana, al botón que lo abre y a su submenú.
const MARGIN: f32 = 6.0;
const GAP: f32 = 4.0;

/// Una fila del menú.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum What {
    Download,
    Timer,
    Hide,
    Share,
    Library,
    Queue,
    Remove,
    Radio,
    Album,
    Artist,
    Miniplayer,
    Fullscreen,
    Jam,
}

/// Las del reproductor.
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

/// Las de una canción de una lista: las mismas sin miniplayer, pantalla completa ni Jam (son
/// del reproductor), y «Quitar de esta playlist» tras la cola en las que se pueden editar.
fn song_rows(remove: bool) -> Vec<What> {
    let mut v = vec![What::Download, What::Timer, What::Hide, What::Share, What::Library, What::Queue];
    if remove {
        v.push(What::Remove);
    }
    v.extend([What::Radio, What::Album, What::Artist]);
    v
}

impl What {
    /// Icono recortado de la referencia; «Quitar» no sale en ella y lleva la papelera dibujada.
    fn mask(self) -> Option<(&'static str, &'static LibMask)> {
        Some(match self {
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
            What::Remove => return None,
        })
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

/// Dónde se abre el menú de una canción.
#[derive(Clone, Copy, Debug)]
pub enum Anchor {
    /// Los tres puntos de la fila (su rectángulo).
    Dots(Rect),
    /// El clic derecho (el puntero).
    Pointer(Pos2),
}

/// Menú abierto de una canción de una lista.
pub struct SongMenu {
    pub track: Track,
    /// El álbum de la página: las pistas de un álbum no traen el suyo.
    pub album_id: Option<String>,
    /// La playlist propia (o "queue") de la que se puede quitar.
    pub remove_from: Option<String>,
    /// La fila que lo abrió (lista y posición): sigue resaltada mientras está abierto.
    pub row: (String, usize),
    pub anchor: Anchor,
    /// La fila cuyo submenú se ve.
    pub sub: Option<usize>,
}

/// Lo que hace falta de la canción para rotular las filas y hacer lo elegido.
struct Song {
    track: Option<Track>,
    id: Option<String>,
    is_episode: bool,
    album_id: Option<String>,
    artists: Vec<(String, String)>,
    remove_from: Option<String>,
}

impl Song {
    fn new(track: Option<Track>, album_ctx: Option<String>, remove_from: Option<String>) -> Self {
        let id = track.as_ref().and_then(|t| t.id.clone());
        let is_episode = track.as_ref().is_some_and(|t| t.kind.as_deref() == Some("episode"));
        let album_id = track.as_ref().and_then(|t| t.album.as_ref().and_then(|a| a.id.clone())).or(album_ctx);
        let artists = track
            .as_ref()
            .map(|t| t.artists.iter().filter_map(|a| a.id.clone().map(|id| (a.name.clone(), id))).collect())
            .unwrap_or_default();
        Song { track, id, is_episode, album_id, artists, remove_from }
    }
}

/// Lo elegido en un fotograma, y si el menú se cierra.
#[derive(Default)]
struct Outcome {
    chosen: Option<What>,
    picked: Option<Pick>,
    close: bool,
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

/// Alto de un panel de `n` filas.
fn natural_height(n: usize) -> f32 {
    FIRST_BASE + ROW * (n as f32 - 1.0) + LAST_PAD
}

/// Arriba del panel: debajo de `below` si cabe; si no, con el borde de abajo en `above`; y si
/// tampoco, lo más abajo posible dentro de la ventana.
fn place_y(screen: Rect, below: f32, above: f32, h: f32) -> f32 {
    if below + h <= screen.max.y - MARGIN {
        below
    } else if above - h >= screen.min.y + MARGIN {
        above - h
    } else {
        (screen.max.y - MARGIN - h).max(screen.min.y + MARGIN)
    }
}

/// El panel de una canción: junto a los tres puntos (el borde derecho a 40,5 px de su centro,
/// como en el reproductor; debajo del botón o encima si no cabe) o desde el puntero (hacia la
/// derecha y abajo, o al revés si no cabe).
fn place(screen: Rect, anchor: Anchor, h: f32) -> Rect {
    let h = h.min(screen.height() - 2.0 * MARGIN);
    let (left, top) = match anchor {
        Anchor::Dots(b) => {
            let right = (b.center().x + RIGHT_OF_DOTS).min(screen.max.x - MARGIN);
            (right - W, place_y(screen, b.max.y + GAP, b.min.y - GAP, h))
        }
        Anchor::Pointer(p) => {
            let left = if p.x + W <= screen.max.x - MARGIN { p.x } else { p.x - W };
            (left, place_y(screen, p.y, p.y, h))
        }
    };
    Rect::from_min_size(pos2(left.max(screen.min.x + MARGIN), top), vec2(W, h))
}

impl App {
    /// Texto de una fila y si se puede elegir.
    fn more_label(&self, w: What, s: &Song) -> (String, bool) {
        let downloaded = s.id.as_ref().is_some_and(|i| self.downloaded.contains(i));
        let hidden = s.id.as_ref().is_some_and(|i| self.hidden_tracks.contains(i));
        match w {
            What::Download => (
                if downloaded { "Descargada" } else if s.is_episode { "Descargar episodio" } else { "Descargar canción" }.to_string(),
                s.id.is_some() && !downloaded,
            ),
            What::Timer => (
                match (self.sleep_at, self.sleep_end_of_track) {
                    (Some(at), _) => format!("Temporizador ({} min)", (at.saturating_duration_since(std::time::Instant::now()).as_secs() + 59) / 60),
                    (None, true) => "Temporizador (al terminar)".to_string(),
                    _ => "Temporizador de apagado".to_string(),
                },
                true,
            ),
            What::Hide => ((if hidden { "Mostrar canción" } else { "Ocultar canción" }).to_string(), s.id.is_some()),
            What::Share => ("Compartir canción".to_string(), s.track.as_ref().is_some_and(|t| !t.uri.is_empty())),
            What::Library => ("Agregar a la biblioteca".to_string(), s.track.is_some()),
            What::Queue => ("Agregar a la cola".to_string(), s.track.is_some()),
            What::Remove => (
                (if s.remove_from.as_deref() == Some("queue") { "Quitar de la cola" } else { "Quitar de esta playlist" }).to_string(),
                s.track.is_some() && s.remove_from.is_some(),
            ),
            What::Radio => ("Ir a la radio de la canción".to_string(), s.id.is_some() && !s.is_episode),
            What::Album => ("Ver álbum".to_string(), s.album_id.is_some()),
            What::Artist => ("Ver artista".to_string(), !s.artists.is_empty()),
            What::Miniplayer => ((if self.miniplayer { "Salir del miniplayer" } else { "Miniplayer" }).to_string(), true),
            What::Fullscreen => ((if self.fullscreen { "Salir de pantalla completa" } else { "Pantalla completa" }).to_string(), true),
            What::Jam => ((if self.jam.is_some() { "Jam" } else { "Iniciar una Jam" }).to_string(), true),
        }
    }

    /// Filas del submenú de `w`.
    fn more_sub_items(&self, w: What, s: &Song) -> Vec<(Pick, String)> {
        match w {
            What::Timer => {
                let mut v: Vec<(Pick, String)> = [15u64, 30, 45, 60].iter().map(|&m| (Pick::Timer(m), format!("{m} minutos"))).collect();
                v.push((Pick::TimerEnd, "Al terminar la canción".into()));
                if self.sleep_at.is_some() || self.sleep_end_of_track {
                    v.push((Pick::TimerCancel, "Cancelar temporizador".into()));
                }
                v
            }
            What::Library => {
                let liked = s.id.as_ref().is_some_and(|i| self.liked_set.contains(i));
                let mut v = Vec::new();
                if s.id.is_some() {
                    v.push((Pick::Like(!liked), if liked { "Quitar de Canciones que te gustan" } else { "Canciones que te gustan" }.to_string()));
                }
                v.push((Pick::AddToPlaylist, "Añadir a una playlist…".into()));
                v
            }
            What::Queue => vec![(Pick::QueueEnd, "Al final de la cola".into()), (Pick::OpenQueue, "Abrir la cola".into())],
            What::Artist => s.artists.iter().map(|(name, aid)| (Pick::Artist(aid.clone()), name.clone())).collect(),
            _ => Vec::new(),
        }
    }

    /// Dibuja el panel en `rect` (y el submenú abierto, `sub`) y recoge lo elegido. `floor` es
    /// el límite de abajo de los submenús; pulsar en `keep` (el botón que lo abre) no lo cierra.
    #[allow(clippy::too_many_arguments)]
    fn glass_menu(&self, ctx: &egui::Context, salt: &'static str, rect: Rect, rows: &[What], s: &Song, sub: &mut Option<usize>, floor: f32, keep: Rect) -> Outcome {
        let ink = ink(ctx);
        let screen = ctx.content_rect();
        let n = rows.len();
        // Sin sitio para todo: la fila base se aprieta un poco (ventanas muy bajas).
        let pitch = if rect.height() < natural_height(n) { (rect.height() - FIRST_BASE - LAST_PAD) / (n as f32 - 1.0) } else { ROW };
        let labels: Vec<(What, String, bool)> = rows.iter().map(|&w| {
            let (l, on) = self.more_label(w, s);
            (w, l, on)
        }).collect();

        let mut out = Outcome::default();
        egui::Area::new(egui::Id::new((salt, "panel")))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .show(ctx, |ui| {
                // Todo el panel recoge el puntero: lo de debajo no reacciona.
                ui.allocate_rect(rect, Sense::click());
                let painter = ui.painter().clone();
                glass(&painter, rect, &ink);
                let anchor_x = px(&painter, pos2(rect.min.x, 0.0)).0;
                for (k, (what, text, on)) in labels.iter().enumerate() {
                    let base = rect.min.y + FIRST_BASE + pitch * k as f32;
                    let c = base - 6.7;
                    let row = Rect::from_min_max(pos2(rect.min.x + 6.0, c - pitch / 2.0 + 1.0), pos2(rect.max.x - 6.0, c + pitch / 2.0 - 1.0));
                    let r = ui.interact(row, egui::Id::new((salt, "row", k)), if *on { Sense::click() } else { Sense::hover() });
                    let sub_open = *sub == Some(k);
                    if *on && (r.hovered() || sub_open) {
                        painter.rect_filled(row, CornerRadius::same(4), ink.hover);
                        if r.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                    }
                    // Al pasar por una fila con submenú se abre; por otra, se cierra.
                    if r.hovered() {
                        *sub = (what.submenu() && *on).then_some(k);
                    }
                    let icon = if *on { ink.icon } else { ink.off };
                    let a = (anchor_x, px(&painter, pos2(0.0, base)).1);
                    match what.mask() {
                        Some((name, m)) => paint_mask(&painter, name, m, a, icon),
                        // Del tamaño de los recortados (~20 px de tinta).
                        None => icons::paint(&painter, Rect::from_center_size(pos2(rect.min.x + 38.0, base - 6.5), vec2(27.0, 27.0)), icon, Icon::Trash),
                    }
                    let max_w = if what.chevron() { 168.0 } else { W - TEXT_X - 18.0 };
                    let color = if *on { ink.text } else { ink.off };
                    let g = galley_truncated(&painter, text, theme::semilight(FONT), color, max_w);
                    let lead = g.rows.first().and_then(|r| r.row.glyphs.first()).map(|gl| gl.pos.x + gl.uv_rect.offset.x).unwrap_or(0.0);
                    text_on_baseline(&painter, pos2(rect.min.x + TEXT_X - lead, base), g, color);
                    if what.chevron() {
                        paint_mask(&painter, "menu-chevron", &mm::CHEVRON, a, icon);
                    }
                    if r.clicked() {
                        if what.submenu() {
                            *sub = Some(k);
                        } else {
                            out.chosen = Some(*what);
                        }
                    }
                }
            });

        // Submenú al lado (a la izquierda; a la derecha si ahí no cabe), con su primera fila a
        // la altura de la que lo abrió.
        let mut sub_rect: Option<Rect> = None;
        if let Some(k) = *sub {
            let items = rows.get(k).map(|&w| self.more_sub_items(w, s)).unwrap_or_default();
            if !items.is_empty() {
                let base0 = rect.min.y + FIRST_BASE + pitch * k as f32;
                let h = natural_height(items.len());
                let top = (base0 - FIRST_BASE).min(floor - h).max(screen.min.y + MARGIN);
                let left = if rect.min.x - MARGIN - SUB_W >= screen.min.x + MARGIN { rect.min.x - MARGIN - SUB_W } else { rect.max.x + MARGIN };
                let srect = Rect::from_min_size(pos2(left, top), vec2(SUB_W, h));
                sub_rect = Some(srect);
                egui::Area::new(egui::Id::new((salt, "sub")))
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
                            let r = ui.interact(row, egui::Id::new((salt, "sub_row", j)), Sense::click());
                            if r.hovered() {
                                painter.rect_filled(row, CornerRadius::same(4), ink.hover);
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            let g = galley_truncated(&painter, text, theme::semilight(FONT), ink.text, SUB_W - SUB_TEXT_X - 18.0);
                            text_on_baseline(&painter, pos2(srect.min.x + SUB_TEXT_X, base), g, ink.text);
                            if r.clicked() {
                                out.picked = Some(pick.clone());
                            }
                        }
                    });
            }
        }

        // Cerrar: al elegir, con Escape o al pulsar fuera (el botón ya lo hace al volver a pulsarlo).
        let pressed_outside = ctx.input(|i| {
            i.pointer.any_pressed()
                && i.pointer.interact_pos().is_some_and(|p| !rect.contains(p) && !keep.contains(p) && !sub_rect.is_some_and(|s| s.contains(p)))
        });
        out.close = out.chosen.is_some() || out.picked.is_some() || pressed_outside || ctx.input(|i| i.key_pressed(egui::Key::Escape));
        out
    }

    /// Hace lo elegido en el menú.
    fn run_more(&mut self, ctx: &egui::Context, out: Outcome, s: &Song) {
        if let Some(w) = out.chosen {
            match w {
                What::Download => {
                    if let Some(id) = s.id.clone() {
                        self.actions.push(if s.is_episode { Action::DownloadEpisodes(vec![id]) } else { Action::Download(vec![id]) });
                    }
                }
                What::Hide => {
                    if let Some(id) = &s.id {
                        self.toggle_hidden(id);
                    }
                }
                What::Share => {
                    if let Some(t) = &s.track {
                        self.actions.push(Action::CopyText(super::widgets::uri_to_link(&t.uri), "Enlace"));
                    }
                }
                What::Remove => {
                    if let (Some(t), Some(pl)) = (&s.track, &s.remove_from) {
                        self.actions.push(Action::RemoveFromPlaylist { playlist_id: pl.clone(), uri: t.uri.clone() });
                    }
                }
                What::Radio => {
                    if let Some(id) = s.id.clone() {
                        self.actions.push(Action::OpenRadio(id));
                    }
                }
                What::Album => {
                    if let Some(aid) = s.album_id.clone() {
                        self.actions.push(Action::Go(Page::Album(aid)));
                    }
                }
                What::Miniplayer => self.toggle_miniplayer(ctx),
                What::Fullscreen => self.toggle_fullscreen(ctx),
                What::Jam => self.jam_open = true,
                What::Timer | What::Library | What::Queue | What::Artist => {}
            }
        }
        if let Some(p) = out.picked {
            match p {
                Pick::Timer(m) => {
                    self.sleep_at = Some(std::time::Instant::now() + std::time::Duration::from_secs(m * 60));
                    self.set_sleep_end_of_track(false);
                    self.status(format!("Se pausará en {m} minutos"));
                }
                Pick::TimerEnd => {
                    // Suspende el fundido hasta que se cumpla (ver `set_sleep_end_of_track`).
                    self.set_sleep_end_of_track(true);
                    self.sleep_at = None;
                    self.status("Se pausará al terminar la canción");
                }
                Pick::TimerCancel => {
                    self.sleep_at = None;
                    self.set_sleep_end_of_track(false);
                }
                Pick::Like(on) => {
                    if let Some(id) = s.id.clone() {
                        self.actions.push(Action::Like(id, on));
                    }
                }
                Pick::AddToPlaylist => {
                    if let Some(t) = &s.track {
                        self.open_add_dialog(vec![t.uri.clone()]);
                    }
                }
                Pick::QueueEnd => {
                    if let Some(t) = &s.track {
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

    /// El panel del reproductor, abierto con el botón de los tres puntos (`button`) o con el
    /// clic derecho en la portada (`player_more_at`, desde el puntero). Se cierra al elegir algo,
    /// al pulsar fuera o con Escape.
    pub(super) fn player_more_panel(&mut self, ctx: &egui::Context, button: Rect) {
        let screen = ctx.content_rect();
        let song = Song::new(self.player.now.as_ref().map(super::track_from_now), None, None);
        let natural = natural_height(ROWS.len());
        let (rect, floor, keep) = match self.player_more_at {
            Some(p) => (place(screen, Anchor::Pointer(p), natural), screen.max.y - MARGIN, Rect::NOTHING),
            None => {
                let bottom = screen.max.y - FROM_BOTTOM;
                let top = (bottom - natural).max(screen.min.y + MARGIN);
                let right = button.center().x + RIGHT_OF_DOTS;
                (Rect::from_min_max(pos2(right - W, top), pos2(right, bottom)), bottom, button)
            }
        };
        let mut sub = self.player_more_sub;
        let out = self.glass_menu(ctx, "player_more", rect, &ROWS, &song, &mut sub, floor, keep);
        self.player_more_sub = sub;
        if out.close {
            self.close_player_more();
        }
        self.run_more(ctx, out, &song);
    }

    pub(super) fn close_player_more(&mut self) {
        self.player_more_open = false;
        self.player_more_sub = None;
        self.player_more_at = None;
    }

    /// Abre (o cierra, si ya era el de esa fila) el menú de una canción de una lista.
    pub(super) fn toggle_song_menu(&mut self, menu: SongMenu) {
        if self.song_more.as_ref().is_some_and(|m| m.row == menu.row && matches!(m.anchor, Anchor::Dots(_)) && matches!(menu.anchor, Anchor::Dots(_))) {
            self.song_more = None;
        } else {
            self.song_more = Some(menu);
        }
    }

    /// Si el menú abierto es el de la fila `i` de `list_id`.
    pub(super) fn song_menu_on(&self, list_id: &str, i: usize) -> bool {
        self.song_more.as_ref().is_some_and(|m| m.row.0 == list_id && m.row.1 == i)
    }

    /// El menú abierto de una canción de una lista (`song_more`). Se cierra al elegir, al pulsar
    /// fuera, con Escape o al desplazar la lista (el botón que lo abrió se movería).
    pub(super) fn song_more_panel(&mut self, ctx: &egui::Context) {
        let Some(m) = self.song_more.take() else {
            return;
        };
        let screen = ctx.content_rect();
        let rows = song_rows(m.remove_from.is_some());
        let rect = place(screen, m.anchor, natural_height(rows.len()));
        let keep = match m.anchor {
            Anchor::Dots(b) => b,
            Anchor::Pointer(_) => Rect::NOTHING,
        };
        let song = Song::new(Some(m.track.clone()), m.album_id.clone(), m.remove_from.clone());
        let mut sub = m.sub;
        let out = self.glass_menu(ctx, "song_more", rect, &rows, &song, &mut sub, screen.max.y - MARGIN, keep);
        let scrolled = ctx.input(|i| i.smooth_scroll_delta != egui::Vec2::ZERO);
        if !out.close && !scrolled {
            self.song_more = Some(SongMenu { sub, ..m });
        }
        self.run_more(ctx, out, &song);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Rect {
        Rect::from_min_size(Pos2::ZERO, vec2(1770.0, 1025.0))
    }

    #[test]
    fn junto_a_los_puntos_debajo_o_encima() {
        let h = natural_height(song_rows(false).len());
        // Fila arriba: el panel cae debajo del botón, con el borde derecho a 40,5 del centro.
        let b = Rect::from_center_size(pos2(1200.0, 200.0), vec2(40.0, 40.0));
        let r = place(screen(), Anchor::Dots(b), h);
        assert_eq!(r.max.x, 1200.0 + RIGHT_OF_DOTS);
        assert_eq!(r.min.y, b.max.y + GAP);
        // Fila abajo: encima del botón.
        let b = Rect::from_center_size(pos2(1200.0, 900.0), vec2(40.0, 40.0));
        let r = place(screen(), Anchor::Dots(b), h);
        assert!((r.max.y - (b.min.y - GAP)).abs() < 1e-3);
        // Sin sitio ni arriba ni abajo: dentro de la ventana.
        let b = Rect::from_center_size(pos2(1200.0, 512.0), vec2(40.0, 40.0));
        let r = place(screen(), Anchor::Dots(b), h);
        assert!(r.min.y >= MARGIN && r.max.y <= 1025.0 - MARGIN);
    }

    #[test]
    fn desde_el_puntero_sin_salirse() {
        let h = natural_height(song_rows(true).len());
        let r = place(screen(), Anchor::Pointer(pos2(1700.0, 100.0)), h);
        assert_eq!(r.max.x, 1700.0);
        assert_eq!(r.min.y, 100.0);
        let r = place(screen(), Anchor::Pointer(pos2(100.0, 1000.0)), h);
        assert_eq!(r.min.x, 100.0);
        assert!((r.max.y - 1000.0).abs() < 1e-3);
    }

    #[test]
    fn quitar_solo_en_las_editables() {
        assert!(!song_rows(false).contains(&What::Remove));
        let v = song_rows(true);
        assert_eq!(v.iter().position(|w| *w == What::Remove), Some(6));
        assert!(!v.contains(&What::Jam) && !v.contains(&What::Miniplayer));
    }
}
