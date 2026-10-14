//! Reintentos con espera creciente (Nanofy).
//!
//! Un fallo pasajero (la red que se corta un momento, un 5xx, el límite de peticiones) dejaba de
//! ser pasajero:
//! - un contexto que no se pudo resolver (el autoplay al acabarse un álbum, la página siguiente
//!   del DJ) quedaba marcado como no disponible para siempre, y la música se paraba al agotarse
//!   lo que había. Ahora se vuelve a pedir a los 2 s, 5 s, 15 s y luego cada 30 s mientras siga
//!   haciendo falta, uno solo a la vez por contexto;
//! - el registro del dispositivo tras reconectar el dealer (PUT con `NEW_DEVICE`) terminaba
//!   Spirc entero, y con él el audio (Nanofy reconectaba de cero). Ahora se repite a 1 s, 3 s,
//!   10 s y cada 30 s.
//!
//! No usa nada del resto del crate, para que sus pruebas corran en el binario de Nanofy.

use std::{
    collections::HashMap,
    hash::Hash,
    time::{Duration, Instant},
};

/// Esperas tras cada fallo seguido al resolver un contexto; el último escalón se repite.
pub const CONTEXT_STEPS: [Duration; 4] = [
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(30),
];

/// Esperas tras cada fallo seguido al registrar el dispositivo; el último escalón se repite.
pub const REGISTRATION_STEPS: [Duration; 4] = [
    Duration::from_secs(1),
    Duration::from_secs(3),
    Duration::from_secs(10),
    Duration::from_secs(30),
];

/// Con la cola agotada a la espera de un reintento, este se adelanta a como mucho esto: ya no hay
/// música que cubra la espera.
pub const HURRY: Duration = Duration::from_secs(1);

/// Espera tras el fallo seguido número `failures` (desde 1): el escalón que toca o, pasado el
/// último, el último.
pub fn delay(steps: &[Duration], failures: u32) -> Duration {
    let i = (failures.max(1) as usize - 1).min(steps.len().saturating_sub(1));
    steps.get(i).copied().unwrap_or_default()
}

/// Lo que pide esperar el límite de peticiones propio de librespot, según su mensaje («rate
/// limited for at least another N seconds»): reintentar antes solo gastaría el intento.
pub fn rate_limit_wait(message: &str) -> Option<Duration> {
    let rest = message.split("for at least another ").nth(1)?;
    let secs = rest.split_whitespace().next()?.parse().ok()?;
    Some(Duration::from_secs(secs))
}

#[derive(Debug)]
struct Entry<V> {
    /// Lo que se vuelve a pedir: la última versión que falló.
    item: V,
    /// Fallos seguidos.
    failures: u32,
    /// Cuándo se vuelve a intentar; `None` mientras el reintento va de camino (uno solo a la vez).
    at: Option<Instant>,
    /// Antes de esto no, aunque se adelante (lo que pidió el límite de peticiones).
    not_before: Instant,
    /// Lo que sonaba al fallar: si ya suena otra cosa, el reintento no hace falta.
    owner: String,
}

/// Los reintentos pendientes de varias cosas a la vez (los contextos de Spirc), cada una con su
/// propia cuenta de fallos. `K` identifica qué se pide (dos peticiones con la misma `K` son el
/// mismo reintento) y `V` es la petición entera que se repite.
#[derive(Debug)]
pub struct Retries<K, V> {
    steps: &'static [Duration],
    entries: HashMap<K, Entry<V>>,
}

impl<K: Eq + Hash + Clone, V: Clone> Retries<K, V> {
    pub fn new(steps: &'static [Duration]) -> Self {
        Self {
            steps,
            entries: HashMap::new(),
        }
    }

    /// `item` (de clave `key`) falló (otra vez) en `now` mientras sonaba `owner`. Devuelve cuánto
    /// se espera hasta el reintento: el escalón que toca o, si es más, `at_least` (el límite de
    /// peticiones). Con otro `owner` la cuenta vuelve a empezar.
    pub fn failed(
        &mut self,
        key: K,
        item: V,
        owner: &str,
        now: Instant,
        at_least: Option<Duration>,
    ) -> Duration {
        let entry = self.entries.entry(key).or_insert_with(|| Entry {
            item: item.clone(),
            failures: 0,
            at: None,
            not_before: now,
            owner: owner.to_string(),
        });
        if entry.owner != owner {
            entry.failures = 0;
            entry.owner = owner.to_string();
        }
        entry.item = item;
        entry.failures += 1;
        let wait = delay(self.steps, entry.failures).max(at_least.unwrap_or_default());
        entry.at = Some(now + wait);
        entry.not_before = now + at_least.unwrap_or_default();
        wait
    }

    /// `key` salió bien: se olvida su cuenta. `true` si era un reintento.
    pub fn succeeded(&mut self, key: &K) -> bool {
        self.entries.remove(key).is_some()
    }

    /// Ya no hace falta reintentar `key`.
    pub fn forget(&mut self, key: &K) {
        self.entries.remove(key);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// ¿Tiene `key` un reintento pendiente, programado o de camino? Pedirlo también por otro lado
    /// sería una petición más (que probablemente falle igual) o la misma dos veces.
    pub fn pending(&self, key: &K) -> bool {
        self.entries.contains_key(key)
    }

    /// El próximo reintento programado, si hay alguno.
    pub fn next_at(&self) -> Option<Instant> {
        self.entries.values().filter_map(|e| e.at).min()
    }

    /// Los que ya toca reintentar en `now`, con lo que sonaba al fallar. Quedan «de camino» (sin
    /// hora) hasta su `succeeded` o su `failed`: así no sale otro detrás mientras tanto.
    pub fn take_due(&mut self, now: Instant) -> Vec<(K, V, String)> {
        self.entries
            .iter_mut()
            .filter(|(_, e)| e.at.is_some_and(|at| at <= now))
            .map(|(k, e)| {
                e.at = None;
                (k.clone(), e.item.clone(), e.owner.clone())
            })
            .collect()
    }

    /// Adelanta los reintentos programados a como mucho `soon` desde `now`, sin bajar de lo que
    /// pidió el límite de peticiones.
    pub fn hurry(&mut self, now: Instant, soon: Duration) {
        for e in self.entries.values_mut() {
            if let Some(at) = e.at.as_mut() {
                *at = (*at).min(now + soon).max(e.not_before);
            }
        }
    }

    /// Lo que espera un reintento (programado o de camino) y falló mientras sonaba `owner`: lo de
    /// otro contexto ya no traerá nada para este (se olvida cuando le toque).
    pub fn items_for<'a>(&'a self, owner: &'a str) -> impl Iterator<Item = &'a V> {
        self.entries
            .values()
            .filter(move |e| e.owner == owner)
            .map(|e| &e.item)
    }

    /// Fallos seguidos de `key` (0 si no tiene reintento pendiente).
    pub fn failures(&self, key: &K) -> u32 {
        self.entries.get(key).map_or(0, |e| e.failures)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: fn(u64) -> Duration = Duration::from_secs;

    fn due(item: &'static str, owner: &str) -> (&'static str, &'static str, String) {
        (item, item, owner.to_string())
    }

    #[test]
    fn escalones_del_contexto_y_luego_cada_30_s() {
        let got: Vec<_> = (1..=6).map(|n| delay(&CONTEXT_STEPS, n)).collect();
        assert_eq!(got, [S(2), S(5), S(15), S(30), S(30), S(30)]);
        // Un «fallo 0» cuenta como el primero.
        assert_eq!(delay(&CONTEXT_STEPS, 0), S(2));
    }

    #[test]
    fn escalones_del_registro() {
        let got: Vec<_> = (1..=5).map(|n| delay(&REGISTRATION_STEPS, n)).collect();
        assert_eq!(got, [S(1), S(3), S(10), S(30), S(30)]);
        assert_eq!(delay(&[], 3), Duration::ZERO);
    }

    #[test]
    fn mensaje_del_limite_de_peticiones() {
        assert_eq!(
            rate_limit_wait(
                "Resource has been exhausted { rate limited for at least another 12 seconds }"
            ),
            Some(S(12))
        );
        assert_eq!(rate_limit_wait("Service unavailable { 503 }"), None);
        assert_eq!(rate_limit_wait("for at least another x seconds"), None);
    }

    #[test]
    fn un_reintento_a_la_vez_y_con_espera_creciente() {
        let t0 = Instant::now();
        let mut r = Retries::new(&CONTEXT_STEPS);
        assert_eq!(r.failed("autoplay", "autoplay", "album", t0, None), S(2));
        assert!(r.pending(&"autoplay"));
        assert_eq!(r.next_at(), Some(t0 + S(2)));
        // Aún no toca.
        assert!(r.take_due(t0 + S(1)).is_empty());
        // Toca: sale una vez y queda de camino, sin hora, pero aún pendiente.
        assert_eq!(r.take_due(t0 + S(2)), vec![due("autoplay", "album")]);
        assert!(r.take_due(t0 + S(3)).is_empty());
        assert_eq!(r.next_at(), None);
        assert!(r.pending(&"autoplay"), "de camino sigue pendiente");
        // Vuelve a fallar: el escalón siguiente.
        let t1 = t0 + S(3);
        assert_eq!(r.failed("autoplay", "autoplay", "album", t1, None), S(5));
        assert_eq!(r.failures(&"autoplay"), 2);
        assert_eq!(r.next_at(), Some(t1 + S(5)));
        // Sale bien: se olvida.
        assert!(r.succeeded(&"autoplay"));
        assert!(!r.pending(&"autoplay"));
        assert_eq!(r.len(), 0);
        assert!(!r.succeeded(&"autoplay"));
        // Y un fallo nuevo empieza otra vez por el primer escalón.
        assert_eq!(r.failed("autoplay", "autoplay", "album", t1, None), S(2));
    }

    #[test]
    fn la_misma_clave_es_un_solo_reintento_con_la_ultima_peticion() {
        // El autoplay de un álbum se pide con la canción que suena como reserva: cambia con cada
        // canción, pero es el mismo reintento.
        let t0 = Instant::now();
        let mut r = Retries::new(&CONTEXT_STEPS);
        r.failed("autoplay", "reserva 9", "album", t0, None);
        assert_eq!(r.failed("autoplay", "reserva 10", "album", t0, None), S(5));
        assert_eq!(r.len(), 1);
        assert_eq!(
            r.take_due(t0 + S(5)),
            vec![("autoplay", "reserva 10", "album".to_string())]
        );
    }

    #[test]
    fn otro_contexto_sonando_empieza_la_cuenta_de_nuevo() {
        let t0 = Instant::now();
        let mut r = Retries::new(&CONTEXT_STEPS);
        r.failed("autoplay", "autoplay", "album:a", t0, None);
        r.failed("autoplay", "autoplay", "album:a", t0, None);
        assert_eq!(r.failed("autoplay", "autoplay", "album:b", t0, None), S(2));
        assert_eq!(r.take_due(t0 + S(2)), vec![due("autoplay", "album:b")]);
    }

    #[test]
    fn pendientes_de_lo_que_suena() {
        let t0 = Instant::now();
        let mut r = Retries::new(&CONTEXT_STEPS);
        r.failed("autoplay", "autoplay", "album:a", t0, None);
        r.failed("page", "page", "dj", t0, None);
        assert_eq!(r.items_for("dj").collect::<Vec<_>>(), vec![&"page"]);
        assert_eq!(r.items_for("album:a").collect::<Vec<_>>(), vec![&"autoplay"]);
        assert_eq!(r.items_for("album:b").count(), 0);
        // De camino también cuenta: aún puede traer algo.
        r.take_due(t0 + S(2));
        assert_eq!(r.items_for("dj").count(), 1);
    }

    #[test]
    fn el_limite_de_peticiones_manda_si_pide_mas() {
        let t0 = Instant::now();
        let mut r = Retries::new(&CONTEXT_STEPS);
        assert_eq!(r.failed("page", "page", "dj", t0, Some(S(40))), S(40));
        // Adelantar no baja de lo que pidió el límite.
        r.hurry(t0, HURRY);
        assert_eq!(r.next_at(), Some(t0 + S(40)));
        // Si pide menos que el escalón, manda el escalón.
        assert_eq!(r.failed("page", "page", "dj", t0, Some(S(1))), S(5));
    }

    #[test]
    fn adelantar_cuando_se_agota_la_cola() {
        let t0 = Instant::now();
        let mut r = Retries::new(&CONTEXT_STEPS);
        for _ in 0..4 {
            r.failed("autoplay", "autoplay", "album", t0, None);
        }
        assert_eq!(r.next_at(), Some(t0 + S(30)));
        let t1 = t0 + S(10);
        r.hurry(t1, HURRY);
        assert_eq!(r.next_at(), Some(t1 + HURRY));
        // Uno que ya iba a ser antes no se retrasa.
        r.failed("page", "page", "album", t1, None);
        r.hurry(t1, S(10));
        assert_eq!(r.take_due(t1 + HURRY), vec![due("autoplay", "album")]);
        assert_eq!(r.next_at(), Some(t1 + S(2)));
        // Uno de camino no se toca.
        assert_eq!(r.take_due(t1 + S(2)), vec![due("page", "album")]);
        r.hurry(t1 + S(2), HURRY);
        assert_eq!(r.next_at(), None);
    }

    #[test]
    fn olvidar_y_vaciar() {
        let t0 = Instant::now();
        let mut r = Retries::new(&CONTEXT_STEPS);
        r.failed("a", "a", "x", t0, None);
        r.failed("b", "b", "x", t0, None);
        r.forget(&"a");
        assert!(!r.pending(&"a"));
        assert!(r.pending(&"b"));
        r.clear();
        assert_eq!(r.len(), 0);
        assert_eq!(r.next_at(), None);
    }
}
