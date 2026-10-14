//! Quién cuida del enlace con el punto de acceso (AP): cuándo renovarlo y qué hacer si la
//! renovación falla (ver `librespot_core::ap_link`).
//!
//! - Enlace caído: se renueva ya, sin límite (cada caída es un enlace nuevo).
//! - Enlace sospechoso (una clave sin respuesta y el AP callado): también, pero como mucho una vez
//!   cada `SUSPECT_EVERY`; el enlace aún sirve y la sospecha puede ser falsa.
//! - Renovación fallida por la red: se insiste con espera creciente (1, 2, 4, 8 y luego cada
//!   15 s) y nunca se reconecta la sesión entera: sin red tampoco llegaría al AP, y solo cortaría
//!   el audio que queda en el búfer.
//! - Por el protocolo: igual, pero al quinto fallo seguido se reconecta de cero.
//! - Rechazada (credenciales revocadas, cuenta sin Premium): se reconecta de cero al momento, que
//!   ya sabe explicar y resolver esos casos.
//!
//! Solo decide; el backend lanza las renovaciones. Cada estado lleva la generación del enlace, y
//! lo de un enlace anterior se ignora.

use std::time::{Duration, Instant};

use librespot_core::ap_link::{LinkState, LinkStatus, RenewFailure};

/// Entre dos renovaciones por sospecha, al menos esto.
pub const SUSPECT_EVERY: Duration = Duration::from_secs(30);
/// Fallos seguidos del protocolo tras los que se reconecta la sesión entera.
pub const PROTOCOL_ESCALATE: u32 = 5;
/// Esperas tras cada fallo seguido de la renovación; el último escalón se repite.
const BACKOFF: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(15),
];

/// Lo que toca hacer con un estado nuevo del enlace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    RenewNow,
    /// Renovar a esa hora (la sospecha llegó demasiado pronto tras la anterior).
    RenewAt(Instant),
    Nothing,
}

/// Lo que toca hacer al terminar una renovación.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Renewed,
    RetryAt(Instant),
    /// Reconectar la sesión entera, como antes de que el enlace se renovara solo.
    Escalate,
}

#[derive(Debug, Default)]
pub struct Keeper {
    /// La generación más nueva vista.
    link_gen: u64,
    renewing: bool,
    /// Renovaciones fallidas seguidas (cualquier motivo), para la espera creciente.
    fails: u32,
    protocol_fails: u32,
    /// Desde cuándo hay problemas con el enlace (caído o sospechoso), hasta que vuelve.
    down_since: Option<Instant>,
    /// Última renovación lanzada por una sospecha.
    last_proactive: Option<Instant>,
    /// El enlace cuya renovación falló y espera su reintento: el estado que la sesión vuelve a
    /// publicar al fallar (caído o sospechoso, de la misma generación) no adelanta el reintento.
    /// Sin esto, sin red, cada fallo lanzaba otro al momento y la espera creciente no servía.
    retry_for: Option<u64>,
}

/// Espera tras el fallo seguido número `n` (desde 1).
pub fn backoff(n: u32) -> Duration {
    BACKOFF[(n.max(1) as usize - 1).min(BACKOFF.len() - 1)]
}

impl Keeper {
    pub fn on_status(&mut self, st: LinkStatus, now: Instant) -> Action {
        // Antes del primer enlace, o de uno ya sustituido: nada.
        if st.generation == 0 || st.generation < self.link_gen {
            return Action::Nothing;
        }
        self.link_gen = st.generation;
        if self.retry_for.is_some_and(|g| g < st.generation) {
            self.retry_for = None;
        }
        let waiting = self.renewing || self.retry_for == Some(st.generation);
        match st.state {
            LinkState::Up => {
                if !self.renewing {
                    self.down_since = None;
                }
                Action::Nothing
            }
            LinkState::Down => {
                self.down_since.get_or_insert(now);
                if waiting {
                    Action::Nothing
                } else {
                    Action::RenewNow
                }
            }
            LinkState::Suspect => {
                self.down_since.get_or_insert(now);
                if waiting {
                    return Action::Nothing;
                }
                match self.last_proactive {
                    Some(last) if now < last + SUSPECT_EVERY => Action::RenewAt(last + SUSPECT_EVERY),
                    _ => {
                        self.last_proactive = Some(now);
                        Action::RenewNow
                    }
                }
            }
            LinkState::Renewing => Action::Nothing,
        }
    }

    /// Va a lanzarse ahora la renovación por sospecha que se aplazó (`Action::RenewAt`): cuenta
    /// para el límite de `SUSPECT_EVERY` igual que una inmediata. Sin esto, otra sospecha justo
    /// después volvía a renovar al momento: dos inicios de sesión en el AP casi seguidos.
    pub fn proactive_started(&mut self, now: Instant) {
        self.last_proactive = Some(now);
    }

    /// Se lanzó una renovación.
    pub fn started(&mut self) {
        self.renewing = true;
        self.retry_for = None;
    }

    /// Terminó la renovación lanzada: la generación del enlace nuevo o por qué falló.
    pub fn on_result(&mut self, r: Result<u64, RenewFailure>, now: Instant) -> Outcome {
        self.renewing = false;
        match r {
            Ok(g) => {
                self.link_gen = self.link_gen.max(g);
                self.fails = 0;
                self.protocol_fails = 0;
                self.down_since = None;
                self.retry_for = None;
                Outcome::Renewed
            }
            Err(RenewFailure::Rejected) => Outcome::Escalate,
            Err(RenewFailure::Protocol) => {
                self.fails += 1;
                self.protocol_fails += 1;
                if self.protocol_fails >= PROTOCOL_ESCALATE {
                    return Outcome::Escalate;
                }
                self.retry_for = Some(self.link_gen);
                Outcome::RetryAt(now + backoff(self.fails))
            }
            Err(RenewFailure::Network) => {
                self.fails += 1;
                self.retry_for = Some(self.link_gen);
                Outcome::RetryAt(now + backoff(self.fails))
            }
        }
    }

    /// Cuánto lleva el enlace con problemas; `None` si está bien.
    pub fn waiting_for(&self, now: Instant) -> Option<Duration> {
        self.down_since.map(|t| now.saturating_duration_since(t))
    }

    /// Conexión nueva o cerrada: se empieza de cero.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(generation: u64, state: LinkState) -> LinkStatus {
        LinkStatus { generation, state, why: None }
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn caida_renueva_ya() {
        let t0 = Instant::now();
        let mut k = Keeper::default();
        assert_eq!(k.on_status(st(1, LinkState::Up), t0), Action::Nothing);
        assert_eq!(k.on_status(st(1, LinkState::Down), t0), Action::RenewNow);
        k.started();
        assert_eq!(k.on_result(Ok(2), t0 + secs(1)), Outcome::Renewed);
        // Otra caída enseguida: otra vez ya, sin límite.
        assert_eq!(k.on_status(st(2, LinkState::Down), t0 + secs(1)), Action::RenewNow);
        k.started();
        assert_eq!(k.on_result(Ok(3), t0 + secs(2)), Outcome::Renewed);
        assert_eq!(k.on_status(st(3, LinkState::Down), t0 + secs(2)), Action::RenewNow);
    }

    #[test]
    fn antes_del_primer_enlace_nada() {
        let t0 = Instant::now();
        let mut k = Keeper::default();
        assert_eq!(k.on_status(LinkStatus::initial(), t0), Action::Nothing);
        assert_eq!(k.waiting_for(t0), None);
    }

    #[test]
    fn sospecha_limitada() {
        let t0 = Instant::now();
        let mut k = Keeper::default();
        assert_eq!(k.on_status(st(1, LinkState::Suspect), t0), Action::RenewNow);
        k.started();
        assert_eq!(k.on_result(Ok(2), t0 + secs(1)), Outcome::Renewed);
        assert_eq!(k.on_status(st(2, LinkState::Suspect), t0 + secs(10)), Action::RenewAt(t0 + SUSPECT_EVERY));
        // Pasado el plazo, otra vez ya.
        assert_eq!(k.on_status(st(2, LinkState::Suspect), t0 + secs(31)), Action::RenewNow);
        assert_eq!(k.on_status(st(2, LinkState::Suspect), t0 + secs(40)), Action::RenewAt(t0 + secs(61)));
    }

    #[test]
    fn sospecha_aplazada_cuenta_para_el_limite() {
        let t0 = Instant::now();
        let mut k = Keeper::default();
        assert_eq!(k.on_status(st(1, LinkState::Suspect), t0), Action::RenewNow);
        k.started();
        assert_eq!(k.on_result(Ok(2), t0 + secs(1)), Outcome::Renewed);
        assert_eq!(k.on_status(st(2, LinkState::Suspect), t0 + secs(10)), Action::RenewAt(t0 + SUSPECT_EVERY));
        // Llega la hora: el backend la lanza (`Next::LinkRetry`).
        k.proactive_started(t0 + secs(30));
        k.started();
        assert_eq!(k.on_result(Ok(3), t0 + secs(31)), Outcome::Renewed);
        // El AP nuevo tampoco contesta enseguida: se espera desde la renovación aplazada, no
        // desde la sospecha anterior.
        assert_eq!(k.on_status(st(3, LinkState::Suspect), t0 + secs(32)), Action::RenewAt(t0 + secs(60)));
    }

    #[test]
    fn sin_doble_renovacion() {
        let t0 = Instant::now();
        let mut k = Keeper::default();
        assert_eq!(k.on_status(st(1, LinkState::Down), t0), Action::RenewNow);
        k.started();
        assert_eq!(k.on_status(st(1, LinkState::Renewing), t0), Action::Nothing);
        assert_eq!(k.on_status(st(1, LinkState::Down), t0), Action::Nothing);
        assert_eq!(k.on_status(st(1, LinkState::Suspect), t0), Action::Nothing);
    }

    #[test]
    fn espera_creciente() {
        let t0 = Instant::now();
        let mut k = Keeper::default();
        k.on_status(st(1, LinkState::Down), t0);
        for wait in [1, 2, 4, 8, 15, 15] {
            k.started();
            assert_eq!(k.on_result(Err(RenewFailure::Network), t0), Outcome::RetryAt(t0 + secs(wait)));
        }
    }

    #[test]
    fn fallo_espera_su_reintento() {
        let t0 = Instant::now();
        let mut k = Keeper::default();
        assert_eq!(k.on_status(st(1, LinkState::Down), t0), Action::RenewNow);
        k.started();
        assert_eq!(k.on_result(Err(RenewFailure::Network), t0), Outcome::RetryAt(t0 + secs(1)));
        // Lo que la sesión publica al fallar no adelanta el reintento.
        assert_eq!(k.on_status(st(1, LinkState::Down), t0), Action::Nothing);
        assert_eq!(k.on_status(st(1, LinkState::Suspect), t0), Action::Nothing);
        // El reintento sale bien: la caída del enlace nuevo se renueva ya.
        k.started();
        assert_eq!(k.on_result(Ok(2), t0 + secs(2)), Outcome::Renewed);
        assert_eq!(k.on_status(st(2, LinkState::Down), t0 + secs(3)), Action::RenewNow);
    }

    #[test]
    fn sin_red_nunca_escala() {
        let t0 = Instant::now();
        let mut k = Keeper::default();
        k.on_status(st(1, LinkState::Down), t0);
        for _ in 0..100 {
            k.started();
            assert!(matches!(k.on_result(Err(RenewFailure::Network), t0), Outcome::RetryAt(_)));
        }
    }

    #[test]
    fn rechazo_escala() {
        let t0 = Instant::now();
        let mut k = Keeper::default();
        k.on_status(st(1, LinkState::Down), t0);
        k.started();
        assert_eq!(k.on_result(Err(RenewFailure::Rejected), t0), Outcome::Escalate);
    }

    #[test]
    fn protocolo_escala_al_quinto() {
        let t0 = Instant::now();
        let mut k = Keeper::default();
        k.on_status(st(1, LinkState::Down), t0);
        for wait in [1, 2, 4, 8] {
            k.started();
            assert_eq!(k.on_result(Err(RenewFailure::Protocol), t0), Outcome::RetryAt(t0 + secs(wait)));
        }
        k.started();
        assert_eq!(k.on_result(Err(RenewFailure::Protocol), t0), Outcome::Escalate);
    }

    #[test]
    fn exito_reinicia() {
        let t0 = Instant::now();
        let mut k = Keeper::default();
        k.on_status(st(1, LinkState::Down), t0);
        for _ in 0..3 {
            k.started();
            k.on_result(Err(RenewFailure::Protocol), t0);
        }
        k.started();
        assert_eq!(k.on_result(Ok(2), t0 + secs(5)), Outcome::Renewed);
        assert_eq!(k.waiting_for(t0 + secs(5)), None);
        // La cuenta de fallos empezó de cero: el siguiente espera 1 s y quedan cinco del protocolo.
        k.on_status(st(2, LinkState::Down), t0 + secs(6));
        for wait in [1, 2, 4, 8] {
            k.started();
            assert_eq!(k.on_result(Err(RenewFailure::Protocol), t0), Outcome::RetryAt(t0 + secs(wait)));
        }
    }

    #[test]
    fn estado_viejo_tras_renovar() {
        let t0 = Instant::now();
        let mut k = Keeper::default();
        k.on_status(st(1, LinkState::Suspect), t0);
        k.started();
        assert_eq!(k.on_result(Ok(2), t0), Outcome::Renewed);
        assert_eq!(k.on_status(st(1, LinkState::Suspect), t0 + secs(60)), Action::Nothing);
        assert_eq!(k.on_status(st(1, LinkState::Down), t0 + secs(60)), Action::Nothing);
        assert_eq!(k.waiting_for(t0 + secs(60)), None);
    }

    #[test]
    fn espera_desde_la_primera_caida() {
        let t0 = Instant::now();
        let mut k = Keeper::default();
        assert_eq!(k.on_status(st(1, LinkState::Up), t0), Action::Nothing);
        assert_eq!(k.waiting_for(t0), None);
        k.on_status(st(1, LinkState::Suspect), t0);
        k.started();
        k.on_status(st(1, LinkState::Renewing), t0 + secs(1));
        assert_eq!(k.waiting_for(t0 + secs(2)), Some(secs(2)));
        // La renovación falla y el enlace cae del todo: la espera sigue contando desde la
        // primera señal, no desde la caída.
        k.on_result(Err(RenewFailure::Network), t0 + secs(3));
        k.on_status(st(1, LinkState::Down), t0 + secs(4));
        assert_eq!(k.waiting_for(t0 + secs(7)), Some(secs(7)));
        // El enlace nuevo, publicado mientras se renueva, aún no la borra; el resultado sí.
        k.started();
        k.on_status(st(2, LinkState::Up), t0 + secs(8));
        assert_eq!(k.waiting_for(t0 + secs(8)), Some(secs(8)));
        k.on_result(Ok(2), t0 + secs(8));
        assert_eq!(k.waiting_for(t0 + secs(8)), None);
        k.on_status(st(2, LinkState::Suspect), t0 + secs(9));
        k.reset();
        assert_eq!(k.waiting_for(t0 + secs(9)), None);
    }
}
