//! «Añadir a una playlist», copiado de la referencia de diseño 14.png (la ventana de 13.webp
//! reducida a 0,853: cliente = (referencia − (5,13, 7,85)) / 0,853). Un panel de cristal de
//! 371,6 × 524 px, como el menú de los tres puntos: cabecera, buscador, «Nueva playlist» (se
//! escribe el nombre ahí mismo), playlists con su círculo para marcar y carpetas con su flecha
//! para desplegarlas; abajo, «Cancelar» y «Listo». Abierto desde el botón del reproductor sale
//! encima de él; desde cualquier otro sitio, en el centro de la ventana.

use std::collections::HashSet;

use egui::{pos2, vec2, Color32, CornerRadius, Rect, Sense, Stroke};

use super::icons::{self, Icon};
use super::library::{paint_mask, px};
use super::library_masks as lm;
use super::menu_masks as mm;
use super::theme::{self, GREEN};
use super::widgets::{galley_truncated, text_on_baseline};
use super::{Action, App, Playlist};
use crate::api::Req;

/// Tamaño del panel.
const W: f32 = 371.6;
const H: f32 = 524.0;
/// Borde derecho a 27,55 px a la derecha del centro del botón de añadir; el de abajo, a 95,16 px
/// del borde de abajo de la ventana.
const RIGHT_OF_BUTTON: f32 = 27.55;
const FROM_BOTTOM: f32 = 95.16;
const RADIUS: u8 = 7;
const BLUR_SIGMA: f32 = 56.0;

// Medidas desde la esquina del panel.
const HEAD_X: f32 = 23.3;
const HEAD_BASE: f32 = 31.65;
const HEAD_LINE: f32 = 51.6;
/// Buscador: caja de (22,3, 73,9) a (348,2, 129), radio 7,5; texto desde 42,2 con la línea base en
/// 106,7; lupa centrada en (315,3, 101,4).
const SEARCH_BOX: [f32; 4] = [22.3, 73.9, 348.2, 129.0];
const SEARCH_TEXT_X: f32 = 43.2;
const SEARCH_BASE: f32 = 106.7;
const SEARCH_ICON: egui::Vec2 = vec2(315.3, 101.4);
/// Lista: entre el buscador y la línea del pie, que está en 451,3.
const LIST_TOP: f32 = 131.0;
const FOOT_LINE: f32 = 451.3;
/// «Nueva playlist»: fila de 57,4 con su centro en 175,2; las demás, cada 55,87.
const NEW_ROW_CENTER: f32 = 175.2;
const NEW_ROW_H: f32 = 57.4;
const ROW_H: f32 = 55.87;
/// Columnas: icono centrado en 42,8, texto desde 73,9, círculo en 327,6 y flecha en 329,4.
const ICON_X: f32 = 42.8;
const TEXT_X: f32 = 73.9;
const CIRCLE_X: f32 = 327.6;
const CHEVRON_X: f32 = 329.4;
/// Hijos de una carpeta desplegada: corridos a la derecha.
const INDENT: f32 = 24.0;
/// Pie: «Cancelar» con la tinta desde 75 y «Listo» en su botón verde; línea base de los dos en
/// 491,2.
const CANCEL_X: f32 = 74.0;
const FOOT_BASE: f32 = 491.2;
const DONE: [f32; 4] = [195.8, 466.6, 349.4, 505.3];

/// Tamaños por la altura de las mayúsculas en la referencia (la letra de la referencia es más
/// ancha que Segoe UI: igualar anchos la hacía 1-2 px más alta).
const HEAD_FONT: f32 = 13.0;
const ROW_FONT: f32 = 16.6;
const SEARCH_FONT: f32 = 14.4;
const FOOT_FONT: f32 = 14.2;
const DONE_FONT: f32 = 15.6;

struct Ink {
    dark: bool,
    glass: Color32,
    line: Color32,
    text: Color32,
    icon: Color32,
    hint: Color32,
    cancel: Color32,
    field: Color32,
    hover: Color32,
}

fn ink(ctx: &egui::Context) -> Ink {
    let p = theme::palette(ctx);
    if p.dark {
        Ink {
            dark: true,
            glass: Color32::from_rgba_unmultiplied(34, 34, 34, 191),
            line: Color32::from_white_alpha(26),
            text: Color32::from_gray(224),
            icon: Color32::from_gray(143),
            hint: Color32::from_gray(101),
            cancel: Color32::from_gray(143),
            field: Color32::from_rgba_unmultiplied(0, 0, 0, 145),
            hover: Color32::from_white_alpha(10),
        }
    } else {
        Ink {
            dark: false,
            glass: Color32::from_rgba_unmultiplied(250, 250, 250, 232),
            line: p.border,
            text: p.text,
            icon: p.weak,
            hint: p.faint,
            cancel: p.weak,
            field: p.card2,
            hover: p.hover,
        }
    }
}

/// Lo que se ve en la lista, en orden.
enum Entry {
    Folder { id: String, name: String, open: bool },
    Playlist { id: String, name: String, indent: bool },
    /// Una carpeta desplegada sin playlists a las que se pueda añadir.
    Empty,
}

impl App {
    /// El panel, si está abierto (`add_dialog`).
    pub(super) fn add_panel(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.add_dialog.take() else {
            return;
        };
        let ink = ink(ctx);
        let screen = ctx.content_rect();
        let rect = match d.bar_x {
            Some(bx) => {
                let right = bx + RIGHT_OF_BUTTON;
                let bottom = screen.max.y - FROM_BOTTOM;
                Rect::from_min_size(pos2(right - W, (bottom - H).max(screen.min.y + 6.0)), vec2(W, H))
            }
            None => Rect::from_center_size(screen.center(), vec2(W, H)),
        };
        let o = rect.min;
        let at = |x: f32, y: f32| pos2(o.x + x, o.y + y);

        // Las playlists a las que se puede añadir: propias, colaborativas y, para quien colabora,
        // las de la biblioteca que no son listas de Spotify.
        let mine: Vec<Playlist> = self
            .playlists
            .iter()
            .filter(|pl| {
                let algorithmic = pl.owner.id.as_deref() == Some("spotify") || pl.id.starts_with("37i9");
                self.is_mine(pl) || pl.collaborative.unwrap_or(false) || !algorithmic
            })
            .cloned()
            .collect();
        let q = d.query.trim().to_lowercase();
        let matches = |s: &str| q.is_empty() || s.to_lowercase().contains(&q);
        let in_folder: HashSet<&str> = self.folders.iter().flat_map(|f| f.playlists.iter().map(String::as_str)).collect();
        let mut entries: Vec<Entry> = Vec::new();
        // Todas las carpetas, también las vacías (se ven y se despliegan como en la referencia).
        for f in &self.folders {
            let children: Vec<&Playlist> = mine.iter().filter(|pl| f.playlists.contains(&pl.id)).collect();
            let child_match = children.iter().any(|pl| matches(&pl.name));
            if !matches(&f.name) && !child_match {
                continue;
            }
            let open = d.open_folders.contains(&f.id) || (!q.is_empty() && child_match);
            entries.push(Entry::Folder { id: f.id.clone(), name: f.name.clone(), open });
            if open && children.is_empty() {
                entries.push(Entry::Empty);
            }
            if open {
                for pl in children {
                    if matches(&pl.name) || matches(&f.name) {
                        entries.push(Entry::Playlist { id: pl.id.clone(), name: pl.name.clone(), indent: true });
                    }
                }
            }
        }
        for pl in &mine {
            if !in_folder.contains(pl.id.as_str()) && matches(&pl.name) {
                entries.push(Entry::Playlist { id: pl.id.clone(), name: pl.name.clone(), indent: false });
            }
        }

        let mut keep = true;
        let mut done = false;
        let mut create: Option<String> = None;
        egui::Area::new(egui::Id::new("add_panel"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .show(ctx, |ui| {
                ui.allocate_rect(rect, Sense::click());
                let painter = ui.painter().clone();
                if ink.dark {
                    painter.add(egui::Shape::Callback(egui::epaint::PaintCallback {
                        rect,
                        callback: std::sync::Arc::new(crate::raster::BackdropBlur { sigma: BLUR_SIGMA, corner: RADIUS as f32 }),
                    }));
                }
                painter.rect_filled(rect, CornerRadius::same(RADIUS), ink.glass);

                // Cabecera.
                let g = painter.layout_no_wrap("Añadir a una playlist".into(), theme::semilight(HEAD_FONT), ink.text);
                text_on_baseline(&painter, at(HEAD_X - lead(&g), HEAD_BASE), g, ink.text);
                painter.line_segment([at(0.0, HEAD_LINE), at(W, HEAD_LINE)], Stroke::new(1.0, ink.line));

                // Buscador.
                let sb = Rect::from_min_max(at(SEARCH_BOX[0], SEARCH_BOX[1]), at(SEARCH_BOX[2], SEARCH_BOX[3]));
                painter.rect_filled(sb, CornerRadius::same(7), ink.field);
                paint_mask(&painter, "add-search", &lm::SEARCH, px(&painter, at(SEARCH_ICON.x - (lm::SEARCH.ox as f32 + 2.0 + 11.0), SEARCH_ICON.y - (lm::SEARCH.oy as f32 + 2.0 + 11.0))), ink.icon);
                if d.query.is_empty() {
                    let g = painter.layout_no_wrap("Buscar una playlist o carpeta".into(), theme::semilight(SEARCH_FONT), ink.hint);
                    text_on_baseline(&painter, at(SEARCH_TEXT_X - lead(&g), SEARCH_BASE), g, ink.hint);
                }
                let field = Rect::from_min_max(at(SEARCH_TEXT_X - 1.0, SEARCH_BOX[1] + 14.0), at(SEARCH_ICON.x - 18.0, SEARCH_BOX[3] - 14.0));
                let mut fc = ui.new_child(egui::UiBuilder::new().max_rect(field));
                fc.add_sized(
                    field.size(),
                    egui::TextEdit::singleline(&mut d.query)
                        .frame(egui::Frame::NONE)
                        .font(theme::semilight(SEARCH_FONT))
                        .text_color(ink.text)
                        .vertical_align(egui::Align::Center),
                );

                // Lista con desplazamiento entre el buscador y el pie.
                let list = Rect::from_min_max(at(0.0, LIST_TOP), at(W, FOOT_LINE - 0.5));
                let mut lc = ui.new_child(egui::UiBuilder::new().max_rect(list));
                lc.set_clip_rect(list);
                egui::ScrollArea::vertical().id_salt("add_panel_list").auto_shrink([false, false]).show(&mut lc, |ui| {
                    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                    ui.add_space(NEW_ROW_CENTER - NEW_ROW_H / 2.0 - LIST_TOP);
                    let x0 = o.x;
                    // «Nueva playlist» (o su nombre en edición).
                    let (row, r) = ui.allocate_exact_size(vec2(W, NEW_ROW_H), Sense::click());
                    let c = row.center().y;
                    match d.new_name.as_mut() {
                        None => {
                            if r.hovered() {
                                ui.painter().rect_filled(row.shrink2(vec2(8.0, 2.0)), CornerRadius::same(5), ink.hover);
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            plus(ui.painter(), pos2(x0 + ICON_X, c), ink.icon);
                            let g = ui.painter().layout_no_wrap("Nueva playlist".into(), theme::semilight(ROW_FONT), ink.text);
                            text_on_baseline(ui.painter(), pos2(x0 + TEXT_X - lead(&g), c + 6.5), g, ink.text);
                            if r.clicked() {
                                d.new_name = Some(String::new());
                                d.focus = true;
                            }
                        }
                        Some(name) => {
                            ui.painter().rect_filled(row.shrink2(vec2(8.0, 6.0)), CornerRadius::same(6), ink.field);
                            if name.trim().is_empty() {
                                plus(ui.painter(), pos2(x0 + ICON_X, c), ink.icon);
                            } else {
                                icons::paint(ui.painter(), Rect::from_center_size(pos2(x0 + ICON_X, c), vec2(16.0, 16.0)), GREEN, Icon::Check);
                            }
                            if name.is_empty() {
                                let g = ui.painter().layout_no_wrap("Nombre de la playlist".into(), theme::semilight(ROW_FONT), ink.hint);
                                text_on_baseline(ui.painter(), pos2(x0 + TEXT_X - lead(&g), c + 6.5), g, ink.hint);
                            }
                            let fr = Rect::from_min_max(pos2(x0 + TEXT_X - 1.0, c - 13.0), pos2(x0 + W - 24.0, c + 13.0));
                            let mut nc = ui.new_child(egui::UiBuilder::new().max_rect(fr));
                            let te = nc.add_sized(
                                fr.size(),
                                egui::TextEdit::singleline(name)
                                    .frame(egui::Frame::NONE)
                                    .font(theme::semilight(ROW_FONT))
                                    .text_color(ink.text)
                                    .vertical_align(egui::Align::Center),
                            );
                            if d.focus {
                                d.focus = false;
                                te.request_focus();
                            }
                            let enter = te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            let mut close_edit = false;
                            if enter && !name.trim().is_empty() {
                                create = Some(name.trim().to_string());
                                close_edit = true;
                            }
                            if te.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                close_edit = true;
                            }
                            if close_edit {
                                d.new_name = None;
                            }
                        }
                    }
                    if entries.is_empty() {
                        let g = ui.painter().layout_no_wrap(
                            if q.is_empty() { "Todavía no tienes playlists propias.".into() } else { "Nada coincide con tu búsqueda.".into() },
                            theme::semilight(FOOT_FONT),
                            ink.hint,
                        );
                        let y = ui.cursor().min.y + 30.0;
                        text_on_baseline(ui.painter(), pos2(x0 + TEXT_X - lead(&g), y), g, ink.hint);
                        ui.add_space(50.0);
                    }
                    for e in &entries {
                        let (row, r) = ui.allocate_exact_size(vec2(W, ROW_H), Sense::click());
                        if !ui.is_rect_visible(row) {
                            continue;
                        }
                        let base = row.min.y + ROW_H / 2.0 + 6.6;
                        if r.hovered() && !matches!(e, Entry::Empty) {
                            ui.painter().rect_filled(row.shrink2(vec2(8.0, 2.0)), CornerRadius::same(5), ink.hover);
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        match e {
                            Entry::Folder { id, name, open } => {
                                icons::paint_side(ui.painter(), pos2(x0 + ICON_X - 0.6, base - 4.7), 28.0, ink.icon, Icon::Folder);
                                let g = galley_truncated(ui.painter(), name, theme::semilight(ROW_FONT), ink.text, CIRCLE_X - TEXT_X - 24.0);
                                text_on_baseline(ui.painter(), pos2(x0 + TEXT_X - lead(&g), base), g, ink.text);
                                if *open {
                                    icons::paint_side(ui.painter(), pos2(x0 + CHEVRON_X, base - 6.0), 16.0, ink.icon, Icon::ChevronDown);
                                } else {
                                    // La flecha del menú de los tres puntos (su centro está 7 px a la
                                    // derecha de la esquina de su máscara), centrada en CHEVRON_X.
                                    let a = px(ui.painter(), pos2(x0 + CHEVRON_X - (mm::CHEVRON.ox as f32 + 7.0), base));
                                    paint_mask(ui.painter(), "add-chevron", &mm::CHEVRON, a, ink.icon);
                                }
                                if r.clicked() {
                                    if d.open_folders.contains(id) {
                                        d.open_folders.remove(id);
                                    } else {
                                        d.open_folders.insert(id.clone());
                                    }
                                }
                            }
                            Entry::Empty => {
                                let g = ui.painter().layout_no_wrap("Esta carpeta está vacía".into(), theme::semilight(ROW_FONT), ink.hint);
                                text_on_baseline(ui.painter(), pos2(x0 + TEXT_X + INDENT - lead(&g), base), g, ink.hint);
                            }
                            Entry::Playlist { id, name, indent } => {
                                let dx = if *indent { INDENT } else { 0.0 };
                                icons::paint_side(ui.painter(), pos2(x0 + ICON_X + dx, base - 7.1), 28.0, ink.icon, Icon::Playlist);
                                let g = galley_truncated(ui.painter(), name, theme::semilight(ROW_FONT), ink.text, CIRCLE_X - TEXT_X - dx - 24.0);
                                text_on_baseline(ui.painter(), pos2(x0 + TEXT_X + dx - lead(&g), base), g, ink.text);
                                let sel = d.selected.contains(id);
                                let cc = pos2(x0 + CIRCLE_X, base - 6.6);
                                if sel {
                                    ui.painter().circle_filled(cc, 11.15, GREEN);
                                    icons::paint(ui.painter(), Rect::from_center_size(cc, vec2(12.0, 12.0)), Color32::BLACK, Icon::Check);
                                } else {
                                    let col = if r.hovered() { ink.text } else { ink.icon };
                                    ui.painter().circle_stroke(cc, 10.4, Stroke::new(1.5, col));
                                }
                                if r.clicked() {
                                    if sel {
                                        d.selected.remove(id);
                                    } else {
                                        d.selected.insert(id.clone());
                                    }
                                }
                            }
                        }
                    }
                    ui.add_space(8.0);
                });

                // Pie.
                painter.line_segment([at(0.0, FOOT_LINE), at(W, FOOT_LINE)], Stroke::new(1.0, ink.line));
                let cancel_hit = Rect::from_min_max(at(22.0, DONE[1]), at(DONE[0] - 14.0, DONE[3]));
                let cr = ui.interact(cancel_hit, egui::Id::new("add_panel_cancel"), Sense::click());
                if cr.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                let cc = if cr.hovered() { ink.text } else { ink.cancel };
                let g = painter.layout_no_wrap("Cancelar".into(), theme::semilight(FOOT_FONT), cc);
                text_on_baseline(&painter, at(CANCEL_X - lead(&g), FOOT_BASE), g, cc);
                if cr.clicked() {
                    keep = false;
                }
                let db = Rect::from_min_max(at(DONE[0], DONE[1]), at(DONE[2], DONE[3]));
                let dr = ui.interact(db, egui::Id::new("add_panel_done"), Sense::click());
                if dr.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                let green = if dr.hovered() { GREEN.lerp_to_gamma(Color32::WHITE, 0.12) } else { GREEN };
                painter.rect_filled(db, CornerRadius::same(7), green);
                let n = d.selected.len();
                let label = if n > 1 { format!("Listo ({n})") } else { "Listo".to_string() };
                let dark = Color32::from_gray(17);
                let g = painter.layout_no_wrap(label, theme::semilight(DONE_FONT), dark);
                let w = g.size().x;
                text_on_baseline(&painter, pos2(db.center().x - w / 2.0, o.y + FOOT_BASE), g, dark);
                if dr.clicked() {
                    done = true;
                }
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
        // Escape (sin estar escribiendo un nombre) o un clic fuera lo cierran, salvo en el botón
        // que lo abrió (ese lo cierra al volver a pulsarlo).
        let outside = ctx.input(|i| {
            i.pointer.any_pressed()
                && i.pointer.interact_pos().is_some_and(|p| !rect.contains(p) && !d.button.is_some_and(|b| b.contains(p)))
        });
        if (ctx.input(|i| i.key_pressed(egui::Key::Escape)) && d.new_name.is_none()) || outside {
            keep = false;
        }
        if done {
            let n = d.selected.len();
            for pid in &d.selected {
                for uri in &d.uris {
                    self.actions.push(Action::AddToPlaylist { playlist_id: pid.clone(), uri: uri.clone() });
                }
            }
            if n > 0 {
                self.status(if n == 1 { "Añadido a la playlist".to_string() } else { format!("Añadido a {n} playlists") });
            }
            keep = false;
        }
        if keep {
            self.add_dialog = Some(d);
        }
    }
}

/// «+» de «Nueva playlist»: dos trazos de 15,2 px.
fn plus(painter: &egui::Painter, c: egui::Pos2, color: Color32) {
    let s = 7.6;
    painter.line_segment([pos2(c.x - s, c.y), pos2(c.x + s, c.y)], Stroke::new(1.6, color));
    painter.line_segment([pos2(c.x, c.y - s), pos2(c.x, c.y + s)], Stroke::new(1.6, color));
}

/// Margen a la izquierda de la tinta del primer glifo.
fn lead(g: &egui::Galley) -> f32 {
    g.rows.first().and_then(|r| r.row.glyphs.first()).map(|gl| gl.pos.x + gl.uv_rect.offset.x).unwrap_or(0.0)
}
