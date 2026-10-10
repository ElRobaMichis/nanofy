//! Explicaciones de Genius para la página de canción (el botón de comentarios de la letra): la
//! canción en su búsqueda, sus anotaciones y dónde cae cada una en nuestra letra. Solo datos: la
//! API pide (api.rs, `Client::genius`) y la página dibuja (app/track_page.rs).
//!
//! Se usa la API de la propia web de genius.com (`genius.com/api/…`), que no pide clave; la
//! documentada (`api.genius.com`) exige un token de aplicación. Ver docs/modulo-genius.pdf.

use serde_json::Value;

/// Una canción de Genius con sus anotaciones.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GeniusSong {
    /// Su página en genius.com.
    pub url: String,
    pub notes: Vec<GeniusNote>,
}

/// Una anotación: el trozo de letra que explica, la explicación y su página en genius.com.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GeniusNote {
    /// El texto anotado, versos separados por '\n'.
    pub fragment: String,
    /// La explicación en texto plano, párrafos separados por una línea en blanco.
    pub body: String,
    pub url: String,
}

/// Dónde va una anotación en nuestra letra: renglones `first..=last` (pueden incluir vacíos).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoteSpan {
    pub note: usize,
    pub first: usize,
    pub last: usize,
}

/// Parecido mínimo de un tramo de nuestra letra con un fragmento para situarlo ahí, y el de las
/// repeticiones (estribillos) respecto al mejor.
const MIN_SCORE: f32 = 0.6;
const REPEAT: f32 = 0.85;

/// Minúsculas, sin tildes y sin lo que no sea letra, número o espacio (los apóstrofos se quitan
/// sin dejar hueco: «we've» → «weve»). Para comparar títulos y artistas.
fn norm(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        let c = fold(c);
        if matches!(c, '\'' | '’' | '‘' | '`' | '´') {
            continue;
        }
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with(' ') {
            out.push(' ');
        }
    }
    out.trim().to_string()
}

/// Una letra sin su tilde («á» → «a», «ñ» → «n»).
fn fold(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ñ' => 'n',
        'ç' => 'c',
        c => c,
    }
}

/// Las palabras de un verso para cruzarlo: normalizadas, sin los encabezados entre corchetes
/// («[Verse 1: …]») y con «-ing» como «-in» («giving» y «givin'» son la misma).
pub fn words(s: &str) -> Vec<String> {
    let mut clean = String::with_capacity(s.len());
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth = depth.saturating_sub(1);
                clean.push(' ');
            }
            c if depth == 0 => clean.push(c),
            _ => {}
        }
    }
    norm(&clean)
        .split_whitespace()
        .map(|w| match w.strip_suffix("ing") {
            Some(stem) if stem.chars().count() >= 2 => format!("{stem}in"),
            _ => w.to_string(),
        })
        .collect()
}

/// Palabras en común (como multiconjunto) entre el máximo de palabras de los dos.
fn score(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut rest: Vec<&String> = b.iter().collect();
    let mut common = 0usize;
    for w in a {
        if let Some(k) = rest.iter().position(|x| *x == w) {
            rest.swap_remove(k);
            common += 1;
        }
    }
    common as f32 / a.len().max(b.len()) as f32
}

/// El título sin «(feat. …)», «[Remastered]» ni «- Live», normalizado.
fn title_key(s: &str) -> String {
    norm(crate::lyrics_sources::search_title(s))
}

/// La canción buscada entre los resultados de `search/multi`: su id y su página. Del «top_hit»
/// y luego de la sección de canciones, el primero cuyo artista principal y título coinciden con
/// los nuestros (normalizados; vale que uno contenga o empiece por el otro). Si ninguno, `None`:
/// mejor nada que las anotaciones de otra canción (un remix o una versión con el mismo nombre).
pub fn pick_song(search: &Value, title: &str, artist: &str) -> Option<(u64, String)> {
    let (want_t, want_a) = (title_key(title), norm(artist));
    if want_t.is_empty() || want_a.is_empty() {
        return None;
    }
    let sections = search["response"]["sections"].as_array()?;
    let hits = ["top_hit", "song"].into_iter().flat_map(|kind| {
        sections
            .iter()
            .filter(move |s| s["type"].as_str() == Some(kind))
            .flat_map(|s| s["hits"].as_array().into_iter().flatten())
    });
    for h in hits {
        let r = &h["result"];
        if h["type"].as_str() != Some("song") {
            continue;
        }
        let a = norm(r["primary_artist"]["name"].as_str().unwrap_or_default());
        let t = title_key(r["title"].as_str().unwrap_or_default());
        let artist_ok = !a.is_empty() && (a == want_a || a.contains(&want_a) || want_a.contains(&a));
        let title_ok = !t.is_empty() && (t == want_t || t.starts_with(&want_t) || want_t.starts_with(&t));
        if artist_ok && title_ok {
            let id = r["id"].as_u64()?;
            return Some((id, r["url"].as_str().unwrap_or_default().to_string()));
        }
    }
    None
}

/// El cuerpo de una anotación limpio: sin retornos de carro ni espacios al final de cada línea,
/// y los saltos de tres o más en un solo párrafo.
fn clean_body(s: &str) -> String {
    let lines: Vec<&str> = s.lines().map(str::trim_end).collect();
    let mut out = String::new();
    let mut blanks = 0;
    for l in lines {
        if l.trim().is_empty() {
            blanks += 1;
            continue;
        }
        if !out.is_empty() {
            out.push_str(if blanks > 0 { "\n\n" } else { "\n" });
        }
        blanks = 0;
        out.push_str(l.trim_start());
    }
    out
}

/// Las anotaciones de una página de `referents` y si hay otra. Solo las aceptadas o
/// verificadas (no las «unreviewed»), con explicación y que anotan letra (no un encabezado como
/// «[Verse 1: …]», que no se puede situar).
pub fn notes_from_referents(v: &Value) -> (Vec<GeniusNote>, bool) {
    let r = &v["response"];
    let notes = r["referents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|x| x["classification"].as_str() != Some("unreviewed"))
        .filter_map(|x| {
            let fragment = x["fragment"].as_str()?.trim().to_string();
            if words(&fragment).len() < 2 {
                return None;
            }
            let a = x["annotations"].as_array()?.first()?;
            let body = clean_body(a["body"]["plain"].as_str()?);
            if body.is_empty() {
                return None;
            }
            let url = x["url"].as_str().or_else(|| a["url"].as_str()).unwrap_or_default().to_string();
            Some(GeniusNote { fragment, body, url })
        })
        .collect();
    (notes, !r["next_page"].is_null())
}

/// Dónde va cada anotación en nuestra letra (`lines`, un renglón por elemento; los vacíos son
/// pausas). Cada tramo de renglones seguidos (saltando los vacíos, hasta tener los del fragmento
/// + 4 o 1,5 veces sus palabras) es un candidato con su parecido (`score`). Una anotación se
/// sitúa si su mejor candidato llega a `MIN_SCORE`, y también en los que llegan al `REPEAT` del
/// mejor (los estribillos que Genius anota una vez). Al final se reparten de mayor a menor
/// parecido sin solaparse: cada renglón, como mucho con una anotación (la que mejor encaja).
/// Ordenado por renglón.
pub fn match_notes(lines: &[&str], notes: &[GeniusNote]) -> Vec<NoteSpan> {
    let lw: Vec<Vec<String>> = lines.iter().map(|l| words(l)).collect();
    let mut cands: Vec<(f32, usize, usize, usize)> = Vec::new();
    for (n, note) in notes.iter().enumerate() {
        let fw = words(&note.fragment);
        if fw.len() < 2 {
            continue;
        }
        let frag_lines = note.fragment.lines().filter(|l| !words(l).is_empty()).count().max(1);
        let mut mine: Vec<(f32, usize, usize)> = Vec::new();
        for i in 0..lines.len() {
            if lw[i].is_empty() {
                continue;
            }
            let mut acc: Vec<String> = Vec::new();
            let mut used = 0;
            for j in i..lines.len() {
                if lw[j].is_empty() {
                    continue;
                }
                acc.extend(lw[j].iter().cloned());
                used += 1;
                mine.push((score(&fw, &acc), i, j));
                if used >= frag_lines + 4 || acc.len() * 2 > fw.len() * 3 {
                    break;
                }
            }
        }
        let best = mine.iter().map(|c| c.0).fold(0.0f32, f32::max);
        if best < MIN_SCORE {
            continue;
        }
        let floor = MIN_SCORE.max(REPEAT * best);
        cands.extend(mine.into_iter().filter(|c| c.0 >= floor - 1e-6).map(|(s, i, j)| (s, i, j, n)));
    }
    // De mayor a menor parecido; a igualdad, el que empieza antes y el más largo.
    cands.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then((b.2 - b.1).cmp(&(a.2 - a.1))));
    let mut taken = vec![false; lines.len()];
    let mut spans = Vec::new();
    for (_, i, j, n) in cands {
        if taken[i..=j].iter().any(|&t| t) {
            continue;
        }
        taken[i..=j].iter_mut().for_each(|t| *t = true);
        spans.push(NoteSpan { note: n, first: i, last: j });
    }
    spans.sort_by_key(|s| s.first);
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(fragment: &str) -> GeniusNote {
        GeniusNote { fragment: fragment.into(), body: "x".into(), url: String::new() }
    }

    #[test]
    fn palabras_normalizadas() {
        assert_eq!(words("What keeps the planet spinnin’, ah-ah"), ["what", "keeps", "the", "planet", "spinnin", "ah", "ah"]);
        assert_eq!(words("Your gift keeps on giving"), words("Your gift keeps on givin'"));
        assert_eq!(words("We’ve come"), words("We've come"));
        assert_eq!(words("[Verse 1: Pharrell Williams]"), Vec::<String>::new());
        assert_eq!(words("Canción del AÑO"), ["cancion", "del", "ano"]);
        // «sing» y «king» no pierden la g: el resto sería de una letra.
        assert_eq!(words("sing king"), ["sing", "king"]);
    }

    #[test]
    fn elige_la_cancion_buscada() {
        // Recorte real de search/multi para «Get Lucky Daft Punk» (oct 2026).
        let v: Value = serde_json::from_str(
            r#"{"response":{"sections":[
                {"type":"top_hit","hits":[{"type":"song","result":{"id":139968,"title":"Get Lucky","url":"https://genius.com/Daft-punk-get-lucky-lyrics","primary_artist":{"name":"Daft Punk"}}}]},
                {"type":"song","hits":[
                    {"type":"song","result":{"id":1850274,"title":"Get Lucky (Daft Punk Remix)","url":"u2","primary_artist":{"name":"Daft Punk"}}},
                    {"type":"song","result":{"id":3276417,"title":"Get Lucky (Daft Punk ft. Pharrell Williams cover)","url":"u3","primary_artist":{"name":"Helia"}}}]},
                {"type":"artist","hits":[{"type":"artist","result":{"id":1,"name":"Daft Punk"}}]}]}}"#,
        )
        .unwrap();
        let got = pick_song(&v, "Get Lucky (feat. Pharrell Williams and Nile Rodgers)", "Daft Punk");
        assert_eq!(got, Some((139968, "https://genius.com/Daft-punk-get-lucky-lyrics".to_string())));
        // Otro artista: ninguna es la buscada.
        assert_eq!(pick_song(&v, "Get Lucky", "Strawberry Guy"), None);
        // La versión de otra artista no se confunde con la original.
        assert_eq!(pick_song(&v, "Get Lucky", "Helia").map(|x| x.0), Some(3276417));
        assert_eq!(pick_song(&serde_json::json!({}), "Get Lucky", "Daft Punk"), None);
    }

    #[test]
    fn anotaciones_de_una_pagina() {
        let v: Value = serde_json::from_str(
            r#"{"response":{"next_page":2,"referents":[
                {"fragment":"Like the legend of the phoenix, huh\n All ends with beginnings","classification":"accepted","url":"https://genius.com/1685964/x",
                 "annotations":[{"body":{"plain":"In Greek mythology \r\n\n\n\nPharrell mentioned  \nthis legend"},"url":"a"}]},
                {"fragment":"[Verse 1: Pharrell Williams]","classification":"accepted","annotations":[{"body":{"plain":"Third time"}}]},
                {"fragment":"Some new words here","classification":"unreviewed","annotations":[{"body":{"plain":"?"}}]},
                {"fragment":"Words with no body","classification":"verified","annotations":[{"body":{"plain":"  "}}]}]}}"#,
        )
        .unwrap();
        let (notes, more) = notes_from_referents(&v);
        assert!(more);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(notes[0].body, "In Greek mythology\n\nPharrell mentioned\nthis legend");
        assert_eq!(notes[0].url, "https://genius.com/1685964/x");
        let (_, more) = notes_from_referents(&serde_json::json!({"response": {"next_page": null, "referents": []}}));
        assert!(!more);
    }

    #[test]
    fn cada_anotacion_en_su_sitio() {
        // Versos inventados con las diferencias de verdad entre fuentes: coletillas, dos versos
        // en uno, «to»/«till», «-ing»/«-in'», apóstrofos tipográficos y estribillos repetidos.
        let lines = [
            "Under the morning light",            // 0
            "Every road begins again",            // 1
            "",                                   // 2
            "We've walked so far to find out where we are", // 3
            "",                                   // 4
            "She's dancing till the dawn",        // 5
            "She's singing all night long",       // 6
            "We're dancing till the dawn",        // 7
            "We're singing all night long",       // 8
            "",                                   // 9
            "Nothing here was ever planned",      // 10
            "",                                   // 11
            "We're dancing till the dawn",        // 12
            "We're singing all night long",       // 13
        ];
        let notes = [
            note("Under the morning light, oh\n Every road begins again"),
            note("We’ve walked so far\n To find out where we are"),
            note("We’re dancing to the dawn\n We’re singin’ all night long"),
            note("She’s dancing to the dawn\n She’s singing all night long"),
            note("A verse that is not in this song at all"),
            note("[Chorus]"),
        ];
        let spans = match_notes(&lines, &notes);
        let got: Vec<(usize, usize, usize)> = spans.iter().map(|s| (s.note, s.first, s.last)).collect();
        assert_eq!(got, [(0, 0, 1), (1, 3, 3), (3, 5, 6), (2, 7, 8), (2, 12, 13)]);
        // Ningún renglón con dos anotaciones.
        for w in spans.windows(2) {
            assert!(w[0].last < w[1].first, "{spans:?}");
        }
    }

    /// La comprobación con datos reales (anotaciones de Genius y la letra de Get Lucky que da
    /// Nanofy): están en qa/, que no se publica; sin ellos la prueba se salta.
    #[test]
    fn get_lucky_de_verdad() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("qa").join("fixtures");
        let (Ok(refs), Ok(lyr)) = (std::fs::read_to_string(dir.join("genius_get_lucky.json")), std::fs::read_to_string(dir.join("get_lucky_lyrics.json"))) else {
            return;
        };
        let (notes, _) = notes_from_referents(&serde_json::from_str(&refs).unwrap());
        let lyr: Value = serde_json::from_str(&lyr).unwrap();
        let owned: Vec<String> = lyr["lines"].as_array().unwrap().iter().map(|l| l["words"].as_str().unwrap_or_default().to_string()).collect();
        let lines: Vec<&str> = owned.iter().map(String::as_str).collect();
        let spans = match_notes(&lines, &notes);
        let first = |frag: &str| {
            let n = notes.iter().position(|x| x.fragment.starts_with(frag)).unwrap();
            spans.iter().filter(|s| s.note == n).map(|s| (s.first, s.last)).collect::<Vec<_>>()
        };
        assert_eq!(notes.len(), 9, "la del encabezado «[Verse 1]» no cuenta");
        assert_eq!(first("Like the legend"), [(0, 1)]);
        assert_eq!(first("What keeps the planet"), [(2, 3)]);
        assert_eq!(first("We’ve come too far"), [(5, 5), (27, 27), (97, 97)]);
        assert_eq!(first("The present has no ribbon"), [(22, 23)]);
        assert_eq!(first("What is this I’m feelin’"), [(24, 25)]);
        assert_eq!(first("We’re up all night to get\n"), [(72, 95)]);
        let annotated: usize = spans.iter().map(|s| (s.first..=s.last).filter(|&k| !lines[k].trim().is_empty()).count()).sum();
        assert_eq!(annotated, 102);
    }
}
