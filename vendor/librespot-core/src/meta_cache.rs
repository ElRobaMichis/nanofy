//! Caché en memoria de los metadatos (extended-metadata) que pide el reproductor (Nanofy).
//!
//! librespot pedía los metadatos de cada canción al cargarla, siempre: una ida y vuelta a
//! spclient en el camino del primer sonido, aunque Nanofy acabara de traer esos mismos bytes en
//! los lotes de 500 con los que pinta cada playlist (y los tirara). Ahora:
//! - lo que pide el reproductor se guarda (20 min): repetir, volver atrás o recargar no lo pide
//!   otra vez;
//! - Nanofy «siembra» aquí lo que traen sus lotes (las primeras filas de cada lista), así la
//!   canción en la que se hace clic ya tiene sus metadatos.
//!
//! Las semillas no se sirven a ciegas: los lotes de Nanofy llevan país y catálogo en la cabecera y
//! la petición del reproductor no. La primera vez que el reproductor encuentra una semilla, la
//! pide igualmente a Spotify y compara los bytes (`SeedTrust`): si son iguales, las semillas
//! valen para el resto del proceso; si no, se dejan de usar y se tiran.
//!
//! El tope es de bytes y no de entradas (Nanofy vigila la memoria): una playlist de 10.000 no
//! puede llenarla, porque solo se siembran sus primeras filas (`SEED_ROWS`), y lo que no se usa
//! sale primero.
//!
//! No usa nada del resto del crate, para que sus pruebas corran en el binario de Nanofy.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// Tope de lo que ocupan las entradas (sus bytes y `ENTRY_OVERHEAD` cada una).
pub const CAP_BYTES: usize = 3 * 1024 * 1024;
/// Lo que vale una entrada: los metadatos de una canción casi nunca cambian en minutos, y así una
/// sesión larga no se queda con ficheros o restricciones viejos.
pub const TTL: Duration = Duration::from_secs(20 * 60);
/// Lo que cuesta cada entrada además de sus bytes y su clave (nodo del mapa, marcas de tiempo):
/// sin esto, miles de entradas diminutas se saltarían el tope.
pub const ENTRY_OVERHEAD: usize = 96;
/// Filas de cada carga de lista que se siembran como mucho (las que se ven primero y las que más
/// se pulsan). Sembrar una lista entera de miles echaría lo que de verdad se está mirando.
pub const SEED_ROWS: usize = 300;

/// Qué metadatos: país de la cuenta (cambia qué ediciones y restricciones trae), tipo de extensión
/// (TRACK_V4, EPISODE_V4…) y uri.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MetaKey {
    pub country: String,
    pub kind: i32,
    pub uri: String,
}

impl MetaKey {
    pub fn new(country: impl Into<String>, kind: i32, uri: impl Into<String>) -> Self {
        Self {
            country: country.into(),
            kind,
            uri: uri.into(),
        }
    }

    fn size(&self) -> usize {
        self.country.len() + self.uri.len() + std::mem::size_of::<i32>()
    }
}

/// Lo que devuelve la caché: el valor y si es una semilla (de un lote de Nanofy) o lo que
/// devolvió Spotify al pedirlo el propio reproductor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit<V> {
    pub value: V,
    pub seeded: bool,
}

struct Entry<V> {
    value: V,
    size: usize,
    at: Instant,
    /// Turno del último uso: sale primero lo que lleva más tiempo sin usarse.
    used: u64,
    seeded: bool,
}

pub struct MetaCache<V> {
    map: HashMap<MetaKey, Entry<V>>,
    bytes: usize,
    cap: usize,
    ttl: Duration,
    tick: u64,
}

impl<V: Clone + AsRef<[u8]>> MetaCache<V> {
    pub fn new(cap: usize, ttl: Duration) -> Self {
        Self {
            map: HashMap::new(),
            bytes: 0,
            cap,
            ttl,
            tick: 0,
        }
    }

    fn fresh(&self, e: &Entry<V>, now: Instant) -> bool {
        now.saturating_duration_since(e.at) < self.ttl
    }

    /// El valor de `key` si aún vale; uno caducado se quita.
    pub fn get(&mut self, key: &MetaKey, now: Instant) -> Option<Hit<V>> {
        let fresh = self.map.get(key).map(|e| self.fresh(e, now))?;
        if !fresh {
            self.remove(key);
            return None;
        }
        self.tick += 1;
        let tick = self.tick;
        let e = self.map.get_mut(key)?;
        e.used = tick;
        Some(Hit {
            value: e.value.clone(),
            seeded: e.seeded,
        })
    }

    /// ¿Hay algo que aún vale para `key`? (sin contarlo como uso).
    pub fn contains(&self, key: &MetaKey, now: Instant) -> bool {
        self.peek(key, now).is_some()
    }

    /// Si hay algo que aún vale para `key`, si es una semilla (sin contarlo como uso).
    pub fn peek(&self, key: &MetaKey, now: Instant) -> Option<bool> {
        self.map
            .get(key)
            .filter(|e| self.fresh(e, now))
            .map(|e| e.seeded)
    }

    /// Guarda `value`. Una semilla no sustituye a lo que devolvió Spotify al reproductor mientras
    /// eso siga valiendo: es lo comprobado. Lo que no cabe ni con la caché vacía no se guarda.
    pub fn put(&mut self, key: MetaKey, value: V, seeded: bool, now: Instant) {
        if seeded
            && self
                .map
                .get(&key)
                .is_some_and(|e| !e.seeded && self.fresh(e, now))
        {
            return;
        }
        let size = key.size() + value.as_ref().len() + ENTRY_OVERHEAD;
        self.remove(&key);
        if size > self.cap {
            return;
        }
        self.tick += 1;
        self.bytes += size;
        self.map.insert(
            key,
            Entry {
                value,
                size,
                at: now,
                used: self.tick,
                seeded,
            },
        );
        self.evict(now);
    }

    pub fn remove(&mut self, key: &MetaKey) {
        if let Some(e) = self.map.remove(key) {
            self.bytes -= e.size;
        }
    }

    /// Quita todas las semillas (dejaron de ser de fiar).
    pub fn drop_seeded(&mut self) {
        let mut freed = 0;
        self.map.retain(|_, e| {
            if e.seeded {
                freed += e.size;
            }
            !e.seeded
        });
        self.bytes -= freed;
    }

    /// Por encima del tope: primero lo caducado y luego lo que lleva más tiempo sin usarse.
    fn evict(&mut self, now: Instant) {
        if self.bytes <= self.cap {
            return;
        }
        let ttl = self.ttl;
        let mut freed = 0;
        self.map.retain(|_, e| {
            let keep = now.saturating_duration_since(e.at) < ttl;
            if !keep {
                freed += e.size;
            }
            keep
        });
        self.bytes -= freed;
        if self.bytes <= self.cap {
            return;
        }
        // Una sola pasada ordenada en vez de buscar el mínimo cada vez: al sembrar un lote se
        // insertan cientos seguidas.
        let mut order: Vec<(u64, MetaKey)> =
            self.map.iter().map(|(k, e)| (e.used, k.clone())).collect();
        order.sort_unstable_by_key(|(used, _)| *used);
        for (_, key) in order {
            if self.bytes <= self.cap {
                break;
            }
            self.remove(&key);
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn seeded(&self) -> usize {
        self.map.values().filter(|e| e.seeded).count()
    }
}

/// ¿Se puede servir al reproductor lo que siembra Nanofy? Se decide una sola vez por proceso,
/// con la primera semilla que el reproductor necesita (ver el comentario del módulo).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SeedTrust {
    /// Aún sin comprobar: las semillas se guardan, pero el reproductor pide igualmente.
    #[default]
    Unverified,
    /// Comprobado: los bytes del lote son los mismos que los de la petición del reproductor.
    Trusted,
    /// No lo son: ni se guardan ni se sirven.
    Rejected,
}

impl SeedTrust {
    /// Tras comparar una semilla con lo que devolvió Spotify para la misma canción. Solo cuenta la
    /// primera comparación: lo decidido ya no cambia.
    pub fn after_check(self, seed: &[u8], fetched: &[u8]) -> SeedTrust {
        match self {
            SeedTrust::Unverified if seed == fetched => SeedTrust::Trusted,
            SeedTrust::Unverified => SeedTrust::Rejected,
            decided => decided,
        }
    }

    /// Se aceptan semillas nuevas.
    pub fn accepts_seeds(self) -> bool {
        self != SeedTrust::Rejected
    }

    /// Una semilla se da al reproductor sin pedir nada.
    pub fn serves_seeds(self) -> bool {
        self == SeedTrust::Trusted
    }

    pub fn name(self) -> &'static str {
        match self {
            SeedTrust::Unverified => "unverified",
            SeedTrust::Trusted => "trusted",
            SeedTrust::Rejected => "rejected",
        }
    }
}

/// Qué hacer con una petición de metadatos del reproductor según lo que haya en la caché.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lookup<V> {
    /// Servir esto sin pedir nada («hit», o «seed» si era una semilla ya de fiar).
    Serve { value: V, seeded: bool },
    /// Pedirlo a Spotify y comparar con esta semilla (la comprobación única).
    Check(V),
    /// Pedirlo a Spotify.
    Fetch,
}

/// La decisión de `Lookup` para lo que devolvió la caché.
pub fn lookup<V>(hit: Option<Hit<V>>, trust: SeedTrust) -> Lookup<V> {
    match hit {
        Some(Hit {
            value,
            seeded: false,
        }) => Lookup::Serve {
            value,
            seeded: false,
        },
        Some(Hit {
            value,
            seeded: true,
        }) => match trust {
            SeedTrust::Trusted => Lookup::Serve {
                value,
                seeded: true,
            },
            SeedTrust::Unverified => Lookup::Check(value),
            SeedTrust::Rejected => Lookup::Fetch,
        },
        None => Lookup::Fetch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(uri: &str) -> MetaKey {
        MetaKey::new("ES", 10, uri)
    }

    fn val(n: usize, b: u8) -> Vec<u8> {
        vec![b; n]
    }

    #[test]
    fn guarda_y_devuelve_y_caduca() {
        let t0 = Instant::now();
        let mut c = MetaCache::new(CAP_BYTES, TTL);
        c.put(key("spotify:track:a"), val(100, 1), false, t0);
        let hit = c.get(&key("spotify:track:a"), t0 + Duration::from_secs(60)).unwrap();
        assert_eq!(hit.value, val(100, 1));
        assert!(!hit.seeded);
        // Otro país u otro tipo es otra entrada.
        assert!(c.get(&MetaKey::new("FR", 10, "spotify:track:a"), t0).is_none());
        assert!(c.get(&MetaKey::new("ES", 11, "spotify:track:a"), t0).is_none());
        // Pasados los 20 min no vale y se quita (con sus bytes).
        assert!(c.get(&key("spotify:track:a"), t0 + TTL).is_none());
        assert_eq!(c.len(), 0);
        assert_eq!(c.bytes(), 0);
    }

    #[test]
    fn el_tope_es_de_bytes_y_sale_lo_menos_usado() {
        let t0 = Instant::now();
        let entry = |uri: &str| key(uri).size() + 1000 + ENTRY_OVERHEAD;
        // Caben tres entradas de 1000 bytes.
        let cap = entry("spotify:track:a") * 3;
        let mut c = MetaCache::new(cap, TTL);
        c.put(key("spotify:track:a"), val(1000, 1), false, t0);
        c.put(key("spotify:track:b"), val(1000, 2), false, t0);
        c.put(key("spotify:track:c"), val(1000, 3), false, t0);
        // Usar «a» la pone por delante: al llegar «d» sale «b».
        assert!(c.get(&key("spotify:track:a"), t0).is_some());
        c.put(key("spotify:track:d"), val(1000, 4), false, t0);
        assert_eq!(c.len(), 3);
        assert!(c.bytes() <= cap);
        assert!(c.contains(&key("spotify:track:a"), t0));
        assert!(!c.contains(&key("spotify:track:b"), t0));
        assert!(c.contains(&key("spotify:track:d"), t0));
        // Lo que no cabe ni solo no se guarda ni echa a nadie.
        c.put(key("spotify:track:big"), val(cap, 9), false, t0);
        assert!(!c.contains(&key("spotify:track:big"), t0));
        assert_eq!(c.len(), 3);
    }

    #[test]
    fn sustituir_no_descuadra_los_bytes() {
        let t0 = Instant::now();
        let mut c = MetaCache::new(CAP_BYTES, TTL);
        c.put(key("spotify:track:a"), val(500, 1), true, t0);
        c.put(key("spotify:track:a"), val(200, 2), false, t0);
        assert_eq!(c.len(), 1);
        assert_eq!(c.bytes(), key("spotify:track:a").size() + 200 + ENTRY_OVERHEAD);
        c.remove(&key("spotify:track:a"));
        assert_eq!(c.bytes(), 0);
    }

    #[test]
    fn una_semilla_no_pisa_lo_que_devolvio_spotify() {
        let t0 = Instant::now();
        let mut c = MetaCache::new(CAP_BYTES, TTL);
        c.put(key("spotify:track:a"), val(10, 1), false, t0);
        c.put(key("spotify:track:a"), val(10, 2), true, t0);
        let hit = c.get(&key("spotify:track:a"), t0).unwrap();
        assert_eq!(hit.value, val(10, 1));
        assert!(!hit.seeded);
        // Caducado lo comprobado, la semilla sí entra.
        c.put(key("spotify:track:a"), val(10, 2), true, t0 + TTL);
        assert!(c.get(&key("spotify:track:a"), t0 + TTL).unwrap().seeded);
    }

    #[test]
    fn tirar_las_semillas_deja_lo_demas() {
        let t0 = Instant::now();
        let mut c = MetaCache::new(CAP_BYTES, TTL);
        c.put(key("spotify:track:a"), val(10, 1), true, t0);
        c.put(key("spotify:track:b"), val(10, 2), false, t0);
        c.put(key("spotify:track:c"), val(10, 3), true, t0);
        assert_eq!(c.seeded(), 2);
        c.drop_seeded();
        assert_eq!(c.len(), 1);
        assert_eq!(c.seeded(), 0);
        assert_eq!(c.bytes(), key("spotify:track:b").size() + 10 + ENTRY_OVERHEAD);
    }

    #[test]
    fn sembrar_una_lista_enorme_respeta_el_tope() {
        let t0 = Instant::now();
        let mut c = MetaCache::new(CAP_BYTES, TTL);
        for i in 0..5000 {
            c.put(key(&format!("spotify:track:{i:022}")), val(2048, 7), true, t0);
            assert!(c.bytes() <= CAP_BYTES);
        }
        // Se quedan las últimas en llegar.
        assert!(c.contains(&key(&format!("spotify:track:{:022}", 4999)), t0));
        assert!(!c.contains(&key(&format!("spotify:track:{:022}", 0)), t0));
    }

    #[test]
    fn la_confianza_se_decide_con_la_primera_comparacion() {
        let t = SeedTrust::default();
        assert_eq!(t, SeedTrust::Unverified);
        assert!(t.accepts_seeds() && !t.serves_seeds());
        let ok = t.after_check(b"abc", b"abc");
        assert_eq!(ok, SeedTrust::Trusted);
        assert!(ok.serves_seeds());
        // Ya decidido, otra comparación no lo cambia.
        assert_eq!(ok.after_check(b"abc", b"xyz"), SeedTrust::Trusted);
        let bad = t.after_check(b"abc", b"abd");
        assert_eq!(bad, SeedTrust::Rejected);
        assert!(!bad.accepts_seeds() && !bad.serves_seeds());
        assert_eq!(bad.after_check(b"abc", b"abc"), SeedTrust::Rejected);
    }

    #[test]
    fn que_hacer_con_cada_caso() {
        let fetched = Some(Hit { value: 1, seeded: false });
        let seed = || Some(Hit { value: 2, seeded: true });
        // Lo que pidió el propio reproductor se sirve siempre.
        for trust in [SeedTrust::Unverified, SeedTrust::Trusted, SeedTrust::Rejected] {
            assert_eq!(
                lookup(fetched.clone(), trust),
                Lookup::Serve { value: 1, seeded: false }
            );
            assert_eq!(lookup::<i32>(None, trust), Lookup::Fetch);
        }
        // Una semilla: comprobar la primera vez, servirla si es de fiar, pedir si no.
        assert_eq!(lookup(seed(), SeedTrust::Unverified), Lookup::Check(2));
        assert_eq!(
            lookup(seed(), SeedTrust::Trusted),
            Lookup::Serve { value: 2, seeded: true }
        );
        assert_eq!(lookup(seed(), SeedTrust::Rejected), Lookup::Fetch);
    }
}
