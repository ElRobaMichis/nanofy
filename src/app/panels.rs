//! Panel lateral (cola y letras) y ventanas flotantes (Jam, editor de playlist, atajos, avisos de
//! actualización).

use egui::{vec2, Align, Button, Color32, Label, Layout, RichText, Sense};

use super::icons::{self, Icon};
use super::theme;
use super::{pick_image_file, Action, App, ERROR_RED, GREEN};
use crate::api::Req;
use crate::model::*;
use crate::update::{FailKind, MoveReason, NoteLine, Stage, UpdateInfo};

impl App {
    pub fn overlays(&mut self, ctx: &egui::Context) {
        self.shortcuts_window(ctx);
        self.jam_window(ctx);
        self.editor_window(ctx);
        self.folder_window(ctx);
        self.song_more_panel(ctx);
        self.add_panel(ctx);
        self.notes_window(ctx);
        self.update_window(ctx);
        self.updated_toast_window(ctx);
    }

    /// Aviso flotante de la actualización (arriba a la derecha, sobre el contenido): el de la
    /// versión nueva en cada estado o, si no está a la vista, el rojo de una que no arrancó.
    fn update_window(&mut self, ctx: &egui::Context) {
        if self.update_banner && self.update.is_none() {
            self.update_banner = false;
        }
        if self.update_banner {
            self.update_banner_card(ctx);
        } else if self.update_failed_banner {
            self.update_failed_card(ctx);
        }
    }

    fn update_banner_card(&mut self, ctx: &egui::Context) {
        let Some(info) = self.update.clone() else {
            return;
        };
        let p = theme::palette(ctx);
        let current = crate::update::current_version();
        let applying = self.update_applying || self.restart_after_exit.is_some();
        // Reiniciar saca de la Jam: el aviso lo dice antes de que se pulse.
        let in_jam = self.jam.is_some() || self.update_confirm_jam;
        let view = banner_view(&self.update_stage, &info, &current, crate::update::can_self_install(), in_jam, applying);
        let mut action: Option<UpdAction> = None;
        egui::Area::new(egui::Id::new("update_banner"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_TOP, vec2(-18.0, 58.0))
            .show(ctx, |ui| {
                Self::dialog_frame(ctx).show(ui, |ui| {
                    ui.set_width(340.0);
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                        icons::paint(ui.painter(), r, GREEN, Icon::Download);
                        ui.add_space(4.0);
                        ui.label(RichText::new(&view.title).font(theme::bold(15.0)));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if icons::button(ui, Icon::Close, 24.0, p.weak).on_hover_text("Recordar más tarde").clicked() {
                                action = Some(UpdAction::Later);
                            }
                        });
                    });
                    if let Some(sub) = &view.sub {
                        ui.add_space(4.0);
                        ui.add(Label::new(RichText::new(sub).small().color(p.weak)).wrap());
                    }
                    if view.notes {
                        let lines = info.preview_lines(3);
                        if !lines.is_empty() {
                            ui.add_space(6.0);
                            for l in lines {
                                ui.add(Label::new(RichText::new(l).small().color(p.text)).wrap());
                            }
                        }
                    }
                    if let Some((frac, text)) = &view.progress {
                        ui.add_space(10.0);
                        // Sin el tamaño de la release no hay fracción: la barra solo late.
                        ui.add(egui::ProgressBar::new(frac.unwrap_or(0.0)).desired_height(6.0).fill(GREEN).animate(frac.is_none()));
                        ui.add_space(4.0);
                        ui.label(RichText::new(text).small().color(p.text));
                    }
                    if let Some(text) = &view.spinner {
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().size(16.0).color(GREEN));
                            ui.label(RichText::new(text).color(p.text));
                        });
                    }
                    if let Some((text, red)) = &view.message {
                        ui.add_space(8.0);
                        ui.add(Label::new(RichText::new(text).small().color(if *red { theme::RED } else { p.text })).wrap());
                    }
                    if let Some(detail) = &view.detail {
                        ui.add_space(2.0);
                        ui.add(Label::new(RichText::new(detail).small().color(p.weak)).wrap());
                    }
                    if view.primary.is_some() || view.secondary.is_some() || view.link.is_some() || view.skip {
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            if let Some((label, a)) = view.primary {
                                if Self::primary_button(ui, label, true).clicked() {
                                    action = Some(a);
                                }
                            }
                            if let Some((label, a)) = view.secondary {
                                if Self::secondary_button(ui, label, true).clicked() {
                                    action = Some(a);
                                }
                            }
                            if let Some((label, a)) = view.link {
                                if Self::small_link(ui, label).clicked() {
                                    action = Some(a);
                                }
                            }
                            if view.skip {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if Self::small_link(ui, "Omitir esta versión").on_hover_text("No volver a avisar de esta versión").clicked() {
                                        action = Some(UpdAction::Skip);
                                    }
                                });
                            }
                        });
                    }
                    if let Some(note) = view.footnote {
                        ui.add_space(6.0);
                        ui.add(Label::new(RichText::new(note).small().color(p.weak)).wrap());
                    }
                });
            });
        if let Some(a) = action {
            self.run_update_action(ctx, a);
        }
    }

    /// Aviso rojo: la versión nueva no arrancaba y se volvió a esta (o falló al instalarla al
    /// abrir). «Reintentar» vuelve a buscarla e instalarla.
    fn update_failed_card(&mut self, ctx: &egui::Context) {
        let Some(msg) = self.update_failed.clone() else {
            self.update_failed_banner = false;
            return;
        };
        let p = theme::palette(ctx);
        let mut action: Option<UpdAction> = None;
        egui::Area::new(egui::Id::new("update_failed_banner"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_TOP, vec2(-18.0, 58.0))
            .show(ctx, |ui| {
                Self::dialog_frame(ctx).stroke(egui::Stroke::new(1.0, theme::RED)).show(ui, |ui| {
                    ui.set_width(340.0);
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                        icons::paint(ui.painter(), r, theme::RED, Icon::Download);
                        ui.add_space(4.0);
                        ui.label(RichText::new("No se pudo actualizar").font(theme::bold(15.0)));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if icons::button(ui, Icon::Close, 24.0, p.weak).on_hover_text("Cerrar").clicked() {
                                action = Some(UpdAction::Later);
                            }
                        });
                    });
                    ui.add_space(6.0);
                    ui.add(Label::new(RichText::new(&msg).small().color(theme::RED)).wrap());
                    ui.add_space(12.0);
                    if Self::primary_button(ui, "Reintentar", !self.update_busy).clicked() {
                        action = Some(UpdAction::Recheck);
                    }
                });
            });
        match action {
            Some(UpdAction::Later) => self.update_failed_banner = false,
            Some(a) => self.run_update_action(ctx, a),
            None => {}
        }
    }

    /// Lo pulsado en el aviso de actualización o en Ajustes › Acerca de.
    pub(super) fn run_update_action(&mut self, ctx: &egui::Context, a: UpdAction) {
        match a {
            // Tras un fallo se reintenta aquí mismo y desde cero: el navegador marca el zip y
            // Windows vuelve a avisar de la app sin firmar, así que queda como último recurso.
            UpdAction::Install => self.install_update(),
            UpdAction::Recheck => self.retry_failed_update(),
            UpdAction::Restart => self.restart_to_update(false),
            UpdAction::RestartJam => self.restart_to_update(true),
            UpdAction::Cancel => self.cancel_update(),
            UpdAction::Later => {
                self.update_banner = false;
                self.update_confirm_jam = false;
            }
            UpdAction::Skip => self.skip_update(),
            UpdAction::Notes => self.open_release_notes(ctx, self.update.clone()),
            UpdAction::Page => self.open_update(ctx, false),
            UpdAction::Download => self.open_update(ctx, true),
        }
    }

    /// Aviso breve tras abrirse la versión recién instalada: «Nanofy se actualizó a la {v}».
    /// Ocupa el sitio del aviso de actualización, así que si este está a la vista espera.
    fn updated_toast_window(&mut self, ctx: &egui::Context) {
        if (self.update_banner && self.update.is_some()) || self.update_failed_banner {
            return;
        }
        let now = std::time::Instant::now();
        let (version, since) = match &mut self.updated_toast {
            Some(t) => (t.version.clone(), *t.since.get_or_insert(now)),
            None => return,
        };
        let left = super::UPDATED_TOAST_FOR.saturating_sub(now.duration_since(since));
        if left.is_zero() {
            self.updated_toast = None;
            return;
        }
        let p = theme::palette(ctx);
        let mut action: Option<u8> = None;
        let resp = egui::Area::new(egui::Id::new("updated_toast"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_TOP, vec2(-18.0, 58.0))
            .show(ctx, |ui| {
                Self::dialog_frame(ctx).inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                        icons::paint(ui.painter(), r, GREEN, Icon::Check);
                        ui.label(RichText::new(format!("Nanofy se actualizó a la {version}")).font(theme::bold(14.0)));
                        if Self::secondary_button(ui, "Ver novedades", true).clicked() {
                            action = Some(1);
                        }
                        if icons::button(ui, Icon::Close, 24.0, p.weak).on_hover_text("Cerrar").clicked() {
                            action = Some(2);
                        }
                    });
                });
            })
            .response;
        // Con el ratón encima no se va: vuelve a contar desde que salga.
        if resp.contains_pointer() {
            if let Some(t) = &mut self.updated_toast {
                t.since = Some(now);
            }
            ctx.request_repaint_after(super::UPDATED_TOAST_FOR);
        } else {
            ctx.request_repaint_after(left);
        }
        match action {
            Some(1) => {
                self.updated_toast = None;
                let info = self.current_release_notes();
                self.open_release_notes(ctx, Some(info));
            }
            Some(2) => self.updated_toast = None,
            _ => {}
        }
    }

    /// Diálogo «Novedades de Nanofy {v}»: las notas completas de la release y su enlace.
    fn notes_window(&mut self, ctx: &egui::Context) {
        let Some(info) = self.notes_dialog.clone() else {
            return;
        };
        let p = theme::palette(ctx);
        let mut open = true;
        let mut github = false;
        // Que quepa también con la ventana pequeña y la interfaz ampliada.
        let max_h = (ctx.content_rect().height() - 220.0).clamp(80.0, 380.0);
        egui::Window::new("notes_dialog")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .fixed_size(vec2(440.0, 0.0))
            .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .frame(Self::dialog_frame(ctx))
            .show(ctx, |ui| {
                if Self::dialog_title(ui, &format!("Novedades de Nanofy {}", info.version)) {
                    open = false;
                }
                egui::ScrollArea::vertical().id_salt("notes_dialog_text").max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                    for line in crate::update::note_lines(&info.notes) {
                        match line {
                            NoteLine::Heading(t) => {
                                ui.add_space(4.0);
                                ui.add(Label::new(RichText::new(t).font(theme::bold(15.0))).wrap());
                            }
                            NoteLine::Bullet(t) => {
                                ui.add(Label::new(RichText::new(format!("• {t}")).color(p.text)).wrap());
                            }
                            NoteLine::Text(t) => {
                                ui.add(Label::new(RichText::new(t).color(p.text)).wrap());
                            }
                            NoteLine::Gap => ui.add_space(8.0),
                        }
                    }
                });
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if Self::small_link(ui, "Ver en GitHub").on_hover_text(&info.page_url).clicked() {
                        github = true;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if Self::secondary_button(ui, "Cerrar", true).clicked() {
                            open = false;
                        }
                    });
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            open = false;
        }
        if github {
            ctx.open_url(egui::OpenUrl::new_tab(info.page_url.clone()));
        }
        if !open {
            self.notes_dialog = None;
        }
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

    /// Enlace de texto pequeño y apagado, para la acción secundaria junto a los botones.
    pub(super) fn small_link(ui: &mut egui::Ui, text: &str) -> egui::Response {
        let p = theme::palette(ui.ctx());
        let r = ui.add(Button::new(RichText::new(text).small().color(p.weak)).frame(false));
        if r.hovered() {
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
                    ed.public_known = true;
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
                        // Colaborativa exige privada: lo que se ve pasa a ser lo que se envía.
                        if collab {
                            ed.public_known = true;
                        }
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
                    public: ed.public_known.then_some(ed.public),
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

// ------------------------------------------------------------ actualización: qué se enseña

/// Lo que se puede pulsar en el aviso de actualización y en Ajustes › Acerca de.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UpdAction {
    /// «Instalar», y «Reintentar» tras un fallo al prepararla (empieza de cero).
    Install,
    /// «Reintentar» sin nada que instalar todavía: tras una versión que no arrancó o una
    /// consulta fallida, vuelve a mirar (y en el primer caso instala lo que encuentre).
    Recheck,
    Restart,
    /// «Reiniciar igualmente» en una Jam.
    RestartJam,
    Cancel,
    /// «Más tarde» y la ×: se esconde el aviso.
    Later,
    Skip,
    /// «Ver novedades»: el diálogo con las notas.
    Notes,
    /// «Ver en GitHub»: la página de la release.
    Page,
    /// «Descargar» (donde la app no se sustituye sola) y «Descargar a mano»: el zip en el navegador.
    Download,
}

/// Contenido del aviso flotante en un estado (`banner_view`). Sin egui, para poder probarlo.
#[derive(Debug, Default)]
pub(super) struct BannerView {
    pub title: String,
    /// Texto bajo el título, en gris.
    pub sub: Option<String>,
    /// Enseñar las primeras líneas de las notas.
    pub notes: bool,
    /// Barra de la descarga: fracción (si se sabe) y texto.
    pub progress: Option<(Option<f32>, String)>,
    /// Rueda de espera con este texto.
    pub spinner: Option<String>,
    /// Qué pasó (en rojo si `true`) y, debajo y en pequeño, el motivo real.
    pub message: Option<(String, bool)>,
    pub detail: Option<String>,
    pub primary: Option<(&'static str, UpdAction)>,
    pub secondary: Option<(&'static str, UpdAction)>,
    /// Enlace pequeño junto a los botones.
    pub link: Option<(&'static str, UpdAction)>,
    /// «Omitir esta versión», a la derecha.
    pub skip: bool,
    /// Nota final en pequeño.
    pub footnote: Option<&'static str>,
}

/// El aviso de la versión `info` en el estado `stage`. `current`: la versión en uso;
/// `self_install`: la app puede sustituirse sola en este sistema; `in_jam`: «Reiniciar» saca de
/// la Jam; `applying`: ya se está sustituyendo el ejecutable.
pub(super) fn banner_view(stage: &Stage, info: &UpdateInfo, current: &str, self_install: bool, in_jam: bool, applying: bool) -> BannerView {
    let v = &info.version;
    let mut view = BannerView { title: format!("Nanofy {v} disponible"), ..Default::default() };
    if applying {
        let version = match stage {
            Stage::Ready { version, .. } => version,
            _ => v,
        };
        view.title = format!("Nanofy {version} está lista");
        view.spinner = Some("Instalando la versión nueva…".to_string());
        return view;
    }
    if !self_install {
        // macOS: la app no puede sustituirse sola; el zip es lo único útil.
        view.sub = Some(format!("Tienes la {current}. Descarga el zip nuevo y sustituye el ejecutable."));
        view.notes = true;
        view.primary = Some(("Descargar", UpdAction::Download));
        view.secondary = Some(("Ver novedades", UpdAction::Notes));
        view.skip = true;
        return view;
    }
    match stage {
        Stage::Downloading { done, total } => {
            view.title = format!("Descargando Nanofy {v}");
            view.progress = Some(crate::update::download_progress(*done, *total));
            view.sub = Some("Puedes seguir escuchando mientras tanto.".to_string());
            view.secondary = Some(("Cancelar", UpdAction::Cancel));
        }
        Stage::Preparing => {
            view.title = format!("Nanofy {v}");
            view.spinner = Some("Preparando la actualización…".to_string());
            view.sub = Some("Puedes seguir escuchando mientras tanto.".to_string());
            view.secondary = Some(("Cancelar", UpdAction::Cancel));
        }
        Stage::Ready { version, .. } => {
            view.title = format!("Nanofy {version} está lista");
            if in_jam {
                view.sub = Some("Estás en una Jam: al reiniciar saldrás de ella.".to_string());
                view.primary = Some(("Reiniciar igualmente", UpdAction::RestartJam));
            } else {
                view.sub = Some("Reinicia para usar la versión nueva. La música seguirá donde estaba.".to_string());
                view.primary = Some(("Reiniciar", UpdAction::Restart));
            }
            view.secondary = Some(("Más tarde", UpdAction::Later));
            view.footnote = Some("Si no, se instalará sola la próxima vez que abras Nanofy.");
        }
        Stage::Failed { kind, detail } => {
            let (text, extra) = crate::update::fail_message(*kind, detail, current);
            // Sin descarga para este sistema no es un error de la app: solo se informa.
            view.message = Some((text, *kind != FailKind::NoAsset));
            view.detail = extra;
            view.primary = Some(if *kind == FailKind::NoAsset { ("Ver en GitHub", UpdAction::Page) } else { ("Reintentar", UpdAction::Install) });
            // Si el antivirus no la deja arrancar, reintentar puede dar lo mismo: queda el zip.
            if *kind == FailKind::Blocked {
                view.link = Some(("Descargar a mano", UpdAction::Download));
            }
            view.skip = true;
        }
        Stage::NeedsMove { dir, reason } => {
            view.message = Some((reason.text(dir), false));
            // Reintentar daría lo mismo. Dentro del zip basta con descomprimirlo; en una carpeta
            // sin permisos, el zip a mano es lo único que sirve.
            if *reason == MoveReason::ReadOnly {
                view.primary = Some(("Descargar a mano", UpdAction::Download));
            }
            view.skip = true;
        }
        Stage::Idle | Stage::Available if info.asset.is_none() => {
            view.sub = Some(format!("Tienes la {current}. {}", crate::update::NO_ASSET_TEXT));
            view.primary = Some(("Ver en GitHub", UpdAction::Page));
            view.skip = true;
        }
        Stage::Idle | Stage::Available => {
            view.sub = Some(format!("Tienes la {current}. Se descarga en segundos y solo tendrás que reiniciar."));
            view.notes = true;
            view.primary = Some(("Instalar", UpdAction::Install));
            view.secondary = Some(("Ver novedades", UpdAction::Notes));
            view.skip = true;
        }
    }
    view
}

/// Color del texto de estado en Ajustes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tone {
    Text,
    Weak,
    Error,
}

/// Fila de estado de Ajustes › Acerca de, debajo de «Buscar actualizaciones» (`about_view`).
#[derive(Debug)]
pub(super) struct AboutView {
    pub text: String,
    pub tone: Tone,
    /// Puntos de espera tras el texto (buscando, instalando).
    pub waiting: bool,
    /// Motivo o qué hacer, en pequeño debajo.
    pub detail: Option<String>,
    pub primary: Option<(&'static str, UpdAction)>,
    pub secondary: Option<(&'static str, UpdAction)>,
    pub link: Option<(&'static str, UpdAction)>,
}

impl AboutView {
    fn new(text: impl Into<String>, tone: Tone) -> Self {
        Self { text: text.into(), tone, waiting: false, detail: None, primary: None, secondary: None, link: None }
    }
}

/// Estado de la actualización en Ajustes. `note`: el resultado en texto de la última consulta
/// (`true` = error); `checking`: hay una consulta en marcha. `None`: nada que decir (aún no se
/// ha consultado).
pub(super) fn about_view(stage: &Stage, update: Option<&UpdateInfo>, note: Option<&(String, bool)>, checking: bool, applying: bool, self_install: bool, current: &str) -> Option<AboutView> {
    if applying {
        return Some(AboutView { waiting: true, ..AboutView::new("Instalando la versión nueva", Tone::Weak) });
    }
    // Lista o preparándose: una consulta que llegue a la vez no lo cambia (ver `App::on_update`).
    match stage {
        Stage::Ready { version, .. } => {
            return Some(AboutView { primary: Some(("Reiniciar", UpdAction::Restart)), ..AboutView::new(format!("Nanofy {version} lista"), Tone::Text) });
        }
        Stage::Downloading { .. } | Stage::Preparing => {
            return Some(AboutView { secondary: Some(("Cancelar", UpdAction::Cancel)), ..AboutView::new(stage.label(), Tone::Weak) });
        }
        _ => {}
    }
    if checking {
        return Some(AboutView { waiting: true, ..AboutView::new("Buscando", Tone::Weak) });
    }
    let Some(u) = update else {
        // Sin versión nueva: al día, aún publicándose o el error de la consulta (o de la
        // actualización que no arrancó), que se puede reintentar.
        let (text, err) = note?;
        let mut view = AboutView::new(text.clone(), if *err { Tone::Error } else { Tone::Weak });
        if *err {
            view.primary = Some(("Reintentar", UpdAction::Recheck));
        }
        return Some(view);
    };
    let mut view = AboutView::new(format!("Nanofy {} disponible", u.version), Tone::Text);
    if !self_install {
        view.primary = Some(("Descargar", UpdAction::Download));
        return Some(view);
    }
    match stage {
        Stage::Failed { kind, detail } => {
            let (text, extra) = crate::update::fail_message(*kind, detail, current);
            view.text = text;
            view.tone = if *kind == FailKind::NoAsset { Tone::Weak } else { Tone::Error };
            view.detail = extra;
            view.primary = Some(if *kind == FailKind::NoAsset { ("Ver en GitHub", UpdAction::Page) } else { ("Reintentar", UpdAction::Install) });
            if *kind == FailKind::Blocked {
                view.link = Some(("Descargar a mano", UpdAction::Download));
            }
        }
        Stage::NeedsMove { dir, reason } => {
            view.detail = Some(reason.text(dir));
            if *reason == MoveReason::ReadOnly {
                view.primary = Some(("Descargar a mano", UpdAction::Download));
            }
        }
        _ if u.asset.is_none() => {
            view.detail = Some(crate::update::NO_ASSET_TEXT.to_string());
            view.primary = Some(("Ver en GitHub", UpdAction::Page));
        }
        _ => {
            view.primary = Some(("Instalar", UpdAction::Install));
            // Una consulta a mano que falló después de encontrar esta versión: se dice debajo.
            if let Some((text, true)) = note {
                view.detail = Some(text.clone());
            }
        }
    }
    Some(view)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::Asset;
    use std::path::PathBuf;

    fn info(asset: bool) -> UpdateInfo {
        UpdateInfo {
            version: "1.7.0".into(),
            page_url: "https://x/v1.7.0".into(),
            notes: "- Arreglos".into(),
            asset: asset.then(|| Asset { name: "Nanofy-windows-x64.zip".into(), url: "https://x/z".into(), size: 10, sha256: None }),
        }
    }

    fn stages() -> Vec<Stage> {
        let mut v = vec![
            Stage::Idle,
            Stage::Available,
            Stage::Downloading { done: 5, total: Some(10) },
            Stage::Downloading { done: 5, total: None },
            Stage::Preparing,
            Stage::Ready { version: "1.7.0".into(), staged: PathBuf::from("nanofy.update.exe") },
            Stage::NeedsMove { dir: PathBuf::from("C:/Program Files/Nanofy"), reason: MoveReason::ReadOnly },
            Stage::NeedsMove { dir: PathBuf::from("C:/Temp/Temp1_x"), reason: MoveReason::RunningFromZip },
        ];
        for kind in [FailKind::Network, FailKind::Corrupt, FailKind::Disk, FailKind::Blocked, FailKind::Swap, FailKind::NoAsset] {
            v.push(Stage::Failed { kind, detail: "motivo".into() });
        }
        v
    }

    /// Donde la app se sustituye sola nunca se ofrece «Descargar» (el navegador) como botón: solo
    /// «Descargar a mano», y solo cuando reintentar no serviría.
    #[test]
    fn sin_descargar_si_se_instala_sola() {
        for stage in stages() {
            for asset in [true, false] {
                for (jam, applying) in [(false, false), (true, false), (false, true)] {
                    let view = banner_view(&stage, &info(asset), "1.6.0", true, jam, applying);
                    assert_ne!(view.primary.map(|p| p.0), Some("Descargar"), "{stage:?}");
                    assert_ne!(view.secondary.map(|p| p.0), Some("Descargar"), "{stage:?}");
                    let about = about_view(&stage, Some(&info(asset)), None, false, applying, true, "1.6.0");
                    assert_ne!(about.and_then(|a| a.primary).map(|p| p.0), Some("Descargar"), "{stage:?}");
                }
            }
        }
        // En macOS sí: es lo único útil.
        let mac = banner_view(&Stage::Available, &info(true), "1.6.0", false, false, false);
        assert_eq!(mac.primary, Some(("Descargar", UpdAction::Download)));
    }

    #[test]
    fn aviso_por_estado() {
        let i = info(true);
        let v = banner_view(&Stage::Available, &i, "1.6.0", true, false, false);
        assert_eq!(v.title, "Nanofy 1.7.0 disponible");
        assert_eq!(v.sub.as_deref(), Some("Tienes la 1.6.0. Se descarga en segundos y solo tendrás que reiniciar."));
        assert!(v.notes && v.skip);
        assert_eq!(v.primary, Some(("Instalar", UpdAction::Install)));
        assert_eq!(v.secondary, Some(("Ver novedades", UpdAction::Notes)));

        let v = banner_view(&Stage::Downloading { done: 3_800_000, total: Some(7_600_000) }, &i, "1.6.0", true, false, false);
        assert_eq!(v.title, "Descargando Nanofy 1.7.0");
        assert_eq!(v.progress, Some((Some(0.5), "50 % · 3.8 de 7.6 MB".to_string())));
        assert_eq!(v.sub.as_deref(), Some("Puedes seguir escuchando mientras tanto."));
        assert_eq!(v.secondary, Some(("Cancelar", UpdAction::Cancel)));
        assert!(v.primary.is_none() && !v.skip);

        assert_eq!(banner_view(&Stage::Preparing, &i, "1.6.0", true, false, false).spinner.as_deref(), Some("Preparando la actualización…"));

        let ready = Stage::Ready { version: "1.7.0".into(), staged: PathBuf::new() };
        let v = banner_view(&ready, &i, "1.6.0", true, false, false);
        assert_eq!(v.title, "Nanofy 1.7.0 está lista");
        assert_eq!(v.sub.as_deref(), Some("Reinicia para usar la versión nueva. La música seguirá donde estaba."));
        assert_eq!(v.primary, Some(("Reiniciar", UpdAction::Restart)));
        assert_eq!(v.secondary, Some(("Más tarde", UpdAction::Later)));
        assert_eq!(v.footnote, Some("Si no, se instalará sola la próxima vez que abras Nanofy."));
        let v = banner_view(&ready, &i, "1.6.0", true, true, false);
        assert_eq!(v.sub.as_deref(), Some("Estás en una Jam: al reiniciar saldrás de ella."));
        assert_eq!(v.primary, Some(("Reiniciar igualmente", UpdAction::RestartJam)));
        // Sustituyendo: solo la espera, sin nada que pulsar.
        let v = banner_view(&ready, &i, "1.6.0", true, false, true);
        assert!(v.spinner.is_some() && v.primary.is_none() && v.secondary.is_none() && !v.skip);

        let failed = |kind| banner_view(&Stage::Failed { kind, detail: String::new() }, &i, "1.6.0", true, false, false);
        let v = failed(FailKind::Network);
        assert_eq!(v.message, Some(("No se pudo descargar la actualización. Revisa tu conexión.".to_string(), true)));
        assert_eq!(v.primary, Some(("Reintentar", UpdAction::Install)));
        assert!(v.link.is_none());
        assert_eq!(failed(FailKind::Blocked).link, Some(("Descargar a mano", UpdAction::Download)));
        let v = failed(FailKind::NoAsset);
        assert_eq!(v.primary, Some(("Ver en GitHub", UpdAction::Page)));
        assert_eq!(v.message.map(|m| m.1), Some(false));

        let v = banner_view(&Stage::NeedsMove { dir: PathBuf::from("D:/x"), reason: MoveReason::RunningFromZip }, &i, "1.6.0", true, false, false);
        assert!(v.primary.is_none());
        assert!(v.message.is_some_and(|m| m.0.starts_with("Estás abriendo Nanofy desde dentro del zip")));
        let v = banner_view(&Stage::NeedsMove { dir: PathBuf::from("D:/x"), reason: MoveReason::ReadOnly }, &i, "1.6.0", true, false, false);
        assert_eq!(v.primary, Some(("Descargar a mano", UpdAction::Download)));

        // Release aún sin zip para este sistema: solo la página.
        let v = banner_view(&Stage::Available, &info(false), "1.6.0", true, false, false);
        assert_eq!(v.primary, Some(("Ver en GitHub", UpdAction::Page)));
    }

    #[test]
    fn ajustes_por_estado() {
        let i = info(true);
        let row = |stage: &Stage, update: Option<&UpdateInfo>, note: Option<&(String, bool)>, checking: bool| about_view(stage, update, note, checking, false, true, "1.6.0");
        assert!(row(&Stage::Idle, None, None, false).is_none());
        let v = row(&Stage::Idle, None, None, true).unwrap();
        assert!(v.waiting && v.text == "Buscando");
        let al_dia = ("Estás al día (1.6.0)".to_string(), false);
        let v = row(&Stage::Idle, None, Some(&al_dia), false).unwrap();
        assert_eq!((v.text.as_str(), v.tone, v.primary), ("Estás al día (1.6.0)", Tone::Weak, None));
        let sin_red = ("Sin conexión con GitHub".to_string(), true);
        let v = row(&Stage::Idle, None, Some(&sin_red), false).unwrap();
        assert_eq!((v.tone, v.primary), (Tone::Error, Some(("Reintentar", UpdAction::Recheck))));
        let v = row(&Stage::Available, Some(&i), None, false).unwrap();
        assert_eq!((v.text.as_str(), v.primary), ("Nanofy 1.7.0 disponible", Some(("Instalar", UpdAction::Install))));
        // Una consulta en marcha no tapa la descarga.
        let v = row(&Stage::Downloading { done: 1, total: Some(4) }, Some(&i), None, true).unwrap();
        assert_eq!((v.text.as_str(), v.secondary), ("Descargando… 25 %", Some(("Cancelar", UpdAction::Cancel))));
        let v = row(&Stage::Ready { version: "1.7.0".into(), staged: PathBuf::new() }, Some(&i), None, false).unwrap();
        assert_eq!((v.text.as_str(), v.primary), ("Nanofy 1.7.0 lista", Some(("Reiniciar", UpdAction::Restart))));
        let v = row(&Stage::Failed { kind: FailKind::Corrupt, detail: "Zip no válido: x".into() }, Some(&i), None, false).unwrap();
        assert_eq!((v.text.as_str(), v.tone), ("La descarga llegó dañada y se descartó.", Tone::Error));
        assert_eq!((v.detail.as_deref(), v.primary), (Some("Zip no válido: x"), Some(("Reintentar", UpdAction::Install))));
    }
}
