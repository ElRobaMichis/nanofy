//! Ajustes, con el diseño del mockup del usuario (oct 2026): título, pestañas como las del
//! artista (Reproducción, Dispositivo, Apariencia, Cuenta, Acerca de) que se quedan arriba al
//! bajar, una lupa para buscar entre todos y, en cada pestaña, sus secciones: el título en
//! negrita y una tarjeta de filas con su icono, nombre, explicación y el control a la derecha.
//! Los controles son los de la app: el interruptor, los chips del inicio, la línea del
//! reproductor (la bolita sale al acercar el ratón) y el menú de cristal para los desplegables.
//! Todo se aplica al instante: ya no hay «Guardar».
//!
//! Medidas del mockup (1.png, el panel empieza en x 6): todo relativo al borde del panel.

use std::collections::HashMap;
use std::sync::mpsc;

use egui::text::LayoutJob;
use egui::{pos2, vec2, Align, Color32, CornerRadius, FontId, Layout, Pos2, Rect, Sense, Stroke, TextureHandle};

use super::artist::{SEPARATOR, TAB_FONT, TAB_GAP, TAB_HOVER, TAB_ICON, TAB_SELECTED, TAB_TEXT};
use super::icons::{self, Icon};
use super::panels::{about_view, Tone, UpdAction};
use super::theme::{self, Palette, GREEN};
use super::widgets::{child_in, galley_truncated, text_on_baseline};
use super::{fmt_thousands, Action, App, Auth, Page};
use crate::config::{Loudness, Quality, Settings, Theme};
use crate::i18n::t;
use crate::model::SOURCE_RATE;

// ------------------------------------------------------------ medidas

/// Margen izquierdo del título, las pestañas y las tarjetas.
const PAD_X: f32 = 37.0;
/// Título «Ajustes»: tamaño y línea base; lo que sube antes de que las pestañas se queden fijas.
const TITLE_FONT: f32 = 36.0;
const TITLE_BASE: f32 = 72.0;
const HEAD_H: f32 = 96.0;
/// Fila de pestañas: alto (el filo va abajo) y línea base. Letra, separación y colores son los
/// de las pestañas del artista (`artist.rs`): las mismas pestañas en toda la app.
const TABS_H: f32 = 57.0;
const TAB_BASE: f32 = 34.0;
/// La lupa, desde el borde derecho del panel.
const SEARCH_FROM_RIGHT: f32 = 59.0;
/// Tarjetas: ancho máximo y esquinas.
const CARD_W: f32 = 880.0;
const CARD_R: u8 = 8;
/// Del filo de las pestañas (o del pie de una tarjeta) a la línea base del título de sección, y
/// de ahí a la tarjeta; sin título, del filo a la tarjeta.
const SECTION_GAP: f32 = 46.0;
const HEAD_TO_CARD: f32 = 17.0;
const BARE_GAP: f32 = 30.0;
const SECTION_FONT: f32 = 16.5;
/// Filas: alto con explicación de una línea, sin explicación y lo que suma cada línea más.
const ROW_H: f32 = 69.0;
const ROW_SHORT: f32 = 60.0;
const LINE: f32 = 18.0;
/// Icono (centro y lado), texto y margen derecho de los controles, desde el borde de la tarjeta.
const ICON_CX: f32 = 31.0;
const ICON_PX: f32 = 22.0;
const TEXT_X: f32 = 58.0;
const RIGHT: f32 = 20.0;
const NAME_FONT: f32 = 15.25;
const SUB_FONT: f32 = 13.25;
/// Ancho máximo de la explicación (el mockup la corta hacia los 420 px).
const SUB_MAX_W: f32 = 430.0;
/// Controles: desplegable, campo, deslizador (más su valor) y botones.
const DROP_W: f32 = 196.0;
const CTRL_H: f32 = 36.0;
const FIELD_W: f32 = 240.0;
const SLIDER_W: f32 = 210.0;
const SLIDER_VALUE_W: f32 = 74.0;
const BTN_H: f32 = 34.0;
/// Tarjeta de la cuenta y de «Acerca de»: alto, foto o logo, nombre y botón.
const HERO_H: f32 = 108.0;
const HERO_PIC: f32 = 64.0;
/// Miniaturas del tema (122 × 80, 14 entre ellas) y alto de su fila.
const THEME_W: f32 = 122.0;
const THEME_H: f32 = 80.0;
const THEME_GAP: f32 = 14.0;
const THEME_ROW_H: f32 = 130.0;
/// Caché de audio: hasta 4 GB, de 128 en 128 MB.
const CACHE_MAX_MB: u64 = 4096;
const CACHE_STEP_MB: u64 = 128;
/// Escala de la interfaz.
const ZOOM_MIN: f32 = 0.8;
const ZOOM_MAX: f32 = 1.6;

pub const TABS: [&str; 5] = [tk!("Reproducción"), tk!("Dispositivo"), tk!("Apariencia"), tk!("Cuenta"), tk!("Acerca de")];

// ------------------------------------------------------------ colores

/// Colores: los del mockup en oscuro (sobre el panel negro); en claro, los de la paleta.
#[derive(Clone, Copy)]
struct Ink {
    dark: bool,
    title: Color32,
    row: Color32,
    sub: Color32,
    icon: Color32,
    card: Color32,
    divider: Color32,
    control: Color32,
    control_hover: Color32,
    control_text: Color32,
    field: Color32,
    field_edge: Color32,
    track: Color32,
    fill: Color32,
    tab_off: Color32,
    tab_hover: Color32,
    tab_on: Color32,
    tab_icon: Color32,
    separator: Color32,
    warn: Color32,
    row_hover: Color32,
}

fn ink(p: &Palette) -> Ink {
    if p.dark {
        Ink {
            dark: true,
            title: Color32::from_gray(242),
            row: Color32::from_gray(230),
            sub: Color32::from_gray(140),
            icon: Color32::from_gray(143),
            card: Color32::from_gray(26),
            divider: Color32::from_gray(38),
            control: Color32::from_gray(42),
            control_hover: Color32::from_gray(54),
            control_text: Color32::from_gray(230),
            field: Color32::from_gray(17),
            field_edge: Color32::from_gray(44),
            track: Color32::from_gray(59),
            fill: Color32::from_rgb(220, 224, 231),
            tab_off: TAB_TEXT,
            tab_hover: TAB_HOVER,
            tab_on: TAB_SELECTED,
            tab_icon: TAB_ICON,
            separator: SEPARATOR,
            warn: p.warn,
            row_hover: Color32::from_white_alpha(6),
        }
    } else {
        Ink {
            dark: false,
            title: p.text,
            row: p.text,
            sub: p.weak,
            icon: p.weak,
            card: p.card2,
            divider: p.border,
            control: p.hover,
            control_hover: p.hover.lerp_to_gamma(p.text, 0.08),
            control_text: p.text,
            field: p.card,
            field_edge: p.border,
            track: p.weak.gamma_multiply(0.35),
            fill: p.text,
            tab_off: p.weak,
            tab_hover: p.text.lerp_to_gamma(p.weak, 0.4),
            tab_on: p.text,
            tab_icon: p.weak,
            separator: p.border,
            warn: p.warn,
            row_hover: Color32::from_black_alpha(8),
        }
    }
}

/// Color atenuado de una fila que no se puede tocar (la opción de la que depende está apagada).
fn dim(c: Color32, on: bool) -> Color32 {
    if on {
        c
    } else {
        c.gamma_multiply(0.42)
    }
}

// ------------------------------------------------------------ filas

/// Una fila de Ajustes. Las mismas sirven para las pestañas y para la búsqueda.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Row {
    Quality,
    Normalize,
    Loudness,
    Output,
    Gapless,
    Crossfade,
    CrossfadeSecs,
    CrossfadeAlbums,
    Autoplay,
    DeviceName,
    MediaKeys,
    Preload,
    Fps,
    AudioCache,
    ImageCache,
    Language,
    Theme,
    Zoom,
    Profile,
    Library,
    DevApp,
    AppInfo,
    UpdateCheck,
    UpdateAuto,
    Shortcuts,
    TechInfo,
}

/// Una sección: pestaña, título (vacío: sin título) y sus filas.
struct Section {
    tab: u8,
    title: &'static str,
    rows: &'static [Row],
}

const SECTIONS: &[Section] = &[
    Section { tab: 0, title: tk!("Sonido"), rows: &[Row::Quality, Row::Normalize, Row::Loudness, Row::Output] },
    Section {
        tab: 0,
        title: tk!("Transiciones"),
        rows: &[Row::Gapless, Row::Crossfade, Row::CrossfadeSecs, Row::CrossfadeAlbums, Row::Autoplay],
    },
    Section { tab: 1, title: tk!("Este equipo"), rows: &[Row::DeviceName, Row::MediaKeys, Row::Preload, Row::Fps] },
    Section { tab: 1, title: tk!("Almacenamiento"), rows: &[Row::AudioCache, Row::ImageCache] },
    Section { tab: 2, title: "", rows: &[Row::Language, Row::Theme, Row::Zoom] },
    Section { tab: 3, title: "", rows: &[Row::Profile] },
    Section { tab: 3, title: tk!("Biblioteca de Spotify"), rows: &[Row::Library, Row::DevApp] },
    Section { tab: 4, title: "", rows: &[Row::AppInfo] },
    Section { tab: 4, title: tk!("Actualizaciones"), rows: &[Row::UpdateCheck, Row::UpdateAuto] },
    Section { tab: 4, title: tk!("Más"), rows: &[Row::Shortcuts, Row::TechInfo] },
];

impl Row {
    fn icon(self) -> Option<Icon> {
        Some(match self {
            Row::Quality => Icon::Waveform,
            Row::Normalize => Icon::Equalizer,
            Row::Output => Icon::Volume,
            Row::Gapless => Icon::Infinity,
            Row::Crossfade => Icon::Crossfade,
            Row::Autoplay => Icon::Radio,
            Row::DeviceName => Icon::Computer,
            Row::MediaKeys => Icon::Keyboard,
            Row::Preload => Icon::Bolt,
            Row::Fps => Icon::Gauge,
            Row::AudioCache => Icon::Drive,
            Row::ImageCache => Icon::Image,
            Row::Language => Icon::Globe,
            Row::Theme => Icon::Contrast,
            Row::Zoom => Icon::TextSize,
            Row::Library => Icon::Cloud,
            Row::DevApp => Icon::Code,
            Row::UpdateCheck => Icon::Refresh,
            Row::Shortcuts => Icon::Keyboard,
            Row::TechInfo => Icon::Chip,
            Row::Loudness | Row::CrossfadeSecs | Row::CrossfadeAlbums | Row::UpdateAuto | Row::Profile | Row::AppInfo => return None,
        })
    }

    /// Palabras de más para la búsqueda (las técnicas, que no se traducen).
    fn keywords(self) -> &'static str {
        match self {
            Row::Quality => "kbps ogg vorbis flac lossless 320 160 96",
            Row::Normalize | Row::Loudness => "lufs loudness volume volumen",
            Row::Output => "khz hz resampling remuestreo device",
            Row::Gapless => "gapless",
            Row::Crossfade | Row::CrossfadeSecs | Row::CrossfadeAlbums => "crossfade",
            Row::Autoplay => "autoplay radio",
            Row::DeviceName => "spotify connect jam",
            Row::MediaKeys => "smtc mpris media",
            Row::Preload => "preload",
            Row::Fps => "fps hz",
            Row::AudioCache => "cache mb gb",
            Row::ImageCache => "cache",
            Row::Language => "idioma language english español",
            Row::Theme => "theme dark light",
            Row::Zoom => "zoom",
            Row::Profile => "premium logout",
            Row::Library => "web api",
            Row::DevApp => "client id developer redirect uri",
            Row::AppInfo => "version update github",
            Row::UpdateCheck | Row::UpdateAuto => "update github",
            Row::Shortcuts => "shortcuts",
            Row::TechInfo => "ram memory",
        }
    }
}

/// Minúsculas y sin tildes, para buscar «reproduccion» y encontrar «Reproducción».
fn fold(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' | 'ã' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' | 'õ' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            c => c,
        })
        .collect()
}

// ------------------------------------------------------------ estado

/// Desplegables.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Drop {
    Quality,
    Language,
}

/// Deslizadores: lo que se ve mientras se arrastra (se aplica al soltar).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Drag {
    Zoom(f32),
    Cache(u64),
}

/// Estado de la página de Ajustes.
#[derive(Default)]
pub struct SettingsUi {
    pub tab: u8,
    pub search: String,
    pub search_open: bool,
    search_focus: bool,
    /// Desplazamiento del fotograma anterior: con él se decide si las pestañas van fijas arriba.
    offset: f32,
    /// Desplazamiento pedido (al cambiar de pestaña o desde el modo de control).
    pub scroll_to: Option<f32>,
    pub dropdown: Option<Drop>,
    /// Dónde está cada desplegable (para abrir su menú justo debajo).
    drop_rects: HashMap<Drop, Rect>,
    pub expanded: Vec<Row>,
    /// El nombre del dispositivo mientras se escribe (se aplica al pulsar Intro o salir).
    name_edit: Option<String>,
    /// El Client ID de la app propia mientras se escribe.
    client_id_edit: Option<String>,
    drag: Option<Drag>,
    /// Caché de imágenes en disco (portadas, bytes) y su cálculo en marcha.
    image_usage: Option<(u64, u64)>,
    image_usage_rx: Option<mpsc::Receiver<(u64, u64)>>,
    /// Dispositivo de salida predeterminado (antes de que suene nada) y su consulta en marcha.
    output_probe: Option<(String, u32)>,
    output_rx: Option<mpsc::Receiver<Option<(String, u32)>>>,
    output_at: Option<std::time::Instant>,
    logo: Option<(u32, TextureHandle)>,
    previews: HashMap<(u8, u32), TextureHandle>,
    /// Dónde quedó cada cosa en el último fotograma (para las pruebas: clics de verdad).
    pub rects: HashMap<String, Rect>,
}

impl SettingsUi {
    /// Abre la pestaña `tab`, sin búsqueda; el desplazamiento vuelve arriba (las pestañas siguen
    /// fijas si lo estaban).
    pub fn open_tab(&mut self, tab: u8) {
        self.tab = tab.min(TABS.len() as u8 - 1);
        self.search.clear();
        self.search_open = false;
        self.dropdown = None;
        self.scroll_to = Some(self.offset.min(HEAD_H));
    }

    pub fn stuck(&self) -> bool {
        self.offset > HEAD_H
    }

    pub fn offset(&self) -> f32 {
        self.offset
    }
}

/// Lo que se hizo en una fila (se aplica después de dibujarla).
enum Event {
    Toggle,
    Choose(usize),
    OpenDrop(Drop),
    SliderMove(f32),
    SliderDone(f32),
    Commit,
    Button(u8),
    Expand,
}

/// Lo que cabe del control a la derecha de una fila.
#[derive(Clone)]
enum Ctl {
    None,
    Toggle(bool),
    Chips(Vec<String>, usize),
    Drop(Drop, String),
    Slider { frac: f32, value: String },
    Field,
    /// Botones de derecha a izquierda: (texto, principal).
    Buttons(Vec<(String, bool)>),
    Output,
    Chevron(bool),
    Theme,
}

/// Lo que se dibuja de una fila.
struct View {
    title: String,
    sub: Option<String>,
    /// Aviso en ámbar tras la explicación.
    warn: Option<String>,
    ctl: Ctl,
    /// Se puede tocar (si no, se ve atenuada).
    on: bool,
    /// Toda la fila es el botón (interruptores y filas con flecha).
    whole: bool,
}

impl View {
    fn new(title: &str, sub: Option<&str>, ctl: Ctl) -> Self {
        View { title: title.to_string(), sub: sub.map(str::to_string), warn: None, ctl, on: true, whole: false }
    }
}

/// Tamaño de un número de bytes: «24,6 MB», «1,2 GB», «812 KB».
fn fmt_bytes(b: u64) -> String {
    let mb = b as f64 / (1024.0 * 1024.0);
    if mb >= 1024.0 {
        crate::i18n::decimal(format!("{:.1} GB", mb / 1024.0))
    } else if mb >= 1.0 {
        crate::i18n::decimal(format!("{mb:.1} MB"))
    } else {
        format!("{} KB", (b / 1024).max(if b > 0 { 1 } else { 0 }))
    }
}

/// Límite de la caché de audio: «Desactivada», «256 MB», «1 GB», «1,5 GB».
fn fmt_cache(mb: u64) -> String {
    if mb == 0 {
        t("Desactivada").to_string()
    } else if mb < 1024 {
        format!("{mb} MB")
    } else if mb % 1024 == 0 {
        format!("{} GB", mb / 1024)
    } else {
        crate::i18n::decimal(format!("{:.1} GB", mb as f64 / 1024.0))
    }
}

/// Portadas y bytes de la carpeta `dir` (con sus subcarpetas).
fn dir_usage(dir: &std::path::Path) -> (u64, u64) {
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            match e.file_type() {
                Ok(ft) if ft.is_dir() => stack.push(e.path()),
                Ok(ft) if ft.is_file() => {
                    files += 1;
                    bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
                }
                _ => {}
            }
        }
    }
    (files, bytes)
}

impl App {
    // ------------------------------------------------------------ página

    /// La página de Ajustes en el panel de contenido `panel`.
    pub fn settings_panel(&mut self, ui: &mut egui::Ui, panel: Rect) {
        let p = theme::palette(ui.ctx());
        let k = ink(&p);
        self.poll_image_usage();
        self.probe_output();
        self.settings_ui.rects.clear();
        // Las pestañas van fijas arriba si el fotograma anterior ya las había subido hasta ahí.
        let stuck = self.settings_ui.stuck();
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(panel));
        c.set_clip_rect(panel.shrink(1.0));
        let mut area = egui::ScrollArea::vertical().id_salt("settings_page").auto_shrink([false, false]);
        if let Some(off) = self.settings_ui.scroll_to.take() {
            area = area.vertical_scroll_offset(off);
        }
        let out = area.show(&mut c, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let w = ui.available_width();
            let (head, _) = ui.allocate_exact_size(vec2(w, HEAD_H), Sense::hover());
            let g = ui.painter().layout_no_wrap(t("Ajustes").to_string(), theme::bold(TITLE_FONT), k.title);
            text_on_baseline(ui.painter(), pos2(head.min.x + PAD_X, head.min.y + TITLE_BASE), g, k.title);
            let (row, _) = ui.allocate_exact_size(vec2(w, TABS_H), Sense::hover());
            if !stuck {
                self.settings_tabs(ui, row, &k);
            }
            self.settings_body(ui, w, &k);
            ui.add_space(48.0);
        });
        self.settings_ui.offset = out.state.offset.y;
        if stuck {
            // Fijas: sobre el contenido que pasa por debajo, con el fondo del panel.
            let row = Rect::from_min_size(panel.min, vec2(panel.width(), TABS_H));
            let bg = if p.dark { p.bg } else { p.card };
            ui.painter().rect_filled(row, CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 }, bg);
            self.settings_tabs(ui, row, &k);
        }
        self.settings_dropdown(ui.ctx(), &k);
    }

    /// Pestañas, el filo de debajo y la lupa (con su campo).
    fn settings_tabs(&mut self, ui: &mut egui::Ui, row: Rect, k: &Ink) {
        let searching = self.settings_ui.search_open && !self.settings_ui.search.trim().is_empty();
        let base = row.min.y + TAB_BASE;
        let sep_y = row.max.y - 0.5;
        let font = theme::regular(TAB_FONT);
        let mut x = row.min.x + PAD_X + 1.0;
        let lupa = pos2(row.max.x - SEARCH_FROM_RIGHT, row.min.y + 28.5);
        // Con la búsqueda abierta y sin sitio para las dos cosas, el campo ocupa la fila.
        let tabs_w: f32 = TABS.iter().map(|l| ui.painter().layout_no_wrap(t(l).to_string(), font.clone(), k.tab_off).size().x + TAB_GAP).sum();
        let field_room = lupa.x - 26.0 - (x + tabs_w);
        let hide_tabs = self.settings_ui.search_open && field_room < 200.0;
        if !hide_tabs {
            for (i, label) in TABS.iter().enumerate() {
                let selected = self.settings_ui.tab == i as u8 && !searching;
                let g = ui.painter().layout_no_wrap(t(label).to_string(), font.clone(), k.tab_off);
                let w = g.size().x;
                let hit = Rect::from_min_max(pos2(x - 8.0, row.min.y + 6.0), pos2(x + w + 8.0, row.max.y));
                let resp = ui.interact(hit, ui.id().with(("settings_tab", i)), Sense::click());
                self.settings_ui.rects.insert(format!("Tab/{i}"), hit);
                let color = if selected {
                    k.tab_on
                } else if resp.hovered() {
                    k.tab_hover
                } else {
                    k.tab_off
                };
                let g = ui.painter().layout_no_wrap(t(label).to_string(), font.clone(), color);
                text_on_baseline(ui.painter(), pos2(x, base), g, color);
                if selected {
                    let under = Rect::from_min_max(pos2(x - 6.0, sep_y - 1.2), pos2(x + w + 6.0, sep_y + 1.5));
                    ui.painter().rect_filled(under, CornerRadius::same(1), GREEN);
                }
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if resp.clicked() {
                    self.settings_ui.open_tab(i as u8);
                    if i == 1 {
                        self.refresh_image_usage();
                    }
                }
                x += w + TAB_GAP;
            }
        }
        // La lupa abre y cierra el campo; con él abierto, la lupa va en verde.
        let color = if self.settings_ui.search_open { GREEN } else { k.tab_icon };
        if Self::slot_button(ui, ui.id().with("settings_search"), lupa, Icon::Search, 26.0, color).on_hover_text(t("Buscar en Ajustes")).clicked() {
            self.settings_ui.search_open = !self.settings_ui.search_open;
            self.settings_ui.search_focus = self.settings_ui.search_open;
            if !self.settings_ui.search_open {
                self.settings_ui.search.clear();
            }
        }
        if self.settings_ui.search_open {
            let right = lupa.x - 26.0;
            let left = if hide_tabs { row.min.x + PAD_X } else { (right - 300.0).max(x + 10.0) };
            let field = Rect::from_min_max(pos2(left, lupa.y - CTRL_H / 2.0), pos2(right, lupa.y + CTRL_H / 2.0));
            ui.painter().rect_filled(field, CornerRadius::same(6), k.field);
            ui.painter().rect_stroke(field, CornerRadius::same(6), Stroke::new(1.0, k.field_edge), egui::StrokeKind::Inside);
            let mut f = child_in(ui, field.shrink2(vec2(12.0, 0.0)), Layout::left_to_right(Align::Center));
            let r = f.add(
                egui::TextEdit::singleline(&mut self.settings_ui.search)
                    .id(egui::Id::new("settings_search_field"))
                    .hint_text(t("Buscar en Ajustes"))
                    .frame(egui::Frame::NONE)
                    .font(theme::regular(14.5))
                    .desired_width(field.width() - 24.0),
            );
            if self.settings_ui.search_focus {
                self.settings_ui.search_focus = false;
                r.request_focus();
            }
            if r.changed() {
                self.settings_ui.dropdown = None;
                self.settings_ui.scroll_to = Some(self.settings_ui.offset.min(HEAD_H));
            }
            if f.input(|i| i.key_pressed(egui::Key::Escape)) && (r.has_focus() || r.lost_focus()) {
                self.settings_ui.search_open = false;
                self.settings_ui.search.clear();
            }
        }
        ui.painter().line_segment([pos2(row.min.x, sep_y), pos2(row.max.x, sep_y)], Stroke::new(1.0, k.separator));
    }

    /// Lo de debajo de las pestañas: las secciones de la pestaña o, buscando, lo que coincide.
    fn settings_body(&mut self, ui: &mut egui::Ui, w: f32, k: &Ink) {
        let card_w = (w - 2.0 * PAD_X).clamp(260.0, CARD_W);
        let x0 = ui.max_rect().min.x + PAD_X;
        let query = fold(self.settings_ui.search.trim());
        if self.settings_ui.search_open && !query.is_empty() {
            let mut first = true;
            for tab in 0..TABS.len() as u8 {
                let rows: Vec<Row> = SECTIONS
                    .iter()
                    .filter(|s| s.tab == tab)
                    .flat_map(|s| s.rows.iter().copied())
                    .filter(|&r| self.row_shown(r) && self.row_matches(r, &query))
                    .collect();
                if rows.is_empty() {
                    continue;
                }
                self.settings_heading(ui, x0, t(TABS[tab as usize]), k);
                self.settings_card(ui, x0, card_w, &rows, k);
                first = false;
            }
            if first {
                ui.add_space(SECTION_GAP);
                let g = galley_truncated(ui.painter(), &tr!("Ningún ajuste coincide con «{}».", self.settings_ui.search.trim()), theme::regular(14.5), k.sub, card_w);
                let (r, _) = ui.allocate_exact_size(vec2(w, 24.0), Sense::hover());
                text_on_baseline(ui.painter(), pos2(x0, r.min.y + 16.0), g, k.sub);
            }
            return;
        }
        let tab = self.settings_ui.tab;
        let mut first = true;
        for s in SECTIONS.iter().filter(|s| s.tab == tab) {
            let rows: Vec<Row> = s.rows.iter().copied().filter(|&r| self.row_shown(r)).collect();
            if rows.is_empty() {
                continue;
            }
            if s.title.is_empty() {
                ui.add_space(if first { BARE_GAP } else { SECTION_GAP - 8.0 });
            } else {
                self.settings_heading(ui, x0, t(s.title), k);
            }
            self.settings_card(ui, x0, card_w, &rows, k);
            if rows.contains(&Row::Profile) {
                self.free_account_note(ui, x0, card_w, k);
            }
            first = false;
        }
        if tab == 4 {
            // Pie, como en el mockup: gris, a 25 px de la última tarjeta.
            ui.add_space(25.0);
            let text = t("Nanofy es un proyecto independiente y no está afiliado a Spotify. Usa librespot para la reproducción y egui para la interfaz, dibujada por CPU.");
            let g = ui.painter().layout(text.to_string(), theme::regular(SUB_FONT), k.sub, card_w.min(480.0));
            let (r, _) = ui.allocate_exact_size(vec2(w, g.size().y), Sense::hover());
            ui.painter().galley(pos2(x0, r.min.y), g, k.sub);
        }
    }

    /// Si la fila se enseña (algunas dependen del sistema o de la sesión).
    fn row_shown(&self, r: Row) -> bool {
        match r {
            Row::UpdateAuto => crate::update::can_self_install(),
            Row::DevApp => self.logged_in() || self.api.personal_configured(),
            _ => true,
        }
    }

    fn row_matches(&self, r: Row, q: &str) -> bool {
        let v = self.row_view(r);
        let mut hay = fold(&v.title);
        if let Some(s) = &v.sub {
            hay.push(' ');
            hay.push_str(&fold(s));
        }
        hay.push(' ');
        hay.push_str(r.keywords());
        // Las filas que dependen de otra salen también con el nombre de la otra.
        let parent = match r {
            Row::Loudness => Some(Row::Normalize),
            Row::CrossfadeSecs | Row::CrossfadeAlbums => Some(Row::Crossfade),
            Row::UpdateAuto => Some(Row::UpdateCheck),
            _ => None,
        };
        if let Some(pr) = parent {
            hay.push(' ');
            hay.push_str(&fold(&self.row_view(pr).title));
        }
        q.split_whitespace().all(|w| hay.contains(w))
    }

    /// Título de sección: negrita a `SECTION_GAP` de lo de arriba y a `HEAD_TO_CARD` de su tarjeta.
    fn settings_heading(&self, ui: &mut egui::Ui, x0: f32, text: &str, k: &Ink) {
        let w = ui.available_width();
        let (r, _) = ui.allocate_exact_size(vec2(w, SECTION_GAP + HEAD_TO_CARD), Sense::hover());
        let g = ui.painter().layout_no_wrap(text.to_string(), theme::bold(SECTION_FONT), k.title);
        text_on_baseline(ui.painter(), pos2(x0, r.min.y + SECTION_GAP), g, k.title);
    }

    /// Tarjeta con sus filas; el fondo se pinta al final, detrás, con el alto que salió.
    fn settings_card(&mut self, ui: &mut egui::Ui, x0: f32, card_w: f32, rows: &[Row], k: &Ink) {
        let bg = ui.painter().add(egui::Shape::Noop);
        let top = ui.cursor().min.y;
        for (i, &row) in rows.iter().enumerate() {
            let y = ui.cursor().min.y;
            if i > 0 {
                ui.painter().line_segment([pos2(x0 + TEXT_X, y + 0.5), pos2(x0 + card_w, y + 0.5)], Stroke::new(1.0, k.divider));
            }
            match row {
                Row::Profile => self.profile_row(ui, x0, card_w, k),
                Row::AppInfo => self.app_info_row(ui, x0, card_w, k),
                _ => self.settings_row(ui, x0, card_w, row, (i == 0, i + 1 == rows.len()), k),
            }
        }
        let bottom = ui.cursor().min.y;
        let rect = Rect::from_min_max(pos2(x0, top), pos2(x0 + card_w, bottom));
        ui.painter().set(bg, egui::Shape::rect_filled(rect, CornerRadius::same(CARD_R), k.card));
    }

    // ------------------------------------------------------------ una fila

    /// Textos y control de una fila, según el estado.
    fn row_view(&self, r: Row) -> View {
        let s = &self.settings;
        match r {
            Row::Quality => View::new(
                t("Calidad de audio"),
                Some(t("Ogg Vorbis, el mismo formato que la app de escritorio de Spotify.")),
                Ctl::Drop(Drop::Quality, s.quality.label().to_string()),
            ),
            Row::Normalize => View {
                whole: true,
                ..View::new(t("Normalizar volumen"), Some(t("Todas las canciones suenan a un volumen parecido.")), Ctl::Toggle(s.normalisation))
            },
            Row::Loudness => {
                let opts = [Loudness::Loud, Loudness::Normal, Loudness::Quiet];
                View {
                    on: s.normalisation,
                    ..View::new(
                        t("Nivel de volumen"),
                        Some(s.loudness.hint()),
                        Ctl::Chips(opts.iter().map(|l| l.label().to_string()).collect(), opts.iter().position(|l| *l == s.loudness).unwrap_or(1)),
                    )
                }
            }
            Row::Output => {
                let hint = match self.output_now().map(|(_, rate)| rate) {
                    None => t("Se sabrá al reproducir la primera canción"),
                    Some(SOURCE_RATE) => t("El dispositivo ya está a 44,1 kHz: el audio llega sin convertir."),
                    _ if cfg!(windows) => t("Para evitar el remuestreo, pon el dispositivo a 44,1 kHz en Configuración → Sistema → Sonido."),
                    _ => t("Para evitar el remuestreo, pon el dispositivo de salida a 44,1 kHz."),
                };
                View::new(t("Salida de audio"), Some(hint), Ctl::Output)
            }
            Row::Gapless => View {
                whole: true,
                ..View::new(t("Reproducción sin pausas"), Some(t("Sin silencios entre canciones, como en los discos en vivo.")), Ctl::Toggle(s.gapless))
            },
            Row::Crossfade => View {
                whole: true,
                // Con «al terminar la canción» puesto, la canción acaba entera (`set_sleep_end_of_track`).
                warn: (s.crossfade && self.sleep_end_of_track)
                    .then(|| t("Fundido suspendido mientras el temporizador «al terminar la canción» está puesto.").to_string()),
                ..View::new(t("Fundido entre canciones"), Some(t("La siguiente entra mientras la anterior se apaga.")), Ctl::Toggle(s.crossfade))
            },
            Row::CrossfadeSecs => {
                let (min, max) = (crate::config::CROSSFADE_SECS_MIN as f32, crate::config::CROSSFADE_SECS_MAX as f32);
                let secs = s.crossfade_secs as f32;
                View {
                    on: s.crossfade,
                    ..View::new(t("Duración del fundido"), None, Ctl::Slider { frac: (secs - min) / (max - min), value: format!("{} s", s.crossfade_secs) })
                }
            }
            Row::CrossfadeAlbums => View {
                on: s.crossfade,
                whole: true,
                ..View::new(t("También entre canciones de un mismo álbum"), Some(t("Spotify no lo hace, para respetar las transiciones del artista.")), Ctl::Toggle(s.crossfade_albums))
            },
            Row::Autoplay => View {
                whole: true,
                ..View::new(t("Reproducir canciones similares"), Some(t("Al acabarse lo que suena, sigue con canciones parecidas.")), Ctl::Toggle(s.autoplay))
            },
            Row::DeviceName => View::new(t("Nombre en Spotify Connect"), Some(t("Así aparece Nanofy en tus otros dispositivos y en las Jams.")), Ctl::Field),
            Row::MediaKeys => {
                let sub = if cfg!(windows) {
                    t("Controla la música con el teclado y desde el panel de Windows.")
                } else if cfg!(target_os = "macos") {
                    t("Controla la música con el teclado y desde el Centro de control.")
                } else {
                    t("Controla la música con el teclado y desde el sistema.")
                };
                View { whole: true, ..View::new(t("Teclas multimedia"), Some(sub), Ctl::Toggle(s.media_keys)) }
            }
            Row::Preload => View {
                whole: true,
                ..View::new(t("Precarga inteligente"), Some(t("Prepara la canción sobre la que pasas el ratón para que suene al instante.")), Ctl::Toggle(s.smart_preload))
            },
            Row::Fps => {
                let opts = [60u32, 120, 144, 240];
                let sel = opts.iter().position(|f| *f == s.fps_cap).unwrap_or(usize::MAX);
                View::new(t("Límite de fotogramas"), Some(t("Solo durante animaciones; en reposo no se repinta.")), Ctl::Chips(opts.iter().map(|f| f.to_string()).collect(), sel))
            }
            Row::AudioCache => {
                let mb = match self.settings_ui.drag {
                    Some(Drag::Cache(mb)) => mb,
                    _ => s.audio_cache_mb,
                };
                View::new(
                    t("Caché de audio"),
                    Some(t("Las canciones descargadas y ya escuchadas se guardan aquí.")),
                    Ctl::Slider { frac: mb.min(CACHE_MAX_MB) as f32 / CACHE_MAX_MB as f32, value: fmt_cache(mb) },
                )
            }
            Row::ImageCache => {
                let sub = match self.settings_ui.image_usage {
                    Some((n, b)) => tr!("{} portadas · {}", fmt_thousands(n), fmt_bytes(b)),
                    None => t("Calculando…").to_string(),
                };
                View { sub: Some(sub), ..View::new(t("Caché de imágenes"), None, Ctl::Buttons(vec![(t("Borrar").to_string(), false)])) }
            }
            Row::Language => {
                let label = if s.language == "auto" || s.language.is_empty() {
                    t("Automático").to_string()
                } else {
                    crate::i18n::languages().into_iter().find(|(c, _)| *c == s.language).map(|(_, n)| n).unwrap_or_else(|| s.language.clone())
                };
                View::new(t("Idioma"), Some(t("Lo que falte en un idioma se ve en español.")), Ctl::Drop(Drop::Language, label))
            }
            Row::Theme => {
                let sub = if cfg!(windows) {
                    t("«Sistema» sigue el modo claro u oscuro de Windows.")
                } else {
                    t("«Sistema» sigue el modo claro u oscuro del sistema.")
                };
                View::new(t("Tema"), Some(sub), Ctl::Theme)
            }
            Row::Zoom => {
                let z = match self.settings_ui.drag {
                    Some(Drag::Zoom(z)) => z,
                    _ => s.zoom,
                };
                View::new(
                    t("Escala de la interfaz"),
                    Some(t("Agranda o achica el texto y los controles.")),
                    Ctl::Slider { frac: (z - ZOOM_MIN) / (ZOOM_MAX - ZOOM_MIN), value: format!("{:.0} %", z * 100.0) },
                )
            }
            Row::Library => {
                if let Some(_c) = &self.web_chain {
                    View::new(
                        t("Esperando al navegador…"),
                        Some(t("Permite que Nanofy lea tu biblioteca en la pestaña que se abrió.")),
                        Ctl::Buttons(vec![(t("Cancelar").to_string(), false), (t("Abrir de nuevo").to_string(), false)]),
                    )
                } else if self.api.web_configured() {
                    View::new(
                        t("Conectada con Spotify"),
                        Some(t("Nanofy lee tus playlists, carpetas y Me gusta, y busca en todo el catálogo.")),
                        Ctl::Buttons(vec![(t("Desconectar").to_string(), false)]),
                    )
                } else {
                    View {
                        on: self.logged_in(),
                        ..View::new(
                            t("Biblioteca sin conectar"),
                            Some(t("Autoriza tu cuenta en el navegador (una sola vez). No necesitas crear ninguna app.")),
                            Ctl::Buttons(vec![(t("Conectar con Spotify").to_string(), true)]),
                        )
                    }
                }
            }
            Row::DevApp => {
                let open = self.settings_ui.expanded.contains(&Row::DevApp);
                let sub = if self.api.personal_configured() {
                    t("Conectada. Las lecturas usan la cuota de tu app.")
                } else {
                    t("Opcional. Nanofy funciona entero sin ella.")
                };
                View { whole: true, ..View::new(t("Tu propia app de desarrollador"), Some(sub), Ctl::Chevron(open)) }
            }
            Row::UpdateCheck => View {
                whole: true,
                ..View::new(t("Buscar versiones nuevas"), Some(t("Al abrir Nanofy y cada pocas horas.")), Ctl::Toggle(s.update_check))
            },
            Row::UpdateAuto => View {
                on: s.update_check,
                whole: true,
                warn: (s.update_check && s.update_auto && !self.auto_update_allowed())
                    .then(|| t("En esta copia (de desarrollo o de pruebas) no se aplica: las versiones nuevas se avisan con «Instalar».").to_string()),
                ..View::new(
                    t("Actualizar automáticamente"),
                    Some(t("Descarga las versiones nuevas en segundo plano; solo tendrás que pulsar Reiniciar.")),
                    Ctl::Toggle(s.update_auto && s.update_check),
                )
            },
            Row::Shortcuts => {
                let sub = if cfg!(target_os = "macos") { t("Espacio, Cmd+→, Cmd+J y los demás.") } else { t("Espacio, Ctrl+→, Ctrl+J y los demás.") };
                View { whole: true, ..View::new(t("Atajos de teclado"), Some(sub), Ctl::Chevron(false)) }
            }
            Row::TechInfo => {
                let open = self.settings_ui.expanded.contains(&Row::TechInfo);
                View { whole: true, ..View::new(t("Información técnica"), Some(t("Memoria, rendimiento y dónde se guardan tus datos.")), Ctl::Chevron(open)) }
            }
            Row::Profile | Row::AppInfo => View::new("", None, Ctl::None),
        }
    }

    /// Ancho del control (para repartir la fila entre él y el texto).
    fn ctl_width(&self, painter: &egui::Painter, ctl: &Ctl) -> f32 {
        let font = theme::regular(14.5);
        let text_w = |s: &str| painter.layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE).size().x;
        match ctl {
            Ctl::None => 0.0,
            Ctl::Toggle(_) => 40.0,
            Ctl::Chips(opts, _) => opts.iter().map(|o| (text_w(o) + 32.0).round()).sum::<f32>() + 9.0 * (opts.len().saturating_sub(1)) as f32,
            Ctl::Drop(..) => DROP_W,
            Ctl::Slider { .. } => SLIDER_W + SLIDER_VALUE_W,
            Ctl::Field => FIELD_W,
            Ctl::Buttons(b) => b.iter().map(|(l, _)| text_w(l) + 36.0).sum::<f32>() + 8.0 * (b.len().saturating_sub(1)) as f32,
            Ctl::Output => 300.0,
            Ctl::Chevron(_) => 20.0,
            Ctl::Theme => 3.0 * THEME_W + 2.0 * THEME_GAP,
        }
    }

    /// Una fila normal: icono, nombre, explicación y control. Las desplegadas añaden lo suyo
    /// debajo, dentro de la misma tarjeta.
    fn settings_row(&mut self, ui: &mut egui::Ui, x0: f32, card_w: f32, row: Row, (first, last): (bool, bool), k: &Ink) {
        let v = self.row_view(row);
        let painter = ui.painter().clone();
        let ctl_w = self.ctl_width(&painter, &v.ctl);
        // El texto se lleva lo que deja el control; si no cabe, el control baja a su propia línea.
        let room = card_w - TEXT_X - RIGHT - 28.0 - ctl_w;
        let stacked = room < 170.0 && !matches!(v.ctl, Ctl::Toggle(_) | Ctl::Chevron(_) | Ctl::None);
        let text_w = if stacked { card_w - TEXT_X - RIGHT } else { room.min(SUB_MAX_W).max(120.0) };
        let title_c = dim(k.row, v.on);
        let sub_c = dim(k.sub, v.on);
        let title_g = galley_truncated(&painter, &v.title, theme::regular(NAME_FONT), title_c, text_w);
        let sub_g = if v.sub.is_some() || v.warn.is_some() {
            let mut job = LayoutJob::default();
            job.wrap.max_width = text_w;
            let fmt = |c: Color32| egui::TextFormat::simple(theme::regular(SUB_FONT), c);
            if let Some(s) = &v.sub {
                job.append(s, 0.0, fmt(sub_c));
            }
            if let Some(w) = &v.warn {
                job.append(if v.sub.is_some() { "\n" } else { "" }, 0.0, fmt(sub_c));
                job.append(w, 0.0, fmt(dim(k.warn, v.on)));
            }
            Some(painter.layout_job(job))
        } else {
            None
        };
        let lines = sub_g.as_ref().map(|g| g.rows.len()).unwrap_or(0);
        let text_h = if lines > 0 { ROW_H - LINE + LINE * lines as f32 } else { ROW_SHORT };
        let ctl_h = match &v.ctl {
            Ctl::Theme => THEME_ROW_H,
            _ => 0.0,
        };
        let stack_h = if stacked { CTRL_H + 16.0 } else { 0.0 };
        let head_h = text_h.max(ctl_h) + stack_h;
        let w = ui.available_width();
        let (alloc, _) = ui.allocate_exact_size(vec2(w, head_h), Sense::hover());
        let r = Rect::from_min_size(pos2(x0, alloc.min.y), vec2(card_w, head_h));
        let text_top = r.min.y + ((head_h - stack_h) - text_h) / 2.0;
        let id = ui.id().with(("settings_row", row));
        self.settings_ui.rects.insert(format!("{row:?}"), r);

        // Toda la fila es el botón en los interruptores y las filas con flecha; se aclara un poco
        // al pasar el ratón (con las esquinas de la tarjeta si es la primera o la última).
        let mut event: Option<Event> = None;
        if v.whole && v.on {
            let resp = ui.interact(r, id.with("whole"), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                let open = self.settings_ui.expanded.contains(&row);
                let (top, bottom) = (if first { CARD_R } else { 0 }, if last && !open { CARD_R } else { 0 });
                painter.rect_filled(r, CornerRadius { nw: top, ne: top, sw: bottom, se: bottom }, k.row_hover);
            }
            if resp.clicked() {
                event = Some(if matches!(v.ctl, Ctl::Chevron(_)) { Event::Expand } else { Event::Toggle });
            }
        }

        // Icono, nombre y explicación.
        let cy = r.min.y + (head_h - stack_h) / 2.0;
        if let Some(icon) = row.icon() {
            icons::paint(&painter, Rect::from_center_size(pos2(x0 + ICON_CX, cy), vec2(ICON_PX, ICON_PX)), dim(k.icon, v.on), icon);
        }
        let title_base = text_top + if lines > 0 { 29.0 } else { 35.0 };
        let title_rect = text_on_baseline(&painter, pos2(x0 + TEXT_X, title_base), title_g, title_c);
        if row == Row::CrossfadeSecs {
            // Lo que el mockup deja fuera de la fila, al pasar el ratón.
            ui.interact(title_rect, id.with("tip"), Sense::hover())
                .on_hover_text(t("No se aplica al cambiar de canción a mano, al repetir una canción ni en podcasts."));
        }
        if let Some(g) = sub_g {
            text_on_baseline(&painter, pos2(x0 + TEXT_X, text_top + 50.5), g, sub_c);
        }

        // Control: a la derecha, centrado en la fila; o debajo del texto si no cabe.
        let (right, ccy) = if stacked { (x0 + TEXT_X + ctl_w, r.max.y - CTRL_H / 2.0 - 14.0) } else { (r.max.x - RIGHT, cy) };
        if let Some(e) = self.row_control(ui, id, row, &v, right, ccy, k) {
            event = Some(e);
        }
        if let Some(e) = event {
            self.row_event(ui.ctx(), row, e);
        }

        // Lo desplegado, dentro de la tarjeta.
        if self.settings_ui.expanded.contains(&row) {
            match row {
                Row::DevApp => self.dev_app_body(ui, x0, card_w, k),
                Row::TechInfo => self.tech_info_body(ui, x0, card_w, k),
                _ => {}
            }
        }
    }

    /// Dibuja el control de la fila y devuelve lo que se hizo con él.
    #[allow(clippy::too_many_arguments)]
    fn row_control(&mut self, ui: &mut egui::Ui, id: egui::Id, row: Row, v: &View, right: f32, cy: f32, k: &Ink) -> Option<Event> {
        let painter = ui.painter().clone();
        match &v.ctl {
            Ctl::None => None,
            Ctl::Toggle(on) => {
                paint_switch(ui, id, pos2(right - 20.0, cy), *on, v.on, k);
                None
            }
            Ctl::Chips(opts, sel) => {
                let font = theme::regular(14.5);
                let widths: Vec<f32> = opts.iter().map(|o| (painter.layout_no_wrap(o.clone(), font.clone(), Color32::WHITE).size().x + 32.0).round()).collect();
                let total = widths.iter().sum::<f32>() + 9.0 * (opts.len().saturating_sub(1)) as f32;
                let mut x = right - total;
                let mut out = None;
                let enabled = ui.is_enabled() && v.on;
                for (i, o) in opts.iter().enumerate() {
                    let mut c = child_in(ui, Rect::from_min_size(pos2(x, cy - 18.0), vec2(widths[i], 36.0)), Layout::left_to_right(Align::Center));
                    if !enabled {
                        c.disable();
                    }
                    let resp = Self::chip(&mut c, pos2(x, cy - 18.0), o, i == *sel, if k.dark { 42 } else { 0 });
                    if !enabled {
                        painter.rect_filled(resp.rect, CornerRadius::same(6), k.card.gamma_multiply(0.55));
                    } else if resp.clicked() && i != *sel {
                        out = Some(Event::Choose(i));
                    }
                    x += widths[i] + 9.0;
                }
                out
            }
            Ctl::Drop(which, label) => {
                let rect = Rect::from_min_size(pos2(right - DROP_W, cy - CTRL_H / 2.0), vec2(DROP_W, CTRL_H));
                self.settings_ui.drop_rects.insert(*which, rect);
                self.settings_ui.rects.insert(format!("{which:?}/drop"), rect);
                let resp = ui.interact(rect, id.with("drop"), Sense::click());
                let open = self.settings_ui.dropdown == Some(*which);
                let fill = if resp.hovered() || open { k.control_hover } else { k.control };
                painter.rect_filled(rect, CornerRadius::same(6), fill);
                let g = galley_truncated(&painter, label, theme::regular(14.5), k.control_text, DROP_W - 52.0);
                text_on_baseline(&painter, pos2(rect.min.x + 15.0, cy + 5.2), g, k.control_text);
                icons::paint(&painter, Rect::from_center_size(pos2(rect.max.x - 19.0, cy), vec2(16.0, 16.0)), k.icon, if open { Icon::ChevronUp } else { Icon::ChevronDown });
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                resp.clicked().then_some(Event::OpenDrop(*which))
            }
            Ctl::Slider { frac, value } => {
                let g = painter.layout_no_wrap(value.clone(), theme::regular(14.5), dim(k.control_text, v.on));
                text_on_baseline(&painter, pos2(right - g.size().x, cy + 5.0), g, dim(k.control_text, v.on));
                let zone = Rect::from_min_max(pos2(right - SLIDER_VALUE_W - SLIDER_W, cy - 10.0), pos2(right - SLIDER_VALUE_W, cy + 10.0));
                self.settings_ui.rects.insert(format!("{row:?}/slider"), zone);
                settings_slider(ui, id.with("slider"), zone, *frac, v.on, k)
            }
            Ctl::Field => self.device_name_field(ui, id, Rect::from_min_size(pos2(right - FIELD_W, cy - CTRL_H / 2.0), vec2(FIELD_W, CTRL_H)), k),
            Ctl::Buttons(buttons) => {
                let mut x = right;
                let mut out = None;
                for (i, (label, primary)) in buttons.iter().enumerate() {
                    let enabled = v.on && !(row == Row::Library && self.web_busy);
                    let resp = pill_button(ui, id.with(("button", i)), x, cy, label, *primary, enabled, k);
                    if resp.clicked() {
                        out = Some(Event::Button(i as u8));
                    }
                    x = resp.rect.min.x - 8.0;
                }
                if row == Row::Library && self.web_busy {
                    let phase = (ui.input(|i| i.time) * 4.0) as usize % 4;
                    let g = painter.layout_no_wrap(["", "·", "··", "···"][phase].to_string(), theme::bold(16.0), k.sub);
                    text_on_baseline(&painter, pos2(x - g.size().x - 4.0, cy + 5.0), g, k.sub);
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
                }
                out
            }
            Ctl::Output => {
                self.output_status(ui, id, right, cy, k);
                None
            }
            Ctl::Chevron(open) => {
                let icon = if matches!(row, Row::Shortcuts) {
                    Icon::ChevronRight
                } else if *open {
                    Icon::ChevronUp
                } else {
                    Icon::ChevronDown
                };
                icons::paint(&painter, Rect::from_center_size(pos2(right - 9.0, cy), vec2(18.0, 18.0)), k.icon, icon);
                None
            }
            Ctl::Theme => self.theme_cards(ui, id, right, cy, k),
        }
    }

    /// Hace lo pedido en una fila.
    fn row_event(&mut self, ctx: &egui::Context, row: Row, e: Event) {
        match (row, e) {
            (Row::Normalize, Event::Toggle) => self.apply_setting(ctx, |s| s.normalisation = !s.normalisation),
            (Row::Loudness, Event::Choose(i)) => self.apply_setting(ctx, |s| s.loudness = [Loudness::Loud, Loudness::Normal, Loudness::Quiet][i.min(2)]),
            (Row::Gapless, Event::Toggle) => self.apply_setting(ctx, |s| s.gapless = !s.gapless),
            (Row::Crossfade, Event::Toggle) => {
                let on = !self.settings.crossfade;
                self.set_crossfade(on, self.settings.crossfade_secs, self.settings.crossfade_albums, true);
            }
            (Row::CrossfadeSecs, Event::SliderMove(f)) => {
                // Mientras se arrastra ya suena con el valor nuevo; se guarda al soltar.
                let secs = self.crossfade_secs_at(f);
                self.set_crossfade(true, secs, self.settings.crossfade_albums, false);
            }
            (Row::CrossfadeSecs, Event::SliderDone(f)) => {
                let secs = self.crossfade_secs_at(f);
                self.set_crossfade(true, secs, self.settings.crossfade_albums, true);
            }
            (Row::CrossfadeAlbums, Event::Toggle) => {
                let albums = !self.settings.crossfade_albums;
                self.set_crossfade(true, self.settings.crossfade_secs, albums, true);
            }
            (Row::Autoplay, Event::Toggle) => self.set_autoplay(!self.settings.autoplay),
            (Row::MediaKeys, Event::Toggle) => self.apply_setting(ctx, |s| s.media_keys = !s.media_keys),
            (Row::Preload, Event::Toggle) => self.apply_setting(ctx, |s| s.smart_preload = !s.smart_preload),
            (Row::Fps, Event::Choose(i)) => self.apply_setting(ctx, |s| s.fps_cap = [60, 120, 144, 240][i.min(3)]),
            (Row::AudioCache, Event::SliderMove(f)) => self.settings_ui.drag = Some(Drag::Cache(cache_mb_at(f))),
            (Row::AudioCache, Event::SliderDone(f)) => {
                self.settings_ui.drag = None;
                let mb = cache_mb_at(f);
                self.apply_setting(ctx, |s| s.audio_cache_mb = mb);
            }
            (Row::ImageCache, Event::Button(_)) => {
                let dir = self.paths.image_cache_dir();
                let _ = std::fs::remove_dir_all(&dir);
                let _ = std::fs::create_dir_all(&dir);
                self.images.clear();
                self.status(t("Caché de imágenes borrada"));
                self.refresh_image_usage();
            }
            (Row::Zoom, Event::SliderMove(f)) => self.settings_ui.drag = Some(Drag::Zoom(zoom_at(f))),
            (Row::Zoom, Event::SliderDone(f)) => {
                self.settings_ui.drag = None;
                let z = zoom_at(f);
                self.apply_setting(ctx, |s| s.zoom = z);
            }
            (Row::Theme, Event::Choose(i)) => self.apply_setting(ctx, |s| s.theme = [Theme::System, Theme::Dark, Theme::Light][i.min(2)]),
            (Row::Library, Event::Button(i)) => {
                if self.web_chain.is_some() {
                    match i {
                        0 => self.cancel_web_chain(),
                        _ => {
                            if let Some(url) = self.web_chain.as_ref().map(|c| c.url.clone()) {
                                crate::webauth::open_in_browser(&url);
                            }
                        }
                    }
                } else if self.api.web_configured() {
                    self.web_busy = true;
                    self.api.send(crate::api::Req::WebDisconnect);
                } else {
                    self.connect_library();
                }
            }
            (Row::DevApp | Row::TechInfo, Event::Expand) => {
                if let Some(i) = self.settings_ui.expanded.iter().position(|r| *r == row) {
                    self.settings_ui.expanded.remove(i);
                } else {
                    self.settings_ui.expanded.push(row);
                }
            }
            (Row::Shortcuts, Event::Expand) => self.show_shortcuts = true,
            (Row::UpdateCheck, Event::Toggle) => self.set_update_check(!self.settings.update_check),
            (Row::UpdateAuto, Event::Toggle) => self.set_update_auto(!self.settings.update_auto),
            (_, Event::OpenDrop(d)) => {
                self.settings_ui.dropdown = if self.settings_ui.dropdown == Some(d) { None } else { Some(d) };
            }
            (Row::DeviceName, Event::Commit) => {
                if let Some(name) = self.settings_ui.name_edit.take() {
                    let name = name.trim().to_string();
                    if !name.is_empty() && name != self.settings.device_name {
                        self.apply_setting(ctx, |s| s.device_name = name);
                        self.status(t("Reconectando con el nombre nuevo…"));
                    }
                }
            }
            _ => {}
        }
    }

    fn crossfade_secs_at(&self, f: f32) -> u8 {
        let (min, max) = (crate::config::CROSSFADE_SECS_MIN as f32, crate::config::CROSSFADE_SECS_MAX as f32);
        (min + f.clamp(0.0, 1.0) * (max - min)).round() as u8
    }

    /// Un ajuste cambiado desde la página: se aplica y se guarda al instante (`save_settings`,
    /// sin el aviso genérico de «Ajustes guardados»).
    pub fn apply_setting(&mut self, ctx: &egui::Context, f: impl FnOnce(&mut Settings)) {
        self.draft = self.settings.clone();
        f(&mut self.draft);
        if self.draft != self.settings {
            self.commit_settings(ctx, true);
        }
    }

    // ------------------------------------------------------------ controles

    /// Campo del nombre del dispositivo: se aplica al pulsar Intro o al salir de él.
    fn device_name_field(&mut self, ui: &mut egui::Ui, id: egui::Id, rect: Rect, k: &Ink) -> Option<Event> {
        let painter = ui.painter().clone();
        painter.rect_filled(rect, CornerRadius::same(6), k.field);
        let current = self.settings.device_name.clone();
        let editing = self.settings_ui.name_edit.get_or_insert(current);
        let mut c = child_in(ui, rect.shrink2(vec2(13.0, 0.0)), Layout::left_to_right(Align::Center));
        let r = c.add(
            egui::TextEdit::singleline(editing)
                .id(id.with("field"))
                .frame(egui::Frame::NONE)
                .font(theme::regular(14.5))
                .text_color(k.control_text)
                .char_limit(64)
                .desired_width(rect.width() - 26.0),
        );
        let edge = if r.has_focus() { GREEN.gamma_multiply(0.8) } else { k.field_edge };
        painter.rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, edge), egui::StrokeKind::Inside);
        if r.lost_focus() {
            return Some(Event::Commit);
        }
        if !r.has_focus() && self.settings_ui.name_edit.as_deref() == Some(self.settings.device_name.as_str()) {
            // Sin editar: que siga al ajuste (p. ej. cambiado desde el modo de control).
            self.settings_ui.name_edit = None;
        }
        None
    }

    /// Dispositivo de salida y su frecuencia: el abierto si ya sonó algo; si no, el predeterminado
    /// del sistema (el que se abrirá).
    fn output_now(&self) -> Option<(String, u32)> {
        let out = crate::backend::audio_output();
        if out.rate > 0 {
            let name = crate::backend::audio_output_name().unwrap_or_else(|| t("Dispositivo predeterminado").to_string());
            return Some((name, out.rate));
        }
        self.settings_ui.output_probe.clone()
    }

    /// Mira de vez en cuando el dispositivo predeterminado (en otro hilo: tarda unos milisegundos).
    fn probe_output(&mut self) {
        let ui = &mut self.settings_ui;
        if let Some(rx) = &ui.output_rx {
            match rx.try_recv() {
                Ok(v) => {
                    ui.output_probe = v;
                    ui.output_rx = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => ui.output_rx = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let due = ui.output_at.is_none_or(|at| at.elapsed() > std::time::Duration::from_secs(10));
        if due && ui.output_rx.is_none() && crate::backend::audio_output().rate == 0 {
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(crate::backend::default_audio_output());
            });
            ui.output_rx = Some(rx);
            ui.output_at = Some(std::time::Instant::now());
        }
    }

    /// «● Altavoces (Realtek) · 48 kHz»: el punto ámbar si hay remuestreo, verde si no.
    fn output_status(&self, ui: &mut egui::Ui, id: egui::Id, right: f32, cy: f32, k: &Ink) {
        let painter = ui.painter().clone();
        let Some((name, rate)) = self.output_now() else {
            let g = painter.layout_no_wrap(t("Aún no ha sonado nada").to_string(), theme::regular(14.5), k.sub);
            text_on_baseline(&painter, pos2(right - g.size().x, cy + 5.0), g, k.sub);
            return;
        };
        let rate_g = painter.layout_no_wrap(format!(" · {} kHz", crate::model::fmt_khz(rate)), theme::regular(14.5), k.sub);
        let name_g = galley_truncated(&painter, &name, theme::regular(14.5), k.row, 260.0);
        let x_rate = right - rate_g.size().x;
        let x_name = x_rate - name_g.size().x;
        let dot = if rate == SOURCE_RATE { GREEN } else { k.warn };
        painter.circle_filled(pos2(x_name - 11.0, cy), 3.5, dot);
        let r1 = text_on_baseline(&painter, pos2(x_name, cy + 5.0), name_g, k.row);
        let r2 = text_on_baseline(&painter, pos2(x_rate, cy + 5.0), rate_g, k.sub);
        if let Some(summary) = crate::backend::audio_output().summary(true) {
            ui.interact(r1.union(r2).expand2(vec2(14.0, 6.0)), id.with("out"), Sense::hover()).on_hover_text(summary);
        }
    }

    /// Las tres miniaturas del tema (Sistema, Oscuro, Claro) con su nombre debajo.
    fn theme_cards(&mut self, ui: &mut egui::Ui, id: egui::Id, right: f32, cy: f32, k: &Ink) -> Option<Event> {
        let painter = ui.painter().clone();
        let top = cy - THEME_ROW_H / 2.0 + 12.0;
        let names = [t("Sistema"), t("Oscuro"), t("Claro")];
        let current = [Theme::System, Theme::Dark, Theme::Light].iter().position(|th| *th == self.settings.theme).unwrap_or(0);
        let mut out = None;
        let ppp = ui.ctx().pixels_per_point();
        for i in 0..3 {
            let x = right - (3 - i) as f32 * THEME_W - (2 - i) as f32 * THEME_GAP;
            let card = Rect::from_min_size(pos2(x, top), vec2(THEME_W, THEME_H));
            self.settings_ui.rects.insert(format!("Theme/{i}"), card);
            let resp = ui.interact(card.expand(4.0), id.with(("theme", i)), Sense::click());
            let tex = self.theme_preview(ui.ctx(), i as u8, (THEME_W * ppp).round() as u32, (THEME_H * ppp).round() as u32);
            painter.image(tex, card, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            let selected = i == current;
            if selected || resp.hovered() {
                let ring = if selected { if k.dark { Color32::from_gray(225) } else { k.title } } else { k.sub };
                painter.rect_stroke(card, CornerRadius::same(9), Stroke::new(2.0, ring), egui::StrokeKind::Outside);
            }
            let color = if selected { k.row } else { k.sub };
            let g = painter.layout_no_wrap(names[i].to_string(), theme::regular(14.5), color);
            text_on_baseline(&painter, pos2(card.center().x - g.size().x / 2.0, card.max.y + 21.0), g, color);
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() && !selected {
                out = Some(Event::Choose(i));
            }
        }
        out
    }

    /// Textura de la miniatura del tema `kind` (0 sistema, 1 oscuro, 2 claro) a `w`×`h` píxeles.
    fn theme_preview(&mut self, ctx: &egui::Context, kind: u8, w: u32, h: u32) -> egui::TextureId {
        if let Some(tex) = self.settings_ui.previews.get(&(kind, w)) {
            return tex.id();
        }
        let img = theme_preview_image(kind, w as usize, h as usize);
        let tex = ctx.load_texture(format!("tema-{kind}-{w}"), img, egui::TextureOptions::LINEAR);
        let id = tex.id();
        self.settings_ui.previews.insert((kind, w), tex);
        id
    }

    // ------------------------------------------------------------ cuenta y «Acerca de»

    /// Tarjeta de la cuenta: foto, nombre, tipo de cuenta y «Cerrar sesión». La foto y el nombre
    /// abren tu perfil.
    fn profile_row(&mut self, ui: &mut egui::Ui, x0: f32, card_w: f32, k: &Ink) {
        let w = ui.available_width();
        let (alloc, _) = ui.allocate_exact_size(vec2(w, HERO_H), Sense::hover());
        let r = Rect::from_min_size(pos2(x0, alloc.min.y), vec2(card_w, HERO_H));
        let painter = ui.painter().clone();
        let pic = Rect::from_min_size(pos2(r.min.x + 20.0, r.min.y + 22.0), vec2(HERO_PIC, HERO_PIC));
        let text_x = pic.max.x + 18.0;
        let cy = r.center().y;
        let id = ui.id().with("settings_profile");
        let auth = self.auth.clone();
        let (title, sub, button): (String, String, Option<(String, bool)>) = match &auth {
            Auth::LoggedIn { .. } => {
                let product = self.user.as_ref().and_then(|u| u.product.clone()).unwrap_or_default();
                let plan = match product.as_str() {
                    "premium" => "Spotify Premium".to_string(),
                    "free" | "open" => "Spotify Free".to_string(),
                    "" => String::new(),
                    other => format!("Spotify {other}"),
                };
                (self.display_name(), plan, Some((t("Cerrar sesión").to_string(), false)))
            }
            Auth::LoggingIn => {
                let cancelling = self.login_cancelling();
                (
                    t("Iniciando sesión").to_string(),
                    t("Completa el inicio de sesión en el navegador").to_string(),
                    Some(((if cancelling { t("Cancelando…") } else { t("Cancelar") }).to_string(), false)),
                )
            }
            Auth::Connecting { .. } => (self.display_name(), t("Conectando con Spotify…").to_string(), None),
            Auth::LoggedOut => (t("Sin sesión").to_string(), t("Inicia sesión para escuchar y ver tu biblioteca.").to_string(), Some((t("Iniciar sesión").to_string(), true))),
        };
        // Foto (o un círculo con la silueta) y nombre: abren tu perfil.
        let my_id = self.my_id().map(|s| s.to_string()).filter(|_| matches!(auth, Auth::LoggedIn { .. }));
        let img = my_id
            .as_ref()
            .and_then(|id| self.users.get(id).and_then(|u| u.cover(160).map(|s| s.to_string())))
            .or_else(|| self.user.as_ref().and_then(|u| u.cover(160).map(|s| s.to_string())));
        let name_g = galley_truncated(&painter, &title, theme::bold(22.0), k.title, (r.max.x - RIGHT - 160.0 - text_x).max(80.0));
        let name_rect = Rect::from_min_size(pos2(text_x, r.min.y + 49.0 - 18.0), vec2(name_g.size().x, 24.0));
        let link = my_id.is_some();
        let hover = link && ui.interact(pic.union(name_rect), id.with("open"), Sense::click()).hovered();
        if img.is_some() {
            self.cover_in(ui, img.as_deref(), pic, (HERO_PIC / 2.0) as u8);
        } else {
            painter.circle_filled(pic.center(), HERO_PIC / 2.0, if k.dark { Color32::from_gray(52) } else { k.control });
            icons::paint(&painter, Rect::from_center_size(pic.center(), vec2(34.0, 34.0)), k.sub, Icon::Person);
        }
        let name_r = text_on_baseline(&painter, pos2(text_x, r.min.y + 49.0), name_g, k.title);
        if hover {
            painter.line_segment([pos2(name_r.min.x, name_r.max.y - 3.0), pos2(name_r.max.x, name_r.max.y - 3.0)], Stroke::new(1.0, k.title));
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if link && ui.interact(pic.union(name_rect), id.with("open"), Sense::click()).on_hover_text(t("Ver mi perfil")).clicked() {
            if let Some(uid) = my_id.clone() {
                self.actions.push(Action::Go(Page::User(uid)));
            }
        }
        if !sub.is_empty() {
            let g = galley_truncated(&painter, &sub, theme::regular(14.5), k.sub, (r.max.x - RIGHT - 160.0 - text_x).max(80.0));
            text_on_baseline(&painter, pos2(text_x + 1.0, r.min.y + 77.0), g, k.sub);
        }
        if let Some((label, primary)) = button {
            let enabled = !(matches!(auth, Auth::LoggingIn) && self.login_cancelling());
            if pill_button(ui, id.with("button"), r.max.x - RIGHT, cy, &label, primary, enabled, k).clicked() {
                match auth {
                    Auth::LoggedIn { .. } => {
                        self.backend.send(crate::backend::Cmd::Logout);
                        self.go(Page::Home);
                    }
                    Auth::LoggingIn => self.cancel_login(),
                    Auth::LoggedOut => self.login(),
                    Auth::Connecting { .. } => {}
                }
            }
        }
    }

    /// Debajo de la tarjeta de la cuenta, si es gratuita: Spotify no deja reproducir.
    fn free_account_note(&self, ui: &mut egui::Ui, x0: f32, card_w: f32, k: &Ink) {
        let free = matches!(self.auth, Auth::LoggedIn { .. }) && self.user.as_ref().and_then(|u| u.product.as_deref()).is_some_and(|p| p == "free" || p == "open");
        if !free {
            return;
        }
        let g = ui.painter().layout(t("Con una cuenta gratuita Spotify no permite reproducir música en clientes externos.").to_string(), theme::regular(SUB_FONT), k.warn, card_w);
        let w = ui.available_width();
        let (r, _) = ui.allocate_exact_size(vec2(w, g.size().y + 14.0), Sense::hover());
        ui.painter().galley(pos2(x0 + 2.0, r.min.y + 10.0), g, k.warn);
    }

    /// Tarjeta de «Acerca de»: logo, versión y «Buscar actualizaciones»; debajo, el estado de la
    /// actualización si hay algo que decir (y sus botones).
    fn app_info_row(&mut self, ui: &mut egui::Ui, x0: f32, card_w: f32, k: &Ink) {
        let w = ui.available_width();
        let (alloc, _) = ui.allocate_exact_size(vec2(w, HERO_H), Sense::hover());
        let r = Rect::from_min_size(pos2(x0, alloc.min.y), vec2(card_w, HERO_H));
        let painter = ui.painter().clone();
        let pic = Rect::from_min_size(pos2(r.min.x + 20.0, r.min.y + 22.0), vec2(HERO_PIC, HERO_PIC));
        let tex = self.app_logo(ui.ctx(), (HERO_PIC * ui.ctx().pixels_per_point()).round() as u32);
        painter.image(tex, pic, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        let text_x = pic.max.x + 18.0;
        let g = painter.layout_no_wrap("Nanofy".into(), theme::bold(22.0), k.title);
        text_on_baseline(&painter, pos2(text_x, r.min.y + 49.0), g, k.title);
        let g = painter.layout_no_wrap(tr!("Versión {}", crate::update::current_version()), theme::regular(14.5), k.sub);
        text_on_baseline(&painter, pos2(text_x + 1.0, r.min.y + 77.0), g, k.sub);
        let applying = self.update_applying || self.restart_after_exit.is_some();
        if pill_button(ui, ui.id().with("settings_check_updates"), r.max.x - RIGHT, r.center().y, t("Buscar actualizaciones"), false, !self.update_busy && !applying, k).clicked() {
            self.check_updates(true);
        }

        // Estado de la actualización, en su propia fila dentro de la tarjeta.
        let current = crate::update::current_version();
        let Some(view) = about_view(&self.update_stage, self.update.as_ref(), self.update_note.as_ref(), self.update_busy, applying, crate::update::can_self_install(), &current) else {
            return;
        };
        let y = ui.cursor().min.y;
        painter.line_segment([pos2(x0 + TEXT_X, y + 0.5), pos2(x0 + card_w, y + 0.5)], Stroke::new(1.0, k.divider));
        let mut buttons: Vec<(&str, UpdAction, bool)> = Vec::new();
        if let Some((l, a)) = view.primary {
            buttons.push((l, a, true));
        }
        if let Some((l, a)) = view.secondary {
            buttons.push((l, a, false));
        }
        if let Some((l, a)) = view.link {
            buttons.push((l, a, false));
        }
        let btn_w: f32 = buttons.iter().map(|(l, _, _)| painter.layout_no_wrap(l.to_string(), theme::regular(14.5), Color32::WHITE).size().x + 44.0).sum();
        let text_w = (card_w - TEXT_X - RIGHT - 24.0 - btn_w).clamp(160.0, 560.0);
        let color = match view.tone {
            Tone::Text => k.row,
            Tone::Weak => k.sub,
            Tone::Error => super::ERROR_RED,
        };
        // El aviso se guardó en el idioma de entonces: se traduce al enseñarlo.
        let mut text = crate::i18n::t_message(&view.text);
        if view.waiting {
            let phase = (ui.input(|i| i.time) * 4.0) as usize % 4;
            text.push_str(["", ".", "..", "..."][phase]);
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
        let title_g = painter.layout(text, theme::regular(NAME_FONT), color, text_w);
        let detail_g = view.detail.as_ref().map(|d| painter.layout(crate::i18n::t_message(d), theme::regular(SUB_FONT), k.sub, text_w));
        let title_lines = title_g.rows.len().max(1);
        let detail_lines = detail_g.as_ref().map(|g| g.rows.len()).unwrap_or(0);
        let h = if detail_lines > 0 { ROW_H - LINE + LINE * (title_lines - 1 + detail_lines) as f32 } else { ROW_SHORT + LINE * (title_lines - 1) as f32 };
        let (alloc, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
        let row = Rect::from_min_size(pos2(x0, alloc.min.y), vec2(card_w, h));
        let icon = match view.tone {
            Tone::Error => Icon::Close,
            _ if view.waiting => Icon::Hourglass,
            _ if view.primary.is_some() => Icon::Download,
            _ => Icon::CheckCircle,
        };
        icons::paint(&painter, Rect::from_center_size(pos2(x0 + ICON_CX, row.center().y), vec2(ICON_PX, ICON_PX)), if view.tone == Tone::Error { color } else { k.icon }, icon);
        let base = row.min.y + if detail_lines > 0 { 29.0 } else { 35.0 };
        text_on_baseline(&painter, pos2(x0 + TEXT_X, base), title_g, color);
        if let Some(g) = detail_g {
            text_on_baseline(&painter, pos2(x0 + TEXT_X, row.min.y + 50.5 + LINE * (title_lines - 1) as f32), g, k.sub);
        }
        let mut x = row.max.x - RIGHT;
        let mut action = None;
        for (i, (label, a, primary)) in buttons.iter().enumerate() {
            let resp = pill_button(ui, ui.id().with(("settings_update_btn", i)), x, row.center().y, label, *primary, true, k);
            if resp.clicked() {
                action = Some(*a);
            }
            x = resp.rect.min.x - 8.0;
        }
        if let Some(a) = action {
            let ctx = ui.ctx().clone();
            self.run_update_action(&ctx, a);
        }
    }

    /// Logo de Nanofy (assets/nanofy.png) a `px` píxeles.
    fn app_logo(&mut self, ctx: &egui::Context, px: u32) -> egui::TextureId {
        if let Some((size, tex)) = &self.settings_ui.logo {
            if *size == px {
                return tex.id();
            }
        }
        const PNG: &[u8] = include_bytes!("../../assets/nanofy.png");
        let img = image::load_from_memory_with_format(PNG, image::ImageFormat::Png)
            .map(|i| i.resize_exact(px.max(8), px.max(8), image::imageops::FilterType::Lanczos3).to_rgba8())
            .ok();
        let color = match img {
            Some(rgba) => egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()),
            None => egui::ColorImage::new([1, 1], vec![Color32::from_gray(34)]),
        };
        let tex = ctx.load_texture("nanofy-logo", color, egui::TextureOptions::LINEAR);
        let id = tex.id();
        self.settings_ui.logo = Some((px, tex));
        id
    }

    // ------------------------------------------------------------ lo desplegado

    /// App de desarrollador propia, desplegada: para qué sirve, la Redirect URI, su Client ID y
    /// los botones. Solo para las lecturas, con su propia cuota.
    fn dev_app_body(&mut self, ui: &mut egui::Ui, x0: f32, card_w: f32, k: &Ink) {
        let inner = card_w - TEXT_X - RIGHT;
        let w = ui.available_width();
        let painter = ui.painter().clone();
        let explain = painter.layout(
            t("Solo si ya tienes una app en developer.spotify.com y quieres que las lecturas usen su cuota: añádele esta Redirect URI y pega aquí su Client ID.").to_string(),
            theme::regular(SUB_FONT),
            k.sub,
            inner.min(560.0),
        );
        let explain_h = explain.size().y;
        let h = explain_h + 4.0 + 30.0 + 12.0 + CTRL_H + 22.0;
        let (alloc, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
        let top = alloc.min.y;
        let x = x0 + TEXT_X;
        painter.galley(pos2(x, top), explain, k.sub);
        // La URI en verde y monoespaciada, con «Copiar».
        let uri_y = top + explain_h + 4.0 + 15.0;
        let g = painter.layout_no_wrap(crate::webauth::PERSONAL_REDIRECT_URI.to_string(), FontId::monospace(13.0), GREEN);
        let uri_r = text_on_baseline(&painter, pos2(x, uri_y + 4.5), g, GREEN);
        let id = ui.id().with("settings_dev_app");
        let copy_w = (painter.layout_no_wrap(t("Copiar").to_string(), theme::regular(14.5), Color32::WHITE).size().x + 36.0).round();
        if pill_button(ui, id.with("copy"), uri_r.max.x + 12.0 + copy_w, uri_y, t("Copiar"), false, true, k).clicked() {
            self.actions.push(Action::CopyText(crate::webauth::PERSONAL_REDIRECT_URI.to_string(), "URI"));
        }
        // Client ID y botones.
        let fy = uri_y + 15.0 + 12.0 + CTRL_H / 2.0;
        let field = Rect::from_min_size(pos2(x, fy - CTRL_H / 2.0), vec2(FIELD_W + 60.0, CTRL_H));
        painter.rect_filled(field, CornerRadius::same(6), k.field);
        let current = self.settings.client_id.clone();
        let edit = self.settings_ui.client_id_edit.get_or_insert(current);
        let mut c = child_in(ui, field.shrink2(vec2(13.0, 0.0)), Layout::left_to_right(Align::Center));
        let r = c.add(
            egui::TextEdit::singleline(edit)
                .id(id.with("client_id"))
                .hint_text(t("Client ID (32 caracteres)"))
                .frame(egui::Frame::NONE)
                .font(theme::regular(14.5))
                .desired_width(field.width() - 26.0),
        );
        let edge = if r.has_focus() { GREEN.gamma_multiply(0.8) } else { k.field_edge };
        painter.rect_stroke(field, CornerRadius::same(6), Stroke::new(1.0, edge), egui::StrokeKind::Inside);
        let personal = self.api.personal_configured();
        let typed = self.settings_ui.client_id_edit.clone().unwrap_or_default().trim().to_string();
        let label = if personal { t("Reconectar app") } else { t("Conectar mi app") };
        let can = !self.web_busy && typed.len() >= 16;
        // `pill_button` va anclado por su borde derecho: justo detrás del campo.
        let wl = painter.layout_no_wrap(label.to_string(), theme::regular(14.5), Color32::WHITE).size().x + 36.0;
        let bx = field.max.x + 12.0 + wl.round();
        let connect = pill_button(ui, id.with("connect"), bx, fy, label, true, can, k);
        if connect.clicked() {
            self.settings.client_id = typed.clone();
            self.draft.client_id = typed.clone();
            self.settings.save(&self.paths);
            self.web_busy = true;
            self.status(t("Se ha abierto el navegador para autorizar tu app…"));
            self.api.send(crate::api::Req::WebConnectPersonal(typed));
        }
        if personal {
            let wq = painter.layout_no_wrap(t("Quitar").to_string(), theme::regular(14.5), Color32::WHITE).size().x + 36.0;
            if pill_button(ui, id.with("remove"), bx + 8.0 + wq.round(), fy, t("Quitar"), false, !self.web_busy, k).clicked() {
                self.web_busy = true;
                self.api.send(crate::api::Req::WebDisconnectPersonal);
            }
        }
    }

    /// Información técnica, desplegada: memoria, portadas, último fotograma, teclas multimedia y
    /// dónde están los ajustes y la caché.
    fn tech_info_body(&mut self, ui: &mut egui::Ui, x0: f32, card_w: f32, k: &Ink) {
        self.refresh_mem();
        let mut lines: Vec<String> = Vec::new();
        if let Some(m) = self.mem_mb {
            let (n_img, img_mb) = (self.images.resident(), self.images.resident_bytes() as f64 / (1024.0 * 1024.0));
            lines.push(tr!(
                "Memoria en uso {} MB · {} portadas en memoria ({} MB) · último fotograma {} ms",
                format!("{m:.0}"),
                n_img,
                crate::i18n::decimal(format!("{img_mb:.1}")),
                crate::i18n::decimal(format!("{:.1}", self.frame_ms))
            ));
        }
        lines.push(tr!("Teclas multimedia: {}", if self.media.active() { t("activas") } else { t("no disponibles") }));
        lines.push(tr!("Ajustes: {}", self.paths.settings_file().display()));
        lines.push(tr!("Caché: {}", self.paths.cache_dir.display()));
        let painter = ui.painter().clone();
        let inner = card_w - TEXT_X - RIGHT;
        let galleys: Vec<_> = lines.into_iter().map(|l| painter.layout(l, theme::regular(SUB_FONT), k.sub, inner)).collect();
        let h: f32 = galleys.iter().map(|g| g.size().y + 4.0).sum::<f32>() + 20.0;
        let w = ui.available_width();
        let (alloc, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
        let mut y = alloc.min.y;
        for g in galleys {
            let gh = g.size().y;
            painter.galley(pos2(x0 + TEXT_X, y), g, k.sub);
            y += gh + 4.0;
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(2));
    }

    // ------------------------------------------------------------ desplegables

    /// El menú abierto de un desplegable, de cristal como los demás, justo debajo de él.
    fn settings_dropdown(&mut self, ctx: &egui::Context, k: &Ink) {
        let Some(which) = self.settings_ui.dropdown else { return };
        let Some(anchor) = self.settings_ui.drop_rects.get(&which).copied() else {
            self.settings_ui.dropdown = None;
            return;
        };
        // Opciones: (texto, elegida, se puede elegir, nota debajo).
        let options: Vec<(String, bool, bool, Option<&str>)> = match which {
            Drop::Quality => {
                let mut v: Vec<_> = [Quality::Low, Quality::Normal, Quality::High]
                    .iter()
                    .map(|q| (q.label().to_string(), *q == self.settings.quality, true, None))
                    .collect();
                // Sin pérdida no se puede: los FLAC de Spotify van tras su DRM.
                v.push((t("Sin pérdida (FLAC)").to_string(), false, false, Some(t("Solo en las apps de Spotify"))));
                v
            }
            Drop::Language => {
                let mut v = vec![(t("Automático").to_string(), self.settings.language == "auto" || self.settings.language.is_empty(), true, None)];
                v.extend(crate::i18n::languages().into_iter().map(|(code, name)| {
                    let on = self.settings.language == code;
                    (name, on, true, None)
                }));
                v
            }
        };
        let painter_font = theme::semilight(15.5);
        let measure = ctx.fonts_mut(|f| options.iter().map(|o| f.layout_no_wrap(o.0.clone(), painter_font.clone(), Color32::WHITE).size().x).fold(0.0f32, f32::max));
        let row_h = 40.0;
        let note_h = 16.0;
        let w = anchor.width().max(measure + 70.0);
        let h = 12.0 + options.iter().map(|o| row_h + if o.3.is_some() { note_h } else { 0.0 }).sum::<f32>();
        let screen = ctx.content_rect();
        let top = if anchor.max.y + 6.0 + h <= screen.max.y - 6.0 { anchor.max.y + 6.0 } else { (anchor.min.y - 6.0 - h).max(screen.min.y + 6.0) };
        let rect = Rect::from_min_size(pos2(anchor.max.x - w, top), vec2(w, h));
        let mut chosen: Option<usize> = None;
        let mut option_rects: Vec<Rect> = Vec::new();
        egui::Area::new(egui::Id::new("settings_dropdown"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .show(ctx, |ui| {
                ui.allocate_rect(rect, Sense::click());
                let painter = ui.painter().clone();
                super::player_menu::paint_glass(ctx, &painter, rect);
                let mut y = rect.min.y + 6.0;
                for (i, (label, on, enabled, note)) in options.iter().enumerate() {
                    let rh = row_h + if note.is_some() { note_h } else { 0.0 };
                    let row = Rect::from_min_size(pos2(rect.min.x + 6.0, y), vec2(w - 12.0, rh));
                    option_rects.push(row);
                    let resp = ui.interact(row, egui::Id::new(("settings_dropdown_row", i)), if *enabled { Sense::click() } else { Sense::hover() });
                    if *enabled && resp.hovered() {
                        painter.rect_filled(row, CornerRadius::same(4), if k.dark { Color32::from_white_alpha(10) } else { Color32::from_black_alpha(10) });
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    let color = if *on {
                        GREEN
                    } else if *enabled {
                        if k.dark {
                            Color32::from_gray(229)
                        } else {
                            k.row
                        }
                    } else {
                        k.sub
                    };
                    let g = galley_truncated(&painter, label, theme::semilight(15.5), color, w - 64.0);
                    text_on_baseline(&painter, pos2(row.min.x + 12.0, y + 25.5), g, color);
                    if let Some(n) = note {
                        let g = painter.layout_no_wrap(n.to_string(), theme::regular(12.5), k.sub);
                        text_on_baseline(&painter, pos2(row.min.x + 12.0, y + 25.5 + note_h + 1.0), g, k.sub);
                    }
                    if *on {
                        icons::paint(&painter, Rect::from_center_size(pos2(row.max.x - 18.0, y + row_h / 2.0), vec2(18.0, 18.0)), GREEN, Icon::Check);
                    }
                    if *enabled && resp.clicked() {
                        chosen = Some(i);
                    }
                    y += rh;
                }
            });
        for (i, r) in option_rects.into_iter().enumerate() {
            self.settings_ui.rects.insert(format!("{which:?}/option/{i}"), r);
        }
        let pressed_outside = ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|q| !rect.contains(q) && !anchor.contains(q)));
        if chosen.is_some() || pressed_outside || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.settings_ui.dropdown = None;
        }
        if let Some(i) = chosen {
            match which {
                Drop::Quality => {
                    let q = [Quality::Low, Quality::Normal, Quality::High][i.min(2)];
                    self.apply_setting(ctx, |s| s.quality = q);
                }
                Drop::Language => {
                    let code = if i == 0 { "auto".to_string() } else { crate::i18n::languages().get(i - 1).map(|(c, _)| c.clone()).unwrap_or_else(|| "auto".into()) };
                    self.apply_setting(ctx, |s| s.language = code);
                }
            }
        }
    }

    // ------------------------------------------------------------ caché de imágenes

    /// Vuelve a medir la caché de imágenes en disco (en otro hilo).
    pub fn refresh_image_usage(&mut self) {
        let (tx, rx) = mpsc::channel();
        let dir = self.paths.image_cache_dir();
        std::thread::spawn(move || {
            let _ = tx.send(dir_usage(&dir));
        });
        self.settings_ui.image_usage_rx = Some(rx);
    }

    fn poll_image_usage(&mut self) {
        if self.settings_ui.image_usage.is_none() && self.settings_ui.image_usage_rx.is_none() {
            self.refresh_image_usage();
        }
        if let Some(rx) = &self.settings_ui.image_usage_rx {
            match rx.try_recv() {
                Ok(v) => {
                    self.settings_ui.image_usage = Some(v);
                    self.settings_ui.image_usage_rx = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => self.settings_ui.image_usage_rx = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }
}

/// Caché de audio en una posición del deslizador (de 128 en 128 MB; 0 la desactiva).
fn cache_mb_at(f: f32) -> u64 {
    let steps = (CACHE_MAX_MB / CACHE_STEP_MB) as f32;
    ((f.clamp(0.0, 1.0) * steps).round() as u64) * CACHE_STEP_MB
}

/// Escala de la interfaz en una posición del deslizador (de 5 en 5 %).
fn zoom_at(f: f32) -> f32 {
    let z = ZOOM_MIN + f.clamp(0.0, 1.0) * (ZOOM_MAX - ZOOM_MIN);
    (z * 20.0).round() / 20.0
}

/// El interruptor de la app (píldora de 40 × 22, verde con la bola negra encendido), centrado en `c`.
fn paint_switch(ui: &egui::Ui, id: egui::Id, c: Pos2, on: bool, enabled: bool, k: &Ink) {
    let p = theme::palette(ui.ctx());
    let tt = ui.ctx().animate_bool_with_time(id.with("switch"), on, 0.12);
    let track = Rect::from_center_size(c, vec2(40.0, 22.0));
    let off = if k.dark { Color32::from_gray(62) } else { p.hover.lerp_to_gamma(p.text, 0.12) };
    let fill = dim(off.lerp_to_gamma(GREEN, tt), enabled);
    ui.painter().rect_filled(track, CornerRadius::same(11), fill);
    let kx = track.min.x + 11.0 + 18.0 * tt;
    let knob = if on { Color32::BLACK } else if k.dark { Color32::from_gray(235) } else { p.card };
    ui.painter().circle_filled(pos2(kx, track.center().y), 8.0, dim(knob, enabled));
}

/// Deslizador como la línea del reproductor: lo pendiente gris, lo elegido claro y la bolita al
/// acercar el ratón. Devuelve la posición mientras se arrastra y al soltar.
fn settings_slider(ui: &mut egui::Ui, id: egui::Id, zone: Rect, frac: f32, enabled: bool, k: &Ink) -> Option<Event> {
    let resp = ui.interact(zone, id, if enabled { Sense::click_and_drag() } else { Sense::hover() });
    let near = enabled && (resp.hovered() || resp.dragged() || resp.is_pointer_button_down_on());
    let (x0, x1) = (zone.min.x, zone.max.x);
    let y = zone.center().y;
    let pointer_f = resp.interact_pointer_pos().or(resp.hover_pos()).map(|hp| ((hp.x - x0) / (x1 - x0)).clamp(0.0, 1.0));
    let shown = if resp.dragged() || resp.is_pointer_button_down_on() { pointer_f.unwrap_or(frac) } else { frac };
    let xp = x0 + (x1 - x0) * shown.clamp(0.0, 1.0);
    let painter = ui.painter();
    let h = 2.0;
    painter.rect_filled(Rect::from_min_max(pos2(x0, y - h), pos2(x1, y + h)), CornerRadius::same(2), dim(k.track, enabled));
    painter.rect_filled(Rect::from_min_max(pos2(x0, y - h), pos2(xp.max(x0 + 4.0), y + h)), CornerRadius::same(2), dim(k.fill, enabled));
    let knob = ui.ctx().animate_bool_with_time(id.with("knob"), near, 0.12);
    if knob > 0.0 {
        painter.circle_filled(pos2(xp, y), 6.0 * knob, k.fill);
    }
    if near {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if resp.drag_stopped() || (resp.clicked() && !resp.dragged()) {
        return pointer_f.or(Some(shown)).map(Event::SliderDone);
    }
    if resp.dragged() || resp.is_pointer_button_down_on() {
        return pointer_f.map(Event::SliderMove);
    }
    None
}

/// Botón de píldora con el borde derecho en `right`, centrado en `cy`: gris como los chips o,
/// el principal, verde con el texto negro.
#[allow(clippy::too_many_arguments)]
fn pill_button(ui: &mut egui::Ui, id: egui::Id, right: f32, cy: f32, label: &str, primary: bool, enabled: bool, k: &Ink) -> egui::Response {
    let font = theme::regular(14.5);
    let g = ui.painter().layout_no_wrap(label.to_string(), font.clone(), Color32::WHITE);
    let w = (g.size().x + 36.0).round();
    let rect = Rect::from_min_size(pos2(right - w, cy - BTN_H / 2.0), vec2(w, BTN_H));
    let resp = ui.interact(rect, id, if enabled { Sense::click() } else { Sense::hover() });
    let hovered = enabled && resp.hovered();
    let (fill, color) = if primary {
        (if hovered { GREEN.lerp_to_gamma(Color32::WHITE, 0.12) } else { GREEN }, Color32::BLACK)
    } else {
        (if hovered { k.control_hover } else { k.control }, k.control_text)
    };
    let (fill, color) = (dim(fill, enabled), dim(color, enabled));
    ui.painter().rect_filled(rect, CornerRadius::same((BTN_H / 2.0) as u8), fill);
    let g = ui.painter().layout_no_wrap(label.to_string(), font, color);
    text_on_baseline(ui.painter(), pos2(rect.center().x - g.size().x / 2.0, cy + 5.0), g, color);
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Miniatura del tema: la ventana de la app en pequeño (lateral, contenido y barra con el punto
/// verde), en oscuro, en claro o partida en diagonal (Sistema). Medidas de la de 122 × 80 del
/// mockup, en píxeles de `w`×`h`.
fn theme_preview_image(kind: u8, w: usize, h: usize) -> egui::ColorImage {
    let s = w as f32 / THEME_W;
    // Cobertura de un rectángulo redondeado en (x, y) (en puntos del diseño), con borde suave.
    let rrect = |x: f32, y: f32, r: Rect, rad: f32| -> f32 {
        let c = r.center();
        let half = r.size() / 2.0 - vec2(rad, rad);
        let dx = ((x - c.x).abs() - half.x).max(0.0);
        let dy = ((y - c.y).abs() - half.y).max(0.0);
        let inside = ((x - c.x).abs() - half.x).max((y - c.y).abs() - half.y).min(0.0);
        let d = (dx * dx + dy * dy).sqrt() + inside - rad;
        (0.5 - d * s).clamp(0.0, 1.0)
    };
    let over = |base: [f32; 4], c: [f32; 3], a: f32| -> [f32; 4] {
        [base[0] + (c[0] - base[0]) * a, base[1] + (c[1] - base[1]) * a, base[2] + (c[2] - base[2]) * a, base[3] + (1.0 - base[3]) * a]
    };
    let g = |v: f32| [v, v, v];
    let paint = |dark: bool, x: f32, y: f32| -> [f32; 4] {
        let card = Rect::from_min_size(pos2(0.0, 0.0), vec2(THEME_W, THEME_H));
        let mut px = [0.0, 0.0, 0.0, 0.0];
        px = over(px, if dark { g(6.0) } else { [238.0, 238.0, 240.0] }, rrect(x, y, card, 8.0));
        let line = if dark { g(58.0) } else { g(196.0) };
        for ly in [12.4, 19.0] {
            px = over(px, line, rrect(x, y, Rect::from_min_max(pos2(7.0, ly), pos2(22.0, ly + 2.6)), 1.3) * px[3]);
        }
        let content = Rect::from_min_max(pos2(32.0, 9.0), pos2(113.0, 58.7));
        px = over(px, if dark { g(42.0) } else { g(226.0) }, rrect(x, y, content, 3.0) * px[3]);
        px = over(px, if dark { g(4.0) } else { g(255.0) }, rrect(x, y, content.shrink(1.0), 2.0) * px[3]);
        let bar = Rect::from_min_max(pos2(32.0, 63.3), pos2(113.0, 71.3));
        if dark {
            px = over(px, [39.0, 56.0, 48.0], rrect(x, y, bar, 4.0) * px[3]);
        } else {
            px = over(px, g(226.0), rrect(x, y, bar, 4.0) * px[3]);
            px = over(px, g(255.0), rrect(x, y, bar.shrink(1.0), 3.0) * px[3]);
        }
        let dot = Rect::from_center_size(pos2(40.0, 67.3), vec2(5.4, 5.4));
        px = over(px, [30.0, 215.0, 96.0], rrect(x, y, dot, 2.7) * px[3]);
        px
    };
    let mut pixels = Vec::with_capacity(w * h);
    for j in 0..h {
        for i in 0..w {
            let (x, y) = ((i as f32 + 0.5) / s, (j as f32 + 0.5) / s);
            let c = match kind {
                1 => paint(true, x, y),
                2 => paint(false, x, y),
                _ => {
                    // Diagonal de la esquina de abajo a la izquierda a la de arriba a la derecha:
                    // arriba, oscuro; abajo, claro.
                    let (d, l) = (paint(true, x, y), paint(false, x, y));
                    let dist = (THEME_H * x + THEME_W * y - THEME_W * THEME_H) / (THEME_W * THEME_W + THEME_H * THEME_H).sqrt();
                    let a = (0.5 + dist * s).clamp(0.0, 1.0);
                    [d[0] + (l[0] - d[0]) * a, d[1] + (l[1] - d[1]) * a, d[2] + (l[2] - d[2]) * a, d[3] + (l[3] - d[3]) * a]
                }
            };
            let a = c[3].clamp(0.0, 1.0);
            pixels.push(Color32::from_rgba_unmultiplied(c[0].round() as u8, c[1].round() as u8, c[2].round() as u8, (a * 255.0).round() as u8));
        }
    }
    egui::ColorImage::new([w, h], pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Buscar sin tildes ni mayúsculas.
    #[test]
    fn buscar_sin_tildes() {
        assert_eq!(fold("Reproducción ÁÉÍÓÚ Ñ"), "reproduccion aeiou n");
        assert!(fold("Calidad de audio").contains(&fold("CALIDAD")));
    }

    /// Los deslizadores van por pasos y no se salen de su rango.
    #[test]
    fn pasos_de_los_deslizadores() {
        assert_eq!(cache_mb_at(0.0), 0);
        assert_eq!(cache_mb_at(0.25), 1024);
        assert_eq!(cache_mb_at(1.0), CACHE_MAX_MB);
        assert_eq!(cache_mb_at(2.0), CACHE_MAX_MB);
        assert_eq!(cache_mb_at(0.26) % CACHE_STEP_MB, 0);
        assert!((zoom_at(0.25) - 1.0).abs() < 1e-4);
        assert!((zoom_at(0.0) - ZOOM_MIN).abs() < 1e-4);
        assert!((zoom_at(1.0) - ZOOM_MAX).abs() < 1e-4);
        assert_eq!(fmt_cache(0), "Desactivada");
        assert_eq!(fmt_cache(256), "256 MB");
        assert_eq!(fmt_cache(1024), "1 GB");
        assert_eq!(fmt_cache(1536), "1,5 GB");
    }

    /// Cada fila sale en una sola sección y cada pestaña tiene alguna.
    #[test]
    fn secciones_bien_formadas() {
        let mut seen = std::collections::HashSet::new();
        for s in SECTIONS {
            assert!((s.tab as usize) < TABS.len());
            for r in s.rows {
                assert!(seen.insert(*r), "{r:?} en dos secciones");
            }
        }
        for tab in 0..TABS.len() as u8 {
            assert!(SECTIONS.iter().any(|s| s.tab == tab), "pestaña {tab} vacía");
        }
    }

    /// Las miniaturas del tema: transparentes en las esquinas, el punto verde y, en «Sistema»,
    /// oscuro arriba a la izquierda y claro abajo a la derecha.
    #[test]
    fn miniaturas_del_tema() {
        let img = theme_preview_image(0, 122, 80);
        let at = |x: usize, y: usize| img.pixels[y * 122 + x];
        assert_eq!(at(0, 0).a(), 0, "esquina redondeada");
        assert!(at(20, 5).r() < 20, "arriba a la izquierda, oscuro");
        assert!(at(110, 76).r() > 200, "abajo a la derecha, claro");
        assert!(at(40, 67).g() > 180 && at(40, 67).r() < 80, "punto verde");
        let dark = theme_preview_image(1, 122, 80);
        assert!(dark.pixels[40 * 122 + 60].r() < 20);
        let light = theme_preview_image(2, 122, 80);
        assert!(light.pixels[40 * 122 + 60].r() > 240);
    }
}
