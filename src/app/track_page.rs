//! Página de una canción (la abre la portada del reproductor), copiada de la referencia de diseño
//! 5.webp (cliente = (ref + (10,25, 4)) / 0,94): fondo con la portada desenfocada, portada de
//! 500, título, datos (artista • álbum • año • duración • escuchas), botones, géneros y los
//! artistas con sus papeles; debajo, las pestañas Letra / Créditos / Más como esta. A la izquierda
//! de la letra, sincronizar (el mismo ajuste que el panel de la letra), traducir, comentarios y
//! tamaño del texto (estos tres, aún sin función).
//!
//! Al bajar hasta la letra, las pestañas y la columna de iconos se quedan fijas arriba y, con la
//! sincronización, la página sigue a la canción; arriba, en la ficha, no se mueve sola.
//!
//! Medidas desde la esquina del panel del contenido, en píxeles del cliente.

use std::time::Duration;

use egui::{pos2, vec2, Color32, CornerRadius, Rect, Sense};

use super::library::{paint_mask, px};
use super::lyrics_panel::{first_base, ink_left, scroll_step, sung_chars, sung_rows};
use super::player_menu::{Anchor, SongMenu};
use super::library_masks::LibMask;
use super::song_masks as sm;
use super::theme::{self, GREEN};
use super::widgets::{child_in, galley_truncated, text_on_baseline, uri_to_link, RowOpts, Source};
use super::{fmt_thousands, Action, App, Page, PlayState, PlayTarget};
use crate::api::Req;
use crate::model::*;

/// Portada: esquina a (37, 38), 500 de lado, esquinas de 10.
const COVER: (f32, f32) = (37.0, 38.0);
const COVER_SIDE: f32 = 500.0;
const COVER_RADIUS: u8 = 10;
/// Título: Segoe UI seminegrita 38 (el trazo de 4,4 px de la referencia; la negrita tiene 5,8)
/// con la tinta desde 600,7; líneas base a 75,5 y cada 48,9; como mucho 680 de ancho (así parte
/// «Get Lucky (feat. Pharrell Williams and | Nile Rodgers)» como la referencia).
const TITLE_FONT: f32 = 38.0;
const TITLE_INK_X: f32 = 600.73;
const TITLE_BASE: f32 = 75.54;
const TITLE_PITCH: f32 = 48.91;
const TITLE_W: f32 = 680.0;
const TITLE_MAX_LINES: usize = 3;
/// Lo de debajo del título está medido con uno de dos líneas: con más o menos, se corre.
const TITLE_LINES_REF: usize = 2;
/// Datos (Segoe UI Semilight 15: el trazo fino de la referencia): línea base a 173. Icono de artista centrado a 12,46 del borde de la
/// columna (598,74, el del play); la tinta del texto, 18,24 tras el centro de su icono (18,75 la
/// del álbum); los puntos (radio 1,35, 5,7 por encima de la línea base) a 12,5 de la tinta de cada
/// lado, y el icono del álbum a 23,38 del punto.
const COL_X: f32 = 598.74;
const META_BASE: f32 = 173.0;
const META_FONT: f32 = 15.0;
const META_ARTIST_ICON: f32 = 12.46;
const META_ARTIST_TEXT: f32 = 18.24;
const META_ALBUM_TEXT: f32 = 18.75;
const META_DOT_GAP: f32 = 12.5;
const META_DOT_ICON: f32 = 23.38;
const META_DOT_DY: f32 = -5.7;
const META_DOT_R: f32 = 1.35;
/// Botones: play verde de 41,5 centrado en (619,49, 219,46); los demás, cada 54,4.
const PLAY_C: (f32, f32) = (619.49, 219.46);
const PLAY_R: f32 = 20.75;
const BTN_PITCH: f32 = 54.4;
/// Géneros: chips de contorno (medidas por el centro del trazo, de 1,6 y blanco al 14 %; egui lo
/// ajusta a píxel entero: la referencia lo tiene en 319,4 y 356,5) desde (600,4, 260) y 37 de
/// alto; texto Semilight 15 con la tinta a 20,2 del trazo izquierdo y 20,1 del derecho, la línea
/// base a 343,8 (24,8 bajo el trazo de arriba); 12,06 de trazo a trazo entre ellos.
const CHIP_X: f32 = 600.4;
const CHIP_Y: f32 = 260.0;
const CHIP_H: f32 = 37.0;
const CHIP_PAD_L: f32 = 20.2;
const CHIP_PAD_R: f32 = 20.1;
const CHIP_BASE: f32 = 24.8;
const CHIP_GAP: f32 = 12.06;
const CHIP_FONT: f32 = 15.0;
const CHIP_STROKE: f32 = 1.6;
/// Artistas: foto de 60,6 desde x 599,84, la primera arriba a 319,27 (22,27 bajo el trazo de abajo de los chips), una
/// cada 76,45. Nombre (17) con la línea base 10,35 por encima del centro de la foto y papeles (17,
/// gris) 21,7 por debajo, con la tinta desde 681,7.
const AV_X: f32 = 599.84;
const AV_BELOW_CHIPS: f32 = 22.27;
const AV_D: f32 = 60.6;
const AV_PITCH: f32 = 76.45;
const AV_NAME_DY: f32 = -10.35;
const AV_ROLE_DY: f32 = 21.7;
const AV_TEXT_X: f32 = 681.7;
const AV_FONT: f32 = 17.0;
/// La línea de las pestañas, 89,3 por debajo de lo más bajo de la ficha (la portada en la
/// referencia, a 538). Pestañas (14: la mayúscula de 9,8 de la referencia) con la línea base 20,2
/// por encima, tinta desde 47,6 y 51,7 entre una y otra; el subrayado verde (1,77 de alto, acaba
/// 0,07 sobre la línea) sobresale 9 por la izquierda y 9,5 por la derecha. La línea: blanca muy
/// tenue, de borde a borde.
const RULE_BELOW: f32 = 89.3;
const RULE_H: f32 = 1.2;
const TAB_X: f32 = 47.6;
const TAB_GAP: f32 = 51.7;
const TAB_BASE_UP: f32 = 20.2;
const TAB_FONT: f32 = 14.0;
const UNDER_L: f32 = 9.0;
const UNDER_R: f32 = 9.5;
const UNDER_UP: f32 = 1.84;
const UNDER_H: f32 = 1.77;
/// Alto de la barra de las pestañas cuando se queda fija arriba (hasta la línea).
const BAR_H: f32 = 55.3;
/// Letra: iconos centrados en x 56,5, el primero 60 por debajo de la línea y cada 52,76;
/// renglones en seminegrita 30 (mismo ancho y casi el mismo trazo) con la tinta desde 288,4, la primera línea base 65,6 por debajo de la
/// línea y cada 53,04 (34 los vacíos, la pausa entre estrofas).
const LYR_ICON_X: f32 = 56.5;
const LYR_ICON_DY: f32 = 60.03;
const LYR_ICON_PITCH: f32 = 52.76;
const LINE_X: f32 = 288.4;
const LINE_FIRST: f32 = 65.61;
const LINE_PITCH: f32 = 53.04;
const LINE_BLANK: f32 = 34.0;
const LINE_FONT: f32 = 30.0;
/// Margen de la derecha (como el de la izquierda, el de la portada).
const MARGIN: f32 = 37.0;
/// Ancho del panel en la referencia y portada más pequeña con el panel más estrecho.
const REF_PANEL_W: f32 = 1492.0;
const MIN_COVER: f32 = 160.0;
/// Siguiendo la canción, el renglón que suena queda a este tanto por uno del alto de la letra a la
/// vista (bajo la barra fija). Tras mover la rueda, la página deja de seguirla estos segundos.
const FOLLOW_AT: f32 = 0.35;
const FOLLOW_PAUSE: f64 = 3.0;

/// Colores de la referencia (la página va siempre sobre su fondo oscuro, también en el tema
/// claro).
struct Ink {
    title: Color32,
    strong: Color32,
    meta: Color32,
    dot: Color32,
    icon: Color32,
    chip: Color32,
    chip_text: Color32,
    name: Color32,
    role: Color32,
    tab_on: Color32,
    tab_off: Color32,
    rule: Color32,
    sung: Color32,
    ahead: Color32,
    note: Color32,
}

/// El blanco de los textos de la referencia es ~230 (blanco al 89 %), no puro.
const INK: Ink = Ink {
    title: Color32::from_rgba_premultiplied(227, 227, 227, 227),
    strong: Color32::from_rgba_premultiplied(227, 227, 227, 227),
    meta: Color32::from_rgba_premultiplied(140, 140, 140, 140),
    dot: Color32::from_rgba_premultiplied(150, 150, 150, 150),
    icon: Color32::from_rgb(167, 169, 173),
    chip: Color32::from_rgba_premultiplied(36, 36, 36, 36),
    chip_text: Color32::from_rgba_premultiplied(132, 132, 132, 132),
    name: Color32::from_rgba_premultiplied(227, 227, 227, 227),
    role: Color32::from_rgba_premultiplied(135, 135, 135, 135),
    tab_on: Color32::from_rgba_premultiplied(232, 232, 232, 232),
    tab_off: Color32::from_rgba_premultiplied(132, 132, 132, 132),
    rule: Color32::from_rgba_premultiplied(16, 16, 16, 16),
    sung: Color32::from_rgba_premultiplied(227, 227, 227, 227),
    ahead: Color32::from_rgba_premultiplied(64, 64, 64, 64),
    note: Color32::from_rgba_premultiplied(150, 150, 150, 150),
};

/// Pestañas de debajo de la ficha.
const TABS: [&str; 3] = ["Letra", "Créditos", "Más como esta"];

/// Donde acaba la tinta del último carácter de la primera fila.
fn ink_right(g: &egui::Galley) -> f32 {
    g.rows
        .first()
        .and_then(|r| r.row.glyphs.last())
        .map(|gl| gl.pos.x + gl.uv_rect.offset.x + gl.uv_rect.size.x)
        .unwrap_or(g.size().x)
}

/// Parte el título en líneas de como mucho `max_w` (por palabras; una palabra más larga va
/// sola) y con `max_lines` como mucho, la última acabada en «…» si no cabe todo.
fn wrap_words(text: &str, max_w: f32, max_lines: usize, width: impl Fn(&str) -> f32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if cur.is_empty() || width(&cand) <= max_w {
            cur = cand;
        } else {
            lines.push(std::mem::replace(&mut cur, word.to_string()));
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.len() > max_lines {
        let mut last = lines[max_lines - 1..].join(" ");
        while width(&format!("{last}…")) > max_w && last.chars().count() > 1 {
            last.pop();
            last = last.trim_end().to_string();
        }
        lines.truncate(max_lines - 1);
        lines.push(format!("{last}…"));
    }
    lines
}

/// Base del fondo cuando Spotify no dio el color de la portada: el tono de la portada llevado a
/// la luminancia de los que sí da (~165).
fn base_from_tint(tint: Option<Color32>) -> [u8; 3] {
    match tint {
        Some(c) => {
            let k = 4.1;
            let f = |v: u8| (v as f32 * k).round().clamp(0.0, 255.0) as u8;
            [f(c.r()), f(c.g()), f(c.b())]
        }
        None => [150, 160, 180],
    }
}

/// Lo que el carril de la página recuerda entre fotogramas: el desplazamiento que pide (lo aplica
/// el área de desplazamiento al fotograma siguiente) y el seguimiento de la letra.
#[derive(Default)]
pub struct TrackScroll {
    /// Desplazamiento que fijar en el próximo fotograma.
    pub set: Option<f32>,
    /// Animación en curso: (desde, hasta, inicio).
    anim: Option<(f32, f32, f64)>,
    /// Sin seguir la canción hasta entonces (se movió la rueda).
    paused_until: f64,
    /// Para qué canción es lo de arriba.
    for_id: String,
}

impl App {
    /// Fondo de la página de canción: la parte `part` del panel `panel` (el fondo va fijo al panel;
    /// la barra fija de las pestañas lo vuelve a pintar debajo de ella).
    pub(super) fn track_backdrop(&mut self, painter: &egui::Painter, panel: Rect, part: Rect, page: Option<&TrackPage>) {
        let part = part.intersect(panel);
        if !part.is_positive() {
            return;
        }
        let r = 8u8;
        let corners = CornerRadius {
            nw: if part.min.y <= panel.min.y + 0.5 && part.min.x <= panel.min.x + 0.5 { r } else { 0 },
            ne: if part.min.y <= panel.min.y + 0.5 && part.max.x >= panel.max.x - 0.5 { r } else { 0 },
            sw: if part.max.y >= panel.max.y - 0.5 && part.min.x <= panel.min.x + 0.5 { r } else { 0 },
            se: if part.max.y >= panel.max.y - 0.5 && part.max.x >= panel.max.x - 0.5 { r } else { 0 },
        };
        // Mientras se dibuja, la página está fuera del mapa (`track_page`): la trae `page`.
        let Page::Track(id) = self.page().clone() else { return };
        let (url, base) = match page.or_else(|| self.track_pages.get(&id)) {
            Some(p) => {
                let url = p.album.as_ref().and_then(|a| a.cover(640)).map(str::to_string);
                let base = p.color.unwrap_or_else(|| base_from_tint(url.as_deref().and_then(|u| self.images.tint(u))));
                (url, base)
            }
            None => (None, [150, 160, 180]),
        };
        let flat = Color32::from_rgb((base[0] as f32 * 0.159 + 6.0) as u8, (base[1] as f32 * 0.159 + 6.0) as u8, (base[2] as f32 * 0.159 + 6.0) as u8);
        let ppp = painter.pixels_per_point();
        let (w, h) = ((panel.width() * ppp).round() as u32, (panel.height() * ppp).round() as u32);
        match url.and_then(|u| self.images.texture_backdrop(&u, w, h, base)) {
            Some(tex) => {
                let uv = Rect::from_min_max(
                    pos2((part.min.x - panel.min.x) / panel.width(), (part.min.y - panel.min.y) / panel.height()),
                    pos2((part.max.x - panel.min.x) / panel.width(), (part.max.y - panel.min.y) / panel.height()),
                );
                painter.add(egui::epaint::RectShape::filled(part, corners, Color32::WHITE).with_texture(tex.id, uv));
            }
            None => {
                painter.rect_filled(part, corners, flat);
            }
        }
    }

    pub(super) fn track_page(&mut self, ui: &mut egui::Ui, id: String) {
        if !self.signed_in() {
            self.welcome(ui);
            return;
        }
        self.request_once(&format!("trackpage:{id}"), Req::TrackPage(id.clone()));
        let Some(page) = self.track_pages.remove(&id) else {
            let o = ui.max_rect().min;
            let failed = self.track_page_failed.contains(&id);
            let text = if failed { "No se pudo cargar la canción. Pulsa para reintentar." } else { "Cargando…" };
            let g = ui.painter().layout_no_wrap(text.to_string(), theme::regular(17.0), INK.note);
            let rect = text_on_baseline(ui.painter(), pos2(o.x + COL_X, o.y + TITLE_BASE), g, INK.note);
            let r = ui.interact(rect, ui.id().with("track_retry"), Sense::click());
            if failed && r.clicked() {
                self.track_page_failed.remove(&id);
                self.requested.remove(&format!("trackpage:{id}"));
            }
            ui.allocate_space(vec2(ui.available_width(), 200.0));
            return;
        };
        self.track_page_body(ui, &page);
        self.track_pages.insert(id, page);
    }

    fn track_page_body(&mut self, ui: &mut egui::Ui, page: &TrackPage) {
        let o = ui.max_rect().min;
        let width = ui.max_rect().width();
        let at = |x: f32, y: f32| pos2(o.x + x, o.y + y);
        let painter = ui.painter().clone();
        let view = self.track_view.unwrap_or(ui.clip_rect());
        if self.track_scroll.for_id != page.id {
            self.track_scroll = TrackScroll { for_id: page.id.clone(), ..Default::default() };
        }

        // Portada.
        let cover_url = page.album.as_ref().and_then(|a| a.cover(640)).map(str::to_string);
        // Con el panel más estrecho que el de la referencia (1492), la portada se encoge (hasta
        // 160) y la columna de la derecha la sigue (`dx`).
        let side = (COVER_SIDE + width - REF_PANEL_W).clamp(MIN_COVER, COVER_SIDE);
        let dx = side - COVER_SIDE;
        let cover = Rect::from_min_size(at(COVER.0, COVER.1), vec2(side, side));
        self.cover_in(ui, cover_url.as_deref(), cover, COVER_RADIUS);

        // Título.
        let font = theme::semibold(TITLE_FONT);
        let title_w = TITLE_W.min(width - MARGIN - TITLE_INK_X - dx).max(120.0);
        let lines = wrap_words(&page.name, title_w, TITLE_MAX_LINES, |s| painter.layout_no_wrap(s.to_string(), font.clone(), INK.title).size().x);
        let mut lead = None;
        for (k, line) in lines.iter().enumerate() {
            let g = painter.layout_no_wrap(line.clone(), font.clone(), INK.title);
            // La primera línea fija dónde empieza la tinta; las demás van con el mismo origen.
            let l = *lead.get_or_insert(ink_left(&g));
            text_on_baseline(&painter, at(TITLE_INK_X + dx - l, TITLE_BASE + k as f32 * TITLE_PITCH), g, INK.title);
        }
        let dy = (lines.len().max(1) as f32 - TITLE_LINES_REF as f32) * TITLE_PITCH;

        // Datos: artista • álbum • año • duración • escuchas.
        self.track_meta(ui, &painter, page, at(COL_X + dx, META_BASE + dy), o.x + width - MARGIN);

        // Botones.
        self.track_buttons(ui, &painter, page, at(PLAY_C.0 + dx, PLAY_C.1 + dy));

        // Géneros.
        let mut y = CHIP_Y + dy;
        if !page.genres.is_empty() {
            let font = theme::semilight(CHIP_FONT);
            let max_x = width - MARGIN;
            let (mut x, mut row) = (CHIP_X + dx, 0usize);
            for g in page.genres.iter().take(6) {
                let gal = painter.layout_no_wrap(capitalize(g), font.clone(), INK.chip_text);
                let w = ink_right(&gal) - ink_left(&gal) + CHIP_PAD_L + CHIP_PAD_R;
                if x > CHIP_X + dx && x + w > max_x {
                    x = CHIP_X + dx;
                    row += 1;
                }
                let chip = Rect::from_min_size(at(x, y + row as f32 * (CHIP_H + CHIP_GAP)), vec2(w, CHIP_H));
                painter.rect_stroke(chip, CornerRadius::same((CHIP_H / 2.0).round() as u8), egui::Stroke::new(CHIP_STROKE, INK.chip), egui::StrokeKind::Middle);
                let l = ink_left(&gal);
                text_on_baseline(&painter, pos2(chip.min.x + CHIP_PAD_L - l, chip.min.y + CHIP_BASE), gal, INK.chip_text);
                x += w + CHIP_GAP;
            }
            y += (row + 1) as f32 * (CHIP_H + CHIP_GAP) - CHIP_GAP + AV_BELOW_CHIPS;
        }

        // Artistas con su foto y sus papeles.
        let mut bottom = COVER.1 + side;
        let mut missing = Vec::new();
        for (i, a) in page.artists.iter().enumerate() {
            let top = y + i as f32 * AV_PITCH;
            let avatar = Rect::from_min_size(at(AV_X + dx, top), vec2(AV_D, AV_D));
            bottom = bottom.max(top + AV_D);
            let img = a.cover(160).map(str::to_string).or_else(|| self.artists.get(&a.id).and_then(|pg| pg.artist.as_ref()).and_then(|ar| ar.cover(160)).map(str::to_string));
            match &img {
                Some(u) => self.cover_in(ui, Some(u), avatar, (AV_D / 2.0) as u8),
                None => {
                    painter.circle_filled(avatar.center(), AV_D / 2.0, Color32::from_white_alpha(20));
                    if !a.id.is_empty() {
                        missing.push(a.id.clone());
                    }
                }
            }
            let cy = avatar.center().y;
            let name = painter.layout_no_wrap(a.name.clone(), theme::regular(AV_FONT), INK.name);
            let roles: Vec<String> = page.roles_of(a).iter().map(|r| role_es(r)).collect();
            let role = painter.layout_no_wrap(roles.join(", "), theme::regular(AV_FONT), INK.role);
            let (ln, lr) = (ink_left(&name), ink_left(&role));
            let nrect = text_on_baseline(&painter, pos2(o.x + AV_TEXT_X + dx - ln, cy + AV_NAME_DY), name, INK.name);
            text_on_baseline(&painter, pos2(o.x + AV_TEXT_X + dx - lr, cy + AV_ROLE_DY), role, INK.role);
            let hit = ui.interact(avatar.union(nrect), ui.id().with(("track_artist", i)), Sense::click());
            if hit.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if hit.clicked() && !a.id.is_empty() {
                self.actions.push(Action::Go(Page::Artist(a.id.clone())));
            }
        }
        if !missing.is_empty() {
            self.request_artist_thumbs(missing);
        }

        // Pestañas y su línea (fijas arriba al bajar).
        let rule_nat = bottom + RULE_BELOW;
        let off = view.min.y - o.y;
        let pinned = off > rule_nat - BAR_H;
        let rule_y = if pinned { view.min.y + BAR_H } else { o.y + rule_nat };

        // Contenido de la pestaña.
        let content_end = match self.track_tab {
            0 => self.track_lyrics(ui, page, o, rule_nat, rule_y, view, pinned),
            1 => self.track_credits(ui, page, o, rule_nat),
            _ => self.track_related(ui, page, o, rule_nat),
        };

        // La barra, encima de lo que pasa por debajo.
        if pinned {
            let bar = Rect::from_min_max(pos2(view.min.x, view.min.y), pos2(view.max.x, rule_y + RULE_H));
            let panel = self.track_panel.unwrap_or(view);
            self.track_backdrop(&painter, panel, bar, Some(page));
            ui.interact(bar, ui.id().with("track_bar"), Sense::click());
        }
        painter.rect_filled(Rect::from_min_max(pos2(o.x, rule_y), pos2(o.x + width, rule_y + RULE_H)), CornerRadius::ZERO, INK.rule);
        let mut x = TAB_X;
        for (k, label) in TABS.iter().enumerate() {
            let on = self.track_tab == k as u8;
            let color = if on { INK.tab_on } else { INK.tab_off };
            let g = painter.layout_no_wrap(label.to_string(), theme::regular(TAB_FONT), color);
            let (l, r) = (ink_left(&g), ink_right(&g));
            let rect = text_on_baseline(&painter, pos2(o.x + x - l, rule_y - TAB_BASE_UP), g, color);
            let hit = ui.interact(rect.expand2(vec2(10.0, 14.0)), ui.id().with(("track_tab", k)), Sense::click());
            if hit.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if hit.clicked() {
                self.track_tab = k as u8;
            }
            if on {
                let u = Rect::from_min_max(pos2(o.x + x - UNDER_L, rule_y - UNDER_UP), pos2(o.x + x + (r - l) + UNDER_R, rule_y - UNDER_UP + UNDER_H));
                painter.rect_filled(u, CornerRadius::ZERO, GREEN);
            }
            x += (r - l) + TAB_GAP;
        }
        // Columna de iconos de la letra (también fija).
        if self.track_tab == 0 {
            self.track_lyric_icons(ui, &painter, o, rule_y, page);
        }

        ui.allocate_rect(Rect::from_min_size(o, vec2(width, content_end.max(view.height()))), Sense::hover());
    }

    /// Artista • álbum • año • duración • escuchas, desde (`start` = borde de la columna, línea
    /// base) y sin pasar de `right`: si no cabe, se recortan con «…» el álbum y, si aún hace falta,
    /// el artista.
    fn track_meta(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, page: &TrackPage, start: egui::Pos2, right: f32) {
        let base = start.y;
        let font = theme::semilight(META_FONT);
        let ink_w = |g: &egui::Galley| ink_right(g) - ink_left(g);
        // Lo que va tras un punto: la tinta 12,5 a cada lado.
        let dot = |x: f32| -> f32 {
            let c = x + META_DOT_GAP;
            painter.circle_filled(pos2(c, base + META_DOT_DY), META_DOT_R, INK.dot);
            c
        };
        let artist = page.artists.first();
        let album = page.album.as_ref();
        let mut tail: Vec<String> = Vec::new();
        if let Some(y) = album.map(|a| a.year().to_string()).filter(|y| !y.is_empty()) {
            tail.push(y);
        }
        tail.push(super::watchdog::mmss(page.duration_ms));
        if let Some(n) = page.playcount {
            tail.push(fmt_thousands(n));
        }
        let tail: Vec<std::sync::Arc<egui::Galley>> = tail.into_iter().map(|t| painter.layout_no_wrap(t, font.clone(), INK.meta)).collect();
        let tail_w: f32 = tail.iter().map(|g| 2.0 * META_DOT_GAP + ink_w(g)).sum();
        // Sitio para los nombres: lo que queda hasta `right` sin lo de detrás.
        let names_room = (right - start.x - META_ARTIST_ICON - META_ARTIST_TEXT - tail_w).max(60.0);
        let album_extra = META_DOT_GAP + META_DOT_ICON + META_ALBUM_TEXT;
        let mut ga = artist.map(|a| painter.layout_no_wrap(a.name.clone(), font.clone(), INK.strong));
        let mut gb = album.map(|al| painter.layout_no_wrap(al.name.clone(), font.clone(), INK.strong));
        let need = ga.as_ref().map_or(0.0, |g| ink_w(g)) + gb.as_ref().map_or(0.0, |g| album_extra + ink_w(g));
        if need > names_room {
            // Primero el álbum (hasta un tercio del sitio); luego el artista.
            let wa = ga.as_ref().map_or(0.0, |g| ink_w(g));
            if let (Some(al), Some(_)) = (album, &gb) {
                let max_b = (names_room - wa - album_extra).max(names_room / 3.0);
                gb = Some(galley_truncated(painter, &al.name, font.clone(), INK.strong, max_b));
            }
            let wb = gb.as_ref().map_or(0.0, |g| album_extra + ink_w(g));
            if let (Some(a), true) = (artist, wa + wb > names_room) {
                ga = Some(galley_truncated(painter, &a.name, font.clone(), INK.strong, (names_room - wb).max(40.0)));
            }
        }
        let mut x = start.x;
        // Artista.
        if let (Some(a), Some(g)) = (artist, ga) {
            let c = start.x + META_ARTIST_ICON;
            paint_mask(painter, "song-meta-artist", &sm::META_ARTIST, px(painter, pos2(c, base)), INK.icon);
            let (l, w) = (ink_left(&g), ink_w(&g));
            let tx = c + META_ARTIST_TEXT;
            let rect = text_on_baseline(painter, pos2(tx - l, base), g, INK.strong);
            self.meta_link(ui, rect, "track_meta_artist", (!a.id.is_empty()).then(|| Page::Artist(a.id.clone())));
            x = tx + w;
        }
        // Álbum.
        if let (Some(al), Some(g)) = (album, gb) {
            let c = if artist.is_some() { dot(x) + META_DOT_ICON } else { start.x + META_ARTIST_ICON };
            paint_mask(painter, "song-meta-album", &sm::META_ALBUM, px(painter, pos2(c, base)), INK.icon);
            let (l, w) = (ink_left(&g), ink_w(&g));
            let tx = c + META_ALBUM_TEXT;
            let rect = text_on_baseline(painter, pos2(tx - l, base), g, INK.strong);
            self.meta_link(ui, rect, "track_meta_album", al.id.clone().map(Page::Album));
            x = tx + w;
        }
        for g in tail {
            let c = dot(x);
            let (l, w) = (ink_left(&g), ink_w(&g));
            let tx = c + META_DOT_GAP;
            text_on_baseline(painter, pos2(tx - l, base), g, INK.meta);
            x = tx + w;
        }
    }

    /// Un texto de los datos que lleva a una página.
    fn meta_link(&mut self, ui: &mut egui::Ui, rect: Rect, id: &str, to: Option<Page>) {
        let Some(to) = to else { return };
        let r = ui.interact(rect, ui.id().with(id), Sense::click());
        if r.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            ui.painter().line_segment([pos2(rect.min.x, rect.max.y - 2.0), pos2(rect.max.x, rect.max.y - 2.0)], egui::Stroke::new(1.0, INK.strong));
        }
        if r.clicked() {
            self.actions.push(Action::Go(to));
        }
    }

    /// Play, me gusta, añadir a una playlist, a la cola, descargar, compartir y más.
    fn track_buttons(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, page: &TrackPage, play: egui::Pos2) {
        let now = self.player.now.as_ref().and_then(|n| n.id.clone());
        let current = now.as_deref() == Some(page.id.as_str());
        let playing = current && self.player.state == PlayState::Playing;
        // Play / pausa.
        let rect = Rect::from_center_size(play, vec2(2.0 * PLAY_R, 2.0 * PLAY_R));
        let r = ui.interact(rect, ui.id().with("track_play"), Sense::click()).on_hover_text(if playing { "Pausar" } else { "Reproducir" });
        let green = if r.hovered() { GREEN.lerp_to_gamma(Color32::WHITE, 0.15) } else { GREEN };
        painter.circle_filled(play, PLAY_R, green);
        if playing {
            for dx in [-4.25, 4.25] {
                painter.rect_filled(Rect::from_center_size(pos2(play.x + dx, play.y), vec2(4.0, 13.5)), CornerRadius::same(1), Color32::BLACK);
            }
        } else {
            paint_mask(painter, "song-play", &sm::PLAY, px(painter, play), Color32::BLACK);
        }
        if r.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if r.clicked() {
            if current {
                self.play_pause();
            } else {
                let target = match page.album.as_ref().and_then(|a| a.uri.clone()) {
                    Some(uri) => PlayTarget::Context { uri, track_uri: Some(page.uri.clone()), index: None, shuffle: false },
                    None => PlayTarget::Tracks { uris: vec![page.uri.clone()], index: Some(0), shuffle: false },
                };
                self.actions.push(Action::Play(target));
            }
        }

        let liked = self.liked_set.contains(&page.id);
        let downloaded = self.downloaded.contains(&page.id);
        let buttons: [(&'static str, &LibMask, &str); 6] = [
            ("song-heart", &sm::HEART, if liked { "Quitar de Me gusta" } else { "Guardar en Me gusta" }),
            ("song-add", &sm::ADD, "Añadir a una playlist"),
            ("song-queue", &sm::QUEUE, "Añadir a la cola"),
            ("song-save", &sm::SAVE, if downloaded { "Descargada" } else { "Descargar" }),
            ("song-share", &sm::SHARE, "Copiar el enlace de la canción"),
            ("song-more", &sm::MORE, "Más opciones"),
        ];
        for (k, (name, mask, tip)) in buttons.into_iter().enumerate() {
            let c = pos2(play.x + BTN_PITCH * (k + 1) as f32, play.y);
            let rect = Rect::from_center_size(c, vec2(40.0, 40.0));
            let r = ui.interact(rect, ui.id().with(("track_btn", k)), Sense::click()).on_hover_text(tip);
            let on = (k == 0 && liked) || (k == 3 && downloaded) || (k == 5 && self.song_more.as_ref().is_some_and(|m| m.row.0 == "track_page"));
            let base = if on && k != 5 { GREEN } else if on { Color32::WHITE } else { INK.icon };
            let color = if r.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                if on { base } else { Color32::WHITE }
            } else {
                base
            };
            if k == 0 && liked {
                // Relleno: el corazón de las filas, del mismo tamaño que la máscara (22,4 × 19,4).
                super::icons::paint(painter, Rect::from_center_size(pos2(c.x, c.y + 0.6), vec2(27.0, 27.0)), GREEN, super::icons::Icon::HeartFilled);
            } else {
                paint_mask(painter, name, mask, px(painter, c), color);
            }
            if r.clicked() {
                match k {
                    0 => self.actions.push(Action::Like(page.id.clone(), !liked)),
                    1 => self.open_add_dialog(vec![page.uri.clone()]),
                    2 => {
                        self.actions.push(Action::AddToQueue(page.uri.clone()));
                        self.status("Añadida a la cola");
                    }
                    3 if !downloaded => self.actions.push(Action::Download(vec![page.id.clone()])),
                    4 => self.actions.push(Action::CopyText(uri_to_link(&page.uri), "Enlace")),
                    5 => self.toggle_song_menu(SongMenu {
                        track: track_of(page),
                        album_id: page.album.as_ref().and_then(|a| a.id.clone()),
                        remove_from: None,
                        row: ("track_page".to_string(), 0),
                        anchor: Anchor::Dots(rect),
                        sub: None,
                    }),
                    _ => {}
                }
            }
        }
    }

    /// La columna de iconos de la letra: sincronizar y tres que aún no hacen nada.
    fn track_lyric_icons(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, o: egui::Pos2, rule_y: f32, page: &TrackPage) {
        let icons: [(&'static str, &LibMask, &str); 4] = [
            ("song-lyr-sync", &sm::LYR_SYNC, ""),
            ("song-lyr-translate", &sm::LYR_TRANSLATE, "Traducir (pronto)"),
            ("song-lyr-comment", &sm::LYR_COMMENT, "Comentarios (pronto)"),
            ("song-lyr-size", &sm::LYR_SIZE, "Tamaño del texto (pronto)"),
        ];
        let on = self.settings.lyrics_sync;
        for (k, (name, mask, tip)) in icons.into_iter().enumerate() {
            let c = pos2(o.x + LYR_ICON_X, rule_y + LYR_ICON_DY + k as f32 * LYR_ICON_PITCH);
            let tip = if k == 0 {
                if on { "La letra sigue a la canción (pulsa para moverte libremente)" } else { "Seguir la canción" }
            } else {
                tip
            };
            let r = ui.interact(Rect::from_center_size(c, vec2(40.0, 40.0)), ui.id().with(("track_lyr_icon", k)), Sense::click()).on_hover_text(tip);
            let base = if k == 0 && on { GREEN } else { INK.icon };
            let color = if r.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                base.lerp_to_gamma(Color32::WHITE, 0.3)
            } else {
                base
            };
            paint_mask(painter, name, mask, px(painter, c), color);
            if k == 0 && r.clicked() {
                self.settings.lyrics_sync = !on;
                self.draft.lyrics_sync = !on;
                if !self.ephemeral {
                    self.settings.save(&self.paths);
                }
                // Al encenderla, la página baja hasta lo que suena.
                self.track_scroll.paused_until = 0.0;
                self.track_follow_now = !on && self.player.now.as_ref().and_then(|n| n.id.as_deref()) == Some(page.id.as_str());
            }
        }
    }

    /// La letra, desde la línea de las pestañas (`rule_nat`, desde arriba de la página). Devuelve
    /// dónde acaba (desde arriba de la página).
    #[allow(clippy::too_many_arguments)]
    fn track_lyrics(&mut self, ui: &mut egui::Ui, page: &TrackPage, o: egui::Pos2, rule_nat: f32, rule_y: f32, view: Rect, pinned: bool) -> f32 {
        let painter = ui.painter().clone();
        // Con el panel más estrecho, la letra empieza proporcionalmente más a la izquierda.
        let line_x = (ui.max_rect().width() * LINE_X / REF_PANEL_W).clamp(120.0, LINE_X);
        let id = page.id.clone();
        let now_id = self.player.now.as_ref().and_then(|n| n.id.clone());
        let current = now_id.as_deref() == Some(id.as_str());
        let (lyrics, loading) = if current {
            self.ensure_lyrics();
            (self.lyrics.clone().filter(|l| l.track_id == id), self.lyrics_loading)
        } else {
            match self.lyrics_cache.get(&id).cloned() {
                Some(l) => (l, false),
                None => {
                    let due = self.track_lyrics_asked.as_ref().is_none_or(|(k, t)| *k != id || t.elapsed() > Duration::from_secs(12));
                    if due {
                        self.track_lyrics_asked = Some((id.clone(), std::time::Instant::now()));
                        match self.lyrics_from_disk(&id) {
                            Some(l) => {
                                self.lyrics_cache.insert(id.clone(), Some(l.clone()));
                            }
                            None => self.api.send(Req::LyricsPrefetch { id: id.clone() }),
                        }
                    }
                    (self.lyrics_cache.get(&id).cloned().flatten(), !self.lyrics_cache.contains_key(&id))
                }
            }
        };
        let note = |text: &str| {
            let g = painter.layout_no_wrap(text.to_string(), theme::regular(17.0), INK.note);
            let l = ink_left(&g);
            text_on_baseline(&painter, pos2(o.x + line_x - l, o.y + rule_nat + LINE_FIRST), g, INK.note);
        };
        let Some(lyrics) = lyrics else {
            note(if loading { "Buscando la letra…" } else { "Esta canción no tiene letra disponible." });
            return rule_nat + LINE_FIRST + 60.0;
        };
        if current {
            self.prefetch_next_lyrics();
        }
        let synced = lyrics.synced();
        let pos = self.player.position();
        let cur_line = if current { lyrics.current_line(pos) } else { None };
        let follow = self.settings.lyrics_sync && synced && current;
        let playing = current && self.player.state == PlayState::Playing;

        // Maqueta (desde la línea).
        let wrap = (o.x + ui.max_rect().width() - MARGIN) - (o.x + line_x);
        let font = theme::semibold(LINE_FONT);
        let mut rows: Vec<(f32, Option<std::sync::Arc<egui::Galley>>)> = Vec::with_capacity(lyrics.lines.len());
        let mut y = LINE_FIRST;
        for line in &lyrics.lines {
            let words = line.words.trim();
            if words.is_empty() {
                rows.push((y, None));
                y += LINE_BLANK;
                continue;
            }
            let g = painter.layout(words.to_string(), font.clone(), INK.sung, wrap);
            let extra = match (g.rows.first(), g.rows.last()) {
                (Some(a), Some(b)) => b.pos.y - a.pos.y,
                _ => 0.0,
            };
            rows.push((y, Some(g)));
            y += LINE_PITCH + extra;
        }
        let end = rule_nat + y + 40.0;

        // Seguir la canción: solo con la letra fija arriba (se bajó hasta ella) o recién encendida.
        let now = ui.input(|i| i.time);
        let wheel = ui.rect_contains_pointer(view) && ui.input(|i| i.smooth_scroll_delta.y != 0.0);
        if wheel && !self.track_follow_now {
            self.track_scroll.paused_until = now + FOLLOW_PAUSE;
            self.track_scroll.anim = None;
        }
        let off = view.min.y - o.y;
        let engaged = follow && (pinned || self.track_follow_now) && now >= self.track_scroll.paused_until;
        if engaged {
            let pin_at = rule_nat - BAR_H;
            let anchor = BAR_H + (FOLLOW_AT * (view.height() - BAR_H)).max(LINE_FIRST);
            let target = cur_line.map(|i| rule_nat + rows[i].0 - anchor).unwrap_or(pin_at).max(pin_at + 1.0);
            match self.track_scroll.anim {
                Some((_, to, _)) if (to - target).abs() < 0.5 => {}
                _ if (off - target).abs() < 0.5 => {
                    self.track_scroll.anim = None;
                    self.track_follow_now = false;
                }
                _ => self.track_scroll.anim = Some((off, target, now)),
            }
            if let Some((from, to, t0)) = self.track_scroll.anim {
                let (v, done) = scroll_step(from, to, now - t0);
                self.track_scroll.set = Some(v);
                if done {
                    self.track_scroll.anim = None;
                    self.track_follow_now = false;
                } else {
                    ui.ctx().request_repaint();
                    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(crate::shell::ANIM_UNTIL_KEY), now + 0.1));
                }
            }
        } else {
            self.track_scroll.anim = None;
            if !follow {
                self.track_follow_now = false;
            }
        }
        let _ = rule_y;

        // Renglones.
        let clip = Rect::from_min_max(pos2(view.min.x, view.min.y), view.max);
        let mut seek_to = None;
        for (i, (base, g)) in rows.iter().enumerate() {
            let Some(g) = g else { continue };
            let base = o.y + rule_nat + base;
            let lead = ink_left(g);
            let min = pos2((o.x + line_x - lead).round(), (base - first_base(g)).round());
            let area = Rect::from_min_size(min, g.size());
            if area.max.y < clip.min.y || area.min.y > clip.max.y {
                continue;
            }
            let line_end = |i: usize| {
                let l = &lyrics.lines[i];
                l.end_ms.or_else(|| lyrics.lines.get(i + 1).map(|n| n.start_ms)).unwrap_or(page.duration_ms)
            };
            let sung = (follow && cur_line == Some(i)).then(|| sung_chars(&lyrics.lines[i], pos, line_end(i)));
            let past = !follow || cur_line.is_some_and(|c| i < c) || matches!(sung, Some(None));
            let hit = (synced && current).then(|| ui.interact(area.expand2(vec2(4.0, 6.0)), ui.id().with(("track_line", i)), Sense::click()));
            let hovered = hit.as_ref().is_some_and(|r| r.hovered());
            if hovered {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if past {
                painter.galley_with_override_text_color(min, g.clone(), INK.sung);
            } else {
                let ahead = if hovered { INK.ahead.gamma_multiply(1.8) } else { INK.ahead };
                painter.galley_with_override_text_color(min, g.clone(), ahead);
                if let Some(Some(chars)) = sung {
                    for (k, x) in sung_rows(g, chars) {
                        let rr = g.rows[k].rect().translate(min.to_vec2());
                        let c = Rect::from_min_max(pos2(rr.min.x - 3.0, rr.min.y - 2.0), pos2(min.x + x, rr.max.y + 6.0)).intersect(clip);
                        painter.with_clip_rect(c).galley_with_override_text_color(min, g.clone(), INK.sung);
                    }
                }
            }
            if hit.is_some_and(|r| r.clicked()) {
                seek_to = Some(lyrics.lines[i].start_ms);
            }
        }
        if let Some(ms) = seek_to {
            self.seek(ms);
            self.track_scroll.paused_until = 0.0;
        }
        if follow && playing {
            let by_syllable = cur_line.is_some_and(|c| !lyrics.lines[c].syllables.is_empty());
            let next = lyrics.lines.iter().map(|l| l.start_ms).find(|&t| t > pos);
            if by_syllable {
                ui.ctx().request_repaint_after(Duration::from_millis(50));
            } else if let Some(t) = next {
                ui.ctx().request_repaint_after(Duration::from_millis((t - pos) as u64 + 5));
            }
        }
        end
    }

    /// Créditos por grupos (Interpretada por, Escrita por, Producida por…).
    fn track_credits(&mut self, ui: &mut egui::Ui, page: &TrackPage, o: egui::Pos2, rule_nat: f32) -> f32 {
        let painter = ui.painter().clone();
        let mut y = rule_nat + 52.0;
        if page.credits.is_empty() {
            let g = painter.layout_no_wrap("Spotify no tiene créditos de esta canción.".to_string(), theme::regular(17.0), INK.note);
            text_on_baseline(&painter, pos2(o.x + TAB_X, o.y + y), g, INK.note);
            return y + 60.0;
        }
        for (gi, group) in page.credits.iter().enumerate() {
            let g = painter.layout_no_wrap(credit_group_es(&group.title), theme::semibold(19.0), INK.title);
            let l = ink_left(&g);
            text_on_baseline(&painter, pos2(o.x + TAB_X - l, o.y + y), g, INK.title);
            y += 44.0;
            for (pi, c) in group.people.iter().enumerate() {
                let name = painter.layout_no_wrap(c.name.clone(), theme::regular(AV_FONT), INK.name);
                let roles: Vec<String> = c.roles.iter().map(|r| role_es(r)).collect();
                let role = painter.layout_no_wrap(roles.join(", "), theme::regular(AV_FONT), INK.role);
                let (ln, lr) = (ink_left(&name), ink_left(&role));
                let rect = text_on_baseline(&painter, pos2(o.x + TAB_X - ln, o.y + y), name, INK.name);
                text_on_baseline(&painter, pos2(o.x + TAB_X - lr, o.y + y + 32.0), role, INK.role);
                if let Some(aid) = &c.id {
                    let r = ui.interact(rect, ui.id().with(("track_credit", gi, pi)), Sense::click());
                    if r.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if r.clicked() {
                        self.actions.push(Action::Go(Page::Artist(aid.clone())));
                    }
                }
                y += 72.0;
            }
            y += 20.0;
        }
        y + 20.0
    }

    /// «Más como esta»: las recomendadas a partir de la canción, como la tabla de una playlist.
    fn track_related(&mut self, ui: &mut egui::Ui, page: &TrackPage, o: egui::Pos2, rule_nat: f32) -> f32 {
        let top = rule_nat + 24.0;
        if page.related.is_empty() {
            let g = ui.painter().layout_no_wrap("Spotify no tiene recomendaciones para esta canción.".to_string(), theme::regular(17.0), INK.note);
            text_on_baseline(ui.painter(), pos2(o.x + TAB_X, o.y + top + 28.0), g, INK.note);
            return top + 80.0;
        }
        let rect = Rect::from_min_max(pos2(o.x + MARGIN, o.y + top), pos2(o.x + ui.max_rect().width() - MARGIN, o.y + top + 100_000.0));
        let mut c = child_in(ui, rect, egui::Layout::top_down(egui::Align::Min));
        c.spacing_mut().item_spacing = vec2(0.0, 0.0);
        let key = format!("track_related:{}", page.id);
        self.track_rows(
            &mut c,
            &key,
            &page.related,
            RowOpts {
                show_cover: true,
                // Las recomendadas no traen el nombre del álbum.
                show_album: false,
                numbered: true,
                header: true,
                source: Source::Tracks,
                editable_playlist: None,
                selectable: true,
                select: false,
            },
        );
        top + c.min_rect().height() + 24.0
    }
}

/// La canción de la página como pista (para el menú de los tres puntos).
fn track_of(page: &TrackPage) -> Track {
    Track {
        id: Some(page.id.clone()),
        uri: page.uri.clone(),
        name: page.name.clone(),
        duration_ms: page.duration_ms,
        artists: page
            .artists
            .iter()
            .map(|a| ArtistRef { id: (!a.id.is_empty()).then(|| a.id.clone()), name: a.name.clone(), uri: (!a.uri.is_empty()).then(|| a.uri.clone()) })
            .collect(),
        album: page.album.clone(),
        kind: Some("track".to_string()),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titulo_partido_por_palabras() {
        // Ancho = número de caracteres.
        let w = |s: &str| s.chars().count() as f32;
        assert_eq!(wrap_words("Get Lucky (feat. Pharrell Williams and Nile Rodgers)", 38.0, 3, w), ["Get Lucky (feat. Pharrell Williams and", "Nile Rodgers)"]);
        assert_eq!(wrap_words("Corta", 38.0, 3, w), ["Corta"]);
        // Una palabra más larga que el ancho va sola; con más de 3 líneas, la última con «…».
        assert_eq!(wrap_words("aaaaaaaaaa b", 5.0, 3, w), ["aaaaaaaaaa", "b"]);
        let l = wrap_words("uno dos tres cuatro cinco seis", 4.0, 3, w);
        assert_eq!(l.len(), 3);
        assert!(l[2].ends_with('…') && w(&l[2]) <= 4.0, "{l:?}");
    }

    #[test]
    fn base_del_fondo_sin_color_de_spotify() {
        assert_eq!(base_from_tint(None), [150, 160, 180]);
        let b = base_from_tint(Some(Color32::from_rgb(30, 39, 87)));
        assert_eq!(b, [123, 160, 255]);
    }
}
