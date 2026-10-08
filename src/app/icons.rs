//! Iconos de línea propios, al estilo de Material Symbols Rounded: rejilla de 24×24, trazo de 2
//! con extremos y uniones redondeados. Cada icono se rasteriza una sola vez por tamaño en
//! píxeles (cobertura por distancia al trazo, con el borde suavizado) y se guarda como textura
//! blanca con transparencia: el color sale del tinte y se pinta a escala 1:1, nítido.

use std::cell::RefCell;
use std::collections::HashMap;
use std::f32::consts::PI;

use egui::{pos2, vec2, Color32, ColorImage, Pos2, Rect, Sense, TextureHandle, TextureOptions, Vec2};

use super::icon_masks::{self, Mask};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    Home,
    Search,
    Heart,
    HeartFilled,
    Pin,
    PinFilled,
    Playlist,
    /// Una playlist de la lista lateral (sin la tapa del icono de la sección).
    PlaylistItem,
    Album,
    Artist,
    History,
    Library,
    Play,
    #[allow(dead_code)]
    Pause,
    Prev,
    Next,
    Shuffle,
    Repeat,
    RepeatOne,
    Volume,
    Mute,
    /// Abrir la cola (píldora y dos líneas).
    Queue,
    /// Añadir a la cola (dos notas con un «+»).
    AddToQueue,
    /// Añadir una lista entera a la cola (líneas con un «+»), en la fila de botones de una
    /// playlist o un álbum.
    QueueList,
    /// Letra: líneas de texto y una nota.
    Lyrics,
    #[allow(dead_code)]
    Mic,
    Podcast,
    Devices,
    Headphones,
    #[allow(dead_code)]
    ThumbsUp,
    Camera,
    Refresh,
    /// Retroceder 15 s (episodios).
    Replay15,
    /// Avanzar 15 s (episodios).
    Forward15,
    CheckCircle,
    Person,
    Plus,
    PlusSquare,
    More,
    Settings,
    Grid,
    List,
    #[allow(dead_code)]
    Back,
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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
    /// «›» y «⌄» de las secciones plegables de la barra lateral.
    ChevronRight,
    ChevronDown,
    /// Inicio elegido: la casa rellena con la puerta recortada.
    HomeFilled,
    /// Personalizar el inicio: dos interruptores (barra y aro).
    Customize,
    /// Flechas con asta de las estanterías del inicio.
    ArrowLeft,
    ArrowRight,
}

/// Grosor del trazo en unidades de la rejilla de 24.
const SW: f32 = 1.9;
/// Rejilla de los iconos del reproductor: a 28 px cada unidad es un píxel, así que sus
/// coordenadas son las de la referencia de diseño tal cual.
const BAR_GRID: f32 = 28.0;

/// Una pieza del dibujo, en unidades de la rejilla.
enum Prim {
    /// Línea abierta (o cerrada, si repite el primer punto) de medio grosor `half`.
    Path(Vec<Pos2>, f32),
    /// Polígono relleno (regla del número de vueltas distinto de cero).
    Fill(Vec<Pos2>),
    /// Circunferencia exacta de medio grosor `half`.
    Ring(Pos2, f32, f32),
    /// Círculo relleno.
    Dot(Pos2, f32),
}

struct Op {
    prim: Prim,
    /// Borra lo dibujado antes (para que un trazo no cruce otro: el móvil sobre el monitor).
    erase: bool,
    alpha: f32,
}

struct Draw {
    ops: Vec<Op>,
    /// Lado de la rejilla en unidades (24 por defecto; 28 los del reproductor).
    grid: f32,
    /// Grosor del trazo en unidades de la rejilla.
    sw: f32,
}

impl Default for Draw {
    fn default() -> Self {
        Draw { ops: Vec::new(), grid: 24.0, sw: SW }
    }
}

fn v((x, y): (f32, f32)) -> Pos2 {
    pos2(x, y)
}

impl Draw {
    fn push(&mut self, prim: Prim) {
        self.ops.push(Op { prim, erase: false, alpha: 1.0 });
    }
    fn erase(&mut self, prim: Prim) {
        self.ops.push(Op { prim, erase: true, alpha: 1.0 });
    }
    fn stroke(&mut self, pts: Vec<Pos2>) {
        self.push(Prim::Path(pts, self.sw / 2.0));
    }
    /// Rejilla del reproductor (28) con su trazo de 1,9.
    fn bar(&mut self) {
        self.grid = BAR_GRID;
        self.sw = 1.9;
    }
    fn line(&mut self, a: (f32, f32), b: (f32, f32)) {
        self.stroke(vec![v(a), v(b)]);
    }
    fn poly(&mut self, pts: &[(f32, f32)]) {
        self.stroke(pts.iter().copied().map(v).collect());
    }
    /// Línea por `pts` con las esquinas redondeadas (radio `r`).
    fn rpoly(&mut self, pts: &[(f32, f32)], r: f32, closed: bool) {
        self.stroke(rounded(pts, r, closed));
    }
    /// Punta de flecha: dos trazos de `len` que llegan a `tip`, abiertos `spread` grados a cada
    /// lado de la dirección contraria a `dir` (grados, 0 a la derecha, crece hacia abajo).
    fn arrowhead(&mut self, tip: (f32, f32), dir: f32, len: f32, spread: f32) {
        let wing = |a: f32| {
            let t = (dir + 180.0 + a).to_radians();
            (tip.0 + len * t.cos(), tip.1 + len * t.sin())
        };
        self.poly(&[wing(-spread), tip, wing(spread)]);
    }
    fn rrect(&mut self, a: (f32, f32), b: (f32, f32), r: f32) {
        self.rpoly(&[a, (b.0, a.1), b, (a.0, b.1)], r, true);
    }
    fn ring(&mut self, c: (f32, f32), r: f32) {
        self.push(Prim::Ring(v(c), r, self.sw / 2.0));
    }
    /// Arco de circunferencia, en grados (0 a la derecha, crece en el sentido de las agujas).
    fn arc(&mut self, c: (f32, f32), r: f32, a0: f32, a1: f32) {
        self.stroke(arc_pts(v(c), r, a0, a1));
    }
    fn cubic(&mut self, a: (f32, f32), c1: (f32, f32), c2: (f32, f32), b: (f32, f32)) {
        self.stroke(cubic_pts(v(a), v(c1), v(c2), v(b)));
    }
    fn quad(&mut self, a: (f32, f32), c: (f32, f32), b: (f32, f32)) {
        self.stroke(quad_pts(v(a), v(c), v(b)));
    }
    fn fill(&mut self, pts: &[(f32, f32)], r: f32) {
        self.push(Prim::Fill(rounded(pts, r, true)));
    }
    fn fill_rrect(&mut self, a: (f32, f32), b: (f32, f32), r: f32) {
        self.fill(&[a, (b.0, a.1), b, (a.0, b.1)], r);
    }
    fn dot(&mut self, c: (f32, f32), r: f32) {
        self.push(Prim::Dot(v(c), r));
    }
    /// Nota musical (corchea): plica en `x` de `top` a `bottom`, cabeza rellena y bandera.
    fn note(&mut self, x: f32, top: f32, bottom: f32) {
        self.line((x, top), (x, bottom));
        self.dot((x - 1.55, bottom + 0.1), 1.85);
        self.line((x, top), (x + 2.3, top + 1.1));
    }
}

/// Puntos de una línea por `pts` con las esquinas cambiadas por curvas de radio `r` (como mucho
/// la mitad de cada lado). Cerrada, repite el primer punto al final.
fn rounded(pts: &[(f32, f32)], r: f32, closed: bool) -> Vec<Pos2> {
    rounded_each(pts, &vec![r; pts.len()], closed)
}

/// Como `rounded`, con un radio por vértice.
fn rounded_each(pts: &[(f32, f32)], radii: &[f32], closed: bool) -> Vec<Pos2> {
    let p: Vec<Pos2> = pts.iter().copied().map(v).collect();
    let n = p.len();
    if n < 3 || radii.iter().all(|&r| r <= 0.0) {
        let mut out = p;
        if closed && !out.is_empty() {
            out.push(out[0]);
        }
        return out;
    }
    let mut out = Vec::new();
    let corners: Vec<usize> = if closed { (0..n).collect() } else { (1..n - 1).collect() };
    if !closed {
        out.push(p[0]);
    }
    for &i in &corners {
        let prev = p[(i + n - 1) % n];
        let cur = p[i];
        let next = p[(i + 1) % n];
        let din = cur - prev;
        let dout = next - cur;
        let r = radii.get(i).copied().unwrap_or(0.0);
        if r <= 0.0 {
            out.push(cur);
            continue;
        }
        let cut = r.min(din.length() / 2.0).min(dout.length() / 2.0);
        let a = cur - din.normalized() * cut;
        let b = cur + dout.normalized() * cut;
        out.push(a);
        out.extend(quad_pts(a, cur, b).into_iter().skip(1));
    }
    if closed {
        out.push(out[0]);
    } else {
        out.push(p[n - 1]);
    }
    out
}

fn quad_pts(a: Pos2, c: Pos2, b: Pos2) -> Vec<Pos2> {
    let n = 10;
    (0..=n)
        .map(|i| {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            pos2(u * u * a.x + 2.0 * u * t * c.x + t * t * b.x, u * u * a.y + 2.0 * u * t * c.y + t * t * b.y)
        })
        .collect()
}

fn cubic_pts(a: Pos2, c1: Pos2, c2: Pos2, b: Pos2) -> Vec<Pos2> {
    let n = 20;
    (0..=n)
        .map(|i| {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            let (k0, k1, k2, k3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            pos2(
                k0 * a.x + k1 * c1.x + k2 * c2.x + k3 * b.x,
                k0 * a.y + k1 * c1.y + k2 * c2.y + k3 * b.y,
            )
        })
        .collect()
}

fn arc_pts(c: Pos2, r: f32, a0: f32, a1: f32) -> Vec<Pos2> {
    let n = ((a1 - a0).abs() / 4.0).ceil().max(2.0) as usize;
    (0..=n)
        .map(|i| {
            let t = (a0 + (a1 - a0) * i as f32 / n as f32).to_radians();
            pos2(c.x + r * t.cos(), c.y + r * t.sin())
        })
        .collect()
}

/// Corazón de punta en V (rejilla de 28): dos lóbulos de radio 5,3 que bajan por sus tangentes
/// hasta la punta y se cortan arriba en el centro.
fn heart_path() -> Vec<Pos2> {
    let (left, right, r) = (pos2(9.0, 9.7), pos2(18.7, 9.7), 5.3);
    let tip = pos2(13.85, 22.3);
    let mut pts = vec![tip];
    pts.extend(arc_pts(left, r, 135.8, 336.2));
    pts.extend(arc_pts(right, r, 203.8, 404.2));
    pts.push(tip);
    pts
}

/// Gira los puntos `deg` grados alrededor del centro de la rejilla.
fn rotated(pts: &[(f32, f32)], deg: f32) -> Vec<(f32, f32)> {
    let (s, c) = deg.to_radians().sin_cos();
    pts.iter().map(|&(x, y)| (12.0 + (x - 12.0) * c - (y - 12.0) * s, 12.0 + (x - 12.0) * s + (y - 12.0) * c)).collect()
}

fn build(icon: Icon, d: &mut Draw) {
    match icon {
        Icon::Home => {
            // Tejado a dos aguas y la puerta como un hueco en la base, copiada de la referencia
            // (rejilla de 28: a 28 px, 1 unidad = 1 px).
            d.grid = BAR_GRID;
            d.sw = 2.2;
            d.stroke(rounded_each(
                &[(9.1, 22.4), (4.0, 22.4), (4.0, 11.8), (13.7, 4.6), (23.3, 11.8), (23.3, 22.4), (19.1, 22.4)],
                &[0.0, 1.6, 1.2, 0.9, 1.2, 1.6, 0.0],
                false,
            ));
        }
        Icon::Search => {
            d.ring((10.6, 10.6), 6.4);
            d.line((15.4, 15.4), (20.4, 20.4));
        }
        Icon::Heart => {
            d.bar();
            d.stroke(heart_path());
        }
        Icon::HeartFilled => {
            d.bar();
            d.push(Prim::Fill(heart_path()));
        }
        Icon::Pin | Icon::PinFilled => {
            // Chincheta de pie (tapa, cuerpo que se abre y base) girada 45°: la aguja abajo a la izquierda.
            let head = rotated(&[(7.2, 3.6), (16.8, 3.6), (15.4, 5.0), (15.4, 9.4), (18.4, 13.4), (5.6, 13.4), (8.6, 9.4), (8.6, 5.0)], 45.0);
            if icon == Icon::PinFilled {
                d.fill(&head, 1.0);
            }
            d.rpoly(&head, 1.0, true);
            let needle = rotated(&[(12.0, 13.4), (12.0, 20.4)], 45.0);
            d.line(needle[0], needle[1]);
        }
        Icon::Playlist => {
            // Copiado de la referencia (rejilla de 28): la tarjeta de atrás asoma por arriba y la de
            // delante lleva una nota de trazo fino.
            d.grid = BAR_GRID;
            d.sw = 2.0;
            d.stroke(rounded_each(&[(7.0, 5.2), (7.0, 2.6), (21.4, 2.6), (21.4, 5.2)], &[0.0, 1.5, 1.5, 0.0], false));
            d.rrect((4.0, 6.0), (24.0, 25.6), 3.0);
            d.sw = 1.4;
            d.line((14.5, 10.6), (14.5, 18.9));
            d.line((14.5, 10.6), (16.8, 10.9));
            d.ring((12.9, 18.9), 1.6);
        }
        Icon::PlaylistItem => {
            d.rrect((4.0, 4.0), (20.0, 20.0), 4.5);
            d.note(13.0, 8.4, 14.6);
        }
        Icon::Album => {
            // Disco abierto abajo a la derecha, donde va la nota.
            d.arc((11.3, 11.3), 8.0, 78.0, 372.0);
            d.ring((11.3, 11.3), 2.4);
            d.note(19.4, 13.4, 19.0);
        }
        Icon::Artist => {
            // Busto (cabeza y hombro) y una nota de cabeza hueca, de la referencia (rejilla de 28).
            d.grid = BAR_GRID;
            d.sw = 2.0;
            d.ring((14.0, 7.4), 3.2);
            let mut body = cubic_pts(pos2(11.0, 14.3), pos2(8.2, 14.8), pos2(6.0, 16.6), pos2(6.0, 19.2));
            body.extend(rounded(&[(6.0, 19.2), (6.0, 22.6), (13.6, 22.6)], 1.2, false).into_iter().skip(1));
            d.stroke(body);
            d.line((21.9, 7.3), (21.9, 17.6));
            d.ring((19.7, 17.6), 2.2);
        }
        Icon::History => {
            d.arc((12.8, 12.0), 7.8, 196.0, 506.0);
            d.poly(&[(2.9, 8.4), (5.3, 10.6), (7.8, 8.4)]);
            d.poly(&[(12.8, 7.8), (12.8, 12.3), (15.7, 14.1)]);
        }
        Icon::Library => {
            // Dos lomos y un libro con la esquina de arriba cortada, en línea.
            d.line((4.0, 3.8), (4.0, 19.2));
            d.line((9.3, 3.8), (9.3, 19.2));
            d.rpoly(&[(13.8, 3.8), (19.4, 7.4), (19.4, 19.2), (13.8, 19.2)], 0.6, true);
        }
        Icon::Play => d.fill(&[(8.0, 4.8), (19.6, 12.0), (8.0, 19.2)], 1.8),
        Icon::Pause => {
            d.fill_rrect((6.0, 5.0), (10.0, 19.0), 1.4);
            d.fill_rrect((14.0, 5.0), (18.0, 19.0), 1.4);
        }
        Icon::Prev => {
            d.bar();
            d.fill_rrect((8.0, 8.55), (10.4, 19.75), 0.7);
            d.fill(&[(20.1, 8.0), (20.1, 20.2), (9.9, 14.1)], 1.3);
        }
        Icon::Next => {
            d.bar();
            d.fill(&[(8.4, 8.0), (8.4, 20.2), (18.6, 14.1)], 1.3);
            d.fill_rrect((18.2, 8.55), (20.55, 19.75), 0.7);
        }
        Icon::Shuffle => {
            d.bar();
            // La que baja se corta donde la cruza la que sube.
            let mut down = cubic_pts(pos2(4.6, 7.2), pos2(10.6, 7.2), pos2(12.4, 19.3), pos2(16.6, 19.3));
            down.push(pos2(22.6, 19.3));
            let mut up = cubic_pts(pos2(4.6, 21.1), pos2(10.0, 21.1), pos2(11.4, 9.4), pos2(16.4, 9.4));
            up.push(pos2(22.4, 9.4));
            d.stroke(down);
            let half = d.sw / 2.0 + 1.2;
            d.erase(Prim::Path(up.clone(), half));
            d.stroke(up);
            d.poly(&[(19.0, 5.7), (22.8, 9.4), (19.0, 13.1)]);
            d.poly(&[(19.1, 15.6), (22.9, 19.3), (19.1, 23.0)]);
        }
        Icon::Repeat | Icon::RepeatOne => {
            d.bar();
            // Rectángulo abierto abajo, con la flecha hacia la izquierda en el lado de abajo.
            let pts = [(11.0, 18.4), (4.7, 18.4), (4.7, 6.0), (23.6, 6.0), (23.6, 18.4), (13.9, 18.4)];
            d.stroke(rounded_each(&pts, &[0.0, 3.0, 3.0, 3.0, 3.0, 0.0], false));
            d.poly(&[(16.3, 14.8), (13.9, 18.4), (16.9, 22.4)]);
            if icon == Icon::RepeatOne {
                d.poly(&[(12.9, 10.9), (14.5, 9.6), (14.5, 15.0)]);
            }
        }
        Icon::Volume | Icon::Mute => {
            d.bar();
            // Cono sin caja: la punta de la izquierda muy redondeada.
            d.stroke(rounded_each(&[(16.5, 5.4), (16.5, 22.6), (3.4, 13.95)], &[1.4, 1.4, 4.4], true));
            if icon == Icon::Volume {
                d.arc((16.5, 14.0), 3.6, -24.0, 24.0);
                d.arc((17.15, 14.0), 6.55, -51.0, 51.0);
            } else {
                d.line((19.8, 11.2), (24.2, 16.8));
                d.line((24.2, 11.2), (19.8, 16.8));
            }
        }
        Icon::Queue => {
            d.bar();
            d.sw = 2.2;
            d.rrect((6.3, 5.3), (21.8, 10.6), 2.65);
            d.line((6.2, 16.5), (22.4, 16.5));
            d.line((6.2, 22.0), (22.4, 22.0));
        }
        Icon::AddToQueue => {
            // Dos notas unidas por la barra y un «+».
            d.line((5.0, 3.4), (5.0, 8.8));
            d.line((2.3, 6.1), (7.7, 6.1));
            d.rpoly(&[(10.4, 17.6), (10.4, 5.4), (19.2, 5.4), (19.2, 17.6)], 0.5, false);
            d.ring((8.45, 18.2), 1.95);
            d.ring((17.25, 18.2), 1.95);
        }
        Icon::QueueList => {
            // Copiado de la referencia (rejilla de 28): una «C» con un «+» a su derecha y dos
            // líneas largas debajo.
            d.grid = BAR_GRID;
            d.sw = 2.2;
            d.stroke(rounded_each(&[(13.0, 6.0), (5.0, 6.0), (5.0, 11.0), (13.0, 11.0)], &[0.0, 2.5, 2.5, 0.0], false));
            d.line((19.0, 4.9), (19.0, 11.1));
            d.line((15.8, 8.0), (22.2, 8.0));
            d.line((6.0, 17.0), (21.2, 17.0));
            d.line((6.0, 22.4), (21.2, 22.4));
        }
        Icon::Lyrics => {
            d.bar();
            d.line((4.4, 7.6), (15.8, 7.6));
            d.line((4.4, 14.5), (12.6, 14.5));
            d.line((4.4, 21.5), (12.8, 21.5));
            d.line((22.3, 6.0), (22.3, 18.6));
            d.ring((19.65, 18.6), 2.65);
        }
        Icon::Mic => {
            d.rrect((9.0, 3.0), (15.0, 13.6), 3.0);
            d.arc((12.0, 10.6), 6.2, 0.0, 180.0);
            d.line((12.0, 16.8), (12.0, 21.0));
        }
        Icon::Podcast => {
            // Líneas de texto y un micrófono, como «transcribir».
            d.line((3.0, 8.0), (7.6, 8.0));
            d.line((3.0, 12.0), (8.6, 12.0));
            d.line((3.0, 16.0), (7.6, 16.0));
            d.rrect((12.6, 3.0), (18.4, 13.2), 2.9);
            d.arc((15.5, 10.4), 5.0, 0.0, 180.0);
            d.line((15.5, 15.4), (15.5, 20.6));
        }
        Icon::Devices => {
            d.bar();
            // El móvil asoma de canto a la izquierda del altavoz.
            d.rpoly(&[(7.6, 7.0), (4.3, 7.0), (4.3, 15.4), (7.6, 15.4)], 2.2, false);
            d.dot((5.9, 20.6), 1.45);
            d.rrect((10.5, 4.9), (22.4, 22.2), 3.0);
            d.dot((16.2, 9.0), 1.4);
            d.dot((16.4, 16.5), 2.9);
        }
        Icon::Headphones => {
            d.arc((12.0, 13.0), 8.2, 160.0, 380.0);
            d.rpoly(&[(4.5, 14.3), (7.3, 13.8), (8.5, 19.6), (5.7, 20.1)], 1.3, true);
            d.rpoly(&[(19.5, 14.3), (16.7, 13.8), (15.5, 19.6), (18.3, 20.1)], 1.3, true);
        }
        Icon::ThumbsUp => {
            d.rpoly(&[(3.6, 10.6), (7.4, 10.6), (7.4, 20.4), (3.6, 20.4)], 0.6, true);
            d.stroke(rounded_each(
                &[(7.4, 20.4), (7.4, 10.6), (10.9, 4.0), (12.6, 4.5), (13.2, 6.3), (12.5, 9.6), (18.8, 9.6), (20.4, 11.4), (18.6, 19.4), (17.2, 20.4)],
                &[0.6, 0.6, 1.0, 1.0, 0.8, 0.6, 1.0, 1.0, 1.0, 0.8],
                true,
            ));
        }
        Icon::Camera => {
            d.rrect((3.0, 7.2), (21.0, 19.6), 1.2);
            d.rpoly(&[(8.0, 7.2), (9.4, 4.6), (14.6, 4.6), (16.0, 7.2)], 0.5, false);
            d.ring((12.0, 13.3), 3.3);
        }
        Icon::Refresh => {
            d.arc((12.0, 12.0), 7.6, 30.0, 345.0);
            let a = 345f32.to_radians();
            d.arrowhead((12.0 + 7.6 * a.cos(), 12.0 + 7.6 * a.sin()), 75.0, 3.4, 40.0);
        }
        Icon::Replay15 | Icon::Forward15 => {
            // Flecha circular con «15» dentro, abajo; la de avanzar es la simétrica.
            let back = icon == Icon::Replay15;
            let (cx, a0, a1, at, dir) = if back { (13.2, 255.0, 430.0, 255.0, 165.0) } else { (10.8, 110.0, 285.0, 285.0, 15.0) };
            d.arc((cx, 12.6), 7.4, a0, a1);
            let a = f32::to_radians(at);
            d.arrowhead((cx + 7.4 * a.cos(), 12.6 + 7.4 * a.sin()), dir, 3.2, 42.0);
            let dx = if back { 0.0 } else { 7.6 };
            d.poly(&[(4.3 + dx, 15.9), (5.6 + dx, 14.8), (5.6 + dx, 21.2)]);
            let mut five = vec![pos2(11.4 + dx, 14.8), pos2(8.4 + dx, 14.8), pos2(8.1 + dx, 17.6)];
            five.extend(cubic_pts(pos2(8.1 + dx, 17.6), pos2(9.2 + dx, 17.0), pos2(11.6 + dx, 17.2), pos2(11.6 + dx, 19.2)).into_iter().skip(1));
            five.extend(cubic_pts(pos2(11.6 + dx, 19.2), pos2(11.6 + dx, 21.3), pos2(8.8 + dx, 21.6), pos2(7.9 + dx, 20.5)).into_iter().skip(1));
            d.stroke(five);
        }
        Icon::CheckCircle => {
            d.ring((12.0, 12.0), 8.6);
            d.poly(&[(8.0, 12.4), (10.8, 15.1), (16.2, 9.4)]);
        }
        Icon::Person => {
            d.ring((12.0, 7.9), 3.8);
            d.rpoly(&[(10.0, 11.6), (9.8, 14.2), (4.9, 17.4), (4.6, 20.3), (19.4, 20.3), (19.1, 17.4), (14.2, 14.2), (14.0, 11.6)], 0.9, false);
        }
        Icon::Plus => {
            d.line((12.0, 4.8), (12.0, 19.2));
            d.line((4.8, 12.0), (19.2, 12.0));
        }
        Icon::PlusSquare => {
            d.bar();
            // Una tarjeta detrás y la de delante con el «+» (añadir a playlist).
            d.ops.push(Op {
                prim: Prim::Path(rounded(&[(6.5, 4.6), (6.5, 2.0), (21.6, 2.0), (21.6, 4.6)], 2.0, false), d.sw / 2.0),
                erase: false,
                alpha: 0.8,
            });
            d.rrect((4.4, 5.0), (24.0, 24.4), 3.4);
            d.line((14.3, 10.3), (14.3, 19.1));
            d.line((9.9, 14.6), (18.9, 14.6));
        }
        Icon::More => {
            d.bar();
            for x in [6.0, 14.0, 22.0] {
                d.dot((x, 14.0), 1.75);
            }
        }
        Icon::Settings => {
            // Engranaje de 8 dientes redondeados.
            let n = 288;
            let mut pts: Vec<Pos2> = (0..n)
                .map(|i| {
                    let a = PI * 2.0 * i as f32 / n as f32 + PI / 8.0;
                    let t = (((8.0 * a).cos() + 1.0) / 2.0 - 0.32) / 0.36;
                    let t = t.clamp(0.0, 1.0);
                    let f = t * t * (3.0 - 2.0 * t);
                    let r = 7.0 + 2.4 * f;
                    pos2(12.0 + r * a.cos(), 12.0 + r * a.sin())
                })
                .collect();
            pts.push(pts[0]);
            d.stroke(pts);
            d.ring((12.0, 12.0), 3.0);
        }
        Icon::Grid => {
            for (x, y) in [(3.5, 3.5), (13.5, 3.5), (3.5, 13.5), (13.5, 13.5)] {
                d.rrect((x, y), (x + 7.0, y + 7.0), 2.0);
            }
        }
        Icon::List => {
            for y in [6.5, 12.0, 17.5] {
                d.dot((4.6, y), 1.3);
                d.line((8.8, y), (20.0, y));
            }
        }
        Icon::Back => d.poly(&[(15.0, 5.0), (8.0, 12.0), (15.0, 19.0)]),
        Icon::Forward => d.poly(&[(9.0, 5.0), (16.0, 12.0), (9.0, 19.0)]),
        Icon::Close => {
            d.line((6.0, 6.0), (18.0, 18.0));
            d.line((18.0, 6.0), (6.0, 18.0));
        }
        Icon::Check => d.poly(&[(5.0, 12.6), (9.6, 17.2), (19.0, 7.6)]),
        Icon::People => {
            d.ring((9.0, 8.0), 3.3);
            let mut body = vec![pos2(3.0, 19.5), pos2(3.0, 17.5)];
            body.extend(cubic_pts(pos2(3.0, 17.5), pos2(3.0, 15.2), pos2(5.8, 13.8), pos2(9.0, 13.8)).into_iter().skip(1));
            body.extend(cubic_pts(pos2(9.0, 13.8), pos2(12.2, 13.8), pos2(15.0, 15.2), pos2(15.0, 17.5)).into_iter().skip(1));
            body.extend([pos2(15.0, 19.5), pos2(3.0, 19.5)]);
            d.stroke(body);
            d.arc((15.6, 8.0), 3.0, -80.0, 80.0);
            d.cubic((17.2, 13.9), (19.4, 14.3), (21.0, 15.7), (21.0, 17.6));
            d.line((21.0, 17.6), (21.0, 19.5));
        }
        Icon::Share => {
            // Bandeja abierta y la flecha que sale hacia arriba (referencia, rejilla de 28).
            d.grid = BAR_GRID;
            d.sw = 2.1;
            d.line((14.3, 4.8), (14.3, 16.2));
            d.poly(&[(9.8, 9.4), (14.3, 4.8), (18.8, 9.4)]);
            d.stroke(rounded_each(&[(6.5, 13.6), (6.5, 21.1), (22.0, 21.1), (22.0, 13.6)], &[0.0, 2.8, 2.8, 0.0], false));
        }
        Icon::Download => {
            d.ring((12.0, 12.0), 8.6);
            d.line((12.0, 6.6), (12.0, 17.2));
            d.poly(&[(7.9, 12.9), (12.0, 17.2), (16.1, 12.9)]);
        }
        Icon::Minus => d.line((5.0, 12.0), (19.0, 12.0)),
        Icon::PlusCircle => {
            d.ring((12.0, 12.0), 8.8);
            d.line((12.0, 7.9), (12.0, 16.1));
            d.line((7.9, 12.0), (16.1, 12.0));
        }
        Icon::Bookmark | Icon::BookmarkFilled => {
            let pts = [(5.3, 3.8), (19.5, 3.8), (19.5, 21.0), (12.4, 16.6), (5.3, 21.0)];
            if icon == Icon::BookmarkFilled {
                d.fill(&pts, 1.8);
            }
            d.rpoly(&pts, 1.8, true);
        }
        Icon::Sort => {
            d.line((8.0, 19.0), (8.0, 5.0));
            d.poly(&[(4.6, 8.4), (8.0, 5.0), (11.4, 8.4)]);
            d.line((16.0, 5.0), (16.0, 19.0));
            d.poly(&[(12.6, 15.6), (16.0, 19.0), (19.4, 15.6)]);
        }
        Icon::Filter => {
            d.line((4.0, 7.0), (20.0, 7.0));
            d.line((7.0, 12.0), (17.0, 12.0));
            d.line((10.0, 17.0), (14.0, 17.0));
        }
        Icon::Episode => {
            d.rrect((3.0, 5.0), (21.0, 19.0), 3.2);
            d.fill(&[(10.0, 8.8), (15.6, 12.0), (10.0, 15.2)], 1.0);
        }
        Icon::Sliders => {
            // Dos interruptores: «⊂ ○» arriba y «○ ⊃» abajo.
            let mut top = vec![pos2(13.4, 4.6)];
            top.extend(arc_pts(pos2(6.8, 7.0), 2.4, 270.0, 90.0));
            top.push(pos2(13.4, 9.4));
            d.stroke(top);
            d.ring((17.6, 7.0), 2.4);
            let mut bottom = vec![pos2(10.6, 14.6)];
            bottom.extend(arc_pts(pos2(17.2, 17.0), 2.4, -90.0, 90.0));
            bottom.push(pos2(10.6, 19.4));
            d.stroke(bottom);
            d.ring((6.4, 17.0), 2.4);
        }
        Icon::Eye | Icon::EyeOff => {
            d.quad((2.6, 12.0), (12.0, 3.6), (21.4, 12.0));
            d.quad((2.6, 12.0), (12.0, 20.4), (21.4, 12.0));
            d.ring((12.0, 12.0), 3.3);
            if icon == Icon::EyeOff {
                d.erase(Prim::Path(vec![pos2(4.0, 4.0), pos2(20.0, 20.0)], d.sw / 2.0 + 1.3));
                d.line((4.0, 4.0), (20.0, 20.0));
            }
        }
        Icon::DragHandle => {
            for (x, y) in [(9.0, 7.0), (15.0, 7.0), (9.0, 12.0), (15.0, 12.0), (9.0, 17.0), (15.0, 17.0)] {
                d.dot((x, y), 1.5);
            }
        }
        Icon::Folder => d.rpoly(&[(3.0, 3.2), (9.6, 3.2), (11.6, 5.3), (21.0, 5.3), (21.0, 18.5), (3.0, 18.5)], 2.0, true),
        Icon::Book => {
            // Libro abierto: dos páginas que se juntan en el lomo.
            d.rpoly(&[(12.0, 5.2), (8.0, 3.8), (3.8, 4.2), (3.8, 19.2), (8.0, 18.8), (12.0, 20.2)], 1.6, true);
            d.rpoly(&[(12.0, 5.2), (16.0, 3.8), (20.2, 4.2), (20.2, 19.2), (16.0, 18.8), (12.0, 20.2)], 1.6, true);
        }
        Icon::Radio => {
            d.dot((12.0, 12.0), 1.9);
            d.arc((12.0, 12.0), 5.0, 140.0, 220.0);
            d.arc((12.0, 12.0), 5.0, -40.0, 40.0);
            d.arc((12.0, 12.0), 8.8, 145.0, 215.0);
            d.arc((12.0, 12.0), 8.8, -35.0, 35.0);
        }
        Icon::Clock => {
            d.ring((12.0, 12.0), 8.6);
            d.poly(&[(12.0, 7.4), (12.0, 12.4), (15.6, 12.4)]);
        }
        Icon::Fullscreen => {
            d.rpoly(&[(4.0, 9.0), (4.0, 4.0), (9.0, 4.0)], 1.5, false);
            d.rpoly(&[(15.0, 4.0), (20.0, 4.0), (20.0, 9.0)], 1.5, false);
            d.rpoly(&[(20.0, 15.0), (20.0, 20.0), (15.0, 20.0)], 1.5, false);
            d.rpoly(&[(9.0, 20.0), (4.0, 20.0), (4.0, 15.0)], 1.5, false);
        }
        Icon::Miniplayer => {
            d.rrect((2.5, 4.5), (21.5, 19.5), 2.5);
            d.fill_rrect((12.3, 11.8), (18.8, 16.8), 1.0);
        }
        Icon::NewTab => {
            d.rpoly(&[(10.5, 4.5), (4.5, 4.5), (4.5, 19.5), (19.5, 19.5), (19.5, 13.5)], 2.2, false);
            d.line((11.6, 12.4), (19.4, 4.6));
            d.poly(&[(13.6, 4.5), (19.5, 4.5), (19.5, 10.4)]);
        }
        Icon::Trash => {
            d.line((3.8, 6.5), (20.2, 6.5));
            d.rpoly(&[(9.3, 6.5), (9.3, 3.8), (14.7, 3.8), (14.7, 6.5)], 1.0, false);
            d.rpoly(&[(5.8, 6.5), (6.9, 20.0), (17.1, 20.0), (18.2, 6.5)], 2.0, false);
            d.line((10.0, 10.5), (10.0, 16.2));
            d.line((14.0, 10.5), (14.0, 16.2));
        }
        Icon::Edit => {
            // Lápiz inclinado, con la punta abajo a la izquierda.
            d.stroke(rounded_each(&[(4.3, 19.7), (5.2, 16.0), (16.4, 4.8), (19.2, 7.6), (8.0, 18.8)], &[0.4, 0.6, 1.4, 1.4, 0.6], true));
        }
        Icon::Keyboard => {
            d.rrect((2.5, 6.0), (21.5, 18.0), 2.5);
            for x in [6.6, 10.2, 13.8, 17.4] {
                d.dot((x, 10.2), 1.05);
            }
            d.line((8.0, 14.4), (16.0, 14.4));
        }
        Icon::HomeFilled => {
            // Copiada de la referencia (rejilla de 28): el borde de fuera de la casa, relleno, con
            // la puerta como un hueco en la base.
            d.grid = BAR_GRID;
            d.fill(
                &[(14.5, 3.4), (25.9, 12.8), (25.9, 24.4), (16.4, 24.4), (16.4, 16.3), (12.6, 16.3), (12.6, 24.4), (3.1, 24.4), (3.1, 12.8)],
                1.4,
            );
        }
        Icon::Customize => {
            // Referencia (rejilla de 28): arriba una barra y un aro; abajo, al revés.
            d.grid = BAR_GRID;
            d.sw = 1.9;
            d.rrect((6.9, 6.9), (14.5, 11.6), 2.3);
            d.ring((21.6, 9.25), 3.0);
            d.ring((8.3, 18.75), 3.0);
            d.rrect((14.5, 17.3), (23.0, 21.1), 1.9);
        }
        Icon::ArrowLeft => {
            d.line((5.0, 12.0), (19.5, 12.0));
            d.poly(&[(11.2, 5.8), (5.0, 12.0), (11.2, 18.2)]);
        }
        Icon::ArrowRight => {
            d.line((4.5, 12.0), (19.0, 12.0));
            d.poly(&[(12.8, 5.8), (19.0, 12.0), (12.8, 18.2)]);
        }
        Icon::ChevronRight => d.poly(&[(9.5, 5.5), (15.0, 12.0), (9.5, 18.5)]),
        Icon::ChevronDown => d.poly(&[(5.5, 9.5), (12.0, 15.0), (18.5, 9.5)]),
        Icon::Hourglass => {
            d.line((6.0, 3.5), (18.0, 3.5));
            d.line((6.0, 20.5), (18.0, 20.5));
            d.rpoly(&[(7.6, 3.5), (7.6, 7.2), (12.0, 12.0), (7.6, 16.8), (7.6, 20.5)], 2.0, false);
            d.rpoly(&[(16.4, 3.5), (16.4, 7.2), (12.0, 12.0), (16.4, 16.8), (16.4, 20.5)], 2.0, false);
        }
    }
}

fn seg_dist(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let len2 = ab.length_sq();
    let t = if len2 > 0.0 { ((p - a).dot(ab) / len2).clamp(0.0, 1.0) } else { 0.0 };
    (p - (a + ab * t)).length()
}

fn path_dist(p: Pos2, pts: &[Pos2]) -> f32 {
    match pts.len() {
        0 => f32::MAX,
        1 => (p - pts[0]).length(),
        _ => pts.windows(2).map(|w| seg_dist(p, w[0], w[1])).fold(f32::MAX, f32::min),
    }
}

/// Distancia con signo al borde del polígono: positiva dentro (número de vueltas distinto de cero).
fn fill_sd(p: Pos2, pts: &[Pos2]) -> f32 {
    let n = pts.len();
    if n < 3 {
        return -f32::MAX;
    }
    let mut winding = 0i32;
    let mut d = f32::MAX;
    for i in 0..n {
        let a = pts[i];
        let b = pts[(i + 1) % n];
        d = d.min(seg_dist(p, a, b));
        if a.y <= p.y {
            if b.y > p.y && (b - a).x * (p.y - a.y) - (p.x - a.x) * (b - a).y > 0.0 {
                winding += 1;
            }
        } else if b.y <= p.y && (b - a).x * (p.y - a.y) - (p.x - a.x) * (b - a).y < 0.0 {
            winding -= 1;
        }
    }
    if winding != 0 {
        d
    } else {
        -d
    }
}

impl Prim {
    /// Cobertura (0..1) del píxel cuyo centro está en `p` (unidades de la rejilla); `scale` son
    /// píxeles por unidad. Borde suavizado de un píxel.
    fn coverage(&self, p: Pos2, scale: f32) -> f32 {
        let sd = match self {
            Prim::Path(pts, half) => half - path_dist(p, pts),
            Prim::Fill(pts) => fill_sd(p, pts),
            Prim::Ring(c, r, half) => half - ((p - *c).length() - r).abs(),
            Prim::Dot(c, r) => r - (p - *c).length(),
        };
        (sd * scale + 0.5).clamp(0.0, 1.0)
    }
}

/// Alfa (0..255) del icono rasterizado a `px`×`px` píxeles.
fn rasterize(icon: Icon, px: usize) -> Vec<u8> {
    let mut d = Draw::default();
    build(icon, &mut d);
    let scale = px as f32 / d.grid;
    let mut out = Vec::with_capacity(px * px);
    for y in 0..px {
        for x in 0..px {
            let p = pos2((x as f32 + 0.5) / scale, (y as f32 + 0.5) / scale);
            let mut a = 0.0f32;
            for op in &d.ops {
                let c = op.prim.coverage(p, scale);
                if op.erase {
                    a *= 1.0 - c;
                } else {
                    a = a.max(c * op.alpha);
                }
            }
            out.push((a * 255.0 + 0.5) as u8);
        }
    }
    out
}

thread_local! {
    /// Texturas ya rasterizadas, por icono y tamaño en píxeles. Solo las usa el hilo de la interfaz.
    static CACHE: RefCell<HashMap<(Icon, u16), TextureHandle>> = RefCell::new(HashMap::new());
}

/// Más de esto (al cambiar mucho de escala) y se empieza de cero: las de ahora vuelven a salir solas.
const CACHE_MAX: usize = 600;

fn texture(ctx: &egui::Context, icon: Icon, px: u16) -> egui::TextureId {
    if let Some(id) = CACHE.with(|c| c.borrow().get(&(icon, px)).map(TextureHandle::id)) {
        return id;
    }
    let alpha = rasterize(icon, px as usize);
    let pixels = alpha.into_iter().map(Color32::from_white_alpha).collect();
    let image = ColorImage::new([px as usize, px as usize], pixels);
    let tex = ctx.load_texture(format!("icono-{icon:?}-{px}"), image, TextureOptions::NEAREST);
    let id = tex.id();
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() >= CACHE_MAX {
            c.clear();
        }
        c.insert((icon, px), tex);
    });
    id
}

// ------------------------------------------------------------ copias exactas de la referencia

/// Máscara copiada de la referencia de diseño para un icono de la barra lateral, si la hay.
fn side_mask(icon: Icon) -> Option<&'static Mask> {
    Some(match icon {
        Icon::Pin => &icon_masks::PIN,
        Icon::Playlist => &icon_masks::PLAYLIST,
        Icon::PlaylistItem => &icon_masks::PLAYLIST_ITEM,
        Icon::Heart => &icon_masks::HEART,
        Icon::Bookmark => &icon_masks::BOOKMARK,
        Icon::Album => &icon_masks::ALBUM,
        Icon::Folder => &icon_masks::FOLDER,
        Icon::Podcast => &icon_masks::PODCAST,
        Icon::Book => &icon_masks::BOOK,
        Icon::Artist => &icon_masks::ARTIST,
        Icon::Library => &icon_masks::LIBRARY,
        Icon::ChevronRight => &icon_masks::CHEVRON_RIGHT,
        Icon::ChevronDown => &icon_masks::CHEVRON_DOWN,
        _ => return None,
    })
}

thread_local! {
    /// Máscaras ya pasadas a textura, por icono y ancho en píxeles.
    static MASKS: RefCell<HashMap<(Icon, u16), TextureHandle>> = RefCell::new(HashMap::new());
}

/// Textura de la máscara de `icon` a `w`×`h` píxeles: tal cual a su tamaño; reescalada (bilineal)
/// si la interfaz está ampliada o reducida.
fn mask_texture(ctx: &egui::Context, icon: Icon, m: &Mask, w: usize, h: usize) -> egui::TextureId {
    if let Some(id) = MASKS.with(|c| c.borrow().get(&(icon, w as u16)).map(TextureHandle::id)) {
        return id;
    }
    let at = |x: isize, y: isize| -> f32 {
        if x < 0 || y < 0 || x >= m.w as isize || y >= m.h as isize {
            0.0
        } else {
            m.alpha[y as usize * m.w + x as usize] as f32
        }
    };
    let (kx, ky) = (w as f32 / m.w as f32, h as f32 / m.h as f32);
    let mut pixels = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let a = if w == m.w && h == m.h {
                m.alpha[y * m.w + x] as f32
            } else {
                let sx = (x as f32 + 0.5) / kx - 0.5;
                let sy = (y as f32 + 0.5) / ky - 0.5;
                let (x0, y0) = (sx.floor(), sy.floor());
                let (fx, fy) = (sx - x0, sy - y0);
                let (x0, y0) = (x0 as isize, y0 as isize);
                let top = at(x0, y0) * (1.0 - fx) + at(x0 + 1, y0) * fx;
                let bottom = at(x0, y0 + 1) * (1.0 - fx) + at(x0 + 1, y0 + 1) * fx;
                top * (1.0 - fy) + bottom * fy
            };
            pixels.push(Color32::from_white_alpha(a.round().clamp(0.0, 255.0) as u8));
        }
    }
    let tex = ctx.load_texture(format!("icono-ref-{icon:?}-{w}"), ColorImage::new([w, h], pixels), TextureOptions::NEAREST);
    let id = tex.id();
    MASKS.with(|c| {
        c.borrow_mut().insert((icon, w as u16), tex);
    });
    id
}

/// Icono de la barra lateral en su cuadro de `side` puntos centrado en `c`: la copia exacta de la
/// referencia de diseño (píxel a píxel a escala 1) si la hay; si no, o con la interfaz muy
/// ampliada o reducida, el dibujo vectorial.
pub fn paint_side(painter: &egui::Painter, c: Pos2, side: f32, color: Color32, icon: Icon) {
    let ppp = painter.pixels_per_point();
    let px = (side * ppp).round();
    let Some(m) = side_mask(icon).filter(|m| (0.74..=2.6).contains(&(px / m.native))) else {
        return paint(painter, Rect::from_center_size(c, Vec2::splat(side)), color, icon);
    };
    if color.a() == 0 {
        return;
    }
    let k = px / m.native;
    // La esquina del cuadro, como en `paint`: así la máscara cae donde se recortó.
    let bx = (c.x * ppp - px / 2.0).round();
    let by = (c.y * ppp - px / 2.0).round();
    let pad = (m.pad * k).round();
    let (w, h) = ((m.w as f32 * k).round() as usize, (m.h as f32 * k).round() as usize);
    let tex = mask_texture(painter.ctx(), icon, m, w, h);
    let r = Rect::from_min_size(pos2((bx - pad) / ppp, (by - pad) / ppp), vec2(w as f32 / ppp, h as f32 / ppp));
    painter.image(tex, r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), color);
}

/// Dibuja el icono dentro de `rect` (se usa el cuadrado inscrito), del color `color`.
pub fn paint(painter: &egui::Painter, rect: Rect, color: Color32, icon: Icon) {
    let side = rect.width().min(rect.height());
    if side.is_nan() || side <= 0.0 || color.a() == 0 {
        return;
    }
    let ppp = painter.pixels_per_point();
    let px = (side * ppp).round().clamp(4.0, 256.0);
    // Esquina en un píxel entero: cada texel cae en un píxel de la pantalla (sin desenfoque).
    let c = rect.center();
    let min = pos2((c.x * ppp - px / 2.0).round() / ppp, (c.y * ppp - px / 2.0).round() / ppp);
    let r = Rect::from_min_size(min, Vec2::splat(px / ppp));
    let id = texture(painter.ctx(), icon, px as u16);
    painter.image(id, r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), color);
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
    paint(ui.painter(), rect.shrink(size * 0.19), c, icon);
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
    let inner = rect.shrink(size * 0.25);
    // El triángulo de play se ve centrado un poco a la derecha.
    let inner = if icon == Icon::Play { inner.translate(vec2(size * 0.025, 0.0)) } else { inner };
    paint(ui.painter(), inner, fg, icon);
    resp
}

#[allow(dead_code)]
/// Icono estático (sin interacción) del tamaño dado.
pub fn show(ui: &mut egui::Ui, icon: Icon, size: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    paint(ui.painter(), rect.shrink(size * 0.12), color, icon);
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &[Icon] = &[
        Icon::Home, Icon::Search, Icon::Heart, Icon::HeartFilled, Icon::Pin, Icon::PinFilled, Icon::Playlist,
        Icon::PlaylistItem, Icon::Album, Icon::Artist, Icon::History, Icon::Library, Icon::Play, Icon::Pause,
        Icon::Prev, Icon::Next, Icon::Shuffle, Icon::Repeat, Icon::RepeatOne, Icon::Volume, Icon::Mute,
        Icon::Queue, Icon::AddToQueue, Icon::QueueList, Icon::Lyrics, Icon::Mic, Icon::Podcast, Icon::Devices, Icon::Headphones,
        Icon::ThumbsUp, Icon::Camera, Icon::Refresh, Icon::Replay15, Icon::Forward15, Icon::CheckCircle, Icon::Person,
        Icon::Plus, Icon::PlusSquare, Icon::More,
        Icon::Settings, Icon::Grid, Icon::List, Icon::Back, Icon::Forward, Icon::Close, Icon::Check,
        Icon::People, Icon::Share, Icon::Hourglass, Icon::Download, Icon::Minus, Icon::PlusCircle,
        Icon::Bookmark, Icon::BookmarkFilled, Icon::Sort, Icon::Filter, Icon::Episode, Icon::Sliders, Icon::Eye,
        Icon::EyeOff, Icon::DragHandle, Icon::Folder, Icon::Book, Icon::Radio, Icon::Clock, Icon::Fullscreen,
        Icon::Miniplayer, Icon::NewTab, Icon::Trash, Icon::Edit, Icon::Keyboard, Icon::ChevronRight,
        Icon::ChevronDown, Icon::HomeFilled, Icon::Customize, Icon::ArrowLeft, Icon::ArrowRight,
    ];

    /// Todos se ven (tienen tinta) y caben en su cuadro: el marco exterior queda casi vacío, así
    /// que ningún trazo sale cortado.
    #[test]
    fn todos_los_iconos_tienen_tinta_y_caben() {
        for &icon in ALL {
            for px in [14usize, 18, 24, 36] {
                let a = rasterize(icon, px);
                let ink: u32 = a.iter().map(|&x| x as u32).sum();
                assert!(ink > 255 * 6, "{icon:?} a {px} px casi vacío");
                let edge = (0..px).flat_map(|i| [a[i], a[(px - 1) * px + i], a[i * px], a[i * px + px - 1]]).max().unwrap();
                assert!(edge < 200, "{icon:?} a {px} px se sale del cuadro (borde {edge})");
            }
        }
    }

    /// El trazo es redondo en los extremos: más allá del final de una línea, a medio grosor, aún
    /// hay tinta (con remate cuadrado la habría también, pero no en la diagonal de la esquina).
    #[test]
    fn extremos_redondeados() {
        let mut d = Draw::default();
        d.line((6.0, 12.0), (18.0, 12.0));
        let at = |x: f32, y: f32| d.ops.iter().map(|o| o.prim.coverage(pos2(x, y), 4.0)).fold(0.0f32, f32::max);
        assert!(at(18.6, 12.0) > 0.9, "el extremo no sigue redondo");
        assert!(at(18.85, 12.85) < 0.5, "la esquina del remate debería estar vacía (remate redondo)");
    }

    /// Los iconos del reproductor a 28 px (alfa en gris) en %TEMP%\nanofy_barra\, para
    /// compararlos con la referencia de diseño píxel a píxel.
    #[test]
    #[ignore]
    fn volcar_barra() {
        let dir = std::env::temp_dir().join("nanofy_barra");
        std::fs::create_dir_all(&dir).unwrap();
        for icon in [Icon::Prev, Icon::Next, Icon::Shuffle, Icon::Repeat, Icon::Volume, Icon::Heart, Icon::PlusSquare, Icon::Lyrics, Icon::Devices, Icon::Queue, Icon::More] {
            let a = rasterize(icon, 28);
            let img = image::GrayImage::from_raw(28, 28, a).unwrap();
            img.save(dir.join(format!("{icon:?}.png"))).unwrap();
        }
    }

    /// Hoja con todos los iconos a 18, 24 y 36 px en %TEMP%\nanofy_iconos.png, para mirarlos.
    #[test]
    #[ignore]
    fn vista_previa() {
        let sizes = [18usize, 24, 36];
        let cell = 44usize;
        let cols = 12usize;
        let rows = ALL.len().div_ceil(cols);
        let (w, h) = (cols * cell * sizes.len(), rows * cell);
        let mut img = image::RgbaImage::from_pixel(w as u32, h as u32, image::Rgba([18, 18, 18, 255]));
        for (k, &icon) in ALL.iter().enumerate() {
            for (si, &px) in sizes.iter().enumerate() {
                let a = rasterize(icon, px);
                let ox = (si * cols + k % cols) * cell + (cell - px) / 2;
                let oy = (k / cols) * cell + (cell - px) / 2;
                for y in 0..px {
                    for x in 0..px {
                        let t = a[y * px + x] as f32 / 255.0;
                        let c = (18.0 + (235.0 - 18.0) * t) as u8;
                        img.put_pixel((ox + x) as u32, (oy + y) as u32, image::Rgba([c, c, c, 255]));
                    }
                }
            }
        }
        let out = std::env::temp_dir().join("nanofy_iconos.png");
        img.save(&out).unwrap();
        println!("{}", out.display());
    }
}
