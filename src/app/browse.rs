//! Buscar sin consulta (referencia «Recent searches / Browse all», 18 oct 2026): las búsquedas
//! recientes de la cuenta y las categorías de Spotify agrupadas en Descubrir, Géneros, Estado de
//! ánimo y actividad, Entretenimiento, Pódcasts y Audiolibros, en filas que se pasan con flechas.
//! También la página de una categoría (`Page::Browse`).
//!
//! Medidas sacadas de la referencia (1671 × 972) a la escala de la página: × 1,064 (el
//! reproductor de la referencia encimado sobre el de las anteriores da la menor diferencia a esa
//! escala) y relativas al borde del panel, que en ella está en (263, 65).

use std::collections::HashMap;

use egui::{pos2, vec2, Color32, CornerRadius, Rect, Sense, Stroke};

use super::theme;
use super::widgets::{galley_truncated, text_on_baseline};
use super::{Action, App, Page};
use crate::api::Req;
use crate::i18n::t;
use crate::model::{BrowseCard, BrowseData, BrowseEntry};

// --- búsquedas recientes
const RS_TITLE_X: f32 = 26.2;
/// Los títulos de sección, 1 px más a la izquierda (en la referencia, «Discover» en 288 y
/// «Recent searches» en 289).
const SEC_X: f32 = 25.2;
const RS_TITLE_BASE: f32 = 41.0;
const TITLE_FONT: f32 = 20.0;
/// «…» de las recientes: tres puntos de 4,3 px cada 8 px, a 45,9 del borde derecho.
const DOTS_DX: f32 = 45.9;
const DOTS_Y: f32 = 33.5;
const DOT_R: f32 = 2.15;
const DOT_GAP: f32 = 7.98;
const RS_TOP: f32 = 69.2;
const RS_LEFT: f32 = 26.6;
const RS_PITCH: f32 = 199.0;
/// Los círculos (artistas) miden 169,5; las cuadradas, 171.
const RS_SIDE: f32 = 169.5;
const RS_SQUARE: f32 = 171.0;
/// Las cuadradas (álbum, canción, playlist…) empiezan 9,6 más abajo; encima del álbum asoma otro
/// (la «pila»): 6,4 de alto, metido 13,8 a la izquierda y 9,6 a la derecha.
const RS_SQUARE_DY: f32 = 9.6;
const STACK_DY: f32 = 1.06;
const STACK_H: f32 = 6.4;
const STACK_IN_L: f32 = 13.8;
const STACK_IN_R: f32 = 9.6;
/// Botón para quitar una: círculo gris 17 de 20 px de radio en la esquina de arriba a la derecha
/// y una cruz gris 137 de 11,7 px.
const RS_X_R: f32 = 19.95;
const RS_X_GLYPH: f32 = 11.7;
const RS_NAME_BASE: f32 = 200.6;
const RS_SQ_TITLE_BASE: f32 = 201.6;
const RS_SUB_BASE: f32 = 226.6;
const RS_NAME_FONT: f32 = 14.5;
const RS_SUB_FONT: f32 = 11.8;
/// El título de las cuadradas se corta a este ancho (en la referencia, «Black Holes…»).
const RS_SQ_TITLE_W: f32 = 98.0;

// --- explorar todo
const BROWSE_TITLE_BASE: f32 = 355.9;
/// Distancia de la línea base de «Explorar todo» a la del título de la primera sección y de esta a
/// sus tarjetas; las secciones, cada 245,8.
const SEC_TITLE_DY: f32 = 59.6;
const SEC_CARDS_DY: f32 = 28.2;
const SEC_PITCH: f32 = 245.8;
const SEC_FONT: f32 = 20.2;
const ROW_LEFT: f32 = 26.6;
const ROW_RIGHT: f32 = 25.5;
const CARD: f32 = 170.2;
const CARD_PITCH: f32 = 181.6;
const CARD_RADIUS: u8 = 6;
const CARD_TEXT_X: f32 = 18.5;
const CARD_TEXT_BASE: f32 = 39.2;
const CARD_LINE: f32 = 27.7;
const CARD_FONT: f32 = 19.3;
const CARD_TEXT_W: f32 = 130.0;
/// La portada girada: esquina de arriba a la izquierda, lado y giro (en grados, horario).
const ART_X: f32 = 111.2;
const ART_Y: f32 = 61.4;
const ART_SIDE: f32 = 101.1;
const ART_ANGLE: f32 = 30.9;
/// Flechas de cada sección: centros a 91,5 y 45,75 del borde derecho, 7,6 sobre la línea base del
/// título; 16 × 12,8, gris 137 (71 si no se puede ir hacia ese lado).
const ARROW_L_DX: f32 = 91.5;
const ARROW_R_DX: f32 = 45.75;
const ARROW_DY: f32 = -7.6;
const ARROW_W: f32 = 16.0;
const ARROW_HEAD: f32 = 6.4;
const DIM: Color32 = Color32::from_gray(137);
const DIMMER: Color32 = Color32::from_gray(71);

/// Secciones de «Explorar todo» y, por id de página, las categorías de cada una en orden (las de
/// la referencia primero). Lo que Spotify dé y no esté aquí va a Géneros (casi todo lo nuevo lo
/// es), salvo lo de dentro de Pódcasts y Audiolibros, que va a su sección.
pub(super) const SECTIONS: [&str; 6] = [tk!("Descubrir"), tk!("Géneros"), tk!("Estado de ánimo y actividad"), tk!("Entretenimiento"), tk!("Pódcasts"), tk!("Audiolibros")];
const GROUPS: [&[&str]; 6] = [
    &[
        "0JQ5DAt0tbjZptfcdMSKl3", // Especialmente para ti (Made For You)
        "0JQ5DAqbMKFz6FAsUtgAab", // Novedades (New Releases)
        "0JQ5DAqbMKFCOmagBNW1Np", // Spotify Classics
        "0JQ5DAudkNjCgYMM0TZXDw", // Listas de éxitos (Charts)
        "0JQ5DAqbMKFQIL0AXnG5AK", // Tendencia (Trending)
        "0JQ5DAtOnAEpjOgUKwXyxj", // Descubrir (Discover)
        "0JQ5DAqbMKFDBgllo2cUIN", // Spotify Singles
        "0JQ5DAqbMKFIVNxQgRNSg0", // Décadas (Decades)
        "0JQ5DAqbMKFImHYGo3eTSg", // Fresh Finds
        "0JQ5DAqbMKFOOxftoKZxod", // RADAR
        "0JQ5DAqbMKFPw634sFwguI", // EQUAL
        "0JQ5DAqbMKFGnsSfvg90Wo", // GLOW
        "0JQ5DAqbMKFBBFWTftkp01", // Spotify Sessions
        "0JQ5DAqbMKFPSyykKYdCTj", // Mezclada por
        "0JQ5DAqbMKFRKBHIxJ5hMm", // Creadores de tendencias
        "0JQ5DAqbMKFSi39LMRT0Cy", // Música
        "0JQ5DAozXW0GUBAKjHsifL", // Eventos en directo
        "0JQ5DAozXW0IW2igQ6L1aE", // Hometown
    ],
    &[
        "0JQ5DAqbMKFEC4WFtoNRpw", // Pop
        "0JQ5DAqbMKFKLfwjuJMoNC", // Country
        "0JQ5DAqbMKFQ00XGBls6ym", // Hip hop
        "0JQ5DAqbMKFDXXwE9BDJAr", // Rock
        "0JQ5DAqbMKFCWjUTdzaG0e", // Indie
        "0JQ5DAqbMKFAjfauKLOZiv", // Punk
        "0JQ5DAqbMKFDkd668ypn6O", // Metal
        "0JQ5DAqbMKFRieVZLLoo9m", // Instrumental
        "0JQ5DAqbMKFxXaXKP7zcDp", // Latina
        "0JQ5DAqbMKFDTEtSaS4R92", // Música Mexicana
        "0JQ5DAqbMKFBeja3pEmYhE", // Música de México
        "0JQ5DAqbMKFHOzuVTgTizF", // Dance y electrónica
        "0JQ5DAqbMKFEZPnFQSFB1T", // R&B
        "0JQ5DAqbMKFFtlLYUHv8bT", // Alternativa
        "0JQ5DAqbMKFR9D66iFXqzk", // Cumbia
        "0JQ5DAqbMKFQkIFo7vWoOd", // Salsa
        "0JQ5DAqbMKFJ67sfrngKm4", // La Tierra del Corrido
        "0JQ5DAqbMKFGvOw3O4nLAf", // K-pop
        "0JQ5DAqbMKFAJ5xb0fwo9m", // Jazz
        "0JQ5DAqbMKFPrEiAOxgac3", // Clásica
        "0JQ5DAqbMKFy78wprEpAjl", // Folk y acústica
        "0JQ5DAqbMKFQiK2EHwyjcU", // Blues
        "0JQ5DAqbMKFIpEuaCnimBj", // Soul
        "0JQ5DAqbMKFFsW9N8maB6z", // Funk
        "0JQ5DAqbMKFJKoGyUMo2hE", // Reggae
        "0JQ5DAqbMKFObNLOHydSW8", // Caribeña
        "0JQ5DAqbMKFGcCCKMatU5w", // Made in Colombia
        "0JQ5DAqbMKFNQ0fGp4byGU", // Afro
        "0JQ5DAqbMKFQFQN0rnK48G", // Flamenco
        "0JQ5DAqbMKFAH7OEQjCEex", // Francófona
        "0JQ5DAqbMKFI9pBj99rHhq", // Música japonesa
        "0JQ5DAqbMKFLjmiZRss79w", // Ambiente
        "0JQ5DAqbMKFSCjnQr8QZ3O", // Compositores/as
    ],
    &[
        "0JQ5DAqbMKFIRybaNTYXXy", // En el coche (In the car)
        "0JQ5DAqbMKFzHmL4tf05da", // Estado de ánimo (Mood)
        "0JQ5DAqbMKFAXlCG6QvYQ4", // Música para entrenar (Workout)
        "0JQ5DAqbMKFFzDl7qN9Apr", // Relax (Chill)
        "0JQ5DAqbMKFCuoRTxhYWow", // Para dormir (Sleep)
        "0JQ5DAqbMKFA6SOHvT3gck", // Fiesta (Party)
        "0JQ5DAqbMKFx0uLQR2okcc", // Para estar en casa (At Home)
        "0JQ5DAqbMKFCbimwdOYlsl", // Concentración (Focus)
        "0JQ5DAqbMKFJ6dHNHTv6Mx", // Fitness
        "0JQ5DAqbMKFAUsdyVjCQuL", // Amor
        "0JQ5DAqbMKFLb2EqgLtpjC", // Bienestar
        "0JQ5DAqbMKFRY5ok2pxXJ0", // Cocinar
        "0JQ5DAqbMKFAQy4HL4XU2D", // Viajes
        "0JQ5DAqbMKFJw7QLnM27p6", // Estudiantes
        "0JQ5DAqbMKFI3pNLtYMD9S", // Naturaleza y sonidos
        "0JQ5DAqbMKFNwFFRSG0HRv", // Karaoke
        "0JQ5DAqbMKFFoimhOqWzLB", // Niños y familia
    ],
    &[
        "0JQ5DAqbMKFEOEBCABAxo9", // Netflix
        "0JQ5DAqbMKFziKOShCi009", // Anime
        "0JQ5DAqbMKFHCxg5H5PtqW", // Disney
        "0JQ5DAqbMKFCfObibaOZbv", // Gaming
        "0JQ5DAqbMKFOzQeOmemkuw", // TV y películas
    ],
    &[
        "0JQ5DArNBzkmxXHCqFLx2J", // Pódcasts
        "0JQ5DAozXW0GUBAKjHsifF", // Éxitos en pódcasts (Podcast Charts)
        "0JQ5DAqbMKFAiDwp2jMWXj", // Podcast New Releases
        "0JQ5DAqbMKFEf03Ma7ZGy8", // Video Podcasts (Video)
        "0JQ5DAqbMKFNr6gDrHHVKL", // Humor
        "0JQ5DAqbMKFRNXsIvgZF9A", // Música + Charla
    ],
    &[
        "0JQ5DAqbMKFETqK4t8f1n3", // Audiolibros
        "0JQ5DAqbMKFFOa6zjY8eoV", // Ciencia ficción y fantasía (Sci-Fi & Fantasy)
        "0JQ5DAqbMKFN0lmhry1LfG", // Ficción y literatura (Fiction & Literature)
        "0JQ5DAqbMKFHF46W0MwaAA", // Misterio e intriga (Mystery & Thriller)
        "0JQ5DAqbMKFJ4DMqAKdPAs", // Autoayuda y mejora personal (Self-Help)
    ],
];

/// Las categorías de cada sección, sin repetir, en el orden de `GROUPS` y luego en el de Spotify.
pub(super) fn grouped(d: &BrowseData) -> Vec<Vec<BrowseCard>> {
    let mut out: Vec<Vec<BrowseCard>> = vec![Vec::new(); SECTIONS.len()];
    let mut seen = std::collections::HashSet::new();
    let all: Vec<(&BrowseCard, Option<usize>)> =
        d.cards.iter().map(|c| (c, None)).chain(d.podcasts.iter().map(|c| (c, Some(4)))).chain(d.audiobooks.iter().map(|c| (c, Some(5)))).collect();
    let by_id: HashMap<&str, &BrowseCard> = all.iter().map(|(c, _)| (c.id(), *c)).collect();
    for (g, ids) in GROUPS.iter().enumerate() {
        for id in *ids {
            if let Some(c) = by_id.get(id) {
                if seen.insert(c.id().to_string()) {
                    out[g].push((*c).clone());
                }
            }
        }
    }
    for (c, home) in all {
        if seen.insert(c.id().to_string()) {
            out[home.unwrap_or(1)].push(c.clone());
        }
    }
    out
}

/// La página a la que lleva una entrada (búsqueda reciente o elemento de una categoría).
pub(super) fn entry_page(e: &BrowseEntry) -> Option<Page> {
    let id = e.id().to_string();
    Some(match e.kind.as_str() {
        "artist" => Page::Artist(id),
        "album" => Page::Album(id),
        "playlist" => Page::Playlist(id),
        "track" => Page::Track(id),
        "show" | "audiobook" => Page::Show(id),
        "user" => Page::User(id),
        "category" => Page::Browse(e.uri.clone()),
        _ => return None,
    })
}

/// La uri de lo que abre una página (para guardarlo en las búsquedas recientes).
pub(super) fn page_uri(page: &Page) -> Option<String> {
    Some(match page {
        Page::Artist(id) => format!("spotify:artist:{id}"),
        Page::Album(id) => format!("spotify:album:{id}"),
        Page::Playlist(id) => format!("spotify:playlist:{id}"),
        Page::Track(id) => format!("spotify:track:{id}"),
        Page::Show(id) => format!("spotify:show:{id}"),
        Page::User(id) => format!("spotify:user:{id}"),
        _ => return None,
    })
}

/// Flecha ← o → de `ARROW_W` de largo centrada en `c`.
fn arrow(painter: &egui::Painter, c: egui::Pos2, right: bool, color: Color32) {
    let s = Stroke::new(1.7, color);
    let (tip, tail) = if right { (c.x + ARROW_W / 2.0, c.x - ARROW_W / 2.0) } else { (c.x - ARROW_W / 2.0, c.x + ARROW_W / 2.0) };
    let back = if right { -ARROW_HEAD } else { ARROW_HEAD };
    painter.line_segment([pos2(tail, c.y), pos2(tip, c.y)], s);
    painter.line_segment([pos2(tip, c.y), pos2(tip + back, c.y - ARROW_HEAD)], s);
    painter.line_segment([pos2(tip, c.y), pos2(tip + back, c.y + ARROW_HEAD)], s);
}

/// Copias en disco de las páginas de categoría: se muestran al instante y se renuevan si tienen
/// más de este tiempo.
const PAGE_FRESH_SECS: u64 = 6 * 3600;

impl App {
    /// Pide una página (clave y petición) una vez. Si falló, la vuelve a pedir sola a los 1,5 y
    /// 5 s; si sigue fallando, devuelve el error para enseñar «Reintentar» (antes se quedaba
    /// cargando para siempre).
    pub(super) fn request_page(&mut self, ctx: &egui::Context, key: &str, req: Req) -> Option<String> {
        if let Some((err, at, tries)) = self.page_errors.get(key).cloned() {
            if tries >= 3 {
                return Some(err);
            }
            let wait = if tries == 1 { 1.5 } else { 5.0 };
            let left = wait - at.elapsed().as_secs_f32();
            if left <= 0.0 {
                self.requested.remove(key);
                // El intento sigue contando: si vuelve a fallar, sube.
                self.request_once(key, req);
            } else {
                ctx.request_repaint_after(std::time::Duration::from_secs_f32(left + 0.05));
            }
            return None;
        }
        self.request_once(key, req);
        None
    }

    /// «No se pudo cargar…» con «Reintentar» (centrado arriba de `area`). Devuelve si se pulsó.
    pub(super) fn page_error(&mut self, ui: &mut egui::Ui, area: Rect, key: &str, err: &str) {
        let p = theme::palette(ui.ctx());
        let painter = ui.painter().clone();
        let top = area.min.y + 60.0;
        let g = painter.layout_no_wrap(t("No se pudo cargar").into(), theme::bold(20.0), p.text);
        let w = g.size().x;
        text_on_baseline(&painter, pos2(area.center().x - w / 2.0, top + 20.0), g, p.text);
        let g = galley_truncated(&painter, &crate::i18n::t_message(err), theme::regular(13.0), p.weak, area.width() - 80.0);
        let w = g.size().x;
        text_on_baseline(&painter, pos2(area.center().x - w / 2.0, top + 46.0), g, p.weak);
        let b = Rect::from_center_size(pos2(area.center().x, top + 84.0), vec2(130.0, 38.0));
        let r = ui.interact(b, ui.id().with(("retry", key)), Sense::click());
        painter.rect_filled(b, CornerRadius::same(19), if r.hovered() { Color32::WHITE } else { Color32::from_gray(235) });
        let g = painter.layout_no_wrap(t("Reintentar").into(), theme::bold(14.0), Color32::BLACK);
        let w = g.size().x;
        text_on_baseline(&painter, pos2(b.center().x - w / 2.0, b.center().y + 5.0), g, Color32::BLACK);
        if r.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if r.clicked() {
            self.page_errors.remove(key);
            self.requested.remove(key);
        }
    }

    fn browse_page_path(&self, uri: &str) -> std::path::PathBuf {
        self.browse_path.with_file_name("browse_pages").join(format!("{}.json", uri.rsplit(':').next().unwrap_or(uri)))
    }

    /// Guarda la página de una categoría (con la fecha, para saber si renovarla).
    pub(super) fn save_browse_page(&self, page: &crate::model::BrowsePage) {
        let path = self.browse_page_path(&page.uri);
        if let Ok(t) = serde_json::to_string(&(crate::cache::now_secs(), page)) {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            crate::cache::write_atomic(&path, &t);
        }
    }

    /// La copia en disco de una categoría, si la hay, y si sigue al día.
    fn load_browse_page(&self, uri: &str) -> Option<(crate::model::BrowsePage, bool)> {
        let t = std::fs::read_to_string(self.browse_page_path(uri)).ok()?;
        let (at, page): (u64, crate::model::BrowsePage) = serde_json::from_str(&t).ok()?;
        Some((page, crate::cache::now_secs().saturating_sub(at) < PAGE_FRESH_SECS))
    }

    /// Precarga la página de una categoría (al pasar el ratón por su tarjeta): al pulsarla ya
    /// está, o casi.
    fn prefetch_browse(&mut self, uri: &str) {
        if self.browse_pages.contains_key(uri) {
            return;
        }
        if let Some((page, fresh)) = self.load_browse_page(uri) {
            self.browse_pages.insert(uri.to_string(), page);
            if fresh {
                return;
            }
        }
        self.request_once(&format!("browse:{uri}"), Req::BrowsePage(uri.to_string()));
    }

    /// Pide lo que falte: las categorías (una vez por sesión, o si su copia tiene más de 12 h) y
    /// las búsquedas recientes (una vez por sesión y tras cada cambio).
    pub(super) fn browse_tick(&mut self) {
        let stale = self.browse.as_ref().is_none_or(|b| crate::cache::now_secs().saturating_sub(b.fetched_at) > 12 * 3600);
        if stale && !self.browse_asked {
            self.browse_asked = true;
            self.api.send(Req::BrowseAll);
        }
        if !self.recent_asked {
            self.recent_asked = true;
            self.api.send(Req::RecentSearches);
        }
    }

    /// Guarda `uri` en las búsquedas recientes de la cuenta (al frente) y la pone primera aquí.
    pub(super) fn remember_search(&mut self, entry: BrowseEntry) {
        if entry.kind == "category" {
            return;
        }
        let uri = entry.uri.clone();
        if let Some(r) = self.recent_searches.as_mut() {
            r.retain(|e| e.uri != uri);
            r.insert(0, entry);
        }
        self.api.send(Req::SaveRecentSearches(vec![uri]));
    }

    /// Guarda `uri` en las búsquedas recientes sin tener sus datos: la lista se vuelve a pedir la
    /// próxima vez que se vea.
    pub(super) fn remember_uri(&mut self, uri: String) {
        self.api.send(Req::SaveRecentSearches(vec![uri]));
        self.recent_asked = false;
    }

    fn forget_searches(&mut self, uris: Vec<String>) {
        if let Some(r) = self.recent_searches.as_mut() {
            r.retain(|e| !uris.contains(&e.uri));
        }
        self.api.send(Req::RemoveRecentSearches(uris));
    }

    /// Buscar sin consulta: recientes y «Explorar todo», en el panel negro, desplazable.
    pub(super) fn search_start(&mut self, ui: &mut egui::Ui, panel: Rect) {
        self.browse_tick();
        let recent: Vec<BrowseEntry> = self.recent_searches.clone().unwrap_or_default();
        let groups = self.browse.as_ref().map(grouped).unwrap_or_default();
        let sections: Vec<(usize, Vec<BrowseCard>)> = groups.into_iter().enumerate().filter(|(_, g)| !g.is_empty()).collect();
        // Sin recientes, «Explorar todo» sube a donde va su título.
        let shift = if recent.is_empty() { RS_TITLE_BASE - BROWSE_TITLE_BASE } else { 0.0 };
        let last_cards = BROWSE_TITLE_BASE + shift + SEC_TITLE_DY + SEC_CARDS_DY + SEC_PITCH * sections.len().saturating_sub(1) as f32;
        let total_h = last_cards + CARD + 40.0;

        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(panel));
        c.set_clip_rect(panel.shrink(1.0));
        let mut area = egui::ScrollArea::vertical().id_salt("search_start").auto_shrink([false, false]);
        if let Some(y) = self.search_start_scroll.take() {
            area = area.vertical_scroll_offset(y);
        }
        area.show(&mut c, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let (area, _) = ui.allocate_exact_size(vec2(panel.width(), total_h), Sense::hover());
            let o = pos2(panel.min.x.round(), area.min.y.round());
            let right = panel.max.x.round();
            let painter = ui.painter().clone();
            let strong = Color32::WHITE;

            if !recent.is_empty() {
                let g = painter.layout_no_wrap(t("Búsquedas recientes").into(), theme::bold(TITLE_FONT), strong);
                text_on_baseline(&painter, pos2(o.x + RS_TITLE_X, o.y + RS_TITLE_BASE), g, strong);
                self.recent_menu(ui, pos2(right - DOTS_DX, o.y + DOTS_Y));
                self.recent_row(ui, o, right, &recent);
            }

            let g = painter.layout_no_wrap(t("Explorar todo").into(), theme::bold(TITLE_FONT), strong);
            text_on_baseline(&painter, pos2(o.x + RS_TITLE_X, o.y + BROWSE_TITLE_BASE + shift), g, strong);
            if sections.is_empty() {
                let p = theme::palette(ui.ctx());
                let text = if self.browse_asked { t("Cargando categorías…") } else { t("Sin categorías") };
                let g = painter.layout_no_wrap(text.into(), theme::regular(14.0), p.weak);
                text_on_baseline(&painter, pos2(o.x + RS_TITLE_X, o.y + BROWSE_TITLE_BASE + shift + 40.0), g, p.weak);
            }
            for (k, (g, cards)) in sections.iter().enumerate() {
                let base = o.y + BROWSE_TITLE_BASE + shift + SEC_TITLE_DY + SEC_PITCH * k as f32;
                self.browse_row(ui, *g, cards, o.x, right, base);
            }
        });
    }

    /// «…» de las recientes: borrarlas todas.
    fn recent_menu(&mut self, ui: &mut egui::Ui, c: egui::Pos2) {
        let hit = Rect::from_center_size(c, vec2(36.0, 28.0));
        let r = ui.interact(hit, ui.id().with("recent_dots"), Sense::click());
        let color = if r.hovered() { Color32::WHITE } else { DIM };
        for k in -1..=1 {
            ui.painter().circle_filled(pos2(c.x + k as f32 * DOT_GAP, c.y), DOT_R, color);
        }
        if r.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let all: Vec<String> = self.recent_searches.iter().flatten().map(|e| e.uri.clone()).collect();
        let mut clear = false;
        egui::Popup::menu(&r).show(|ui| {
            if Self::menu_item(ui, Some(super::icons::Icon::Trash), t("Borrar búsquedas recientes"), false).clicked() {
                clear = true;
                ui.close();
            }
        });
        if clear && !all.is_empty() {
            self.forget_searches(all);
        }
    }

    /// Fila de búsquedas recientes: artistas en círculo con el nombre centrado; lo demás cuadrado
    /// (los álbumes con otro asomando detrás), con título y subtítulo; cada una con su «×».
    fn recent_row(&mut self, ui: &mut egui::Ui, o: egui::Pos2, right: f32, recent: &[BrowseEntry]) {
        let row = Rect::from_min_max(pos2(o.x + RS_LEFT, o.y + RS_TOP), pos2(right - ROW_RIGHT, o.y + RS_TOP + RS_SUB_BASE + 8.0));
        let clip = row.intersect(ui.clip_rect());
        let painter = ui.painter().with_clip_rect(clip);
        let p = theme::palette(ui.ctx());
        let mut open: Option<BrowseEntry> = None;
        let mut remove: Option<String> = None;
        for (k, e) in recent.iter().enumerate() {
            let x = row.min.x + RS_PITCH * k as f32;
            if x > row.max.x {
                break;
            }
            let round = e.kind == "artist" || e.kind == "user";
            let side = if round { RS_SIDE } else { RS_SQUARE };
            let card = Rect::from_min_size(pos2(x, row.min.y), vec2(side, side + RS_SQUARE_DY));
            let id = ui.id().with(("recent", &e.uri));
            let hit = ui.interact(card.intersect(clip), id, Sense::click());
            if round {
                let img = Rect::from_min_size(card.min, vec2(RS_SIDE, RS_SIDE));
                let mut c = ui.new_child(egui::UiBuilder::new().max_rect(img));
                c.set_clip_rect(clip);
                self.cover_in(&mut c, e.image.as_deref(), img, (RS_SIDE / 2.0) as u8);
                let g = galley_truncated(&painter, &e.title, theme::regular(RS_NAME_FONT), strong_text(hit.hovered()), RS_SIDE);
                let w = g.size().x;
                text_on_baseline(&painter, pos2(img.center().x - w / 2.0, card.min.y + RS_NAME_BASE), g, strong_text(hit.hovered()));
            } else {
                let img = Rect::from_min_size(pos2(card.min.x, card.min.y + RS_SQUARE_DY), vec2(RS_SQUARE, RS_SQUARE));
                if e.kind == "album" {
                    let stack = Rect::from_min_max(pos2(img.min.x + STACK_IN_L, card.min.y + STACK_DY), pos2(img.max.x - STACK_IN_R, card.min.y + STACK_DY + STACK_H + 4.0));
                    painter.rect_filled(stack, CornerRadius::same(3), STACK);
                }
                let mut c = ui.new_child(egui::UiBuilder::new().max_rect(img));
                c.set_clip_rect(clip);
                self.cover_in(&mut c, e.image.as_deref(), img, 6);
                let g = galley_truncated(&painter, &e.title, theme::semibold(RS_NAME_FONT), strong_text(hit.hovered()), RS_SQ_TITLE_W);
                text_on_baseline(&painter, pos2(img.min.x - 0.6, card.min.y + RS_SQ_TITLE_BASE), g, strong_text(hit.hovered()));
                if !e.subtitle.is_empty() {
                    let g = galley_truncated(&painter, &e.subtitle, theme::regular(RS_SUB_FONT), p.weak, RS_SIDE);
                    text_on_baseline(&painter, pos2(img.min.x - 0.6, card.min.y + RS_SUB_BASE - 0.7), g, p.weak);
                }
            }
            // «×» en la esquina de arriba a la derecha (de la caja entera, con la pila).
            let xc = pos2(card.max.x - RS_X_R, card.min.y + RS_X_R);
            let xr = ui.interact(Rect::from_center_size(xc, vec2(RS_X_R * 2.0, RS_X_R * 2.0)).intersect(clip), id.with("x"), Sense::click());
            painter.circle_filled(xc, RS_X_R, if xr.hovered() { Color32::from_gray(40) } else { Color32::from_gray(17) });
            let h = RS_X_GLYPH / 2.0;
            let xcol = if xr.hovered() { Color32::WHITE } else { DIM };
            painter.line_segment([pos2(xc.x - h, xc.y - h), pos2(xc.x + h, xc.y + h)], Stroke::new(1.6, xcol));
            painter.line_segment([pos2(xc.x - h, xc.y + h), pos2(xc.x + h, xc.y - h)], Stroke::new(1.6, xcol));
            if hit.hovered() || xr.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if xr.on_hover_text(t("Quitar de las búsquedas recientes")).clicked() {
                remove = Some(e.uri.clone());
            } else if hit.clicked() {
                open = Some(e.clone());
            }
        }
        if let Some(uri) = remove {
            self.forget_searches(vec![uri]);
        } else if let Some(e) = open {
            if let Some(page) = entry_page(&e) {
                self.remember_search(e);
                self.actions.push(Action::Go(page));
            }
        }
    }

    /// Una sección de «Explorar todo»: título, flechas y su fila de tarjetas.
    fn browse_row(&mut self, ui: &mut egui::Ui, g: usize, cards: &[BrowseCard], left: f32, right: f32, base: f32) {
        let painter = ui.painter().clone();
        let g_title = painter.layout_no_wrap(t(SECTIONS[g]).into(), theme::semibold(SEC_FONT), Color32::WHITE);
        text_on_baseline(&painter, pos2(left + SEC_X, base), g_title, Color32::WHITE);
        let row = Rect::from_min_max(pos2(left + ROW_LEFT, base + SEC_CARDS_DY), pos2(right - ROW_RIGHT, base + SEC_CARDS_DY + CARD));
        let visible = (((row.width() + CARD_PITCH - CARD) / CARD_PITCH) + 0.01).floor().max(1.0) as usize;
        let max_first = cards.len().saturating_sub(visible) as f32;
        let target = self.browse_offset.get(&(g as u8)).copied().unwrap_or(0.0).clamp(0.0, max_first);
        let first = ui.ctx().animate_value_with_time(ui.id().with(("browse_off", g)), target, 0.3);
        // Flechas.
        let ay = base + ARROW_DY;
        for (dx, dir) in [(ARROW_L_DX, -1.0f32), (ARROW_R_DX, 1.0)] {
            let c = pos2(right - dx, ay);
            let can = if dir < 0.0 { target > 0.0 } else { target < max_first };
            let r = ui.interact(Rect::from_center_size(c, vec2(30.0, 28.0)), ui.id().with(("browse_arrow", g, dir as i8)), if can { Sense::click() } else { Sense::hover() });
            let color = if !can { DIMMER } else if r.hovered() { Color32::WHITE } else { DIM };
            arrow(&painter, c, dir > 0.0, color);
            if can && r.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if can && r.clicked() {
                let next = (target + dir * visible as f32).clamp(0.0, max_first);
                self.browse_offset.insert(g as u8, next);
            }
        }
        // Tarjetas (también se pasan con la rueda en horizontal o con Mayús + rueda).
        let clip = row.intersect(ui.clip_rect());
        if ui.rect_contains_pointer(clip) {
            let d = ui.input(|i| i.smooth_scroll_delta.x);
            if d.abs() > 0.5 {
                let next = (target - d / CARD_PITCH).clamp(0.0, max_first);
                self.browse_offset.insert(g as u8, next);
                ui.ctx().request_repaint();
            }
        }
        let mut open: Option<BrowseCard> = None;
        for (k, card) in cards.iter().enumerate() {
            let x = row.min.x + (k as f32 - first) * CARD_PITCH;
            if x + CARD < row.min.x - 1.0 || x > row.max.x + 1.0 {
                continue;
            }
            let rect = Rect::from_min_size(pos2(x.round(), row.min.y.round()), vec2(CARD, CARD));
            let r = self.browse_card(ui, rect, clip, card, ("browse", g, k));
            // Precarga tras un cuarto de segundo encima (no al cruzar la fila con el ratón).
            if r.hovered() && card.feature.is_none() {
                match &self.browse_hover {
                    Some((u, at)) if *u == card.uri => {
                        if at.elapsed() >= std::time::Duration::from_millis(250) {
                            self.prefetch_browse(&card.uri);
                        } else {
                            ui.ctx().request_repaint_after(std::time::Duration::from_millis(260));
                        }
                    }
                    _ => self.browse_hover = Some((card.uri.clone(), std::time::Instant::now())),
                }
            }
            if r.clicked() {
                open = Some(card.clone());
            }
        }
        if let Some(c) = open {
            self.open_browse_card(&c);
        }
    }

    pub(super) fn open_browse_card(&mut self, c: &BrowseCard) {
        match c.feature.as_deref() {
            // Especiales que Nanofy aún no tiene (listas de pódcasts, conciertos…).
            Some(f) if !f.starts_with("spotify:page:") => self.status(tr!("«{}» aún no está en Nanofy", c.title)),
            _ => self.actions.push(Action::Go(Page::Browse(c.uri.clone()))),
        }
    }

    /// Una tarjeta de categoría: color de fondo, título arriba a la izquierda (hasta dos líneas) y
    /// la portada girada asomando por la esquina de abajo a la derecha.
    pub(super) fn browse_card(&mut self, ui: &mut egui::Ui, rect: Rect, clip: Rect, card: &BrowseCard, id: impl std::hash::Hash + std::fmt::Debug) -> egui::Response {
        let r = ui.interact(rect.intersect(clip), ui.id().with(id), Sense::click());
        let painter = ui.painter().with_clip_rect(clip);
        let [cr, cg, cb] = card.color;
        let mut bg = Color32::from_rgb(cr, cg, cb);
        if r.hovered() {
            bg = bg.lerp_to_gamma(Color32::WHITE, 0.06);
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        painter.rect_filled(rect, CornerRadius::same(CARD_RADIUS), bg);
        // Portada girada, recortada a la tarjeta.
        if let Some(url) = card.image.as_deref() {
            let ppp = ui.ctx().pixels_per_point();
            if let Some(tex) = self.images.texture(url, ((ART_SIDE * ppp) as u32).clamp(16, 512)) {
                let card_clip = rect.intersect(clip);
                let p2 = ui.painter().with_clip_rect(card_clip);
                let a = ART_ANGLE.to_radians();
                let (s, c) = a.sin_cos();
                let tl = pos2(rect.min.x + ART_X, rect.min.y + ART_Y);
                let ux = vec2(c, s) * ART_SIDE;
                let uy = vec2(-s, c) * ART_SIDE;
                let mut mesh = egui::Mesh::with_texture(tex.id);
                let corners = [(tl, pos2(0.0, 0.0)), (tl + ux, pos2(1.0, 0.0)), (tl + ux + uy, pos2(1.0, 1.0)), (tl + uy, pos2(0.0, 1.0))];
                for (p, uv) in corners {
                    mesh.vertices.push(egui::epaint::Vertex { pos: p, uv, color: Color32::WHITE });
                }
                mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
                p2.add(egui::Shape::mesh(mesh));
                // La esquina redondeada de abajo a la derecha, por donde pasa la portada.
                corner_mask(&p2, rect, CARD_RADIUS as f32, theme::palette(ui.ctx()).bg);
            }
        }
        // Título: hasta tres líneas de CARD_TEXT_W.
        let lines = wrap_lines(&painter, &card.title, theme::bold(CARD_FONT), CARD_TEXT_W);
        for (i, line) in lines.into_iter().enumerate() {
            let g = painter.layout_no_wrap(line, theme::bold(CARD_FONT), Color32::WHITE);
            text_on_baseline(&painter, pos2(rect.min.x + CARD_TEXT_X, rect.min.y + CARD_TEXT_BASE + CARD_LINE * i as f32), g, Color32::WHITE);
        }
        r
    }

    /// La página de una categoría: su título sobre su color y sus secciones.
    pub(super) fn browse_page(&mut self, ui: &mut egui::Ui, uri: String) {
        let key = format!("browse:{uri}");
        if !self.browse_pages.contains_key(&uri) {
            // La copia en disco al instante; si no está al día, se renueva detrás.
            match self.load_browse_page(&uri) {
                Some((page, fresh)) => {
                    self.browse_pages.insert(uri.clone(), page);
                    if !fresh {
                        self.request_once(&key, Req::BrowsePage(uri.clone()));
                    }
                }
                None => {
                    let ctx = ui.ctx().clone();
                    if let Some(err) = self.request_page(&ctx, &key, Req::BrowsePage(uri.clone())) {
                        let area = ui.max_rect();
                        self.page_error(ui, area, &key, &err);
                        return;
                    }
                }
            }
        }
        let Some(page) = self.browse_pages.get(&uri).cloned() else {
            Self::loading(ui, t("Cargando"));
            return;
        };
        let p = theme::palette(ui.ctx());
        let color = page.color.map(|c| Color32::from_rgb(c[0], c[1], c[2])).unwrap_or(p.card2);
        let w = ui.available_width();
        let (head, _) = ui.allocate_exact_size(vec2(w, 150.0), Sense::hover());
        ui.painter().rect_filled(head, CornerRadius::same(8), color);
        let g = ui.painter().layout_no_wrap(page.title.clone(), theme::bold(54.0), Color32::WHITE);
        text_on_baseline(ui.painter(), pos2(head.min.x + 28.0, head.max.y - 30.0), g, Color32::WHITE);
        ui.add_space(12.0);
        for (k, (title, items)) in page.sections.iter().enumerate() {
            if !title.is_empty() {
                Self::section_header(ui, title, None);
            }
            let cats: Vec<BrowseCard> = items.iter().filter_map(|e| e.card()).collect();
            if !cats.is_empty() {
                // Categorías: tarjetas de color en filas.
                let per_row = ((ui.available_width() + CARD_PITCH - CARD) / CARD_PITCH).floor().max(1.0) as usize;
                let mut open = None;
                for chunk in cats.chunks(per_row) {
                    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), CARD + 12.0), Sense::hover());
                    for (i, card) in chunk.iter().enumerate() {
                        let rect = Rect::from_min_size(pos2(row.min.x + CARD_PITCH * i as f32, row.min.y), vec2(CARD, CARD));
                        if self.browse_card(ui, rect, ui.clip_rect(), card, ("bp", k, &card.uri)).clicked() {
                            open = Some(card.clone());
                        }
                    }
                }
                if let Some(c) = open {
                    self.open_browse_card(&c);
                }
                continue;
            }
            let mut go = None;
            Self::cards_row(ui, &format!("bp_{uri}_{k}"), |ui| {
                for e in items {
                    let kind = if e.kind == "artist" { super::widgets::CardKind::Artist } else { super::widgets::CardKind::Album };
                    let r = self.card(ui, super::widgets::CardInfo { kind, cover: e.image.as_deref(), title: &e.title, subtitle: &e.subtitle, count: None, pinned: false });
                    if r.clicked() {
                        go = entry_page(e);
                    }
                }
            });
            if let Some(page) = go {
                self.actions.push(Action::Go(page));
            }
        }
        ui.add_space(24.0);
    }
}

fn strong_text(hovered: bool) -> Color32 {
    if hovered {
        Color32::WHITE
    } else {
        Color32::from_gray(235)
    }
}

/// La pila del álbum: el gris azulado de la referencia (33·40·57).
const STACK: Color32 = Color32::from_rgb(33, 40, 57);

/// Tapa con el fondo lo que queda fuera de la esquina redondeada de abajo a la derecha de `r`.
fn corner_mask(painter: &egui::Painter, r: Rect, radius: f32, bg: Color32) {
    let c = pos2(r.max.x - radius, r.max.y - radius);
    let mut pts = vec![pos2(r.max.x, r.max.y)];
    let n = 8;
    for i in 0..=n {
        let t = std::f32::consts::FRAC_PI_2 * i as f32 / n as f32;
        pts.push(pos2(c.x + radius * t.sin(), c.y + radius * t.cos()));
    }
    // Abanico desde la esquina: cada trozo es convexo.
    for w in pts[1..].windows(2) {
        painter.add(egui::Shape::convex_polygon(vec![pts[0], w[0], w[1]], bg, Stroke::NONE));
    }
}

/// Parte `text` en líneas de `max_w` por palabras, hasta tres (la última se corta con «…» si
/// no cabe): los nombres en español son más largos que los de la referencia («Ciencia ficción y
/// fantasía»).
fn wrap_lines(painter: &egui::Painter, text: &str, font: egui::FontId, max_w: f32) -> Vec<String> {
    let width = |s: &str| painter.layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE).size().x;
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut lines: Vec<String> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        if lines.len() == 2 {
            let rest = words[i..].join(" ");
            lines.push(galley_truncated(painter, &rest, font.clone(), Color32::WHITE, max_w).text().to_string());
            break;
        }
        let mut line = words[i].to_string();
        i += 1;
        while i < words.len() {
            let cand = format!("{line} {}", words[i]);
            if width(&cand) > max_w {
                break;
            }
            line = cand;
            i += 1;
        }
        lines.push(line);
    }
    lines
}
