//! Esperar a un hilo con plazo (Nanofy). `JoinHandle::join` espera sin límite: al soltar el
//! reproductor (cerrar la app, reconectar, «Reiniciar») un hilo de carga atascado en una espera
//! síncrona (una lectura de la CDN sin red, una clave que no llega) bloqueaba con él al que lo
//! soltaba, un hilo del runtime del backend.
//!
//! Sin dependencias ni rutas `crate::`: el binario incluye este fichero con `#[path]` para que
//! `cargo test` corra sus pruebas (las de las dependencias no se ejecutan desde el binario).

use std::{
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// Cada cuánto se mira si el hilo ya terminó: la librería estándar no tiene `join` con plazo.
const POLL: Duration = Duration::from_millis(5);

/// Espera a que `handle` termine, como mucho hasta `deadline`. `Some` con lo que devolvió el hilo
/// (o su pánico) si terminó; `None` si seguía en marcha: entonces se suelta (sigue solo y nadie lo
/// espera), así que lo que necesite para terminar tiene que llevarlo consigo.
pub fn join_by<T>(handle: JoinHandle<T>, deadline: Instant) -> Option<thread::Result<T>> {
    loop {
        if handle.is_finished() {
            return Some(handle.join());
        }
        let now = Instant::now();
        if now >= deadline {
            return None;
        }
        thread::sleep(POLL.min(deadline - now));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn un_hilo_que_termina_a_tiempo_se_espera_y_da_su_resultado() {
        let h = thread::spawn(|| {
            thread::sleep(ms(20));
            7
        });
        let t0 = Instant::now();
        assert_eq!(join_by(h, t0 + ms(2000)).map(|r| r.ok()), Some(Some(7)));
        assert!(t0.elapsed() < ms(1000), "{:?}", t0.elapsed());
    }

    #[test]
    fn un_hilo_atascado_no_retiene_mas_alla_del_plazo() {
        let soltar = Arc::new(AtomicBool::new(false));
        let s = soltar.clone();
        let h = thread::spawn(move || {
            // Como una carga en una espera larga: solo termina cuando se le deja.
            while !s.load(Ordering::Relaxed) {
                thread::sleep(ms(5));
            }
        });
        let t0 = Instant::now();
        assert!(join_by(h, t0 + ms(100)).is_none());
        let tardado = t0.elapsed();
        assert!(tardado >= ms(100) && tardado < ms(1000), "{tardado:?}");
        soltar.store(true, Ordering::Relaxed);
    }

    #[test]
    fn un_hilo_que_ya_termino_no_espera_nada_aunque_el_plazo_haya_pasado() {
        let plazo = Instant::now();
        let h = thread::spawn(|| ());
        while !h.is_finished() {
            thread::sleep(ms(1));
        }
        assert!(Instant::now() > plazo);
        assert!(matches!(join_by(h, plazo), Some(Ok(()))));
    }

    #[test]
    fn el_panico_del_hilo_llega_como_error() {
        let h = thread::spawn(|| panic!("prueba: el hilo falla"));
        assert!(matches!(join_by(h, Instant::now() + ms(2000)), Some(Err(_))));
    }
}
