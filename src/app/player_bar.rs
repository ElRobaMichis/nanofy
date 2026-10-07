//! Barra superior (pestañas y búsqueda), biblioteca lateral y reproductor flotante.

use std::time::{Duration, Instant};

use egui::{pos2, vec2, Align, Color32, CornerRadius, Label, Layout, Rect, RichText, Sense, UiBuilder};
use librespot_playback::player::LoadFailure;

use super::icons::{self, Icon};
use super::theme::{self, GREEN};
use super::widgets::{child_in, galley_truncated, MenuKind, RowOpts};
use super::{Action, App, Auth, Page, PlayState, PlayTarget, Repeat, SideTab, SEARCH_ID, SIDEBAR_W};
use crate::api::Req;
use crate::config::{vol_pct_to_raw, vol_raw_db, vol_raw_to_pct};
use crate::model::*;

impl App {
    // ------------------------------------------------------------ barra superior

    pub fn top_bar(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        let full = ui.available_rect_before_wrap();
        let page = self.page().clone();

        // Izquierda: "Tu biblioteca"
        let left = Rect::from_min_max(full.min, pos2(full.min.x + SIDEBAR_W, full.max.y));
        {
            let mut l = child_in(ui, left.shrink2(vec2(14.0, 0.0)), Layout::left_to_right(Align::Center));
            let r = l.allocate_response(vec2(l.available_width(), 34.0), Sense::click());
            let color = if page == Page::Library { p.text } else { p.weak.lerp_to_gamma(p.text, 0.5) };
            icons::paint(l.painter(), Rect::from_center_size(pos2(r.rect.min.x + 12.0, r.rect.center().y), vec2(18.0, 18.0)), color, Icon::Library);
            l.painter().text(
                pos2(r.rect.min.x + 32.0, r.rect.center().y),
                egui::Align2::LEFT_CENTER,
                "Tu biblioteca",
                theme::regular(14.0),
                color,
            );
            if r.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if r.clicked() {
                self.go(Page::Library);
            }
        }

        // Derecha: ajustes y perfil y, con una versión nueva lista, «Reiniciar para actualizar».
        // Sigue a la vista aunque se cierre el aviso: es lo único que queda por hacer.
        let ready = match &self.update_stage {
            crate::update::Stage::Ready { version, .. } => Some(version.clone()),
            _ => None,
        };
        let pill_text_w = ready.as_ref().map(|_| ui.painter().layout_no_wrap(UPDATE_PILL_TEXT.to_string(), theme::bold(13.0), Color32::BLACK).size().x);
        let (right_w, pill) = top_right_layout(full.width(), pill_text_w);
        let right = Rect::from_min_max(pos2(full.max.x - right_w, full.min.y), full.max);
        {
            let mut r = child_in(ui, right.shrink2(vec2(10.0, 0.0)), Layout::right_to_left(Align::Center));
            r.spacing_mut().item_spacing.x = 6.0;
            // Avatar
            let (arect, aresp) = r.allocate_exact_size(vec2(32.0, 32.0), Sense::click());
            // Foto del perfil interno si ya llegó; si no, la que da /me (viene en la instantánea).
            let login_name = match &self.auth {
                Auth::LoggedIn { username } | Auth::Connecting { username } => Some(username.clone()),
                _ => None,
            };
            let img = self
                .my_id()
                .map(|s| s.to_string())
                .or(login_name)
                .and_then(|id| self.users.get(&id))
                .and_then(|u| u.cover(64).map(|s| s.to_string()))
                .or_else(|| self.user.as_ref().and_then(|u| u.cover(64).map(|s| s.to_string())));
            match img {
                Some(url) => self.cover_in(&mut r, Some(&url), arect, 16),
                None => {
                    r.painter().circle_filled(arect.center(), 16.0, p.card2);
                    let initial = self
                        .user
                        .as_ref()
                        .and_then(|u| u.display_name.clone())
                        .and_then(|n| n.chars().next())
                        .map(|c| c.to_uppercase().to_string())
                        .unwrap_or_else(|| "·".to_string());
                    r.painter().text(arect.center(), egui::Align2::CENTER_CENTER, initial, theme::bold(14.0), p.text);
                }
            }
            if aresp.hovered() {
                r.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            let logged = self.logged_in();
            if aresp.clicked() {
                if let Some(id) = self.my_id().map(|s| s.to_string()) {
                    self.go(Page::User(id));
                } else if !logged {
                    self.login();
                }
            }
            aresp.on_hover_text(if logged { "Tu perfil" } else { "Iniciar sesión" });
            if icons::button(&mut r, Icon::Settings, 32.0, p.weak)
                .on_hover_text("Ajustes (Ctrl+,)")
                .clicked()
            {
                self.draft = self.settings.clone();
                self.go(Page::Settings);
            }
            if let (Some(version), Some(pill)) = (ready, pill) {
                if self.update_pill(&mut r, &version, pill).clicked() {
                    // En una Jam, el aviso pide confirmarlo antes (se sale de ella).
                    self.restart_to_update(false);
                }
            }
        }

        // Pestañas pegadas a la izquierda de la zona de contenido: Inicio, Buscar (que se
        // expande como campo solo al hacer clic) y una pestaña por página abierta.
        let center = Rect::from_min_max(pos2(left.max.x, full.min.y), pos2(right.min.x, full.max.y));
        let searching = page == Page::Search;
        let mut c = child_in(ui, center, Layout::left_to_right(Align::Center));
        // Con muchas pestañas abiertas no se pintan encima de lo de la derecha.
        c.set_clip_rect(center.intersect(c.clip_rect()));
        c.spacing_mut().item_spacing.x = 6.0;
        c.add_space(4.0);
        if Self::top_tab(&mut c, Icon::Home, "Inicio", self.active == super::ActiveTab::Home, 0.0, false).clicked() {
            self.go(Page::Home);
        }
        let expanded = searching;
        if expanded {
            let search_w = 320.0f32.min(center.width() * 0.4);
            let (rect, resp) = c.allocate_exact_size(vec2(search_w, 36.0), Sense::click());
            c.painter().rect_filled(rect, CornerRadius::same(18), if searching { p.card2 } else { p.hover.gamma_multiply(0.7) });
            icons::paint(c.painter(), Rect::from_center_size(pos2(rect.min.x + 20.0, rect.center().y), vec2(18.0, 18.0)), p.weak, Icon::Search);
            let field = Rect::from_min_max(pos2(rect.min.x + 36.0, rect.min.y + 4.0), pos2(rect.max.x - 10.0, rect.max.y - 4.0));
            let mut f = child_in(&mut c, field, Layout::left_to_right(Align::Center));
            let edit = egui::TextEdit::singleline(&mut self.search_query)
                .id(egui::Id::new(SEARCH_ID))
                .frame(egui::Frame::NONE)
                .hint_text("Buscar…")
                .desired_width(field.width());
            let r = f.add(edit);
            let _ = resp;
            if self.focus_search {
                self.focus_search = false;
                r.request_focus();
                // La consulta anterior queda seleccionada: lo que ella escriba la sustituye (como
                // Ctrl+F en un navegador), y con las flechas o un clic sigue pudiendo editarla.
                let id = egui::Id::new(SEARCH_ID);
                if let Some(mut state) = egui::TextEdit::load_state(f.ctx(), id) {
                    let all = egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(self.search_query.chars().count()));
                    state.cursor.set_char_range(Some(all));
                    state.store(f.ctx(), id);
                }
            }
            if r.lost_focus() && f.input(|i| i.key_pressed(egui::Key::Enter)) {
                self.run_search();
            }
        } else {
            let r = Self::top_tab(&mut c, Icon::Search, "Buscar", false, 0.0, false);
            if r.clicked() {
                self.focus_search = true;
                self.go(Page::Search);
            }
        }
        // Pestañas de contenido
        let tabs: Vec<(usize, Icon, String)> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let (icon, title) = self.page_label(t.page());
                (i, icon, title)
            })
            .collect();
        for (i, icon, title) in tabs {
            let selected = self.active == super::ActiveTab::Tab(i);
            let r = Self::top_tab(&mut c, icon, &title, selected, 0.0, true);
            // Cerrar: × al pasar el ratón o botón central
            let close = Rect::from_center_size(pos2(r.rect.max.x - 20.0, r.rect.center().y), vec2(20.0, 20.0));
            let hovered = r.hovered() || c.rect_contains_pointer(close);
            if hovered {
                let cr = c.interact(close, c.id().with(("close_tab", i)), Sense::click());
                icons::paint(c.painter(), close.shrink(5.0), if cr.hovered() { p.text } else { p.weak }, Icon::Close);
                if cr.clicked() {
                    self.actions.push(Action::CloseTab(i));
                    continue;
                }
            }
            if r.middle_clicked() {
                self.actions.push(Action::CloseTab(i));
            } else if r.clicked() && !selected {
                self.actions.push(Action::ActivateTab(i));
            }
            r.context_menu(|ui| {
                if Self::menu_item(ui, Some(Icon::Close), "Cerrar pestaña", false).clicked() {
                    self.actions.push(Action::CloseTab(i));
                    ui.close();
                }
            });
        }
        // Atrás / adelante
        c.add_space(6.0);
        let can_back = self.can_back();
        let can_fwd = self.can_forward();
        if icons::button(&mut c, Icon::Back, 32.0, if can_back { p.text } else { p.faint })
            .on_hover_text("Atrás (Alt+←)")
            .clicked()
        {
            self.back();
        }
        if icons::button(&mut c, Icon::Forward, 32.0, if can_fwd { p.text } else { p.faint })
            .on_hover_text("Adelante (Alt+→)")
            .clicked()
        {
            self.forward();
        }
        ui.allocate_rect(full, Sense::hover());
    }

    /// Píldora verde «Reiniciar para actualizar» de la barra superior (o solo su icono si no cabe).
    /// Mientras se sustituye el ejecutable no se puede pulsar.
    fn update_pill(&self, ui: &mut egui::Ui, version: &str, pill: UpdatePill) -> egui::Response {
        let applying = self.update_applying || self.restart_after_exit.is_some();
        let w = match pill {
            UpdatePill::Full(w) => w,
            UpdatePill::Icon => PILL_H,
        };
        let (rect, resp) = ui.allocate_exact_size(vec2(w, PILL_H), if applying { Sense::hover() } else { Sense::click() });
        let fill = if resp.hovered() && !applying { GREEN.lerp_to_gamma(Color32::WHITE, 0.12) } else { GREEN };
        ui.painter().rect_filled(rect, CornerRadius::same((PILL_H / 2.0) as u8), fill);
        match pill {
            UpdatePill::Full(_) => {
                let icon = Rect::from_center_size(pos2(rect.min.x + PILL_PAD + 9.0, rect.center().y), vec2(18.0, 18.0));
                icons::paint(ui.painter(), icon, Color32::BLACK, Icon::Download);
                ui.painter().text(pos2(icon.max.x + 6.0, rect.center().y), egui::Align2::LEFT_CENTER, UPDATE_PILL_TEXT, theme::bold(13.0), Color32::BLACK);
            }
            UpdatePill::Icon => icons::paint(ui.painter(), Rect::from_center_size(rect.center(), vec2(18.0, 18.0)), Color32::BLACK, Icon::Download),
        }
        if resp.hovered() && !applying {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let tip = if applying {
            "Instalando la versión nueva…".to_string()
        } else {
            let tip = format!("Nanofy {version} está lista. La música seguirá donde estaba.");
            // Solo el icono: el texto de la píldora pasa al globo.
            if pill == UpdatePill::Icon { format!("{UPDATE_PILL_TEXT}. {tip}") } else { tip }
        };
        resp.on_hover_text(tip)
    }

    fn top_tab(ui: &mut egui::Ui, icon: Icon, text: &str, selected: bool, width: f32, closable: bool) -> egui::Response {
        let p = theme::palette(ui.ctx());
        let measured = ui.painter().layout_no_wrap(text.to_string(), theme::regular(14.0), p.text).size().x;
        // Las pestañas cerrables reservan sitio para la × a la derecha del texto.
        let right_pad = if closable { 36.0 } else { 14.0 };
        let width = if width > 0.0 { width } else { (measured + 46.0 + right_pad + 2.0).min(240.0) };
        let (rect, resp) = ui.allocate_exact_size(vec2(width, 36.0), Sense::click());
        if selected {
            ui.painter().rect_filled(rect, CornerRadius::same(18), p.card2);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(18), p.hover.gamma_multiply(0.6));
        }
        let color = if selected { p.text } else { p.weak.lerp_to_gamma(p.text, 0.5) };
        icons::paint(ui.painter(), Rect::from_center_size(pos2(rect.min.x + 26.0, rect.center().y), vec2(18.0, 18.0)), color, icon);
        let text_rect = Rect::from_min_max(pos2(rect.min.x + 46.0, rect.min.y), pos2(rect.max.x - right_pad, rect.max.y));
        let mut t = child_in(ui, text_rect, Layout::left_to_right(Align::Center));
        t.add(Label::new(RichText::new(text).font(theme::regular(14.0)).color(color)).truncate());
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        resp
    }

    /// Icono y título de una página, para las pestañas de la barra superior.
    pub fn page_label(&self, page: &Page) -> (Icon, String) {
        match page {
            Page::Home => (Icon::Home, "Inicio".into()),
            Page::Search => (Icon::Search, "Buscar".into()),
            Page::Library => (Icon::Library, "Tu biblioteca".into()),
            Page::History => (Icon::History, "Historial".into()),
            Page::Liked => (Icon::Heart, "Canciones que te gustan".into()),
            Page::Albums => (Icon::Album, "Álbumes".into()),
            Page::Saves => (Icon::Bookmark, "Guardados".into()),
            Page::Shows => (Icon::Lyrics, "Podcasts".into()),
            Page::Audiobooks => (Icon::Book, "Audiolibros".into()),
            Page::Folders => (Icon::Folder, "Carpetas".into()),
            Page::Artists => (Icon::Artist, "Artistas".into()),
            Page::Settings => (Icon::Settings, "Ajustes".into()),
            Page::Show(id) => (Icon::Lyrics, self.shows.get(id).map(|s| s.0.name.clone()).unwrap_or_else(|| "Podcast".into())),
            Page::Playlist(id) => (
                Icon::Playlist,
                self.playlists
                    .iter()
                    .find(|pl| &pl.id == id)
                    .map(|pl| pl.name.clone())
                    .or_else(|| self.playlist_meta.get(id).map(|pl| pl.name.clone()))
                    .unwrap_or_else(|| "Playlist".into()),
            ),
            Page::Album(id) => (
                Icon::Album,
                self.albums.get(id).map(|a| a.name.clone()).unwrap_or_else(|| "Álbum".into()),
            ),
            Page::Artist(id) => (
                Icon::Artist,
                self.artists
                    .get(id)
                    .and_then(|pg| pg.artist.as_ref().map(|a| a.name.clone()))
                    .unwrap_or_else(|| "Artista".into()),
            ),
            Page::User(id) => (
                Icon::Artist,
                self.users.get(id).map(|u| u.name().to_string()).unwrap_or_else(|| "Perfil".into()),
            ),
        }
    }

    // ---------------------------------------------------------- barra lateral

    pub fn sidebar(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        let page = self.page().clone();
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.add_space(4.0);

        if Self::nav_item(ui, Some(Icon::Artist), "Artistas", page == Page::Artists, 0.0).clicked() {
            self.go(Page::Artists);
        }

        // Fijados. Índices y no copias: este panel se dibuja en todas las páginas, cada fotograma.
        let pinned: Vec<usize> = {
            let set: std::collections::HashSet<&str> = self.settings.pinned.iter().map(String::as_str).collect();
            (0..self.playlists.len()).filter(|&i| set.contains(self.playlists[i].id.as_str())).collect()
        };
        let r = Self::nav_item(ui, Some(Icon::Pin), "Fijados", false, 0.0);
        Self::chevron(ui, r.rect, self.sidebar_pins_open, p.weak);
        if r.clicked() {
            self.sidebar_pins_open = !self.sidebar_pins_open;
        }
        if self.sidebar_pins_open {
            if pinned.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(44.0);
                    ui.label(RichText::new("Fija playlists con el clic derecho").small().color(p.faint));
                });
            }
            for i in pinned {
                // Copia solo de las fijadas (pocas): el menú contextual necesita `&mut self`.
                let pl = self.playlists[i].clone();
                let sel = matches!(&page, Page::Playlist(id) if *id == pl.id);
                let r = Self::nav_item(ui, Some(Icon::Playlist), &pl.name, sel, 24.0);
                if r.clicked() {
                    self.go(Page::Playlist(pl.id.clone()));
                }
                self.playlist_context_menu(&r, &pl);
            }
        }

        // Playlists
        let r = Self::nav_item(ui, Some(Icon::Playlist), "Playlists", false, 0.0);
        Self::chevron(ui, r.rect, self.sidebar_playlists_open, p.weak);
        {
            // botón "+" a la izquierda del chevron
            let plus = Rect::from_center_size(pos2(r.rect.max.x - 46.0, r.rect.center().y), vec2(24.0, 24.0));
            let pr = ui.interact(plus, ui.id().with("new_pl"), Sense::click());
            if pr.hovered() || r.hovered() {
                icons::paint(ui.painter(), plus.shrink(5.0), if pr.hovered() { p.text } else { p.weak }, Icon::Plus);
            }
            if pr.clicked() && self.logged_in() {
                self.actions.push(Action::OpenEditor(None));
            } else if r.clicked() {
                self.sidebar_playlists_open = !self.sidebar_playlists_open;
            }
            pr.on_hover_text("Nueva playlist (Ctrl+N)");
        }
        if self.sidebar_playlists_open {
            // Sin copia (primer arranque): el hueco de las filas mientras llega el rootlist. Con
            // copia no hace falta aviso: se ve la lista y se sustituye al llegar.
            if self.signed_in() && !self.playlists_loaded && self.playlists.is_empty() {
                Self::skeleton_rows(ui, 6, 34.0, 18.0, 28.0);
            }
            let avail = (ui.available_height() - 9.0 * 36.0 - 60.0).max(80.0);
            // Solo las filas a la vista: con cientos de playlists, copiarlas y maquetarlas todas
            // costaba casi un milisegundo por fotograma en cualquier página. nav_item mide 34 px
            // y show_rows ya suma el espaciado de 2 px entre filas.
            egui::ScrollArea::vertical()
                .id_salt("sidebar_playlists")
                .auto_shrink([false, true])
                .max_height(avail)
                .show_rows(ui, 34.0, self.playlists.len(), |ui, range| {
                    for i in range {
                        let Some(pl) = self.playlists.get(i).cloned() else { break };
                        let sel = matches!(&page, Page::Playlist(id) if *id == pl.id);
                        // Id de la fila por playlist: nav_item gasta dos ids automáticos y
                        // show_rows solo salta uno por fila oculta, así que con ids automáticos
                        // el hover y el menú abierto saltarían de fila al desplazarse.
                        let key = ui.id().with(("sb_pl", i, &pl.id));
                        let r = ui
                            .scope_builder(UiBuilder::new().id(key), |ui| {
                                Self::nav_item(ui, Some(Icon::Playlist), &pl.name, sel, 24.0)
                            })
                            .inner;
                        if r.clicked() {
                            self.go(Page::Playlist(pl.id.clone()));
                        }
                        self.playlist_context_menu(&r, &pl);
                    }
                });
        }

        ui.add_space(6.0);
        let items = [
            (Icon::Heart, "Canciones que te gustan", Page::Liked),
            (Icon::Bookmark, "Guardados", Page::Saves),
            (Icon::Album, "Álbumes", Page::Albums),
            (Icon::Folder, "Carpetas", Page::Folders),
            (Icon::Lyrics, "Podcasts", Page::Shows),
            (Icon::Book, "Audiolibros", Page::Audiobooks),
            (Icon::History, "Historial", Page::History),
        ];
        for (icon, label, pg) in items {
            if Self::nav_item(ui, Some(icon), label, page == pg, 0.0).clicked() {
                self.go(pg);
            }
        }

        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            self.refresh_mem();
            let mem = self
                .mem_mb
                .map(|m| format!("{m:.0} MB · {:.1} ms", self.frame_ms))
                .unwrap_or_default();
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.label(RichText::new(mem).small().color(p.faint))
                    .on_hover_text("Memoria usada por Nanofy y coste del último fotograma");
            });
            if let Some((text, _, err)) = self.status.clone() {
                let color = if err { theme::RED } else { p.weak };
                ui.add_space(6.0);
                // «Conecta tu biblioteca…» llega desde cualquier página que lea la Web API: la
                // acción va con el aviso, sin buscarla en el inicio ni en Ajustes. (De abajo arriba:
                // queda debajo del texto.)
                if text == crate::api::NO_APP_HINT && !self.api.web_configured() && !self.web_busy {
                    ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        if ui.link(RichText::new("Conectar con Spotify").small()).clicked() {
                            self.connect_library();
                        }
                    });
                }
                let w = ui.available_width() - 20.0;
                let galley = ui.painter().layout(text, theme::regular(12.0), color, w);
                let (rect, _) = ui.allocate_exact_size(vec2(w, galley.size().y), Sense::hover());
                ui.painter().galley(pos2(rect.min.x + 14.0, rect.min.y), galley, color);
            }
        });
    }

    fn chevron(ui: &mut egui::Ui, rect: Rect, open: bool, color: Color32) {
        let c = pos2(rect.max.x - 18.0, rect.center().y);
        let s = 5.0;
        let pts = if open {
            vec![pos2(c.x - s, c.y - s / 2.0), pos2(c.x, c.y + s / 2.0), pos2(c.x + s, c.y - s / 2.0)]
        } else {
            vec![pos2(c.x - s / 2.0, c.y - s), pos2(c.x + s / 2.0, c.y), pos2(c.x - s / 2.0, c.y + s)]
        };
        ui.painter().add(egui::Shape::line(pts, egui::Stroke::new(1.6, color)));
    }

    fn playlist_context_menu(&mut self, r: &egui::Response, pl: &Playlist) {
        let mine = self.is_mine(pl);
        let pinned = self.settings.pinned.contains(&pl.id);
        r.context_menu(|ui| {
            if Self::menu_item(ui, Some(Icon::NewTab), "Abrir en una nueva pestaña", false).clicked() {
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
            let label = if mine { "Eliminar" } else { "Quitar de la biblioteca" };
            if Self::menu_item(ui, Some(Icon::Trash), label, false).clicked() {
                self.actions.push(Action::FollowPlaylist(pl.id.clone(), false));
                ui.close();
            }
        });
    }

    // -------------------------------------------------------------- reproductor

    pub fn player_bar(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        let full = ui.available_rect_before_wrap();
        // Fondo dinámico: color de la portada al reproducir, gris en pausa (con transición).
        let playing = self.player.state == PlayState::Playing;
        let target = if playing {
            self.player
                .now
                .as_ref()
                .and_then(|n| n.cover_url.as_deref())
                .and_then(|u| self.images.color(u, p.dark))
                .unwrap_or(p.card)
        } else {
            p.card2
        };
        let anim = |ui: &egui::Ui, k: u8, v: u8| ui.ctx().animate_value_with_time(egui::Id::new(("player_bg", k)), v as f32, 0.6);
        let bg = Color32::from_rgb(anim(ui, 0, target.r()) as u8, anim(ui, 1, target.g()) as u8, anim(ui, 2, target.b()) as u8);
        ui.painter().rect_filled(full, CornerRadius::same(14), bg);
        ui.painter().rect_stroke(full, CornerRadius::same(14), egui::Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
        let inner = full.shrink2(vec2(14.0, 8.0));
        let w = inner.width();
        let wide = w >= 1100.0;

        // Etiqueta de calidad: solo con la canción sonando aquí (no en otro dispositivo).
        let badge_w = match self.player.local_audio() {
            Some(a) => ui.painter().layout_no_wrap(a.badge(), theme::regular(BADGE_FONT), p.weak).size().x + 2.0 * BADGE_PAD + 4.0,
            None => 0.0,
        };
        let BarWidths { vol_zone, transport_w, times_w, vol_w, text_w, actions_w, wave_w, badge } = bar_widths(w, badge_w);
        let now_w = 46.0 + 10.0 + text_w;

        let h = inner.height();
        let seg = |x0: f32, width: f32| Rect::from_min_size(pos2(x0, inner.min.y), vec2(width, h));
        let mut x = inner.min.x;
        let transport = seg(x, transport_w);
        x += transport_w;
        let times_wave = seg(x, times_w + wave_w);
        x += times_w + wave_w;
        let vol = seg(x, vol_w);
        x += vol_w + 12.0;
        let now = seg(x, now_w);
        let actions = Rect::from_min_max(pos2(inner.max.x - actions_w, inner.min.y), inner.max);

        let mut t = child_in(ui, transport, Layout::left_to_right(Align::Center));
        self.transport_widget(&mut t);
        let mut tw = child_in(ui, times_wave, Layout::left_to_right(Align::Center));
        self.progress_widget(&mut tw, wave_w);
        let mut v = child_in(ui, vol, Layout::left_to_right(Align::Center));
        self.volume_widget(&mut v, vol_zone);
        let mut n = child_in(ui, now, Layout::left_to_right(Align::Center));
        self.now_playing_widget(&mut n, text_w, wide);
        let mut a = child_in(ui, actions, Layout::right_to_left(Align::Center));
        self.player_actions_widget(&mut a, actions_w, badge);
        ui.allocate_rect(full, Sense::hover());
    }

    fn transport_widget(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        ui.spacing_mut().item_spacing.x = 4.0;
        let (icon, fill) = match self.player.state {
            PlayState::Playing => (Icon::Pause, GREEN),
            PlayState::Loading => (Icon::Hourglass, p.card2),
            _ => (Icon::Play, GREEN),
        };
        let fg = if self.player.state == PlayState::Loading { p.weak } else { Color32::BLACK };
        if icons::round_button(ui, icon, 38.0, fill, fg).on_hover_text("Reproducir / pausar (Espacio)").clicked() {
            self.play_pause();
        }
        if icons::button(ui, Icon::Prev, 30.0, p.text).on_hover_text("Anterior (Ctrl+←)").clicked() {
            self.prev();
        }
        if icons::button(ui, Icon::Next, 30.0, p.text).on_hover_text("Siguiente (Ctrl+→)").clicked() {
            self.next();
        }
        let shuffle_color = if self.player.shuffle { GREEN } else { p.weak };
        if icons::button(ui, Icon::Shuffle, 30.0, shuffle_color).on_hover_text("Aleatorio (S)").clicked() {
            self.toggle_shuffle();
        }
        let (rep_icon, rep_color) = match self.player.repeat {
            Repeat::Off => (Icon::Repeat, p.weak),
            Repeat::Context => (Icon::Repeat, GREEN),
            Repeat::Track => (Icon::RepeatOne, GREEN),
        };
        if icons::button(ui, rep_icon, 30.0, rep_color).on_hover_text("Repetir (R)").clicked() {
            self.cycle_repeat();
        }
    }

    /// Tiempo transcurrido, forma de onda con la posición y duración total.
    fn progress_widget(&mut self, ui: &mut egui::Ui, wave_w: f32) {
        let p = theme::palette(ui.ctx());
        ui.spacing_mut().item_spacing.x = 0.0;
        let dur = self.player.now.as_ref().map(|n| n.duration_ms).unwrap_or(0);
        let pos = self.seek_drag.unwrap_or_else(|| self.player.position());
        let (lr, _) = ui.allocate_exact_size(vec2(42.0, 24.0), Sense::hover());
        ui.painter().text(pos2(lr.max.x - 6.0, lr.center().y), egui::Align2::RIGHT_CENTER, fmt_ms(pos), theme::regular(12.0), p.weak);
        let (wr, _) = ui.allocate_exact_size(vec2(wave_w, 30.0), Sense::hover());
        self.progress_line(ui, wr, dur, pos);
        let (rr, _) = ui.allocate_exact_size(vec2(42.0, 24.0), Sense::hover());
        ui.painter().text(pos2(rr.min.x + 6.0, rr.center().y), egui::Align2::LEFT_CENTER, fmt_ms(dur), theme::regular(12.0), p.weak);
    }

    /// Línea de progreso: gris con lo reproducido en blanco; al acercar el ratón aparece la
    /// bolita. Clic o arrastre para saltar.
    fn progress_line(&mut self, ui: &mut egui::Ui, rect: Rect, dur: u32, pos: u32) {
        let p = theme::palette(ui.ctx());
        let resp = ui.interact(rect, ui.id().with("progress"), Sense::click_and_drag());
        let near = resp.hovered() || resp.dragged() || resp.is_pointer_button_down_on();
        let frac = if dur > 0 { (pos as f32 / dur as f32).clamp(0.0, 1.0) } else { 0.0 };
        let y = rect.center().y;
        let x0 = rect.min.x + 6.0;
        let x1 = rect.max.x - 6.0;
        let xp = x0 + (x1 - x0) * frac;
        let painter = ui.painter();
        painter.line_segment([pos2(x0, y), pos2(x1, y)], egui::Stroke::new(3.0, p.weak.gamma_multiply(0.45)));
        painter.line_segment([pos2(x0, y), pos2(xp, y)], egui::Stroke::new(3.0, p.text));
        let knob = ui.ctx().animate_bool_with_time(ui.id().with("knob"), near, 0.12);
        if knob > 0.0 {
            painter.circle_filled(pos2(xp, y), 6.0 * knob, p.text);
        }
        if let Some(hp) = resp.hover_pos() {
            if dur > 0 && !resp.dragged() {
                let f = ((hp.x - x0) / (x1 - x0)).clamp(0.0, 1.0);
                resp.clone().on_hover_text(fmt_ms((f * dur as f32) as u32));
            }
        }
        if dur > 0 {
            if resp.dragged() || resp.is_pointer_button_down_on() {
                if let Some(hp) = resp.interact_pointer_pos() {
                    let f = ((hp.x - x0) / (x1 - x0)).clamp(0.0, 1.0);
                    self.seek_drag = Some((f * dur as f32) as u32);
                }
            } else if let Some(v) = self.seek_drag.take() {
                self.seek(v);
            }
        }
    }

    /// Icono de silencio y línea de volumen (mismo estilo que la de progreso).
    fn volume_widget(&mut self, ui: &mut egui::Ui, zone_w: f32) {
        let p = theme::palette(ui.ctx());
        ui.spacing_mut().item_spacing.x = 0.0;
        let icon = if self.player.volume == 0 { Icon::Mute } else { Icon::Volume };
        if icons::button(ui, icon, 28.0, p.weak).on_hover_text("Silenciar (M)").clicked() {
            self.toggle_mute();
        }
        if zone_w <= 0.0 {
            return;
        }
        let (zone, _) = ui.allocate_exact_size(vec2(zone_w, 28.0), Sense::hover());
        let resp = ui.interact(zone, ui.id().with("volume"), Sense::click_and_drag());
        let near = resp.hovered() || resp.dragged() || resp.is_pointer_button_down_on();
        let raw_now = self.volume_drag.unwrap_or(self.player.volume);
        let pct = vol_raw_to_pct(raw_now);
        let y = zone.center().y;
        let x0 = zone.min.x + 8.0;
        let x1 = zone.max.x - 8.0;
        let xp = x0 + (x1 - x0) * (pct / 100.0).clamp(0.0, 1.0);
        let painter = ui.painter();
        painter.line_segment([pos2(x0, y), pos2(x1, y)], egui::Stroke::new(3.0, p.weak.gamma_multiply(0.45)));
        painter.line_segment([pos2(x0, y), pos2(xp, y)], egui::Stroke::new(3.0, p.text));
        let knob = ui.ctx().animate_bool_with_time(ui.id().with("vol_knob"), near, 0.12);
        if knob > 0.0 {
            painter.circle_filled(pos2(xp, y), 6.0 * knob, p.text);
        }
        let db = vol_raw_db(raw_now);
        let tip = if db.is_finite() { format!("{pct:.0} %  ({db:+.1} dB)") } else { "Silencio".to_string() };
        if resp.hovered() {
            resp.clone().on_hover_text(tip);
        }
        if resp.dragged() || resp.is_pointer_button_down_on() {
            if let Some(hp) = resp.interact_pointer_pos() {
                let f = ((hp.x - x0) / (x1 - x0)).clamp(0.0, 1.0);
                let raw = vol_pct_to_raw(f * 100.0);
                if self.volume_drag != Some(raw) {
                    self.volume_drag = Some(raw);
                    self.preview_volume(raw);
                    if self.last_volume_sent.elapsed() > Duration::from_millis(250) {
                        self.set_volume(raw);
                    }
                }
            }
        } else if let Some(raw) = self.volume_drag.take() {
            self.set_volume(raw);
        }
        if resp.hovered() {
            // Scroll sin suavizar: el suavizado de egui reparte cada muesca en varios
            // fotogramas y generaba una orden por fotograma (a tirones y con cola).
            let scroll: f32 = ui.input(|i| {
                i.raw
                    .events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::MouseWheel { unit, delta, .. } => Some(match unit {
                            egui::MouseWheelUnit::Point => delta.y,
                            egui::MouseWheelUnit::Line => delta.y * 50.0,
                            egui::MouseWheelUnit::Page => delta.y * 400.0,
                        }),
                        _ => None,
                    })
                    .sum()
            });
            if scroll != 0.0 {
                self.volume_wheel(scroll);
            }
        }
    }

    fn now_playing_widget(&mut self, ui: &mut egui::Ui, text_w: f32, show_album: bool) {
        let p = theme::palette(ui.ctx());
        ui.spacing_mut().item_spacing.x = 10.0;
        let Some(np) = self.player.now.clone() else {
            ui.label(RichText::new("Nada en reproducción").color(p.faint));
            return;
        };
        let r = ui
            .scope(|ui| self.cover(ui, np.cover_url.as_deref(), 46.0, false))
            .response
            .interact(Sense::click());
        if r.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if r.clicked() {
            if let Some(id) = &np.album_id {
                self.actions.push(Action::Go(Page::Album(id.clone())));
            }
        }
        r.context_menu(|ui| {
            let t = super::track_from_now(&np);
            self.song_menu(ui, &t, MenuKind::NowPlaying, &RowOpts::tracks(false, false));
        });
        // Tres líneas en posiciones fijas dentro de la altura de la portada (46 px), sin depender
        // de la altura de la fuente (los caracteres japoneses usan una fuente de respaldo más alta).
        let (col, _) = ui.allocate_exact_size(vec2(text_w, 46.0), Sense::hover());
        let painter = ui.painter().clone();
        // Tarda en cargar (`watchdog::SLOW_AFTER`): la tercera línea lo dice, en lugar del álbum.
        let slow = self.player.state == PlayState::Loading
            && self.player.remote.is_none()
            && self.load_watch.as_ref().is_some_and(|w| w.is_slow());
        let lines: Vec<(String, egui::FontId, Color32, f32, f32)> = {
            let names = np.artists.iter().map(|a| a.0.as_str()).collect::<Vec<_>>().join(", ");
            let mut v = vec![
                (np.name.clone(), theme::regular(14.0), p.text, 0.0, 18.0),
                (names, theme::regular(12.0), p.weak, 17.0, 14.0),
            ];
            if slow {
                v.push((SLOW_TEXT.to_string(), theme::regular(12.0), p.warn, 32.0, 14.0));
            } else if show_album && !np.album.is_empty() {
                v.push((np.album.clone(), theme::regular(12.0), p.faint, 32.0, 14.0));
            }
            v
        };
        for (i, (text, font, color, y, h)) in lines.iter().enumerate() {
            let galley = galley_truncated(&painter, text, font.clone(), *color, text_w);
            let rect = Rect::from_min_size(pos2(col.min.x, col.min.y + y), vec2(galley.size().x.min(text_w), *h));
            painter.galley(pos2(rect.min.x, rect.center().y - galley.size().y / 2.0), galley, *color);
            // El título y el aviso de carga lenta no llevan a ninguna parte.
            if i == 0 || (slow && i == 2) {
                continue;
            }
            let resp = ui.interact(rect, ui.id().with(("np_line", i)), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if i == 1 {
                if np.artists.len() == 1 {
                    if resp.clicked() {
                        if let Some(id) = &np.artists[0].1 {
                            self.actions.push(Action::Go(Page::Artist(id.clone())));
                        }
                    }
                } else {
                    egui::Popup::menu(&resp).show(|ui| {
                        for (name, id) in &np.artists {
                            if let Some(id) = id {
                                if Self::menu_item(ui, Some(Icon::Artist), name, false).clicked() {
                                    self.actions.push(Action::Go(Page::Artist(id.clone())));
                                    ui.close();
                                }
                            }
                        }
                    });
                }
            } else if resp.clicked() {
                if let Some(id) = &np.album_id {
                    self.actions.push(Action::Go(Page::Album(id.clone())));
                }
            }
        }
    }

    /// De izquierda a derecha: calidad, corazón, playlist, letra, dispositivos, más | cola.
    fn player_actions_widget(&mut self, ui: &mut egui::Ui, width: f32, badge: bool) {
        let p = theme::palette(ui.ctx());
        ui.spacing_mut().item_spacing.x = 2.0;
        let compact = width < 200.0;

        let q_color = if self.side == Some(SideTab::Queue) { GREEN } else { p.weak };
        if icons::button(ui, Icon::Queue, 30.0, q_color).on_hover_text("Cola (Q)").clicked() {
            self.toggle_side(SideTab::Queue);
        }
        // Separador vertical
        let (sr, _) = ui.allocate_exact_size(vec2(20.0, 30.0), Sense::hover());
        ui.painter().line_segment([pos2(sr.center().x, sr.min.y + 4.0), pos2(sr.center().x, sr.max.y - 4.0)], egui::Stroke::new(1.0, p.border));

        // Más opciones
        let more = icons::button(ui, Icon::More, 30.0, p.weak).on_hover_text("Más");
        egui::Popup::menu(&more).show(|ui| self.player_more_menu(ui));

        // Dispositivos
        let dev_color = if self.player.remote.is_some() { theme::BLUE } else { p.weak };
        let r = icons::button(ui, Icon::Devices, 30.0, dev_color).on_hover_text(match &self.player.remote {
            Some(d) => format!("Sonando en {}", d.name),
            None => "Dispositivos".to_string(),
        });
        if r.clicked() {
            self.api.send(Req::Devices);
        }
        egui::Popup::menu(&r).width(280.0).show(|ui| self.devices_menu(ui));

        if !compact {
            let lyr_color = if self.side == Some(SideTab::Lyrics) { GREEN } else { p.weak };
            if icons::button(ui, Icon::Lyrics, 30.0, lyr_color).on_hover_text("Letra (L)").clicked() {
                self.toggle_side(SideTab::Lyrics);
            }
        }
        if let Some(np) = self.player.now.clone() {
            if !compact {
                if icons::button(ui, Icon::PlusSquare, 30.0, p.weak).on_hover_text("Añadir a playlist").clicked() {
                    self.open_add_dialog(vec![np.uri.clone()]);
                }
            }
            if let Some(id) = &np.id {
                let liked = self.player.liked.unwrap_or(false);
                let (icon, color) = if liked { (Icon::HeartFilled, GREEN) } else { (Icon::Heart, p.weak) };
                if icons::button(ui, icon, 30.0, color)
                    .on_hover_text(if liked { "Quitar de Me gusta" } else { "Guardar en Me gusta" })
                    .clicked()
                {
                    self.actions.push(Action::Like(id.clone(), !liked));
                }
            }
        }
        // La última en un diseño de derecha a izquierda: queda a la izquierda del todo.
        if badge && !compact {
            self.quality_badge(ui);
        }
    }

    /// Etiqueta «320 kbps» con lo que suena de verdad: formato, salida y normalización en el
    /// globo; en ámbar si la canción suena por debajo de la calidad pedida. Abre los ajustes.
    fn quality_badge(&mut self, ui: &mut egui::Ui) {
        let Some(info) = self.player.local_audio() else { return };
        let p = theme::palette(ui.ctx());
        let color = if info.below_requested() { p.warn } else { p.weak };
        let galley = ui.painter().layout_no_wrap(info.badge(), theme::regular(BADGE_FONT), color);
        let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 2.0 * BADGE_PAD, 20.0), Sense::click());
        let radius = CornerRadius::same(10);
        if resp.hovered() {
            ui.painter().rect_filled(rect, radius, p.hover);
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        ui.painter().rect_stroke(rect, radius, egui::Stroke::new(1.0, color.gamma_multiply(0.6)), egui::StrokeKind::Inside);
        let text_pos = pos2(rect.center().x - galley.size().x / 2.0, rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(text_pos, galley, color);
        // El globo solo se arma al pasar por encima (la barra se pinta en cada fotograma).
        let resp = if resp.hovered() {
            let tip = info
                .tooltip(crate::backend::audio_output(), self.settings.loudness.label(), self.settings.quality.kbps())
                .join("\n");
            resp.on_hover_text(tip)
        } else {
            resp
        };
        if resp.clicked() {
            // Si ya se está en Ajustes, no se tiran los cambios sin guardar.
            if !matches!(self.page(), Page::Settings) {
                self.draft = self.settings.clone();
            }
            self.go(Page::Settings);
        }
    }

    fn player_more_menu(&mut self, ui: &mut egui::Ui) {
        match self.player.now.clone() {
            Some(np) => {
                let t = super::track_from_now(&np);
                self.song_menu(ui, &t, MenuKind::PlayerMore, &RowOpts::tracks(false, false));
            }
            None => {
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
                if Self::menu_item(ui, Some(Icon::People), "Iniciar una Jam", false).clicked() {
                    self.jam_open = true;
                    ui.close();
                }
                if Self::menu_item(ui, Some(Icon::Keyboard), "Atajos de teclado", false).clicked() {
                    self.show_shortcuts = true;
                    ui.close();
                }
            }
        }
    }

    fn devices_menu(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Reproducir en").strong());
        ui.separator();
        if !self.logged_in() {
            ui.label("Inicia sesión para ver tus dispositivos.");
            return;
        }
        if self.devices.is_empty() {
            Self::loading(ui, "Buscando dispositivos");
        }
        let devices = self.devices.clone();
        let mut listed_self = false;
        for d in devices {
            let is_self = d.id.as_deref() == Some(self.device_id.as_str());
            listed_self |= is_self;
            let name = if is_self {
                format!("{} (este equipo)", d.name)
            } else {
                d.name.clone()
            };
            let color = if d.is_active { GREEN } else { ui.visuals().text_color() };
            if ui
                .add(egui::Button::new(RichText::new(name).color(color)).frame(false))
                .clicked()
            {
                self.select_device(d, is_self);
                ui.close();
            }
        }
        if !listed_self && !self.device_id.is_empty() {
            let me = Device {
                id: Some(self.device_id.clone()),
                name: self.settings.device_name.clone(),
                is_active: false,
                kind: "computer".into(),
                volume_percent: None,
            };
            let text = format!("{} (este equipo)", me.name);
            if ui.add(egui::Button::new(text).frame(false)).clicked() {
                self.select_device(me, true);
                ui.close();
            }
        }
        ui.separator();
        if ui
            .add(egui::Button::new(RichText::new("Actualizar lista").small()).frame(false))
            .clicked()
        {
            self.api.send(Req::Devices);
        }
    }

    /// Aviso de reproducción encima de la barra: por qué no suena una canción y qué se hace
    /// (reintentar sola, saltarla, detenerse), con [Reintentar] y [Saltar] cuando sirven.
    pub fn playback_error_banner(&mut self, ctx: &egui::Context) {
        let Some(err) = self.playback_error.clone() else { return };
        // Lo de otro dispositivo no se reproduce aquí: su aviso no aplica.
        if self.player.remote.is_some() {
            return;
        }
        let p = theme::palette(ctx);
        let color = match err.kind {
            // Lo que se arregla solo (o al conectar algo) en ámbar; lo que no, en rojo.
            PlaybackErrorKind::Retrying
            | PlaybackErrorKind::Stalled
            | PlaybackErrorKind::NoOutput
            | PlaybackErrorKind::NotPremium => p.warn,
            _ => theme::RED,
        };
        let width = (ctx.content_rect().width() - 2.0 * 24.0).clamp(240.0, 620.0);
        let mut action: Option<BannerAction> = None;
        egui::Area::new(egui::Id::new("playback_error_banner"))
            .order(egui::Order::Foreground)
            // Justo encima de la barra del reproductor (96 px) y centrado.
            .anchor(egui::Align2::CENTER_BOTTOM, vec2(0.0, -(96.0 + 8.0)))
            .show(ctx, |ui| {
                Self::dialog_frame(ctx)
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.7)))
                    .show(ui, |ui| {
                        ui.set_width(width);
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                            if matches!(err.kind, PlaybackErrorKind::Retrying | PlaybackErrorKind::Stalled) {
                                icons::paint(ui.painter(), r, color, Icon::Hourglass);
                            } else {
                                ui.painter().circle_filled(r.center(), 9.0, color);
                                ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "!", theme::bold(13.0), Color32::BLACK);
                            }
                            // El texto ocupa lo que deja la × (y salta de línea si no cabe).
                            let text_w = (ui.available_width() - 32.0).max(80.0);
                            ui.allocate_ui_with_layout(vec2(text_w, 20.0), Layout::top_down(Align::Min), |ui| {
                                ui.set_max_width(text_w);
                                ui.add_space(1.0);
                                ui.add(Label::new(RichText::new(&err.text).color(p.text)).wrap());
                            });
                            if icons::button(ui, Icon::Close, 24.0, p.weak).on_hover_text("Cerrar").clicked() {
                                action = Some(BannerAction::Dismiss);
                            }
                        });
                        if err.has_actions() {
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                ui.add_space(28.0);
                                ui.spacing_mut().item_spacing.x = 8.0;
                                if Self::primary_button(ui, "Reintentar", true).clicked() {
                                    action = Some(BannerAction::Retry);
                                }
                                if Self::secondary_button(ui, "Saltar", true).on_hover_text("Pasar a la siguiente canción").clicked() {
                                    action = Some(BannerAction::Skip);
                                }
                            });
                        }
                    });
            });
        match action {
            Some(BannerAction::Retry) => match err.kind {
                PlaybackErrorKind::Stalled => self.retry_stall_now(),
                PlaybackErrorKind::Stuck => self.retry_stuck_load(),
                _ => self.retry_failed_load(),
            },
            Some(BannerAction::Skip) => self.skip_failed_load(),
            Some(BannerAction::Dismiss) => self.playback_error = None,
            None => {}
        }
    }

    /// Nombre a mostrar del usuario.
    pub fn display_name(&self) -> String {
        self.user
            .as_ref()
            .and_then(|u| u.display_name.clone())
            .unwrap_or_else(|| match &self.auth {
                Auth::LoggedIn { username } | Auth::Connecting { username } => username.clone(),
                _ => String::new(),
            })
    }
}

// ------------------------------------------------------------ aviso de reproducción

/// Qué clase de aviso de reproducción hay encima de la barra.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackErrorKind {
    /// Un fallo pasajero: la canción queda en pausa y la app la reintenta sola.
    Retrying,
    /// Se agotaron los reintentos automáticos: queda en manos del usuario.
    Failed,
    /// No se puede reproducir y ya se pasó a la siguiente: solo informa y se va solo.
    Skipped,
    /// Varias seguidas no se pudieron reproducir y se detuvo la reproducción.
    Cascade,
    /// La red se cortó a media canción: en pausa en su segundo, sigue sola al volver.
    Stalled,
    /// Se dejó de esperar una carga a los 30 s (Spotify no respondía).
    Stuck,
    /// No hay ningún dispositivo de salida de audio; sigue sola al conectar uno.
    NoOutput,
    /// La cuenta no es Premium: aquí no se puede reproducir.
    NotPremium,
}

/// El aviso que se enseña (`App::playback_error`).
#[derive(Clone, Debug)]
pub struct PlaybackError {
    pub kind: PlaybackErrorKind,
    pub text: String,
    pub since: Instant,
}

/// Lo que dura a la vista un aviso que solo informa (la canción ya se saltó).
pub const SKIPPED_BANNER_FOR: Duration = Duration::from_secs(8);

/// Reintentos automáticos de una canción que no se pudo cargar por algo pasajero: el primero a
/// los 15 s y el segundo a los 60 s; si también fallan, el aviso se queda con [Reintentar] y
/// [Saltar]. Con Spotify frenando las claves, reintentar enseguida solo alargaría el freno.
pub const LOAD_RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(15), Duration::from_secs(60)];

impl PlaybackError {
    pub fn new(kind: PlaybackErrorKind, text: String) -> Self {
        Self { kind, text, since: Instant::now() }
    }

    /// Lleva [Reintentar] y [Saltar].
    pub fn has_actions(&self) -> bool {
        matches!(
            self.kind,
            PlaybackErrorKind::Retrying | PlaybackErrorKind::Failed | PlaybackErrorKind::Stalled | PlaybackErrorKind::Stuck
        )
    }

    /// Lo que le queda a la vista; `None` si no se va solo.
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        (self.kind == PlaybackErrorKind::Skipped).then(|| SKIPPED_BANNER_FOR.saturating_sub(now.saturating_duration_since(self.since)))
    }

    /// Ya se puede quitar (solo los que informan, pasado `SKIPPED_BANNER_FOR`).
    pub fn expired(&self, now: Instant) -> bool {
        self.remaining(now).is_some_and(|d| d.is_zero())
    }
}

/// Botón pulsado en el aviso.
enum BannerAction {
    Retry,
    Skip,
    Dismiss,
}

/// Espera hasta el reintento automático tras el fallo número `failures` (1 = el primero) de la
/// misma canción; `None` cuando ya no quedan reintentos.
pub fn load_retry_delay(failures: u8) -> Option<Duration> {
    LOAD_RETRY_DELAYS.get(usize::from(failures.checked_sub(1)?)).copied()
}

/// «Nombre» de la canción entre comillas, o «la canción» si no se sabe cuál es.
fn quoted(name: Option<&str>) -> String {
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => format!("«{n}»"),
        None => "la canción".to_string(),
    }
}

/// Primera letra en mayúscula (para «la canción» al principio de una frase).
fn capitalized(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Aviso mientras la app reintenta sola una carga que falló por algo pasajero.
pub fn retrying_text(reason: &LoadFailure, name: Option<&str>) -> String {
    let t = quoted(name);
    match reason {
        LoadFailure::Network(_) => format!("Se cortó la conexión al cargar {t}; reintentando…"),
        // Claves negadas o sin respuesta y el límite de peticiones: Spotify está frenando.
        _ => format!("Spotify está frenando las peticiones de reproducción; reintentando {t}…"),
    }
}

/// Aviso cuando ya no quedan reintentos automáticos.
pub fn failed_text(name: Option<&str>) -> String {
    format!("No se pudo reproducir {}.", quoted(name))
}

/// Aviso de un fallo definitivo: la canción se marca y se pasa a la siguiente.
pub fn skipped_text(reason: &LoadFailure, name: Option<&str>) -> String {
    let t = quoted(name);
    match reason {
        LoadFailure::NotAvailable(_) | LoadFailure::NoFormat => {
            capitalized(&format!("{t} no está disponible en tu país o se retiró. Pasamos a la siguiente."))
        }
        _ => format!("No se pudo reproducir {t}. Pasamos a la siguiente."),
    }
}

/// Aviso cuando se detuvo una cascada de canciones que no se pueden reproducir.
pub const CASCADE_TEXT: &str = "Varias canciones seguidas no se pudieron reproducir; se detuvo la reproducción.";

/// Aviso de una canción cortada por la red a media reproducción (en pausa en `position_ms`).
pub fn stalled_text(position_ms: u32) -> String {
    format!("Se cortó la conexión. Seguirá desde {} en cuanto vuelva.", super::watchdog::mmss(position_ms))
}

/// Aviso cuando se deja de esperar una carga (30 s en «cargando»).
pub fn stuck_text(name: Option<&str>) -> String {
    format!("Spotify no responde y {} no termina de cargar.", quoted(name))
}

/// Aviso sin ningún dispositivo de salida de audio.
pub const NO_OUTPUT_TEXT: &str = "No hay ninguna salida de audio. Conecta unos auriculares o altavoces.";

/// Aviso con una cuenta que no es Premium.
pub const NOT_PREMIUM_TEXT: &str = "Spotify solo permite reproducir en apps externas con Premium. Puedes seguir explorando tu biblioteca, buscar y gestionar playlists.";

/// Lo que dice la barra mientras una canción tarda en cargar (`watchdog::SLOW_AFTER`).
pub const SLOW_TEXT: &str = "Cargando… la conexión va lenta";

// ------------------------------------------------------------ anchos de la barra

/// Tamaño de letra y margen a cada lado del texto de la etiqueta de calidad.
const BADGE_FONT: f32 = 11.5;
const BADGE_PAD: f32 = 8.0;
/// Onda más estrecha que se acepta para hacer sitio a la etiqueta de calidad (más estrecha ya no
/// sirve para buscar con precisión). Por debajo, la etiqueta le quita sitio al texto.
const BADGE_MIN_WAVE_W: f32 = 100.0;
/// Texto de la canción más estrecho que se acepta para hacer sitio a la etiqueta.
const BADGE_MIN_TEXT_W: f32 = 140.0;

/// Anchos de la barra del reproductor: transporte | tiempo | ONDA | tiempo | volumen |
/// portada+texto | acciones.
#[derive(Debug, PartialEq)]
struct BarWidths {
    vol_zone: f32,
    transport_w: f32,
    times_w: f32,
    vol_w: f32,
    text_w: f32,
    actions_w: f32,
    wave_w: f32,
    /// Cabe la etiqueta de calidad (ya sumada a `actions_w`).
    badge: bool,
}

/// Reparte el ancho `w` de la barra. `badge_w`: lo que ocupa la etiqueta de calidad (0 si no
/// hay nada que enseñar). La etiqueta solo entra en la barra ancha y si cabe sin estropear lo
/// demás: primero estrecha la onda (hasta `BADGE_MIN_WAVE_W`) y después el texto de la canción
/// (hasta `BADGE_MIN_TEXT_W`); si ni así, no se enseña y la barra queda como sin ella.
fn bar_widths(w: f32, badge_w: f32) -> BarWidths {
    let vol_zone = if w >= 900.0 { 100.0 } else { 0.0 };
    let transport_w = 38.0 + 4.0 * 30.0 + 5.0 * 4.0 + 8.0;
    let times_w = 2.0 * 42.0;
    let vol_w = 28.0 + vol_zone + 8.0;
    let mut actions_w = if w >= 900.0 { 6.0 * 32.0 + 24.0 + 16.0 } else { 4.0 * 32.0 + 24.0 };
    let mut text_w = (w * 0.20).clamp(140.0, 300.0);
    let free = |text_w: f32, actions_w: f32| w - transport_w - times_w - vol_w - (46.0 + 10.0 + text_w) - actions_w - 3.0 * 12.0;
    let mut badge = false;
    if w >= 900.0 && badge_w > 0.0 {
        let from_wave = (free(text_w, actions_w) - BADGE_MIN_WAVE_W).clamp(0.0, badge_w);
        let from_text = badge_w - from_wave;
        if text_w - from_text >= BADGE_MIN_TEXT_W {
            text_w -= from_text;
            actions_w += badge_w;
            badge = true;
        }
    }
    let wave_w = free(text_w, actions_w).max(80.0);
    BarWidths { vol_zone, transport_w, times_w, vol_w, text_w, actions_w, wave_w, badge }
}

// ------------------------------------------------------------ píldora de actualización

/// Texto de la píldora de la barra superior cuando hay una versión nueva lista.
const UPDATE_PILL_TEXT: &str = "Reiniciar para actualizar";
/// Alto de la píldora (el ancho de solo el icono) y margen a cada lado del contenido.
const PILL_H: f32 = 32.0;
const PILL_PAD: f32 = 14.0;
/// Ancho de la zona de la derecha sin la píldora (ajustes y perfil).
const TOP_RIGHT_W: f32 = 110.0;
/// Lo que se deja como mínimo a las pestañas (Inicio, Buscar, atrás y adelante) antes de
/// encoger la píldora a solo el icono.
const TOP_TABS_MIN_W: f32 = 320.0;

/// Cómo cabe la píldora: entera (con este ancho) o solo el icono.
#[derive(Clone, Copy, Debug, PartialEq)]
enum UpdatePill {
    Full(f32),
    Icon,
}

/// Ancho de la zona de la derecha de la barra superior y cómo cabe en ella la píldora de
/// «Reiniciar para actualizar». `full_w`: ancho de la barra; `pill_text_w`: ancho del texto de
/// la píldora (`None`: no hay versión lista). Con la ventana estrecha o la interfaz ampliada
/// queda solo el icono, para no quitarles el sitio a las pestañas ni montarse sobre el perfil.
fn top_right_layout(full_w: f32, pill_text_w: Option<f32>) -> (f32, Option<UpdatePill>) {
    let Some(text_w) = pill_text_w else {
        return (TOP_RIGHT_W, None);
    };
    // Separación con el botón de ajustes (la de la fila).
    let gap = 6.0;
    let full = (PILL_PAD + 18.0 + 6.0 + text_w + PILL_PAD).ceil();
    let tabs = full_w - SIDEBAR_W - (TOP_RIGHT_W + gap + full);
    if tabs >= TOP_TABS_MIN_W {
        (TOP_RIGHT_W + gap + full, Some(UpdatePill::Full(full)))
    } else {
        (TOP_RIGHT_W + gap + PILL_H, Some(UpdatePill::Icon))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lo que ocupan juntas las partes de la barra (con la onda que quede).
    fn total(b: &BarWidths) -> f32 {
        b.transport_w + b.times_w + b.wave_w + b.vol_w + 12.0 + 46.0 + 10.0 + b.text_w + b.actions_w + 2.0 * 12.0
    }

    /// La etiqueta de calidad no cambia nada si no hay qué enseñar, entra en la ventana por
    /// defecto (quitando sitio primero a la onda y luego al texto) y nunca hace que la barra se
    /// salga de su ancho más de lo que ya se salía sin ella.
    #[test]
    fn anchos_de_la_barra_con_etiqueta_de_calidad() {
        let badge = 64.0;
        for w in [700.0, 899.0, 900.0, 960.0, 1030.0, 1076.0, 1200.0, 1500.0, 1876.0, 2500.0] {
            let sin = bar_widths(w, 0.0);
            assert!(!sin.badge, "{w}");
            let con = bar_widths(w, badge);
            if !con.badge {
                // No cabe: la barra queda exactamente como sin etiqueta.
                assert_eq!(con, sin, "{w}");
                continue;
            }
            assert!(w >= 900.0, "{w}");
            assert_eq!(con.actions_w, sin.actions_w + badge, "{w}");
            assert!(con.text_w >= BADGE_MIN_TEXT_W, "{w}");
            assert!(con.text_w <= sin.text_w, "{w}");
            // La onda solo baja de BADGE_MIN_WAVE_W si ya estaba por debajo sin la etiqueta.
            assert!(con.wave_w >= BADGE_MIN_WAVE_W.min(sin.wave_w), "{w}: {} {}", con.wave_w, sin.wave_w);
            // Nada se sale más de lo que ya se salía.
            assert!(total(&con) <= total(&sin).max(w) + 0.01, "{w}: {} {}", total(&con), total(&sin));
        }
        // Barra estrecha (modo compacto): nunca.
        assert!(!bar_widths(899.0, badge).badge);
        // Ventana por defecto (1120 px, barra de ~1076): cabe, quitando un poco al texto.
        let def = bar_widths(1076.0, badge);
        assert!(def.badge);
        assert!(def.text_w < bar_widths(1076.0, 0.0).text_w);
        // Pantalla grande: sale de la onda y el texto no cambia.
        let big = bar_widths(1876.0, badge);
        assert!(big.badge);
        assert_eq!(big.text_w, bar_widths(1876.0, 0.0).text_w);
        assert_eq!(big.wave_w, bar_widths(1876.0, 0.0).wave_w - badge);
        // Justo por encima de 900 ya no queda sitio ni en la onda ni en el texto.
        assert!(!bar_widths(960.0, badge).badge);
    }

    /// Dos reintentos automáticos (15 s y 60 s) y después el aviso con botones.
    #[test]
    fn reintentos_de_una_carga_fallida() {
        assert_eq!(load_retry_delay(0), None);
        assert_eq!(load_retry_delay(1), Some(Duration::from_secs(15)));
        assert_eq!(load_retry_delay(2), Some(Duration::from_secs(60)));
        assert_eq!(load_retry_delay(3), None);
        assert_eq!(load_retry_delay(u8::MAX), None);
    }

    #[test]
    fn textos_del_aviso_de_reproduccion() {
        let frenando = LoadFailure::KeyDenied(2);
        assert_eq!(
            retrying_text(&frenando, Some("Hey Jude")),
            "Spotify está frenando las peticiones de reproducción; reintentando «Hey Jude»…"
        );
        assert_eq!(
            retrying_text(&LoadFailure::RateLimited, None),
            "Spotify está frenando las peticiones de reproducción; reintentando la canción…"
        );
        assert_eq!(
            retrying_text(&LoadFailure::Network("cdn".into()), Some("X")),
            "Se cortó la conexión al cargar «X»; reintentando…"
        );
        assert_eq!(failed_text(Some("Hey Jude")), "No se pudo reproducir «Hey Jude».");
        // Un nombre vacío es como no saberlo.
        assert_eq!(failed_text(Some("  ")), "No se pudo reproducir la canción.");
        let retirada = LoadFailure::NotAvailable("país".into());
        assert_eq!(
            skipped_text(&retirada, Some("Hey Jude")),
            "«Hey Jude» no está disponible en tu país o se retiró. Pasamos a la siguiente."
        );
        assert_eq!(
            skipped_text(&LoadFailure::NoFormat, None),
            "La canción no está disponible en tu país o se retiró. Pasamos a la siguiente."
        );
        assert_eq!(
            skipped_text(&LoadFailure::Decode("x".into()), Some("Y")),
            "No se pudo reproducir «Y». Pasamos a la siguiente."
        );
        assert_eq!(stalled_text(133_400), "Se cortó la conexión. Seguirá desde 2:13 en cuanto vuelva.");
        assert_eq!(stuck_text(Some("Hey Jude")), "Spotify no responde y «Hey Jude» no termina de cargar.");
        assert_eq!(stuck_text(None), "Spotify no responde y la canción no termina de cargar.");
        // Ningún texto de un fallo menciona ya la cuenta sin Premium: casi nunca era eso. Solo
        // el aviso propio, que llega cuando Spotify dice de verdad que la cuenta no lo es.
        for t in [
            retrying_text(&frenando, None),
            failed_text(None),
            skipped_text(&retirada, None),
            CASCADE_TEXT.to_string(),
            stalled_text(0),
            stuck_text(None),
            NO_OUTPUT_TEXT.to_string(),
            SLOW_TEXT.to_string(),
        ] {
            assert!(!t.contains("Premium"), "{t}");
        }
        assert!(NOT_PREMIUM_TEXT.contains("Premium"));
    }

    #[test]
    fn avisos_que_se_van_solos() {
        let t0 = Instant::now();
        let mut e = PlaybackError::new(PlaybackErrorKind::Skipped, "x".into());
        e.since = t0;
        assert!(!e.has_actions());
        assert!(!e.expired(t0));
        assert_eq!(e.remaining(t0 + Duration::from_secs(3)), Some(SKIPPED_BANNER_FOR - Duration::from_secs(3)));
        assert!(e.expired(t0 + SKIPPED_BANNER_FOR));
        for kind in [
            PlaybackErrorKind::Retrying,
            PlaybackErrorKind::Failed,
            PlaybackErrorKind::Cascade,
            PlaybackErrorKind::Stalled,
            PlaybackErrorKind::Stuck,
            PlaybackErrorKind::NoOutput,
            PlaybackErrorKind::NotPremium,
        ] {
            let mut e = PlaybackError::new(kind, "x".into());
            e.since = t0;
            assert_eq!(e.remaining(t0), None);
            assert!(!e.expired(t0 + Duration::from_secs(3600)), "{kind:?}");
            // [Reintentar] y [Saltar] solo donde sirven: sin salida o sin Premium no hay nada
            // que reintentar desde el aviso (sigue sola al conectar un dispositivo).
            let actions = !matches!(
                kind,
                PlaybackErrorKind::Cascade | PlaybackErrorKind::NoOutput | PlaybackErrorKind::NotPremium
            );
            assert_eq!(e.has_actions(), actions, "{kind:?}");
        }
    }

    #[test]
    fn pildora_de_actualizacion() {
        // Sin versión lista, la zona de siempre.
        assert_eq!(top_right_layout(1120.0, None), (TOP_RIGHT_W, None));
        // Ventana normal: entera, y la zona crece justo lo que ocupa.
        let (w, pill) = top_right_layout(1120.0, Some(160.0));
        let Some(UpdatePill::Full(pw)) = pill else { panic!("{pill:?}") };
        assert_eq!(w, TOP_RIGHT_W + 6.0 + pw);
        assert!(pw >= 160.0 + 18.0 + 2.0 * PILL_PAD);
        assert!(1120.0 - SIDEBAR_W - w >= TOP_TABS_MIN_W);
        // Interfaz al 200 % (560 puntos de ancho) o la ventana mínima: solo el icono.
        for full in [560.0, 760.0] {
            let (w, pill) = top_right_layout(full, Some(160.0));
            assert_eq!(pill, Some(UpdatePill::Icon), "{full}");
            assert_eq!(w, TOP_RIGHT_W + 6.0 + PILL_H);
        }
    }
}
