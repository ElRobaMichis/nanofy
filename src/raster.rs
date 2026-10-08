//! Rasterizador por software para las mallas de egui.
//!
//! egui produce triángulos con color por vértice (alfa premultiplicada) y coordenadas de
//! textura. Aquí se dibujan directamente sobre un búfer `0RGB` de 32 bits, sin GPU ni
//! driver gráfico. Las esquinas suavizadas ya vienen "emplumadas" del teselador de egui,
//! así que no hace falta antialiasing adicional.

use std::collections::HashMap;

use egui::epaint::textures::TexturesDelta;
use egui::epaint::{ClippedPrimitive, ImageData, Primitive, TextureId, Vertex};
use egui::Color32;

enum Pixels {
    /// RGBA premultiplicada, fila a fila.
    Rgba(Vec<[u8; 4]>),
    /// Solo cobertura (atlas de fuentes: blanco premultiplicado, así que r=g=b=a).
    Alpha(Vec<u8>),
}

struct Texture {
    w: usize,
    h: usize,
    px: Pixels,
    /// Todos los texels con alfa 255 (portadas): se copian sin mezclar.
    opaque: bool,
}

impl Texture {
    #[inline]
    fn texel(&self, x: usize, y: usize) -> [u8; 4] {
        match &self.px {
            Pixels::Rgba(p) => p[y * self.w + x],
            Pixels::Alpha(p) => {
                let a = p[y * self.w + x];
                [a, a, a, a]
            }
        }
    }

    #[inline]
    fn set(&mut self, x: usize, y: usize, c: [u8; 4]) {
        match &mut self.px {
            Pixels::Rgba(p) => p[y * self.w + x] = c,
            Pixels::Alpha(p) => p[y * self.w + x] = c[3],
        }
    }

    /// Muestreo con el vecino más cercano (para el atlas de fuentes, alineado a píxel).
    #[inline]
    fn nearest(&self, u: f32, v: f32) -> [u8; 4] {
        let x = ((u * self.w as f32) as isize).clamp(0, self.w as isize - 1) as usize;
        let y = ((v * self.h as f32) as isize).clamp(0, self.h as isize - 1) as usize;
        self.texel(x, y)
    }

    /// Muestreo bilineal (para portadas escaladas).
    #[inline]
    fn bilinear(&self, u: f32, v: f32) -> [u8; 4] {
        let fx = (u * self.w as f32 - 0.5).clamp(0.0, (self.w - 1) as f32);
        let fy = (v * self.h as f32 - 0.5).clamp(0.0, (self.h - 1) as f32);
        let x0 = fx as usize;
        let y0 = fy as usize;
        let x1 = (x0 + 1).min(self.w - 1);
        let y1 = (y0 + 1).min(self.h - 1);
        // Pesos en punto fijo 8 bits: sin flotantes por canal.
        let tx = ((fx - x0 as f32) * 256.0) as u32;
        let ty = ((fy - y0 as f32) * 256.0) as u32;
        let (itx, ity) = (256 - tx, 256 - ty);
        let p00 = self.texel(x0, y0);
        let p10 = self.texel(x1, y0);
        let p01 = self.texel(x0, y1);
        let p11 = self.texel(x1, y1);
        let mut out = [0u8; 4];
        for c in 0..4 {
            let top = p00[c] as u32 * itx + p10[c] as u32 * tx;
            let bottom = p01[c] as u32 * itx + p11[c] as u32 * tx;
            out[c] = ((top * ity + bottom * ty + 32768) >> 16) as u8;
        }
        out
    }
}

#[derive(Default)]
pub struct Raster {
    textures: HashMap<TextureId, Texture>,
}

/// Efecto «cristal» de un panel: en un `egui::epaint::PaintCallback` con esto dentro,
/// `Raster::paint` desenfoca lo ya pintado bajo el rectángulo del callback (en puntos), con las
/// esquinas redondeadas de `corner` puntos, antes de seguir con lo que va encima. El desenfoque
/// es casi gaussiano, de desviación `sigma` puntos.
pub struct BackdropBlur {
    pub sigma: f32,
    pub corner: f32,
}

impl Raster {
    /// Aplica las texturas nuevas o modificadas. Llamar antes de pintar.
    pub fn update_textures(&mut self, delta: &TexturesDelta) {
        for (id, deltas) in &delta.set {
            for d in deltas.iter() {
                let ImageData::Color(img) = &d.image;
                let [iw, ih] = img.size;
                match d.pos {
                    None => {
                        let px = if *id == TextureId::Managed(0) {
                            Pixels::Alpha(img.pixels.iter().map(|c| c.a()).collect())
                        } else {
                            Pixels::Rgba(img.pixels.iter().map(|c| c.to_array()).collect())
                        };
                        let opaque = matches!(&px, Pixels::Rgba(p) if p.iter().all(|c| c[3] == 255));
                        self.textures.insert(
                            *id,
                            Texture {
                                w: iw,
                                h: ih,
                                px,
                                opaque,
                            },
                        );
                    }
                    Some([x, y]) => {
                        if let Some(t) = self.textures.get_mut(id) {
                            t.opaque = false;
                            for row in 0..ih {
                                let ty = y + row;
                                if ty >= t.h {
                                    break;
                                }
                                for col in 0..iw {
                                    let tx = x + col;
                                    if tx >= t.w {
                                        break;
                                    }
                                    t.set(tx, ty, img.pixels[row * iw + col].to_array());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Libera texturas. Llamar después de pintar.
    pub fn free_textures(&mut self, delta: &TexturesDelta) {
        for id in &delta.free {
            self.textures.remove(id);
        }
    }

    /// Pinta todas las primitivas en `buf` (ancho `w`, alto `h`, formato 0RGB).
    ///
    /// Con ventanas grandes el trabajo se reparte en bandas horizontales entre varios hilos:
    /// cada banda recibe todas las primitivas recortadas a sus filas, así ningún hilo escribe
    /// fuera de su trozo del búfer y no hace falta sincronización.
    pub fn paint(
        &self,
        buf: &mut [u32],
        w: usize,
        h: usize,
        ppp: f32,
        primitives: &[ClippedPrimitive],
        clear: Color32,
    ) {
        // Por tramos: cada `BackdropBlur` desenfoca lo pintado hasta él antes de seguir.
        let clear_px = pack(clear.to_array());
        let mut start = 0;
        let mut cleared = false;
        for (i, p) in primitives.iter().enumerate() {
            let Primitive::Callback(cb) = &p.primitive else { continue };
            let Some(blur) = cb.callback.downcast_ref::<BackdropBlur>() else { continue };
            self.paint_pass(buf, w, h, ppp, &primitives[start..i], (!cleared).then_some(clear_px));
            cleared = true;
            backdrop_blur(buf, w, h, ppp, cb.rect.intersect(p.clip_rect), blur);
            start = i + 1;
        }
        self.paint_pass(buf, w, h, ppp, &primitives[start..], (!cleared).then_some(clear_px));
    }

    /// Un tramo de primitivas; con `clear`, empieza borrando el búfer de ese color.
    fn paint_pass(&self, buf: &mut [u32], w: usize, h: usize, ppp: f32, primitives: &[ClippedPrimitive], clear: Option<u32>) {
        let threads = if w * h >= 400_000 {
            std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, 8)
        } else {
            1
        };
        if threads == 1 {
            if let Some(c) = clear {
                buf.fill(c);
            }
            self.paint_rows(buf, w, 0, h, ppp, primitives);
            return;
        }
        let rows = h.div_ceil(threads * 3).max(8);
        let bands: std::sync::Mutex<Vec<(usize, &mut [u32])>> = std::sync::Mutex::new(
            buf.chunks_mut(rows * w).enumerate().map(|(i, b)| (i * rows, b)).rev().collect(),
        );
        std::thread::scope(|s| {
            for _ in 0..threads {
                let bands = &bands;
                s.spawn(move || loop {
                    let Some((y0, band)) = bands.lock().unwrap().pop() else { break };
                    let y1 = y0 + band.len() / w;
                    if let Some(c) = clear {
                        band.fill(c);
                    }
                    self.paint_rows(band, w, y0, y1, ppp, primitives);
                });
            }
        });
    }

    /// Pinta las primitivas en las filas [y0, y1) de la ventana; `band` empieza en la fila y0.
    fn paint_rows(&self, band: &mut [u32], w: usize, y0: usize, y1: usize, ppp: f32, primitives: &[ClippedPrimitive]) {
        for p in primitives {
            let Primitive::Mesh(mesh) = &p.primitive else {
                continue;
            };
            let clip = Clip {
                x0: ((p.clip_rect.min.x * ppp).floor().max(0.0)) as i32,
                y0: ((p.clip_rect.min.y * ppp).floor().max(y0 as f32)) as i32,
                x1: ((p.clip_rect.max.x * ppp).ceil()).min(w as f32) as i32,
                y1: ((p.clip_rect.max.y * ppp).ceil()).min(y1 as f32) as i32,
            };
            if clip.x0 >= clip.x1 || clip.y0 >= clip.y1 {
                continue;
            }
            let Some(tex) = self.textures.get(&mesh.texture_id) else {
                continue;
            };
            // Vecino más cercano para el atlas de fuentes (alineado a píxel) y para imágenes
            // dibujadas a su tamaño natural; bilineal solo cuando hay escalado real.
            let is_font = mesh.texture_id == TextureId::Managed(0) || is_one_to_one(mesh, tex, ppp);
            for tri in mesh.indices.chunks_exact(3) {
                let v0 = &mesh.vertices[tri[0] as usize];
                let v1 = &mesh.vertices[tri[1] as usize];
                let v2 = &mesh.vertices[tri[2] as usize];
                triangle(band, w, y0 as i32, &clip, tex, is_font, v0, v1, v2, ppp);
            }
        }
    }
}

/// Desenfoca `buf` dentro de `rect` (puntos): a un cuarto de resolución, tres pasadas de caja en
/// cada dirección (casi gaussiano) que leen también lo que hay alrededor del rectángulo, y de
/// vuelta con interpolación bilineal, solo dentro de las esquinas redondeadas. Los tres canales y
/// la vuelta se reparten entre hilos.
fn backdrop_blur(buf: &mut [u32], w: usize, h: usize, ppp: f32, rect: egui::Rect, b: &BackdropBlur) {
    let x0 = (rect.min.x * ppp).floor().max(0.0) as usize;
    let y0 = (rect.min.y * ppp).floor().max(0.0) as usize;
    let x1 = ((rect.max.x * ppp).ceil().max(0.0) as usize).min(w);
    let y1 = ((rect.max.y * ppp).ceil().max(0.0) as usize).min(h);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let s = if b.sigma * ppp >= 12.0 { 4usize } else { 2 };
    let r = ((b.sigma * ppp) / s as f32).round().max(1.0) as usize;
    let m = 3 * r * s;
    let (sx0, sy0) = (x0.saturating_sub(m), y0.saturating_sub(m));
    let (sx1, sy1) = ((x1 + m).min(w), (y1 + m).min(h));
    let (dw, dh) = ((sx1 - sx0).div_ceil(s), (sy1 - sy0).div_ceil(s));
    // Planos R, G, B reducidos (×16 para no perder precisión entre pasadas).
    let src: &[u32] = buf;
    let mut planes: [Vec<u32>; 3] = std::array::from_fn(|_| vec![0u32; dw * dh]);
    std::thread::scope(|sc| {
        for (c, plane) in planes.iter_mut().enumerate() {
            sc.spawn(move || {
                let shift = 16 - 8 * c as u32;
                for dy in 0..dh {
                    for dx in 0..dw {
                        let (mut acc, mut n) = (0u32, 0u32);
                        for yy in (sy0 + dy * s)..(sy0 + dy * s + s).min(sy1) {
                            for xx in (sx0 + dx * s)..(sx0 + dx * s + s).min(sx1) {
                                acc += (src[yy * w + xx] >> shift) & 0xff;
                                n += 1;
                            }
                        }
                        plane[dy * dw + dx] = acc * 16 / n.max(1);
                    }
                }
                let mut tmp = vec![0u32; dw * dh];
                for _ in 0..3 {
                    box_pass(plane, &mut tmp, dw, dh, r, dw, 1);
                    box_pass(&tmp, plane, dh, dw, r, 1, dw);
                }
            });
        }
    });
    // De vuelta, dentro del rectángulo con esquinas redondeadas, por bandas de filas.
    let cr = b.corner * ppp;
    let (fx0, fy0, fx1, fy1) = (rect.min.x * ppp, rect.min.y * ppp, rect.max.x * ppp, rect.max.y * ppp);
    let planes = &planes;
    let sample = move |plane: &[u32], fx: f32, fy: f32| -> u32 {
        let fx = fx.clamp(0.0, (dw - 1) as f32);
        let fy = fy.clamp(0.0, (dh - 1) as f32);
        let (ix, iy) = (fx.floor() as usize, fy.floor() as usize);
        let (ix1, iy1) = ((ix + 1).min(dw - 1), (iy + 1).min(dh - 1));
        let (tx, ty) = (fx - ix as f32, fy - iy as f32);
        let a = plane[iy * dw + ix] as f32 * (1.0 - tx) + plane[iy * dw + ix1] as f32 * tx;
        let c = plane[iy1 * dw + ix] as f32 * (1.0 - tx) + plane[iy1 * dw + ix1] as f32 * tx;
        ((a * (1.0 - ty) + c * ty) / 16.0).round().clamp(0.0, 255.0) as u32
    };
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, 8);
    let rows_per = (y1 - y0).div_ceil(threads).max(1);
    std::thread::scope(|sc| {
        for (k, chunk) in buf[y0 * w..y1 * w].chunks_mut(rows_per * w).enumerate() {
            sc.spawn(move || {
                let first = y0 + k * rows_per;
                for (j, row) in chunk.chunks_mut(w).enumerate() {
                    let py = (first + j) as f32 + 0.5;
                    let fy = (py - sy0 as f32) / s as f32 - 0.5;
                    for x in x0..x1 {
                        let px = x as f32 + 0.5;
                        // Fuera de una esquina redondeada: se deja como estaba.
                        let qx = if px < fx0 + cr { fx0 + cr - px } else if px > fx1 - cr { px - (fx1 - cr) } else { 0.0 };
                        let qy = if py < fy0 + cr { fy0 + cr - py } else if py > fy1 - cr { py - (fy1 - cr) } else { 0.0 };
                        if qx > 0.0 && qy > 0.0 && qx * qx + qy * qy > cr * cr {
                            continue;
                        }
                        let fx = (px - sx0 as f32) / s as f32 - 0.5;
                        let rr = sample(&planes[0], fx, fy);
                        let gg = sample(&planes[1], fx, fy);
                        let bb = sample(&planes[2], fx, fy);
                        row[x] = (rr << 16) | (gg << 8) | bb;
                    }
                }
            });
        }
    });
}

/// Una pasada de media móvil de radio `r` a lo largo de `n` elementos (paso `step`) en cada una
/// de `lines` líneas (separadas `stride`), con los bordes repetidos.
fn box_pass(src: &[u32], dst: &mut [u32], n: usize, lines: usize, r: usize, stride: usize, step: usize) {
    let div = (2 * r + 1) as u32;
    let at = |line: usize, i: isize| -> u32 {
        let i = i.clamp(0, n as isize - 1) as usize;
        src[line * stride + i * step]
    };
    for line in 0..lines {
        let mut acc: u32 = 0;
        for i in -(r as isize)..=(r as isize) {
            acc += at(line, i);
        }
        for i in 0..n {
            dst[line * stride + i * step] = (acc + div / 2) / div;
            acc += at(line, i as isize + r as isize + 1);
            acc -= at(line, i as isize - r as isize);
        }
    }
}

/// `true` si la malla dibuja la textura a escala 1:1 (± medio píxel).
fn is_one_to_one(mesh: &egui::epaint::Mesh, tex: &Texture, ppp: f32) -> bool {
    if mesh.vertices.len() > 512 {
        return false;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    let (mut u0, mut v0, mut u1, mut v1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for v in &mesh.vertices {
        x0 = x0.min(v.pos.x);
        y0 = y0.min(v.pos.y);
        x1 = x1.max(v.pos.x);
        y1 = y1.max(v.pos.y);
        u0 = u0.min(v.uv.x);
        v0 = v0.min(v.uv.y);
        u1 = u1.max(v.uv.x);
        v1 = v1.max(v.uv.y);
    }
    let px_w = (x1 - x0) * ppp;
    let px_h = (y1 - y0) * ppp;
    let tex_w = (u1 - u0) * tex.w as f32;
    let tex_h = (v1 - v0) * tex.h as f32;
    (px_w - tex_w).abs() <= 0.75 && (px_h - tex_h).abs() <= 0.75
}

struct Clip {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

#[inline]
fn pack(c: [u8; 4]) -> u32 {
    ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | (c[2] as u32)
}

/// Mezcla `src` (RGBA premultiplicada) sobre un píxel 0RGB opaco.
#[inline]
fn blend(dst: &mut u32, src: [u8; 4]) {
    let a = src[3] as u32;
    if a == 0 {
        return;
    }
    if a == 255 {
        *dst = pack(src);
        return;
    }
    let inv = 255 - a;
    let d = *dst;
    let dr = (d >> 16) & 0xff;
    let dg = (d >> 8) & 0xff;
    let db = d & 0xff;
    let r = (src[0] as u32 + (dr * inv + 127) / 255).min(255);
    let g = (src[1] as u32 + (dg * inv + 127) / 255).min(255);
    let b = (src[2] as u32 + (db * inv + 127) / 255).min(255);
    *dst = (r << 16) | (g << 8) | b;
}

/// Producto por canal de dos colores premultiplicados (texel × color de vértice).
#[inline]
fn modulate(t: [u8; 4], c: [f32; 4]) -> [u8; 4] {
    [
        (t[0] as f32 * c[0] * (1.0 / 255.0) + 0.5) as u8,
        (t[1] as f32 * c[1] * (1.0 / 255.0) + 0.5) as u8,
        (t[2] as f32 * c[2] * (1.0 / 255.0) + 0.5) as u8,
        (t[3] as f32 * c[3] * (1.0 / 255.0) + 0.5) as u8,
    ]
}

#[inline]
fn color_f(c: Color32) -> [f32; 4] {
    let a = c.to_array();
    [a[0] as f32, a[1] as f32, a[2] as f32, a[3] as f32]
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
fn triangle(
    buf: &mut [u32],
    stride: usize,
    y_off: i32,
    clip: &Clip,
    tex: &Texture,
    is_font: bool,
    v0: &Vertex,
    v1: &Vertex,
    v2: &Vertex,
    ppp: f32,
) {
    let (x0, y0) = (v0.pos.x * ppp, v0.pos.y * ppp);
    let (x1, y1) = (v1.pos.x * ppp, v1.pos.y * ppp);
    let (x2, y2) = (v2.pos.x * ppp, v2.pos.y * ppp);

    let area = (x1 - x0) * (y2 - y0) - (x2 - x0) * (y1 - y0);
    if area.abs() < 1e-6 {
        return;
    }
    // Normalizamos la orientación: con `sign` las funciones de arista son >= 0 dentro.
    let sign = if area > 0.0 { 1.0 } else { -1.0 };
    let inv_area = 1.0 / area.abs();

    let min_x = (x0.min(x1).min(x2).floor() as i32).max(clip.x0);
    let max_x = (x0.max(x1).max(x2).ceil() as i32).min(clip.x1);
    let min_y = (y0.min(y1).min(y2).floor() as i32).max(clip.y0);
    let max_y = (y0.max(y1).max(y2).ceil() as i32).min(clip.y1);
    if min_x >= max_x || min_y >= max_y {
        return;
    }

    // Caso rápido: color plano y sin textura variable (rellenos de rectángulos).
    let flat = v0.uv == v1.uv && v1.uv == v2.uv && v0.color == v1.color && v1.color == v2.color;
    let flat_color = if flat {
        Some(modulate(tex.nearest(v0.uv.x, v0.uv.y), color_f(v0.color)))
    } else {
        None
    };
    if let Some(c) = flat_color {
        if c[3] == 0 {
            return;
        }
    }

    let c0 = color_f(v0.color);
    let c1 = color_f(v1.color);
    let c2 = color_f(v2.color);
    // Casos frecuentes: imagen con color de vértice constante (portadas) y degradado sin
    // textura (uv constante). En ambos se evita interpolar lo que no cambia.
    let const_color = v0.color == v1.color && v1.color == v2.color;
    let white = const_color && v0.color == Color32::WHITE;
    let const_uv = v0.uv == v1.uv && v1.uv == v2.uv;
    let const_texel = if const_uv { tex.nearest(v0.uv.x, v0.uv.y) } else { [255; 4] };
    let uv0 = v0.uv;
    let uv1 = v1.uv;
    let uv2 = v2.uv;

    // Derivadas de cada función de arista respecto a x (constante por fila) y a y.
    let e0_dx = (y1 - y2) * sign;
    let e1_dx = (y2 - y0) * sign;
    let e2_dx = (y0 - y1) * sign;
    let e0_dy = (x2 - x1) * sign;
    let e1_dy = (x0 - x2) * sign;
    let e2_dy = (x1 - x0) * sign;

    for py in min_y..max_y {
        let sy = py as f32 + 0.5;
        let sx = min_x as f32 + 0.5;
        let w0 = ((x2 - x1) * (sy - y1) - (y2 - y1) * (sx - x1)) * sign;
        let w1 = ((x0 - x2) * (sy - y2) - (y0 - y2) * (sx - x2)) * sign;
        let w2 = ((x1 - x0) * (sy - y0) - (y1 - y0) * (sx - x0)) * sign;

        // Tramo [xs, xe) de la fila dentro del triángulo, con regla "top-left": un píxel
        // cuyo centro cae exactamente sobre una arista pertenece solo a uno de los dos
        // triángulos que la comparten (arista izquierda o superior). Así los rellenos
        // translúcidos no se mezclan dos veces en las costuras.
        let mut xs = min_x;
        let mut xe = max_x;
        for (w, e, ey) in [(w0, e0_dx, e0_dy), (w1, e1_dx, e1_dy), (w2, e2_dx, e2_dy)] {
            if e > 0.0 {
                // arista izquierda: inclusiva (w >= 0)
                xs = xs.max((min_x as f32 - w / e).ceil() as i32);
            } else if e < 0.0 {
                // arista derecha: exclusiva (w > 0)
                xe = xe.min((min_x as f32 - w / e).ceil() as i32);
            } else if w < 0.0 || (w == 0.0 && ey <= 0.0) {
                // arista horizontal: solo la superior (el interior queda debajo) incluye w == 0
                xs = xe;
                break;
            }
        }
        let xs = xs.max(min_x);
        let xe = xe.min(max_x);
        if xs >= xe {
            continue;
        }
        let row = (py - y_off) as usize * stride;
        match flat_color {
            Some(c) if c[3] == 255 => {
                buf[row + xs as usize..row + xe as usize].fill(pack(c));
            }
            Some(c) => {
                for dst in &mut buf[row + xs as usize..row + xe as usize] {
                    blend(dst, c);
                }
            }
            None => {
                // Baricéntricas al inicio del tramo y su derivada por píxel: uv y color
                // avanzan por suma en vez de recalcularse.
                let off = (xs - min_x) as f32;
                let b0 = ((w0 + e0_dx * off) * inv_area).clamp(0.0, 1.0);
                let b1 = ((w1 + e1_dx * off) * inv_area).clamp(0.0, 1.0 - b0);
                let b2 = 1.0 - b0 - b1;
                let db0 = e0_dx * inv_area;
                let db1 = e1_dx * inv_area;
                let db2 = -(db0 + db1);
                let mut u = uv0.x * b0 + uv1.x * b1 + uv2.x * b2;
                let mut v = uv0.y * b0 + uv1.y * b1 + uv2.y * b2;
                let du = uv0.x * db0 + uv1.x * db1 + uv2.x * db2;
                let dv = uv0.y * db0 + uv1.y * db1 + uv2.y * db2;
                let mut col = [
                    c0[0] * b0 + c1[0] * b1 + c2[0] * b2,
                    c0[1] * b0 + c1[1] * b1 + c2[1] * b2,
                    c0[2] * b0 + c1[2] * b1 + c2[2] * b2,
                    c0[3] * b0 + c1[3] * b1 + c2[3] * b2,
                ];
                let dcol = [
                    c0[0] * db0 + c1[0] * db1 + c2[0] * db2,
                    c0[1] * db0 + c1[1] * db1 + c2[1] * db2,
                    c0[2] * db0 + c1[2] * db1 + c2[2] * db2,
                    c0[3] * db0 + c1[3] * db1 + c2[3] * db2,
                ];
                let span = &mut buf[row + xs as usize..row + xe as usize];
                if const_uv && dcol.iter().all(|d| d.abs() < 1e-4) {
                    // Degradado vertical: el color no cambia a lo largo de la fila.
                    let c = modulate(const_texel, col);
                    if c[3] == 255 {
                        span.fill(pack(c));
                    } else {
                        for dst in span {
                            blend(dst, c);
                        }
                    }
                } else if const_uv {
                    // Degradado: texel fijo, solo cambia el color.
                    let tf = [const_texel[0] as f32, const_texel[1] as f32, const_texel[2] as f32, const_texel[3] as f32];
                    for dst in span {
                        let c = [
                            (col[0] * tf[0] * (1.0 / 255.0) + 0.5) as u8,
                            (col[1] * tf[1] * (1.0 / 255.0) + 0.5) as u8,
                            (col[2] * tf[2] * (1.0 / 255.0) + 0.5) as u8,
                            (col[3] * tf[3] * (1.0 / 255.0) + 0.5) as u8,
                        ];
                        blend(dst, c);
                        for k in 0..4 {
                            col[k] += dcol[k];
                        }
                    }
                } else if white && is_font && tex.opaque && dv.abs() < 1e-6 && (du * tex.w as f32 - 1.0).abs() < 1e-3 {
                    // Portada opaca a escala 1:1 y sin rotar: la fila de texels se copia tal cual.
                    if let Pixels::Rgba(p) = &tex.px {
                        let ty = ((v * tex.h as f32) as isize).clamp(0, tex.h as isize - 1) as usize;
                        let mut tx = ((u * tex.w as f32) as isize).clamp(0, tex.w as isize - 1) as usize;
                        let row_px = &p[ty * tex.w..(ty + 1) * tex.w];
                        for dst in span {
                            *dst = pack(row_px[tx]);
                            tx = (tx + 1).min(tex.w - 1);
                        }
                    }
                } else if white {
                    // Imagen sin tinte: texel directo.
                    for dst in span {
                        let t = if is_font { tex.nearest(u, v) } else { tex.bilinear(u, v) };
                        blend(dst, t);
                        u += du;
                        v += dv;
                    }
                } else if const_color {
                    for dst in span {
                        let t = if is_font { tex.nearest(u, v) } else { tex.bilinear(u, v) };
                        blend(dst, modulate(t, c0));
                        u += du;
                        v += dv;
                    }
                } else {
                    for dst in span {
                        let t = if is_font { tex.nearest(u, v) } else { tex.bilinear(u, v) };
                        blend(dst, modulate(t, col));
                        u += du;
                        v += dv;
                        for k in 0..4 {
                            col[k] += dcol[k];
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// El desenfoque deja igual un fondo liso, suaviza un borde (también en vertical, por toda la
    /// altura del panel) y no toca nada fuera del rectángulo.
    #[test]
    fn desenfoque_del_fondo() {
        let (w, h) = (240usize, 400usize);
        // Mitad de arriba negra, mitad de abajo blanca; una columna roja fuera del panel.
        let mut buf: Vec<u32> = (0..w * h).map(|i| if i / w < h / 2 { 0x000000 } else { 0xffffff }).collect();
        for y in 0..h {
            buf[y * w + 5] = 0xff0000;
        }
        let rect = egui::Rect::from_min_max(egui::pos2(40.0, 20.0), egui::pos2(200.0, 380.0));
        backdrop_blur(&mut buf, w, h, 1.0, rect, &BackdropBlur { sigma: 12.0, corner: 0.0 });
        let px = |x: usize, y: usize| buf[y * w + x] & 0xff;
        // Lejos del borde, igual que antes; en el borde, un gris intermedio; a lo largo, suave.
        assert!(px(120, 40) < 4 && px(120, 360) > 250, "{} {}", px(120, 40), px(120, 360));
        let mid = px(120, h / 2);
        assert!((90..=165).contains(&mid), "{mid}");
        assert!(px(120, h / 2 - 10) < mid && px(120, h / 2 + 10) > mid);
        // Fuera del rectángulo, nada cambia.
        assert_eq!(buf[200 * w + 5], 0xff0000);
        assert_eq!(buf[(h / 2 - 1) * w + 20] & 0xff, 0);
        assert_eq!(buf[(h / 2) * w + 20] & 0xff, 0xff);
    }
}
