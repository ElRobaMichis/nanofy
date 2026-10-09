//! Widgets reutilizables: portadas, tarjetas, chips, cabeceras, tabla de pistas y menús.
//!
//! Las columnas de la tabla y las zonas de la barra del reproductor se colocan con
//! rectángulos explícitos: `allocate_ui_with_layout` encoge la zona a su contenido y no sirve
//! para alinear columnas.

use egui::{pos2, vec2, Align, Color32, CornerRadius, Label, Layout, Rect, RichText, Sense, Stroke, UiBuilder};

use super::icons::{self, Icon};
use super::player_menu::{Anchor, SongMenu};
use super::theme::{self, GREEN};
use super::{Action, App, Page, PlayTarget, ROW_H};
use crate::model::*;

pub const CARD_W: f32 = 168.0;
pub const CARD_COVER: f32 = 152.0;
pub const CARD_H: f32 = 226.0;
/// Aviso de una fila que seguro no puede sonar (`Track::is_playable`); se puede pulsar igual.
const UNPLAYABLE_HINT: &str = "Puede que no esté disponible en tu país o que se haya retirado de Spotify";

#[derive(Clone, Copy)]
pub enum Source<'a> {
    /// Reproducir dentro de un contexto (playlist o álbum) por uri.
    Context(&'a str),
    /// Reproducir la lista tal cual (uris sueltas).
    Tracks,
}

/// Pistas a la vista de una lista (ver `App::filter_tracks`): todas, prestadas, o las que
/// coinciden con la búsqueda interna, compartidas con lo guardado (sin copiarlas otra vez).
pub enum Shown<'t> {
    All(&'t [Track]),
    Filtered(std::rc::Rc<[Track]>),
}

impl std::ops::Deref for Shown<'_> {
    type Target = [Track];

    fn deref(&self) -> &[Track] {
        match self {
            Shown::All(t) => t,
            Shown::Filtered(t) => t,
        }
    }
}

#[derive(Clone, Copy)]
pub struct RowOpts<'a> {
    pub show_cover: bool,
    pub show_album: bool,
    pub numbered: bool,
    pub header: bool,
    pub source: Source<'a>,
    /// Playlist propia desde la que se pueden quitar pistas.
    pub editable_playlist: Option<&'a str>,
    /// Fila "rica": corazón siempre visible y botones de playlist/descargar/más al pasar el ratón.
    pub selectable: bool,
    /// Círculo de selección múltiple (solo en páginas con barra de selección).
    pub select: bool,
}

impl<'a> RowOpts<'a> {
    pub fn tracks(show_cover: bool, show_album: bool) -> Self {
        Self {
            show_cover,
            show_album,
            numbered: false,
            header: false,
            source: Source::Tracks,
            editable_playlist: None,
            selectable: true,
            select: false,
        }
    }

    pub fn target(&self, i: usize, tracks: &[Track], shuffle: bool) -> PlayTarget {
        match self.source {
            Source::Context(uri) => PlayTarget::Context {
                uri: uri.to_string(),
                track_uri: Some(tracks[i].uri.clone()),
                index: Some(i as u32),
                shuffle,
            },
            Source::Tracks => PlayTarget::Tracks {
                uris: tracks.iter().map(|t| t.uri.clone()).collect(),
                index: Some(i as u32),
                shuffle,
            },
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CardKind {
    Playlist,
    Album,
    Artist,
    Liked,
}

pub struct CardInfo<'a> {
    pub kind: CardKind,
    pub cover: Option<&'a str>,
    pub title: &'a str,
    pub subtitle: &'a str,
    pub count: Option<u32>,
    pub pinned: bool,
}

/// Fracciones del ancho del título y del subtítulo de la fila `i` de un esqueleto: distintas de
/// una fila a otra, como los nombres de verdad.
fn skeleton_widths(i: usize) -> (f32, f32) {
    const TITLE: [f32; 6] = [0.46, 0.32, 0.40, 0.27, 0.36, 0.42];
    const SUB: [f32; 6] = [0.24, 0.30, 0.18, 0.26, 0.21, 0.28];
    (TITLE[i % TITLE.len()], SUB[i % SUB.len()])
}

/// Sub-`Ui` sobre un rectángulo concreto con un layout dado.
pub fn child_in(ui: &mut egui::Ui, rect: Rect, layout: Layout) -> egui::Ui {
    ui.new_child(UiBuilder::new().max_rect(rect).layout(layout))
}

/// Sub-`Ui` sobre `rect` con un id propio del elemento y no de su posición: en las listas que
/// solo dibujan lo visible, el hover y el menú contextual siguen al elemento aunque el filtro,
/// el orden o el desplazamiento lo cambien de sitio.
pub fn keyed_child(ui: &mut egui::Ui, rect: Rect, key: impl egui::AsIdSalt) -> egui::Ui {
    let id = ui.id().with(key);
    ui.new_child(UiBuilder::new().id(id).max_rect(rect).layout(Layout::top_down(Align::Min)))
}

/// Degradado vertical (arriba → abajo) sobre un rectángulo.
pub fn vertical_gradient(painter: &egui::Painter, rect: Rect, top: Color32, bottom: Color32) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

impl App {
    /// Portada en un rectángulo dado (recorte centrado para cubrirlo).
    pub fn cover_in(&mut self, ui: &mut egui::Ui, url: Option<&str>, rect: Rect, radius: u8) {
        self.cover_in_corners(ui, url, rect, CornerRadius::same(radius));
    }

    /// Como `cover_in` con esquinas independientes. Entiende `spotify:image:<id>` y los
    /// mosaicos `spotify:mosaic:<id>:<id>:<id>:<id>` (cuatro portadas en cuadrícula).
    pub fn cover_in_corners(&mut self, ui: &mut egui::Ui, url: Option<&str>, rect: Rect, corners: CornerRadius) {
        // Fuera de la zona visible (estanterías desplazadas, secciones bajo el pliegue) no se
        // pide ni se pinta nada: así las texturas residentes son solo las que se ven.
        if !ui.is_rect_visible(rect) {
            return;
        }
        if let Some(u) = url {
            if let Some(rest) = u.strip_prefix("spotify:mosaic:") {
                let ids: Vec<String> = rest.split(':').map(|s| format!("https://i.scdn.co/image/{s}")).collect();
                if ids.len() >= 4 {
                    let (w, h) = (rect.width() / 2.0, rect.height() / 2.0);
                    let cells = [
                        (Rect::from_min_size(rect.min, vec2(w, h)), CornerRadius { nw: corners.nw, ..CornerRadius::ZERO }),
                        (Rect::from_min_size(pos2(rect.min.x + w, rect.min.y), vec2(w, h)), CornerRadius { ne: corners.ne, ..CornerRadius::ZERO }),
                        (Rect::from_min_size(pos2(rect.min.x, rect.min.y + h), vec2(w, h)), CornerRadius { sw: corners.sw, ..CornerRadius::ZERO }),
                        (Rect::from_min_size(pos2(rect.min.x + w, rect.min.y + h), vec2(w, h)), CornerRadius { se: corners.se, ..CornerRadius::ZERO }),
                    ];
                    for (i, (r, c)) in cells.into_iter().enumerate() {
                        self.cover_in_corners(ui, Some(&ids[i]), r, c);
                    }
                    return;
                }
            }
            if let Some(id) = u.strip_prefix("spotify:image:") {
                let http = format!("https://i.scdn.co/image/{id}");
                return self.cover_in_corners(ui, Some(&http), rect, corners);
            }
        }
        let ppp = ui.ctx().pixels_per_point();
        let px = (rect.width().max(rect.height()) * ppp).round() as u32;
        let (pw, ph) = ((rect.width() * ppp).round() as u32, (rect.height() * ppp).round() as u32);
        // Cabeceras grandes: textura ya recortada a su tamaño exacto (se pinta 1:1).
        let big = pw as u64 * ph as u64 > 160_000;
        let tex = match url {
            // Tope de 1280 px de ancho: en pantallas 4K la cabecera no debe costar 8 MB.
            Some(u) if big => {
                let k = (1280.0 / pw as f32).min(1.0);
                self.images.texture_fit(u, ((pw as f32 * k) as u32).max(1), ((ph as f32 * k) as u32).max(1))
            }
            Some(u) => self.images.texture(u, px.clamp(16, 1024)),
            None => None,
        };
        match tex {
            Some(t) => {
                let (tw, th) = (t.size.x.max(1.0), t.size.y.max(1.0));
                let target = rect.width() / rect.height().max(1.0);
                let src = tw / th;
                let uv = if src > target {
                    let keep = target / src;
                    let dx = (1.0 - keep) / 2.0;
                    Rect::from_min_max(pos2(dx, 0.0), pos2(1.0 - dx, 1.0))
                } else {
                    let keep = src / target;
                    let dy = (1.0 - keep) / 2.0;
                    Rect::from_min_max(pos2(0.0, dy), pos2(1.0, 1.0 - dy))
                };
                egui::Image::from_texture(t)
                    .uv(uv)
                    .maintain_aspect_ratio(false)
                    .corner_radius(corners)
                    .paint_at(ui, rect);
            }
            None => {
                let p = theme::palette(ui.ctx());
                ui.painter().rect_filled(rect, corners, p.card2);
            }
        }
    }

    pub fn cover(&mut self, ui: &mut egui::Ui, url: Option<&str>, size: f32, round: bool) {
        let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
        let radius = if round { (size / 2.0).min(255.0) as u8 } else { 6 };
        self.cover_in(ui, url, rect, radius);
    }

    /// Nombres separados por comas; los que tienen id son enlaces a una página.
    pub fn name_links<'a>(
        &mut self,
        ui: &mut egui::Ui,
        items: impl Iterator<Item = (&'a str, Option<&'a str>)>,
        color: Color32,
        page: fn(String) -> Page,
    ) {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (i, (name, id)) in items.enumerate() {
            if i > 0 {
                ui.label(RichText::new(", ").small().color(color));
            }
            match id {
                Some(id) => {
                    let r = ui.add(
                        Label::new(RichText::new(name).small().color(color)).sense(Sense::click()),
                    );
                    if r.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if r.clicked() {
                        self.actions.push(Action::Go(page(id.to_string())));
                    }
                }
                None => {
                    ui.label(RichText::new(name).small().color(color));
                }
            }
        }
    }

    /// Indicador de carga barato: tres puntos que avanzan a 4 fps.
    pub fn loading(ui: &mut egui::Ui, text: &str) {
        let phase = (ui.input(|i| i.time) * 4.0) as usize % 4;
        let dots = ["", ".", "..", "..."][phase];
        let weak = ui.visuals().weak_text_color();
        ui.label(RichText::new(format!("{text}{dots}")).color(weak));
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
    }

    /// Filas de esqueleto mientras llega una lista que aún no tiene copia: el hueco con la forma
    /// de lo que va a salir (portada y una o dos líneas de texto), en vez de un «Cargando» que
    /// salta al llegar. Quietas: sin animación no obligan a repintar (todo se dibuja en la CPU).
    /// `cover`: lado de la portada (0, sin ella); `indent`: sangría por la izquierda.
    pub fn skeleton_rows(ui: &mut egui::Ui, rows: usize, row_h: f32, cover: f32, indent: f32) {
        let p = theme::palette(ui.ctx());
        let w = ui.available_width();
        for i in 0..rows {
            let (rect, _) = ui.allocate_exact_size(vec2(w, row_h), Sense::hover());
            if !ui.is_rect_visible(rect) {
                continue;
            }
            let mut x = rect.min.x + 8.0 + indent;
            if cover > 0.0 {
                let c = Rect::from_min_size(pos2(x, rect.center().y - cover / 2.0), vec2(cover, cover));
                ui.painter().rect_filled(c, CornerRadius::same(if cover > 24.0 { 6 } else { 4 }), p.hover);
                x = c.max.x + 12.0;
            }
            let text_w = (rect.max.x - 8.0 - x).max(0.0);
            let (title, sub) = skeleton_widths(i);
            let painter = ui.painter();
            if row_h >= 44.0 {
                let y = rect.center().y;
                painter.rect_filled(Rect::from_min_size(pos2(x, y - 11.0), vec2(text_w * title, 10.0)), CornerRadius::same(5), p.hover);
                painter.rect_filled(Rect::from_min_size(pos2(x, y + 4.0), vec2(text_w * sub, 8.0)), CornerRadius::same(4), p.card2);
            } else {
                painter.rect_filled(Rect::from_center_size(pos2(x + text_w * title / 2.0, rect.center().y), vec2(text_w * title, 9.0)), CornerRadius::same(4), p.hover);
            }
        }
    }

    /// Tarjetas de esqueleto (la cuadrícula de la biblioteca o de álbumes sin copia), como
    /// `skeleton_rows`.
    pub fn skeleton_cards(ui: &mut egui::Ui, n: usize) {
        let p = theme::palette(ui.ctx());
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 8.0);
            for i in 0..n {
                let (rect, _) = ui.allocate_exact_size(vec2(CARD_W, CARD_H), Sense::hover());
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                let pad = (CARD_W - CARD_COVER) / 2.0;
                let cover = Rect::from_min_size(pos2(rect.min.x + pad, rect.min.y + pad + 8.0), vec2(CARD_COVER, CARD_COVER));
                let (title, sub) = skeleton_widths(i);
                let painter = ui.painter();
                painter.rect_filled(cover, CornerRadius::same(8), p.hover);
                let y = cover.max.y + 14.0;
                painter.rect_filled(Rect::from_min_size(pos2(cover.min.x, y), vec2(CARD_COVER * (title + 0.2), 10.0)), CornerRadius::same(5), p.hover);
                painter.rect_filled(Rect::from_min_size(pos2(cover.min.x, y + 18.0), vec2(CARD_COVER * (sub + 0.2), 8.0)), CornerRadius::same(4), p.card2);
            }
        });
    }

    pub fn section_title(ui: &mut egui::Ui, text: &str) {
        ui.add_space(14.0);
        ui.label(RichText::new(text).font(theme::bold(20.0)));
        ui.add_space(6.0);
    }

    /// Chip / pestaña redondeada.
    pub fn pill(ui: &mut egui::Ui, text: &str, selected: bool) -> egui::Response {
        let p = theme::palette(ui.ctx());
        let measure = ui.painter().layout_no_wrap(text.to_string(), theme::regular(13.0), p.text);
        let size = vec2(measure.size().x + 24.0, 28.0);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        let (fill, color) = if selected {
            (p.text, p.card)
        } else if resp.hovered() {
            (p.hover.lerp_to_gamma(p.text, 0.08), p.text)
        } else {
            (p.hover, p.text)
        };
        ui.painter().rect_filled(rect, CornerRadius::same(14), fill);
        let galley = ui.painter().layout_no_wrap(text.to_string(), theme::regular(13.0), color);
        let pos = rect.center() - galley.size() / 2.0;
        ui.painter().galley(pos, galley, color);
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        resp
    }

    /// Tarjeta de biblioteca. La forma distingue el tipo: playlist con "pila" arriba, álbum
    /// cuadrado, artista redondo, Me gusta verde. Pin y contador opcionales.
    pub fn card(&mut self, ui: &mut egui::Ui, info: CardInfo) -> egui::Response {
        let p = theme::palette(ui.ctx());
        let (rect, resp) = ui.allocate_exact_size(vec2(CARD_W, CARD_H), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(12), p.hover);
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let pad = (CARD_W - CARD_COVER) / 2.0;
        let cover_top = rect.min.y + pad + 8.0;
        let cover = Rect::from_min_size(pos2(rect.min.x + pad, cover_top), vec2(CARD_COVER, CARD_COVER));
        match info.kind {
            CardKind::Playlist => {
                // Dos hojas detrás, cada vez más estrechas: "pila de canciones".
                let base = p.card2.lerp_to_gamma(p.text, 0.18);
                ui.painter().rect_filled(
                    Rect::from_min_max(pos2(cover.min.x + 16.0, cover.min.y - 8.0), pos2(cover.max.x - 16.0, cover.min.y + 4.0)),
                    CornerRadius::same(4),
                    base.gamma_multiply(0.7),
                );
                ui.painter().rect_filled(
                    Rect::from_min_max(pos2(cover.min.x + 8.0, cover.min.y - 4.0), pos2(cover.max.x - 8.0, cover.min.y + 4.0)),
                    CornerRadius::same(4),
                    base,
                );
                self.cover_in(ui, info.cover, cover, 8);
                if info.cover.is_none() {
                    icons::paint(ui.painter(), cover.shrink(CARD_COVER * 0.3), p.faint, Icon::Playlist);
                }
            }
            CardKind::Album => {
                self.cover_in(ui, info.cover, cover, 8);
                if info.cover.is_none() {
                    icons::paint(ui.painter(), cover.shrink(CARD_COVER * 0.3), p.faint, Icon::Album);
                }
            }
            CardKind::Artist => {
                self.cover_in(ui, info.cover, cover, (CARD_COVER / 2.0) as u8);
                if info.cover.is_none() {
                    icons::paint(ui.painter(), cover.shrink(CARD_COVER * 0.3), p.faint, Icon::Artist);
                }
            }
            CardKind::Liked => {
                ui.painter().rect_filled(cover, CornerRadius::same(8), theme::GREEN_DARK);
                icons::paint(ui.painter(), cover.shrink(CARD_COVER * 0.28), GREEN, Icon::HeartFilled);
            }
        }
        if info.pinned {
            let c = pos2(cover.max.x - 14.0, cover.min.y + 14.0);
            ui.painter().circle_filled(c, 14.0, p.card.gamma_multiply(0.95));
            icons::paint(ui.painter(), Rect::from_center_size(c, vec2(16.0, 16.0)), GREEN, Icon::PinFilled);
        }
        // Textos
        let count_w = info.count.map(|_| 36.0).unwrap_or(0.0);
        let title_rect = Rect::from_min_size(pos2(cover.min.x, cover.max.y + 10.0), vec2(CARD_COVER - count_w, 20.0));
        let mut t = child_in(ui, title_rect, Layout::left_to_right(Align::Center));
        t.add(Label::new(RichText::new(info.title).font(theme::regular(14.0)).color(p.text)).truncate());
        if let Some(n) = info.count {
            let count_rect = Rect::from_min_size(pos2(title_rect.max.x, title_rect.min.y), vec2(count_w, 20.0));
            let mut k = child_in(ui, count_rect, Layout::right_to_left(Align::Center));
            let color = if info.kind == CardKind::Liked { GREEN } else { p.weak };
            k.label(RichText::new(n.to_string()).small().color(color));
        }
        let sub_rect = Rect::from_min_size(pos2(cover.min.x, title_rect.max.y + 2.0), vec2(CARD_COVER, 36.0));
        let mut s = child_in(ui, sub_rect, Layout::top_down(Align::Min));
        s.set_width(CARD_COVER);
        s.add(Label::new(RichText::new(info.subtitle).small().color(p.weak)).truncate());
        resp
    }

    /// Botón "Seguir / Siguiendo" para artistas y usuarios.
    pub fn follow_button(&mut self, ui: &mut egui::Ui, kind: &'static str, id: &str) {
        let key = format!("{kind}:{id}");
        let state = self.following.get(&key).copied().or({
            if kind == "artist" && self.artists_loaded {
                Some(false)
            } else {
                None
            }
        });
        match state {
            Some(true) => {
                if Self::pill(ui, "Siguiendo", true).on_hover_text("Dejar de seguir").clicked() {
                    self.actions.push(Action::Follow {
                        kind,
                        id: id.to_string(),
                        on: false,
                    });
                }
            }
            Some(false) => {
                if Self::pill(ui, "Seguir", false).clicked() {
                    self.actions.push(Action::Follow {
                        kind,
                        id: id.to_string(),
                        on: true,
                    });
                }
            }
            None => {
                ui.add_enabled(false, egui::Button::new("Seguir"));
            }
        }
    }

    /// Fila de acciones de una colección: play redondo, aleatorio y acciones extra.
    pub fn action_row(
        &mut self,
        ui: &mut egui::Ui,
        play: PlayTarget,
        shuffle: PlayTarget,
        extra: impl FnOnce(&mut Self, &mut egui::Ui),
    ) {
        let p = theme::palette(ui.ctx());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            if icons::round_button(ui, Icon::Play, 44.0, GREEN, Color32::BLACK)
                .on_hover_text("Reproducir")
                .clicked()
            {
                self.actions.push(Action::Play(play));
            }
            if icons::button(ui, Icon::Shuffle, 34.0, p.weak)
                .on_hover_text("Aleatorio")
                .clicked()
            {
                self.actions.push(Action::Play(shuffle));
            }
            extra(self, ui);
        });
    }

    /// Texto de la búsqueda interna si pertenece a esta lista.
    pub fn list_query(&self, list_id: &str) -> String {
        if self.list_search_id == list_id {
            self.album_search.trim().to_lowercase()
        } else {
            String::new()
        }
    }

    /// Pistas que coinciden con la búsqueda interna; sin búsqueda devuelve la lista prestada
    /// (sin copiar cientos de pistas en cada fotograma). Con la versión de la lista (`gen`, las
    /// de `lists`) el resultado se guarda y se reutiliza mientras no cambien la consulta ni sus
    /// pistas: si no, cada fotograma pasaba a minúsculas y copiaba miles de pistas. Sin versión
    /// (álbumes, historial, listas cortas) se filtra cada vez.
    pub fn filter_tracks<'t>(&mut self, list_id: &str, gen: Option<u64>, tracks: &'t [Track]) -> Shown<'t> {
        let q = self.list_query(list_id);
        if q.is_empty() {
            // Búsqueda borrada: su resultado (copia de miles de pistas, quizá) no se guarda más.
            if self.filter_cache.as_ref().is_some_and(|c| c.0 == list_id) {
                self.filter_cache = None;
            }
            return Shown::All(tracks);
        }
        if let (Some(gen), Some((id, cq, cgen, len, hit))) = (gen, &self.filter_cache) {
            if id == list_id && *cq == q && *cgen == gen && *len == tracks.len() {
                return Shown::Filtered(hit.clone());
            }
        }
        let found: std::rc::Rc<[Track]> = tracks
            .iter()
            .filter(|t| {
                t.name.to_lowercase().contains(&q)
                    || t.artists.iter().any(|a| a.name.to_lowercase().contains(&q))
                    || t.album.as_ref().map(|a| a.name.to_lowercase().contains(&q)).unwrap_or(false)
            })
            .cloned()
            .collect();
        if let Some(gen) = gen {
            self.filter_cache = Some((list_id.to_string(), q, gen, tracks.len(), found.clone()));
        }
        Shown::Filtered(found)
    }

    /// Barra de acciones de una colección de pistas, copia de la referencia de diseño: el play
    /// verde de 44 px y, cada 54,5 px, aleatorio, `extra` (si la página pone un botón propio:
    /// devuelve si lo puso), añadir a la cola, descargar, compartir y «Más»; la lupa a la
    /// derecha. Con selección activa se convierte en la barra verde de selección. `context` es el
    /// uri a reproducir como contexto (si no, las pistas sueltas); `menu` rellena «Más».
    #[allow(clippy::too_many_arguments)]
    pub fn collection_bar(
        &mut self,
        ui: &mut egui::Ui,
        list_id: &str,
        all: &[Track],
        context: Option<&str>,
        link: Option<&str>,
        extra: impl FnOnce(&mut Self, &mut egui::Ui, egui::Pos2) -> bool,
        menu: Option<Box<dyn FnOnce(&mut Self, &mut egui::Ui) + '_>>,
    ) {
        if self.list_search_id != list_id {
            self.list_search_id = list_id.to_string();
            self.album_search.clear();
            self.album_search_open = false;
        }
        if self.sel_list == list_id && !self.sel.is_empty() {
            self.selection_bar(ui, list_id, all);
            return;
        }
        let p = theme::palette(ui.ctx());
        let ink = theme::ink(&p);
        // Ids y uris se recorren o se copian solo al usarse: copiarlos en cada fotograma costaba
        // con listas de miles de pistas.
        let ids = || all.iter().filter_map(|t| t.id.as_ref());
        let target = |shuffle: bool| match context {
            Some(uri) => PlayTarget::Context { uri: uri.to_string(), track_uri: None, index: None, shuffle },
            None => PlayTarget::Tracks {
                uris: all.iter().map(|t| t.uri.clone()).collect(),
                index: if shuffle { None } else { Some(0) },
                shuffle,
            },
        };
        let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), BAR_BUTTONS_H), Sense::hover());
        let cy = bar.center().y;
        let id = ui.id().with(("collection_bar", list_id));
        let play = Rect::from_center_size(pos2(bar.min.x + 21.0, cy), vec2(44.0, 44.0));
        let mut c = child_in(ui, play, Layout::left_to_right(Align::Center));
        if icons::round_button(&mut c, Icon::Play, 44.0, GREEN, Color32::BLACK).on_hover_text("Reproducir").clicked() && !all.is_empty() {
            self.actions.push(Action::Play(target(false)));
        }
        // Huecos de los botones, de izquierda a derecha.
        let slot = |k: usize| pos2(bar.min.x + 75.5 + 54.5 * k as f32, cy);
        if Self::slot_button(ui, id.with("shuffle"), slot(0) - vec2(0.5, 0.0), Icon::Shuffle, 30.0, ink.dim).on_hover_text("Aleatorio").clicked() && !all.is_empty() {
            self.actions.push(Action::Play(target(true)));
        }
        let mut k = if extra(self, ui, slot(1)) { 2 } else { 1 };
        let mut next = || {
            k += 1;
            slot(k - 1)
        };
        if Self::slot_button(ui, id.with("queue"), next(), Icon::QueueList, 28.0, ink.dim).on_hover_text("Añadir a la cola").clicked() {
            for t in all {
                self.actions.push(Action::AddToQueue(t.uri.clone()));
            }
        }
        // Sin guardar: lo descargado cambia por su cuenta, aparte de la lista.
        let all_dl = ids().next().is_some() && ids().all(|i| self.downloaded.contains(i));
        let busy = !self.downloading.is_empty() && ids().any(|i| self.downloading.contains(i));
        let (icon, color, tip) = if all_dl {
            (Icon::Download, GREEN, "Descargado (en la caché de audio)")
        } else if busy {
            (Icon::Hourglass, ink.dim, "Descargando…")
        } else {
            (Icon::Download, ink.dim, "Descargar")
        };
        if Self::slot_button(ui, id.with("download"), next(), icon, 24.0, color).on_hover_text(tip).clicked() && !all_dl && !busy {
            let ids: Vec<String> = ids().cloned().collect();
            if all.first().and_then(|t| t.kind.as_deref()) == Some("episode") {
                self.actions.push(Action::DownloadEpisodes(ids));
            } else {
                self.actions.push(Action::Download(ids));
            }
        }
        if let Some(link) = link {
            if Self::slot_button(ui, id.with("share"), next(), Icon::Share, 29.0, ink.dim).on_hover_text("Copiar enlace").clicked() {
                self.actions.push(Action::CopyText(link.to_string(), "Enlace"));
            }
        }
        if let Some(menu) = menu {
            let more = Self::slot_button(ui, id.with("more"), next() - vec2(1.0, 0.0), Icon::More, 29.0, ink.dim).on_hover_text("Más");
            egui::Popup::menu(&more).show(|ui| {
                ui.set_min_width(200.0);
                menu(self, ui);
            });
        }
        // Lupa a la derecha; el campo de búsqueda se abre a su izquierda.
        let lupa = pos2(bar.max.x - 23.0, cy - 2.0);
        let color = if self.album_search_open { GREEN } else { ink.dim };
        if Self::slot_button(ui, id.with("search"), lupa, Icon::Search, 31.0, color).on_hover_text("Buscar en esta lista").clicked() {
            self.album_search_open = !self.album_search_open;
            self.album_search_focus = self.album_search_open;
            if !self.album_search_open {
                self.album_search.clear();
            }
        }
        if self.album_search_open {
            let field = Rect::from_min_max(pos2((lupa.x - 250.0).max(bar.min.x + 360.0), cy - 15.0), pos2(lupa.x - 24.0, cy + 15.0));
            let mut f = child_in(ui, field, Layout::right_to_left(Align::Center));
            let r = f.add(egui::TextEdit::singleline(&mut self.album_search).hint_text("Buscar en esta lista").desired_width(field.width()));
            if self.album_search_focus {
                self.album_search_focus = false;
                r.request_focus();
            }
            if r.has_focus() && f.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.album_search_open = false;
                self.album_search.clear();
            }
        }
    }

    /// Botón de icono en un sitio fijo: el icono de `px` píxeles centrado en `c` (sin fondo,
    /// como en la referencia) y una zona de clic de 40 px. Al pasar el ratón se aclara.
    pub fn slot_button(ui: &mut egui::Ui, id: egui::Id, c: egui::Pos2, icon: Icon, px: f32, color: Color32) -> egui::Response {
        let resp = ui.interact(Rect::from_center_size(c, vec2(40.0, 40.0)), id, Sense::click());
        let color = if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            ui.visuals().strong_text_color().lerp_to_gamma(color, 0.15)
        } else {
            color
        };
        icons::paint(ui.painter(), Rect::from_center_size(c, vec2(px, px)), color, icon);
        resp
    }

    /// Barra de acciones sobre la selección múltiple (sustituye a la fila de reproducir).
    pub fn selection_bar(&mut self, ui: &mut egui::Ui, list_id: &str, all: &[Track]) {
        let p = theme::palette(ui.ctx());
        let sel: Vec<Track> = all.iter().filter(|t| self.sel.contains(&t.uri)).cloned().collect();
        let ids: Vec<String> = sel.iter().filter_map(|t| t.id.clone()).collect();
        let uris: Vec<String> = sel.iter().map(|t| t.uri.clone()).collect();
        let all_liked = !ids.is_empty() && ids.iter().all(|i| self.liked_set.contains(i));
        egui::Frame::new()
            .fill(GREEN.gamma_multiply(0.14))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(egui::Margin::symmetric(10, 5))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    let (icon, color) = if all_liked { (Icon::HeartFilled, GREEN) } else { (Icon::Heart, p.text) };
                    if icons::button(ui, icon, 34.0, color)
                        .on_hover_text(if all_liked { "Quitar de Me gusta" } else { "Añadir a Me gusta" })
                        .clicked()
                    {
                        for i in &ids {
                            if self.liked_set.contains(i) == all_liked {
                                self.actions.push(Action::Like(i.clone(), !all_liked));
                            }
                        }
                    }
                    if icons::button(ui, Icon::PlusSquare, 34.0, p.text).on_hover_text("Añadir a una playlist").clicked() {
                        self.open_add_dialog(uris.clone());
                    }
                    if icons::button(ui, Icon::Queue, 34.0, p.text).on_hover_text("Añadir a la cola").clicked() {
                        for u in &uris {
                            self.actions.push(Action::AddToQueue(u.clone()));
                        }
                    }
                    if icons::button(ui, Icon::Download, 34.0, p.text).on_hover_text("Descargar").clicked() {
                        if sel.first().and_then(|t| t.kind.as_deref()) == Some("episode") {
                            self.actions.push(Action::DownloadEpisodes(ids.clone()));
                        } else {
                            self.actions.push(Action::Download(ids.clone()));
                        }
                    }
                    if icons::button(ui, Icon::Share, 34.0, p.text).on_hover_text("Copiar enlaces").clicked() {
                        let links: Vec<String> = uris.iter().map(|u| uri_to_link(u)).collect();
                        self.actions.push(Action::CopyText(links.join("\n"), "Enlaces"));
                    }
                    let more = icons::button(ui, Icon::More, 34.0, p.text).on_hover_text("Más");
                    egui::Popup::menu(&more).show(|ui| {
                        ui.set_min_width(200.0);
                        if Self::menu_item(ui, Some(Icon::Play), "Reproducir la selección", false).clicked() {
                            self.actions.push(Action::Play(PlayTarget::Tracks { uris: uris.clone(), index: Some(0), shuffle: false }));
                            ui.close();
                        }
                        if Self::menu_item(ui, Some(Icon::Check), "Seleccionar todo", false).clicked() {
                            self.sel_list = list_id.to_string();
                            self.sel = all.iter().map(|t| t.uri.clone()).collect();
                            ui.close();
                        }
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        if icons::round_button(ui, Icon::Minus, 26.0, GREEN, Color32::BLACK).on_hover_text("Deseleccionar todo").clicked() {
                            self.sel.clear();
                        }
                        ui.label(RichText::new(format!("{} seleccionadas", uris.len())).small().color(p.weak));
                    });
                });
            });
    }

    /// Fila de menú: icono blanco a la izquierda, texto blanco y, si es submenú, chevron.
    pub fn menu_item(ui: &mut egui::Ui, icon: Option<Icon>, text: &str, submenu: bool) -> egui::Response {
        Self::menu_item_enabled(ui, icon, text, submenu, true)
    }

    pub fn menu_item_enabled(ui: &mut egui::Ui, icon: Option<Icon>, text: &str, submenu: bool, enabled: bool) -> egui::Response {
        let p = theme::palette(ui.ctx());
        // Ancho fijo y compacto: dentro de un menú el ancho disponible sería el de la pantalla.
        const MENU_W: f32 = 250.0;
        ui.set_max_width(MENU_W);
        let w = ui.available_width().clamp(200.0, MENU_W);
        let (rect, resp) = ui.allocate_exact_size(vec2(w, 34.0), if enabled { Sense::click() } else { Sense::hover() });
        let color = if enabled { p.text } else { p.faint };
        if enabled && resp.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(6), p.hover);
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let mut x = rect.min.x + 10.0;
        if let Some(ic) = icon {
            icons::paint(ui.painter(), Rect::from_center_size(pos2(x + 9.0, rect.center().y), vec2(18.0, 18.0)), color, ic);
        }
        x += 32.0;
        let max_w = (rect.max.x - x - if submenu { 24.0 } else { 8.0 }).max(20.0);
        let galley = galley_truncated(ui.painter(), text, theme::regular(14.0), color, max_w);
        ui.painter().galley(pos2(x, rect.center().y - galley.size().y / 2.0), galley, color);
        if submenu {
            let c = pos2(rect.max.x - 14.0, rect.center().y);
            let s = 4.0;
            ui.painter().add(egui::Shape::line(
                vec![pos2(c.x - s / 2.0, c.y - s), pos2(c.x + s / 2.0, c.y), pos2(c.x - s / 2.0, c.y + s)],
                Stroke::new(1.5, p.weak),
            ));
        }
        resp
    }

    /// Interruptor (píldora con botón redondo; verde cuando está activo). Devuelve `true` si cambió.
    pub fn toggle(ui: &mut egui::Ui, on: &mut bool, text: &str) -> bool {
        Self::toggle_pad(ui, on, text, 10.0)
    }

    /// Interruptor con sangría izquierda del texto configurable.
    pub fn toggle_pad(ui: &mut egui::Ui, on: &mut bool, text: &str, pad: f32) -> bool {
        let p = theme::palette(ui.ctx());
        let w = ui.available_width().max(200.0);
        let (rect, resp) = ui.allocate_exact_size(vec2(w, 34.0), Sense::click());
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let mut changed = false;
        if resp.clicked() {
            *on = !*on;
            changed = true;
        }
        let t = ui.ctx().animate_bool_with_time(resp.id.with("toggle"), *on, 0.12);
        let galley = galley_truncated(ui.painter(), text, theme::regular(14.0), p.text, w - 60.0 - pad);
        ui.painter().galley(pos2(rect.min.x + pad, rect.center().y - galley.size().y / 2.0), galley, p.text);
        let track = Rect::from_center_size(pos2(rect.max.x - 26.0, rect.center().y), vec2(40.0, 22.0));
        let fill = p.hover.lerp_to_gamma(GREEN, t);
        ui.painter().rect_filled(track, CornerRadius::same(11), fill);
        let kx = track.min.x + 11.0 + 18.0 * t;
        ui.painter().circle_filled(pos2(kx, track.center().y), 8.0, if *on { Color32::BLACK } else { p.text });
        changed
    }

    /// Menú de cristal de la canción `t`, la fila `i` de `list_id` (`player_menu.rs`).
    fn song_menu_for(t: &Track, opts: &RowOpts, list_id: &str, i: usize, anchor: Anchor) -> SongMenu {
        SongMenu {
            track: t.clone(),
            // Las pistas de un álbum no traen su álbum: se toma del contexto de la página.
            album_id: match opts.source {
                Source::Context(u) => u.strip_prefix("spotify:album:").map(|s| s.to_string()),
                Source::Tracks => None,
            },
            remove_from: opts.editable_playlist.map(|s| s.to_string()),
            row: (list_id.to_string(), i),
            anchor,
            sub: None,
        }
    }

    pub fn track_rows(&mut self, ui: &mut egui::Ui, list_id: &str, tracks: &[Track], opts: RowOpts) {
        let n = tracks.len();
        if n == 0 {
            return;
        }
        // Playlists, álbumes y Me gusta: la tabla grande de la referencia.
        if opts.header && opts.selectable {
            self.track_table(ui, list_id, tracks, opts);
            return;
        }
        let p = theme::palette(ui.ctx());
        let width = ui.available_width();
        let show_album = opts.show_album && width > 620.0;

        // Columnas (en puntos): número | portada | título+artistas | álbum | acciones+duración
        let gap = 12.0;
        let pad = 12.0;
        // Paneles estrechos (cola lateral): sin número y con la columna derecha mínima.
        let narrow = width < 420.0 && !opts.selectable;
        let num_w = if narrow { 0.0 } else { 26.0 };
        let cover_w = if opts.show_cover { 40.0 } else { 0.0 };
        // En modo seleccionable la columna de duración va más a la izquierda (≈ 38 % del ancho).
        let right_w = if opts.selectable && show_album {
            (width * 0.30).max(230.0)
        } else if opts.selectable {
            (width * 0.38).max(230.0)
        } else if narrow {
            50.0 + 28.0 + 4.0
        } else {
            50.0 + 28.0 * 3.0 + 8.0
        };
        // Playlists con varios autores: columnas «Añadida por» y «Fecha» entre álbum y duración.
        // Si no hay sitio para el título, se ocultan por orden: fecha, «Añadida por» y álbum.
        let contrib_all = self.rows_added_by && show_album && opts.selectable;
        let mut show_date = contrib_all;
        let mut show_by = contrib_all;
        let mut show_album = show_album;
        let left = ui.cursor().min.x;
        let mut x0 = left + pad;
        let num_x = x0;
        x0 += if narrow { 0.0 } else { num_w + gap };
        let cover_x = x0;
        if opts.show_cover {
            x0 += cover_w + gap;
        }
        let title_x = x0;
        let right_x = left + width - pad - right_w;
        let (album_w, by_w, date_w, album_x, by_x, date_x, title_w) = loop {
            let contrib = show_by;
            let by_w = if show_by { 120.0 } else { 0.0 };
            let date_w = if show_date { 100.0 } else { 0.0 };
            let album_w = if show_album && contrib {
                (width * 0.18).clamp(90.0, 260.0)
            } else if show_album && opts.selectable {
                (width * 0.22).clamp(100.0, 320.0)
            } else if show_album {
                (width * 0.30).clamp(120.0, 380.0)
            } else {
                0.0
            };
            let date_x = if show_date { right_x - gap - date_w } else { right_x };
            let by_x = if show_by { date_x - gap - by_w } else { date_x };
            let album_x = if show_album { by_x - gap - album_w } else { by_x };
            let title_w = album_x - gap - title_x;
            let min_title = if show_date { 200.0 } else if show_by { 180.0 } else { 160.0 };
            if title_w >= min_title || (!show_date && !show_by && !show_album) {
                break (album_w, by_w, date_w, album_x, by_x, date_x, title_w.max(60.0));
            }
            if show_date {
                show_date = false;
            } else if show_by {
                show_by = false;
            } else {
                show_album = false;
            }
        };
        let contrib = show_by;

        if opts.header {
            let (hrect, _) = ui.allocate_exact_size(vec2(width, 30.0), Sense::hover());
            let y = hrect.center().y;
            let small = |ui: &mut egui::Ui, x: f32, text: &str, right: bool| {
                let galley = ui.painter().layout_no_wrap(text.to_string(), theme::regular(12.0), p.weak);
                let pos = if right {
                    pos2(x - galley.size().x, y - galley.size().y / 2.0)
                } else {
                    pos2(x, y - galley.size().y / 2.0)
                };
                ui.painter().galley(pos, galley, p.weak);
            };
            {
                let galley = ui.painter().layout_no_wrap("#".to_string(), theme::regular(12.0), p.weak);
                let pos = pos2(num_x + num_w / 2.0 - galley.size().x / 2.0, y - galley.size().y / 2.0);
                ui.painter().galley(pos, galley, p.weak);
            }
            small(ui, title_x, "Título", false);
            if show_album {
                small(ui, album_x, "Álbum", false);
            }
            if contrib {
                small(ui, by_x, "Añadida por", false);
            }
            if show_date {
                small(ui, date_x, "Fecha", false);
            }
            if opts.selectable {
                small(ui, right_x, "Duración", false);
            } else {
                small(ui, right_x + right_w, "Duración", true);
            }
            ui.painter().line_segment(
                [pos2(hrect.min.x + pad, hrect.max.y - 1.0), pos2(hrect.max.x - pad, hrect.max.y - 1.0)],
                Stroke::new(1.0, p.border),
            );
            ui.add_space(4.0);
        }

        let (rect, _) = ui.allocate_exact_size(vec2(width, n as f32 * ROW_H), Sense::hover());
        let clip = ui.clip_rect();
        let first = ((clip.top() - rect.top()) / ROW_H).floor().max(0.0) as usize;
        let last = (((clip.bottom() - rect.top()) / ROW_H).ceil().max(0.0) as usize).min(n);
        if first >= last {
            return;
        }

        let now_uri = self.player.now.as_ref().map(|n| n.uri.clone());
        let shuffle = self.player.shuffle;

        for i in first..last {
            let t = &tracks[i];
            let y = rect.min.y + i as f32 * ROW_H;
            let row = Rect::from_min_size(pos2(rect.min.x, y), vec2(width, ROW_H));
            let id = ui.id().with((list_id, i));
            let resp = ui.interact(row, id, Sense::click());
            let selected = self
                .selected
                .as_ref()
                .map(|s| s.0 == list_id && s.1 == i)
                .unwrap_or(false);
            // `contains_pointer` y no solo `hovered`: los botones de la fila se superponen a ella y,
            // al pasar sobre uno, la fila dejaría de estar "hovered" y sus botones desaparecerían.
            // Con el menú «más» abierto la fila sigue "en hover": si no, al mover el ratón al
            // menú el botón desaparecería y el menú con él.
            let hov = resp.hovered() || resp.contains_pointer() || self.song_menu_on(list_id, i);
            if selected || hov {
                ui.painter().rect_filled(row.shrink2(vec2(4.0, 1.0)), CornerRadius::same(10), p.hover);
            }
            let is_now = now_uri.as_deref() == Some(t.uri.as_str());
            let y_range = y..=y + ROW_H;
            // Botón de reproducir de la fila apretado (aún sin soltar): la precarga inteligente la
            // prepara entera en ese rato.
            let mut pressing = false;

            // Número / indicador de reproducción
            if !narrow {
                let r = Rect::from_x_y_ranges(num_x..=num_x + num_w, y_range.clone());
                if is_now {
                    icons::paint(ui.painter(), Rect::from_center_size(r.center(), vec2(14.0, 14.0)), GREEN, Icon::Play);
                } else if hov {
                    // El número se convierte en un botón de reproducir verde.
                    let pr = ui.interact(Rect::from_center_size(r.center(), vec2(26.0, 26.0)), id.with("play"), Sense::click());
                    icons::paint(ui.painter(), Rect::from_center_size(r.center(), vec2(14.0, 14.0)), GREEN, Icon::Play);
                    if pr.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    pressing = pr.is_pointer_button_down_on();
                    if pr.clicked() {
                        self.actions.push(Action::Play(opts.target(i, tracks, shuffle)));
                    }
                } else {
                    let num = if opts.numbered {
                        t.track_number.unwrap_or(i as u32 + 1)
                    } else {
                        i as u32 + 1
                    };
                    ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, num.to_string(), theme::regular(14.0), p.weak);
                }
            }

            if opts.show_cover {
                let r = Rect::from_min_size(pos2(cover_x, y + (ROW_H - cover_w) / 2.0), vec2(cover_w, cover_w));
                let url = t.cover(64).map(|s| s.to_string());
                self.cover_in(ui, url.as_deref(), r, 6);
            }

            // Título y artistas
            {
                let r = Rect::from_x_y_ranges(title_x..=title_x + title_w, y_range.clone());
                let mut c = child_in(ui, r, Layout::top_down(Align::Min));
                c.set_width(title_w);
                c.spacing_mut().item_spacing.y = 2.0;
                c.add_space((ROW_H - 36.0) / 2.0);
                let hidden = t.id.as_ref().map(|i| self.hidden_tracks.contains(i)).unwrap_or(false);
                // Una que seguro no puede sonar se ve apagada (como una oculta), pero se puede
                // pulsar igual: es solo un aviso, quien decide es el reproductor.
                let unplayable = t.is_playable == Some(false);
                let color = if is_now { GREEN } else if hidden || unplayable { p.faint } else { p.text };
                c.add(Label::new(RichText::new(&t.name).font(theme::regular(14.0)).color(color)).truncate());
                if narrow {
                    let names = t.artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ");
                    c.add(Label::new(RichText::new(names).small().color(p.weak)).truncate());
                } else {
                    c.horizontal(|ui| {
                        if t.explicit {
                            ui.label(RichText::new("E ").small().strong().color(p.weak));
                        }
                        self.name_links(
                            ui,
                            t.artists.iter().map(|a| (a.name.as_str(), a.id.as_deref())),
                            p.weak,
                            Page::Artist,
                        );
                    });
                }
            }

            if show_album {
                let r = Rect::from_x_y_ranges(album_x..=album_x + album_w, y_range.clone());
                let mut c = child_in(ui, r, Layout::left_to_right(Align::Center));
                c.set_width(album_w);
                if let Some(a) = &t.album {
                    let l = c.add(
                        Label::new(RichText::new(&a.name).small().color(p.weak))
                            .truncate()
                            .sense(Sense::click()),
                    );
                    if l.hovered() {
                        c.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if l.clicked() {
                        if let Some(id) = &a.id {
                            self.actions.push(Action::Go(Page::Album(id.clone())));
                        }
                    }
                }
            }
            if contrib {
                // Añadida por (nombre visible, clic → perfil) y fecha.
                if let Some(user) = t.added_by.clone() {
                    let name = self.user_display(&user);
                    let r = Rect::from_x_y_ranges(by_x..=by_x + by_w, y_range.clone());
                    let mut c = child_in(ui, r, Layout::left_to_right(Align::Center));
                    c.set_width(by_w);
                    let l = c.add(Label::new(RichText::new(name).small().color(p.weak)).truncate().sense(Sense::click()));
                    if l.hovered() {
                        c.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if l.clicked() {
                        self.actions.push(Action::Go(Page::User(user)));
                    }
                }
                if let (true, Some(d)) = (show_date, &t.added_at) {
                    let r = Rect::from_x_y_ranges(date_x..=date_x + date_w, y_range.clone());
                    let g = ui.painter().layout_no_wrap(fmt_date(d), theme::regular(12.0), p.weak);
                    ui.painter().galley(pos2(r.min.x, r.center().y - g.size().y / 2.0), g, p.weak);
                }
            }

            // Duración, corazón y añadir a playlist (alineados a la derecha)
            if opts.selectable {
                let r = Rect::from_x_y_ranges(right_x..=right_x + right_w, y_range);
                let uri = t.uri.clone();
                let is_sel = self.sel_list == list_id && self.sel.contains(&uri);
                // Círculo de selección en el extremo derecho
                let circle = Rect::from_center_size(pos2(r.max.x - 14.0, r.center().y), vec2(28.0, 28.0));
                if opts.select && (hov || is_sel) {
                    let cr = ui.interact(circle, id.with("sel"), Sense::click());
                    if is_sel {
                        ui.painter().circle_filled(circle.center(), 9.0, GREEN);
                        icons::paint(ui.painter(), Rect::from_center_size(circle.center(), vec2(11.0, 11.0)), Color32::BLACK, Icon::Check);
                    } else {
                        let col = if cr.hovered() { p.text } else { p.weak };
                        ui.painter().circle_stroke(circle.center(), 9.0, Stroke::new(1.5, col));
                    }
                    if cr.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if cr.clicked() {
                        if self.sel_list != list_id {
                            self.sel.clear();
                            self.sel_list = list_id.to_string();
                        }
                        if is_sel {
                            self.sel.remove(&uri);
                        } else {
                            self.sel.insert(uri.clone());
                        }
                    }
                }
                let mut c = child_in(ui, Rect::from_min_max(r.min, pos2(circle.min.x - 6.0, r.max.y)), Layout::left_to_right(Align::Center));
                c.spacing_mut().item_spacing.x = 2.0;
                let g = c.painter().layout_no_wrap(fmt_ms(t.duration_ms), theme::regular(12.0), p.weak);
                let (drect, _) = c.allocate_exact_size(vec2(50.0, ROW_H), Sense::hover());
                c.painter().galley(pos2(drect.min.x, drect.center().y - g.size().y / 2.0), g, p.weak);
                if let Some(tid) = &t.id {
                    // El corazón se ve siempre: verde relleno con Me gusta, solo contorno si no.
                    let liked = self.liked_set.contains(tid);
                    let (icon, color) = if liked { (Icon::HeartFilled, GREEN) } else { (Icon::Heart, p.weak) };
                    if icons::button(&mut c, icon, 28.0, color).clicked() {
                        self.actions.push(Action::Like(tid.clone(), !liked));
                    }
                    if hov {
                        if icons::button(&mut c, Icon::PlusSquare, 28.0, p.weak).on_hover_text("Añadir a playlist").clicked() {
                            self.open_add_dialog(vec![uri.clone()]);
                        }
                        let dl = self.downloaded.contains(tid);
                        let busy = self.downloading.contains(tid);
                        let (icon, color, tip) = if dl {
                            (Icon::Download, GREEN, "Descargada (en la caché de audio)")
                        } else if busy {
                            (Icon::Hourglass, p.weak, "Descargando…")
                        } else {
                            (Icon::Download, p.weak, "Descargar")
                        };
                        if icons::button(&mut c, icon, 28.0, color).on_hover_text(tip).clicked() && !dl && !busy {
                            if t.kind.as_deref() == Some("episode") {
                                self.actions.push(Action::DownloadEpisodes(vec![tid.clone()]));
                            } else {
                                self.actions.push(Action::Download(vec![tid.clone()]));
                            }
                        }
                        let more = icons::button(&mut c, Icon::More, 28.0, p.weak).on_hover_text("Más");
                        if more.clicked() {
                            self.toggle_song_menu(Self::song_menu_for(t, &opts, list_id, i, Anchor::Dots(more.rect)));
                        }
                    }
                }
            } else {
                let r = Rect::from_x_y_ranges(right_x..=right_x + right_w, y_range);
                // Duración pegada a la derecha; a su izquierda, de izquierda a derecha: corazón
                // (siempre), añadir a playlist y quitar de la cola (al pasar el ratón).
                let g = ui.painter().layout_no_wrap(fmt_ms(t.duration_ms), theme::regular(12.0), p.weak);
                ui.painter().galley(pos2(r.max.x - g.size().x, r.center().y - g.size().y / 2.0), g, p.weak);
                // En paneles estrechos los botones de hover se superponen al final del título
                // (con el fondo de la fila debajo) para no robarle sitio al texto.
                let extra = if narrow && hov { 60.0 } else { 0.0 };
                let area = Rect::from_min_max(pos2(r.min.x - extra, r.min.y), pos2(r.max.x - 50.0, r.max.y));
                if extra > 0.0 {
                    ui.painter().rect_filled(area.shrink2(vec2(0.0, 6.0)), CornerRadius::same(6), p.hover);
                }
                let mut c = child_in(ui, area, Layout::left_to_right(Align::Center));
                c.spacing_mut().item_spacing.x = 2.0;
                if let Some(id) = &t.id {
                    let liked = self.liked_set.contains(id);
                    let (icon, color) = if liked { (Icon::HeartFilled, GREEN) } else { (Icon::Heart, p.weak) };
                    if icons::button(&mut c, icon, 28.0, color).clicked() {
                        self.actions.push(Action::Like(id.clone(), !liked));
                    }
                    if hov {
                        if icons::button(&mut c, Icon::PlusSquare, 28.0, p.weak).on_hover_text("Añadir a playlist").clicked() {
                            self.open_add_dialog(vec![t.uri.clone()]);
                        }
                        if opts.editable_playlist == Some("queue") {
                            if icons::button(&mut c, Icon::Close, 28.0, p.weak).on_hover_text("Quitar de la cola").clicked() {
                                self.actions.push(Action::RemoveFromPlaylist { playlist_id: "queue".into(), uri: t.uri.clone() });
                            }
                        }
                    }
                }
            }

            if hov {
                self.report_row_hover(&t.uri, pressing);
            }
            let resp = if t.is_playable == Some(false) {
                resp.on_hover_text(UNPLAYABLE_HINT)
            } else {
                resp
            };
            if resp.double_clicked() {
                if list_id.starts_with("queue") {
                    // Desde la cola: saltar dentro de lo que ya suena, sin cambiar el origen.
                    self.play_from_queue(&t.uri, tracks, i);
                } else {
                    self.actions.push(Action::Play(opts.target(i, tracks, shuffle)));
                }
            } else if resp.clicked() {
                self.selected = Some((list_id.to_string(), i));
            }
            if resp.secondary_clicked() {
                if let Some(at) = resp.interact_pointer_pos() {
                    self.song_more = Some(Self::song_menu_for(t, &opts, list_id, i, Anchor::Pointer(at)));
                }
            }
        }
    }
}

impl App {
    /// Tabla de canciones de playlists, álbumes y Me gusta, copia de la referencia de diseño:
    /// cabecera con separador y filas de 72 px con número, portada de 50 px, título y artistas,
    /// álbum, duración y corazón. Al pasar el ratón, el número pasa a ser «reproducir» y salen
    /// añadir a playlist, descargar, «Más» y el círculo de selección.
    fn track_table(&mut self, ui: &mut egui::Ui, list_id: &str, tracks: &[Track], opts: RowOpts) {
        let n = tracks.len();
        let p = theme::palette(ui.ctx());
        let ink = theme::ink(&p);
        let w = ui.available_width();
        let left = ui.cursor().min.x;
        let cols = table_cols(w, opts.show_cover, opts.show_album, self.rows_added_by);
        let small = theme::regular(TABLE_SMALL_FONT);

        // Cabecera: textos con la línea base a 44 px y el separador a 59.
        let (head, _) = ui.allocate_exact_size(vec2(w, TABLE_HEAD_H), Sense::hover());
        {
            let base = head.min.y + 44.0;
            let painter = ui.painter();
            let g = painter.layout_no_wrap("#".into(), small.clone(), ink.dim);
            let gx = left + cols.num_c - g.size().x / 2.0;
            text_on_baseline(painter, pos2(gx, base), g, ink.dim);
            let put = |x: f32, text: &str| {
                let g = painter.layout_no_wrap(text.into(), small.clone(), ink.dim);
                text_on_baseline(painter, pos2(left + x, base), g, ink.dim);
            };
            put(TABLE_COVER_X, "Título");
            if let Some(x) = cols.album_x {
                put(x, "Álbum");
            }
            if let Some(x) = cols.by_x {
                put(x, "Añadida por");
            }
            if let Some(x) = cols.date_x {
                put(x, "Fecha");
            }
            put(cols.dur_x, "Duración");
            let y = head.min.y + 59.3;
            painter.line_segment([pos2(left - 1.0, y), pos2(left + w, y)], Stroke::new(2.2, ink.line));
        }

        let (rect, _) = ui.allocate_exact_size(vec2(w, n as f32 * TABLE_ROW_H), Sense::hover());
        let clip = ui.clip_rect();
        let first = ((clip.top() - rect.top()) / TABLE_ROW_H).floor().max(0.0) as usize;
        let last = (((clip.bottom() - rect.top()) / TABLE_ROW_H).ceil().max(0.0) as usize).min(n);
        if first >= last {
            return;
        }
        let now_uri = self.player.now.as_ref().map(|n| n.uri.clone());
        let shuffle = self.player.shuffle;
        let title_font = theme::regular(TABLE_TITLE_FONT);
        let num_font = theme::regular(TABLE_NUM_FONT);

        for i in first..last {
            let t = &tracks[i];
            let y = rect.min.y + i as f32 * TABLE_ROW_H;
            let row = Rect::from_min_size(pos2(left, y), vec2(w, TABLE_ROW_H));
            let id = ui.id().with((list_id, i));
            let resp = ui.interact(row, id, Sense::click());
            let selected = self.selected.as_ref().is_some_and(|s| s.0 == list_id && s.1 == i);
            // Como en `track_rows`: los botones de la fila y su menú abierto la mantienen «en hover».
            let more_id = ui.id().with(("more", list_id, i));
            // Pedido por el modo de control: como si se pulsaran sus tres puntos.
            let want_menu = self.song_more_req.as_ref().is_some_and(|(l, k, _)| *k == i && l.as_deref().is_none_or(|l| l == list_id));
            let hov = resp.hovered() || resp.contains_pointer() || self.song_menu_on(list_id, i) || want_menu;
            if selected || hov {
                ui.painter().rect_filled(row.expand2(vec2(8.0, 0.0)).shrink2(vec2(0.0, 2.0)), CornerRadius::same(6), ink.hover);
            }
            let is_now = now_uri.as_deref() == Some(t.uri.as_str());
            let cy = y + TABLE_ROW_H / 2.0;
            let mut pressing = false;

            // Número, o reproducir al pasar el ratón (verde: la que suena)
            let num_c = pos2(left + cols.num_c, cy);
            if is_now || hov {
                let color = if is_now { GREEN } else { ink.strong };
                icons::paint(ui.painter(), Rect::from_center_size(num_c, vec2(16.0, 16.0)), color, Icon::Play);
                if hov {
                    let pr = ui.interact(Rect::from_center_size(num_c, vec2(30.0, 30.0)), id.with("play"), Sense::click());
                    if pr.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    pressing = pr.is_pointer_button_down_on();
                    if pr.clicked() {
                        self.actions.push(Action::Play(opts.target(i, tracks, shuffle)));
                    }
                }
            } else {
                let num = if opts.numbered { t.track_number.unwrap_or(i as u32 + 1) } else { i as u32 + 1 };
                let g = ui.painter().layout_no_wrap(num.to_string(), num_font.clone(), ink.dim);
                let gx = num_c.x - g.size().x / 2.0;
                text_on_baseline(ui.painter(), pos2(gx, y + 42.0), g, ink.dim);
            }

            if opts.show_cover {
                let r = Rect::from_min_size(pos2(left + TABLE_COVER_X, y + 11.0), vec2(TABLE_COVER, TABLE_COVER));
                let url = t.cover(64).map(|s| s.to_string());
                self.cover_in(ui, url.as_deref(), r, 4);
            }

            // Título y, debajo, «E» si es explícita y los artistas (enlaces).
            {
                let hidden = t.id.as_ref().is_some_and(|i| self.hidden_tracks.contains(i));
                // Una que seguro no puede sonar se ve apagada (como una oculta), pero se puede
                // pulsar igual: es solo un aviso, quien decide es el reproductor.
                let unplayable = t.is_playable == Some(false);
                let color = if is_now { GREEN } else if hidden || unplayable { ink.dim } else { ink.strong };
                let x = left + cols.title_x;
                let g = galley_truncated(ui.painter(), &t.name, title_font.clone(), color, cols.title_w);
                text_on_baseline(ui.painter(), pos2(x, y + 30.0), g, color);
                let mut ax = x;
                if t.explicit {
                    let badge = Rect::from_min_size(pos2(x + 1.0, y + 43.0), vec2(15.0, 15.0));
                    ui.painter().rect_filled(badge, CornerRadius::same(2), ink.dim);
                    let g = ui.painter().layout_no_wrap("E".into(), theme::bold(10.5), Color32::BLACK);
                    ui.painter().galley(badge.center() - g.size() / 2.0, g, Color32::BLACK);
                    ax = badge.max.x + 6.0;
                }
                let artists: Vec<(&str, Option<&str>)> = t.artists.iter().map(|a| (a.name.as_str(), a.id.as_deref())).collect();
                self.links_on_baseline(ui, id.with("artists"), &artists, pos2(ax, y + 54.5), x + cols.title_w, small.clone(), ink.dim, ink.strong, Page::Artist);
            }

            if let (Some(ax), Some(a)) = (cols.album_x, &t.album) {
                let link = [(a.name.as_str(), a.id.as_deref())];
                self.links_on_baseline(ui, id.with("album"), &link, pos2(left + ax, y + 42.0), left + ax + cols.album_w, small.clone(), ink.dim, ink.strong, Page::Album);
            }
            if let (Some(bx), Some(user)) = (cols.by_x, t.added_by.clone()) {
                // Añadida por (nombre visible, clic → perfil).
                let name = self.user_display(&user);
                let link = [(name.as_str(), Some(user.as_str()))];
                self.links_on_baseline(ui, id.with("by"), &link, pos2(left + bx, y + 42.0), left + bx + cols.by_w, small.clone(), ink.dim, ink.strong, Page::User);
            }
            if let (Some(dx), Some(d)) = (cols.date_x, &t.added_at) {
                let g = galley_truncated(ui.painter(), &fmt_date(d), small.clone(), ink.dim, cols.date_w);
                text_on_baseline(ui.painter(), pos2(left + dx, y + 42.0), g, ink.dim);
            }

            let g = ui.painter().layout_no_wrap(fmt_ms(t.duration_ms), num_font.clone(), ink.dim);
            text_on_baseline(ui.painter(), pos2(left + cols.dur_x, y + 42.0), g, ink.dim);

            let uri = t.uri.clone();
            if let Some(tid) = &t.id {
                // El corazón se ve siempre: verde relleno con Me gusta, solo contorno si no.
                let liked = self.liked_set.contains(tid);
                let (icon, color) = if liked { (Icon::HeartFilled, GREEN) } else { (Icon::Heart, ink.dim) };
                let tip = if liked { "Quitar de Me gusta" } else { "Me gusta" };
                // El dibujo del corazón queda algo alto en su cuadro: en la referencia va centrado.
                if Self::slot_button(ui, id.with("like"), pos2(left + cols.heart_c - 1.0, cy + 1.5), icon, TABLE_HEART, color).on_hover_text(tip).clicked() {
                    self.actions.push(Action::Like(tid.clone(), !liked));
                }
                if hov {
                    // Detrás del corazón, lo que quepa antes del círculo de selección.
                    let room = left + w - 36.0;
                    let at = |k: f32| pos2(left + cols.heart_c + 46.0 * k, cy);
                    let fits = |k: f32| at(k).x + 16.0 <= room;
                    let n_fit = (1..=3).take_while(|&k| fits(k as f32)).count();
                    let mut k = 1.0;
                    if n_fit >= 1 {
                        // El mismo icono y tamaño que en el reproductor (al lado del corazón); el
                        // círculo con «+» es el de guardar en la biblioteca.
                        if Self::slot_button(ui, id.with("add"), at(k), Icon::PlusSquare, TABLE_HEART, ink.dim).on_hover_text("Añadir a playlist").clicked() {
                            self.open_add_dialog(vec![uri.clone()]);
                        }
                        k += 1.0;
                    }
                    if n_fit >= 3 {
                        let dl = self.downloaded.contains(tid);
                        let busy = self.downloading.contains(tid);
                        let (icon, color, tip) = if dl {
                            (Icon::Download, GREEN, "Descargada (en la caché de audio)")
                        } else if busy {
                            (Icon::Hourglass, ink.dim, "Descargando…")
                        } else {
                            (Icon::Download, ink.dim, "Descargar")
                        };
                        if Self::slot_button(ui, id.with("dl"), at(k), icon, 24.0, color).on_hover_text(tip).clicked() && !dl && !busy {
                            if t.kind.as_deref() == Some("episode") {
                                self.actions.push(Action::DownloadEpisodes(vec![tid.clone()]));
                            } else {
                                self.actions.push(Action::Download(vec![tid.clone()]));
                            }
                        }
                        k += 1.0;
                    }
                    if n_fit >= 2 {
                        let more = Self::slot_button(ui, more_id.with("btn"), at(k), Icon::More, 30.0, ink.dim).on_hover_text("Más");
                        if more.clicked() || want_menu {
                            let mut m = Self::song_menu_for(t, &opts, list_id, i, Anchor::Dots(more.rect));
                            if want_menu {
                                m.sub = self.song_more_req.take().and_then(|r| r.2);
                                self.song_more = Some(m);
                            } else {
                                self.toggle_song_menu(m);
                            }
                        }
                    }
                }
            }

            // Círculo de selección múltiple en el extremo derecho.
            let is_sel = self.sel_list == list_id && self.sel.contains(&uri);
            if opts.select && (hov || is_sel) {
                let circle = Rect::from_center_size(pos2(left + w - 16.0, cy), vec2(28.0, 28.0));
                let cr = ui.interact(circle, id.with("sel"), Sense::click());
                if is_sel {
                    ui.painter().circle_filled(circle.center(), 9.0, GREEN);
                    icons::paint(ui.painter(), Rect::from_center_size(circle.center(), vec2(11.0, 11.0)), Color32::BLACK, Icon::Check);
                } else {
                    let col = if cr.hovered() { ink.strong } else { ink.dim };
                    ui.painter().circle_stroke(circle.center(), 9.0, Stroke::new(1.5, col));
                }
                if cr.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if cr.clicked() {
                    if self.sel_list != list_id {
                        self.sel.clear();
                        self.sel_list = list_id.to_string();
                    }
                    if is_sel {
                        self.sel.remove(&uri);
                    } else {
                        self.sel.insert(uri.clone());
                    }
                }
            }

            if hov {
                self.report_row_hover(&t.uri, pressing);
            }
            let resp = if t.is_playable == Some(false) { resp.on_hover_text(UNPLAYABLE_HINT) } else { resp };
            if resp.double_clicked() {
                self.actions.push(Action::Play(opts.target(i, tracks, shuffle)));
            } else if resp.clicked() {
                self.selected = Some((list_id.to_string(), i));
            }
            if resp.secondary_clicked() {
                if let Some(at) = resp.interact_pointer_pos() {
                    self.song_more = Some(Self::song_menu_for(t, &opts, list_id, i, Anchor::Pointer(at)));
                }
            }
        }
    }

    /// Nombres separados por comas desde `at` (línea base), recortados en `max_x`; los que tienen
    /// id son enlaces a `page` (se aclaran y subrayan al pasar el ratón).
    #[allow(clippy::too_many_arguments)]
    fn links_on_baseline(
        &mut self,
        ui: &mut egui::Ui,
        id: egui::Id,
        items: &[(&str, Option<&str>)],
        at: egui::Pos2,
        max_x: f32,
        font: egui::FontId,
        color: Color32,
        hover: Color32,
        page: fn(String) -> Page,
    ) {
        let mut x = at.x;
        for (i, (name, link)) in items.iter().enumerate() {
            let last = i + 1 == items.len();
            let text = if last { name.to_string() } else { format!("{name},") };
            let room = max_x - x;
            if room < 12.0 {
                break;
            }
            let g = galley_truncated(ui.painter(), &text, font.clone(), color, room);
            let cut = g.size().x >= room - 0.5 || g.text() != text;
            let rect = Rect::from_min_size(pos2(x, at.y - font.size), vec2(g.size().x, font.size * 1.3));
            let resp = link.map(|_| ui.interact(rect, id.with(i), Sense::click()));
            let hovered = resp.as_ref().is_some_and(|r| r.hovered());
            let c = if hovered { hover } else { color };
            let drawn = text_on_baseline(ui.painter(), pos2(x, at.y), g, c);
            if hovered {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                let uy = at.y + 2.0;
                ui.painter().line_segment([pos2(drawn.min.x, uy), pos2(drawn.max.x - if last { 0.0 } else { 4.0 }, uy)], Stroke::new(1.0, c));
            }
            if resp.is_some_and(|r| r.clicked()) {
                if let Some(link) = link {
                    self.actions.push(Action::Go(page(link.to_string())));
                }
            }
            x = drawn.max.x + 4.0;
            if cut {
                break;
            }
        }
    }
}

/// Medidas de la cabecera de playlists y álbumes (referencia de diseño): bloque de título y
/// metadatos y fila de botones.
pub const HEADER_H: f32 = 106.0;
pub const BAR_BUTTONS_H: f32 = 44.0;
/// Tabla de canciones: cabecera (con el separador), filas, portada y corazón.
pub const TABLE_HEAD_H: f32 = 77.0;
pub const TABLE_ROW_H: f32 = 72.0;
const TABLE_COVER: f32 = 50.0;
const TABLE_COVER_X: f32 = 51.0;
const TABLE_HEART: f32 = 30.0;
/// Título de la canción; artistas, álbum y cabecera; número y duración.
const TABLE_TITLE_FONT: f32 = 17.5;
const TABLE_SMALL_FONT: f32 = 14.5;
const TABLE_NUM_FONT: f32 = 16.0;

/// Columnas de la tabla (x desde su borde izquierdo). `album_x`, `by_x` y `date_x` faltan si
/// la columna no se ve.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TableCols {
    num_c: f32,
    title_x: f32,
    title_w: f32,
    album_x: Option<f32>,
    album_w: f32,
    by_x: Option<f32>,
    by_w: f32,
    date_x: Option<f32>,
    date_w: f32,
    dur_x: f32,
    heart_c: f32,
}

/// Columnas para un ancho `w`: las de la referencia (1039 px: álbum en 360, duración en 670 y
/// el corazón en 771), proporcionales al ancho. En playlists de varios autores (`contrib`),
/// «Añadida por» y «Fecha» entre el álbum y la duración si hay sitio.
fn table_cols(w: f32, cover: bool, album: bool, contrib: bool) -> TableCols {
    let title_x = if cover { 111.0 } else { TABLE_COVER_X };
    let dur_x = (w * 0.645).round().min(w - 150.0).max(title_x + 100.0);
    let gap = 16.0;
    let (album_x, by_x, date_x) = if album && contrib && w >= 900.0 {
        (Some((w * 0.28).round()), Some((w * 0.43).round()), Some((w * 0.55).round()))
    } else if album && w >= 620.0 {
        (Some((w * 0.3465).round()), None, None)
    } else {
        (None, None, None)
    };
    let title_end = album_x.unwrap_or(dur_x);
    let album_end = by_x.unwrap_or(dur_x);
    let by_end = date_x.unwrap_or(dur_x);
    TableCols {
        num_c: 20.5,
        title_x,
        title_w: (title_end - gap - title_x).max(40.0),
        album_x,
        album_w: album_x.map_or(0.0, |x| album_end - gap - x),
        by_x,
        by_w: by_x.map_or(0.0, |x| by_end - gap - x),
        date_x,
        date_w: date_x.map_or(0.0, |x| dur_x - gap - x),
        dur_x,
        heart_c: dur_x + 101.0,
    }
}

/// Pinta `galley` con su borde izquierdo en `at.x` y la línea base en `at.y` (en un píxel
/// entero); devuelve dónde quedó.
pub fn text_on_baseline(painter: &egui::Painter, at: egui::Pos2, galley: std::sync::Arc<egui::Galley>, color: Color32) -> Rect {
    let base = galley
        .rows
        .first()
        .and_then(|r| r.row.glyphs.first().map(|g| r.pos.y + g.pos.y))
        .unwrap_or(galley.size().y * 0.8);
    let min = pos2(at.x.round(), (at.y - base).round());
    let rect = Rect::from_min_size(min, galley.size());
    painter.galley(min, galley, color);
    rect
}

/// Texto en una línea recortado con puntos suspensivos para caber en `max_w`.
pub fn galley_truncated(painter: &egui::Painter, text: &str, font: egui::FontId, color: Color32, max_w: f32) -> std::sync::Arc<egui::Galley> {
    let mut label = text.to_string();
    let mut galley = painter.layout_no_wrap(label.clone(), font.clone(), color);
    while galley.size().x > max_w && label.chars().count() > 3 {
        let n = label.chars().count() - 2;
        label = label.chars().take(n).collect::<String>().trim_end().to_string() + "…";
        galley = painter.layout_no_wrap(label.clone(), font.clone(), color);
    }
    galley
}

pub fn uri_to_link(uri: &str) -> String {
    let parts: Vec<&str> = uri.split(':').collect();
    if parts.len() == 3 {
        format!("https://open.spotify.com/{}/{}", parts[1], parts[2])
    } else {
        uri.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Con el ancho de la referencia (tabla de 1039 px) cada columna cae donde en ella.
    #[test]
    fn tabla_como_la_referencia() {
        let c = table_cols(1039.0, true, true, false);
        assert_eq!((c.num_c, c.title_x, c.album_x, c.dur_x, c.heart_c), (20.5, 111.0, Some(360.0), 670.0, 771.0));
        assert_eq!((c.by_x, c.date_x), (None, None));
        assert_eq!(c.title_w, 360.0 - 16.0 - 111.0);
    }

    /// A cualquier ancho las columnas van en orden, sin solaparse, y el corazón cabe.
    #[test]
    fn tabla_a_cualquier_ancho() {
        for w in (380..=2400).step_by(7).map(|w| w as f32) {
            for (cover, album, contrib) in [(true, true, false), (true, true, true), (false, false, false), (true, false, false)] {
                let c = table_cols(w, cover, album, contrib);
                let mut xs = vec![c.title_x];
                xs.extend(c.album_x);
                xs.extend(c.by_x);
                xs.extend(c.date_x);
                xs.push(c.dur_x);
                assert!(xs.windows(2).all(|p| p[1] - p[0] >= 56.0), "{w} {xs:?}");
                assert!(c.title_w >= 40.0 && c.heart_c + 16.0 <= w, "{w} {c:?}");
                assert_eq!(c.album_x.is_some(), album && w >= 620.0, "{w}");
                assert_eq!(c.by_x.is_some(), album && contrib && w >= 900.0, "{w}");
            }
        }
    }
}
