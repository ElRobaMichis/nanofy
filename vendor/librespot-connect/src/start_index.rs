//! Con qué canción del contexto empieza una carga (Nanofy).
//!
//! Spirc buscaba la canción pedida por su uri en lo que ya tenía del contexto y, si no estaba
//! (una playlist enorme cuya página aún no había llegado), empezaba por la primera: un clic en la
//! canción 5.000 sonaba la 1. Tampoco distinguía dos copias de la misma canción en una lista:
//! siempre sonaba la primera. Ahora:
//! - la interfaz manda también la fila pulsada (`fallback_index`), pero solo se usa si en esa
//!   posición está justo la canción pedida: con la lista filtrada la fila no es la posición en el
//!   contexto, y entonces se busca por uri como siempre;
//! - si no se encuentra, Spirc trae más páginas del contexto (como mucho `MAX_EXTRA_PAGES` o
//!   `EXTRA_PAGES_BUDGET`) antes de rendirse;
//! - y al rendirse empieza por la primera canción, nunca falla la carga entera (antes un índice
//!   fuera de rango dejaba la interfaz en «cargando»).
//!
//! No usa nada del resto del crate, para que sus pruebas corran en el binario de Nanofy.

use std::time::Duration;

/// Páginas del contexto que se traen como mucho para encontrar la canción pedida.
pub const MAX_EXTRA_PAGES: usize = 5;
/// Tiempo como mucho para eso: después se empieza por la primera (lo que quede de páginas se
/// sigue trayendo en segundo plano, como siempre).
pub const EXTRA_PAGES_BUDGET: Duration = Duration::from_secs(2);

/// La canción pedida.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wanted<'a> {
    Uri(&'a str),
    Uid(&'a str),
    Index(usize),
}

/// Posición de la canción pedida en `tracks` (lo que ya se tiene del contexto), o `None` si no
/// está. `hint`: la fila que pulsó el usuario; manda solo si ahí está justo esa canción.
pub fn locate<T>(
    tracks: &[T],
    uri: impl Fn(&T) -> &str,
    uid: impl Fn(&T) -> &str,
    wanted: Wanted<'_>,
    hint: Option<usize>,
) -> Option<usize> {
    match wanted {
        Wanted::Uri(u) => hint
            .filter(|&i| tracks.get(i).is_some_and(|t| uri(t) == u))
            .or_else(|| tracks.iter().position(|t| uri(t) == u)),
        Wanted::Uid(id) => tracks.iter().position(|t| uid(t) == id),
        Wanted::Index(i) => (i < tracks.len()).then_some(i),
    }
}

/// Dónde empezar cuando la canción pedida no apareció: el índice que propuso Spotify (en una
/// orden de otro dispositivo) si existe en el contexto; si no, la primera.
pub fn give_up(len: usize, spotify_fallback: Option<usize>) -> usize {
    spotify_fallback.filter(|&i| i < len).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracks(uris: &[&str]) -> Vec<(String, String)> {
        uris.iter()
            .enumerate()
            .map(|(i, u)| (u.to_string(), format!("uid{i}")))
            .collect()
    }

    fn at(t: &[(String, String)], wanted: Wanted<'_>, hint: Option<usize>) -> Option<usize> {
        locate(t, |x| x.0.as_str(), |x| x.1.as_str(), wanted, hint)
    }

    #[test]
    fn la_fila_pulsada_manda_si_es_esa_cancion() {
        // La misma canción dos veces: suena la copia pulsada, no la primera.
        let t = tracks(&["a", "b", "a", "c"]);
        assert_eq!(at(&t, Wanted::Uri("a"), Some(2)), Some(2));
        assert_eq!(at(&t, Wanted::Uri("a"), None), Some(0));
    }

    #[test]
    fn con_la_lista_filtrada_la_fila_no_se_usa() {
        // Con el filtro, la fila 1 de la vista es otra canción del contexto: se busca por uri.
        let t = tracks(&["a", "b", "c", "d"]);
        assert_eq!(at(&t, Wanted::Uri("d"), Some(1)), Some(3));
        // Una fila fuera de lo que ya hay no rompe nada.
        assert_eq!(at(&t, Wanted::Uri("c"), Some(5000)), Some(2));
    }

    #[test]
    fn lo_que_no_esta_no_se_inventa() {
        // La canción 5.000 de una lista de la que solo hay la primera página: no está (Spirc
        // traerá más páginas), y la fila pulsada no sirve porque esa posición aún no existe.
        let t = tracks(&["a", "b"]);
        assert_eq!(at(&t, Wanted::Uri("z"), Some(4999)), None);
        assert_eq!(at(&t, Wanted::Uri("z"), Some(1)), None);
        assert_eq!(at(&t, Wanted::Index(2), None), None);
        assert_eq!(at(&t, Wanted::Uid("nada"), None), None);
        assert_eq!(at(&[] as &[(String, String)], Wanted::Uri("a"), Some(0)), None);
    }

    #[test]
    fn por_indice_y_por_uid() {
        let t = tracks(&["a", "b", "c"]);
        assert_eq!(at(&t, Wanted::Index(1), Some(0)), Some(1));
        assert_eq!(at(&t, Wanted::Uid("uid2"), None), Some(2));
    }

    #[test]
    fn al_rendirse_nunca_un_indice_que_no_existe() {
        assert_eq!(give_up(10, None), 0);
        assert_eq!(give_up(10, Some(4)), 4);
        assert_eq!(give_up(10, Some(10)), 0);
        assert_eq!(give_up(0, Some(0)), 0);
    }
}
