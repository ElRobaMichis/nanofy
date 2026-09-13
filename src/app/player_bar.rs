//! Barra superior (pestañas y búsqueda), biblioteca lateral y reproductor flotante.

use std::time::Duration;

use egui::{pos2, vec2, Align, Color32, CornerRadius, Label, Layout, Rect, RichText, Sense};

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

        // Derecha: ajustes y perfil
        let right_w = 110.0;
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
        }

        // Pestañas pegadas a la izquierda de la zona de contenido: Inicio, Buscar (que se
        // expande como campo solo al hacer clic) y una pestaña por página abierta.
        let center = Rect::from_min_max(pos2(left.max.x, full.min.y), pos2(right.min.x, full.max.y));
        let searching = page == Page::Search;
        let mut c = child_in(ui, center, Layout::left_to_right(Align::Center));
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

        // Fijados
        let pinned: Vec<Playlist> = self
            .playlists
            .iter()
            .filter(|pl| self.settings.pinned.contains(&pl.id))
            .cloned()
            .collect();
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
            for pl in &pinned {
                let sel = page == Page::Playlist(pl.id.clone());
                let r = Self::nav_item(ui, Some(Icon::Playlist), &pl.name, sel, 24.0);
                if r.clicked() {
                    self.go(Page::Playlist(pl.id.clone()));
                }
                self.playlist_context_menu(&r, pl);
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
            if self.logged_in() && !self.playlists_loaded {
                ui.horizontal(|ui| {
                    ui.add_space(44.0);
                    Self::loading(ui, "Cargando");
                });
            }
            let list: Vec<Playlist> = self.playlists.clone();
            let avail = (ui.available_height() - 9.0 * 36.0 - 60.0).max(80.0);
            egui::ScrollArea::vertical()
                .id_salt("sidebar_playlists")
                .auto_shrink([false, true])
                .max_height(avail)
                .show(ui, |ui| {
                    for pl in &list {
                        let sel = page == Page::Playlist(pl.id.clone());
                        let r = Self::nav_item(ui, Some(Icon::Playlist), &pl.name, sel, 24.0);
                        if r.clicked() {
                            self.go(Page::Playlist(pl.id.clone()));
                        }
                        self.playlist_context_menu(&r, pl);
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

        // Anchos fijos: transporte | tiempo | ONDA | tiempo | volumen | portada+texto | acciones
        let vol_zone = if w >= 900.0 { 100.0 } else { 0.0 };
        let transport_w = 38.0 + 4.0 * 30.0 + 5.0 * 4.0 + 8.0;
        let times_w = 2.0 * 42.0;
        let vol_w = 28.0 + vol_zone + 8.0;
        let actions_w = if w >= 900.0 { 6.0 * 32.0 + 24.0 + 16.0 } else { 4.0 * 32.0 + 24.0 };
        let text_w = (w * 0.20).clamp(140.0, 300.0);
        let now_w = 46.0 + 10.0 + text_w;
        let wave_w = (w - transport_w - times_w - vol_w - now_w - actions_w - 3.0 * 12.0).max(80.0);

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
        self.player_actions_widget(&mut a, actions_w);
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
        let lines: Vec<(String, egui::FontId, Color32, f32, f32)> = {
            let names = np.artists.iter().map(|a| a.0.as_str()).collect::<Vec<_>>().join(", ");
            let mut v = vec![
                (np.name.clone(), theme::regular(14.0), p.text, 0.0, 18.0),
                (names, theme::regular(12.0), p.weak, 17.0, 14.0),
            ];
            if show_album && !np.album.is_empty() {
                v.push((np.album.clone(), theme::regular(12.0), p.faint, 32.0, 14.0));
            }
            v
        };
        for (i, (text, font, color, y, h)) in lines.iter().enumerate() {
            let galley = galley_truncated(&painter, text, font.clone(), *color, text_w);
            let rect = Rect::from_min_size(pos2(col.min.x, col.min.y + y), vec2(galley.size().x.min(text_w), *h));
            painter.galley(pos2(rect.min.x, rect.center().y - galley.size().y / 2.0), galley, *color);
            if i == 0 {
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

    /// De izquierda a derecha: corazón, playlist, letra, dispositivos, más | cola.
    fn player_actions_widget(&mut self, ui: &mut egui::Ui, width: f32) {
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
