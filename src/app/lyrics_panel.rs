//! Panel de la letra (el botón de la letra del reproductor), copiado de las referencias de diseño
//! 2.webp (cliente = (ref − (8, −3)) / 0,852) y 3.webp (cliente = (ref + (0,25, 1)) / 0,94): un
//! cristal de 432 × 536 sobre el contenido, con lo de detrás desenfocado como los menús, el borde
//! derecho a 27,5 px del centro del botón y el de abajo a 96,5 del borde de la ventana. Arriba,
//! «Letra ↗», sincronizar y compartir; debajo, la letra: lo ya cantado en blanco, lo que falta en
//! gris y el renglón que suena rellenándose de blanco según avanza.
//!
//! Sincronizar (verde, con un punto debajo) hace que la letra siga a la canción; apagado, se
//! desplaza con la rueda libremente. No se cierra al pulsar fuera ni con Escape: solo con su
//! botón (o la tecla L).

use std::time::Duration;

use egui::{pos2, vec2, Color32, CornerRadius, Pos2, Rect, Sense, Stroke};

use super::icons::{self, Icon};
use super::theme::{self, GREEN};
use super::widgets::{galley_truncated, text_on_baseline, uri_to_link};
use super::{Action, App, PlayState};
use crate::model::LyricLine;

/// Tamaño del panel; el borde derecho, a 27,5 px del centro del botón de la letra (como «Añadir
/// a una playlist») y el de abajo, a 96,5 del borde de la ventana. Esquinas de 8.
const W: f32 = 432.0;
const H: f32 = 536.0;
const RIGHT_OF_BUTTON: f32 = 27.5;
const FROM_BOTTOM: f32 = 96.5;
const RADIUS: u8 = 8;
/// El mismo desenfoque que los menús de cristal.
const BLUR_SIGMA: f32 = 56.0;

/// Cabecera (desde la esquina del panel): «Letra» (Segoe UI 14) con la tinta desde 23,5 y la línea
/// base a 32,9; la flecha ↗ con su esquina en (89,5, 22,5) y brazos de 9; sincronizar, un reloj de
/// radio 8,8 centrado en (349,8, 28) abierto arriba a la izquierda, donde van dos rayas, y el
/// punto verde (radio 1,9) en (348, 43,5) cuando está activo; compartir con su tinta centrada en
/// (404, 26,5). La línea de debajo, a 53,5.
const TITLE_X: f32 = 23.5;
const TITLE_BASE: f32 = 32.9;
const TITLE_FONT: f32 = 14.0;
const ARROW_CORNER: (f32, f32) = (89.5, 22.5);
const ARROW_ARM: f32 = 9.0;
const ARROW_TAIL: (f32, f32) = (79.0, 33.0);
const SYNC_C: (f32, f32) = (349.8, 28.0);
const SYNC_R: f32 = 8.8;
const SYNC_DOT: (f32, f32) = (348.0, 43.5);
const SHARE_C: (f32, f32) = (404.0, 26.5);
const RULE_Y: f32 = 53.5;

/// Letra: Segoe UI seminegrita 21 con la tinta desde 23,5; un renglón cada 42 (la primera línea
/// base, 42 por debajo de la línea de la cabecera) y 27 por cada renglón vacío (pausa entre
/// estrofas). Con la sincronización, la línea base del renglón que suena queda a 237 de la línea.
const LINE_X: f32 = 23.5;
const LINE_FONT: f32 = 21.0;
const LINE_PITCH: f32 = 42.0;
const BLANK: f32 = 27.0;
const FIRST_BASE: f32 = 42.0;
const FOLLOW_AT: f32 = 237.0;

/// Colores: los de la referencia en oscuro; en claro, los de la paleta.
struct Ink {
    dark: bool,
    glass: Color32,
    title: Color32,
    icon: Color32,
    rule: Color32,
    sung: Color32,
    ahead: Color32,
    note: Color32,
}

fn ink(ctx: &egui::Context) -> Ink {
    let p = theme::palette(ctx);
    if p.dark {
        Ink {
            dark: true,
            glass: Color32::from_rgba_unmultiplied(34, 34, 34, 191),
            title: Color32::from_gray(228),
            icon: Color32::from_gray(155),
            rule: Color32::from_white_alpha(34),
            sung: Color32::from_gray(250),
            ahead: Color32::from_white_alpha(56),
            note: Color32::from_gray(150),
        }
    } else {
        Ink {
            dark: false,
            glass: Color32::from_rgba_unmultiplied(250, 250, 250, 225),
            title: p.text,
            icon: p.weak,
            rule: p.border,
            sung: p.text,
            ahead: p.faint,
            note: p.weak,
        }
    }
}

fn glass(painter: &egui::Painter, rect: Rect, ink: &Ink) {
    if ink.dark {
        painter.add(egui::Shape::Callback(egui::epaint::PaintCallback {
            rect,
            callback: std::sync::Arc::new(crate::raster::BackdropBlur { sigma: BLUR_SIGMA, corner: RADIUS as f32 }),
        }));
    }
    painter.rect_filled(rect, CornerRadius::same(RADIUS), ink.glass);
}

/// Línea base de la primera fila de un texto ya maquetado (desde su esquina).
fn first_base(g: &egui::Galley) -> f32 {
    g.rows.first().and_then(|r| r.row.glyphs.first().map(|gl| r.pos.y + gl.pos.y)).unwrap_or(g.size().y * 0.8)
}

/// Donde empieza la tinta del primer carácter (para alinear tinta, no cajas).
fn ink_left(g: &egui::Galley) -> f32 {
    g.rows.first().and_then(|r| r.row.glyphs.first()).map(|gl| gl.uv_rect.offset.x.max(0.0) + gl.pos.x).unwrap_or(0.0)
}

/// Caracteres ya cantados de un renglón con tiempos por sílaba (con decimales: la sílaba que suena
/// se va llenando hasta que empieza la siguiente); `None` si la letra solo trae el comienzo de
/// cada renglón, que entonces se ilumina entero. `end`: cuando acaba el renglón.
fn sung_chars(line: &LyricLine, pos: u32, end: u32) -> Option<f32> {
    if line.syllables.is_empty() {
        return None;
    }
    let mut done = 0.0;
    for (k, syl) in line.syllables.iter().enumerate() {
        if pos < syl.start_ms {
            break;
        }
        let next = line.syllables.get(k + 1).map(|n| n.start_ms).unwrap_or(end).max(syl.start_ms + 1);
        let f = ((pos - syl.start_ms) as f32 / (next - syl.start_ms) as f32).min(1.0);
        done += syl.chars as f32 * f;
        if f < 1.0 {
            break;
        }
    }
    // Los espacios del principio no se pintan (el renglón se pinta recortado).
    let lead = line.words.chars().count() - line.words.trim_start().chars().count();
    Some((done - lead as f32).max(0.0))
}

/// Hasta dónde llega lo cantado en cada fila del texto maquetado: (fila, x desde la esquina del
/// texto), las filas enteras primero y la que va a medias al final.
fn sung_rows(g: &egui::Galley, mut chars: f32) -> Vec<(usize, f32)> {
    let mut out = Vec::new();
    for (k, r) in g.rows.iter().enumerate() {
        if chars <= 0.0 {
            break;
        }
        let n = r.row.glyphs.len() as f32;
        if chars >= n {
            out.push((k, r.pos.x + r.row.size.x));
            chars -= n;
            continue;
        }
        let gl = &r.row.glyphs[chars.floor() as usize];
        out.push((k, r.pos.x + gl.pos.x + chars.fract() * gl.advance_width));
        break;
    }
    out
}

/// Desplazamiento que deja la línea base `base` (desde arriba de la lista) a `FOLLOW_AT`, sin
/// pasar de los límites.
fn follow_offset(base: f32, max_off: f32) -> f32 {
    (base - FOLLOW_AT).clamp(0.0, max_off.max(0.0))
}

impl App {
    /// El panel de la letra, abierto con su botón (`side` = Letra). `content` es el panel del
    /// contenido: sin el botón a la vista, el panel va pegado a su borde derecho.
    pub(super) fn lyrics_float(&mut self, ctx: &egui::Context, content: Rect) {
        let screen = ctx.content_rect();
        let right = match self.lyrics_button {
            Some(b) => (b.center().x + RIGHT_OF_BUTTON).min(content.max.x - 1.0),
            None => content.max.x - 1.0,
        };
        let bottom = screen.max.y - FROM_BOTTOM;
        let top = (bottom - H).max(content.min.y + 2.0);
        let left = (right - W).max(content.min.x + 1.0);
        let rect = Rect::from_min_max(pos2(left, top), pos2(right, bottom));
        if rect.height() < 160.0 {
            return;
        }
        let ink = ink(ctx);
        egui::Area::new(egui::Id::new("lyrics_panel"))
            .order(egui::Order::Middle)
            .fixed_pos(rect.min)
            .show(ctx, |ui| {
                ui.set_clip_rect(rect);
                // Todo el panel recoge el puntero: lo de debajo no reacciona.
                ui.allocate_rect(rect, Sense::click());
                let painter = ui.painter().clone();
                glass(&painter, rect, &ink);
                self.lyrics_header(ui, &painter, rect, &ink);
                let y = rect.min.y + RULE_Y;
                painter.rect_filled(Rect::from_min_max(pos2(rect.min.x, y - 0.5), pos2(rect.max.x, y + 0.5)), CornerRadius::ZERO, ink.rule);
                let list = Rect::from_min_max(pos2(rect.min.x, y + 0.5), rect.max);
                self.lyrics_list(ui, list, &ink);
            });
    }

    fn lyrics_header(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, rect: Rect, ink: &Ink) {
        let at = |x: f32, y: f32| pos2(rect.min.x + x, rect.min.y + y);
        let g = galley_truncated(painter, "Letra", theme::regular(TITLE_FONT), ink.title, 100.0);
        let lead = ink_left(&g);
        text_on_baseline(painter, at(TITLE_X - lead, TITLE_BASE), g, ink.title);
        // ↗ (aún sin función).
        let s = Stroke::new(1.8, ink.icon);
        let c = at(ARROW_CORNER.0, ARROW_CORNER.1);
        painter.line_segment([at(ARROW_TAIL.0, ARROW_TAIL.1), c], s);
        painter.add(egui::Shape::line(vec![pos2(c.x - ARROW_ARM, c.y), c, pos2(c.x, c.y + ARROW_ARM)], s));

        // Sincronizar.
        let on = self.settings.lyrics_sync;
        let sc = at(SYNC_C.0, SYNC_C.1);
        let r = ui
            .interact(Rect::from_center_size(pos2(sc.x - 2.0, sc.y + 2.0), vec2(34.0, 38.0)), egui::Id::new("lyrics_sync"), Sense::click())
            .on_hover_text(if on { "La letra sigue a la canción (pulsa para moverte libremente)" } else { "Seguir la canción" });
        let base = if on { GREEN } else { ink.icon };
        let color = if r.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            base.lerp_to_gamma(Color32::WHITE, 0.3)
        } else {
            base
        };
        paint_sync(painter, sc, color);
        if on {
            painter.circle_filled(at(SYNC_DOT.0, SYNC_DOT.1), 1.9, color);
        }
        if r.clicked() {
            self.settings.lyrics_sync = !on;
            self.draft.lyrics_sync = !on;
            if !self.ephemeral {
                self.settings.save(&self.paths);
            }
        }

        // Compartir: copia el renglón que suena (o el primero) con la canción y su enlace.
        let shc = at(SHARE_C.0, SHARE_C.1);
        let r = ui.interact(Rect::from_center_size(shc, vec2(34.0, 34.0)), egui::Id::new("lyrics_share"), Sense::click()).on_hover_text("Compartir este fragmento");
        let color = if r.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            ink.icon.lerp_to_gamma(Color32::WHITE, 0.45)
        } else {
            ink.icon
        };
        // Su dibujo (rejilla de 28) tiene la tinta 0,25 a la derecha y 1,05 arriba del centro.
        icons::paint(painter, Rect::from_center_size(pos2(shc.x - 0.26, shc.y + 1.09), vec2(29.0, 29.0)), color, Icon::Share);
        if r.clicked() {
            if let Some(text) = self.lyrics_share_text() {
                self.actions.push(Action::CopyText(text, "Fragmento de la letra"));
            }
        }
    }

    /// Lo que copia «Compartir»: el renglón que suena (o el primero con texto), la canción y su
    /// enlace.
    fn lyrics_share_text(&self) -> Option<String> {
        let np = self.player.now.as_ref()?;
        let lyrics = self.lyrics.as_ref()?;
        let cur = lyrics.current_line(self.player.position()).filter(|&i| !lyrics.lines[i].words.trim().is_empty());
        let line = cur.map(|i| lyrics.lines[i].words.trim()).or_else(|| lyrics.lines.iter().map(|l| l.words.trim()).find(|w| !w.is_empty()))?;
        Some(format!("«{line}»\n— {} · {}\n{}", np.name, np.artists_str(), uri_to_link(&np.uri)))
    }

    fn lyrics_list(&mut self, ui: &mut egui::Ui, list: Rect, ink: &Ink) {
        let painter = ui.painter_at(list);
        let note = |painter: &egui::Painter, text: &str| {
            let g = galley_truncated(painter, text, theme::regular(15.0), ink.note, list.width() - 2.0 * LINE_X);
            let lead = ink_left(&g);
            text_on_baseline(painter, pos2(list.min.x + LINE_X - lead, list.min.y + FIRST_BASE), g, ink.note);
        };
        if !self.signed_in() {
            note(&painter, "Inicia sesión para ver la letra.");
            return;
        }
        self.ensure_lyrics();
        let Some(np) = self.player.now.clone() else {
            note(&painter, "Nada en reproducción.");
            return;
        };
        if self.lyrics_loading {
            note(&painter, "Buscando la letra…");
            return;
        }
        let Some(lyrics) = self.lyrics.clone() else {
            note(&painter, "Esta canción no tiene letra disponible.");
            return;
        };
        let synced = lyrics.synced();
        let pos = self.player.position();
        let current = lyrics.current_line(pos);
        let playing = self.player.state == PlayState::Playing;

        // Maqueta: línea base de la primera fila de cada renglón (desde arriba de la lista).
        let wrap = list.width() - 2.0 * LINE_X;
        let mut rows: Vec<(f32, Option<std::sync::Arc<egui::Galley>>)> = Vec::with_capacity(lyrics.lines.len());
        let mut y = FIRST_BASE;
        for line in &lyrics.lines {
            let words = line.words.trim();
            if words.is_empty() {
                rows.push((y, None));
                y += BLANK;
                continue;
            }
            let g = painter.layout(words.to_string(), theme::semibold(LINE_FONT), ink.ahead, wrap);
            let extra = match (g.rows.first(), g.rows.last()) {
                (Some(a), Some(b)) => b.pos.y - a.pos.y,
                _ => 0.0,
            };
            rows.push((y, Some(g)));
            y += LINE_PITCH + extra;
        }
        let last_base = rows.last().map(|r| r.0).unwrap_or(0.0);
        let max_off = (y - list.height() + 12.0).max(last_base - FOLLOW_AT);

        // Desplazamiento: siguiendo la canción (suave) o con la rueda.
        if self.lyrics_follow_track != lyrics.track_id {
            self.lyrics_follow_track = lyrics.track_id.clone();
            self.lyrics_offset = 0.0;
        }
        // Sin sincronizar (apagado, o letra sin tiempos): toda en blanco y quieta.
        let follow = self.settings.lyrics_sync && synced;
        if follow {
            let target = current.map(|i| follow_offset(rows[i].0, max_off)).unwrap_or(0.0);
            let dt = ui.input(|i| i.stable_dt).min(0.1);
            let k = 1.0 - (-dt * 9.0).exp();
            self.lyrics_offset += (target - self.lyrics_offset) * k;
            if (target - self.lyrics_offset).abs() > 0.5 {
                ui.ctx().request_repaint();
            } else {
                self.lyrics_offset = target;
            }
        } else if ui.rect_contains_pointer(list) {
            let wheel = ui.input(|i| i.smooth_scroll_delta.y);
            self.lyrics_offset = (self.lyrics_offset - wheel).clamp(0.0, max_off.max(0.0));
        }
        let off = self.lyrics_offset.round();

        let mut seek_to = None;
        for (i, (base, g)) in rows.iter().enumerate() {
            let Some(g) = g else { continue };
            let base = list.min.y + base - off;
            let lead = ink_left(g);
            let min = pos2((list.min.x + LINE_X - lead).round(), (base - first_base(g)).round());
            let area = Rect::from_min_size(min, g.size());
            if area.max.y < list.min.y || area.min.y > list.max.y {
                continue;
            }
            // Siguiendo la canción: lo ya cantado en blanco y lo que falta en gris; el renglón que
            // suena, con tiempos por sílaba, se llena según se canta, y si no, entero en blanco.
            // Sin seguirla, toda en blanco.
            let line_end = |i: usize| {
                let l = &lyrics.lines[i];
                l.end_ms.or_else(|| lyrics.lines.get(i + 1).map(|n| n.start_ms)).unwrap_or(np.duration_ms)
            };
            let sung = (follow && current == Some(i)).then(|| sung_chars(&lyrics.lines[i], pos, line_end(i)));
            let past = !follow || current.is_some_and(|c| i < c) || matches!(sung, Some(None));
            let hit = synced.then(|| ui.interact(area.expand2(vec2(4.0, 6.0)), egui::Id::new(("lyrics_line", i)), Sense::click()));
            let hovered = hit.as_ref().is_some_and(|r| r.hovered());
            if hovered {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if past {
                painter.galley_with_override_text_color(min, g.clone(), ink.sung);
            } else {
                let ahead = if hovered { ink.ahead.gamma_multiply(1.8) } else { ink.ahead };
                painter.galley_with_override_text_color(min, g.clone(), ahead);
                if let Some(Some(chars)) = sung {
                    for (k, x) in sung_rows(g, chars) {
                        let rr = g.rows[k].rect().translate(min.to_vec2());
                        let clip = Rect::from_min_max(pos2(rr.min.x - 3.0, rr.min.y - 2.0), pos2(min.x + x, rr.max.y + 6.0)).intersect(list);
                        painter.with_clip_rect(clip).galley_with_override_text_color(min, g.clone(), ink.sung);
                    }
                }
            }
            if hit.is_some_and(|r| r.clicked()) {
                seek_to = Some(lyrics.lines[i].start_ms);
            }
        }
        if let Some(ms) = seek_to {
            self.seek(ms);
        }
        // Mientras suena y se sigue: con tiempos por sílaba, un fotograma cada 50 ms (se llena);
        // si no, uno justo cuando empieza el renglón siguiente.
        if follow && playing {
            let by_syllable = current.is_some_and(|c| !lyrics.lines[c].syllables.is_empty());
            let next = lyrics.lines.iter().map(|l| l.start_ms).find(|&t| t > pos);
            if by_syllable {
                ui.ctx().request_repaint_after(Duration::from_millis(50));
            } else if let Some(t) = next {
                ui.ctx().request_repaint_after(Duration::from_millis(((t - pos) as u64).max(30)));
            }
        }
    }
}

/// Sincronizar: un reloj abierto arriba a la izquierda (de −100° a 147°) con sus dos agujas, y
/// dos rayas cortas a su izquierda. `c` es el centro del reloj.
fn paint_sync(painter: &egui::Painter, c: Pos2, color: Color32) {
    let s = Stroke::new(1.9, color);
    let arc: Vec<Pos2> = (0..=48)
        .map(|k| {
            let a = (-100.0 + 247.0 * k as f32 / 48.0).to_radians();
            pos2(c.x + SYNC_R * a.cos(), c.y + SYNC_R * a.sin())
        })
        .collect();
    painter.add(egui::Shape::line(arc, s));
    painter.add(egui::Shape::line(vec![pos2(c.x + 0.4, c.y - 6.5), pos2(c.x + 0.4, c.y), pos2(c.x + 5.0, c.y + 4.0)], s));
    for dy in [-7.0, -1.0] {
        painter.rect_filled(Rect::from_min_max(pos2(c.x - 10.9, c.y + dy - 1.0), pos2(c.x - 4.3, c.y + dy + 1.0)), CornerRadius::same(1), color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::model::Syllable;

    fn line(words: &str, syl: &[(u32, usize)]) -> LyricLine {
        LyricLine {
            start_ms: syl.first().map(|s| s.0).unwrap_or(0),
            words: words.into(),
            syllables: syl.iter().map(|&(start_ms, chars)| Syllable { start_ms, chars }).collect(),
            end_ms: None,
        }
    }

    #[test]
    fn relleno_por_silabas() {
        // Cada palabra (con su espacio) cuando se canta, no a ritmo fijo.
        let l = line("Hemos llegado tan lejos", &[(1000, 6), (1200, 8), (3000, 4), (3100, 5)]);
        assert_eq!(sung_chars(&l, 900, 4000), Some(0.0));
        assert_eq!(sung_chars(&l, 1000, 4000), Some(0.0));
        assert_eq!(sung_chars(&l, 1100, 4000), Some(3.0));
        // «llegado » se alarga hasta los 3 s: a mitad, 6 + 4.
        assert_eq!(sung_chars(&l, 2100, 4000), Some(10.0));
        assert_eq!(sung_chars(&l, 3100, 4000), Some(18.0));
        assert_eq!(sung_chars(&l, 5000, 4000), Some(23.0));
        // Solo con el comienzo del renglón: sin relleno (se ilumina entero).
        assert_eq!(sung_chars(&line("Sin tiempos", &[]), 5000, 9000), None);
        // Espacios delante: no cuentan.
        assert_eq!(sung_chars(&line("  ab", &[(0, 4)]), 9000, 9000), Some(2.0));
    }

    #[test]
    fn seguir_la_cancion() {
        // Arriba del todo, sin desplazar; luego, el renglón a FOLLOW_AT; al final, el tope.
        assert_eq!(follow_offset(42.0, 1000.0), 0.0);
        assert_eq!(follow_offset(FOLLOW_AT + 100.0, 1000.0), 100.0);
        assert_eq!(follow_offset(5000.0, 800.0), 800.0);
        assert_eq!(follow_offset(5000.0, -3.0), 0.0);
    }
}
