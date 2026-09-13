//! Página de artista: cabecera "hero", pestañas (Inicio, Álbumes, Sencillos y EPs,
//! Recopilatorios, Aparece en, Acerca de), búsqueda dentro del artista, discografía en
//! cuadrícula o en lista desplegable.

use egui::{pos2, vec2, Align, Color32, CornerRadius, Label, Layout, Rect, RichText, Sense};

use super::icons::{self, Icon};
use super::theme::{self, GREEN};
use super::widgets::{child_in, uri_to_link, vertical_gradient, CardInfo, CardKind, RowOpts, Source};
use super::{fmt_thousands, Action, App, Page, PlayTarget};
use crate::api::Req;
use crate::model::*;

pub const ARTIST_TABS: [&str; 7] = ["Inicio", "Álbumes", "Sencillos y EPs", "Recopilatorios", "Aparece en", "Playlists", "Acerca de"];

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
            self.api.send(Req::Artist(id.clone()));
            self.api.send(Req::ArtistTop(id.clone()));
            self.api.send(Req::ArtistView(id.clone()));
        }
        let page = self.artists.remove(&id).unwrap_or_default();
        let view = self.artist_views.get(&id).cloned();

        // ---------------- cabecera
        let width = ui.available_width();
        let hero_h = (width * 0.30).clamp(220.0, 340.0);
        let (hero, _) = ui.allocate_exact_size(vec2(width, hero_h), Sense::hover());
        let header_img = view
            .as_ref()
            .and_then(|v| v.header.clone())
            .or_else(|| page.artist.as_ref().and_then(|a| a.cover(640).map(|s| s.to_string())));
        self.cover_in(ui, header_img.as_deref(), hero, 14);
        let bottom = Rect::from_min_max(pos2(hero.min.x, hero.max.y - hero_h * 0.6), hero.max);
        vertical_gradient(ui.painter(), bottom, Color32::TRANSPARENT, p.card);
        let name = page.artist.as_ref().map(|a| a.name.clone()).unwrap_or_else(|| "Artista".into());
        ui.painter().text(
            pos2(hero.min.x + 28.0, hero.max.y - 62.0),
            egui::Align2::LEFT_BOTTOM,
            &name,
            theme::bold(44.0),
            p.text,
        );
        let followers = page.artist.as_ref().and_then(|a| a.followers.as_ref().and_then(|f| f.total));
        let sub = match followers {
            Some(n) => format!("{} seguidores", fmt_thousands(n)),
            None => String::new(),
        };
        ui.painter().text(pos2(hero.min.x + 28.0, hero.max.y - 34.0), egui::Align2::LEFT_BOTTOM, &sub, theme::regular(13.0), p.weak);

        // Acciones sobre la imagen (derecha)
        let top_uris: Vec<String> = page.top.iter().map(|t| t.uri.clone()).collect();
        let actions_rect = Rect::from_min_max(pos2(hero.max.x - 420.0, hero.max.y - 76.0), pos2(hero.max.x - 20.0, hero.max.y - 24.0));
        let mut a = child_in(ui, actions_rect, Layout::right_to_left(Align::Center));
        a.spacing_mut().item_spacing.x = 8.0;
        {
            let more = icons::button(&mut a, Icon::More, 34.0, p.text).on_hover_text("Más");
            let artist_uri = format!("spotify:artist:{id}");
            egui::Popup::menu(&more).show(|ui| {
                if Self::menu_item(ui, Some(Icon::Share), "Copiar enlace", false).clicked() {
                    self.actions.push(Action::CopyText(uri_to_link(&artist_uri), "Enlace"));
                    ui.close();
                }
                if Self::menu_item(ui, Some(Icon::NewTab), "Abrir en una nueva pestaña", false).clicked() {
                    self.actions.push(Action::OpenInTab(Page::Artist(id.clone())));
                    ui.close();
                }
            });
            if !top_uris.is_empty() {
                if icons::button(&mut a, Icon::Queue, 34.0, p.text).on_hover_text("Añadir populares a la cola").clicked() {
                    for u in &top_uris {
                        self.actions.push(Action::AddToQueue(u.clone()));
                    }
                }
                if icons::button(&mut a, Icon::PlusSquare, 34.0, p.text).on_hover_text("Añadir populares a una playlist").clicked() {
                    self.open_add_dialog(top_uris.clone());
                }
            }
            self.follow_button(&mut a, "artist", &id);
            if !top_uris.is_empty() {
                let uris = top_uris.clone();
                if icons::round_button(&mut a, Icon::Play, 44.0, GREEN, Color32::BLACK).on_hover_text("Reproducir populares").clicked() {
                    self.actions.push(Action::Play(PlayTarget::Tracks { uris, index: Some(0), shuffle: false }));
                }
            }
        }

        // ---------------- pestañas + búsqueda dentro del artista + vista
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            for (i, label) in ARTIST_TABS.iter().enumerate() {
                if Self::tab(ui, label, self.artist_tab == i as u8).clicked() {
                    self.artist_tab = i as u8;
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                // Cuadrícula / lista solo en las pestañas de discografía (1..=4); en Inicio,
                // Playlists y Acerca de no hay nada que ordenar.
                if (1..=4).contains(&self.artist_tab) {
                    let grid = self.artist_grid;
                    if icons::button(ui, Icon::Grid, 30.0, if grid { p.text } else { p.weak }).on_hover_text("Cuadrícula").clicked() {
                        self.artist_grid = true;
                    }
                    if icons::button(ui, Icon::List, 30.0, if !grid { p.text } else { p.weak }).on_hover_text("Lista").clicked() {
                        self.artist_grid = false;
                    }
                    ui.add_space(6.0);
                }
                if self.artist_search_open {
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.artist_search)
                            .hint_text(format!("Buscar en {name}"))
                            .desired_width(200.0),
                    );
                    if self.artist_search_focus {
                        self.artist_search_focus = false;
                        r.request_focus();
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) && r.has_focus() {
                        self.artist_search_open = false;
                        self.artist_search.clear();
                    }
                }
                let color = if self.artist_search_open { GREEN } else { p.weak };
                if icons::button(ui, Icon::Search, 30.0, color).on_hover_text("Buscar dentro del artista").clicked() {
                    self.artist_search_open = !self.artist_search_open;
                    self.artist_search_focus = self.artist_search_open;
                    if !self.artist_search_open {
                        self.artist_search.clear();
                    }
                }
            });
        });
        ui.painter().line_segment(
            [pos2(hero.min.x, ui.cursor().min.y), pos2(hero.max.x, ui.cursor().min.y)],
            egui::Stroke::new(1.0, p.border),
        );
        ui.add_space(12.0);

        let filter = self.artist_search.trim().to_lowercase();
        let matches = |s: &str| filter.is_empty() || s.to_lowercase().contains(&filter);

        match self.artist_tab {
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
        self.artists.insert(id, page);
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

    fn album_grid(&mut self, ui: &mut egui::Ui, list: &[AlbumRef]) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 8.0);
            for al in list {
                if let Some(aid) = &al.id {
                    self.album_card(ui, aid, al.cover(300), &al.name, al.year(), al.total_tracks);
                }
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
                    if icons::button(ui, Icon::Queue, 34.0, p.weak).on_hover_text("Añadir a la cola").clicked() {
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
