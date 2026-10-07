//! Claves de audio robustas (Nanofy): reintentos ante una negativa pasajera de Spotify, caché en
//! memoria de las claves ya concedidas y el estado «claves frenadas» de toda la sesión.
//!
//! Los reintentos siguen el PR #1763 de librespot: hasta 3 intentos con 1 s entre uno y otro, y
//! solo si Spotify contestó con el código 0x0002 (una negativa pasajera, típica de las ráfagas de
//! peticiones: repetida al cabo de un segundo se concede) o si la respuesta no llegó a tiempo.
//! Cualquier otro código se devuelve al momento: reintentar solo retrasaría la siguiente canción.
//!
//! Como `ttfs` y `fault`, no usa nada del resto del crate, para que sus pruebas corran en el
//! binario de Nanofy.

use std::{
    collections::VecDeque,
    future::Future,
    sync::Mutex,
    time::{Duration, Instant},
};

/// Código con el que Spotify niega una clave de forma pasajera («error audio key 0 2»).
pub const TRANSIENT_DENIAL: u16 = 0x0002;
/// Intentos por petición de clave, contando el primero.
pub const ATTEMPTS: u32 = 3;
/// Espera fija entre intentos.
pub const RETRY_DELAY: Duration = Duration::from_secs(1);
/// Claves que se recuerdan (solo en memoria, nunca en disco): bastan para repetir, volver atrás,
/// recargar tras «Reiniciar» o retomar tras una reconexión sin pedir otra vez la misma clave.
pub const CACHE_CAP: usize = 64;
/// Tras una negativa pasajera, durante cuánto se considera que Spotify está frenando las claves:
/// mientras tanto no se precarga nada por adelantado (solo la siguiente cerca del final).
pub const THROTTLE_FOR: Duration = Duration::from_secs(5 * 60);

/// Cómo terminó un intento fallido de pedir una clave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attempt {
    /// Spotify la negó con este código.
    Denied(u16),
    /// No llegó respuesta a tiempo.
    Timeout,
    /// Otro fallo (la sesión se cerró, el canal se cortó…): reintentar no lo arregla.
    Other,
}

/// ¿Merece otro intento? Solo la negativa pasajera y la falta de respuesta.
pub fn retryable(attempt: Attempt) -> bool {
    matches!(attempt, Attempt::Denied(TRANSIENT_DENIAL) | Attempt::Timeout)
}

/// Llama a `request(n)` (n = 0, 1, 2…) hasta que dé una clave, hasta `attempts` veces, esperando
/// `delay` entre intentos. Solo reintenta lo que `classify` considera pasajero (`retryable`); el
/// resto se devuelve al momento.
pub async fn with_retries<T, E, F, Fut>(
    attempts: u32,
    delay: Duration,
    classify: impl Fn(&E) -> Attempt,
    mut request: F,
) -> Result<T, E>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let attempts = attempts.max(1);
    let mut n = 0;
    loop {
        match request(n).await {
            Ok(value) => {
                if n > 0 {
                    log::info!("Clave de audio concedida al intento {}", n + 1);
                }
                return Ok(value);
            }
            Err(e) => {
                n += 1;
                let kind = classify(&e);
                if n >= attempts || !retryable(kind) {
                    return Err(e);
                }
                log::warn!(
                    "Clave de audio no concedida ({kind:?}); intento {} de {attempts} en {} ms",
                    n + 1,
                    delay.as_millis()
                );
                tokio::time::sleep(delay).await;
            }
        }
    }
}

/// Lista de lo usado más recientemente con un tope: lo más antiguo sale primero. Con 64 entradas
/// una búsqueda lineal cuesta menos que un mapa, y cabe en un `static` (constructor `const`).
pub struct Lru<K, V> {
    cap: usize,
    /// Del más antiguo (delante) al más reciente (detrás).
    items: VecDeque<(K, V)>,
}

impl<K: PartialEq, V: Clone> Lru<K, V> {
    pub const fn new(cap: usize) -> Self {
        Self {
            cap,
            items: VecDeque::new(),
        }
    }

    /// El valor de `key`, que pasa a ser el más reciente.
    pub fn get(&mut self, key: &K) -> Option<V> {
        let i = self.items.iter().position(|(k, _)| k == key)?;
        let item = self.items.remove(i)?;
        let value = item.1.clone();
        self.items.push_back(item);
        Some(value)
    }

    /// Guarda `value` como el más reciente; si ya no cabe, sale el más antiguo.
    pub fn put(&mut self, key: K, value: V) {
        if let Some(i) = self.items.iter().position(|(k, _)| *k == key) {
            self.items.remove(i);
        }
        self.items.push_back((key, value));
        while self.items.len() > self.cap {
            self.items.pop_front();
        }
    }

    pub fn remove(&mut self, key: &K) {
        self.items.retain(|(k, _)| k != key);
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Hasta cuándo Spotify está frenando las claves (la última negativa pasajera más `THROTTLE_FOR`).
#[derive(Debug, Default)]
pub struct Throttle {
    until: Option<Instant>,
}

impl Throttle {
    pub const fn new() -> Self {
        Self { until: None }
    }

    /// Una negativa pasajera en `now`: el freno dura `THROTTLE_FOR` desde la última.
    pub fn note(&mut self, now: Instant) {
        self.until = Some(now + THROTTLE_FOR);
    }

    pub fn active(&self, now: Instant) -> bool {
        self.until.is_some_and(|until| now < until)
    }

    /// Lo que le queda al freno en `now`; `None` si no hay.
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        self.until
            .and_then(|until| until.checked_duration_since(now))
            .filter(|d| !d.is_zero())
    }
}

/// Estado de toda la sesión (del proceso: sobrevive a reconexiones, como el freno de Spotify, que
/// es de la cuenta y no de la conexión).
static THROTTLE: Mutex<Throttle> = Mutex::new(Throttle::new());

fn throttle() -> std::sync::MutexGuard<'static, Throttle> {
    THROTTLE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Spotify acaba de negar una clave con el código pasajero.
pub fn note_transient_denial() {
    let was = keys_throttled();
    throttle().note(Instant::now());
    if !was {
        log::warn!(
            "Spotify está frenando las claves de audio: sin precarga temprana durante {} min",
            THROTTLE_FOR.as_secs() / 60
        );
    }
}

/// ¿Está Spotify frenando las claves ahora mismo? Mientras tanto no se pide nada por adelantado.
pub fn keys_throttled() -> bool {
    throttle().active(Instant::now())
}

/// Lo que le queda al freno de las claves, si lo hay.
pub fn keys_throttled_for() -> Option<Duration> {
    throttle().remaining(Instant::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Error de prueba: lo que contestaría el servidor en cada intento.
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Fallo {
        Codigo(u16),
        SinRespuesta,
        Canal,
    }

    fn clasifica(f: &Fallo) -> Attempt {
        match *f {
            Fallo::Codigo(c) => Attempt::Denied(c),
            Fallo::SinRespuesta => Attempt::Timeout,
            Fallo::Canal => Attempt::Other,
        }
    }

    /// Simula un servidor que contesta, en orden, lo de `respuestas` (y después concede).
    async fn pide(respuestas: &[Fallo], intentos: &Cell<u32>) -> Result<[u8; 16], Fallo> {
        with_retries(ATTEMPTS, ms(1), clasifica, |n| {
            intentos.set(intentos.get() + 1);
            assert_eq!(n + 1, intentos.get(), "el número de intento va de uno en uno");
            let r = match respuestas.get(n as usize) {
                Some(f) => Err(*f),
                None => Ok([7u8; 16]),
            };
            async move { r }
        })
        .await
    }

    #[tokio::test]
    async fn negada_dos_veces_con_codigo_2_y_luego_concedida() {
        let intentos = Cell::new(0);
        let r = pide(&[Fallo::Codigo(2), Fallo::Codigo(2)], &intentos).await;
        assert_eq!(r, Ok([7u8; 16]));
        assert_eq!(intentos.get(), 3);
    }

    #[tokio::test]
    async fn sin_respuesta_tambien_se_reintenta() {
        let intentos = Cell::new(0);
        let r = pide(&[Fallo::SinRespuesta], &intentos).await;
        assert_eq!(r, Ok([7u8; 16]));
        assert_eq!(intentos.get(), 2);
    }

    #[tokio::test]
    async fn como_mucho_tres_intentos() {
        let intentos = Cell::new(0);
        let r = pide(&[Fallo::Codigo(2); 5], &intentos).await;
        assert_eq!(r, Err(Fallo::Codigo(2)));
        assert_eq!(intentos.get(), ATTEMPTS);

        let intentos = Cell::new(0);
        let r = pide(&[Fallo::SinRespuesta, Fallo::Codigo(2), Fallo::SinRespuesta], &intentos).await;
        assert_eq!(r, Err(Fallo::SinRespuesta));
        assert_eq!(intentos.get(), 3);
    }

    #[tokio::test]
    async fn otros_codigos_vuelven_al_momento() {
        for f in [Fallo::Codigo(1), Fallo::Codigo(0x0101), Fallo::Codigo(0), Fallo::Canal] {
            let intentos = Cell::new(0);
            let r = pide(&[f], &intentos).await;
            assert_eq!(r, Err(f));
            assert_eq!(intentos.get(), 1, "{f:?} no debe reintentarse");
        }
        // Un código definitivo tras uno pasajero corta ahí.
        let intentos = Cell::new(0);
        let r = pide(&[Fallo::Codigo(2), Fallo::Codigo(1)], &intentos).await;
        assert_eq!(r, Err(Fallo::Codigo(1)));
        assert_eq!(intentos.get(), 2);
    }

    #[tokio::test]
    async fn la_espera_entre_intentos_se_respeta() {
        let intentos = Cell::new(0);
        let t0 = Instant::now();
        let r = with_retries(3, ms(30), clasifica, |_| {
            intentos.set(intentos.get() + 1);
            let r = if intentos.get() < 3 { Err(Fallo::Codigo(2)) } else { Ok(()) };
            async move { r }
        })
        .await;
        assert_eq!(r, Ok(()));
        // Dos esperas de 30 ms entre los tres intentos.
        assert!(t0.elapsed() >= ms(60), "{:?}", t0.elapsed());
    }

    #[test]
    fn que_se_reintenta() {
        assert!(retryable(Attempt::Denied(TRANSIENT_DENIAL)));
        assert!(retryable(Attempt::Timeout));
        assert!(!retryable(Attempt::Denied(1)));
        assert!(!retryable(Attempt::Denied(0x0200)));
        assert!(!retryable(Attempt::Other));
    }

    #[test]
    fn cache_de_claves_con_tope() {
        let mut c: Lru<(u32, u32), u8> = Lru::new(3);
        assert!(c.is_empty());
        c.put((1, 1), 10);
        c.put((2, 2), 20);
        c.put((3, 3), 30);
        assert_eq!(c.len(), 3);
        // Usar la primera la hace la más reciente: sale la segunda al llegar la cuarta.
        assert_eq!(c.get(&(1, 1)), Some(10));
        c.put((4, 4), 40);
        assert_eq!(c.len(), 3);
        assert_eq!(c.get(&(2, 2)), None);
        assert_eq!(c.get(&(1, 1)), Some(10));
        assert_eq!(c.get(&(3, 3)), Some(30));
        assert_eq!(c.get(&(4, 4)), Some(40));
        // Guardar otra vez la misma clave no la duplica.
        c.put((4, 4), 41);
        assert_eq!(c.len(), 3);
        assert_eq!(c.get(&(4, 4)), Some(41));
        // La misma canción con otro fichero es otra clave.
        assert_eq!(c.get(&(4, 5)), None);
        c.remove(&(4, 4));
        assert_eq!(c.get(&(4, 4)), None);
        assert_eq!(c.len(), 2);
        c.clear();
        assert!(c.is_empty());

        // El tope de verdad.
        let mut c: Lru<u32, u32> = Lru::new(CACHE_CAP);
        for i in 0..200 {
            c.put(i, i);
        }
        assert_eq!(c.len(), CACHE_CAP);
        assert_eq!(c.get(&(200 - CACHE_CAP as u32 - 1)), None);
        assert_eq!(c.get(&(200 - CACHE_CAP as u32)), Some(200 - CACHE_CAP as u32));
    }

    #[test]
    fn freno_de_claves_cinco_minutos_desde_la_ultima_negativa() {
        let t0 = Instant::now();
        let mut t = Throttle::new();
        assert!(!t.active(t0));
        assert_eq!(t.remaining(t0), None);
        t.note(t0);
        assert!(t.active(t0));
        assert!(t.active(t0 + THROTTLE_FOR - ms(1)));
        assert!(!t.active(t0 + THROTTLE_FOR));
        assert_eq!(t.remaining(t0 + ms(1000)), Some(THROTTLE_FOR - ms(1000)));
        // Otra negativa alarga el freno desde ella.
        t.note(t0 + ms(60_000));
        assert!(t.active(t0 + THROTTLE_FOR));
        assert!(!t.active(t0 + ms(60_000) + THROTTLE_FOR));
        assert_eq!(t.remaining(t0 + ms(60_000) + THROTTLE_FOR), None);
    }
}
