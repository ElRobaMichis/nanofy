//! Nunca atascado cargando: el vigilante por etapas de una canción que no termina de cargar y los
//! reintentos de una canción cortada por la red a media reproducción.
//!
//! Solo decide (qué toca y cuándo); quien lo usa (`App`) envía las órdenes y enseña los avisos.
//! Así se prueba con un reloj inventado, sin reproductor ni red.

use std::time::{Duration, Instant};

/// Cargando más que esto: la barra dice que la conexión va lenta.
pub const SLOW_AFTER: Duration = Duration::from_millis(2500);
/// Cargando más que esto y sin avanzar: se comprueba Spirc y se vuelve a pedir la carga.
pub const RETRY_AFTER: Duration = Duration::from_secs(8);
/// Cargando más que esto y sin avanzar: se reconecta con Spotify.
pub const RECONNECT_AFTER: Duration = Duration::from_secs(15);
/// Cargando más que esto: se deja de esperar y se dice, con [Reintentar]. Nunca más que esto en
/// «cargando» sin un aviso.
pub const GIVE_UP_AFTER: Duration = Duration::from_secs(30);
/// Sin ninguna señal de avance (marcas de la carga, lecturas del fichero) en este tiempo, la
/// carga está atascada y no solo lenta: reintentar una que avanza tiraría lo hecho y pediría otra
/// clave de audio, justo lo que Spotify frena.
pub const PROGRESS_WINDOW: Duration = Duration::from_secs(2);

/// Lo que toca hacer con una carga que dura.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Nada nuevo.
    Wait,
    /// Avisar de que va lenta (solo en la barra).
    Slow,
    /// Comprobar que Spirc atiende y volver a pedir la carga (`Cmd::RetryLoad`).
    Retry,
    /// Reconectar (`Cmd::Stalled`).
    Reconnect,
    /// Dejar de esperar: parada y aviso con [Reintentar].
    GiveUp,
}

/// Una canción cargando aquí, desde que la interfaz la vio en «cargando» hasta que suena, se
/// pausa, falla o se pide otra cosa (entonces se crea otro). Cada etapa sale una sola vez.
#[derive(Clone, Debug)]
pub struct LoadWatchdog {
    since: Instant,
    /// Último valor visto del contador de actividad (`ttfs::activity`) y cuándo cambió.
    activity: u64,
    progress_at: Instant,
    slow: bool,
    retried: bool,
    reconnected: bool,
    gave_up: bool,
}

impl LoadWatchdog {
    pub fn new(now: Instant, activity: u64) -> Self {
        Self {
            since: now,
            activity,
            progress_at: now,
            slow: false,
            retried: false,
            reconnected: false,
            gave_up: false,
        }
    }

    /// Cuánto lleva cargando.
    pub fn age(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.since)
    }

    /// Ya pasó de `SLOW_AFTER` (la barra lo dice).
    pub fn is_slow(&self) -> bool {
        self.slow
    }

    /// Última etapa alcanzada, para el modo de control.
    pub fn stage_name(&self) -> &'static str {
        if self.gave_up {
            "gave_up"
        } else if self.reconnected {
            "reconnect"
        } else if self.retried {
            "retry"
        } else if self.slow {
            "slow"
        } else {
            "loading"
        }
    }

    /// Se llama a menudo mientras carga, con el contador de actividad de ahora. Devuelve la etapa
    /// que toca, una vez cada una. Reintentar y reconectar solo si la carga no avanza: una que
    /// avanza despacio llega antes sola. Rendirse, siempre a los 30 s.
    pub fn tick(&mut self, now: Instant, activity: u64) -> Stage {
        if activity != self.activity {
            self.activity = activity;
            self.progress_at = now;
        }
        let age = self.age(now);
        let stuck = now.saturating_duration_since(self.progress_at) >= PROGRESS_WINDOW;
        if !self.gave_up && age >= GIVE_UP_AFTER {
            self.gave_up = true;
            return Stage::GiveUp;
        }
        if !self.reconnected && age >= RECONNECT_AFTER && stuck {
            self.reconnected = true;
            // Reconectar ya repite la carga: el reintento que no llegó a salir sobra.
            self.retried = true;
            return Stage::Reconnect;
        }
        if !self.retried && age >= RETRY_AFTER && stuck {
            self.retried = true;
            return Stage::Retry;
        }
        if !self.slow && age >= SLOW_AFTER {
            self.slow = true;
            return Stage::Slow;
        }
        Stage::Wait
    }
}

/// ¿La carga atascada espera a que Spotify resuelva el contexto? Por las fases de su medida
/// (`ttfs`): Spirc empezó la carga («spirc:load») y nunca tuvo el contexto («spirc:context»).
/// Entonces repetirla igual volvería a esperar lo mismo; como lista de pistas sueltas no hace falta
/// esa petición.
pub fn context_suspect<'a>(phases: impl IntoIterator<Item = &'a str>) -> bool {
    let (mut load, mut context) = (false, false);
    for p in phases {
        load |= p == "spirc:load";
        context |= p == "spirc:context";
    }
    load && !context
}

/// Reintentos automáticos de una canción cortada por la red: el primero a los 3 s, luego a los 6
/// y a los 12, y después cada 30 s. Mientras el fichero sigue abierto, cada intento solo comprueba
/// si llegan datos (una petición al servidor de audio, sin claves): sin red no cuesta nada, y con
/// red vuelve a sonar enseguida. Tras recargarla, cada intento es una carga en su segundo (la
/// clave ya está en memoria).
pub const STALL_RETRY_DELAYS: [Duration; 4] =
    [Duration::from_secs(3), Duration::from_secs(6), Duration::from_secs(12), Duration::from_secs(30)];
/// Intentos fallidos seguidos tras los que se vuelve a cargar la canción entera en su segundo: el
/// enlace del servidor de audio caduca (tras una pausa larga a medio descargar) y con el viejo los
/// datos no volverían nunca.
pub const STALL_RELOAD_AFTER: u32 = 2;
/// Vuelve a sonar y se corta otra vez antes de esto: «enseguida» (cuenta como intento fallido).
/// Sonando más que esto, el corte se da por superado.
pub const STALL_QUICK: Duration = Duration::from_secs(20);
/// Pasado esto sin volver a sonar, se deja de reintentar solo: queda el aviso con [Reintentar].
pub const STALL_GIVE_UP: Duration = Duration::from_secs(10 * 60);

/// Espera hasta el reintento tras `failures` intentos fallidos (0 = el primer corte).
pub fn stall_retry_delay(failures: u32) -> Duration {
    STALL_RETRY_DELAYS[(failures as usize).min(STALL_RETRY_DELAYS.len() - 1)]
}

/// Lo que toca tras un corte o un intento fallido.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StallStep {
    /// Intentar reanudar pasado esto.
    Retry(Duration),
    /// Volver a cargar la canción entera en su segundo (enlace nuevo), ya.
    Reload,
    /// Dejar de intentarlo solo.
    GiveUp,
}

/// Una canción cortada por la red a media reproducción, en pausa en su segundo, y lo intentado
/// para reanudarla.
#[derive(Clone, Debug)]
pub struct StallRecovery {
    /// La canción, como la enseña la barra (`TrackChanged`).
    pub uri: String,
    /// La misma canción con el id que se pidió, si Spotify la sirve con otro (relinking: otra
    /// edición equivalente para el país). El reproductor avisa de los cortes y de los fallos de
    /// carga con el id pedido, y la barra enseña el servido.
    pub alias: Option<String>,
    /// Lo último que se oyó: desde ahí sigue.
    pub position_ms: u32,
    /// Intentos fallidos seguidos.
    pub failures: u32,
    /// Ya se recargó la canción entera en este corte.
    pub reloaded: bool,
    /// Próximo intento automático.
    pub at: Option<Instant>,
    /// Volvió a sonar (cuándo): si dura `STALL_QUICK`, el corte está superado.
    pub resumed_at: Option<Instant>,
    since: Instant,
}

impl StallRecovery {
    /// Primer corte de `uri`: se reintenta a los 3 s.
    pub fn new(uri: String, position_ms: u32, now: Instant) -> Self {
        let mut s = Self {
            uri,
            alias: None,
            position_ms,
            failures: 0,
            reloaded: false,
            at: None,
            resumed_at: None,
            since: now,
        };
        s.at = Some(now + stall_retry_delay(0));
        s
    }

    /// Llegó otro corte de esta canción en `position_ms`: un intento de reanudar que no encontró
    /// datos, o que volvió a sonar y se cortó enseguida. Devuelve lo que toca.
    pub fn on_stalled(&mut self, position_ms: u32, now: Instant) -> StallStep {
        self.position_ms = position_ms;
        if let Some(t) = self.resumed_at.take() {
            if now.saturating_duration_since(t) >= STALL_QUICK {
                // Sonó un buen rato: es un corte nuevo, se empieza de cero.
                let alias = self.alias.take();
                *self = Self::new(std::mem::take(&mut self.uri), position_ms, now);
                self.alias = alias;
                return StallStep::Retry(stall_retry_delay(0));
            }
        }
        self.on_failed_attempt(now)
    }

    /// Un intento de reanudar falló (sin datos, o la recarga tampoco pudo con la red).
    pub fn on_failed_attempt(&mut self, now: Instant) -> StallStep {
        self.failures = self.failures.saturating_add(1);
        self.next_step(now)
    }

    fn next_step(&mut self, now: Instant) -> StallStep {
        self.at = None;
        if now.saturating_duration_since(self.since) >= STALL_GIVE_UP {
            return StallStep::GiveUp;
        }
        if !self.reloaded && self.failures >= STALL_RELOAD_AFTER {
            self.reloaded = true;
            return StallStep::Reload;
        }
        let delay = stall_retry_delay(self.failures);
        self.at = Some(now + delay);
        StallStep::Retry(delay)
    }

    /// ¿Es esta canción, por el id que enseña la barra o por el que se pidió?
    pub fn is(&self, uri: &str) -> bool {
        self.uri == uri || self.alias.as_deref() == Some(uri)
    }

    /// Volvió a sonar.
    pub fn on_playing(&mut self, now: Instant) {
        self.at = None;
        self.resumed_at = Some(now);
    }

    /// Ya suena desde hace `STALL_QUICK`: el corte está superado y se puede olvidar.
    pub fn settled(&self, now: Instant) -> bool {
        self.resumed_at.is_some_and(|t| now.saturating_duration_since(t) >= STALL_QUICK)
    }

    /// Lo que falta para el próximo intento (cero si ya toca); `None` si no hay ninguno previsto.
    pub fn due_in(&self, now: Instant) -> Option<Duration> {
        self.at.map(|at| at.saturating_duration_since(now))
    }
}

/// «m:ss» de una posición, para los avisos.
pub fn mmss(ms: u32) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Reanudaciones automáticas al volver la salida de audio permitidas en `OUTPUT_RESUME_WINDOW`.
pub const OUTPUT_RESUME_MAX: u32 = 3;
pub const OUTPUT_RESUME_WINDOW: Duration = Duration::from_secs(60);

/// Cuántas veces se reanudó sola la música al volver un dispositivo de salida. El vigilante de la
/// salida solo mira si el sistema tiene un dispositivo predeterminado, no si se deja abrir: uno en
/// uso exclusivo por otra app o una pantalla HDMI dormida está ahí pero falla al sonar. Sin límite
/// sería un bucle sin fin (reproducir, fallar, pausa, «vuelve la salida»… cada 1,5 s), y cada
/// vuelta avisa a Spotify del cambio de estado. Pasado el límite queda el aviso y basta con darle
/// a reproducir.
#[derive(Clone, Debug, Default)]
pub struct OutputResumes {
    count: u32,
    since: Option<Instant>,
}

impl OutputResumes {
    /// ¿Se puede reanudar sola ahora? Si sí, cuenta.
    pub fn allow(&mut self, now: Instant) -> bool {
        if self.since.is_none_or(|t| now.saturating_duration_since(t) >= OUTPUT_RESUME_WINDOW) {
            self.count = 0;
            self.since = Some(now);
        }
        if self.count >= OUTPUT_RESUME_MAX {
            return false;
        }
        self.count += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_salida_que_vuelve_y_no_suena_no_hace_un_bucle() {
        let t0 = Instant::now();
        let mut r = OutputResumes::default();
        // Cada 1,5 s «vuelve» y vuelve a fallar: solo las tres primeras se reanudan solas.
        let allowed: Vec<bool> = (0..6).map(|i| r.allow(t0 + ms(1_500 * i))).collect();
        assert_eq!(allowed, vec![true, true, true, false, false, false]);
        // Pasado el minuto se vuelve a permitir (otra desconexión, otro día).
        assert!(r.allow(t0 + OUTPUT_RESUME_WINDOW + ms(1)));
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Recorre el vigilante cada 250 ms hasta `until`, con la actividad que diga `activity` en
    /// cada instante, y devuelve en qué momento salió cada etapa.
    fn run(until: u64, activity: impl Fn(u64) -> u64) -> Vec<(u64, Stage)> {
        let t0 = Instant::now();
        let mut w = LoadWatchdog::new(t0, activity(0));
        let mut out = Vec::new();
        let mut t = 0;
        while t <= until {
            let stage = w.tick(t0 + ms(t), activity(t));
            if stage != Stage::Wait {
                out.push((t, stage));
            }
            t += 250;
        }
        out
    }

    #[test]
    fn carga_atascada_pasa_por_todas_las_etapas_una_vez() {
        let stages = run(40_000, |_| 7);
        assert_eq!(
            stages,
            vec![(2_500, Stage::Slow), (8_000, Stage::Retry), (15_000, Stage::Reconnect), (30_000, Stage::GiveUp)]
        );
    }

    #[test]
    fn una_carga_que_avanza_no_se_reintenta_pero_se_rinde_a_los_30_s() {
        // Avanza cada segundo: nunca 2 s quieta.
        let stages = run(40_000, |t| t / 1000);
        assert_eq!(stages, vec![(2_500, Stage::Slow), (30_000, Stage::GiveUp)]);
    }

    #[test]
    fn el_reintento_espera_a_que_la_carga_se_pare() {
        // Avanza hasta los 9 s y se para: el reintento sale 2 s después, no a los 8 s.
        let stages = run(12_000, |t| t.min(9_000) / 100);
        assert_eq!(stages, vec![(2_500, Stage::Slow), (11_000, Stage::Retry)]);
    }

    #[test]
    fn reconectar_sustituye_al_reintento_que_no_salio() {
        // Avanza hasta los 14 s: a los 16 s ya no avanza y toca reconectar, sin reintento antes.
        let stages = run(20_000, |t| t.min(14_000) / 100);
        assert_eq!(stages, vec![(2_500, Stage::Slow), (16_000, Stage::Reconnect)]);
        let t0 = Instant::now();
        let mut w = LoadWatchdog::new(t0, 0);
        assert_eq!(w.stage_name(), "loading");
        w.tick(t0 + ms(2_600), 0);
        assert!(w.is_slow());
        assert_eq!(w.stage_name(), "slow");
    }

    #[test]
    fn rapida_no_dice_nada() {
        let t0 = Instant::now();
        let mut w = LoadWatchdog::new(t0, 0);
        assert_eq!(w.tick(t0 + ms(2_400), 0), Stage::Wait);
        assert!(!w.is_slow());
        assert_eq!(w.age(t0 + ms(2_400)), ms(2_400));
    }

    #[test]
    fn reintentos_de_un_corte_de_red() {
        assert_eq!(stall_retry_delay(0), Duration::from_secs(3));
        assert_eq!(stall_retry_delay(1), Duration::from_secs(6));
        assert_eq!(stall_retry_delay(2), Duration::from_secs(12));
        assert_eq!(stall_retry_delay(3), Duration::from_secs(30));
        assert_eq!(stall_retry_delay(50), Duration::from_secs(30));

        let t0 = Instant::now();
        let mut s = StallRecovery::new("spotify:track:a".into(), 61_000, t0);
        assert_eq!(s.due_in(t0), Some(Duration::from_secs(3)));
        // Primer intento sin datos: 6 s.
        assert_eq!(s.on_stalled(61_000, t0 + ms(3_000)), StallStep::Retry(Duration::from_secs(6)));
        // Segundo: dos seguidos sin datos, el enlace pudo caducar: se recarga entera (una vez).
        assert_eq!(s.on_stalled(61_000, t0 + ms(9_000)), StallStep::Reload);
        assert!(s.reloaded);
        assert_eq!(s.due_in(t0 + ms(9_000)), None);
        // La recarga tampoco pudo (sin red): 30 s y así hasta que vuelva.
        assert_eq!(s.on_failed_attempt(t0 + ms(10_000)), StallStep::Retry(Duration::from_secs(30)));
        assert_eq!(s.on_failed_attempt(t0 + ms(40_000)), StallStep::Retry(Duration::from_secs(30)));
        // Diez minutos sin volver: se deja de intentar solo.
        assert_eq!(s.on_failed_attempt(t0 + STALL_GIVE_UP), StallStep::GiveUp);
        assert_eq!(s.due_in(t0 + STALL_GIVE_UP), None);
    }

    #[test]
    fn vuelve_a_sonar_y_se_corta_enseguida_dos_veces() {
        let t0 = Instant::now();
        let mut s = StallRecovery::new("spotify:track:a".into(), 10_000, t0);
        // Suena tras el primer intento y se corta a los 5 s: cuenta como fallo.
        s.on_playing(t0 + ms(3_000));
        assert!(!s.settled(t0 + ms(5_000)));
        assert_eq!(s.on_stalled(15_000, t0 + ms(8_000)), StallStep::Retry(Duration::from_secs(6)));
        assert_eq!(s.position_ms, 15_000);
        // Otra vez: dos cortes rápidos, se recarga entera.
        s.on_playing(t0 + ms(14_000));
        assert_eq!(s.on_stalled(16_000, t0 + ms(16_000)), StallStep::Reload);
    }

    #[test]
    fn corte_de_una_cancion_relinkada() {
        // La barra enseña la edición que sirve Spotify; el reproductor avisa con el id pedido.
        let t0 = Instant::now();
        let mut s = StallRecovery::new("spotify:track:servida".into(), 84_000, t0);
        s.alias = Some("spotify:track:pedida".into());
        assert!(s.is("spotify:track:servida"));
        assert!(s.is("spotify:track:pedida"));
        assert!(!s.is("spotify:track:otra"));
        // Un corte nuevo tras sonar un buen rato conserva el alias.
        s.on_playing(t0 + ms(3_000));
        assert_eq!(s.on_stalled(90_000, t0 + ms(3_000) + STALL_QUICK), StallStep::Retry(Duration::from_secs(3)));
        assert!(s.is("spotify:track:pedida"));
    }

    #[test]
    fn un_corte_tras_sonar_un_buen_rato_empieza_de_cero() {
        let t0 = Instant::now();
        let mut s = StallRecovery::new("spotify:track:a".into(), 10_000, t0);
        s.on_stalled(10_000, t0 + ms(3_000));
        s.on_playing(t0 + ms(9_000));
        assert!(s.settled(t0 + ms(9_000) + STALL_QUICK));
        let later = t0 + ms(9_000) + STALL_QUICK + ms(1);
        assert_eq!(s.on_stalled(50_000, later), StallStep::Retry(Duration::from_secs(3)));
        assert_eq!(s.failures, 0);
        assert!(!s.reloaded);
        assert_eq!(s.uri, "spotify:track:a");
        assert_eq!(s.position_ms, 50_000);
    }

    #[test]
    fn contexto_que_no_llega() {
        assert!(context_suspect(["cmd", "spirc:activate", "spirc:load"]));
        // Ya resuelto: lo atascado es otra cosa (metadatos, clave, CDN).
        assert!(!context_suspect(["cmd", "spirc:load", "spirc:context", "player:load"]));
        // Ni siquiera llegó a Spirc (o no era una carga de contexto).
        assert!(!context_suspect(["cmd"]));
        assert!(!context_suspect(["cmd", "spirc:next", "player:load"]));
    }

    #[test]
    fn minutos_y_segundos() {
        assert_eq!(mmss(0), "0:00");
        assert_eq!(mmss(61_999), "1:01");
        assert_eq!(mmss(605_000), "10:05");
    }
}
