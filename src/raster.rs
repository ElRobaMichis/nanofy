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
                        self.textures.insert(
                            *id,
                            Texture {
                                w: iw,
                                h: ih,
                                px,
                            },
                        );
                    }
                    Some([x, y]) => {
                        if let Some(t) = self.textures.get_mut(id) {
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
    pub fn paint(
        &self,
        buf: &mut [u32],
        w: usize,
        h: usize,
        ppp: f32,
        primitives: &[ClippedPrimitive],
        clear: Color32,
    ) {
        buf.fill(pack(clear.to_array()));
        for p in primitives {
            let Primitive::Mesh(mesh) = &p.primitive else {
                continue;
            };
            let clip = Clip {
                x0: ((p.clip_rect.min.x * ppp).floor().max(0.0)) as i32,
                y0: ((p.clip_rect.min.y * ppp).floor().max(0.0)) as i32,
                x1: ((p.clip_rect.max.x * ppp).ceil()).min(w as f32) as i32,
                y1: ((p.clip_rect.max.y * ppp).ceil()).min(h as f32) as i32,
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
                triangle(buf, w, &clip, tex, is_font, v0, v1, v2, ppp);
            }
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
fn triangle(
    buf: &mut [u32],
    stride: usize,
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
        let row = py as usize * stride;
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
