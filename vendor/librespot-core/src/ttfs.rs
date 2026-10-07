//! Instrumentación de Nanofy: tiempo hasta el primer sonido («ttfs», *time to first sound*)
//! desglosado por fases, y contadores de lo que hace fallar una reproducción.
//!
//! Va en librespot-core porque una orden atraviesa la interfaz, Spirc (librespot-connect), el
//! reproductor (librespot-playback) y la red (este crate), y todos dependen de core: es el único
//! sitio desde el que cada capa puede dejar su marca en la misma medida.
//!
//! Cómo se mide:
//! - la interfaz abre una medida al pedir algo (`begin`): reproducir, siguiente, anterior,
//!   reanudar, pausar o buscar. Solo hay una abierta: la orden siguiente la sustituye y la
//!   anterior queda «abandonada», que no es lo mismo que fallida;
//! - cada capa marca su fase (`mark`): Spirc al recibir la orden y al resolver el contexto; el
//!   reproductor al aceptar la carga (`claim`), y su hilo de carga, atado a la medida con
//!   `bind_thread`, al tener los metadatos, el almacenamiento, la clave, la cabecera y la cola del
//!   fichero, el decodificador y la posición; luego la salida y el evento de «sonando»;
//! - la medida se cierra donde se oye: la primera escritura en la salida tras empezar a sonar
//!   (`on_output`), o, en una pausa, cuando la salida se ha detenido.
//!
//! El indicador principal no es el tiempo sino las reproducciones fallidas (`Counter`): una carga
//! que no llega a sonar es lo que hace abandonar una aplicación, no 100 ms de más.
//!
//! Sin medida abierta, cada marca cuesta una lectura atómica: no cambia nada de lo que hace el
//! reproductor, solo añade líneas de depuración. El módulo no usa nada del resto del crate (solo
//! std y log) para que sus pruebas se compilen también en el binario de Nanofy, cuyo `cargo test`
//! no ejecuta las de las dependencias.

use std::{
    cell::Cell,
    fmt::Write as _,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// Tope de fases por medida: una orden que se encadena con otras (reintentos, saltos) no hace
/// crecer la lista sin fin.
pub const MAX_PHASES: usize = 40;

/// Una carga que llega más tarde que esto tras la orden ya no es de esa orden (el paso automático
/// a la canción siguiente, por ejemplo) y no se le atribuye.
pub const CLAIM_WINDOW: Duration = Duration::from_secs(30);

/// Órdenes que cargan una canción: son las «reproducciones» del indicador de fallos.
pub fn is_play(kind: &str) -> bool {
    matches!(kind, "play" | "next" | "prev")
}

/// Cómo terminó una medida.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Outcome {
    /// Todavía no ha sonado ni fallado.
    #[default]
    Pending,
    /// Llegó a la salida (en una pausa: la salida se detuvo).
    Done,
    /// La carga falló. El indicador principal: debe quedarse en cero.
    Failed,
    /// Llegó otra orden antes de que sonara.
    Abandoned,
}

impl Outcome {
    pub fn name(self) -> &'static str {
        match self {
            Outcome::Pending => "pending",
            Outcome::Done => "done",
            Outcome::Failed => "failed",
            Outcome::Abandoned => "abandoned",
        }
    }
}

/// Una marca: qué pasó y cuándo, desde la orden y desde la marca anterior.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Phase {
    pub name: &'static str,
    pub at_ms: u64,
    pub ms: u64,
    /// Detalle opcional: «miss», «cache», el error…
    pub info: Option<String>,
}

/// Copia de la última medida, para el modo de control.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Breakdown {
    pub seq: u64,
    pub kind: &'static str,
    /// La canción que cargó el reproductor para esta orden (si llegó a cargar alguna).
    pub track: Option<String>,
    pub outcome: Outcome,
    /// De la orden a la marca final (la que se oye, el fallo…); `None` mientras está pendiente.
    pub total_ms: Option<u64>,
    /// Desde la orden hasta ahora: una medida pendiente con mucha edad es una carga atascada.
    pub age_ms: u64,
    pub phases: Vec<Phase>,
    pub error: Option<String>,
}

/// El último fallo de carga, medido o no (el paso automático a la siguiente también cuenta).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LastError {
    /// Medida a la que pertenecía; 0 si la carga no venía de una orden medida (precarga, paso
    /// automático a la siguiente canción).
    pub seq: u64,
    /// Tipo de la orden medida («play», «next»…); vacío si no lo era.
    pub kind: &'static str,
    pub track: String,
    pub what: String,
    pub at: Instant,
}

struct Measure {
    seq: u64,
    kind: &'static str,
    t0: Instant,
    /// Momento de la marca anterior (desde `t0`), para el tiempo de cada fase.
    last_at: Duration,
    phases: Vec<Phase>,
    outcome: Outcome,
    total: Option<Duration>,
    track: Option<String>,
    claimed: bool,
    error: Option<String>,
}

fn ms(d: Duration) -> u64 {
    d.as_millis().min(u64::MAX as u128) as u64
}

impl Measure {
    fn push(&mut self, name: &'static str, info: Option<String>, now: Instant) -> Phase {
        let at = now.saturating_duration_since(self.t0);
        let phase = Phase {
            name,
            at_ms: ms(at),
            ms: ms(at.saturating_sub(self.last_at)),
            info,
        };
        // Pasado el tope se deja de guardar, pero el tiempo de la fase siguiente sigue contando
        // desde aquí.
        if self.phases.len() < MAX_PHASES {
            self.phases.push(phase.clone());
        }
        self.last_at = at;
        phase
    }

    fn is_pending(&self) -> bool {
        self.outcome == Outcome::Pending
    }
}

/// La medida en curso y el último fallo. Las funciones libres del módulo usan una global; las
/// pruebas usan instancias propias (y un `Instant` inventado) para no pisarse entre ellas.
pub struct Recorder {
    next_seq: u64,
    current: Option<Measure>,
    last_error: Option<LastError>,
}

impl Default for Recorder {
    fn default() -> Self {
        Self::new()
    }
}

impl Recorder {
    pub const fn new() -> Self {
        Self {
            next_seq: 0,
            current: None,
            last_error: None,
        }
    }

    /// Abre una medida. Devuelve su número y, si había otra pendiente, el tipo de la que quedó
    /// abandonada.
    pub fn begin(&mut self, kind: &'static str, now: Instant) -> (u64, Option<&'static str>) {
        let abandoned = match &mut self.current {
            Some(m) if m.is_pending() => {
                m.outcome = Outcome::Abandoned;
                Some(m.kind)
            }
            _ => None,
        };
        self.next_seq += 1;
        let seq = self.next_seq;
        let mut m = Measure {
            seq,
            kind,
            t0: now,
            last_at: Duration::ZERO,
            phases: Vec::new(),
            outcome: Outcome::Pending,
            total: None,
            track: None,
            claimed: false,
            error: None,
        };
        m.push("cmd", None, now);
        self.current = Some(m);
        (seq, abandoned)
    }

    fn pending_mut(&mut self, seq: u64) -> Option<&mut Measure> {
        self.current
            .as_mut()
            .filter(|m| seq != 0 && m.seq == seq && m.is_pending())
    }

    /// Número de la medida pendiente; 0 si no hay ninguna.
    pub fn pending_seq(&self) -> u64 {
        self.current
            .as_ref()
            .filter(|m| m.is_pending())
            .map_or(0, |m| m.seq)
    }

    /// La medida pendiente si es de uno de estos tipos; 0 si no.
    pub fn pending_of(&self, kinds: &[&str]) -> u64 {
        self.current
            .as_ref()
            .filter(|m| m.is_pending() && kinds.contains(&m.kind))
            .map_or(0, |m| m.seq)
    }

    /// Marca una fase de la medida `seq`, si sigue pendiente.
    pub fn mark(
        &mut self,
        seq: u64,
        name: &'static str,
        info: Option<String>,
        now: Instant,
    ) -> Option<Phase> {
        self.pending_mut(seq).map(|m| m.push(name, info, now))
    }

    /// El reproductor empieza a cargar `track`: si es la primera carga tras una orden pendiente
    /// (y no una pausa, que no carga nada), la carga pasa a ser la de esa orden. Devuelve el
    /// número de la medida, o 0 si la carga no es de ninguna (precarga, paso automático, una
    /// segunda carga encadenada a la primera…).
    pub fn claim(&mut self, track: impl FnOnce() -> String, now: Instant) -> u64 {
        match &mut self.current {
            Some(m)
                if m.is_pending()
                    && m.kind != "pause"
                    && !m.claimed
                    && now.saturating_duration_since(m.t0) < CLAIM_WINDOW =>
            {
                m.claimed = true;
                m.track = Some(track());
                m.push("player:load", None, now);
                m.seq
            }
            _ => 0,
        }
    }

    /// Cierra la medida con su última marca. Devuelve su tipo si estaba pendiente.
    pub fn finish(
        &mut self,
        seq: u64,
        name: &'static str,
        info: Option<String>,
        now: Instant,
    ) -> Option<&'static str> {
        let m = self.pending_mut(seq)?;
        m.push(name, info, now);
        m.outcome = Outcome::Done;
        m.total = Some(now.saturating_duration_since(m.t0));
        Some(m.kind)
    }

    /// La carga de la medida `seq` falló. Devuelve su tipo si estaba pendiente; si ya no lo estaba
    /// (otra orden la sustituyó, o el hilo de carga ya contó el fallo con más detalle), nada.
    pub fn fail(&mut self, seq: u64, what: String, now: Instant) -> Option<&'static str> {
        let m = self.pending_mut(seq)?;
        m.push("error", Some(what.clone()), now);
        m.outcome = Outcome::Failed;
        m.total = Some(now.saturating_duration_since(m.t0));
        m.error = Some(what.clone());
        let kind = m.kind;
        self.last_error = Some(LastError {
            seq,
            kind,
            track: m.track.clone().unwrap_or_default(),
            what,
            at: now,
        });
        Some(kind)
    }

    /// Un fallo de carga de `track`. Si la carga era la de la medida `seq` (pendiente), la medida
    /// falla; en cualquier caso queda como último fallo. Devuelve el tipo de la medida fallida.
    pub fn load_failed(
        &mut self,
        seq: u64,
        track: String,
        what: String,
        now: Instant,
    ) -> Option<&'static str> {
        if let Some(kind) = self.fail(seq, what.clone(), now) {
            if let Some(e) = &mut self.last_error {
                e.track = track;
            }
            return Some(kind);
        }
        self.last_error = Some(LastError {
            seq: 0,
            kind: "",
            track,
            what,
            at: now,
        });
        None
    }

    pub fn snapshot(&self, now: Instant) -> Option<Breakdown> {
        let m = self.current.as_ref()?;
        Some(Breakdown {
            seq: m.seq,
            kind: m.kind,
            track: m.track.clone(),
            outcome: m.outcome,
            total_ms: m.total.map(ms),
            age_ms: ms(now.saturating_duration_since(m.t0)),
            phases: m.phases.clone(),
            error: m.error.clone(),
        })
    }

    pub fn last_error(&self) -> Option<&LastError> {
        self.last_error.as_ref()
    }
}

/// Una línea legible con todas las fases («cmd 0 · spirc:load 4 (+4) · metadata 92 (+88) miss…»).
pub fn summary(b: &Breakdown) -> String {
    let mut s = String::new();
    for (i, p) in b.phases.iter().enumerate() {
        if i > 0 {
            s.push_str(" · ");
        }
        let _ = write!(s, "{} {}", p.name, p.at_ms);
        if i > 0 {
            let _ = write!(s, " (+{})", p.ms);
        }
        if let Some(info) = &p.info {
            let _ = write!(s, " {info}");
        }
    }
    s
}

/// Contadores de todo el proceso (no se reinician): las pruebas leen la diferencia entre antes y
/// después de cada ronda.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Counter {
    /// Órdenes que cargan una canción (reproducir, siguiente, anterior).
    Plays,
    /// …que llegaron a la salida.
    PlaysAudible,
    /// …cuya carga falló.
    PlaysFailed,
    /// …sustituidas por otra orden antes de sonar.
    PlaysAbandoned,
    /// Claves de audio denegadas por Spotify («error audio key 0 2»).
    KeyErrors,
    /// Claves de audio sin respuesta a tiempo.
    KeyTimeouts,
    /// Peticiones que el limitador propio de librespot rechazó (300 cada 30 s por dominio).
    RateLimited,
    /// Respuestas 429 de Spotify.
    Http429,
    /// Canciones que el reproductor dio por no disponibles (carga o precarga fallida).
    Unavailable,
    /// Saltos automáticos de Spirc a la siguiente tras una no disponible: la cascada.
    AutoSkips,
    /// Cortes de la red a media canción: la canción se queda en pausa en su segundo y sigue al
    /// volver la red, en vez de saltar a la siguiente.
    Stalls,
}

impl Counter {
    pub const ALL: [Counter; 11] = [
        Counter::Plays,
        Counter::PlaysAudible,
        Counter::PlaysFailed,
        Counter::PlaysAbandoned,
        Counter::KeyErrors,
        Counter::KeyTimeouts,
        Counter::RateLimited,
        Counter::Http429,
        Counter::Unavailable,
        Counter::AutoSkips,
        Counter::Stalls,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Counter::Plays => "plays",
            Counter::PlaysAudible => "audible",
            Counter::PlaysFailed => "failed",
            Counter::PlaysAbandoned => "abandoned",
            Counter::KeyErrors => "key_errors",
            Counter::KeyTimeouts => "key_timeouts",
            Counter::RateLimited => "rate_limited",
            Counter::Http429 => "http_429",
            Counter::Unavailable => "unavailable",
            Counter::AutoSkips => "auto_skips",
            Counter::Stalls => "stalls",
        }
    }
}

static COUNTERS: [AtomicU64; Counter::ALL.len()] =
    [const { AtomicU64::new(0) }; Counter::ALL.len()];

pub fn count(c: Counter) {
    COUNTERS[c as usize].fetch_add(1, Ordering::Relaxed);
}

/// (nombre, valor) de cada contador, en el orden de `Counter::ALL`.
pub fn counters() -> Vec<(&'static str, u64)> {
    Counter::ALL
        .iter()
        .map(|&c| (c.name(), COUNTERS[c as usize].load(Ordering::Relaxed)))
        .collect()
}

static RECORDER: Mutex<Recorder> = Mutex::new(Recorder::new());
/// Número de la medida pendiente (0 = ninguna): la comprobación rápida de cada marca, sin cerrojo.
static PENDING: AtomicU64 = AtomicU64::new(0);
/// Medida que espera a la primera escritura en la salida (0 = ninguna).
static AUDIBLE: AtomicU64 = AtomicU64::new(0);
/// Actividad de la reproducción: sube con cada marca de cualquier capa (haya medida o no) y con
/// cada lectura del fichero de audio. La interfaz la mira mientras una canción carga: si no se ha
/// movido en un rato, la carga está atascada y no solo lenta, y entonces sí merece la pena
/// reintentarla (reintentar una carga que avanza tira lo hecho y pide otra clave).
static ACTIVITY: AtomicU64 = AtomicU64::new(0);

/// Algo avanzó en la carga o la reproducción (ver `ACTIVITY`).
pub fn note_activity() {
    ACTIVITY.fetch_add(1, Ordering::Relaxed);
}

/// Contador de actividad (ver `ACTIVITY`): solo importa si cambió desde la última vez.
pub fn activity() -> u64 {
    ACTIVITY.load(Ordering::Relaxed)
}

thread_local! {
    /// Medida de la carga que hace este hilo (el reproductor crea uno por carga); 0 = ninguna.
    /// Así la red (almacenamiento, clave) marca su fase sin que haya que pasarle el número.
    static THREAD_SEQ: Cell<u64> = const { Cell::new(0) };
    /// La carga de este hilo es por adelantado (`bind_thread_speculative`).
    static THREAD_SPECULATIVE: Cell<bool> = const { Cell::new(false) };
}

fn with<R>(f: impl FnOnce(&mut Recorder, Instant) -> R) -> R {
    // Un pánico con el cerrojo tomado no debe dejar la instrumentación (ni a quien marca) colgada.
    let mut r = RECORDER.lock().unwrap_or_else(|e| e.into_inner());
    let out = f(&mut r, Instant::now());
    PENDING.store(r.pending_seq(), Ordering::Relaxed);
    out
}

fn log_phase(seq: u64, p: &Phase) {
    log::debug!(
        "[ttfs] #{seq} {} {} ms (+{}){}",
        p.name,
        p.at_ms,
        p.ms,
        p.info.as_deref().map(|i| format!(" {i}")).unwrap_or_default()
    );
}

/// Abre una medida para una orden de la interfaz y devuelve su número.
pub fn begin(kind: &'static str) -> u64 {
    let (seq, abandoned) = with(|r, now| r.begin(kind, now));
    AUDIBLE.store(0, Ordering::Relaxed);
    if is_play(kind) {
        count(Counter::Plays);
    }
    if abandoned.is_some_and(is_play) {
        count(Counter::PlaysAbandoned);
    }
    log::debug!("[ttfs] #{seq} {kind}: orden");
    seq
}

/// Número de la medida pendiente; 0 si no hay ninguna.
pub fn pending() -> u64 {
    PENDING.load(Ordering::Relaxed)
}

pub fn is_pending(seq: u64) -> bool {
    seq != 0 && PENDING.load(Ordering::Relaxed) == seq
}

/// La medida pendiente si es de uno de estos tipos; 0 si no.
pub fn pending_of(kinds: &[&str]) -> u64 {
    if pending() == 0 {
        return 0;
    }
    with(|r, _| r.pending_of(kinds))
}

/// Marca una fase de la medida pendiente, sea cual sea (Spirc, que atiende las órdenes de una en
/// una).
pub fn mark(name: &'static str, info: Option<String>) {
    mark_seq(pending(), name, info);
}

/// Marca una fase de la medida `seq`, si sigue pendiente.
pub fn mark_seq(seq: u64, name: &'static str, info: Option<String>) {
    // Cuenta como avance aunque no haya medida: una carga sin orden medida también progresa.
    note_activity();
    if !is_pending(seq) {
        return;
    }
    if let Some(p) = with(|r, now| r.mark(seq, name, info, now)) {
        log_phase(seq, &p);
    }
}

/// Ver `Recorder::claim`. `track` solo se evalúa si hay una medida pendiente.
pub fn claim(track: impl FnOnce() -> String) -> u64 {
    if pending() == 0 {
        return 0;
    }
    let seq = with(|r, now| r.claim(track, now));
    if seq != 0 {
        log::debug!("[ttfs] #{seq} el reproductor carga la canción de la orden");
    }
    seq
}

/// Ata el hilo actual (el de una carga) a la medida `seq`.
pub fn bind_thread(seq: u64) {
    THREAD_SEQ.with(|s| s.set(seq));
    THREAD_SPECULATIVE.with(|s| s.set(false));
}

/// Como `bind_thread`, para una carga por adelantado (la canción pulsada, mientras Spirc resuelve
/// el contexto): sus fases cuentan para la medida, pero su fallo no la da por fallida. Quizá
/// nadie llegue a pedirla (Spirc empezó por otra), y si se pide, el reproductor anota el fallo de
/// esa carga.
pub fn bind_thread_speculative(seq: u64) {
    THREAD_SEQ.with(|s| s.set(seq));
    THREAD_SPECULATIVE.with(|s| s.set(true));
}

/// Medida a la que está atado este hilo (0 = ninguna).
pub fn thread_seq() -> u64 {
    THREAD_SEQ.with(|s| s.get())
}

/// Marca una fase de la medida a la que está atado este hilo.
pub fn mark_thread(name: &'static str, info: Option<String>) {
    mark_seq(thread_seq(), name, info);
}

/// La carga de `track` que hace este hilo falló. Ver `Recorder::load_failed`. Una carga por
/// adelantado solo deja el último fallo (ver `bind_thread_speculative`).
pub fn load_failed(track: String, what: String) {
    let seq = if THREAD_SPECULATIVE.with(|s| s.get()) {
        0
    } else {
        thread_seq()
    };
    if let Some(kind) = with(|r, now| r.load_failed(seq, track, what, now)) {
        if is_play(kind) {
            count(Counter::PlaysFailed);
        }
        log::debug!("[ttfs] #{seq} {kind}: la carga falló");
    }
}

/// La carga de la medida `seq` falló sin más detalle (si el hilo de carga ya lo contó, nada).
pub fn fail(seq: u64, what: &str) {
    if !is_pending(seq) {
        return;
    }
    if let Some(kind) = with(|r, now| r.fail(seq, what.to_string(), now)) {
        if is_play(kind) {
            count(Counter::PlaysFailed);
        }
        log::debug!("[ttfs] #{seq} {kind}: {what}");
    }
}

/// La medida `seq` se cerrará con la próxima escritura en la salida (`on_output`).
pub fn arm_audible(seq: u64) {
    if is_pending(seq) {
        AUDIBLE.store(seq, Ordering::Relaxed);
    }
}

/// La salida acaba de recibir audio. Lo llama el sink en cada escritura: sin medida esperando es
/// una lectura atómica.
pub fn on_output() {
    let seq = AUDIBLE.load(Ordering::Relaxed);
    if seq != 0
        && AUDIBLE
            .compare_exchange(seq, 0, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        finish(seq, "audible", None);
    }
}

/// Cierra la medida `seq` con su última marca.
pub fn finish(seq: u64, name: &'static str, info: Option<String>) {
    if !is_pending(seq) {
        return;
    }
    let done = with(|r, now| {
        let kind = r.finish(seq, name, info, now)?;
        Some((kind, r.snapshot(now)?))
    });
    if let Some((kind, b)) = done {
        if is_play(kind) && name == "audible" {
            count(Counter::PlaysAudible);
        }
        log::debug!(
            "[ttfs] #{seq} {kind}: {} ms — {}",
            b.total_ms.unwrap_or_default(),
            summary(&b)
        );
    }
}

/// La última medida (pendiente o cerrada).
pub fn snapshot() -> Option<Breakdown> {
    with(|r, now| r.snapshot(now))
}

/// El último fallo de carga.
pub fn last_error() -> Option<LastError> {
    with(|r, _| r.last_error().cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    fn names(b: &Breakdown) -> Vec<&'static str> {
        b.phases.iter().map(|p| p.name).collect()
    }

    fn values() -> Vec<u64> {
        counters().iter().map(|c| c.1).collect()
    }

    /// Cuánto subió el contador `c` de `before` a `after`.
    fn diff(before: &[u64], after: &[u64], c: Counter) -> u64 {
        after[c as usize] - before[c as usize]
    }

    /// Una reproducción completa: cada fase guarda su momento desde la orden y lo que tardó desde
    /// la anterior, y la marca que se oye cierra la medida con el total.
    #[test]
    fn medida_completa_con_tiempos_por_fase() {
        let t0 = Instant::now();
        let mut r = Recorder::new();
        let (seq, abandoned) = r.begin("play", t0);
        assert_eq!(seq, 1);
        assert_eq!(abandoned, None);
        assert_eq!(r.pending_seq(), 1);
        r.mark(seq, "spirc:load", None, at(t0, 5));
        assert_eq!(r.claim(|| "spotify:track:a".into(), at(t0, 140)), seq);
        r.mark(seq, "metadata", Some("miss".into()), at(t0, 230));
        r.mark(seq, "playing", None, at(t0, 600));
        assert_eq!(r.finish(seq, "audible", None, at(t0, 612)), Some("play"));

        let b = r.snapshot(at(t0, 700)).unwrap();
        assert_eq!(b.outcome, Outcome::Done);
        assert_eq!(b.total_ms, Some(612));
        assert_eq!(b.age_ms, 700);
        assert_eq!(b.track.as_deref(), Some("spotify:track:a"));
        assert_eq!(
            names(&b),
            ["cmd", "spirc:load", "player:load", "metadata", "playing", "audible"]
        );
        let meta = &b.phases[3];
        assert_eq!((meta.at_ms, meta.ms), (230, 90));
        assert_eq!(meta.info.as_deref(), Some("miss"));
        assert_eq!((b.phases[5].at_ms, b.phases[5].ms), (612, 12));
        // Cerrada: ni marcas ni un segundo cierre la cambian.
        assert!(r.mark(seq, "tarde", None, at(t0, 800)).is_none());
        assert_eq!(r.finish(seq, "audible", None, at(t0, 900)), None);
        assert_eq!(r.pending_seq(), 0);
        assert_eq!(r.snapshot(at(t0, 900)).unwrap().total_ms, Some(612));
        assert!(summary(&b).starts_with("cmd 0 · spirc:load 5 (+5) · player:load 140 (+135)"));
    }

    /// Solo la primera carga tras la orden es suya: ni una segunda (el salto automático tras un
    /// fallo), ni una pausa, ni una carga que llega pasada la ventana.
    #[test]
    fn solo_la_primera_carga_es_de_la_orden() {
        let t0 = Instant::now();
        let mut r = Recorder::new();
        let (seq, _) = r.begin("next", t0);
        assert_eq!(r.claim(|| "a".into(), at(t0, 10)), seq);
        assert_eq!(r.claim(|| "b".into(), at(t0, 20)), 0);

        let (pausa, _) = r.begin("pause", at(t0, 30));
        assert_eq!(r.claim(|| "c".into(), at(t0, 40)), 0);
        assert_eq!(r.pending_of(&["pause"]), pausa);
        assert_eq!(r.pending_of(&["seek", "prev"]), 0);

        let (tarde, _) = r.begin("play", at(t0, 50));
        let pasada = at(t0, 50) + CLAIM_WINDOW;
        assert_eq!(r.claim(|| "d".into(), pasada), 0);
        assert_eq!(r.claim(|| "d".into(), pasada - Duration::from_millis(1)), tarde);
    }

    /// Una orden nueva abandona la pendiente (no es un fallo), y lo que llegue tarde para la
    /// abandonada no toca la nueva.
    #[test]
    fn otra_orden_abandona_la_pendiente() {
        let t0 = Instant::now();
        let mut r = Recorder::new();
        let (a, _) = r.begin("next", t0);
        let (b, abandoned) = r.begin("next", at(t0, 30));
        assert_eq!(abandoned, Some("next"));
        assert_ne!(a, b);
        assert!(r.mark(a, "metadata", None, at(t0, 40)).is_none());
        assert_eq!(r.finish(a, "audible", None, at(t0, 50)), None);
        assert_eq!(r.fail(a, "clave".into(), at(t0, 50)), None);
        let s = r.snapshot(at(t0, 60)).unwrap();
        assert_eq!((s.seq, s.outcome), (b, Outcome::Pending));
        assert_eq!(s.total_ms, None);
        assert_eq!(names(&s), ["cmd"]);
        // La abandonada ya no estaba pendiente: abrir otra no la cuenta otra vez.
        r.finish(b, "audible", None, at(t0, 70));
        assert_eq!(r.begin("play", at(t0, 80)).1, None);
    }

    /// Un fallo cierra la medida con el error como última fase y queda como último fallo; el
    /// fallo genérico que llega después (el del hilo del reproductor) no lo pisa.
    #[test]
    fn el_fallo_cierra_la_medida_y_queda_como_ultimo_error() {
        let t0 = Instant::now();
        let mut r = Recorder::new();
        let (seq, _) = r.begin("play", t0);
        r.claim(|| "spotify:track:x".into(), at(t0, 100));
        let track = "spotify:track:x".to_string();
        let kind = r.load_failed(seq, track, "clave: denegada".into(), at(t0, 400));
        assert_eq!(kind, Some("play"));
        assert_eq!(r.fail(seq, "la carga falló".into(), at(t0, 401)), None);

        let s = r.snapshot(at(t0, 500)).unwrap();
        assert_eq!(s.outcome, Outcome::Failed);
        assert_eq!(s.total_ms, Some(400));
        assert_eq!(s.error.as_deref(), Some("clave: denegada"));
        assert_eq!(s.phases.last().unwrap().name, "error");
        let e = r.last_error().unwrap();
        assert_eq!((e.seq, e.kind), (seq, "play"));
        assert_eq!(e.track, "spotify:track:x");
        assert_eq!(e.what, "clave: denegada");

        // Un fallo que no es de ninguna orden (precarga) no toca la medida, pero es el último.
        let otra = r.load_failed(0, "spotify:track:y".into(), "metadatos".into(), at(t0, 600));
        assert_eq!(otra, None);
        let e = r.last_error().unwrap();
        assert_eq!((e.seq, e.kind, e.track.as_str()), (0, "", "spotify:track:y"));
        assert_eq!(r.snapshot(at(t0, 700)).unwrap().error.as_deref(), Some("clave: denegada"));
    }

    /// El número 0 nunca es una medida: marcar o cerrar con él no hace nada.
    #[test]
    fn el_cero_no_es_ninguna_medida() {
        let t0 = Instant::now();
        let mut r = Recorder::new();
        assert!(r.snapshot(t0).is_none());
        assert!(r.mark(0, "x", None, t0).is_none());
        r.begin("seek", t0);
        assert!(r.mark(0, "x", None, t0).is_none());
        assert_eq!(r.finish(0, "audible", None, t0), None);
        assert_eq!(r.fail(0, "x".into(), t0), None);
        assert_eq!(names(&r.snapshot(t0).unwrap()), ["cmd"]);
    }

    /// Las fases tienen tope; pasado, la medida sigue y el total es el real.
    #[test]
    fn tope_de_fases() {
        let t0 = Instant::now();
        let mut r = Recorder::new();
        let (seq, _) = r.begin("play", t0);
        for i in 0..100 {
            r.mark(seq, "spirc:volume", None, at(t0, i));
        }
        r.finish(seq, "audible", None, at(t0, 250));
        let s = r.snapshot(at(t0, 250)).unwrap();
        assert_eq!(s.phases.len(), MAX_PHASES);
        assert_eq!(s.total_ms, Some(250));
    }

    /// Las funciones globales (las que usan Spirc, el reproductor y la salida). Una sola prueba
    /// las toca, para que las demás, en paralelo, no se pisen.
    #[test]
    fn api_global_hilo_atado_y_primera_escritura() {
        let before = values();
        let seq = begin("play");
        assert!(is_pending(seq));
        mark("spirc:load", None);
        assert_eq!(claim(|| "spotify:track:g".into()), seq);
        // El hilo de carga, atado a la medida, marca sin conocer el número; otro hilo sin atar no.
        std::thread::spawn(move || {
            bind_thread(seq);
            mark_thread("metadata", Some("miss".into()));
        })
        .join()
        .unwrap();
        std::thread::spawn(|| mark_thread("ajeno", None)).join().unwrap();
        // Sin armar, escribir en la salida no cierra nada (lo que aún sonaba de la anterior).
        on_output();
        assert!(is_pending(seq));
        arm_audible(seq);
        on_output();
        on_output();
        assert!(!is_pending(seq));
        let s = snapshot().unwrap();
        assert_eq!(s.outcome, Outcome::Done);
        assert_eq!(names(&s), ["cmd", "spirc:load", "player:load", "metadata", "audible"]);
        let after = values();
        assert_eq!(diff(&before, &after, Counter::Plays), 1);
        assert_eq!(diff(&before, &after, Counter::PlaysAudible), 1);

        // Una pausa no cuenta como reproducción y se cierra con la salida detenida.
        let p = begin("pause");
        assert_eq!(pending_of(&["pause"]), p);
        finish(p, "silent", None);
        assert_eq!(pending(), 0);
        let after2 = values();
        assert_eq!(diff(&after, &after2, Counter::Plays), 0);

        // Fallo desde el hilo de carga: cuenta como reproducción fallida.
        let f = begin("next");
        assert_eq!(claim(|| "spotify:track:h".into()), f);
        std::thread::spawn(move || {
            bind_thread(f);
            load_failed("spotify:track:h".into(), "clave: denegada".into());
        })
        .join()
        .unwrap();
        fail(f, "la carga falló");
        assert_eq!(diff(&after2, &values(), Counter::PlaysFailed), 1);
        assert_eq!(last_error().unwrap().what, "clave: denegada");
        assert_eq!(snapshot().unwrap().outcome, Outcome::Failed);

        // Carga por adelantado (la canción pulsada mientras Spirc resuelve el contexto): sus fases
        // cuentan, pero que falle antes de que la pida el reproductor no da la orden por fallida.
        let after3 = values();
        let w = begin("play");
        std::thread::spawn(move || {
            bind_thread_speculative(w);
            mark_thread("metadata", Some("seed".into()));
            load_failed("spotify:track:w".into(), "no disponible".into());
        })
        .join()
        .unwrap();
        assert!(is_pending(w));
        assert_eq!(last_error().unwrap().what, "no disponible");
        assert_eq!(names(&snapshot().unwrap()), ["cmd", "metadata"]);
        assert_eq!(diff(&after3, &values(), Counter::PlaysFailed), 0);
        // Un hilo atado después de la forma normal ya no es por adelantado.
        std::thread::spawn(move || {
            bind_thread_speculative(w);
            bind_thread(w);
            load_failed("spotify:track:w".into(), "clave: denegada".into());
        })
        .join()
        .unwrap();
        assert!(!is_pending(w));
        assert_eq!(diff(&after3, &values(), Counter::PlaysFailed), 1);
    }

    #[test]
    fn la_actividad_sube_con_cada_marca_aunque_no_haya_medida() {
        // Global y compartida con las demás pruebas: solo se puede afirmar que sube.
        let a = activity();
        mark_seq(0, "nada", None);
        let b = activity();
        assert!(b > a, "una marca sin medida también es avance");
        note_activity();
        assert!(activity() > b);
    }

    #[test]
    fn nombres_de_los_contadores() {
        let n: Vec<&str> = Counter::ALL.iter().map(|c| c.name()).collect();
        assert_eq!(n.len(), Counter::ALL.len());
        let mut sorted = n.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), n.len(), "nombres repetidos");
        for (i, c) in Counter::ALL.iter().enumerate() {
            assert_eq!(*c as usize, i, "ALL debe seguir el orden de la enumeración");
        }
    }
}
