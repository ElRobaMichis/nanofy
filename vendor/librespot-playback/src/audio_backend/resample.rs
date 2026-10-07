//! Remuestreador sinc polifásico para la salida de audio (44,1 kHz → frecuencia del dispositivo).
//!
//! En Windows la mezcla compartida de WASAPI suele ir a 48 kHz y cpal no deja abrir el
//! dispositivo a 44,1 kHz. rodio convertía entonces cada paquete decodificado con interpolación
//! lineal, y además reiniciaba el conversor en cada paquete (~43 veces por segundo): agudos
//! apagados (−3,7 dB a 15 kHz) y una distorsión granulosa (SINAD de 8–35 dB). Aquí la conversión
//! es continua, con un filtro de ventana de Kaiser (β = 11, ~108 dB de rechazo, 160 coeficientes
//! por fase), y rodio recibe el audio ya a la frecuencia del dispositivo, de modo que su conversor
//! queda en paso directo.
//!
//! Sin dependencias ni rutas `crate::`: el binario incluye este fichero con `#[path]` para que
//! `cargo test` corra sus pruebas (las de las dependencias no se ejecutan desde el binario).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// β de la ventana de Kaiser: ~108 dB de atenuación en la banda eliminada.
const BETA: f64 = 11.0;
/// Coeficientes por fase al subir de frecuencia; al bajar se escala por fs_in/fs_out para que la
/// banda de transición no se ensanche respecto a la Nyquist de salida.
const TAPS: usize = 160;
/// Corte del filtro respecto a la menor de las dos Nyquist. Con 160 coeficientes la transición
/// mide ~1,9 kHz: a 44,1 → 48 kHz va de 20,0 a 21,9 kHz, por debajo de los 22,05 kHz.
const CUTOFF: f64 = 0.95;
/// Más fases que esto (relaciones raras como 44 100 → 47 999) pedirían una tabla de cientos de
/// MB; esa conversión se deja a rodio.
const MAX_PHASES: u32 = 2048;

/// Conversión de frecuencia de muestreo por un factor racional L/M (L = fs_out/mcd,
/// M = fs_in/mcd) con un banco polifásico: cada muestra de salida es el producto escalar de una
/// fila de la tabla (la fase) por los últimos `taps` marcos de entrada de cada canal.
///
/// Alineación: la muestra de salida n corresponde exactamente al instante n·M/L de la entrada
/// (sin desfase), y `process` + `flush` producen en total ⌈N·L/M⌉ marcos para N de entrada. El
/// precio es retener ~`taps`/2 marcos de entrada (1,8 ms a 44,1 kHz) hasta que llega lo
/// siguiente o se vacía con `flush`.
pub struct Resampler {
    /// `up` filas de `taps` coeficientes, en el orden en que se multiplican contra la historia y
    /// con ganancia exactamente 1 en continua cada una. Compartida entre salidas (ver `table`).
    coefs: Arc<[f32]>,
    taps: usize,
    /// L: fases del banco.
    up: usize,
    /// M: avance de fase por muestra de salida.
    down: usize,
    channels: usize,
    /// Historia por canal, sin entrelazar: el producto escalar recorre memoria contigua y el
    /// compilador lo vectoriza.
    hist: Vec<Vec<f32>>,
    /// Marco de la historia donde empieza la ventana de la próxima muestra de salida.
    start: usize,
    /// Fase (0..up) de la próxima muestra de salida.
    phase: usize,
    /// Canal al que va la próxima muestra de entrada: un trozo podría cortar un marco a medias.
    next_ch: usize,
}

impl Resampler {
    /// `None` si no hace falta convertir (misma frecuencia), si los parámetros no valen, o si la
    /// relación pediría más de `MAX_PHASES` fases (quien llama deja entonces la conversión a
    /// rodio y lo avisa en el registro).
    pub fn new(fs_in: u32, fs_out: u32, channels: usize) -> Option<Self> {
        if fs_in == 0 || fs_out == 0 || fs_in == fs_out || channels == 0 {
            return None;
        }
        let g = gcd(fs_in, fs_out);
        let (up, down) = (fs_out / g, fs_in / g);
        if up > MAX_PHASES {
            return None;
        }
        let taps = taps_for(fs_in, fs_out);
        let mut r = Self {
            coefs: table(fs_in, fs_out, up as usize, taps),
            taps,
            up: up as usize,
            down: down as usize,
            channels,
            hist: vec![Vec::new(); channels],
            start: 0,
            phase: 0,
            next_ch: 0,
        };
        r.reset();
        Some(r)
    }

    /// Convierte muestras entrelazadas (`channels` por marco) y añade a `out` las de salida,
    /// también entrelazadas, que ya se pueden calcular. Partir la entrada en trozos de cualquier
    /// tamaño (incluso a mitad de un marco) da exactamente las mismas muestras que una sola llamada.
    pub fn process(&mut self, input: &[f64], out: &mut Vec<f32>) {
        for &s in input {
            self.hist[self.next_ch].push(s as f32);
            self.next_ch += 1;
            if self.next_ch == self.channels {
                self.next_ch = 0;
            }
        }
        self.run(out);
    }

    /// Fin del flujo (pausa, parada): saca lo que la ventana aún retenía, completándolo con
    /// silencio, y deja el remuestreador como nuevo para lo siguiente que llegue. Un marco a
    /// medias no se puede sacar y se descarta.
    pub fn flush(&mut self, out: &mut Vec<f32>) {
        for c in 0..self.next_ch {
            self.hist[c].pop();
        }
        self.next_ch = 0;
        let pad = self.taps / 2;
        for h in &mut self.hist {
            h.resize(h.len() + pad, 0.0);
        }
        self.run(out);
        self.reset();
    }

    /// Historia en silencio y fase 0. Se precargan `taps`/2 − 1 ceros: con eso el centro de la
    /// ventana de la primera muestra de salida cae justo sobre el primer marco de entrada (sin
    /// silencio añadido al principio ni desfase). También para tirar lo que retenía sin sacarlo,
    /// cuando la cola de la salida se vacía (buscar, otra canción) y lo siguiente no continúa.
    pub fn reset(&mut self) {
        let pre = self.taps / 2 - 1;
        for h in &mut self.hist {
            h.clear();
            h.resize(pre, 0.0);
        }
        self.start = 0;
        self.phase = 0;
        self.next_ch = 0;
    }

    /// Calcula todas las muestras de salida cuya ventana ya está completa y descarta de la
    /// historia los marcos que ninguna ventana futura va a usar.
    fn run(&mut self, out: &mut Vec<f32>) {
        let t = self.taps;
        // Marcos completos: el último canal es el que se llena el último.
        let frames = self.hist[self.channels - 1].len();
        let (up, down) = (self.up, self.down);
        let (mut start, mut phase) = (self.start, self.phase);
        if frames + 1 >= t + start {
            // Muestras que van a salir: n tal que start + ⌊(phase + n·M)/L⌋ + t ≤ frames.
            let n = (((frames + 1 - t - start) * up).saturating_sub(phase)).div_ceil(down);
            out.reserve(n * self.channels);
        }
        while start + t <= frames {
            let row = &self.coefs[phase * t..(phase + 1) * t];
            for h in &self.hist {
                out.push(dot(row, &h[start..start + t]));
            }
            phase += down;
            start += phase / up;
            phase %= up;
        }
        let drop = start.min(frames);
        if drop > 0 {
            for h in &mut self.hist {
                h.drain(..drop);
            }
        }
        self.start = start - drop;
        self.phase = phase;
    }
}

/// Mezcla a mono un audio estéreo entrelazado, (L + R) / 2. Para dispositivos de un solo canal:
/// el conversor de canales de rodio (2 → 1) se queda con el izquierdo y tira el derecho.
pub fn downmix_stereo(samples: &[f64]) -> Vec<f64> {
    samples.chunks_exact(2).map(|f| (f[0] + f[1]) * 0.5).collect()
}

/// Producto escalar con ocho acumuladores independientes: sin ellos el compilador no puede
/// reordenar las sumas en coma flotante y no usa SIMD (el bucle saldría unas 4 veces más lento).
#[inline]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0f32; 8];
    let (ca, cb) = (a.chunks_exact(8), b.chunks_exact(8));
    let (ra, rb) = (ca.remainder(), cb.remainder());
    for (x, y) in ca.zip(cb) {
        for i in 0..8 {
            acc[i] += x[i] * y[i];
        }
    }
    let mut s = ((acc[0] + acc[1]) + (acc[2] + acc[3])) + ((acc[4] + acc[5]) + (acc[6] + acc[7]));
    for (x, y) in ra.iter().zip(rb) {
        s += x * y;
    }
    s
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Coeficientes por fase: 160 al subir; al bajar, escalados por fs_in/fs_out y redondeados
/// hacia arriba a múltiplo de 8 (el producto escalar va de 8 en 8).
fn taps_for(fs_in: u32, fs_out: u32) -> usize {
    if fs_out >= fs_in {
        TAPS
    } else {
        let t = (TAPS as f64 * fs_in as f64 / fs_out as f64).ceil() as usize;
        t.div_ceil(8) * 8
    }
}

/// Tabla de coeficientes para (fs_in, fs_out), calculada una vez por proceso: cada reapertura de
/// la salida (pausa larga, cambio de dispositivo) la reutiliza en vez de recalcularla.
fn table(fs_in: u32, fs_out: u32, up: usize, taps: usize) -> Arc<[f32]> {
    type Tables = Mutex<HashMap<(u32, u32), Arc<[f32]>>>;
    static TABLES: OnceLock<Tables> = OnceLock::new();
    let tables = TABLES.get_or_init(Default::default);
    if let Some(t) = tables
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&(fs_in, fs_out))
    {
        return t.clone();
    }
    // Se calcula fuera del candado: son ~1 ms y no hay por qué bloquear a nadie.
    let t: Arc<[f32]> = design(fs_in, fs_out, up, taps).into();
    tables
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry((fs_in, fs_out))
        .or_insert(t)
        .clone()
}

/// Banco polifásico de un paso bajo sinc con ventana de Kaiser. La fila p sirve para la muestra
/// de salida que cae a p/L marcos del inicio de su ventana; el coeficiente k multiplica al marco
/// k de la ventana, que está a `d` marcos de entrada del instante que se calcula. El filtro queda
/// centrado (fase lineal, sin desfase) y cada fila se normaliza a suma 1: ganancia exacta en
/// continua para todas las fases, sin rizado de nivel entre ellas.
fn design(fs_in: u32, fs_out: u32, up: usize, taps: usize) -> Vec<f32> {
    // Corte en ciclos por muestra de entrada.
    let fc = CUTOFF * 0.5 * fs_in.min(fs_out) as f64 / fs_in as f64;
    let half = taps as f64 / 2.0;
    let i0_beta = bessel_i0(BETA);
    let mut out = Vec::with_capacity(up * taps);
    let mut row = vec![0f64; taps];
    for p in 0..up {
        let frac = p as f64 / up as f64;
        for (k, c) in row.iter_mut().enumerate() {
            let d = k as f64 + 1.0 - half - frac;
            let r = d / half;
            *c = if r.abs() >= 1.0 {
                0.0
            } else {
                let window = bessel_i0(BETA * (1.0 - r * r).sqrt()) / i0_beta;
                2.0 * fc * sinc(2.0 * fc * d) * window
            };
        }
        let sum: f64 = row.iter().sum();
        out.extend(row.iter().map(|&c| (c / sum) as f32));
    }
    out
}

fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

/// Bessel modificada de primera especie y orden 0, por su serie (25 términos: para β ≤ 12 el
/// último aporta menos de 1e-13 relativo).
fn bessel_i0(x: f64) -> f64 {
    let q = x / 2.0;
    let (mut term, mut sum) = (1.0, 1.0);
    for k in 1..25 {
        term *= q / k as f64;
        sum += term * term;
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    const FS_IN: u32 = 44_100;

    /// Todo de una vez: lo que sale con la entrada entera más el vaciado final.
    fn resample_all(r: &mut Resampler, input: &[f64]) -> Vec<f32> {
        let mut out = Vec::new();
        r.process(input, &mut out);
        r.flush(&mut out);
        out
    }

    /// Seno entrelazado, igual en todos los canales.
    fn sine(f: f64, amp: f64, frames: usize, channels: usize) -> Vec<f64> {
        let w = 2.0 * PI * f / FS_IN as f64;
        (0..frames)
            .flat_map(|n| std::iter::repeat_n(amp * (w * n as f64).sin(), channels))
            .collect()
    }

    fn channel(v: &[f32], channels: usize, c: usize) -> Vec<f64> {
        v.iter().skip(c).step_by(channels).map(|&s| s as f64).collect()
    }

    /// Ajuste por mínimos cuadrados de y[n] ≈ c + Σ (a_i·sen(ω_i n) + b_i·cos(ω_i n)), con n
    /// absoluto (`offset` + índice). Devuelve la amplitud y la fase de cada frecuencia y la
    /// potencia media del residuo (ruido + distorsión + imágenes no ajustadas).
    fn fit(y: &[f64], offset: usize, fs: f64, freqs: &[f64]) -> (Vec<(f64, f64)>, f64) {
        let dim = 1 + 2 * freqs.len();
        let basis = |n: usize| -> Vec<f64> {
            let mut b = vec![1.0];
            for &f in freqs {
                let a = 2.0 * PI * f / fs * (n + offset) as f64;
                b.push(a.sin());
                b.push(a.cos());
            }
            b
        };
        let mut m = vec![vec![0.0; dim + 1]; dim];
        for (n, &v) in y.iter().enumerate() {
            let b = basis(n);
            for i in 0..dim {
                for j in 0..dim {
                    m[i][j] += b[i] * b[j];
                }
                m[i][dim] += b[i] * v;
            }
        }
        // Gauss con pivoteo parcial.
        for col in 0..dim {
            let piv = (col..dim)
                .max_by(|&a, &b| m[a][col].abs().total_cmp(&m[b][col].abs()))
                .unwrap();
            m.swap(col, piv);
            let pivot = m[col].clone();
            for (r, row) in m.iter_mut().enumerate() {
                if r != col {
                    let k = row[col] / pivot[col];
                    for c in col..=dim {
                        row[c] -= k * pivot[c];
                    }
                }
            }
        }
        let x: Vec<f64> = (0..dim).map(|i| m[i][dim] / m[i][i]).collect();
        let mut resid = 0.0;
        for (n, &v) in y.iter().enumerate() {
            let model: f64 = basis(n).iter().zip(&x).map(|(b, c)| b * c).sum();
            resid += (v - model).powi(2);
        }
        let tones = (0..freqs.len())
            .map(|i| {
                let (a, b) = (x[1 + 2 * i], x[2 + 2 * i]);
                ((a * a + b * b).sqrt(), b.atan2(a))
            })
            .collect();
        (tones, resid / y.len() as f64)
    }

    fn db(ratio: f64) -> f64 {
        20.0 * ratio.log10()
    }

    /// Calidad de un tono convertido: (SINAD en dB, error de nivel en dB, fase en radianes).
    /// Se descartan los extremos, donde el arranque desde silencio y el vaciado final no son
    /// régimen permanente.
    fn tone_quality(fs_out: u32, f: f64) -> (f64, f64, f64) {
        let amp = 0.5;
        let mut r = Resampler::new(FS_IN, fs_out, 2).unwrap();
        let out = resample_all(&mut r, &sine(f, amp, 22_050, 2));
        let left = channel(&out, 2, 0);
        let skip = 1000;
        let seg = &left[skip..left.len() - skip];
        let (tones, resid) = fit(seg, skip, fs_out as f64, &[f]);
        let (a, phase) = tones[0];
        let sinad = 10.0 * ((a * a / 2.0) / resid).log10();
        (sinad, db(a / amp), phase)
    }

    #[test]
    fn rejects_what_it_cannot_or_need_not_do() {
        assert!(Resampler::new(FS_IN, FS_IN, 2).is_none());
        assert!(Resampler::new(FS_IN, 48_000, 0).is_none());
        assert!(Resampler::new(0, 48_000, 2).is_none());
        // 44 100 y 44 101 son primos entre sí: 44 101 fases.
        assert!(Resampler::new(FS_IN, 44_101, 2).is_none());
        let r = Resampler::new(FS_IN, 48_000, 2).unwrap();
        assert_eq!((r.up, r.down, r.taps), (160, 147, 160));
        // Al bajar se escala: 160·44,1/32 = 220,5 → 224.
        let r = Resampler::new(FS_IN, 32_000, 2).unwrap();
        assert_eq!((r.up, r.down, r.taps), (320, 441, 224));
    }

    #[test]
    fn table_is_shared_between_outputs() {
        let a = Resampler::new(FS_IN, 48_000, 2).unwrap();
        let b = Resampler::new(FS_IN, 48_000, 1).unwrap();
        assert!(Arc::ptr_eq(&a.coefs, &b.coefs));
    }

    #[test]
    fn every_phase_has_unit_dc_gain() {
        for fs_out in [22_050, 32_000, 48_000, 96_000, 192_000] {
            let r = Resampler::new(FS_IN, fs_out, 1).unwrap();
            for row in r.coefs.chunks_exact(r.taps) {
                let s: f64 = row.iter().map(|&c| c as f64).sum();
                assert!((s - 1.0).abs() < 1e-6, "{fs_out} Hz: suma de fila {s}");
            }
        }
    }

    #[test]
    fn dc_passes_unchanged() {
        for fs_out in [22_050, 32_000, 48_000, 88_200, 96_000, 192_000] {
            let mut r = Resampler::new(FS_IN, fs_out, 2).unwrap();
            let out = resample_all(&mut r, &vec![1.0; 2 * 10_000]);
            let frames = out.len() / 2;
            // Fuera del arranque y del vaciado, donde entra y sale el silencio.
            for (i, &s) in out[2 * 500..2 * (frames - 500)].iter().enumerate() {
                assert!((s as f64 - 1.0).abs() <= 1e-6, "{fs_out} Hz, muestra {i}: {s}");
            }
        }
    }

    #[test]
    fn sine_44k1_to_48k_is_clean_and_flat() {
        for f in [1_000.0, 2_000.0, 5_000.0, 10_000.0, 15_000.0, 16_000.0, 18_000.0] {
            let (sinad, level, phase) = tone_quality(48_000, f);
            let min = if f > 16_000.0 { 95.0 } else { 100.0 };
            assert!(sinad >= min, "{f} Hz: SINAD {sinad:.1} dB < {min} dB");
            assert!(level.abs() <= 0.01, "{f} Hz: nivel {level:+.4} dB");
            // Sin desfase: la salida n es la entrada en el instante n·M/L.
            assert!(phase.abs() < 1e-4, "{f} Hz: fase {phase:e} rad");
        }
    }

    #[test]
    fn sine_quality_at_other_device_rates() {
        for fs_out in [88_200, 96_000, 192_000] {
            for f in [1_000.0, 10_000.0, 18_000.0] {
                let (sinad, level, _) = tone_quality(fs_out, f);
                assert!(sinad >= 100.0, "{fs_out} Hz, {f} Hz: SINAD {sinad:.1} dB");
                assert!(level.abs() <= 0.01, "{fs_out} Hz, {f} Hz: nivel {level:+.4} dB");
            }
        }
        // Al bajar a 32 kHz el tono tiene que caer dentro de la banda útil (corte en 15,2 kHz).
        for f in [1_000.0, 10_000.0] {
            let (sinad, level, _) = tone_quality(32_000, f);
            assert!(sinad >= 100.0, "32 kHz, {f} Hz: SINAD {sinad:.1} dB");
            assert!(level.abs() <= 0.01, "32 kHz, {f} Hz: nivel {level:+.4} dB");
        }
    }

    #[test]
    fn images_rejected_when_upsampling_to_96k() {
        // Un tono de 15 kHz deja imágenes en 44,1 − 15 = 29,1 kHz y 44,1 + 15 = 59,1 kHz, que a
        // 96 kHz se pliega en 36,9 kHz.
        let amp = 0.5;
        let mut r = Resampler::new(FS_IN, 96_000, 2).unwrap();
        let out = resample_all(&mut r, &sine(15_000.0, amp, 22_050, 2));
        let right = channel(&out, 2, 1);
        let skip = 2000;
        let seg = &right[skip..right.len() - skip];
        let (tones, _) = fit(seg, skip, 96_000.0, &[15_000.0, 29_100.0, 36_900.0]);
        assert!(db(tones[0].0 / amp).abs() <= 0.01);
        for &(a, _) in &tones[1..] {
            assert!(db(a / amp) <= -100.0, "imagen a {:.1} dB", db(a / amp));
        }
    }

    #[test]
    fn content_above_output_nyquist_is_removed_when_downsampling() {
        // 20 kHz no cabe en 32 kHz: sin filtro reaparecería plegado en 12 kHz.
        let amp = 0.5;
        let mut r = Resampler::new(FS_IN, 32_000, 1).unwrap();
        let out = resample_all(&mut r, &sine(20_000.0, amp, 22_050, 1));
        let y = channel(&out, 1, 0);
        let skip = 1000;
        let seg = &y[skip..y.len() - skip];
        let rms = (seg.iter().map(|s| s * s).sum::<f64>() / seg.len() as f64).sqrt();
        let level = db(rms / (amp / 2f64.sqrt()));
        assert!(level <= -100.0, "alias a {level:.1} dB");
    }

    /// Generador pseudoaleatorio fijo (xorshift64) para que la prueba sea reproducible.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    fn noise(rng: &mut Rng, samples: usize) -> Vec<f64> {
        (0..samples)
            .map(|_| (rng.next() >> 11) as f64 / (1u64 << 53) as f64 - 0.5)
            .collect()
    }

    #[test]
    fn chunking_gives_bit_identical_output() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for fs_out in [48_000, 32_000, 96_000] {
            let input = noise(&mut rng, 2 * 60_000);
            let whole = resample_all(&mut Resampler::new(FS_IN, fs_out, 2).unwrap(), &input);

            // Trozos de 1 a 3000 marcos, como los paquetes del decodificador.
            let mut r = Resampler::new(FS_IN, fs_out, 2).unwrap();
            let mut out = Vec::new();
            let mut rest = &input[..];
            while !rest.is_empty() {
                let n = (2 * (1 + rng.below(3000))).min(rest.len());
                r.process(&rest[..n], &mut out);
                rest = &rest[n..];
            }
            r.flush(&mut out);
            assert_eq!(out.len(), whole.len(), "{fs_out} Hz");
            assert!(out.iter().zip(&whole).all(|(a, b)| a.to_bits() == b.to_bits()), "{fs_out} Hz");

            // Y cortando marcos por la mitad (trozos de un número impar de muestras).
            let mut r = Resampler::new(FS_IN, fs_out, 2).unwrap();
            let mut out = Vec::new();
            let mut rest = &input[..];
            while !rest.is_empty() {
                let n = (1 + rng.below(999)).min(rest.len());
                r.process(&rest[..n], &mut out);
                rest = &rest[n..];
            }
            r.flush(&mut out);
            assert_eq!(out.len(), whole.len(), "{fs_out} Hz, marcos partidos");
            assert!(
                out.iter().zip(&whole).all(|(a, b)| a.to_bits() == b.to_bits()),
                "{fs_out} Hz, marcos partidos"
            );
        }
    }

    #[test]
    fn flush_leaves_it_as_new() {
        let mut rng = Rng(42);
        let input = noise(&mut rng, 2 * 5_000);
        let mut r = Resampler::new(FS_IN, 48_000, 2).unwrap();
        let first = resample_all(&mut r, &input);
        // Un marco a medias antes del vaciado se descarta y no desalinea los canales.
        let mut out = Vec::new();
        r.process(&input[..1], &mut out);
        r.flush(&mut out);
        let again = resample_all(&mut r, &input);
        assert_eq!(first, again);
    }

    #[test]
    fn channels_do_not_leak_into_each_other() {
        let mono_in = sine(3_000.0, 0.5, 8_000, 1);
        let stereo_in: Vec<f64> = mono_in.iter().flat_map(|&s| [s, 0.0]).collect();
        let mono = resample_all(&mut Resampler::new(FS_IN, 48_000, 1).unwrap(), &mono_in);
        let stereo = resample_all(&mut Resampler::new(FS_IN, 48_000, 2).unwrap(), &stereo_in);
        assert_eq!(stereo.len(), 2 * mono.len());
        for (i, f) in stereo.chunks_exact(2).enumerate() {
            assert_eq!(f[0].to_bits(), mono[i].to_bits());
            assert_eq!(f[1], 0.0);
        }
    }

    #[test]
    fn output_length_matches_the_ratio() {
        for fs_out in [32_000u32, 48_000, 88_200, 96_000, 192_000] {
            for frames in [0usize, 1, 7, 1_000, 44_100, 44_137] {
                let mut r = Resampler::new(FS_IN, fs_out, 2).unwrap();
                let out = resample_all(&mut r, &vec![0.25; 2 * frames]);
                let expected = (frames as u64 * fs_out as u64).div_ceil(FS_IN as u64) as usize;
                assert_eq!(out.len(), 2 * expected, "{fs_out} Hz, {frames} marcos");
            }
        }
    }

    #[test]
    fn ten_minutes_keep_the_exact_frame_count() {
        // 600 s en paquetes de 1024 marcos: sin deriva acumulada, y lo que queda retenido antes
        // del vaciado no pasa de media ventana.
        let frames_in = 600 * FS_IN as usize;
        let chunk = vec![0.0; 1024];
        let mut r = Resampler::new(FS_IN, 48_000, 1).unwrap();
        let mut out = Vec::new();
        let mut produced = 0usize;
        let mut fed = 0usize;
        while fed < frames_in {
            let n = chunk.len().min(frames_in - fed);
            r.process(&chunk[..n], &mut out);
            fed += n;
            produced += out.len();
            out.clear();
        }
        let expected = 600 * 48_000;
        let held = expected - produced;
        assert!(held <= r.taps / 2 * 48_000 / FS_IN as usize + 1, "retenidos {held}");
        r.flush(&mut out);
        produced += out.len();
        assert!(produced.abs_diff(expected) <= 1, "{produced} != {expected}");
    }

    #[test]
    fn downmix_averages_both_channels() {
        assert_eq!(downmix_stereo(&[1.0, 0.0, 0.5, -0.5, 0.25, 0.75]), vec![0.5, 0.0, 0.5]);
    }
}
