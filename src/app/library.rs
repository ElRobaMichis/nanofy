//! Página «Tu biblioteca» copiada de la referencia de diseño 12.webp (cliente = referencia +
//! (2, 2); el panel empieza en (272, 59)). Arriba, fija, la barra de herramientas: lista y
//! cuadrícula, «Recientes» (o «Antiguos»), «Agrupar», la lupa y «+». Debajo, todo lo guardado en
//! tarjetas de 171 px cada 181,25 px y filas cada 273,7 px, o en lista: «Canciones que te gustan»,
//! playlists, carpetas, álbumes y artistas, lo fijado primero con su chincheta.
//!
//! Los iconos son las máscaras recortadas de la referencia (`library_masks.rs`) pintadas en el
//! mismo píxel; las medidas de abajo son las de la referencia, en píxeles desde la esquina del
//! panel (barra) o de la tarjeta (cuadrícula).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use egui::{pos2, vec2, Color32, ColorImage, CornerRadius, Rect, Sense, TextureHandle, TextureOptions};

use super::icons::Icon;
use super::library_masks::{self as lm, LibMask};
use super::theme;
use super::widgets::{galley_truncated, keyed_child, text_on_baseline, CardKind};
use super::{strip_html, Action, App, FolderDialog, Page, PlayTarget, LIKED};
use crate::api::Req;

// --- Barra de herramientas (desde la esquina del panel) ----------------------------------------

/// Fondo del modo de vista elegido: 40,8 × 40,1, radio 6, centrado 0,16 px por encima del icono.
const VIEW_BG_W: f32 = 40.8;
const VIEW_BG_H: f32 = 40.1;
const VIEW_BG_DY: f32 = -0.16;
/// Centros de los iconos de lista y cuadrícula.
const LIST_C: egui::Pos2 = pos2(45.0, 29.5);
const GRID_C: egui::Pos2 = pos2(95.5, 29.5);
/// Texto de «Recientes»: tinta desde x 197, línea base en 36.
const SORT_TEXT_X: f32 = 197.0;
const BAR_BASE: f32 = 36.0;
const BAR_FONT: f32 = 14.7;
/// Huecos de la referencia: del texto de orden al icono de agrupar (46), de ese icono a su texto
/// (15) y de ese texto a la lupa (44).
const GAP_SORT_GROUP: f32 = 46.0;
const GAP_GROUP_TEXT: f32 = 15.0;
const GAP_GROUP_SEARCH: f32 = 44.0;
/// Donde empieza la zona que se desplaza (la barra queda fija encima).
const SCROLL_TOP: f32 = 60.0;

// --- Cuadrícula (desde la esquina del panel; la de la tarjeta, ver `card`) ---------------------

const GRID_LEFT: f32 = 24.3;
const GRID_RIGHT: f32 = 27.0;
const CARD: f32 = 171.0;
/// Ancho de las portadas cuadradas: 170,6 en la referencia (medido en cinco tarjetas); alto 171.
const COVER_W: f32 = 170.6;
const PITCH_X: f32 = 181.25;
const PITCH_Y: f32 = 273.7;
/// Borde de arriba de la primera fila.
const FIRST_ROW: f32 = 80.48;
/// Portada desde el borde de arriba de su fila: playlists (dos hojas), álbumes (una), carpetas.
const COVER_TOP_PLAYLIST: f32 = 11.4;
const COVER_TOP_ALBUM: f32 = 9.0;
const COVER_TOP_FOLDER: f32 = 9.4;
/// Línea base del título desde el borde de arriba de la fila.
const TITLE_BASE_PLAYLIST: f32 = 203.55;
const TITLE_BASE_ALBUM: f32 = 199.55;
const TITLE_BASE_ARTIST: f32 = 198.5;
/// Subtítulo: primera línea 25 px bajo el título, líneas cada 16.
const SUB_GAP: f32 = 25.0;
const SUB_LINE: f32 = 16.0;
const TITLE_FONT: f32 = 14.7;
const COUNT_FONT: f32 = 15.0;
const SUB_FONT: f32 = 12.9;
/// Chincheta: círculo de radio 20 en (151,1, 19,4) desde la esquina de la fila.
const PIN_C: egui::Vec2 = vec2(151.1, 19.4);
const PIN_R: f32 = 20.0;
/// Cabecera de un grupo (con «Agrupar»): alto y línea base de su texto.
const GROUP_HEAD_H: f32 = 52.0;
const GROUP_HEAD_BASE: f32 = 30.0;

// --- Colores de la referencia -------------------------------------------------------------------

const ICON: Color32 = Color32::from_gray(147);
const ICON_ON: Color32 = Color32::from_gray(234);
/// Textos de la barra («Recientes», «Agrupar»): en la referencia, más tenues que los iconos.
const BAR_TEXT: Color32 = Color32::from_gray(108);
const VIEW_BG: Color32 = Color32::from_gray(19);
const TITLE: Color32 = Color32::from_gray(228);
const SUB: Color32 = Color32::from_gray(122);
const COUNT: Color32 = Color32::from_gray(127);
const PLACEHOLDER: Color32 = Color32::from_gray(61);
const PLACEHOLDER_ICON: Color32 = Color32::from_gray(147);
const PLACEHOLDER_STACK: Color32 = Color32::from_gray(38);
const LIKED_COVER: Color32 = Color32::from_rgb(11, 57, 27);
const LIKED_HEART: Color32 = Color32::from_rgb(37, 211, 101);
const LIKED_FRONT: Color32 = Color32::from_rgb(20, 68, 36);
const LIKED_BACK: Color32 = Color32::from_rgb(15, 38, 23);
const LIKED_COUNT: Color32 = Color32::from_rgb(137, 157, 146);
const PIN_BG: Color32 = Color32::from_gray(18);
const PIN_ICON: Color32 = Color32::from_rgb(124, 190, 154);
/// Las playlists de Spotify con id así son sus mixes y listas: su número se tiñe con la franja
/// de color de abajo de la portada.
const SPOTIFY_MADE: &str = "37i9dQZ";

/// Colores de la página: en oscuro, los de la referencia; en claro, los de la paleta (la
/// referencia solo existe en oscuro).
#[derive(Clone, Copy)]
struct Style {
    dark: bool,
    icon: Color32,
    icon_on: Color32,
    hover: Color32,
    bar_text: Color32,
    view_bg: Color32,
    title: Color32,
    title_hover: Color32,
    sub: Color32,
    count: Color32,
    placeholder: Color32,
    placeholder_icon: Color32,
    placeholder_stack: Color32,
    folder_icon: Color32,
    pin_bg: Color32,
    field_bg: Color32,
    field_text: Color32,
    field_hint: Color32,
}

fn style(ctx: &egui::Context) -> Style {
    let p = theme::palette(ctx);
    if p.dark {
        Style {
            dark: true,
            icon: ICON,
            icon_on: ICON_ON,
            hover: Color32::from_gray(220),
            bar_text: BAR_TEXT,
            view_bg: VIEW_BG,
            title: TITLE,
            title_hover: Color32::WHITE,
            sub: SUB,
            count: COUNT,
            placeholder: PLACEHOLDER,
            placeholder_icon: PLACEHOLDER_ICON,
            placeholder_stack: PLACEHOLDER_STACK,
            folder_icon: Color32::from_gray(143),
            pin_bg: PIN_BG,
            field_bg: Color32::from_gray(31),
            field_text: Color32::from_gray(235),
            field_hint: Color32::from_gray(120),
        }
    } else {
        Style {
            dark: false,
            icon: p.weak,
            icon_on: p.text,
            hover: p.text,
            bar_text: p.weak,
            view_bg: p.hover,
            title: p.text,
            title_hover: p.text,
            sub: p.weak,
            count: p.weak,
            placeholder: Color32::from_gray(222),
            placeholder_icon: Color32::from_gray(150),
            placeholder_stack: Color32::from_gray(200),
            folder_icon: Color32::from_gray(150),
            pin_bg: Color32::from_gray(245),
            field_bg: p.card2,
            field_text: p.text,
            field_hint: p.faint,
        }
    }
}

/// Qué es cada cosa de la biblioteca (índice en su lista de `App`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Item {
    Liked,
    Playlist(usize),
    Folder(usize),
    Album(usize),
    Artist(usize),
}

impl Item {
    fn group(self) -> usize {
        match self {
            Item::Liked | Item::Playlist(_) => 0,
            Item::Folder(_) => 1,
            Item::Album(_) => 2,
            Item::Artist(_) => 3,
        }
    }
}

const GROUPS: [&str; 4] = ["Playlists", "Carpetas", "Álbumes", "Artistas"];

/// Clave de una uri de Spotify en la biblioteca (la de `pinned` y del registro de lo reciente):
/// el id de una playlist, `album:<id>`, `artist:<id>` o `liked`.
pub fn library_key(uri: &str) -> Option<String> {
    let parts: Vec<&str> = uri.split(':').collect();
    match parts.as_slice() {
        ["spotify", "playlist", id] => Some(id.to_string()),
        ["spotify", "album", id] => Some(format!("album:{id}")),
        ["spotify", "artist", id] => Some(format!("artist:{id}")),
        ["spotify", "collection", ..] => Some("liked".into()),
        ["spotify", "user", _, "collection", ..] => Some("liked".into()),
        _ => None,
    }
}

thread_local! {
    /// Máscaras de la biblioteca ya pasadas a textura, por nombre y escala.
    static MASKS: RefCell<HashMap<(&'static str, u16), TextureHandle>> = RefCell::new(HashMap::new());
}

/// Pinta la máscara `m` con su esquina a (`ox`, `oy`) píxeles del ancla `anchor` (en píxeles de
/// pantalla ya redondeados). Con la interfaz ampliada se escala.
pub(super) fn paint_mask(painter: &egui::Painter, name: &'static str, m: &LibMask, anchor: (f32, f32), color: Color32) {
    if color.a() == 0 {
        return;
    }
    let ppp = painter.pixels_per_point();
    let (w, h) = ((m.w as f32 * ppp).round() as usize, (m.h as f32 * ppp).round() as usize);
    let key = (name, (ppp * 100.0).round() as u16);
    let tex = MASKS.with(|c| {
        c.borrow_mut()
            .entry(key)
            .or_insert_with(|| {
                let at = |x: isize, y: isize| -> f32 {
                    if x < 0 || y < 0 || x >= m.w as isize || y >= m.h as isize {
                        0.0
                    } else {
                        m.alpha[y as usize * m.w + x as usize] as f32
                    }
                };
                let mut pixels = Vec::with_capacity(w * h);
                for y in 0..h {
                    for x in 0..w {
                        let a = if w == m.w && h == m.h {
                            m.alpha[y * m.w + x] as f32
                        } else {
                            let sx = (x as f32 + 0.5) / ppp - 0.5;
                            let sy = (y as f32 + 0.5) / ppp - 0.5;
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
                painter.ctx().load_texture(format!("biblioteca-{name}-{w}"), ColorImage::new([w, h], pixels), TextureOptions::NEAREST)
            })
            .id()
    });
    let x = anchor.0 + (m.ox as f32 * ppp).round();
    let y = anchor.1 + (m.oy as f32 * ppp).round();
    let r = Rect::from_min_size(pos2(x / ppp, y / ppp), vec2(w as f32 / ppp, h as f32 / ppp));
    painter.image(tex, r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), color);
}

/// Un punto en píxeles de pantalla redondeados (el ancla de una máscara).
pub(super) fn px(painter: &egui::Painter, p: egui::Pos2) -> (f32, f32) {
    let ppp = painter.pixels_per_point();
    ((p.x * ppp).round(), (p.y * ppp).round())
}

/// Número de canciones teñido con la franja de abajo de la portada de un mix de Spotify: gris
/// claro (195) con un 12 % de lo que esa franja se aparta de su gris.
fn count_tint(bottom: Color32) -> Color32 {
    let (r, g, b) = (bottom.r() as f32, bottom.g() as f32, bottom.b() as f32);
    let m = (r + g + b) / 3.0;
    let c = |v: f32| (195.0 + 0.12 * (v - m)).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(c(r), c(g), c(b))
}

/// Lo que pinta una tarjeta.
struct CardData {
    item: Item,
    key: String,
    title: String,
    subtitle: String,
    count: Option<u32>,
    count_color: Color32,
    cover: Option<String>,
    pinned: bool,
}

impl App {
    /// Clave de una cosa de la biblioteca (ver `library_key`).
    fn item_key(&self, item: Item) -> String {
        match item {
            Item::Liked => "liked".into(),
            Item::Playlist(i) => self.playlists[i].id.clone(),
            Item::Folder(i) => format!("folder:{}", self.folders[i].id),
            Item::Album(i) => format!("album:{}", self.saved_albums[i].id),
            Item::Artist(i) => format!("artist:{}", self.followed_artists[i].id),
        }
    }

    /// ¿Está fijada? «Canciones que te gustan» va fijada de fábrica, como en Spotify.
    fn item_pinned(&self, key: &str) -> bool {
        if key == "liked" {
            !self.settings.liked_unpinned
        } else {
            self.settings.pinned.iter().any(|k| k == key)
        }
    }

    pub(super) fn set_library_pin(&mut self, key: &str, on: bool) {
        if key == "liked" {
            self.settings.liked_unpinned = !on;
            self.settings.save(&self.paths);
        } else {
            self.actions.push(Action::Pin(key.to_string(), on));
        }
    }

    /// Apunta que acaba de sonar algo de la biblioteca (lo ordena «Recientes»).
    pub(super) fn note_library_play(&mut self, t: &PlayTarget) {
        let key = match t {
            PlayTarget::Context { uri, .. } => library_key(uri),
            PlayTarget::Tracks { .. } if matches!(self.last_play_page, Some(Page::Liked)) => Some("liked".into()),
            PlayTarget::Tracks { .. } => None,
        };
        if let Some(k) = key {
            self.library_recent.insert(k, crate::cache::now_secs());
            self.save_library_recent();
        }
    }

    /// Junta lo reproducido hace poco en la cuenta (recently-played) con lo apuntado aquí.
    pub(super) fn merge_recent_contexts(&mut self, list: Vec<(String, u64)>) {
        let mut changed = false;
        for (uri, at) in list {
            if let Some(k) = library_key(&uri) {
                let e = self.library_recent.entry(k).or_insert(0);
                if at > *e {
                    *e = at;
                    changed = true;
                }
            }
        }
        if changed {
            self.save_library_recent();
        }
    }

    fn save_library_recent(&self) {
        // Solo lo más reciente: el archivo no debe crecer sin fin.
        let mut v: Vec<(&String, &u64)> = self.library_recent.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        v.truncate(1000);
        let map: HashMap<&String, &u64> = v.into_iter().collect();
        if let Ok(json) = serde_json::to_string(&map) {
            let _ = std::fs::write(&self.library_recent_path, json);
        }
    }

    /// Lo que se ve, en orden: lo fijado primero (por orden de fijado) y el resto por lo último
    /// que sonó (o al revés), con lo que nunca sonó detrás en el orden de la biblioteca. Dentro
    /// de una carpeta, solo sus playlists.
    fn library_items(&self) -> Vec<Item> {
        let filter = self.library_filter.trim().to_lowercase();
        let matches = |s: &str| filter.is_empty() || s.to_lowercase().contains(&filter);
        let mut items: Vec<(Item, u64, usize)> = Vec::new();
        let recent = |k: &str| self.library_recent.get(k).copied().unwrap_or(0);
        let pl_index: HashMap<&str, usize> = self.playlists.iter().enumerate().map(|(i, p)| (p.id.as_str(), i)).collect();
        if let Some(fid) = &self.library_folder {
            if let Some(f) = self.folders.iter().find(|f| &f.id == fid) {
                for id in &f.playlists {
                    if let Some(&i) = pl_index.get(id.as_str()) {
                        if matches(&self.playlists[i].name) {
                            items.push((Item::Playlist(i), recent(id), i));
                        }
                    }
                }
            }
        } else {
            if matches("canciones que te gustan") {
                items.push((Item::Liked, recent("liked"), 0));
            }
            let in_folder: HashSet<&str> = self.folders.iter().flat_map(|f| f.playlists.iter().map(String::as_str)).collect();
            for (i, pl) in self.playlists.iter().enumerate() {
                if !in_folder.contains(pl.id.as_str()) && matches(&pl.name) {
                    items.push((Item::Playlist(i), recent(&pl.id), 1 + i));
                }
            }
            for (i, f) in self.folders.iter().enumerate() {
                if !matches(&f.name) {
                    continue;
                }
                let first = f.playlists.iter().filter_map(|id| pl_index.get(id.as_str())).min().copied().unwrap_or(self.playlists.len());
                let last = f.playlists.iter().map(|id| recent(id)).max().unwrap_or(0);
                items.push((Item::Folder(i), last, 1 + first));
            }
            for (i, a) in self.saved_albums.iter().enumerate() {
                if matches(&a.name) || matches(&a.artists_str()) {
                    items.push((Item::Album(i), recent(&format!("album:{}", a.id)), 100_000 + i));
                }
            }
            for (i, a) in self.followed_artists.iter().enumerate() {
                if matches(&a.name) {
                    items.push((Item::Artist(i), recent(&format!("artist:{}", a.id)), 200_000 + i));
                }
            }
        }
        // Fijadas: «Canciones que te gustan» y luego por orden de fijado (la última primero).
        let pin_order: HashMap<&str, usize> = self.settings.pinned.iter().enumerate().map(|(i, k)| (k.as_str(), i)).collect();
        let (mut pinned, mut rest): (Vec<_>, Vec<_>) = items.into_iter().partition(|(it, ..)| self.item_pinned(&self.item_key(*it)));
        pinned.sort_by_key(|(it, ..)| if *it == Item::Liked { 0 } else { 1 + pin_order.get(self.item_key(*it).as_str()).copied().unwrap_or(usize::MAX - 1) });
        rest.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
        if self.settings.library_oldest {
            rest.reverse();
        }
        pinned.into_iter().chain(rest).map(|(it, ..)| it).collect()
    }

    /// Título, subtítulo, número y portada de una tarjeta.
    fn card_data(&self, item: Item) -> CardData {
        let key = self.item_key(item);
        let pinned = self.item_pinned(&key);
        let and_more = |names: Vec<String>, more: bool| -> String {
            let list = names.join(", ");
            if more && !list.is_empty() {
                format!("{list} y más")
            } else {
                list
            }
        };
        match item {
            Item::Liked => {
                let list = self.lists.get(LIKED);
                let total = list.map(|l| l.total).filter(|&t| t > 0).unwrap_or(self.liked_set.len() as u32);
                let names: Vec<String> = list.map(|l| l.tracks.iter().take(3).map(|t| t.name.clone()).collect()).unwrap_or_default();
                let subtitle = if names.is_empty() { format!("{total} canciones") } else { and_more(names, total > 3) };
                CardData { item, key, title: "Canciones que te gustan".into(), subtitle, count: Some(total), count_color: LIKED_COUNT, cover: None, pinned }
            }
            Item::Playlist(i) => {
                let pl = &self.playlists[i];
                let cover = pl.cover(300).map(str::to_string);
                let spotify = pl.id.starts_with(SPOTIFY_MADE);
                let description = pl.description.as_deref().map(strip_html).filter(|d| !d.trim().is_empty());
                let subtitle = match (spotify, description) {
                    (true, Some(d)) => d,
                    (_, d) => {
                        let mut seen = HashSet::new();
                        let artists: Vec<String> = self
                            .lists
                            .get(&pl.id)
                            .map(|l| {
                                l.tracks
                                    .iter()
                                    .filter_map(|t| t.artists.first().map(|a| a.name.clone()))
                                    .filter(|n| seen.insert(n.clone()))
                                    .take(3)
                                    .collect()
                            })
                            .unwrap_or_default();
                        if artists.len() >= 2 {
                            let more = artists.len() > 2;
                            and_more(artists.into_iter().take(2).collect(), more)
                        } else if let Some(d) = d {
                            d
                        } else {
                            match pl.owner_name() {
                                "" => "Playlist".to_string(),
                                owner => format!("De {owner}"),
                            }
                        }
                    }
                };
                let count = pl.tracks.as_ref().map(|t| t.total).or_else(|| self.lists.get(&pl.id).map(|l| l.total));
                let count_color = match &cover {
                    Some(u) if spotify => self.images.bottom(u).map(count_tint).unwrap_or(COUNT),
                    _ => COUNT,
                };
                CardData { item, key, title: pl.name.clone(), subtitle, count, count_color, cover, pinned }
            }
            Item::Folder(i) => {
                let f = &self.folders[i];
                let names: Vec<String> = f
                    .playlists
                    .iter()
                    .filter_map(|id| self.playlists.iter().find(|p| &p.id == id).map(|p| p.name.clone()))
                    .collect();
                let n = f.playlists.len() as u32;
                let more = names.len() > 2;
                let subtitle = and_more(names.into_iter().take(2).collect(), more);
                CardData { item, key, title: f.name.clone(), subtitle, count: Some(n), count_color: COUNT, cover: None, pinned }
            }
            Item::Album(i) => {
                let a = &self.saved_albums[i];
                CardData {
                    item,
                    key,
                    title: a.name.clone(),
                    subtitle: a.artists_str(),
                    count: a.total_tracks,
                    count_color: COUNT,
                    cover: a.cover(300).map(str::to_string),
                    pinned,
                }
            }
            Item::Artist(i) => {
                let a = &self.followed_artists[i];
                CardData { item, key, title: a.name.clone(), subtitle: String::new(), count: None, count_color: COUNT, cover: a.cover(300).map(str::to_string), pinned }
            }
        }
    }

    /// La página entera dentro del panel `panel`: la barra fija y lo de debajo, que se desplaza.
    pub(super) fn library_panel(&mut self, ui: &mut egui::Ui, panel: Rect) {
        self.request_once("albums", Req::SavedAlbums);
        self.request_once("artists", Req::FollowedArtists);
        self.request_once("rootlist", Req::Rootlist);
        self.request_once("recent_contexts", Req::RecentContexts);
        let origin = pos2(panel.min.x.round(), panel.min.y.round());
        self.library_toolbar(ui, origin, panel);

        let view = Rect::from_min_max(pos2(panel.min.x, origin.y + SCROLL_TOP), pos2(panel.max.x, panel.max.y - 1.0));
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(view));
        c.set_clip_rect(view.intersect(panel.shrink(1.0)));
        let salt = format!("biblioteca-{:?}-{}", self.library_folder, self.settings.library_grid);
        egui::ScrollArea::vertical().id_salt(salt).auto_shrink([false, false]).show(&mut c, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            if self.settings.library_grid {
                self.library_grid(ui, origin, panel);
            } else {
                self.library_list(ui);
            }
        });
    }

    /// Barra de arriba: vista, orden, agrupar, buscar y «+», en las posiciones de la referencia.
    fn library_toolbar(&mut self, ui: &mut egui::Ui, o: egui::Pos2, panel: Rect) {
        let st = style(ui.ctx());
        let painter = ui.painter().clone();
        let at = |x: f32, y: f32| pos2(o.x + x, o.y + y);
        let hand = |ui: &egui::Ui, r: &egui::Response| {
            if r.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        };

        // Lista y cuadrícula: la elegida en blanco sobre su recuadro.
        let grid = self.settings.library_grid;
        let on = if grid { GRID_C } else { LIST_C };
        let bg = Rect::from_center_size(at(on.x, on.y + VIEW_BG_DY), vec2(VIEW_BG_W, VIEW_BG_H));
        painter.rect_filled(bg, CornerRadius::same(6), st.view_bg);
        let list_r = ui.interact(Rect::from_center_size(at(LIST_C.x, LIST_C.y), vec2(40.0, 40.0)), ui.id().with("lib_list"), Sense::click());
        let grid_r = ui.interact(Rect::from_center_size(at(GRID_C.x, GRID_C.y), vec2(40.0, 40.0)), ui.id().with("lib_grid"), Sense::click());
        hand(ui, &list_r);
        hand(ui, &grid_r);
        let tone = |sel: bool, hov: bool| if sel { st.icon_on } else if hov { st.hover } else { st.icon };
        paint_mask(&painter, "list", &lm::LIST, px(&painter, o), tone(!grid, list_r.hovered()));
        paint_mask(&painter, "grid", &lm::GRID, px(&painter, o), tone(grid, grid_r.hovered()));
        if list_r.on_hover_text("Lista").clicked() && grid {
            self.set_library_grid(false);
        }
        if grid_r.on_hover_text("Cuadrícula").clicked() && !grid {
            self.set_library_grid(true);
        }

        // Orden: «Recientes» o «Antiguos».
        let label = if self.settings.library_oldest { "Antiguos" } else { "Recientes" };
        let g = painter.layout_no_wrap(label.into(), theme::semibold(BAR_FONT), st.bar_text);
        let ink = ink_left(&g);
        let text_w = g.size().x - ink - ink_right(&g);
        let sort_hit = Rect::from_min_max(at(157.0, 9.0), at(SORT_TEXT_X + text_w + 8.0, 50.0));
        let sort_r = ui.interact(sort_hit, ui.id().with("lib_sort"), Sense::click());
        hand(ui, &sort_r);
        let hov = sort_r.hovered();
        paint_mask(&painter, "sort", &lm::SORT, px(&painter, o), if hov { st.hover } else { st.icon });
        text_on_baseline(&painter, at(SORT_TEXT_X - ink, BAR_BASE), g, if hov { st.hover } else { st.bar_text });
        let tip = if self.settings.library_oldest { "De lo más antiguo a lo más reciente" } else { "De lo más reciente a lo más antiguo" };
        if sort_r.on_hover_text(tip).clicked() {
            self.settings.library_oldest = !self.settings.library_oldest;
            self.settings.save(&self.paths);
        }

        // Agrupar: su icono a 46 px del texto de orden y su texto 15 más allá.
        let group_x = SORT_TEXT_X + text_w + GAP_SORT_GROUP;
        let dx = group_x - (lm::GROUP.ox as f32 + 2.0);
        let ga = painter.layout_no_wrap("Agrupar".into(), theme::semibold(BAR_FONT), st.icon);
        let ga_ink = ink_left(&ga);
        let ga_w = ga.size().x - ga_ink - ink_right(&ga);
        let gtext_x = group_x + 20.0 + GAP_GROUP_TEXT;
        let group_hit = Rect::from_min_max(at(group_x - 8.0, 9.0), at(gtext_x + ga_w + 8.0, 50.0));
        let group_r = ui.interact(group_hit, ui.id().with("lib_group"), Sense::click());
        hand(ui, &group_r);
        let grouped = self.settings.library_grouped;
        let hov = group_r.hovered();
        let c = if grouped { st.icon_on } else if hov { st.hover } else { st.icon };
        paint_mask(&painter, "group", &lm::GROUP, px(&painter, at(dx, 0.0)), c);
        let tc = if grouped { st.icon_on } else if hov { st.hover } else { st.bar_text };
        text_on_baseline(&painter, at(gtext_x - ga_ink, BAR_BASE), ga, tc);
        egui::Popup::menu(&group_r).show(|ui| {
            ui.set_min_width(200.0);
            if Self::menu_item(ui, if grouped { None } else { Some(Icon::Check) }, "Sin agrupar", false).clicked() {
                self.settings.library_grouped = false;
                self.settings.save(&self.paths);
                ui.close();
            }
            if Self::menu_item(ui, if grouped { Some(Icon::Check) } else { None }, "Por tipo", false).clicked() {
                self.settings.library_grouped = true;
                self.settings.save(&self.paths);
                ui.close();
            }
        });

        // Lupa: abre un campo para filtrar la biblioteca.
        let search_x = gtext_x + ga_w + GAP_GROUP_SEARCH;
        let sdx = search_x - (lm::SEARCH.ox as f32 + 2.0);
        let s_hit = Rect::from_min_size(at(search_x - 9.0, 9.0), vec2(40.0, 41.0));
        let open = self.library_search_open || !self.library_filter.is_empty();
        if open {
            let field_bg = Rect::from_min_max(at(search_x - 12.0, 10.0), at(search_x + 260.0, 48.0));
            painter.rect_filled(field_bg, CornerRadius::same(6), st.field_bg);
        }
        let s_r = ui.interact(s_hit, ui.id().with("lib_search"), Sense::click());
        hand(ui, &s_r);
        let c = if open { st.icon_on } else if s_r.hovered() { st.hover } else { st.icon };
        paint_mask(&painter, "search", &lm::SEARCH, px(&painter, at(sdx, 0.0)), c);
        if s_r.on_hover_text("Buscar en Tu biblioteca").clicked() {
            self.library_search_open = !open;
            if !self.library_search_open {
                self.library_filter.clear();
            }
        }
        if open {
            let field = Rect::from_min_max(at(search_x + 32.0, 16.0), at(search_x + 250.0, 42.0));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(field));
            let edit = egui::TextEdit::singleline(&mut self.library_filter)
                .frame(egui::Frame::NONE)
                .font(theme::regular(14.0))
                .text_color(st.field_text)
                .desired_width(field.width())
                .vertical_align(egui::Align::Center)
                .hint_text(egui::RichText::new("Buscar en Tu biblioteca").color(st.field_hint));
            let r = child.add_sized(field.size(), edit);
            if self.library_search_open && self.library_filter.is_empty() && !r.has_focus() && !r.lost_focus() {
                r.request_focus();
            }
            if r.lost_focus() && self.library_filter.is_empty() {
                self.library_search_open = false;
            }
            if r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.library_filter.clear();
                self.library_search_open = false;
            }
        }

        // «+»: crear playlist o carpeta.
        let po = pos2(panel.max.x.round(), o.y);
        let plus_hit = Rect::from_center_size(pos2(po.x + lm::PLUS.ox as f32 + 2.0 + 8.5, o.y + 29.0), vec2(40.0, 40.0));
        let plus_r = ui.interact(plus_hit, ui.id().with("lib_plus"), Sense::click());
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

    fn set_library_grid(&mut self, on: bool) {
        self.library_grid = on;
        self.settings.library_grid = on;
        self.settings.save(&self.paths);
    }

    /// Cuadrícula: columnas de 171 cada 181,25 desde x 24,3; filas cada 273,7. Solo se pinta lo que se
    /// ve (las filas fuera de la vista solo reservan su alto).
    fn library_grid(&mut self, ui: &mut egui::Ui, o: egui::Pos2, panel: Rect) {
        let st = style(ui.ctx());
        let items = self.library_items();
        let avail = panel.width() - GRID_LEFT - GRID_RIGHT + (PITCH_X - CARD);
        let cols = ((avail / PITCH_X).floor() as usize).max(1);
        // Secciones: una sola sin agrupar; por tipo, una por cada tipo con algo.
        let mut sections: Vec<(Option<&str>, Vec<Item>)> = Vec::new();
        if self.settings.library_grouped && self.library_folder.is_none() {
            for (g, name) in GROUPS.iter().enumerate() {
                let v: Vec<Item> = items.iter().copied().filter(|it| it.group() == g).collect();
                if !v.is_empty() {
                    sections.push((Some(name), v));
                }
            }
        } else {
            sections.push((None, items));
        }
        let folder_head = self.library_folder.is_some();
        let empty = sections.iter().all(|(_, v)| v.is_empty());
        // Alto de todo para reservarlo de una vez.
        let mut h = FIRST_ROW - SCROLL_TOP + if folder_head { GROUP_HEAD_H } else { 0.0 };
        for (head, v) in &sections {
            if head.is_some() {
                h += GROUP_HEAD_H;
            }
            h += v.len().div_ceil(cols) as f32 * PITCH_Y;
        }
        if empty {
            h += 60.0;
        }
        let (block, _) = ui.allocate_exact_size(vec2(panel.width() - 2.0, h + 24.0), Sense::hover());
        let clip = ui.clip_rect();
        let mut y = block.min.y + FIRST_ROW - SCROLL_TOP;
        // La cabecera de la carpeta va siempre, también vacía: es la forma de salir de ella.
        if folder_head {
            self.folder_header(ui, pos2(o.x + GRID_LEFT, block.min.y + 8.0));
            y += GROUP_HEAD_H;
        }
        if empty {
            let text = if !self.library_filter.trim().is_empty() {
                "Nada coincide con tu búsqueda"
            } else if folder_head {
                "Esta carpeta está vacía. Añade playlists desde su menú («Añadir a carpeta»)."
            } else {
                "Aquí aparecerá lo que guardes"
            };
            let g = ui.painter().layout_no_wrap(text.into(), theme::regular(14.0), st.sub);
            let at = if folder_head { y - FIRST_ROW + SCROLL_TOP + 20.0 } else { y + 20.0 };
            text_on_baseline(ui.painter(), pos2(o.x + GRID_LEFT, at), g, st.sub);
            return;
        }
        for (head, v) in sections {
            if let Some(name) = head {
                if y + GROUP_HEAD_H > clip.min.y && y < clip.max.y {
                    let g = ui.painter().layout_no_wrap(name.to_string(), theme::semibold(20.0), st.title);
                    text_on_baseline(ui.painter(), pos2(o.x + GRID_LEFT, y + GROUP_HEAD_BASE), g, st.title);
                }
                y += GROUP_HEAD_H;
            }
            for (row, chunk) in v.chunks(cols).enumerate() {
                let t = y + row as f32 * PITCH_Y;
                if t + PITCH_Y < clip.min.y || t > clip.max.y {
                    continue;
                }
                for (col, &item) in chunk.iter().enumerate() {
                    let x = o.x + GRID_LEFT + col as f32 * PITCH_X;
                    self.library_card(ui, x, t, item);
                }
            }
            y += v.len().div_ceil(cols) as f32 * PITCH_Y;
        }
    }

    /// Cabecera dentro de una carpeta: «‹ Nombre», que vuelve a la biblioteca.
    fn folder_header(&mut self, ui: &mut egui::Ui, at: egui::Pos2) {
        let st = style(ui.ctx());
        let name = self.library_folder.as_ref().and_then(|id| self.folders.iter().find(|f| &f.id == id)).map(|f| f.name.clone());
        let Some(name) = name else {
            self.library_folder = None;
            return;
        };
        let g = ui.painter().layout_no_wrap(name, theme::semibold(20.0), st.title);
        let w = g.size().x;
        let rect = Rect::from_min_size(at, vec2(30.0 + w, 30.0));
        let r = ui.interact(rect, ui.id().with("lib_folder_back"), Sense::click());
        if r.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        super::icons::paint(ui.painter(), Rect::from_center_size(pos2(at.x + 10.0, at.y + 15.0), vec2(20.0, 20.0)), if r.hovered() { st.title } else { st.icon }, Icon::ArrowLeft);
        text_on_baseline(ui.painter(), pos2(at.x + 28.0, at.y + 22.0), g, st.title);
        if r.on_hover_text("Volver a Tu biblioteca").clicked() {
            self.library_folder = None;
        }
    }

    /// Una tarjeta con la esquina de su fila en (`x`, `t`).
    fn library_card(&mut self, ui: &mut egui::Ui, x: f32, t: f32, item: Item) {
        let st = style(ui.ctx());
        let d = self.card_data(item);
        let rect = Rect::from_min_size(pos2(x, t), vec2(CARD, PITCH_Y - 12.0));
        let resp = ui.interact(rect, ui.id().with(("lib_card", &d.key)), Sense::click());
        let hovered = resp.hovered();
        if hovered {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let painter = ui.painter().clone();
        let top_rounded = |r: u8| CornerRadius { nw: r, ne: r, sw: 0, se: 0 };
        let stack = |app: &App| match (&d.item, &d.cover) {
            (Item::Liked, _) => LIKED_FRONT,
            (_, Some(u)) => app.images.stack(u).unwrap_or(st.placeholder_stack),
            _ => st.placeholder_stack,
        };
        let dim = |c: Color32| Color32::from_rgb((c.r() as f32 * 0.55) as u8, (c.g() as f32 * 0.55) as u8, (c.b() as f32 * 0.55) as u8);

        // Hojas detrás y portada.
        let (cover_top, title_base) = match item {
            Item::Liked | Item::Playlist(_) => {
                let front = stack(self);
                let back = if item == Item::Liked { LIKED_BACK } else { dim(front) };
                painter.rect_filled(Rect::from_min_max(pos2(x + 19.0, t + 0.3), pos2(x + CARD - 19.0, t + 3.3)), top_rounded(2), back);
                painter.rect_filled(Rect::from_min_max(pos2(x + 9.0, t + 4.4), pos2(x + CARD - 9.0, t + 10.5)), top_rounded(3), front);
                (t + COVER_TOP_PLAYLIST, t + TITLE_BASE_PLAYLIST)
            }
            Item::Album(_) => {
                painter.rect_filled(Rect::from_min_max(pos2(x + 8.0, t), pos2(x + CARD - 8.0, t + 7.6)), top_rounded(4), stack(self));
                (t + COVER_TOP_ALBUM, t + TITLE_BASE_ALBUM)
            }
            Item::Folder(_) => {
                // La pestaña de la carpeta.
                painter.rect_filled(Rect::from_min_max(pos2(x + 7.0, t + 0.5), pos2(x + 79.0, t + 7.6)), top_rounded(4), st.placeholder_stack);
                (t + COVER_TOP_FOLDER, t + TITLE_BASE_PLAYLIST)
            }
            Item::Artist(_) => (t, t + TITLE_BASE_ARTIST),
        };
        let cover_w = if matches!(item, Item::Artist(_)) { CARD } else { COVER_W };
        let cover = Rect::from_min_size(pos2(x, cover_top), vec2(cover_w, CARD));
        let anchor = px(&painter, cover.min);
        let radius = if matches!(item, Item::Artist(_)) { (CARD / 2.0) as u8 } else { 6 };
        match item {
            Item::Liked => {
                painter.rect_filled(cover, CornerRadius::same(6), LIKED_COVER);
                paint_mask(&painter, "heart", &lm::HEART, anchor, LIKED_HEART);
            }
            Item::Folder(_) => {
                painter.rect_filled(cover, CornerRadius::same(6), st.placeholder);
                paint_mask(&painter, "folder", &lm::FOLDER, anchor, st.folder_icon);
            }
            _ if d.cover.is_none() => {
                painter.rect_filled(cover, CornerRadius::same(radius), st.placeholder);
                paint_mask(&painter, "note", &lm::NOTE, anchor, st.placeholder_icon);
            }
            _ => self.cover_in(ui, d.cover.as_deref(), cover, radius),
        }
        if hovered {
            painter.rect_filled(cover, CornerRadius::same(radius), Color32::from_white_alpha(14));
        }

        // Chincheta: círculo oscuro arriba a la derecha con el alfiler verde.
        if d.pinned {
            painter.circle_filled(pos2(x + PIN_C.x, t + PIN_C.y), PIN_R, st.pin_bg);
            let a = px(&painter, pos2(x + CARD, t));
            paint_mask(&painter, "pin", &lm::PIN, (anchor.0 + (CARD * painter.pixels_per_point()).round(), a.1), PIN_ICON);
        }

        // Textos.
        let title_color = if hovered { st.title_hover } else { st.title };
        let count_color = if st.dark || item == Item::Liked { d.count_color } else { st.count };
        if let Item::Artist(_) = item {
            let g = galley_truncated(&painter, &d.title, theme::semibold(TITLE_FONT), title_color, CARD);
            let w = g.size().x;
            text_on_baseline(&painter, pos2(x + (CARD - w) / 2.0, title_base), g, title_color);
        } else {
            let mut title_w = CARD;
            if let Some(n) = d.count {
                let g = painter.layout_no_wrap(n.to_string(), theme::regular(COUNT_FONT), count_color);
                let w = g.size().x - ink_right(&g);
                text_on_baseline(&painter, pos2(x + cover_w - 1.2 - w, title_base), g, count_color);
                title_w = CARD - w - 12.0;
            }
            let g = galley_truncated(&painter, &d.title, theme::semibold(TITLE_FONT), title_color, title_w);
            let ink = ink_left(&g);
            text_on_baseline(&painter, pos2(x - ink, title_base), g, title_color);
            if !d.subtitle.is_empty() {
                let mut job = egui::text::LayoutJob::single_section(
                    d.subtitle.clone(),
                    egui::TextFormat { font_id: theme::regular(SUB_FONT), color: st.sub, line_height: Some(SUB_LINE), ..Default::default() },
                );
                job.wrap = egui::text::TextWrapping { max_width: CARD, max_rows: 2, break_anywhere: false, overflow_character: Some('…') };
                let g = painter.layout_job(job);
                let ink = ink_left(&g);
                text_on_baseline(&painter, pos2(x - ink, title_base + SUB_GAP), g, st.sub);
            }
        }

        if resp.clicked() {
            self.open_library_item(item);
        }
        self.library_item_menu(&resp, item, &d.key, d.pinned);
    }

    fn open_library_item(&mut self, item: Item) {
        match item {
            Item::Liked => self.actions.push(Action::Go(Page::Liked)),
            Item::Playlist(i) => {
                let pl = self.playlists[i].clone();
                self.actions.push(Action::OpenPlaylist(pl));
            }
            Item::Folder(i) => {
                self.library_folder = Some(self.folders[i].id.clone());
                self.library_filter.clear();
                self.library_search_open = false;
            }
            Item::Album(i) => self.actions.push(Action::Go(Page::Album(self.saved_albums[i].id.clone()))),
            Item::Artist(i) => self.actions.push(Action::Go(Page::Artist(self.followed_artists[i].id.clone()))),
        }
    }

    /// Menú del clic derecho: el de las playlists de siempre; en lo demás, fijar y lo propio.
    fn library_item_menu(&mut self, resp: &egui::Response, item: Item, key: &str, pinned: bool) {
        if let Item::Playlist(i) = item {
            let pl = self.playlists[i].clone();
            self.playlist_row_menu(resp, &pl);
            return;
        }
        let key = key.to_string();
        resp.context_menu(|ui| {
            if Self::menu_item(ui, Some(Icon::Pin), if pinned { "Desfijar" } else { "Fijar" }, false).clicked() {
                self.set_library_pin(&key, !pinned);
                ui.close();
            }
            match item {
                Item::Folder(i) => {
                    let f = self.folders[i].clone();
                    if Self::menu_item(ui, Some(Icon::Edit), "Renombrar…", false).clicked() {
                        self.folder_dialog = Some(FolderDialog { id: Some(f.id.clone()), name: f.name.clone(), playlist: None, busy: false });
                        ui.close();
                    }
                    if Self::menu_item(ui, Some(Icon::Trash), "Eliminar carpeta", false).clicked() {
                        self.api.send(Req::FolderDelete(f.id.clone()));
                        ui.close();
                    }
                }
                Item::Album(i) => {
                    let id = self.saved_albums[i].id.clone();
                    if Self::menu_item(ui, Some(Icon::NewTab), "Abrir en una nueva pestaña", false).clicked() {
                        self.actions.push(Action::OpenInTab(Page::Album(id)));
                        ui.close();
                    }
                }
                Item::Artist(i) => {
                    let id = self.followed_artists[i].id.clone();
                    if Self::menu_item(ui, Some(Icon::NewTab), "Abrir en una nueva pestaña", false).clicked() {
                        self.actions.push(Action::OpenInTab(Page::Artist(id)));
                        ui.close();
                    }
                }
                _ => {}
            }
        });
    }

    /// Vista de lista: una fila de 56 px por cosa (las de siempre), con lo fijado primero.
    fn library_list(&mut self, ui: &mut egui::Ui) {
        let items = self.library_items();
        ui.add_space(FIRST_ROW - SCROLL_TOP - 8.0);
        if self.library_folder.is_some() {
            let at = ui.min_rect().min;
            self.folder_header(ui, pos2(at.x + GRID_LEFT, ui.cursor().min.y));
            ui.add_space(GROUP_HEAD_H);
        }
        let n = items.len();
        if n == 0 {
            let text = if !self.library_filter.trim().is_empty() {
                "Nada coincide con tu búsqueda"
            } else if self.library_folder.is_some() {
                "Esta carpeta está vacía. Añade playlists desde su menú («Añadir a carpeta»)."
            } else {
                "Aquí aparecerá lo que guardes"
            };
            let g = ui.painter().layout_no_wrap(text.into(), theme::regular(14.0), style(ui.ctx()).sub);
            let at = pos2(ui.min_rect().min.x + GRID_LEFT, ui.cursor().min.y + 20.0);
            text_on_baseline(ui.painter(), at, g, style(ui.ctx()).sub);
            ui.add_space(40.0);
            return;
        }
        let pitch = 56.0;
        let full = ui.available_width();
        let (block, _) = ui.allocate_exact_size(vec2(full, n as f32 * pitch), Sense::hover());
        let clip = ui.clip_rect();
        let first = ((clip.top() - block.top()) / pitch).floor().max(0.0) as usize;
        let last = (((clip.bottom() - block.top()) / pitch).ceil().max(0.0) as usize).min(n);
        for (row, &item) in items.iter().enumerate().take(last).skip(first) {
            let rect = Rect::from_min_size(pos2(block.min.x + GRID_LEFT - 8.0, block.min.y + row as f32 * pitch), vec2(full - GRID_LEFT - GRID_RIGHT + 16.0, 56.0));
            let d = self.card_data(item);
            let kind = match item {
                Item::Liked => CardKind::Liked,
                Item::Album(_) => CardKind::Album,
                Item::Artist(_) => CardKind::Artist,
                _ => CardKind::Playlist,
            };
            let what = match item {
                Item::Liked | Item::Playlist(_) => "Playlist",
                Item::Folder(_) => "Carpeta",
                Item::Album(_) => "Álbum",
                Item::Artist(_) => "Artista",
            };
            let sub = if d.subtitle.is_empty() || matches!(item, Item::Liked) { what.to_string() } else { format!("{what} · {}", d.subtitle) };
            let mut c = keyed_child(ui, rect, ("lib_row", &d.key));
            let r = self.list_entry(&mut c, d.cover.as_deref(), kind, &d.title, &sub);
            if d.pinned {
                super::icons::paint_side(ui.painter(), pos2(rect.max.x - 24.0, rect.center().y), 16.0, PIN_ICON, Icon::Pin);
            }
            if r.clicked() {
                self.open_library_item(item);
            }
            self.library_item_menu(&r, item, &d.key, d.pinned);
        }
    }
}

/// Lo que hay a la izquierda de la tinta del primer glifo de `g` (su margen dentro del galley).
fn ink_left(g: &egui::Galley) -> f32 {
    g.rows
        .first()
        .and_then(|r| r.row.glyphs.first())
        .map(|gl| gl.uv_rect.offset.x.max(0.0) + gl.pos.x)
        .unwrap_or(0.0)
}

/// Lo que hay a la derecha de la tinta del último glifo de `g`.
fn ink_right(g: &egui::Galley) -> f32 {
    g.rows
        .first()
        .and_then(|r| r.row.glyphs.last().map(|gl| (r.row.size.x - (gl.pos.x + gl.uv_rect.offset.x + gl.uv_rect.size.x)).max(0.0)))
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claves_de_la_biblioteca() {
        assert_eq!(library_key("spotify:playlist:abc").as_deref(), Some("abc"));
        assert_eq!(library_key("spotify:album:x").as_deref(), Some("album:x"));
        assert_eq!(library_key("spotify:artist:y").as_deref(), Some("artist:y"));
        assert_eq!(library_key("spotify:user:pepe:collection").as_deref(), Some("liked"));
        assert_eq!(library_key("spotify:collection:tracks").as_deref(), Some("liked"));
        assert_eq!(library_key("spotify:track:z"), None);
    }

    #[test]
    fn numero_teñido_con_la_franja() {
        // Franja gris: gris claro sin tinte.
        assert_eq!(count_tint(Color32::from_gray(90)), Color32::from_gray(195));
        // Franja amarilla (Rock Mix de la referencia): amarillento claro.
        let c = count_tint(Color32::from_rgb(200, 231, 112));
        assert!(c.g() > c.b() && c.r() > c.b(), "{c:?}");
    }
}
