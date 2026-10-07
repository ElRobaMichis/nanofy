//! Caché de storage-resolve y orden de los servidores de la CDN (Nanofy).
//!
//! Antes de bajar el primer trozo de una canción, librespot pregunta a spclient dónde está el
//! fichero (storage-resolve): otra ida y vuelta en cada carga, aunque la respuesta (las URL de la
//! CDN) caduca a las horas y se acababa de pedir para la misma canción (repetir, volver atrás,
//! recargar tras un corte, o al pasar el ratón por encima, ver la precarga inteligente). Ahora se
//! guarda mientras a su URL que antes caduca le queden más de `MIN_LEFT`.
//!
//! Además se recuerda qué servidor de la CDN contestó la última vez y cuáles fallaron: librespot
//! prueba las URL en orden y espera hasta 10 s a cada una, así que con un servidor caído cada
//! canción esperaba esos 10 s antes de probar el siguiente.
//!
//! No usa nada del resto del crate, para que sus pruebas corran en el binario de Nanofy.

use std::time::{Duration, Instant};

/// Ficheros cuya respuesta se guarda (las de las canciones recientes: unos pocos KB en total).
pub const CAP: usize = 64;
/// Una respuesta sirve mientras a su URL que antes caduca le queda más que esto (la caducidad
/// guardada ya lleva descontado el margen de 5 min de librespot).
pub const MIN_LEFT: Duration = Duration::from_secs(120);
/// Una respuesta sin caducidad en sus URL (la CDN actual siempre la pone) se guarda como mucho
/// esto, por si acaso.
pub const MAX_AGE_NO_EXPIRY: Duration = Duration::from_secs(30 * 60);
/// Lo que un servidor que falló pasa al final de la lista.
pub const BAD_HOST_FOR: Duration = Duration::from_secs(10 * 60);
/// Servidores que se recuerdan como fallidos a la vez.
const BAD_HOSTS_CAP: usize = 8;

/// ¿Sigue valiendo una respuesta guardada en `stored`? `min_expiry_ms`: caducidad (ms desde
/// 1970) de la URL que antes caduca; `None` si ninguna la trae.
pub fn usable(min_expiry_ms: Option<i64>, stored: Instant, now_ms: i64, now: Instant) -> bool {
    match min_expiry_ms {
        Some(expiry) => expiry.saturating_sub(now_ms) > MIN_LEFT.as_millis() as i64,
        None => now.saturating_duration_since(stored) < MAX_AGE_NO_EXPIRY,
    }
}

struct Stored<K, V> {
    key: K,
    value: V,
    min_expiry_ms: Option<i64>,
    at: Instant,
}

/// Las últimas `cap` respuestas, por fichero. Búsqueda lineal: son pocas y así cabe en un
/// `static` (constructor `const`).
pub struct StorageCache<K, V> {
    entries: Vec<Stored<K, V>>,
    cap: usize,
}

impl<K: PartialEq, V: Clone> StorageCache<K, V> {
    pub const fn new(cap: usize) -> Self {
        Self {
            entries: Vec::new(),
            cap,
        }
    }

    /// La respuesta guardada para `key` si aún sirve; una que ya no sirve se quita.
    pub fn get(&mut self, key: &K, now_ms: i64, now: Instant) -> Option<V> {
        let i = self.entries.iter().position(|e| e.key == *key)?;
        let e = &self.entries[i];
        if usable(e.min_expiry_ms, e.at, now_ms, now) {
            Some(e.value.clone())
        } else {
            self.entries.remove(i);
            None
        }
    }

    /// ¿Hay una respuesta que aún sirve para `key`? (sin quitar nada).
    pub fn contains(&self, key: &K, now_ms: i64, now: Instant) -> bool {
        self.entries
            .iter()
            .any(|e| e.key == *key && usable(e.min_expiry_ms, e.at, now_ms, now))
    }

    /// Guarda la respuesta de `key`; si ya no caben, sale la más antigua.
    pub fn put(&mut self, key: K, value: V, min_expiry_ms: Option<i64>, now: Instant) {
        self.forget(&key);
        if self.cap == 0 {
            return;
        }
        while self.entries.len() >= self.cap {
            self.entries.remove(0);
        }
        self.entries.push(Stored {
            key,
            value,
            min_expiry_ms,
            at: now,
        });
    }

    /// Olvida la respuesta de `key` (todas sus URL fallaron, o caducó antes de lo previsto).
    pub fn forget(&mut self, key: &K) {
        self.entries.retain(|e| e.key != *key);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// El servidor de una URL («audio4-fa.scdn.co»), sin puerto.
pub fn host_of(url: &str) -> Option<&str> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    // Sin usuario ni puerto.
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = authority.split(':').next().unwrap_or(authority);
    (!host.is_empty()).then_some(host)
}

/// Qué servidores de la CDN funcionaron y cuáles no, para probar antes el bueno.
pub struct HostPrefs {
    last_good: Option<String>,
    bad: Vec<(String, Instant)>,
}

impl Default for HostPrefs {
    fn default() -> Self {
        Self::new()
    }
}

impl HostPrefs {
    pub const fn new() -> Self {
        Self {
            last_good: None,
            bad: Vec::new(),
        }
    }

    /// Resultado de abrir un fichero: los servidores probados, en orden. librespot deja de probar
    /// en cuanto uno contesta, así que si se abrió (`ok`) el último es el bueno y los de antes
    /// fallaron; si no, fallaron todos.
    pub fn note(&mut self, attempted: &[String], ok: bool, now: Instant) {
        let (good, failed) = match (ok, attempted.split_last()) {
            (true, Some((good, failed))) => (Some(good), failed),
            _ => (None, attempted),
        };
        self.bad
            .retain(|(_, at)| now.saturating_duration_since(*at) < BAD_HOST_FOR);
        for host in failed {
            if good == Some(host) {
                continue;
            }
            self.bad.retain(|(h, _)| h != host);
            self.bad.push((host.clone(), now));
            if self.last_good.as_ref() == Some(host) {
                self.last_good = None;
            }
        }
        while self.bad.len() > BAD_HOSTS_CAP {
            self.bad.remove(0);
        }
        if let Some(good) = good {
            self.bad.retain(|(h, _)| h != good);
            self.last_good = Some(good.clone());
        }
    }

    /// 0: el último que funcionó; 1: sin historia; 2: falló hace poco.
    pub fn rank(&self, host: &str, now: Instant) -> u8 {
        if self.last_good.as_deref() == Some(host) {
            0
        } else if self
            .bad
            .iter()
            .any(|(h, at)| h == host && now.saturating_duration_since(*at) < BAD_HOST_FOR)
        {
            2
        } else {
            1
        }
    }

    /// Ordena `items` (URL) para probar primero el servidor bueno y al final los que fallaron. Es
    /// estable: entre iguales se respeta el orden de Spotify.
    pub fn order<T>(&self, items: &mut [T], url: impl Fn(&T) -> &str, now: Instant) {
        items.sort_by_key(|item| host_of(url(item)).map_or(1, |h| self.rank(h, now)));
    }

    pub fn last_good(&self) -> Option<&str> {
        self.last_good.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW_MS: i64 = 1_700_000_000_000;

    #[test]
    fn una_respuesta_vale_mientras_le_quedan_mas_de_dos_minutos() {
        let t0 = Instant::now();
        let min = MIN_LEFT.as_millis() as i64;
        assert!(usable(Some(NOW_MS + min + 1), t0, NOW_MS, t0));
        assert!(!usable(Some(NOW_MS + min), t0, NOW_MS, t0));
        assert!(!usable(Some(NOW_MS - 1), t0, NOW_MS, t0));
        // Sin caducidad: media hora como mucho.
        assert!(usable(None, t0, NOW_MS, t0 + MAX_AGE_NO_EXPIRY - Duration::from_secs(1)));
        assert!(!usable(None, t0, NOW_MS, t0 + MAX_AGE_NO_EXPIRY));
    }

    #[test]
    fn guarda_olvida_y_tira_lo_caducado() {
        let t0 = Instant::now();
        let mut c: StorageCache<u8, &str> = StorageCache::new(CAP);
        let later = NOW_MS + 3_600_000;
        c.put(1, "a", Some(later), t0);
        assert_eq!(c.get(&1, NOW_MS, t0), Some("a"));
        assert!(c.contains(&1, NOW_MS, t0));
        // A dos minutos de caducar ya no sirve, y se quita.
        assert_eq!(c.get(&1, later - 60_000, t0), None);
        assert!(c.is_empty());
        c.put(2, "b", Some(later), t0);
        c.forget(&2);
        assert_eq!(c.get(&2, NOW_MS, t0), None);
        // La misma clave otra vez sustituye a la anterior.
        c.put(3, "c", Some(later), t0);
        c.put(3, "d", Some(later), t0);
        assert_eq!(c.len(), 1);
        assert_eq!(c.get(&3, NOW_MS, t0), Some("d"));
    }

    #[test]
    fn con_el_tope_sale_la_mas_antigua() {
        let t0 = Instant::now();
        let mut c: StorageCache<usize, usize> = StorageCache::new(3);
        for i in 0..5 {
            c.put(i, i * 10, None, t0);
        }
        assert_eq!(c.len(), 3);
        assert_eq!(c.get(&0, NOW_MS, t0), None);
        assert_eq!(c.get(&1, NOW_MS, t0), None);
        assert_eq!(c.get(&4, NOW_MS, t0), Some(40));
    }

    #[test]
    fn el_servidor_de_una_url() {
        assert_eq!(
            host_of("https://audio4-fa.scdn.co/audio/abc?1688165560_x"),
            Some("audio4-fa.scdn.co")
        );
        assert_eq!(
            host_of("https://audio-ak-spotify-com.akamaized.net:443/audio/a?__token__=exp=1~h"),
            Some("audio-ak-spotify-com.akamaized.net")
        );
        assert_eq!(host_of("https://u:p@cdn.example?x"), Some("cdn.example"));
        assert_eq!(host_of("https:///nada"), None);
    }

    #[test]
    fn primero_el_bueno_y_al_final_los_que_fallaron() {
        let t0 = Instant::now();
        let mut prefs = HostPrefs::new();
        let urls = |hosts: &[&str]| -> Vec<String> {
            hosts.iter().map(|h| format!("https://{h}/audio/f?x")).collect()
        };
        // Sin historia, el orden de Spotify.
        let mut u = urls(&["a", "b", "c"]);
        prefs.order(&mut u, |s| s.as_str(), t0);
        assert_eq!(u, urls(&["a", "b", "c"]));
        // «a» no contestó y sí «b»: la próxima vez, «b» primero y «a» al final.
        prefs.note(&["a".into(), "b".into()], true, t0);
        assert_eq!(prefs.last_good(), Some("b"));
        let mut u = urls(&["a", "b", "c"]);
        prefs.order(&mut u, |s| s.as_str(), t0);
        assert_eq!(u, urls(&["b", "c", "a"]));
        // Pasado un rato, «a» vuelve a tener su sitio.
        let mut u = urls(&["a", "b", "c"]);
        prefs.order(&mut u, |s| s.as_str(), t0 + BAD_HOST_FOR);
        assert_eq!(u, urls(&["b", "a", "c"]));
    }

    #[test]
    fn si_fallan_todos_el_bueno_deja_de_serlo() {
        let t0 = Instant::now();
        let mut prefs = HostPrefs::new();
        prefs.note(&["b".into()], true, t0);
        prefs.note(&["b".into(), "c".into()], false, t0);
        assert_eq!(prefs.last_good(), None);
        assert_eq!(prefs.rank("b", t0), 2);
        assert_eq!(prefs.rank("c", t0), 2);
        assert_eq!(prefs.rank("d", t0), 1);
        // Y si luego contesta, vuelve a ser el bueno y deja la lista de fallidos.
        prefs.note(&["b".into()], true, t0);
        assert_eq!(prefs.rank("b", t0), 0);
        // Sin intentos (fichero en la caché del disco) no cambia nada.
        prefs.note(&[], true, t0);
        assert_eq!(prefs.last_good(), Some("b"));
    }

    #[test]
    fn los_fallidos_no_crecen_sin_fin() {
        let t0 = Instant::now();
        let mut prefs = HostPrefs::new();
        let hosts: Vec<String> = (0..20).map(|i| format!("h{i}")).collect();
        prefs.note(&hosts, false, t0);
        assert!(prefs.bad.len() <= BAD_HOSTS_CAP);
        // Se quedan los últimos.
        assert_eq!(prefs.rank("h19", t0), 2);
        assert_eq!(prefs.rank("h0", t0), 1);
    }
}
