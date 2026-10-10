//! Páginas: inicio, biblioteca, historial, búsqueda, playlist, álbum, artista, perfil y ajustes.

use std::time::{Duration, Instant};

use egui::{pos2, vec2, Align, Button, Color32, CornerRadius, Label, Layout, Rect, RichText, Sense};

use super::icons::{self, Icon};
use super::theme::{self, GREEN};
use super::widgets::{child_in, galley_truncated, text_on_baseline, uri_to_link, CardInfo, CardKind, RowOpts, Source, HEADER_H, TABLE_HEAD_H, TABLE_ROW_H};
use super::{fmt_thousands, strip_html, Action, App, Auth, FolderDialog, Page, PlayTarget, ERROR_RED, LIKED};
use super::panels::{about_view, Tone};
use crate::api::Req;
use crate::config::{Loudness, Quality, Theme};
use crate::model::*;

/// Columna derecha de información en playlists y álbumes y su separación de la izquierda (de
/// la referencia de diseño: portada de 320 px a 46 px de la tabla).
const INFO_W: f32 = 320.0;
const INFO_GAP: f32 = 46.0;
/// Título de las páginas y su línea de metadatos.
const TITLE_FONT: f32 = 35.5;
const META_FONT: f32 = 14.5;
/// Inicio (referencia 9.png): chips, estanterías y tarjetas.
const HOME_CHIP_H: f32 = 36.0;
const HOME_CHIP_FONT: f32 = 14.5;
const HOME_SHELF_H: f32 = 343.0;
const HOME_TITLE_FONT: f32 = 20.0;
const HOME_CARD: f32 = 171.0;
const CARD_PITCH: f32 = 181.5;
const HOME_TILE_H: f32 = 262.0;
const HOME_CARD_TITLE_FONT: f32 = 14.75;
const HOME_CARD_SUB_FONT: f32 = 12.75;
/// Chincheta de una sección fijada (verde menta, como en la referencia).
const HOME_PIN: Color32 = Color32::from_rgb(118, 202, 150);
/// Panel «Personalizar inicio»: ancho, centro de la primera fila desde arriba, alto de las filas y
/// verde de la chincheta de una sección fijada.
const CUST_W: f32 = 432.0;
const CUST_FIRST_ROW: f32 = 159.0;
const CUST_ROW_H: f32 = 53.4;
/// Alto del panel en la referencia (con 11 secciones): con más, las filas se desplazan.
const CUST_MAX_H: f32 = 791.0;
const CUST_PIN: Color32 = Color32::from_rgb(85, 200, 130);

impl App {
    pub fn page_ui(&mut self, ui: &mut egui::Ui) {
        match self.page().clone() {
            Page::Home => self.home_page(ui),
            Page::Library => self.library_page(ui),
            Page::History => self.history_page(ui),
            Page::Search => self.search_page(ui),
            Page::Liked => self.liked_page(ui),
            Page::Albums => self.albums_page(ui),
            Page::Artists => self.artists_page(ui),
            Page::Saves => self.saves_page(ui),
            Page::Shows => self.shows_page(ui),
            Page::Audiobooks => self.audiobooks_page(ui),
            Page::Folders => self.folders_page(ui),
            Page::Playlist(id) => self.playlist_page(ui, id),
            Page::Album(id) => self.album_page(ui, id),
            Page::Artist(id) => self.artist_page(ui, id),
            Page::Track(id) => self.track_page(ui, id),
            Page::Show(id) => self.show_page(ui, id),
            Page::User(id) => self.user_page(ui, id),
            Page::Settings => self.settings_page(ui),
        }
    }

    pub fn welcome(&mut self, ui: &mut egui::Ui) {
        if self.diag {
            log::info!("[diag] pantalla de bienvenida pintada");
        }
        let p = theme::palette(ui.ctx());
        ui.add_space(60.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("Nanofy").font(theme::bold(44.0)).color(GREEN));
            ui.label(RichText::new("Spotify nativo, en unos pocos MB.").font(theme::regular(18.0)).color(p.weak));
            ui.add_space(22.0);
            let logging = matches!(self.auth, Auth::LoggingIn);
            let b = Button::new(RichText::new("Iniciar sesión con Spotify").font(theme::regular(15.0)).color(Color32::BLACK).strong())
                .fill(GREEN)
                .corner_radius(CornerRadius::same(20));
            if ui.add_enabled(!logging, b).clicked() {
                self.login();
            }
            if logging {
                ui.add_space(8.0);
                Self::loading(ui, "Completa el inicio de sesión en el navegador");
                // Si se cerró la pestaña o se cambió de idea: antes no había salida y la espera
                // no acababa nunca.
                ui.add_space(6.0);
                let cancelling = self.login_cancelling();
                if Self::secondary_button(ui, if cancelling { "Cancelando…" } else { "Cancelar" }, !cancelling).clicked() {
                    self.cancel_login();
                }
            }
            ui.add_space(16.0);
            // Si la autorización de la biblioteca va a encadenarse, se dice antes: la segunda
            // pantalla de permisos no debe parecer un bucle.
            let chained = !self.api.web_configured() && !self.settings.library_consent_asked;
            let note = if chained {
                "El inicio de sesión se hace en la web de Spotify (OAuth con PKCE); Nanofy nunca ve tu \
                 contraseña. Spotify te pedirá permiso dos veces seguidas: para reproducir y para leer tu \
                 biblioteca (no hace falta crear ninguna app). Para reproducir música hace falta Premium."
            } else {
                "El inicio de sesión se hace en la web de Spotify (OAuth con PKCE); \
                 Nanofy nunca ve tu contraseña. Para reproducir música hace falta Premium."
            };
            ui.label(RichText::new(note).small().color(p.weak));
        });
    }

    // ------------------------------------------------------------ utilidades

    pub(super) fn playlist_card(&mut self, ui: &mut egui::Ui, pl: &Playlist) -> egui::Response {
        let owner = match pl.owner_name() {
            "" => "Playlist".to_string(),
            owner => format!("De {owner}"),
        };
        let pinned = self.settings.pinned.contains(&pl.id);
        let r = self.card(
            ui,
            CardInfo {
                kind: CardKind::Playlist,
                cover: pl.cover(300),
                title: &pl.name,
                subtitle: &owner,
                count: pl.tracks.as_ref().map(|t| t.total),
                pinned,
            },
        );
        if r.clicked() {
            self.actions.push(Action::OpenPlaylist(pl.clone()));
        }
        let mine = self.is_mine(pl);
        r.context_menu(|ui| {
            if Self::menu_item(ui, Some(Icon::NewTab), "Abrir en una nueva pestaña", false).clicked() {
                self.playlist_meta.entry(pl.id.clone()).or_insert_with(|| pl.clone());
                self.actions.push(Action::OpenInTab(Page::Playlist(pl.id.clone())));
                ui.close();
            }
            if Self::menu_item(ui, Some(Icon::Play), "Reproducir", false).clicked() {
                let shuffle = self.player.shuffle;
                self.actions.push(Action::Play(PlayTarget::Context {
                    uri: pl.uri.clone(),
                    track_uri: None,
                    index: None,
                    shuffle,
                }));
                ui.close();
            }
            if Self::menu_item(ui, Some(Icon::Pin), if pinned { "Desfijar" } else { "Fijar" }, false).clicked() {
                self.actions.push(Action::Pin(pl.id.clone(), !pinned));
                ui.close();
            }
            if mine && Self::menu_item(ui, Some(Icon::Edit), "Editar…", false).clicked() {
                self.actions.push(Action::OpenEditor(Some(pl.clone())));
                ui.close();
            }
            if mine {
                self.invite_menu_item(ui, &pl.id);
            }
            self.folder_menu(ui, &pl.id);
            if Self::menu_item(ui, Some(Icon::Share), "Copiar enlace", false).clicked() {
                self.actions.push(Action::CopyText(uri_to_link(&pl.uri), "Enlace"));
                ui.close();
            }
        });
        r
    }

    pub(super) fn album_card(&mut self, ui: &mut egui::Ui, id: &str, cover: Option<&str>, name: &str, subtitle: &str, count: Option<u32>) {
        let r = self.card(
            ui,
            CardInfo {
                kind: CardKind::Album,
                cover,
                title: name,
                subtitle,
                count,
                pinned: false,
            },
        );
        if r.clicked() {
            self.actions.push(Action::Go(Page::Album(id.to_string())));
        }
        let id = id.to_string();
        r.context_menu(|ui| {
            if Self::menu_item(ui, Some(Icon::NewTab), "Abrir en una nueva pestaña", false).clicked() {
                self.actions.push(Action::OpenInTab(Page::Album(id.clone())));
                ui.close();
            }
            if Self::menu_item(ui, Some(Icon::Play), "Reproducir", false).clicked() {
                let shuffle = self.player.shuffle;
                self.actions.push(Action::Play(PlayTarget::Context {
                    uri: format!("spotify:album:{id}"),
                    track_uri: None,
                    index: None,
                    shuffle,
                }));
                ui.close();
            }
            if Self::menu_item(ui, Some(Icon::Share), "Copiar enlace", false).clicked() {
                self.actions.push(Action::CopyText(format!("https://open.spotify.com/album/{id}"), "Enlace"));
                ui.close();
            }
        });
    }

    fn artist_card(&mut self, ui: &mut egui::Ui, a: &Artist) {
        let sub = a.genres.first().cloned().unwrap_or_else(|| "Artista".into());
        let r = self.card(
            ui,
            CardInfo {
                kind: CardKind::Artist,
                cover: a.cover(300),
                title: &a.name,
                subtitle: &sub,
                count: None,
                pinned: false,
            },
        );
        if r.clicked() {
            self.actions.push(Action::Go(Page::Artist(a.id.clone())));
        }
        let id = a.id.clone();
        r.context_menu(|ui| {
            if Self::menu_item(ui, Some(Icon::NewTab), "Abrir en una nueva pestaña", false).clicked() {
                self.actions.push(Action::OpenInTab(Page::Artist(id.clone())));
                ui.close();
            }
            if Self::menu_item(ui, Some(Icon::Share), "Copiar enlace", false).clicked() {
                self.actions.push(Action::CopyText(format!("https://open.spotify.com/artist/{id}"), "Enlace"));
                ui.close();
            }
        });
    }

    /// Fila horizontal de tarjetas con scroll lateral (no se corta nada).
    pub(super) fn cards_row(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui)) {
        egui::ScrollArea::horizontal()
            .id_salt(id)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    add(ui);
                });
            });
    }

    fn section_header(ui: &mut egui::Ui, title: &str, link: Option<&str>) -> bool {
        let p = theme::palette(ui.ctx());
        let mut clicked = false;
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).font(theme::bold(20.0)));
            if let Some(l) = link {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let r = ui.add(Label::new(RichText::new(l).small().color(p.weak)).sense(Sense::click()));
                    if r.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    clicked = r.clicked();
                });
            }
        });
        ui.add_space(6.0);
        clicked
    }

    /// Título grande de página con línea de metadatos debajo.
    fn page_title(ui: &mut egui::Ui, kind: &str, title: &str, meta: &str) {
        let p = theme::palette(ui.ctx());
        if !kind.is_empty() {
            ui.label(RichText::new(kind).small().strong().color(p.weak));
        }
        ui.add(Label::new(RichText::new(title).font(theme::bold(TITLE_FONT))).truncate());
        ui.add_space(2.0);
        ui.add(Label::new(RichText::new(meta).font(theme::regular(META_FONT)).color(p.weak)).truncate());
        ui.add_space(10.0);
    }

    /// Cabecera de una playlist, un álbum o Me gusta, copia de la referencia de diseño: el título
    /// en negrita y debajo «De <autores> • <partes>», con los autores en blanco y en negrita
    /// (enlaces a su página). Ocupa `HEADER_H`: la fila de botones va justo debajo. `tip`: globo
    /// del título (la descripción de la playlist).
    fn collection_header(&mut self, ui: &mut egui::Ui, title: &str, tip: Option<&str>, by: &[(String, Option<Page>)], parts: &[String]) {
        let p = theme::palette(ui.ctx());
        let ink = theme::ink(&p);
        let w = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(vec2(w, HEADER_H), Sense::hover());
        let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        let x = rect.min.x;
        let g = galley_truncated(&painter, title, theme::bold(TITLE_FONT), ink.strong, w);
        let tr = text_on_baseline(&painter, pos2(x, rect.min.y + 40.0), g, ink.strong);
        if let Some(tip) = tip {
            ui.interact(tr, ui.id().with("header_title"), Sense::hover()).on_hover_text(tip);
        }
        let base = rect.min.y + 84.0;
        let font = theme::regular(META_FONT);
        let mut cx = x;
        if !by.is_empty() {
            let g = painter.layout_no_wrap("De ".into(), font.clone(), ink.dim);
            cx = text_on_baseline(&painter, pos2(cx, base), g, ink.dim).max.x;
            for (i, (name, page)) in by.iter().enumerate() {
                if i > 0 {
                    let g = painter.layout_no_wrap(", ".into(), font.clone(), ink.dim);
                    cx = text_on_baseline(&painter, pos2(cx, base), g, ink.dim).max.x;
                }
                let g = painter.layout_no_wrap(name.clone(), theme::bold(META_FONT), ink.strong);
                let r = text_on_baseline(&painter, pos2(cx, base), g, ink.strong);
                if let Some(page) = page {
                    let resp = ui.interact(r, ui.id().with(("header_by", i)), Sense::click());
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        painter.line_segment([pos2(r.min.x, base + 2.0), pos2(r.max.x, base + 2.0)], egui::Stroke::new(1.0, ink.strong));
                    }
                    if resp.clicked() {
                        self.actions.push(Action::Go(page.clone()));
                    }
                }
                cx = r.max.x;
            }
        }
        for (i, part) in parts.iter().enumerate() {
            if i > 0 || !by.is_empty() {
                // Punto de 7 px entre partes, a media altura de las minúsculas.
                let dot = pos2(cx + 10.5, base - 5.5);
                painter.circle_filled(dot, 3.0, ink.dim);
                cx = dot.x + 9.5;
            }
            let g = painter.layout_no_wrap(part.clone(), font.clone(), ink.dim);
            cx = text_on_baseline(&painter, pos2(cx, base), g, ink.dim).max.x;
        }
    }

    /// Dos columnas cuando hay sitio: contenido a la izquierda, tarjeta de información a la derecha.
    fn two_columns(
        &mut self,
        ui: &mut egui::Ui,
        left: impl FnOnce(&mut Self, &mut egui::Ui),
        right: impl FnOnce(&mut Self, &mut egui::Ui),
    ) {
        let width = ui.available_width();
        if width < 900.0 {
            left(self, ui);
            return;
        }
        let top = ui.cursor().min;
        let left_w = width - INFO_W - INFO_GAP;
        let left_rect = Rect::from_min_size(top, vec2(left_w, f32::INFINITY));
        let right_rect = Rect::from_min_size(pos2(top.x + left_w + INFO_GAP, top.y), vec2(INFO_W, f32::INFINITY));
        let mut l = child_in(ui, left_rect, Layout::top_down(Align::Min));
        l.set_width(left_w);
        left(self, &mut l);
        let mut r = child_in(ui, right_rect, Layout::top_down(Align::Min));
        r.set_width(INFO_W);
        right(self, &mut r);
        let h = l.min_rect().height().max(r.min_rect().height());
        ui.allocate_rect(Rect::from_min_size(top, vec2(width, h)), Sense::hover());
    }

    /// Tarjeta de información: portada grande, chips y artistas con avatar. `ready`: la lista ya
    /// no cambia (terminó de cargar). Solo entonces se piden las imágenes que falten, todas en
    /// una petición: mientras llegan lotes, los artistas más presentes aún van cambiando.
    fn info_card(&mut self, ui: &mut egui::Ui, cover: Option<&str>, chips: &[String], artists: &[ArtistRef], ready: bool) {
        let p = theme::palette(ui.ctx());
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        let side = ui.available_width().min(INFO_W);
        let (rect, _) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
        self.cover_in(ui, cover, rect, 6);
        if cover.is_none() {
            icons::paint(ui.painter(), rect.shrink(side * 0.3), p.faint, Icon::Album);
        }
        self.info_chips_and_artists(ui, chips, artists, ready);
    }

    /// Debajo de la portada de la columna derecha (referencia de diseño): chips de contorno de
    /// 39 px a 30 px de la portada y los artistas con su foto redonda de 60 px, uno cada 76 px.
    fn info_chips_and_artists(&mut self, ui: &mut egui::Ui, chips: &[String], artists: &[ArtistRef], ready: bool) {
        let p = theme::palette(ui.ctx());
        let ink = theme::ink(&p);
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        let w = ui.available_width();
        if !chips.is_empty() {
            ui.add_space(30.0);
            let font = theme::regular(15.5);
            let galleys: Vec<_> = chips.iter().map(|c| ui.painter().layout_no_wrap(c.clone(), font.clone(), ink.chip_text)).collect();
            // Posiciones por filas: 10 px entre chips y entre filas.
            let mut placed = Vec::with_capacity(galleys.len());
            let (mut x, mut row) = (0.0f32, 0usize);
            for g in &galleys {
                let cw = (g.size().x + 37.0).round().min(w);
                if x > 0.0 && x + cw > w {
                    x = 0.0;
                    row += 1;
                }
                placed.push((x, row, cw));
                x += cw + 10.0;
            }
            let rows = row + 1;
            let (area, _) = ui.allocate_exact_size(vec2(w, rows as f32 * 49.0 - 10.0), Sense::hover());
            for ((x, row, cw), g) in placed.into_iter().zip(galleys) {
                let chip = Rect::from_min_size(pos2(area.min.x + x, area.min.y + row as f32 * 49.0), vec2(cw, 39.0));
                ui.painter().rect_stroke(chip, CornerRadius::same(20), egui::Stroke::new(1.6, ink.chip), egui::StrokeKind::Inside);
                text_on_baseline(ui.painter(), pos2(chip.min.x + 19.0, chip.min.y + 25.0), g, ink.chip_text);
            }
        }
        if artists.is_empty() {
            return;
        }
        ui.add_space(if chips.is_empty() { 30.0 } else { 31.0 });
        let mut missing: Vec<String> = Vec::new();
        for (i, a) in artists.iter().take(10).enumerate() {
            if i > 0 {
                ui.add_space(16.0);
            }
            let img = a
                .id
                .as_ref()
                .and_then(|id| self.artists.get(id))
                .and_then(|pg| pg.artist.as_ref())
                .and_then(|ar| ar.cover(64).map(|s| s.to_string()));
            let (row, r) = ui.allocate_exact_size(vec2(w, 60.0), Sense::click());
            let avatar = Rect::from_min_size(row.min, vec2(60.0, 60.0));
            match &img {
                Some(u) => self.cover_in(ui, Some(u), avatar, 30),
                None => {
                    ui.painter().circle_filled(avatar.center(), 30.0, p.hover);
                    let initial = a.name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
                    ui.painter().text(avatar.center(), egui::Align2::CENTER_CENTER, initial, theme::regular(22.0), ink.strong);
                }
            }
            let color = if r.hovered() && a.id.is_some() { ink.strong } else { ink.name };
            let g = galley_truncated(ui.painter(), &a.name, theme::regular(17.0), color, (w - 80.0).max(20.0));
            text_on_baseline(ui.painter(), pos2(row.min.x + 80.0, avatar.center().y + 5.5), g, color);
            if r.hovered() && a.id.is_some() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if r.clicked() {
                if let Some(id) = &a.id {
                    self.actions.push(Action::Go(Page::Artist(id.clone())));
                }
            }
            if let Some(id) = &a.id {
                if ready && img.is_none() {
                    missing.push(id.clone());
                }
            }
        }
        // Las imágenes que falten, de una vez y una sola vez por artista (metadatos ligeros).
        if !missing.is_empty() {
            self.request_artist_thumbs(missing);
        }
    }

    /// Artistas más frecuentes de una lista de pistas.
    fn top_artists(tracks: &[Track], n: usize) -> Vec<ArtistRef> {
        // Cada artista (id y nombre, como antes) lleva a su puesto en `counts`, que conserva el
        // orden de primera aparición: buscarlo recorriendo `counts` era O(pistas × artistas), y
        // la ordenación estable deja los empates en el mismo orden que antes.
        let mut index: std::collections::HashMap<(Option<&str>, &str), usize> = std::collections::HashMap::new();
        let mut counts: Vec<(&ArtistRef, usize)> = Vec::new();
        for t in tracks {
            for a in &t.artists {
                match index.entry((a.id.as_deref(), a.name.as_str())) {
                    std::collections::hash_map::Entry::Occupied(e) => counts[*e.get()].1 += 1,
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(counts.len());
                        counts.push((a, 1));
                    }
                }
            }
        }
        counts.sort_by(|x, y| y.1.cmp(&x.1));
        counts.into_iter().take(n).map(|(a, _)| a.clone()).collect()
    }

    /// Resumen de una lista (duración, artistas más presentes, quién añadió canciones), guardado
    /// por versión: solo se recalcula cuando cambian sus pistas, no en cada fotograma. La lista
    /// llega aparte porque las páginas la sacan de `lists` mientras se dibujan.
    fn list_summary(&mut self, key: &str, list: &crate::app::TrackList) -> std::rc::Rc<crate::app::ListSummary> {
        // La longitud también cuenta, por si algún cambio se quedara sin su versión nueva.
        if let Some((gen, len, s)) = self.page_cache.get(key) {
            if *gen == list.gen && *len == list.tracks.len() {
                return s.clone();
            }
        }
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let added_by = list
            .tracks
            .iter()
            .filter_map(|t| t.added_by.as_deref())
            .filter(|b| seen.insert(b))
            .map(str::to_string)
            .collect();
        let s = std::rc::Rc::new(crate::app::ListSummary {
            total_ms: list.tracks.iter().map(|t| t.duration_ms as u64).sum(),
            top: Self::top_artists(&list.tracks, 5),
            added_by,
        });
        self.page_cache.insert(key.to_string(), (list.gen, list.tracks.len(), s.clone()));
        s
    }

    // ------------------------------------------------------------------ inicio

    fn home_page(&mut self, ui: &mut egui::Ui) {
        if !self.signed_in() {
            self.welcome(ui);
            return;
        }
        let p = theme::palette(ui.ctx());
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        // Chips (referencia 9.png): rectángulos de 36 px con esquinas de 6, 16 px de relleno y 9,5
        // entre ellos; el elegido, claro con el texto negro. A la derecha, personalizar.
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), HOME_CHIP_H), Sense::hover());
        let mut x = row.min.x;
        for (i, label) in ["Todo", "Música", "Podcasts", "Audiolibros"].iter().enumerate() {
            let r = Self::home_chip(ui, pos2(x, row.min.y), label, self.home_filter == i as u8);
            if r.clicked() {
                self.home_filter = i as u8;
            }
            x = r.rect.max.x + 9.5;
        }
        let b = Self::slot_button(ui, egui::Id::new("home_customize_btn"), pos2(row.max.x - 24.7, row.center().y - 1.7), Icon::Customize, 28.0, Color32::from_gray(143));
        let b = if self.home_customize_open { b } else { b.on_hover_text("Personalizar el inicio") };
        if b.clicked() {
            self.home_customize_open = !self.home_customize_open;
        }
        if self.home_customize_once {
            self.home_customize_once = false;
            self.home_customize_open = true;
        }
        if self.home_customize_open {
            let ctx = ui.ctx().clone();
            self.home_customize_panel(&ctx, b.rect);
        }
        if !self.api.web_configured() {
            ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
            ui.add_space(12.0);
            self.web_api_banner(ui);
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        }
        if self.home_feed.is_empty() {
            ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
            ui.add_space(8.0);
            Self::loading(ui, "Preparando tu inicio");
            self.home_page_legacy(ui);
            return;
        }
        let f = self.home_filter;
        let sections = self.home_sections();
        let hidden = self.settings.home_hidden.clone();
        let pinned = self.settings.home_pinned.clone();
        let recs = self.settings.home_recs;
        let mut shown = 0;
        for sec in &sections {
            if hidden.contains(&sec.id) || (!recs && sec.is_recommendation()) {
                continue;
            }
            let cat = sec.category();
            let ok = match f {
                1 => cat == 0,
                2 => cat == 1,
                3 => cat == 2,
                _ => true,
            };
            if !ok {
                continue;
            }
            shown += 1;
            self.home_shelf(ui, sec, pinned.contains(&sec.id));
        }
        if shown == 0 {
            ui.add_space(24.0);
            let msg = match f {
                2 => "Spotify todavía no te recomienda podcasts; escucha alguno y aparecerán aquí.",
                3 => "Spotify todavía no te recomienda audiolibros; escucha alguno y aparecerán aquí.",
                _ => "No hay secciones que mostrar. Actívalas desde el botón de personalizar, arriba a la derecha.",
            };
            ui.label(RichText::new(msg).color(p.weak));
        }
    }

    /// Todas las secciones del inicio (playlists añadidas + feed), en el orden del usuario y
    /// con las fijadas primero. Mantiene `settings.home_order` al día.
    fn home_sections(&mut self) -> Vec<HomeSection> {
        // Caché: la lista solo cambia con el feed, los ajustes o las playlists añadidas.
        let key = (
            self.home_feed.len(),
            self.home_feed.iter().map(|s| s.items.len()).sum::<usize>(),
            self.settings.home_order.len(),
            self.settings.home_pinned.clone(),
            self.settings.home_hidden.len(),
            self.settings.home_custom.clone(),
            self.settings.home_custom.iter().map(|p| self.lists.get(p).map(|l| l.tracks.len()).unwrap_or(0)).sum::<usize>(),
            self.settings.home_order.clone(),
            // Al llegar (o fallar) el listado de esta sesión, las secciones de playlists que aún
            // no se pidieron miran si su copia sigue al día (ensure_playlist).
            self.listing_settled(),
        );
        if let Some((k, cached)) = &self.home_cache {
            if *k == key {
                return cached.clone();
            }
        }
        let out = self.home_sections_uncached();
        self.home_cache = Some((key, out.clone()));
        out
    }

    fn home_sections_uncached(&mut self) -> Vec<HomeSection> {
        let mut all: Vec<HomeSection> = Vec::new();
        // Playlists de la biblioteca añadidas como sección: sus canciones como tarjetas.
        for pid in self.settings.home_custom.clone() {
            let Some(pl) = self.playlists.iter().find(|p| p.id == pid).cloned() else { continue };
            // Misma clave que la página de la playlist: una sola carga por playlist, y ninguna
            // si su copia en disco sigue al día. Con copia a la vista se espera al listado que se
            // está pidiendo: si no, al arrancar (antes de que llegue) se pedían siempre.
            self.warm_list(&pid);
            if self.listing_settled() || !self.lists.contains_key(&pid) {
                self.ensure_playlist(&pid);
            }
            let items: Vec<HomeItem> = self
                .lists
                .get(&pid)
                .map(|l| {
                    l.tracks
                        .iter()
                        .take(20)
                        .map(|t| HomeItem {
                            uri: t.uri.clone(),
                            title: t.name.clone(),
                            subtitle: t.artists_str(),
                            image: t.cover(300).map(|s| s.to_string()),
                            context: Some(pl.uri.clone()),
                        })
                        .collect()
                })
                .unwrap_or_default();
            all.push(HomeSection { id: format!("custom:{pid}"), title: pl.name.clone(), items });
        }
        let made_for = |s: &HomeSection| {
            let t = s.title.to_lowercase();
            t.starts_with("hecho para") || t.starts_with("made for")
        };
        for s in self.home_feed.iter().filter(|s| made_for(s)).chain(self.home_feed.iter().filter(|s| !made_for(s))) {
            all.push(s.clone());
        }
        // Orden guardado: lo conocido en su sitio, lo nuevo al final.
        let mut changed = false;
        let known: Vec<String> = self.settings.home_order.iter().filter(|id| all.iter().any(|s| &s.id == *id)).cloned().collect();
        if known.len() != self.settings.home_order.len() {
            changed = true;
        }
        let mut order = known;
        for s in &all {
            if !order.contains(&s.id) {
                order.push(s.id.clone());
                changed = true;
            }
        }
        if changed {
            self.settings.home_order = order.clone();
            self.settings.save(&self.paths);
        }
        let pos = |id: &str| order.iter().position(|x| x == id).unwrap_or(usize::MAX);
        let pinned = self.settings.home_pinned.clone();
        all.sort_by_key(|s| (!pinned.contains(&s.id), pos(&s.id)));
        all
    }

    /// Panel «Personalizar inicio» (referencia 11.webp): cristal desenfocado de 432 px anclado al
    /// botón, con la cabecera, «Seleccionar de la biblioteca», una fila por sección (chincheta,
    /// nombre, asa para reordenar arrastrando y ojo para ocultar) cada 53,4 px y, abajo, el
    /// interruptor de las filas recomendadas. Se cierra al pulsar fuera o con Escape.
    fn home_customize_panel(&mut self, ctx: &egui::Context, button: Rect) {
        let p = theme::palette(ctx);
        let bc = button.center();
        let sections = self.home_sections();
        let screen = ctx.content_rect();
        let top = bc.y + 32.0;
        let natural = CUST_FIRST_ROW + (sections.len().max(1) as f32 - 1.0) * CUST_ROW_H + 41.5 + 56.5;
        // Como mucho la altura de la referencia (11 filas) y siempre 16,5 px por encima del
        // reproductor; las filas que no caben se desplazan dentro del panel.
        let bottom_limit = screen.max.y - super::PLAYER_PANEL_H - 16.5;
        let h = natural.min(CUST_MAX_H).min(bottom_limit - top).max(260.0);
        let rect = Rect::from_min_size(pos2(bc.x + 28.0 - CUST_W, top), vec2(CUST_W, h));
        let (l, r) = (rect.min.x, rect.max.x);
        let dark = p.dark;
        let ink = |v: u8| if dark { Color32::from_gray(v) } else { p.text.gamma_multiply(v as f32 / 255.0 + 0.2) };
        let sub_id = egui::Id::new("home_customize_pick");
        egui::Area::new(egui::Id::new("home_customize_panel"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .show(ctx, |ui| {
                // Todo el panel recoge el puntero: lo de debajo no reacciona.
                ui.allocate_rect(rect, Sense::click_and_drag());
                let painter = ui.painter().clone();
                painter.add(egui::Shape::Callback(egui::epaint::PaintCallback {
                    rect,
                    callback: std::sync::Arc::new(crate::raster::BackdropBlur { sigma: 28.0, corner: 9.0 }),
                }));
                let glass = if dark { Color32::from_rgba_unmultiplied(35, 35, 35, 191) } else { Color32::from_rgba_unmultiplied(250, 250, 250, 225) };
                painter.rect_filled(rect, CornerRadius::same(9), glass);
                let line = if dark { Color32::from_white_alpha(28) } else { p.border };

                // Cabecera
                let g = painter.layout_no_wrap("Personalizar inicio".into(), theme::regular(15.0), ink(231));
                text_on_baseline(&painter, pos2(l + 22.5, rect.min.y + 33.0), g, ink(231));
                painter.line_segment([pos2(l, rect.min.y + 52.5), pos2(r, rect.min.y + 52.5)], egui::Stroke::new(1.0, line));

                // Seleccionar de la biblioteca (lista de playlists que aún no están en el inicio)
                let sy = rect.min.y + 99.0;
                let srow = Rect::from_min_max(pos2(l + 8.0, sy - 24.0), pos2(r - 8.0, sy + 24.0));
                let sr = ui.interact(srow, egui::Id::new("home_customize_select"), Sense::click());
                if sr.hovered() {
                    painter.rect_filled(srow, CornerRadius::same(6), Color32::from_white_alpha(8));
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                let pc = ink(234);
                painter.line_segment([pos2(l + 44.0, sy - 7.5), pos2(l + 44.0, sy + 7.5)], egui::Stroke::new(1.6, pc));
                painter.line_segment([pos2(l + 36.5, sy), pos2(l + 51.5, sy)], egui::Stroke::new(1.6, pc));
                let g = painter.layout_no_wrap("Seleccionar de la biblioteca".into(), theme::regular(17.5), pc);
                text_on_baseline(&painter, pos2(l + 74.0, sy + 6.0), g, pc);
                let mine: Vec<(String, String)> = self
                    .playlists
                    .iter()
                    .filter(|pl| !self.settings.home_custom.contains(&pl.id))
                    .map(|pl| (pl.id.clone(), pl.name.clone()))
                    .collect();
                egui::Popup::menu(&sr).id(sub_id).show(|ui| {
                    ui.set_min_width(240.0);
                    if mine.is_empty() {
                        ui.label(RichText::new("Todas tus playlists ya están en el inicio.").small().color(p.weak));
                    }
                    egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                        for (pid, name) in &mine {
                            if Self::menu_item(ui, Some(Icon::Playlist), name, false).clicked() {
                                self.settings.home_custom.push(pid.clone());
                                self.settings.save(&self.paths);
                                ui.close();
                            }
                        }
                    });
                });

                // Filas de las secciones (con scroll si no caben)
                let rows_rect = Rect::from_min_max(pos2(l, rect.min.y + CUST_FIRST_ROW - CUST_ROW_H / 2.0), pos2(r, rect.max.y - 56.5 - 41.5 + CUST_ROW_H / 2.0));
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rows_rect));
                egui::ScrollArea::vertical().id_salt("home_customize_rows").max_height(rows_rect.height()).show(&mut child, |ui| {
                    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                    for sec in &sections {
                        let pinned = self.settings.home_pinned.contains(&sec.id);
                        let hidden = self.settings.home_hidden.contains(&sec.id);
                        let dragging = self.home_drag.as_deref() == Some(sec.id.as_str());
                        let (row, rr) = ui.allocate_exact_size(vec2(CUST_W, CUST_ROW_H), Sense::hover());
                        let rc = row.center().y;
                        let pnt = ui.painter().clone();
                        if dragging || rr.hovered() {
                            pnt.rect_filled(row.shrink2(vec2(8.0, 3.0)), CornerRadius::same(6), Color32::from_white_alpha(if dragging { 14 } else { 8 }));
                        }
                        // Chincheta
                        let pin_c = pos2(l + 45.0, rc - 2.5);
                        let pin_col = if pinned { CUST_PIN } else if hidden { ink(90) } else { ink(144) };
                        let pr = Self::slot_button(ui, ui.id().with(("cust_pin", &sec.id)), pin_c, Icon::Pin, 30.0, pin_col)
                            .on_hover_text(if pinned { "Desfijar" } else { "Fijar arriba" });
                        if pr.clicked() {
                            self.settings.home_pinned.retain(|x| x != &sec.id);
                            if !pinned {
                                self.settings.home_pinned.insert(0, sec.id.clone());
                            }
                            self.settings.save(&self.paths);
                        }
                        // Nombre
                        let custom = sec.id.starts_with("custom:");
                        let tc = if hidden { ink(142) } else { ink(245) };
                        let text_w = if custom { 322.0 - 14.0 - 74.0 } else { 347.0 - 20.0 - 74.0 };
                        let g = galley_truncated(&pnt, &sec.title, theme::regular(14.5), tc, text_w);
                        text_on_baseline(&pnt, pos2(l + 74.0, rc + 5.0), g, tc);
                        if custom {
                            let xr = Self::slot_button(ui, ui.id().with(("cust_rm", &sec.id)), pos2(l + 322.0, rc), Icon::Close, 14.0, ink(142)).on_hover_text("Quitar del inicio");
                            if xr.clicked() {
                                let pid = sec.id.trim_start_matches("custom:").to_string();
                                self.settings.home_custom.retain(|x| x != &pid);
                                self.settings.save(&self.paths);
                            }
                        }
                        // Asa para arrastrar
                        let hr = Rect::from_center_size(pos2(l + 347.0, rc - 1.5), vec2(26.0, 34.0));
                        let hh = ui.interact(hr, ui.id().with(("cust_drag", &sec.id)), Sense::drag());
                        let hc = if hh.hovered() || dragging { ink(220) } else if hidden { ink(90) } else { ink(142) };
                        icons::paint(&pnt, Rect::from_center_size(pos2(l + 347.0, rc - 1.5), vec2(28.0, 28.0)), hc, Icon::DragHandle);
                        if hh.hovered() || dragging {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                        }
                        if hh.drag_started() {
                            self.home_drag = Some(sec.id.clone());
                        }
                        // Ojo: ocultar o mostrar
                        let (eye, ec, ey) = if hidden { (Icon::EyeOff, ink(91), rc + 3.0) } else { (Icon::Eye, ink(144), rc - 2.0) };
                        let er = Self::slot_button(ui, ui.id().with(("cust_eye", &sec.id)), pos2(l + 393.5, ey), eye, 28.0, ec)
                            .on_hover_text(if hidden { "Mostrar" } else { "Ocultar" });
                        if er.clicked() {
                            if hidden {
                                self.settings.home_hidden.retain(|x| x != &sec.id);
                            } else {
                                self.settings.home_hidden.push(sec.id.clone());
                            }
                            self.settings.save(&self.paths);
                        }
                    }
                });

                // Pie: filas recomendadas
                let fy = rect.max.y;
                painter.line_segment([pos2(l, fy - 56.5), pos2(r, fy - 56.5)], egui::Stroke::new(1.0, line));
                let g = painter.layout_no_wrap("Permitir filas recomendadas".into(), theme::regular(14.5), ink(226));
                text_on_baseline(&painter, pos2(l + 30.5, fy - 22.0), g, ink(226));
                let on = self.settings.home_recs;
                let track = Rect::from_min_max(pos2(l + 352.0, fy - 28.5 - 12.0), pos2(l + 399.5, fy - 28.5 + 12.0));
                let tr = ui.interact(Rect::from_min_max(pos2(l + 8.0, fy - 52.0), pos2(r - 8.0, fy - 6.0)), egui::Id::new("home_customize_recs"), Sense::click());
                painter.rect_filled(track, CornerRadius::same(12), if dark { Color32::from_gray(17) } else { p.hover });
                let knob_x = if on { l + 388.5 } else { l + 364.0 };
                painter.circle_filled(pos2(knob_x, fy - 28.5), 10.0, if on { GREEN } else { Color32::from_gray(150) });
                if tr.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if tr.clicked() {
                    self.settings.home_recs = !on;
                    self.settings.save(&self.paths);
                }
            });

        // Arrastre: la sección arrastrada toma el sitio de la fila bajo el puntero.
        if let Some(d) = self.home_drag.clone() {
            let released = ctx.input(|i| i.pointer.any_released());
            if let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) {
                // La fila bajo el puntero, por su posición (todas miden lo mismo).
                let first = rect.min.y + CUST_FIRST_ROW - CUST_ROW_H / 2.0;
                let k = ((pos.y - first) / CUST_ROW_H).floor();
                if k >= 0.0 {
                    if let Some(target) = sections.get(k as usize).map(|s| s.id.clone()) {
                        if target != d {
                            let order = &mut self.settings.home_order;
                            if let (Some(from), Some(to)) = (order.iter().position(|x| x == &d), order.iter().position(|x| x == &target)) {
                                let item = order.remove(from);
                                order.insert(to, item);
                                self.settings.save(&self.paths);
                            }
                        }
                    }
                }
            }
            if released {
                self.home_drag = None;
            }
            ctx.request_repaint();
        }

        // Cerrar al pulsar fuera (salvo en la lista de playlists) o con Escape.
        let pressed_outside = ctx.input(|i| {
            i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|q| !rect.contains(q) && !button.contains(q))
        });
        if (pressed_outside && !egui::Popup::is_id_open(ctx, sub_id)) || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.home_customize_open = false;
        }
    }

    /// Estantería del inicio: título, flechas para desplazar, menú (fijar / ocultar) y tarjetas.
    /// Chip de filtro del inicio en `at` (esquina de arriba a la izquierda).
    fn home_chip(ui: &mut egui::Ui, at: egui::Pos2, label: &str, selected: bool) -> egui::Response {
        Self::chip(ui, at, label, selected, 36)
    }

    /// Chip de filtro de la referencia: rectángulo de 36 px con esquinas de 6 y 16 px de relleno;
    /// el elegido, claro con el texto negro; los demás, gris `rest` (Inicio 36, Buscar 24).
    fn chip(ui: &mut egui::Ui, at: egui::Pos2, label: &str, selected: bool, rest: u8) -> egui::Response {
        let p = theme::palette(ui.ctx());
        let font = theme::regular(HOME_CHIP_FONT);
        let g = ui.painter().layout_no_wrap(label.to_string(), font.clone(), Color32::WHITE);
        let rect = Rect::from_min_size(at, vec2((g.size().x + 32.0).round(), HOME_CHIP_H));
        let resp = ui.interact(rect, ui.id().with(("home_chip", label)), Sense::click());
        let (fill, color) = match (selected, p.dark) {
            (true, true) => (Color32::from_gray(225), Color32::BLACK),
            (true, false) => (p.text, p.card),
            (false, true) => (if resp.hovered() { Color32::from_gray(rest + 10) } else { Color32::from_gray(rest) }, Color32::from_gray(224)),
            (false, false) => (if resp.hovered() { p.hover.lerp_to_gamma(p.text, 0.08) } else { p.hover }, p.text),
        };
        ui.painter().rect_filled(rect, CornerRadius::same(6), fill);
        let g = ui.painter().layout_no_wrap(label.to_string(), font, color);
        let x = rect.center().x - g.size().x / 2.0;
        text_on_baseline(ui.painter(), pos2(x, rect.center().y + 5.7), g, color);
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        resp
    }

    /// Estantería del inicio (referencia 9.png): bloque de 343 px con el título en negrita de 20 y
    /// la línea base a 53 del borde de arriba, la chincheta verde detrás del título si está fijada
    /// y, a la derecha, ← → y «⋯»; debajo, las tarjetas en una tira con scroll lateral.
    fn home_shelf(&mut self, ui: &mut egui::Ui, sec: &HomeSection, pinned: bool) {
        let p = theme::palette(ui.ctx());
        let (offset, max) = self.home_offsets.get(&sec.id).copied().unwrap_or((0.0, 0.0));
        let width = ui.available_width();
        let (block, _) = ui.allocate_exact_size(vec2(width, HOME_SHELF_H), Sense::hover());
        let base = block.min.y + 52.0;
        let title_color = if p.dark { Color32::from_gray(241) } else { p.text };
        let g = galley_truncated(ui.painter(), &sec.title, theme::bold(HOME_TITLE_FONT), title_color, (width - 220.0).max(80.0));
        let tr = text_on_baseline(ui.painter(), pos2(block.min.x, base), g, title_color);
        if pinned {
            icons::paint(ui.painter(), Rect::from_center_size(pos2(tr.max.x + 35.0, base - 8.1), vec2(27.0, 27.0)), HOME_PIN, Icon::Pin);
        }
        let page = (width - CARD_PITCH).max(CARD_PITCH);
        let can_next = offset + 1.0 < max;
        let can_prev = offset > 1.0;
        let arrow = |on: bool| if on { Color32::from_gray(146) } else { Color32::from_gray(75) };
        let id = ui.id().with(("home_shelf", &sec.id));
        let ay = base - 8.0;
        if Self::slot_button(ui, id.with("prev"), pos2(block.max.x - 112.4, ay), Icon::ArrowLeft, 24.0, arrow(can_prev)).clicked() && can_prev {
            self.home_scroll.insert(sec.id.clone(), (offset - page).max(0.0));
        }
        if Self::slot_button(ui, id.with("next"), pos2(block.max.x - 67.8, ay), Icon::ArrowRight, 24.0, arrow(can_next)).clicked() && can_next {
            self.home_scroll.insert(sec.id.clone(), (offset + page).min(max));
        }
        let more = Self::slot_button(ui, id.with("more"), pos2(block.max.x - 22.6, ay), Icon::More, 29.0, Color32::from_gray(149)).on_hover_text("Opciones de la sección");
        egui::Popup::menu(&more).show(|ui| {
            ui.set_min_width(190.0);
            let pin_label = if pinned { "Desfijar del inicio" } else { "Fijar arriba del inicio" };
            if Self::menu_item(ui, Some(Icon::Pin), pin_label, false).clicked() {
                self.settings.home_pinned.retain(|x| x != &sec.id);
                if !pinned {
                    self.settings.home_pinned.insert(0, sec.id.clone());
                }
                self.settings.save(&self.paths);
                ui.close();
            }
            if Self::menu_item(ui, Some(Icon::EyeOff), "Ocultar esta sección", false).clicked() {
                if !self.settings.home_hidden.contains(&sec.id) {
                    self.settings.home_hidden.push(sec.id.clone());
                }
                self.settings.save(&self.paths);
                ui.close();
            }
        });

        // Tira de tarjetas desde 25,6 px bajo la línea base del título.
        let strip = Rect::from_min_max(pos2(block.min.x, base + 26.6), block.max);
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(strip).layout(Layout::left_to_right(Align::Min)));
        let mut area = egui::ScrollArea::horizontal().id_salt(&sec.id).auto_shrink([false, true]);
        if let Some(t) = self.home_scroll.remove(&sec.id) {
            area = area.horizontal_scroll_offset(t);
        }
        let out = area.show(&mut c, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = vec2(CARD_PITCH - HOME_CARD, 0.0);
                for item in &sec.items {
                    self.home_card(ui, item);
                }
            });
        });
        let max_off = (out.content_size.x - out.inner_rect.width()).max(0.0);
        self.home_offsets.insert(sec.id.clone(), (out.state.offset.x, max_off));
    }

    /// Tarjeta del inicio de 171 px de ancho (referencia 9.png): las playlists con dos hojas
    /// detrás (tonos oscuros de su portada), los álbumes con una y los artistas en círculo; debajo,
    /// el título (14,75) y el subtítulo en dos líneas (12,75).
    fn home_tile(&mut self, ui: &mut egui::Ui, shape: CardKind, cover: Option<&str>, title: &str, subtitle: &str) -> egui::Response {
        let p = theme::palette(ui.ctx());
        let (rect, resp) = ui.allocate_exact_size(vec2(HOME_CARD, HOME_TILE_H), Sense::click());
        if !ui.is_rect_visible(rect) {
            return resp;
        }
        let x = rect.min.x.round();
        let t = rect.min.y;
        let hovered = resp.hovered();
        let stack = cover.and_then(|u| self.images.stack(u)).unwrap_or(Color32::from_gray(40));
        let cover_rect = if shape == CardKind::Artist {
            Rect::from_min_size(pos2(x, t.round()), vec2(HOME_CARD, HOME_CARD))
        } else {
            let top = (t + 14.4).round();
            if matches!(shape, CardKind::Playlist | CardKind::Liked) {
                let back = Color32::from_rgb((stack.r() as f32 * 0.55) as u8, (stack.g() as f32 * 0.55) as u8, (stack.b() as f32 * 0.55) as u8);
                ui.painter().rect_filled(Rect::from_min_max(pos2(x + 18.5, t + 2.6), pos2(x + HOME_CARD - 18.5, t + 4.8)), CornerRadius { nw: 3, ne: 3, sw: 0, se: 0 }, back);
            }
            ui.painter().rect_filled(Rect::from_min_max(pos2(x + 8.0, t + 6.4), pos2(x + HOME_CARD - 8.0, top - 1.8)), CornerRadius { nw: 3, ne: 3, sw: 0, se: 0 }, stack);
            Rect::from_min_size(pos2(x, top), vec2(HOME_CARD, HOME_CARD))
        };
        match shape {
            CardKind::Artist => self.cover_in(ui, cover, cover_rect, (HOME_CARD / 2.0) as u8),
            CardKind::Liked => {
                ui.painter().rect_filled(cover_rect, CornerRadius::same(6), theme::GREEN_DARK);
                icons::paint(ui.painter(), cover_rect.shrink(HOME_CARD * 0.3), GREEN, Icon::HeartFilled);
            }
            _ => self.cover_in(ui, cover, cover_rect, 6),
        }
        if hovered {
            let r = if shape == CardKind::Artist { (HOME_CARD / 2.0) as u8 } else { 6 };
            ui.painter().rect_filled(cover_rect, CornerRadius::same(r), Color32::from_white_alpha(14));
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let title_color = if p.dark { if hovered { Color32::WHITE } else { Color32::from_gray(241) } } else { p.text };
        let g = galley_truncated(ui.painter(), title, theme::regular(HOME_CARD_TITLE_FONT), title_color, HOME_CARD);
        text_on_baseline(ui.painter(), pos2(x, cover_rect.max.y + 21.0), g, title_color);
        if !subtitle.is_empty() {
            let sub_color = if p.dark { Color32::from_gray(133) } else { p.weak };
            let mut job = egui::text::LayoutJob::single_section(
                subtitle.to_string(),
                egui::TextFormat { font_id: theme::regular(HOME_CARD_SUB_FONT), color: sub_color, line_height: Some(16.2), ..Default::default() },
            );
            job.wrap = egui::text::TextWrapping { max_width: HOME_CARD, max_rows: 2, break_anywhere: false, overflow_character: Some('…') };
            let g = ui.painter().layout_job(job);
            text_on_baseline(ui.painter(), pos2(x, cover_rect.max.y + 46.2), g, sub_color);
        }
        resp
    }

    fn home_card(&mut self, ui: &mut egui::Ui, item: &HomeItem) {
        let kind = item.kind();
        if kind == "track" {
            let sub = item.subtitle.clone();
            let r = self.home_tile(ui, CardKind::Album, item.image.as_deref(), &item.title, &sub);
            let (uri, ctx) = (item.uri.clone(), item.context.clone());
            if r.clicked() {
                let shuffle = self.player.shuffle;
                self.actions.push(Action::Play(match ctx.clone() {
                    Some(c) => PlayTarget::Context { uri: c, track_uri: Some(uri.clone()), index: None, shuffle },
                    None => PlayTarget::Tracks { uris: vec![uri.clone()], index: Some(0), shuffle },
                }));
            }
            r.context_menu(|ui| {
                if Self::menu_item(ui, Some(Icon::AddToQueue), "Añadir a la cola", false).clicked() {
                    self.actions.push(Action::AddToQueue(uri.clone()));
                    ui.close();
                }
                if Self::menu_item(ui, Some(Icon::Share), "Copiar enlace", false).clicked() {
                    self.actions.push(Action::CopyText(uri_to_link(&uri), "Enlace"));
                    ui.close();
                }
            });
            return;
        }
        let page = match kind {
            "playlist" => Some(Page::Playlist(item.id().to_string())),
            "album" => Some(Page::Album(item.id().to_string())),
            "artist" => Some(Page::Artist(item.id().to_string())),
            "show" => Some(Page::Show(item.id().to_string())),
            "collection" => Some(Page::Liked),
            _ => None,
        };
        let card_kind = match kind {
            "playlist" => CardKind::Playlist,
            "artist" => CardKind::Artist,
            "collection" => CardKind::Liked,
            _ => CardKind::Album,
        };
        let subtitle = if item.subtitle.is_empty() {
            match kind {
                "artist" => "Artista",
                "show" => "Podcast",
                "album" => "Álbum",
                _ => "",
            }
            .to_string()
        } else {
            item.subtitle.clone()
        };
        let r = self.home_tile(ui, card_kind, item.image.as_deref(), &item.title, &subtitle);
        // El DJ no es una playlist que se pueda abrir (la Web API no la da): como en Spotify,
        // pulsarlo lo pone a sonar.
        if item.uri == super::DJ_URI {
            if r.clicked() {
                if !self.dj_active() {
                    self.dj_button();
                } else if self.player.state == super::PlayState::Paused {
                    self.play_pause();
                }
            }
            return;
        }
        let Some(page) = page else { return };
        if kind == "playlist" && (r.clicked() || r.secondary_clicked()) {
            // Lo que ya sabemos por la tarjeta (nombre, portada generada, autor Spotify) se ve al
            // instante; la Web API no devuelve las radios y mixes de Spotify a apps externas.
            let pid = item.id().to_string();
            let spotify_made = pid.starts_with("37i9dQZ");
            self.playlist_meta.entry(pid.clone()).or_insert_with(|| Playlist {
                id: pid,
                name: item.title.clone(),
                uri: item.uri.clone(),
                description: Some(item.subtitle.clone()).filter(|d| !d.is_empty()),
                images: item.image.as_ref().map(|u| vec![Image { url: u.clone(), width: Some(300), height: Some(300) }]),
                owner: Owner { display_name: spotify_made.then(|| "Spotify".to_string()), id: spotify_made.then(|| "spotify".to_string()) },
                ..Playlist::default()
            });
        }
        if r.clicked() {
            self.actions.push(Action::Go(page.clone()));
        }
        let uri = item.uri.clone();
        r.context_menu(|ui| {
            if Self::menu_item(ui, Some(Icon::NewTab), "Abrir en una nueva pestaña", false).clicked() {
                self.actions.push(Action::OpenInTab(page.clone()));
                ui.close();
            }
            if matches!(kind, "playlist" | "album" | "artist" | "show") && Self::menu_item(ui, Some(Icon::Play), "Reproducir", false).clicked() {
                let shuffle = self.player.shuffle;
                self.actions.push(Action::Play(PlayTarget::Context { uri: uri.clone(), track_uri: None, index: None, shuffle }));
                ui.close();
            }
            if kind != "collection" && Self::menu_item(ui, Some(Icon::Share), "Copiar enlace", false).clicked() {
                self.actions.push(Action::CopyText(uri_to_link(&uri), "Enlace"));
                ui.close();
            }
        });
    }

    fn home_page_legacy(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        let name = self.display_name();
        ui.label(RichText::new(format!("Hola, {name}")).font(theme::bold(26.0)));
        let f = 0u8;

        if f == 0 && !self.recent.is_empty() {
            if Self::section_header(ui, "Reproducidos recientemente", Some("Ver historial")) {
                self.actions.push(Action::Go(Page::History));
            }
            let recent = std::mem::take(&mut self.recent);
            let mut shown: Vec<Track> = Vec::new();
            for t in &recent {
                if !shown.iter().any(|s| s.uri == t.uri) {
                    shown.push(t.clone());
                }
                if shown.len() >= 6 {
                    break;
                }
            }
            self.track_rows(ui, "recent", &shown, RowOpts::tracks(true, true));
            self.recent = recent;
        }

        if (f == 0 || f == 1) && !self.playlists.is_empty() {
            if Self::section_header(ui, "Tus playlists", Some("Ver biblioteca")) {
                self.actions.push(Action::Go(Page::Library));
            }
            let mut pls = self.playlists.clone();
            let pinned = self.settings.pinned.clone();
            pls.sort_by_key(|pl| !pinned.contains(&pl.id));
            Self::cards_row(ui, "home_playlists", |ui| {
                for pl in pls.iter().take(20) {
                    self.playlist_card(ui, pl);
                }
            });
        }

        self.request_once("albums", Req::SavedAlbums);
        if (f == 0 || f == 2) && !self.saved_albums.is_empty() {
            if Self::section_header(ui, "Álbumes guardados", Some("Ver todos")) {
                self.actions.push(Action::Go(Page::Albums));
            }
            let albums = std::mem::take(&mut self.saved_albums);
            Self::cards_row(ui, "home_albums", |ui| {
                for a in albums.iter().take(20) {
                    let sub = a.artists_str();
                    self.album_card(ui, &a.id.clone(), a.cover(300), &a.name, &sub, a.total_tracks);
                }
            });
            self.saved_albums = albums;
        }

        self.request_once("artists", Req::FollowedArtists);
        if (f == 0 || f == 3) && !self.followed_artists.is_empty() {
            if Self::section_header(ui, "Artistas que sigues", Some("Ver todos")) {
                self.actions.push(Action::Go(Page::Artists));
            }
            let artists = std::mem::take(&mut self.followed_artists);
            Self::cards_row(ui, "home_artists", |ui| {
                for a in artists.iter().take(20) {
                    self.artist_card(ui, a);
                }
            });
            self.followed_artists = artists;
        }
        if f != 0 {
            ui.add_space(8.0);
            ui.label(RichText::new("Filtro activo: solo se muestra una categoría.").small().color(p.faint));
        }
    }

    // -------------------------------------------------------------- biblioteca

    /// «Tu biblioteca» sin sesión. Con sesión, la página entera es `library_panel` (library.rs),
    /// que `mod.rs` llama fuera del desplazamiento general (lleva su barra fija).
    fn library_page(&mut self, ui: &mut egui::Ui) {
        self.welcome(ui);
    }

    pub(super) fn playlist_row_menu(&mut self, r: &egui::Response, pl: &Playlist) {
        let pinned = self.settings.pinned.contains(&pl.id);
        let mine = self.is_mine(pl);
        r.context_menu(|ui| {
            if Self::menu_item(ui, Some(Icon::NewTab), "Abrir en una nueva pestaña", false).clicked() {
                self.actions.push(Action::OpenInTab(Page::Playlist(pl.id.clone())));
                ui.close();
            }
            if Self::menu_item(ui, Some(Icon::Pin), if pinned { "Desfijar" } else { "Fijar" }, false).clicked() {
                self.actions.push(Action::Pin(pl.id.clone(), !pinned));
                ui.close();
            }
            if mine {
                self.invite_menu_item(ui, &pl.id);
            }
            self.folder_menu(ui, &pl.id);
            if Self::menu_item(ui, Some(Icon::Share), "Copiar enlace", false).clicked() {
                self.actions.push(Action::CopyText(uri_to_link(&pl.uri), "Enlace"));
                ui.close();
            }
        });
    }

    /// Fila de biblioteca en vista de lista: miniatura con forma según el tipo + textos.
    pub(super) fn list_entry(&mut self, ui: &mut egui::Ui, cover: Option<&str>, kind: CardKind, title: &str, subtitle: &str) -> egui::Response {
        let p = theme::palette(ui.ctx());
        let w = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(vec2(w, 56.0), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(10), p.hover);
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let img = Rect::from_min_size(pos2(rect.min.x + 8.0, rect.min.y + 8.0), vec2(40.0, 40.0));
        match kind {
            CardKind::Liked => {
                ui.painter().rect_filled(img, CornerRadius::same(6), theme::GREEN_DARK);
                icons::paint(ui.painter(), img.shrink(10.0), GREEN, Icon::HeartFilled);
            }
            CardKind::Artist => self.cover_in(ui, cover, img, 20),
            _ => self.cover_in(ui, cover, img, 6),
        }
        let text = Rect::from_min_max(pos2(img.max.x + 12.0, rect.min.y), pos2(rect.max.x - 8.0, rect.max.y));
        let mut c = child_in(ui, text, Layout::top_down(Align::Min));
        c.set_width(text.width());
        c.spacing_mut().item_spacing.y = 2.0;
        c.add_space(9.0);
        c.add(Label::new(RichText::new(title).color(p.text)).truncate());
        c.add(Label::new(RichText::new(subtitle).small().color(p.weak)).truncate());
        resp
    }

    // ---------------------------------------------------------------- historial

    /// Historial: lo reproducido en Nanofy (registro local, con hora) y después lo que Spotify
    /// devuelve de otros dispositivos y no está ya en la lista. Se recalcula solo cuando cambia.
    pub(super) fn history_list(&mut self) -> Vec<Track> {
        let key = (
            self.play_log.entries.len(),
            self.play_log.entries.iter().map(|e| e.last).max().unwrap_or(0),
            self.recent.len(),
            self.recent.first().map(|t| t.uri.clone()).unwrap_or_default(),
        );
        if let Some((k, cached)) = &self.history_cache {
            if *k == key {
                return cached.clone();
            }
        }
        let mut local: Vec<&crate::cache::PlayEntry> = self.play_log.entries.iter().collect();
        local.sort_by(|a, b| b.last.cmp(&a.last));
        let mut out: Vec<Track> = Vec::with_capacity(200);
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for e in local.into_iter().take(200) {
            if seen.insert(e.track.uri.clone()) {
                let mut t = e.track.clone();
                // Guardada al empezar a sonar, sin su álbum: el de Spotify, si lo trae.
                if t.album.as_ref().is_none_or(|a| a.id.is_none()) {
                    if let Some(r) = self.recent.iter().find(|r| r.uri == t.uri && r.album.as_ref().is_some_and(|a| a.id.is_some())) {
                        t.album = r.album.clone();
                    }
                }
                out.push(t);
            }
        }
        for t in &self.recent {
            if seen.insert(t.uri.clone()) {
                out.push(t.clone());
            }
        }
        self.history_cache = Some((key, out.clone()));
        out
    }

    fn history_page(&mut self, ui: &mut egui::Ui) {
        if !self.signed_in() {
            self.welcome(ui);
            return;
        }
        let p = theme::palette(ui.ctx());
        Self::page_title(ui, "", "Historial", "Lo último que has escuchado, del más reciente al más antiguo");
        if self.recent_at.elapsed() > Duration::from_secs(45) {
            self.recent_at = Instant::now();
            self.api.send(Req::Recent);
        }
        let recent = self.history_list();
        if recent.is_empty() {
            Self::loading(ui, "Cargando");
            return;
        }
        self.collection_bar(ui, "history", &recent, None, None, |_, _, _| false, None);
        ui.add_space(8.0);
        let shown = self.filter_tracks("history", None, &recent);
        self.track_rows(ui, "history", &shown, RowOpts { header: true, select: true, ..RowOpts::tracks(true, true) });
        ui.add_space(8.0);
        ui.label(
            RichText::new("Lo escuchado en Nanofy se guarda en este equipo; de otros dispositivos Spotify solo entrega las últimas 50 reproducciones.")
                .small()
                .color(p.faint),
        );
    }

    // ---------------------------------------------------------------- búsqueda

    fn search_page(&mut self, ui: &mut egui::Ui) {
        if !self.signed_in() {
            self.welcome(ui);
            return;
        }
        let p = theme::palette(ui.ctx());
        const FILTERS: [&str; 9] = ["Todo", "Canciones", "Artistas", "Álbumes", "Playlists", "Podcasts", "Episodios", "Audiolibros", "Perfiles"];
        // Chips como los de Inicio (referencia 10.png), con el gris más oscuro de la búsqueda; si
        // no caben en una fila, siguen en la de abajo.
        let width = ui.available_width();
        let widths: Vec<f32> = FILTERS
            .iter()
            .map(|l| (ui.painter().layout_no_wrap(l.to_string(), theme::regular(HOME_CHIP_FONT), Color32::WHITE).size().x + 32.0).round())
            .collect();
        let mut rows = 1;
        let mut x = 0.0;
        for w in &widths {
            if x > 0.0 && x + w > width {
                rows += 1;
                x = 0.0;
            }
            x += w + 9.5;
        }
        let (area, _) = ui.allocate_exact_size(vec2(width, rows as f32 * (HOME_CHIP_H + 9.5) - 9.5), Sense::hover());
        let (mut x, mut y) = (area.min.x, area.min.y);
        for (i, (label, w)) in FILTERS.iter().zip(&widths).enumerate() {
            if x > area.min.x && x + w > area.max.x {
                x = area.min.x;
                y += HOME_CHIP_H + 9.5;
            }
            if Self::chip(ui, pos2(x, y), label, self.search_filter == i as u8, 24).clicked() {
                self.set_search_filter(i as u8);
            }
            x += w + 9.5;
        }
        ui.add_space(14.0);
        let f = self.search_filter;
        if f == 8 {
            let q = self.search_query.trim().to_string();
            if q.is_empty() {
                ui.label(RichText::new("Escribe el nombre de usuario exacto (el que aparece en su enlace open.spotify.com/user/…).").color(p.weak));
                return;
            }
            match self.users.get(&q).cloned() {
                Some(u) => {
                    let r = self.list_entry(ui, u.cover(64), CardKind::Artist, u.name(), &format!("Perfil · {}", u.id));
                    if r.clicked() {
                        self.actions.push(Action::Go(Page::User(u.id.clone())));
                    }
                }
                None => {
                    ui.label(RichText::new("Buscando ese usuario… Spotify no permite buscar perfiles por nombre, solo por nombre de usuario exacto.").small().color(p.weak));
                }
            }
            return;
        }
        // Una consulta en vuelo sin search_loading (la del diagnóstico) también cuenta; el refresco
        // detrás de un resultado de la caché no: lo que se ve ya es de esa consulta.
        let waiting = self.search_loading || self.search_pending.as_ref().is_some_and(|q| !self.search_refreshing.contains(q));
        let Some(result) = self.search_result.take() else {
            if waiting {
                Self::loading(ui, "Buscando");
                return;
            }
            ui.add_space(20.0);
            ui.label(
                RichText::new(
                    "Escribe arriba y pulsa Intro. También puedes pegar un enlace de Spotify \
                     (canción, álbum, artista, playlist, podcast, perfil o Jam).",
                )
                .color(p.weak),
            );
            return;
        };
        // Con una búsqueda nueva en vuelo, los resultados anteriores siguen a la vista pero
        // atenuados, con un aviso pequeño encima, en vez de vaciar la página hasta la respuesta.
        // No se devuelve el foco a la caja: Espacio, S, R… dejarían de ser atajos.
        let opacity = ui.opacity();
        if waiting {
            Self::loading(ui, "Buscando");
            ui.multiply_opacity(0.5);
        }

        if let Some(tracks) = &result.tracks {
            if (f == 0 || f == 1) && !tracks.items.is_empty() {
                Self::section_header(ui, "Canciones", None);
                let items: Vec<Track> = if f == 0 { tracks.items.iter().take(5).cloned().collect() } else { tracks.items.clone() };
                self.track_rows(ui, "search", &items, RowOpts::tracks(true, true));
            }
        }
        if let Some(artists) = &result.artists {
            let items: Vec<Artist> = artists.items.iter().flatten().cloned().collect();
            if (f == 0 || f == 2) && !items.is_empty() {
                Self::section_header(ui, "Artistas", None);
                Self::cards_row(ui, "search_artists", |ui| {
                    for a in &items {
                        self.artist_card(ui, a);
                    }
                });
            }
        }
        if let Some(albums) = &result.albums {
            let items: Vec<AlbumRef> = albums.items.iter().flatten().cloned().collect();
            if (f == 0 || f == 3) && !items.is_empty() {
                Self::section_header(ui, "Álbumes", None);
                Self::cards_row(ui, "search_albums", |ui| {
                    for a in &items {
                        if let Some(id) = &a.id {
                            let sub = format!("{} · {}", a.year(), a.artists_str());
                            self.album_card(ui, id, a.cover(300), &a.name, &sub, a.total_tracks);
                        }
                    }
                });
            }
        }
        if let Some(playlists) = &result.playlists {
            let items: Vec<Playlist> = playlists.items.iter().flatten().cloned().collect();
            if (f == 0 || f == 4) && !items.is_empty() {
                Self::section_header(ui, "Playlists", None);
                Self::cards_row(ui, "search_playlists", |ui| {
                    for pl in &items {
                        self.playlist_card(ui, pl);
                    }
                });
            }
        }
        if let Some(shows) = &result.shows {
            let items: Vec<Show> = shows.items.iter().flatten().cloned().collect();
            if (f == 0 || f == 5) && !items.is_empty() {
                Self::section_header(ui, "Podcasts", None);
                Self::cards_row(ui, "search_shows", |ui| {
                    for s in &items {
                        let r = self.card(ui, CardInfo { kind: CardKind::Album, cover: s.cover(300), title: &s.name, subtitle: &s.publisher, count: s.total_episodes, pinned: false });
                        if r.clicked() {
                            self.shows.entry(s.id.clone()).or_insert_with(|| (s.clone(), Vec::new()));
                            self.actions.push(Action::Go(Page::Show(s.id.clone())));
                        }
                        let sid = s.id.clone();
                        r.context_menu(|ui| {
                            if Self::menu_item(ui, Some(Icon::NewTab), "Abrir en una nueva pestaña", false).clicked() {
                                self.actions.push(Action::OpenInTab(Page::Show(sid.clone())));
                                ui.close();
                            }
                        });
                    }
                });
            }
        }
        if let Some(episodes) = &result.episodes {
            let items: Vec<Track> = episodes.items.iter().flatten().map(|e| e.as_track("Episodio")).collect();
            if (f == 0 || f == 6) && !items.is_empty() {
                Self::section_header(ui, "Episodios", None);
                let items: Vec<Track> = if f == 0 { items.into_iter().take(5).collect() } else { items };
                self.track_rows(ui, "search_episodes", &items, RowOpts::tracks(true, true));
            }
        }
        if let Some(books) = &result.audiobooks {
            let items: Vec<Audiobook> = books.items.iter().flatten().cloned().collect();
            if (f == 0 || f == 7) && !items.is_empty() {
                Self::section_header(ui, "Audiolibros", None);
                Self::cards_row(ui, "search_books", |ui| {
                    for b in &items {
                        let sub = b.authors_str();
                        let r = self.card(ui, CardInfo { kind: CardKind::Album, cover: b.cover(300), title: &b.name, subtitle: &sub, count: None, pinned: false });
                        if r.clicked() {
                            self.actions.push(Action::Go(Page::Show(b.id.clone())));
                        }
                    }
                });
            }
        }
        ui.set_opacity(opacity);
        self.search_result = Some(result);
    }

    // ----------------------------------------------------------------- podcast

    pub fn show_page(&mut self, ui: &mut egui::Ui, id: String) {
        if !self.signed_in() {
            self.welcome(ui);
            return;
        }
        self.request_once(&format!("show:{id}"), Req::Show(id.clone()));
        self.request_once("saved_shows", Req::SavedShows);
        self.request_once("saved_episodes", Req::SavedEpisodes);
        let Some((show, episodes)) = self.shows.get(&id).cloned() else {
            Self::loading(ui, "Cargando");
            return;
        };
        let lid = format!("show:{id}");
        if self.list_search_id != lid {
            self.list_search_id = lid.clone();
            self.album_search.clear();
            self.album_search_open = false;
        }
        let followed = self.followed_shows.contains(&id);
        let link = uri_to_link(&show.uri);
        let cover = show.cover(640).map(|s| s.to_string());
        let about = strip_html(&show.description);
        let keywords = show.keywords.clone();
        let id2 = id.clone();

        self.two_columns(
            ui,
            |app, ui| {
                let p = theme::palette(ui.ctx());
                ui.add(Label::new(RichText::new(&show.name).font(theme::bold(30.0))).truncate());
                ui.add_space(4.0);
                ui.label(RichText::new(&show.publisher).small().color(p.weak));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    if Self::pill(ui, if followed { "Siguiendo" } else { "Seguir" }, followed).clicked() {
                        app.actions.push(Action::FollowShow(id2.clone(), !followed));
                    }
                    let more = icons::button(ui, Icon::More, 34.0, p.weak).on_hover_text("Más");
                    egui::Popup::menu(&more).show(|ui| {
                        ui.set_min_width(200.0);
                        if Self::menu_item(ui, Some(Icon::NewTab), "Abrir en una nueva pestaña", false).clicked() {
                            app.actions.push(Action::OpenInTab(Page::Show(id2.clone())));
                            ui.close();
                        }
                        if Self::menu_item(ui, Some(Icon::Share), "Copiar enlace", false).clicked() {
                            app.actions.push(Action::CopyText(link.clone(), "Enlace"));
                            ui.close();
                        }
                    });
                });
                ui.add_space(18.0);

                // Cabecera de la lista: título + orden, filtro y búsqueda a la derecha
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Todos los episodios").font(theme::bold(18.0)));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let color = if app.album_search_open { GREEN } else { p.weak };
                        if icons::button(ui, Icon::Search, 30.0, color).on_hover_text("Buscar episodio").clicked() {
                            app.album_search_open = !app.album_search_open;
                            app.album_search_focus = app.album_search_open;
                            if !app.album_search_open {
                                app.album_search.clear();
                            }
                        }
                        if app.album_search_open {
                            let r = ui.add(egui::TextEdit::singleline(&mut app.album_search).hint_text("Buscar episodio").desired_width(180.0));
                            if app.album_search_focus {
                                app.album_search_focus = false;
                                r.request_focus();
                            }
                            if r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                app.album_search_open = false;
                                app.album_search.clear();
                            }
                        }
                        ui.add_space(8.0);
                        const FILTROS: [&str; 3] = ["Todos los episodios", "Descargados", "Guardados"];
                        let fl = ui.add(Label::new(RichText::new(FILTROS[app.show_filter as usize]).small().color(p.weak)).sense(Sense::click()));
                        let fb = icons::button(ui, Icon::Filter, 30.0, p.weak).on_hover_text("Filtrar");
                        let fresp = fb.union(fl);
                        egui::Popup::menu(&fresp).show(|ui| {
                            for (i, f) in FILTROS.iter().enumerate() {
                                if ui.selectable_label(app.show_filter == i as u8, *f).clicked() {
                                    app.show_filter = i as u8;
                                    ui.close();
                                }
                            }
                        });
                        ui.add_space(8.0);
                        let sl = ui.add(Label::new(RichText::new(if app.show_sort_newest { "Nuevos" } else { "Antiguos" }).small().color(p.weak)).sense(Sense::click()));
                        let sb = icons::button(ui, Icon::Sort, 30.0, p.weak).on_hover_text("Cambiar el orden");
                        if sb.clicked() || sl.clicked() {
                            app.show_sort_newest = !app.show_sort_newest;
                        }
                    });
                });
                ui.add_space(6.0);

                let q = app.list_query(&lid);
                let mut eps: Vec<Episode> = episodes
                    .iter()
                    .filter(|e| q.is_empty() || e.name.to_lowercase().contains(&q) || e.description.to_lowercase().contains(&q))
                    .filter(|e| match app.show_filter {
                        1 => app.downloaded.contains(&e.id),
                        2 => app.saved_episodes.contains(&e.id),
                        _ => true,
                    })
                    .cloned()
                    .collect();
                if app.show_sort_newest {
                    eps.sort_by(|a, b| b.release_date.cmp(&a.release_date));
                } else {
                    eps.sort_by(|a, b| a.release_date.cmp(&b.release_date));
                }
                if eps.is_empty() {
                    let msg = if episodes.is_empty() { "Cargando episodios…" } else { "Ningún episodio coincide." };
                    ui.label(RichText::new(msg).small().color(p.weak));
                }
                let uris: Vec<String> = eps.iter().map(|e| e.uri.clone()).collect();
                for (i, e) in eps.iter().enumerate() {
                    app.episode_row(ui, &show, e, i, &uris);
                }
            },
            |app, ui| {
                let p = theme::palette(ui.ctx());
                egui::Frame::new().fill(p.card2).corner_radius(CornerRadius::same(14)).inner_margin(14).show(ui, |ui| {
                    ui.set_width(INFO_W - 28.0);
                    let side = INFO_W - 28.0;
                    let (rect, _) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
                    app.cover_in(ui, cover.as_deref(), rect, 10);
                    ui.add_space(14.0);
                    ui.label(RichText::new("Acerca de").font(theme::bold(16.0)));
                    ui.add_space(4.0);
                    if !about.is_empty() {
                        ui.add(Label::new(RichText::new(&about).small().color(p.weak)).wrap());
                    }
                    if !keywords.is_empty() {
                        ui.add_space(10.0);
                        ui.horizontal_wrapped(|ui| {
                            for k in keywords.iter().take(6) {
                                let _ = Self::pill(ui, k, false);
                            }
                        });
                    }
                });
            },
        );
    }

    /// Fila de episodio: miniatura, título, fecha • duración, descripción y botones.
    fn episode_row(&mut self, ui: &mut egui::Ui, show: &Show, e: &Episode, i: usize, uris: &[String]) {
        let p = theme::palette(ui.ctx());
        let width = ui.available_width();
        let (row, resp) = ui.allocate_exact_size(vec2(width, 152.0), Sense::hover());
        let hov = resp.hovered() || resp.contains_pointer();
        if hov {
            ui.painter().rect_filled(row, CornerRadius::same(12), p.hover.gamma_multiply(0.5));
        }
        let cover = Rect::from_min_size(pos2(row.min.x + 6.0, row.min.y + 12.0), vec2(118.0, 118.0));
        let url = e.cover(300).or_else(|| show.cover(300)).map(|s| s.to_string());
        self.cover_in(ui, url.as_deref(), cover, 8);
        let text = Rect::from_min_max(pos2(cover.max.x + 16.0, row.min.y + 8.0), pos2(row.max.x - 8.0, row.max.y - 4.0));
        let mut c = child_in(ui, text, Layout::top_down(Align::Min));
        c.set_width(text.width());
        c.spacing_mut().item_spacing.y = 3.0;
        c.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let (ir, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
            icons::paint(ui.painter(), ir, p.weak, Icon::Episode);
            ui.add(Label::new(RichText::new(&e.name).font(theme::bold(15.0))).truncate());
        });
        let date = e.release_date.as_deref().map(fmt_date).unwrap_or_default();
        let meta = if date.is_empty() { fmt_total(e.duration_ms as u64) } else { format!("{date}  •  {}", fmt_total(e.duration_ms as u64)) };
        c.label(RichText::new(meta).small().color(p.weak));
        c.add_space(2.0);
        // Descripción en dos líneas como máximo (aprox. por ancho).
        let max_chars = ((text.width() / 6.2) * 2.0) as usize;
        let desc = strip_html(&e.description);
        let short: String = if desc.chars().count() > max_chars {
            desc.chars().take(max_chars.saturating_sub(1)).collect::<String>() + "…"
        } else {
            desc
        };
        c.add(Label::new(RichText::new(short).small().color(p.weak)).wrap());
        c.add_space(4.0);
        c.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if icons::round_button(ui, Icon::Play, 34.0, GREEN, Color32::BLACK).on_hover_text("Reproducir").clicked() {
                self.actions.push(Action::Play(PlayTarget::Tracks { uris: uris.to_vec(), index: Some(i as u32), shuffle: false }));
            }
            let saved = self.saved_episodes.contains(&e.id);
            let (icon, color, tip) = if saved { (Icon::BookmarkFilled, GREEN, "Quitar de tus episodios") } else { (Icon::Bookmark, p.weak, "Guardar en tus episodios") };
            if icons::button(ui, icon, 30.0, color).on_hover_text(tip).clicked() {
                self.actions.push(Action::SaveEpisode(e.id.clone(), !saved));
            }
            if icons::button(ui, Icon::PlusSquare, 30.0, p.weak).on_hover_text("Añadir a playlist").clicked() {
                self.open_add_dialog(vec![e.uri.clone()]);
            }
            let dl = self.downloaded.contains(&e.id);
            let busy = self.downloading.contains(&e.id);
            let (icon, color, tip) = if dl {
                (Icon::Download, GREEN, "Descargado (en la caché de audio)")
            } else if busy {
                (Icon::Hourglass, p.weak, "Descargando…")
            } else {
                (Icon::Download, p.weak, "Descargar")
            };
            if icons::button(ui, icon, 30.0, color).on_hover_text(tip).clicked() && !dl && !busy {
                self.actions.push(Action::DownloadEpisodes(vec![e.id.clone()]));
            }
            if icons::button(ui, Icon::Share, 30.0, p.weak).on_hover_text("Copiar enlace").clicked() {
                self.actions.push(Action::CopyText(uri_to_link(&e.uri), "Enlace"));
            }
            let more = icons::button(ui, Icon::More, 30.0, p.weak).on_hover_text("Más");
            egui::Popup::menu(&more).show(|ui| {
                ui.set_min_width(180.0);
                if Self::menu_item(ui, Some(Icon::AddToQueue), "Añadir a la cola", false).clicked() {
                    self.actions.push(Action::AddToQueue(e.uri.clone()));
                    ui.close();
                }
                if Self::menu_item(ui, Some(Icon::Share), "Copiar enlace", false).clicked() {
                    self.actions.push(Action::CopyText(uri_to_link(&e.uri), "Enlace"));
                    ui.close();
                }
            });
        });
        ui.painter().line_segment(
            [pos2(row.min.x + 6.0, row.max.y - 1.0), pos2(row.max.x - 6.0, row.max.y - 1.0)],
            egui::Stroke::new(1.0, p.border),
        );
    }

    // ---------------------------------------------------------------- me gusta

    fn liked_page(&mut self, ui: &mut egui::Ui) {
        if !self.signed_in() {
            self.welcome(ui);
            return;
        }
        self.request_once(LIKED, Req::Liked);
        let list = self.lists.remove(LIKED).unwrap_or_default();
        let summary = self.list_summary(LIKED, &list);
        let total_ms = summary.total_ms;
        let tracks = list.tracks;
        let loading = list.loading;
        let total = list.total;
        let gen = list.gen;
        // Con la lista completa (o la copia entera a la vista mientras se refresca), los artistas
        // más presentes ya no cambian: es cuando se piden sus imágenes. Un total 0 en carga aún
        // no se conoce (no es una lista vacía): con él, cada lote pediría otro artista suelto.
        let thumbs_ready = !loading || (total > 0 && tracks.len() >= total as usize);

        let me = self.display_name();
        let by: Vec<(String, Option<Page>)> = if me.is_empty() { Vec::new() } else { vec![(me, self.my_id().map(|id| Page::User(id.to_string())))] };
        let mut parts = vec![format!("{total} canciones")];
        if !tracks.is_empty() {
            parts.push(fmt_total(total_ms));
        }

        self.two_columns(
            ui,
            |app, ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                app.collection_header(ui, "Canciones que te gustan", None, &by, &parts);
                if !tracks.is_empty() {
                    app.collection_bar(ui, LIKED, &tracks, None, None, |_, _, _| false, None);
                }
                if loading && tracks.is_empty() {
                    // Primer arranque sin copia: el hueco de las filas mientras llegan.
                    ui.add_space(TABLE_HEAD_H);
                    Self::skeleton_rows(ui, 8, TABLE_ROW_H, 50.0, 43.0);
                } else if loading {
                    ui.add_space(8.0);
                    Self::loading(ui, &format!("Cargando {} de {}", tracks.len(), total));
                }
                let shown = app.filter_tracks(LIKED, Some(gen), &tracks);
                app.track_rows(ui, LIKED, &shown, RowOpts { header: true, select: true, ..RowOpts::tracks(true, true) });
            },
            |app, ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                let side = ui.available_width().min(INFO_W);
                let (rect, _) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
                ui.painter().rect_filled(rect, CornerRadius::same(6), theme::GREEN_DARK);
                icons::paint(ui.painter(), rect.shrink(side * 0.28), GREEN, Icon::HeartFilled);
                let chips = [format!("{total} canciones"), fmt_total(total_ms)];
                app.info_chips_and_artists(ui, &chips, &summary.top, thumbs_ready);
            },
        );
        // Con su versión: si no, el resumen y la búsqueda guardados no valdrían al fotograma siguiente.
        self.lists.insert(LIKED.to_string(), crate::app::TrackList { tracks, total, loading, gen });
    }

    // ----------------------------------------------------------------- álbumes

    fn albums_page(&mut self, ui: &mut egui::Ui) {
        if !self.signed_in() {
            self.welcome(ui);
            return;
        }
        self.request_once("albums", Req::SavedAlbums);
        Self::page_title(ui, "", "Álbumes guardados", &format!("{} álbumes", self.saved_albums.len()));
        if self.saved_albums.is_empty() {
            // Sin copia: el hueco de las tarjetas mientras llegan.
            Self::skeleton_cards(ui, 8);
            return;
        }
        let albums = std::mem::take(&mut self.saved_albums);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 8.0);
            for a in &albums {
                let sub = format!("{} · {}", a.year(), a.artists_str());
                self.album_card(ui, &a.id.clone(), a.cover(300), &a.name, &sub, a.total_tracks);
            }
        });
        self.saved_albums = albums;
    }

    fn artists_page(&mut self, ui: &mut egui::Ui) {
        if !self.signed_in() {
            self.welcome(ui);
            return;
        }
        let p = theme::palette(ui.ctx());
        self.request_once("artists", Req::FollowedArtists);
        Self::page_title(ui, "", "Artistas que sigues", &format!("{} artistas", self.followed_artists.len()));
        if self.followed_artists.is_empty() {
            ui.label(RichText::new("Todavía no sigues a ningún artista.").color(p.weak));
            return;
        }
        let artists = std::mem::take(&mut self.followed_artists);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 8.0);
            for a in &artists {
                self.artist_card(ui, a);
            }
        });
        self.followed_artists = artists;
    }

    // ------------------------------------------------ guardados / podcasts / audiolibros / carpetas

    /// Episodios guardados ("Tus episodios").
    fn saves_page(&mut self, ui: &mut egui::Ui) {
        if !self.signed_in() {
            self.welcome(ui);
            return;
        }
        let p = theme::palette(ui.ctx());
        self.request_once("saved_episodes", Req::SavedEpisodes);
        let list = self.saved_episode_list.clone();
        Self::page_title(ui, "", "Guardados", &format!("{} episodios guardados", list.len()));
        if list.is_empty() {
            ui.label(RichText::new("Guarda episodios con el marcador de cualquier podcast y aparecerán aquí.").color(p.weak));
            return;
        }
        let uris: Vec<String> = list.iter().map(|e| e.episode.uri.clone()).collect();
        for (i, se) in list.iter().enumerate() {
            let show = Show { id: se.show_id.clone(), name: se.show_name.clone(), ..Show::default() };
            self.episode_row(ui, &show, &se.episode, i, &uris);
        }
    }

    /// Podcasts que sigues.
    fn shows_page(&mut self, ui: &mut egui::Ui) {
        if !self.signed_in() {
            self.welcome(ui);
            return;
        }
        let p = theme::palette(ui.ctx());
        self.request_once("saved_shows", Req::SavedShows);
        let list = self.followed_shows_list.clone();
        Self::page_title(ui, "", "Podcasts", &format!("{} podcasts que sigues", list.len()));
        if list.is_empty() {
            ui.label(RichText::new("Todavía no sigues ningún podcast. Búscalos y pulsa «Seguir».").color(p.weak));
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 8.0);
            for s in &list {
                let r = self.card(ui, CardInfo { kind: CardKind::Album, cover: s.cover(300), title: &s.name, subtitle: &s.publisher, count: None, pinned: false });
                if r.clicked() {
                    self.shows.entry(s.id.clone()).or_insert_with(|| (s.clone(), Vec::new()));
                    self.actions.push(Action::Go(Page::Show(s.id.clone())));
                }
                let sid = s.id.clone();
                r.context_menu(|ui| {
                    if Self::menu_item(ui, Some(Icon::NewTab), "Abrir en una nueva pestaña", false).clicked() {
                        self.actions.push(Action::OpenInTab(Page::Show(sid.clone())));
                        ui.close();
                    }
                    if Self::menu_item(ui, Some(Icon::Close), "Dejar de seguir", false).clicked() {
                        self.actions.push(Action::FollowShow(sid.clone(), false));
                        ui.close();
                    }
                });
            }
        });
    }

    /// Audiolibros guardados.
    fn audiobooks_page(&mut self, ui: &mut egui::Ui) {
        if !self.signed_in() {
            self.welcome(ui);
            return;
        }
        let p = theme::palette(ui.ctx());
        self.request_once("saved_audiobooks", Req::SavedAudiobooks);
        let list = self.audiobooks.clone();
        Self::page_title(ui, "", "Audiolibros", &format!("{} audiolibros guardados", list.len()));
        if list.is_empty() {
            ui.label(RichText::new("No tienes audiolibros guardados.").color(p.weak));
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 8.0);
            for b in &list {
                let sub = b.authors_str();
                let r = self.card(ui, CardInfo { kind: CardKind::Album, cover: b.cover(300), title: &b.name, subtitle: &sub, count: None, pinned: false });
                if r.clicked() {
                    self.actions.push(Action::Go(Page::Show(b.id.clone())));
                }
            }
        });
    }

    /// Carpetas de playlists (del rootlist interno de Spotify).
    fn folders_page(&mut self, ui: &mut egui::Ui) {
        if !self.signed_in() {
            self.welcome(ui);
            return;
        }
        let p = theme::palette(ui.ctx());
        self.request_once("rootlist", Req::Rootlist);
        let folders = self.folders.clone();
        Self::page_title(ui, "", "Carpetas", &format!("{} carpetas", folders.len()));
        if Self::secondary_button(ui, "Nueva carpeta", true).clicked() {
            self.folder_dialog = Some(FolderDialog { id: None, name: String::new(), playlist: None, busy: false });
        }
        ui.add_space(6.0);
        if folders.is_empty() {
            if self.requested.contains("rootlist") && self.folders.is_empty() {
                ui.label(RichText::new("No tienes carpetas. Crea una aquí o desde el menú de cualquier playlist («Añadir a carpeta»).").color(p.weak));
            }
            return;
        }
        for f in &folders {
            ui.add_space(10.0);
            let head = ui
                .horizontal(|ui| {
                    let (ir, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                    icons::paint(ui.painter(), ir, p.weak, Icon::Folder);
                    ui.label(RichText::new(&f.name).font(theme::bold(18.0)));
                    ui.label(RichText::new(format!("{} playlists", f.playlists.len())).small().color(p.weak));
                    icons::button(ui, Icon::More, 22.0, p.weak).on_hover_text("Opciones de la carpeta")
                })
                .inner;
            let menu = |app: &mut Self, ui: &mut egui::Ui| {
                if Self::menu_item(ui, Some(Icon::Edit), "Renombrar…", false).clicked() {
                    app.folder_dialog = Some(FolderDialog { id: Some(f.id.clone()), name: f.name.clone(), playlist: None, busy: false });
                    ui.close();
                }
                if Self::menu_item(ui, Some(Icon::Trash), "Eliminar carpeta", false).clicked() {
                    app.api.send(Req::FolderDelete(f.id.clone()));
                    ui.close();
                }
            };
            egui::Popup::menu(&head).show(|ui| {
                ui.set_min_width(250.0);
                menu(self, ui)
            });
            ui.add_space(4.0);
            let pls: Vec<Playlist> = f.playlists.iter().filter_map(|id| self.playlists.iter().find(|pl| &pl.id == id).cloned()).collect();
            if pls.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(26.0);
                    ui.label(RichText::new("Sus playlists no están en tu biblioteca cargada.").small().color(p.faint));
                });
            }
            for pl in &pls {
                let sub = match pl.owner_name() {
                    "" => "Playlist".to_string(),
                    owner => format!("De {owner}"),
                };
                let r = self.list_entry(ui, pl.cover(64), CardKind::Playlist, &pl.name, &sub);
                if r.clicked() {
                    self.actions.push(Action::Go(Page::Playlist(pl.id.clone())));
                }
                self.playlist_row_menu(&r, pl);
            }
        }
    }

    // ---------------------------------------------------------------- playlist

    fn playlist_page(&mut self, ui: &mut egui::Ui, id: String) {
        if !self.signed_in() {
            self.welcome(ui);
            return;
        }
        // Medida para el registro: desde que se abre la página hasta que se ve su primera fila.
        // Es una apertura nueva si cambia el id o si el fotograma anterior no la dibujó.
        let frame = ui.ctx().cumulative_frame_nr();
        if self.pl_open_mark.as_ref().map_or(true, |(m, last, _)| *m != id || last + 1 < frame) {
            self.pl_open_mark = Some((id.clone(), frame, Some(Instant::now())));
        }
        let meta = self
            .playlists
            .iter()
            .find(|pl| pl.id == id)
            .cloned()
            .or_else(|| self.playlist_meta.get(&id).cloned());
        // Las pistas primero: su carga trae también nombre, portada y descripción (playlist4).
        // /playlists/{id} solo añade privacidad, seguidores y el nombre del propietario, que de
        // las de la biblioteca ya se saben; y las de Spotify (37i9) no están en la Web API. Así
        // abrirlas no gasta una lectura de cuota ni un hilo esperando un 429. Con su copia en
        // disco al día (mismo snapshot_id que el listado) no se pide nada.
        self.ensure_playlist(&id);
        let in_library = self.in_library(&id);
        // Sin la biblioteca cargada aún (primer arranque, sin instantánea) no se sabe si está en
        // ella: se espera a saberlo en vez de gastar la lectura en la ráfaga inicial.
        if self.playlists_loaded && !in_library && !id.starts_with("37i9dQZ") {
            self.request_once(&format!("plmeta:{id}"), Req::PlaylistMeta(id.clone()));
        }
        let list = self.lists.remove(&id).unwrap_or_default();
        if let Some((_, last, opened)) = self.pl_open_mark.as_mut() {
            *last = frame;
            if !list.tracks.is_empty() {
                if let Some(t0) = opened.take() {
                    log::info!("[t] abrir playlist {id}: primera fila en {} ms", t0.elapsed().as_millis());
                }
            }
        }

        // De las de la biblioteca ya no se pide /playlists/{id}: la privacidad buena es la de la
        // biblioteca (se recarga al conectar y tras editarla), no la de playlist4 (que no la trae)
        // ni la de una copia en disco antigua. El editor parte de `public` (sin dato, pública):
        // con una vieja, guardar otro cambio podía deshacer el de privacidad.
        let full_meta = match (self.playlist_meta.get(&id).cloned(), meta) {
            (Some(mut m), Some(lib)) if in_library => {
                if lib.public.is_some() {
                    m.public = lib.public;
                }
                if m.owner.display_name.is_none() && lib.owner.display_name.is_some() {
                    m.owner = lib.owner;
                }
                Some(m)
            }
            (m, lib) => m.or(lib),
        };
        let mine = full_meta.as_ref().map(|pl| self.is_mine(pl)).unwrap_or(false);
        let collaborative = full_meta.as_ref().and_then(|pl| pl.collaborative).unwrap_or(false);
        let (name, owner, owner_id, cover, uri) = match &full_meta {
            Some(pl) => (
                pl.name.clone(),
                pl.owner_name().to_string(),
                pl.owner.id.clone(),
                pl.cover(300).map(|s| s.to_string()),
                pl.uri.clone(),
            ),
            None => ("Playlist".to_string(), String::new(), None, None, format!("spotify:playlist:{id}")),
        };
        let total = if list.total > 0 {
            list.total
        } else {
            full_meta.as_ref().and_then(|pl| pl.tracks.as_ref()).map(|t| t.total).unwrap_or(0)
        };
        let summary = self.list_summary(&id, &list);
        let total_ms = summary.total_ms;
        let mut parts = vec![format!("{total} canciones")];
        if !list.tracks.is_empty() {
            parts.push(fmt_total(total_ms));
        }
        let mut chips = vec![if mine { "Tu playlist".to_string() } else { "Playlist".to_string() }];
        if let Some(pl) = &full_meta {
            match (pl.collaborative, pl.public) {
                (Some(true), _) => chips.push("Colaborativa".into()),
                (_, Some(false)) => chips.push("Privada".into()),
                (_, Some(true)) => chips.push("Pública".into()),
                _ => {}
            }
            if let Some(f) = pl.followers.as_ref().and_then(|f| f.total) {
                if f > 0 {
                    chips.push(format!("{} seguidores", fmt_thousands(f)));
                }
            }
        }
        let description = full_meta.as_ref().and_then(|pl| pl.description.clone()).filter(|d| !d.is_empty()).map(|d| strip_html(&d));
        let pinned = self.settings.pinned.contains(&id);
        let tracks = list.tracks;
        let loading = list.loading;
        let gen = list.gen;
        // Como en Me gusta: imágenes de artistas solo con la lista completa.
        let thumbs_ready = !loading || (total > 0 && tracks.len() >= total as usize);
        // Autores: el propietario y, por orden de aparición, quienes han añadido canciones. El
        // propietario se pone aquí y no en el resumen: sus datos pueden llegar después.
        let mut contributors: Vec<String> = owner_id.iter().cloned().collect();
        contributors.extend(summary.added_by.iter().filter(|b| owner_id.as_ref() != Some(*b)).cloned());
        // Spotify ya no marca como colaborativas las públicas: si hay varios autores, lo es.
        let collaborative = collaborative || contributors.len() > 1;
        let editable = mine || collaborative;
        // «De …»: el propietario o, si varias personas han añadido canciones, todas (enlaces a su
        // perfil, como en Spotify).
        let by: Vec<(String, Option<Page>)> = if contributors.len() > 1 {
            contributors.iter().map(|u| (self.user_display(u), Some(Page::User(u.clone())))).collect()
        } else if !owner.is_empty() {
            vec![(owner.clone(), owner_id.clone().map(Page::User))]
        } else {
            Vec::new()
        };
        let id2 = id.clone();
        let uri2 = uri.clone();
        let full_meta2 = full_meta.clone();

        self.two_columns(
            ui,
            |app, ui| {
                let p = theme::palette(ui.ctx());
                ui.spacing_mut().item_spacing.y = 0.0;
                app.collection_header(ui, &name, description.as_deref(), &by, &parts);
                let link = uri_to_link(&uri2);
                let id_m = id2.clone();
                let meta_m = full_meta2.clone();
                let tracks_m = &tracks;
                let menu: Option<Box<dyn FnOnce(&mut Self, &mut egui::Ui) + '_>> = Some(Box::new(move |app: &mut Self, ui: &mut egui::Ui| {
                    if Self::menu_item(ui, Some(Icon::Pin), if pinned { "Desfijar de la biblioteca" } else { "Fijar en la biblioteca" }, false).clicked() {
                        app.actions.push(Action::Pin(id_m.clone(), !pinned));
                        ui.close();
                    }
                    if mine {
                        if Self::menu_item(ui, Some(Icon::Edit), "Editar playlist…", false).clicked() {
                            app.actions.push(Action::OpenEditor(meta_m.clone()));
                            ui.close();
                        }
                        if Self::menu_item(ui, Some(Icon::Camera), "Cambiar imagen…", false).clicked() {
                            app.actions.push(Action::PickPlaylistImage(id_m.clone()));
                            ui.close();
                        }
                        app.invite_menu_item(ui, &id_m);
                    } else {
                        if Self::menu_item(ui, Some(Icon::PlusCircle), if in_library { "Quitar de tu biblioteca" } else { "Guardar en tu biblioteca" }, false).clicked() {
                            app.actions.push(Action::FollowPlaylist(id_m.clone(), !in_library));
                            ui.close();
                        }
                        // Su botón de la barra es el de guardarla: copiar sus canciones, desde aquí.
                        if Self::menu_item(ui, Some(Icon::PlusSquare), "Añadir todas a una playlist", false).clicked() {
                            app.open_add_dialog(tracks_m.iter().map(|t| t.uri.clone()).collect());
                            ui.close();
                        }
                    }
                    if in_library || mine {
                        app.folder_menu(ui, &id_m);
                    }
                    if Self::menu_item(ui, Some(Icon::NewTab), "Abrir en una nueva pestaña", false).clicked() {
                        app.actions.push(Action::OpenInTab(Page::Playlist(id_m.clone())));
                        ui.close();
                    }
                }));
                app.collection_bar(
                    ui,
                    &id,
                    &tracks,
                    Some(&uri2),
                    Some(&link),
                    |app, ui, at| {
                        let ink = theme::ink(&theme::palette(ui.ctx()));
                        if mine {
                            // Ya es de tu biblioteca: sus canciones a otra playlist, con el icono de
                            // añadir a playlist del reproductor. Los uris solo al pulsar: copiarlos
                            // todos en cada fotograma costaba con miles.
                            let r = Self::slot_button(ui, ui.id().with("add_all"), at, Icon::PlusSquare, 29.0, ink.dim);
                            if r.on_hover_text("Añadir todas a una playlist").clicked() {
                                app.open_add_dialog(tracks.iter().map(|t| t.uri.clone()).collect());
                            }
                        } else {
                            // De otra persona, mix o radio: como un álbum, guardarla en tu biblioteca.
                            let (icon, color, tip) = if in_library {
                                (Icon::CheckCircle, GREEN, "Quitar de tu biblioteca")
                            } else {
                                (Icon::PlusCircle, ink.dim, "Guardar en tu biblioteca")
                            };
                            if Self::slot_button(ui, ui.id().with("save_playlist"), at, icon, 24.0, color).on_hover_text(tip).clicked() {
                                app.actions.push(Action::FollowPlaylist(id.clone(), !in_library));
                            }
                        }
                        true
                    },
                    menu,
                );
                // La carga falló o llegó con huecos: lo que hay y «Reintentar». Texto fijo, no
                // Self::loading, que repinta 4 veces por segundo mientras espera el reintento.
                // Vacía cuenta aunque no se sepa el total (falló el primer lote sin metadatos).
                let partial = app.list_retry.contains_key(&id) && (tracks.is_empty() || tracks.len() < total as usize);
                let retrying = partial && app.list_retry_busy(&id);
                // Sin ninguna fila aún (ni copia): el hueco de las filas, no un «Cargando».
                if (loading || retrying) && tracks.is_empty() {
                    ui.add_space(TABLE_HEAD_H);
                    Self::skeleton_rows(ui, 8, TABLE_ROW_H, 50.0, 43.0);
                } else if loading {
                    ui.add_space(8.0);
                    Self::loading(ui, &format!("Cargando {} de {}", tracks.len(), total));
                } else if partial {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let shown = if tracks.is_empty() {
                            "No se pudo cargar la playlist".to_string()
                        } else {
                            format!("Mostrando {} de {}", tracks.len(), total)
                        };
                        ui.label(RichText::new(shown).color(p.weak));
                        ui.label(RichText::new("·").color(p.faint));
                        if retrying {
                            ui.label(RichText::new("reintentando").color(p.weak));
                        } else {
                            let r = ui.add(Label::new(RichText::new("Reintentar").color(p.text)).sense(Sense::click()));
                            if r.hovered() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            if r.clicked() {
                                app.actions.push(Action::RetryList(id.clone()));
                            }
                        }
                    });
                } else if tracks.is_empty() && (total > 0 || app.warming.contains_key(&id)) {
                    // También mientras se lee su copia en disco (sin total aún): no está vacía.
                    ui.add_space(TABLE_HEAD_H);
                    Self::skeleton_rows(ui, 8, TABLE_ROW_H, 50.0, 43.0);
                } else if tracks.is_empty() {
                    ui.add_space(24.0);
                    ui.label(RichText::new("Esta playlist está vacía. Añade canciones con el botón + de cualquier fila.").color(p.weak));
                }
                let shown = app.filter_tracks(&id, Some(gen), &tracks);
                let src = if shown.len() == tracks.len() { Source::Context(&uri) } else { Source::Tracks };
                // En playlists colaborativas se ve quién añadió cada canción (como en Spotify).
                app.rows_added_by = collaborative;
                app.track_rows(
                    ui,
                    &id,
                    &shown,
                    RowOpts {
                        show_cover: true,
                        show_album: true,
                        numbered: false,
                        header: true,
                        source: src,
                        editable_playlist: editable.then_some(id.as_str()),
                        selectable: true,
                        select: true,
                    },
                );
                app.rows_added_by = false;
            },
            |app, ui| {
                app.info_card(ui, cover.as_deref(), &chips, &summary.top, thumbs_ready);
            },
        );
        // Con su versión: si no, el resumen y la búsqueda guardados no valdrían al fotograma siguiente.
        self.lists.insert(id.clone(), crate::app::TrackList { tracks, total, loading, gen });
    }

    // ------------------------------------------------------------------- álbum

    fn album_page(&mut self, ui: &mut egui::Ui, id: String) {
        if !self.signed_in() {
            self.welcome(ui);
            return;
        }
        self.warm_album(&id);
        self.request_once(&format!("album:{id}"), Req::Album(id.clone()));
        let Some(album) = self.albums.remove(&id) else {
            Self::loading(ui, "Cargando");
            return;
        };
        let all: Vec<Track> = album.tracks.as_ref().map(|pg| pg.items.clone()).unwrap_or_default();
        let total_ms: u64 = all.iter().map(|t| t.duration_ms as u64).sum();
        let kind = match album.album_type.as_deref() {
            Some("single") => "Sencillo",
            Some("compilation") => "Recopilatorio",
            _ => "Álbum",
        };
        let parts = vec![album.year().to_string(), format!("{} canciones", all.len()), fmt_total(total_ms)];
        // Chips: géneros del álbum (metadatos internos) y, si faltan, los de sus artistas.
        let mut chips: Vec<String> = album.genres.iter().take(5).cloned().collect();
        for a in &album.artists {
            if let Some(ar) = a.id.as_ref().and_then(|i| self.artists.get(i)).and_then(|pg| pg.artist.as_ref()) {
                for g in ar.genres.iter().take(5) {
                    if !chips.contains(g) {
                        chips.push(g.clone());
                    }
                }
            }
        }
        if chips.is_empty() {
            chips = vec![kind.to_string(), album.year().to_string()];
        }
        // Artistas del álbum y todos los colaboradores de sus canciones (sin repetir).
        let artists = album.artists.clone();
        // Colaboradores de las canciones (solo para la columna derecha).
        let mut collaborators = artists.clone();
        for t in &all {
            for a in &t.artists {
                if a.id.is_some() && !collaborators.iter().any(|x| x.id == a.id) {
                    collaborators.push(a.clone());
                }
            }
        }
        let cover = album.cover(300).map(|s| s.to_string());
        let uri = album.uri.clone();
        let name = album.name.clone();
        let filter = self.list_query(&id);
        let filtered = self.filter_tracks(&id, None, &all);
        if self.sel_list == id {
            if let Some(mark) = self.sel.iter().find(|s| s.starts_with('#')).cloned() {
                self.sel.remove(&mark);
                if let Some(t) = mark[1..].parse::<usize>().ok().and_then(|n| all.get(n)) {
                    self.sel.insert(t.uri.clone());
                }
            }
        }
        let saved = self.saved_albums.iter().any(|a| a.id == id);
        let by: Vec<(String, Option<Page>)> = artists.iter().map(|a| (a.name.clone(), a.id.clone().map(Page::Artist))).collect();

        self.two_columns(
            ui,
            |app, ui| {
                let p = theme::palette(ui.ctx());
                ui.spacing_mut().item_spacing.y = 0.0;
                app.collection_header(ui, &name, None, &by, &parts);
                let link = uri_to_link(&uri);
                let id_m = id.clone();
                let uri_m = uri.clone();
                let artists_m = artists.clone();
                app.collection_bar(
                    ui,
                    &id,
                    &all,
                    Some(&uri),
                    Some(&link),
                    |app, ui, at| {
                        let ink = theme::ink(&theme::palette(ui.ctx()));
                        let (icon, color, tip) = if saved {
                            (Icon::CheckCircle, GREEN, "Quitar de tu biblioteca")
                        } else {
                            (Icon::PlusCircle, ink.dim, "Guardar en tu biblioteca")
                        };
                        if Self::slot_button(ui, ui.id().with("save_album"), at, icon, 24.0, color).on_hover_text(tip).clicked() {
                            app.actions.push(Action::SaveAlbum(id.clone(), !saved));
                        }
                        true
                    },
                    Some(Box::new(move |app: &mut Self, ui: &mut egui::Ui| {
                        if Self::menu_item(ui, Some(Icon::NewTab), "Abrir en una nueva pestaña", false).clicked() {
                            app.actions.push(Action::OpenInTab(Page::Album(id_m.clone())));
                            ui.close();
                        }
                        if Self::menu_item(ui, Some(Icon::Share), "Copiar enlace", false).clicked() {
                            app.actions.push(Action::CopyText(uri_to_link(&uri_m), "Enlace"));
                            ui.close();
                        }
                        for a in &artists_m {
                            if let Some(aid) = &a.id {
                                if Self::menu_item(ui, Some(Icon::Artist), &format!("Ir a {}", a.name), false).clicked() {
                                    app.actions.push(Action::Go(Page::Artist(aid.clone())));
                                    ui.close();
                                }
                            }
                        }
                    })),
                );
                let src = if filter.is_empty() { Source::Context(&uri) } else { Source::Tracks };
                app.track_rows(
                    ui,
                    &id,
                    &filtered,
                    RowOpts {
                        show_cover: false,
                        show_album: false,
                        numbered: filter.is_empty(),
                        header: true,
                        source: src,
                        editable_playlist: None,
                        selectable: true,
                        select: true,
                    },
                );
                if filtered.is_empty() && !all.is_empty() {
                    ui.label(RichText::new("Ninguna canción coincide con la búsqueda.").small().color(p.weak));
                }
            },
            |app, ui| {
                app.info_card(ui, cover.as_deref(), &chips, &collaborators, true);
            },
        );
        self.albums.insert(id, album);
    }

    // ------------------------------------------------------------------ perfil

    fn user_page(&mut self, ui: &mut egui::Ui, id: String) {
        if !self.signed_in() {
            self.welcome(ui);
            return;
        }
        let p = theme::palette(ui.ctx());
        if !self.requested.contains(&format!("user:{id}")) {
            self.requested.insert(format!("user:{id}"));
            self.api.send(Req::User(id.clone()));
        }
        let is_me = self.my_id() == Some(id.as_str());
        match self.users.get(&id).cloned() {
            Some(u) => {
                let n_pl = self.user_playlists.get(&id).map(|pl| pl.len()).unwrap_or(0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 20.0;
                    self.cover(ui, u.cover(300), 120.0, true);
                    ui.vertical(|ui| {
                        ui.add_space(14.0);
                        ui.label(RichText::new(if is_me { "TU PERFIL" } else { "PERFIL" }).small().strong().color(p.weak));
                        ui.label(RichText::new(u.name()).font(theme::bold(30.0)));
                        let mut meta = u.followers.as_ref().and_then(|f| f.total).map(|n| format!("{} seguidores", fmt_thousands(n))).unwrap_or_default();
                        if n_pl > 0 {
                            if !meta.is_empty() {
                                meta.push_str(" · ");
                            }
                            meta.push_str(&format!("{n_pl} playlists públicas"));
                        }
                        ui.label(RichText::new(meta).small().color(p.weak));
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if !is_me {
                                self.follow_button(ui, "user", &id);
                            }
                            if icons::button(ui, Icon::Share, 30.0, p.weak).on_hover_text("Copiar enlace").clicked() {
                                self.actions.push(Action::CopyText(format!("https://open.spotify.com/user/{id}"), "Enlace"));
                            }
                        });
                    });
                });
            }
            None => {
                Self::loading(ui, "Cargando");
            }
        }
        if let Some(pls) = self.user_playlists.get(&id).cloned() {
            if !pls.is_empty() {
                Self::section_header(ui, "Playlists públicas", None);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(4.0, 8.0);
                    for pl in &pls {
                        self.playlist_card(ui, pl);
                    }
                });
            }
        }
    }

    fn web_api_banner(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        egui::Frame::new()
            .fill(GREEN.gamma_multiply(0.12))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(12)
            .show(ui, |ui| {
                if self.web_busy {
                    // Esperando a que se acepte en el navegador (encadenada al iniciar sesión o con
                    // «Conectar con Spotify»): por si la pestaña se cerró o se quiere dejar.
                    ui.label(RichText::new("Un último paso en el navegador").strong());
                    ui.label("Permite que Nanofy lea tu biblioteca (no tienes que crear nada). En cuanto aceptes, aparece aquí.");
                    ui.add_space(4.0);
                    let url = self.web_chain.as_ref().map(|c| c.url.clone());
                    ui.horizontal(|ui| {
                        if let Some(url) = url {
                            if ui.button("Abrir de nuevo en el navegador").clicked() {
                                crate::webauth::open_in_browser(&url);
                            }
                            if ui.button("Cancelar").clicked() {
                                self.cancel_web_chain();
                            }
                        }
                        Self::loading(ui, "Esperando");
                    });
                } else {
                    // Sin desvío a Ajustes: la autorización se abre desde aquí mismo.
                    ui.label(RichText::new("Conecta tu biblioteca").strong());
                    ui.label("Solo tienes que autorizar tu cuenta en la web de Spotify; no hace falta crear ninguna app.");
                    ui.add_space(4.0);
                    if Self::primary_button(ui, "Conectar con Spotify", true).clicked() {
                        self.connect_library();
                    }
                }
            });
        ui.add_space(6.0);
    }

    /// Tarjeta de sección de ajustes.
    fn settings_card(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
        let p = theme::palette(ui.ctx());
        ui.add_space(14.0);
        egui::Frame::new()
            .fill(p.card2)
            .corner_radius(CornerRadius::same(14))
            .inner_margin(egui::Margin::symmetric(18, 14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(title).font(theme::bold(17.0)));
                ui.add_space(8.0);
                add(ui);
            });
    }

    /// Fila de ajuste: etiqueta (y nota) a la izquierda, control a la derecha.
    fn setting_row(ui: &mut egui::Ui, label: &str, hint: Option<&str>, control: impl FnOnce(&mut egui::Ui)) {
        let p = theme::palette(ui.ctx());
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_width(280.0);
                ui.label(RichText::new(label).color(p.text));
                if let Some(h) = hint {
                    ui.label(RichText::new(h).small().color(p.weak));
                }
            });
            ui.add_space(12.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                control(ui);
            });
        });
        ui.add_space(4.0);
    }

    fn web_api_settings(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        let configured = self.api.web_configured();
        ui.horizontal(|ui| {
            let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui.painter().circle_filled(dot.center(), 5.0, if configured { GREEN } else { ERROR_RED });
            if configured {
                ui.label(RichText::new("Conectada con Spotify").strong());
            } else {
                ui.label(RichText::new("Sin conectar").strong());
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if configured {
                    if Self::secondary_button(ui, "Desconectar", !self.web_busy).clicked() {
                        self.web_busy = true;
                        self.api.send(Req::WebDisconnect);
                    }
                }
                let label = if configured { "Reconectar" } else { "Conectar con Spotify" };
                if Self::primary_button(ui, label, !self.web_busy).clicked() {
                    self.connect_library();
                }
                if self.web_busy {
                    Self::loading(ui, "");
                }
            });
        });
        // Esperando al navegador: por si la pestaña se cerró.
        if let Some(url) = self.web_chain.as_ref().map(|c| c.url.clone()) {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("¿No se abrió o la cerraste?").small().color(p.weak));
                if ui.link(RichText::new("Abrir de nuevo").small()).clicked() {
                    crate::webauth::open_in_browser(&url);
                }
                if ui.link(RichText::new("Cancelar").small()).clicked() {
                    self.cancel_web_chain();
                }
            });
        }
        ui.add_space(8.0);
        ui.label(
            RichText::new(
                "Autoriza tu cuenta en el navegador (una sola vez). Habilita la biblioteca, la \
                 búsqueda, las playlists, el corazón y seguir artistas. No necesitas crear ninguna app.",
            )
            .small()
            .color(p.weak),
        );

        // Opcional y plegado: app de desarrollador propia solo para LECTURAS, con su propia cuota.
        // Las escrituras (corazón, seguir) siguen yendo por la conexión de arriba. Nadie la
        // necesita, así que no está a la vista ni se vende como «más rápida».
        ui.add_space(10.0);
        let personal = self.api.personal_configured();
        egui::CollapsingHeader::new(RichText::new("Avanzado (opcional, no hace falta)").color(p.weak))
            .id_salt("web_api_avanzado")
            .default_open(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                    ui.painter().circle_filled(dot.center(), 5.0, if personal { GREEN } else { p.weak });
                    ui.label(RichText::new("Tu propia app de desarrollador").strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if personal && Self::secondary_button(ui, "Quitar", !self.web_busy).clicked() {
                            self.web_busy = true;
                            self.api.send(Req::WebDisconnectPersonal);
                        }
                        let label = if personal { "Reconectar app" } else { "Conectar mi app" };
                        let can = !self.web_busy && self.draft.client_id.trim().len() >= 16;
                        if Self::primary_button(ui, label, can).clicked() {
                            self.settings.client_id = self.draft.client_id.trim().to_string();
                            self.settings.save(&self.paths);
                            self.web_busy = true;
                            self.status("Se ha abierto el navegador para autorizar tu app…");
                            self.api.send(Req::WebConnectPersonal(self.settings.client_id.clone()));
                        }
                        if self.web_busy {
                            Self::loading(ui, "");
                        }
                    });
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "No hace falta: Nanofy funciona entero sin ella. Solo si ya tienes una app en \
                         developer.spotify.com y quieres que las lecturas usen su cuota, añádele esta \
                         Redirect URI y pega aquí su Client ID:",
                    )
                    .small()
                    .color(p.weak),
                );
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(RichText::new(crate::webauth::PERSONAL_REDIRECT_URI).monospace().color(GREEN));
                    if icons::button(ui, Icon::Share, 20.0, p.weak).on_hover_text("Copiar la URI").clicked() {
                        self.actions.push(Action::CopyText(crate::webauth::PERSONAL_REDIRECT_URI.to_string(), "URI"));
                    }
                });
                Self::field_label(ui, "CLIENT ID DE TU APP");
                let w = (ui.available_width() - 40.0).max(220.0);
                Self::field_box_fill(ui, p.card, |ui| ui.add(egui::TextEdit::singleline(&mut self.draft.client_id).hint_text("32 caracteres").frame(egui::Frame::NONE).desired_width(w)));
            });
    }

    /// Fundido entre canciones. Va fuera del borrador: se aplica y se guarda al instante, sin
    /// «Guardar» ni reiniciar el reproductor (`set_crossfade`), por eso lee de `settings`.
    fn crossfade_settings(&mut self, ui: &mut egui::Ui) {
        let mut on = self.settings.crossfade;
        if Self::toggle_pad(ui, &mut on, "Fundido entre canciones", 0.0) {
            self.set_crossfade(on, self.settings.crossfade_secs, self.settings.crossfade_albums, true);
            self.status(if on { "Fundido activado" } else { "Fundido desactivado" });
        }
        // Apagado, sus opciones se ven atenuadas (como el nivel de volumen sin normalización).
        ui.add_enabled_ui(self.settings.crossfade, |ui| {
            Self::setting_row(
                ui,
                "Duración del fundido",
                Some(
                    "La siguiente canción entra mientras la anterior se apaga. No se aplica al cambiar de \
                     canción a mano, al repetir una canción ni en podcasts.",
                ),
                |ui| {
                    let p = theme::palette(ui.ctx());
                    // El raíl del deslizador debe verse sobre la tarjeta (mismo color que su fondo por defecto).
                    ui.visuals_mut().widgets.inactive.bg_fill = p.hover;
                    let (min, max) = (crate::config::CROSSFADE_SECS_MIN, crate::config::CROSSFADE_SECS_MAX);
                    let mut secs = self.settings.crossfade_secs;
                    ui.label(RichText::new(format!("{min} s")).small().color(p.weak));
                    let r = ui.add_sized(vec2(200.0, 22.0), egui::Slider::new(&mut secs, min..=max).step_by(1.0).show_value(false));
                    ui.label(RichText::new(format!("{max} s")).small().color(p.weak));
                    ui.label(RichText::new(format!("{secs} s")).strong().color(p.text));
                    // Mientras se arrastra ya suena con el valor nuevo; se guarda al soltar (o al
                    // cambiarlo con clic o teclado, que no arrastran).
                    let persist = r.drag_stopped() || (r.changed() && !r.dragged());
                    if r.changed() || persist {
                        self.set_crossfade(true, secs, self.settings.crossfade_albums, persist);
                    }
                },
            );
            let mut albums = self.settings.crossfade_albums;
            if Self::toggle_pad(ui, &mut albums, "Fundir también canciones seguidas de un mismo álbum", 0.0) {
                self.set_crossfade(true, self.settings.crossfade_secs, albums, true);
            }
            let p = theme::palette(ui.ctx());
            ui.label(
                RichText::new("Spotify no funde temas consecutivos de un álbum para respetar las transiciones del artista.")
                    .small()
                    .color(p.weak),
            );
        });
        // Con «al terminar la canción» puesto, la canción acaba entera (ver `set_sleep_end_of_track`).
        if self.settings.crossfade && self.sleep_end_of_track {
            ui.label(
                RichText::new("Fundido suspendido mientras el temporizador «al terminar la canción» está puesto.")
                    .small()
                    .color(theme::palette(ui.ctx()).weak),
            );
        }
    }

    pub fn settings_page(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let p = theme::palette(&ctx);
        ui.label(RichText::new("Ajustes").font(theme::bold(28.0)));
        ui.label(
            RichText::new("El fundido entre canciones se aplica al instante; los demás cambios de reproducción, al guardar; el resto, al instante.")
                .small()
                .color(p.weak),
        );

        Self::settings_card(ui, "Reproducción", |ui| {
            Self::setting_row(ui, "Nombre en Spotify Connect", Some("Así aparece Nanofy en la lista de dispositivos."), |ui| {
                let p = theme::palette(ui.ctx());
                ui.set_max_width(260.0);
                Self::field_box_fill(ui, p.card, |ui| ui.add(egui::TextEdit::singleline(&mut self.draft.device_name).frame(egui::Frame::NONE).desired_width(240.0)));
            });
            // Sin opción «sin pérdida»: los FLAC de Spotify van tras su DRM y nunca podían sonar.
            // Tampoco se promete «sin remuestreo»: en Windows la salida va a la frecuencia del
            // dispositivo, que suele ser 48 kHz.
            Self::setting_row(
                ui,
                "Calidad de audio",
                Some(
                    "Ogg Vorbis, el mismo formato que la app de escritorio de Spotify. «Muy alta» es la \
                     máxima calidad que Spotify entrega a apps que no son las suyas.",
                ),
                |ui| {
                    for q in [Quality::Low, Quality::Normal, Quality::High] {
                        if Self::pill(ui, q.label(), self.draft.quality == q).clicked() {
                            self.draft.quality = q;
                        }
                    }
                },
            );
            ui.label(
                RichText::new("Sin pérdida (FLAC): Spotify solo la entrega a sus apps y dispositivos certificados.")
                    .small()
                    .color(theme::palette(ui.ctx()).weak),
            );
            // Solo lectura: a qué frecuencia abre el sistema el dispositivo no lo elige Nanofy (en
            // Windows manda el formato compartido del dispositivo, casi siempre 48 kHz).
            let out = crate::backend::audio_output();
            let out_hint = match out.resampling() {
                Some(Resampling::None) => "El dispositivo ya está a 44,1 kHz: el audio llega sin convertir.",
                _ if cfg!(windows) => {
                    "Para evitar el remuestreo, pon el dispositivo en 44100 Hz: Configuración → Sistema → \
                     Sonido → (dispositivo) → Formato."
                }
                _ => "Para evitar el remuestreo, pon el dispositivo de salida en 44100 Hz.",
            };
            Self::setting_row(ui, "Salida de audio", Some(out_hint), |ui| {
                let p = theme::palette(ui.ctx());
                match out.summary(true) {
                    Some(text) => ui.label(RichText::new(text).color(p.text)),
                    None => ui.label(RichText::new("Se sabrá al reproducir la primera canción").color(p.weak)),
                };
            });
            ui.add_space(4.0);
            let mut v = self.draft.normalisation;
            if Self::toggle_pad(ui, &mut v, "Normalizar volumen", 0.0) {
                self.draft.normalisation = v;
            }
            ui.label(
                RichText::new("Como en Spotify: todas las canciones suenan a un volumen parecido.")
                    .small()
                    .color(theme::palette(ui.ctx()).weak),
            );
            // Los tres niveles de Spotify. Sin normalización no hacen nada: se ven apagados.
            let loudness_hint = self.draft.loudness.hint();
            ui.add_enabled_ui(self.draft.normalisation, |ui| {
                Self::setting_row(ui, "Nivel de volumen", Some(loudness_hint), |ui| {
                    for l in [Loudness::Loud, Loudness::Normal, Loudness::Quiet] {
                        if Self::pill(ui, l.label(), self.draft.loudness == l).on_hover_text(l.hint()).clicked() {
                            self.draft.loudness = l;
                        }
                    }
                });
            });
            let mut v = self.draft.gapless;
            if Self::toggle_pad(ui, &mut v, "Reproducción sin pausas (gapless)", 0.0) {
                self.draft.gapless = v;
            }
            self.crossfade_settings(ui);
            let mut v = self.draft.autoplay;
            if Self::toggle_pad(ui, &mut v, "Autoplay al terminar una lista", 0.0) {
                self.draft.autoplay = v;
            }
            let mut v = self.draft.smart_preload;
            if Self::toggle_pad(ui, &mut v, "Precarga inteligente", 0.0) {
                self.draft.smart_preload = v;
            }
            ui.label(
                RichText::new("Prepara la canción sobre la que pasas el ratón para que suene al instante.")
                    .small()
                    .color(theme::palette(ui.ctx()).weak),
            );
            let mut v = self.draft.media_keys;
            if Self::toggle_pad(ui, &mut v, "Teclas multimedia del sistema", 0.0) {
                self.draft.media_keys = v;
            }
            Self::setting_row(ui, "Caché de audio en disco", Some("Las canciones descargadas y ya escuchadas se guardan aquí."), |ui| {
                // El raíl del deslizador debe verse sobre la tarjeta (mismo color que su fondo por defecto).
                ui.visuals_mut().widgets.inactive.bg_fill = theme::palette(ui.ctx()).hover;
                ui.add_sized(vec2(220.0, 22.0), egui::Slider::new(&mut self.draft.audio_cache_mb, 0..=4096).step_by(64.0).show_value(false));
                ui.label(RichText::new(format!("{} MB", self.draft.audio_cache_mb)).color(theme::palette(ui.ctx()).text));
            });
        });

        Self::settings_card(ui, "Apariencia y rendimiento", |ui| {
            Self::setting_row(ui, "Tema", None, |ui| {
                for (t, l) in [(Theme::System, "Sistema"), (Theme::Dark, "Oscuro"), (Theme::Light, "Claro")] {
                    if Self::pill(ui, l, self.draft.theme == t).clicked() {
                        self.draft.theme = t;
                    }
                }
            });
            Self::setting_row(ui, "Escala de la interfaz", None, |ui| {
                // El raíl del deslizador debe verse sobre la tarjeta (mismo color que su fondo por defecto).
                ui.visuals_mut().widgets.inactive.bg_fill = theme::palette(ui.ctx()).hover;
                ui.add_sized(vec2(220.0, 22.0), egui::Slider::new(&mut self.draft.zoom, 0.8..=1.6).step_by(0.05).show_value(false));
                ui.label(RichText::new(format!("{:.0} %", self.draft.zoom * 100.0)).color(theme::palette(ui.ctx()).text));
            });
            Self::setting_row(ui, "Límite de fotogramas", Some("Solo durante animaciones; en reposo no se repinta."), |ui| {
                for f in [60u32, 120, 144, 240] {
                    if Self::pill(ui, &format!("{f} fps"), self.draft.fps_cap == f).clicked() {
                        self.draft.fps_cap = f;
                    }
                }
            });
        });

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let changed = self.draft != self.settings;
            if Self::primary_button(ui, "Guardar", changed).clicked() {
                self.save_settings(&ctx);
            }
            if Self::secondary_button(ui, "Descartar", changed).clicked() {
                self.draft = self.settings.clone();
            }
            // La calidad, gapless y el volumen se aplican sin reiniciar; solo esto corta la música.
            if self.settings.restart_differs(&self.draft) {
                ui.label(RichText::new("Al guardar se reinicia el reproductor.").small().color(p.weak));
            }
        });

        Self::settings_card(ui, "Biblioteca · playlists, búsqueda, Me gusta y perfiles", |ui| {
            self.web_api_settings(ui);
        });

        Self::settings_card(ui, "Cuenta", |ui| match self.auth.clone() {
            Auth::LoggedIn { username } => {
                let product = self.user.as_ref().and_then(|u| u.product.clone()).unwrap_or_default();
                let img = self
                    .my_id()
                    .map(|s| s.to_string())
                    .and_then(|id| self.users.get(&id).and_then(|u| u.cover(64).map(|s| s.to_string())))
                    .or_else(|| self.user.as_ref().and_then(|u| u.cover(64).map(|s| s.to_string())));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    self.cover(ui, img.as_deref(), 44.0, true);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(self.display_name()).strong());
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&username).small().color(p.weak));
                            let _ = Self::pill(ui, if product == "premium" { "Premium" } else { &product }, product == "premium");
                        });
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if Self::secondary_button(ui, "Cerrar sesión", true).clicked() {
                            self.backend.send(crate::backend::Cmd::Logout);
                            self.go(Page::Home);
                        }
                        if let Some(id) = self.my_id().map(|s| s.to_string()) {
                            if Self::secondary_button(ui, "Ver mi perfil", true).clicked() {
                                self.actions.push(Action::Go(Page::User(id)));
                            }
                        }
                    });
                });
                if product == "free" {
                    ui.add_space(6.0);
                    ui.label(RichText::new("Con una cuenta gratuita Spotify no permite reproducir música en clientes externos.").small().color(ERROR_RED));
                }
            }
            Auth::LoggingIn => {
                ui.horizontal(|ui| {
                    Self::loading(ui, "Completa el inicio de sesión en el navegador");
                    let cancelling = self.login_cancelling();
                    if Self::secondary_button(ui, if cancelling { "Cancelando…" } else { "Cancelar" }, !cancelling).clicked() {
                        self.cancel_login();
                    }
                });
            }
            Auth::Connecting { .. } => Self::loading(ui, "Conectando con Spotify"),
            Auth::LoggedOut => {
                if Self::primary_button(ui, "Iniciar sesión con Spotify", true).clicked() {
                    self.login();
                }
            }
        });

        self.refresh_mem();
        let weak = p.weak;
        let mem = self.mem_mb;
        let (n_img, img_mb, frame_ms) = (self.images.resident(), self.images.resident_bytes() as f64 / (1024.0 * 1024.0), self.frame_ms);
        let media_active = self.media.active();
        let settings_file = self.paths.settings_file().display().to_string();
        let cache_dir = self.paths.cache_dir.display().to_string();
        Self::settings_card(ui, "Acerca de Nanofy", |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Versión {}", crate::update::current_version())).strong());
                let _ = Self::pill(ui, "Rust · egui · librespot", false);
            });
            ui.add_space(6.0);
            let applying = self.update_applying || self.restart_after_exit.is_some();
            if Self::secondary_button(ui, "Buscar actualizaciones", !self.update_busy && !applying).clicked() {
                self.check_updates(true);
            }
            // El estado va en su propia fila, con el motivo debajo: con la interfaz ampliada no
            // cabe al lado del botón.
            let current = crate::update::current_version();
            let row = about_view(&self.update_stage, self.update.as_ref(), self.update_note.as_ref(), self.update_busy, applying, crate::update::can_self_install(), &current);
            if let Some(row) = row {
                ui.add_space(6.0);
                if row.waiting {
                    Self::loading(ui, &row.text);
                } else {
                    let color = match row.tone {
                        Tone::Text => p.text,
                        Tone::Weak => weak,
                        Tone::Error => ERROR_RED,
                    };
                    ui.add(Label::new(RichText::new(&row.text).color(color)).wrap());
                }
                if let Some(detail) = &row.detail {
                    ui.add(Label::new(RichText::new(detail).small().color(weak)).wrap());
                }
                if row.primary.is_some() || row.secondary.is_some() || row.link.is_some() {
                    ui.add_space(4.0);
                    let mut action = None;
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if let Some((label, a)) = row.primary {
                            if Self::primary_button(ui, label, true).clicked() {
                                action = Some(a);
                            }
                        }
                        if let Some((label, a)) = row.secondary {
                            if Self::secondary_button(ui, label, true).clicked() {
                                action = Some(a);
                            }
                        }
                        if let Some((label, a)) = row.link {
                            if Self::small_link(ui, label).clicked() {
                                action = Some(a);
                            }
                        }
                    });
                    if let Some(a) = action {
                        self.run_update_action(&ctx, a);
                    }
                }
            }
            ui.add_space(6.0);
            let mut v = self.settings.update_check;
            if Self::toggle_pad(ui, &mut v, "Buscar versiones nuevas automáticamente", 0.0) {
                self.set_update_check(v);
            }
            // Donde la app no se sustituye sola (macOS) no hay nada que pueda hacer sola.
            if crate::update::can_self_install() {
                // Sin consulta automática no hay nada que preparar solo: la opción queda apagada a la vista.
                let check_on = self.settings.update_check;
                let mut auto = self.settings.update_auto && check_on;
                let changed = ui.add_enabled_ui(check_on, |ui| Self::toggle_pad(ui, &mut auto, "Actualizar automáticamente", 0.0)).inner;
                if changed && check_on {
                    self.set_update_auto(auto);
                }
                ui.label(RichText::new("Descarga las versiones nuevas en segundo plano; solo tendrás que pulsar Reiniciar.").small().color(weak));
                // Activada pero sin efecto en esta copia (`auto_update_allowed`): que no parezca
                // que falla.
                if check_on && self.settings.update_auto && !self.auto_update_allowed() {
                    ui.label(RichText::new("En esta copia (de desarrollo o de pruebas) no se aplica: las versiones nuevas se avisan con «Instalar».").small().color(weak));
                }
            }
            if let Some(m) = mem {
                ui.label(RichText::new(format!("Memoria en uso {m:.0} MB · {n_img} portadas en memoria ({img_mb:.1} MB) · último fotograma {frame_ms:.1} ms")).small().color(weak));
            }
            ui.label(RichText::new(format!("Teclas multimedia: {}", if media_active { "activas" } else { "no disponibles" })).small().color(weak));
            ui.label(RichText::new(format!("Ajustes: {settings_file}")).small().color(weak));
            ui.label(RichText::new(format!("Caché: {cache_dir}")).small().color(weak));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if Self::secondary_button(ui, "Borrar caché de imágenes", true).clicked() {
                    let dir = self.paths.image_cache_dir();
                    let _ = std::fs::remove_dir_all(&dir);
                    let _ = std::fs::create_dir_all(&dir);
                    self.images.clear();
                    self.status("Caché de imágenes borrada");
                }
                if Self::secondary_button(ui, "Atajos de teclado", true).clicked() {
                    self.show_shortcuts = true;
                }
            });
            ui.add_space(6.0);
        ui.label(
                RichText::new(
                    "Nanofy es un proyecto independiente y no está afiliado a Spotify. \
                     Usa librespot para la reproducción y egui para la interfaz, dibujada por CPU.",
                )
                .small()
                .color(weak),
            );
        });
    }
}
