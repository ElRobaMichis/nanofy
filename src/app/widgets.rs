//! Widgets reutilizables: portadas, tarjetas, chips, cabeceras, tabla de pistas y menús.
//!
//! Las columnas de la tabla y las zonas de la barra del reproductor se colocan con
//! rectángulos explícitos: `allocate_ui_with_layout` encoge la zona a su contenido y no sirve
//! para alinear columnas.

use std::time::Duration;

use egui::{pos2, vec2, Align, Color32, CornerRadius, Label, Layout, Rect, RichText, Sense, Stroke, UiBuilder};

use super::icons::{self, Icon};
use super::theme::{self, GREEN};
use super::{Action, App, Page, PlayTarget, ROW_H};
use crate::model::*;

pub const CARD_W: f32 = 168.0;
pub const CARD_COVER: f32 = 152.0;
pub const CARD_H: f32 = 226.0;

#[derive(Clone, Copy)]
pub enum Source<'a> {
    /// Reproducir dentro de un contexto (playlist o álbum) por uri.
    Context(&'a str),
    /// Reproducir la lista tal cual (uris sueltas).
    Tracks,
}

/// Dónde se abre el menú de canción.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MenuKind {
    NowPlaying,
    PlayerMore,
    Row,
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

/// Sub-`Ui` sobre un rectángulo concreto con un layout dado.
pub fn child_in(ui: &mut egui::Ui, rect: Rect, layout: Layout) -> egui::Ui {
    ui.new_child(UiBuilder::new().max_rect(rect).layout(layout))
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

    /// Pestaña con subrayado verde (páginas de artista).
    pub fn tab(ui: &mut egui::Ui, text: &str, selected: bool) -> egui::Response {
        let p = theme::palette(ui.ctx());
        let color = if selected { p.text } else { p.weak };
        let galley = ui.painter().layout_no_wrap(text.to_string(), theme::regular(14.0), color);
        let size = vec2(galley.size().x + 8.0, 34.0);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        let pos = pos2(rect.min.x + 4.0, rect.center().y - galley.size().y / 2.0 - 3.0);
        ui.painter().galley(pos, galley, color);
        if selected {
            let y = rect.max.y - 2.0;
            ui.painter().line_segment(
                [pos2(rect.min.x + 4.0, y), pos2(rect.max.x - 4.0, y)],
                Stroke::new(2.0, GREEN),
            );
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        resp
    }

    /// Elemento de la barra lateral: icono + texto, con píldora al pasar el ratón.
    pub fn nav_item(ui: &mut egui::Ui, icon: Option<Icon>, text: &str, selected: bool, indent: f32) -> egui::Response {
        let p = theme::palette(ui.ctx());
        let w = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(vec2(w, 34.0), Sense::click());
        if selected || resp.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(10), p.hover);
        }
        let color = if selected { p.text } else { p.weak.lerp_to_gamma(p.text, 0.45) };
        let mut x = rect.min.x + 12.0 + indent;
        if let Some(icon) = icon {
            let icon_rect = Rect::from_center_size(pos2(x + 9.0, rect.center().y), vec2(18.0, 18.0));
            icons::paint(ui.painter(), icon_rect, color, icon);
            x = icon_rect.max.x + 12.0;
        }
        let text_rect = Rect::from_min_max(pos2(x, rect.min.y), pos2(rect.max.x - 8.0, rect.max.y));
        let mut c = child_in(ui, text_rect, Layout::left_to_right(Align::Center));
        c.add(Label::new(RichText::new(text).color(color)).truncate());
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
    /// (sin copiar cientos de pistas en cada fotograma).
    pub fn filter_tracks<'t>(&self, list_id: &str, tracks: &'t [Track]) -> std::borrow::Cow<'t, [Track]> {
        let q = self.list_query(list_id);
        if q.is_empty() {
            return std::borrow::Cow::Borrowed(tracks);
        }
        std::borrow::Cow::Owned(
            tracks
                .iter()
                .filter(|t| {
                    t.name.to_lowercase().contains(&q)
                        || t.artists.iter().any(|a| a.name.to_lowercase().contains(&q))
                        || t.album.as_ref().map(|a| a.name.to_lowercase().contains(&q)).unwrap_or(false)
                })
                .cloned()
                .collect(),
        )
    }

    /// Barra de acciones de una colección de pistas. Con selección activa se convierte en la
    /// barra verde de selección. `context` es el uri a reproducir como contexto (si no, las
    /// pistas sueltas); `extra` añade botones propios de la página tras Aleatorio; `menu`
    /// rellena el menú «Más».
    #[allow(clippy::too_many_arguments)]
    pub fn collection_bar(
        &mut self,
        ui: &mut egui::Ui,
        list_id: &str,
        all: &[Track],
        context: Option<&str>,
        link: Option<&str>,
        extra: impl FnOnce(&mut Self, &mut egui::Ui),
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
        let ids: Vec<String> = all.iter().filter_map(|t| t.id.clone()).collect();
        let uris: Vec<String> = all.iter().map(|t| t.uri.clone()).collect();
        let target = |shuffle: bool| match context {
            Some(uri) => PlayTarget::Context { uri: uri.to_string(), track_uri: None, index: None, shuffle },
            None => PlayTarget::Tracks { uris: uris.clone(), index: if shuffle { None } else { Some(0) }, shuffle },
        };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            if icons::round_button(ui, Icon::Play, 44.0, GREEN, Color32::BLACK).on_hover_text("Reproducir").clicked() && !uris.is_empty() {
                self.actions.push(Action::Play(target(false)));
            }
            if icons::button(ui, Icon::Shuffle, 34.0, p.weak).on_hover_text("Aleatorio").clicked() && !uris.is_empty() {
                self.actions.push(Action::Play(target(true)));
            }
            extra(self, ui);
            if icons::button(ui, Icon::Queue, 34.0, p.weak).on_hover_text("Añadir a la cola").clicked() {
                for u in &uris {
                    self.actions.push(Action::AddToQueue(u.clone()));
                }
            }
            let all_dl = !ids.is_empty() && ids.iter().all(|i| self.downloaded.contains(i));
            let busy = ids.iter().any(|i| self.downloading.contains(i));
            let (icon, color, tip) = if all_dl {
                (Icon::Download, GREEN, "Descargado (en la caché de audio)")
            } else if busy {
                (Icon::Hourglass, p.weak, "Descargando…")
            } else {
                (Icon::Download, p.weak, "Descargar")
            };
            if icons::button(ui, icon, 34.0, color).on_hover_text(tip).clicked() && !all_dl && !busy {
                if all.first().and_then(|t| t.kind.as_deref()) == Some("episode") {
                    self.actions.push(Action::DownloadEpisodes(ids.clone()));
                } else {
                    self.actions.push(Action::Download(ids.clone()));
                }
            }
            if let Some(link) = link {
                if icons::button(ui, Icon::Share, 34.0, p.weak).on_hover_text("Copiar enlace").clicked() {
                    self.actions.push(Action::CopyText(link.to_string(), "Enlace"));
                }
            }
            if let Some(menu) = menu {
                let more = icons::button(ui, Icon::More, 34.0, p.weak).on_hover_text("Más");
                egui::Popup::menu(&more).show(|ui| {
                    ui.set_min_width(200.0);
                    menu(self, ui);
                });
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let color = if self.album_search_open { GREEN } else { p.weak };
                if icons::button(ui, Icon::Search, 34.0, color).on_hover_text("Buscar en esta lista").clicked() {
                    self.album_search_open = !self.album_search_open;
                    self.album_search_focus = self.album_search_open;
                    if !self.album_search_open {
                        self.album_search.clear();
                    }
                }
                if self.album_search_open {
                    let r = ui.add(egui::TextEdit::singleline(&mut self.album_search).hint_text("Buscar en esta lista").desired_width(200.0));
                    if self.album_search_focus {
                        self.album_search_focus = false;
                        r.request_focus();
                    }
                    if r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        self.album_search_open = false;
                        self.album_search.clear();
                    }
                }
            });
        });
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

    /// Menú contextual de una canción (clic derecho en una fila). Misma lista que el reproductor.
    pub fn track_menu(&mut self, ui: &mut egui::Ui, t: &Track, _target: PlayTarget, opts: &RowOpts) {
        self.song_menu(ui, t, MenuKind::Row, opts);
    }

    /// Menú de canción compartido por el clic derecho en filas (`Row`), el clic derecho en la
    /// portada del reproductor (`NowPlaying`) y los tres puntos del reproductor (`PlayerMore`).
    pub fn song_menu(&mut self, ui: &mut egui::Ui, t: &Track, kind: MenuKind, opts: &RowOpts) {
        let id = t.id.clone();
        let is_episode = t.kind.as_deref() == Some("episode");
        // Descargar
        if let Some(id) = &id {
            let dl = self.downloaded.contains(id);
            let label = if dl { "Descargada" } else if is_episode { "Descargar episodio" } else { "Descargar canción" };
            if Self::menu_item_enabled(ui, Some(Icon::Download), label, false, !dl).clicked() {
                if is_episode {
                    self.actions.push(Action::DownloadEpisodes(vec![id.clone()]));
                } else {
                    self.actions.push(Action::Download(vec![id.clone()]));
                }
                ui.close();
            }
        }
        // Temporizador de apagado (submenú al pasar el ratón)
        let timer_label = match (self.sleep_at, self.sleep_end_of_track) {
            (Some(at), _) => format!("Temporizador ({} min)", (at.saturating_duration_since(std::time::Instant::now()).as_secs() + 59) / 60),
            (None, true) => "Temporizador (al terminar)".to_string(),
            _ => "Temporizador de apagado".to_string(),
        };
        let r = Self::menu_item(ui, Some(Icon::Clock), &timer_label, true);
        egui::containers::menu::SubMenu::default().show(ui, &r, |ui| self.sleep_timer_items(ui));
        // Ocultar
        if let Some(id) = &id {
            let hidden = self.hidden_tracks.contains(id);
            let (icon, label) = if hidden { (Icon::Eye, "Mostrar canción") } else { (Icon::EyeOff, "Ocultar canción") };
            if Self::menu_item(ui, Some(icon), label, false).clicked() {
                self.toggle_hidden(id);
                ui.close();
            }
        }
        // Compartir
        if !t.uri.is_empty() && Self::menu_item(ui, Some(Icon::Share), "Compartir canción", false).clicked() {
            self.actions.push(Action::CopyText(uri_to_link(&t.uri), "Enlace"));
            ui.close();
        }
        // Agregar a la biblioteca (submenú: Me gusta / playlist)
        let r = Self::menu_item(ui, Some(Icon::PlusSquare), "Agregar a la biblioteca", true);
        let uri = t.uri.clone();
        let liked = id.as_ref().map(|i| self.liked_set.contains(i)).unwrap_or(false);
        egui::containers::menu::SubMenu::default().show(ui, &r, |ui| {
            if let Some(id) = &id {
                let (icon, label) = if liked { (Icon::HeartFilled, "Quitar de Canciones que te gustan") } else { (Icon::Heart, "Canciones que te gustan") };
                if Self::menu_item(ui, Some(icon), label, false).clicked() {
                    self.actions.push(Action::Like(id.clone(), !liked));
                    ui.close();
                }
            }
            if Self::menu_item(ui, Some(Icon::Playlist), "Añadir a una playlist…", false).clicked() {
                self.open_add_dialog(vec![uri.clone()]);
                ui.close();
            }
        });
        // Cola
        if Self::menu_item(ui, Some(Icon::Queue), "Agregar canción a la cola", false).clicked() {
            self.actions.push(Action::AddToQueue(t.uri.clone()));
            ui.close();
        }
        if let Some(pl) = opts.editable_playlist {
            if Self::menu_item(ui, Some(Icon::Trash), if pl == "queue" { "Quitar de la cola" } else { "Quitar de esta playlist" }, false).clicked() {
                self.actions.push(Action::RemoveFromPlaylist { playlist_id: pl.to_string(), uri: t.uri.clone() });
                ui.close();
            }
        }
        // Radio
        if let Some(id) = &id {
            if !is_episode && Self::menu_item(ui, Some(Icon::Radio), "Ir a la radio de la canción", false).clicked() {
                self.actions.push(Action::OpenRadio(id.clone()));
                ui.close();
            }
        }
        // Álbum
        // Las pistas de un álbum no traen su álbum: se toma del contexto de la página.
        let album_id = t.album.as_ref().and_then(|a| a.id.clone()).or_else(|| match opts.source {
            Source::Context(u) => u.strip_prefix("spotify:album:").map(|s| s.to_string()),
            Source::Tracks => None,
        });
        if Self::menu_item_enabled(ui, Some(Icon::Album), "Ver álbum", false, album_id.is_some()).clicked() {
            if let Some(aid) = album_id {
                self.actions.push(Action::Go(Page::Album(aid)));
            }
            ui.close();
        }
        // Artista(s)
        let artists: Vec<(String, String)> = t.artists.iter().filter_map(|a| a.id.clone().map(|id| (a.name.clone(), id))).collect();
        match artists.len() {
            0 => {}
            1 => {
                if Self::menu_item(ui, Some(Icon::Artist), &format!("Ver {}", artists[0].0), false).clicked() {
                    self.actions.push(Action::Go(Page::Artist(artists[0].1.clone())));
                    ui.close();
                }
            }
            _ => {
                let r = Self::menu_item(ui, Some(Icon::Artist), "Ver artista", true);
                egui::containers::menu::SubMenu::default().show(ui, &r, |ui| {
                    for (name, aid) in &artists {
                        if Self::menu_item(ui, Some(Icon::Artist), name, false).clicked() {
                            self.actions.push(Action::Go(Page::Artist(aid.clone())));
                            ui.close();
                        }
                    }
                });
            }
        }
        if kind != MenuKind::Row {
            if Self::menu_item(ui, Some(Icon::Miniplayer), if self.miniplayer { "Salir del miniplayer" } else { "Miniplayer" }, false).clicked() {
                let ctx = ui.ctx().clone();
                self.toggle_miniplayer(&ctx);
                ui.close();
            }
            if Self::menu_item(ui, Some(Icon::Fullscreen), if self.fullscreen { "Salir de pantalla completa" } else { "Pantalla completa" }, false).clicked() {
                let ctx = ui.ctx().clone();
                self.toggle_fullscreen(&ctx);
                ui.close();
            }
        }
        if kind == MenuKind::PlayerMore {
            if Self::menu_item(ui, Some(Icon::People), if self.jam.is_some() { "Jam" } else { "Iniciar una Jam" }, false).clicked() {
                self.jam_open = true;
                ui.close();
            }
            if Self::menu_item(ui, Some(Icon::Keyboard), "Atajos de teclado", false).clicked() {
                self.show_shortcuts = true;
                ui.close();
            }
        }
    }

    /// Opciones del temporizador de apagado.
    pub fn sleep_timer_items(&mut self, ui: &mut egui::Ui) {
        for m in [15u64, 30, 45, 60] {
            if Self::menu_item(ui, Some(Icon::Clock), &format!("{m} minutos"), false).clicked() {
                self.sleep_at = Some(std::time::Instant::now() + Duration::from_secs(m * 60));
                self.sleep_end_of_track = false;
                self.status(format!("Se pausará en {m} minutos"));
                ui.close();
            }
        }
        if Self::menu_item(ui, Some(Icon::Clock), "Al terminar la canción", false).clicked() {
            self.sleep_end_of_track = true;
            self.sleep_at = None;
            self.status("Se pausará al terminar la canción");
            ui.close();
        }
        if (self.sleep_at.is_some() || self.sleep_end_of_track)
            && Self::menu_item(ui, Some(Icon::Close), "Cancelar temporizador", false).clicked()
        {
            self.sleep_at = None;
            self.sleep_end_of_track = false;
            ui.close();
        }
    }

    pub fn track_rows(&mut self, ui: &mut egui::Ui, list_id: &str, tracks: &[Track], opts: RowOpts) {
        let n = tracks.len();
        if n == 0 {
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
            let more_id = ui.id().with(("more", list_id, i));
            let hov = resp.hovered() || resp.contains_pointer() || egui::Popup::is_id_open(ui.ctx(), more_id);
            if selected || hov {
                ui.painter().rect_filled(row.shrink2(vec2(4.0, 1.0)), CornerRadius::same(10), p.hover);
            }
            let is_now = now_uri.as_deref() == Some(t.uri.as_str());
            let y_range = y..=y + ROW_H;

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
                let color = if is_now { GREEN } else if hidden { p.faint } else { p.text };
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
                        let target = opts.target(i, tracks, shuffle);
                        egui::Popup::menu(&more).id(more_id).show(|ui| self.track_menu(ui, t, target, &opts));
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
            resp.context_menu(|ui| {
                let target = opts.target(i, tracks, shuffle);
                self.track_menu(ui, t, target, &opts);
            });
        }
    }
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
