//! Paleta, tipografía y estilo de egui de la interfaz.
//!
//! Fondo casi negro, contenido en tarjetas redondeadas ligeramente más claras, acento verde
//! y una tipografía geométrica del sistema con familia negrita propia para los títulos.

use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke};

use crate::config::Theme;

pub const GREEN: Color32 = Color32::from_rgb(30, 215, 96);
pub const GREEN_DARK: Color32 = Color32::from_rgb(20, 90, 50);
pub const RED: Color32 = Color32::from_rgb(240, 90, 90);
pub const BLUE: Color32 = Color32::from_rgb(70, 140, 255);

#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct Palette {
    pub dark: bool,
    /// Fondo de la ventana (detrás de las tarjetas).
    pub bg: Color32,
    /// Tarjetas: contenido, barra del reproductor, paneles laterales.
    pub card: Color32,
    /// Elementos dentro de una tarjeta (filas, campos, chips).
    pub card2: Color32,
    pub hover: Color32,
    pub border: Color32,
    pub text: Color32,
    pub weak: Color32,
    pub faint: Color32,
}

impl Palette {
    pub fn new(dark: bool) -> Self {
        if dark {
            Self {
                dark,
                bg: Color32::from_rgb(9, 9, 9),
                card: Color32::from_rgb(20, 20, 20),
                card2: Color32::from_rgb(30, 30, 30),
                hover: Color32::from_rgb(38, 38, 38),
                border: Color32::from_rgb(36, 36, 36),
                text: Color32::from_rgb(242, 242, 242),
                weak: Color32::from_rgb(160, 160, 160),
                faint: Color32::from_rgb(100, 100, 100),
            }
        } else {
            Self {
                dark,
                bg: Color32::from_rgb(238, 238, 240),
                card: Color32::WHITE,
                card2: Color32::from_rgb(244, 244, 246),
                hover: Color32::from_rgb(232, 232, 236),
                border: Color32::from_rgb(222, 222, 226),
                text: Color32::from_rgb(20, 20, 20),
                weak: Color32::from_rgb(105, 105, 110),
                faint: Color32::from_rgb(170, 170, 175),
            }
        }
    }
}

pub fn palette(ctx: &egui::Context) -> Palette {
    Palette::new(ctx.theme() == egui::Theme::Dark)
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

pub fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// Aplica tema, escala y estilo visual.
pub fn apply(ctx: &egui::Context, theme: Theme, zoom: f32) {
    match theme {
        Theme::System => ctx.set_theme(egui::ThemePreference::System),
        Theme::Dark => ctx.set_theme(egui::ThemePreference::Dark),
        Theme::Light => ctx.set_theme(egui::ThemePreference::Light),
    }
    ctx.set_zoom_factor(zoom);
    for dark in [true, false] {
        let p = Palette::new(dark);
        let mut style = (*ctx.style_of(if dark { egui::Theme::Dark } else { egui::Theme::Light })).clone();
        let v = &mut style.visuals;
        *v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        v.override_text_color = Some(p.text);
        v.panel_fill = p.bg;
        // Menús y ventanas: gris uniforme, sin borde marcado.
        v.window_fill = p.card;
        v.extreme_bg_color = p.card2;
        v.faint_bg_color = p.card2;
        v.code_bg_color = p.card2;
        v.window_stroke = Stroke::new(1.0, p.border);
        v.window_corner_radius = CornerRadius::same(14);
        v.menu_corner_radius = CornerRadius::same(14);
        v.popup_shadow.color = Color32::from_black_alpha(if dark { 120 } else { 40 });
        v.window_shadow.color = Color32::from_black_alpha(if dark { 140 } else { 50 });
        v.selection.bg_fill = GREEN.gamma_multiply(0.35);
        v.selection.stroke = Stroke::new(1.0, GREEN);
        v.hyperlink_color = GREEN;
        v.widgets.noninteractive.bg_fill = p.card;
        v.widgets.noninteractive.weak_bg_fill = p.card2;
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
        v.widgets.inactive.bg_fill = p.card2;
        v.widgets.inactive.weak_bg_fill = p.card2;
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text);
        v.widgets.inactive.bg_stroke = Stroke::NONE;
        v.widgets.hovered.bg_fill = p.hover;
        v.widgets.hovered.weak_bg_fill = p.hover;
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, p.text);
        v.widgets.hovered.bg_stroke = Stroke::NONE;
        v.widgets.active.bg_fill = p.hover;
        v.widgets.active.weak_bg_fill = p.hover;
        v.widgets.active.fg_stroke = Stroke::new(1.0, p.text);
        v.widgets.active.bg_stroke = Stroke::NONE;
        v.widgets.open.bg_fill = p.hover;
        v.widgets.open.weak_bg_fill = p.hover;
        v.widgets.open.fg_stroke = Stroke::new(1.0, p.text);
        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.corner_radius = CornerRadius::same(10);
            w.expansion = 0.0;
        }
        v.slider_trailing_fill = true;
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.menu_margin = egui::Margin::same(8);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size = egui::vec2(36.0, 24.0);
        style.spacing.slider_rail_height = 4.0;
        style.spacing.scroll.bar_width = 6.0;
        style.spacing.scroll.floating = true;
        style.interaction.selectable_labels = false;
        style.text_styles.insert(egui::TextStyle::Body, regular(14.0));
        style.text_styles.insert(egui::TextStyle::Button, regular(14.0));
        style.text_styles.insert(egui::TextStyle::Small, regular(12.0));
        style.text_styles.insert(egui::TextStyle::Heading, bold(22.0));
        style.text_styles.insert(egui::TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace));
        ctx.set_style_of(if dark { egui::Theme::Dark } else { egui::Theme::Light }, style);
    }
}
