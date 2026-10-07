//! Cortacircuitos de saltos automáticos (Nanofy): cuando varias canciones seguidas fallan de
//! verdad (no disponibles, sin formato…), Spirc deja de saltar a la siguiente y se detiene. Sin
//! esto, una lista entera que no se puede reproducir se recorría en milisegundos marcándolo todo
//! como no disponible (la cascada de «Skipping to next track» de los registros).
//!
//! No usa nada del resto del crate, para que sus pruebas corran en el binario de Nanofy.

use std::time::{Duration, Instant};

/// Fallos seguidos que detienen la reproducción.
pub const MAX_FAILURES: usize = 5;
/// Ventana en la que cuentan: fallos más separados no son una cascada.
pub const WINDOW: Duration = Duration::from_secs(20);

/// Lleva la cuenta de los fallos definitivos de la canción actual que acabarían en un salto
/// automático.
#[derive(Debug, Default)]
pub struct SkipBreaker {
    /// Momentos de los fallos recientes, del más antiguo al más nuevo.
    failures: Vec<Instant>,
}

impl SkipBreaker {
    pub const fn new() -> Self {
        Self {
            failures: Vec::new(),
        }
    }

    /// Una canción falló en `now`. `true` si con ella ya son `MAX_FAILURES` en `WINDOW`: hay que
    /// detenerse en vez de saltar (y la cuenta vuelve a empezar). `false`: se puede saltar.
    pub fn trip(&mut self, now: Instant) -> bool {
        self.failures
            .retain(|t| now.saturating_duration_since(*t) < WINDOW);
        self.failures.push(now);
        if self.failures.len() >= MAX_FAILURES {
            self.failures.clear();
            true
        } else {
            false
        }
    }

    /// Fallos que cuentan ahora mismo.
    pub fn count(&self, now: Instant) -> usize {
        self.failures
            .iter()
            .filter(|t| now.saturating_duration_since(**t) < WINDOW)
            .count()
    }

    /// Algo sonó, o el usuario pidió otra cosa: lo de antes ya no es una cascada.
    pub fn reset(&mut self) {
        self.failures.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn cinco_fallos_seguidos_detienen() {
        let t0 = Instant::now();
        let mut b = SkipBreaker::new();
        // Cuatro saltos, el quinto fallo se detiene: la cascada de ~30 saltos en 4 ms ya no pasa.
        for i in 0..4 {
            assert!(!b.trip(t0 + ms(i)), "fallo {} aún salta", i + 1);
        }
        assert_eq!(b.count(t0 + ms(4)), 4);
        assert!(b.trip(t0 + ms(4)));
        // Tras detenerse la cuenta empieza de cero.
        assert_eq!(b.count(t0 + ms(5)), 0);
        assert!(!b.trip(t0 + ms(5)));
    }

    #[test]
    fn fallos_separados_no_son_cascada() {
        let t0 = Instant::now();
        let mut b = SkipBreaker::new();
        // Uno cada 6 s: en 20 s nunca hay cinco.
        for i in 0..20 {
            assert!(!b.trip(t0 + ms(i * 6000)), "fallo {i}");
        }
        // Justo en el límite de la ventana el más viejo ya no cuenta.
        let mut b = SkipBreaker::new();
        for i in 0..4 {
            assert!(!b.trip(t0 + ms(i * 1000)));
        }
        assert!(!b.trip(t0 + ms(20_000)), "el primero (0 s) ya salió de la ventana");
        assert!(b.trip(t0 + ms(20_500)));
    }

    #[test]
    fn algo_que_suena_reinicia_la_cuenta() {
        let t0 = Instant::now();
        let mut b = SkipBreaker::new();
        for i in 0..4 {
            assert!(!b.trip(t0 + ms(i)));
        }
        b.reset();
        for i in 0..4 {
            assert!(!b.trip(t0 + ms(10 + i)));
        }
        assert!(b.trip(t0 + ms(20)));
    }
}
