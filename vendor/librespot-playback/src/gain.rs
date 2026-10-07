//! Ganancia de la normalización de volumen: el factor de cada canción según el modelo de Spotify
//! y la rampa con la que un cambio en vivo (nivel, activarla o desactivarla) pasa de una ganancia
//! a otra.
//!
//! Sin dependencias ni rutas `crate::`: el binario incluye este fichero con `#[path]` para que
//! `cargo test` corra sus pruebas (las de las dependencias no se ejecutan desde el binario).

/// Duración de la rampa de un cambio de ganancia en vivo. Sin ella el salto (p. ej. −8 dB al
/// activar la normalización en una canción muy fuerte) cae entre dos muestras y se oye como un
/// chasquido; antes no pasaba porque el cambio reiniciaba el reproductor y el silencio lo tapaba.
/// 50 ms no se oyen como fundido y bastan para que no chasquee.
pub const RAMP_MS: u32 = 50;

fn db_to_ratio(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Unos metadatos rotos (NaN, ±∞) no deben dejar la canción muda ni a todo volumen: se toman
/// como «sin datos».
fn finite_or(v: f64, default: f64) -> f64 {
    if v.is_finite() { v } else { default }
}

/// La canción no trae datos de volumen: el cargador deja los valores por defecto (ganancia 0,
/// pico 1) cuando no los hay (MP3 de pódcast, archivo local sin ReplayGain, cabecera ilegible), y
/// una cabecera a ceros trae pico 0. Sin saber cuánto suena no se toca (factor 1, como antes de
/// los niveles): si no, «Normal» la bajaría 1 dB por un pico supuesto, «Bajo» 6 dB y «Alto» la
/// subiría 3 dB sin motivo. Una canción real con ganancia exactamente 0,0 es prácticamente
/// imposible (Spotify la guarda como f32 calculado).
pub fn no_data(gain_db: f64, peak: f64) -> bool {
    finite_or(gain_db, 0.0) == 0.0 && (peak == 1.0 || !(peak.is_finite() && peak > 0.0))
}

/// Factor de los niveles «Normal» y «Bajo» de Spotify: la ganancia de la canción (o del álbum)
/// más la del nivel, también hacia arriba, pero solo hasta dejar el pico en `threshold_dbfs`
/// (Spotify deja 1 dB de margen en lo comprimido con pérdida). Sin limitador: lo que no cabe no se
/// sube. librespot recortaba además el factor a 1.0, de modo que un máster tranquilo nunca subía
/// y esas canciones sonaban más bajas que en Spotify.
pub fn basic_factor(gain_db: f64, pregain_db: f64, threshold_dbfs: f64, peak: f64) -> f64 {
    // Un pico 0 o no finito daría un tope infinito o NaN: se supone el peor caso, a fondo de escala.
    let peak = if peak.is_finite() && peak > 0.0 { peak } else { 1.0 };
    let gain = db_to_ratio(finite_or(gain_db, 0.0) + finite_or(pregain_db, 0.0));
    let ceiling = db_to_ratio(finite_or(threshold_dbfs, 0.0)) / peak;
    gain.min(ceiling)
}

/// Factor del nivel «Alto»: la ganancia entera, sin tope; los picos que pasen del umbral los baja
/// el limitador del reproductor (el único nivel que lo usa, como en Spotify).
pub fn dynamic_factor(gain_db: f64, pregain_db: f64) -> f64 {
    db_to_ratio(finite_or(gain_db, 0.0) + finite_or(pregain_db, 0.0))
}

/// Paso lineal de una ganancia a otra a lo largo de `frames` marcos. Avanza por marco y no por
/// muestra: los canales de un mismo marco llevan la misma ganancia y la imagen estéreo no se mueve.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GainRamp {
    from: f64,
    to: f64,
    frames: u32,
    done: u32,
}

impl GainRamp {
    pub fn new(from: f64, to: f64, frames: u32) -> Self {
        Self {
            from,
            to,
            frames: frames.max(1),
            done: 0,
        }
    }

    /// Rampa de `RAMP_MS` para audio a `rate` Hz.
    pub fn with_duration(from: f64, to: f64, rate: u32) -> Self {
        Self::new(from, to, (rate as u64 * RAMP_MS as u64 / 1000) as u32)
    }

    /// Ganancia final: la que el reproductor tiene guardada para la canción.
    pub fn target(&self) -> f64 {
        self.to
    }

    pub fn finished(&self) -> bool {
        self.done >= self.frames
    }

    /// Ganancia del último marco aplicado (la de partida si aún no se aplicó ninguno). Si el nivel
    /// vuelve a cambiar a medio camino, la rampa nueva sale de aquí y no hay salto.
    pub fn current(&self) -> f64 {
        if self.finished() {
            // Exacto, sin el redondeo de la interpolación: al terminar queda justo el destino.
            self.to
        } else {
            self.from + (self.to - self.from) * (self.done as f64 / self.frames as f64)
        }
    }

    /// Ganancia del siguiente marco; al terminar se queda en el destino.
    pub fn next_gain(&mut self) -> f64 {
        if self.done < self.frames {
            self.done += 1;
        }
        self.current()
    }
}

/// Ganancia que sonaba justo antes de un cambio en vivo, de donde sale la rampa nueva: el punto
/// actual de la rampa anterior si iba hacia `old_factor` (el nivel cambió otra vez a medio
/// camino) o, si no, `old_factor`. Si el limitador deja de actuar (se sale de «Alto»), su
/// reducción de ese instante (`limiter_gain`) también desaparece y se cuenta aquí.
pub fn ramp_start(old_factor: f64, previous: Option<GainRamp>, limiter_gain: f64, limiter_stops: bool) -> f64 {
    let applied = match previous {
        Some(r) if r.target() == old_factor => r.current(),
        _ => old_factor,
    };
    if limiter_stops { applied * limiter_gain } else { applied }
}

/// Multiplica las muestras entrelazadas por `gain · volume`, o por la rampa mientras dure (y la
/// quita al terminar: lo que queda del paquete ya va con `gain`, que es su destino). Con ganancia
/// 1 y sin rampa no recorre el paquete.
pub fn apply_gain(samples: &mut [f64], channels: usize, gain: f64, volume: f64, ramp: &mut Option<GainRamp>) {
    let channels = channels.max(1);
    let mut start = 0;
    if let Some(r) = ramp.as_mut() {
        while !r.finished() && start + channels <= samples.len() {
            let g = r.next_gain() * volume;
            for s in &mut samples[start..start + channels] {
                *s *= g;
            }
            start += channels;
        }
    }
    if ramp.is_some_and(|r| r.finished()) {
        *ramp = None;
    }
    let g = gain * volume;
    if g != 1.0 {
        for s in &mut samples[start..] {
            *s *= g;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    /// Los valores del plan (umbral −1 dBFS): Normal sube hasta dejar 1 dB de margen, baja sin
    /// tope, Bajo resta 5 dB y Alto suma 3 dB sin tope (ya limitará el reproductor).
    #[test]
    fn factores_como_spotify() {
        // Normal: +6 dB con pico 0,5 → el tope del pico (0,891 / 0,5).
        assert!(close(basic_factor(6.0, 0.0, -1.0, 0.5), 1.782, 0.001), "{}", basic_factor(6.0, 0.0, -1.0, 0.5));
        // Normal: −6 dB → 0,501.
        assert!(close(basic_factor(-6.0, 0.0, -1.0, 1.0), 0.501, 0.001));
        // Normal: +3 dB con pico a fondo de escala → solo hasta −1 dBFS.
        assert!(close(basic_factor(3.0, 0.0, -1.0, 1.0), 0.891, 0.001));
        // Alto: −6 + 3 dB → 0,708, sin tope.
        assert!(close(dynamic_factor(-6.0, 3.0), 0.708, 0.001));
        assert!(close(dynamic_factor(6.0, 3.0), 2.818, 0.001));
        // Bajo: −6 − 5 dB → 0,282.
        assert!(close(basic_factor(-6.0, -5.0, -1.0, 1.0), 0.282, 0.001));
        // Un máster tranquilo sube por encima de 1 si su pico lo permite (antes se recortaba a 1).
        let f = basic_factor(4.0, 0.0, -1.0, 0.5);
        assert!(close(f, 1.585, 0.001) && f > 1.0, "{f}");
        // El pico resultante nunca pasa del umbral.
        for (g, peak) in [(10.0, 0.3), (2.0, 0.9), (0.0, 1.2), (-3.0, 1.5), (12.0, 0.05)] {
            let f = basic_factor(g, 0.0, -1.0, peak);
            assert!(f * peak <= db_to_ratio(-1.0) + 1e-12, "ganancia {g} pico {peak}: {}", f * peak);
        }
    }

    /// Metadatos rotos o ausentes: nunca un factor NaN, infinito o nulo.
    #[test]
    fn picos_y_ganancias_imposibles() {
        let full = basic_factor(0.0, 0.0, -1.0, 1.0);
        for peak in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(basic_factor(0.0, 0.0, -1.0, peak), full, "pico {peak}");
        }
        for gain in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let f = basic_factor(gain, 0.0, -1.0, 1.0);
            assert_eq!(f, full, "ganancia {gain}");
            let d = dynamic_factor(gain, 3.0);
            assert!(close(d, db_to_ratio(3.0), 1e-12), "ganancia {gain}: {d}");
        }
    }

    /// Lo que deja el cargador cuando no hay datos (ganancia 0 con pico 1, o una cabecera a ceros)
    /// cuenta como «sin datos»; cualquier ganancia real, no.
    #[test]
    fn sin_datos_de_volumen() {
        assert!(no_data(0.0, 1.0));
        assert!(no_data(0.0, 0.0));
        assert!(no_data(f64::NAN, 1.0));
        assert!(no_data(0.0, f64::NAN));
        assert!(!no_data(-8.3, 1.0));
        assert!(!no_data(0.0, 0.7));
        assert!(!no_data(2.1, 0.0));
    }

    /// La rampa va en línea recta y en pasos iguales del origen al destino, termina exactamente
    /// en el destino y se queda ahí.
    #[test]
    fn rampa_lineal_sin_saltos() {
        let n = 2205;
        let mut r = GainRamp::new(0.4, 1.0, n);
        assert_eq!(r.current(), 0.4);
        assert_eq!(r.target(), 1.0);
        let step = 0.6 / n as f64;
        let mut prev = r.current();
        for k in 1..=n {
            assert!(!r.finished());
            let g = r.next_gain();
            assert!(g > prev, "marco {k}: {g} <= {prev}");
            assert!(g - prev <= step + 1e-12, "marco {k}: paso {}", g - prev);
            prev = g;
        }
        assert!(r.finished());
        assert_eq!(prev, 1.0);
        assert_eq!(r.next_gain(), 1.0);
        assert_eq!(r.current(), 1.0);

        // Hacia abajo igual.
        let mut down = GainRamp::new(1.0, 0.25, 10);
        let gains: Vec<f64> = (0..12).map(|_| down.next_gain()).collect();
        assert!(gains.windows(2).all(|w| w[1] <= w[0]));
        assert!(close(gains[0], 0.925, 1e-12));
        assert_eq!(gains[9], 0.25);
        assert_eq!(gains[11], 0.25);
        // Sin marcos: termina al primero.
        let mut z = GainRamp::new(0.5, 2.0, 0);
        assert_eq!(z.next_gain(), 2.0);
        assert!(z.finished());
    }

    #[test]
    fn rampa_de_50_ms() {
        assert_eq!(GainRamp::with_duration(1.0, 0.5, 44_100).frames, 2205);
        assert_eq!(GainRamp::with_duration(1.0, 0.5, 48_000).frames, 2400);
    }

    /// En estéreo los dos canales de cada marco llevan la misma ganancia; acabada la rampa, el
    /// resto del paquete va con la ganancia final y la rampa desaparece.
    #[test]
    fn apply_gain_por_marcos() {
        let n = 100u32;
        let mut ramp = Some(GainRamp::new(0.5, 1.5, n));
        let mut buf = vec![1.0f64; 2 * 150];
        apply_gain(&mut buf, 2, 1.5, 1.0, &mut ramp);
        assert!(ramp.is_none());
        for (k, frame) in buf.chunks_exact(2).enumerate() {
            assert_eq!(frame[0], frame[1], "marco {k}");
            let want = if (k as u32) < n { 0.5 + (k as f64 + 1.0) / n as f64 } else { 1.5 };
            assert!(close(frame[0], want, 1e-12), "marco {k}: {} != {want}", frame[0]);
        }
        // El volumen se multiplica encima, también durante la rampa.
        let mut ramp = Some(GainRamp::new(0.5, 1.5, n));
        let mut buf = vec![1.0f64; 2 * 150];
        apply_gain(&mut buf, 2, 1.5, 0.5, &mut ramp);
        assert!(close(buf[0], (0.5 + 1.0 / n as f64) * 0.5, 1e-12));
        assert!(close(buf[299], 0.75, 1e-12));
    }

    /// Trocear el audio en paquetes de cualquier tamaño da exactamente lo mismo que de una vez.
    #[test]
    fn apply_gain_en_trozos_igual_que_de_una_vez() {
        let input: Vec<f64> = (0..2 * 3000).map(|i| ((i as f64) * 0.37).sin()).collect();
        let mut whole = input.clone();
        let mut ramp = Some(GainRamp::new(0.2, 0.9, 2205));
        apply_gain(&mut whole, 2, 0.9, 1.0, &mut ramp);
        assert!(ramp.is_none());

        let mut chunked = input.clone();
        let mut ramp = Some(GainRamp::new(0.2, 0.9, 2205));
        let mut pos = 0;
        let sizes = [2usize, 254, 2048, 6, 1024, 512, 98, 4096];
        let mut i = 0;
        while pos < chunked.len() {
            let len = sizes[i % sizes.len()].min(chunked.len() - pos);
            apply_gain(&mut chunked[pos..pos + len], 2, 0.9, 1.0, &mut ramp);
            pos += len;
            i += 1;
        }
        assert!(ramp.is_none());
        assert_eq!(whole, chunked);
    }

    /// Si el nivel cambia otra vez a media rampa, la nueva sale de donde iba la anterior: el salto
    /// entre marcos no supera un paso de rampa.
    #[test]
    fn cambio_a_medio_camino_sigue_desde_donde_iba() {
        let mut ramp = Some(GainRamp::new(1.0, 0.3, 2205));
        let mut buf = vec![1.0f64; 2 * 1000];
        apply_gain(&mut buf, 2, 0.3, 1.0, &mut ramp);
        let last = buf[buf.len() - 1];
        let r = ramp.expect("la rampa sigue a medias");
        assert!(close(r.current(), last, 1e-12));

        let mut ramp = Some(GainRamp::new(r.current(), 2.0, 2205));
        let mut buf = vec![1.0f64; 2 * 10];
        apply_gain(&mut buf, 2, 2.0, 1.0, &mut ramp);
        let step = (2.0 - last) / 2205.0;
        assert!(buf[0] > last && buf[0] - last <= step + 1e-12, "{} tras {last}", buf[0]);
    }

    /// Punto de partida de una rampa nueva: lo que de verdad sonaba en ese instante.
    #[test]
    fn la_rampa_sale_de_lo_que_sonaba() {
        // Sin rampa previa: el factor anterior.
        assert_eq!(ramp_start(0.5, None, 1.0, false), 0.5);
        // Rampa a medias hacia el factor anterior: desde su punto actual.
        let mut r = GainRamp::new(1.0, 0.5, 100);
        for _ in 0..40 {
            r.next_gain();
        }
        assert!(close(ramp_start(0.5, Some(r), 1.0, false), 0.8, 1e-12));
        // Una rampa hacia otro factor (de otra canción) no cuenta.
        assert_eq!(ramp_start(0.7, Some(r), 1.0, false), 0.7);
        // Se sale de «Alto» con el limitador bajando 2 dB: se parte de la ganancia con esa bajada.
        let lim = db_to_ratio(-2.0);
        assert!(close(ramp_start(1.4, None, lim, true), 1.4 * lim, 1e-12));
        // Si el limitador sigue actuando, su reducción sigue y no se cuenta.
        assert_eq!(ramp_start(1.4, None, lim, false), 1.4);
    }

    /// Sin cambio de ganancia el audio pasa intacto, bit a bit.
    #[test]
    fn sin_ganancia_no_toca_nada() {
        let input: Vec<f64> = (0..512).map(|i| ((i as f64) * 0.11).cos() * 1.3).collect();
        let mut buf = input.clone();
        let mut ramp = None;
        apply_gain(&mut buf, 2, 1.0, 1.0, &mut ramp);
        assert_eq!(buf, input);
        // Una rampa ya terminada se quita y no cambia nada más.
        let mut done = GainRamp::new(0.5, 1.0, 1);
        done.next_gain();
        let mut ramp = Some(done);
        apply_gain(&mut buf, 2, 1.0, 1.0, &mut ramp);
        assert!(ramp.is_none());
        assert_eq!(buf, input);
    }
}
