//! Barra superior (pestañas y búsqueda), biblioteca lateral y reproductor flotante.

use std::time::{Duration, Instant};

use egui::{pos2, vec2, Align, Color32, CornerRadius, Label, Layout, Rect, RichText, Sense, UiBuilder};
use librespot_playback::player::LoadFailure;

use super::icons::{self, Icon};
use super::theme::{self, GREEN};
use super::widgets::{child_in, galley_truncated, text_on_baseline, MenuKind, RowOpts};
use super::{Action, App, Auth, Page, PlayState, PlayTarget, Repeat, SideTab, SEARCH_ID, SIDEBAR_W};
use crate::api::Req;
use crate::config::{vol_pct_to_raw, vol_raw_db, vol_raw_to_pct};
use crate::model::*;

impl App {
    // ------------------------------------------------------------ barra superior

    /// Barra superior, copia de la referencia de diseño: «Tu biblioteca» sobre la barra lateral;
    /// Inicio y Buscar (icono y texto, sin fondo) y, detrás, una pestaña por página abierta; a la
    /// derecha, ajustes y la foto del perfil. Posiciones fijas medidas desde los bordes.
    pub fn top_bar(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        let full = ui.available_rect_before_wrap();
        let page = self.page().clone();
        let st = TopStyle::new(&p);
        let cy = full.min.y + 29.0;
        let x0 = full.min.x;

        // «Tu biblioteca»
        {
            let rect = Rect::from_min_max(pos2(x0 + 18.0, cy - 18.0), pos2(x0 + SIDEBAR_W - 20.0, cy + 18.0));
            let resp = ui.interact(rect, ui.id().with("top_library"), Sense::click());
            let color = st.color(page == Page::Library, resp.hovered());
            icons::paint(ui.painter(), Rect::from_center_size(pos2(x0 + 41.5, cy), vec2(27.0, 27.0)), st.icon_of(color), Icon::Library);
            let g = ui.painter().layout_no_wrap("Tu biblioteca".into(), theme::regular(TOP_FONT), color);
            text_on_baseline(ui.painter(), pos2(x0 + 72.0, cy + 6.0), g, color);
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                self.go(Page::Library);
            }
        }

        // Derecha: foto del perfil, ajustes y, con una versión nueva lista, «Reiniciar para
        // actualizar» a su izquierda.
        let xr = full.max.x;
        let avatar = Rect::from_center_size(pos2(xr - 25.0, cy), vec2(34.0, 34.0));
        self.top_avatar(ui, avatar, &p);
        let gear = Rect::from_center_size(pos2(xr - 74.0, cy), vec2(30.0, 30.0));
        let gr = ui.interact(gear.expand(4.0), ui.id().with("top_gear"), Sense::click()).on_hover_text("Ajustes (Ctrl+,)");
        let gear_color = st.color(page == Page::Settings, gr.hovered());
        icons::paint(ui.painter(), gear, st.icon_of(gear_color), Icon::Settings);
        if gr.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if gr.clicked() {
            self.draft = self.settings.clone();
            self.go(Page::Settings);
        }
        let mut tabs_end = gear.min.x - 24.0;
        let ready = match &self.update_stage {
            crate::update::Stage::Ready { version, .. } => Some(version.clone()),
            _ => None,
        };
        if let Some(version) = ready {
            let text_w = ui.painter().layout_no_wrap(UPDATE_PILL_TEXT.to_string(), theme::bold(13.0), Color32::BLACK).size().x;
            let (_, pill) = top_right_layout(full.width(), Some(text_w));
            if let Some(pill) = pill {
                let w = match pill {
                    UpdatePill::Full(w) => w,
                    UpdatePill::Icon => PILL_H,
                };
                let rect = Rect::from_min_size(pos2(gear.min.x - 16.0 - w, cy - PILL_H / 2.0), vec2(w, PILL_H));
                let mut c = child_in(ui, rect, Layout::left_to_right(Align::Center));
                if self.update_pill(&mut c, &version, pill).clicked() {
                    // En una Jam, el aviso pide confirmarlo antes (se sale de ella).
                    self.restart_to_update(false);
                }
                tabs_end = rect.min.x - 16.0;
            }
        }

        // Inicio y Buscar en las posiciones de la referencia (con la ventana estrecha, Buscar se
        // acerca a Inicio); Buscar se vuelve un campo de texto en su página.
        let home_c = pos2(x0 + 303.0, cy);
        if self.top_item(ui, home_c, Icon::Home, 28.0, "Inicio", self.active == super::ActiveTab::Home, &st).clicked() {
            self.go(Page::Home);
        }
        let search_c = pos2((x0 + 722.5).min(tabs_end - 90.0).max(home_c.x + 130.0), cy);
        if page == Page::Search {
            self.search_field(ui, search_c, &st, &p);
        } else if self.top_item(ui, search_c, Icon::Search, 29.0, "Buscar", false, &st).clicked() {
            self.focus_search = true;
            self.go(Page::Search);
        }

        // Una pestaña por página abierta, cada 210 px detrás de Buscar (más juntas si no caben).
        let tabs: Vec<(usize, Icon, String)> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let (icon, title) = self.page_label(t.page());
                (i, icon, title)
            })
            .collect();
        let first = search_c.x + TAB_PITCH;
        let room = (tabs_end - (first - 16.0)).max(0.0);
        let pitch = if tabs.is_empty() { TAB_PITCH } else { (room / tabs.len() as f32).clamp(56.0, TAB_PITCH) };
        for (k, (i, icon, title)) in tabs.into_iter().enumerate() {
            let c = pos2(first + k as f32 * pitch, cy);
            if c.x + 40.0 > tabs_end {
                break;
            }
            let selected = self.active == super::ActiveTab::Tab(i);
            let text_w = (pitch - 30.5 - 40.0).max(0.0);
            let r = self.top_tab(ui, c, icon, &title, text_w, selected, &st);
            // Cerrar: × al pasar el ratón o botón central
            let close = Rect::from_center_size(pos2(c.x + pitch - 34.0, cy), vec2(20.0, 20.0));
            let hovered = r.hovered() || ui.rect_contains_pointer(close);
            if hovered {
                let cr = ui.interact(close, ui.id().with(("close_tab", i)), Sense::click());
                icons::paint(ui.painter(), close.shrink(5.0), if cr.hovered() { p.text } else { st.text }, Icon::Close);
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
        ui.allocate_rect(full, Sense::hover());
    }

    /// Foto del perfil (o la inicial) en un círculo; abre el perfil o inicia sesión.
    fn top_avatar(&mut self, ui: &mut egui::Ui, rect: Rect, p: &theme::Palette) {
        let resp = ui.interact(rect, ui.id().with("top_avatar"), Sense::click());
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
            Some(url) => self.cover_in(ui, Some(&url), rect, 17),
            None => {
                ui.painter().circle_filled(rect.center(), 17.0, p.card2);
                let initial = self
                    .user
                    .as_ref()
                    .and_then(|u| u.display_name.clone())
                    .and_then(|n| n.chars().next())
                    .map(|c| c.to_uppercase().to_string())
                    .unwrap_or_else(|| "·".to_string());
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, initial, theme::bold(15.0), p.text);
            }
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let logged = self.logged_in();
        if resp.clicked() {
            if let Some(id) = self.my_id().map(|s| s.to_string()) {
                self.go(Page::User(id));
            } else if !logged {
                self.login();
            }
        }
        resp.on_hover_text(if logged { "Tu perfil" } else { "Iniciar sesión" });
    }

    /// Inicio o Buscar: icono centrado en `c` (de lado `size`) y el texto 31 px a su derecha.
    fn top_item(&mut self, ui: &mut egui::Ui, c: egui::Pos2, icon: Icon, size: f32, text: &str, active: bool, st: &TopStyle) -> egui::Response {
        let g = ui.painter().layout_no_wrap(text.to_string(), theme::regular(TOP_FONT), st.text);
        let rect = Rect::from_min_max(pos2(c.x - 18.0, c.y - 18.0), pos2(c.x + 31.0 + g.size().x + 10.0, c.y + 18.0));
        let resp = ui.interact(rect, ui.id().with(("top_item", text)), Sense::click());
        let color = st.color(active, resp.hovered());
        // La lupa (con el mango abajo a la derecha) va 1 px arriba a la izquierda.
        let ic = if icon == Icon::Search { c - vec2(1.0, 2.0) } else { c };
        icons::paint(ui.painter(), Rect::from_center_size(ic, vec2(size, size)), st.icon_of(color), icon);
        let g = ui.painter().layout_no_wrap(text.to_string(), theme::regular(TOP_FONT), color);
        text_on_baseline(ui.painter(), pos2(c.x + 31.0, c.y + 6.0), g, color);
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        resp
    }

    /// Pestaña de una página abierta: como Inicio y Buscar, con el texto recortado a `text_w`.
    fn top_tab(&mut self, ui: &mut egui::Ui, c: egui::Pos2, icon: Icon, title: &str, text_w: f32, selected: bool, st: &TopStyle) -> egui::Response {
        let rect = Rect::from_min_max(pos2(c.x - 18.0, c.y - 18.0), pos2(c.x + 31.0 + text_w + 34.0, c.y + 18.0));
        let resp = ui.interact(rect, ui.id().with(("top_tab", title, c.x as i32)), Sense::click());
        let color = st.color(selected, resp.hovered());
        icons::paint(ui.painter(), Rect::from_center_size(c, vec2(26.0, 26.0)), st.icon_of(color), icon);
        if text_w > 8.0 {
            let g = galley_truncated(ui.painter(), title, theme::regular(TOP_FONT), color, text_w);
            text_on_baseline(ui.painter(), pos2(c.x + 31.0, c.y + 6.0), g, color);
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        resp
    }

    /// Buscar en su página: la lupa en su sitio y el campo de texto a su derecha, sobre una
    /// píldora tenue.
    fn search_field(&mut self, ui: &mut egui::Ui, c: egui::Pos2, st: &TopStyle, p: &theme::Palette) {
        let pill = Rect::from_min_max(pos2(c.x - 22.0, c.y - 19.0), pos2(c.x + 330.0, c.y + 19.0));
        ui.painter().rect_filled(pill, CornerRadius::same(19), p.hover.gamma_multiply(0.8));
        icons::paint(ui.painter(), Rect::from_center_size(c - vec2(1.0, 2.0), vec2(29.0, 29.0)), st.icon_of(st.active), Icon::Search);
        let field = Rect::from_min_max(pos2(c.x + 31.0, c.y - 13.0), pos2(pill.max.x - 14.0, c.y + 13.0));
        let mut f = child_in(ui, field, Layout::left_to_right(Align::Center));
        let edit = egui::TextEdit::singleline(&mut self.search_query)
            .id(egui::Id::new(SEARCH_ID))
            .frame(egui::Frame::NONE)
            .font(theme::regular(TOP_FONT))
            .hint_text("Buscar…")
            .desired_width(field.width());
        let r = f.add(edit);
        if self.focus_search {
            self.focus_search = false;
            r.request_focus();
            // La consulta anterior queda seleccionada: lo que se escriba la sustituye (como
            // Ctrl+F en un navegador), y con las flechas o un clic se puede seguir editando.
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
            Page::Shows => (Icon::Podcast, "Podcasts".into()),
            Page::Audiobooks => (Icon::Book, "Audiolibros".into()),
            Page::Folders => (Icon::Folder, "Carpetas".into()),
            Page::Artists => (Icon::Artist, "Artistas".into()),
            Page::Settings => (Icon::Settings, "Ajustes".into()),
            Page::Show(id) => (Icon::Podcast, self.shows.get(id).map(|s| s.0.name.clone()).unwrap_or_else(|| "Podcast".into())),
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
                Icon::Person,
                self.users.get(id).map(|u| u.name().to_string()).unwrap_or_else(|| "Perfil".into()),
            ),
        }
    }

    // ---------------------------------------------------------- barra lateral

    /// Barra lateral, copia de la referencia de diseño: filas de 51,5 px (Fijados y Playlists
    /// con «›» para desplegar), icono de 28 px y texto gris. Historial va al final, detrás de
    /// Artistas. Abajo, el último aviso de estado si lo hay.
    pub fn sidebar(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        let st = TopStyle::new(&p);
        let page = self.page().clone();
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        ui.add_space(SIDE_FIRST_CENTER - SIDE_ROW_H / 2.0);

        // Fijados. Índices y no copias: este panel se dibuja en todas las páginas, cada fotograma.
        let pinned: Vec<usize> = {
            let set: std::collections::HashSet<&str> = self.settings.pinned.iter().map(String::as_str).collect();
            (0..self.playlists.len()).filter(|&i| set.contains(self.playlists[i].id.as_str())).collect()
        };
        let r = Self::side_row(ui, Icon::Pin, "Fijados", false, &st);
        Self::side_chevron(ui, r.rect, self.sidebar_pins_open, st.icon_of(st.color(false, r.hovered())));
        if r.clicked() {
            self.sidebar_pins_open = !self.sidebar_pins_open;
        }
        if self.sidebar_pins_open {
            if pinned.is_empty() {
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), SIDE_CHILD_H), Sense::hover());
                let g = ui.painter().layout_no_wrap("Fija playlists con el clic derecho".into(), theme::regular(12.5), p.faint);
                text_on_baseline(ui.painter(), pos2(rect.min.x + SIDE_CHILD_TEXT_X, rect.center().y + 5.0), g, p.faint);
            }
            for i in pinned {
                // Copia solo de las fijadas (pocas): el menú contextual necesita `&mut self`.
                let pl = self.playlists[i].clone();
                let sel = matches!(&page, Page::Playlist(id) if *id == pl.id);
                let r = Self::side_child(ui, &pl.name, sel, &st);
                if r.clicked() {
                    self.go(Page::Playlist(pl.id.clone()));
                }
                self.playlist_context_menu(&r, &pl);
            }
        }

        // Playlists, con «+» (nueva playlist) al pasar el ratón.
        let r = Self::side_row(ui, Icon::Playlist, "Playlists", false, &st);
        Self::side_chevron(ui, r.rect, self.sidebar_playlists_open, st.icon_of(st.color(false, r.hovered())));
        {
            let plus = Rect::from_center_size(pos2(r.rect.min.x + 214.0, r.rect.center().y), vec2(26.0, 26.0));
            let pr = ui.interact(plus, ui.id().with("new_pl"), Sense::click());
            if pr.hovered() || r.hovered() {
                icons::paint(ui.painter(), plus.shrink(5.0), if pr.hovered() { st.hover } else { st.icon }, Icon::Plus);
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
                Self::skeleton_rows(ui, 6, SIDE_CHILD_H, 18.0, 28.0);
            }
            // Debajo quedan 8 filas (Me gusta… Historial) y el aviso de estado.
            let avail = (ui.available_height() - 8.0 * SIDE_ROW_H - 40.0).max(SIDE_CHILD_H * 2.0);
            // Solo las filas a la vista: con cientos de playlists, copiarlas y maquetarlas todas
            // costaba casi un milisegundo por fotograma en cualquier página.
            egui::ScrollArea::vertical()
                .id_salt("sidebar_playlists")
                .auto_shrink([false, true])
                .max_height(avail)
                .show_rows(ui, SIDE_CHILD_H, self.playlists.len(), |ui, range| {
                    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                    for i in range {
                        let Some(pl) = self.playlists.get(i).cloned() else { break };
                        let sel = matches!(&page, Page::Playlist(id) if *id == pl.id);
                        // Id de la fila por playlist: con ids automáticos el hover y el menú
                        // abierto saltarían de fila al desplazarse.
                        let key = ui.id().with(("sb_pl", i, &pl.id));
                        let r = ui.scope_builder(UiBuilder::new().id(key), |ui| Self::side_child(ui, &pl.name, sel, &st)).inner;
                        if r.clicked() {
                            self.go(Page::Playlist(pl.id.clone()));
                        }
                        self.playlist_context_menu(&r, &pl);
                    }
                });
        }

        let items = [
            (Icon::Heart, "Canciones que te gustan", Page::Liked),
            (Icon::Bookmark, "Guardados", Page::Saves),
            (Icon::Album, "Álbumes", Page::Albums),
            (Icon::Folder, "Carpetas", Page::Folders),
            (Icon::Podcast, "Podcasts", Page::Shows),
            (Icon::Book, "Audiolibros", Page::Audiobooks),
            (Icon::Artist, "Artistas", Page::Artists),
            (Icon::History, "Historial", Page::History),
        ];
        for (icon, label, pg) in items {
            if Self::side_row(ui, icon, label, page == pg, &st).clicked() {
                self.go(pg);
            }
        }

        // Último aviso de estado, abajo.
        if let Some((text, _, err)) = self.status.clone() {
            ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                let color = if err { theme::RED } else { p.weak };
                ui.add_space(12.0);
                // «Conecta tu biblioteca…» llega desde cualquier página que lea la Web API: la
                // acción va con el aviso. (De abajo arriba: queda debajo del texto.)
                if text == crate::api::NO_APP_HINT && !self.api.web_configured() && !self.web_busy {
                    ui.horizontal(|ui| {
                        ui.add_space(24.0);
                        if ui.link(RichText::new("Conectar con Spotify").small()).clicked() {
                            self.connect_library();
                        }
                    });
                }
                let w = ui.available_width() - 48.0;
                let galley = ui.painter().layout(text, theme::regular(12.0), color, w);
                let (rect, _) = ui.allocate_exact_size(vec2(w, galley.size().y), Sense::hover());
                ui.painter().galley(pos2(rect.min.x + 24.0, rect.min.y), galley, color);
            });
        }
    }

    /// Fila principal de la barra lateral: icono de 28 px centrado a 37 px del borde y el texto a
    /// 67 px, con la línea base 7 px por debajo del centro.
    fn side_row(ui: &mut egui::Ui, icon: Icon, text: &str, selected: bool, st: &TopStyle) -> egui::Response {
        let w = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(vec2(w, SIDE_ROW_H), Sense::click());
        let color = st.color(selected, resp.hovered());
        let c = rect.center().y;
        let (side, dx, dy) = side_icon_fit(icon);
        icons::paint(ui.painter(), Rect::from_center_size(pos2(rect.min.x + 37.0 + dx, c + dy), vec2(side, side)), st.icon_of(color), icon);
        let g = galley_truncated(ui.painter(), text, theme::regular(SIDE_FONT), color, (w - 67.0 - 44.0).max(20.0));
        text_on_baseline(ui.painter(), pos2(rect.min.x + 67.0, c + 7.0), g, color);
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        resp
    }

    /// Playlist dentro de Fijados o Playlists desplegados: fila de 40 px, más metida.
    fn side_child(ui: &mut egui::Ui, text: &str, selected: bool, st: &TopStyle) -> egui::Response {
        let w = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(vec2(w, SIDE_CHILD_H), Sense::click());
        let color = st.color(selected, resp.hovered());
        let c = rect.center().y;
        icons::paint(ui.painter(), Rect::from_center_size(pos2(rect.min.x + 60.0, c), vec2(22.0, 22.0)), st.icon_of(color), Icon::PlaylistItem);
        let g = galley_truncated(ui.painter(), text, theme::regular(SIDE_CHILD_FONT), color, (w - SIDE_CHILD_TEXT_X - 24.0).max(20.0));
        text_on_baseline(ui.painter(), pos2(rect.min.x + SIDE_CHILD_TEXT_X, c + 5.0), g, color);
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        resp
    }

    /// «›» (plegado) o «⌄» (desplegado) a 245,5 px del borde, de 9 × 14 px.
    fn side_chevron(ui: &mut egui::Ui, rect: Rect, open: bool, color: Color32) {
        let c = pos2(rect.min.x + 245.5, rect.center().y + 1.0);
        let pts = if open {
            vec![pos2(c.x - 6.5, c.y - 3.0), pos2(c.x, c.y + 3.5), pos2(c.x + 6.5, c.y - 3.0)]
        } else {
            vec![pos2(c.x - 3.0, c.y - 6.5), pos2(c.x + 3.5, c.y), pos2(c.x - 3.0, c.y + 6.5)]
        };
        ui.painter().add(egui::Shape::line(pts, egui::Stroke::new(2.0, color)));
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

    /// Reproductor de abajo, copia de la referencia de diseño (barra de 1491 × 80 px): a la
    /// izquierda los mandos, el tiempo, el progreso y el altavoz; en medio la portada con título,
    /// artista y álbum; a la derecha corazón, playlist, letra, dispositivos, más | Jam y cola.
    /// Todo va en posiciones fijas medidas desde los bordes (`bar_layout`); solo el progreso y el
    /// texto cambian de ancho con la ventana.
    pub fn player_bar(&mut self, ui: &mut egui::Ui) {
        let full = ui.available_rect_before_wrap();
        let st = BarStyle::new(ui);
        ui.painter().rect_filled(full, CornerRadius::same(BAR_RADIUS), st.bg);
        let lay = bar_layout(full.width());
        // Centro vertical de los mandos: un píxel por debajo del centro de la barra.
        let cy = full.center().y + 1.0;
        let x0 = full.min.x;
        let xr = full.max.x;
        // Cajas de 28 px de los iconos: esquina superior izquierda (la de la referencia).
        let boxed = |x: f32, dy: f32| Rect::from_min_size(pos2(x, cy - 14.0 + dy), vec2(BOX, BOX));

        // ---- transporte
        self.play_button(ui, pos2(x0 + 40.0, cy), &st);
        if lay.shuffle_repeat {
            let episode = self.player.now.as_ref().is_some_and(|n| n.uri.starts_with("spotify:episode:"));
            if self.bar_icon(ui, boxed(x0 + 69.0, 0.0), Icon::Prev, st.icon, "Anterior (Ctrl+←)").clicked() {
                self.prev();
            }
            if self.bar_icon(ui, boxed(x0 + 111.0, 0.0), Icon::Next, st.icon, "Siguiente (Ctrl+→)").clicked() {
                self.next();
            }
            if episode {
                // En un episodio, como en Spotify: 15 s atrás y adelante en lugar de aleatorio y repetir.
                if self.bar_icon(ui, boxed(x0 + 154.0, 0.0), Icon::Replay15, st.icon, "Retroceder 15 s").clicked() {
                    self.seek_by(-15_000);
                }
                if self.bar_icon(ui, boxed(x0 + 197.0, 0.0), Icon::Forward15, st.icon, "Avanzar 15 s").clicked() {
                    self.seek_by(15_000);
                }
            } else {
                let shuffle = if self.player.shuffle { GREEN } else { st.icon };
                if self.bar_icon(ui, boxed(x0 + 154.0, 0.0), Icon::Shuffle, shuffle, "Aleatorio (S)").clicked() {
                    self.toggle_shuffle();
                }
                let (rep_icon, rep_color) = match self.player.repeat {
                    Repeat::Off => (Icon::Repeat, st.icon),
                    Repeat::Context => (Icon::Repeat, GREEN),
                    Repeat::Track => (Icon::RepeatOne, GREEN),
                };
                if self.bar_icon(ui, boxed(x0 + 197.0, 1.0), rep_icon, rep_color, "Repetir (R)").clicked() {
                    self.cycle_repeat();
                }
            }
        } else {
            if self.bar_icon(ui, boxed(x0 + 69.0, 0.0), Icon::Prev, st.icon, "Anterior (Ctrl+←)").clicked() {
                self.prev();
            }
            if self.bar_icon(ui, boxed(x0 + 111.0, 0.0), Icon::Next, st.icon, "Siguiente (Ctrl+→)").clicked() {
                self.next();
            }
        }

        // ---- tiempo y progreso
        let px0 = x0 + lay.prog_x;
        let px1 = px0 + lay.prog_w;
        let dur = self.player.now.as_ref().map(|n| n.duration_ms).unwrap_or(0);
        let pos = self.seek_drag.unwrap_or_else(|| self.player.position());
        let painter = ui.painter().clone();
        let time = theme::regular(TIME_FONT);
        let left = painter.layout_no_wrap(fmt_ms(pos), time.clone(), st.dim);
        let lw = left.size().x;
        text_on_baseline(&painter, pos2(px0 - 8.0 - lw, cy + 5.0), left, st.dim);
        if lay.tail {
            let right = painter.layout_no_wrap(fmt_ms(dur), time, st.dim);
            text_on_baseline(&painter, pos2(px1 + 9.0, cy + 5.0), right, st.dim);
        }
        self.progress_line(ui, Rect::from_min_max(pos2(px0, cy - 10.0), pos2(px1, cy + 8.0)), cy - 0.75, dur, pos, &st);

        // ---- volumen: el altavoz; la línea de volumen sale al pasar por encima
        if lay.tail {
            self.volume_button(ui, boxed(px1 + 54.0, 0.0), &st);
        }

        // ---- portada, título, artista y álbum
        if lay.cover {
            let cover_x = x0 + lay.cover_x();
            let text_x = x0 + lay.text_x();
            let text_w = (xr - lay.right_w - 12.0 - text_x).max(40.0);
            self.now_playing_block(ui, Rect::from_min_size(pos2(cover_x, cy - 26.0), vec2(COVER, COVER)), text_x, text_w, cy, &st);
        }

        // ---- derecha
        self.bar_actions(ui, xr, cy, &lay, &st);
        ui.allocate_rect(full, Sense::hover());
    }

    /// Botón verde de reproducir / pausar (círculo de 39 px con un aro oscuro).
    fn play_button(&mut self, ui: &mut egui::Ui, c: egui::Pos2, st: &BarStyle) {
        let rect = Rect::from_center_size(c, vec2(42.0, 42.0));
        let resp = ui.interact(rect, ui.id().with("bar_play"), Sense::click()).on_hover_text("Reproducir / pausar (Espacio)");
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let loading = self.player.state == PlayState::Loading;
        let fill = if loading { st.loading } else { GREEN };
        let fill = if resp.hovered() { fill.lerp_to_gamma(Color32::WHITE, 0.12) } else { fill };
        let painter = ui.painter();
        // Aro oscuro, más grueso arriba (sombra), y un brillo claro por dentro abajo.
        painter.circle_filled(c, 20.8, st.play_ring);
        painter.circle_filled(pos2(c.x, c.y - 0.8), 20.6, st.play_ring);
        painter.circle_filled(pos2(c.x, c.y + 0.6), 19.4, fill.lerp_to_gamma(Color32::WHITE, 0.35));
        painter.circle_filled(c, 19.3, fill);
        match self.player.state {
            PlayState::Playing => {
                for dx in [-3.5, 3.5] {
                    let bar = Rect::from_center_size(pos2(c.x + dx, c.y - 0.3), vec2(2.6, 11.6));
                    painter.rect_filled(bar, CornerRadius::same(1), Color32::BLACK);
                }
            }
            PlayState::Loading => icons::paint(painter, Rect::from_center_size(c, vec2(18.0, 18.0)), st.icon, Icon::Hourglass),
            _ => icons::paint(painter, Rect::from_center_size(pos2(c.x + 1.0, c.y), vec2(20.0, 20.0)), Color32::BLACK, Icon::Play),
        }
        if resp.clicked() {
            self.play_pause();
        }
    }

    /// Icono del reproductor en su caja de 28 px; al pasar el ratón se aclara.
    fn bar_icon(&mut self, ui: &mut egui::Ui, rect: Rect, icon: Icon, color: Color32, tip: &str) -> egui::Response {
        let resp = ui.interact(rect, ui.id().with(("bar_icon", rect.min.x as i32, icon)), Sense::click()).on_hover_text(tip);
        let color = if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            color.lerp_to_gamma(Color32::WHITE, 0.45)
        } else {
            color
        };
        icons::paint(ui.painter(), rect, color, icon);
        resp
    }

    /// Línea de progreso: lo pendiente en gris oscuro, lo reproducido claro con una sombra
    /// debajo; al acercar el ratón aparece la bolita. Clic o arrastre para saltar.
    fn progress_line(&mut self, ui: &mut egui::Ui, zone: Rect, y: f32, dur: u32, pos: u32, st: &BarStyle) {
        let resp = ui.interact(zone, ui.id().with("progress"), Sense::click_and_drag());
        let near = resp.hovered() || resp.dragged() || resp.is_pointer_button_down_on();
        let frac = if dur > 0 { (pos as f32 / dur as f32).clamp(0.0, 1.0) } else { 0.0 };
        let (x0, x1) = (zone.min.x, zone.max.x);
        let xp = x0 + (x1 - x0) * frac;
        let painter = ui.painter();
        let h = PROGRESS_H / 2.0;
        painter.rect_filled(Rect::from_min_max(pos2(x0, y - h), pos2(x1, y + h)), CornerRadius::same(2), st.track);
        if xp > x0 {
            painter.rect_filled(Rect::from_min_max(pos2(x0 + 1.0, y + h), pos2(xp, y + h + 2.0)), CornerRadius::same(1), st.shadow);
            painter.rect_filled(Rect::from_min_max(pos2(x0, y - h), pos2(xp.max(x0 + PROGRESS_H), y + h)), CornerRadius::same(2), st.fill);
        }
        let knob = ui.ctx().animate_bool_with_time(ui.id().with("knob"), near, 0.12);
        if knob > 0.0 {
            painter.circle_filled(pos2(xp, y), 6.0 * knob, st.fill);
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

    /// Altavoz: clic para silenciar, rueda para subir o bajar. Al pasar por encima sale arriba
    /// la línea de volumen, que se queda mientras el ratón esté en ella.
    fn volume_button(&mut self, ui: &mut egui::Ui, rect: Rect, st: &BarStyle) {
        let icon = if self.player.volume == 0 { Icon::Mute } else { Icon::Volume };
        let resp = self.bar_icon(ui, rect, icon, st.icon, "Volumen · clic para silenciar (M)");
        if resp.clicked() {
            self.toggle_mute();
        }
        if resp.hovered() {
            self.volume_wheel_from(ui);
        }
        let id = egui::Id::new("volume_popup");
        let popup_rect = Rect::from_center_size(pos2(rect.center().x, rect.min.y - 30.0), vec2(150.0, 32.0));
        let was_open = ui.ctx().data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
        let pointer_in = ui.ctx().pointer_hover_pos().is_some_and(|p| popup_rect.expand(6.0).contains(p) || rect.contains(p));
        let open = resp.hovered() || self.volume_drag.is_some() || (was_open && pointer_in);
        ui.ctx().data_mut(|d| d.insert_temp(id, open));
        if !open {
            return;
        }
        egui::Area::new(id)
            .order(egui::Order::Foreground)
            .fixed_pos(popup_rect.min)
            .show(ui.ctx(), |ui| {
                let (area, _) = ui.allocate_exact_size(popup_rect.size(), Sense::hover());
                ui.painter().rect_filled(area, CornerRadius::same(16), st.bg.lerp_to_gamma(Color32::BLACK, 0.25));
                self.volume_line(ui, area.shrink2(vec2(14.0, 2.0)), st);
            });
    }

    /// Línea de volumen (misma forma que la de progreso), en dB, con la rueda y el arrastre.
    fn volume_line(&mut self, ui: &mut egui::Ui, zone: Rect, st: &BarStyle) {
        let resp = ui.interact(zone, egui::Id::new("volume_line"), Sense::click_and_drag());
        let raw_now = self.volume_drag.unwrap_or(self.player.volume);
        let pct = vol_raw_to_pct(raw_now);
        let y = zone.center().y;
        let (x0, x1) = (zone.min.x, zone.max.x);
        let xp = x0 + (x1 - x0) * (pct / 100.0).clamp(0.0, 1.0);
        let painter = ui.painter();
        let h = PROGRESS_H / 2.0;
        painter.rect_filled(Rect::from_min_max(pos2(x0, y - h), pos2(x1, y + h)), CornerRadius::same(2), st.track);
        painter.rect_filled(Rect::from_min_max(pos2(x0, y - h), pos2(xp.max(x0 + PROGRESS_H), y + h)), CornerRadius::same(2), st.fill);
        painter.circle_filled(pos2(xp, y), 6.0, st.fill);
        let db = vol_raw_db(raw_now);
        let tip = if db.is_finite() { format!("{pct:.0} %  ({db:+.1} dB)") } else { "Silencio".to_string() };
        if resp.hovered() {
            resp.clone().on_hover_text(tip);
            self.volume_wheel_from(ui);
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
    }

    /// La rueda del ratón sube o baja el volumen. Sin suavizar: el suavizado de egui reparte cada
    /// muesca en varios fotogramas y generaba una orden por fotograma (a tirones y con cola).
    fn volume_wheel_from(&mut self, ui: &egui::Ui) {
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

    /// Portada en `cover` y, a su derecha, título, artista y álbum en líneas de base fijas.
    fn now_playing_block(&mut self, ui: &mut egui::Ui, cover: Rect, text_x: f32, text_w: f32, cy: f32, st: &BarStyle) {
        let painter = ui.painter().clone();
        let Some(np) = self.player.now.clone() else {
            let g = painter.layout_no_wrap("Nada en reproducción".into(), theme::regular(ARTIST_FONT), st.dim);
            text_on_baseline(&painter, pos2(cover.min.x, cy + 5.0), g, st.dim);
            return;
        };
        self.cover_in(ui, np.cover_url.as_deref(), cover, COVER_RADIUS);
        let r = ui.interact(cover, ui.id().with("np_cover"), Sense::click());
        if r.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        // Lo que suena de verdad (formato, salida, normalización): en el globo de la portada.
        let r = match self.player.local_audio() {
            Some(info) if r.hovered() => {
                let tip = info
                    .tooltip(crate::backend::audio_output(), self.settings.loudness.label(), self.settings.quality.kbps())
                    .join("\n");
                r.on_hover_text(format!("{}\n{tip}", info.badge()))
            }
            _ => r,
        };
        if r.clicked() {
            if let Some(id) = &np.album_id {
                self.actions.push(Action::Go(Page::Album(id.clone())));
            }
        }
        r.context_menu(|ui| {
            let t = super::track_from_now(&np);
            self.song_menu(ui, &t, MenuKind::NowPlaying, &RowOpts::tracks(false, false));
        });
        // Tarda en cargar (`watchdog::SLOW_AFTER`): la tercera línea lo dice, en lugar del álbum.
        let slow = self.player.state == PlayState::Loading
            && self.player.remote.is_none()
            && self.load_watch.as_ref().is_some_and(|w| w.is_slow());
        let names = np.artists.iter().map(|a| a.0.as_str()).collect::<Vec<_>>().join(", ");
        let mut lines: Vec<(String, f32, Color32, f32)> = vec![
            (np.name.clone(), TITLE_FONT, st.title, cy - 13.0),
            (names, ARTIST_FONT, st.dim, cy + 5.0),
        ];
        if slow {
            lines.push((SLOW_TEXT.to_string(), ARTIST_FONT, theme::palette(ui.ctx()).warn, cy + 24.0));
        } else if !np.album.is_empty() {
            lines.push((np.album.clone(), ARTIST_FONT, st.dim, cy + 24.0));
        }
        for (i, (text, size, color, base)) in lines.iter().enumerate() {
            let galley = galley_truncated(&painter, text, theme::regular(*size), *color, text_w);
            let rect = text_on_baseline(&painter, pos2(text_x, *base), galley, *color);
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

    /// Derecha de la barra: corazón, añadir a playlist, letra, dispositivos y más; separador,
    /// Jam y cola. Cada cosa a su distancia del borde derecho (`bar_layout`).
    fn bar_actions(&mut self, ui: &mut egui::Ui, xr: f32, cy: f32, lay: &BarLayout, st: &BarStyle) {
        let boxed = |from_right: f32, dy: f32| Rect::from_min_size(pos2(xr - from_right, cy - 14.0 + dy), vec2(BOX, BOX));
        let np = self.player.now.clone();
        if let Some(np) = &np {
            if let (Some(id), Some(x)) = (&np.id, lay.heart) {
                let liked = self.player.liked.unwrap_or(false);
                let (icon, color) = if liked { (Icon::HeartFilled, GREEN) } else { (Icon::Heart, st.icon) };
                let tip = if liked { "Quitar de Me gusta" } else { "Guardar en Me gusta" };
                if self.bar_icon(ui, boxed(x, 1.0), icon, color, tip).clicked() {
                    self.actions.push(Action::Like(id.clone(), !liked));
                }
            }
            if let Some(x) = lay.add {
                if self.bar_icon(ui, boxed(x, 0.0), Icon::PlusSquare, st.icon, "Añadir a playlist").clicked() {
                    self.open_add_dialog(vec![np.uri.clone()]);
                }
            }
        }
        if let Some(x) = lay.lyrics {
            let color = if self.side == Some(SideTab::Lyrics) { GREEN } else { st.icon };
            if self.bar_icon(ui, boxed(x, 0.0), Icon::Lyrics, color, "Letra (L)").clicked() {
                self.toggle_side(SideTab::Lyrics);
            }
        }
        let dev_color = if self.player.remote.is_some() { theme::BLUE } else { st.icon };
        let dev_tip = match &self.player.remote {
            Some(d) => format!("Sonando en {}", d.name),
            None => "Dispositivos".to_string(),
        };
        if lay.devices != lay.more {
            let r = self.bar_icon(ui, boxed(lay.devices, 0.0), Icon::Devices, dev_color, &dev_tip);
            if r.clicked() {
                self.api.send(Req::Devices);
            }
            egui::Popup::menu(&r).width(280.0).show(|ui| self.devices_menu(ui));
        }
        let more = self.bar_icon(ui, boxed(lay.more, 0.0), Icon::More, st.icon, "Más");
        egui::Popup::menu(&more).show(|ui| self.player_more_menu(ui));
        if let Some(x) = lay.separator {
            let sx = xr - x;
            ui.painter().line_segment([pos2(sx, cy - 19.0), pos2(sx, cy + 18.0)], egui::Stroke::new(1.3, st.separator));
        }
        if let Some(x) = lay.jam {
            self.jam_orb(ui, pos2(xr - x, cy - 1.5));
        }
        let q_color = if self.side == Some(SideTab::Queue) { GREEN } else { st.icon };
        if self.bar_icon(ui, boxed(lay.queue, 0.0), Icon::Queue, q_color, "Cola (Q)").clicked() {
            self.toggle_side(SideTab::Queue);
        }
    }

    /// Botón de Jam: un orbe azul con un aro cian irregular. Abre la ventana de Jam.
    fn jam_orb(&mut self, ui: &mut egui::Ui, c: egui::Pos2) {
        let rect = Rect::from_center_size(c, vec2(36.0, 36.0));
        let resp = ui.interact(rect, ui.id().with("jam_orb"), Sense::click()).on_hover_text("Jam (Ctrl+J)");
        let hover = resp.hovered();
        if hover {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let painter = ui.painter();
        let blue = Color32::from_rgb(9, 90, 180);
        let blue = if hover { blue.lerp_to_gamma(Color32::WHITE, 0.1) } else { blue };
        // Borde difuminado como en la referencia: sólido hasta el radio 15 y de ahí al 19 con
        // opacidad 0,89 → 0,65 → 0,32 → 0,09 (cada corona suma su alfa a las de fuera).
        for (r, a) in [(19.0, 0.09), (18.0, 0.253), (17.0, 0.485), (16.0, 0.686)] {
            painter.circle_filled(c, r, blue.gamma_multiply(a));
        }
        painter.circle_filled(c, 15.0, blue);
        // Anillo cian irregular, con un bulto arriba a la derecha y un halo azul oscuro.
        let ring: Vec<egui::Pos2> = (0..96)
            .map(|i| {
                let a = std::f32::consts::TAU * i as f32 / 96.0;
                let bump = {
                    let d = (a - 5.55 + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
                    1.5 * (-(d * d) / 0.1).exp()
                };
                let r = 7.9 + 0.8 * (2.0 * a + 0.6).sin() + 0.35 * (3.0 * a + 2.1).sin() + bump;
                pos2(c.x - 0.6 + r * a.cos(), c.y + 0.9 + r * a.sin())
            })
            .collect();
        painter.add(egui::Shape::closed_line(ring.clone(), egui::Stroke::new(3.6, Color32::from_rgb(0, 72, 150))));
        painter.add(egui::Shape::closed_line(ring, egui::Stroke::new(1.9, Color32::from_rgb(84, 208, 232))));
        if resp.clicked() {
            self.jam_open = !self.jam_open;
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
            let icon = if is_self { Icon::Headphones } else { Icon::Devices };
            let label = if d.is_active { format!("{name} · sonando") } else { name };
            if Self::menu_item(ui, Some(icon), &label, false).clicked() {
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
            if Self::menu_item(ui, Some(Icon::Headphones), &text, false).clicked() {
                self.select_device(me, true);
                ui.close();
            }
        }
        ui.separator();
        if Self::menu_item(ui, Some(Icon::Refresh), "Actualizar lista", false).clicked() {
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
            // Justo encima de la barra del reproductor y centrado.
            .anchor(egui::Align2::CENTER_BOTTOM, vec2(0.0, -(super::PLAYER_PANEL_H + 8.0)))
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

// ------------------------------------------------------------ medidas del reproductor

/// Caja de los iconos del reproductor (su rejilla: cada unidad, un píxel).
const BOX: f32 = 28.0;
const BAR_RADIUS: u8 = 10;
const COVER: f32 = 51.0;
const COVER_RADIUS: u8 = 4;
const TITLE_FONT: f32 = 15.0;
const ARTIST_FONT: f32 = 14.0;
const TIME_FONT: f32 = 14.0;
/// Grosor de las líneas de progreso y de volumen.
const PROGRESS_H: f32 = 3.0;
/// Progreso más corto que se acepta antes de quitarle sitio al texto de la canción.
const PROG_MIN: f32 = 160.0;
/// Texto de la canción: el ancho de la referencia y el mínimo antes de pasar a una barra más
/// sencilla (sin Jam; después sin letra ni playlist).
const TEXT_MAX: f32 = 357.0;
const TEXT_MIN: f32 = 180.0;

/// Colores del reproductor: en tema oscuro, los de la referencia de diseño; en claro, los de la
/// paleta.
struct BarStyle {
    bg: Color32,
    icon: Color32,
    title: Color32,
    dim: Color32,
    track: Color32,
    fill: Color32,
    shadow: Color32,
    separator: Color32,
    play_ring: Color32,
    loading: Color32,
}

impl BarStyle {
    fn new(ui: &egui::Ui) -> Self {
        let p = theme::palette(ui.ctx());
        if ui.visuals().dark_mode {
            BarStyle {
                bg: Color32::from_rgb(50, 56, 66),
                icon: Color32::from_rgb(136, 141, 147),
                title: Color32::from_rgb(212, 218, 228),
                dim: Color32::from_rgb(126, 132, 142),
                track: Color32::from_rgb(70, 76, 86),
                fill: Color32::from_rgb(220, 224, 231),
                shadow: Color32::from_rgb(34, 40, 50),
                separator: Color32::from_rgb(70, 76, 86),
                play_ring: Color32::from_rgb(2, 44, 20),
                loading: Color32::from_rgb(70, 76, 86),
            }
        } else {
            BarStyle {
                bg: p.card2,
                icon: p.weak,
                title: p.text,
                dim: p.weak,
                track: p.weak.gamma_multiply(0.35),
                fill: p.text,
                shadow: Color32::TRANSPARENT,
                separator: p.border,
                play_ring: GREEN.gamma_multiply(0.5),
                loading: p.card,
            }
        }
    }
}

/// Dónde va cada cosa en una barra de ancho `w`. A la izquierda, desde el borde: el progreso
/// (`prog_x`, `prog_w`). A la derecha, la distancia desde el borde derecho a la esquina
/// izquierda de cada caja de 28 px (o al centro del separador y del orbe); `None`: no cabe.
/// `right_w`: lo que ocupa la derecha.
#[derive(Clone, Copy, Debug, PartialEq)]
struct BarLayout {
    shuffle_repeat: bool,
    /// Duración a la derecha del progreso y altavoz (sin ellos, la portada va pegada al progreso).
    tail: bool,
    /// Portada y texto de la canción (sin ellos, el progreso llega hasta lo de la derecha).
    cover: bool,
    prog_x: f32,
    prog_w: f32,
    heart: Option<f32>,
    add: Option<f32>,
    lyrics: Option<f32>,
    devices: f32,
    more: f32,
    separator: Option<f32>,
    jam: Option<f32>,
    queue: f32,
    right_w: f32,
}

impl BarLayout {
    /// Desde el final del progreso hasta la portada: duración y altavoz (o solo un margen).
    fn to_cover(&self) -> f32 {
        if self.tail {
            114.0
        } else {
            12.0
        }
    }

    /// Desde el final del progreso hasta el texto (la portada y su margen).
    fn after_progress(&self) -> f32 {
        if self.cover {
            self.to_cover() + 60.0
        } else {
            0.0
        }
    }

    fn cover_x(&self) -> f32 {
        self.prog_x + self.prog_w + self.to_cover()
    }

    fn text_x(&self) -> f32 {
        self.cover_x() + 60.0
    }
}

/// Reparte el ancho `w` de la barra. Con la referencia (1491 px) sale exactamente lo de la
/// referencia; más ancha, solo crece el progreso; más estrecha, se quitan primero el orbe de Jam
/// y el separador, después la letra y añadir a playlist y, al final, aleatorio y repetir.
fn bar_layout(w: f32) -> BarLayout {
    let full = BarLayout {
        shuffle_repeat: true,
        tail: true,
        cover: true,
        prog_x: 268.0,
        prog_w: 0.0,
        heart: Some(360.0),
        add: Some(315.0),
        lyrics: Some(268.0),
        devices: 222.0,
        more: 176.0,
        separator: Some(127.5),
        jam: Some(88.5),
        queue: 52.0,
        right_w: 360.0,
    };
    let no_jam = BarLayout { heart: Some(282.0), add: Some(236.0), lyrics: Some(190.0), devices: 144.0, more: 98.0, separator: None, jam: None, right_w: 282.0, ..full };
    let compact = BarLayout { heart: Some(190.0), add: None, lyrics: None, right_w: 190.0, ..no_jam };
    let tiny = BarLayout { shuffle_repeat: false, prog_x: 182.0, ..compact };
    // La más estrecha (ventana mínima con la interfaz ampliada): sin duración a la derecha ni
    // altavoz, y a la derecha solo más y cola (el corazón y la letra siguen en «Más» y con L).
    let micro = BarLayout { tail: false, heart: None, devices: 98.0, more: 98.0, queue: 52.0, right_w: 98.0, ..tiny };
    let avail = |l: &BarLayout| w - l.prog_x - l.after_progress() - l.right_w - 12.0;
    for l in [full, no_jam, compact] {
        let a = avail(&l);
        if a >= PROG_MIN + TEXT_MIN {
            let text = (a - PROG_MIN).min(TEXT_MAX);
            return BarLayout { prog_w: a - text, ..l };
        }
    }
    let a = avail(&tiny);
    if a >= 60.0 + 120.0 {
        return BarLayout { prog_w: (a * 0.4).max(60.0), ..tiny };
    }
    let a = avail(&micro);
    if a >= 60.0 + 60.0 {
        return BarLayout { prog_w: (a * 0.45).max(60.0), ..micro };
    }
    // Interfaz muy ampliada en la ventana mínima: sin portada ni texto (siguen en el aviso de
    // «Reproduciendo ahora» del sistema y en la cola).
    let nano = BarLayout { cover: false, ..micro };
    BarLayout { prog_w: avail(&nano).max(60.0), ..nano }
}

// ------------------------------------------------------------ píldora de actualización

/// Texto de la píldora de la barra superior cuando hay una versión nueva lista.
const UPDATE_PILL_TEXT: &str = "Reiniciar para actualizar";
/// Alto de la píldora (el ancho de solo el icono) y margen a cada lado del contenido.
const PILL_H: f32 = 32.0;
const PILL_PAD: f32 = 14.0;
/// Ancho de la zona de la derecha sin la píldora (ajustes y perfil).
const TOP_RIGHT_W: f32 = 110.0;
/// Lo que se deja como mínimo a Inicio y Buscar (hasta el final de «Buscar» en la referencia)
/// antes de encoger la píldora a solo el icono.
const TOP_TABS_MIN_W: f32 = 540.0;

// ------------------------------------------------------------ medidas de la barra superior y la lateral

/// Texto de la barra superior.
const TOP_FONT: f32 = 14.0;
/// Distancia entre las pestañas de páginas abiertas (de icono a icono), como de Inicio a Buscar.
const TAB_PITCH: f32 = 210.0;
/// Centro de la primera fila de la barra lateral desde su borde de arriba, alto de las filas,
/// lado de sus iconos y tamaño de su texto.
const SIDE_FIRST_CENTER: f32 = 30.0;
const SIDE_ROW_H: f32 = 51.5;
const SIDE_ICON: f32 = 28.0;
const SIDE_FONT: f32 = 14.5;
/// Filas de las playlists desplegadas: alto, texto y dónde empieza.
const SIDE_CHILD_H: f32 = 40.0;
const SIDE_CHILD_FONT: f32 = 14.0;
const SIDE_CHILD_TEXT_X: f32 = 80.0;

/// Lado y desplazamiento (x, y) de cada icono de la barra lateral para que su tinta caiga donde
/// en la referencia (sus dibujos no ocupan el cuadro igual).
fn side_icon_fit(icon: Icon) -> (f32, f32, f32) {
    match icon {
        Icon::Pin => (30.0, 0.0, 1.5),
        Icon::Heart => (SIDE_ICON, 0.5, 2.0),
        Icon::Album => (SIDE_ICON, -1.0, 0.5),
        Icon::Podcast => (30.0, 1.0, -0.5),
        _ => (SIDE_ICON, 0.0, 0.0),
    }
}

/// Colores de la barra superior y la lateral: iconos y texto grises, más claros al pasar el
/// ratón, y blancos en la página abierta.
struct TopStyle {
    icon: Color32,
    text: Color32,
    hover: Color32,
    active: Color32,
}

impl TopStyle {
    fn new(p: &theme::Palette) -> Self {
        if p.dark {
            Self { icon: Color32::from_gray(143), text: Color32::from_gray(130), hover: Color32::from_gray(205), active: p.text }
        } else {
            Self { icon: p.weak, text: p.weak, hover: p.text, active: p.text }
        }
    }

    /// Color del texto según esté en su página o bajo el ratón.
    fn color(&self, active: bool, hovered: bool) -> Color32 {
        if active {
            self.active
        } else if hovered {
            self.hover
        } else {
            self.text
        }
    }

    /// Color del icono que acompaña a un texto de `color` (en reposo el icono es algo más claro).
    fn icon_of(&self, color: Color32) -> Color32 {
        if color == self.text { self.icon } else { color }
    }
}

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

    /// Con el ancho de la referencia (1491 px) todo queda donde en la referencia.
    #[test]
    fn reproductor_como_la_referencia() {
        let l = bar_layout(1491.0);
        assert!(l.shuffle_repeat);
        assert_eq!((l.prog_x, l.prog_w), (268.0, 320.0));
        assert_eq!(l.text_x(), 762.0);
        assert_eq!((l.heart, l.add, l.lyrics), (Some(360.0), Some(315.0), Some(268.0)));
        assert_eq!((l.devices, l.more, l.separator, l.jam, l.queue), (222.0, 176.0, Some(127.5), Some(88.5), 52.0));
    }

    /// A cualquier ancho nada se monta: el texto acaba antes de lo de la derecha, el progreso
    /// nunca baja de 60 px y lo que se quita va en orden (Jam, después letra y playlist).
    #[test]
    fn reproductor_a_cualquier_ancho() {
        let mut prev: Option<BarLayout> = None;
        let widths: Vec<f32> = (360..=2600).step_by(10).map(|w| w as f32).collect();
        for &w in widths.iter().rev() {
            let l = bar_layout(w);
            assert!(l.prog_w >= 60.0, "{w}: {l:?}");
            if l.cover {
                let text_w = w - l.right_w - 12.0 - l.text_x();
                assert!(text_w >= 40.0, "{w}: texto {text_w}");
            } else {
                assert!(l.prog_x + l.prog_w <= w - l.right_w - 12.0 + 0.01, "{w}: {l:?}");
            }
            assert!(l.prog_w <= w, "{w}");
            if l.jam.is_some() {
                assert!(l.lyrics.is_some() && l.add.is_some() && l.separator.is_some(), "{w}");
            }
            if !l.tail {
                assert!(l.heart.is_none() && l.lyrics.is_none(), "{w}");
            }
            if !l.cover {
                assert!(!l.tail, "{w}");
            }
            if let Some(p) = prev {
                // Al estrechar, nada que se había quitado vuelve.
                assert!(!(!p.tail && l.tail), "{w}");
                assert!(!(!p.cover && l.cover), "{w}");
                assert!(!(p.jam.is_none() && l.jam.is_some()), "{w}");
                assert!(!(p.lyrics.is_none() && l.lyrics.is_some()), "{w}");
                assert!(!(!p.shuffle_repeat && l.shuffle_repeat), "{w}");
            }
            prev = Some(l);
        }
        // Más ancha que la referencia: solo crece el progreso.
        let big = bar_layout(1900.0);
        assert_eq!(big.prog_w, 320.0 + 409.0);
        assert_eq!(1900.0 - big.right_w - 12.0 - big.text_x(), TEXT_MAX);
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
        let (w, pill) = top_right_layout(1280.0, Some(160.0));
        let Some(UpdatePill::Full(pw)) = pill else { panic!("{pill:?}") };
        assert_eq!(w, TOP_RIGHT_W + 6.0 + pw);
        assert!(pw >= 160.0 + 18.0 + 2.0 * PILL_PAD);
        assert!(1280.0 - SIDEBAR_W - w >= TOP_TABS_MIN_W);
        // Interfaz al 200 % (560 puntos de ancho), la ventana mínima o una algo mayor: solo el
        // icono, sin montarse sobre Buscar.
        for full in [560.0, 760.0, 1000.0] {
            let (w, pill) = top_right_layout(full, Some(160.0));
            assert_eq!(pill, Some(UpdatePill::Icon), "{full}");
            assert_eq!(w, TOP_RIGHT_W + 6.0 + PILL_H);
        }
    }
}
