//! El enlace con el punto de acceso (AP) de Spotify (Nanofy), separado de la sesión.
//!
//! Antes, perder el AP mataba la conexión entera: la tarea que lee del AP cerraba la sesión
//! (`Session::shutdown`), Spirc terminaba, se borraba el dispositivo en el servidor y Nanofy
//! reconectaba de cero, con el reproductor nuevo y el audio cortado. Pero del AP solo dependen las
//! claves de audio (y el Pong): Spirc, el dealer y spclient van por HTTP y websocket. Ahora una
//! pérdida del AP es un suceso del enlace: la sesión sigue siendo válida y se abre otro enlace,
//! autenticado con las credenciales guardadas, que sustituye al perdido con una generación nueva.
//! La sesión solo queda inválida con un cierre deliberado (cerrar sesión, reiniciar, salir).
//!
//! Aquí está la parte pura: el estado del enlace por generaciones (`LinkSlot`) y la prueba de
//! que el AP enmudeció. Como `fault` y `key_policy`, no usa nada del resto del crate, para que sus
//! pruebas corran en el binario de Nanofy.

use std::time::{Duration, Instant};

/// Lo que se espera, en cada intento de pedir una clave, a que haya un enlace con el que pedirla
/// (mientras se renueva): pasado esto el intento falla como sin respuesta y se reintenta.
pub const KEY_LINK_WAIT: Duration = Duration::from_secs(4);
/// Plazo de una renovación entera: la vuelta por los AP y la autenticación.
pub const RENEW_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkState {
    /// Enlazado y sin motivo de queja.
    Up,
    /// Enlazado, pero una clave se quedó sin respuesta sin que llegara nada del AP: probablemente
    /// mudo. Se sigue usando mientras se renueva.
    Suspect,
    /// Renovación en marcha: las peticiones nuevas esperan al enlace nuevo.
    Renewing,
    /// Sin enlace.
    Down,
}

/// Por qué se perdió (o se sospecha de) un enlace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkLoss {
    ReadError,
    Eof,
    WriteError,
    /// No llegó el Ping o el PongAck a tiempo.
    KeepAlive,
    /// Una clave sin respuesta y nada del AP desde que se pidió (ver `key_timeout_suspect`).
    KeyTimeout,
    /// Corte simulado (`NANOFY_FAULT=ap_drop`).
    Fault,
}

impl LinkLoss {
    /// ¿Se da el enlace por perdido? Una clave sin respuesta solo lo hace sospechoso: el enlace
    /// puede estar vivo y la clave, simplemente, no haber llegado.
    pub fn fatal(self) -> bool {
        !matches!(self, LinkLoss::KeyTimeout)
    }
}

/// Por qué falló una renovación, para decidir si se insiste o se reconecta de cero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenewFailure {
    /// TCP, DNS o tiempo agotado: lo más probable es que no haya red, y reconectar la sesión
    /// entera tampoco llegaría al AP.
    Network,
    /// El saludo cifrado se completó, pero la autenticación se rompió por el camino (un paquete
    /// inesperado o la conexión cerrada a medias). Un fallo del propio saludo cuenta como
    /// `Network`: reconectar de cero pasaría por el mismo saludo.
    Protocol,
    /// El AP rechazó el inicio de sesión (`LoginFailed`: credenciales malas o revocadas, cuenta
    /// sin Premium).
    Rejected,
}

/// Por qué no hubo enlace con el que pedir algo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkWait {
    /// La sesión se cerró: no lo habrá.
    Closed,
    /// No lo hubo en el tiempo dado.
    TimedOut,
}

/// Estado del enlace que se publica (canal `watch` de la sesión).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkStatus {
    /// Generación del enlace (sube con cada uno que se abre); 0 antes del primero.
    pub generation: u64,
    pub state: LinkState,
    /// Lo que lo provocó, si fue una pérdida.
    pub why: Option<LinkLoss>,
}

impl LinkStatus {
    pub const fn initial() -> Self {
        Self {
            generation: 0,
            state: LinkState::Down,
            why: None,
        }
    }
}

/// El enlace actual de una sesión: su generación, su estado y si sigue enlazado (con sus tareas
/// de lectura y escritura). Cada pérdida lleva la generación del enlace que la vio, así que la de
/// un enlace ya sustituido no toca al nuevo.
#[derive(Debug)]
pub struct LinkSlot {
    generation: u64,
    state: LinkState,
    attached: bool,
    since: Instant,
}

impl LinkSlot {
    pub fn new(now: Instant) -> Self {
        Self {
            generation: 0,
            state: LinkState::Down,
            attached: false,
            since: now,
        }
    }

    fn set(&mut self, state: LinkState, now: Instant) {
        if self.state != state {
            self.since = now;
        }
        self.state = state;
    }

    /// Un enlace nuevo entra en servicio: devuelve su generación.
    pub fn attach(&mut self, now: Instant) -> u64 {
        self.generation += 1;
        self.attached = true;
        self.set(LinkState::Up, now);
        self.generation
    }

    /// El enlace `generation` se perdió (o es sospechoso) por `why`. Devuelve si cambió algo:
    /// `false` con la generación de otro enlace o si ya estaba así.
    pub fn lost(&mut self, generation: u64, why: LinkLoss, now: Instant) -> bool {
        if generation != self.generation {
            return false;
        }
        if why.fatal() {
            if !self.attached {
                return false;
            }
            self.attached = false;
            // La renovación en marcha sigue: el enlace que se pierde era el sospechoso.
            if self.state != LinkState::Renewing {
                self.set(LinkState::Down, now);
            }
            true
        } else if self.state == LinkState::Up {
            self.set(LinkState::Suspect, now);
            true
        } else {
            false
        }
    }

    /// Empieza una renovación.
    pub fn renewing(&mut self, now: Instant) {
        self.set(LinkState::Renewing, now);
    }

    /// La renovación falló: se vuelve a lo que había (el enlace sospechoso, si sigue).
    pub fn renew_failed(&mut self, now: Instant) {
        let state = if self.attached {
            LinkState::Suspect
        } else {
            LinkState::Down
        };
        self.set(state, now);
    }

    /// La sesión se cierra: sin enlace.
    pub fn close(&mut self) {
        self.attached = false;
        self.state = LinkState::Down;
    }

    /// ¿Se puede pedir algo por el enlace ahora? Mientras se renueva, no: lo pedido iría por el
    /// enlace sospechoso, y quien pide espera al nuevo (como mucho `KEY_LINK_WAIT`).
    pub fn usable(&self) -> bool {
        self.attached && matches!(self.state, LinkState::Up | LinkState::Suspect)
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn state(&self) -> LinkState {
        self.state
    }

    /// Desde cuándo está en su estado actual.
    pub fn since(&self) -> Instant {
        self.since
    }

    pub fn status(&self, why: Option<LinkLoss>) -> LinkStatus {
        LinkStatus {
            generation: self.generation,
            state: self.state,
            why,
        }
    }
}

/// ¿Enmudeció el AP? Una clave pedida en `sent_at` se quedó sin respuesta y desde entonces no ha
/// llegado ningún paquete de ninguna clase (`last_inbound`: el último). Una clave negada
/// (`AesKeyError`) es un paquete, así que el freno de claves de Spotify nunca cuenta como silencio.
pub fn key_timeout_suspect(sent_at: Instant, last_inbound: Option<Instant>) -> bool {
    last_inbound.is_none_or(|t| t < sent_at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn enlace_nuevo_sube_la_generacion() {
        let t0 = Instant::now();
        let mut slot = LinkSlot::new(t0);
        assert_eq!(slot.generation(), 0);
        assert_eq!(slot.state(), LinkState::Down);
        assert!(!slot.usable());
        assert_eq!(slot.attach(t0), 1);
        assert_eq!(slot.attach(t0 + ms(10)), 2);
        assert_eq!(slot.state(), LinkState::Up);
        assert!(slot.usable());
        assert_eq!(
            slot.status(None),
            LinkStatus { generation: 2, state: LinkState::Up, why: None }
        );
        assert_eq!(LinkStatus::initial().generation, 0);
        assert_eq!(LinkStatus::initial().state, LinkState::Down);
    }

    #[test]
    fn perdida_de_otra_generacion_se_ignora() {
        let t0 = Instant::now();
        let mut slot = LinkSlot::new(t0);
        slot.attach(t0);
        slot.attach(t0);
        for why in [LinkLoss::ReadError, LinkLoss::Eof, LinkLoss::KeyTimeout, LinkLoss::Fault] {
            assert!(!slot.lost(1, why, t0));
            assert!(!slot.lost(3, why, t0));
        }
        assert_eq!(slot.state(), LinkState::Up);
        assert!(slot.usable());
    }

    #[test]
    fn perdidas_graves_sueltan_el_enlace() {
        let t0 = Instant::now();
        for why in [
            LinkLoss::ReadError,
            LinkLoss::Eof,
            LinkLoss::WriteError,
            LinkLoss::KeepAlive,
            LinkLoss::Fault,
        ] {
            assert!(why.fatal());
            let mut slot = LinkSlot::new(t0);
            let g = slot.attach(t0);
            assert!(slot.lost(g, why, t0 + ms(5)));
            assert_eq!(slot.state(), LinkState::Down);
            assert!(!slot.usable());
            assert_eq!(slot.since(), t0 + ms(5));
            // Ya suelto: la segunda no cambia nada.
            assert!(!slot.lost(g, why, t0 + ms(6)));
            assert!(!slot.lost(g, LinkLoss::Eof, t0 + ms(6)));
        }
    }

    #[test]
    fn clave_sin_respuesta_es_sospecha() {
        let t0 = Instant::now();
        assert!(!LinkLoss::KeyTimeout.fatal());
        let mut slot = LinkSlot::new(t0);
        let g = slot.attach(t0);
        assert!(slot.lost(g, LinkLoss::KeyTimeout, t0));
        assert_eq!(slot.state(), LinkState::Suspect);
        assert!(slot.usable());
        assert!(!slot.lost(g, LinkLoss::KeyTimeout, t0));
        assert_eq!(slot.state(), LinkState::Suspect);

        // Desde Down o Renewing, nada.
        let mut down = LinkSlot::new(t0);
        let g = down.attach(t0);
        down.lost(g, LinkLoss::Eof, t0);
        assert!(!down.lost(g, LinkLoss::KeyTimeout, t0));
        assert_eq!(down.state(), LinkState::Down);
        let mut renewing = LinkSlot::new(t0);
        let g = renewing.attach(t0);
        renewing.renewing(t0);
        assert!(!renewing.lost(g, LinkLoss::KeyTimeout, t0));
        assert_eq!(renewing.state(), LinkState::Renewing);
    }

    #[test]
    fn grave_durante_la_renovacion() {
        let t0 = Instant::now();
        let mut slot = LinkSlot::new(t0);
        let g = slot.attach(t0);
        slot.lost(g, LinkLoss::KeyTimeout, t0);
        slot.renewing(t0);
        assert!(slot.lost(g, LinkLoss::ReadError, t0));
        // La renovación sigue, pero ya no hay enlace que usar.
        assert_eq!(slot.state(), LinkState::Renewing);
        assert!(!slot.usable());
        slot.renew_failed(t0);
        assert_eq!(slot.state(), LinkState::Down);
        assert!(!slot.usable());
    }

    #[test]
    fn renovacion_fallida() {
        let t0 = Instant::now();
        let mut slot = LinkSlot::new(t0);
        let g = slot.attach(t0);
        slot.lost(g, LinkLoss::KeyTimeout, t0);
        slot.renewing(t0);
        slot.renew_failed(t0);
        assert_eq!(slot.state(), LinkState::Suspect);
        assert!(slot.usable());

        let mut down = LinkSlot::new(t0);
        let g = down.attach(t0);
        down.lost(g, LinkLoss::Fault, t0);
        down.renewing(t0);
        down.renew_failed(t0);
        assert_eq!(down.state(), LinkState::Down);
        assert!(!down.usable());
        // La renovación que sí sale da un enlace nuevo.
        assert_eq!(down.attach(t0), g + 1);
        assert!(down.usable());
    }

    #[test]
    fn renovando_no_se_envia() {
        let t0 = Instant::now();
        let mut slot = LinkSlot::new(t0);
        slot.attach(t0);
        slot.renewing(t0);
        assert_eq!(slot.state(), LinkState::Renewing);
        assert!(!slot.usable());
    }

    #[test]
    fn sospecha_por_silencio() {
        let t0 = Instant::now();
        let sent = t0 + ms(100);
        // Nada desde que se abrió el enlace, o lo último llegó antes de pedir: mudo.
        assert!(key_timeout_suspect(sent, None));
        assert!(key_timeout_suspect(sent, Some(t0)));
        assert!(key_timeout_suspect(sent, Some(sent - ms(1))));
        // Llegó algo después (un Ping, una clave negada): el AP vive.
        assert!(!key_timeout_suspect(sent, Some(sent)));
        assert!(!key_timeout_suspect(sent, Some(sent + ms(700))));
    }

    #[test]
    fn cerrar_suelta_el_enlace() {
        let t0 = Instant::now();
        let mut slot = LinkSlot::new(t0);
        let g = slot.attach(t0);
        slot.close();
        assert_eq!(slot.state(), LinkState::Down);
        assert!(!slot.usable());
        // Cerrada, ninguna pérdida cambia nada.
        assert!(!slot.lost(g, LinkLoss::Eof, t0));
        assert!(!slot.lost(g, LinkLoss::KeyTimeout, t0));
    }
}
