//! Panel lateral (cola y letras) y ventanas flotantes (Jam, editor de playlist, atajos).

use egui::{vec2, Align, Button, Color32, Label, Layout, RichText, Sense};

use super::icons::{self, Icon};
use super::theme;

use super::widgets::RowOpts;
use super::{pick_image_file, Action, App, PlayState, SideTab, ERROR_RED, GREEN};
use crate::api::Req;
use crate::model::*;

impl App {
    pub fn side_panel(&mut self, ui: &mut egui::Ui, tab: SideTab) {
        ui.horizontal(|ui| {
            for (t, label) in [(SideTab::Queue, "Cola"), (SideTab::Lyrics, "Letra")] {
                let selected = tab == t;
                if ui.selectable_label(selected, RichText::new(label).strong()).clicked() && !selected {
                    self.toggle_side(t);
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let p = theme::palette(ui.ctx());
                if icons::button(ui, Icon::Close, 28.0, p.weak).on_hover_text("Cerrar").clicked() {
                    self.side = None;
                }
            });
        });
        ui.separator();
        match tab {
            SideTab::Queue => self.queue_panel(ui),
            SideTab::Lyrics => self.lyrics_panel(ui),
        }
    }

    fn queue_panel(&mut self, ui: &mut egui::Ui) {
        if !self.signed_in() {
            ui.label("Inicia sesión para ver la cola.");
            return;
        }
        let weak = ui.visuals().weak_text_color();
        let Some(q) = self.queue.take() else {
            Self::loading(ui, "Cargando la cola");
            return;
        };
        egui::ScrollArea::vertical()
            .id_salt("queue_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if let Some(now) = &q.currently_playing {
                    ui.label(RichText::new("Sonando ahora").small().strong().color(weak));
                    let one = vec![now.clone()];
                    self.track_rows(ui, "queue_now", &one, RowOpts { selectable: false, ..RowOpts::tracks(true, false) });
                    ui.add_space(8.0);
                }
                // Lo añadido a la cola desde Nanofy va primero; el resto es la continuación del
                // contexto (playlist, radio, álbum…). Es el mismo orden en que sonarán.
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
                if q.queue.is_empty() {
                    ui.label(RichText::new("La cola está vacía.").color(weak));
                }
                if !queued.is_empty() {
                    let ms: u64 = queued.iter().map(|t| t.duration_ms as u64).sum();
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("Siguiente en la cola · {} · {}", queued.len(), fmt_total(ms))).small().strong().color(weak));
                        if self.player.remote.is_none() {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.add(Button::new(RichText::new("Vaciar cola").small()).frame(false)).clicked() {
                                    self.queue_clear();
                                }
                            });
                        }
                    });
                    self.track_rows(
                        ui,
                        "queue_next",
                        &queued,
                        RowOpts { selectable: false, editable_playlist: Some("queue"), ..RowOpts::tracks(true, false) },
                    );
                    ui.add_space(8.0);
                }
                if !rest.is_empty() {
                    let ms: u64 = rest.iter().map(|t| t.duration_ms as u64).sum();
                    let ctx = self.now_context();
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.label(RichText::new("Siguientes de:").small().strong().color(weak));
                        match &ctx {
                            Some((name, Some(page))) => {
                                let l = ui.add(Label::new(RichText::new(name).small().strong().color(GREEN)).sense(Sense::click()));
                                if l.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                }
                                if l.clicked() {
                                    self.actions.push(Action::Go(page.clone()));
                                }
                            }
                            Some((name, None)) => {
                                ui.label(RichText::new(name).small().strong().color(weak));
                            }
                            None => {
                                ui.label(RichText::new("la reproducción actual").small().strong().color(weak));
                            }
                        }
                        ui.label(RichText::new(format!("· {} · {}", rest.len(), fmt_total(ms))).small().color(weak));
                    });
                    self.track_rows(ui, "queue_rest", &rest, RowOpts { selectable: false, ..RowOpts::tracks(true, false) });
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "La cola la mantiene Spotify; se actualiza cada pocos segundos. \
                         Añade canciones con «Añadir a la cola» en el menú de cualquier fila.",
                    )
                    .small()
                    .color(weak),
                );
            });
        self.queue = Some(q);
    }

    fn lyrics_panel(&mut self, ui: &mut egui::Ui) {
        let weak = ui.visuals().weak_text_color();
        if !self.signed_in() {
            ui.label("Inicia sesión para ver las letras.");
            return;
        }
        self.ensure_lyrics();
        let Some(np) = self.player.now.clone() else {
            ui.label(RichText::new("Nada en reproducción.").color(weak));
            return;
        };
        ui.add(Label::new(RichText::new(&np.name).strong()).truncate());
        ui.add(Label::new(RichText::new(np.artists_str()).small().color(weak)).truncate());
        ui.add_space(6.0);
        if self.lyrics_loading {
            Self::loading(ui, "Buscando la letra");
            return;
        }
        let Some(lyrics) = self.lyrics.clone() else {
            ui.label(RichText::new("Esta canción no tiene letra disponible.").color(weak));
            return;
        };
        let pos = self.player.position();
        let current = lyrics.current_line(pos);
        let playing = self.player.state == PlayState::Playing;
        let mut seek_to: Option<u32> = None;
        egui::ScrollArea::vertical()
            .id_salt(("lyrics_scroll", &lyrics.track_id))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (i, line) in lyrics.lines.iter().enumerate() {
                    let is_cur = current == Some(i);
                    let past = current.map(|c| i < c).unwrap_or(false);
                    let text = if line.words.trim().is_empty() {
                        "♪"
                    } else {
                        line.words.as_str()
                    };
                    let rich = if is_cur {
                        RichText::new(text).size(18.0).strong().color(GREEN)
                    } else if past {
                        RichText::new(text).size(16.0).color(weak.gamma_multiply(0.8))
                    } else {
                        RichText::new(text).size(16.0)
                    };
                    let r = ui.add(Label::new(rich).wrap().sense(if lyrics.synced() {
                        Sense::click()
                    } else {
                        Sense::hover()
                    }));
                    if is_cur && playing {
                        r.scroll_to_me(Some(Align::Center));
                    }
                    if lyrics.synced() && r.clicked() {
                        seek_to = Some(line.start_ms);
                    }
                    ui.add_space(4.0);
                }
                ui.add_space(12.0);
                if !lyrics.provider.is_empty() {
                    ui.label(
                        RichText::new(format!("Letra: {}", lyrics.provider))
                            .small()
                            .color(weak),
                    );
                }
                if !lyrics.synced() {
                    ui.label(
                        RichText::new("Letra sin sincronizar.")
                            .small()
                            .color(weak),
                    );
                }
            });
        if let Some(ms) = seek_to {
            self.seek(ms);
        }
    }

    pub fn overlays(&mut self, ctx: &egui::Context) {
        self.shortcuts_window(ctx);
        self.jam_window(ctx);
        self.editor_window(ctx);
        self.folder_window(ctx);
        self.add_dialog_window(ctx);
    }

    /// Diálogo «Nueva carpeta» / «Renombrar carpeta» (mismo estilo que los demás).
    fn folder_window(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.folder_dialog.take() else {
            return;
        };
        let mut open = true;
        let mut submit = false;
        let title = if d.id.is_some() { "Renombrar carpeta" } else { "Nueva carpeta" };
        let p = theme::palette(ctx);
        egui::Window::new("folder_dialog")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .fixed_size(vec2(400.0, 0.0))
            .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .frame(Self::dialog_frame(ctx))
            .show(ctx, |ui| {
                if Self::dialog_title(ui, title) {
                    open = false;
                }
                Self::field_label(ui, "NOMBRE");
                let r = Self::field_box(ui, |ui| {
                    let r = ui.add(egui::TextEdit::singleline(&mut d.name).hint_text("Mi carpeta").frame(egui::Frame::NONE).desired_width(f32::INFINITY));
                    if !d.busy {
                        r.request_focus();
                    }
                    r
                });
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                if d.playlist.is_some() {
                    ui.add_space(6.0);
                    ui.label(RichText::new("La playlist se moverá dentro de la carpeta nueva.").small().color(p.faint));
                }
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        let label = if d.id.is_some() { "Guardar" } else { "Crear" };
                        if Self::primary_button(ui, label, !d.busy && !d.name.trim().is_empty()).clicked() {
                            submit = true;
                        }
                        if Self::secondary_button(ui, "Cancelar", true).clicked() {
                            open = false;
                        }
                        if d.busy {
                            Self::loading(ui, "");
                        }
                    });
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            open = false;
        }
        if !open {
            return;
        }
        if submit && !d.busy && !d.name.trim().is_empty() {
            d.busy = true;
            let name = d.name.trim().to_string();
            match &d.id {
                Some(id) => self.api.send(Req::FolderRename { id: id.clone(), name }),
                None => self.api.send(Req::FolderCreate { name, playlists: d.playlist.iter().cloned().collect() }),
            }
        }
        self.folder_dialog = Some(d);
    }

    /// Fila del diálogo de playlists: icono, nombre y, a la derecha, círculo de selección o
    /// chevron (carpetas). Devuelve (clic en la fila, clic en el círculo).
    fn add_dialog_row(ui: &mut egui::Ui, icon: Icon, name: &str, selected: Option<bool>, folder_open: Option<bool>, indent: f32) -> (bool, bool) {
        let p = theme::palette(ui.ctx());
        let w = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(vec2(w, 40.0), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(rect, egui::CornerRadius::same(8), p.hover);
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let ir = egui::Rect::from_center_size(egui::pos2(rect.min.x + 18.0 + indent, rect.center().y), vec2(16.0, 16.0));
        icons::paint(ui.painter(), ir, p.weak, icon);
        let text_w = w - 36.0 - indent - 44.0;
        let galley = ui.painter().layout(name.to_string(), theme::regular(14.0), p.text, text_w.max(40.0));
        let ty = rect.center().y - galley.size().y / 2.0;
        ui.painter().galley(egui::pos2(rect.min.x + 40.0 + indent, ty), galley, p.text);
        let mut circle_clicked = false;
        if let Some(sel) = selected {
            let cr = egui::Rect::from_center_size(egui::pos2(rect.max.x - 22.0, rect.center().y), vec2(28.0, 28.0));
            let c = ui.interact(cr, ui.id().with(("addsel", name, indent as i32)), Sense::click());
            if sel {
                ui.painter().circle_filled(cr.center(), 10.0, GREEN);
                icons::paint(ui.painter(), egui::Rect::from_center_size(cr.center(), vec2(12.0, 12.0)), Color32::BLACK, Icon::Check);
            } else {
                let col = if c.hovered() || resp.hovered() { p.text } else { p.weak };
                ui.painter().circle_stroke(cr.center(), 10.0, egui::Stroke::new(1.5, col));
            }
            circle_clicked = c.clicked();
        } else if let Some(open) = folder_open {
            let c = egui::pos2(rect.max.x - 22.0, rect.center().y);
            let s = 5.0;
            let pts = if open {
                vec![egui::pos2(c.x - s, c.y - s / 2.0), egui::pos2(c.x, c.y + s / 2.0), egui::pos2(c.x + s, c.y - s / 2.0)]
            } else {
                vec![egui::pos2(c.x - s / 2.0, c.y - s), egui::pos2(c.x + s / 2.0, c.y), egui::pos2(c.x - s / 2.0, c.y + s)]
            };
            ui.painter().add(egui::Shape::line(pts, egui::Stroke::new(1.6, p.weak)));
        }
        (resp.clicked(), circle_clicked)
    }

    /// Ventana «Añadir a una playlist»: buscador, nueva playlist inline, playlists y carpetas con
    /// círculo de selección, y pie fijo con Cancelar / Listo.
    fn add_dialog_window(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.add_dialog.take() else {
            return;
        };
        let p = theme::palette(ctx);
        let mut keep = true;
        let mut done = false;
        // Se muestran las playlists a las que el usuario puede añadir: propias, colaborativas
        // (la bandera de la Web API no es fiable en las públicas) y, para colaboradores, las de
        // la biblioteca que no son listas algorítmicas de Spotify.
        let mine: Vec<Playlist> = self
            .playlists
            .iter()
            .filter(|pl| {
                let owner = pl.owner.id.as_deref().unwrap_or("");
                let algorithmic = owner == "spotify" || pl.id.starts_with("37i9");
                let _ = owner;
                self.is_mine(pl) || pl.collaborative.unwrap_or(false) || !algorithmic
            })
            .cloned()
            .collect();
        let folders = self.folders.clone();
        let in_folder: std::collections::HashSet<String> = folders.iter().flat_map(|f| f.playlists.iter().cloned()).collect();
        let q = d.query.trim().to_lowercase();
        let matches = |s: &str| q.is_empty() || s.to_lowercase().contains(&q);
        let mut create: Option<String> = None;

        egui::Window::new("add_to_playlist")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .fixed_size(vec2(380.0, 540.0))
            .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .frame(
                egui::Frame::new()
                    .fill(p.card)
                    .stroke(egui::Stroke::new(1.0, p.border))
                    .corner_radius(egui::CornerRadius::same(14))
                    .inner_margin(egui::Margin::same(18)),
            )
            .show(ctx, |ui| {
                ui.label(RichText::new("Añadir a una playlist").font(theme::bold(16.0)));
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(10.0);
                // Buscador
                egui::Frame::new().fill(p.card2).corner_radius(egui::CornerRadius::same(10)).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let w = ui.available_width() - 28.0;
                        ui.add(
                            egui::TextEdit::singleline(&mut d.query)
                                .hint_text("Buscar una playlist o carpeta")
                                .frame(egui::Frame::NONE)
                                .desired_width(w),
                        );
                        let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                        icons::paint(ui.painter(), r, p.weak, Icon::Search);
                    });
                });
                ui.add_space(8.0);

                // Lista (con desplazamiento); el pie queda fijo debajo.
                egui::ScrollArea::vertical().id_salt("add_dialog_list").max_height(360.0).auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    match d.new_name.as_mut() {
                        None => {
                            let (row_clicked, _) = Self::add_dialog_row(ui, Icon::Plus, "Nueva playlist", None, None, 0.0);
                            if row_clicked {
                                d.new_name = Some(String::new());
                                d.focus = true;
                            }
                        }
                        Some(name) => {
                            let mut close_edit = false;
                            egui::Frame::new().fill(p.card2).corner_radius(egui::CornerRadius::same(8)).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                                    let icon = if name.trim().is_empty() { Icon::Plus } else { Icon::Check };
                                    icons::paint(ui.painter(), r, if name.trim().is_empty() { p.weak } else { GREEN }, icon);
                                    let te = ui.add(
                                        egui::TextEdit::singleline(name)
                                            .hint_text("Nombre de la playlist")
                                            .frame(egui::Frame::NONE)
                                            .desired_width(ui.available_width() - 4.0),
                                    );
                                    if d.focus {
                                        d.focus = false;
                                        te.request_focus();
                                    }
                                    let enter = te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                    if enter && !name.trim().is_empty() {
                                        create = Some(name.trim().to_string());
                                        close_edit = true;
                                    }
                                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) && te.has_focus() {
                                        close_edit = true;
                                    }
                                });
                            });
                            if close_edit {
                                d.new_name = None;
                            }
                        }
                    }
                    if mine.is_empty() {
                        ui.add_space(6.0);
                        ui.label(RichText::new("Todavía no tienes playlists propias.").small().color(p.weak));
                    }
                    // Carpetas (con sus playlists dentro)
                    for f in &folders {
                        let children: Vec<&Playlist> = mine.iter().filter(|pl| f.playlists.contains(&pl.id)).collect();
                        if children.is_empty() {
                            continue;
                        }
                        let child_match = children.iter().any(|pl| matches(&pl.name));
                        if !matches(&f.name) && !child_match {
                            continue;
                        }
                        let open = d.open_folders.contains(&f.id) || (!q.is_empty() && child_match);
                        let (row_clicked, _) = Self::add_dialog_row(ui, Icon::Folder, &f.name, None, Some(open), 0.0);
                        if row_clicked {
                            if d.open_folders.contains(&f.id) {
                                d.open_folders.remove(&f.id);
                            } else {
                                d.open_folders.insert(f.id.clone());
                            }
                        }
                        if open {
                            for pl in children {
                                if !matches(&pl.name) && !matches(&f.name) {
                                    continue;
                                }
                                let sel = d.selected.contains(&pl.id);
                                let (rc, cc) = Self::add_dialog_row(ui, Icon::Playlist, &pl.name, Some(sel), None, 24.0);
                                if rc || cc {
                                    if sel {
                                        d.selected.remove(&pl.id);
                                    } else {
                                        d.selected.insert(pl.id.clone());
                                    }
                                }
                            }
                        }
                    }
                    // Playlists sueltas
                    for pl in &mine {
                        if in_folder.contains(&pl.id) || !matches(&pl.name) {
                            continue;
                        }
                        let sel = d.selected.contains(&pl.id);
                        let (rc, cc) = Self::add_dialog_row(ui, Icon::Playlist, &pl.name, Some(sel), None, 0.0);
                        if rc || cc {
                            if sel {
                                d.selected.remove(&pl.id);
                            } else {
                                d.selected.insert(pl.id.clone());
                            }
                        }
                    }
                });

                // Pie fijo
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        let n = d.selected.len();
                        let label = if n > 1 { format!("Listo ({n})") } else { "Listo".to_string() };
                        let b = Button::new(RichText::new(label).color(Color32::BLACK).strong())
                            .fill(GREEN)
                            .corner_radius(egui::CornerRadius::same(18))
                            .min_size(vec2(120.0, 36.0));
                        let r = ui.add_enabled(n > 0, b).on_disabled_hover_text("Marca al menos una playlist");
                        if n > 0 && r.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if r.clicked() {
                            done = true;
                        }
                        let cancel = Button::new(RichText::new("Cancelar").color(p.text))
                            .fill(p.card2)
                            .corner_radius(egui::CornerRadius::same(18))
                            .min_size(vec2(100.0, 36.0));
                        let r = ui.add(cancel);
                        if r.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if r.clicked() {
                            keep = false;
                        }
                    });
                });
            });

        if let Some(name) = create {
            match self.my_id().map(|s| s.to_string()) {
                Some(user_id) => {
                    d.pending_new = Some(name.clone());
                    self.api.send(Req::CreatePlaylist { user_id, name, description: String::new(), public: false, collaborative: false });
                }
                None => self.status_err("Todavía no se ha cargado tu perfil; inténtalo en un momento"),
            }
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && d.new_name.is_none() {
            keep = false;
        }
        if done {
            let n = d.selected.len();
            for pid in &d.selected {
                for uri in &d.uris {
                    self.actions.push(Action::AddToPlaylist { playlist_id: pid.clone(), uri: uri.clone() });
                }
            }
            self.status(if n == 1 { "Añadido a la playlist".to_string() } else { format!("Añadido a {n} playlists") });
            keep = false;
        }
        if keep {
            self.add_dialog = Some(d);
        }
    }

    fn shortcuts_window(&mut self, ctx: &egui::Context) {
        if !self.show_shortcuts {
            return;
        }
        let mut open = self.show_shortcuts;
        egui::Window::new("Atajos de teclado")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                let rows = [
                    ("Espacio", "Reproducir o pausar"),
                    ("Ctrl+← / Ctrl+→", "Anterior / siguiente"),
                    ("Shift+← / Shift+→", "Retroceder / avanzar 10 s"),
                    ("Ctrl+↑ / Ctrl+↓", "Volumen"),
                    ("M", "Silenciar"),
                    ("S / R", "Aleatorio / repetir"),
                    ("Q / L", "Cola / letra"),
                    ("Ctrl+J", "Jam (escuchar en grupo)"),
                    ("Ctrl+N", "Nueva playlist"),
                    ("Ctrl+F o /", "Buscar (o pegar un enlace)"),
                    ("Ctrl+B", "Mostrar u ocultar la barra lateral"),
                    ("Alt+← / Alt+→", "Atrás / adelante"),
                    ("Ctrl+H / Ctrl+L", "Inicio / Canciones que te gustan"),
                    ("Ctrl+,", "Ajustes"),
                    ("Ctrl+/ o ?", "Esta ventana"),
                    ("Ctrl+Q", "Salir"),
                ];
                egui::Grid::new("shortcuts").num_columns(2).spacing([24.0, 6.0]).show(ui, |ui| {
                    for (k, d) in rows {
                        ui.label(RichText::new(k).monospace().strong());
                        ui.label(d);
                        ui.end_row();
                    }
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new("En macOS, Cmd sustituye a Ctrl. Las teclas multimedia del teclado también funcionan.")
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
            });
        self.show_shortcuts = open;
    }

    // ------------------------------------------------------------ estilo de diálogos

    /// Marco de diálogo: el mismo que «Añadir a una playlist».
    pub(super) fn dialog_frame(ctx: &egui::Context) -> egui::Frame {
        let p = theme::palette(ctx);
        egui::Frame::new()
            .fill(p.card)
            .stroke(egui::Stroke::new(1.0, p.border))
            .corner_radius(egui::CornerRadius::same(14))
            .inner_margin(egui::Margin::same(18))
    }

    /// Título del diálogo con × a la derecha; devuelve `true` si se pulsó cerrar.
    pub(super) fn dialog_title(ui: &mut egui::Ui, text: &str) -> bool {
        let p = theme::palette(ui.ctx());
        let mut close = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new(text).font(theme::bold(16.0)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icons::button(ui, Icon::Close, 26.0, p.weak).on_hover_text("Cerrar (Esc)").clicked() {
                    close = true;
                }
            });
        });
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(10.0);
        close
    }

    /// Etiqueta pequeña sobre un campo.
    pub(super) fn field_label(ui: &mut egui::Ui, text: &str) {
        let p = theme::palette(ui.ctx());
        ui.label(RichText::new(text).small().strong().color(p.weak));
        ui.add_space(2.0);
    }

    /// Caja de texto con el fondo de la app (una línea o varias).
    pub(super) fn field_box(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> egui::Response) -> egui::Response {
        let p = theme::palette(ui.ctx());
        Self::field_box_fill(ui, p.card2, add)
    }

    /// Caja de texto con un relleno concreto (en tarjetas card2 se usa el fondo más oscuro).
    pub(super) fn field_box_fill(ui: &mut egui::Ui, fill: Color32, add: impl FnOnce(&mut egui::Ui) -> egui::Response) -> egui::Response {
        egui::Frame::new()
            .fill(fill)
            .corner_radius(egui::CornerRadius::same(10))
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                add(ui)
            })
            .inner
    }

    /// Botón principal (verde) y secundario (gris), en píldora y con cursor de mano.
    pub(super) fn primary_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
        let b = Button::new(RichText::new(text).color(Color32::BLACK).strong())
            .fill(GREEN)
            .corner_radius(egui::CornerRadius::same(18))
            .min_size(vec2(110.0, 36.0));
        let r = ui.add_enabled(enabled, b);
        if enabled && r.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        r
    }

    pub(super) fn secondary_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
        let p = theme::palette(ui.ctx());
        let b = Button::new(RichText::new(text).color(p.text))
            .fill(p.card2)
            .corner_radius(egui::CornerRadius::same(18))
            .min_size(vec2(100.0, 36.0));
        let r = ui.add_enabled(enabled, b);
        if enabled && r.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        r
    }

    // --------------------------------------------------------------------- Jam

    fn jam_window(&mut self, ctx: &egui::Context) {
        if !self.jam_open {
            return;
        }
        let mut open = true;
        let p = theme::palette(ctx);
        egui::Window::new("jam_window")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .fixed_size(vec2(400.0, 0.0))
            .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .frame(Self::dialog_frame(ctx))
            .show(ctx, |ui| {
                if Self::dialog_title(ui, "Jam · escuchar en grupo") {
                    open = false;
                }
                if !self.logged_in() {
                    ui.label(RichText::new("Inicia sesión para usar Jam.").color(p.weak));
                    return;
                }
                let busy = self.jam_busy;
                match self.jam.clone() {
                    Some(jam) => {
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                            icons::paint(ui.painter(), r, GREEN, Icon::People);
                            ui.label(RichText::new(if jam.is_owner { "Estás en un Jam que has creado tú" } else { "Estás en un Jam" }).strong());
                        });
                        ui.add_space(12.0);
                        Self::field_label(ui, "ENLACE PARA INVITAR");
                        let link = jam.join_url.clone();
                        ui.horizontal(|ui| {
                            let w = ui.available_width() - 110.0;
                            ui.scope(|ui| {
                                ui.set_max_width(w);
                                Self::field_box(ui, |ui| {
                                    ui.add(egui::TextEdit::singleline(&mut link.clone()).frame(egui::Frame::NONE).interactive(false).desired_width(w - 20.0))
                                });
                            });
                            if Self::secondary_button(ui, "Copiar", true).clicked() {
                                self.actions.push(Action::CopyText(link.clone(), "Enlace del Jam"));
                            }
                        });
                        ui.add_space(12.0);
                        let members = if jam.max_members > 0 {
                            format!("PARTICIPANTES ({} DE {})", jam.members.len(), jam.max_members)
                        } else {
                            format!("PARTICIPANTES ({})", jam.members.len())
                        };
                        Self::field_label(ui, &members);
                        for m in &jam.members {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 10.0;
                                self.cover(ui, m.image_url.as_deref(), 28.0, true);
                                ui.label(RichText::new(&m.name).color(p.text));
                                if m.is_owner {
                                    let _ = Self::pill(ui, "Anfitrión", false);
                                }
                            });
                        }
                        ui.add_space(14.0);
                        ui.separator();
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            if Self::secondary_button(ui, "Salir del Jam", !busy).clicked() {
                                self.jam_busy = true;
                                self.api.send(Req::JamLeave(jam.session_id.clone()));
                            }
                            if jam.is_owner && Self::secondary_button(ui, "Finalizar para todos", !busy).clicked() {
                                self.jam_busy = true;
                                self.api.send(Req::JamEnd(jam.session_id.clone()));
                            }
                            if Self::secondary_button(ui, "Actualizar", !busy).clicked() {
                                self.jam_busy = true;
                                self.api.send(Req::JamCurrent);
                            }
                            if busy {
                                Self::loading(ui, "");
                            }
                        });
                    }
                    None => {
                        ui.label(RichText::new("Varias personas escuchan y controlan la misma música desde sus propios dispositivos.").color(p.weak));
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if Self::primary_button(ui, "Crear un Jam", !busy).clicked() {
                                self.jam_busy = true;
                                self.jam_error = None;
                                self.api.send(Req::JamCurrent);
                            }
                            if busy {
                                Self::loading(ui, "");
                            }
                        });
                        ui.add_space(14.0);
                        Self::field_label(ui, "O ÚNETE CON UN ENLACE O CÓDIGO");
                        let mut join = false;
                        ui.horizontal(|ui| {
                            let w = ui.available_width() - 110.0;
                            ui.scope(|ui| {
                                ui.set_max_width(w);
                                let r = Self::field_box(ui, |ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.jam_link)
                                            .hint_text("https://open.spotify.com/socialsession/…")
                                            .frame(egui::Frame::NONE)
                                            .desired_width(w - 20.0),
                                    )
                                });
                                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                    join = true;
                                }
                            });
                            if Self::secondary_button(ui, "Unirme", !busy).clicked() {
                                join = true;
                            }
                        });
                        if join && !busy {
                            match jam_token_from_link(&self.jam_link) {
                                Some(token) => self.jam_join(&token),
                                None => self.jam_error = Some("Enlace no válido".into()),
                            }
                        }
                    }
                }
                if let Some(e) = &self.jam_error {
                    ui.add_space(6.0);
                    ui.label(RichText::new(e).small().color(ERROR_RED));
                }
                ui.add_space(10.0);
                ui.label(
                    RichText::new("Jam usa una API interna de Spotify a través de librespot; requiere Premium y puede dejar de funcionar si Spotify la cambia.")
                        .small()
                        .color(p.faint),
                );
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            open = false;
        }
        self.jam_open = open;
    }

    // ------------------------------------------------------------ editor de playlist

    fn editor_window(&mut self, ctx: &egui::Context) {
        let Some(mut ed) = self.editor.take() else {
            return;
        };
        let mut open = true;
        let mut submit = false;
        let title = if ed.id.is_some() { "Editar playlist" } else { "Nueva playlist" };
        let p = theme::palette(ctx);
        egui::Window::new("playlist_editor")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .fixed_size(vec2(420.0, 0.0))
            .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .frame(Self::dialog_frame(ctx))
            .show(ctx, |ui| {
                if Self::dialog_title(ui, title) {
                    open = false;
                }
                Self::field_label(ui, "NOMBRE");
                let r = Self::field_box(ui, |ui| {
                    ui.add(egui::TextEdit::singleline(&mut ed.name).hint_text("Mi playlist").frame(egui::Frame::NONE).desired_width(f32::INFINITY))
                });
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                ui.add_space(10.0);
                Self::field_label(ui, "DESCRIPCIÓN");
                Self::field_box(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut ed.description)
                            .hint_text("Opcional")
                            .frame(egui::Frame::NONE)
                            .desired_width(f32::INFINITY)
                            .desired_rows(3),
                    )
                });
                ui.add_space(10.0);
                let mut public = ed.public;
                if Self::toggle(ui, &mut public, "Playlist pública") {
                    ed.public = public;
                    if public {
                        ed.collaborative = false;
                    }
                }
                if ed.public {
                    // Pública: se colabora por invitación (como en Spotify), no con la bandera.
                    ui.add_space(4.0);
                    Self::field_label(ui, "COLABORADORES");
                    match &ed.id {
                        Some(id) => {
                            let me = self.my_id().map(|s| s.to_string());
                            let members = self.members.get(id).cloned().unwrap_or_default();
                            let others: Vec<String> = members.iter().filter(|(u, c)| *c && Some(u) != me.as_ref()).map(|(u, _)| u.clone()).collect();
                            if others.is_empty() {
                                ui.label(RichText::new("Todavía nadie más colabora en esta playlist.").small().color(p.faint));
                            } else {
                                ui.horizontal_wrapped(|ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    for (i, u) in others.iter().enumerate() {
                                        if i > 0 {
                                            ui.label(RichText::new("·").small().color(p.faint));
                                        }
                                        ui.label(RichText::new(self.user_display(u)).small().color(p.text));
                                    }
                                });
                            }
                            ui.add_space(6.0);
                            if Self::secondary_button(ui, "Copiar enlace de invitación", true).clicked() {
                                self.request_invite(id);
                            }
                            ui.label(RichText::new("Quien abra el enlace en Spotify podrá añadir y quitar canciones.").small().color(p.faint));
                        }
                        None => {
                            ui.label(RichText::new("Podrás invitar colaboradores en cuanto la crees.").small().color(p.faint));
                        }
                    }
                } else {
                    let mut collab = ed.collaborative;
                    if Self::toggle(ui, &mut collab, "Colaborativa (otras personas pueden añadir y quitar canciones)") {
                        ed.collaborative = collab;
                    }
                }
                ui.add_space(10.0);
                Self::field_label(ui, "IMAGEN");
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    if Self::secondary_button(ui, "Elegir…", true).clicked() {
                        if let Some(path) = pick_image_file() {
                            ed.image_path = Some(path);
                        }
                    }
                    match &ed.image_path {
                        Some(path) => {
                            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("imagen").to_string();
                            ui.label(RichText::new(name).small().color(p.text));
                            if icons::button(ui, Icon::Close, 22.0, p.weak).on_hover_text("Quitar").clicked() {
                                ed.image_path = None;
                            }
                        }
                        None => {
                            ui.label(RichText::new("o arrastra un JPG/PNG aquí").small().color(p.faint));
                        }
                    }
                });
                ui.add_space(14.0);
                ui.separator();
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        let label = if ed.id.is_some() { "Guardar" } else { "Crear" };
                        let can = !ed.busy && !ed.name.trim().is_empty();
                        if Self::primary_button(ui, label, can).clicked() {
                            submit = true;
                        }
                        // Cancelar siempre disponible (si una petición sigue en curso, termina sola).
                        if Self::secondary_button(ui, "Cancelar", true).clicked() {
                            open = false;
                        }
                        if ed.busy {
                            Self::loading(ui, "");
                        }
                    });
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            open = false;
        }

        if !open {
            return;
        }
        if submit && !ed.busy && !ed.name.trim().is_empty() {
            ed.busy = true;
            match &ed.id {
                Some(id) => self.api.send(Req::UpdatePlaylist {
                    id: id.clone(),
                    name: ed.name.trim().to_string(),
                    description: ed.description.trim().to_string(),
                    public: ed.public,
                    collaborative: ed.collaborative,
                }),
                None => {
                    let Some(user_id) = self.my_id().map(|s| s.to_string()) else {
                        self.status_err("Todavía no se ha cargado tu perfil; inténtalo en un momento");
                        ed.busy = false;
                        self.editor = Some(ed);
                        return;
                    };
                    self.api.send(Req::CreatePlaylist {
                        user_id,
                        name: ed.name.trim().to_string(),
                        description: ed.description.trim().to_string(),
                        public: ed.public,
                        collaborative: ed.collaborative,
                    });
                }
            }
        }
        self.editor = Some(ed);
    }
}
