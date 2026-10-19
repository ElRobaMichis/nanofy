//! Páginas de un podcast y de uno de sus episodios, copiadas de sus referencias (18 oct 2026, «The
//! Joe Rogan Experience» y su episodio #2149). Medidas a la escala de la página (× 1,064: su
//! reproductor encimado sobre el de las referencias anteriores da la menor diferencia ahí) y
//! relativas al borde del panel (en la referencia, (264, 66) y (264, 65)).
//!
//! El panel: un degradado del color de la portada (más vivo que el de los álbumes) a casi negro.
//! Los textos son blanco translúcido, así que toman el tono del fondo como en la referencia.

use egui::{pos2, vec2, Color32, CornerRadius, Rect, Sense, Stroke};

use super::icons::{self, Icon};
use super::library_masks as lm;
use super::theme::{self, GREEN};
use super::widgets::{galley_truncated, text_on_baseline};
use super::{Action, App, Page, PlayTarget};
use crate::api::Req;
use crate::model::{Episode, Show};

// --- colores (blanco translúcido sobre el degradado)
const STRONG: Color32 = Color32::from_rgba_premultiplied(230, 230, 230, 230);
const WEAK: Color32 = Color32::from_rgba_premultiplied(130, 130, 130, 130);
const FAINT: Color32 = Color32::from_rgba_premultiplied(26, 26, 26, 26);
/// Fondo del degradado abajo.
const BOTTOM: Color32 = Color32::from_rgb(6, 6, 5);

// --- cabecera (las dos páginas)
const LEFT: f32 = 37.2;
const TITLE_BASE: f32 = 61.7;
const TITLE_FONT: f32 = 36.8;
const SUB_BASE: f32 = 107.5;
const SUB_FONT: f32 = 14.5;
// Seguir y valoración: píldoras de 42,6 y 44,7 de alto.
const FOLLOW_X: f32 = 35.1;
const FOLLOW_Y: f32 = 131.9;
const FOLLOW_W: f32 = 86.2;
const FOLLOW_H: f32 = 42.6;
const PILL_FONT: f32 = 13.2;
const PILL_BASE: f32 = 158.5;
const RATING_X: f32 = 133.0;
const RATING_Y: f32 = 130.9;
const RATING_H: f32 = 44.7;
const RATING_PAD: f32 = 20.2;
const RATING_GAP1: f32 = 17.1;
const RATING_GAP2: f32 = 15.9;
const STAR: f32 = 13.8;
const DOTS_GAP: f32 = 24.5;
const DOTS_Y: f32 = 152.7;
// «Todos los episodios» y sus herramientas.
const SEC_BASE: f32 = 243.7;
const SEC_FONT: f32 = 20.7;
const TOOL_BASE: f32 = 241.5;
const TOOL_FONT: f32 = 14.8;
const TOOL_Y: f32 = 235.6;
// Filas de episodio.
const ROW0: f32 = 272.4;
const ROW_PITCH: f32 = 205.3;
const COVER: f32 = 129.8;
const ICON_X: f32 = 189.4;
const ICON_DY: f32 = 12.8;
const EP_TITLE_X: f32 = 220.2;
const EP_TITLE_DY: f32 = 19.1;
const EP_TITLE_FONT: f32 = 18.8;
const META_X: f32 = 186.2;
const META_DY: f32 = 60.6;
const META_FONT: f32 = 14.3;
const DESC_X: f32 = 185.1;
const DESC_DY: f32 = 98.9;
const ACT_DY: f32 = 143.0;
/// Botones de la fila: play (41,5) y luego guardar, añadir, descargar, compartir y más.
const ACT_XS: [f32; 6] = [204.85, 259.6, 313.9, 368.7, 424.0, 477.8];
const PLAY_D: f32 = 41.5;
const SEP_DY: f32 = 183.0;
// Columna derecha (portada, «Acerca de» y temas).
const RIGHT_W: f32 = 321.3;
const RIGHT_MARGIN: f32 = 50.2;
const COL_GAP: f32 = 56.4;
const RCOVER_Y: f32 = 22.3;
const ABOUT_BASE: f32 = 393.7;
const ABOUT_TEXT_TOP: f32 = 415.0;
const ABOUT_FONT: f32 = 16.3;
const ABOUT_LINE: f32 = 21.9;
const CHIP_H: f32 = 38.3;
const CHIP_PAD: f32 = 20.2;
const CHIP_GAP: f32 = 9.6;
const CHIP_FONT: f32 = 15.5;
const CHIP_DY: f32 = 35.4;
// Episodio.
const EPI_META_BASE: f32 = 109.6;
const EPI_ACT_Y: f32 = 155.85;
const EPI_ACT_XS: [f32; 6] = [57.45, 111.7, 166.5, 220.8, 275.0, 330.4];
const EPI_PLAY_D: f32 = 42.6;
const VIDEO_Y: f32 = 206.4;
const VIDEO_RIGHT: f32 = 58.5;
const VIDEO_PLAY_D: f32 = 83.0;

/// Lo que hay a la izquierda de la tinta del primer glifo de `g`.
fn ink_left(g: &egui::Galley) -> f32 {
    g.rows.first().and_then(|r| r.row.glyphs.first()).map(|gl| gl.uv_rect.offset.x.max(0.0)).unwrap_or(0.0)
}

/// Fecha corta como en Spotify: «10 may» (con el año si no es el actual).
fn short_date(iso: &str) -> String {
    const M: [&str; 12] = ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sept", "oct", "nov", "dic"];
    let mut it = iso.get(..10).unwrap_or(iso).split('-');
    let (Some(y), Some(m), Some(d)) = (it.next(), it.next(), it.next()) else { return iso.to_string() };
    let (Ok(m), Ok(d)) = (m.parse::<usize>(), d.parse::<u32>()) else { return iso.to_string() };
    let name = M.get(m.wrapping_sub(1)).copied().unwrap_or("");
    let this_year = crate::cache::now_secs() / 31_556_952 + 1970;
    if y.parse::<u64>().ok() == Some(this_year) {
        format!("{d} {name}")
    } else {
        format!("{d} {name} {y}")
    }
}

/// «2 h 15 min» o «54 min» (minutos truncados, como Spotify).
fn duration(ms: u32) -> String {
    let m = ms as u64 / 60_000;
    if m >= 60 {
        format!("{} h {} min", m / 60, m % 60)
    } else {
        format!("{m} min")
    }
}

/// «956 K» -> «956K»; «1,2 M».
fn compact(n: u32) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f32 / 1e6).replace('.', ",")
    } else if n >= 1000 {
        format!("{}K", (n + 500) / 1000)
    } else {
        n.to_string()
    }
}

/// Texto plano de una descripción en HTML: sin etiquetas, con saltos donde había <br> o </p>.
fn html_text(html: &str) -> String {
    let mut out = String::new();
    let mut tag = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' if in_tag => {
                in_tag = false;
                let t = tag.trim().to_ascii_lowercase();
                if t.starts_with("br") {
                    out.push('\n');
                } else if t == "/p" {
                    // Párrafos separados por una línea en blanco, como en la referencia.
                    out.push_str("\n\u{1}\n");
                }
            }
            _ if in_tag => tag.push(c),
            _ => out.push(c),
        }
    }
    let out = out.replace("&#34;", "\"").replace("&quot;", "\"").replace("&#39;", "'").replace("&#43;", "+").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&nbsp;", " ");
    let lines: Vec<&str> = out.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    // `\u{1}` marca el hueco entre párrafos: uno solo, nunca al principio ni al final.
    let mut keep: Vec<&str> = Vec::new();
    for l in lines {
        if l == "\u{1}" && (keep.is_empty() || keep.last() == Some(&"")) {
            continue;
        }
        keep.push(if l == "\u{1}" { "" } else { l });
    }
    while keep.last() == Some(&"") {
        keep.pop();
    }
    keep.join("\n")
}

/// Parte `text` en líneas de `max_w` por palabras.
fn wrap(painter: &egui::Painter, text: &str, font: &egui::FontId, max_w: f32) -> Vec<String> {
    let width = |s: &str| painter.layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE).size().x;
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for w in para.split_whitespace() {
            let cand = if line.is_empty() { w.to_string() } else { format!("{line} {w}") };
            if !line.is_empty() && width(&cand) > max_w {
                lines.push(std::mem::take(&mut line));
                line = w.to_string();
            } else {
                line = cand;
            }
        }
        lines.push(line);
    }
    lines
}

impl App {
    /// Color de arriba del degradado: el tono de la paleta de Spotify de la portada con
    /// saturación 0,73 y luminancia 52,6 (de (255, 165, 100) sale (82, 47, 22), el de la
    /// referencia); sin paleta, el tinte de la portada.
    fn podcast_tint(&self, accent: Option<[u8; 3]>, cover: Option<&str>) -> Color32 {
        if let Some([r, g, b]) = accent {
            return tint_from_hue(r, g, b);
        }
        cover.and_then(|u| self.images.tint_with(u, 52.6, 0.73)).unwrap_or(Color32::from_rgb(40, 40, 40))
    }

    /// El panel de un podcast o un episodio: degradado lineal del color de la portada (arriba) a
    /// casi negro (abajo) y el filo de 1 px de los demás paneles.
    pub(super) fn podcast_backdrop(&self, painter: &egui::Painter, panel: Rect, accent: Option<[u8; 3]>, cover: Option<&str>) {
        let top = self.podcast_tint(accent, cover);
        let r = CornerRadius::same(8);
        painter.rect_filled(panel, r, BOTTOM);
        let inner = panel.shrink(1.0);
        let mut mesh = egui::epaint::Mesh::default();
        mesh.colored_vertex(inner.left_top(), top);
        mesh.colored_vertex(inner.right_top(), top);
        mesh.colored_vertex(inner.right_bottom(), BOTTOM);
        mesh.colored_vertex(inner.left_bottom(), BOTTOM);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(egui::Shape::mesh(mesh));
        painter.rect_stroke(panel, r, Stroke::new(1.0, Color32::from_white_alpha(22)), egui::StrokeKind::Inside);
    }

    /// Un icono con su zona de clic; `active` lo pinta en verde.
    fn pod_icon(&mut self, ui: &mut egui::Ui, id: impl std::hash::Hash + std::fmt::Debug, c: egui::Pos2, side: f32, icon: Icon, active: bool, tip: &str) -> egui::Response {
        let r = ui.interact(Rect::from_center_size(c, vec2(side + 12.0, side + 12.0)), ui.id().with(id), Sense::click()).on_hover_text(tip);
        let color = if active {
            GREEN
        } else if r.hovered() {
            Color32::WHITE
        } else {
            WEAK
        };
        icons::paint_side(ui.painter(), c, side, color, icon);
        if r.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        r
    }

    /// Botón verde de reproducir de `d` de diámetro.
    fn pod_play(&mut self, ui: &mut egui::Ui, id: impl std::hash::Hash + std::fmt::Debug, c: egui::Pos2, d: f32, playing: bool) -> egui::Response {
        let r = ui.interact(Rect::from_center_size(c, vec2(d, d)), ui.id().with(id), Sense::click()).on_hover_text(if playing { "Pausar" } else { "Reproducir" });
        let fill = if r.hovered() { GREEN.lerp_to_gamma(Color32::WHITE, 0.12) } else { GREEN };
        ui.painter().circle_filled(c, d / 2.0, fill);
        icons::paint_side(ui.painter(), c, d * 0.42, Color32::BLACK, if playing { Icon::Pause } else { Icon::Play });
        if r.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        r
    }

    /// Menú «…» de un episodio.
    fn episode_menu(&mut self, r: &egui::Response, e: &Episode) {
        let link = super::widgets::uri_to_link(&e.uri);
        egui::Popup::menu(r).show(|ui| {
            ui.set_min_width(200.0);
            if Self::menu_item(ui, Some(Icon::AddToQueue), "Añadir a la cola", false).clicked() {
                self.actions.push(Action::AddToQueue(e.uri.clone()));
                ui.close();
            }
            if Self::menu_item(ui, Some(Icon::NewTab), "Abrir en una nueva pestaña", false).clicked() {
                self.actions.push(Action::OpenInTab(Page::Episode(e.id.clone())));
                ui.close();
            }
            if Self::menu_item(ui, Some(Icon::Share), "Copiar enlace", false).clicked() {
                self.actions.push(Action::CopyText(link.clone(), "Enlace"));
                ui.close();
            }
        });
    }

    /// Guardar, añadir, descargar, compartir y más de un episodio, centrados en `xs` a la altura `y`
    /// (`add_circle`: el «+» en círculo de la página del episodio, en vez del cuadrado).
    fn episode_actions(&mut self, ui: &mut egui::Ui, e: &Episode, o: egui::Pos2, xs: &[f32; 6], y: f32, add_circle: bool) {
        let saved = self.saved_episodes.contains(&e.id);
        let k = &e.id;
        if self.pod_icon(ui, ("ep_save", k), pos2(o.x + xs[1], y), 29.3, if saved { Icon::BookmarkFilled } else { Icon::Bookmark }, saved, if saved { "Quitar de Guardados" } else { "Guardar en Guardados" }).clicked() {
            self.actions.push(Action::SaveEpisode(e.id.clone(), !saved));
        }
        let (add_icon, add_side) = if add_circle { (Icon::PlusCircle, 23.4) } else { (Icon::PlusSquare, 27.2) };
        if self.pod_icon(ui, ("ep_add", k), pos2(o.x + xs[2], y), add_side, add_icon, false, "Añadir a una playlist").clicked() {
            self.open_add_dialog(vec![e.uri.clone()]);
        }
        let dl = self.downloaded.contains(&e.id);
        let busy = self.downloading.contains(&e.id);
        let tip = if dl { "Descargado" } else if busy { "Descargando…" } else { "Descargar" };
        if self.pod_icon(ui, ("ep_dl", k), pos2(o.x + xs[3], y), 24.5, if busy { Icon::Hourglass } else { Icon::Download }, dl, tip).clicked() && !dl && !busy {
            self.actions.push(Action::DownloadEpisodes(vec![e.id.clone()]));
        }
        if self.pod_icon(ui, ("ep_share", k), pos2(o.x + xs[4], y), 27.2, Icon::Share, false, "Copiar enlace").clicked() {
            self.actions.push(Action::CopyText(super::widgets::uri_to_link(&e.uri), "Enlace"));
        }
        let more = self.pod_icon(ui, ("ep_more", k), pos2(o.x + xs[5], y), 30.2, Icon::More, false, "Más");
        self.episode_menu(&more, e);
    }

    /// La columna derecha: portada, «Acerca de…», su texto y, en los podcasts, sus temas.
    fn podcast_right(&mut self, ui: &mut egui::Ui, o: egui::Pos2, right: f32, cover: Option<&str>, title: &str, about: &str, chips: &[String], link: Option<&str>) {
        let x = right - RIGHT_MARGIN - RIGHT_W;
        let painter = ui.painter().clone();
        let rc = Rect::from_min_size(pos2(x, o.y + RCOVER_Y), vec2(RIGHT_W, RIGHT_W));
        self.cover_in(ui, cover, rc, 8);
        let g = painter.layout_no_wrap(title.to_string(), theme::bold(SEC_FONT), STRONG);
        text_on_baseline(&painter, pos2(x - ink_left(&g), o.y + ABOUT_BASE), g, STRONG);
        let font = theme::regular(ABOUT_FONT);
        let lines = wrap(&painter, about, &font, RIGHT_W);
        let cap = 11.7;
        let mut base = o.y + ABOUT_TEXT_TOP + cap;
        for line in &lines {
            let g = painter.layout_no_wrap(line.clone(), font.clone(), WEAK);
            text_on_baseline(&painter, pos2(x, base), g, WEAK);
            base += ABOUT_LINE;
        }
        if let Some(l) = link {
            // El enlace, un renglón más abajo (en la referencia, «www.sebastianlive.com»).
            base += ABOUT_LINE * 0.45;
            let g = painter.layout_no_wrap(l.to_string(), font.clone(), WEAK);
            let r = text_on_baseline(&painter, pos2(x, base), g, WEAK);
            let resp = ui.interact(r, ui.id().with("about_link"), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                painter.line_segment([pos2(r.min.x, base + 2.0), pos2(r.max.x, base + 2.0)], Stroke::new(1.0, WEAK));
            }
            if resp.clicked() {
                let url = if l.starts_with("http") { l.to_string() } else { format!("https://{l}") };
                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
            }
        }
        if !chips.is_empty() {
            let mut cx = x;
            let top = base - ABOUT_LINE + CHIP_DY;
            for c in chips.iter().take(6) {
                let g = painter.layout_no_wrap(c.clone(), theme::regular(CHIP_FONT), WEAK);
                let w = (g.size().x - ink_left(&g) + 2.0 * CHIP_PAD).round();
                if cx + w > x + RIGHT_W + 1.0 && cx > x {
                    break;
                }
                let chip = Rect::from_min_size(pos2(cx, top), vec2(w, CHIP_H));
                painter.rect_stroke(chip, CornerRadius::same((CHIP_H / 2.0) as u8), Stroke::new(1.3, FAINT.gamma_multiply(1.5)), egui::StrokeKind::Inside);
                let gl = ink_left(&g);
                text_on_baseline(&painter, pos2(cx + CHIP_PAD - gl, top + CHIP_H / 2.0 + 5.5), g, WEAK);
                cx += w + CHIP_GAP;
            }
        }
    }

    /// Página de un podcast (primera referencia): título, editorial, Seguir, valoración y «…»; a
    /// la izquierda sus episodios con orden, filtro y búsqueda; a la derecha portada, «Acerca de» y
    /// temas.
    pub(super) fn podcast_panel(&mut self, ui: &mut egui::Ui, panel: Rect, id: String) {
        self.request_once(&format!("show:{id}"), Req::Show(id.clone()));
        self.request_once("saved_shows", Req::SavedShows);
        self.request_once("saved_episodes", Req::SavedEpisodes);
        let data = self.shows.get(&id).cloned();
        let cover = data.as_ref().and_then(|(s, _)| s.cover(640).map(str::to_string));
        let accent = data.as_ref().and_then(|(s, _)| s.accent);
        self.podcast_backdrop(ui.painter(), panel, accent, cover.as_deref());
        let Some((show, episodes)) = data else {
            let g = ui.painter().layout_no_wrap("Cargando…".into(), theme::regular(15.0), WEAK);
            text_on_baseline(ui.painter(), pos2(panel.min.x + LEFT, panel.min.y + 60.0), g, WEAK);
            return;
        };
        let lid = format!("show:{id}");
        if self.list_search_id != lid {
            self.list_search_id = lid.clone();
            self.album_search.clear();
            self.album_search_open = false;
        }
        let q = self.list_query(&lid);
        let mut eps: Vec<Episode> = episodes
            .iter()
            .filter(|e| q.is_empty() || e.name.to_lowercase().contains(&q) || e.description.to_lowercase().contains(&q))
            .filter(|e| match self.show_filter {
                1 => e.played.as_deref() != Some("COMPLETED") && e.played.as_deref() != Some("IN_PROGRESS"),
                2 => e.played.as_deref() == Some("IN_PROGRESS"),
                3 => self.saved_episodes.contains(&e.id),
                4 => self.downloaded.contains(&e.id),
                _ => true,
            })
            .cloned()
            .collect();
        if self.show_sort_newest {
            eps.sort_by(|a, b| b.release_date.cmp(&a.release_date));
        } else {
            eps.sort_by(|a, b| a.release_date.cmp(&b.release_date));
        }
        let total_h = ROW0 + ROW_PITCH * eps.len().max(1) as f32 + 40.0;
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(panel));
        c.set_clip_rect(panel.shrink(1.0));
        egui::ScrollArea::vertical().id_salt(("podcast", &id)).auto_shrink([false, false]).show(&mut c, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let (area, _) = ui.allocate_exact_size(vec2(panel.width(), total_h.max(panel.height())), Sense::hover());
            let o = pos2(panel.min.x.round(), area.min.y.round());
            let right = panel.max.x.round();
            let col_right = right - RIGHT_MARGIN - RIGHT_W - COL_GAP;
            self.podcast_header(ui, o, &show);
            self.episodes_toolbar(ui, o, col_right);
            let uris: Vec<String> = eps.iter().map(|e| e.uri.clone()).collect();
            if eps.is_empty() {
                let msg = if episodes.is_empty() { "Cargando episodios…" } else { "Ningún episodio coincide." };
                let g = ui.painter().layout_no_wrap(msg.into(), theme::regular(META_FONT), WEAK);
                text_on_baseline(ui.painter(), pos2(o.x + LEFT, o.y + ROW0 + 20.0), g, WEAK);
            }
            for (i, e) in eps.iter().enumerate() {
                let top = o.y + ROW0 + ROW_PITCH * i as f32;
                if top > ui.clip_rect().max.y + 10.0 || top + ROW_PITCH < ui.clip_rect().min.y - 10.0 {
                    continue;
                }
                self.podcast_row(ui, o, col_right, top, &show, e, i, &uris);
            }
            let about = strip(&show.description);
            self.podcast_right(ui, o, right, cover.as_deref(), "Acerca de", &about, &show.keywords, None);
        });
    }

    /// Título, editorial, Seguir, valoración y «…».
    fn podcast_header(&mut self, ui: &mut egui::Ui, o: egui::Pos2, show: &Show) {
        let painter = ui.painter().clone();
        let g = galley_truncated(&painter, &show.name, theme::bold(TITLE_FONT), STRONG, 1000.0);
        text_on_baseline(&painter, pos2(o.x + LEFT - ink_left(&g), o.y + TITLE_BASE), g, STRONG);
        let g = painter.layout_no_wrap(show.publisher.clone(), theme::regular(SUB_FONT), STRONG);
        text_on_baseline(&painter, pos2(o.x + LEFT - ink_left(&g), o.y + SUB_BASE), g, STRONG);
        // Seguir / Siguiendo.
        let followed = self.followed_shows.contains(&show.id);
        let label = if followed { "Siguiendo" } else { "Seguir" };
        let g = painter.layout_no_wrap(label.into(), theme::regular(PILL_FONT), STRONG);
        let w = FOLLOW_W.max(g.size().x + 2.0 * 23.4);
        let pill = Rect::from_min_size(pos2(o.x + FOLLOW_X, o.y + FOLLOW_Y), vec2(w, FOLLOW_H));
        let r = ui.interact(pill, ui.id().with("show_follow"), Sense::click());
        painter.rect_filled(pill, CornerRadius::same((FOLLOW_H / 2.0) as u8), if r.hovered() { Color32::from_white_alpha(40) } else { FAINT });
        let tw = g.size().x - ink_left(&g);
        text_on_baseline(&painter, pos2(pill.center().x - tw / 2.0 - ink_left(&g), o.y + PILL_BASE), g, STRONG);
        if r.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if r.clicked() {
            self.actions.push(Action::FollowShow(show.id.clone(), !followed));
        }
        let mut x = pill.max.x + (RATING_X - FOLLOW_X - FOLLOW_W);
        // Valoración: «4,7 ★ 956K» en una píldora con filo.
        if let Some((avg, n)) = show.rating {
            let g1 = painter.layout_no_wrap(format!("{avg:.1}").replace('.', ","), theme::regular(PILL_FONT), STRONG);
            let g2 = painter.layout_no_wrap(compact(n), theme::regular(PILL_FONT * 0.93), WEAK);
            let w1 = g1.size().x - ink_left(&g1);
            let w2 = g2.size().x - ink_left(&g2);
            let w = RATING_PAD + w1 + RATING_GAP1 + STAR + RATING_GAP2 + w2 + RATING_PAD;
            let pill = Rect::from_min_size(pos2(x, o.y + RATING_Y), vec2(w, RATING_H));
            painter.rect_stroke(pill, CornerRadius::same((RATING_H / 2.0) as u8), Stroke::new(1.3, FAINT.gamma_multiply(1.4)), egui::StrokeKind::Inside);
            let mut tx = pill.min.x + RATING_PAD;
            let l1 = ink_left(&g1);
            text_on_baseline(&painter, pos2(tx - l1, o.y + PILL_BASE), g1, STRONG);
            tx += w1 + RATING_GAP1;
            star(&painter, pos2(tx + STAR / 2.0, o.y + 152.6), STAR, STRONG);
            tx += STAR + RATING_GAP2;
            let l2 = ink_left(&g2);
            text_on_baseline(&painter, pos2(tx - l2, o.y + PILL_BASE), g2, WEAK);
            ui.interact(pill, ui.id().with("show_rating"), Sense::hover()).on_hover_text(format!("{} valoraciones", n));
            x = pill.max.x;
        }
        // «…»
        let dc = pos2(x + DOTS_GAP + 10.1, o.y + DOTS_Y);
        let more = ui.interact(Rect::from_center_size(dc, vec2(34.0, 30.0)), ui.id().with("show_more"), Sense::click());
        for k in -1..=1 {
            painter.circle_filled(pos2(dc.x + k as f32 * 8.5, dc.y), 1.7, if more.hovered() { Color32::WHITE } else { STRONG });
        }
        let link = super::widgets::uri_to_link(&show.uri);
        let sid = show.id.clone();
        egui::Popup::menu(&more).show(|ui| {
            ui.set_min_width(200.0);
            if Self::menu_item(ui, Some(Icon::NewTab), "Abrir en una nueva pestaña", false).clicked() {
                self.actions.push(Action::OpenInTab(Page::Show(sid.clone())));
                ui.close();
            }
            if Self::menu_item(ui, Some(Icon::Share), "Copiar enlace", false).clicked() {
                self.actions.push(Action::CopyText(link.clone(), "Enlace"));
                ui.close();
            }
        });
    }

    /// «Todos los episodios» y, a la derecha, orden, filtro y búsqueda.
    fn episodes_toolbar(&mut self, ui: &mut egui::Ui, o: egui::Pos2, col_right: f32) {
        let painter = ui.painter().clone();
        let g = painter.layout_no_wrap("Todos los episodios".into(), theme::bold(SEC_FONT), STRONG);
        text_on_baseline(&painter, pos2(o.x + LEFT - ink_left(&g), o.y + SEC_BASE), g, STRONG);
        let y = o.y + TOOL_Y;
        // Lupa (máscara de la biblioteca), con el campo de búsqueda a su izquierda al abrirla.
        let lupa_right = col_right - 9.7;
        let lupa_c = pos2(lupa_right - 11.2, y - 1.0);
        let lr = ui.interact(Rect::from_center_size(lupa_c, vec2(34.0, 34.0)), ui.id().with("ep_search"), Sense::click()).on_hover_text("Buscar episodio");
        let lcol = if self.album_search_open { GREEN } else if lr.hovered() { Color32::WHITE } else { WEAK };
        super::library::paint_lib_mask(&painter, "pod-search", &lm::SEARCH, lupa_c, lcol);
        if lr.clicked() {
            self.album_search_open = !self.album_search_open;
            self.album_search_focus = self.album_search_open;
            if !self.album_search_open {
                self.album_search.clear();
            }
        }
        let mut right = lupa_c.x - 11.2 - 20.2;
        if self.album_search_open {
            let field = Rect::from_min_max(pos2(right - 200.0, y - 13.0), pos2(right, y + 13.0));
            let mut f = ui.new_child(egui::UiBuilder::new().max_rect(field));
            let r = f.add(egui::TextEdit::singleline(&mut self.album_search).hint_text("Buscar episodio").desired_width(field.width()));
            if self.album_search_focus {
                self.album_search_focus = false;
                r.request_focus();
            }
            if r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.album_search_open = false;
                self.album_search.clear();
            }
            right = field.min.x - 20.2;
        }
        // Filtro.
        const FILTERS: [&str; 5] = ["Todos los episodios", "Sin escuchar", "Empezados", "Guardados", "Descargados"];
        let fg = painter.layout_no_wrap(FILTERS[self.show_filter as usize % FILTERS.len()].into(), theme::regular(TOOL_FONT), WEAK);
        let fw = fg.size().x - ink_left(&fg);
        let fl = fg.clone();
        let label_x = right - fw;
        let icon_c = pos2(label_x - 15.9 - 10.15, y);
        let hit = Rect::from_min_max(pos2(icon_c.x - 14.0, y - 16.0), pos2(right + 4.0, y + 16.0));
        let fr = ui.interact(hit, ui.id().with("ep_filter"), Sense::click());
        let fcol = if fr.hovered() { Color32::WHITE } else { WEAK };
        super::library::paint_lib_mask(&painter, "pod-filter", &lm::GROUP, icon_c, fcol);
        text_on_baseline(&painter, pos2(label_x - ink_left(&fl), o.y + TOOL_BASE), fl, fcol);
        if fr.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        egui::Popup::menu(&fr).show(|ui| {
            ui.set_min_width(200.0);
            for (i, f) in FILTERS.iter().enumerate() {
                if Self::menu_item(ui, None, f, self.show_filter as usize == i).clicked() {
                    self.show_filter = i as u8;
                    ui.close();
                }
            }
        });
        // Orden: «Nuevos» / «Antiguos».
        let right = icon_c.x - 10.15 - 21.4;
        let sg = painter.layout_no_wrap(if self.show_sort_newest { "Nuevos" } else { "Antiguos" }.into(), theme::regular(TOOL_FONT), WEAK);
        let sw = sg.size().x - ink_left(&sg);
        let sx = right - sw;
        let sort_c = pos2(sx - 14.9 - 12.25, y);
        let hit = Rect::from_min_max(pos2(sort_c.x - 15.0, y - 16.0), pos2(right + 4.0, y + 16.0));
        let sr = ui.interact(hit, ui.id().with("ep_sort"), Sense::click()).on_hover_text("Cambiar el orden");
        let scol = if sr.hovered() { Color32::WHITE } else { WEAK };
        super::library::paint_lib_mask(&painter, "pod-sort", &lm::SORT, sort_c, scol);
        let sl = ink_left(&sg);
        text_on_baseline(&painter, pos2(sx - sl, o.y + TOOL_BASE), sg, scol);
        if sr.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if sr.clicked() {
            self.show_sort_newest = !self.show_sort_newest;
        }
    }

    /// Una fila de episodio.
    #[allow(clippy::too_many_arguments)]
    fn podcast_row(&mut self, ui: &mut egui::Ui, o: egui::Pos2, col_right: f32, top: f32, show: &Show, e: &Episode, i: usize, uris: &[String]) {
        let painter = ui.painter().clone();
        let cover_r = Rect::from_min_size(pos2(o.x + LEFT, top), vec2(COVER, COVER));
        let url = e.cover(300).or_else(|| show.cover(300)).map(str::to_string);
        self.cover_in(ui, url.as_deref(), cover_r, 6);
        let open = ui.interact(cover_r, ui.id().with(("ep_cover", &e.id)), Sense::click());
        // Icono (vídeo o episodio) y título.
        let icon_c = pos2(o.x + ICON_X + 8.0, top + ICON_DY);
        if e.video {
            video_icon(&painter, icon_c, WEAK);
        } else {
            icons::paint_side(&painter, icon_c, 17.0, WEAK, Icon::Episode);
        }
        let max_w = col_right - (o.x + EP_TITLE_X) - 10.0;
        let g = galley_truncated(&painter, &e.name, theme::regular(EP_TITLE_FONT), STRONG, max_w);
        let tr = text_on_baseline(&painter, pos2(o.x + EP_TITLE_X - ink_left(&g), top + EP_TITLE_DY), g, STRONG);
        let title_r = ui.interact(tr, ui.id().with(("ep_title", &e.id)), Sense::click());
        if title_r.hovered() {
            painter.line_segment([pos2(tr.min.x, top + EP_TITLE_DY + 2.5), pos2(tr.max.x, top + EP_TITLE_DY + 2.5)], Stroke::new(1.0, STRONG));
        }
        if title_r.hovered() || open.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if title_r.clicked() || open.clicked() {
            self.episodes.entry(e.id.clone()).or_insert_with(|| Episode { show_id: show.id.clone(), show_name: show.name.clone(), ..e.clone() });
            self.actions.push(Action::Go(Page::Episode(e.id.clone())));
        }
        // Fecha · duración.
        let mut x = o.x + META_X;
        if let Some(d) = e.release_date.as_deref() {
            let g = painter.layout_no_wrap(short_date(d), theme::regular(META_FONT), WEAK);
            let gl = ink_left(&g);
            let r = text_on_baseline(&painter, pos2(x - gl, top + META_DY), g, WEAK);
            painter.circle_filled(pos2(r.max.x + 13.3, top + META_DY - 6.9), 1.6, WEAK);
            x = r.max.x + 24.5;
        }
        let g = painter.layout_no_wrap(duration(e.duration_ms), theme::regular(META_FONT), WEAK);
        let gl = ink_left(&g);
        text_on_baseline(&painter, pos2(x - gl, top + META_DY), g, WEAK);
        // Descripción en una línea.
        let desc = e.description.replace('\n', " ");
        let g = galley_truncated(&painter, desc.trim(), theme::regular(META_FONT), WEAK, col_right - (o.x + DESC_X) - 10.0);
        let gl = ink_left(&g);
        text_on_baseline(&painter, pos2(o.x + DESC_X - gl, top + DESC_DY), g, WEAK);
        // Botones.
        let y = top + ACT_DY;
        let playing = self.player.now.as_ref().is_some_and(|n| n.uri == e.uri) && self.player.state == super::PlayState::Playing;
        if self.pod_play(ui, ("ep_play", &e.id), pos2(o.x + ACT_XS[0], y), PLAY_D, playing).clicked() {
            if self.player.now.as_ref().is_some_and(|n| n.uri == e.uri) {
                self.play_pause();
            } else {
                self.actions.push(Action::Play(PlayTarget::Tracks { uris: uris.to_vec(), index: Some(i as u32), shuffle: false, context: None }));
            }
        }
        self.episode_actions(ui, e, o, &ACT_XS, y, false);
        // Separador de 2 px.
        let sy = top + SEP_DY;
        painter.rect_filled(Rect::from_min_max(pos2(o.x + LEFT, sy), pos2(col_right, sy + 2.1)), CornerRadius::ZERO, Color32::from_white_alpha(18));
    }

    /// Página de un episodio (segunda referencia): título, podcast · fecha · duración, botones, el
    /// vídeo (su imagen 16:9 con un botón de reproducir: el audio suena en Nanofy) y, a la
    /// derecha, portada y «Acerca del episodio».
    pub(super) fn episode_panel(&mut self, ui: &mut egui::Ui, panel: Rect, id: String) {
        if self.episodes.get(&id).is_none_or(|e| e.html_description.is_empty() && e.wide_image.is_none()) {
            self.request_once(&format!("episode:{id}"), Req::Episode(id.clone()));
        }
        self.request_once("saved_episodes", Req::SavedEpisodes);
        let e = self.episodes.get(&id).cloned();
        let cover = e.as_ref().and_then(|e| e.cover(640).map(str::to_string));
        let accent = e.as_ref().and_then(|e| e.accent);
        self.podcast_backdrop(ui.painter(), panel, accent, cover.as_deref());
        let Some(e) = e else {
            let g = ui.painter().layout_no_wrap("Cargando…".into(), theme::regular(15.0), WEAK);
            text_on_baseline(ui.painter(), pos2(panel.min.x + LEFT, panel.min.y + 60.0), g, WEAK);
            return;
        };
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(panel));
        c.set_clip_rect(panel.shrink(1.0));
        egui::ScrollArea::vertical().id_salt(("episode", &id)).auto_shrink([false, false]).show(&mut c, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let (area, _) = ui.allocate_exact_size(vec2(panel.width(), panel.height().max(800.0)), Sense::hover());
            let o = pos2(panel.min.x.round(), area.min.y.round());
            let right = panel.max.x.round();
            let col_right = right - RIGHT_MARGIN - RIGHT_W - COL_GAP;
            let painter = ui.painter().clone();
            let g = galley_truncated(&painter, &e.name, theme::bold(TITLE_FONT), STRONG, col_right - o.x - LEFT);
            text_on_baseline(&painter, pos2(o.x + LEFT - ink_left(&g), o.y + TITLE_BASE), g, STRONG);
            // Podcast · fecha · duración.
            icons::paint_side(&painter, pos2(o.x + 49.0, o.y + 103.7), 20.7, WEAK, Icon::Podcast);
            let g = painter.layout_no_wrap(e.show_name.clone(), theme::regular(SUB_FONT), STRONG);
            let gl = ink_left(&g);
            let r = text_on_baseline(&painter, pos2(o.x + 66.0 - gl, o.y + EPI_META_BASE), g, STRONG);
            let sr = ui.interact(r, ui.id().with("ep_show"), Sense::click());
            if sr.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                painter.line_segment([pos2(r.min.x, o.y + EPI_META_BASE + 2.0), pos2(r.max.x, o.y + EPI_META_BASE + 2.0)], Stroke::new(1.0, STRONG));
            }
            if sr.clicked() && !e.show_id.is_empty() {
                self.actions.push(Action::Go(Page::Show(e.show_id.clone())));
            }
            let mut x = r.max.x;
            for part in [e.release_date.as_deref().map(short_date).unwrap_or_default(), duration(e.duration_ms)] {
                if part.is_empty() {
                    continue;
                }
                painter.circle_filled(pos2(x + 11.7, o.y + EPI_META_BASE - 6.9), 1.6, WEAK);
                let g = painter.layout_no_wrap(part, theme::regular(META_FONT), WEAK);
                let gl = ink_left(&g);
                x = text_on_baseline(&painter, pos2(x + 24.5 - gl, o.y + EPI_META_BASE), g, WEAK).max.x;
            }
            // Botones.
            let y = o.y + EPI_ACT_Y;
            let playing = self.player.now.as_ref().is_some_and(|n| n.uri == e.uri) && self.player.state == super::PlayState::Playing;
            let play_r = self.pod_play(ui, "epi_play", pos2(o.x + EPI_ACT_XS[0], y), EPI_PLAY_D, playing);
            self.episode_actions(ui, &e, o, &EPI_ACT_XS, y, true);
            // El vídeo: su imagen 16:9 con un botón de reproducir en medio (los episodios solo de
            // audio no lo llevan).
            let has_video = e.video || e.wide_image.is_some();
            let vw = if has_video { (col_right - o.x - LEFT - VIDEO_RIGHT).max(200.0) } else { 0.0 };
            let video = Rect::from_min_size(pos2(o.x + LEFT, o.y + VIDEO_Y), vec2(vw, vw * 9.0 / 16.0));
            let mut video_clicked = false;
            if has_video {
                let img = e.wide_image.clone().or_else(|| cover.clone());
                self.cover_in(ui, img.as_deref(), video, 8);
                let vr = ui.interact(video, ui.id().with("epi_video"), Sense::click());
                let pc = video.center();
                painter.circle_filled(pc, VIDEO_PLAY_D / 2.0, if vr.hovered() { Color32::WHITE } else { Color32::from_gray(238) });
                icons::paint_side(&painter, pc + vec2(2.0, 0.0), VIDEO_PLAY_D * 0.36, Color32::BLACK, if playing { Icon::Pause } else { Icon::Play });
                if vr.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                video_clicked = vr.on_hover_text("Reproducir (en Nanofy suena el audio del vídeo)").clicked();
            }
            if play_r.clicked() || video_clicked {
                if self.player.now.as_ref().is_some_and(|n| n.uri == e.uri) {
                    self.play_pause();
                } else {
                    self.actions.push(Action::Play(PlayTarget::Tracks { uris: vec![e.uri.clone()], index: Some(0), shuffle: false, context: None }));
                }
            }
            // Columna derecha.
            let text = if e.html_description.is_empty() { e.description.clone() } else { html_text(&e.html_description) };
            // El primer enlace suelto del final va aparte, como en la referencia.
            self.podcast_right(ui, o, right, cover.as_deref(), "Acerca del episodio", &text, &[], None);
        });
    }
}

/// Color de su tono (HSV) con saturación 0,73 y luminancia 52,6.
fn tint_from_hue(r: u8, g: u8, b: u8) -> Color32 {
    let (r, g, b) = (r as f32, g as f32, b as f32);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    if max - min < 1.0 {
        return Color32::from_rgb(52, 52, 52);
    }
    let h = if max == r {
        ((g - b) / (max - min)).rem_euclid(6.0)
    } else if max == g {
        (b - r) / (max - min) + 2.0
    } else {
        (r - g) / (max - min) + 4.0
    };
    let s = 0.73;
    // Con v = 1: los tres canales y su luminancia; luego se escala a 52,6.
    let c = s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let m = 1.0 - c;
    let (r1, g1, b1) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let (r1, g1, b1) = (r1 + m, g1 + m, b1 + m);
    let l = 0.2126 * r1 + 0.7152 * g1 + 0.0722 * b1;
    let v = 52.6 / l;
    let q = |t: f32| (t * v).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(q(r1), q(g1), q(b1))
}

fn strip(s: &str) -> String {
    html_text(s)
}

/// Icono de episodio con vídeo (referencia: rectángulo redondeado de 16 × 12,8 y un triángulo).
fn video_icon(painter: &egui::Painter, c: egui::Pos2, color: Color32) {
    let r = Rect::from_center_size(c, vec2(15.0, 11.8));
    painter.rect_stroke(r, CornerRadius::same(2), Stroke::new(1.3, color), egui::StrokeKind::Middle);
    let t = 2.6;
    painter.add(egui::Shape::convex_polygon(vec![pos2(c.x - t * 0.7, c.y - t), pos2(c.x + t * 1.1, c.y), pos2(c.x - t * 0.7, c.y + t)], color, Stroke::NONE));
}

/// Estrella de cinco puntas (contorno) de `d` de ancho centrada en `c`.
fn star(painter: &egui::Painter, c: egui::Pos2, d: f32, color: Color32) {
    let r_out = d / 2.0;
    let r_in = r_out * 0.45;
    let mut pts = Vec::with_capacity(11);
    for k in 0..10 {
        let a = -std::f32::consts::FRAC_PI_2 + k as f32 * std::f32::consts::PI / 5.0;
        let r = if k % 2 == 0 { r_out } else { r_in };
        pts.push(pos2(c.x + r * a.cos(), c.y + r * a.sin()));
    }
    pts.push(pts[0]);
    painter.add(egui::Shape::line(pts, Stroke::new(1.2, color)));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La paleta de The Joe Rogan Experience da el color de arriba de su referencia (82, 47, 22).
    #[test]
    fn color_del_fondo() {
        let c = tint_from_hue(255, 165, 100);
        assert!((c.r() as i32 - 82).abs() <= 1 && (c.g() as i32 - 47).abs() <= 1 && (c.b() as i32 - 22).abs() <= 1, "{c:?}");
    }

    #[test]
    fn descripcion_duracion_y_fecha() {
        let t = html_text("<p>Sebastian is a comedian, &#34;The Pete&#34;.</p><p>www.sebastianlive.com</p><p></p>");
        assert_eq!(t, "Sebastian is a comedian, \"The Pete\".\n\nwww.sebastianlive.com");
        assert_eq!(duration(2 * 3_600_000 + 15 * 60_000 + 59_000), "2 h 15 min");
        assert_eq!(duration(54 * 60_000), "54 min");
        assert_eq!(short_date("2024-05-10"), "10 may 2024");
        assert_eq!(compact(955_500), "956K");
    }
}
