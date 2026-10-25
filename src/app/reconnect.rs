//! Avisos de una reconexión con Spotify. Casi todas duran uno o dos segundos y la música sigue
//! sola en su punto: los tres avisos de antes («se perdió la conexión», «conectando», «restablecida»)
//! alarmaban más que el propio corte. Ahora se calla si es breve y, si tarda, hay un solo aviso que
//! se actualiza en su sitio.
//!
//! Solo decide (qué texto toca y cuándo); `App` lo lleva al aviso de estado. Así se prueba con un
//! reloj inventado.

use std::time::{Duration, Instant};

use crate::backend::ReconnectReason;
use crate::i18n::t;

/// Una reconexión que acaba antes de esto no se cuenta: el corte ya se oyó y la música vuelve sola.
pub const NOTICE_AFTER: Duration = Duration::from_secs(4);
/// El que sale si la reconexión tarda (y mientras se intenta).
pub const RECONNECTING: &str = crate::backend::RECONNECTING;
/// Al volver, solo si antes se dijo que se reconectaba.
pub const RESTORED: &str = tk!("Conexión con Spotify restablecida");

/// El aviso de estado de la app: texto, cuándo se puso y si es de error.
pub type Status = Option<(String, Instant, bool)>;

/// Una reconexión en marcha, desde `Event::Reconnecting` hasta que vuelve la sesión.
#[derive(Clone, Debug)]
pub struct Reconnect {
    pub reason: ReconnectReason,
    since: Instant,
    /// Próximo intento tras uno fallido (`Event::ReconnectFailed`).
    retry_at: Option<Instant>,
    /// Intentos fallidos.
    pub failures: u32,
    /// Se pidió algo que necesita la sesión: el aviso sale ya, sin esperar `NOTICE_AFTER`.
    asked: bool,
    /// Texto que puso este aviso, para reconocerlo en el de estado y actualizarlo en su sitio.
    posted: Option<String>,
}

impl Reconnect {
    pub fn new(reason: ReconnectReason, now: Instant) -> Self {
        Self {
            reason,
            since: now,
            retry_at: None,
            failures: 0,
            asked: false,
            posted: None,
        }
    }

    /// Cuánto lleva.
    pub fn age(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.since)
    }

    /// Un intento falló; el siguiente, en `retry_in`.
    pub fn on_failed(&mut self, retry_in: Duration, now: Instant) {
        self.failures = self.failures.saturating_add(1);
        self.retry_at = Some(now + retry_in);
    }

    /// ¿Se llegó a enseñar? Entonces, al volver, se dice que se restableció.
    pub fn shown(&self) -> bool {
        self.posted.is_some()
    }

    /// Lo que falta para el próximo intento (`None` si no hay uno previsto o ya se está intentando).
    pub fn retry_in(&self, now: Instant) -> Option<Duration> {
        self.retry_at.map(|at| at.saturating_duration_since(now)).filter(|d| !d.is_zero())
    }

    /// El texto que toca ahora (`None`: aún nada). Tras un intento fallido cuenta los segundos
    /// hasta el siguiente; mientras se intenta, «Reconectando…».
    pub fn text(&self, now: Instant) -> Option<String> {
        if !self.asked && self.age(now) < NOTICE_AFTER {
            return None;
        }
        Some(match self.retry_in(now) {
            Some(d) => tr!("Sin conexión con Spotify; reintentando en {} s…", secs_up(d)),
            None => t(RECONNECTING).to_string(),
        })
    }

    /// Lleva el aviso al de estado (`status`), si toca. Es uno solo: se actualiza en su sitio y,
    /// mientras la reconexión dura, no caduca (se renueva su hora). Si encima hay otro más
    /// reciente (algo que hizo el usuario), se deja leer; este vuelve cuando aquel caduque.
    pub fn show(&mut self, now: Instant, status: &mut Status) {
        let Some(text) = self.text(now) else { return };
        if status.is_none() || self.owns(status) {
            *status = Some((text.clone(), now, false));
            self.posted = Some(text);
        }
    }

    /// Una orden no se pudo ejecutar porque se está reconectando: se dice ya, encima de lo que
    /// hubiera (es lo más reciente).
    pub fn ask(&mut self, now: Instant, status: &mut Status) {
        self.asked = true;
        *status = None;
        self.show(now, status);
    }

    /// La sesión volvió (o la reconexión terminó de otro modo, `restored = false`). Si se llegó a
    /// decir que se reconectaba, se dice que se restableció; si no, el corte pasó sin aviso. El
    /// aviso propio que quede se quita.
    pub fn finish(self, now: Instant, restored: bool, status: &mut Status) {
        if restored && self.shown() {
            *status = Some((t(RESTORED).to_string(), now, false));
        } else if self.owns(status) {
            *status = None;
        }
    }

    /// Cuándo volver a mirar: al cumplirse `NOTICE_AFTER` y, después, cada vez que cambia el
    /// segundo de la cuenta atrás (o cada segundo, para que el aviso no caduque).
    pub fn next_check(&self, now: Instant) -> Duration {
        let age = self.age(now);
        if !self.asked && age < NOTICE_AFTER {
            return NOTICE_AFTER - age;
        }
        match self.retry_in(now) {
            Some(d) => match d.as_millis() % 1000 {
                0 => Duration::from_secs(1),
                ms => Duration::from_millis(ms as u64),
            },
            None => Duration::from_secs(1),
        }
    }

    /// ¿El aviso de estado es el que puso esta reconexión?
    fn owns(&self, status: &Status) -> bool {
        matches!((status, &self.posted), (Some((t, _, false)), Some(p)) if t == p)
    }
}

/// Segundos que faltan, redondeando hacia arriba (con 1,2 s quedan «2 s»; nunca «0 s»).
fn secs_up(d: Duration) -> u64 {
    (d.as_millis() as u64).div_ceil(1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn una_reconexion_breve_no_dice_nada() {
        let t0 = Instant::now();
        let mut r = Reconnect::new(ReconnectReason::Lost, t0);
        let mut status: Status = None;
        r.show(t0 + ms(1_800), &mut status);
        assert_eq!(status, None);
        assert_eq!(r.next_check(t0 + ms(1_800)), ms(2_200));
        r.finish(t0 + ms(1_900), true, &mut status);
        assert_eq!(status, None, "sin «restablecida» si no se dijo nada antes");
    }

    #[test]
    fn si_tarda_se_dice_una_vez_y_al_volver_se_restablece() {
        let t0 = Instant::now();
        let mut r = Reconnect::new(ReconnectReason::Lost, t0);
        let mut status: Status = None;
        r.show(t0 + ms(3_999), &mut status);
        assert_eq!(status, None);
        let t4 = t0 + NOTICE_AFTER;
        r.show(t4, &mut status);
        assert_eq!(status, Some((RECONNECTING.to_string(), t4, false)));
        assert!(r.shown());
        // Mientras dura no caduca: se renueva su hora en el mismo sitio.
        let t12 = t0 + ms(12_000);
        r.show(t12, &mut status);
        assert_eq!(status, Some((RECONNECTING.to_string(), t12, false)));
        r.finish(t0 + ms(13_000), true, &mut status);
        assert_eq!(status.map(|s| s.0), Some(RESTORED.to_string()));
    }

    #[test]
    fn los_intentos_fallidos_actualizan_un_solo_aviso() {
        let t0 = Instant::now();
        let mut r = Reconnect::new(ReconnectReason::Stalled, t0);
        let mut status: Status = None;
        // El primer intento falla enseguida (sin red): aún no se dice nada.
        r.on_failed(Duration::from_secs(2), t0 + ms(300));
        r.show(t0 + ms(1_000), &mut status);
        assert_eq!(status, None);
        // Vuelve a fallar; a los 4 s, la cuenta atrás hasta el siguiente.
        r.on_failed(Duration::from_secs(4), t0 + ms(2_500));
        r.show(t0 + ms(4_000), &mut status);
        assert_eq!(status.as_ref().map(|s| s.0.as_str()), Some("Sin conexión con Spotify; reintentando en 3 s…"));
        assert_eq!(r.next_check(t0 + ms(4_000)), ms(500));
        r.show(t0 + ms(5_600), &mut status);
        assert_eq!(status.as_ref().map(|s| s.0.as_str()), Some("Sin conexión con Spotify; reintentando en 1 s…"));
        // Se está intentando: «Reconectando…», en el mismo aviso.
        r.show(t0 + ms(6_600), &mut status);
        assert_eq!(status.as_ref().map(|s| s.0.as_str()), Some(RECONNECTING));
        assert_eq!(r.failures, 2);
    }

    #[test]
    fn otro_aviso_mas_reciente_se_deja_leer() {
        let t0 = Instant::now();
        let mut r = Reconnect::new(ReconnectReason::Lost, t0);
        let mut status: Status = None;
        r.show(t0 + NOTICE_AFTER, &mut status);
        let other = Some(("Añadida a la cola".to_string(), t0 + ms(5_000), false));
        status = other.clone();
        r.show(t0 + ms(6_000), &mut status);
        assert_eq!(status, other);
        // Caducó (la app lo quita a los 8 s): vuelve el de la reconexión.
        status = None;
        r.show(t0 + ms(13_100), &mut status);
        assert_eq!(status.map(|s| s.0), Some(RECONNECTING.to_string()));
    }

    #[test]
    fn una_orden_durante_la_reconexion_lo_dice_ya() {
        let t0 = Instant::now();
        let mut r = Reconnect::new(ReconnectReason::Lost, t0);
        let mut status: Status = Some(("Añadida a la cola".to_string(), t0, false));
        r.ask(t0 + ms(500), &mut status);
        assert_eq!(status.as_ref().map(|s| s.0.as_str()), Some(RECONNECTING));
        assert_eq!(r.next_check(t0 + ms(500)), Duration::from_secs(1));
        r.finish(t0 + ms(1_500), true, &mut status);
        assert_eq!(status.map(|s| s.0), Some(RESTORED.to_string()));
    }

    #[test]
    fn terminar_sin_sesion_quita_el_aviso_propio() {
        let t0 = Instant::now();
        let mut r = Reconnect::new(ReconnectReason::Lost, t0);
        let mut status: Status = None;
        r.show(t0 + NOTICE_AFTER, &mut status);
        r.clone().finish(t0 + ms(5_000), false, &mut status);
        assert_eq!(status, None);
        // Uno ajeno (el error que explica por qué se acabó) no se toca.
        let err = Some(("La cuenta no es Premium".to_string(), t0, true));
        status = err.clone();
        r.finish(t0 + ms(5_000), false, &mut status);
        assert_eq!(status, err);
    }

    #[test]
    fn segundos_hacia_arriba() {
        assert_eq!(secs_up(ms(1)), 1);
        assert_eq!(secs_up(ms(1_000)), 1);
        assert_eq!(secs_up(ms(1_001)), 2);
        assert_eq!(secs_up(ms(30_000)), 30);
    }
}
