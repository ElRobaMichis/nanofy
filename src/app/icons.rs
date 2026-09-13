//! Iconos vectoriales de línea, dibujados con el painter de egui. Todos comparten grosor,
//! caja y estilo, así que la interfaz es consistente (los emoji del sistema no lo son).

use egui::{pos2, vec2, Color32, Pos2, Rect, Sense, Shape, Stroke, Vec2};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Home,
    Search,
    Heart,
    HeartFilled,
    Pin,
    PinFilled,
    Playlist,
    Album,
    Artist,
    History,
    Library,
    Play,
    Pause,
    Prev,
    Next,
    Shuffle,
    Repeat,
    RepeatOne,
    Volume,
    Mute,
    Queue,
    Lyrics,
    Devices,
    Plus,
    PlusSquare,
    More,
    Settings,
    Grid,
    List,
    Back,
    Forward,
    Close,
    Check,
    #[allow(dead_code)]
    People,
    Share,
    Hourglass,
    Download,
    Minus,
    PlusCircle,
    Bookmark,
    BookmarkFilled,
    Sort,
    Filter,
    Episode,
    Sliders,
    Eye,
    EyeOff,
    DragHandle,
    Folder,
    Book,
    Radio,
    Clock,
    Fullscreen,
    Miniplayer,
    NewTab,
    Trash,
    Edit,
    Keyboard,
    Image,
}

/// Dibuja el icono dentro de `rect` (se usa el cuadrado inscrito).
pub fn paint(painter: &egui::Painter, rect: Rect, color: Color32, icon: Icon) {
    let side = rect.width().min(rect.height());
    let r = Rect::from_center_size(rect.center(), Vec2::splat(side));
    let s = side;
    let p = |x: f32, y: f32| pos2(r.min.x + x * s, r.min.y + y * s);
    let w = (s * 0.085).clamp(1.2, 3.0);
    let stroke = Stroke::new(w, color);
    let line = |a: Pos2, b: Pos2| {
        painter.line_segment([a, b], stroke);
    };
    let poly = |pts: &[Pos2]| {
        painter.add(Shape::line(pts.to_vec(), stroke));
    };
    let closed = |pts: &[Pos2]| {
        painter.add(Shape::closed_line(pts.to_vec(), stroke));
    };
    let circle = |c: Pos2, rad: f32| {
        painter.circle_stroke(c, rad, stroke);
    };
    let arc = |c: Pos2, rad: f32, a0: f32, a1: f32| {
        let n = 24;
        let pts: Vec<Pos2> = (0..=n)
            .map(|i| {
                let t = a0 + (a1 - a0) * i as f32 / n as f32;
                pos2(c.x + rad * t.cos(), c.y + rad * t.sin())
            })
            .collect();
        painter.add(Shape::line(pts, stroke));
    };
    let fill_tri = |a: Pos2, b: Pos2, c: Pos2| {
        painter.add(Shape::convex_polygon(vec![a, b, c], color, Stroke::NONE));
    };
    let rrect = |min: Pos2, max: Pos2, rad: f32| {
        painter.rect_stroke(
            Rect::from_min_max(min, max),
            egui::CornerRadius::same(rad as u8),
            stroke,
            egui::StrokeKind::Middle,
        );
    };
    use std::f32::consts::PI;

    match icon {
        Icon::Home => {
            poly(&[p(0.12, 0.5), p(0.5, 0.15), p(0.88, 0.5)]);
            poly(&[p(0.22, 0.42), p(0.22, 0.85), p(0.42, 0.85), p(0.42, 0.62), p(0.58, 0.62), p(0.58, 0.85), p(0.78, 0.85), p(0.78, 0.42)]);
        }
        Icon::Search => {
            circle(p(0.44, 0.44), 0.27 * s);
            line(p(0.64, 0.64), p(0.86, 0.86));
        }
        Icon::Heart | Icon::HeartFilled => {
            let pts: Vec<Pos2> = (0..=40)
                .map(|i| {
                    let t = PI * 2.0 * i as f32 / 40.0;
                    let x = 16.0 * t.sin().powi(3);
                    let y = 13.0 * t.cos() - 5.0 * (2.0 * t).cos() - 2.0 * (3.0 * t).cos() - (4.0 * t).cos();
                    p(0.5 + x / 40.0, 0.5 - (y + 2.55) / 40.0)
                })
                .collect();
            if icon == Icon::HeartFilled {
                painter.add(Shape::convex_polygon(pts, color, Stroke::NONE));
            } else {
                // Dos trazos abiertos: el remate en inglete de un ángulo tan agudo como la punta
                // dibujaría un pico por debajo del corazón.
                let (a, b) = pts.split_at(21);
                let mut right = b.to_vec();
                right.push(pts[0]);
                painter.add(Shape::line(a.to_vec(), stroke));
                painter.add(Shape::line(right, stroke));
            }
        }
        Icon::Pin | Icon::PinFilled => {
            // Chincheta clásica (cabeza, cuerpo, reborde y aguja) girada 45°, punta abajo a la izquierda.
            let rot = |x: f32, y: f32| {
                let (dx, dy) = (x - 0.5, y - 0.5);
                let (c, sn) = (std::f32::consts::FRAC_1_SQRT_2, std::f32::consts::FRAC_1_SQRT_2);
                p(0.5 + dx * c - dy * sn, 0.5 + dx * sn + dy * c)
            };
            let body = [rot(0.40, 0.06), rot(0.60, 0.06), rot(0.60, 0.50), rot(0.40, 0.50)];
            let flange = [rot(0.40, 0.50), rot(0.60, 0.50), rot(0.74, 0.58), rot(0.74, 0.64), rot(0.26, 0.64), rot(0.26, 0.58)];
            if icon == Icon::PinFilled {
                painter.add(Shape::convex_polygon(body.to_vec(), color, Stroke::NONE));
                painter.add(Shape::convex_polygon(flange.to_vec(), color, Stroke::NONE));
            } else {
                closed(&[rot(0.40, 0.06), rot(0.60, 0.06), rot(0.60, 0.50), rot(0.74, 0.58), rot(0.74, 0.64), rot(0.26, 0.64), rot(0.26, 0.58), rot(0.40, 0.50)]);
            }
            line(rot(0.5, 0.64), rot(0.5, 0.96));
        }
        Icon::Playlist => {
            rrect(p(0.18, 0.14), p(0.82, 0.86), 0.12 * s);
            circle(p(0.44, 0.62), 0.09 * s);
            line(p(0.53, 0.62), p(0.53, 0.32));
            line(p(0.53, 0.32), p(0.68, 0.36));
        }
        Icon::Album => {
            circle(p(0.5, 0.5), 0.36 * s);
            circle(p(0.5, 0.5), 0.1 * s);
        }
        Icon::Artist => {
            circle(p(0.5, 0.36), 0.18 * s);
            arc(p(0.5, 0.98), 0.38 * s, PI * 1.12, PI * 1.88);
        }
        Icon::History => {
            circle(p(0.5, 0.5), 0.36 * s);
            line(p(0.5, 0.28), p(0.5, 0.52));
            line(p(0.5, 0.52), p(0.66, 0.62));
        }
        Icon::Library => {
            line(p(0.2, 0.18), p(0.2, 0.82));
            line(p(0.38, 0.18), p(0.38, 0.82));
            line(p(0.55, 0.22), p(0.8, 0.8));
        }
        Icon::Play => fill_tri(p(0.32, 0.18), p(0.32, 0.82), p(0.84, 0.5)),
        Icon::Pause => {
            painter.rect_filled(Rect::from_min_max(p(0.26, 0.2), p(0.42, 0.8)), 1.0, color);
            painter.rect_filled(Rect::from_min_max(p(0.58, 0.2), p(0.74, 0.8)), 1.0, color);
        }
        Icon::Prev => {
            painter.rect_filled(Rect::from_min_max(p(0.18, 0.22), p(0.28, 0.78)), 1.0, color);
            fill_tri(p(0.82, 0.22), p(0.82, 0.78), p(0.34, 0.5));
        }
        Icon::Next => {
            painter.rect_filled(Rect::from_min_max(p(0.72, 0.22), p(0.82, 0.78)), 1.0, color);
            fill_tri(p(0.18, 0.22), p(0.18, 0.78), p(0.66, 0.5));
        }
        Icon::Shuffle => {
            poly(&[p(0.12, 0.3), p(0.32, 0.3), p(0.68, 0.7), p(0.86, 0.7)]);
            poly(&[p(0.12, 0.7), p(0.32, 0.7), p(0.68, 0.3), p(0.86, 0.3)]);
            poly(&[p(0.76, 0.2), p(0.88, 0.3), p(0.76, 0.4)]);
            poly(&[p(0.76, 0.6), p(0.88, 0.7), p(0.76, 0.8)]);
        }
        Icon::Repeat | Icon::RepeatOne => {
            poly(&[p(0.24, 0.62), p(0.24, 0.4), p(0.32, 0.3), p(0.78, 0.3)]);
            poly(&[p(0.7, 0.2), p(0.82, 0.3), p(0.7, 0.4)]);
            poly(&[p(0.76, 0.38), p(0.76, 0.6), p(0.68, 0.7), p(0.22, 0.7)]);
            poly(&[p(0.3, 0.6), p(0.18, 0.7), p(0.3, 0.8)]);
            if icon == Icon::RepeatOne {
                line(p(0.5, 0.42), p(0.5, 0.6));
                line(p(0.44, 0.47), p(0.5, 0.42));
            }
        }
        Icon::Volume | Icon::Mute => {
            closed(&[p(0.16, 0.4), p(0.32, 0.4), p(0.5, 0.24), p(0.5, 0.76), p(0.32, 0.6), p(0.16, 0.6)]);
            if icon == Icon::Volume {
                arc(p(0.5, 0.5), 0.2 * s, -PI * 0.28, PI * 0.28);
                arc(p(0.5, 0.5), 0.32 * s, -PI * 0.28, PI * 0.28);
            } else {
                line(p(0.62, 0.4), p(0.82, 0.6));
                line(p(0.82, 0.4), p(0.62, 0.6));
            }
        }
        Icon::Queue => {
            line(p(0.16, 0.3), p(0.84, 0.3));
            line(p(0.16, 0.5), p(0.84, 0.5));
            line(p(0.16, 0.7), p(0.56, 0.7));
            fill_tri(p(0.68, 0.6), p(0.68, 0.8), p(0.86, 0.7));
        }
        Icon::Lyrics => {
            // micrófono
            rrect(p(0.4, 0.14), p(0.6, 0.56), 0.1 * s);
            arc(p(0.5, 0.5), 0.2 * s, 0.0, PI);
            line(p(0.5, 0.7), p(0.5, 0.84));
            line(p(0.38, 0.84), p(0.62, 0.84));
        }
        Icon::Devices => {
            rrect(p(0.14, 0.22), p(0.74, 0.64), 0.06 * s);
            line(p(0.36, 0.78), p(0.52, 0.78));
            line(p(0.44, 0.64), p(0.44, 0.78));
            rrect(p(0.64, 0.42), p(0.88, 0.8), 0.06 * s);
        }
        Icon::Plus => {
            line(p(0.5, 0.2), p(0.5, 0.8));
            line(p(0.2, 0.5), p(0.8, 0.5));
        }
        Icon::PlusSquare => {
            rrect(p(0.16, 0.16), p(0.84, 0.84), 0.12 * s);
            line(p(0.5, 0.34), p(0.5, 0.66));
            line(p(0.34, 0.5), p(0.66, 0.5));
        }
        Icon::More => {
            for x in [0.24, 0.5, 0.76] {
                painter.circle_filled(p(x, 0.5), w * 0.9, color);
            }
        }
        Icon::Settings => {
            circle(p(0.5, 0.5), 0.13 * s);
            for i in 0..8 {
                let a = PI * 2.0 * i as f32 / 8.0;
                let (c, sn) = (a.cos(), a.sin());
                line(
                    p(0.5 + 0.26 * c, 0.5 + 0.26 * sn),
                    p(0.5 + 0.36 * c, 0.5 + 0.36 * sn),
                );
            }
            circle(p(0.5, 0.5), 0.26 * s);
        }
        Icon::Grid => {
            for (x, y) in [(0.18, 0.18), (0.54, 0.18), (0.18, 0.54), (0.54, 0.54)] {
                rrect(p(x, y), p(x + 0.28, y + 0.28), 0.05 * s);
            }
        }
        Icon::List => {
            for y in [0.26, 0.5, 0.74] {
                painter.circle_filled(p(0.2, y), w * 0.8, color);
                line(p(0.34, y), p(0.84, y));
            }
        }
        Icon::Back => poly(&[p(0.62, 0.22), p(0.34, 0.5), p(0.62, 0.78)]),
        Icon::Forward => poly(&[p(0.38, 0.22), p(0.66, 0.5), p(0.38, 0.78)]),
        Icon::Close => {
            line(p(0.26, 0.26), p(0.74, 0.74));
            line(p(0.74, 0.26), p(0.26, 0.74));
        }
        Icon::Check => poly(&[p(0.2, 0.52), p(0.42, 0.74), p(0.8, 0.3)]),
        Icon::People => {
            circle(p(0.38, 0.36), 0.14 * s);
            arc(p(0.38, 0.9), 0.3 * s, PI * 1.12, PI * 1.88);
            arc(p(0.66, 0.36), 0.14 * s, -PI * 0.6, PI * 0.6);
            arc(p(0.68, 0.9), 0.3 * s, PI * 1.35, PI * 1.85);
        }
        Icon::Share => {
            line(p(0.5, 0.16), p(0.5, 0.6));
            poly(&[p(0.36, 0.3), p(0.5, 0.16), p(0.64, 0.3)]);
            poly(&[p(0.28, 0.46), p(0.2, 0.46), p(0.2, 0.84), p(0.8, 0.84), p(0.8, 0.46), p(0.72, 0.46)]);
        }
        Icon::Download => {
            line(p(0.5, 0.16), p(0.5, 0.6));
            poly(&[p(0.34, 0.46), p(0.5, 0.62), p(0.66, 0.46)]);
            poly(&[p(0.2, 0.62), p(0.2, 0.84), p(0.8, 0.84), p(0.8, 0.62)]);
        }
        Icon::Minus => line(p(0.24, 0.5), p(0.76, 0.5)),
        Icon::PlusCircle => {
            circle(p(0.5, 0.5), 0.36 * s);
            line(p(0.5, 0.32), p(0.5, 0.68));
            line(p(0.32, 0.5), p(0.68, 0.5));
        }
        Icon::Bookmark | Icon::BookmarkFilled => {
            let pts = [p(0.28, 0.16), p(0.72, 0.16), p(0.72, 0.84), p(0.5, 0.66), p(0.28, 0.84)];
            if icon == Icon::BookmarkFilled {
                painter.add(Shape::convex_polygon(pts.to_vec(), color, Stroke::NONE));
            } else {
                closed(&pts);
            }
        }
        Icon::Sort => {
            line(p(0.34, 0.2), p(0.34, 0.8));
            poly(&[p(0.2, 0.34), p(0.34, 0.2), p(0.48, 0.34)]);
            line(p(0.66, 0.2), p(0.66, 0.8));
            poly(&[p(0.52, 0.66), p(0.66, 0.8), p(0.8, 0.66)]);
        }
        Icon::Filter => {
            line(p(0.18, 0.3), p(0.82, 0.3));
            line(p(0.3, 0.5), p(0.7, 0.5));
            line(p(0.42, 0.7), p(0.58, 0.7));
        }
        Icon::Episode => {
            closed(&[p(0.16, 0.24), p(0.84, 0.24), p(0.84, 0.76), p(0.16, 0.76)]);
            closed(&[p(0.42, 0.38), p(0.64, 0.5), p(0.42, 0.62)]);
        }
        Icon::Sliders => {
            line(p(0.16, 0.34), p(0.84, 0.34));
            line(p(0.16, 0.66), p(0.84, 0.66));
            painter.circle_filled(p(0.62, 0.34), 0.09 * s, color);
            painter.circle_filled(p(0.38, 0.66), 0.09 * s, color);
        }
        Icon::Eye | Icon::EyeOff => {
            let pts: Vec<Pos2> = (0..=24)
                .map(|i| {
                    let t = i as f32 / 24.0;
                    let x = 0.14 + 0.72 * t;
                    let y = 0.5 - 0.26 * (t * PI).sin();
                    p(x, y)
                })
                .collect();
            let mut lower: Vec<Pos2> = pts.iter().map(|q| pos2(q.x, 2.0 * p(0.5, 0.5).y - q.y)).collect();
            lower.reverse();
            painter.add(Shape::line(pts, stroke));
            painter.add(Shape::line(lower, stroke));
            circle(p(0.5, 0.5), 0.11 * s);
            if icon == Icon::EyeOff {
                line(p(0.2, 0.8), p(0.8, 0.2));
            }
        }
        Icon::DragHandle => {
            for (x, y) in [(0.38, 0.28), (0.62, 0.28), (0.38, 0.5), (0.62, 0.5), (0.38, 0.72), (0.62, 0.72)] {
                painter.circle_filled(p(x, y), 0.06 * s, color);
            }
        }
        Icon::Folder => {
            closed(&[p(0.14, 0.28), p(0.4, 0.28), p(0.48, 0.38), p(0.86, 0.38), p(0.86, 0.78), p(0.14, 0.78)]);
        }
        Icon::Book => {
            closed(&[p(0.16, 0.2), p(0.5, 0.3), p(0.84, 0.2), p(0.84, 0.78), p(0.5, 0.88), p(0.16, 0.78)]);
            line(p(0.5, 0.3), p(0.5, 0.88));
        }
        Icon::Radio => {
            painter.circle_filled(p(0.5, 0.5), 0.07 * s, color);
            arc(p(0.5, 0.5), 0.2 * s, PI * 0.75, PI * 1.25);
            arc(p(0.5, 0.5), 0.2 * s, -PI * 0.25, PI * 0.25);
            arc(p(0.5, 0.5), 0.34 * s, PI * 0.75, PI * 1.25);
            arc(p(0.5, 0.5), 0.34 * s, -PI * 0.25, PI * 0.25);
        }
        Icon::Clock => {
            circle(p(0.5, 0.54), 0.32 * s);
            line(p(0.5, 0.54), p(0.5, 0.34));
            line(p(0.5, 0.54), p(0.64, 0.6));
            line(p(0.42, 0.14), p(0.58, 0.14));
        }
        Icon::Fullscreen => {
            poly(&[p(0.18, 0.38), p(0.18, 0.18), p(0.38, 0.18)]);
            poly(&[p(0.62, 0.18), p(0.82, 0.18), p(0.82, 0.38)]);
            poly(&[p(0.82, 0.62), p(0.82, 0.82), p(0.62, 0.82)]);
            poly(&[p(0.38, 0.82), p(0.18, 0.82), p(0.18, 0.62)]);
        }
        Icon::Miniplayer => {
            rrect(p(0.14, 0.2), p(0.86, 0.8), 0.06 * s);
            painter.rect_filled(Rect::from_min_max(p(0.5, 0.5), p(0.78, 0.72)), 1.0, color);
        }
        Icon::NewTab => {
            poly(&[p(0.44, 0.18), p(0.18, 0.18), p(0.18, 0.82), p(0.82, 0.82), p(0.82, 0.56)]);
            line(p(0.5, 0.5), p(0.82, 0.18));
            poly(&[p(0.6, 0.18), p(0.82, 0.18), p(0.82, 0.4)]);
        }
        Icon::Trash => {
            line(p(0.2, 0.28), p(0.8, 0.28));
            line(p(0.4, 0.28), p(0.4, 0.18));
            line(p(0.4, 0.18), p(0.6, 0.18));
            line(p(0.6, 0.18), p(0.6, 0.28));
            poly(&[p(0.26, 0.28), p(0.3, 0.84), p(0.7, 0.84), p(0.74, 0.28)]);
        }
        Icon::Edit => {
            line(p(0.2, 0.8), p(0.68, 0.32));
            line(p(0.68, 0.32), p(0.8, 0.44));
            line(p(0.8, 0.44), p(0.32, 0.92 - 0.1));
            line(p(0.2, 0.8), p(0.18, 0.94 - 0.1));
            line(p(0.18, 0.84), p(0.32, 0.82));
        }
        Icon::Keyboard => {
            rrect(p(0.12, 0.28), p(0.88, 0.72), 0.06 * s);
            for (x, y) in [(0.26, 0.42), (0.42, 0.42), (0.58, 0.42), (0.74, 0.42)] {
                painter.circle_filled(p(x, y), 0.03 * s, color);
            }
            line(p(0.3, 0.6), p(0.7, 0.6));
        }
        Icon::Image => {
            rrect(p(0.16, 0.2), p(0.84, 0.8), 0.06 * s);
            circle(p(0.36, 0.4), 0.07 * s);
            poly(&[p(0.2, 0.76), p(0.42, 0.54), p(0.56, 0.66), p(0.66, 0.58), p(0.82, 0.76)]);
        }
        Icon::Hourglass => {
            closed(&[p(0.28, 0.18), p(0.72, 0.18), p(0.5, 0.5), p(0.72, 0.82), p(0.28, 0.82), p(0.5, 0.5)]);
        }
    }
}

/// Botón de icono: cuadrado de `size` con el icono dentro y un círculo sutil al pasar el ratón.
pub fn button(ui: &mut egui::Ui, icon: Icon, size: f32, color: Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    if resp.hovered() {
        ui.painter().circle_filled(
            rect.center(),
            size / 2.0,
            ui.visuals().widgets.hovered.weak_bg_fill,
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let c = if resp.hovered() {
        ui.visuals().strong_text_color().lerp_to_gamma(color, 0.4)
    } else {
        color
    };
    paint(ui.painter(), rect.shrink(size * 0.22), c, icon);
    resp
}

/// Botón redondo relleno (p. ej. el de reproducir).
pub fn round_button(ui: &mut egui::Ui, icon: Icon, size: f32, fill: Color32, fg: Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let fill = if resp.hovered() { fill.lerp_to_gamma(Color32::WHITE, 0.12) } else { fill };
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    ui.painter().circle_filled(rect.center(), size / 2.0, fill);
    let inner = rect.shrink(size * 0.28);
    // El triángulo de play se ve centrado un poco a la derecha.
    let inner = if icon == Icon::Play { inner.translate(vec2(size * 0.03, 0.0)) } else { inner };
    paint(ui.painter(), inner, fg, icon);
    resp
}

#[allow(dead_code)]
/// Icono estático (sin interacción) del tamaño dado.
pub fn show(ui: &mut egui::Ui, icon: Icon, size: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    paint(ui.painter(), rect.shrink(size * 0.12), color, icon);
}
