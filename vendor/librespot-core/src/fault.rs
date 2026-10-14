//! Fallos simulados para las pruebas de Nanofy (`NANOFY_FAULT`): reproducen, sin depender de que
//! Spotify falle de verdad, lo que hace que una reproducción no llegue a sonar. Claves denegadas o
//! sin respuesta, cortes de la CDN, peticiones colgadas, el límite de peticiones agotado y la
//! falta de salida de audio.
//!
//! La variable se lee una sola vez, la primera vez que alguien pregunta. Sin ella (o vacía) todo
//! esto es inerte: cada consulta mira un `OnceLock` ya resuelto a «nada que simular».
//!
//! Formato: lista separada por comas.
//! - `key_err:N`: las N primeras peticiones de clave de audio fallan como cuando Spotify las niega
//!   («error audio key 0 2»), sin llegar a enviarse (no gastan cupo de claves).
//! - `key_timeout:N`: las N primeras no reciben respuesta y agotan su tiempo límite (1,5 s).
//! - `cdn_stall_ms:N@P%`: una vez por proceso, cuando la reproducción pasa por el P % de un
//!   fichero, la CDN deja de entregar datos durante N ms. Cada lectura espera como la de verdad y,
//!   si lo que queda de corte pasa del tiempo límite de descarga, falla con `TimedOut` al agotarlo.
//! - `spclient_hang:<endpoint>`: las peticiones a spclient cuya ruta contiene `<endpoint>`
//!   (`connect-state`, `context-resolve`, `extended-metadata`, `storage-resolve`…) no vuelven
//!   nunca, como con un socket medio cerrado tras suspender el equipo.
//!   `spclient_hang:<endpoint>@<ms>` las retrasa solo ese tiempo.
//! - `limiter_exhaust`: con cada orden de reproducir, el presupuesto de librespot para spotify.com
//!   (300 peticiones cada 30 s) aparece agotado, como tras cargar una biblioteca grande.
//! - `no_output`: no hay ninguna salida de audio. `no_output:<ms>`: vuelve a haberla ese tiempo
//!   después del primer intento de abrirla.
//! - `ap_drop:<ms>`: una vez por proceso, el enlace con el punto de acceso (AP) se corta de golpe
//!   `<ms>` después de abrirse, como un `ConnectionReset` (Wi-Fi que se va, NAT que olvida la
//!   conexión): el enlace se pierde y se renueva dentro de la misma sesión (`ap_link`).
//!   `ap_drop_every:<ms>`: cada enlace, también los renovados, así que se renueva una y otra vez.
//! - `ap_silent:<ms>`: una vez por proceso, desde `<ms>` tras abrirse un enlace con el AP no llega
//!   nada por él (ni Ping ni respuestas de claves): muere en silencio y solo se nota al pedirle
//!   algo o cuando vence la espera del Ping. Le pasa a un solo enlace: el renovado va bien.
//! - `ap_renew_fail:<n>`: las `<n>` siguientes renovaciones del enlace con el AP fallan al momento
//!   como si no hubiera red, sin llegar a conectar (no gastan inicios de sesión en el AP).
//! - `dealer_nopong:<ms>`: una vez, el primer ping del dealer (websocket) tras `<ms>` se queda sin
//!   pong, como un corte de unos segundos: el dealer se reconecta y Spirc vuelve a registrarse.
//! - `spclient_err:<trozo>:<n>` (o `…:<n>@<saltar>`): tras dejar pasar `<saltar>` peticiones HTTP
//!   cuya ruta contiene `<trozo>`, las `<n>` siguientes fallan como un error de red
//!   (`autoplay`, `lexicon`, `connect-state`…). Con `…:<n>@t<ms>`, las `<n>` primeras desde
//!   `<ms>` después de arrancar.
//!
//! Como `ttfs`, no usa nada del resto del crate, para que sus pruebas corran en el binario.

use std::{
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// Corte de la CDN: cuánto dura y en qué punto del fichero empieza.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StallPlan {
    pub duration: Duration,
    /// Porcentaje del fichero (0–100) en el que empieza.
    pub pct: f64,
}

/// Qué debe hacer una lectura durante un corte simulado.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stall {
    /// Leer con normalidad.
    No,
    /// Esperar esto y leer: el corte acaba antes del tiempo límite de descarga.
    Wait(Duration),
    /// Esperar esto (el tiempo límite) y fallar con `TimedOut`, como `AudioFileStreaming::read`.
    TimedOut(Duration),
}

impl StallPlan {
    /// Lo que le queda al corte en `now`, si ya empezó (`started`) y no ha terminado.
    pub fn left(&self, started: Option<Instant>, now: Instant) -> Option<Duration> {
        let end = started? + self.duration;
        (now < end).then(|| end - now)
    }

    /// Decide qué hace una lectura de `read_len` bytes en `pos` de un fichero de `len` bytes.
    /// `started` es cuándo empezó el corte (`None`: aún no). Solo una lectura que pasa por el
    /// punto del corte lo empieza, y solo si `can_start` (el fichero ya está sonando: el sondeo
    /// del final y la búsqueda inicial del decodificador también leen más allá de ese punto).
    /// Empezado, el corte afecta a cualquier lectura hasta que termina, como una caída de red.
    pub fn decide(
        &self,
        started: &mut Option<Instant>,
        pos: u64,
        read_len: u64,
        len: u64,
        can_start: bool,
        now: Instant,
        timeout: Duration,
    ) -> Stall {
        let start = match *started {
            Some(s) => s,
            None => {
                if !can_start || len == 0 {
                    return Stall::No;
                }
                let threshold = (len as f64 * self.pct.clamp(0.0, 100.0) / 100.0) as u64;
                let end = pos.saturating_add(read_len.max(1));
                if !(pos <= threshold && threshold < end) {
                    return Stall::No;
                }
                *started = Some(now);
                now
            }
        };
        let end = start + self.duration;
        if now >= end {
            return Stall::No;
        }
        let left = end - now;
        if left <= timeout {
            Stall::Wait(left)
        } else {
            Stall::TimedOut(timeout)
        }
    }
}

/// Lo pedido en `NANOFY_FAULT`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spec {
    pub key_err: u32,
    pub key_timeout: u32,
    pub cdn_stall: Option<StallPlan>,
    /// (trozo de la ruta, retraso; `None` = para siempre).
    pub spclient_hang: Vec<(String, Option<Duration>)>,
    pub limiter_exhaust: bool,
    /// `Some(None)`: nunca hay salida; `Some(Some(d))`: vuelve a haberla tras `d`.
    pub no_output: Option<Option<Duration>>,
    /// (tras cuánto de abrirse cada enlace con el AP; `true`: en cada enlace, también los
    /// renovados).
    pub ap_drop: Option<(Duration, bool)>,
    pub ap_silent: Option<Duration>,
    /// Renovaciones del enlace con el AP que fallan sin red.
    pub ap_renew_fail: u32,
    pub dealer_nopong: Option<Duration>,
    /// (trozo de la ruta, cuántas fallan, cuántas se dejan pasar antes, desde cuándo).
    pub spclient_err: Vec<(String, u32, u32, Option<Duration>)>,
}

impl Spec {
    pub fn is_empty(&self) -> bool {
        *self == Spec::default()
    }
}

fn parse_ms(s: &str) -> Option<Duration> {
    s.trim().parse::<u64>().ok().filter(|&n| n > 0).map(Duration::from_millis)
}

/// Interpreta la lista. Devuelve lo entendido y las entradas que no se entendieron (se ignoran).
pub fn parse(s: &str) -> (Spec, Vec<String>) {
    let mut spec = Spec::default();
    let mut bad = Vec::new();
    for item in s.split(',').map(str::trim).filter(|i| !i.is_empty()) {
        let (name, value) = match item.split_once(':') {
            Some((n, v)) => (n.trim(), Some(v.trim())),
            None => (item, None),
        };
        let ok = match (name, value) {
            ("key_err", Some(v)) => v.parse().map(|n| spec.key_err = n).is_ok(),
            ("key_timeout", Some(v)) => v.parse().map(|n| spec.key_timeout = n).is_ok(),
            ("cdn_stall_ms", Some(v)) => {
                let plan = v.split_once('@').and_then(|(ms, pct)| {
                    let pct = pct.trim().trim_end_matches('%').trim().parse::<f64>().ok()?;
                    (0.0..=100.0).contains(&pct).then_some(StallPlan {
                        duration: parse_ms(ms)?,
                        pct,
                    })
                });
                plan.map(|p| spec.cdn_stall = Some(p)).is_some()
            }
            ("spclient_hang", Some(v)) if !v.is_empty() => {
                let hang = match v.split_once('@') {
                    Some((endpoint, ms)) => parse_ms(ms).map(|d| (endpoint.trim(), Some(d))),
                    None => Some((v, None)),
                };
                match hang {
                    Some((endpoint, d)) if !endpoint.is_empty() => {
                        spec.spclient_hang.push((endpoint.to_string(), d));
                        true
                    }
                    _ => false,
                }
            }
            ("limiter_exhaust", None) => {
                spec.limiter_exhaust = true;
                true
            }
            ("no_output", None) => {
                spec.no_output = Some(None);
                true
            }
            ("no_output", Some(v)) => parse_ms(v).map(|d| spec.no_output = Some(Some(d))).is_some(),
            ("ap_drop", Some(v)) => parse_ms(v).map(|d| spec.ap_drop = Some((d, false))).is_some(),
            ("ap_drop_every", Some(v)) => parse_ms(v).map(|d| spec.ap_drop = Some((d, true))).is_some(),
            ("ap_silent", Some(v)) => parse_ms(v).map(|d| spec.ap_silent = Some(d)).is_some(),
            ("ap_renew_fail", Some(v)) => v.parse().map(|n| spec.ap_renew_fail = n).is_ok(),
            ("dealer_nopong", Some(v)) => parse_ms(v).map(|d| spec.dealer_nopong = Some(d)).is_some(),
            ("spclient_err", Some(v)) => {
                let entry = v.rsplit_once(':').and_then(|(endpoint, n)| {
                    let (n, skip, after) = match n.split_once('@') {
                        Some((n, at)) => match at.trim().strip_prefix('t') {
                            Some(ms) => (n.trim().parse::<u32>().ok()?, 0, Some(parse_ms(ms)?)),
                            None => (n.trim().parse::<u32>().ok()?, at.trim().parse::<u32>().ok()?, None),
                        },
                        None => (n.trim().parse::<u32>().ok()?, 0, None),
                    };
                    let endpoint = endpoint.trim();
                    (!endpoint.is_empty() && n > 0).then(|| (endpoint.to_string(), n, skip, after))
                });
                entry.map(|e| spec.spclient_err.push(e)).is_some()
            }
            _ => false,
        };
        if !ok {
            bad.push(item.to_string());
        }
    }
    (spec, bad)
}

/// Lo que dura `spclient_hang` para una ruta: `None` si no se cuelga, `Some(None)` para siempre.
pub fn hang_for(spec: &Spec, path: &str) -> Option<Option<Duration>> {
    spec.spclient_hang
        .iter()
        .find(|(endpoint, _)| path.contains(endpoint.as_str()))
        .map(|(_, d)| *d)
}

/// ¿Falta la salida de audio en `now`? `since` es el primer intento de abrirla (se anota aquí).
pub fn output_missing(
    no_output: Option<Option<Duration>>,
    since: &mut Option<Instant>,
    now: Instant,
) -> bool {
    match no_output {
        None => false,
        Some(None) => true,
        Some(Some(d)) => now.saturating_duration_since(*since.get_or_insert(now)) < d,
    }
}

/// Cuál de las peticiones con ese trozo falla: `saltar` primero pasan, luego fallan `n`. `seen` es
/// cuántas ya se vieron antes de esta.
pub fn err_hits(n: u32, skip: u32, seen: u32) -> bool {
    seen >= skip && seen < skip.saturating_add(n)
}

struct Faults {
    raw: String,
    spec: Spec,
    key_err_left: AtomicU32,
    key_timeout_left: AtomicU32,
    stall_started: Mutex<Option<Instant>>,
    limiter_armed: AtomicBool,
    no_output_since: Mutex<Option<Instant>>,
    created: Instant,
    ap_drop_fired: AtomicBool,
    /// El enlace (número de `next_session_id`) que se quedó mudo; 0: ninguno aún.
    ap_silent_session: AtomicU64,
    ap_renew_fail_left: AtomicU32,
    dealer_nopong_fired: AtomicBool,
    /// Peticiones vistas por cada entrada de `spclient_err`.
    spclient_err_seen: Mutex<Vec<u32>>,
}

static FAULTS: OnceLock<Option<Faults>> = OnceLock::new();
/// Fallos simulados hasta ahora (cada clave, cada corte, cada petición colgada…), para que las
/// pruebas comprueben que el fallo pedido llegó a ocurrir.
static INJECTED: AtomicU64 = AtomicU64::new(0);

fn get() -> Option<&'static Faults> {
    FAULTS
        .get_or_init(|| {
            let raw = std::env::var("NANOFY_FAULT").ok()?;
            let raw = raw.trim();
            if raw.is_empty() {
                return None;
            }
            let (spec, bad) = parse(raw);
            for b in bad {
                log::warn!("NANOFY_FAULT: no se entiende «{b}»; se ignora");
            }
            if spec.is_empty() {
                return None;
            }
            log::warn!("NANOFY_FAULT activo: {raw}");
            Some(Faults {
                raw: raw.to_string(),
                key_err_left: AtomicU32::new(spec.key_err),
                key_timeout_left: AtomicU32::new(spec.key_timeout),
                ap_renew_fail_left: AtomicU32::new(spec.ap_renew_fail),
                spclient_err_seen: Mutex::new(vec![0; spec.spclient_err.len()]),
                spec,
                stall_started: Mutex::new(None),
                limiter_armed: AtomicBool::new(false),
                no_output_since: Mutex::new(None),
                created: Instant::now(),
                ap_drop_fired: AtomicBool::new(false),
                ap_silent_session: AtomicU64::new(0),
                dealer_nopong_fired: AtomicBool::new(false),
            })
        })
        .as_ref()
}

fn injected() {
    INJECTED.fetch_add(1, Ordering::Relaxed);
}

/// Gasta una de las `n` que quedan; `false` si ya no queda ninguna.
fn take(left: &AtomicU32) -> bool {
    left.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
        .is_ok()
}

/// La lista activa tal como se escribió; `None` sin fallos simulados.
pub fn describe() -> Option<&'static str> {
    get().map(|f| f.raw.as_str())
}

/// Cuántos fallos se han simulado ya.
pub fn injected_count() -> u64 {
    INJECTED.load(Ordering::Relaxed)
}

/// ¿Debe esta petición de clave fallar como denegada?
pub fn key_error() -> bool {
    let hit = get().is_some_and(|f| take(&f.key_err_left));
    if hit {
        injected();
    }
    hit
}

/// ¿Debe esta petición de clave quedarse sin respuesta?
pub fn key_timeout() -> bool {
    let hit = get().is_some_and(|f| take(&f.key_timeout_left));
    if hit {
        injected();
    }
    hit
}

/// Si una petición a spclient con esta ruta debe colgarse: `Some(None)` para siempre,
/// `Some(Some(d))` ese tiempo.
pub fn spclient_hang(path: &str) -> Option<Option<Duration>> {
    let hang = hang_for(&get()?.spec, path);
    if hang.is_some() {
        injected();
    }
    hang
}

/// Una orden de reproducir: con `limiter_exhaust`, la próxima petición a spotify.com encontrará
/// el presupuesto agotado.
pub fn arm_limiter_drain() {
    if let Some(f) = get().filter(|f| f.spec.limiter_exhaust) {
        f.limiter_armed.store(true, Ordering::Relaxed);
    }
}

/// ¿Hay que agotar ahora el presupuesto? Solo una vez por orden armada.
pub fn take_limiter_drain() -> bool {
    let hit = get().is_some_and(|f| f.limiter_armed.swap(false, Ordering::Relaxed));
    if hit {
        injected();
    }
    hit
}

/// ¿Falta ahora la salida de audio?
pub fn no_output() -> bool {
    let Some(f) = get() else { return false };
    if f.spec.no_output.is_none() {
        return false;
    }
    let mut since = f.no_output_since.lock().unwrap_or_else(|e| e.into_inner());
    let missing = output_missing(f.spec.no_output, &mut since, Instant::now());
    if missing {
        injected();
    }
    missing
}

/// Número para cada enlace con el AP (para `ap_silent`), desde 1.
pub fn next_session_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Si hay que cortar un enlace con el AP: tras cuánto de abrirse. Con `ap_drop` solo el primer
/// enlace que llega a ese tiempo (`ap_drop_claim`); con `ap_drop_every`, todos.
pub fn ap_drop_after() -> Option<(Duration, bool)> {
    get()?.spec.ap_drop
}

/// El enlace que llegó al corte: ¿le toca? (`ap_drop` una sola vez por proceso).
pub fn ap_drop_claim(every: bool) -> bool {
    let Some(f) = get() else { return false };
    let hit = every || !f.ap_drop_fired.swap(true, Ordering::Relaxed);
    if hit {
        injected();
    }
    hit
}

/// ¿Está mudo el enlace `id`, abierto en `started`? El primero que pasa del tiempo de `ap_silent`
/// se queda mudo para siempre; los demás, nunca.
pub fn ap_silent(id: u64, started: Instant) -> bool {
    let Some(f) = get() else { return false };
    let Some(after) = f.spec.ap_silent else { return false };
    let claimed = f.ap_silent_session.load(Ordering::Relaxed);
    if claimed != 0 {
        return claimed == id;
    }
    if started.elapsed() < after {
        return false;
    }
    let won = f
        .ap_silent_session
        .compare_exchange(0, id, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok();
    if won {
        injected();
        log::warn!("NANOFY_FAULT: el enlace con el AP se queda mudo (no llega nada más)");
    }
    won
}

/// ¿Debe fallar esta renovación del enlace con el AP como si no hubiera red (`ap_renew_fail`)?
pub fn ap_renew_fail() -> bool {
    let hit = get().is_some_and(|f| take(&f.ap_renew_fail_left));
    if hit {
        injected();
    }
    hit
}

/// ¿Se ignora el pong de este ping del dealer? Solo el primero tras el tiempo de `dealer_nopong`.
pub fn dealer_skip_pong() -> bool {
    let Some(f) = get() else { return false };
    let Some(after) = f.spec.dealer_nopong else { return false };
    if f.created.elapsed() < after || f.dealer_nopong_fired.swap(true, Ordering::Relaxed) {
        return false;
    }
    injected();
    log::warn!("NANOFY_FAULT: el ping del dealer se queda sin pong");
    true
}

/// ¿Debe fallar esta petición HTTP (`spclient_err`)?
pub fn spclient_error(path: &str) -> bool {
    let Some(f) = get() else { return false };
    if f.spec.spclient_err.is_empty() {
        return false;
    }
    let mut seen = f.spclient_err_seen.lock().unwrap_or_else(|e| e.into_inner());
    let mut hit = false;
    for (k, (endpoint, n, skip, after)) in f.spec.spclient_err.iter().enumerate() {
        // Con hora de empezar, las de antes ni cuentan.
        if path.contains(endpoint.as_str()) && after.is_none_or(|a| f.created.elapsed() >= a) {
            hit |= err_hits(*n, *skip, seen[k]);
            seen[k] += 1;
        }
    }
    if hit {
        injected();
    }
    hit
}

/// ¿Hay un corte de la CDN simulado? Para que el reproductor no compruebe nada más en cada lectura.
pub fn cdn_stall_planned() -> bool {
    get().is_some_and(|f| f.spec.cdn_stall.is_some())
}

/// Qué hace una lectura ante el corte de la CDN simulado (ver `StallPlan::decide`).
pub fn cdn_stall(pos: u64, read_len: u64, len: u64, can_start: bool, timeout: Duration) -> Stall {
    let Some((f, plan)) = get().and_then(|f| Some((f, f.spec.cdn_stall?))) else {
        return Stall::No;
    };
    let mut started = f.stall_started.lock().unwrap_or_else(|e| e.into_inner());
    let was_started = started.is_some();
    let stall = plan.decide(&mut started, pos, read_len, len, can_start, Instant::now(), timeout);
    if !was_started && started.is_some() {
        injected();
        log::warn!(
            "NANOFY_FAULT: la CDN deja de entregar datos durante {} ms (en el {} % del fichero)",
            plan.duration.as_millis(),
            plan.pct
        );
    }
    stall
}

/// ¿Está ahora mismo cortada la CDN simulada? Un error de lectura en ese rato es un corte de red,
/// no el final del fichero, aunque el fichero estuviera ya descargado entero.
pub fn cdn_stalled() -> bool {
    let Some(f) = get() else { return false };
    let Some(plan) = f.spec.cdn_stall else { return false };
    let started = *f.stall_started.lock().unwrap_or_else(|e| e.into_inner());
    started.is_some_and(|s| s.elapsed() < plan.duration)
}

/// Lo que le queda al corte de la CDN simulado, si está en marcha. El reproductor lo mira antes
/// de reanudar una canción cortada: la descarga de verdad no se entera del corte simulado (solo
/// las lecturas del decodificador), así que sin esto «la red ya volvió» desde el primer intento.
pub fn cdn_stall_left() -> Option<Duration> {
    let f = get()?;
    let plan = f.spec.cdn_stall?;
    let started = *f.stall_started.lock().unwrap_or_else(|e| e.into_inner());
    plan.left(started, Instant::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn lista_completa() {
        let (spec, bad) = parse(
            "key_err:2, key_timeout:1,cdn_stall_ms:15000@50%,spclient_hang:context-resolve,\
             spclient_hang:connect-state@3000,limiter_exhaust,no_output",
        );
        assert!(bad.is_empty(), "{bad:?}");
        assert_eq!(spec.key_err, 2);
        assert_eq!(spec.key_timeout, 1);
        assert_eq!(spec.cdn_stall, Some(StallPlan { duration: ms(15_000), pct: 50.0 }));
        assert_eq!(
            spec.spclient_hang,
            vec![
                ("context-resolve".to_string(), None),
                ("connect-state".to_string(), Some(ms(3000)))
            ]
        );
        assert!(spec.limiter_exhaust);
        assert_eq!(spec.no_output, Some(None));
        assert!(!spec.is_empty());
    }

    #[test]
    fn entradas_que_no_se_entienden() {
        let (spec, bad) = parse(
            "key_err:x,cdn_stall_ms:1000,cdn_stall_ms:1000@150%,cdn_stall_ms:0@5,spclient_hang:,\
             spclient_hang:@100,limiter_exhaust:3,no_output:0,otra,,  ,no_output:2500",
        );
        assert_eq!(
            bad,
            [
                "key_err:x",
                "cdn_stall_ms:1000",
                "cdn_stall_ms:1000@150%",
                "cdn_stall_ms:0@5",
                "spclient_hang:",
                "spclient_hang:@100",
                "limiter_exhaust:3",
                "no_output:0",
                "otra"
            ]
        );
        // Lo que sí se entendió queda, aunque el resto no.
        assert_eq!(spec.no_output, Some(Some(ms(2500))));
        assert_eq!(spec.key_err, 0);
        assert!(spec.cdn_stall.is_none());
        // El porcentaje sin «%» también vale.
        assert_eq!(parse("cdn_stall_ms:500@25").0.cdn_stall.map(|p| p.pct), Some(25.0));
        assert!(parse("").0.is_empty());
        assert!(parse(" , ").0.is_empty());
    }

    #[test]
    fn peticiones_colgadas_por_ruta() {
        let (spec, _) = parse("spclient_hang:context-resolve,spclient_hang:storage-resolve@800");
        assert_eq!(hang_for(&spec, "/context-resolve/v1/spotify:playlist:x"), Some(None));
        assert_eq!(
            hang_for(&spec, "/storage-resolve/files/audio/interactive/abc"),
            Some(Some(ms(800)))
        );
        assert_eq!(hang_for(&spec, "/connect-state/v1/devices/x"), None);
        assert_eq!(hang_for(&Spec::default(), "/context-resolve/v1/x"), None);
    }

    /// El corte empieza solo con la lectura que pasa por su punto y si el fichero ya suena (no con
    /// el sondeo del final ni con la búsqueda inicial). Mientras dura, cada lectura espera; lo que
    /// pasa del tiempo límite de descarga falla al agotarlo; luego se lee con normalidad, y no
    /// vuelve a empezar.
    #[test]
    fn corte_de_cdn_en_su_punto_y_con_su_duracion() {
        let plan = StallPlan { duration: ms(15_000), pct: 50.0 };
        let timeout = ms(8000);
        let len = 10_000_000;
        let t0 = Instant::now();
        let mut started = None;

        // Lectura de `n` bytes en `pos` a los `t` ms; `sonando`: el fichero ya pasó su carga.
        let mut read = |pos: u64, n: u64, sonando: bool, t: u64| {
            plan.decide(&mut started, pos, n, len, sonando, t0 + ms(t), timeout)
        };
        // Antes del punto, o más allá sin pasar por él (el sondeo del final), nada.
        assert_eq!(read(0, 32_768, true, 0), Stall::No);
        assert_eq!(read(9_934_693, 65_536, true, 0), Stall::No);
        // Pasa por el punto, pero el fichero aún se está abriendo: nada.
        assert_eq!(read(4_990_000, 32_768, false, 0), Stall::No);

        // Sonando: empieza. Quedan 15 s, más que el límite: espera 8 s y falla.
        assert_eq!(read(4_990_000, 32_768, true, 100), Stall::TimedOut(timeout));
        // 8 s después (la búsqueda para retomar, en cualquier punto): quedan 7 s, espera y lee.
        assert_eq!(read(4_000_000, 4096, false, 8100), Stall::Wait(ms(7000)));
        // Terminado el corte, todo normal, también al volver a pasar por el punto.
        assert_eq!(read(4_990_000, 32_768, true, 15_100), Stall::No);
        assert_eq!(read(4_999_999, 2, true, 20_000), Stall::No);
        // Empezó una sola vez: con la lectura sonando que pasó por el punto.
        assert_eq!(started, Some(t0 + ms(100)));

        // Un corte corto solo hace esperar.
        let corto = StallPlan { duration: ms(2000), pct: 0.0 };
        let mut s = None;
        assert_eq!(corto.decide(&mut s, 0, 1, len, true, t0, timeout), Stall::Wait(ms(2000)));
        // Fichero vacío: nunca.
        let mut s = None;
        assert_eq!(corto.decide(&mut s, 0, 1, 0, true, t0, timeout), Stall::No);
    }

    #[test]
    fn lo_que_le_queda_al_corte() {
        let plan = StallPlan { duration: ms(15_000), pct: 50.0 };
        let t0 = Instant::now();
        // Sin empezar no queda nada que esperar.
        assert_eq!(plan.left(None, t0), None);
        assert_eq!(plan.left(Some(t0), t0), Some(ms(15_000)));
        assert_eq!(plan.left(Some(t0), t0 + ms(9_000)), Some(ms(6_000)));
        // Terminado (justo en el final y después).
        assert_eq!(plan.left(Some(t0), t0 + ms(15_000)), None);
        assert_eq!(plan.left(Some(t0), t0 + ms(20_000)), None);
    }

    #[test]
    fn salida_que_falta_y_vuelve() {
        let t0 = Instant::now();
        let mut since = None;
        assert!(!output_missing(None, &mut since, t0));
        assert!(since.is_none());
        assert!(output_missing(Some(None), &mut since, t0 + ms(99_999)));
        let vuelve = Some(Some(ms(3000)));
        assert!(output_missing(vuelve, &mut since, t0));
        assert_eq!(since, Some(t0));
        assert!(output_missing(vuelve, &mut since, t0 + ms(2999)));
        assert!(!output_missing(vuelve, &mut since, t0 + ms(3000)));
    }

    #[test]
    fn fallos_de_red() {
        let (spec, bad) = parse(
            "ap_drop:60000,ap_silent:30000,dealer_nopong:45000,spclient_err:autoplay:2,\
             spclient_err:connect-state:3@1,spclient_err:lexicon-session-provider/context-resolve:1",
        );
        assert!(bad.is_empty(), "{bad:?}");
        assert_eq!(spec.ap_drop, Some((ms(60_000), false)));
        assert_eq!(spec.ap_silent, Some(ms(30_000)));
        assert_eq!(spec.dealer_nopong, Some(ms(45_000)));
        assert_eq!(
            spec.spclient_err,
            vec![
                ("autoplay".to_string(), 2, 0, None),
                ("connect-state".to_string(), 3, 1, None),
                ("lexicon-session-provider/context-resolve".to_string(), 1, 0, None)
            ]
        );
        assert_eq!(
            parse("spclient_err:connect-state:3@t40000").0.spclient_err,
            vec![("connect-state".to_string(), 3, 0, Some(ms(40_000)))]
        );
        assert_eq!(parse("spclient_err:x:1@t0").1, ["spclient_err:x:1@t0"]);
        assert_eq!(parse("ap_drop_every:5000").0.ap_drop, Some((ms(5000), true)));
        let (_, bad) = parse("ap_drop:0,ap_silent,spclient_err:autoplay,spclient_err::2,spclient_err:x:0,spclient_err:x:1@y");
        assert_eq!(bad.len(), 6, "{bad:?}");
        // Dejar pasar 1 y fallar 3: la 1.ª pasa, de la 2.ª a la 4.ª fallan, luego pasan.
        let hits: Vec<bool> = (0..6).map(|seen| err_hits(3, 1, seen)).collect();
        assert_eq!(hits, [false, true, true, true, false, false]);
        assert!(err_hits(2, 0, 0) && err_hits(2, 0, 1) && !err_hits(2, 0, 2));
    }

    /// Renovaciones del enlace que fallan sin red: exactamente las pedidas.
    #[test]
    fn renovaciones_que_fallan() {
        let (spec, bad) = parse("ap_drop:20000,ap_renew_fail:3");
        assert!(bad.is_empty(), "{bad:?}");
        assert_eq!(spec.ap_renew_fail, 3);
        assert_eq!(spec.ap_drop, Some((ms(20_000), false)));
        let left = AtomicU32::new(spec.ap_renew_fail);
        let fired = (0..10).filter(|_| take(&left)).count();
        assert_eq!(fired, 3);
        assert_eq!(parse("ap_renew_fail:x").1, ["ap_renew_fail:x"]);
        assert_eq!(parse("ap_renew_fail").1, ["ap_renew_fail"]);
        assert!(parse("").0.ap_renew_fail == 0);
    }

    #[test]
    fn contador_que_se_gasta() {
        let left = AtomicU32::new(2);
        assert!(take(&left));
        assert!(take(&left));
        assert!(!take(&left));
        assert!(!take(&left));
        assert_eq!(left.load(Ordering::Relaxed), 0);
    }
}
