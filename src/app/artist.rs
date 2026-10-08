//! Página de artista: cabecera "hero", pestañas (Inicio, Álbumes, Sencillos y EPs,
//! Recopilatorios, Aparece en, Acerca de), búsqueda dentro del artista, discografía en
//! cuadrícula o en lista desplegable.

use egui::{pos2, vec2, Align, Color32, CornerRadius, Label, Layout, Rect, RichText, Sense};

use super::icons::{self, Icon};
use super::theme::{self, GREEN};
use super::widgets::{child_in, galley_truncated, text_on_baseline, uri_to_link, vertical_gradient, CardInfo, CardKind, RowOpts, Source};
use super::{fmt_thousands, Action, App, Page, PlayTarget};
use crate::api::Req;
use crate::model::*;

pub const ARTIST_TABS: [&str; 7] = ["Inicio", "Álbumes", "Sencillos y EPs", "Recopilatorios", "Aparece en", "Playlists", "Acerca de"];

// ------------------------------------------------------------ medidas (referencia 7.png)

/// Imagen de la cabecera y sus textos.
const HERO_H: f32 = 424.0;
const HERO_TITLE_FONT: f32 = 49.0;
const HERO_SUB_FONT: f32 = 15.75;
const HERO_TITLE: Color32 = Color32::from_gray(232);
const HERO_SUB: Color32 = Color32::from_gray(220);
const HERO_FOLLOW: Color32 = Color32::from_gray(200);
const HERO_ICON: Color32 = Color32::from_gray(232);
/// Fila de pestañas: alto, tamaño de letra, separación entre nombres y colores.
const TAB_ROW_H: f32 = 61.0;
const TAB_FONT: f32 = 14.75;
const TAB_GAP: f32 = 51.0;
const TAB_TEXT: Color32 = Color32::from_gray(130);
const TAB_HOVER: Color32 = Color32::from_gray(205);
const TAB_SELECTED: Color32 = Color32::from_gray(214);
const TAB_ICON: Color32 = Color32::from_gray(138);
const TOGGLE_BG: Color32 = Color32::from_gray(19);
const SEPARATOR: Color32 = Color32::from_gray(26);
/// Márgenes del contenido bajo las pestañas y hueco hasta la primera fila de tarjetas.
const BODY_LEFT: i8 = 23;
const BODY_RIGHT: i8 = 26;
const GRID_TOP: f32 = 40.0;
/// Tarjetas: portada, hueco entre portadas, lo que ocupa cada fila además de la portada y
/// textos.
const CARD_SIDE: f32 = 171.0;
const CARD_GAP: f32 = 10.5;
const ROW_EXTRA: f32 = 103.2;
const CARD_TITLE_FONT: f32 = 15.25;
const CARD_SMALL_FONT: f32 = 13.25;
const CARD_TITLE: Color32 = Color32::from_gray(212);
const CARD_DIM: Color32 = Color32::from_gray(114);

impl App {
    pub fn artist_page(&mut self, ui: &mut egui::Ui, id: String) {
        if !self.signed_in() {
            self.welcome(ui);
            return;
        }
        let p = theme::palette(ui.ctx());
        if !self.requested.contains(&format!("artist:{id}")) {
            self.requested.insert(format!("artist:{id}"));
            self.requested.insert(format!("artistmeta:{id}"));
            // Req::Artist trae seguidores y géneros (Web API); las populares llegan con la vista,
            // que hace un único Artist::get para ambas.
            self.api.send(Req::Artist(id.clone()));
            self.api.send(Req::ArtistView(id.clone()));
        }
        let page = self.artists.remove(&id).unwrap_or_default();
        let view = self.artist_views.get(&id).cloned();

        // ---------------- cabecera y pestañas (de borde a borde del panel, como la referencia)
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        let name = page
            .artist
            .as_ref()
            .map(|a| a.name.clone())
            .or_else(|| view.as_ref().map(|v| v.name.clone()).filter(|n| !n.is_empty()))
            .unwrap_or_else(|| "Artista".into());
        self.artist_hero(ui, &id, &name, &page, view.as_ref());
        self.artist_tab_row(ui, &name);

        let filter = self.artist_search.trim().to_lowercase();
        let matches = |s: &str| filter.is_empty() || s.to_lowercase().contains(&filter);

        // Debajo de las pestañas, el contenido con los márgenes de la referencia.
        let tab_now = self.artist_tab;
        let grid = self.artist_grid && (1..=4).contains(&tab_now);
        egui::Frame::NONE
            .inner_margin(egui::Margin { left: BODY_LEFT, right: BODY_RIGHT, top: 0, bottom: 0 })
            .show(ui, |ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
        ui.add_space(if grid { GRID_TOP } else { 24.0 });
        match tab_now {
            0 => self.artist_home(ui, &id, &name, &page, view.as_ref(), &matches),
            5 => self.artist_playlists(ui, &id, &name),
            6 => self.artist_about(ui, &page, view.as_ref()),
            tab => {
                let list: Vec<AlbumRef> = match (tab, view.as_ref()) {
                    (1, Some(v)) => v.albums.clone(),
                    (2, Some(v)) => v.singles.clone(),
                    (3, Some(v)) => v.compilations.clone(),
                    (4, Some(v)) => v.appears_on.clone(),
                    (1, None) => page.albums.iter().filter(|a| a.album_type.as_deref() != Some("single")).cloned().collect(),
                    (2, None) => page.albums.iter().filter(|a| a.album_type.as_deref() == Some("single")).cloned().collect(),
                    _ => Vec::new(),
                };
                let mut list: Vec<AlbumRef> = list.into_iter().filter(|a| matches(&a.name)).collect();
                list.sort_by(|x, y| y.release_date.cmp(&x.release_date));
                if list.is_empty() {
                    let msg = if view.is_none() && page.albums.is_empty() { "Cargando…" } else { "Nada por aquí." };
                    ui.label(RichText::new(msg).color(p.weak));
                } else if self.artist_grid {
                    self.album_grid(ui, &list);
                } else {
                    self.album_list(ui, &list);
                }
            }
        }
            });
        self.artists.insert(id, page);
    }

    /// Cabecera del artista, copia de la referencia: la imagen ocupa el panel de borde a borde
    /// (424 px), el nombre en negrita de 48,5 px y los seguidores debajo; a la derecha, sobre la
    /// imagen, reproducir, «Seguir», añadir a una playlist, a la cola y más.
    fn artist_hero(&mut self, ui: &mut egui::Ui, id: &str, name: &str, page: &super::ArtistPage, view: Option<&ArtistView>) {
        let p = theme::palette(ui.ctx());
        let width = ui.available_width();
        let (hero, _) = ui.allocate_exact_size(vec2(width, HERO_H), Sense::hover());
        let header_img = view
            .and_then(|v| v.header.clone())
            .or_else(|| page.artist.as_ref().and_then(|a| a.cover(640).map(|s| s.to_string())));
        // La foto del artista compuesta a lo ancho de la cabecera, 1:1 en píxeles.
        let corners = CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 };
        let ppp = ui.ctx().pixels_per_point();
        let (pw, ph) = (((hero.width() * ppp).round() as u32).min(3200), (hero.height() * ppp).round() as u32);
        match header_img.as_deref().and_then(|u| self.images.texture_hero(u, pw, ph)) {
            Some(t) => {
                egui::Image::from_texture(t).maintain_aspect_ratio(false).corner_radius(corners).paint_at(ui, hero);
            }
            None => {
                ui.painter().rect_filled(hero, corners, p.card2);
            }
        }
        // Velo como el de la referencia: nada en el 12 % de arriba y de ahí, lineal, hasta un
        // negro del 74 % en el pie (el texto y los botones se leen sobre cualquier foto).
        let veil = Rect::from_min_max(pos2(hero.min.x, hero.min.y + hero.height() * 0.12), hero.max);
        vertical_gradient(ui.painter(), veil, Color32::TRANSPARENT, Color32::from_black_alpha(189));

        let painter = ui.painter().clone();
        let g = galley_truncated(&painter, name, theme::bold(HERO_TITLE_FONT), HERO_TITLE, (hero.width() - 36.0 - 380.0).max(80.0));
        text_on_baseline(&painter, pos2(hero.min.x + 36.0, hero.max.y - 86.0), g, HERO_TITLE);
        let followers = page.artist.as_ref().and_then(|a| a.followers.as_ref().and_then(|f| f.total));
        if let Some(n) = followers {
            let g = painter.layout_no_wrap(format!("{} seguidores", fmt_thousands(n)), theme::regular(HERO_SUB_FONT), HERO_SUB);
            text_on_baseline(&painter, pos2(hero.min.x + 37.0, hero.max.y - 36.0), g, HERO_SUB);
        }

        // Botones, centrados 51,6 px por encima del pie de la imagen.
        let cy = hero.max.y - 51.6;
        let xr = hero.max.x;
        let top_uris: Vec<String> = page.top.iter().map(|t| t.uri.clone()).collect();
        let play = Rect::from_center_size(pos2(xr - 319.0, cy), vec2(41.0, 41.0));
        let mut c = child_in(ui, play, Layout::left_to_right(Align::Center));
        if icons::round_button(&mut c, Icon::Play, 41.0, GREEN, Color32::BLACK).on_hover_text("Reproducir populares").clicked() && !top_uris.is_empty() {
            self.actions.push(Action::Play(PlayTarget::Tracks { uris: top_uris.clone(), index: Some(0), shuffle: false }));
        }
        self.follow_pill(ui, Rect::from_center_size(pos2(xr - 242.3, cy), vec2(86.0, 43.0)), "artist", id);
        let hid = ui.id().with(("artist_hero", id));
        if Self::slot_button(ui, hid.with("add"), pos2(xr - 167.2, cy), Icon::PlusSquare, 28.0, HERO_ICON)
            .on_hover_text("Añadir populares a una playlist")
            .clicked()
            && !top_uris.is_empty()
        {
            self.open_add_dialog(top_uris.clone());
        }
        if Self::slot_button(ui, hid.with("queue"), pos2(xr - 112.9, cy), Icon::QueueList, 28.0, HERO_ICON)
            .on_hover_text("Añadir populares a la cola")
            .clicked()
        {
            for u in &top_uris {
                self.actions.push(Action::AddToQueue(u.clone()));
            }
        }
        let more = Self::slot_button(ui, hid.with("more"), pos2(xr - 58.5, cy), Icon::More, 30.0, HERO_ICON).on_hover_text("Más");
        let artist_uri = format!("spotify:artist:{id}");
        egui::Popup::menu(&more).show(|ui| {
            if Self::menu_item(ui, Some(Icon::Share), "Copiar enlace", false).clicked() {
                self.actions.push(Action::CopyText(uri_to_link(&artist_uri), "Enlace"));
                ui.close();
            }
            if Self::menu_item(ui, Some(Icon::NewTab), "Abrir en una nueva pestaña", false).clicked() {
                self.actions.push(Action::OpenInTab(Page::Artist(id.to_string())));
                ui.close();
            }
        });
        let _ = p;
    }

    /// «Seguir» / «Siguiendo» de la cabecera: píldora translúcida de `rect`.
    fn follow_pill(&mut self, ui: &mut egui::Ui, rect: Rect, kind: &'static str, id: &str) {
        let key = format!("{kind}:{id}");
        let state = self.following.get(&key).copied().or(if kind == "artist" && self.artists_loaded { Some(false) } else { None });
        let resp = ui.interact(rect, ui.id().with(("follow_pill", id)), if state.is_some() { Sense::click() } else { Sense::hover() });
        let following = state == Some(true);
        let fill = if resp.hovered() && state.is_some() { Color32::from_white_alpha(34) } else { Color32::from_white_alpha(21) };
        ui.painter().rect_filled(rect, CornerRadius::same((rect.height() / 2.0) as u8), fill);
        let text = if following { "Siguiendo" } else { "Seguir" };
        let color = if resp.hovered() && state.is_some() { Color32::WHITE } else { HERO_FOLLOW };
        let g = ui.painter().layout_no_wrap(text.into(), theme::regular(14.0), color);
        let x = rect.center().x - g.size().x / 2.0;
        text_on_baseline(ui.painter(), pos2(x, rect.center().y + 5.4), g, color);
        if resp.hovered() && state.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let resp = if following { resp.on_hover_text("Dejar de seguir") } else { resp };
        if resp.clicked() {
            self.actions.push(Action::Follow { kind, id: id.to_string(), on: !following });
        }
    }

    /// Pestañas del artista (copia de la referencia): fila de 61 px bajo la imagen con los
    /// nombres a 44 px del borde, 51 px entre uno y otro, la elegida en blanco y subrayada en
    /// verde; la lupa justo detrás de la última y, a la derecha, lista y cuadrícula. Debajo, un
    /// filo de 1 px de lado a lado.
    fn artist_tab_row(&mut self, ui: &mut egui::Ui, name: &str) {
        let width = ui.available_width();
        let (row, _) = ui.allocate_exact_size(vec2(width, TAB_ROW_H), Sense::hover());
        let base = row.min.y + 38.3;
        let sep_y = row.max.y - 0.5;
        let font = theme::regular(TAB_FONT);
        let mut x = row.min.x + 44.0;
        for (i, label) in ARTIST_TABS.iter().enumerate() {
            let selected = self.artist_tab == i as u8;
            let g = ui.painter().layout_no_wrap(label.to_string(), font.clone(), TAB_TEXT);
            let w = g.size().x;
            let hit = Rect::from_min_max(pos2(x - 8.0, row.min.y + 8.0), pos2(x + w + 8.0, row.max.y));
            let resp = ui.interact(hit, ui.id().with(("artist_tab", i)), Sense::click());
            let color = if selected { TAB_SELECTED } else if resp.hovered() { TAB_HOVER } else { TAB_TEXT };
            let g = ui.painter().layout_no_wrap(label.to_string(), font.clone(), color);
            text_on_baseline(ui.painter(), pos2(x, base), g, color);
            if selected {
                let under = Rect::from_min_max(pos2(x - 6.0, sep_y - 1.2), pos2(x + w + 6.0, sep_y + 1.5));
                ui.painter().rect_filled(under, CornerRadius::same(1), GREEN);
            }
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                self.artist_tab = i as u8;
            }
            x += w + TAB_GAP;
        }

        // Lupa detrás de la última pestaña; el campo de búsqueda se abre a su derecha.
        let lupa = pos2(x - TAB_GAP + 70.0, row.min.y + 31.1);
        let color = if self.artist_search_open { GREEN } else { TAB_ICON };
        if Self::slot_button(ui, ui.id().with("artist_search"), lupa, Icon::Search, 29.0, color).on_hover_text("Buscar dentro del artista").clicked() {
            self.artist_search_open = !self.artist_search_open;
            self.artist_search_focus = self.artist_search_open;
            if !self.artist_search_open {
                self.artist_search.clear();
            }
        }
        let toggles = (1..=4).contains(&self.artist_tab);
        if self.artist_search_open {
            let right = if toggles { row.max.x - 110.0 } else { row.max.x - 24.0 };
            let field = Rect::from_min_max(pos2(lupa.x + 24.0, lupa.y - 15.0), pos2((lupa.x + 284.0).min(right), lupa.y + 15.0));
            if field.width() > 60.0 {
                let mut f = child_in(ui, field, Layout::left_to_right(Align::Center));
                let r = f.add(egui::TextEdit::singleline(&mut self.artist_search).hint_text(format!("Buscar en {name}")).desired_width(field.width()));
                if self.artist_search_focus {
                    self.artist_search_focus = false;
                    r.request_focus();
                }
                if f.input(|i| i.key_pressed(egui::Key::Escape)) && r.has_focus() {
                    self.artist_search_open = false;
                    self.artist_search.clear();
                }
            }
        }

        // Lista y cuadrícula (solo en la discografía); la elegida, sobre un cuadrado tenue.
        if toggles {
            let cy = row.min.y + 31.1;
            let grid_c = pos2(row.max.x - 36.0, cy);
            let list_c = pos2(row.max.x - 84.0, cy);
            let sel_c = if self.artist_grid { grid_c } else { list_c };
            ui.painter().rect_filled(Rect::from_center_size(sel_c - vec2(0.6, 0.0), vec2(37.0, 38.0)), CornerRadius::same(5), TOGGLE_BG);
            let (gc, lc) = if self.artist_grid { (TAB_SELECTED, TAB_ICON) } else { (TAB_ICON, TAB_SELECTED) };
            if Self::slot_button(ui, ui.id().with("artist_list"), list_c, Icon::List, 25.0, lc).on_hover_text("Lista").clicked() {
                self.artist_grid = false;
            }
            if Self::slot_button(ui, ui.id().with("artist_grid"), grid_c, Icon::Grid, 28.0, gc).on_hover_text("Cuadrícula").clicked() {
                self.artist_grid = true;
            }
        }
        ui.painter().line_segment([pos2(row.min.x, sep_y), pos2(row.max.x, sep_y)], egui::Stroke::new(1.0, SEPARATOR));
    }

    fn artist_home(
        &mut self,
        ui: &mut egui::Ui,
        id: &str,
        name: &str,
        page: &super::ArtistPage,
        view: Option<&ArtistView>,
        matches: &dyn Fn(&str) -> bool,
    ) {
        let p = theme::palette(ui.ctx());
        let width = ui.available_width();
        let right_w = if width >= 980.0 { 300.0 } else { 0.0 };
        let top = ui.cursor().min;
        let left_w = if right_w > 0.0 { width - right_w - 24.0 } else { width };
        let mut l = child_in(ui, Rect::from_min_size(top, vec2(left_w, f32::INFINITY)), Layout::top_down(Align::Min));
        l.set_width(left_w);

        // Tus más escuchadas (registro local de reproducciones)
        let mut mine: Vec<(u32, Track)> = self
            .play_log
            .entries
            .iter()
            .filter(|e| e.track.artists.iter().any(|a| a.id.as_deref() == Some(id)))
            .filter(|e| matches(&e.track.name))
            .map(|e| (e.count, e.track.clone()))
            .collect();
        mine.sort_by(|a, b| b.0.cmp(&a.0));
        let mine: Vec<Track> = mine.into_iter().take(5).map(|(_, t)| t).collect();
        Self::section_title(&mut l, "Tus más escuchadas");
        if mine.is_empty() {
            l.label(RichText::new(format!("Todavía no has escuchado nada de {name} en Nanofy.")).small().color(p.weak));
        } else {
            self.track_rows(&mut l, &format!("mine:{id}"), &mine, RowOpts::tracks(true, false));
        }

        // Populares
        let popular: Vec<Track> = page.top.iter().filter(|t| matches(&t.name)).cloned().collect();
        Self::section_title(&mut l, "Populares");
        if popular.is_empty() {
            if page.top.is_empty() {
                Self::loading(&mut l, "Cargando");
            }
        } else {
            self.track_rows(&mut l, &format!("top:{id}"), &popular, RowOpts::tracks(true, false));
        }

        // Artistas relacionados
        if let Some(v) = view {
            if !v.related.is_empty() {
                Self::section_title(&mut l, "Los fans también escuchan");
                let rel = v.related.clone();
                Self::cards_row(&mut l, &format!("rel:{id}"), |ui| {
                    for ar in rel.iter().take(12) {
                        let r = self.card(
                            ui,
                            CardInfo {
                                kind: CardKind::Artist,
                                cover: ar.cover(300),
                                title: &ar.name,
                                subtitle: "Artista",
                                count: None,
                                pinned: false,
                            },
                        );
                        if r.clicked() {
                            self.actions.push(Action::Go(Page::Artist(ar.id.clone())));
                        }
                        let rid = ar.id.clone();
                        r.context_menu(|ui| {
                            if Self::menu_item(ui, Some(Icon::NewTab), "Abrir en una nueva pestaña", false).clicked() {
                                self.actions.push(Action::OpenInTab(Page::Artist(rid.clone())));
                                ui.close();
                            }
                        });
                    }
                });
            }
        }
        let left_h = l.min_rect().height();

        // Columna derecha: selección del artista, retrato, biografía y géneros
        let mut right_h = 0.0;
        if right_w > 0.0 {
            let mut r = child_in(ui, Rect::from_min_size(pos2(top.x + left_w + 24.0, top.y), vec2(right_w, f32::INFINITY)), Layout::top_down(Align::Min));
            r.set_width(right_w);
            Self::section_title(&mut r, "Selección del artista");
            let latest = view.and_then(|v| v.latest.clone());
            match latest {
                Some(al) => {
                    let resp = egui::Frame::new()
                        .fill(p.card2)
                        .corner_radius(CornerRadius::same(12))
                        .inner_margin(10)
                        .show(&mut r, |ui| {
                            ui.set_width(right_w - 20.0);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 12.0;
                                self.cover(ui, al.cover(300), 84.0, false);
                                ui.vertical(|ui| {
                                    ui.set_width(right_w - 20.0 - 96.0);
                                    ui.label(RichText::new("Último lanzamiento").small().color(p.weak));
                                    ui.add(Label::new(RichText::new(&al.name).strong()).wrap());
                                    ui.label(RichText::new(al.year()).small().color(p.weak));
                                });
                            });
                        })
                        .response
                        .interact(Sense::click());
                    if resp.hovered() {
                        r.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        if let Some(aid) = &al.id {
                            self.actions.push(Action::Go(Page::Album(aid.clone())));
                        }
                    }
                }
                None => {
                    r.label(RichText::new("Sin novedades.").small().color(p.weak));
                }
            }
            r.add_space(12.0);
            let portrait = page.artist.as_ref().and_then(|a| a.cover(640).map(|s| s.to_string()));
            if portrait.is_some() {
                let (rect, _) = r.allocate_exact_size(vec2(right_w, right_w * 0.66), Sense::hover());
                self.cover_in(&mut r, portrait.as_deref(), rect, 12);
                r.add_space(10.0);
            }
            if let Some(bio) = view.map(|v| v.biography.clone()).filter(|b| !b.is_empty()) {
                let excerpt: String = bio.chars().take(320).collect();
                let more = bio.chars().count() > 320;
                r.add(Label::new(RichText::new(format!("{excerpt}{}", if more { "…" } else { "" })).small().color(p.weak)).wrap());
                if more {
                    let link = r.add(Label::new(RichText::new("Leer más").small().color(GREEN)).sense(Sense::click()));
                    if link.clicked() {
                        self.artist_tab = 6;
                    }
                }
                r.add_space(8.0);
            }
            if let Some(a) = &page.artist {
                if !a.genres.is_empty() {
                    r.horizontal_wrapped(|ui| {
                        for g in a.genres.iter().take(6) {
                            let _ = Self::pill(ui, g, false);
                        }
                    });
                }
            }
            right_h = r.min_rect().height();
        }
        ui.allocate_rect(Rect::from_min_size(top, vec2(width, left_h.max(right_h))), Sense::hover());
    }

    /// Playlists creadas por el artista (las que Spotify muestra como "Artist playlists").
    fn artist_playlists(&mut self, ui: &mut egui::Ui, id: &str, name: &str) {
        let p = theme::palette(ui.ctx());
        self.request_once(&format!("artistpl:{id}"), Req::ArtistPlaylists { id: id.to_string(), name: name.to_string() });
        match self.artist_playlists.get(id).cloned() {
            None => Self::loading(ui, "Buscando playlists del artista"),
            Some(list) if list.is_empty() => {
                ui.label(RichText::new(format!("{name} no tiene playlists públicas propias.")).color(p.weak));
            }
            Some(list) => {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(4.0, 8.0);
                    for pl in &list {
                        let r = self.playlist_card(ui, pl);
                        if r.clicked() {
                            self.playlist_meta.entry(pl.id.clone()).or_insert_with(|| pl.clone());
                            self.actions.push(Action::Go(Page::Playlist(pl.id.clone())));
                        }
                    }
                });
            }
        }
    }

    fn artist_about(&mut self, ui: &mut egui::Ui, page: &super::ArtistPage, view: Option<&ArtistView>) {
        let p = theme::palette(ui.ctx());
        let width = ui.available_width();
        let right_w = if width >= 900.0 { 260.0 } else { 0.0 };
        let top = ui.cursor().min;
        let left_w = if right_w > 0.0 { width - right_w - 32.0 } else { width };
        let mut l = child_in(ui, Rect::from_min_size(top, vec2(left_w, f32::INFINITY)), Layout::top_down(Align::Min));
        l.set_width(left_w);
        match view.map(|v| v.biography.clone()).filter(|b| !b.is_empty()) {
            Some(bio) => {
                for para in bio.split('\n').filter(|s| !s.trim().is_empty()) {
                    l.add(Label::new(RichText::new(para.trim()).font(theme::regular(15.0)).color(p.text)).wrap());
                    l.add_space(8.0);
                }
            }
            None => {
                if view.is_none() {
                    Self::loading(&mut l, "Cargando");
                } else {
                    l.label(RichText::new("Spotify no tiene biografía para este artista.").color(p.weak));
                }
            }
        }
        if let Some(v) = view {
            if !v.related.is_empty() {
                Self::section_title(&mut l, "Los fans también escuchan");
                let rel = v.related.clone();
                Self::cards_row(&mut l, "about_rel", |ui| {
                    for ar in rel.iter().take(12) {
                        let r = self.card(
                            ui,
                            CardInfo { kind: CardKind::Artist, cover: ar.cover(300), title: &ar.name, subtitle: "Artista", count: None, pinned: false },
                        );
                        if r.clicked() {
                            self.actions.push(Action::Go(Page::Artist(ar.id.clone())));
                        }
                    }
                });
            }
        }
        let left_h = l.min_rect().height();
        let mut right_h = 0.0;
        if right_w > 0.0 {
            let mut r = child_in(ui, Rect::from_min_size(pos2(top.x + left_w + 32.0, top.y), vec2(right_w, f32::INFINITY)), Layout::top_down(Align::Min));
            r.set_width(right_w);
            if let Some(n) = page.artist.as_ref().and_then(|a| a.followers.as_ref().and_then(|f| f.total)) {
                r.label(RichText::new(fmt_thousands(n)).font(theme::bold(26.0)));
                r.label(RichText::new("Seguidores").small().color(p.weak));
                r.add_space(12.0);
            }
            if let Some(a) = &page.artist {
                if !a.genres.is_empty() {
                    r.label(RichText::new("Géneros").small().color(p.weak));
                    r.horizontal_wrapped(|ui| {
                        for g in a.genres.iter().take(8) {
                            let _ = Self::pill(ui, g, false);
                        }
                    });
                    r.add_space(12.0);
                }
            }
            r.label(
                RichText::new(
                    "Oyentes mensuales, ciudades y reproducciones por canción solo están disponibles \
                     en el reproductor web de Spotify; no hay API que los ofrezca a otros clientes.",
                )
                .small()
                .color(p.faint),
            );
            right_h = r.min_rect().height();
        }
        ui.allocate_rect(Rect::from_min_size(top, vec2(width, left_h.max(right_h))), Sense::hover());
    }

    /// Discografía en cuadrícula (copia de la referencia): portadas de 171 px cada 181,5, filas
    /// cada 274,2; cuantas columnas quepan, con las tarjetas estiradas para llenar el ancho.
    fn album_grid(&mut self, ui: &mut egui::Ui, list: &[AlbumRef]) {
        let width = ui.available_width();
        let cols = (((width + CARD_GAP) / (CARD_SIDE + CARD_GAP)).floor() as usize).max(1);
        let side = ((width - CARD_GAP * (cols as f32 - 1.0)) / cols as f32).min(CARD_SIDE * 1.4);
        let pitch_x = side + CARD_GAP;
        let pitch_y = side + ROW_EXTRA;
        let albums: Vec<&AlbumRef> = list.iter().filter(|a| a.id.is_some()).collect();
        let rows = albums.len().div_ceil(cols);
        let left = ui.cursor().min.x;
        let (area, _) = ui.allocate_exact_size(vec2(width, rows as f32 * pitch_y), Sense::hover());
        for (i, al) in albums.into_iter().enumerate() {
            let x = (left + (i % cols) as f32 * pitch_x).round();
            let y = (area.min.y + (i / cols) as f32 * pitch_y).round();
            let tile = Rect::from_min_size(pos2(x, y), vec2(side, side + ROW_EXTRA - 10.0));
            if ui.is_rect_visible(tile) {
                self.album_tile(ui, tile, side, al);
            }
        }
    }

    /// Tarjeta de un álbum de la discografía: la «pila» (tono oscuro de la portada) asomando 9 px
    /// por encima, la portada con esquinas de 6 px, el título y el número de canciones en una
    /// línea y el año debajo.
    fn album_tile(&mut self, ui: &mut egui::Ui, tile: Rect, side: f32, al: &AlbumRef) {
        let Some(aid) = al.id.clone() else { return };
        let resp = ui.interact(tile, ui.id().with(("album_tile", &aid)), Sense::click());
        let url = al.cover(300).map(|s| s.to_string());
        let cover = Rect::from_min_size(pos2(tile.min.x, tile.min.y + 9.0), vec2(side, side));
        let stack = url.as_deref().and_then(|u| self.images.stack(u)).unwrap_or(Color32::from_gray(40));
        let band = Rect::from_min_max(pos2(cover.min.x + 6.5, tile.min.y), pos2(cover.max.x - 6.5, cover.min.y + 3.0));
        ui.painter().rect_filled(band, CornerRadius { nw: 4, ne: 4, sw: 0, se: 0 }, stack);
        ui.painter().rect_filled(Rect::from_min_max(pos2(band.min.x, cover.min.y - 2.5), pos2(band.max.x, cover.min.y)), CornerRadius::ZERO, Color32::from_black_alpha(150));
        self.cover_in(ui, url.as_deref(), cover, 6);
        if resp.hovered() {
            ui.painter().rect_filled(cover, CornerRadius::same(6), Color32::from_white_alpha(14));
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let painter = ui.painter();
        let base = cover.max.y + 19.5;
        let count = al.total_tracks.map(|n| painter.layout_no_wrap(n.to_string(), theme::regular(CARD_SMALL_FONT), CARD_DIM));
        // Como en la referencia: unos 38 px libres entre el título y el número.
        let count_w = count.as_ref().map_or(0.0, |g| g.size().x + 38.0);
        let title_color = if resp.hovered() { Color32::WHITE } else { CARD_TITLE };
        let g = galley_truncated(painter, &al.name, theme::regular(CARD_TITLE_FONT), title_color, (side - count_w).max(30.0));
        text_on_baseline(painter, pos2(cover.min.x, base), g, title_color);
        if let Some(g) = count {
            let x = cover.max.x - 1.5 - g.size().x;
            text_on_baseline(painter, pos2(x, base), g, CARD_DIM);
        }
        let g = painter.layout_no_wrap(al.year().to_string(), theme::regular(CARD_SMALL_FONT), CARD_DIM);
        text_on_baseline(painter, pos2(cover.min.x, cover.max.y + 46.0), g, CARD_DIM);
        if resp.clicked() {
            self.actions.push(Action::Go(Page::Album(aid.clone())));
        }
        let uri = al.uri.clone().unwrap_or_else(|| format!("spotify:album:{aid}"));
        resp.context_menu(|ui| {
            if Self::menu_item(ui, Some(Icon::NewTab), "Abrir en una nueva pestaña", false).clicked() {
                self.actions.push(Action::OpenInTab(Page::Album(aid.clone())));
                ui.close();
            }
            if Self::menu_item(ui, Some(Icon::Play), "Reproducir", false).clicked() {
                let shuffle = self.player.shuffle;
                self.actions.push(Action::Play(PlayTarget::Context { uri: uri.clone(), track_uri: None, index: None, shuffle }));
                ui.close();
            }
            if Self::menu_item(ui, Some(Icon::Share), "Copiar enlace", false).clicked() {
                self.actions.push(Action::CopyText(format!("https://open.spotify.com/album/{aid}"), "Enlace"));
                ui.close();
            }
        });
    }

    /// Lista desplegable: cada álbum con portada, datos, acciones y una flecha que muestra
    /// sus canciones (se piden al desplegar y quedan en caché).
    fn album_list(&mut self, ui: &mut egui::Ui, list: &[AlbumRef]) {
        let p = theme::palette(ui.ctx());
        // `--page artist:<id>:1:list:#n` (capturas): "#n" se resuelve al n-ésimo álbum de la lista.
        if let Some(mark) = self.expanded_albums.iter().find(|s| s.starts_with('#')).cloned() {
            self.expanded_albums.remove(&mark);
            if let Some(al) = mark[1..].parse::<usize>().ok().and_then(|n| list.get(n)) {
                if let Some(aid) = &al.id {
                    self.expanded_albums.insert(aid.clone());
                }
            }
        }
        for al in list {
            let Some(aid) = al.id.clone() else { continue };
            let expanded = self.expanded_albums.contains(&aid);
            let width = ui.available_width();
            let (row, resp) = ui.allocate_exact_size(vec2(width, 128.0), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(row, CornerRadius::same(12), p.hover.gamma_multiply(0.5));
            }
            let cover = Rect::from_min_size(pos2(row.min.x + 8.0, row.min.y + 12.0), vec2(104.0, 104.0));
            self.cover_in(ui, al.cover(300), cover, 8);
            let text = Rect::from_min_max(pos2(cover.max.x + 16.0, row.min.y + 10.0), pos2(row.max.x - 44.0, row.max.y));
            let mut t = child_in(ui, text, Layout::top_down(Align::Min));
            t.set_width(text.width());
            t.spacing_mut().item_spacing.y = 4.0;
            t.add(Label::new(RichText::new(&al.name).font(theme::bold(18.0))).truncate());
            let mut meta = al.year().to_string();
            if let Some(n) = al.total_tracks {
                meta.push_str(&format!(" · {n} canciones"));
            }
            if let Some(a) = self.albums.get(&aid) {
                let ms: u64 = a.tracks.as_ref().map(|pg| pg.items.iter().map(|x| x.duration_ms as u64).sum()).unwrap_or(0);
                if ms > 0 {
                    meta.push_str(&format!(" · {}", fmt_total(ms)));
                }
            }
            t.label(RichText::new(meta).small().color(p.weak));
            t.add_space(6.0);
            let uri = al.uri.clone().unwrap_or_else(|| format!("spotify:album:{aid}"));
            let aid2 = aid.clone();
            self.action_row(
                &mut t,
                PlayTarget::Context { uri: uri.clone(), track_uri: None, index: None, shuffle: false },
                PlayTarget::Context { uri: uri.clone(), track_uri: None, index: None, shuffle: true },
                |app, ui| {
                    let p = theme::palette(ui.ctx());
                    let plus = icons::button(ui, Icon::PlusSquare, 34.0, p.weak).on_hover_text("Añadir el álbum a una playlist");
                    let tracks: Vec<String> = app
                        .albums
                        .get(&aid2)
                        .and_then(|a| a.tracks.as_ref().map(|pg| pg.items.iter().map(|x| x.uri.clone()).collect()))
                        .unwrap_or_default();
                    egui::Popup::menu(&plus).show(|ui| {
                        if tracks.is_empty() {
                            ui.label(RichText::new("Despliega el álbum para cargar sus canciones").small());
                        } else {
                            let mine: Vec<(String, String)> = app
                                .playlists
                                .iter()
                                .filter(|pl| app.is_mine(pl) || pl.collaborative.unwrap_or(false))
                                .map(|pl| (pl.id.clone(), pl.name.clone()))
                                .collect();
                            for (pid, pname) in &mine {
                                if Self::menu_item(ui, Some(Icon::Playlist), pname, false).clicked() {
                                    for u in &tracks {
                                        app.actions.push(Action::AddToPlaylist { playlist_id: pid.clone(), uri: u.clone() });
                                    }
                                    ui.close();
                                }
                            }
                        }
                    });
                    if icons::button(ui, Icon::AddToQueue, 34.0, p.weak).on_hover_text("Añadir a la cola").clicked() {
                        for u in &tracks {
                            app.actions.push(Action::AddToQueue(u.clone()));
                        }
                        if tracks.is_empty() {
                            app.status("Despliega el álbum para cargar sus canciones");
                        }
                    }
                    if icons::button(ui, Icon::Share, 34.0, p.weak).on_hover_text("Copiar enlace").clicked() {
                        app.actions.push(Action::CopyText(uri_to_link(&uri), "Enlace"));
                    }
                    if icons::button(ui, Icon::More, 34.0, p.weak).on_hover_text("Abrir el álbum").clicked() {
                        app.actions.push(Action::Go(Page::Album(aid2.clone())));
                    }
                },
            );
            // Flecha desplegar
            let arrow = Rect::from_center_size(pos2(row.max.x - 24.0, row.min.y + 24.0), vec2(28.0, 28.0));
            let ar = ui.interact(arrow, ui.id().with(("exp", &aid)), Sense::click());
            let c = arrow.center();
            let s = 5.0;
            let pts = if expanded {
                vec![pos2(c.x - s, c.y - s / 2.0), pos2(c.x, c.y + s / 2.0), pos2(c.x + s, c.y - s / 2.0)]
            } else {
                vec![pos2(c.x - s / 2.0, c.y - s), pos2(c.x + s / 2.0, c.y), pos2(c.x - s / 2.0, c.y + s)]
            };
            ui.painter().add(egui::Shape::line(pts, egui::Stroke::new(1.6, if ar.hovered() { p.text } else { p.weak })));
            if ar.clicked() || resp.clicked() {
                if expanded {
                    self.expanded_albums.remove(&aid);
                } else {
                    self.expanded_albums.insert(aid.clone());
                    self.request_once(&format!("album:{aid}"), Req::Album(aid.clone()));
                }
            }
            if expanded {
                match self.albums.remove(&aid) {
                    Some(album) => {
                        let tracks: Vec<Track> = album.tracks.as_ref().map(|pg| pg.items.clone()).unwrap_or_default();
                        let uri2 = album.uri.clone();
                        ui.add_space(4.0);
                        self.track_rows(
                            ui,
                            &format!("exp:{aid}"),
                            &tracks,
                            RowOpts { show_cover: false, show_album: false, numbered: true, header: false, source: Source::Context(&uri2), editable_playlist: None, selectable: true, select: false, },
                        );
                        self.albums.insert(aid.clone(), album);
                    }
                    None => {
                        self.request_once(&format!("album:{aid}"), Req::Album(aid.clone()));
                        ui.horizontal(|ui| {
                            ui.add_space(128.0);
                            Self::loading(ui, "Cargando canciones");
                        });
                    }
                }
            }
            ui.add_space(6.0);
            ui.painter().line_segment(
                [pos2(row.min.x + 8.0, ui.cursor().min.y), pos2(row.max.x - 8.0, ui.cursor().min.y)],
                egui::Stroke::new(1.0, p.border),
            );
            ui.add_space(8.0);
        }
    }
}
