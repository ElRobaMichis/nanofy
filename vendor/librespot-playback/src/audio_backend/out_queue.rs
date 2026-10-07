//! Lo que la salida de audio tiene en cola sin sonar, y la entrada suave tras vaciarla (Nanofy).
//!
//! Pausar ya no espera a que la cola de rodio (~0,3-0,5 s de audio decodificado) termine de sonar:
//! la salida se calla al instante y lo encolado se queda para reanudar justo ahí. Pero el
//! decodificador va por delante de lo que se oye en esa cola, así que la posición de la pausa es
//! la del decodificador menos lo que aún no sonó. Para saberlo se cuenta lo que se entrega a la
//! salida y lo que el dispositivo va tomando (`QueueClock`). Si la cola se pierde en la pausa (la
//! salida se suelta a los 5 s, o se reabre en otro dispositivo), al reanudar hay que volver a
//! decodificar desde lo oído (`resume_from`); si no, se saltaría ese trozo.
//!
//! Vaciar la cola (buscar, otra canción) corta el audio a media onda: lo siguiente entra con una
//! rampa de 5 ms (`FadeIn`) para que no chasquee.
//!
//! Sin dependencias ni rutas `crate::`: el binario incluye este fichero con `#[path]` para que
//! `cargo test` corra sus pruebas (las de las dependencias no se ejecutan desde el binario).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Duración de la entrada suave tras vaciar la cola: no se oye como fundido y basta para que el
/// corte no suene a chasquido.
pub const FADE_IN_MS: u32 = 5;

/// Muestras que el hilo de audio cuenta antes de publicarlas (una operación atómica por lote y
/// no por muestra). La cuenta de la cola se equivoca como mucho en esto: 2,9 ms a 44,1 kHz estéreo.
pub const COUNT_BATCH: u64 = 256;

/// Si al reanudar falta en la cola menos que esto de lo que había al pausar, se da por intacta
/// (lo que el dispositivo aún tomó en los 5 ms que tarda rodio en enterarse de la pausa).
pub const RESYNC_TOLERANCE_MS: u32 = 20;

/// Lo entregado a la salida y lo que el dispositivo ya ha tomado, en muestras entrelazadas a la
/// frecuencia y con los canales de los búferes que se entregan. La diferencia no ha sonado aún.
#[derive(Debug)]
pub struct QueueClock {
    appended: u64,
    consumed: Arc<AtomicU64>,
    samples_per_sec: u64,
}

impl QueueClock {
    pub fn new(rate: u32, channels: u16) -> Self {
        Self {
            appended: 0,
            consumed: Arc::new(AtomicU64::new(0)),
            samples_per_sec: (rate as u64 * channels.max(1) as u64).max(1),
        }
    }

    /// Contador para el búfer que se va a entregar: lo lleva consigo al hilo de audio.
    pub fn counter(&self) -> ConsumeCounter {
        ConsumeCounter {
            shared: self.consumed.clone(),
            pending: 0,
        }
    }

    /// Se entregaron `samples` muestras más.
    pub fn on_append(&mut self, samples: usize) {
        self.appended += samples as u64;
    }

    pub fn queued_samples(&self) -> u64 {
        self.appended
            .saturating_sub(self.consumed.load(Ordering::Relaxed))
    }

    /// Audio en cola que aún no ha sonado, en ms.
    pub fn queued_ms(&self) -> u32 {
        u32::try_from(self.queued_samples() * 1000 / self.samples_per_sec).unwrap_or(u32::MAX)
    }
}

/// Lado del hilo de audio de `QueueClock`: cuenta cada muestra que el dispositivo toma de un
/// búfer y la publica por lotes. Al terminar el búfer, o al soltarlo, publica lo que le quede.
#[derive(Debug)]
pub struct ConsumeCounter {
    shared: Arc<AtomicU64>,
    pending: u64,
}

impl ConsumeCounter {
    #[inline]
    pub fn tick(&mut self) {
        self.pending += 1;
        if self.pending >= COUNT_BATCH {
            self.publish();
        }
    }

    pub fn publish(&mut self) {
        if self.pending > 0 {
            self.shared.fetch_add(self.pending, Ordering::Relaxed);
            self.pending = 0;
        }
    }
}

impl Drop for ConsumeCounter {
    fn drop(&mut self) {
        self.publish();
    }
}

/// Entrada suave de `FADE_IN_MS` sobre audio entrelazado, con forma de coseno alzado (sin
/// esquinas al empezar ni al acabar). Avanza por marco: los canales de un mismo marco llevan la
/// misma ganancia. Partir el audio en trozos de cualquier tamaño da lo mismo que de una vez.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FadeIn {
    done: u32,
    frames: u32,
}

impl FadeIn {
    /// Rampa para audio a `rate` Hz.
    pub fn new(rate: u32) -> Self {
        Self {
            done: 0,
            frames: (rate as u64 * FADE_IN_MS as u64 / 1000).max(1) as u32,
        }
    }

    pub fn finished(&self) -> bool {
        self.done >= self.frames
    }

    /// Ganancia del marco `k` (desde 0): sube de casi 0 a exactamente 1 en el último.
    fn gain(&self, k: u32) -> f64 {
        if k + 1 >= self.frames {
            1.0
        } else {
            let x = (k + 1) as f64 / self.frames as f64;
            0.5 - 0.5 * (std::f64::consts::PI * x).cos()
        }
    }

    /// Aplica lo que quede de rampa al principio de `samples`; lo que venga detrás no se toca.
    pub fn apply(&mut self, samples: &mut [f64], channels: usize) {
        let channels = channels.max(1);
        for frame in samples.chunks_mut(channels) {
            if self.finished() {
                break;
            }
            let g = self.gain(self.done);
            for s in frame {
                *s *= g;
            }
            self.done += 1;
        }
    }
}

/// Silencio que se encola delante del audio cuando la cola de rodio está vacía (al abrir la
/// salida, tras vaciarla al buscar o cambiar de canción, tras escurrirla entre canciones o tras
/// quedarse sin datos). Es el `THRESHOLD` de la cola de rodio 0.21: con la cola vacía, su mezclador
/// toma el formato de la fuente vacía o del relleno de silencio que acaba de terminar (mono, 48 o
/// 44,1 kHz) y con él lee las 512 muestras siguientes. Si fueran música, sonarían ~11 ms a media
/// velocidad y con los canales cruzados (un chasquido áspero al empezar, en cada búsqueda y en
/// cada «siguiente»); así son ceros y da igual cómo se lean. Cuesta ~5 ms de silencio en esos
/// momentos y nada mientras suena, porque la cola nunca está vacía.
pub const SPAN_GUARD_SAMPLES: usize = 512;

/// Muestras de silencio que hay que encolar antes del siguiente búfer (`SPAN_GUARD_SAMPLES` si la
/// cola de rodio está vacía, 0 si no).
pub fn span_guard(queue_empty: bool) -> usize {
    if queue_empty { SPAN_GUARD_SAMPLES } else { 0 }
}

/// Posición que se oyó: la del decodificador menos lo que aún esperaba en la cola de la salida.
pub fn heard_ms(stream_position_ms: u32, queued_ms: u32) -> u32 {
    stream_position_ms.saturating_sub(queued_ms)
}

/// Al reanudar tras una pausa. Al pausar quedaban `queued_at_pause_ms` en la cola de la salida
/// sin sonar; ahora quedan `queued_now_ms`. Si se perdieron (se soltó la salida, se reabrió en
/// otro dispositivo), devuelve desde dónde hay que volver a decodificar: lo que de verdad se oyó.
/// `None`: la cola sigue ahí y basta con reanudarla.
pub fn resume_from(stream_position_ms: u32, queued_at_pause_ms: u32, queued_now_ms: u32) -> Option<u32> {
    let lost = queued_at_pause_ms.saturating_sub(queued_now_ms);
    (lost > RESYNC_TOLERANCE_MS).then(|| heard_ms(stream_position_ms, queued_at_pause_ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44_100;

    #[test]
    fn el_silencio_de_guarda_cubre_la_lectura_con_formato_ajeno() {
        // Lo que rodio 0.21 lee con el formato equivocado tras la cola vacía: su `THRESHOLD`.
        assert_eq!(SPAN_GUARD_SAMPLES, 512);
        // Marcos enteros en mono y en estéreo: el audio de detrás no se desalinea de canal.
        assert_eq!(SPAN_GUARD_SAMPLES % 2, 0);
        assert_eq!(span_guard(true), SPAN_GUARD_SAMPLES);
        assert_eq!(span_guard(false), 0, "mientras suena no se añade nada");
        // Como mucho ~6 ms de silencio de más al empezar, también en el peor caso (44,1 kHz).
        let mut clock = QueueClock::new(RATE, 2);
        clock.on_append(span_guard(true));
        assert!(clock.queued_ms() <= 6, "{} ms", clock.queued_ms());
    }

    /// El dispositivo toma `n` muestras de un búfer.
    fn consume(c: &mut ConsumeCounter, n: usize) {
        for _ in 0..n {
            c.tick();
        }
    }

    #[test]
    fn la_cola_es_lo_entregado_menos_lo_tomado() {
        let mut clock = QueueClock::new(RATE, 2);
        assert_eq!(clock.queued_ms(), 0);
        // Un segundo de estéreo entregado en dos búferes.
        clock.on_append(RATE as usize);
        clock.on_append(RATE as usize);
        assert_eq!(clock.queued_ms(), 1000);
        let mut a = clock.counter();
        consume(&mut a, RATE as usize);
        // Sin publicar el último lote la cuenta se equivoca como mucho en un lote (< 3 ms).
        let ms = clock.queued_ms();
        assert!((500..=503).contains(&ms), "{ms}");
        a.publish();
        assert_eq!(clock.queued_ms(), 500);
        // Al soltar el contador (búfer terminado o descartado) se publica lo que quedara.
        let mut b = clock.counter();
        consume(&mut b, 100);
        drop(b);
        assert_eq!(clock.queued_samples(), RATE as u64 - 100);
    }

    #[test]
    fn la_cola_nunca_es_negativa_y_respeta_la_frecuencia_de_salida() {
        let mut clock = QueueClock::new(48_000, 1);
        clock.on_append(4_800);
        assert_eq!(clock.queued_ms(), 100);
        let mut c = clock.counter();
        consume(&mut c, 10_000);
        c.publish();
        assert_eq!(clock.queued_ms(), 0);
    }

    #[test]
    fn la_entrada_suave_sube_de_casi_cero_a_uno_en_5_ms() {
        let mut fade = FadeIn::new(RATE);
        let frames = (RATE * FADE_IN_MS / 1000) as usize;
        assert_eq!(frames, 220);
        let mut audio = vec![1.0; (frames + 50) * 2];
        fade.apply(&mut audio, 2);
        assert!(fade.finished());
        // Mismo valor en los dos canales de cada marco: la imagen estéreo no se mueve.
        assert!(audio.chunks(2).all(|f| f[0] == f[1]));
        let gains: Vec<f64> = audio.chunks(2).map(|f| f[0]).collect();
        assert!(gains[0] > 0.0 && gains[0] < 1e-3, "primer marco {}", gains[0]);
        assert!(gains.windows(2).all(|w| w[0] <= w[1]), "no baja nunca");
        assert_eq!(gains[frames - 1], 1.0, "termina justo en 1");
        assert!(gains[frames..].iter().all(|&g| g == 1.0), "lo de detrás, intacto");
        // A mitad de la rampa, la mitad (coseno alzado simétrico).
        assert!((gains[frames / 2 - 1] - 0.5).abs() < 0.01);
    }

    #[test]
    fn la_entrada_suave_da_lo_mismo_en_trozos_que_de_una_vez() {
        let input: Vec<f64> = (0..1000).map(|i| ((i as f64) * 0.37).sin()).collect();
        let mut whole = input.clone();
        FadeIn::new(RATE).apply(&mut whole, 2);
        let mut parts = input.clone();
        let mut fade = FadeIn::new(RATE);
        // Trozos de tamaños dispares, siempre de marcos enteros como los paquetes del reproductor.
        let mut start = 0;
        for len in [2, 34, 6, 118, 300, 540] {
            fade.apply(&mut parts[start..start + len], 2);
            start += len;
        }
        assert_eq!(whole, parts);
        // Una vez terminada no toca nada.
        let mut after = vec![0.25; 16];
        fade.apply(&mut after, 2);
        assert!(after.iter().all(|&s| s == 0.25));
    }

    #[test]
    fn al_reanudar_se_vuelve_a_lo_oido_si_la_cola_se_perdio() {
        // Pausa a 61,4 s del decodificador con 400 ms aún en cola: se oyó hasta 61,0 s.
        assert_eq!(heard_ms(61_400, 400), 61_000);
        // La cola sigue intacta (o casi: lo que el dispositivo tomó en lo que rodio tarda en
        // enterarse de la pausa): basta con reanudar.
        assert_eq!(resume_from(61_400, 400, 400), None);
        assert_eq!(resume_from(61_400, 400, 395), None);
        // Se soltó la salida tras 5 s en pausa: hay que volver a decodificar desde 61,0 s, o
        // se saltarían 0,4 s.
        assert_eq!(resume_from(61_400, 400, 0), Some(61_000));
        // Sin nada en cola al pausar no hay nada que recuperar.
        assert_eq!(resume_from(61_400, 0, 0), None);
        assert_eq!(resume_from(61_400, 15, 0), None);
        // Al principio de la canción no se pasa de 0.
        assert_eq!(resume_from(200, 400, 0), Some(0));
    }
}
