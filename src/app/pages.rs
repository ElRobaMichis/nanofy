//! Páginas: inicio, biblioteca, historial, búsqueda, playlist, álbum, artista, perfil y ajustes.

use std::time::{Duration, Instant};

use egui::{pos2, vec2, Align, Button, Color32, CornerRadius, Label, Layout, Rect, RichText, Sense};

use super::icons::{self, Icon};
use super::theme::{self, GREEN};
use super::widgets::{child_in, uri_to_link, CardInfo, CardKind, RowOpts, Source, CARD_W};
use super::{fmt_thousands, strip_html, Action, App, Auth, FolderDialog, Page, PlayTarget, ERROR_RED, LIKED};
use crate::api::Req;
use crate::config::{Quality, Theme};
use crate::model::*;

/// Ancho de la columna derecha de información en playlists y álbumes.
const INFO_W: f32 = 300.0;

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
            }
            ui.add_space(16.0);
            ui.label(
                RichText::new(
                    "El inicio de sesión se hace en la web de Spotify (OAuth con PKCE); \
                     Nanofy nunca ve tu contraseña. Para reproducir música hace falta Premium.",
                )
                .small()
                .color(p.weak),
            );
        });
    }

    // ------------------------------------------------------------ utilidades

    pub(super) fn playlist_card(&mut self, ui: &mut egui::Ui, pl: &Playlist) -> egui::Response {
        let owner = format!("De {}", pl.owner_name());
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

    fn liked_card(&mut self, ui: &mut egui::Ui) {
        let total = self.lists.get(LIKED).map(|l| l.total).unwrap_or(0);
        let preview: Vec<String> = self
            .lists
            .get(LIKED)
            .map(|l| l.tracks.iter().take(3).map(|t| t.name.clone()).collect())
            .unwrap_or_default();
        let sub = if preview.is_empty() { "Tus canciones guardadas".to_string() } else { preview.join(", ") };
        let r = self.card(
            ui,
            CardInfo {
                kind: CardKind::Liked,
                cover: None,
                title: "Canciones que te gustan",
                subtitle: &sub,
                count: (total > 0).then_some(total),
                pinned: false,
            },
        );
        if r.clicked() {
            self.actions.push(Action::Go(Page::Liked));
        }
        r.context_menu(|ui| {
            if Self::menu_item(ui, Some(Icon::NewTab), "Abrir en una nueva pestaña", false).clicked() {
                self.actions.push(Action::OpenInTab(Page::Liked));
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
        ui.add(Label::new(RichText::new(title).font(theme::bold(30.0))).truncate());
        ui.add_space(2.0);
        ui.add(Label::new(RichText::new(meta).small().color(p.weak)).truncate());
        ui.add_space(10.0);
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
        let left_w = width - INFO_W - 24.0;
        let left_rect = Rect::from_min_size(top, vec2(left_w, f32::INFINITY));
        let right_rect = Rect::from_min_size(pos2(top.x + left_w + 24.0, top.y), vec2(INFO_W, f32::INFINITY));
        let mut l = child_in(ui, left_rect, Layout::top_down(Align::Min));
        l.set_width(left_w);
        left(self, &mut l);
        let mut r = child_in(ui, right_rect, Layout::top_down(Align::Min));
        r.set_width(INFO_W);
        right(self, &mut r);
        let h = l.min_rect().height().max(r.min_rect().height());
        ui.allocate_rect(Rect::from_min_size(top, vec2(width, h)), Sense::hover());
    }

    /// Tarjeta de información: portada grande, chips y artistas con avatar.
    fn info_card(&mut self, ui: &mut egui::Ui, cover: Option<&str>, chips: &[String], artists: &[ArtistRef]) {
        let p = theme::palette(ui.ctx());
        egui::Frame::new()
            .fill(p.card2)
            .corner_radius(CornerRadius::same(14))
            .inner_margin(14)
            .show(ui, |ui| {
                ui.set_width(INFO_W - 28.0);
                let side = INFO_W - 28.0;
                let (rect, _) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
                self.cover_in(ui, cover, rect, 10);
                if cover.is_none() {
                    icons::paint(ui.painter(), rect.shrink(side * 0.3), p.faint, Icon::Album);
                }
                if !chips.is_empty() {
                    ui.add_space(12.0);
                    ui.horizontal_wrapped(|ui| {
                        for c in chips {
                            let _ = Self::pill(ui, c, false);
                        }
                    });
                }
                if !artists.is_empty() {
                    ui.add_space(12.0);
                    for a in artists.iter().take(24) {
                        let img = a
                            .id
                            .as_ref()
                            .and_then(|id| self.artists.get(id))
                            .and_then(|pg| pg.artist.as_ref())
                            .and_then(|ar| ar.cover(64).map(|s| s.to_string()));
                        let r = ui
                            .horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 10.0;
                                let (rect, _) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::hover());
                                match &img {
                                    Some(u) => self.cover_in(ui, Some(u), rect, 18),
                                    None => {
                                        ui.painter().circle_filled(rect.center(), 18.0, p.hover);
                                        let initial = a.name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
                                        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, initial, theme::regular(14.0), p.text);
                                    }
                                }
                                ui.add(Label::new(RichText::new(&a.name).color(p.text)).truncate());
                            })
                            .response
                            .interact(Sense::click());
                        if r.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if r.clicked() {
                            if let Some(id) = &a.id {
                                self.actions.push(Action::Go(Page::Artist(id.clone())));
                            }
                        }
                        // Pide la imagen del artista una sola vez (metadatos ligeros).
                        if let Some(id) = &a.id {
                            if img.is_none() {
                                self.request_once(&format!("artistmeta:{id}"), Req::Artist(id.clone()));
                            }
                        }
                    }
                }
            });
    }

    /// Artistas más frecuentes de una lista de pistas.
    fn top_artists(tracks: &[Track], n: usize) -> Vec<ArtistRef> {
        let mut counts: Vec<(ArtistRef, usize)> = Vec::new();
        for t in tracks {
            for a in &t.artists {
                if let Some(e) = counts.iter_mut().find(|(x, _)| x.id == a.id && x.name == a.name) {
                    e.1 += 1;
                } else {
                    counts.push((a.clone(), 1));
                }
            }
        }
        counts.sort_by(|x, y| y.1.cmp(&x.1));
        counts.into_iter().take(n).map(|(a, _)| a).collect()
    }

    // ------------------------------------------------------------------ inicio

    fn home_page(&mut self, ui: &mut egui::Ui) {
        if !self.signed_in() {
            self.welcome(ui);
            return;
        }
        let p = theme::palette(ui.ctx());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for (i, label) in ["Todo", "Música", "Podcasts", "Audiolibros"].iter().enumerate() {
                if Self::pill(ui, label, self.home_filter == i as u8).clicked() {
                    self.home_filter = i as u8;
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let b = icons::button(ui, Icon::Sliders, 30.0, p.weak).on_hover_text("Personalizar el inicio");
                let pid = egui::Id::new("home_customize");
                if self.home_customize_once {
                    self.home_customize_once = false;
                    egui::Popup::open_id(ui.ctx(), pid);
                }
                egui::Popup::menu(&b).id(pid).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| self.home_customize_panel(ui));
            });
        });
        if !self.api.web_configured() {
            self.web_api_banner(ui);
        }
        if self.home_feed.is_empty() {
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
            ui.add_space(12.0);
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
            // Misma clave que la página de la playlist: una sola carga por playlist.
            self.request_once(&format!("pl:{pid}"), Req::PlaylistTracks(pid.clone()));
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

    /// Panel «Personalizar inicio»: añadir playlists, fijar, reordenar arrastrando y ocultar.
    fn home_customize_panel(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        ui.set_min_width(340.0);
        ui.set_max_width(340.0);
        ui.label(RichText::new("Personalizar inicio").font(theme::bold(16.0)));
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);
        let mine: Vec<(String, String)> = self
            .playlists
            .iter()
            .filter(|pl| !self.settings.home_custom.contains(&pl.id))
            .map(|pl| (pl.id.clone(), pl.name.clone()))
            .collect();
        let r = Self::menu_item(ui, Some(Icon::Plus), "Seleccionar de la biblioteca", true);
        egui::containers::menu::SubMenu::default().show(ui, &r, |ui| {
            ui.set_min_width(220.0);
            if mine.is_empty() {
                ui.label(RichText::new("Todas tus playlists ya están en el inicio.").small().color(p.weak));
            }
            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                for (pid, name) in &mine {
                    if Self::menu_item(ui, Some(Icon::Playlist), name, false).clicked() {
                        self.settings.home_custom.push(pid.clone());
                        self.settings.save(&self.paths);
                        ui.close();
                    }
                }
            });
        });
        ui.add_space(4.0);
        let sections = self.home_sections();
        let mut rows: Vec<(String, Rect)> = Vec::new();
        egui::ScrollArea::vertical().max_height(440.0).show(ui, |ui| {
            for sec in &sections {
                let pinned = self.settings.home_pinned.contains(&sec.id);
                let hidden = self.settings.home_hidden.contains(&sec.id);
                let dragging = self.home_drag.as_deref() == Some(sec.id.as_str());
                let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
                if dragging {
                    ui.painter().rect_filled(row, CornerRadius::same(8), p.hover);
                }
                rows.push((sec.id.clone(), row));
                let mut c = child_in(ui, row, Layout::left_to_right(Align::Center));
                c.spacing_mut().item_spacing.x = 8.0;
                let (icon, color) = if pinned { (Icon::PinFilled, GREEN) } else { (Icon::Pin, p.weak) };
                if icons::button(&mut c, icon, 26.0, color).on_hover_text(if pinned { "Desfijar" } else { "Fijar arriba" }).clicked() {
                    self.settings.home_pinned.retain(|x| x != &sec.id);
                    if !pinned {
                        self.settings.home_pinned.insert(0, sec.id.clone());
                    }
                    self.settings.save(&self.paths);
                }
                let text_color = if hidden { p.faint } else { p.text };
                let title_w = row.width() - 26.0 * 3.0 - 8.0 * 4.0;
                let (tr, _) = c.allocate_exact_size(vec2(title_w, 26.0), Sense::hover());
                let mut t = child_in(&mut c, tr, Layout::left_to_right(Align::Center));
                t.add(Label::new(RichText::new(&sec.title).color(text_color)).truncate());
                let (hr, _) = c.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
                let h = c.interact(hr, c.id().with(("drag", &sec.id)), Sense::drag());
                icons::paint(c.painter(), hr.shrink(5.0), if h.hovered() || dragging { p.text } else { p.weak }, Icon::DragHandle);
                if h.hovered() || dragging {
                    c.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                }
                if h.drag_started() {
                    self.home_drag = Some(sec.id.clone());
                }
                let (icon, color) = if hidden { (Icon::EyeOff, p.faint) } else { (Icon::Eye, p.weak) };
                if icons::button(&mut c, icon, 26.0, color).on_hover_text(if hidden { "Mostrar" } else { "Ocultar" }).clicked() {
                    if hidden {
                        self.settings.home_hidden.retain(|x| x != &sec.id);
                    } else {
                        self.settings.home_hidden.push(sec.id.clone());
                    }
                    self.settings.save(&self.paths);
                }
                if sec.id.starts_with("custom:") {
                    let r = c.add(Label::new(RichText::new("×").color(p.weak)).sense(Sense::click())).on_hover_text("Quitar del inicio");
                    if r.clicked() {
                        let pid = sec.id.trim_start_matches("custom:").to_string();
                        self.settings.home_custom.retain(|x| x != &pid);
                        self.settings.save(&self.paths);
                    }
                }
            }
        });
        // Arrastre: la sección arrastrada toma el sitio de la fila bajo el puntero.
        if let Some(d) = self.home_drag.clone() {
            let released = ui.input(|i| i.pointer.any_released());
            if let Some(pos) = ui.input(|i| i.pointer.latest_pos()) {
                if let Some((target, _)) = rows.iter().find(|(id, r)| id != &d && r.y_range().contains(pos.y)) {
                    let order = &mut self.settings.home_order;
                    if let (Some(from), Some(to)) = (order.iter().position(|x| x == &d), order.iter().position(|x| x == target)) {
                        let item = order.remove(from);
                        order.insert(to, item);
                        self.settings.save(&self.paths);
                    }
                }
            }
            if released {
                self.home_drag = None;
            }
            ui.ctx().request_repaint();
        }
        ui.add_space(6.0);
        ui.separator();
        ui.add_space(4.0);
        let mut recs = self.settings.home_recs;
        if Self::toggle(ui, &mut recs, "Permitir filas recomendadas") {
            self.settings.home_recs = recs;
            self.settings.save(&self.paths);
        }
    }

    /// Estantería del inicio: título, flechas para desplazar, menú (fijar / ocultar) y tarjetas.
    fn home_shelf(&mut self, ui: &mut egui::Ui, sec: &HomeSection, pinned: bool) {
        let p = theme::palette(ui.ctx());
        ui.add_space(14.0);
        let (offset, max) = self.home_offsets.get(&sec.id).copied().unwrap_or((0.0, 0.0));
        let page = (ui.available_width() - CARD_W).max(CARD_W);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if pinned {
                let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                icons::paint(ui.painter(), r, GREEN, Icon::PinFilled);
            }
            ui.label(RichText::new(&sec.title).font(theme::bold(20.0)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let more = icons::button(ui, Icon::More, 28.0, p.weak).on_hover_text("Opciones de la sección");
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
                let can_next = offset + 1.0 < max;
                let can_prev = offset > 1.0;
                if icons::button(ui, Icon::Forward, 28.0, if can_next { p.text } else { p.faint }).clicked() && can_next {
                    self.home_scroll.insert(sec.id.clone(), (offset + page).min(max));
                }
                if icons::button(ui, Icon::Back, 28.0, if can_prev { p.text } else { p.faint }).clicked() && can_prev {
                    self.home_scroll.insert(sec.id.clone(), (offset - page).max(0.0));
                }
            });
        });
        ui.add_space(4.0);
        let mut area = egui::ScrollArea::horizontal().id_salt(&sec.id).auto_shrink([false, true]);
        if let Some(t) = self.home_scroll.remove(&sec.id) {
            area = area.horizontal_scroll_offset(t);
        }
        let out = area.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for item in &sec.items {
                    self.home_card(ui, item);
                }
            });
        });
        let max_off = (out.content_size.x - out.inner_rect.width()).max(0.0);
        self.home_offsets.insert(sec.id.clone(), (out.state.offset.x, max_off));
    }

    fn home_card(&mut self, ui: &mut egui::Ui, item: &HomeItem) {
        let kind = item.kind();
        if kind == "track" {
            let sub = item.subtitle.clone();
            let r = self.card(ui, CardInfo { kind: CardKind::Album, cover: item.image.as_deref(), title: &item.title, subtitle: &sub, count: None, pinned: false });
            let (uri, ctx) = (item.uri.clone(), item.context.clone());
            if r.clicked() {
                let shuffle = self.player.shuffle;
                self.actions.push(Action::Play(match ctx.clone() {
                    Some(c) => PlayTarget::Context { uri: c, track_uri: Some(uri.clone()), index: None, shuffle },
                    None => PlayTarget::Tracks { uris: vec![uri.clone()], index: Some(0), shuffle },
                }));
            }
            r.context_menu(|ui| {
                if Self::menu_item(ui, Some(Icon::Queue), "Añadir a la cola", false).clicked() {
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
        let r = self.card(
            ui,
            CardInfo { kind: card_kind, cover: item.image.as_deref(), title: &item.title, subtitle: &subtitle, count: None, pinned: false },
        );
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

    fn library_page(&mut self, ui: &mut egui::Ui) {
        if !self.signed_in() {
            self.welcome(ui);
            return;
        }
        let p = theme::palette(ui.ctx());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let grid = self.library_grid;
            if icons::button(ui, Icon::List, 32.0, if !grid { p.text } else { p.weak }).on_hover_text("Lista").clicked() {
                self.library_grid = false;
                self.settings.library_grid = false;
            }
            if icons::button(ui, Icon::Grid, 32.0, if grid { p.text } else { p.weak }).on_hover_text("Cuadrícula").clicked() {
                self.library_grid = true;
                self.settings.library_grid = true;
            }
            ui.add_space(8.0);
            if Self::pill(ui, "Recientes", !self.library_sort_name).clicked() {
                self.library_sort_name = false;
            }
            if Self::pill(ui, "Nombre", self.library_sort_name).clicked() {
                self.library_sort_name = true;
            }
            ui.add_space(8.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.library_filter)
                    .hint_text("Filtrar tu biblioteca")
                    .desired_width(220.0),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icons::button(ui, Icon::Plus, 32.0, p.weak).on_hover_text("Nueva playlist (Ctrl+N)").clicked() {
                    self.actions.push(Action::OpenEditor(None));
                }
            });
        });
        ui.add_space(10.0);
        self.request_once("albums", Req::SavedAlbums);
        self.request_once("artists", Req::FollowedArtists);

        let filter = self.library_filter.trim().to_lowercase();
        let matches = |s: &str| filter.is_empty() || s.to_lowercase().contains(&filter);
        let mut pls: Vec<Playlist> = self.playlists.iter().filter(|pl| matches(&pl.name)).cloned().collect();
        let pinned = self.settings.pinned.clone();
        if self.library_sort_name {
            pls.sort_by_key(|pl| pl.name.to_lowercase());
        }
        pls.sort_by_key(|pl| !pinned.contains(&pl.id));
        let mut albums: Vec<Album> = self.saved_albums.iter().filter(|a| matches(&a.name) || matches(&a.artists_str())).cloned().collect();
        if self.library_sort_name {
            albums.sort_by_key(|a| a.name.to_lowercase());
        }
        let mut artists: Vec<Artist> = self.followed_artists.iter().filter(|a| matches(&a.name)).cloned().collect();
        if self.library_sort_name {
            artists.sort_by_key(|a| a.name.to_lowercase());
        }

        if self.library_grid {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 8.0);
                if matches("canciones que te gustan") {
                    self.liked_card(ui);
                }
                for pl in &pls {
                    self.playlist_card(ui, pl);
                }
                for a in &albums {
                    let sub = a.artists_str();
                    self.album_card(ui, &a.id.clone(), a.cover(300), &a.name, &sub, a.total_tracks);
                }
                for a in &artists {
                    self.artist_card(ui, a);
                }
            });
        } else {
            let liked_total = self.lists.get(LIKED).map(|l| l.total).unwrap_or(0);
            if matches("canciones que te gustan") {
                let r = self.list_entry(ui, None, CardKind::Liked, "Canciones que te gustan", &format!("{liked_total} canciones"));
                if r.clicked() {
                    self.actions.push(Action::Go(Page::Liked));
                }
            }
            for pl in &pls {
                let sub = format!("Playlist · {}", pl.owner_name());
                let r = self.list_entry(ui, pl.cover(64), CardKind::Playlist, &pl.name, &sub);
                if r.clicked() {
                    self.actions.push(Action::OpenPlaylist(pl.clone()));
                }
                self.playlist_row_menu(&r, pl);
            }
            for a in &albums {
                let sub = format!("Álbum · {}", a.artists_str());
                let r = self.list_entry(ui, a.cover(64), CardKind::Album, &a.name, &sub);
                if r.clicked() {
                    self.actions.push(Action::Go(Page::Album(a.id.clone())));
                }
            }
            for a in &artists {
                let r = self.list_entry(ui, a.cover(64), CardKind::Artist, &a.name, "Artista");
                if r.clicked() {
                    self.actions.push(Action::Go(Page::Artist(a.id.clone())));
                }
            }
        }
    }

    fn playlist_row_menu(&mut self, r: &egui::Response, pl: &Playlist) {
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
    fn list_entry(&mut self, ui: &mut egui::Ui, cover: Option<&str>, kind: CardKind, title: &str, subtitle: &str) -> egui::Response {
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
    fn history_list(&mut self) -> Vec<Track> {
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
                out.push(e.track.clone());
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
        self.collection_bar(ui, "history", &recent, None, None, |_, _| {}, None);
        ui.add_space(8.0);
        let shown = self.filter_tracks("history", &recent);
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
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            for (i, label) in FILTERS.iter().enumerate() {
                if Self::pill(ui, label, self.search_filter == i as u8).clicked() {
                    self.search_filter = i as u8;
                    if i == 8 && !self.search_query.trim().is_empty() {
                        self.api.send(Req::User(self.search_query.trim().to_string()));
                    }
                }
            }
        });
        ui.add_space(6.0);
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
        if self.search_loading {
            Self::loading(ui, "Buscando");
            return;
        }
        let Some(result) = self.search_result.take() else {
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
                if Self::menu_item(ui, Some(Icon::Queue), "Añadir a la cola", false).clicked() {
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
        let total_ms: u64 = list.tracks.iter().map(|t| t.duration_ms as u64).sum();
        let meta = format!("{} canciones · {}", list.total, fmt_total(total_ms));
        let uris: Vec<String> = list.tracks.iter().map(|t| t.uri.clone()).collect();
        let top = Self::top_artists(&list.tracks, 5);
        let tracks = list.tracks;
        let loading = list.loading;
        let total = list.total;

        self.two_columns(
            ui,
            |app, ui| {
                Self::page_title(ui, "PLAYLIST", "Canciones que te gustan", &meta);
                if !uris.is_empty() {
                    app.collection_bar(ui, LIKED, &tracks, None, None, |_, _| {}, None);
                    ui.add_space(8.0);
                }
                if loading {
                    Self::loading(ui, &format!("Cargando {} de {}", tracks.len(), total));
                }
                let shown = app.filter_tracks(LIKED, &tracks);
                app.track_rows(ui, LIKED, &shown, RowOpts { header: true, select: true, ..RowOpts::tracks(true, true) });
            },
            |app, ui| {
                let p = theme::palette(ui.ctx());
                egui::Frame::new().fill(p.card2).corner_radius(CornerRadius::same(14)).inner_margin(14).show(ui, |ui| {
                    ui.set_width(INFO_W - 28.0);
                    let side = INFO_W - 28.0;
                    let (rect, _) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
                    ui.painter().rect_filled(rect, CornerRadius::same(10), theme::GREEN_DARK);
                    icons::paint(ui.painter(), rect.shrink(side * 0.28), GREEN, Icon::HeartFilled);
                    ui.add_space(12.0);
                    ui.horizontal_wrapped(|ui| {
                        let _ = Self::pill(ui, &format!("{} canciones", total), false);
                        let _ = Self::pill(ui, &fmt_total(total_ms), false);
                    });
                    if !top.is_empty() {
                        ui.add_space(12.0);
                        ui.label(RichText::new("Artistas más presentes").small().color(p.weak));
                        ui.add_space(4.0);
                        app.info_card_artists(ui, &top);
                    }
                });
            },
        );
        self.lists.insert(LIKED.to_string(), crate::app::TrackList { tracks, total, loading });
    }

    /// Lista de artistas con avatar (parte de la tarjeta de información).
    fn info_card_artists(&mut self, ui: &mut egui::Ui, artists: &[ArtistRef]) {
        let p = theme::palette(ui.ctx());
        for a in artists.iter().take(6) {
            let img = a
                .id
                .as_ref()
                .and_then(|id| self.artists.get(id))
                .and_then(|pg| pg.artist.as_ref())
                .and_then(|ar| ar.cover(64).map(|s| s.to_string()));
            let r = ui
                .horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    let (rect, _) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::hover());
                    match &img {
                        Some(u) => self.cover_in(ui, Some(u), rect, 18),
                        None => {
                            ui.painter().circle_filled(rect.center(), 18.0, p.hover);
                            let initial = a.name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
                            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, initial, theme::regular(14.0), p.text);
                        }
                    }
                    ui.add(Label::new(RichText::new(&a.name).color(p.text)).truncate());
                })
                .response
                .interact(Sense::click());
            if r.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if r.clicked() {
                if let Some(id) = &a.id {
                    self.actions.push(Action::Go(Page::Artist(id.clone())));
                }
            }
            if let Some(id) = &a.id {
                if img.is_none() {
                    self.request_once(&format!("artistmeta:{id}"), Req::Artist(id.clone()));
                }
            }
        }
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
            Self::loading(ui, "Cargando");
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
                let sub = format!("De {}", pl.owner_name());
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
        let meta = self
            .playlists
            .iter()
            .find(|pl| pl.id == id)
            .cloned()
            .or_else(|| self.playlist_meta.get(&id).cloned());
        self.request_once(&format!("plmeta:{id}"), Req::PlaylistMeta(id.clone()));
        self.request_once(&format!("pl:{id}"), Req::PlaylistTracks(id.clone()));
        let list = self.lists.remove(&id).unwrap_or_default();

        let full_meta = self.playlist_meta.get(&id).cloned().or(meta);
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
        let total_ms: u64 = list.tracks.iter().map(|t| t.duration_ms as u64).sum();
        let mut meta_line = String::new();
        if !owner.is_empty() {
            meta_line.push_str(&format!("De {owner} · "));
        }
        meta_line.push_str(&format!("{total} canciones"));
        if !list.tracks.is_empty() {
            meta_line.push_str(&format!(" · {}", fmt_total(total_ms)));
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
        let top = Self::top_artists(&list.tracks, 5);
        let in_library = self.in_library(&id);
        let pinned = self.settings.pinned.contains(&id);
        let tracks = list.tracks;
        let loading = list.loading;
        // Autores: el propietario y, por orden de aparición, quienes han añadido canciones.
        let mut contributors: Vec<String> = owner_id.iter().cloned().collect();
        for t in &tracks {
            if let Some(b) = &t.added_by {
                if !contributors.contains(b) {
                    contributors.push(b.clone());
                }
            }
        }
        // Spotify ya no marca como colaborativas las públicas: si hay varios autores, lo es.
        let collaborative = collaborative || contributors.len() > 1;
        let editable = mine || collaborative;
        let id2 = id.clone();
        let uri2 = uri.clone();
        let full_meta2 = full_meta.clone();

        self.two_columns(
            ui,
            |app, ui| {
                let p = theme::palette(ui.ctx());
                Self::page_title(ui, "", &name, &meta_line);
                if let Some(d) = &description {
                    ui.add(Label::new(RichText::new(d).small().color(p.weak)).wrap());
                    ui.add_space(8.0);
                }
                if contributors.len() > 1 {
                    // Varias personas han añadido canciones: se listan como en Spotify.
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.label(RichText::new("Por").small().color(p.weak));
                        for (i, user) in contributors.iter().enumerate() {
                            if i > 0 {
                                ui.label(RichText::new("·").small().color(p.faint));
                            }
                            let img = app.users.get(user).and_then(|u| u.cover(64).map(|s| s.to_string()));
                            let name = app.user_display(user);
                            let r = ui
                                .horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 5.0;
                                    app.cover(ui, img.as_deref(), 20.0, true);
                                    ui.label(RichText::new(name).small().color(p.text));
                                })
                                .response
                                .interact(Sense::click());
                            if r.hovered() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            if r.clicked() {
                                app.actions.push(Action::Go(Page::User(user.clone())));
                            }
                        }
                    });
                    ui.add_space(6.0);
                } else if let Some(oid) = &owner_id {
                    if !owner.is_empty() {
                        let r = ui.add(Label::new(RichText::new(format!("Ver perfil de {owner}")).small().color(GREEN)).sense(Sense::click()));
                        if r.clicked() {
                            app.actions.push(Action::Go(Page::User(oid.clone())));
                        }
                        ui.add_space(6.0);
                    }
                }
                let link = uri_to_link(&uri2);
                let id_m = id2.clone();
                let meta_m = full_meta2.clone();
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
                        if Self::menu_item(ui, Some(Icon::Image), "Cambiar imagen…", false).clicked() {
                            app.actions.push(Action::PickPlaylistImage(id_m.clone()));
                            ui.close();
                        }
                        app.invite_menu_item(ui, &id_m);
                    } else if Self::menu_item(ui, Some(Icon::PlusCircle), if in_library { "Quitar de tu biblioteca" } else { "Guardar en tu biblioteca" }, false).clicked() {
                        app.actions.push(Action::FollowPlaylist(id_m.clone(), !in_library));
                        ui.close();
                    }
                    if in_library || mine {
                        app.folder_menu(ui, &id_m);
                    }
                    if Self::menu_item(ui, Some(Icon::NewTab), "Abrir en una nueva pestaña", false).clicked() {
                        app.actions.push(Action::OpenInTab(Page::Playlist(id_m.clone())));
                        ui.close();
                    }
                }));
                let all_uris: Vec<String> = tracks.iter().map(|t| t.uri.clone()).collect();
                app.collection_bar(
                    ui,
                    &id,
                    &tracks,
                    Some(&uri2),
                    Some(&link),
                    |app, ui| {
                        let p = theme::palette(ui.ctx());
                        if icons::button(ui, Icon::PlusSquare, 34.0, p.weak).on_hover_text("Añadir todas a una playlist").clicked() {
                            app.open_add_dialog(all_uris.clone());
                        }
                    },
                    menu,
                );
                ui.add_space(8.0);
                if loading {
                    Self::loading(ui, &format!("Cargando {} de {}", tracks.len(), total));
                } else if tracks.is_empty() && total > 0 {
                    Self::loading(ui, "Cargando");
                } else if tracks.is_empty() {
                    ui.label(RichText::new("Esta playlist está vacía. Añade canciones con el botón + de cualquier fila.").color(p.weak));
                }
                let shown = app.filter_tracks(&id, &tracks);
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
                app.info_card(ui, cover.as_deref(), &chips, &top);
            },
        );
        self.lists.insert(id.clone(), crate::app::TrackList { tracks, total, loading });
    }

    // ------------------------------------------------------------------- álbum

    fn album_page(&mut self, ui: &mut egui::Ui, id: String) {
        if !self.signed_in() {
            self.welcome(ui);
            return;
        }
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
        let meta = format!("{}  •  {} canciones  •  {}", album.year(), all.len(), fmt_total(total_ms));
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
        let filtered = self.filter_tracks(&id, &all);
        if self.sel_list == id {
            if let Some(mark) = self.sel.iter().find(|s| s.starts_with('#')).cloned() {
                self.sel.remove(&mark);
                if let Some(t) = mark[1..].parse::<usize>().ok().and_then(|n| all.get(n)) {
                    self.sel.insert(t.uri.clone());
                }
            }
        }
        let saved = self.saved_albums.iter().any(|a| a.id == id);

        self.two_columns(
            ui,
            |app, ui| {
                let p = theme::palette(ui.ctx());
                ui.add(Label::new(RichText::new(&name).font(theme::bold(30.0))).truncate());
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let (ir, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                    icons::paint(ui.painter(), ir, p.weak, Icon::Artist);
                    // Artistas en una sola línea truncada (con menú si son varios) y los metadatos detrás.
                    let meta_text = format!(" •  {meta}");
                    let meta_w = ui.painter().layout_no_wrap(meta_text.clone(), theme::regular(12.0), p.weak).size().x;
                    let avail = (ui.available_width() - meta_w - 24.0).max(80.0);
                    let names = artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ");
                    let l = ui
                        .scope(|ui| {
                            ui.set_max_width(avail);
                            ui.add(Label::new(RichText::new(names).font(theme::regular(13.0)).color(p.text)).truncate().sense(Sense::click()))
                        })
                        .inner;
                    if l.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if artists.len() == 1 {
                        if l.clicked() {
                            if let Some(aid) = &artists[0].id {
                                app.actions.push(Action::Go(Page::Artist(aid.clone())));
                            }
                        }
                    } else {
                        egui::Popup::menu(&l).show(|ui| {
                            for a in &artists {
                                if let Some(aid) = &a.id {
                                    if Self::menu_item(ui, Some(Icon::Artist), a.name.as_str(), false).clicked() {
                                        app.actions.push(Action::Go(Page::Artist(aid.clone())));
                                        ui.close();
                                    }
                                }
                            }
                        });
                    }
                    ui.label(RichText::new(meta_text).small().color(p.weak));
                });
                ui.add_space(10.0);
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
                    |app, ui| {
                        let p = theme::palette(ui.ctx());
                        let (icon, color, tip) = if saved {
                            (Icon::Check, GREEN, "Quitar de tu biblioteca")
                        } else {
                            (Icon::PlusCircle, p.weak, "Guardar en tu biblioteca")
                        };
                        if icons::button(ui, icon, 34.0, color).on_hover_text(tip).clicked() {
                            app.actions.push(Action::SaveAlbum(id.clone(), !saved));
                        }
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
                ui.add_space(8.0);
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
                app.info_card(ui, cover.as_deref(), &chips, &collaborators);
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
                ui.label(RichText::new("Falta un paso para ver tu biblioteca").strong());
                ui.label(
                    "Spotify limita la Web API del cliente compartido, así que Nanofy necesita \
                     un Client ID propio (gratis, dos minutos). Con él cargan tus playlists, \
                     la búsqueda, los perfiles y todo lo demás. La reproducción no lo necesita.",
                );
                ui.add_space(4.0);
                if ui.button("Configurar en Ajustes").clicked() {
                    self.draft = self.settings.clone();
                    self.actions.push(Action::Go(Page::Settings));
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
                    self.web_busy = true;
                    self.status("Se ha abierto el navegador para autorizar Nanofy en Spotify…");
                    self.api.send(Req::WebConnect(crate::webauth::WEB_CLIENT_ID.to_string()));
                }
                if self.web_busy {
                    Self::loading(ui, "");
                }
            });
        });
        ui.add_space(8.0);
        ui.label(
            RichText::new(
                "Autoriza tu cuenta en el navegador (una sola vez). Habilita la biblioteca, la \
                 búsqueda, las playlists, el corazón y seguir artistas. No necesitas crear ninguna app.",
            )
            .small()
            .color(p.weak),
        );

        // Opcional (avanzado): app propia del usuario solo para LECTURAS, con su propia cuota.
        // Las escrituras (corazón, seguir) siguen yendo por la conexión de arriba.
        ui.add_space(14.0);
        let personal = self.api.personal_configured();
        ui.horizontal(|ui| {
            let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui.painter().circle_filled(dot.center(), 5.0, if personal { GREEN } else { p.weak });
            ui.label(RichText::new("Lecturas rápidas con tu propia app (opcional)").strong());
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
                "Da la máxima velocidad de carga: las lecturas usan tu cuota, no el límite \
                 compartido. Crea una app en developer.spotify.com/dashboard, añade esta Redirect URI \
                 y pega el Client ID:",
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
    }

    pub fn settings_page(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let p = theme::palette(&ctx);
        ui.label(RichText::new("Ajustes").font(theme::bold(28.0)));
        ui.label(RichText::new("Los cambios de reproducción se aplican al guardar; el resto, al instante.").small().color(p.weak));

        Self::settings_card(ui, "Reproducción", |ui| {
            Self::setting_row(ui, "Nombre en Spotify Connect", Some("Así aparece Nanofy en la lista de dispositivos."), |ui| {
                let p = theme::palette(ui.ctx());
                ui.set_max_width(260.0);
                Self::field_box_fill(ui, p.card, |ui| ui.add(egui::TextEdit::singleline(&mut self.draft.device_name).frame(egui::Frame::NONE).desired_width(240.0)));
            });
            Self::setting_row(
                ui,
                "Calidad de audio",
                Some("Sin pérdida pide FLAC; si tu cuenta o la canción no lo ofrecen, suena a 320 kbps. Salida en 32 bits flotantes sin remuestreo."),
                |ui| {
                    for q in [Quality::Low, Quality::Normal, Quality::High, Quality::Lossless] {
                        if Self::pill(ui, q.label(), self.draft.quality == q).clicked() {
                            self.draft.quality = q;
                        }
                    }
                },
            );
            let mut v = self.draft.normalisation;
            if Self::toggle_pad(ui, &mut v, "Normalizar volumen entre canciones", 0.0) {
                self.draft.normalisation = v;
            }
            let mut v = self.draft.gapless;
            if Self::toggle_pad(ui, &mut v, "Reproducción sin pausas (gapless)", 0.0) {
                self.draft.gapless = v;
            }
            let mut v = self.draft.autoplay;
            if Self::toggle_pad(ui, &mut v, "Autoplay al terminar una lista", 0.0) {
                self.draft.autoplay = v;
            }
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
            if self.settings.playback_differs(&self.draft) {
                ui.label(RichText::new("Al guardar se reinicia el reproductor.").small().color(p.weak));
            }
        });

        Self::settings_card(ui, "Web API · biblioteca, búsqueda, playlists y perfiles", |ui| {
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
            Auth::LoggingIn => Self::loading(ui, "Conectando"),
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
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if Self::secondary_button(ui, "Buscar actualizaciones", !self.update_busy).clicked() {
                    self.check_updates(true);
                }
                if self.update_busy {
                    Self::loading(ui, "Consultando GitHub");
                } else if let Some((text, err)) = self.update_note.clone() {
                    ui.label(RichText::new(text).small().color(if err { ERROR_RED } else { weak }));
                    if self.update.is_some() && Self::primary_button(ui, "Descargar", true).clicked() {
                        self.open_update(&ctx, true);
                    }
                }
            });
            let mut v = self.settings.update_check;
            if Self::toggle_pad(ui, &mut v, "Avisar al arrancar cuando haya una versión nueva", 0.0) {
                self.set_update_check(v);
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
