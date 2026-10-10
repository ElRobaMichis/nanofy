//! Panel de la cola (el botón de la cola del reproductor), copiado de la referencia de diseño
//! 1.webp (la ventana de 12.webp reducida a 0,8525; cliente = (ref − (1,25, 1,25)) / 0,8525): un
//! cristal de 432 px sobre el lado derecho del contenido, con lo de detrás desenfocado como los
//! menús de cristal. Arriba, las pestañas «Cola» y «Recientes»; en la cola, la canción que suena
//! (con barras verdes que se mueven sobre su portada), «A continuación:» y un grupo por origen: lo
//! añadido a la cola desde Nanofy y lo que queda de la lista en curso, con aleatorio, repetir y
//! una X para quitarlo. Cada canción lleva un asa para reordenar (las añadidas a la cola) y una X
//! para quitarla. Abajo, el interruptor de la reproducción automática.

use std::time::Duration;

use egui::{pos2, vec2, Color32, CornerRadius, Pos2, Rect, Sense};

use super::icons::{self, Icon};
use super::player_menu::{Anchor, SongMenu};
use super::theme::{self, GREEN};
use super::widgets::{galley_truncated, text_on_baseline};
use super::{Action, App, Page, PlayState, PlayTarget, Repeat};
use crate::api::Req;
use crate::model::Track;

/// Ancho del panel; va 2 px por debajo del borde de arriba del contenido y 1 px dentro de los de
/// la derecha y abajo, con esquinas de 8.
const W: f32 = 432.0;
const TOP_INSET: f32 = 2.0;
const EDGE_INSET: f32 = 1.0;
const RADIUS: u8 = 8;
/// El mismo desenfoque que los menús de cristal (`player_menu.rs`).
const BLUR_SIGMA: f32 = 56.0;

/// Pestañas: tinta desde 24,35 y 122, línea base a 31,4 (Segoe UI 14). La línea de debajo
/// (1 px) y el subrayado verde de la elegida (2 px, de 5 antes a 12 después del texto) van
/// centrados en 51,75.
const TAB_X: [f32; 2] = [24.35, 122.0];
const TAB_BASE: f32 = 31.4;
const TAB_FONT: f32 = 14.0;
const RULE_Y: f32 = 51.75;
const UNDER_PAD: (f32, f32) = (5.0, 12.0);

/// Los tamaños de letra son enteros: egui pinta cada fuente a un número entero de píxeles (la
/// altura de las mayúsculas, igualada con la referencia, sale de ahí).
///
/// La lista va de debajo de la línea de las pestañas a encima de la del pie.
const LIST_TOP: f32 = 52.5;

/// Filas, una cada 64,2: portada de 53 a 23,7 del borde (radio 4); texto (Segoe UI Semilight,
/// de trazo fino como el de la referencia) desde 89, el título (16) con la línea base 4,2 por
/// encima del centro y el artista (14) 19 por debajo; asa de 2 × 3 puntos (radio 1,6, cada
/// 7,5) centrada en 351 y X de 12 en 397.
const ROW: f32 = 64.2;
const COVER_X: f32 = 23.7;
const COVER: f32 = 53.0;
const TEXT_X: f32 = 89.0;
const TEXT_END: f32 = 330.0;
const TITLE_DY: f32 = -4.2;
const ARTIST_DY: f32 = 19.0;
const TITLE_FONT: f32 = 16.0;
const ARTIST_FONT: f32 = 14.0;
const HANDLE_X: f32 = 351.0;
const HANDLE_DY: f32 = 1.2;
const DOT_GAP: f32 = 7.5;
const DOT_R: f32 = 1.6;
const CLOSE_X: f32 = 397.0;
const CLOSE_DY: f32 = 0.8;
const CLOSE: f32 = 19.0;

/// Centro de la canción que suena (desde arriba del panel), línea base de «A continuación:»
/// (negrita 17) 79 más abajo y centro de la cabecera del primer grupo 33 por debajo de ella.
const NOW_Y: f32 = 98.0;
const NEXT_DY: f32 = 79.0;
const NEXT_FONT: f32 = 17.0;
const HEAD_DY: f32 = 33.0;
/// Cabecera de grupo: icono (17) centrado en 33,5; nombre (Semilight 16) desde 50 con la línea
/// base 7,8 bajo el centro; aleatorio en 305, repetir en 351,5 y X en 397. La primera fila, 62
/// por debajo; tras la última, la cabecera siguiente a 67,4.
const HEAD_ICON_X: f32 = 33.5;
const HEAD_ICON: f32 = 17.0;
const HEAD_TEXT_X: f32 = 50.0;
const HEAD_BASE_DY: f32 = 7.8;
const HEAD_FONT: f32 = 16.0;
const SHUFFLE_X: f32 = 305.0;
const REPEAT_X: f32 = 351.5;
const BAR_ICON: f32 = 28.0;
const FIRST_ROW_DY: f32 = 62.0;
const NEXT_HEAD_DY: f32 = 67.4;

/// Barras de la canción que suena: tres de 2,5 cada 6, centradas 0,6 a la izquierda del centro de
/// la portada y con el pie 8 por debajo de él; de 4 a 16 de alto. La portada, oscurecida.
const BAR_W: f32 = 2.5;
const BAR_GAP: f32 = 6.0;
const BAR_DX: f32 = -0.6;
const BAR_FOOT: f32 = 8.0;
const BAR_MIN: f32 = 4.0;
const BAR_MAX: f32 = 16.0;
/// En pausa, quietas a la altura de la referencia.
const BAR_STILL: [f32; 3] = [10.0, 15.0, 6.5];
const NOW_SHADE: u8 = 194;

/// Pie (desde abajo del panel): línea a 56,1; texto (Semilight 14) desde 23,9 con la línea base
/// a 23,5; e interruptor de 50,4 × 25,8 desde 359 con el centro a 29, con la bola (radio 9,6) a
/// 12,3 o 37,1 de su borde izquierdo, como el de «Agrupar».
const FOOT_RULE: f32 = 56.1;
const FOOT_TEXT_X: f32 = 23.9;
const FOOT_BASE: f32 = 23.5;
const FOOT_FONT: f32 = 14.0;
const SWITCH_X: f32 = 359.0;
const SWITCH: (f32, f32) = (50.4, 25.8);
const SWITCH_DY: f32 = 29.0;
const KNOB_R: f32 = 9.6;
const KNOB_OFF: f32 = 12.3;
const KNOB_ON: f32 = 37.1;

/// Colores: los de la referencia en oscuro; en claro, los de la paleta.
struct Ink {
    dark: bool,
    glass: Color32,
    tab_on: Color32,
    tab_off: Color32,
    title: Color32,
    artist: Color32,
    head: Color32,
    foot: Color32,
    icon: Color32,
    rule: Color32,
    hover: Color32,
    track: Color32,
    knob_off: Color32,
}

fn ink(ctx: &egui::Context) -> Ink {
    let p = theme::palette(ctx);
    if p.dark {
        Ink {
            dark: true,
            glass: Color32::from_rgba_unmultiplied(34, 34, 34, 191),
            tab_on: Color32::from_gray(232),
            tab_off: Color32::from_gray(136),
            title: Color32::from_gray(230),
            artist: Color32::from_gray(128),
            head: Color32::WHITE,
            foot: Color32::from_gray(214),
            icon: Color32::from_gray(160),
            rule: Color32::from_white_alpha(44),
            hover: Color32::from_white_alpha(10),
            track: Color32::from_gray(17),
            knob_off: Color32::from_gray(53),
        }
    } else {
        Ink {
            dark: false,
            glass: Color32::from_rgba_unmultiplied(250, 250, 250, 225),
            tab_on: p.text,
            tab_off: p.weak,
            title: p.text,
            artist: p.weak,
            head: p.text,
            foot: p.text,
            icon: p.weak,
            rule: p.border,
            hover: p.hover,
            track: p.card2,
            knob_off: p.faint,
        }
    }
}

/// De dónde viene una fila: qué hacen su X y su asa.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// La que suena: la X la salta.
    Now,
    /// Añadida a la cola desde Nanofy (posición entre ellas): se quita y se reordena.
    Queued(usize),
    /// Lo que queda de la lista en curso: Spotify no deja quitarla suelta.
    Context,
    /// En «Recientes»: sin asa ni X.
    Recent,
}

/// Grupo de la cola.
struct Group {
    icon: Icon,
    name: String,
    page: Option<Page>,
    /// Lo que queda de la lista en curso (aleatorio, repetir y X que la quita); si no, lo
    /// añadido a la cola (solo X, que la vacía).
    context: bool,
    tracks: Vec<Track>,
}

/// Alturas de las tres barras en el instante `t` (s): ondas de periodos distintos, para que no
/// se muevan a la vez.
fn bar_heights(t: f64, playing: bool) -> [f32; 3] {
    if !playing {
        return BAR_STILL;
    }
    const F: [(f64, f64, f64); 3] = [(1.9, 3.1, 0.0), (2.3, 1.3, 1.7), (1.6, 2.7, 3.4)];
    std::array::from_fn(|i| {
        let (a, b, ph) = F[i];
        let tau = std::f64::consts::TAU;
        let s = 0.5 + 0.32 * (t * a * tau + ph).sin() + 0.18 * (t * b * tau + 2.0 * ph).sin();
        BAR_MIN + (BAR_MAX - BAR_MIN) * s.clamp(0.0, 1.0) as f32
    })
}

/// Hueco (0..=n, antes de cada fila o tras la última) más cercano a `dy`, la distancia desde el
/// centro de la primera fila.
fn slot_at(dy: f32, n: usize) -> usize {
    ((dy / ROW) + 0.5).floor().clamp(0.0, n as f32) as usize
}

/// La cola con la de `from` llevada al hueco `slot`; `None` si queda igual.
fn reordered(order: &[String], from: usize, slot: usize) -> Option<Vec<String>> {
    if from >= order.len() {
        return None;
    }
    let to = if slot > from { slot - 1 } else { slot };
    if to == from {
        return None;
    }
    let mut v = order.to_vec();
    let u = v.remove(from);
    v.insert(to.min(v.len()), u);
    Some(v)
}

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

/// Donde empieza la tinta del primer carácter de un texto (para alinear tinta, no cajas).
fn ink_left(g: &egui::Galley) -> f32 {
    g.rows.first().and_then(|r| r.row.glyphs.first()).map(|gl| gl.uv_rect.offset.x.max(0.0) + gl.pos.x).unwrap_or(0.0)
}

/// Texto con su tinta desde `x` y la línea base en `base`; devuelve su rectángulo.
fn text_at(painter: &egui::Painter, text: &str, font: egui::FontId, color: Color32, x: f32, base: f32, max_w: f32) -> Rect {
    let g = galley_truncated(painter, text, font, color, max_w);
    let lead = ink_left(&g);
    text_on_baseline(painter, pos2(x - lead, base), g, color)
}

/// Botón de icono: zona de 34 px, se aclara al pasar el ratón.
fn icon_button(ui: &mut egui::Ui, id: egui::Id, c: Pos2, side: f32, icon: Icon, color: Color32, tip: &str) -> egui::Response {
    let r = ui.interact(Rect::from_center_size(c, vec2(34.0, 34.0)), id, Sense::click()).on_hover_text(tip);
    let color = if r.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        color.lerp_to_gamma(Color32::WHITE, 0.45)
    } else {
        color
    };
    icons::paint(ui.painter(), Rect::from_center_size(c, vec2(side, side)), color, icon);
    r
}

impl App {
    /// El panel de la cola, abierto con el botón de la cola (`side` = Cola), sobre el lado derecho
    /// del panel de contenido `content`.
    pub(super) fn queue_panel(&mut self, ctx: &egui::Context, content: Rect) {
        let left = (content.max.x - EDGE_INSET - W).max(content.min.x + EDGE_INSET);
        let rect = Rect::from_min_max(pos2(left, content.min.y + TOP_INSET), pos2(content.max.x - EDGE_INSET, content.max.y - EDGE_INSET));
        if rect.height() < 200.0 {
            return;
        }
        let ink = ink(ctx);
        egui::Area::new(egui::Id::new("queue_panel"))
            .order(egui::Order::Middle)
            .fixed_pos(rect.min)
            .show(ctx, |ui| {
                ui.set_clip_rect(rect);
                // Todo el panel recoge el puntero: lo de debajo no reacciona.
                ui.allocate_rect(rect, Sense::click());
                let painter = ui.painter().clone();
                glass(&painter, rect, &ink);
                self.queue_tabs(ui, &painter, rect, &ink);
                let list = Rect::from_min_max(pos2(rect.min.x, rect.min.y + LIST_TOP), pos2(rect.max.x, rect.max.y - FOOT_RULE - 0.5));
                let mut c = ui.new_child(egui::UiBuilder::new().max_rect(list));
                c.set_clip_rect(list);
                egui::ScrollArea::vertical()
                    .id_salt("queue_list")
                    .auto_shrink([false, false])
                    .show(&mut c, |ui| {
                        // Origen de las medidas (lo de arriba del panel) dentro de lo desplazado.
                        let o = pos2(rect.min.x, ui.max_rect().min.y - LIST_TOP);
                        let h = if self.queue_recent { self.recent_list(ui, o, &ink) } else { self.queue_list(ui, o, &ink) };
                        ui.allocate_rect(Rect::from_min_size(pos2(rect.min.x, o.y + LIST_TOP), vec2(W, (h - LIST_TOP).max(0.0))), Sense::hover());
                    });
                self.queue_footer(ui, &painter, rect, &ink);
            });
    }

    fn queue_tabs(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, rect: Rect, ink: &Ink) {
        let y = rect.min.y + RULE_Y;
        painter.rect_filled(Rect::from_min_max(pos2(rect.min.x, y - 0.5), pos2(rect.max.x, y + 0.5)), CornerRadius::ZERO, ink.rule);
        for (k, label) in ["Cola", "Recientes"].into_iter().enumerate() {
            let on = self.queue_recent == (k == 1);
            let x = rect.min.x + TAB_X[k];
            let r = text_at(painter, label, theme::regular(TAB_FONT), if on { ink.tab_on } else { ink.tab_off }, x, rect.min.y + TAB_BASE, 200.0);
            let ink_right = r.max.x;
            if on {
                let under = Rect::from_min_max(pos2(x - UNDER_PAD.0, y - 1.0), pos2(ink_right + UNDER_PAD.1, y + 1.0));
                painter.rect_filled(under, CornerRadius::same(1), GREEN);
            }
            let hit = Rect::from_min_max(pos2(x - 12.0, rect.min.y), pos2(ink_right + 14.0, y));
            let resp = ui.interact(hit, egui::Id::new(("queue_tab", k)), Sense::click());
            if resp.hovered() && !on {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                self.queue_recent = k == 1;
            }
        }
    }

    /// La cola: la que suena, «A continuación:» y los grupos. Devuelve hasta dónde llega (desde
    /// arriba del panel).
    fn queue_list(&mut self, ui: &mut egui::Ui, o: Pos2, ink: &Ink) -> f32 {
        let painter = ui.painter().clone();
        if !self.signed_in() {
            text_at(&painter, "Inicia sesión para ver la cola.", theme::regular(ARTIST_FONT + 1.0), ink.artist, o.x + TAB_X[0], o.y + NOW_Y, W - 50.0);
            return NOW_Y + 20.0;
        }
        let Some(q) = self.queue.clone() else {
            text_at(&painter, "Cargando la cola…", theme::regular(ARTIST_FONT + 1.0), ink.artist, o.x + TAB_X[0], o.y + NOW_Y, W - 50.0);
            return NOW_Y + 20.0;
        };
        // Lo añadido a la cola desde Nanofy va primero; el resto es la continuación de la lista
        // en curso. Es el mismo orden en que sonarán.
        let mut queued: Vec<Track> = Vec::new();
        let mut rest: Vec<Track> = Vec::new();
        let mut pending: Vec<String> = self.queued_local.clone();
        for t in &q.queue {
            if rest.is_empty() {
                if let Some(i) = pending.iter().position(|u| u == &t.uri) {
                    pending.remove(i);
                    queued.push(t.clone());
                    continue;
                }
            }
            rest.push(t.clone());
        }
        let mut groups: Vec<Group> = Vec::new();
        if !queued.is_empty() {
            groups.push(Group { icon: Icon::QueueList, name: "Siguiente en la cola".into(), page: None, context: false, tracks: queued });
        }
        if !rest.is_empty() {
            // El nombre de la lista en curso, si aún no se tiene, se pide como al abrir su página.
            match self.now_context() {
                Some((_, Some(Page::Playlist(id)))) if !self.playlist_meta.contains_key(&id) && !self.playlists.iter().any(|p| p.id == id) => {
                    self.ensure_playlist(&id)
                }
                Some((_, Some(Page::Album(id)))) if !self.albums.contains_key(&id) => self.request_once(&format!("album:{id}"), Req::Album(id.clone())),
                _ => {}
            }
            let (name, page) = self.now_context().unwrap_or_else(|| ("Siguientes canciones".into(), None));
            let icon = page.as_ref().map(|p| self.page_label(p).0).unwrap_or(Icon::Playlist);
            groups.push(Group { icon, name, page, context: true, tracks: rest });
        }

        // La que suena (la del reproductor, que se entera antes que la cola de Spotify).
        let now = self.player.now.as_ref().map(super::track_from_now).or(q.currently_playing.clone());
        let mut y = NOW_Y;
        if let Some(t) = &now {
            self.queue_row(ui, o, y, t, Kind::Now, "queue_now", 0, &[], ink);
            y += NEXT_DY;
        } else {
            y = LIST_TOP + 32.0;
        }
        if groups.is_empty() {
            text_at(&painter, "La cola está vacía.", theme::regular(ARTIST_FONT + 1.0), ink.artist, o.x + TAB_X[0], o.y + y, W - 50.0);
            return y + 24.0;
        }
        text_at(&painter, "A continuación:", theme::bold(NEXT_FONT), ink.head, o.x + TAB_X[0], o.y + y, W - 50.0);
        let mut head = y + HEAD_DY;
        let mut end = head;
        // Primera fila y cuántas tiene el grupo de lo añadido a la cola (para reordenar).
        let mut queued_rows: Option<(f32, usize)> = None;
        for (gi, g) in groups.iter().enumerate() {
            self.queue_group_head(ui, o, head, g, gi, ink);
            let list_id = if g.context { "queue_rest" } else { "queue_next" };
            let first = head + FIRST_ROW_DY;
            for (i, t) in g.tracks.iter().enumerate() {
                let c = first + ROW * i as f32;
                let kind = if g.context { Kind::Context } else { Kind::Queued(i) };
                self.queue_row(ui, o, c, t, kind, list_id, i, &g.tracks, ink);
            }
            let last = first + ROW * (g.tracks.len() as f32 - 1.0);
            if !g.context {
                queued_rows = Some((first, g.tracks.len()));
            }
            end = last + ROW / 2.0;
            head = last + NEXT_HEAD_DY;
        }
        // Arrastrando una añadida por su asa: una raya verde en el hueco donde caería y, al
        // soltar, la cola se rehace en el orden nuevo.
        if let Some(from) = self.queue_drag {
            let down = ui.input(|i| i.pointer.any_down());
            let py = ui.input(|i| i.pointer.interact_pos().or(i.pointer.hover_pos())).map(|p| p.y);
            if let (Some((first, n)), Some(py)) = (queued_rows, py) {
                let slot = slot_at(py - o.y - first, n);
                if down {
                    let my = o.y + first - ROW / 2.0 + ROW * slot as f32;
                    painter.rect_filled(Rect::from_min_max(pos2(o.x + COVER_X, my - 1.0), pos2(o.x + CLOSE_X + 8.0, my + 1.0)), CornerRadius::same(1), GREEN);
                } else if let Some(order) = reordered(&self.queued_local, from, slot) {
                    self.queue_rebuild(order);
                }
            }
            if !down {
                self.queue_drag = None;
            }
        }
        // Las barras se mueven: un fotograma cada 80 ms mientras suena (solo con ellas a la vista).
        if now.is_some() && self.player.state == PlayState::Playing {
            ui.ctx().request_repaint_after(Duration::from_millis(80));
        }
        end + 16.0
    }

    /// «Recientes»: lo último escuchado, del más reciente al más antiguo.
    fn recent_list(&mut self, ui: &mut egui::Ui, o: Pos2, ink: &Ink) -> f32 {
        let painter = ui.painter().clone();
        if self.recent_at.elapsed() > Duration::from_secs(45) {
            self.recent_at = std::time::Instant::now();
            self.api.send(Req::Recent);
        }
        let recent = self.history_list();
        if recent.is_empty() {
            text_at(&painter, "Cargando…", theme::regular(ARTIST_FONT + 1.0), ink.artist, o.x + TAB_X[0], o.y + NOW_Y, W - 50.0);
            return NOW_Y + 20.0;
        }
        for (i, t) in recent.iter().enumerate() {
            self.queue_row(ui, o, NOW_Y + ROW * i as f32, t, Kind::Recent, "queue_recent", i, &recent, ink);
        }
        NOW_Y + ROW * (recent.len() as f32 - 0.5) + 16.0
    }

    fn queue_group_head(&mut self, ui: &mut egui::Ui, o: Pos2, cy: f32, g: &Group, gi: usize, ink: &Ink) {
        let painter = ui.painter().clone();
        let y = o.y + cy;
        icons::paint(&painter, Rect::from_center_size(pos2(o.x + HEAD_ICON_X, y), vec2(HEAD_ICON, HEAD_ICON)), ink.icon, g.icon);
        let end = if g.context { SHUFFLE_X - 22.0 } else { CLOSE_X - 22.0 };
        let r = text_at(&painter, &g.name, theme::semilight(HEAD_FONT), ink.head, o.x + HEAD_TEXT_X, y + HEAD_BASE_DY, end - HEAD_TEXT_X);
        if let Some(page) = &g.page {
            let resp = ui.interact(r.expand(3.0), egui::Id::new(("queue_head", gi)), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                painter.line_segment([pos2(r.min.x, r.max.y + 1.0), pos2(r.max.x, r.max.y + 1.0)], egui::Stroke::new(1.0, ink.head));
            }
            if resp.clicked() {
                self.actions.push(Action::Go(page.clone()));
            }
        }
        if g.context {
            let shuffle = if self.player.shuffle { GREEN } else { ink.icon };
            if icon_button(ui, egui::Id::new(("queue_shuffle", gi)), pos2(o.x + SHUFFLE_X, y + 0.5), BAR_ICON, Icon::Shuffle, shuffle, "Aleatorio").clicked() {
                self.toggle_shuffle();
            }
            let (icon, color) = match self.player.repeat {
                Repeat::Off => (Icon::Repeat, ink.icon),
                Repeat::Context => (Icon::Repeat, GREEN),
                Repeat::Track => (Icon::RepeatOne, GREEN),
            };
            if icon_button(ui, egui::Id::new(("queue_repeat", gi)), pos2(o.x + REPEAT_X, y + 1.5), BAR_ICON, icon, color, "Repetir").clicked() {
                self.cycle_repeat();
            }
            let tip = format!("Quitar lo que queda de {}", g.name);
            if icon_button(ui, egui::Id::new(("queue_drop", gi)), pos2(o.x + CLOSE_X, y), CLOSE, Icon::Close, ink.icon, &tip).clicked() {
                self.queue_drop_context();
            }
        } else if icon_button(ui, egui::Id::new(("queue_clear", gi)), pos2(o.x + CLOSE_X, y), CLOSE, Icon::Close, ink.icon, "Vaciar la cola").clicked() {
            self.queue_clear();
        }
    }

    /// Una canción centrada en `cy` (desde arriba del panel): portada, título, artista, asa y X.
    #[allow(clippy::too_many_arguments)]
    fn queue_row(&mut self, ui: &mut egui::Ui, o: Pos2, cy: f32, t: &Track, kind: Kind, list_id: &str, i: usize, rows: &[Track], ink: &Ink) {
        let y = o.y + cy;
        let row = Rect::from_min_max(pos2(o.x + 10.0, y - ROW / 2.0 + 2.0), pos2(o.x + W - 10.0, y + ROW / 2.0 - 2.0));
        if !ui.is_rect_visible(row) {
            return;
        }
        let resp = ui.interact(row, egui::Id::new(("queue_row", list_id, i)), Sense::click());
        let painter = ui.painter().clone();
        let menu_here = self.song_menu_on(list_id, i);
        if resp.hovered() || resp.contains_pointer() || menu_here || self.queue_drag.is_some_and(|f| kind == Kind::Queued(f)) {
            painter.rect_filled(row, CornerRadius::same(6), ink.hover);
        }
        // Portada; la que suena, oscurecida y con las barras verdes.
        let cover = Rect::from_center_size(pos2(o.x + COVER_X + COVER / 2.0, y), vec2(COVER, COVER));
        let url = t.cover(64).map(|s| s.to_string());
        self.cover_in(ui, url.as_deref(), cover, 4);
        if kind == Kind::Now {
            painter.rect_filled(cover, CornerRadius::same(4), Color32::from_black_alpha(NOW_SHADE));
            let playing = self.player.state == PlayState::Playing;
            let hs = bar_heights(ui.input(|i| i.time), playing);
            let foot = y + BAR_FOOT;
            for (k, h) in hs.iter().enumerate() {
                let cx = cover.center().x + BAR_DX + BAR_GAP * (k as f32 - 1.0);
                painter.rect_filled(Rect::from_min_max(pos2(cx - BAR_W / 2.0, foot - h), pos2(cx + BAR_W / 2.0, foot)), CornerRadius::same(1), GREEN);
            }
        }
        let max_w = TEXT_END - TEXT_X;
        text_at(&painter, &t.name, theme::semilight(TITLE_FONT), ink.title, o.x + TEXT_X, y + TITLE_DY, max_w);
        text_at(&painter, &t.artists_str(), theme::semilight(ARTIST_FONT), ink.artist, o.x + TEXT_X, y + ARTIST_DY, max_w);

        if kind != Kind::Recent {
            // Asa: arrastrar reordena las añadidas a la cola.
            let hc = pos2(o.x + HANDLE_X, y + HANDLE_DY);
            let handle = ui.interact(Rect::from_center_size(hc, vec2(26.0, 34.0)), egui::Id::new(("queue_handle", list_id, i)), Sense::drag());
            let movable = matches!(kind, Kind::Queued(_));
            if movable && handle.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
            }
            if let (Kind::Queued(k), true) = (kind, handle.drag_started()) {
                self.queue_drag = Some(k);
            }
            if movable && handle.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            }
            let dots = if movable && handle.hovered() { ink.icon.lerp_to_gamma(Color32::WHITE, 0.45) } else { ink.icon };
            for dx in [-DOT_GAP / 2.0, DOT_GAP / 2.0] {
                for dy in [-DOT_GAP, 0.0, DOT_GAP] {
                    painter.circle_filled(pos2(hc.x + dx, hc.y + dy), DOT_R, dots);
                }
            }
            let tip = match kind {
                Kind::Now => "Saltar",
                _ => "Quitar de la cola",
            };
            if icon_button(ui, egui::Id::new(("queue_x", list_id, i)), pos2(o.x + CLOSE_X, y + CLOSE_DY), CLOSE, Icon::Close, ink.icon, tip).clicked() {
                match kind {
                    Kind::Now => self.next(),
                    // Las de la lista en curso: `queue_remove` explica que no se puede.
                    _ => self.queue_remove(&t.uri),
                }
            }
        }

        if resp.double_clicked() {
            match kind {
                Kind::Now => {}
                Kind::Recent => self.actions.push(Action::Play(PlayTarget::Tracks {
                    uris: rows.iter().map(|t| t.uri.clone()).collect(),
                    index: Some(i as u32),
                    shuffle: self.player.shuffle,
                })),
                _ => self.play_from_queue(&t.uri, rows, i),
            }
        }
        if resp.secondary_clicked() {
            if let Some(at) = resp.interact_pointer_pos() {
                self.song_more = Some(SongMenu {
                    track: t.clone(),
                    album_id: None,
                    remove_from: matches!(kind, Kind::Queued(_)).then(|| "queue".to_string()),
                    row: (list_id.to_string(), i),
                    anchor: Anchor::Pointer(at),
                    sub: None,
                });
            }
        }
    }

    fn queue_footer(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, rect: Rect, ink: &Ink) {
        let b = rect.max.y;
        let ry = b - FOOT_RULE;
        painter.rect_filled(Rect::from_min_max(pos2(rect.min.x, ry - 0.5), pos2(rect.max.x, ry + 0.5)), CornerRadius::ZERO, ink.rule);
        text_at(painter, "Reproducir canciones similares", theme::semilight(FOOT_FONT), ink.foot, rect.min.x + FOOT_TEXT_X, b - FOOT_BASE, SWITCH_X - FOOT_TEXT_X - 12.0);
        let cy = b - SWITCH_DY;
        let track = Rect::from_min_size(pos2(rect.min.x + SWITCH_X, cy - SWITCH.1 / 2.0), vec2(SWITCH.0, SWITCH.1));
        let on = self.settings.autoplay;
        let hit = Rect::from_min_max(pos2(rect.min.x + 8.0, ry + 4.0), pos2(rect.max.x - 8.0, b - 4.0));
        let resp = ui
            .interact(hit, egui::Id::new("queue_autoplay"), Sense::click())
            .on_hover_text("Al acabarse lo que suena, sigue con canciones parecidas");
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        painter.rect_filled(track, CornerRadius::same(13), ink.track);
        let t = ui.ctx().animate_bool_with_time(egui::Id::new("queue_autoplay_knob"), on, 0.12);
        let kx = track.min.x + KNOB_OFF + (KNOB_ON - KNOB_OFF) * t;
        painter.circle_filled(pos2(kx, cy), KNOB_R, if on { GREEN } else { ink.knob_off });
        if resp.clicked() {
            self.set_autoplay(!on);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn huecos_y_orden_al_soltar() {
        let o: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        // Sobre su propio hueco o el de después: igual.
        assert_eq!(reordered(&o, 1, 1), None);
        assert_eq!(reordered(&o, 1, 2), None);
        assert_eq!(reordered(&o, 0, 4).unwrap(), ["b", "c", "d", "a"]);
        assert_eq!(reordered(&o, 3, 0).unwrap(), ["d", "a", "b", "c"]);
        assert_eq!(reordered(&o, 1, 3).unwrap(), ["a", "c", "b", "d"]);
        // El hueco: medio paso por encima del centro de una fila es el de antes de ella.
        assert_eq!(slot_at(-40.0, 4), 0);
        assert_eq!(slot_at(-31.0, 4), 0);
        assert_eq!(slot_at(33.0, 4), 1);
        assert_eq!(slot_at(500.0, 4), 4);
    }

    #[test]
    fn barras_quietas_en_pausa_y_dentro_de_su_alto() {
        assert_eq!(bar_heights(3.7, false), BAR_STILL);
        for k in 0..200 {
            for h in bar_heights(k as f64 * 0.037, true) {
                assert!((BAR_MIN..=BAR_MAX).contains(&h));
            }
        }
    }
}
