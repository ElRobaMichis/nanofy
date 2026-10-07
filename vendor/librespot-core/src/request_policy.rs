//! Plazos de las peticiones a spclient (Nanofy).
//!
//! librespot no ponía ningún plazo a estas peticiones: una que no volvía (un socket medio cerrado
//! tras suspender el equipo, un servidor que no contesta) dejaba esperando para siempre a quien
//! la hizo. Si era el aviso de estado de Spirc o la resolución del contexto, Pausa y Siguiente se
//! quedaban en cola detrás; si era la carga de la canción, «cargando» sin fin. Ahora cada intento
//! tiene un plazo según lo que se pide, y se hacen como mucho tres.
//!
//! No usa nada del resto del crate, para que sus pruebas corran en el binario de Nanofy.

use std::time::Duration;

/// Lo que está en el camino de cada orden o de cada canción: el estado de Spotify Connect, el
/// contexto, la resolución del fichero de audio y los metadatos de una canción. Son respuestas
/// pequeñas; si en 6 s no ha llegado nada, es mejor probar otra vez (y a la tercera, otro punto
/// de acceso) que seguir esperando.
pub const FAST: Duration = Duration::from_secs(6);
/// El resto de lo que pide librespot (letras, perfiles, radio…).
pub const NORMAL: Duration = Duration::from_secs(10);
/// Listas enteras (rootlist, playlists de miles de canciones), lotes de metadatos de hasta 500
/// entidades, la portada (un JSON grande) y las escrituras de Nanofy (cambios de playlist): con
/// mala conexión pueden tardar más de 10 s de verdad, y Nanofy ya les pone su propio plazo de
/// 15-30 s. Uno más corto aquí los cortaría antes que el suyo y una lista grande no cargaría
/// nunca.
pub const LONG: Duration = Duration::from_secs(30);
/// Intentos como mucho: con tres de 6 s una petición colgada se da por perdida en 18 s.
pub const MAX_TRIES: usize = 3;

/// Cómo se hace cada intento de una petición.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttemptPolicy {
    /// Plazo de cada intento, desde pedir los tokens hasta tener la respuesta entera.
    pub timeout: Duration,
    pub max_tries: usize,
    /// Se puede repetir tras agotar el plazo. Una escritura que no es idempotente (añadir a una
    /// playlist, un comando de la Jam) quizá ya se aplicó aunque la respuesta no llegara: al
    /// repetirla se añadiría dos veces o se saltarían dos canciones.
    pub retry_on_timeout: bool,
}

/// Un POST que en realidad es una lectura: repetirlo no cambia nada.
fn is_read_post(path: &str) -> bool {
    path.starts_with("/extended-metadata/") || path.starts_with("/context-resolve/")
}

/// Plazo, intentos y si se repite tras agotar el plazo, según el método y la ruta (con o sin
/// parámetros) de la petición.
pub fn attempt_policy(method: &str, endpoint: &str) -> AttemptPolicy {
    let path = endpoint.split('?').next().unwrap_or(endpoint);
    let idempotent =
        matches!(method, "GET" | "HEAD" | "PUT" | "DELETE" | "OPTIONS") || is_read_post(path);
    let fast = path.starts_with("/connect-state/v1/devices/")
        || path.starts_with("/context-resolve/")
        || path.starts_with("/storage-resolve/");
    let long = path.starts_with("/playlist/")
        || path.starts_with("/playlist-permission/")
        || path.starts_with("/extended-metadata/")
        || path.starts_with("/homeview/");
    let timeout = if fast {
        FAST
    } else if long {
        LONG
    } else if !idempotent && !path.starts_with("/connect-state/") {
        // Escrituras (Jam, social…): las de Nanofy llevan su propio plazo, más largo. Las de
        // connect-state (transferir, comandos de la Jam) las espera Spirc: esas, el normal.
        LONG
    } else {
        NORMAL
    };
    AttemptPolicy {
        timeout,
        max_tries: MAX_TRIES,
        retry_on_timeout: idempotent,
    }
}

/// Como `attempt_policy`, pero con el plazo corto: para los metadatos de una sola canción que
/// pide el reproductor al cargarla (la misma ruta que los lotes grandes de Nanofy).
pub fn fast_policy(method: &str, endpoint: &str) -> AttemptPolicy {
    AttemptPolicy {
        timeout: FAST,
        ..attempt_policy(method, endpoint)
    }
}

/// Tras el intento número `tries` fallido por la red o por el plazo, ¿se cambia de punto de
/// acceso antes del siguiente? Cada tres, como librespot, y también antes del último: con solo
/// tres intentos, la regla de librespot no lo cambiaba nunca y los tres iban al que no contesta.
pub fn flush_accesspoint_after(tries: usize, max_tries: Option<usize>) -> bool {
    tries % 3 == 0 || max_tries.is_some_and(|max| tries + 1 == max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lo_que_bloquea_la_reproduccion_tiene_el_plazo_corto() {
        let device = "/connect-state/v1/devices/abc123";
        assert_eq!(attempt_policy("PUT", device).timeout, FAST);
        assert_eq!(attempt_policy("PUT", &format!("{device}/inactive?notify=false")).timeout, FAST);
        assert_eq!(attempt_policy("DELETE", device).timeout, FAST);
        assert_eq!(attempt_policy("GET", "/context-resolve/v1/spotify:playlist:x").timeout, FAST);
        assert_eq!(attempt_policy("POST", "/context-resolve/v1/autoplay").timeout, FAST);
        assert_eq!(
            attempt_policy("GET", "/storage-resolve/files/audio/interactive/ABCD?salt=1").timeout,
            FAST
        );
        // Todo eso se puede repetir: leer o sustituir el estado entero.
        for (m, e) in [("PUT", device), ("POST", "/context-resolve/v1/autoplay")] {
            assert!(attempt_policy(m, e).retry_on_timeout, "{m} {e}");
        }
    }

    #[test]
    fn listas_y_lotes_grandes_no_se_cortan_antes_que_nanofy() {
        // Nanofy espera hasta 15-30 s a estas: un plazo más corto aquí las cortaría antes.
        for e in [
            "/playlist/v2/user/u/rootlist?decorate=revision&from=0&length=5000",
            "/playlist/v2/playlist/37i9dQZF1DXcBWIGoYBM5M",
            "/extended-metadata/v0/extended-metadata",
            "/homeview/v1/home?platform=web&locale=es",
        ] {
            assert_eq!(attempt_policy("GET", e).timeout, LONG, "{e}");
        }
        assert_eq!(
            attempt_policy("POST", "/extended-metadata/v0/extended-metadata").timeout,
            LONG
        );
        // …salvo los metadatos de una canción que pide el reproductor, que van por el corto.
        let one = fast_policy("POST", "/extended-metadata/v0/extended-metadata");
        assert_eq!(one.timeout, FAST);
        assert!(one.retry_on_timeout);
    }

    #[test]
    fn una_escritura_no_se_repite_tras_agotar_el_plazo() {
        // Añadir canciones: quizá ya se aplicó; repetirla las añadiría dos veces.
        let add = attempt_policy("POST", "/playlist/v2/playlist/abc/changes");
        assert!(!add.retry_on_timeout);
        assert_eq!(add.timeout, LONG);
        // Un comando de la Jam o una transferencia: los espera Spirc, plazo normal y sin repetir
        // (un «siguiente» repetido saltaría dos canciones).
        let jam = attempt_policy("POST", "/connect-state/v1/player/command/from/a/to/social-connect-b");
        assert_eq!(jam.timeout, NORMAL);
        assert!(!jam.retry_on_timeout);
        let transfer = attempt_policy("POST", "/connect-state/v1/connect/transfer/from/a/to/a");
        assert_eq!(transfer.timeout, NORMAL);
        assert!(!transfer.retry_on_timeout);
        // Otras escrituras (salir de una Jam): el largo.
        assert_eq!(attempt_policy("POST", "/social-connect/v3/sessions/x/leave").timeout, LONG);
        // PUT y DELETE sustituyen o quitan: repetirlos da lo mismo.
        assert!(attempt_policy("PUT", "/playlist-permission/v1/playlist/x/permission/base").retry_on_timeout);
        assert!(attempt_policy("DELETE", "/social-connect/v3/sessions/x/members/me").retry_on_timeout);
    }

    #[test]
    fn el_resto_tiene_el_normal_y_tres_intentos() {
        for e in [
            "/color-lyrics/v2/track/abc",
            "/user-profile-view/v3/profile/u",
            "/radio-apollo/v3/stations/spotify:track:x?autoplay=true",
            "/inspiredby-mix/v2/seed_to_playlist/spotify:track:x?response-format=json",
        ] {
            let p = attempt_policy("GET", e);
            assert_eq!(p.timeout, NORMAL, "{e}");
            assert_eq!(p.max_tries, MAX_TRIES);
            assert!(p.retry_on_timeout);
        }
        // Lo peor para el bucle de Spirc: un PUT de estado que nunca contesta.
        let p = attempt_policy("PUT", "/connect-state/v1/devices/x");
        assert_eq!(p.timeout * p.max_tries as u32, Duration::from_secs(18));
    }

    #[test]
    fn el_punto_de_acceso_se_cambia_antes_del_ultimo_intento() {
        // Con tres intentos: tras el segundo fallo, para que el tercero vaya a otro.
        assert!(!flush_accesspoint_after(1, Some(3)));
        assert!(flush_accesspoint_after(2, Some(3)));
        // Con la estrategia de librespot (diez), cada tres como siempre y antes del último.
        let flushed: Vec<usize> = (1..10).filter(|&t| flush_accesspoint_after(t, Some(10))).collect();
        assert_eq!(flushed, vec![3, 6, 9]);
        // Sin límite, cada tres.
        assert!(flush_accesspoint_after(3, None));
        assert!(!flush_accesspoint_after(4, None));
        // Con un solo intento no se cambia: como en librespot, entonces lo decide quien llama.
        assert!(!flush_accesspoint_after(1, Some(1)));
    }
}
