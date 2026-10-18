//! «Canciones que te gustan», copia de su referencia de diseño (1.png, 17 oct 2026): el panel
//! negro con filo, la barra de la biblioteca (lista o cuadrícula, orden, filtro, lupa y «+») y,
//! debajo, la tabla de canciones a todo lo ancho, sin cabecera grande ni columna de la derecha.
//! Las medidas son las de la referencia en una ventana con cliente de 1772 × 1029 (la misma
//! escala que la biblioteca): la barra cae donde la de la biblioteca, así que se comparten.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use egui::{pos2, vec2, Color32, CornerRadius, Rect, Sense};

use super::super::icons::Icon;
use super::super::theme;
use super::super::widgets::{galley_truncated, text_on_baseline, RowOpts};
use super::super::{Action, App, Page, PlayTarget, CONTENT_PAD_LEFT, CONTENT_PAD_RIGHT, LIKED};
use super::liked_masks as km;
use super::{ink_left, ink_right, lm, paint_mask, px, style, FolderDialog};
use super::{BAR_BASE, BAR_FONT, CARD, COVER_W, GAP_GROUP_SEARCH, GAP_GROUP_TEXT, GAP_SORT_GROUP, GRID_C, GRID_LEFT, GRID_RIGHT, LIST_C};
use super::{PITCH_X, PITCH_Y, SORT_TEXT_X, SUB_FONT, SUB_GAP, SUB_LINE, TITLE_FONT, VIEW_BG_DY, VIEW_BG_H, VIEW_BG_W};
use crate::api::Req;
use crate::model::Track;

/// Arriba de la cabecera de la tabla (su «#», «Título»…), desde el borde del panel: la línea
/// separadora queda a 59,3 de ahí, en la y 175 de la referencia.
const TABLE_TOP: f32 = 54.1;
/// Borde izquierdo de la tabla (su «#» centrado 20,5 más allá) y derecho de la línea
/// separadora, desde los del panel (309,5 y 1729,5 en la referencia).
const TABLE_LEFT: f32 = 35.0;
const TABLE_RIGHT: f32 = 38.5;
/// El «+» queda 2 px más a la izquierda que en la biblioteca.
const PLUS_DX: f32 = -2.0;
/// La cuadrícula empieza como la de la biblioteca (60 bajo el panel, la primera fila a 80,5).
const GRID_TOP: f32 = 60.0;
const GRID_FIRST: f32 = 20.48;
/// Portada de una canción en la cuadrícula y línea base de su título (las de un álbum).
const GRID_COVER_TOP: f32 = 9.0;
const GRID_TITLE_BASE: f32 = 199.55;

/// Izquierda del icono de orden, desde el borde del panel (434 − 275 en la referencia): de ahí
/// cuelga su menú, como el de «Agrupar» del suyo.
const SORT_ICON_X: f32 = 159.0;

/// Órdenes del botón «Recientes» (`Settings::liked_sort`).
const SORTS: [&str; 6] = ["Recientes", "Antiguas", "Título", "Artista", "Álbum", "Duración"];
/// Filas del menú de filtro (`Settings::liked_filter`: 0 todo, 1 un artista, 2 un género).
const FILTERS: [&str; 3] = ["Todo", "Artistas", "Géneros"];

/// Menú de filtro (2.png, misma escala que la página, medido por el antialias de sus bordes):
/// 18,84 px a la izquierda de la tinta de su icono y 57,1 bajo el borde del panel; 191,5 de ancho
/// y 28,8 de margen arriba y abajo; filas cada 50,5 con la primera centrada a 29,38 del borde (así caen las bases de todas sus filas),
/// texto Semilight 17 a 25,3. Sus submenús (artistas, géneros) a 198,29 de su borde izquierdo y
/// 3,97 por encima de la fila que los abre. El cristal, gris 31 sobre el negro del panel (38 con
/// la misma transparencia del de «Agrupar»).
const FM_DX: f32 = -18.84;
const FM_DY: f32 = 57.1;
const FM_W: f32 = 191.5;
const FM_EDGE: f32 = 28.8;
const FM_PAD: f32 = 29.38;
const FM_ROW: f32 = 50.5;
const FM_TEXT_X: f32 = 25.3;
const FM_FONT: f32 = 17.0;
const FM_SUB_DX: f32 = 198.29;
const FM_SUB_DY: f32 = -3.97;
/// En el submenú el texto va medio píxel más abajo respecto de su borde (sus dos filas de la
/// referencia, a 34,87 y 84,87 del borde; las del menú, a 33,9).
const FM_SUB_TEXT_DY: f32 = 0.5;
const FM_FILL: u8 = 38;
const FM_TEXT: u8 = 211;
const FM_CHEVRON: u8 = 142;
/// El submenú no baja de aquí (desde el borde de abajo del panel); con más filas, se desplaza.
const FM_SUB_BOTTOM: f32 = 16.0;

/// «Playlist dinámica» (3.png): el texto acaba 52 px antes de la tinta del «+» (107 antes de su
/// ancla) y el rayo va 28 antes del texto; texto verde apagado y rayo verde.
const DYN_TEXT_RIGHT: f32 = -107.0;
const DYN_TEXT: Color32 = Color32::from_rgb(131, 164, 148);
const DYN_TEXT_HOVER: Color32 = Color32::from_rgb(170, 214, 188);
const DYN_BOLT: Color32 = Color32::from_rgb(65, 210, 124);

/// Lo que define la vista ordenada y filtrada que se guarda entre fotogramas: versión y largo de
/// la lista, búsqueda, orden, tipo y valor del filtro y versión de los géneros.
pub(in crate::app) type LikedViewKey = (u64, usize, String, u8, u8, String, u64);

/// Una playlist dinámica: la playlist `id` lleva las canciones que te gustan del artista o del
/// género del filtro (`kind` 1 o 2, `value` su id) y se pone al día sola cuando cambian
/// (`sync_dynamic_playlists`). `synced`: lo que Nanofy añadió, para quitar solo eso (lo que añadas
/// tú a mano no se toca). Se guardan aparte de los ajustes (`dynamic_playlists.json`), para que
/// guardar la página de ajustes no las pise.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(in crate::app) struct DynamicPlaylist {
    pub id: String,
    pub name: String,
    pub kind: u8,
    pub value: String,
    pub label: String,
    #[serde(default)]
    pub synced: Vec<String>,
}

pub(in crate::app) fn load_dynamic(path: &PathBuf) -> Vec<DynamicPlaylist> {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Una fila de los menús de la página.
struct FmItem {
    text: String,
    chevron: bool,
    green: bool,
    dim: bool,
}

impl FmItem {
    fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), chevron: false, green: false, dim: false }
    }
}

/// Rectángulo de un menú de `n` filas con la esquina en `min` (sin redondear: sus bordes caen
/// entre píxeles como en la referencia).
fn fm_rect(min: egui::Pos2, n: usize) -> Rect {
    Rect::from_min_size(min, vec2(FM_W, 2.0 * FM_EDGE + FM_ROW * n.saturating_sub(1) as f32))
}

/// El cristal de los menús: lo de detrás desenfocado (como el de «Agrupar») y el velo.
fn fm_glass(painter: &egui::Painter, rect: Rect, dark: bool) {
    if dark {
        painter.add(egui::Shape::Callback(egui::epaint::PaintCallback {
            rect,
            callback: std::sync::Arc::new(crate::raster::BackdropBlur { sigma: 56.0, corner: 7.0 }),
        }));
    }
    let glass = if dark { Color32::from_rgba_unmultiplied(FM_FILL, FM_FILL, FM_FILL, 191) } else { Color32::from_rgba_unmultiplied(250, 250, 250, 232) };
    painter.rect_filled(rect, CornerRadius::same(7), glass);
}

/// Una fila de menú centrada en `cy` dentro de `rect` (su menú): fondo al pasar el ratón, texto y,
/// si abre un submenú, su «›». `chevron_row`: qué fila es, para bajar el «›» las que hagan falta.
/// Devuelve la respuesta de la fila.
fn fm_row(ui: &mut egui::Ui, id: egui::Id, rect: Rect, cy: f32, item: &FmItem, chevron_row: usize) -> egui::Response {
    let p = theme::palette(ui.ctx());
    let painter = ui.painter().clone();
    let row = Rect::from_min_max(pos2(rect.min.x + 8.0, cy - FM_ROW / 2.0 + 1.0), pos2(rect.max.x - 8.0, cy + FM_ROW / 2.0 - 1.0));
    let r = ui.interact(row, id, if item.dim { Sense::hover() } else { Sense::click() });
    // Las que abren un submenú no se resaltan (2.png: el ratón sobre «Genres», sin fondo).
    if r.hovered() && !item.dim && !item.chevron {
        painter.rect_filled(row, CornerRadius::same(4), if p.dark { Color32::from_white_alpha(10) } else { p.hover });
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let color = if item.dim {
        if p.dark { Color32::from_gray(110) } else { p.faint }
    } else if item.green {
        theme::GREEN
    } else if p.dark {
        Color32::from_gray(FM_TEXT)
    } else {
        p.text
    };
    let max_w = FM_W - FM_TEXT_X - if item.chevron { 48.0 } else { 16.0 };
    let g = galley_truncated(&painter, &item.text, theme::semilight(FM_FONT), color, max_w);
    let lead = ink_left(&g);
    text_on_baseline(&painter, pos2((rect.min.x + FM_TEXT_X).round() - lead, cy + 6.0), g, color);
    if item.chevron {
        let c = if p.dark { Color32::from_gray(FM_CHEVRON) } else { p.weak };
        // Los «›» de la referencia van cada 50 px justos (las filas, cada 50,5).
        let anchor = px(&painter, pos2(rect.min.x, rect.min.y + (FM_ROW * (chevron_row as f32 - 1.0)).floor()));
        paint_mask(&painter, "fm_chevron", &km::CHEVRON, anchor, c);
    }
    r
}

impl App {
    /// La página entera en el panel `panel` (como `library_panel`): la barra fija y la tabla (o
    /// la cuadrícula) debajo, que se desplaza.
    pub(in crate::app) fn liked_panel(&mut self, ui: &mut egui::Ui, panel: Rect) {
        self.request_once(LIKED, Req::Liked);
        if self.list_search_id != LIKED {
            self.list_search_id = LIKED.to_string();
            self.album_search.clear();
            self.album_search_open = false;
        }
        self.liked_genres_tick();
        let list = self.lists.remove(LIKED).unwrap_or_default();
        let origin = pos2(panel.min.x.round(), panel.min.y.round());
        let grid = self.settings.liked_grid;

        let view = self.liked_view(&list.tracks, list.gen);
        let shown: &[Track] = view.as_deref().unwrap_or(&list.tracks);
        let filtered = self.liked_filter_on().is_some();

        let selecting = self.sel_list == LIKED && !self.sel.is_empty();
        if selecting {
            // Con canciones elegidas, su barra en el sitio de la de herramientas.
            let bar = Rect::from_min_max(pos2(panel.min.x + CONTENT_PAD_LEFT, origin.y + 8.0), pos2(panel.max.x - CONTENT_PAD_RIGHT, origin.y + 52.0));
            let mut c = ui.new_child(egui::UiBuilder::new().max_rect(bar));
            self.selection_bar(&mut c, LIKED, shown);
        }

        let top = origin.y + if grid { GRID_TOP } else { TABLE_TOP };
        let area = Rect::from_min_max(pos2(panel.min.x, top), pos2(panel.max.x, panel.max.y - 1.0));
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(area));
        c.set_clip_rect(area.intersect(panel.shrink(1.0)));
        let salt = format!("liked-{grid}");
        egui::ScrollArea::vertical().id_salt(salt).auto_shrink([false, false]).show(&mut c, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            if list.loading && list.tracks.is_empty() {
                ui.add_space(super::super::widgets::TABLE_HEAD_H);
                Self::skeleton_rows(ui, 8, super::super::widgets::TABLE_ROW_H, 50.0, 43.0);
            } else if shown.is_empty() {
                let st = style(ui.ctx());
                let text = if !self.album_search.trim().is_empty() {
                    "Nada coincide con tu búsqueda"
                } else if filtered {
                    "Ninguna canción que te gusta tiene este filtro"
                } else {
                    "Las canciones que te gusten aparecerán aquí"
                };
                ui.add_space(20.0);
                let g = ui.painter().layout_no_wrap(text.into(), theme::regular(14.0), st.sub);
                text_on_baseline(ui.painter(), pos2(panel.min.x + CONTENT_PAD_LEFT, ui.cursor().min.y + 20.0), g, st.sub);
            } else if grid {
                self.liked_grid(ui, origin, panel, shown);
            } else {
                // La tabla a todo lo ancho (la línea y el resaltado de las filas), con las columnas
                // donde caerían con la columna de la derecha de las demás listas: así en la
                // referencia, con «Álbum» en 672 y «Duración» en 984.
                let left = panel.min.x + TABLE_LEFT;
                let w = (panel.max.x - TABLE_RIGHT - left).max(200.0);
                let cols_w = (panel.width() - CONTENT_PAD_LEFT - CONTENT_PAD_RIGHT - super::super::pages::INFO_W - super::super::pages::INFO_GAP).max(400.0);
                let mut row = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(left, ui.cursor().min.y), vec2(w, f32::INFINITY))));
                row.spacing_mut().item_spacing = vec2(0.0, 0.0);
                row.set_width(w);
                // Filtrada, cada canción con su número en la lista entera (3.png: 1, 2, 5, 7).
                self.track_rows(&mut row, LIKED, shown, RowOpts { header: true, select: true, numbered: filtered, cols_w: Some(cols_w.min(w)), ..RowOpts::tracks(true, true) });
                let used = row.min_rect().height();
                ui.allocate_exact_size(vec2(panel.width() - 2.0, used + 24.0), Sense::hover());
            }
        });
        self.lists.insert(LIKED.to_string(), list);
        // La barra, con la lista ya en su sitio: los submenús del filtro sacan de ella sus opciones.
        if !selecting {
            self.liked_toolbar(ui, origin, panel);
        }
    }

    /// Las canciones ordenadas y filtradas según la barra, más la búsqueda de la lupa. `None` si
    /// queda la lista tal cual (de fábrica: «Recientes» y «Todo», sin buscar), sin copiarla.
    /// Con miles de canciones se guarda y solo se rehace si cambia algo de lo que la define.
    fn liked_view(&mut self, tracks: &[Track], gen: u64) -> Option<Rc<[Track]>> {
        let sort = self.settings.liked_sort.min(SORTS.len() as u8 - 1);
        let filter = self.liked_filter_on();
        let q = self.list_query(LIKED);
        if sort == 0 && filter.is_none() && q.is_empty() {
            self.liked_view = None;
            return None;
        }
        let (kind, value) = filter.clone().unwrap_or((0, String::new()));
        let key: LikedViewKey = (gen, tracks.len(), q.clone(), sort, kind, value.clone(), self.liked_genres_ver);
        if let Some((k, v)) = &self.liked_view {
            if *k == key {
                return Some(v.clone());
            }
        }
        let mut v: Vec<Track> = tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                q.is_empty()
                    || t.name.to_lowercase().contains(&q)
                    || t.artists.iter().any(|a| a.name.to_lowercase().contains(&q))
                    || t.album.as_ref().is_some_and(|a| a.name.to_lowercase().contains(&q))
            })
            .filter(|(_, t)| kind == 0 || self.liked_match(t, kind, &value) == Some(true))
            .map(|(i, t)| {
                let mut t = t.clone();
                // Con filtro se ve su número en la lista entera (ver `numbered` en la tabla).
                if kind != 0 {
                    t.track_number = Some(i as u32 + 1);
                }
                t
            })
            .collect();
        // Por texto, sin distinguir mayúsculas y con lo que no tiene nombre (canciones retiradas)
        // al final.
        let text_key = |s: Option<&str>| {
            let s = s.unwrap_or("").trim();
            (s.is_empty(), s.to_lowercase())
        };
        match sort {
            1 => v.reverse(),
            2 => v.sort_by_cached_key(|t| text_key(Some(&t.name))),
            3 => v.sort_by_cached_key(|t| text_key(t.artists.first().map(|a| a.name.as_str()))),
            4 => v.sort_by_cached_key(|t| text_key(t.album.as_ref().map(|a| a.name.as_str()))),
            5 => v.sort_by_key(|t| t.duration_ms),
            _ => {}
        }
        let v: Rc<[Track]> = v.into();
        self.liked_view = Some((key, v.clone()));
        Some(v)
    }

    /// La barra de la biblioteca (mismas máscaras y medidas, ver `library_toolbar`), con orden y
    /// filtro propios de las canciones.
    fn liked_toolbar(&mut self, ui: &mut egui::Ui, o: egui::Pos2, panel: Rect) {
        let st = style(ui.ctx());
        let painter = ui.painter().clone();
        let at = |x: f32, y: f32| pos2(o.x + x, o.y + y);
        let hand = |ui: &egui::Ui, r: &egui::Response| {
            if r.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        };

        // Lista y cuadrícula.
        let grid = self.settings.liked_grid;
        let on = if grid { GRID_C } else { LIST_C };
        let bg = Rect::from_center_size(at(on.x, on.y + VIEW_BG_DY), vec2(VIEW_BG_W, VIEW_BG_H));
        painter.rect_filled(bg, CornerRadius::same(6), st.view_bg);
        let list_r = ui.interact(Rect::from_center_size(at(LIST_C.x, LIST_C.y), vec2(40.0, 40.0)), ui.id().with("liked_list"), Sense::click());
        let grid_r = ui.interact(Rect::from_center_size(at(GRID_C.x, GRID_C.y), vec2(40.0, 40.0)), ui.id().with("liked_grid"), Sense::click());
        hand(ui, &list_r);
        hand(ui, &grid_r);
        let tone = |sel: bool, hov: bool| if sel { st.icon_on } else if hov { st.hover } else { st.icon };
        paint_mask(&painter, "list", &lm::LIST, px(&painter, o), tone(!grid, list_r.hovered()));
        paint_mask(&painter, "grid", &lm::GRID, px(&painter, o), tone(grid, grid_r.hovered()));
        if list_r.on_hover_text("Lista").clicked() && grid {
            self.set_liked_grid(false);
        }
        if grid_r.on_hover_text("Cuadrícula").clicked() && !grid {
            self.set_liked_grid(true);
        }

        // Orden: el nombre del elegido; al pulsar, la lista de órdenes.
        let sort = (self.settings.liked_sort as usize).min(SORTS.len() - 1);
        let g = painter.layout_no_wrap(SORTS[sort].into(), theme::semibold(BAR_FONT), st.bar_text);
        let ink = ink_left(&g);
        let text_w = g.size().x - ink - ink_right(&g);
        let sort_hit = Rect::from_min_max(at(157.0, 9.0), at(SORT_TEXT_X + text_w + 8.0, 50.0));
        let sort_r = ui.interact(sort_hit, ui.id().with("liked_sort"), Sense::click());
        hand(ui, &sort_r);
        let hov = sort_r.hovered();
        let open = self.liked_sort_open;
        paint_mask(&painter, "sort", &lm::SORT, px(&painter, o), if open { st.icon_on } else if hov { st.hover } else { st.icon });
        text_on_baseline(&painter, at(SORT_TEXT_X - ink, BAR_BASE), g, if open { st.icon_on } else if hov { st.hover } else { st.bar_text });
        if sort_r.clicked() {
            self.liked_sort_open = !self.liked_sort_open;
            self.liked_filter_open = false;
        }
        if self.liked_sort_open {
            let ctx = ui.ctx().clone();
            self.liked_sort_menu(&ctx, at(SORT_ICON_X + FM_DX, FM_DY), sort_hit, sort);
        }

        // Filtro: su icono (el de «Agrupar» de la biblioteca) a 46 px del texto de orden.
        let filter = self.liked_filter_on();
        let filter_x = SORT_TEXT_X + text_w + GAP_SORT_GROUP;
        let dx = filter_x - (lm::GROUP.ox as f32 + 2.0);
        let label = match &filter {
            Some((1, _)) => format!("Filtro: Artista ({})", self.settings.liked_filter_label),
            Some(_) => format!("Filtro: Género ({})", self.settings.liked_filter_label),
            None => "Filtro: Todo".to_string(),
        };
        let fa = painter.layout_no_wrap(label, theme::semibold(BAR_FONT), st.bar_text);
        let fa_ink = ink_left(&fa);
        let fa_w = fa.size().x - fa_ink - ink_right(&fa);
        let ftext_x = filter_x + 20.0 + GAP_GROUP_TEXT;
        let filter_hit = Rect::from_min_max(at(filter_x - 8.0, 9.0), at(ftext_x + fa_w + 8.0, 50.0));
        let filter_r = ui.interact(filter_hit, ui.id().with("liked_filter"), Sense::click());
        hand(ui, &filter_r);
        let hov = filter_r.hovered();
        // En blanco con un filtro puesto, como «Agrupar» con uno elegido.
        let active = filter.is_some() || self.liked_filter_open;
        let c = if active { st.icon_on } else if hov { st.hover } else { st.icon };
        paint_mask(&painter, "group", &lm::GROUP, px(&painter, at(dx, 0.0)), c);
        let tc = if active { st.icon_on } else if hov { st.hover } else { st.bar_text };
        text_on_baseline(&painter, at(ftext_x - fa_ink, BAR_BASE), fa, tc);
        if filter_r.clicked() {
            self.liked_filter_open = !self.liked_filter_open;
            self.liked_filter_sub = None;
            self.liked_sort_open = false;
        }
        if self.liked_filter_open {
            let ctx = ui.ctx().clone();
            self.liked_filter_menu(&ctx, at(filter_x + FM_DX, FM_DY), filter_hit, panel);
        }

        // Lupa: la búsqueda de la lista (la misma de las demás listas de canciones).
        let search_x = ftext_x + fa_w + GAP_GROUP_SEARCH;
        let sdx = search_x - (lm::SEARCH.ox as f32 + 2.0);
        let s_hit = Rect::from_min_size(at(search_x - 9.0, 9.0), vec2(40.0, 41.0));
        let open = self.album_search_open || !self.album_search.is_empty();
        if open {
            let field_bg = Rect::from_min_max(at(search_x - 12.0, 10.0), at(search_x + 260.0, 48.0));
            painter.rect_filled(field_bg, CornerRadius::same(6), st.field_bg);
        }
        let s_r = ui.interact(s_hit, ui.id().with("liked_search"), Sense::click());
        hand(ui, &s_r);
        let c = if open { st.icon_on } else if s_r.hovered() { st.hover } else { st.icon };
        paint_mask(&painter, "search", &lm::SEARCH, px(&painter, at(sdx, 0.0)), c);
        if s_r.on_hover_text("Buscar en Canciones que te gustan").clicked() {
            self.album_search_open = !open;
            self.album_search_focus = self.album_search_open;
            if !self.album_search_open {
                self.album_search.clear();
            }
        }
        if open {
            let field = Rect::from_min_max(at(search_x + 32.0, 16.0), at(search_x + 250.0, 42.0));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(field));
            let edit = egui::TextEdit::singleline(&mut self.album_search)
                .frame(egui::Frame::NONE)
                .font(theme::regular(14.0))
                .text_color(st.field_text)
                .desired_width(field.width())
                .vertical_align(egui::Align::Center)
                .hint_text(egui::RichText::new("Buscar en Canciones que te gustan").color(st.field_hint));
            let r = child.add_sized(field.size(), edit);
            if self.album_search_focus {
                self.album_search_focus = false;
                r.request_focus();
            }
            if r.lost_focus() && self.album_search.is_empty() {
                self.album_search_open = false;
            }
            if r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.album_search.clear();
                self.album_search_open = false;
            }
        }

        let po = pos2(panel.max.x.round() + PLUS_DX, o.y);
        if filter.is_some() {
            let g = painter.layout_no_wrap("Playlist dinámica".into(), theme::semibold(BAR_FONT), DYN_TEXT);
            let lead = ink_left(&g);
            let w = g.size().x - lead - ink_right(&g);
            let right = po.x + DYN_TEXT_RIGHT;
            let left = right - w;
            let hit = Rect::from_min_max(pos2(left - 40.0, o.y + 9.0), pos2(right + 8.0, o.y + 50.0));
            let r = ui.interact(hit, ui.id().with("liked_dynamic"), Sense::click());
            hand(ui, &r);
            let hov = r.hovered();
            let bolt = if hov { Color32::from_rgb(110, 235, 160) } else { DYN_BOLT };
            paint_mask(&painter, "bolt", &km::BOLT, px(&painter, pos2(left, o.y)), bolt);
            text_on_baseline(&painter, pos2(left - lead, o.y + BAR_BASE), g, if hov { DYN_TEXT_HOVER } else { DYN_TEXT });
            let exists = self.liked_dynamic_rule().is_some();
            let tip = if exists { "Abrir su playlist dinámica" } else { "Guardar como playlist dinámica: se pone al día sola cuando cambian tus canciones que te gustan" };
            if r.on_hover_text(tip).clicked() {
                self.liked_dynamic_click();
            }
        }

        // «+»: crear playlist o carpeta, como en la biblioteca.
        let plus_hit = Rect::from_center_size(pos2(po.x + lm::PLUS.ox as f32 + 2.0 + 8.5, o.y + 29.0), vec2(40.0, 40.0));
        let plus_r = ui.interact(plus_hit, ui.id().with("liked_plus"), Sense::click());
        hand(ui, &plus_r);
        paint_mask(&painter, "plus", &lm::PLUS, px(&painter, po), if plus_r.hovered() { st.hover } else { st.icon });
        let plus_r = plus_r.on_hover_text("Crear");
        egui::Popup::menu(&plus_r).show(|ui| {
            ui.set_min_width(220.0);
            if Self::menu_item(ui, Some(Icon::Playlist), "Playlist", false).clicked() {
                self.actions.push(Action::OpenEditor(None));
                ui.close();
            }
            if Self::menu_item(ui, Some(Icon::Folder), "Carpeta", false).clicked() {
                self.folder_dialog = Some(FolderDialog { id: None, name: String::new(), playlist: None, busy: false });
                ui.close();
            }
        });
    }

    /// El filtro en uso (tipo y valor), o `None` si se ven todas (de unos ajustes viejos, un
    /// tipo sin valor cuenta como «Todo»).
    pub(in crate::app) fn liked_filter_on(&self) -> Option<(u8, String)> {
        let kind = self.settings.liked_filter;
        let value = &self.settings.liked_filter_value;
        (matches!(kind, 1 | 2) && !value.is_empty()).then(|| (kind, value.clone()))
    }

    /// ¿Entra la canción en el filtro? Por artista, si es uno de los suyos; por género, si su
    /// álbum lo tiene en Deezer (`None` si aún no se conocen sus géneros).
    fn liked_match(&self, t: &Track, kind: u8, value: &str) -> Option<bool> {
        match kind {
            1 => Some(t.artists.iter().any(|a| a.id.as_deref() == Some(value))),
            2 => {
                let g: u32 = value.parse().ok()?;
                t.id.as_ref().and_then(|i| self.liked_genres.get(i)).map(|gs| gs.contains(&g))
            }
            _ => Some(true),
        }
    }

    /// Opciones del submenú `kind` (1 artistas, 2 géneros): (valor, nombre), de los que más
    /// canciones tienen a los que menos; y, si aún se buscan géneros, una nota para el final.
    pub(in crate::app) fn liked_filter_options(&self, kind: u8) -> (Vec<(String, String)>, Option<String>) {
        let mut count: HashMap<String, (String, usize)> = HashMap::new();
        if let Some(list) = self.lists.get(LIKED) {
            for t in &list.tracks {
                if kind == 1 {
                    for a in &t.artists {
                        if let Some(id) = &a.id {
                            count.entry(id.clone()).or_insert_with(|| (a.name.clone(), 0)).1 += 1;
                        }
                    }
                } else if let Some(gs) = t.id.as_ref().and_then(|i| self.liked_genres.get(i)) {
                    for g in gs {
                        let name = self.genre_names.get(g).cloned().unwrap_or_else(|| g.to_string());
                        count.entry(g.to_string()).or_insert_with(|| (name, 0)).1 += 1;
                    }
                }
            }
        }
        let mut v: Vec<(String, String, usize)> = count.into_iter().map(|(k, (name, n))| (k, name, n)).collect();
        v.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
        let note = match (kind, self.liked_genres_pending) {
            (2, Some(n)) if n > 0 && n != usize::MAX => Some(format!("Buscando géneros… ({n})")),
            (2, Some(n)) if n > 0 => Some("Buscando géneros…".to_string()),
            (2, None) => Some("Buscando géneros…".to_string()),
            _ => None,
        };
        (v.into_iter().map(|(k, name, _)| (k, name)).collect(), note)
    }

    /// El menú de orden, con el cristal y el sitio del de filtro (respecto de su icono).
    fn liked_sort_menu(&mut self, ctx: &egui::Context, min: egui::Pos2, button: Rect, sort: usize) {
        let dark = theme::palette(ctx).dark;
        let rect = fm_rect(min, SORTS.len());
        let mut pick = None;
        egui::Area::new(egui::Id::new("liked_sort_menu")).order(egui::Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
            ui.allocate_rect(rect, Sense::click());
            fm_glass(ui.painter(), rect, dark);
            for (k, name) in SORTS.iter().enumerate() {
                let item = FmItem { green: k == sort, ..FmItem::new(*name) };
                if fm_row(ui, egui::Id::new(("liked_sort_row", k)), rect, rect.min.y + FM_PAD + FM_ROW * k as f32, &item, 0).clicked() {
                    pick = Some(k);
                }
            }
        });
        if let Some(k) = pick {
            self.settings.liked_sort = k as u8;
            self.settings.save(&self.paths);
        }
        let outside = ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !rect.contains(p) && !button.contains(p)));
        if pick.is_some() || outside || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.liked_sort_open = false;
        }
    }

    /// El menú de filtro (2.png): «Todo», «Artistas ›» y «Géneros ›»; al pasar por estas dos, su
    /// submenú a la derecha con las opciones de tus canciones (con muchas, se desplaza).
    fn liked_filter_menu(&mut self, ctx: &egui::Context, min: egui::Pos2, button: Rect, panel: Rect) {
        let dark = theme::palette(ctx).dark;
        let rect = fm_rect(min, FILTERS.len());
        let mut pick: Option<(u8, String, String)> = None;
        let mut sub = self.liked_filter_sub;
        egui::Area::new(egui::Id::new("liked_filter_menu")).order(egui::Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
            ui.allocate_rect(rect, Sense::click());
            fm_glass(ui.painter(), rect, dark);
            for (k, name) in FILTERS.iter().enumerate() {
                let item = FmItem { chevron: k > 0, ..FmItem::new(*name) };
                let r = fm_row(ui, egui::Id::new(("liked_filter_row", k)), rect, rect.min.y + FM_PAD + FM_ROW * k as f32, &item, k);
                if k > 0 && (r.hovered() || r.clicked()) {
                    sub = Some(k as u8);
                }
                if k == 0 && r.clicked() {
                    pick = Some((0, String::new(), String::new()));
                }
                if k == 0 && r.hovered() {
                    sub = None;
                }
            }
        });
        let mut sub_rect = Rect::NOTHING;
        if let Some(kind) = sub {
            let (options, note) = self.liked_filter_options(kind);
            let mut items: Vec<(Option<(String, String)>, FmItem)> = options.into_iter().map(|(v, name)| (Some((v, name.clone())), FmItem::new(name))).collect();
            if let Some(n) = note {
                items.push((None, FmItem { dim: true, ..FmItem::new(n) }));
            }
            if items.is_empty() {
                items.push((None, FmItem { dim: true, ..FmItem::new("Nada aún") }));
            }
            let smin = pos2(rect.min.x + FM_SUB_DX, rect.min.y + FM_ROW * kind as f32 + FM_SUB_DY);
            let room = ((panel.max.y - FM_SUB_BOTTOM - smin.y - 2.0 * FM_EDGE) / FM_ROW).floor().max(0.0) as usize + 1;
            let shown = items.len().min(room.max(1));
            sub_rect = fm_rect(smin, shown);
            let scroll = items.len() > shown;
            let r = sub_rect;
            egui::Area::new(egui::Id::new("liked_filter_sub")).order(egui::Order::Foreground).fixed_pos(r.min).show(ctx, |ui| {
                ui.allocate_rect(r, Sense::hover());
                fm_glass(ui.painter(), r, dark);
                let mut draw = |ui: &mut egui::Ui, k: usize, cy: f32, frame: Rect| {
                    let (value, item) = &items[k];
                    if fm_row(ui, egui::Id::new(("liked_filter_opt", kind, k)), frame, cy, item, 0).clicked() {
                        if let Some((v, name)) = value {
                            pick = Some((kind, v.clone(), name.clone()));
                        }
                    }
                };
                if scroll {
                    let inner = r.shrink2(vec2(0.0, 4.0));
                    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(inner));
                    c.set_clip_rect(inner);
                    egui::ScrollArea::vertical().id_salt(("liked_filter_scroll", kind)).max_height(inner.height()).auto_shrink([false, false]).show(&mut c, |ui| {
                        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                        // Las filas a mano, cada 50,5 exactos (con `allocate` se redondearían).
                        let h = 2.0 * FM_EDGE - 8.0 + FM_ROW * (items.len() - 1) as f32;
                        let (all, _) = ui.allocate_exact_size(vec2(FM_W, h), Sense::hover());
                        let frame = Rect::from_min_size(pos2(r.min.x, all.min.y - 4.0), vec2(FM_W, h + 8.0));
                        for k in 0..items.len() {
                            draw(ui, k, frame.min.y + FM_PAD + FM_SUB_TEXT_DY + FM_ROW * k as f32, frame);
                        }
                    });
                } else {
                    for k in 0..items.len() {
                        draw(ui, k, r.min.y + FM_PAD + FM_SUB_TEXT_DY + FM_ROW * k as f32, r);
                    }
                }
            });
        }
        self.liked_filter_sub = sub;
        if let Some((kind, value, label)) = pick.clone() {
            self.settings.liked_filter = kind;
            self.settings.liked_filter_value = value;
            self.settings.liked_filter_label = label;
            self.settings.save(&self.paths);
        }
        let outside = ctx.input(|i| {
            i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !rect.contains(p) && !sub_rect.contains(p) && !button.contains(p))
        });
        if pick.is_some() || outside || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.liked_filter_open = false;
            self.liked_filter_sub = None;
        }
    }

    /// ¿Es `id` una playlist dinámica de Nanofy?
    pub(in crate::app) fn is_dynamic(&self, id: &str) -> bool {
        self.dynamic_playlists.iter().any(|d| d.id == id)
    }

    /// Marca de la cabecera de una playlist dinámica: texto y explicación.
    pub(in crate::app) fn dynamic_badge(&self, id: &str) -> Option<(String, String)> {
        let d = self.dynamic_playlists.iter().find(|d| d.id == id)?;
        let what = if d.kind == 1 { format!("de {}", d.label) } else { format!("del género {}", d.label) };
        Some((
            format!("Dinámica · {}", d.label),
            format!(
                "Playlist dinámica: Nanofy le añade y le quita tus canciones que te gustan {what} en cuanto cambian, también si les das «Me gusta» desde el móvil u otra app (mientras Nanofy está abierta; si no, al abrirla). Lo que añadas tú a mano no se toca.\n\nAñadidas por Nanofy: {}.",
                d.synced.len()
            ),
        ))
    }

    /// La playlist dinámica del filtro en uso, si ya existe.
    fn liked_dynamic_rule(&self) -> Option<&DynamicPlaylist> {
        let (kind, value) = self.liked_filter_on()?;
        self.dynamic_playlists.iter().find(|r| r.kind == kind && r.value == value)
    }

    /// «Playlist dinámica»: abre la del filtro si ya existe; si no, crea una playlist privada con
    /// su nombre y, cuando llega (`Resp::PlaylistCreated`), se rellena y queda al día.
    fn liked_dynamic_click(&mut self) {
        if let Some(r) = self.liked_dynamic_rule() {
            self.actions.push(Action::Go(Page::Playlist(r.id.clone())));
            return;
        }
        let Some((kind, value)) = self.liked_filter_on() else { return };
        if self.pending_dynamic.is_some() {
            return;
        }
        let Some(user_id) = self.my_id().map(str::to_string) else {
            self.status_err("Inicia sesión para crear playlists");
            return;
        };
        let label = self.settings.liked_filter_label.clone();
        let name = format!("{label} · Canciones que te gustan");
        let what = if kind == 1 { "de" } else { "del género" };
        let description = format!("Playlist dinámica de Nanofy: tus canciones que te gustan {what} {label}. Se pone al día sola cuando añades o quitas canciones.");
        self.pending_dynamic = Some(DynamicPlaylist { id: String::new(), name: name.clone(), kind, value, label, synced: Vec::new() });
        self.api.send(Req::CreatePlaylist { user_id, name, description, public: false, collaborative: false });
        self.status("Creando la playlist dinámica…");
    }

    pub(in crate::app) fn save_dynamic(&self) {
        if let Ok(t) = serde_json::to_string(&self.dynamic_playlists) {
            crate::cache::write_atomic(&self.dynamic_path, &t);
        }
    }

    /// Pide los géneros de Deezer de las canciones que te gustan que falten (en su propio hilo,
    /// ver `genres_deezer`), una vez por versión de la lista y sin pisar una búsqueda en curso.
    pub(in crate::app) fn liked_genres_tick(&mut self) {
        let Some(list) = self.lists.get(LIKED) else { return };
        if list.loading || list.tracks.is_empty() {
            return;
        }
        let mark = (list.gen, list.tracks.len());
        if self.liked_genres_asked == Some(mark) || self.liked_genres_pending.is_some_and(|p| p > 0) {
            return;
        }
        let songs: Vec<crate::genres_deezer::LikedSong> = list
            .tracks
            .iter()
            .filter_map(|t| Some((t.id.clone()?, t.name.clone(), t.artists.first().map(|a| a.name.clone()).unwrap_or_default(), t.duration_ms)))
            .collect();
        self.liked_genres_asked = Some(mark);
        self.liked_genres_pending = Some(usize::MAX);
        self.api.send(Req::LikedGenres { songs });
    }

    /// Pone al día las playlists dinámicas con las canciones que te gustan: añade las que entran
    /// en su filtro y quita las que añadió y ya no entran (dejaron de gustarte o, por género, ya se
    /// sabe que no lo tienen). Solo cuando cambia la lista, sus géneros o las reglas.
    pub(in crate::app) fn sync_dynamic_playlists(&mut self) {
        if self.dynamic_playlists.is_empty() {
            return;
        }
        let Some(list) = self.lists.get(LIKED) else { return };
        if list.loading || list.tracks.is_empty() {
            return;
        }
        let key = (list.gen, list.tracks.len(), self.liked_genres_ver, self.dynamic_playlists.len());
        if self.dynamic_sync_key == Some(key) {
            return;
        }
        self.dynamic_sync_key = Some(key);
        let by_uri: HashMap<&str, &Track> = list.tracks.iter().map(|t| (t.uri.as_str(), t)).collect();
        let mut rules = std::mem::take(&mut self.dynamic_playlists);
        let mut changed = false;
        for r in rules.iter_mut().filter(|r| !r.id.is_empty()) {
            let have: HashSet<&str> = r.synced.iter().map(String::as_str).collect();
            let mut seen = HashSet::new();
            let add: Vec<String> = list
                .tracks
                .iter()
                .filter(|t| !have.contains(t.uri.as_str()) && self.liked_match(t, r.kind, &r.value) == Some(true) && seen.insert(t.uri.clone()))
                .map(|t| t.uri.clone())
                .collect();
            let remove: Vec<String> = r
                .synced
                .iter()
                .filter(|u| by_uri.get(u.as_str()).is_none_or(|t| self.liked_match(t, r.kind, &r.value) == Some(false)))
                .cloned()
                .collect();
            if !add.is_empty() {
                log::info!("[dinamica] «{}»: se añaden {}", r.name, add.len());
                self.api.send(Req::AddToPlaylist { id: r.id.clone(), uris: add.clone() });
                r.synced.extend(add);
                changed = true;
            }
            if !remove.is_empty() {
                log::info!("[dinamica] «{}»: se quitan {}", r.name, remove.len());
                self.api.send(Req::RemoveFromPlaylist { id: r.id.clone(), uris: remove.clone() });
                let gone: HashSet<String> = remove.into_iter().collect();
                r.synced.retain(|u| !gone.contains(u));
                changed = true;
            }
        }
        self.dynamic_playlists = rules;
        if changed {
            self.save_dynamic();
        }
    }

    fn set_liked_grid(&mut self, on: bool) {
        self.settings.liked_grid = on;
        self.settings.save(&self.paths);
    }

    /// Cuadrícula de canciones con las tarjetas de la biblioteca (portada de 170,6, título y
    /// artistas); solo se pintan las filas a la vista. Pulsar una la reproduce, y siguen las demás.
    fn liked_grid(&mut self, ui: &mut egui::Ui, o: egui::Pos2, panel: Rect, tracks: &[Track]) {
        let st = style(ui.ctx());
        let avail = panel.width() - GRID_LEFT - GRID_RIGHT + (PITCH_X - CARD);
        let cols = ((avail / PITCH_X).floor() as usize).max(1);
        let rows = tracks.len().div_ceil(cols);
        let h = GRID_FIRST + rows as f32 * PITCH_Y;
        let (block, _) = ui.allocate_exact_size(vec2(panel.width() - 2.0, h + 24.0), Sense::hover());
        let clip = ui.clip_rect();
        let first = (((clip.min.y - block.min.y - GRID_FIRST) / PITCH_Y).floor().max(0.0)) as usize;
        let last = ((((clip.max.y - block.min.y - GRID_FIRST) / PITCH_Y).ceil().max(0.0)) as usize).min(rows);
        let shuffle = self.player.shuffle;
        let now_uri = self.player.now.as_ref().map(|n| n.uri.clone());
        for row in first..last {
            let t = block.min.y + GRID_FIRST + row as f32 * PITCH_Y;
            for col in 0..cols {
                let i = row * cols + col;
                let Some(track) = tracks.get(i) else { break };
                let x = o.x + GRID_LEFT + col as f32 * PITCH_X;
                let rect = Rect::from_min_size(pos2(x, t), vec2(CARD, PITCH_Y - 12.0));
                let resp = ui.interact(rect, ui.id().with(("liked_card", i)), Sense::click());
                let hovered = resp.hovered();
                if hovered {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                let painter = ui.painter().clone();
                let cover = Rect::from_min_size(pos2(x, t + GRID_COVER_TOP), vec2(COVER_W, CARD));
                match track.cover(300) {
                    Some(url) => self.cover_in(ui, Some(url), cover, 6),
                    None => {
                        painter.rect_filled(cover, CornerRadius::same(6), st.placeholder);
                        paint_mask(&painter, "note", &lm::NOTE, px(&painter, cover.min), st.placeholder_icon);
                    }
                }
                if hovered {
                    painter.rect_filled(cover, CornerRadius::same(6), Color32::from_white_alpha(14));
                }
                let is_now = now_uri.as_deref() == Some(track.uri.as_str());
                let title_color = if is_now { theme::GREEN } else if hovered { st.title_hover } else { st.title };
                let base = t + GRID_TITLE_BASE;
                let g = galley_truncated(&painter, &track.name, theme::semibold(TITLE_FONT), title_color, CARD);
                let ink = ink_left(&g);
                text_on_baseline(&painter, pos2(x - ink, base), g, title_color);
                let mut job = egui::text::LayoutJob::single_section(
                    track.artists_str(),
                    egui::TextFormat { font_id: theme::regular(SUB_FONT), color: st.sub, line_height: Some(SUB_LINE), ..Default::default() },
                );
                job.wrap = egui::text::TextWrapping { max_width: CARD, max_rows: 2, break_anywhere: false, overflow_character: Some('…') };
                let g = painter.layout_job(job);
                let ink = ink_left(&g);
                text_on_baseline(&painter, pos2(x - ink, base + SUB_GAP), g, st.sub);
                if resp.clicked() {
                    let uris: Vec<String> = tracks.iter().map(|t| t.uri.clone()).collect();
                    self.actions.push(Action::Play(PlayTarget::Tracks { uris, index: Some(i as u32), shuffle, context: None }));
                }
            }
        }
    }
}
