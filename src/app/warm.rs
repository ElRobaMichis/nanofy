//! Precarga inteligente desde la interfaz: qué canción preparar mientras el usuario decide.
//!
//! - Ratón encima de una fila `HOVER_DELAY` (150 ms): metadatos y ubicación del fichero, sin bajar
//!   audio (`Cmd::Warm`). Menos tiempo es solo pasar por encima camino de otra cosa.
//! - Botón de reproducir de una fila apretado (antes de soltarlo): la canción entera, con su primer
//!   trozo (`Cmd::WarmHead`). Entre apretar y soltar pasan 80-150 ms que ya cuentan.
//!
//! Lo que ya se pidió hace poco no se vuelve a pedir (`REWARM_AFTER`): pasar el ratón arriba y
//! abajo por una lista no repite nada. Quién puede precargar (ajuste, cuenta, otro dispositivo,
//! Jam, una carga en curso) lo decide la app; aquí solo el cuándo.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Ratón quieto sobre una fila este tiempo: probablemente la va a pulsar.
pub const HOVER_DELAY: Duration = Duration::from_millis(150);
/// Una canción ya preparada no se vuelve a pedir antes de esto (lo preparado dura más en la
/// memoria del reproductor).
pub const REWARM_AFTER: Duration = Duration::from_secs(5 * 60);
/// Canciones que se recuerdan como ya pedidas.
const RECENT_CAP: usize = 64;

/// Lo que toca hacer en este fotograma.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Step {
    /// Preparar metadatos y ubicación de esta canción (`Cmd::Warm`).
    pub hover: Option<String>,
    /// Preparar entera esta canción (`Cmd::WarmHead`).
    pub press: Option<String>,
    /// Volver a mirar dentro de esto aunque nada se mueva (el ratón quieto no repinta).
    pub wait: Option<Duration>,
}

#[derive(Default)]
pub struct WarmTracker {
    /// Canción bajo el ratón y desde cuándo.
    hovering: Option<(String, Instant)>,
    /// Ya se decidió qué hacer con este rato encima de la misma fila.
    hover_done: bool,
    /// Botón de reproducir apretado en el fotograma anterior (solo cuenta el momento de apretar).
    pressing: Option<String>,
    /// Pedidas hace poco, para no repetirlas.
    recent: VecDeque<(String, Instant)>,
}

impl WarmTracker {
    /// Una vez por fotograma: la canción con el ratón encima y la del botón de reproducir apretado
    /// (si alguna). Sin precarga posible, `None` en las dos: así se olvida el rato que llevaba.
    pub fn frame(&mut self, hover: Option<&str>, press: Option<&str>, now: Instant) -> Step {
        let mut step = Step::default();

        // Apretar: solo el fotograma en que empieza.
        if press.is_some() && press != self.pressing.as_deref() {
            if let Some(uri) = press {
                step.press = Some(uri.to_string());
                // Entera ya incluye lo del ratón encima.
                self.remember(uri, now);
                self.hover_done = true;
            }
        }
        self.pressing = press.map(str::to_string);

        match hover {
            None => {
                self.hovering = None;
                self.hover_done = false;
            }
            Some(uri) => {
                if self.hovering.as_ref().map(|(u, _)| u.as_str()) != Some(uri) {
                    self.hovering = Some((uri.to_string(), now));
                    self.hover_done = step.press.as_deref() == Some(uri);
                }
                if !self.hover_done {
                    let since = self.hovering.as_ref().map_or(now, |(_, at)| *at);
                    let waited = now.saturating_duration_since(since);
                    if waited < HOVER_DELAY {
                        step.wait = Some(HOVER_DELAY - waited);
                    } else {
                        self.hover_done = true;
                        if !self.recently(uri, now) {
                            self.remember(uri, now);
                            step.hover = Some(uri.to_string());
                        }
                    }
                }
            }
        }
        step
    }

    fn recently(&self, uri: &str, now: Instant) -> bool {
        self.recent
            .iter()
            .any(|(u, at)| u == uri && now.saturating_duration_since(*at) < REWARM_AFTER)
    }

    fn remember(&mut self, uri: &str, now: Instant) {
        self.recent.retain(|(u, _)| u != uri);
        self.recent.push_back((uri.to_string(), now));
        while self.recent.len() > RECENT_CAP {
            self.recent.pop_front();
        }
    }

    /// Olvida lo ya pedido (al cambiar de cuenta o de calidad: lo preparado ya no vale).
    pub fn forget(&mut self) {
        self.recent.clear();
    }
}

/// ¿Es algo que el reproductor puede preparar (una canción o un episodio de Spotify)?
pub fn warmable(uri: &str) -> bool {
    uri.starts_with("spotify:track:") || uri.starts_with("spotify:episode:")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn pasar_por_encima_no_precarga_quedarse_si() {
        let t0 = Instant::now();
        let mut w = WarmTracker::default();
        // Fila «a» 100 ms y luego «b»: «a» no se pide.
        assert_eq!(w.frame(Some("a"), None, t0).wait, Some(HOVER_DELAY));
        assert_eq!(w.frame(Some("a"), None, t0 + ms(100)).hover, None);
        let s = w.frame(Some("b"), None, t0 + ms(110));
        assert_eq!(s.hover, None);
        assert_eq!(s.wait, Some(HOVER_DELAY));
        // «b» llega a 150 ms: se pide una sola vez.
        assert_eq!(w.frame(Some("b"), None, t0 + ms(260)).hover.as_deref(), Some("b"));
        assert_eq!(w.frame(Some("b"), None, t0 + ms(400)), Step::default());
    }

    #[test]
    fn volver_a_la_misma_fila_no_la_repite() {
        let t0 = Instant::now();
        let mut w = WarmTracker::default();
        w.frame(Some("a"), None, t0);
        assert!(w.frame(Some("a"), None, t0 + HOVER_DELAY).hover.is_some());
        w.frame(None, None, t0 + ms(300));
        w.frame(Some("a"), None, t0 + ms(400));
        assert_eq!(w.frame(Some("a"), None, t0 + ms(600)).hover, None);
        // Pasado REWARM_AFTER, sí.
        let later = t0 + REWARM_AFTER + ms(400);
        w.frame(None, None, later);
        w.frame(Some("a"), None, later);
        assert_eq!(w.frame(Some("a"), None, later + HOVER_DELAY).hover.as_deref(), Some("a"));
    }

    #[test]
    fn apretar_el_boton_prepara_entera_una_vez() {
        let t0 = Instant::now();
        let mut w = WarmTracker::default();
        let s = w.frame(Some("a"), Some("a"), t0);
        assert_eq!(s.press.as_deref(), Some("a"));
        // Mientras sigue apretado, nada más; y ya no hace falta la de ratón encima.
        assert_eq!(w.frame(Some("a"), Some("a"), t0 + ms(80)), Step::default());
        assert_eq!(w.frame(Some("a"), None, t0 + ms(300)), Step::default());
        // Soltar y volver a apretar es otra vez.
        assert_eq!(w.frame(Some("a"), Some("a"), t0 + ms(400)).press.as_deref(), Some("a"));
    }

    #[test]
    fn sin_precarga_posible_se_olvida_el_rato() {
        let t0 = Instant::now();
        let mut w = WarmTracker::default();
        w.frame(Some("a"), None, t0);
        // La app no deja precargar (p. ej. carga en curso): el rato se pierde.
        w.frame(None, None, t0 + ms(100));
        let s = w.frame(Some("a"), None, t0 + ms(200));
        assert_eq!(s.hover, None);
        assert_eq!(s.wait, Some(HOVER_DELAY));
    }

    #[test]
    fn la_lista_de_pedidas_no_crece_sin_fin() {
        let t0 = Instant::now();
        let mut w = WarmTracker::default();
        for i in 0..200u64 {
            let uri = format!("spotify:track:{i}");
            let at = t0 + ms(i * 200);
            w.frame(Some(&uri), None, at);
            assert!(w.frame(Some(&uri), None, at + HOVER_DELAY).hover.is_some());
        }
        assert!(w.recent.len() <= RECENT_CAP);
        w.forget();
        assert!(w.recent.is_empty());
    }

    #[test]
    fn solo_canciones_y_episodios() {
        assert!(warmable("spotify:track:abc"));
        assert!(warmable("spotify:episode:abc"));
        assert!(!warmable("spotify:local:a:b:c:1"));
        assert!(!warmable("spotify:album:abc"));
    }
}
