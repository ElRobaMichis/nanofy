//! «Canciones que te gustan», copia de su referencia de diseño (1.png, 17 oct 2026): el panel
//! negro con filo, la barra de la biblioteca (lista o cuadrícula, orden, filtro, lupa y «+») y,
//! debajo, la tabla de canciones a todo lo ancho, sin cabecera grande ni columna de la derecha.
//! Las medidas son las de la referencia en una ventana con cliente de 1772 × 1029 (la misma
//! escala que la biblioteca): la barra cae donde la de la biblioteca, así que se comparten.

use std::rc::Rc;

use egui::{pos2, vec2, Color32, CornerRadius, Rect, Sense};

use super::super::icons::Icon;
use super::super::theme;
use super::super::widgets::{galley_truncated, text_on_baseline, RowOpts};
use super::super::{Action, App, PlayTarget, CONTENT_PAD_LEFT, CONTENT_PAD_RIGHT, LIKED};
use super::{glass_choice_menu, ink_left, ink_right, lm, paint_mask, px, style, FolderDialog, GM_LEFT, GM_TOP};
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
/// Filtros (`Settings::liked_filter`); el botón dice «Filtro: <nombre>».
const FILTERS: [&str; 2] = ["Todo", "Descargadas"];

/// Lo que define la vista ordenada y filtrada que se guarda entre fotogramas.
pub(in crate::app) type LikedViewKey = (u64, usize, String, u8, u8, usize);

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
        let list = self.lists.remove(LIKED).unwrap_or_default();
        let origin = pos2(panel.min.x.round(), panel.min.y.round());
        let grid = self.settings.liked_grid;

        let view = self.liked_view(&list.tracks, list.gen);
        let shown: &[Track] = view.as_deref().unwrap_or(&list.tracks);

        if self.sel_list == LIKED && !self.sel.is_empty() {
            // Con canciones elegidas, su barra en el sitio de la de herramientas.
            let bar = Rect::from_min_max(pos2(panel.min.x + CONTENT_PAD_LEFT, origin.y + 8.0), pos2(panel.max.x - CONTENT_PAD_RIGHT, origin.y + 52.0));
            let mut c = ui.new_child(egui::UiBuilder::new().max_rect(bar));
            self.selection_bar(&mut c, LIKED, shown);
        } else {
            self.liked_toolbar(ui, origin, panel);
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
                } else if self.settings.liked_filter != 0 {
                    "Ninguna canción que te gusta está descargada"
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
                self.track_rows(&mut row, LIKED, shown, RowOpts { header: true, select: true, cols_w: Some(cols_w.min(w)), ..RowOpts::tracks(true, true) });
                let used = row.min_rect().height();
                ui.allocate_exact_size(vec2(panel.width() - 2.0, used + 24.0), Sense::hover());
            }
        });
        self.lists.insert(LIKED.to_string(), list);
    }

    /// Las canciones ordenadas y filtradas según la barra, más la búsqueda de la lupa. `None` si
    /// queda la lista tal cual (de fábrica: «Recientes» y «Todo», sin buscar), sin copiarla.
    /// Con miles de canciones se guarda y solo se rehace si cambia algo de lo que la define.
    fn liked_view(&mut self, tracks: &[Track], gen: u64) -> Option<Rc<[Track]>> {
        let sort = self.settings.liked_sort.min(SORTS.len() as u8 - 1);
        let filter = self.settings.liked_filter.min(FILTERS.len() as u8 - 1);
        let q = self.list_query(LIKED);
        if sort == 0 && filter == 0 && q.is_empty() {
            self.liked_view = None;
            return None;
        }
        let key: LikedViewKey = (gen, tracks.len(), q.clone(), sort, filter, self.downloaded.len());
        if let Some((k, v)) = &self.liked_view {
            if *k == key {
                return Some(v.clone());
            }
        }
        let mut v: Vec<Track> = tracks
            .iter()
            .filter(|t| {
                q.is_empty()
                    || t.name.to_lowercase().contains(&q)
                    || t.artists.iter().any(|a| a.name.to_lowercase().contains(&q))
                    || t.album.as_ref().is_some_and(|a| a.name.to_lowercase().contains(&q))
            })
            .filter(|t| filter == 0 || t.id.as_ref().is_some_and(|i| self.downloaded.contains(i)))
            .cloned()
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
            let (pick, close) = glass_choice_menu(&ctx, "liked_sort_menu", at(SORT_ICON_X + GM_LEFT, GM_TOP), sort_hit, &SORTS, sort);
            if let Some(k) = pick {
                self.settings.liked_sort = k as u8;
                self.settings.save(&self.paths);
            }
            if close {
                self.liked_sort_open = false;
            }
        }

        // Filtro: su icono (el de «Agrupar» de la biblioteca) a 46 px del texto de orden.
        let filter = (self.settings.liked_filter as usize).min(FILTERS.len() - 1);
        let filter_x = SORT_TEXT_X + text_w + GAP_SORT_GROUP;
        let dx = filter_x - (lm::GROUP.ox as f32 + 2.0);
        let label = format!("Filtro: {}", FILTERS[filter]);
        let fa = painter.layout_no_wrap(label, theme::semibold(BAR_FONT), st.bar_text);
        let fa_ink = ink_left(&fa);
        let fa_w = fa.size().x - fa_ink - ink_right(&fa);
        let ftext_x = filter_x + 20.0 + GAP_GROUP_TEXT;
        let filter_hit = Rect::from_min_max(at(filter_x - 8.0, 9.0), at(ftext_x + fa_w + 8.0, 50.0));
        let filter_r = ui.interact(filter_hit, ui.id().with("liked_filter"), Sense::click());
        hand(ui, &filter_r);
        let hov = filter_r.hovered();
        // En blanco con un filtro puesto, como «Agrupar» con uno elegido.
        let active = filter != 0 || self.liked_filter_open;
        let c = if active { st.icon_on } else if hov { st.hover } else { st.icon };
        paint_mask(&painter, "group", &lm::GROUP, px(&painter, at(dx, 0.0)), c);
        let tc = if active { st.icon_on } else if hov { st.hover } else { st.bar_text };
        text_on_baseline(&painter, at(ftext_x - fa_ink, BAR_BASE), fa, tc);
        if filter_r.clicked() {
            self.liked_filter_open = !self.liked_filter_open;
            self.liked_sort_open = false;
        }
        if self.liked_filter_open {
            let ctx = ui.ctx().clone();
            let (pick, close) = glass_choice_menu(&ctx, "liked_filter_menu", at(filter_x + GM_LEFT, GM_TOP), filter_hit, &FILTERS, filter);
            if let Some(k) = pick {
                self.settings.liked_filter = k as u8;
                self.settings.save(&self.paths);
            }
            if close {
                self.liked_filter_open = false;
            }
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

        // «+»: crear playlist o carpeta, como en la biblioteca.
        let po = pos2(panel.max.x.round() + PLUS_DX, o.y);
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
